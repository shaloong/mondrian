//! The live admission consumer must include both row arrays at each transition.
use super::*;
use std::alloc::Layout;

#[test]
fn peak_includes_all_four_actual_phase_row_types_and_one_event_key_payload() {
    let (lanes, events, keys) = (3, 14, 19);
    let projected = Layout::array::<ProjectedLane>(lanes).unwrap().size();
    let requested = Layout::array::<RequestedTarget>(lanes).unwrap().size();
    let prepared = Layout::array::<PreparedNoteTarget>(lanes).unwrap().size();
    let event_bytes = Layout::array::<PreparedNoteEvent>(events).unwrap().size();
    let expected = [
        projected,
        projected + requested,
        requested + prepared,
        prepared,
    ]
    .into_iter()
    .max()
    .unwrap()
        + event_bytes
        + keys;
    assert_eq!(
        requested_note_lowering_peak_bytes(lanes, events, keys).unwrap(),
        expected
    );
    assert_eq!(requested_note_lowering_peak_bytes(0, 0, 0).unwrap(), 0);
}

#[test]
fn peak_checks_independent_row_event_and_key_extents_without_allocations() {
    for (lanes, events, keys) in [(usize::MAX, 0, 0), (0, usize::MAX, 0), (1, 0, usize::MAX)] {
        assert_eq!(
            requested_note_lowering_peak_bytes(lanes, events, keys),
            Err(AudioCompileError::NoteEventCountOverflow)
        );
    }
}
