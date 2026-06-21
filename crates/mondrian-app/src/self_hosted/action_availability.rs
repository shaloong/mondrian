//! Shared self-hosted action availability gates.
//!
//! Menus, focused shortcuts, and panel adapters use this module to keep their
//! disabled states aligned before actions reach `AppState`.

use mondrian_core::types::TrackId;
use mondrian_editor_state::Action;

use crate::app::ui_actions::{
    APP_SHELL_IMPORT_MEDIA_DIALOG, APP_SHELL_NAMESPACE, APP_SHELL_SAVE_PROJECT_AS_DIALOG,
    APP_SHELL_SEQUENCE_SETTINGS, SEQUENCE_DELETE, SEQUENCE_DUPLICATE, SEQUENCE_NAMESPACE,
    SEQUENCE_RETURN_TO_PARENT, SEQUENCE_SET_ACTIVE_DEFAULT, SEQUENCE_SWITCH_ACTIVE,
    TIMELINE_CLEAR_IN_OUT_POINTS, TIMELINE_NAMESPACE,
};
use crate::app::AppState;

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
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_SAVE_PROJECT_AS_DIALOG =>
        {
            state.sequence.is_some()
        }
        Action::Custom { namespace, name, .. }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_CLEAR_IN_OUT_POINTS =>
        {
            sequence_has_in_out_points(state)
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
    state.sequence.as_ref().is_some_and(|sequence| {
        sequence.in_point_frame.is_some() || sequence.out_point_frame().is_some()
    })
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
    let frame = state.current_frame();
    state.sequence.as_ref().is_some_and(|sequence| {
        sequence
            .video_tracks
            .iter()
            .filter(|track| !track.is_locked)
            .chain(sequence.audio_tracks.iter().filter(|track| !track.is_locked))
            .flat_map(|track| track.clips.iter())
            .any(|clip| frame > clip.position.frame && frame < clip.end_position().frame)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ui_actions::timeline_clear_in_out_points_action;
    use crate::app::SelectedClipRef;
    use mondrian_core::types::{AssetId, TimeCode};
    use mondrian_timeline::clip::Clip;
    use mondrian_timeline::sequence::Sequence;

    fn state_with_selected_clip() -> AppState {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("Edit");
        let tb = sequence.time_base();
        let track_id = sequence.video_tracks[0].id;
        let clip = Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(20, tb));
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("add clip");
        state.sequence = Some(sequence);
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];
        state
    }

    #[test]
    fn app_state_action_gate_disables_timeline_editing_without_targets() {
        let state = AppState::new();
        let clear_in_out_action = timeline_clear_in_out_points_action();

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
        .chain(std::iter::once(&clear_in_out_action))
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
}
