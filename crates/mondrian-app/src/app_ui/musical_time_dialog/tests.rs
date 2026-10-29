use super::*;
use crate::app_ui::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
use mondrian_ui_core::widget::EventRequests;
use std::cell::RefCell;

#[test]
fn exact_music_text_round_trips_full_core_range_and_unknown_meter() {
    for duration in [
        TimelineTime::new(1, i64::MAX).unwrap(),
        TimelineTime::new(i64::MAX, 1).unwrap(),
        TimelineTime::new(3, 4).unwrap(),
        TimelineTime::new(i64::MAX - 1, i64::MAX).unwrap(),
    ] {
        for origin in [
            TimelineTime::new(i64::MIN, 1).unwrap(),
            TimelineTime::new(-13, 8).unwrap(),
        ] {
            for signature in [None, Some(MusicalTimeSignature::new(5, 7).unwrap())] {
                let mut sequence = Sequence::new("Exact music");
                sequence.audio_program.musical_time_reference = Some(
                    MusicalTimeReference::new(origin, duration)
                        .unwrap()
                        .with_time_signature(signature),
                );
                let draft = MusicalTimeDraft::from_sequence(&sequence);
                assert_eq!(
                    draft.into_payload().unwrap().reference,
                    sequence.audio_program.musical_time_reference
                );
            }
        }
    }
}

#[test]
fn exact_music_decimal_edit_never_rounds_and_retains_unknown_signature() {
    let mut draft = MusicalTimeDraft::from_sequence(&Sequence::new("Edit"));
    assert_eq!(draft.clone().into_payload().unwrap().reference, None);
    draft.enabled = true;
    draft.tempo = "80.1".into();
    draft.origin = "-.125".into();
    let reference = draft.clone().into_payload().unwrap().reference.unwrap();
    assert_eq!(
        reference.quarter_note_duration(),
        TimelineTime::new(200, 267).unwrap()
    );
    assert_eq!(
        reference.quarter_note_zero(),
        TimelineTime::new(-1, 8).unwrap()
    );
    assert_eq!(reference.time_signature(), None);
    draft.signature = " 7 / 8 ".into();
    assert_eq!(
        draft.into_payload().unwrap().reference.unwrap().time_signature(),
        Some(MusicalTimeSignature::new(7, 8).unwrap())
    );
}

#[test]
fn invalid_music_text_refuses_instead_of_defaulting_or_clamping() {
    let mut draft = MusicalTimeDraft::from_sequence(&Sequence::new("Invalid"));
    draft.enabled = true;
    for text in [
        "",
        "-1",
        "0",
        "NaN",
        "inf",
        "1e3",
        "1/0",
        "1/-2",
        ".",
        "1.2.3",
        "1/2/3",
        "170141183460469231731687303715884105728",
        "0.000000000000000000000000000001",
    ] {
        draft.tempo = text.into();
        assert!(
            matches!(draft.clone().into_payload(), Err("music-invalid-tempo")),
            "{text}"
        );
    }
    draft.tempo = "80".into();
    for text in ["", "1/0", "NaN", "9223372036854775808", "--1", "1e-3"] {
        draft.origin = text.into();
        assert!(
            matches!(draft.clone().into_payload(), Err("music-invalid-origin")),
            "{text}"
        );
    }
    draft.origin = "0".into();
    for text in ["0/4", "4/0", "4", "-1/4", "65536/4", "7/8/2"] {
        draft.signature = text.into();
        assert!(
            matches!(draft.clone().into_payload(), Err("music-invalid-signature")),
            "{text}"
        );
    }
    draft.enabled = false;
    assert_eq!(
        draft.into_payload().unwrap().reference,
        None,
        "explicit removal does not require a valid discarded draft"
    );
}

#[test]
fn music_controls_preserve_identity_and_draft_while_validity_changes() {
    let mut dialog = MusicalTimeDialog::new(
        MusicalTimeDraft::from_sequence(&Sequence::new("Draft")),
        AppUiLocale::EnUs,
    );
    let ids = dialog.inputs.each_ref().map(|input| input.id());
    assert_eq!(dialog.title.text(), "Musical time");
    dialog.apply_update(MusicalTimeDraftUpdate::Enabled(true));
    dialog.apply_update(MusicalTimeDraftUpdate::Tempo("80/".into()));
    assert!(dialog.payload().is_none());
    assert!(!dialog.error.text().is_empty());
    assert_eq!(dialog.inputs[0].text(), "80/");
    dialog.apply_update(MusicalTimeDraftUpdate::Origin("-1/3".into()));
    dialog.apply_update(MusicalTimeDraftUpdate::Tempo("80".into()));
    assert_eq!(dialog.inputs.each_ref().map(|input| input.id()), ids);
    assert!(dialog.error.text().is_empty());
    assert_eq!(
        dialog.payload().unwrap().reference.unwrap().quarter_note_zero(),
        TimelineTime::new(-1, 3).unwrap()
    );
    for input in &dialog.inputs {
        assert!(
            input.accessibility().is_some(),
            "normal text controls remain in the accessibility tree"
        );
    }
}

#[test]
fn music_keyboard_keeps_ime_preedit_local_then_applies_or_cancels() {
    let mut dialog = MusicalTimeDialog::new(
        MusicalTimeDraft::from_sequence(&Sequence::new("Keyboard")),
        AppUiLocale::EnUs,
    );
    dialog.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
    let actions = RefCell::new(Vec::new());
    let dispatch = |a| actions.borrow_mut().push(a);
    let (mut focus, mut shortcuts, mut tooltip, mut requests) = (
        DummyFocus,
        DummyShortcut,
        DummyTooltip,
        EventRequests::default(),
    );
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcuts,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );
    dialog.apply_update(MusicalTimeDraftUpdate::Enabled(true));
    dialog.inputs[0].event(&UiEvent::focus_gained_programmatic(), &mut ctx);
    dialog.inputs[0].event(&UiEvent::ImePreedit("8".into()), &mut ctx);
    assert!(dialog.payload().is_none());
    dialog.event(
        &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
        &mut ctx,
    );
    assert!(
        actions.borrow().is_empty(),
        "IME confirmation must not apply the old valid value"
    );
    dialog.event(
        &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
        &mut ctx,
    );
    assert!(
        actions.borrow().is_empty(),
        "first Escape belongs to active composition"
    );
    assert!(!dialog.inputs[0].is_composing());
    dialog.event(
        &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
        &mut ctx,
    );
    assert_eq!(
        actions.borrow_mut().pop(),
        Some(app_shell_confirm_musical_time_action())
    );
    dialog.event(
        &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
        &mut ctx,
    );
    assert_eq!(
        actions.borrow_mut().pop(),
        Some(app_shell_close_modal_action())
    );
}
