//! Public author transactions over the sole Sequence musical reference.
use crate::app::product_action::{
    ProductAction, SequenceProductAction, SequenceSetMusicalTimeReferencePayload,
    SEQUENCE_NAMESPACE, SEQUENCE_SET_MUSICAL_TIME_REFERENCE,
};
use crate::app::ui_actions::sequence_set_musical_time_reference_action;
use crate::app::AppState;
use mondrian_core::{MusicalTimeReference, MusicalTimeSignature, SequenceId, TimelineTime};
use mondrian_editor_state::Action;
use mondrian_timeline::Sequence;

fn reference() -> MusicalTimeReference {
    MusicalTimeReference::new(
        TimelineTime::new(-13, 8).unwrap(),
        TimelineTime::new(3, 4).unwrap(),
    )
    .unwrap()
    .with_time_signature(Some(MusicalTimeSignature::new(7, 8).unwrap()))
}

fn action(sequence_id: SequenceId, reference: Option<MusicalTimeReference>) -> Action {
    sequence_set_musical_time_reference_action(SequenceSetMusicalTimeReferencePayload {
        sequence_id,
        reference,
    })
}

#[test]
fn musical_reference_public_edit_history_and_archive_preserve_unrelated_author_state() {
    let mut state = AppState::new();
    let sequence = Sequence::new("Musical authoring");
    let id = sequence.id;
    state.test_set_sequence(Some(sequence.clone()));
    let initial = state.project_author_generation();
    let binding = state.current_audio_playback_author_binding().unwrap();
    let before_revision = state.active_sequence().unwrap().revision;
    let reference = reference();
    let typed = ProductAction::Sequence(SequenceProductAction::SetMusicalTimeReference(
        SequenceSetMusicalTimeReferencePayload { sequence_id: id, reference: Some(reference) },
    ));
    assert!(state.product_action_availability().allows(&typed));
    assert_eq!(
        ProductAction::decode_external(&typed.clone().into_external_action()).unwrap(),
        Some(typed.clone())
    );
    state.dispatch_action(action(id, Some(reference))).unwrap();
    assert_eq!(state.project_author_generation(), initial + 1);
    assert!(!state.product_action_availability().allows(&typed));
    assert_ne!(
        state.current_audio_playback_author_binding().unwrap(),
        binding
    );
    let mut expected = sequence.clone();
    expected.audio_program.musical_time_reference = Some(reference);
    assert!(state.active_sequence().unwrap().author_state_eq_ignoring_revision(&expected));
    assert!(state.active_sequence().unwrap().revision > before_revision);
    let history = state.authoring.as_ref().unwrap().history().diagnostics();
    state.dispatch_action(action(id, Some(reference))).unwrap();
    assert_eq!(state.project_author_generation(), initial + 1);
    assert_eq!(
        state.authoring.as_ref().unwrap().history().diagnostics(),
        history
    );
    assert!(state.undo_timeline().unwrap());
    assert!(state.active_sequence().unwrap().author_state_eq_ignoring_revision(&sequence));
    assert_eq!(
        state.current_audio_playback_author_binding().unwrap(),
        binding
    );
    assert!(state.redo_timeline().unwrap());
    assert!(state.active_sequence().unwrap().author_state_eq_ignoring_revision(&expected));
    state.dispatch_action(action(id, None)).unwrap();
    assert_eq!(
        state.active_sequence().unwrap().audio_program.musical_time_reference,
        None
    );
    assert!(state.undo_timeline().unwrap());
    assert_eq!(
        state.active_sequence().unwrap().audio_program.musical_time_reference,
        Some(reference)
    );

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("音乐参考.mdp");
    state.save_project_file_as(path.clone()).unwrap();
    drop(state);
    let mut reopened = AppState::new();
    reopened.open_project_file(path).unwrap();
    assert!(reopened.active_sequence().unwrap().author_state_eq_ignoring_revision(&expected));
}

#[test]
fn musical_reference_codec_refuses_incomplete_or_invalid_payloads_atomically() {
    let mut state = AppState::new();
    let sequence = Sequence::new("Invalid musical edits");
    let id = sequence.id;
    state.test_set_sequence(Some(sequence));
    state.dispatch_action(action(id, Some(reference()))).unwrap();
    let before = serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
    let generation = state.project_author_generation();
    let history = state.authoring.as_ref().unwrap().history().diagnostics();
    let valid = serde_json::to_value(reference()).unwrap();
    let mut duration_zero = valid.clone();
    duration_zero["quarter_note_duration"] = serde_json::to_value(TimelineTime::ZERO).unwrap();
    let mut invalid_meter = valid.clone();
    invalid_meter["time_signature"]["denominator"] = 0.into();
    for payload in [
        serde_json::json!({"sequence_id": id}),
        serde_json::json!({"reference": valid}),
        serde_json::json!({"sequence_id": id, "reference": duration_zero}),
        serde_json::json!({"sequence_id": id, "reference": invalid_meter}),
        serde_json::json!({"sequence_id": id, "reference": null, "tempo": 120}),
    ] {
        assert!(state
            .dispatch_action(Action::Custom {
                namespace: SEQUENCE_NAMESPACE.into(),
                name: SEQUENCE_SET_MUSICAL_TIME_REFERENCE.into(),
                payload,
            })
            .is_err());
    }
    assert!(state.dispatch_action(action(SequenceId::new(), None)).is_err());
    assert_eq!(state.project_author_generation(), generation);
    assert_eq!(
        state.authoring.as_ref().unwrap().history().diagnostics(),
        history
    );
    assert_eq!(
        serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap(),
        before
    );
}

#[test]
fn musical_reference_action_keeps_explicit_owner_after_navigation() {
    let mut state = AppState::new();
    state.test_set_sequence(Some(Sequence::new("First")));
    let first = state.active_sequence_id().unwrap();
    let retained = action(first, Some(reference()));
    let second = state.new_sequence("Second").unwrap();
    assert_eq!(state.active_sequence_id(), Some(second));
    state.dispatch_action(retained).unwrap();
    assert_eq!(state.active_sequence_id(), Some(second));
    assert_eq!(
        state.active_sequence().unwrap().audio_program.musical_time_reference,
        None
    );
    assert_eq!(
        state.sequence_by_id(first).unwrap().audio_program.musical_time_reference,
        Some(reference())
    );
}
