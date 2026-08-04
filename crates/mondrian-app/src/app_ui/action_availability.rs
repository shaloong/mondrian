//! Shared app UI action availability gates.
//!
//! Menus, focused shortcuts, and panel adapters use this module to keep their
//! disabled states aligned before actions reach `AppState`.

use mondrian_core::types::{FramePosition, Rational, TrackId};
use mondrian_core::TimelineTime;
use mondrian_editor_state::Action;

use crate::app::product_action::ProductAction;
use crate::app::ui_actions::{
    TimelineInsertAssetPayload, TimelineMoveTrackPayload, TimelineSetInOutPointPayload,
    TimelineSetSelectedClipsEnabledPayload, TimelineSetTrackControlPayload,
    TimelineSetTrackTargetingPayload, TimelineTrimPayloadEdge,
    TimelineTrimSelectedClipsToPlayheadPayload, APP_SHELL_IMPORT_MEDIA_DIALOG, APP_SHELL_NAMESPACE,
    APP_SHELL_PROJECT_SETTINGS, APP_SHELL_QUIT, APP_SHELL_SAVE_PROJECT_AS_DIALOG,
    APP_SHELL_SEQUENCE_SETTINGS, TIMELINE_ADD_TRACK, TIMELINE_CLEAR_IN_OUT_POINTS,
    TIMELINE_CREATE_BASIC_TITLE, TIMELINE_EXTRACT_RANGE, TIMELINE_INSERT_ASSET,
    TIMELINE_LIFT_RANGE, TIMELINE_LINK_SELECTED_CLIPS, TIMELINE_MOVE_TRACK, TIMELINE_NAMESPACE,
    TIMELINE_ROLL_SELECTED_CUT_TO_PLAYHEAD, TIMELINE_SET_IN_OUT_POINT,
    TIMELINE_SET_SELECTED_CLIPS_ENABLED, TIMELINE_SET_TRACK_CONTROL, TIMELINE_SET_TRACK_TARGETING,
    TIMELINE_TRIM_SELECTED_CLIPS_TO_PLAYHEAD, TIMELINE_UNLINK_SELECTED_CLIPS,
};
use crate::app::AppState;
use mondrian_core::types::ClipId;
use mondrian_timeline::clip::Clip;
use mondrian_timeline::sequence::Sequence;
use mondrian_timeline::track::Track;
use mondrian_timeline::{assess_clip_link_edit, ClipLinkEditKind, ClipLinkEditRequest};

/// Whether a shell-dispatched action can produce a useful editor operation for
/// the supplied application state snapshot.
pub fn app_state_action_enabled(action: &Action, state: &AppState) -> bool {
    if state.project_close_blocks_actions() {
        return matches!(action, Action::CloseProject)
            || matches!(
                action,
                Action::Custom { namespace, name, .. }
                    if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_QUIT
            );
    }
    match ProductAction::decode_external(action) {
        Ok(Some(action)) => {
            return state.product_action_availability().allows(&action);
        }
        Err(_) => return false,
        Ok(None) => {}
    }

    match action {
        Action::OpenProject(path) => !path.as_os_str().is_empty(),
        Action::SaveProject => state.has_open_project(),
        Action::SaveProjectAs(path) => !path.as_os_str().is_empty() && state.has_open_project(),
        Action::CloseProject => {
            state.active_sequence().is_some() || state.current_project_path().is_some()
        }
        Action::ImportMedia(paths) => {
            state.asset_library().is_some()
                && !paths.is_empty()
                && paths.iter().all(|path| !path.as_os_str().is_empty())
        }
        Action::Undo => state.can_undo_action(),
        Action::Redo => state.can_redo_action(),
        Action::Cut => state.can_cut_to_app_clipboard(),
        Action::Copy => state.can_copy_to_app_clipboard(),
        Action::Paste => state.can_paste_from_app_clipboard(),
        Action::DeleteSelection => has_deletable_timeline_selection(state),
        Action::RippleDeleteSelection => {
            state.selection.selected_video_transition.is_none()
                && has_deletable_clip_or_track_selection(state)
        }
        Action::Duplicate => state.can_cut_to_app_clipboard(),
        Action::NudgeClip { clip_id, delta_frames } => {
            *delta_frames != 0 && clip_is_editable(state, *clip_id)
        }
        Action::Play => state.active_sequence().is_some() && !state.is_playing(),
        Action::Pause => state.active_sequence().is_some() && state.is_playing(),
        Action::Seek(position) => valid_transport_seek(state, *position),
        Action::SplitClipAtPlayhead => can_split_at_playhead(state),
        Action::MarkInAtPlayhead
        | Action::MarkOutAtPlayhead
        | Action::TogglePlay
        | Action::StepForward
        | Action::StepBack
        | Action::GoToStart
        | Action::GoToEnd => state.active_sequence().is_some(),
        Action::SelectAll => sequence_has_selectable_clips(state),
        Action::DeselectAll => has_any_app_selection(state),
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_IMPORT_MEDIA_DIALOG =>
        {
            state.asset_library().is_some()
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_SEQUENCE_SETTINGS =>
        {
            state.active_sequence().is_some()
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_PROJECT_SETTINGS =>
        {
            state.has_open_project()
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_SAVE_PROJECT_AS_DIALOG =>
        {
            state.active_sequence().is_some()
        }
        Action::Custom { namespace, name, .. }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_CLEAR_IN_OUT_POINTS =>
        {
            sequence_has_in_out_points(state)
        }
        Action::Custom { namespace, name, .. }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_LIFT_RANGE =>
        {
            state.can_apply_timeline_range_edit(mondrian_timeline::RangeEditKind::Lift)
        }
        Action::Custom { namespace, name, .. }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_EXTRACT_RANGE =>
        {
            state.can_apply_timeline_range_edit(mondrian_timeline::RangeEditKind::Extract)
        }
        Action::Custom { namespace, name, .. }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_LINK_SELECTED_CLIPS =>
        {
            clip_link_edit_available(state, ClipLinkEditKind::Link)
        }
        Action::Custom { namespace, name, .. }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_UNLINK_SELECTED_CLIPS =>
        {
            clip_link_edit_available(state, ClipLinkEditKind::Unlink)
        }
        Action::Custom { namespace, name, .. }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_CREATE_BASIC_TITLE =>
        {
            state.active_sequence().is_some()
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
            state.active_sequence().is_some()
                && parse_payload::<TimelineSetInOutPointPayload>(payload).is_some()
        }
        Action::Custom { namespace, name, payload }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_SET_SELECTED_CLIPS_ENABLED =>
        {
            parse_payload::<TimelineSetSelectedClipsEnabledPayload>(payload).is_some()
                && selected_clips_are_editable(state)
        }
        Action::Custom { namespace, name, payload }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_SET_TRACK_CONTROL =>
        {
            parse_payload::<TimelineSetTrackControlPayload>(payload).is_some_and(|payload| {
                sequence_has_track(state, payload.track_id, payload.is_video_track)
            })
        }
        Action::Custom { namespace, name, payload }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_SET_TRACK_TARGETING =>
        {
            parse_payload::<TimelineSetTrackTargetingPayload>(payload)
                .is_some_and(|payload| sequence_has_any_track(state, payload.track_id))
        }
        Action::Custom { namespace, name, .. }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_ADD_TRACK =>
        {
            state.active_sequence().is_some()
        }
        Action::Custom { namespace, name, payload }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_MOVE_TRACK =>
        {
            parse_payload::<TimelineMoveTrackPayload>(payload).is_some_and(|payload| {
                sequence_has_track(state, payload.track_id, payload.is_video_track)
            })
        }
        Action::Custom { namespace, name, payload }
            if namespace == TIMELINE_NAMESPACE && name == TIMELINE_INSERT_ASSET =>
        {
            parse_payload::<TimelineInsertAssetPayload>(payload)
                .is_some_and(|payload| timeline_insert_scope_is_available(state, &payload))
        }
        _ => true,
    }
}

fn valid_transport_seek(state: &AppState, position: FramePosition) -> bool {
    let Some(sequence) = state.active_sequence() else {
        return false;
    };
    TimelineTime::from_frame_position(position)
        .ok()
        .filter(|time| !time.is_negative())
        .and_then(|time| {
            time.to_frame_position(
                sequence.settings.frame_rate,
                mondrian_core::FrameRounding::Nearest,
            )
            .ok()
        })
        .is_some_and(|position| position.frame >= 0)
}

fn timeline_insert_scope_is_available(
    state: &AppState,
    payload: &TimelineInsertAssetPayload,
) -> bool {
    if payload.insert_frame < 0
        || payload.source_in_frame < 0
        || payload.duration_frames <= 0
        || payload.ripple_track_ids.is_empty()
    {
        return false;
    }
    let Some(sequence) = state.active_sequence() else {
        return false;
    };
    let ripple_tracks = payload
        .ripple_track_ids
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let targets = [payload.video_target_track_id, payload.audio_target_track_id]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if targets.is_empty() || targets.iter().any(|track_id| !ripple_tracks.contains(track_id)) {
        return false;
    }
    ripple_tracks.iter().all(|track_id| {
        sequence
            .video_tracks
            .iter()
            .chain(&sequence.audio_tracks)
            .find(|track| track.id == *track_id)
            .is_some_and(|track| !track.is_locked)
    })
}

fn parse_payload<T: serde::de::DeserializeOwned>(payload: &serde_json::Value) -> Option<T> {
    serde_json::from_value(payload.clone()).ok()
}

fn has_timeline_selection(state: &AppState) -> bool {
    !state.selection.selected_clips.is_empty()
        || !state.selection.selected_track_ids.is_empty()
        || state.selection.selected_video_transition.is_some()
}

fn has_deletable_timeline_selection(state: &AppState) -> bool {
    if let Some(selection) = state.selection.selected_video_transition {
        return transition_track_lock(state, selection.transition_id) == Some(false);
    }

    has_deletable_clip_or_track_selection(state)
}

fn has_deletable_clip_or_track_selection(state: &AppState) -> bool {
    if !state.selection.selected_clips.is_empty() {
        let Some(sequence) = state.active_sequence() else {
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

fn transition_track_lock(
    state: &AppState,
    transition_id: mondrian_core::VideoTransitionId,
) -> Option<bool> {
    let sequence = state.active_sequence()?;
    let transition = sequence
        .video_transitions
        .iter()
        .find(|transition| transition.id == transition_id)?;
    sequence.video_tracks.iter().find_map(|track| {
        let has_left = track.clips.iter().any(|clip| clip.id == transition.left);
        let has_right = track.clips.iter().any(|clip| clip.id == transition.right);
        (has_left && has_right).then_some(track.is_locked)
    })
}

fn has_single_editable_selected_clip(state: &AppState) -> bool {
    if state.selection.selected_clips.len() != 1 {
        return false;
    }
    let Some(sequence) = state.active_sequence() else {
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
    let Some(sequence) = state.active_sequence() else {
        return false;
    };
    state.selection.selected_clips.iter().all(|selection| {
        track_for_ref(sequence, selection.track_id, selection.is_video_track).is_some_and(|track| {
            !track.is_locked && track.clips.iter().any(|clip| clip.id == selection.clip_id)
        })
    })
}

fn clip_is_editable(state: &AppState, clip_id: ClipId) -> bool {
    state.active_sequence().is_some_and(|sequence| {
        sequence
            .video_tracks
            .iter()
            .chain(&sequence.audio_tracks)
            .any(|track| !track.is_locked && track.clips.iter().any(|clip| clip.id == clip_id))
    })
}

fn clip_link_edit_available(state: &AppState, kind: ClipLinkEditKind) -> bool {
    let Some(sequence) = state.active_sequence() else {
        return false;
    };
    let request = ClipLinkEditRequest::new(
        kind,
        state.selection.selected_clips.iter().map(|selection| selection.clip_id),
    );
    assess_clip_link_edit(sequence, &request).is_ok_and(|assessment| assessment.would_change)
}

fn sequence_has_track(state: &AppState, track_id: TrackId, is_video_track: bool) -> bool {
    state
        .active_sequence()
        .and_then(|sequence| track_for_ref(sequence, track_id, is_video_track))
        .is_some()
}

fn sequence_has_any_track(state: &AppState, track_id: TrackId) -> bool {
    state.active_sequence().is_some_and(|sequence| {
        sequence
            .video_tracks
            .iter()
            .chain(&sequence.audio_tracks)
            .any(|track| track.id == track_id)
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
    let Some(sequence) = state.active_sequence() else {
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
    let Some(sequence) = state.active_sequence() else {
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
        .active_sequence()
        .is_some_and(|sequence| sequence.in_point.is_some() || sequence.out_point.is_some())
}

fn sequence_has_selectable_clips(state: &AppState) -> bool {
    state.active_sequence().is_some_and(|sequence| {
        sequence
            .video_tracks
            .iter()
            .chain(sequence.audio_tracks.iter())
            .any(|track| !track.clips.is_empty())
    })
}

fn can_split_at_playhead(state: &AppState) -> bool {
    state.active_sequence().is_some_and(|sequence| {
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
        app_shell_project_settings_action, assets_delete_selection_action,
        assets_import_files_action, assets_move_selection_action,
        clip_write_parameter_values_action, export_cancel_action,
        export_clear_terminal_history_action, export_edit_draft_action, export_enqueue_action,
        project_create_with_settings_action, sequence_new_action, sequence_switch_active_action,
        sequence_update_settings_action, timeline_add_track_action,
        timeline_clear_in_out_points_action, timeline_extract_range_action,
        timeline_lift_range_action, timeline_move_clip_action, timeline_move_track_action,
        timeline_roll_selected_cut_to_playhead_action, timeline_seek_action,
        timeline_select_clip_action, timeline_set_in_out_point_action,
        timeline_set_selected_clips_enabled_action, timeline_set_track_control_action,
        timeline_set_track_targeting_action, timeline_trim_clips_action,
        timeline_trim_selected_clips_to_playhead_action,
        viewer_set_preview_resolution_scale_action, AssetsDeleteSelectionPayload,
        AssetsImportFilesPayload, AssetsMoveSelectionPayload, ClipParameterValueWrite,
        ClipWriteParameterValuesPayload, ExportDraftEdit, ProjectCreateWithSettingsPayload,
        SequenceTargetPayload, SequenceUpdateSettingsPayload, TimelineAddTrackKind,
        TimelineAddTrackPayload, TimelineExportRequest, TimelineInOutPointPayloadKind,
        TimelineMoveClipPayload, TimelineMoveTrackPayload, TimelineSelectClipPayload,
        TimelineSetInOutPointPayload, TimelineSetSelectedClipsEnabledPayload,
        TimelineSetTrackControlPayload, TimelineSetTrackTargetingPayload,
        TimelineTrackControlPayloadKind, TimelineTrackTargetingControl, TimelineTrimClipsPayload,
        TimelineTrimPayloadEdge, ViewerSetPreviewResolutionScalePayload,
    };
    use crate::app::SelectedClipRef;
    use mondrian_core::automation::PropertyValue;
    use mondrian_core::types::{AssetId, ClipId, TrackId};
    use mondrian_timeline::clip::{Clip, Transform2D};
    use mondrian_timeline::sequence::Sequence;

    fn state_with_selected_clip() -> AppState {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("Edit");
        let tb = sequence.time_base();
        let track_id = sequence.video_tracks[0].id;
        let clip = Clip::new(AssetId::new(), tt(10, tb), tt(20, tb)).expect("valid clip");
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("add clip");
        state.test_set_sequence(Some(sequence));
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];
        state
    }

    #[test]
    fn project_settings_requires_an_open_authoring_session() {
        let action = app_shell_project_settings_action();
        let mut state = AppState::new();

        assert!(!app_state_action_enabled(&action, &state));
        state.test_set_sequence(Some(Sequence::new("Edit")));
        assert!(app_state_action_enabled(&action, &state));
        state.test_set_project_path(std::path::PathBuf::from("project.mdp"));
        assert!(app_state_action_enabled(&action, &state));
    }

    #[test]
    fn app_state_action_gate_disables_timeline_editing_without_targets() {
        let state = AppState::new();
        let clear_in_out_action = timeline_clear_in_out_points_action();
        let roll_cut_action = timeline_roll_selected_cut_to_playhead_action();
        let trim_action = timeline_trim_selected_clips_to_playhead_action(
            TimelineTrimSelectedClipsToPlayheadPayload { edge: TimelineTrimPayloadEdge::In },
        );
        let enable_action =
            timeline_set_selected_clips_enabled_action(TimelineSetSelectedClipsEnabledPayload {
                enabled: false,
            });

        for action in [
            Action::Cut,
            Action::Copy,
            Action::Paste,
            Action::DeleteSelection,
            Action::RippleDeleteSelection,
            Action::Duplicate,
            Action::SplitClipAtPlayhead,
            Action::MarkInAtPlayhead,
            Action::MarkOutAtPlayhead,
            Action::Play,
            Action::Pause,
            Action::Seek(FramePosition::new(0, Rational::new(1, 25))),
            Action::TogglePlay,
            Action::StepBack,
            Action::StepForward,
            Action::GoToStart,
            Action::GoToEnd,
            Action::SelectAll,
            Action::DeselectAll,
        ]
        .iter()
        .chain([
            &clear_in_out_action,
            &roll_cut_action,
            &trim_action,
            &enable_action,
        ]) {
            assert!(!app_state_action_enabled(action, &state), "{action:?}");
        }
    }

    #[test]
    fn app_state_action_gate_rejects_structurally_empty_intents() {
        let mut state = state_with_selected_clip();
        let clip_id = state.selection.selected_clips[0].clip_id;
        let library_root = std::env::temp_dir().join(format!(
            "mondrian-action-availability-{}",
            mondrian_core::AssetId::new()
        ));
        state.test_set_asset_library(Some(
            mondrian_assets::AssetLibrary::open(library_root.clone()).expect("asset library"),
        ));

        let invalid_actions = [
            Action::OpenProject(std::path::PathBuf::new()),
            Action::SaveProjectAs(std::path::PathBuf::new()),
            Action::ImportMedia(Vec::new()),
            Action::ImportMedia(vec![std::path::PathBuf::new()]),
            Action::NudgeClip { clip_id, delta_frames: 0 },
            assets_import_files_action(AssetsImportFilesPayload {
                paths: Vec::new(),
                folder_id: None,
            }),
            assets_delete_selection_action(AssetsDeleteSelectionPayload {
                asset_ids: Vec::new(),
                folder_ids: Vec::new(),
            }),
            assets_move_selection_action(AssetsMoveSelectionPayload {
                asset_ids: Vec::new(),
                folder_ids: Vec::new(),
                target_folder_id: None,
            }),
            project_create_with_settings_action(ProjectCreateWithSettingsPayload {
                project_file: std::path::PathBuf::new(),
                name: "Invalid".into(),
                sequence_settings: mondrian_timeline::sequence::SequenceSettings::default(),
                color_environment: mondrian_core::ProjectColorEnvironment::default(),
                project_settings: mondrian_core::ProjectSettings::default(),
            }),
        ];

        for action in invalid_actions {
            assert!(!app_state_action_enabled(&action, &state), "{action:?}");
        }
        assert!(app_state_action_enabled(
            &Action::NudgeClip { clip_id, delta_frames: 1 },
            &state
        ));
        assert!(app_state_action_enabled(
            &Action::ImportMedia(vec![std::path::PathBuf::from("clip.mov")]),
            &state
        ));

        drop(state);
        let _ = std::fs::remove_dir_all(library_root);
    }

    #[test]
    fn app_state_action_gate_enables_timeline_editing_with_valid_targets() {
        let mut state = state_with_selected_clip();
        state.seek(15).expect("seek");

        for action in [
            Action::DeleteSelection,
            Action::RippleDeleteSelection,
            Action::Duplicate,
            Action::SplitClipAtPlayhead,
            Action::MarkInAtPlayhead,
            Action::MarkOutAtPlayhead,
            Action::Play,
            Action::Seek(FramePosition::new(15, Rational::new(1, 25))),
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
        state.seek(15).expect("seek");
        let selection = state.selection.selected_clips[0];
        let clip_id = selection.clip_id;
        let track_id = selection.track_id;

        for action in [
            timeline_select_clip_action(TimelineSelectClipPayload {
                clip_id,
                mode: crate::app::ui_actions::TimelineClipSelectionModePayload::Replace,
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
            timeline_set_track_targeting_action(TimelineSetTrackTargetingPayload {
                track_id,
                control: TimelineTrackTargetingControl::Target,
                enabled: false,
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
        state.seek(15).expect("seek");
        let stale_clip = ClipId::new();
        let stale_track = TrackId::new();

        for action in [
            timeline_select_clip_action(TimelineSelectClipPayload {
                clip_id: stale_clip,
                mode: crate::app::ui_actions::TimelineClipSelectionModePayload::Replace,
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
            timeline_set_track_targeting_action(TimelineSetTrackTargetingPayload {
                track_id: stale_track,
                control: TimelineTrackTargetingControl::SyncLock,
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
    fn app_state_action_gate_disables_malformed_recognized_product_action() {
        let state = state_with_selected_clip();
        let action = Action::Custom {
            namespace: crate::app::product_action::TIMELINE_NAMESPACE.to_owned(),
            name: crate::app::product_action::TIMELINE_TRIM_CLIPS.to_owned(),
            payload: serde_json::json!({"clip_ids": []}),
        };

        assert!(!app_state_action_enabled(&action, &state));
    }

    #[test]
    fn app_state_action_gate_uses_viewer_and_clip_product_availability() {
        let mut state = state_with_selected_clip();
        let clip_id = state.selection.selected_clips[0].clip_id;
        let position_parameter = state.active_sequence().expect("sequence").video_tracks[0].clips
            [0]
        .intrinsic_parameter_bag()
        .address_for_path(Transform2D::POSITION_PATH)
        .expect("position parameter");
        let preview_scale =
            viewer_set_preview_resolution_scale_action(ViewerSetPreviewResolutionScalePayload {
                scale: 0.25,
            });
        let transform = clip_write_parameter_values_action(ClipWriteParameterValuesPayload {
            clip_id,
            writes: vec![ClipParameterValueWrite {
                parameter: position_parameter.clone(),
                value: PropertyValue::Vec2(glam::Vec2::new(10.0, 20.0)),
            }],
        });
        let empty_transform = clip_write_parameter_values_action(ClipWriteParameterValuesPayload {
            clip_id,
            writes: Vec::new(),
        });

        assert!(app_state_action_enabled(&preview_scale, &state));
        assert!(app_state_action_enabled(&transform, &state));
        assert!(!app_state_action_enabled(&empty_transform, &state));

        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].is_locked = true;
        assert!(!app_state_action_enabled(&transform, &state));

        let stale = clip_write_parameter_values_action(ClipWriteParameterValuesPayload {
            clip_id: ClipId::new(),
            writes: vec![ClipParameterValueWrite {
                parameter: position_parameter,
                value: PropertyValue::Vec2(glam::Vec2::new(10.0, 20.0)),
            }],
        });
        assert!(!app_state_action_enabled(&stale, &state));
    }

    #[test]
    fn app_state_action_gate_uses_sequence_product_availability() {
        let mut state = AppState::new();
        assert!(!app_state_action_enabled(&sequence_new_action(), &state));

        let first = Sequence::new("First");
        let first_id = first.id;
        state.test_set_sequence(Some(first));
        assert!(app_state_action_enabled(&sequence_new_action(), &state));

        let second_id = state.new_sequence("Second").expect("new sequence");
        assert!(app_state_action_enabled(
            &sequence_switch_active_action(SequenceTargetPayload { sequence_id: first_id }),
            &state,
        ));
        assert!(!app_state_action_enabled(
            &sequence_switch_active_action(SequenceTargetPayload { sequence_id: second_id }),
            &state,
        ));
        assert!(!app_state_action_enabled(
            &sequence_switch_active_action(SequenceTargetPayload {
                sequence_id: mondrian_core::SequenceId::new(),
            }),
            &state,
        ));

        let sequence = state.sequence_by_id(second_id).expect("second sequence");
        assert!(!app_state_action_enabled(
            &sequence_update_settings_action(SequenceUpdateSettingsPayload {
                sequence_id: second_id,
                name: sequence.name.clone(),
                settings: sequence.settings.clone(),
            }),
            &state,
        ));
    }

    #[test]
    fn app_state_action_gate_uses_export_product_availability() {
        let mut state = AppState::new();
        let sequence = Sequence::new("Export");
        let sequence_id = sequence.id;
        state.test_set_sequence(Some(sequence));

        assert!(!app_state_action_enabled(
            &export_edit_draft_action(ExportDraftEdit::BuiltinPreset(
                state.export_draft.selected_builtin_preset,
            )),
            &state,
        ));
        assert!(app_state_action_enabled(
            &export_edit_draft_action(ExportDraftEdit::OutputPath("delivery.mp4".to_owned())),
            &state,
        ));
        assert!(app_state_action_enabled(
            &export_enqueue_action(TimelineExportRequest {
                preset: mondrian_export::preset::ExportPreset::h264_aac_sdr_1080p(),
                sequence_id: Some(sequence_id),
                range: mondrian_export::preset::TimelineExportRange::EntireSequence,
                output_path: std::path::PathBuf::from("delivery.mp4"),
                output_policy: mondrian_export::preset::ExportOutputPolicy::CreateNew,
            }),
            &state,
        ));
        assert!(!app_state_action_enabled(
            &export_cancel_action(mondrian_core::types::JobId::new()),
            &state,
        ));
        assert!(!app_state_action_enabled(
            &export_clear_terminal_history_action(),
            &state,
        ));
    }

    #[test]
    fn range_edit_actions_require_an_admissible_in_out_scope() {
        let mut state = state_with_selected_clip();
        let time_base = state.active_sequence().expect("sequence").time_base();
        {
            let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
            sequence.in_point = Some(tt(12, time_base));
            sequence.out_point = Some(tt(18, time_base));
        }

        assert!(app_state_action_enabled(
            &timeline_lift_range_action(),
            &state
        ));
        assert!(app_state_action_enabled(
            &timeline_extract_range_action(),
            &state
        ));

        let track_ids = state
            .active_sequence()
            .expect("sequence")
            .video_tracks
            .iter()
            .chain(&state.active_sequence().expect("sequence").audio_tracks)
            .map(|track| track.id)
            .collect::<Vec<_>>();
        for track_id in track_ids {
            state.set_timeline_track_targeted(track_id, false).expect("untarget");
        }
        assert!(!app_state_action_enabled(
            &timeline_lift_range_action(),
            &state
        ));
        assert!(!app_state_action_enabled(
            &timeline_extract_range_action(),
            &state
        ));
    }

    #[test]
    fn app_state_action_gate_disables_typed_clip_edits_on_locked_tracks() {
        let mut state = state_with_selected_clip();
        state.seek(15).expect("seek");
        let selection = state.selection.selected_clips[0];
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].is_locked = true;

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
        state.seek(10).expect("seek");
        assert!(!app_state_action_enabled(
            &Action::SplitClipAtPlayhead,
            &state
        ));

        state.seek(15).expect("seek");
        assert!(app_state_action_enabled(
            &Action::SplitClipAtPlayhead,
            &state
        ));

        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].is_locked = true;
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

        state.seek(10).expect("seek");
        assert!(!app_state_action_enabled(&trim_in, &state));
        assert!(app_state_action_enabled(&trim_out, &state));

        state.seek(29).expect("seek");
        assert!(app_state_action_enabled(&trim_in, &state));
        assert!(!app_state_action_enabled(&trim_out, &state));
    }
}
