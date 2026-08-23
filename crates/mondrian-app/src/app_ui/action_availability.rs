//! Shared app UI action availability gates.
//!
//! Menus, focused shortcuts, and panel adapters use this module to keep their
//! disabled states aligned before actions reach `AppState`.

use mondrian_core::types::{FramePosition, TrackId};
use mondrian_core::TimelineTime;
use mondrian_editor_state::Action;

use crate::app::product_action::ProductAction;
use crate::app::ui_actions::{
    APP_SHELL_IMPORT_MEDIA_DIALOG, APP_SHELL_NAMESPACE, APP_SHELL_PROJECT_SETTINGS, APP_SHELL_QUIT,
    APP_SHELL_SAVE_PROJECT_AS_DIALOG, APP_SHELL_SEQUENCE_SETTINGS,
};
use crate::app::AppState;
use mondrian_core::types::ClipId;

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

fn clip_is_editable(state: &AppState, clip_id: ClipId) -> bool {
    state.active_sequence().is_some_and(|sequence| {
        sequence
            .video_tracks
            .iter()
            .chain(&sequence.audio_tracks)
            .any(|track| !track.is_locked && track.clips.iter().any(|clip| clip.id == clip_id))
    })
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
    use mondrian_core::Rational;

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
        sequence_update_settings_action, timeline_clear_in_out_points_action,
        timeline_extract_range_action, timeline_lift_range_action, timeline_move_clip_action,
        timeline_roll_selected_cut_to_playhead_action, timeline_seek_action,
        timeline_select_clip_action, timeline_set_in_out_point_action,
        timeline_set_selected_clips_enabled_action, timeline_trim_clips_action,
        timeline_trim_selected_clips_to_playhead_action, track_add_action, track_move_action,
        track_set_author_control_action, track_set_edit_policy_action,
        viewer_set_preview_resolution_scale_action, AssetsDeleteSelectionPayload,
        AssetsImportFilesPayload, AssetsMoveSelectionPayload, ClipParameterValueWrite,
        ClipWriteParameterValuesPayload, ExportDraftEdit, ProjectCreateWithSettingsPayload,
        SequenceTargetPayload, SequenceUpdateSettingsPayload, TimelineExportRequest,
        TimelineInOutPointKind, TimelineMoveClipPayload, TimelineSelectClipPayload,
        TimelineSetInOutPointPayload, TimelineTrimClipsPayload, TimelineTrimPayloadEdge,
        TrackAddKind, TrackAddPayload, TrackAuthorControl, TrackEditPolicyControl,
        TrackMovePayload, TrackSetAuthorControlPayload, TrackSetEditPolicyPayload,
        ViewerSetPreviewResolutionScalePayload,
    };
    use crate::app::SelectedClipRef;
    use mondrian_core::automation::PropertyValue;
    use mondrian_core::types::{AssetId, ClipId, TrackId};
    use mondrian_timeline::clip::{Clip, Transform2D};
    use mondrian_timeline::sequence::Sequence;
    use mondrian_timeline::TrackRelativePlacement;

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
        let trim_action =
            timeline_trim_selected_clips_to_playhead_action(TimelineTrimPayloadEdge::In);
        let enable_action = timeline_set_selected_clips_enabled_action(false);

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
        let time_base = state.active_sequence().expect("sequence").time_base();
        let mut adjacent =
            Clip::new(AssetId::new(), tt(30, time_base), tt(20, time_base)).expect("valid clip");
        adjacent.set_source_origin(tt(20, time_base)).expect("incoming source handle");
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0]
            .add_clip(adjacent)
            .expect("add adjacent clip");
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
        let time_base = state.active_sequence().expect("sequence").time_base();
        let anchor_id =
            state.active_sequence_mut_uncommitted().expect("sequence").add_video_track();

        for action in [
            timeline_select_clip_action(TimelineSelectClipPayload {
                clip_id,
                mode: crate::app::ui_actions::TimelineClipSelectionModePayload::Replace,
            }),
            timeline_move_clip_action(TimelineMoveClipPayload {
                target_track_id: track_id,
                clip_id,
                position: FramePosition::new(12, time_base),
            }),
            timeline_trim_clips_action(TimelineTrimClipsPayload {
                clip_ids: vec![clip_id],
                edge: TimelineTrimPayloadEdge::In,
                position: FramePosition::new(15, time_base),
            }),
            timeline_trim_selected_clips_to_playhead_action(TimelineTrimPayloadEdge::In),
            timeline_set_in_out_point_action(TimelineSetInOutPointPayload {
                point: TimelineInOutPointKind::In,
                position: FramePosition::new(15, time_base),
            }),
            timeline_set_selected_clips_enabled_action(false),
            timeline_seek_action(FramePosition::new(42, time_base)),
            track_set_author_control_action(TrackSetAuthorControlPayload {
                track_id,
                control: TrackAuthorControl::Lock,
                enabled: true,
            }),
            track_set_edit_policy_action(TrackSetEditPolicyPayload {
                track_id,
                control: TrackEditPolicyControl::Target,
                enabled: false,
            }),
            track_add_action(TrackAddPayload { kind: TrackAddKind::Video }),
            track_move_action(TrackMovePayload {
                track_id,
                placement: TrackRelativePlacement::After(anchor_id),
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
        let time_base = state.active_sequence().expect("sequence").time_base();

        for action in [
            timeline_select_clip_action(TimelineSelectClipPayload {
                clip_id: stale_clip,
                mode: crate::app::ui_actions::TimelineClipSelectionModePayload::Replace,
            }),
            timeline_move_clip_action(TimelineMoveClipPayload {
                target_track_id: stale_track,
                clip_id: stale_clip,
                position: FramePosition::new(12, time_base),
            }),
            timeline_trim_clips_action(TimelineTrimClipsPayload {
                clip_ids: vec![stale_clip],
                edge: TimelineTrimPayloadEdge::In,
                position: FramePosition::new(15, time_base),
            }),
            track_set_author_control_action(TrackSetAuthorControlPayload {
                track_id: stale_track,
                control: TrackAuthorControl::Visibility,
                enabled: false,
            }),
            track_set_edit_policy_action(TrackSetEditPolicyPayload {
                track_id: stale_track,
                control: TrackEditPolicyControl::SyncLock,
                enabled: false,
            }),
            track_move_action(TrackMovePayload {
                track_id: stale_track,
                placement: TrackRelativePlacement::After(TrackId::new()),
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
        let time_base = state.active_sequence().expect("sequence").time_base();
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].is_locked = true;

        for action in [
            timeline_move_clip_action(TimelineMoveClipPayload {
                target_track_id: selection.track_id,
                clip_id: selection.clip_id,
                position: FramePosition::new(12, time_base),
            }),
            timeline_trim_clips_action(TimelineTrimClipsPayload {
                clip_ids: vec![selection.clip_id],
                edge: TimelineTrimPayloadEdge::In,
                position: FramePosition::new(15, time_base),
            }),
            timeline_trim_selected_clips_to_playhead_action(TimelineTrimPayloadEdge::In),
            timeline_set_selected_clips_enabled_action(false),
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
        let trim_in = timeline_trim_selected_clips_to_playhead_action(TimelineTrimPayloadEdge::In);
        let trim_out =
            timeline_trim_selected_clips_to_playhead_action(TimelineTrimPayloadEdge::Out);

        state.seek(10).expect("seek");
        assert!(!app_state_action_enabled(&trim_in, &state));
        assert!(app_state_action_enabled(&trim_out, &state));

        state.seek(29).expect("seek");
        assert!(app_state_action_enabled(&trim_in, &state));
        assert!(!app_state_action_enabled(&trim_out, &state));
    }
}
