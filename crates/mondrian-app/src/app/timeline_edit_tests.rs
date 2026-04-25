use super::*;
use mondrian_effects::EffectRenderOp;

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
    state.project_in_point = Some(10);
    state.project_out_point = Some(40);

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
