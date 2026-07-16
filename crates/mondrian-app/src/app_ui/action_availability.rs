//! Shared app UI action availability gates.
//!
//! Menus, focused shortcuts, and panel adapters use this module to keep their
//! disabled states aligned before actions reach `AppState`.

use mondrian_core::types::{FramePosition, Rational, TrackId};
use mondrian_core::TimelineTime;
use mondrian_editor_state::Action;

use crate::app::ui_actions::{
    TimelineMoveClipPayload, TimelineMoveTrackPayload, TimelineSelectClipPayload,
    TimelineSetInOutPointPayload, TimelineSetSelectedClipsEnabledPayload,
    TimelineSetTrackControlPayload, TimelineTrimClipsPayload, TimelineTrimPayloadEdge,
    TimelineTrimSelectedClipsToPlayheadPayload, APP_SHELL_IMPORT_MEDIA_DIALOG, APP_SHELL_NAMESPACE,
    APP_SHELL_PROJECT_SETTINGS, APP_SHELL_SAVE_PROJECT_AS_DIALOG, APP_SHELL_SEQUENCE_SETTINGS,
    SEQUENCE_DELETE, SEQUENCE_DUPLICATE, SEQUENCE_NAMESPACE, SEQUENCE_RETURN_TO_PARENT,
    SEQUENCE_SET_ACTIVE_DEFAULT, SEQUENCE_SWITCH_ACTIVE, TIMELINE_ADD_TRACK,
    TIMELINE_CLEAR_IN_OUT_POINTS, TIMELINE_MOVE_CLIP, TIMELINE_MOVE_TRACK, TIMELINE_NAMESPACE,
    TIMELINE_ROLL_SELECTED_CUT_TO_PLAYHEAD, TIMELINE_SEEK, TIMELINE_SELECT_CLIP,
    TIMELINE_SET_IN_OUT_POINT, TIMELINE_SET_SELECTED_CLIPS_ENABLED, TIMELINE_SET_TRACK_CONTROL,
    TIMELINE_TRIM_CLIPS, TIMELINE_TRIM_SELECTED_CLIPS_TO_PLAYHEAD,
};
use crate::app::AppState;
use mondrian_core::types::ClipId;
use mondrian_timeline::clip::Clip;
use mondrian_timeline::sequence::Sequence;
use mondrian_timeline::track::Track;

/// Whether a shell-dispatched action can produce a useful editor operation for
/// the supplied application state snapshot.
pub fn app_state_action_enabled(action: &Action, state: &AppState) -> bool {
    match action {
        Action::SaveProject => state.has_open_project(),
        Action::SaveProjectAs(_) => state.sequence.is_some(),
        Action::CloseProject => state.sequence.is_some() || state.current_project_path.is_some(),
        Action::ImportMedia(_) => state.asset_library.is_some(),
        Action::Undo => state.can_undo_action(),
        Action::Redo => state.can_redo_action(),
        Action::Cut => state.can_cut_to_app_clipboard(),
        Action::Copy => state.can_copy_to_app_clipboard(),
        Action::Paste => state.can_paste_from_app_clipboard(),
        Action::DeleteSelection | Action::RippleDeleteSelection => {
            has_deletable_timeline_selection(state)
        }
        Action::Duplicate => state.can_cut_to_app_clipboard(),
        Action::SplitClipAtPlayhead => can_split_at_playhead(state),
        Action::MarkInAtPlayhead
        | Action::MarkOutAtPlayhead
        | Action::TogglePlay
        | Action::StepForward
        | Action::StepBack
        | Action::GoToStart
        | Action::GoToEnd => state.sequence.is_some(),
        Action::SelectAll => sequence_has_selectable_clips(state),
        Action::DeselectAll => has_any_app_selection(state),
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_IMPORT_MEDIA_DIALOG =>
        {
            state.asset_library.is_some()
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_SEQUENCE_SETTINGS =>
        {
            state.sequence.is_some()
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_PROJECT_SETTINGS =>
        {
            state.has_open_project()
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_SAVE_PROJECT_AS_DIALOG =>
        {
            state.sequence.is_some()
        }
        Action::Custom { namespace, name, .. }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_CLEAR_IN_OUT_POINTS =>
        {
            sequence_has_in_out_points(state)
        }
        Action::Custom { namespace, name, payload }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_SELECT_CLIP =>
        {
            parse_payload::<TimelineSelectClipPayload>(payload).is_some_and(|payload| {
                sequence_has_clip_in_track(
                    state,
                    payload.track_id,
                    payload.is_video_track,
                    payload.clip_id,
                )
            })
        }
        Action::Custom { namespace, name, payload }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_MOVE_CLIP =>
        {
            parse_payload::<TimelineMoveClipPayload>(payload)
                .is_some_and(|payload| timeline_move_clip_target_is_available(state, payload))
        }
        Action::Custom { namespace, name, payload }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_TRIM_CLIPS =>
        {
            parse_payload::<TimelineTrimClipsPayload>(payload)
                .is_some_and(|payload| timeline_trim_targets_are_available(state, &payload))
        }
        Action::Custom { namespace, name, payload }
            if namespace == TIMELINE_NAMESPACE
                && name == TIMELINE_TRIM_SELECTED_CLIPS_TO_PLAYHEAD =>
        {
            parse_payload::<TimelineTrimSelectedClipsToPlayheadPayload>(payload).is_some_and(
                |payload| selected_clip_trim_to_playhead_is_available(state, payload.edge),
            )
        }
        Action::Custom { namespace, name, .. }
            if namespace == TIMELINE_NAMESPACE
                && name == TIMELINE_ROLL_SELECTED_CUT_TO_PLAYHEAD =>
        {
            has_single_editable_selected_clip(state)
        }
        Action::Custom { namespace, name, payload }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_SET_IN_OUT_POINT =>
        {
            state.sequence.is_some()
                && parse_payload::<TimelineSetInOutPointPayload>(payload).is_some()
        }
        Action::Custom { namespace, name, payload }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_SET_SELECTED_CLIPS_ENABLED =>
        {
            parse_payload::<TimelineSetSelectedClipsEnabledPayload>(payload).is_some()
                && selected_clips_are_editable(state)
        }
        Action::Custom { namespace, name, .. }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_SEEK =>
        {
            state.sequence.is_some()
        }
        Action::Custom { namespace, name, payload }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_SET_TRACK_CONTROL =>
        {
            parse_payload::<TimelineSetTrackControlPayload>(payload).is_some_and(|payload| {
                sequence_has_track(state, payload.track_id, payload.is_video_track)
            })
        }
        Action::Custom { namespace, name, .. }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_ADD_TRACK =>
        {
            state.sequence.is_some()
        }
        Action::Custom { namespace, name, payload }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_MOVE_TRACK =>
        {
            parse_payload::<TimelineMoveTrackPayload>(payload).is_some_and(|payload| {
                sequence_has_track(state, payload.track_id, payload.is_video_track)
            })
        }
        Action::Custom { namespace, name, .. }
            if namespace == SEQUENCE_NAMESPACE && name == SEQUENCE_RETURN_TO_PARENT =>
        {
            !state.sequence_navigation_stack.is_empty()
        }
        Action::Custom { namespace, name, .. }
            if namespace == SEQUENCE_NAMESPACE && name == SEQUENCE_SET_ACTIVE_DEFAULT =>
        {
            state.active_sequence_id.is_some()
                && state.default_sequence_id != state.active_sequence_id
        }
        Action::Custom { namespace, name, .. }
            if namespace == SEQUENCE_NAMESPACE && name == SEQUENCE_DUPLICATE =>
        {
            state.active_sequence_id.is_some()
        }
        Action::Custom { namespace, name, .. }
            if namespace == SEQUENCE_NAMESPACE && name == SEQUENCE_DELETE =>
        {
            state.active_sequence_id.is_some() && state.export_sequences_snapshot().len() > 1
        }
        Action::Custom { namespace, name, .. }
            if namespace == SEQUENCE_NAMESPACE && name == SEQUENCE_SWITCH_ACTIVE =>
        {
            state.active_sequence_id.is_some()
        }
        _ => true,
    }
}

fn parse_payload<T: serde::de::DeserializeOwned>(payload: &serde_json::Value) -> Option<T> {
    serde_json::from_value(payload.clone()).ok()
}

fn has_timeline_selection(state: &AppState) -> bool {
    !state.selection.selected_clips.is_empty() || !state.selection.selected_track_ids.is_empty()
}

fn has_deletable_timeline_selection(state: &AppState) -> bool {
    if !state.selection.selected_clips.is_empty() {
        let Some(sequence) = state.sequence.as_ref() else {
            return false;
        };
        return state.selection.selected_clips.iter().all(|selection| {
            let tracks = if selection.is_video_track {
                &sequence.video_tracks
            } else {
                &sequence.audio_tracks
            };
            tracks
                .iter()
                .find(|track| track.id == selection.track_id)
                .is_some_and(|track| !track.is_locked)
        });
    }

    selected_tracks_are_deletable(state)
}

fn has_single_editable_selected_clip(state: &AppState) -> bool {
    if state.selection.selected_clips.len() != 1 {
        return false;
    }
    let Some(sequence) = state.sequence.as_ref() else {
        return false;
    };
    state.selection.selected_clips.iter().all(|selection| {
        let tracks = if selection.is_video_track {
            &sequence.video_tracks
        } else {
            &sequence.audio_tracks
        };
        tracks.iter().find(|track| track.id == selection.track_id).is_some_and(|track| {
            !track.is_locked && track.clips.iter().any(|clip| clip.id == selection.clip_id)
        })
    })
}

fn selected_clips_are_editable(state: &AppState) -> bool {
    if state.selection.selected_clips.is_empty() {
        return false;
    }
    let Some(sequence) = state.sequence.as_ref() else {
        return false;
    };
    state.selection.selected_clips.iter().all(|selection| {
        track_for_ref(sequence, selection.track_id, selection.is_video_track).is_some_and(|track| {
            !track.is_locked && track.clips.iter().any(|clip| clip.id == selection.clip_id)
        })
    })
}

fn sequence_has_clip_in_track(
    state: &AppState,
    track_id: TrackId,
    is_video_track: bool,
    clip_id: ClipId,
) -> bool {
    state.sequence.as_ref().is_some_and(|sequence| {
        track_for_ref(sequence, track_id, is_video_track)
            .is_some_and(|track| track.clips.iter().any(|clip| clip.id == clip_id))
    })
}

fn sequence_has_track(state: &AppState, track_id: TrackId, is_video_track: bool) -> bool {
    state
        .sequence
        .as_ref()
        .and_then(|sequence| track_for_ref(sequence, track_id, is_video_track))
        .is_some()
}

fn timeline_move_clip_target_is_available(
    state: &AppState,
    payload: TimelineMoveClipPayload,
) -> bool {
    let Some(sequence) = state.sequence.as_ref() else {
        return false;
    };
    let source_unlocked = sequence
        .video_tracks
        .iter()
        .chain(sequence.audio_tracks.iter())
        .find(|track| track.clips.iter().any(|clip| clip.id == payload.clip_id))
        .is_some_and(|track| !track.is_locked);
    let target_unlocked = track_for_ref(sequence, payload.target_track_id, payload.is_video_track)
        .is_some_and(|track| !track.is_locked);
    source_unlocked && target_unlocked
}

fn timeline_trim_targets_are_available(
    state: &AppState,
    payload: &TimelineTrimClipsPayload,
) -> bool {
    if payload.clip_ids.is_empty() {
        return false;
    }
    let Some(sequence) = state.sequence.as_ref() else {
        return false;
    };
    payload.clip_ids.iter().all(|clip_id| {
        clip_with_track(sequence, *clip_id).is_some_and(|(track, clip)| {
            !track.is_locked
                && clip_can_trim_to_frame(clip, payload.edge, payload.frame, sequence.time_base())
        })
    })
}

fn selected_clip_trim_to_playhead_is_available(
    state: &AppState,
    edge: TimelineTrimPayloadEdge,
) -> bool {
    if state.selection.selected_clips.is_empty() {
        return false;
    }
    let target_frame = match edge {
        TimelineTrimPayloadEdge::In => state.current_frame(),
        TimelineTrimPayloadEdge::Out => state.current_frame().saturating_add(1),
    };
    let Some(sequence) = state.sequence.as_ref() else {
        return false;
    };
    state.selection.selected_clips.iter().all(|selection| {
        track_for_ref(sequence, selection.track_id, selection.is_video_track).is_some_and(|track| {
            !track.is_locked
                && track.clips.iter().find(|clip| clip.id == selection.clip_id).is_some_and(
                    |clip| clip_can_trim_to_frame(clip, edge, target_frame, sequence.time_base()),
                )
        })
    })
}

fn track_for_ref(sequence: &Sequence, track_id: TrackId, is_video_track: bool) -> Option<&Track> {
    if is_video_track {
        sequence.video_tracks.iter().find(|track| track.id == track_id)
    } else {
        sequence.audio_tracks.iter().find(|track| track.id == track_id)
    }
}

fn clip_with_track(sequence: &Sequence, clip_id: ClipId) -> Option<(&Track, &Clip)> {
    sequence
        .video_tracks
        .iter()
        .chain(sequence.audio_tracks.iter())
        .find_map(|track| {
            track.clips.iter().find(|clip| clip.id == clip_id).map(|clip| (track, clip))
        })
}

fn clip_can_trim_to_frame(
    clip: &Clip,
    edge: TimelineTrimPayloadEdge,
    target_frame: i64,
    time_base: Rational,
) -> bool {
    let Ok(target) = TimelineTime::from_frame_position(FramePosition::new(target_frame, time_base))
    else {
        return false;
    };
    let Ok(end) = clip.end_position() else {
        return false;
    };
    match edge {
        TimelineTrimPayloadEdge::In => target > clip.position && target < end,
        TimelineTrimPayloadEdge::Out => target > clip.position && target < end,
    }
}

fn selected_tracks_are_deletable(state: &AppState) -> bool {
    if state.selection.selected_track_ids.is_empty() {
        return false;
    }
    let Some(sequence) = state.sequence.as_ref() else {
        return false;
    };

    let mut targets = Vec::<TrackId>::new();
    let mut video_targets = 0usize;
    let mut audio_targets = 0usize;
    for track_id in &state.selection.selected_track_ids {
        if targets.contains(track_id) {
            continue;
        }
        targets.push(*track_id);

        if sequence.video_tracks.iter().any(|track| track.id == *track_id) {
            video_targets += 1;
        } else if sequence.audio_tracks.iter().any(|track| track.id == *track_id) {
            audio_targets += 1;
        } else {
            return false;
        }
    }

    sequence.video_tracks.len().saturating_sub(video_targets) >= 1
        && sequence.audio_tracks.len().saturating_sub(audio_targets) >= 1
}

fn has_any_app_selection(state: &AppState) -> bool {
    has_timeline_selection(state)
        || state.selection.selected_effect.is_some()
        || state.selection.selected_mask.is_some()
        || state.animation_selection.active_property.is_some()
        || !state.animation_selection.selected_keyframes.is_empty()
}

fn sequence_has_in_out_points(state: &AppState) -> bool {
    state
        .sequence
        .as_ref()
        .is_some_and(|sequence| sequence.in_point.is_some() || sequence.out_point.is_some())
}

fn sequence_has_selectable_clips(state: &AppState) -> bool {
    state.sequence.as_ref().is_some_and(|sequence| {
        sequence
            .video_tracks
            .iter()
            .chain(sequence.audio_tracks.iter())
            .any(|track| !track.clips.is_empty())
    })
}

fn can_split_at_playhead(state: &AppState) -> bool {
    state.sequence.as_ref().is_some_and(|sequence| {
        let Ok(time) = TimelineTime::from_frame_position(FramePosition::new(
            state.current_frame(),
            sequence.time_base(),
        )) else {
            return false;
        };
        sequence
            .video_tracks
            .iter()
            .filter(|track| !track.is_locked)
            .chain(sequence.audio_tracks.iter().filter(|track| !track.is_locked))
            .flat_map(|track| track.clips.iter())
            .any(|clip| clip.end_position().is_ok_and(|end| time > clip.position && time < end))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tt(frame: i64, time_base: mondrian_core::Rational) -> mondrian_core::TimelineTime {
        let numerator = frame.checked_mul(time_base.num).expect("test time fits i64");
        mondrian_core::TimelineTime::new(numerator, time_base.den).expect("valid test time")
    }
    use crate::app::ui_actions::{
        app_shell_project_settings_action, timeline_add_track_action,
        timeline_clear_in_out_points_action, timeline_move_clip_action, timeline_move_track_action,
        timeline_roll_selected_cut_to_playhead_action, timeline_seek_action,
        timeline_select_clip_action, timeline_set_in_out_point_action,
        timeline_set_selected_clips_enabled_action, timeline_set_track_control_action,
        timeline_trim_clips_action, timeline_trim_selected_clips_to_playhead_action,
        TimelineAddTrackKind, TimelineAddTrackPayload, TimelineInOutPointPayloadKind,
        TimelineMoveClipPayload, TimelineMoveTrackPayload, TimelineSelectClipPayload,
        TimelineSetInOutPointPayload, TimelineSetSelectedClipsEnabledPayload,
        TimelineSetTrackControlPayload, TimelineTrackControlPayloadKind, TimelineTrimClipsPayload,
        TimelineTrimPayloadEdge,
    };
    use crate::app::SelectedClipRef;
    use mondrian_core::types::{AssetId, ClipId, TrackId};
    use mondrian_timeline::clip::Clip;
    use mondrian_timeline::sequence::Sequence;

    fn state_with_selected_clip() -> AppState {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("Edit");
        let tb = sequence.time_base();
        let track_id = sequence.video_tracks[0].id;
        let clip = Clip::new(AssetId::new(), tt(10, tb), tt(20, tb)).expect("valid clip");
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("add clip");
        state.sequence = Some(sequence);
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];
        state
    }

    #[test]
    fn project_settings_requires_a_saved_open_project() {
        let action = app_shell_project_settings_action();
        let mut state = AppState::new();

        assert!(!app_state_action_enabled(&action, &state));
        state.sequence = Some(Sequence::new("Edit"));
        assert!(!app_state_action_enabled(&action, &state));
        state.current_project_path = Some(std::path::PathBuf::from("project.mdp"));
        assert!(app_state_action_enabled(&action, &state));
    }

    #[test]
    fn app_state_action_gate_disables_timeline_editing_without_targets() {
        let state = AppState::new();
        let clear_in_out_action = timeline_clear_in_out_points_action();
        let roll_cut_action = timeline_roll_selected_cut_to_playhead_action();

        for action in [
            Action::DeleteSelection,
            Action::RippleDeleteSelection,
            Action::Duplicate,
            Action::SplitClipAtPlayhead,
            Action::MarkInAtPlayhead,
            Action::MarkOutAtPlayhead,
            Action::TogglePlay,
            Action::StepBack,
            Action::StepForward,
            Action::GoToStart,
            Action::GoToEnd,
            Action::SelectAll,
            Action::DeselectAll,
        ]
        .iter()
        .chain([&clear_in_out_action, &roll_cut_action])
        {
            assert!(!app_state_action_enabled(action, &state), "{action:?}");
        }
    }

    #[test]
    fn app_state_action_gate_enables_timeline_editing_with_valid_targets() {
        let mut state = state_with_selected_clip();
        state.seek(15);

        for action in [
            Action::DeleteSelection,
            Action::RippleDeleteSelection,
            Action::Duplicate,
            Action::SplitClipAtPlayhead,
            Action::MarkInAtPlayhead,
            Action::MarkOutAtPlayhead,
            Action::TogglePlay,
            Action::StepBack,
            Action::StepForward,
            Action::GoToStart,
            Action::GoToEnd,
            Action::SelectAll,
            Action::DeselectAll,
        ] {
            assert!(app_state_action_enabled(&action, &state), "{action:?}");
        }
        assert!(app_state_action_enabled(
            &timeline_roll_selected_cut_to_playhead_action(),
            &state
        ));
    }

    #[test]
    fn app_state_action_gate_enables_typed_timeline_actions_with_valid_targets() {
        let mut state = state_with_selected_clip();
        state.seek(15);
        let selection = state.selection.selected_clips[0];
        let clip_id = selection.clip_id;
        let track_id = selection.track_id;

        for action in [
            timeline_select_clip_action(TimelineSelectClipPayload {
                track_id,
                is_video_track: true,
                clip_id,
            }),
            timeline_move_clip_action(TimelineMoveClipPayload {
                target_track_id: track_id,
                is_video_track: true,
                clip_id,
                frame: 12,
            }),
            timeline_trim_clips_action(TimelineTrimClipsPayload {
                clip_ids: vec![clip_id],
                edge: TimelineTrimPayloadEdge::In,
                frame: 15,
            }),
            timeline_trim_selected_clips_to_playhead_action(
                TimelineTrimSelectedClipsToPlayheadPayload { edge: TimelineTrimPayloadEdge::In },
            ),
            timeline_set_in_out_point_action(TimelineSetInOutPointPayload {
                point: TimelineInOutPointPayloadKind::In,
                frame: 15,
            }),
            timeline_set_selected_clips_enabled_action(TimelineSetSelectedClipsEnabledPayload {
                enabled: false,
            }),
            timeline_seek_action(42),
            timeline_set_track_control_action(TimelineSetTrackControlPayload {
                track_id,
                is_video_track: true,
                control: TimelineTrackControlPayloadKind::Lock,
                enabled: true,
            }),
            timeline_add_track_action(TimelineAddTrackPayload {
                kind: TimelineAddTrackKind::Video,
            }),
            timeline_move_track_action(TimelineMoveTrackPayload {
                track_id,
                is_video_track: true,
                target_index: 0,
            }),
        ] {
            assert!(app_state_action_enabled(&action, &state), "{action:?}");
        }
    }

    #[test]
    fn app_state_action_gate_disables_typed_timeline_actions_with_stale_targets() {
        let mut state = state_with_selected_clip();
        state.seek(15);
        let stale_clip = ClipId::new();
        let stale_track = TrackId::new();

        for action in [
            timeline_select_clip_action(TimelineSelectClipPayload {
                track_id: stale_track,
                is_video_track: true,
                clip_id: stale_clip,
            }),
            timeline_move_clip_action(TimelineMoveClipPayload {
                target_track_id: stale_track,
                is_video_track: true,
                clip_id: stale_clip,
                frame: 12,
            }),
            timeline_trim_clips_action(TimelineTrimClipsPayload {
                clip_ids: vec![stale_clip],
                edge: TimelineTrimPayloadEdge::In,
                frame: 15,
            }),
            timeline_set_track_control_action(TimelineSetTrackControlPayload {
                track_id: stale_track,
                is_video_track: true,
                control: TimelineTrackControlPayloadKind::Visibility,
                enabled: false,
            }),
            timeline_move_track_action(TimelineMoveTrackPayload {
                track_id: stale_track,
                is_video_track: true,
                target_index: 0,
            }),
        ] {
            assert!(!app_state_action_enabled(&action, &state), "{action:?}");
        }
    }

    #[test]
    fn app_state_action_gate_disables_typed_clip_edits_on_locked_tracks() {
        let mut state = state_with_selected_clip();
        state.seek(15);
        let selection = state.selection.selected_clips[0];
        state.sequence.as_mut().expect("sequence").video_tracks[0].is_locked = true;

        for action in [
            timeline_move_clip_action(TimelineMoveClipPayload {
                target_track_id: selection.track_id,
                is_video_track: true,
                clip_id: selection.clip_id,
                frame: 12,
            }),
            timeline_trim_clips_action(TimelineTrimClipsPayload {
                clip_ids: vec![selection.clip_id],
                edge: TimelineTrimPayloadEdge::In,
                frame: 15,
            }),
            timeline_trim_selected_clips_to_playhead_action(
                TimelineTrimSelectedClipsToPlayheadPayload { edge: TimelineTrimPayloadEdge::In },
            ),
            timeline_set_selected_clips_enabled_action(TimelineSetSelectedClipsEnabledPayload {
                enabled: false,
            }),
        ] {
            assert!(!app_state_action_enabled(&action, &state), "{action:?}");
        }
    }

    #[test]
    fn app_state_action_gate_disables_split_outside_unlocked_clip_body() {
        let mut state = state_with_selected_clip();
        state.seek(10);
        assert!(!app_state_action_enabled(
            &Action::SplitClipAtPlayhead,
            &state
        ));

        state.seek(15);
        assert!(app_state_action_enabled(
            &Action::SplitClipAtPlayhead,
            &state
        ));

        state.sequence.as_mut().expect("sequence").video_tracks[0].is_locked = true;
        assert!(!app_state_action_enabled(
            &Action::SplitClipAtPlayhead,
            &state
        ));
    }

    #[test]
    fn app_state_action_gate_uses_edge_specific_trim_to_playhead_validity() {
        let mut state = state_with_selected_clip();
        let trim_in = timeline_trim_selected_clips_to_playhead_action(
            TimelineTrimSelectedClipsToPlayheadPayload { edge: TimelineTrimPayloadEdge::In },
        );
        let trim_out = timeline_trim_selected_clips_to_playhead_action(
            TimelineTrimSelectedClipsToPlayheadPayload { edge: TimelineTrimPayloadEdge::Out },
        );

        state.seek(10);
        assert!(!app_state_action_enabled(&trim_in, &state));
        assert!(app_state_action_enabled(&trim_out, &state));

        state.seek(29);
        assert!(app_state_action_enabled(&trim_in, &state));
        assert!(!app_state_action_enabled(&trim_out, &state));
    }
}
