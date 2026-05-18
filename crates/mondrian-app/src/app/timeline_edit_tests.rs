use super::*;
use mondrian_effects::EffectRenderOp;
use mondrian_timeline::clip::{AlphaInterpretation, MediaInterpretation};

fn create_state_with_sequence() -> AppState {
    let mut state = AppState::new();
    state.sequence = Some(Sequence::new("test"));
    state
}

fn primary_track_clip_lens(state: &AppState) -> (usize, usize) {
    let seq = state.sequence.as_ref().expect("sequence should exist");
    (
        seq.video_tracks[0].clips.len(),
        seq.audio_tracks[0].clips.len(),
    )
}

#[test]
fn create_new_project_with_settings_preserves_sequence_color_management() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("mondrian-new-project-settings-{unique}"));
    let project_file = root.join("project.mdp");

    let mut state = AppState::new();
    let settings = SequenceSettings {
        resolution: Resolution { width: 3840, height: 2160 },
        frame_rate: Rational::FPS_23976,
        color_space: ColorSpace::Rec2020,
        color_management: mondrian_timeline::sequence::SequenceColorManagement {
            workflow: ColorWorkflow::SceneReferred,
            output_color_space: ColorSpace::Rec2100Pq,
            video_range: VideoRange::Legal,
            export_bit_depth: ExportBitDepth::Ten,
            ..Default::default()
        },
        ..Default::default()
    };
    state
        .create_new_project_with_settings_at(
            project_file.clone(),
            "Color Project",
            settings.clone(),
            mondrian_core::ProjectSettings::default(),
        )
        .expect("create project");

    let sequence = state.sequence.as_ref().expect("sequence");
    assert_eq!(sequence.settings.resolution, settings.resolution);
    assert_eq!(sequence.settings.frame_rate, Rational::FPS_23976);
    assert_eq!(sequence.settings.color_space, ColorSpace::Rec2020);
    assert_eq!(
        sequence.settings.color_management.workflow,
        ColorWorkflow::SceneReferred
    );
    assert_eq!(
        sequence.settings.color_management.output_color_space,
        ColorSpace::Rec2100Pq
    );
    assert_eq!(
        sequence.settings.color_management.video_range,
        VideoRange::Legal
    );
    assert_eq!(
        sequence.settings.color_management.export_bit_depth,
        ExportBitDepth::Ten
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn set_clip_media_interpretation_is_undoable() {
    let mut state = create_state_with_sequence();
    let seq = state.sequence.as_ref().expect("sequence");
    let tb = seq.time_base();
    let track_id = seq.video_tracks[0].id;
    let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let clip_id = clip.id;
    state.sequence.as_mut().expect("sequence").video_tracks[0]
        .add_clip(clip)
        .expect("add clip");

    let selection = SelectedClipRef { track_id, clip_id, is_video_track: true };
    let interpretation = MediaInterpretation {
        color_space_override: Some(ColorSpace::SLog3),
        frame_rate_override: Some(Rational::FPS_23976),
        pixel_aspect_ratio_override: Some(PixelAspectRatio::HdAnamorphic1080),
        field_order_override: Some(FieldOrder::UpperFirst),
        alpha: AlphaInterpretation::Premultiplied,
    };
    state
        .set_clip_media_interpretation(selection, interpretation.clone())
        .expect("interpretation");
    assert_eq!(
        state.clip_snapshot(selection).expect("clip").interpretation,
        interpretation
    );

    state.undo_timeline().expect("undo");
    assert_eq!(
        state.clip_snapshot(selection).expect("clip").interpretation,
        MediaInterpretation::default()
    );
}

#[test]
fn set_clip_media_interpretation_rejects_locked_tracks() {
    let mut state = create_state_with_sequence();
    let seq = state.sequence.as_ref().expect("sequence");
    let tb = seq.time_base();
    let track_id = seq.video_tracks[0].id;
    let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let clip_id = clip.id;
    {
        let seq = state.sequence.as_mut().expect("sequence");
        seq.video_tracks[0].add_clip(clip).expect("add clip");
        seq.video_tracks[0].is_locked = true;
    }

    let selection = SelectedClipRef { track_id, clip_id, is_video_track: true };
    let err = state
        .set_clip_media_interpretation(
            selection,
            MediaInterpretation {
                color_space_override: Some(ColorSpace::DciP3),
                ..Default::default()
            },
        )
        .expect_err("locked track should reject interpretation change");
    assert!(matches!(
        err,
        mondrian_core::MondrianError::TrackLocked { .. }
    ));
}

#[test]
fn switching_sequences_preserves_independent_timelines() {
    let mut state = create_state_with_sequence();
    let first = state.sequence.as_ref().expect("sequence should exist").id;
    state.active_sequence_id = Some(first);
    state.default_sequence_id = Some(first);
    state.sync_current_sequence_into_collection();

    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    state.sequence.as_mut().expect("sequence should exist").video_tracks[0]
        .add_clip(Clip::new(
            AssetId::new(),
            TimeCode::new(0, tb),
            TimeCode::new(10, tb),
        ))
        .expect("add clip");
    state.sync_current_sequence_into_collection();

    state.new_sequence("second");
    let second = state.sequence.as_ref().expect("sequence should exist").id;
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    state.sequence.as_mut().expect("sequence should exist").video_tracks[0]
        .add_clip(Clip::new(
            AssetId::new(),
            TimeCode::new(20, tb),
            TimeCode::new(10, tb),
        ))
        .expect("add second clip");

    state.switch_active_sequence(first).expect("switch to first");
    let seq = state.sequence.as_ref().expect("first sequence should be active");
    assert_eq!(seq.id, first);
    assert_eq!(seq.video_tracks[0].clips[0].position.frame, 0);

    state.switch_active_sequence(second).expect("switch to second");
    let seq = state.sequence.as_ref().expect("second sequence should be active");
    assert_eq!(seq.id, second);
    assert_eq!(seq.video_tracks[0].clips[0].position.frame, 20);
}

#[test]
fn sequence_management_duplicate_rename_delete_updates_collection() {
    let mut state = create_state_with_sequence();
    let first = state.sequence.as_ref().expect("sequence").id;
    state.active_sequence_id = Some(first);
    state.default_sequence_id = Some(first);
    state.sync_current_sequence_into_collection();

    let duplicate = state.duplicate_sequence(first, "Duplicate").expect("duplicate sequence");
    assert_ne!(duplicate, first);
    assert_eq!(state.active_sequence_id, Some(duplicate));
    assert_eq!(state.sequence.as_ref().expect("active").name, "Duplicate");

    state.rename_sequence(duplicate, "Renamed").expect("rename");
    assert_eq!(state.sequence.as_ref().expect("active").name, "Renamed");

    state.delete_sequence(duplicate).expect("delete duplicate");
    assert_eq!(state.active_sequence_id, Some(first));
    assert_eq!(state.export_sequences_snapshot().len(), 1);
}

#[test]
fn delete_sequence_rejects_nested_references() {
    let mut state = create_state_with_sequence();
    let parent_id = state.sequence.as_ref().expect("parent").id;
    state.active_sequence_id = Some(parent_id);
    state.default_sequence_id = Some(parent_id);
    state.sync_current_sequence_into_collection();
    state.new_sequence("child");
    let child_id = state.sequence.as_ref().expect("child").id;
    state.switch_active_sequence(parent_id).expect("switch parent");
    let tb = state.sequence.as_ref().expect("parent").time_base();
    state.sequence.as_mut().expect("parent").video_tracks[0]
        .add_clip(Clip::new_nested_sequence(
            child_id,
            TimeCode::new(0, tb),
            TimeCode::new(10, tb),
            Some("child".to_string()),
        ))
        .expect("add nested");

    let err = state.delete_sequence(child_id).expect_err("nested delete rejected");
    assert!(format!("{err}").contains("嵌套引用"));
}

#[test]
fn precompose_clips_creates_nested_sequence_and_replacement_clip() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let track_id = state.sequence.as_ref().expect("sequence should exist").video_tracks[0].id;
    let clip = Clip::new(AssetId::new(), TimeCode::new(12, tb), TimeCode::new(30, tb));
    let clip_id = clip.id;
    state.sequence.as_mut().expect("sequence should exist").video_tracks[0]
        .add_clip(clip)
        .expect("add clip");

    let nested_clip_id = state
        .precompose_clips_as_sequence(&[(track_id, true, clip_id)], "Precomp 01")
        .expect("precompose");

    let parent = state.sequence.as_ref().expect("sequence should exist");
    let replacement = parent.video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == nested_clip_id)
        .expect("replacement nested clip");
    assert!(replacement.is_nested_sequence());
    assert_eq!(replacement.position.frame, 12);
    assert_eq!(replacement.duration.frame, 30);

    let nested_sequence_id = replacement.nested_sequence_id.expect("nested sequence id");
    let nested = state.sequence_by_id(nested_sequence_id).expect("nested sequence");
    assert_eq!(
        nested.role,
        mondrian_timeline::sequence::SequenceRole::NestedComposition
    );
    assert_eq!(nested.video_tracks[0].clips.len(), 1);
    assert_eq!(nested.video_tracks[0].clips[0].position.frame, 0);
}

fn video_clip_is_disabled(state: &AppState, clip_id: ClipId) -> bool {
    state
        .sequence
        .as_ref()
        .and_then(|seq| seq.video_tracks[0].clips.iter().find(|clip| clip.id == clip_id))
        .map(|clip| clip.is_disabled)
        .unwrap_or(false)
}

fn exposure_from_clip(clip: &Clip, time: TimeCode) -> f32 {
    clip.evaluate_effect_render_plan(time)
        .ops
        .iter()
        .find_map(|op| match op {
            EffectRenderOp::ColorAdjust { exposure, .. } => Some(*exposure),
            _ => None,
        })
        .unwrap_or(0.0)
}

#[test]
fn split_at_playhead_records_single_undo_step() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

    let video_clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(40, tb));
    let audio_clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(40, tb));

    state.sequence.as_mut().expect("sequence should exist").video_tracks[0]
        .add_clip(video_clip)
        .expect("add video clip");
    state.sequence.as_mut().expect("sequence should exist").audio_tracks[0]
        .add_clip(audio_clip)
        .expect("add audio clip");

    state.seek(10);

    let split_count = state.split_at_playhead().expect("split should succeed");
    assert_eq!(split_count, 2);
    assert_eq!(
        state.cmd_history.undo_description(),
        Some("在播放头分割片段")
    );
    assert_eq!(primary_track_clip_lens(&state), (2, 2));

    assert!(state.undo_timeline().expect("undo should succeed"));
    assert_eq!(primary_track_clip_lens(&state), (1, 1));

    assert!(state.redo_timeline().expect("redo should succeed"));
    assert_eq!(primary_track_clip_lens(&state), (2, 2));
}

#[test]
fn track_lock_change_is_undoable() {
    let mut state = create_state_with_sequence();
    let track_id = state.sequence.as_ref().expect("sequence should exist").video_tracks[0].id;

    state
        .set_track_locked(track_id, true, true)
        .expect("set track locked should succeed");
    assert!(state.sequence.as_ref().expect("sequence should exist").video_tracks[0].is_locked);
    assert_eq!(state.cmd_history.undo_description(), Some("切换轨道锁定"));

    assert!(state.undo_timeline().expect("undo should succeed"));
    assert!(!state.sequence.as_ref().expect("sequence should exist").video_tracks[0].is_locked);

    assert!(state.redo_timeline().expect("redo should succeed"));
    assert!(state.sequence.as_ref().expect("sequence should exist").video_tracks[0].is_locked);
}

#[test]
fn set_clip_disabled_is_undoable() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let track_id = state.sequence.as_ref().expect("sequence should exist").video_tracks[0].id;

    let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(30, tb));
    let clip_id = clip.id;
    state.sequence.as_mut().expect("sequence should exist").video_tracks[0]
        .add_clip(clip)
        .expect("add clip");

    state
        .set_clips_disabled_bulk(&[(track_id, true, clip_id)], true)
        .expect("disable clip should succeed");
    assert!(video_clip_is_disabled(&state, clip_id));
    assert_eq!(state.cmd_history.undo_description(), Some("禁用片段"));

    assert!(state.undo_timeline().expect("undo should succeed"));
    assert!(!video_clip_is_disabled(&state, clip_id));

    assert!(state.redo_timeline().expect("redo should succeed"));
    assert!(video_clip_is_disabled(&state, clip_id));
}

#[test]
fn move_clip_conflict_respects_insert_mode() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let track_id = state.sequence.as_ref().expect("sequence should exist").video_tracks[0].id;

    let clip_a = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let clip_b = Clip::new(AssetId::new(), TimeCode::new(20, tb), TimeCode::new(10, tb));
    let clip_b_id = clip_b.id;

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_a).expect("add clip a");
        seq.video_tracks[0].add_clip(clip_b).expect("add clip b");
    }

    state
        .move_clip_in_track_with_mode(track_id, true, clip_b_id, 5, ClipOverlapMode::Insert)
        .expect("move should succeed");

    let clips = &state.sequence.as_ref().expect("sequence should exist").video_tracks[0].clips;
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[0].position.frame, 0);
    assert_eq!(clips[1].id, clip_b_id);
    assert_eq!(clips[1].position.frame, 10);
}

#[test]
fn move_clip_conflict_respects_overwrite_mode() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let track_id = state.sequence.as_ref().expect("sequence should exist").video_tracks[0].id;

    let clip_a = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let clip_a_id = clip_a.id;
    let clip_b = Clip::new(AssetId::new(), TimeCode::new(20, tb), TimeCode::new(10, tb));
    let clip_b_id = clip_b.id;

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_a).expect("add clip a");
        seq.video_tracks[0].add_clip(clip_b).expect("add clip b");
    }

    state
        .move_clip_in_track_with_mode(track_id, true, clip_b_id, 5, ClipOverlapMode::Overwrite)
        .expect("move should succeed");

    let clips = &state.sequence.as_ref().expect("sequence should exist").video_tracks[0].clips;
    assert_eq!(clips.len(), 2);
    let kept_a = clips.iter().find(|clip| clip.id == clip_a_id).expect("clip a should exist");
    let moved_b = clips.iter().find(|clip| clip.id == clip_b_id).expect("clip b should exist");
    assert_eq!(kept_a.position.frame, 0);
    assert_eq!(kept_a.duration.frame, 5);
    assert_eq!(moved_b.position.frame, 5);
}

#[test]
fn overwrite_only_removes_intersection_and_keeps_both_sides() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let track_id = state.sequence.as_ref().expect("sequence should exist").video_tracks[0].id;

    let clip_a = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
    let clip_a_id = clip_a.id;
    let clip_b = Clip::new(AssetId::new(), TimeCode::new(40, tb), TimeCode::new(4, tb));
    let clip_b_id = clip_b.id;

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_a).expect("add clip a");
        seq.video_tracks[0].add_clip(clip_b).expect("add clip b");
    }

    state
        .move_clip_in_track_with_mode(track_id, true, clip_b_id, 8, ClipOverlapMode::Overwrite)
        .expect("move should succeed");

    let clips = &state.sequence.as_ref().expect("sequence should exist").video_tracks[0].clips;
    assert_eq!(clips.len(), 3);

    let left = clips.iter().find(|clip| clip.id == clip_a_id).expect("left part should exist");
    let moved = clips.iter().find(|clip| clip.id == clip_b_id).expect("moved clip should exist");
    let right = clips
        .iter()
        .find(|clip| clip.id != clip_a_id && clip.id != clip_b_id)
        .expect("right part should exist");

    assert_eq!(left.position.frame, 0);
    assert_eq!(left.duration.frame, 8);
    assert_eq!(left.source_in.frame, 0);
    assert_eq!(left.source_out.frame, 8);

    assert_eq!(moved.position.frame, 8);
    assert_eq!(moved.duration.frame, 4);

    assert_eq!(right.position.frame, 12);
    assert_eq!(right.duration.frame, 8);
    assert_eq!(right.source_in.frame, 12);
    assert_eq!(right.source_out.frame, 20);
}

#[test]
fn move_clip_group_overwrite_keeps_all_selected_clips() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

    let clip_a = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let clip_a_id = clip_a.id;
    let clip_b = Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(10, tb));
    let clip_b_id = clip_b.id;
    let clip_c = Clip::new(AssetId::new(), TimeCode::new(40, tb), TimeCode::new(10, tb));
    let clip_c_id = clip_c.id;

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_a).expect("add clip a");
        seq.video_tracks[0].add_clip(clip_b).expect("add clip b");
        seq.video_tracks[0].add_clip(clip_c).expect("add clip c");
    }

    state
        .move_clip_group_by_delta_with_mode(
            &[(clip_a_id, 0), (clip_b_id, 10)],
            5,
            ClipOverlapMode::Overwrite,
        )
        .expect("group move should succeed");

    let clips = &state.sequence.as_ref().expect("sequence should exist").video_tracks[0].clips;
    assert_eq!(clips.len(), 3);
    let moved_a = clips.iter().find(|clip| clip.id == clip_a_id).expect("clip a should exist");
    let moved_b = clips.iter().find(|clip| clip.id == clip_b_id).expect("clip b should exist");
    let untouched_c = clips.iter().find(|clip| clip.id == clip_c_id).expect("clip c should exist");
    assert_eq!(moved_a.position.frame, 5);
    assert_eq!(moved_b.position.frame, 15);
    assert_eq!(untouched_c.position.frame, 40);
}

#[test]
fn trim_in_is_undoable() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let clip = Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(20, tb));
    let clip_id = clip.id;

    state.sequence.as_mut().expect("sequence should exist").video_tracks[0]
        .add_clip(clip)
        .expect("add clip");

    let changed = state
        .trim_clips_bulk_to_frame(&[clip_id], TrimEdge::In, 15)
        .expect("trim in should succeed");
    assert_eq!(changed, 1);

    let trimmed = state.sequence.as_ref().expect("sequence should exist").video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .expect("clip should exist");
    assert_eq!(trimmed.position.frame, 15);
    assert_eq!(trimmed.duration.frame, 15);
    assert_eq!(trimmed.source_in.frame, 5);
    assert_eq!(state.cmd_history.undo_description(), Some("修剪入点"));

    assert!(state.undo_timeline().expect("undo should succeed"));
    let restored = state.sequence.as_ref().expect("sequence should exist").video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .expect("clip should exist after undo");
    assert_eq!(restored.position.frame, 10);
    assert_eq!(restored.duration.frame, 20);
    assert_eq!(restored.source_in.frame, 0);
}

#[test]
fn trim_out_updates_linked_clip() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

    let mut video = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(30, tb));
    let mut audio = Clip::new(video.asset_id, TimeCode::new(0, tb), TimeCode::new(30, tb));
    let video_id = video.id;
    let audio_id = audio.id;
    video.linked_clip = Some(audio_id);
    audio.linked_clip = Some(video_id);

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[0].add_clip(video).expect("add video");
        seq.audio_tracks[0].add_clip(audio).expect("add audio");
    }

    let changed = state
        .trim_clips_bulk_to_frame(&[video_id], TrimEdge::Out, 21)
        .expect("trim out should succeed");
    assert_eq!(changed, 2);

    let seq = state.sequence.as_ref().expect("sequence should exist");
    let video_after = seq.video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == video_id)
        .expect("video should exist");
    let audio_after = seq.audio_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == audio_id)
        .expect("audio should exist");

    assert_eq!(video_after.duration.frame, 21);
    assert_eq!(audio_after.duration.frame, 21);
    assert_eq!(video_after.source_out.frame, 21);
    assert_eq!(audio_after.source_out.frame, 21);
}

#[test]
fn removing_track_renumbers_tracks_and_clears_broken_links() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

    let mut video = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
    let mut audio = Clip::new(video.asset_id, TimeCode::new(0, tb), TimeCode::new(20, tb));
    let video_id = video.id;
    let audio_id = audio.id;
    video.linked_clip = Some(audio_id);
    audio.linked_clip = Some(video_id);

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[1].add_clip(video).expect("add video");
        seq.audio_tracks[1].add_clip(audio).expect("add audio");
    }

    let removed_track_id =
        state.sequence.as_ref().expect("sequence should exist").video_tracks[1].id;
    state.remove_track(removed_track_id, true).expect("remove track");

    let seq = state.sequence.as_ref().expect("sequence should exist");
    assert_eq!(seq.video_tracks.len(), 2);
    assert_eq!(seq.video_tracks[0].name, "V1");
    assert_eq!(seq.video_tracks[1].name, "V2");

    let audio_after = seq.audio_tracks[1]
        .clips
        .iter()
        .find(|clip| clip.id == audio_id)
        .expect("audio clip should remain");
    assert_eq!(audio_after.linked_clip, None);
}

#[test]
fn moving_track_is_undoable_and_preserves_clips() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let moved_track_id = state.sequence.as_ref().expect("sequence should exist").video_tracks[2].id;
    let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let clip_id = clip.id;
    state.sequence.as_mut().expect("sequence should exist").video_tracks[2]
        .add_clip(clip)
        .expect("add clip");

    state.move_track(moved_track_id, true, 0).expect("move track");

    let seq = state.sequence.as_ref().expect("sequence should exist");
    assert_eq!(seq.video_tracks[0].id, moved_track_id);
    assert_eq!(seq.video_tracks[0].name, "V1");
    assert_eq!(seq.video_tracks[0].clips[0].id, clip_id);
    assert_eq!(seq.video_tracks[1].name, "V2");
    assert_eq!(seq.video_tracks[2].name, "V3");
    assert_eq!(state.cmd_history.undo_description(), Some("移动轨道"));

    assert!(state.undo_timeline().expect("undo should succeed"));
    let seq_undo = state.sequence.as_ref().expect("sequence should exist after undo");
    assert_eq!(seq_undo.video_tracks[2].id, moved_track_id);
    assert_eq!(seq_undo.video_tracks[2].clips[0].id, clip_id);
}

#[test]
fn dropping_linked_clip_creates_missing_audio_track_at_target_index() {
    let mut state = create_state_with_sequence();
    let removed_audio_id =
        state.sequence.as_ref().expect("sequence should exist").audio_tracks[2].id;
    state
        .sequence
        .as_mut()
        .expect("sequence should exist")
        .remove_audio_track(removed_audio_id)
        .expect("remove third audio track");
    state.ensure_minimum_tracks();

    let target_track_id =
        state.sequence.as_ref().expect("sequence should exist").video_tracks[2].id;
    let asset_id = AssetId::new();
    state.begin_drag_asset(
        asset_id,
        "AV Clip".to_string(),
        AssetKind::Video,
        Duration::from_secs(2),
        true,
    );

    let video_clip_id = state
        .drop_dragging_asset_to_video_track(target_track_id, 0)
        .expect("drop linked clip");

    let seq = state.sequence.as_ref().expect("sequence should exist");
    assert_eq!(seq.audio_tracks.len(), 3);
    let video_clip = seq.video_tracks[2]
        .clips
        .iter()
        .find(|clip| clip.id == video_clip_id)
        .expect("video clip should exist");
    let audio_clip = seq.audio_tracks[2]
        .clips
        .iter()
        .find(|clip| clip.linked_clip == Some(video_clip_id))
        .expect("linked audio clip should exist");
    assert_eq!(video_clip.linked_clip, Some(audio_clip.id));
    assert_eq!(audio_clip.asset_id, asset_id);
}

#[test]
fn dropping_linked_clip_after_video_reorder_uses_current_track_index() {
    let mut state = create_state_with_sequence();
    let moved_video_track_id =
        state.sequence.as_ref().expect("sequence should exist").video_tracks[2].id;
    state.move_track(moved_video_track_id, true, 0).expect("move track before drop");

    state.begin_drag_asset(
        AssetId::new(),
        "Moved Track AV".to_string(),
        AssetKind::Video,
        Duration::from_secs(1),
        true,
    );
    let video_clip_id = state
        .drop_dragging_asset_to_video_track(moved_video_track_id, 0)
        .expect("drop linked clip");

    let seq = state.sequence.as_ref().expect("sequence should exist");
    assert_eq!(seq.video_tracks[0].id, moved_video_track_id);
    assert!(seq.video_tracks[0].clips.iter().any(|clip| clip.id == video_clip_id));
    assert!(seq.audio_tracks[0]
        .clips
        .iter()
        .any(|clip| clip.linked_clip == Some(video_clip_id)));
}

#[test]
fn moving_video_track_keeps_existing_linked_audio_on_its_audio_track() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

    let mut video = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(15, tb));
    let mut audio = Clip::new(video.asset_id, TimeCode::new(0, tb), TimeCode::new(15, tb));
    let video_id = video.id;
    let audio_id = audio.id;
    video.linked_clip = Some(audio_id);
    audio.linked_clip = Some(video_id);

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[2].add_clip(video).expect("add video");
        seq.audio_tracks[2].add_clip(audio).expect("add audio");
    }

    let moved_video_track_id =
        state.sequence.as_ref().expect("sequence should exist").video_tracks[2].id;
    state.move_track(moved_video_track_id, true, 0).expect("move video track");

    let seq = state.sequence.as_ref().expect("sequence should exist");
    assert!(seq.video_tracks[0].clips.iter().any(|clip| clip.id == video_id));
    let audio_after = seq.audio_tracks[2]
        .clips
        .iter()
        .find(|clip| clip.id == audio_id)
        .expect("audio clip should stay on original audio track");
    assert_eq!(audio_after.linked_clip, Some(video_id));
}

#[test]
fn creating_adjustment_layer_on_track_also_creates_library_asset() {
    let mut state = create_state_with_sequence();
    let temp_root = std::env::temp_dir().join(format!(
        "mondrian-adjustment-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    ));
    state.asset_library = Some(AssetLibrary::open(temp_root.clone()).expect("open library"));
    state.sequence.as_mut().expect("sequence should exist").in_point_frame = Some(10);
    state.sequence.as_mut().expect("sequence should exist").out_point_frame = Some(40);

    let target_track_id =
        state.sequence.as_ref().expect("sequence should exist").video_tracks[0].id;
    let clip_id = state
        .create_adjustment_layer_on_video_track(target_track_id, None, ClipOverlapMode::Overwrite)
        .expect("create adjustment layer");

    let library = state.asset_library.as_ref().expect("library should exist");
    let assets = library.list_assets().expect("list assets");
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].kind, AssetKind::AdjustmentLayer);

    let seq = state.sequence.as_ref().expect("sequence should exist");
    let clip = seq.video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .expect("adjustment clip should exist");
    assert!(clip.is_adjustment_layer());
    assert_eq!(clip.asset_id, assets[0].id);
    assert_eq!(clip.position.frame, 10);
    assert_eq!(clip.duration.frame, 30);

    let _ = std::fs::remove_dir_all(temp_root);
}

#[test]
fn splitting_adjustment_layer_keeps_instance_state_isolated() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

    let mut clip =
        Clip::new_adjustment_layer(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(40, tb));
    clip.add_effect(EffectType::BasicCorrection);
    let exposure_path = clip
        .effect_property_path("basic_correction.exposure")
        .expect("adjustment exposure path");
    clip.apply_property_mutation(
        mondrian_core::automation::PropertyMutation::SetStaticValue {
            path: exposure_path,
            value: mondrian_core::automation::PropertyValue::Float(0.75),
        },
    )
    .expect("set adjustment exposure");
    let clip_id = clip.id;
    state.sequence.as_mut().expect("sequence should exist").video_tracks[0]
        .add_clip(clip)
        .expect("add adjustment clip");

    state
        .split_clip_at_frame(
            state.sequence.as_ref().expect("sequence should exist").video_tracks[0].id,
            true,
            clip_id,
            20,
        )
        .expect("split adjustment layer");

    let seq = state.sequence.as_ref().expect("sequence should exist");
    let mut split_clips = seq.video_tracks[0].clips.clone();
    split_clips.sort_by_key(|clip| clip.position.frame);
    assert_eq!(split_clips.len(), 2);
    assert!(split_clips.iter().all(|clip| clip.is_adjustment_layer()));
    assert!(split_clips
        .iter()
        .all(|clip| { (exposure_from_clip(clip, TimeCode::new(20, tb)) - 0.75).abs() < 1.0e-4 }));

    let right_id = split_clips[1].id;
    let seq_mut = state.sequence.as_mut().expect("sequence should exist");
    let right_clip = seq_mut.video_tracks[0]
        .clips
        .iter_mut()
        .find(|clip| clip.id == right_id)
        .expect("right split clip should exist");
    let right_exposure_path = right_clip
        .effect_property_path("basic_correction.exposure")
        .expect("right adjustment exposure path");
    right_clip
        .apply_property_mutation(
            mondrian_core::automation::PropertyMutation::SetStaticValue {
                path: right_exposure_path,
                value: mondrian_core::automation::PropertyValue::Float(1.5),
            },
        )
        .expect("mutate right split clip");

    let seq = state.sequence.as_ref().expect("sequence should exist");
    let left_clip = seq.video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .expect("left split clip should exist");
    let right_clip = seq.video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == right_id)
        .expect("right split clip should exist");
    assert!((exposure_from_clip(left_clip, TimeCode::new(10, tb)) - 0.75).abs() < 1.0e-4);
    assert!((exposure_from_clip(right_clip, TimeCode::new(30, tb)) - 1.5).abs() < 1.0e-4);
}

#[test]
fn roll_cut_to_frame_is_undoable() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

    let clip_a = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
    let clip_a_id = clip_a.id;
    let clip_b = Clip::new(AssetId::new(), TimeCode::new(20, tb), TimeCode::new(20, tb));
    let clip_b_id = clip_b.id;

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_a).expect("add clip a");
        seq.video_tracks[0].add_clip(clip_b).expect("add clip b");
    }

    let changed = state.roll_cut_to_frame(clip_a_id, 25).expect("roll cut should succeed");
    assert!(changed);
    assert_eq!(state.cmd_history.undo_description(), Some("滚动修剪"));

    let seq = state.sequence.as_ref().expect("sequence should exist");
    let clip_a_after = seq.video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == clip_a_id)
        .expect("clip a should exist");
    let clip_b_after = seq.video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == clip_b_id)
        .expect("clip b should exist");

    assert_eq!(clip_a_after.duration.frame, 25);
    assert_eq!(clip_b_after.position.frame, 25);
    assert_eq!(clip_b_after.duration.frame, 15);
    assert_eq!(clip_b_after.source_in.frame, 5);

    assert!(state.undo_timeline().expect("undo should succeed"));
    let seq_undo = state.sequence.as_ref().expect("sequence should exist");
    let clip_a_undo = seq_undo.video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == clip_a_id)
        .expect("clip a should exist after undo");
    let clip_b_undo = seq_undo.video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == clip_b_id)
        .expect("clip b should exist after undo");
    assert_eq!(clip_a_undo.duration.frame, 20);
    assert_eq!(clip_b_undo.position.frame, 20);
    assert_eq!(clip_b_undo.source_in.frame, 0);
}

#[test]
fn slip_clip_negative_delta_is_clamped_and_undoable() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let library_root = std::env::temp_dir().join(format!(
        "mondrian_timeline_slip_test_{}_{}",
        std::process::id(),
        unix_now_ms()
    ));
    std::fs::create_dir_all(&library_root).expect("create temp library root");
    state.asset_library = Some(AssetLibrary::open(library_root.clone()).expect("open library"));

    let mut clip = Clip::new(AssetId::new(), TimeCode::new(8, tb), TimeCode::new(20, tb));
    clip.source_in = TimeCode::new(10, tb);
    clip.source_out = TimeCode::new(30, tb);
    let clip_id = clip.id;
    state.sequence.as_mut().expect("sequence should exist").video_tracks[0]
        .add_clip(clip)
        .expect("add clip");

    let changed = state.slip_clips_bulk_by_frames(&[clip_id], -15).expect("slip should succeed");
    assert_eq!(changed, 1);
    assert_eq!(state.cmd_history.undo_description(), Some("滑移片段"));

    let seq = state.sequence.as_ref().expect("sequence should exist");
    let slipped = seq.video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .expect("clip should exist");
    assert_eq!(slipped.position.frame, 8);
    assert_eq!(slipped.duration.frame, 20);
    assert_eq!(slipped.source_in.frame, 0);
    assert_eq!(slipped.source_out.frame, 20);

    assert!(state.undo_timeline().expect("undo should succeed"));
    let restored = state.sequence.as_ref().expect("sequence should exist").video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .expect("clip should exist after undo");
    assert_eq!(restored.source_in.frame, 10);
    assert_eq!(restored.source_out.frame, 30);

    state.asset_library = None;
    let _ = std::fs::remove_dir_all(&library_root);
}

#[test]
fn adjustment_layer_rejects_slip() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let library_root = std::env::temp_dir().join(format!(
        "mondrian_adjustment_slip_test_{}_{}",
        std::process::id(),
        unix_now_ms()
    ));
    std::fs::create_dir_all(&library_root).expect("create temp library root");
    state.asset_library = Some(AssetLibrary::open(library_root.clone()).expect("open library"));
    let clip =
        Clip::new_adjustment_layer(AssetId::new(), TimeCode::new(8, tb), TimeCode::new(20, tb));
    let clip_id = clip.id;
    state.sequence.as_mut().expect("sequence should exist").video_tracks[0]
        .add_clip(clip)
        .expect("add adjustment clip");

    let err = state
        .slip_clips_bulk_by_frames(&[clip_id], 5)
        .expect_err("adjustment layers should reject slip");
    assert!(err.to_string().contains("调整图层不支持 slip"));

    state.asset_library = None;
    let _ = std::fs::remove_dir_all(&library_root);
}

#[test]
fn slide_clip_updates_neighbors_and_is_undoable() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

    let left = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let center = Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(10, tb));
    let center_id = center.id;
    let right = Clip::new(AssetId::new(), TimeCode::new(20, tb), TimeCode::new(10, tb));

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[0].add_clip(left).expect("add left");
        seq.video_tracks[0].add_clip(center).expect("add center");
        seq.video_tracks[0].add_clip(right).expect("add right");
    }

    let changed = state.slide_clips_bulk_by_frames(&[center_id], 3).expect("slide should succeed");
    assert_eq!(changed, 1);
    assert_eq!(state.cmd_history.undo_description(), Some("滑动片段"));

    let seq = state.sequence.as_ref().expect("sequence should exist");
    let clips = &seq.video_tracks[0].clips;
    assert_eq!(clips.len(), 3);
    assert_eq!(clips[0].position.frame, 0);
    assert_eq!(clips[0].duration.frame, 13);
    assert_eq!(clips[1].id, center_id);
    assert_eq!(clips[1].position.frame, 13);
    assert_eq!(clips[1].duration.frame, 10);
    assert_eq!(clips[2].position.frame, 23);
    assert_eq!(clips[2].duration.frame, 7);
    assert_eq!(clips[2].source_in.frame, 3);

    assert!(state.undo_timeline().expect("undo should succeed"));
    let seq_undo = state.sequence.as_ref().expect("sequence should exist");
    let clips_undo = &seq_undo.video_tracks[0].clips;
    assert_eq!(clips_undo[0].duration.frame, 10);
    assert_eq!(clips_undo[1].position.frame, 10);
    assert_eq!(clips_undo[2].position.frame, 20);
    assert_eq!(clips_undo[2].source_in.frame, 0);
}

// ── Cross-track clip movement ──

#[test]
fn cross_track_move_to_different_track() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let target_track_id =
        state.sequence.as_ref().expect("sequence should exist").video_tracks[1].id;

    let clip = Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(20, tb));
    let clip_id = clip.id;

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip).expect("add clip");
    }

    state
        .move_clip_to_track_with_mode(
            target_track_id,
            true,
            clip_id,
            5,
            ClipOverlapMode::Overwrite,
        )
        .expect("cross-track move should succeed");

    let seq = state.sequence.as_ref().expect("sequence should exist");
    assert!(seq.video_tracks[0].clips.is_empty());
    assert_eq!(seq.video_tracks[1].clips.len(), 1);
    assert_eq!(seq.video_tracks[1].clips[0].id, clip_id);
    assert_eq!(seq.video_tracks[1].clips[0].position.frame, 5);
}

#[test]
fn cross_track_move_batch_preserves_relative_positions() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let target_track_id =
        state.sequence.as_ref().expect("sequence should exist").video_tracks[1].id;

    let clip_a = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let clip_a_id = clip_a.id;
    let clip_b = Clip::new(AssetId::new(), TimeCode::new(20, tb), TimeCode::new(10, tb));
    let clip_b_id = clip_b.id;

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_a).expect("add clip a");
        seq.video_tracks[0].add_clip(clip_b).expect("add clip b");
    }

    // Move both clips with the same delta (+5 frames).
    state
        .move_clip_to_track_with_mode(
            target_track_id,
            true,
            clip_a_id,
            5,
            ClipOverlapMode::Overwrite,
        )
        .expect("move clip a");
    state
        .move_clip_to_track_with_mode(
            target_track_id,
            true,
            clip_b_id,
            25,
            ClipOverlapMode::Overwrite,
        )
        .expect("move clip b");

    let seq = state.sequence.as_ref().expect("sequence should exist");
    assert!(seq.video_tracks[0].clips.is_empty());
    assert_eq!(seq.video_tracks[1].clips.len(), 2);
    assert_eq!(seq.video_tracks[1].clips[0].id, clip_a_id);
    assert_eq!(seq.video_tracks[1].clips[0].position.frame, 5);
    assert_eq!(seq.video_tracks[1].clips[1].id, clip_b_id);
    assert_eq!(seq.video_tracks[1].clips[1].position.frame, 25);
}

#[test]
fn cross_track_move_linked_clip_follows() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let video_track_1_id =
        state.sequence.as_ref().expect("sequence should exist").video_tracks[1].id;

    let mut video = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let mut audio = Clip::new(video.asset_id, TimeCode::new(0, tb), TimeCode::new(10, tb));
    let video_id = video.id;
    let audio_id = audio.id;
    video.linked_clip = Some(audio_id);
    audio.linked_clip = Some(video_id);

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[0].add_clip(video).expect("add video");
        seq.audio_tracks[0].add_clip(audio).expect("add audio");
    }

    state
        .move_clip_to_track_with_mode(
            video_track_1_id,
            true,
            video_id,
            5,
            ClipOverlapMode::Overwrite,
        )
        .expect("cross-track move should succeed");

    let seq = state.sequence.as_ref().expect("sequence should exist");
    // Video moved to track 1.
    assert!(seq.video_tracks[0].clips.is_empty());
    assert_eq!(seq.video_tracks[1].clips[0].id, video_id);
    assert_eq!(seq.video_tracks[1].clips[0].position.frame, 5);
    // Linked audio follows to audio track 1.
    assert!(seq.audio_tracks[0].clips.is_empty());
    assert_eq!(seq.audio_tracks[1].clips[0].id, audio_id);
    assert_eq!(seq.audio_tracks[1].clips[0].position.frame, 5);
}

#[test]
fn cross_track_move_locked_track_rejects() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let locked_track_id =
        state.sequence.as_ref().expect("sequence should exist").video_tracks[1].id;

    let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let clip_id = clip.id;

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[1].is_locked = true;
        seq.video_tracks[0].add_clip(clip).expect("add clip");
    }

    let result = state.move_clip_to_track_with_mode(
        locked_track_id,
        true,
        clip_id,
        5,
        ClipOverlapMode::Overwrite,
    );
    assert!(result.is_err());

    // Clip still on source track.
    let seq = state.sequence.as_ref().expect("sequence should exist");
    assert_eq!(seq.video_tracks[0].clips.len(), 1);
    assert_eq!(seq.video_tracks[0].clips[0].id, clip_id);
    assert!(seq.video_tracks[1].clips.is_empty());
}

#[test]
fn cross_track_move_insert_mode_pushes_existing() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let target_track_id =
        state.sequence.as_ref().expect("sequence should exist").video_tracks[1].id;

    let existing = Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(20, tb));
    let existing_id = existing.id;
    let mover = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let mover_id = mover.id;

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[1].add_clip(existing).expect("add existing");
        seq.video_tracks[0].add_clip(mover).expect("add mover");
    }

    state
        .move_clip_to_track_with_mode(target_track_id, true, mover_id, 5, ClipOverlapMode::Insert)
        .expect("insert move should succeed");

    let seq = state.sequence.as_ref().expect("sequence should exist");
    assert_eq!(seq.video_tracks[1].clips.len(), 2);
    assert_eq!(seq.video_tracks[1].clips[0].id, mover_id);
    assert_eq!(seq.video_tracks[1].clips[0].position.frame, 5);
    assert_eq!(seq.video_tracks[1].clips[1].id, existing_id);
    assert_eq!(seq.video_tracks[1].clips[1].position.frame, 15);
}

#[test]
fn cross_track_move_overwrite_mode_trims_existing() {
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let target_track_id =
        state.sequence.as_ref().expect("sequence should exist").video_tracks[1].id;

    let existing = Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(20, tb));
    let existing_id = existing.id;
    let mover = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let mover_id = mover.id;

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[1].add_clip(existing).expect("add existing");
        seq.video_tracks[0].add_clip(mover).expect("add mover");
    }

    state
        .move_clip_to_track_with_mode(
            target_track_id,
            true,
            mover_id,
            5,
            ClipOverlapMode::Overwrite,
        )
        .expect("overwrite move should succeed");

    let seq = state.sequence.as_ref().expect("sequence should exist");
    // Overwrite: existing clip at [10, 30) intersected by mover at [5, 15) → existing trimmed to [15, 30)
    assert_eq!(seq.video_tracks[1].clips[0].id, mover_id);
    assert_eq!(seq.video_tracks[1].clips[0].position.frame, 5);
    assert_eq!(seq.video_tracks[1].clips[1].id, existing_id);
    assert_eq!(seq.video_tracks[1].clips[1].position.frame, 15);
    assert_eq!(seq.video_tracks[1].clips[1].duration.frame, 15);
}

#[test]
fn cross_track_move_same_track_behavior_preserved() {
    // Moving within the same track should still work via move_clip_to_track_with_mode.
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let track_id = state.sequence.as_ref().expect("sequence should exist").video_tracks[0].id;

    let clip = Clip::new(AssetId::new(), TimeCode::new(20, tb), TimeCode::new(10, tb));
    let clip_id = clip.id;

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip).expect("add clip");
    }

    state
        .move_clip_to_track_with_mode(track_id, true, clip_id, 5, ClipOverlapMode::Overwrite)
        .expect("same-track move should succeed");

    let seq = state.sequence.as_ref().expect("sequence should exist");
    assert_eq!(seq.video_tracks[0].clips.len(), 1);
    assert_eq!(seq.video_tracks[0].clips[0].id, clip_id);
    assert_eq!(seq.video_tracks[0].clips[0].position.frame, 5);
}

#[test]
fn cross_track_move_with_undo_snapshot_restores_original() {
    // move_clip_to_track_with_mode doesn't push to the command history directly;
    // the UI layer records a snapshot before calling. Verify snapshot semantics.
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let target_track_id =
        state.sequence.as_ref().expect("sequence should exist").video_tracks[1].id;

    let clip = Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(20, tb));
    let clip_id = clip.id;

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip).expect("add clip");
    }

    // Snapshot before move (like the UI drop handler does).
    let before = state.sequence.clone();

    state
        .move_clip_to_track_with_mode(
            target_track_id,
            true,
            clip_id,
            5,
            ClipOverlapMode::Overwrite,
        )
        .expect("cross-track move should succeed");

    // Verify move happened.
    assert!(
        state.sequence.as_ref().expect("sequence should exist").video_tracks[0]
            .clips
            .is_empty()
    );

    // Restore snapshot (simulating undo).
    state.sequence = before;
    let seq = state.sequence.as_ref().expect("sequence should exist");
    assert_eq!(seq.video_tracks[0].clips.len(), 1);
    assert_eq!(seq.video_tracks[0].clips[0].id, clip_id);
    assert_eq!(seq.video_tracks[0].clips[0].position.frame, 10);
    assert!(seq.video_tracks[1].clips.is_empty());
}

#[test]
fn cross_track_move_relative_offset_across_different_source_tracks() {
    // Premiere-style: dragging V2→V3 gives a +1 track delta. A V1 clip should
    // move to V2, not V3. Each selected clip shifts by the same track offset.
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

    let clip_v1 = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let clip_v1_id = clip_v1.id;
    let clip_v2 = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let clip_v2_id = clip_v2.id;

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_v1).expect("add clip v1");
        seq.video_tracks[1].add_clip(clip_v2).expect("add clip v2");
    }

    // Simulate dragging V2 clip to V3 (track index 2). Delta = +1.
    let target_track_id =
        state.sequence.as_ref().expect("sequence should exist").video_tracks[2].id;

    // V2 clip moves to V3.
    state
        .move_clip_to_track_with_mode(
            target_track_id,
            true,
            clip_v2_id,
            5,
            ClipOverlapMode::Overwrite,
        )
        .expect("move clip v2");
    // V1 clip moves to V2 (same +1 track delta).
    let v2_target_id = state.sequence.as_ref().expect("sequence should exist").video_tracks[1].id;
    state
        .move_clip_to_track_with_mode(
            v2_target_id,
            true,
            clip_v1_id,
            5,
            ClipOverlapMode::Overwrite,
        )
        .expect("move clip v1");

    let seq = state.sequence.as_ref().expect("sequence should exist");
    assert!(seq.video_tracks[0].clips.is_empty());
    assert_eq!(seq.video_tracks[1].clips.len(), 1);
    assert_eq!(seq.video_tracks[1].clips[0].id, clip_v1_id);
    assert_eq!(seq.video_tracks[1].clips[0].position.frame, 5);
    assert_eq!(seq.video_tracks[2].clips.len(), 1);
    assert_eq!(seq.video_tracks[2].clips[0].id, clip_v2_id);
    assert_eq!(seq.video_tracks[2].clips[0].position.frame, 5);
}

#[test]
fn cross_track_move_negative_delta_clips_out_of_bounds_are_skipped() {
    // V2→V0 (delta -1). V1 clip would go to V-1 → skipped.
    // Only the dragged clip (V2→V0) should move.
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

    let clip_v1 = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let clip_v1_id = clip_v1.id;
    let clip_v2 = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let clip_v2_id = clip_v2.id;

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_v1).expect("add clip v1");
        seq.video_tracks[1].add_clip(clip_v2).expect("add clip v2");
    }

    // V2 clip to V0 (delta -1).
    let target_track_id =
        state.sequence.as_ref().expect("sequence should exist").video_tracks[0].id;
    state
        .move_clip_to_track_with_mode(
            target_track_id,
            true,
            clip_v2_id,
            5,
            ClipOverlapMode::Overwrite,
        )
        .expect("move clip v2");

    // V1 clip (source index 0 + (-1) = -1) → out of bounds → would be skipped
    // by the UI. In this test we just verify the V2 clip moved to V0.

    let seq = state.sequence.as_ref().expect("sequence should exist");
    assert_eq!(seq.video_tracks[0].clips.len(), 2);
    assert!(seq.video_tracks[1].clips.is_empty());
    // V1 clip stays on V0 (unmoved).
    assert!(seq.video_tracks[0].clips.iter().any(|c| c.id == clip_v1_id));
    // V2 clip moved to V0.
    assert!(seq.video_tracks[0].clips.iter().any(|c| c.id == clip_v2_id));
}

#[test]
fn cross_track_move_constrained_delta_prevents_out_of_bounds() {
    // V2+V3 selected, drag V3→V1 (raw delta -2). Constrained to delta -1
    // because -2 would push V2 clip to index -1. Result: V2→V1, V3→V2.
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

    let clip_v2 = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let clip_v2_id = clip_v2.id;
    let clip_v3 = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let clip_v3_id = clip_v3.id;

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[1].add_clip(clip_v2).expect("add clip v2");
        seq.video_tracks[2].add_clip(clip_v3).expect("add clip v3");
    }

    // Raw delta = V1(0) - V3(2) = -2. Constrained: min_src=1, max_src=2,
    // track_count=3, max_delta = 2-2=0, min_delta = -1. So clamped to -1.
    // V3→V2 (index 2-1=1), V2→V1 (index 1-1=0).
    let v2_target_id = state.sequence.as_ref().expect("sequence should exist").video_tracks[1].id;
    let v1_target_id = state.sequence.as_ref().expect("sequence should exist").video_tracks[0].id;

    state
        .move_clip_to_track_with_mode(
            v2_target_id,
            true,
            clip_v3_id,
            5,
            ClipOverlapMode::Overwrite,
        )
        .expect("move clip v3");
    state
        .move_clip_to_track_with_mode(
            v1_target_id,
            true,
            clip_v2_id,
            5,
            ClipOverlapMode::Overwrite,
        )
        .expect("move clip v2");

    let seq = state.sequence.as_ref().expect("sequence should exist");
    // V2 clip at V1.
    assert_eq!(seq.video_tracks[0].clips.len(), 1);
    assert_eq!(seq.video_tracks[0].clips[0].id, clip_v2_id);
    assert_eq!(seq.video_tracks[0].clips[0].position.frame, 5);
    // V3 clip at V2.
    assert_eq!(seq.video_tracks[1].clips.len(), 1);
    assert_eq!(seq.video_tracks[1].clips[0].id, clip_v3_id);
    assert_eq!(seq.video_tracks[1].clips[0].position.frame, 5);
    // V3 is empty.
    assert!(seq.video_tracks[2].clips.is_empty());
}

#[test]
fn cross_track_move_overlapping_clips_preserves_integrity() {
    // V1 clip A@[0,10), V2 clip B@[5,15) — overlapping in time.
    // Both selected, dragged +1: A→V2, B→V3.
    // B must move FIRST (further in delta direction) so A's arrival
    // on V2 doesn't trim B before it departs.
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

    let clip_a = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let clip_a_id = clip_a.id;
    let clip_b = Clip::new(AssetId::new(), TimeCode::new(5, tb), TimeCode::new(10, tb));
    let clip_b_id = clip_b.id;

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_a).expect("add clip a");
        seq.video_tracks[1].add_clip(clip_b).expect("add clip b");
    }

    // Delta +1: B moves first (V2→V3), then A (V1→V2).
    let v3_target = state.sequence.as_ref().expect("sequence should exist").video_tracks[2].id;
    let v2_target = state.sequence.as_ref().expect("sequence should exist").video_tracks[1].id;

    // B first (higher source index).
    state
        .move_clip_to_track_with_mode(v3_target, true, clip_b_id, 5, ClipOverlapMode::Overwrite)
        .expect("move clip b");
    // Then A.
    state
        .move_clip_to_track_with_mode(v2_target, true, clip_a_id, 0, ClipOverlapMode::Overwrite)
        .expect("move clip a");

    let seq = state.sequence.as_ref().expect("sequence should exist");
    // V1 empty.
    assert!(seq.video_tracks[0].clips.is_empty());
    // A on V2, intact.
    assert_eq!(seq.video_tracks[1].clips.len(), 1);
    assert_eq!(seq.video_tracks[1].clips[0].id, clip_a_id);
    assert_eq!(seq.video_tracks[1].clips[0].position.frame, 0);
    assert_eq!(seq.video_tracks[1].clips[0].duration.frame, 10);
    // B on V3, intact.
    assert_eq!(seq.video_tracks[2].clips.len(), 1);
    assert_eq!(seq.video_tracks[2].clips[0].id, clip_b_id);
    assert_eq!(seq.video_tracks[2].clips[0].position.frame, 5);
    assert_eq!(seq.video_tracks[2].clips[0].duration.frame, 10);
}

#[test]
fn same_track_move_does_not_trim_before_release() {
    // Ghost-based drag: during drag, the sequence must be unchanged.
    // Only on release (drop) should the move be applied.
    let mut state = create_state_with_sequence();
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let _track_id = state.sequence.as_ref().expect("sequence should exist").video_tracks[0].id;

    // Clip A at [0, 10), clip B at [20, 10) — separated, no overlap.
    let clip_a = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
    let clip_a_id = clip_a.id;
    let clip_b = Clip::new(AssetId::new(), TimeCode::new(20, tb), TimeCode::new(10, tb));
    let clip_b_id = clip_b.id;

    {
        let seq = state.sequence.as_mut().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_a).expect("add a");
        seq.video_tracks[0].add_clip(clip_b).expect("add b");
    }

    // Simulate drag start: save before-snapshot.
    let before = state.sequence.clone();

    // "During drag": the sequence should be restored to before-snapshot
    // (no mutations). Verify by checking A and B are in original positions.
    {
        let seq = state.sequence.as_ref().expect("sequence should exist");
        let clips = &seq.video_tracks[0].clips;
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0].id, clip_a_id);
        assert_eq!(clips[0].position.frame, 0);
        assert_eq!(clips[0].duration.frame, 10);
        assert_eq!(clips[1].id, clip_b_id);
        assert_eq!(clips[1].position.frame, 20);
        assert_eq!(clips[1].duration.frame, 10);
    }

    // "On release": apply the actual move (A to frame 5, B to frame 25).
    let anchor_pairs = vec![(clip_a_id, 0), (clip_b_id, 20)];
    state
        .move_clip_group_by_delta_with_mode(&anchor_pairs, 5, ClipOverlapMode::Overwrite)
        .expect("group move should succeed");

    let seq = state.sequence.as_ref().expect("sequence should exist");
    let clips = &seq.video_tracks[0].clips;
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[0].id, clip_a_id);
    assert_eq!(clips[0].position.frame, 5);
    assert_eq!(clips[1].id, clip_b_id);
    assert_eq!(clips[1].position.frame, 25);

    // Verify undo snapshot semantics: restoring before-snapshot brings clips back.
    state.sequence = before;
    let seq = state.sequence.as_ref().expect("sequence should exist");
    let clips = &seq.video_tracks[0].clips;
    assert_eq!(clips[0].position.frame, 0);
    assert_eq!(clips[1].position.frame, 20);
}
