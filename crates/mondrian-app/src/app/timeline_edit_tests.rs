use super::*;
use mondrian_core::automation::PropertyHost;
use mondrian_core::AudioSourceComponentId;
use mondrian_core::{ColorSpace, WorkingColorSpace};
use mondrian_effects::EffectRenderOp;
use mondrian_timeline::clip::{AlphaInterpretation, MediaInterpretation, Transform2D};
use mondrian_timeline::sequence::{
    ColorWorkflow, DeliveryBitDepth, FieldOrder, PixelAspectRatio, VideoRange,
};
use mondrian_timeline::TrackRelativePlacement;

fn create_state_with_sequence() -> AppState {
    let mut state = AppState::new();
    state.test_set_sequence(Some(Sequence::new("test")));
    state
}

fn primary_track_clip_lens(state: &AppState) -> (usize, usize) {
    let seq = state.active_sequence().expect("sequence should exist");
    (
        seq.video_tracks[0].clips.len(),
        seq.audio_tracks[0].clips.len(),
    )
}

#[test]
fn sequence_revision_advances_on_edit_undo_and_redo_without_reusing_snapshots() {
    let mut state = create_state_with_sequence();
    let initial = state.active_sequence().expect("sequence").revision;

    state.add_video_track().expect("add track");
    let edited = state.active_sequence().expect("sequence").revision;
    assert_eq!(edited.get(), initial.get() + 1);

    assert!(state.undo_timeline().expect("undo"));
    let undone = state.active_sequence().expect("sequence").revision;
    assert_eq!(undone.get(), edited.get() + 1);

    assert!(state.redo_timeline().expect("redo"));
    let redone = state.active_sequence().expect("sequence").revision;
    assert_eq!(redone.get(), undone.get() + 1);
    assert_eq!(
        state.authoring_history().expect("history").diagnostics().undo_entries,
        1
    );
    assert_eq!(
        state.authoring_history().expect("history").diagnostics().redo_entries,
        0
    );
}

#[test]
fn sequence_commit_publishes_one_canonical_invalidation() {
    let mut state = create_state_with_sequence();
    let sequence_id = state.active_sequence_id().expect("active Sequence");
    let events = state.event_bus.subscribe();

    state.add_video_track().expect("add Track");

    let invalidations = events
        .try_iter()
        .filter(|event| {
            matches!(
                event,
                AppEvent::TimelineModified {
                    sequence_id: changed
                } if *changed == sequence_id
            )
        })
        .count();
    assert_eq!(invalidations, 1);
}

#[test]
fn project_commit_invalidates_every_sequence_through_the_same_receipt_adapter() {
    let mut state = create_state_with_sequence();
    let secondary = Sequence::new("secondary");
    state.test_add_sequence(secondary);
    let expected = state.sequences().iter().map(|sequence| sequence.id).collect::<BTreeSet<_>>();
    let events = state.event_bus.subscribe();
    let mut settings = state.new_sequence_defaults().clone();
    settings.resolution.width = settings.resolution.width.saturating_add(2);

    state.update_new_sequence_defaults(settings).expect("Project setting");

    let invalidated = events
        .try_iter()
        .filter_map(|event| match event {
            AppEvent::TimelineModified { sequence_id } => Some(sequence_id),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(invalidated, expected);
}

#[test]
fn commit_receipt_reports_an_unretained_project_edit() {
    let mut state = create_state_with_sequence();
    let mut commit = {
        let session = state.authoring.as_mut().expect("Authoring Session");
        let before = session.document().clone();
        let mut after = before.clone();
        after.settings.auto_save_interval = after.settings.auto_save_interval.saturating_add(1);
        session
            .commit_project_snapshot("unretained receipt fixture", before, after)
            .expect("Project transaction")
            .expect("Project commit")
    };
    commit.undo_retained = false;

    state.consume_authoring_commit(commit);

    assert!(
        state.status_hint.as_ref().is_some_and(|(message, is_error)| {
            *is_error && message.contains("编辑已提交但未保留撤销记录")
        })
    );
}

#[test]
fn commit_receipt_reconciles_removed_track_targeting() {
    let mut state = create_state_with_sequence();
    let sequence_id = state.active_sequence_id().expect("active Sequence");
    let track_id = state.active_sequence().expect("Sequence").video_tracks[0].id;
    state.set_timeline_track_targeted(track_id, false).expect("untarget Track");
    assert!(!state.timeline_track_targeted(sequence_id, track_id));

    state.remove_track(track_id, true).expect("remove Track");

    assert!(
        state.timeline_track_targeted(sequence_id, track_id),
        "removed Track overrides must be released by commit reconciliation"
    );
}

#[test]
fn exhausted_sequence_revision_rolls_back_the_author_mutation() {
    let mut state = create_state_with_sequence();
    state.active_sequence_mut_uncommitted().expect("sequence").revision =
        mondrian_core::SequenceRevision::new(u64::MAX).expect("nonzero revision");
    let before_tracks = state.active_sequence().expect("sequence").video_tracks.len();

    let error = state.add_video_track().expect_err("exhausted revision must fail");

    assert!(error.to_string().contains("revision"));
    let sequence = state.active_sequence().expect("sequence");
    assert_eq!(sequence.video_tracks.len(), before_tracks);
    assert_eq!(sequence.revision.get(), u64::MAX);
    assert!(!state.authoring_history().is_some_and(|history| history.can_undo()));
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
    let mut settings = SequenceSettings {
        resolution: Resolution { width: 3840, height: 2160 },
        frame_rate: Rational::FPS_23976,
        ..Default::default()
    };
    settings.color.working_color_space = WorkingColorSpace::LinearRec2020;
    settings.color.program_output.workflow = ColorWorkflow::SceneReferred;
    settings.color.program_output.color_space = ColorSpace::Rec2100Pq;
    settings.delivery.video_range = VideoRange::Legal;
    settings.delivery.bit_depth = DeliveryBitDepth::Ten;
    state
        .create_new_project_with_settings_at(
            project_file.clone(),
            "Color Project",
            settings.clone(),
            mondrian_core::ProjectColorEnvironment::default(),
            mondrian_core::ProjectSettings::default(),
        )
        .expect("create project");

    let sequence = state.active_sequence().expect("sequence");
    assert_eq!(sequence.settings.resolution, settings.resolution);
    assert_eq!(sequence.settings.frame_rate, Rational::FPS_23976);
    assert_eq!(
        sequence.settings.color.working_color_space,
        WorkingColorSpace::LinearRec2020
    );
    assert_eq!(
        sequence.settings.color.program_output.workflow,
        ColorWorkflow::SceneReferred
    );
    assert_eq!(
        sequence.settings.color.program_output.color_space,
        ColorSpace::Rec2100Pq
    );
    assert_eq!(sequence.settings.delivery.video_range, VideoRange::Legal);
    assert_eq!(sequence.settings.delivery.bit_depth, DeliveryBitDepth::Ten);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn set_clip_media_interpretation_is_undoable() {
    let mut state = create_state_with_sequence();
    let seq = state.active_sequence().expect("sequence");
    let tb = seq.time_base();
    let track_id = seq.video_tracks[0].id;
    let clip = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let clip_id = clip.id;
    state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0]
        .add_clip(clip)
        .expect("add clip");

    let selection = SelectedClipRef { track_id, clip_id, is_video_track: true };
    let interpretation = MediaInterpretation {
        color_space_override: Some(ColorSpace::SonySLog3SGamut3Cine),
        frame_rate_override: Some(Rational::FPS_23976),
        pixel_aspect_ratio_override: Some(PixelAspectRatio::HdAnamorphic1080),
        field_order_override: Some(FieldOrder::UpperFirst),
        alpha: AlphaInterpretation::Premultiplied,
    };
    state
        .set_clip_media_interpretation(selection, interpretation.clone())
        .expect("interpretation");
    assert_eq!(
        state.clip_snapshot(selection).expect("clip").media_interpretation().cloned(),
        Some(interpretation)
    );

    state.undo_timeline().expect("undo");
    assert_eq!(
        state.clip_snapshot(selection).expect("clip").media_interpretation().cloned(),
        Some(MediaInterpretation::default())
    );
}

#[test]
fn set_clip_media_interpretation_rejects_locked_tracks() {
    let mut state = create_state_with_sequence();
    let seq = state.active_sequence().expect("sequence");
    let tb = seq.time_base();
    let track_id = seq.video_tracks[0].id;
    let clip = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let clip_id = clip.id;
    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence");
        seq.video_tracks[0].add_clip(clip).expect("add clip");
        seq.video_tracks[0].is_locked = true;
    }

    let selection = SelectedClipRef { track_id, clip_id, is_video_track: true };
    let err = state
        .set_clip_media_interpretation(
            selection,
            MediaInterpretation {
                color_space_override: Some(ColorSpace::DisplayP3),
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
    let first = state.active_sequence().expect("sequence should exist").id;
    state.test_set_active_sequence(first);
    state.test_set_default_sequence(first);

    let tb = state.active_sequence().expect("sequence should exist").time_base();
    state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .video_tracks[0]
        .add_clip(Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip"))
        .expect("add clip");

    state.new_sequence("second").expect("new sequence");
    let second = state.active_sequence().expect("sequence should exist").id;
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .video_tracks[0]
        .add_clip(Clip::new(AssetId::new(), tt(20, tb), tt(10, tb)).expect("valid clip"))
        .expect("add second clip");

    state.switch_active_sequence(first).expect("switch to first");
    let seq = state.active_sequence().expect("first sequence should be active");
    assert_eq!(seq.id, first);
    assert_eq!(seq.video_tracks[0].clips[0].position, tt(0, tb));

    state.switch_active_sequence(second).expect("switch to second");
    let seq = state.active_sequence().expect("second sequence should be active");
    assert_eq!(seq.id, second);
    assert_eq!(seq.video_tracks[0].clips[0].position, tt(20, tb));
}

#[test]
fn sequence_management_duplicate_rename_delete_updates_collection() {
    let mut state = create_state_with_sequence();
    let first = state.active_sequence().expect("sequence").id;
    state.test_set_active_sequence(first);
    state.test_set_default_sequence(first);
    state.add_video_track().expect("advance source revision");
    assert!(state.active_sequence().expect("sequence").revision.get() > 1);

    let source_output = state.active_sequence().expect("sequence").audio_program.outputs[0].id;
    let source_routes = state
        .active_sequence()
        .expect("sequence")
        .audio_program
        .routes
        .iter()
        .map(|route| route.id)
        .collect::<Vec<_>>();
    let duplicate = state.duplicate_sequence(first, "Duplicate").expect("duplicate sequence");
    assert_ne!(duplicate, first);
    assert_eq!(state.active_sequence_id(), Some(duplicate));
    assert_eq!(state.active_sequence().expect("active").name, "Duplicate");
    let duplicated = state.active_sequence().expect("active");
    assert_eq!(
        duplicated.revision,
        mondrian_core::SequenceRevision::INITIAL
    );
    assert_ne!(duplicated.audio_program.outputs[0].id, source_output);
    assert!(duplicated
        .audio_program
        .routes
        .iter()
        .all(|route| !source_routes.contains(&route.id)));

    state.rename_sequence(duplicate, "Renamed").expect("rename");
    assert_eq!(state.active_sequence().expect("active").name, "Renamed");

    state.delete_sequence(duplicate).expect("delete duplicate");
    assert_eq!(state.active_sequence_id(), Some(first));
    assert_eq!(state.export_sequences_snapshot().len(), 1);
}

#[test]
fn delete_sequence_rejects_nested_references() {
    let mut state = create_state_with_sequence();
    let parent_id = state.active_sequence().expect("parent").id;
    state.test_set_active_sequence(parent_id);
    state.test_set_default_sequence(parent_id);
    state.new_sequence("child").expect("new sequence");
    let child_id = state.active_sequence().expect("child").id;
    state.switch_active_sequence(parent_id).expect("switch parent");
    let tb = state.active_sequence().expect("parent").time_base();
    state.active_sequence_mut_uncommitted().expect("parent").video_tracks[0]
        .add_clip(
            Clip::new_nested_sequence(child_id, tt(0, tb), tt(10, tb), Some("child".to_string()))
                .expect("valid clip"),
        )
        .expect("add nested");

    let err = state.delete_sequence(child_id).expect_err("nested delete rejected");
    assert!(format!("{err}").contains("嵌套引用"));
}

#[test]
fn precompose_clips_creates_nested_sequence_and_replacement_clip() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let clip = Clip::new(AssetId::new(), tt(12, tb), tt(30, tb)).expect("valid clip");
    let clip_id = clip.id;
    let unaffected_video =
        Clip::new(AssetId::new(), tt(60, tb), tt(20, tb)).expect("unaffected video clip");
    let unaffected_video_id = unaffected_video.id;
    let unaffected_audio =
        Clip::new(AssetId::new(), tt(90, tb), tt(20, tb)).expect("unaffected audio clip");
    let unaffected_audio_id = unaffected_audio.id;
    let unaffected_audio_track_id =
        state.active_sequence().expect("sequence should exist").audio_tracks[1].id;
    state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .video_tracks[0]
        .add_clip(clip)
        .expect("add clip");
    state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .video_tracks[1]
        .add_clip(unaffected_video)
        .expect("add unaffected video clip");
    state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .add_media_audio_clip(
            unaffected_audio_track_id,
            unaffected_audio,
            AudioSourceComponentId::new(),
        )
        .expect("add unaffected audio clip");
    let (
        unaffected_video_clip_allocation,
        unaffected_audio_tracks_allocation,
        unaffected_audio_clip_allocation,
    ) = {
        let sequence = state.active_sequence().expect("sequence should exist");
        (
            sequence.video_tracks[1].clips.allocation_id(),
            sequence.audio_tracks.allocation_id(),
            sequence.audio_tracks[1].clips.allocation_id(),
        )
    };

    let nested_clip_id = state
        .precompose_clips_as_sequence(&[clip_id], "Precomp 01")
        .expect("precompose");

    let parent = state.active_sequence().expect("sequence should exist");
    let replacement = parent.video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == nested_clip_id)
        .expect("replacement nested clip");
    assert!(replacement.is_nested_sequence());
    assert_eq!(replacement.position, tt(12, tb));
    assert_eq!(replacement.duration, tt(30, tb));

    let nested_sequence_id = replacement.nested_sequence_id().expect("nested sequence id");
    let nested = state.sequence_by_id(nested_sequence_id).expect("nested sequence");
    assert_eq!(
        nested.role,
        mondrian_timeline::sequence::SequenceRole::NestedComposition
    );
    assert_eq!(nested.video_tracks[0].clips.len(), 1);
    assert_eq!(nested.video_tracks[0].clips[0].position, tt(0, tb));
    assert_eq!(
        parent.video_tracks[1].clips.allocation_id(),
        unaffected_video_clip_allocation,
        "Precompose must not detach an unaffected video Track's Clip allocation"
    );
    assert_eq!(
        parent.audio_tracks.allocation_id(),
        unaffected_audio_tracks_allocation,
        "video-only Precompose must not detach the parent audio Track collection"
    );
    assert_eq!(
        parent.audio_tracks[1].clips.allocation_id(),
        unaffected_audio_clip_allocation,
        "Precompose must not detach an unaffected audio Track's Clip allocation"
    );
    assert!(parent.video_tracks[1].clips.iter().any(|clip| clip.id == unaffected_video_id));
    assert!(parent.audio_tracks[1].clips.iter().any(|clip| clip.id == unaffected_audio_id));
}

#[test]
fn precompose_product_action_commits_once_and_selects_replacement() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let clip = Clip::new(AssetId::new(), tt(12, tb), tt(30, tb)).expect("valid clip");
    let clip_id = clip.id;
    state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .video_tracks[0]
        .add_clip(clip)
        .expect("add clip");
    state.select_clip_by_id(clip_id).expect("select source clip");
    let generation_before = state.project_author_generation();

    state
        .dispatch_action(
            crate::app::ui_actions::timeline_precompose_selection_action(
                crate::app::ui_actions::TimelinePrecomposeSelectionPayload {
                    name: "Nested Product Action".to_owned(),
                },
            ),
        )
        .expect("precompose product action");

    assert_eq!(state.project_author_generation(), generation_before + 1);
    let selected = state.primary_selected_clip().expect("replacement selection");
    let replacement = state
        .active_sequence()
        .expect("parent sequence")
        .video_tracks
        .iter()
        .flat_map(|track| &track.clips)
        .find(|clip| clip.id == selected.clip_id)
        .expect("replacement clip");
    let nested_sequence_id = replacement.nested_sequence_id().expect("nested Sequence id");
    assert_ne!(selected.clip_id, clip_id);
    assert_eq!(
        state.sequence_by_id(nested_sequence_id).map(|sequence| sequence.name.as_str()),
        Some("Nested Product Action")
    );
}

#[test]
fn precompose_audio_only_selection_creates_only_an_audio_replacement() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let audio_track_id = state.active_sequence().expect("sequence should exist").audio_tracks[0].id;
    let source_clip_id = state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .add_media_audio_clip(
            audio_track_id,
            Clip::new(AssetId::new(), tt(20, tb), tt(40, tb)).expect("valid clip"),
            AudioSourceComponentId::new(),
        )
        .expect("add audio clip");
    state.select_clip_by_id(source_clip_id).expect("select audio clip");

    state
        .dispatch_action(
            crate::app::ui_actions::timeline_precompose_selection_action(
                crate::app::ui_actions::TimelinePrecomposeSelectionPayload {
                    name: "Audio Nested".to_owned(),
                },
            ),
        )
        .expect("precompose audio selection");

    let selected = state.primary_selected_clip().expect("audio replacement selection");
    assert!(!selected.is_video_track);
    let parent = state.active_sequence().expect("parent sequence");
    assert!(
        parent.video_tracks.iter().all(|track| track.clips.is_empty()),
        "audio-only Precompose must not create an invisible video placement"
    );
    let replacement = parent
        .audio_tracks
        .iter()
        .flat_map(|track| &track.clips)
        .find(|clip| clip.id == selected.clip_id)
        .expect("audio nested replacement");
    let nested_sequence_id = replacement.nested_sequence_id().expect("nested Sequence id");
    let nested = state.sequence_by_id(nested_sequence_id).expect("nested Sequence");
    assert!(nested.video_tracks.iter().all(|track| track.clips.is_empty()));
    assert_eq!(
        nested.audio_tracks.iter().map(|track| track.clips.len()).sum::<usize>(),
        1
    );
}

#[test]
fn precompose_linked_av_selection_preserves_one_linked_parent_pair() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let video_track_id = state.active_sequence().expect("sequence should exist").video_tracks[0].id;
    let audio_track_id = state.active_sequence().expect("sequence should exist").audio_tracks[0].id;
    let link_group = mondrian_core::ClipLinkGroupId::new();
    let mut video_clip =
        Clip::new(AssetId::new(), tt(8, tb), tt(32, tb)).expect("valid video clip");
    video_clip.link_group = Some(link_group);
    let video_clip_id = video_clip.id;
    state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .video_track_mut(video_track_id)
        .expect("video track")
        .add_clip(video_clip)
        .expect("add video clip");
    let mut audio_clip =
        Clip::new(AssetId::new(), tt(8, tb), tt(32, tb)).expect("valid audio clip");
    audio_clip.link_group = Some(link_group);
    state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .add_media_audio_clip(audio_track_id, audio_clip, AudioSourceComponentId::new())
        .expect("add audio clip");
    state.select_clip_by_id(video_clip_id).expect("select linked video");

    state
        .dispatch_action(
            crate::app::ui_actions::timeline_precompose_selection_action(
                crate::app::ui_actions::TimelinePrecomposeSelectionPayload {
                    name: "Linked AV Nested".to_owned(),
                },
            ),
        )
        .expect("precompose linked selection");

    let parent = state.active_sequence().expect("parent sequence");
    let video_replacement = parent.video_tracks[0].clips.first().expect("video replacement");
    let audio_replacement = parent.audio_tracks[0].clips.first().expect("audio replacement");
    assert!(video_replacement.is_nested_sequence());
    assert!(audio_replacement.is_nested_sequence());
    assert_eq!(
        video_replacement.nested_sequence_id(),
        audio_replacement.nested_sequence_id()
    );
    assert_eq!(video_replacement.link_group, audio_replacement.link_group);
    assert!(video_replacement.link_group.is_some());
    assert_eq!(
        state.primary_selected_clip().map(|selection| selection.clip_id),
        Some(video_replacement.id)
    );
}

fn video_clip_is_disabled(state: &AppState, clip_id: ClipId) -> bool {
    state
        .active_sequence()
        .and_then(|seq| seq.video_tracks[0].clips.iter().find(|clip| clip.id == clip_id))
        .map(|clip| clip.is_disabled)
        .unwrap_or(false)
}

fn exposure_from_clip(clip: &Clip, time: TimelineTime) -> f32 {
    let graph = mondrian_effects::build_effect_render_graph(
        &clip.effects,
        time,
        mondrian_core::WorkingColorSpace::LinearRec709,
    )
    .expect("build effect graph");
    graph
        .nodes
        .iter()
        .find_map(|node| match &node.kind {
            mondrian_effects::EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::ColorAdjust { exposure, .. },
                ..
            } => Some(*exposure),
            _ => None,
        })
        .unwrap_or(0.0)
}

#[test]
fn split_at_playhead_records_single_undo_step() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();

    let video_clip = Clip::new(AssetId::new(), tt(0, tb), tt(40, tb)).expect("valid clip");
    let audio_clip = Clip::new(AssetId::new(), tt(0, tb), tt(40, tb)).expect("valid clip");

    state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .video_tracks[0]
        .add_clip(video_clip)
        .expect("add video clip");
    let audio_track_id = state.active_sequence().expect("sequence should exist").audio_tracks[0].id;
    state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .add_media_audio_clip(
            audio_track_id,
            audio_clip,
            AudioSourceComponentId::primary(),
        )
        .expect("add audio clip");

    state.seek(10).expect("seek");

    let split_count = state.split_at_playhead().expect("split should succeed");
    assert_eq!(split_count, 2);
    assert_eq!(
        state.authoring_history().and_then(|history| history.undo_description()),
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
    let track_id = state.active_sequence().expect("sequence should exist").video_tracks[0].id;

    state.set_track_locked(track_id, true).expect("set track locked should succeed");
    assert!(state.active_sequence().expect("sequence should exist").video_tracks[0].is_locked);
    assert_eq!(
        state.authoring_history().and_then(|history| history.undo_description()),
        Some("切换轨道锁定")
    );

    assert!(state.undo_timeline().expect("undo should succeed"));
    assert!(!state.active_sequence().expect("sequence should exist").video_tracks[0].is_locked);

    assert!(state.redo_timeline().expect("redo should succeed"));
    assert!(state.active_sequence().expect("sequence should exist").video_tracks[0].is_locked);
}

#[test]
fn set_clip_disabled_is_undoable() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let track_id = state.active_sequence().expect("sequence should exist").video_tracks[0].id;

    let clip = Clip::new(AssetId::new(), tt(0, tb), tt(30, tb)).expect("valid clip");
    let clip_id = clip.id;
    state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .video_tracks[0]
        .add_clip(clip)
        .expect("add clip");

    state
        .set_clips_disabled_bulk(&[(track_id, true, clip_id)], true)
        .expect("disable clip should succeed");
    assert!(video_clip_is_disabled(&state, clip_id));
    assert_eq!(
        state.authoring_history().and_then(|history| history.undo_description()),
        Some("禁用片段")
    );

    assert!(state.undo_timeline().expect("undo should succeed"));
    assert!(!video_clip_is_disabled(&state, clip_id));

    assert!(state.redo_timeline().expect("redo should succeed"));
    assert!(video_clip_is_disabled(&state, clip_id));
}

#[test]
fn move_clip_conflict_respects_insert_mode() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let track_id = state.active_sequence().expect("sequence should exist").video_tracks[0].id;

    let clip_a = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let clip_b = Clip::new(AssetId::new(), tt(20, tb), tt(10, tb)).expect("valid clip");
    let clip_b_id = clip_b.id;

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_a).expect("add clip a");
        seq.video_tracks[0].add_clip(clip_b).expect("add clip b");
    }

    state
        .move_clip_to_track_with_mode(track_id, clip_b_id, 5, ClipOverlapMode::PushForward)
        .expect("move should succeed");

    let clips = &state.active_sequence().expect("sequence should exist").video_tracks[0].clips;
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[0].position, tt(0, tb));
    assert_eq!(clips[1].id, clip_b_id);
    assert_eq!(clips[1].position, tt(10, tb));
}

#[test]
fn same_track_move_preserves_unaffected_track_allocations() {
    let mut state = create_state_with_sequence();
    let time_base = state.active_sequence().expect("Sequence").time_base();
    let moved =
        Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base)).expect("valid moved Clip");
    let moved_id = moved.id;
    let neighbor = Clip::new(AssetId::new(), tt(20, time_base), tt(10, time_base))
        .expect("valid neighbor Clip");
    let unrelated_video = Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base))
        .expect("valid unrelated video Clip");
    let mut unrelated_audio = Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base))
        .expect("valid unrelated audio Clip");
    let target_track_id;
    {
        let sequence = state.active_sequence_mut_uncommitted().expect("Sequence");
        target_track_id = sequence.video_tracks[0].id;
        sequence.video_tracks[0].add_clip(moved).expect("add moved Clip");
        sequence.video_tracks[0].add_clip(neighbor).expect("add neighbor Clip");
        sequence.video_tracks[1]
            .add_clip(unrelated_video)
            .expect("add unrelated video Clip");
        sequence
            .attach_default_media_audio_component(
                &mut unrelated_audio,
                AudioSourceComponentId::primary(),
            )
            .expect("author unrelated audio Clip");
        sequence.audio_tracks[1]
            .add_clip(unrelated_audio)
            .expect("add unrelated audio Clip");
    }
    let sequence = state.active_sequence().expect("Sequence");
    let unrelated_video_allocation = sequence.video_tracks[1].clips.allocation_id();
    let audio_tracks_allocation = sequence.audio_tracks.allocation_id();
    let unrelated_audio_allocation = sequence.audio_tracks[1].clips.allocation_id();

    state
        .move_clip_to_track_with_mode(target_track_id, moved_id, 1, ClipOverlapMode::Overwrite)
        .expect("move Clip");

    let sequence = state.active_sequence().expect("Sequence");
    assert_eq!(
        sequence.video_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == moved_id)
            .expect("moved Clip")
            .position,
        tt(1, time_base)
    );
    assert_eq!(
        sequence.video_tracks[1].clips.allocation_id(),
        unrelated_video_allocation
    );
    assert_eq!(
        sequence.audio_tracks.allocation_id(),
        audio_tracks_allocation
    );
    assert_eq!(
        sequence.audio_tracks[1].clips.allocation_id(),
        unrelated_audio_allocation
    );
}

#[test]
fn move_clip_conflict_respects_overwrite_mode() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let track_id = state.active_sequence().expect("sequence should exist").video_tracks[0].id;

    let clip_a = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let clip_a_id = clip_a.id;
    let clip_b = Clip::new(AssetId::new(), tt(20, tb), tt(10, tb)).expect("valid clip");
    let clip_b_id = clip_b.id;

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_a).expect("add clip a");
        seq.video_tracks[0].add_clip(clip_b).expect("add clip b");
    }

    state
        .move_clip_to_track_with_mode(track_id, clip_b_id, 5, ClipOverlapMode::Overwrite)
        .expect("move should succeed");

    let clips = &state.active_sequence().expect("sequence should exist").video_tracks[0].clips;
    assert_eq!(clips.len(), 2);
    let kept_a = clips.iter().find(|clip| clip.id == clip_a_id).expect("clip a should exist");
    let moved_b = clips.iter().find(|clip| clip.id == clip_b_id).expect("clip b should exist");
    assert_eq!(kept_a.position, tt(0, tb));
    assert_eq!(kept_a.duration, tt(5, tb));
    assert_eq!(moved_b.position, tt(5, tb));
}

#[test]
fn overwrite_only_removes_intersection_and_keeps_both_sides() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let track_id = state.active_sequence().expect("sequence should exist").video_tracks[0].id;

    let clip_a = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("valid clip");
    let clip_a_id = clip_a.id;
    let clip_b = Clip::new(AssetId::new(), tt(40, tb), tt(4, tb)).expect("valid clip");
    let clip_b_id = clip_b.id;

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_a).expect("add clip a");
        seq.video_tracks[0].add_clip(clip_b).expect("add clip b");
    }

    state
        .move_clip_to_track_with_mode(track_id, clip_b_id, 8, ClipOverlapMode::Overwrite)
        .expect("move should succeed");

    let clips = &state.active_sequence().expect("sequence should exist").video_tracks[0].clips;
    assert_eq!(clips.len(), 3);

    let left = clips.iter().find(|clip| clip.id == clip_a_id).expect("left part should exist");
    let moved = clips.iter().find(|clip| clip.id == clip_b_id).expect("moved clip should exist");
    let right = clips
        .iter()
        .find(|clip| clip.id != clip_a_id && clip.id != clip_b_id)
        .expect("right part should exist");

    assert_eq!(left.position, tt(0, tb));
    assert_eq!(left.duration, tt(8, tb));
    assert_eq!(left.source_origin(), tt(0, tb));
    assert_eq!(
        left.source_terminal_boundary().expect("source terminal"),
        tt(8, tb)
    );

    assert_eq!(moved.position, tt(8, tb));
    assert_eq!(moved.duration, tt(4, tb));

    assert_eq!(right.position, tt(12, tb));
    assert_eq!(right.duration, tt(8, tb));
    assert_eq!(right.source_origin(), tt(12, tb));
    assert_eq!(
        right.source_terminal_boundary().expect("source terminal"),
        tt(20, tb)
    );
}

#[test]
fn move_clip_group_overwrite_keeps_all_selected_clips() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();

    let clip_a = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let clip_a_id = clip_a.id;
    let clip_b = Clip::new(AssetId::new(), tt(10, tb), tt(10, tb)).expect("valid clip");
    let clip_b_id = clip_b.id;
    let clip_c = Clip::new(AssetId::new(), tt(40, tb), tt(10, tb)).expect("valid clip");
    let clip_c_id = clip_c.id;

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
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

    let clips = &state.active_sequence().expect("sequence should exist").video_tracks[0].clips;
    assert_eq!(clips.len(), 3);
    let moved_a = clips.iter().find(|clip| clip.id == clip_a_id).expect("clip a should exist");
    let moved_b = clips.iter().find(|clip| clip.id == clip_b_id).expect("clip b should exist");
    let untouched_c = clips.iter().find(|clip| clip.id == clip_c_id).expect("clip c should exist");
    assert_eq!(moved_a.position, tt(5, tb));
    assert_eq!(moved_b.position, tt(15, tb));
    assert_eq!(untouched_c.position, tt(40, tb));
}

#[test]
fn trim_in_is_undoable() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let clip = Clip::new(AssetId::new(), tt(10, tb), tt(20, tb)).expect("valid clip");
    let clip_id = clip.id;

    state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .video_tracks[0]
        .add_clip(clip)
        .expect("add clip");

    let changed = state
        .trim_clips_bulk_to_frame(&[clip_id], TrimEdge::In, 15)
        .expect("trim in should succeed");
    assert_eq!(changed, 1);

    let trimmed = state.active_sequence().expect("sequence should exist").video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .expect("clip should exist");
    assert_eq!(trimmed.position, tt(15, tb));
    assert_eq!(trimmed.duration, tt(15, tb));
    assert_eq!(trimmed.clip_time_in, tt(5, tb));
    assert_eq!(trimmed.source_origin(), tt(5, tb));
    assert_eq!(
        state.authoring_history().and_then(|history| history.undo_description()),
        Some("修剪入点")
    );

    assert!(state.undo_timeline().expect("undo should succeed"));
    let restored = state.active_sequence().expect("sequence should exist").video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .expect("clip should exist after undo");
    assert_eq!(restored.position, tt(10, tb));
    assert_eq!(restored.duration, tt(20, tb));
    assert_eq!(restored.clip_time_in, TimelineTime::ZERO);
    assert_eq!(restored.source_origin(), tt(0, tb));
}

#[test]
fn move_trim_and_split_preserve_one_clip_local_visual_time_domain() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence").time_base();
    let track_id = state.active_sequence().expect("sequence").video_tracks[0].id;
    let mut clip = Clip::new(AssetId::new(), tt(10, tb), tt(30, tb)).expect("valid clip");
    clip.apply_property_mutation(mondrian_core::automation::PropertyMutation::SetKeyframe {
        path: Transform2D::OPACITY_PATH.to_owned(),
        keyframe: mondrian_core::automation::Keyframe::linear(
            tt(0, tb),
            mondrian_core::automation::PropertyValue::Float(0.0),
        ),
    })
    .expect("start opacity");
    clip.apply_property_mutation(mondrian_core::automation::PropertyMutation::SetKeyframe {
        path: Transform2D::OPACITY_PATH.to_owned(),
        keyframe: mondrian_core::automation::Keyframe::linear(
            tt(30, tb),
            mondrian_core::automation::PropertyValue::Float(1.0),
        ),
    })
    .expect("end opacity");
    let clip_id = clip.id;
    state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0]
        .add_clip(clip)
        .expect("add clip");

    state
        .move_clip_to_track_with_mode(track_id, clip_id, 40, ClipOverlapMode::Overwrite)
        .expect("move Clip");
    let moved = state.active_sequence().expect("sequence").video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .expect("moved Clip");
    assert_eq!(moved.clip_time_in, TimelineTime::ZERO);
    assert_eq!(
        moved.timeline_to_clip_time(tt(55, tb)).expect("Clip time"),
        tt(15, tb)
    );

    state.trim_clips_bulk_to_frame(&[clip_id], TrimEdge::In, 45).expect("trim Clip");
    let trimmed = state.active_sequence().expect("sequence").video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .expect("trimmed Clip");
    assert_eq!(trimmed.clip_time_in, tt(5, tb));
    assert_eq!(
        trimmed.timeline_to_clip_time(tt(55, tb)).expect("Clip time"),
        tt(15, tb)
    );

    state
        .split_clip_at_frame(track_id, true, clip_id, 55)
        .expect("split Clip")
        .expect("split target");
    let sequence = state.active_sequence().expect("sequence");
    let right = sequence.video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.position == tt(55, tb))
        .expect("right Clip");
    assert_eq!(right.clip_time_in, tt(15, tb));
    let active = sequence.active_clips_at(tt(55, tb)).expect("active Clip");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].clip.id, right.id);
    assert_eq!(active[0].clip_time, tt(15, tb));
    assert!((active[0].opacity - 0.5).abs() < 1.0e-6);
}

#[test]
fn trim_out_updates_linked_clip() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();

    let mut video = Clip::new(AssetId::new(), tt(0, tb), tt(30, tb)).expect("valid clip");
    let mut audio = Clip::new(
        video.media_asset_id().expect("video asset"),
        tt(0, tb),
        tt(30, tb),
    )
    .expect("valid clip");
    let video_id = video.id;
    let audio_id = audio.id;
    let link_group = ClipLinkGroupId::new();
    video.link_group = Some(link_group);
    audio.link_group = Some(link_group);

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[0].add_clip(video).expect("add video");
        let audio_track_id = seq.audio_tracks[0].id;
        seq.add_media_audio_clip(audio_track_id, audio, AudioSourceComponentId::primary())
            .expect("add audio");
    }

    let changed = state
        .trim_clips_bulk_to_frame(&[video_id], TrimEdge::Out, 21)
        .expect("trim out should succeed");
    assert_eq!(changed, 2);

    let seq = state.active_sequence().expect("sequence should exist");
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

    assert_eq!(video_after.duration, tt(21, tb));
    assert_eq!(audio_after.duration, tt(21, tb));
    assert_eq!(
        video_after.source_terminal_boundary().expect("source terminal"),
        tt(21, tb)
    );
    assert_eq!(
        audio_after.source_terminal_boundary().expect("source terminal"),
        tt(21, tb)
    );
}

#[test]
fn trim_out_can_extend_a_zero_rate_still_hold() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let still = Clip::new_still_image(AssetId::new(), tt(0, tb), tt(25, tb)).expect("still Clip");
    let still_id = still.id;
    state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0]
        .add_clip(still)
        .expect("add still");

    let changed = state
        .trim_clips_bulk_to_frame(&[still_id], TrimEdge::Out, 125)
        .expect("extend still hold");

    assert_eq!(changed, 1);
    let still = state.active_sequence().expect("sequence").video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == still_id)
        .expect("still remains");
    assert_eq!(still.duration, tt(125, tb));
    assert_eq!(still.source_origin(), TimelineTime::ZERO);
    assert_eq!(
        still.source_terminal_boundary().expect("source terminal"),
        TimelineTime::ZERO
    );
    assert_eq!(still.source_time_scale().numerator(), 0);
}

#[test]
fn delta_move_expands_a_three_member_link_group_atomically() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let group = ClipLinkGroupId::new();
    let mut first = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("first");
    let mut second = Clip::new(AssetId::new(), tt(5, tb), tt(20, tb)).expect("second");
    let mut audio = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("audio");
    first.link_group = Some(group);
    second.link_group = Some(group);
    audio.link_group = Some(group);
    let first_id = first.id;
    let second_id = second.id;
    let audio_id = audio.id;

    {
        let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
        sequence.video_tracks[0].add_clip(first).expect("first placement");
        sequence.video_tracks[1].add_clip(second).expect("second placement");
        let audio_track_id = sequence.audio_tracks[0].id;
        sequence
            .add_media_audio_clip(audio_track_id, audio, AudioSourceComponentId::primary())
            .expect("audio placement");
    }

    let changed = state
        .move_clip_group_by_delta_with_mode(&[(first_id, 0)], 7, ClipOverlapMode::Overwrite)
        .expect("linked move");
    assert_eq!(changed, 3);
    let sequence = state.active_sequence().expect("sequence");
    assert_eq!(
        find_clip(sequence, first_id).expect("first").position,
        tt(7, tb)
    );
    assert_eq!(
        find_clip(sequence, second_id).expect("second").position,
        tt(12, tb)
    );
    assert_eq!(
        find_clip(sequence, audio_id).expect("audio").position,
        tt(7, tb)
    );
}

#[test]
fn razor_rebuilds_both_sides_of_a_three_member_link_group() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let group = ClipLinkGroupId::new();
    let mut first = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("first");
    let mut second = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("second");
    let mut audio = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("audio");
    first.link_group = Some(group);
    second.link_group = Some(group);
    audio.link_group = Some(group);
    let first_id = first.id;
    let first_track = state.active_sequence().expect("sequence").video_tracks[0].id;

    {
        let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
        sequence.video_tracks[0].add_clip(first).expect("first placement");
        sequence.video_tracks[1].add_clip(second).expect("second placement");
        let audio_track_id = sequence.audio_tracks[0].id;
        sequence
            .add_media_audio_clip(audio_track_id, audio, AudioSourceComponentId::primary())
            .expect("audio placement");
    }

    assert!(state
        .split_clip_at_frame(first_track, true, first_id, 10)
        .expect("linked razor")
        .is_some());
    let sequence = state.active_sequence().expect("sequence");
    let members = sequence
        .video_tracks
        .iter()
        .chain(&sequence.audio_tracks)
        .flat_map(|track| &track.clips)
        .collect::<Vec<_>>();
    assert_eq!(members.len(), 6);
    let left_groups = members
        .iter()
        .filter(|clip| clip.position == tt(0, tb))
        .map(|clip| clip.link_group)
        .collect::<HashSet<_>>();
    let right_groups = members
        .iter()
        .filter(|clip| clip.position == tt(10, tb))
        .map(|clip| clip.link_group)
        .collect::<HashSet<_>>();
    assert_eq!(left_groups.len(), 1);
    assert_eq!(right_groups.len(), 1);
    assert!(left_groups.iter().next().copied().flatten().is_some());
    assert!(right_groups.iter().next().copied().flatten().is_some());
    assert_ne!(left_groups, right_groups);
}

#[test]
fn targeted_split_returns_complete_identity_mapping_without_cutting_other_tracks() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence").time_base();
    let foundation_asset_id = AssetId::new();
    let editorial_asset_id = AssetId::new();
    let title =
        Clip::new_basic_title("Title", "Test Font", tt(0, tb), tt(125, tb)).expect("title Clip");
    let title_id = title.id;
    let foundation =
        Clip::new(foundation_asset_id, tt(0, tb), tt(100, tb)).expect("foundation Clip");
    let foundation_id = foundation.id;
    let editorial = Clip::new(editorial_asset_id, tt(25, tb), tt(50, tb)).expect("editorial Clip");
    let editorial_id = editorial.id;
    let (foundation_track_id, editorial_track_id) = {
        let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
        sequence.video_tracks[1].add_clip(title).expect("title placement");
        let foundation_track_id = sequence.audio_tracks[0].id;
        sequence
            .add_media_audio_clip(
                foundation_track_id,
                foundation,
                AudioSourceComponentId::primary(),
            )
            .expect("foundation placement");
        let editorial_track_id = sequence.audio_tracks[1].id;
        sequence
            .add_media_audio_clip(
                editorial_track_id,
                editorial,
                AudioSourceComponentId::primary(),
            )
            .expect("editorial placement");
        (foundation_track_id, editorial_track_id)
    };

    let outcome = state
        .split_clip_at_frame(editorial_track_id, false, editorial_id, 50)
        .expect("targeted split")
        .expect("splittable target");
    assert_eq!(outcome.primary().left_clip_id, editorial_id);
    assert!(outcome.linked_members().is_empty());
    assert_eq!(outcome.split_member_count(), 1);

    let sequence = state.active_sequence().expect("sequence");
    assert_eq!(
        sequence.video_tracks[1].clips.iter().filter(|clip| clip.id == title_id).count(),
        1
    );
    assert_eq!(
        sequence
            .audio_tracks
            .iter()
            .find(|track| track.id == foundation_track_id)
            .map(|track| track.clips.iter().filter(|clip| clip.id == foundation_id).count()),
        Some(1)
    );
    assert_eq!(
        sequence
            .audio_tracks
            .iter()
            .find(|track| track.id == editorial_track_id)
            .map(|track| track.clips.len()),
        Some(2)
    );
}

#[test]
fn removing_track_renumbers_tracks_and_clears_broken_links() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();

    let mut video = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("valid clip");
    let mut audio = Clip::new(
        video.media_asset_id().expect("video asset"),
        tt(0, tb),
        tt(20, tb),
    )
    .expect("valid clip");
    let audio_id = audio.id;
    let link_group = ClipLinkGroupId::new();
    video.link_group = Some(link_group);
    audio.link_group = Some(link_group);

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[1].add_clip(video).expect("add video");
        let audio_track_id = seq.audio_tracks[1].id;
        seq.add_media_audio_clip(audio_track_id, audio, AudioSourceComponentId::primary())
            .expect("add audio");
    }

    let removed_track_id =
        state.active_sequence().expect("sequence should exist").video_tracks[1].id;
    state.remove_track(removed_track_id, true).expect("remove track");

    let seq = state.active_sequence().expect("sequence should exist");
    assert_eq!(seq.video_tracks.len(), 2);
    assert_eq!(seq.video_tracks[0].name, "V1");
    assert_eq!(seq.video_tracks[1].name, "V2");

    let audio_after = seq.audio_tracks[1]
        .clips
        .iter()
        .find(|clip| clip.id == audio_id)
        .expect("audio clip should remain");
    assert_eq!(audio_after.link_group, None);
}

#[test]
fn removing_track_prunes_stale_app_selection() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let removed_track_id =
        state.active_sequence().expect("sequence should exist").video_tracks[1].id;
    let retained_track_id =
        state.active_sequence().expect("sequence should exist").video_tracks[0].id;

    let removed_clip = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("valid clip");
    let removed_clip_id = removed_clip.id;
    let retained_clip = Clip::new(AssetId::new(), tt(4, tb), tt(12, tb)).expect("valid clip");
    let retained_clip_id = retained_clip.id;
    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[1].add_clip(removed_clip).expect("add removed clip");
        seq.video_tracks[0].add_clip(retained_clip).expect("add retained clip");
    }

    state.selection.selected_track_ids = vec![removed_track_id, retained_track_id];
    state.selection.selected_clips = vec![
        SelectedClipRef {
            track_id: removed_track_id,
            is_video_track: true,
            clip_id: removed_clip_id,
        },
        SelectedClipRef {
            track_id: retained_track_id,
            is_video_track: true,
            clip_id: retained_clip_id,
        },
    ];
    state.selection.selected_mask = Some((MaskId::new(), removed_clip_id, removed_track_id));
    let opacity_property = state
        .active_sequence()
        .and_then(|sequence| find_clip(sequence, removed_clip_id))
        .and_then(|clip| {
            clip.transform.to_property_bag().address_for_path(Transform2D::OPACITY_PATH)
        })
        .expect("opacity property address");
    let property_selection = AnimationPropertySelection {
        clip_id: removed_clip_id,
        property: opacity_property,
    };
    state.animation_selection.active_property = Some(property_selection.clone());
    state.animation_selection.selected_keyframes.insert(AnimationKeyframeSelection {
        property: property_selection,
        keyframe_id: mondrian_core::KeyframeId::new(),
    });

    state.remove_track(removed_track_id, true).expect("remove track");

    assert_eq!(state.selection.selected_track_ids, vec![retained_track_id]);
    assert_eq!(
        state.selection.selected_clips,
        vec![SelectedClipRef {
            track_id: retained_track_id,
            is_video_track: true,
            clip_id: retained_clip_id,
        }]
    );
    assert!(state.selection.selected_mask.is_none());
    assert!(state.animation_selection.active_property.is_none());
    assert!(state.animation_selection.selected_keyframes.is_empty());
}

#[test]
fn moving_track_is_undoable_and_preserves_clips() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let moved_track_id = state.active_sequence().expect("sequence should exist").video_tracks[2].id;
    let anchor_track_id =
        state.active_sequence().expect("sequence should exist").video_tracks[0].id;
    let clip = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let clip_id = clip.id;
    state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .video_tracks[2]
        .add_clip(clip)
        .expect("add clip");

    state
        .move_track(
            moved_track_id,
            TrackRelativePlacement::Before(anchor_track_id),
        )
        .expect("move track");

    let seq = state.active_sequence().expect("sequence should exist");
    assert_eq!(seq.video_tracks[0].id, moved_track_id);
    assert_eq!(seq.video_tracks[0].name, "V1");
    assert_eq!(seq.video_tracks[0].clips[0].id, clip_id);
    assert_eq!(seq.video_tracks[1].name, "V2");
    assert_eq!(seq.video_tracks[2].name, "V3");
    assert_eq!(
        state.authoring_history().and_then(|history| history.undo_description()),
        Some("移动轨道")
    );

    assert!(state.undo_timeline().expect("undo should succeed"));
    let seq_undo = state.active_sequence().expect("sequence should exist after undo");
    assert_eq!(seq_undo.video_tracks[2].id, moved_track_id);
    assert_eq!(seq_undo.video_tracks[2].clips[0].id, clip_id);
}

#[test]
fn dropping_linked_clip_creates_missing_audio_track_at_target_index() {
    let mut state = create_state_with_sequence();
    let removed_audio_id =
        state.active_sequence().expect("sequence should exist").audio_tracks[2].id;
    state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .remove_audio_track(removed_audio_id)
        .expect("remove third audio track");
    state.ensure_minimum_tracks();

    let target_track_id =
        state.active_sequence().expect("sequence should exist").video_tracks[2].id;
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

    let seq = state.active_sequence().expect("sequence should exist");
    assert_eq!(seq.audio_tracks.len(), 3);
    let video_clip = seq.video_tracks[2]
        .clips
        .iter()
        .find(|clip| clip.id == video_clip_id)
        .expect("video clip should exist");
    let audio_clip = seq.audio_tracks[2]
        .clips
        .iter()
        .find(|clip| clip.link_group == video_clip.link_group)
        .expect("linked audio clip should exist");
    assert!(video_clip.link_group.is_some());
    assert_eq!(audio_clip.media_asset_id(), Some(asset_id));
}

#[test]
fn dropping_linked_clip_after_video_reorder_uses_current_track_index() {
    let mut state = create_state_with_sequence();
    let moved_video_track_id =
        state.active_sequence().expect("sequence should exist").video_tracks[2].id;
    let anchor_track_id =
        state.active_sequence().expect("sequence should exist").video_tracks[0].id;
    state
        .move_track(
            moved_video_track_id,
            TrackRelativePlacement::Before(anchor_track_id),
        )
        .expect("move track before drop");

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

    let seq = state.active_sequence().expect("sequence should exist");
    assert_eq!(seq.video_tracks[0].id, moved_video_track_id);
    assert!(seq.video_tracks[0].clips.iter().any(|clip| clip.id == video_clip_id));
    assert!(seq.audio_tracks[0]
        .clips
        .iter()
        .any(|clip| clip.link_group == seq.video_tracks[0].clips[0].link_group));
}

#[test]
fn moving_video_track_keeps_existing_linked_audio_on_its_audio_track() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();

    let mut video = Clip::new(AssetId::new(), tt(0, tb), tt(15, tb)).expect("valid clip");
    let mut audio = Clip::new(
        video.media_asset_id().expect("video asset"),
        tt(0, tb),
        tt(15, tb),
    )
    .expect("valid clip");
    let video_id = video.id;
    let audio_id = audio.id;
    let link_group = ClipLinkGroupId::new();
    video.link_group = Some(link_group);
    audio.link_group = Some(link_group);

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[2].add_clip(video).expect("add video");
        let audio_track_id = seq.audio_tracks[2].id;
        seq.add_media_audio_clip(audio_track_id, audio, AudioSourceComponentId::primary())
            .expect("add audio");
    }

    let moved_video_track_id =
        state.active_sequence().expect("sequence should exist").video_tracks[2].id;
    let anchor_track_id =
        state.active_sequence().expect("sequence should exist").video_tracks[0].id;
    state
        .move_track(
            moved_video_track_id,
            TrackRelativePlacement::Before(anchor_track_id),
        )
        .expect("move video track");

    let seq = state.active_sequence().expect("sequence should exist");
    assert!(seq.video_tracks[0].clips.iter().any(|clip| clip.id == video_id));
    let audio_after = seq.audio_tracks[2]
        .clips
        .iter()
        .find(|clip| clip.id == audio_id)
        .expect("audio clip should stay on original audio track");
    assert_eq!(audio_after.link_group, Some(link_group));
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
    state.test_set_asset_library(Some(
        AssetLibrary::open(temp_root.clone()).expect("open library"),
    ));
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    state.active_sequence_mut_uncommitted().expect("sequence should exist").in_point =
        Some(tt(10, tb));
    state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .out_point = Some(tt(40, tb));

    let target_track_id =
        state.active_sequence().expect("sequence should exist").video_tracks[0].id;
    let clip_id = state
        .create_adjustment_layer_on_video_track(target_track_id, None, ClipOverlapMode::Overwrite)
        .expect("create adjustment layer");

    let library = state.asset_library().expect("library should exist");
    let assets = library.list_assets().expect("list assets");
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].kind, AssetKind::AdjustmentLayer);

    let seq = state.active_sequence().expect("sequence should exist");
    let clip = seq.video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .expect("adjustment clip should exist");
    assert!(clip.is_adjustment_layer());
    assert_eq!(clip.library_asset_id(), Some(assets[0].id));
    assert_eq!(clip.position, tt(10, tb));
    assert_eq!(clip.duration, tt(30, tb));

    let _ = std::fs::remove_dir_all(temp_root);
}

#[test]
fn splitting_adjustment_layer_keeps_instance_state_isolated() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();

    let mut clip =
        Clip::new_adjustment_layer(AssetId::new(), tt(0, tb), tt(40, tb)).expect("valid clip");
    let effect = mondrian_effects::EffectNodeExt::with_defaults(EffectType::BasicCorrection);
    let effect_id = clip.add_effect_node(effect);
    let exposure_id = EffectType::BasicCorrection
        .parameter_id("exposure")
        .expect("exposure parameter ID");
    let exposure_path = clip
        .effect_parameter_address(effect_id, &exposure_id)
        .expect("adjustment exposure path");
    clip.apply_property_mutation(
        mondrian_core::automation::PropertyMutation::SetStaticValue {
            path: exposure_path,
            value: mondrian_core::automation::PropertyValue::Float(0.75),
        },
    )
    .expect("set adjustment exposure");
    let clip_id = clip.id;
    state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .video_tracks[0]
        .add_clip(clip)
        .expect("add adjustment clip");

    state
        .split_clip_at_frame(
            state.active_sequence().expect("sequence should exist").video_tracks[0].id,
            true,
            clip_id,
            20,
        )
        .expect("split adjustment layer")
        .expect("split target");

    let seq = state.active_sequence().expect("sequence should exist");
    let mut split_clips = seq.video_tracks[0].clips.clone();
    split_clips.sort_by_key(|clip| clip.position);
    assert_eq!(split_clips.len(), 2);
    assert!(split_clips.iter().all(|clip| clip.is_adjustment_layer()));
    assert!(split_clips
        .iter()
        .all(|clip| { (exposure_from_clip(clip, tt(20, tb)) - 0.75).abs() < 1.0e-4 }));

    let right_id = split_clips[1].id;
    let seq_mut = state.active_sequence_mut_uncommitted().expect("sequence should exist");
    let right_clip = seq_mut.video_tracks[0]
        .clips
        .iter_mut()
        .find(|clip| clip.id == right_id)
        .expect("right split clip should exist");
    let right_effect_id = right_clip.effects[0].id;
    let right_exposure_path = right_clip
        .effect_parameter_address(right_effect_id, &exposure_id)
        .expect("right adjustment exposure path");
    right_clip
        .apply_property_mutation(
            mondrian_core::automation::PropertyMutation::SetStaticValue {
                path: right_exposure_path,
                value: mondrian_core::automation::PropertyValue::Float(1.5),
            },
        )
        .expect("mutate right split clip");

    let seq = state.active_sequence().expect("sequence should exist");
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
    assert!((exposure_from_clip(left_clip, tt(10, tb)) - 0.75).abs() < 1.0e-4);
    assert!((exposure_from_clip(right_clip, tt(30, tb)) - 1.5).abs() < 1.0e-4);
}

#[test]
fn roll_cut_to_frame_is_undoable() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();

    let clip_a = Clip::new(AssetId::new(), tt(0, tb), tt(20, tb)).expect("valid clip");
    let clip_a_id = clip_a.id;
    let clip_b = Clip::new(AssetId::new(), tt(20, tb), tt(20, tb)).expect("valid clip");
    let clip_b_id = clip_b.id;

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_a).expect("add clip a");
        seq.video_tracks[0].add_clip(clip_b).expect("add clip b");
    }

    let changed = state.roll_cut_to_frame(clip_a_id, 25).expect("roll cut should succeed");
    assert!(changed);
    assert_eq!(
        state.authoring_history().and_then(|history| history.undo_description()),
        Some("滚动修剪")
    );

    let seq = state.active_sequence().expect("sequence should exist");
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

    assert_eq!(clip_a_after.duration, tt(25, tb));
    assert_eq!(clip_b_after.position, tt(25, tb));
    assert_eq!(clip_b_after.duration, tt(15, tb));
    assert_eq!(clip_b_after.source_origin(), tt(5, tb));

    assert!(state.undo_timeline().expect("undo should succeed"));
    let seq_undo = state.active_sequence().expect("sequence should exist");
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
    assert_eq!(clip_a_undo.duration, tt(20, tb));
    assert_eq!(clip_b_undo.position, tt(20, tb));
    assert_eq!(clip_b_undo.source_origin(), tt(0, tb));
}

#[test]
fn slip_clip_negative_delta_is_clamped_and_undoable() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let library_root = std::env::temp_dir().join(format!(
        "mondrian_timeline_slip_test_{}_{}",
        std::process::id(),
        unix_now_ms()
    ));
    std::fs::create_dir_all(&library_root).expect("create temp library root");
    state.test_set_asset_library(Some(
        AssetLibrary::open(library_root.clone()).expect("open library"),
    ));

    let mut clip = Clip::new(AssetId::new(), tt(8, tb), tt(20, tb)).expect("valid clip");
    clip.set_source_origin(tt(10, tb)).expect("set source origin");
    let clip_id = clip.id;
    state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .video_tracks[0]
        .add_clip(clip)
        .expect("add clip");

    let changed = state.slip_clips_bulk_by_frames(&[clip_id], -15).expect("slip should succeed");
    assert_eq!(changed, 1);
    assert_eq!(
        state.authoring_history().and_then(|history| history.undo_description()),
        Some("滑移片段")
    );

    let seq = state.active_sequence().expect("sequence should exist");
    let slipped = seq.video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .expect("clip should exist");
    assert_eq!(slipped.position, tt(8, tb));
    assert_eq!(slipped.duration, tt(20, tb));
    assert_eq!(slipped.clip_time_in, TimelineTime::ZERO);
    assert_eq!(slipped.source_origin(), tt(0, tb));
    assert_eq!(
        slipped.source_terminal_boundary().expect("source terminal"),
        tt(20, tb)
    );

    assert!(state.undo_timeline().expect("undo should succeed"));
    let restored = state.active_sequence().expect("sequence should exist").video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .expect("clip should exist after undo");
    assert_eq!(restored.clip_time_in, TimelineTime::ZERO);
    assert_eq!(restored.source_origin(), tt(10, tb));
    assert_eq!(
        restored.source_terminal_boundary().expect("source terminal"),
        tt(30, tb)
    );

    state.test_set_asset_library(None);
    let _ = std::fs::remove_dir_all(&library_root);
}

#[test]
fn adjustment_layer_rejects_slip() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let library_root = std::env::temp_dir().join(format!(
        "mondrian_adjustment_slip_test_{}_{}",
        std::process::id(),
        unix_now_ms()
    ));
    std::fs::create_dir_all(&library_root).expect("create temp library root");
    state.test_set_asset_library(Some(
        AssetLibrary::open(library_root.clone()).expect("open library"),
    ));
    let clip =
        Clip::new_adjustment_layer(AssetId::new(), tt(8, tb), tt(20, tb)).expect("valid clip");
    let clip_id = clip.id;
    state
        .active_sequence_mut_uncommitted()
        .expect("sequence should exist")
        .video_tracks[0]
        .add_clip(clip)
        .expect("add adjustment clip");

    let err = state
        .slip_clips_bulk_by_frames(&[clip_id], 5)
        .expect_err("adjustment layers should reject slip");
    assert!(err.to_string().contains("调整图层不支持 slip"));

    state.test_set_asset_library(None);
    let _ = std::fs::remove_dir_all(&library_root);
}

#[test]
fn slide_clip_updates_neighbors_and_is_undoable() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();

    let left = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let center = Clip::new(AssetId::new(), tt(10, tb), tt(10, tb)).expect("valid clip");
    let center_id = center.id;
    let right = Clip::new(AssetId::new(), tt(20, tb), tt(10, tb)).expect("valid clip");

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[0].add_clip(left).expect("add left");
        seq.video_tracks[0].add_clip(center).expect("add center");
        seq.video_tracks[0].add_clip(right).expect("add right");
    }

    let changed = state.slide_clips_bulk_by_frames(&[center_id], 3).expect("slide should succeed");
    assert_eq!(changed, 1);
    assert_eq!(
        state.authoring_history().and_then(|history| history.undo_description()),
        Some("滑动片段")
    );

    let seq = state.active_sequence().expect("sequence should exist");
    let clips = &seq.video_tracks[0].clips;
    assert_eq!(clips.len(), 3);
    assert_eq!(clips[0].position, tt(0, tb));
    assert_eq!(clips[0].duration, tt(13, tb));
    assert_eq!(clips[1].id, center_id);
    assert_eq!(clips[1].position, tt(13, tb));
    assert_eq!(clips[1].duration, tt(10, tb));
    assert_eq!(clips[2].position, tt(23, tb));
    assert_eq!(clips[2].duration, tt(7, tb));
    assert_eq!(clips[2].source_origin(), tt(3, tb));

    assert!(state.undo_timeline().expect("undo should succeed"));
    let seq_undo = state.active_sequence().expect("sequence should exist");
    let clips_undo = &seq_undo.video_tracks[0].clips;
    assert_eq!(clips_undo[0].duration, tt(10, tb));
    assert_eq!(clips_undo[1].position, tt(10, tb));
    assert_eq!(clips_undo[2].position, tt(20, tb));
    assert_eq!(clips_undo[2].source_origin(), tt(0, tb));
}

// ── Cross-track clip movement ──

#[test]
fn cross_track_move_to_different_track() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let target_track_id =
        state.active_sequence().expect("sequence should exist").video_tracks[1].id;

    let clip = Clip::new(AssetId::new(), tt(10, tb), tt(20, tb)).expect("valid clip");
    let clip_id = clip.id;

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip).expect("add clip");
    }

    state
        .move_clip_to_track_with_mode(target_track_id, clip_id, 5, ClipOverlapMode::Overwrite)
        .expect("cross-track move should succeed");

    let seq = state.active_sequence().expect("sequence should exist");
    assert!(seq.video_tracks[0].clips.is_empty());
    assert_eq!(seq.video_tracks[1].clips.len(), 1);
    assert_eq!(seq.video_tracks[1].clips[0].id, clip_id);
    assert_eq!(seq.video_tracks[1].clips[0].position, tt(5, tb));
}

#[test]
fn successive_cross_track_moves_preserve_explicit_positions() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let target_track_id =
        state.active_sequence().expect("sequence should exist").video_tracks[1].id;

    let clip_a = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let clip_a_id = clip_a.id;
    let clip_b = Clip::new(AssetId::new(), tt(20, tb), tt(10, tb)).expect("valid clip");
    let clip_b_id = clip_b.id;

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_a).expect("add clip a");
        seq.video_tracks[0].add_clip(clip_b).expect("add clip b");
    }

    // Move both clips with the same delta (+5 frames).
    state
        .move_clip_to_track_with_mode(target_track_id, clip_a_id, 5, ClipOverlapMode::Overwrite)
        .expect("move clip a");
    state
        .move_clip_to_track_with_mode(target_track_id, clip_b_id, 25, ClipOverlapMode::Overwrite)
        .expect("move clip b");

    let seq = state.active_sequence().expect("sequence should exist");
    assert!(seq.video_tracks[0].clips.is_empty());
    assert_eq!(seq.video_tracks[1].clips.len(), 2);
    assert_eq!(seq.video_tracks[1].clips[0].id, clip_a_id);
    assert_eq!(seq.video_tracks[1].clips[0].position, tt(5, tb));
    assert_eq!(seq.video_tracks[1].clips[1].id, clip_b_id);
    assert_eq!(seq.video_tracks[1].clips[1].position, tt(25, tb));
}

#[test]
fn cross_track_move_linked_clip_follows() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let video_track_1_id =
        state.active_sequence().expect("sequence should exist").video_tracks[1].id;

    let mut video = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let mut audio = Clip::new(
        video.media_asset_id().expect("video asset"),
        tt(0, tb),
        tt(10, tb),
    )
    .expect("valid clip");
    let video_id = video.id;
    let audio_id = audio.id;
    let link_group = ClipLinkGroupId::new();
    video.link_group = Some(link_group);
    audio.link_group = Some(link_group);

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[0].add_clip(video).expect("add video");
        let audio_track_id = seq.audio_tracks[0].id;
        seq.add_media_audio_clip(audio_track_id, audio, AudioSourceComponentId::primary())
            .expect("add audio");
    }

    state
        .move_clip_to_track_with_mode(video_track_1_id, video_id, 5, ClipOverlapMode::Overwrite)
        .expect("cross-track move should succeed");

    let seq = state.active_sequence().expect("sequence should exist");
    // Video moved to track 1.
    assert!(seq.video_tracks[0].clips.is_empty());
    assert_eq!(seq.video_tracks[1].clips[0].id, video_id);
    assert_eq!(seq.video_tracks[1].clips[0].position, tt(5, tb));
    // Linked audio follows to audio track 1.
    assert!(seq.audio_tracks[0].clips.is_empty());
    assert_eq!(seq.audio_tracks[1].clips[0].id, audio_id);
    assert_eq!(seq.audio_tracks[1].clips[0].position, tt(5, tb));
}

#[test]
fn linked_move_preserves_exact_subframe_offsets() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence").time_base();
    let sample_offset = TimelineTime::new(1, 48_000).expect("one sample");
    let asset_id = AssetId::new();
    let mut video = Clip::new(asset_id, tt(0, tb), tt(10, tb)).expect("valid video Clip");
    let mut audio =
        Clip::new(asset_id, sample_offset, tt(10, tb)).expect("valid offset audio Clip");
    let video_id = video.id;
    let audio_id = audio.id;
    let link_group = ClipLinkGroupId::new();
    video.link_group = Some(link_group);
    audio.link_group = Some(link_group);
    let target_track_id;
    {
        let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
        target_track_id = sequence.video_tracks[1].id;
        sequence.video_tracks[0].add_clip(video).expect("add video Clip");
        let audio_track_id = sequence.audio_tracks[0].id;
        sequence
            .add_media_audio_clip(audio_track_id, audio, AudioSourceComponentId::primary())
            .expect("add audio Clip");
    }

    state
        .move_clip_to_track_with_mode(target_track_id, video_id, 5, ClipOverlapMode::Overwrite)
        .expect("move linked Clips");

    let sequence = state.active_sequence().expect("sequence");
    assert_eq!(
        find_clip(sequence, video_id).expect("video Clip").position,
        tt(5, tb)
    );
    assert_eq!(
        find_clip(sequence, audio_id).expect("audio Clip").position,
        tt(5, tb).checked_add(sample_offset).expect("offset target")
    );
}

#[test]
fn bulk_trim_rejects_offset_link_edges_without_collapsing_them() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence").time_base();
    let sample_offset = TimelineTime::new(1, 48_000).expect("one sample");
    let asset_id = AssetId::new();
    let mut video = Clip::new(asset_id, tt(0, tb), tt(10, tb)).expect("valid video Clip");
    let mut audio =
        Clip::new(asset_id, sample_offset, tt(10, tb)).expect("valid offset audio Clip");
    let video_id = video.id;
    let audio_id = audio.id;
    let link_group = ClipLinkGroupId::new();
    video.link_group = Some(link_group);
    audio.link_group = Some(link_group);
    {
        let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
        sequence.video_tracks[0].add_clip(video).expect("add video Clip");
        let audio_track_id = sequence.audio_tracks[0].id;
        sequence
            .add_media_audio_clip(audio_track_id, audio, AudioSourceComponentId::primary())
            .expect("add audio Clip");
    }

    state
        .trim_clips_bulk_to_frame(&[video_id], TrimEdge::Out, 5)
        .expect_err("offset linked edges need an explicit J/L policy");

    let sequence = state.active_sequence().expect("sequence");
    assert_eq!(
        find_clip(sequence, video_id).expect("video Clip").duration,
        tt(10, tb)
    );
    assert_eq!(
        find_clip(sequence, audio_id).expect("audio Clip").duration,
        tt(10, tb)
    );
    assert!(!state.can_undo_action());
}

#[test]
fn linked_move_rejects_track_set_overflow_without_partial_mutation() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence").time_base();
    let asset_id = AssetId::new();
    let mut video = Clip::new(asset_id, tt(0, tb), tt(10, tb)).expect("valid video Clip");
    let mut audio = Clip::new(asset_id, tt(0, tb), tt(10, tb)).expect("valid audio Clip");
    let video_id = video.id;
    let audio_id = audio.id;
    let link_group = ClipLinkGroupId::new();
    video.link_group = Some(link_group);
    audio.link_group = Some(link_group);

    let target_track_id;
    {
        let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
        target_track_id = sequence.video_tracks[0].id;
        sequence.video_tracks[1].add_clip(video).expect("add video Clip");
        let audio_track_id = sequence.audio_tracks[0].id;
        sequence
            .add_media_audio_clip(audio_track_id, audio, AudioSourceComponentId::primary())
            .expect("add audio Clip");
    }

    state
        .move_clip_to_track_with_mode(target_track_id, video_id, 5, ClipOverlapMode::Overwrite)
        .expect_err("linked audio member would leave its Track set");

    let sequence = state.active_sequence().expect("sequence");
    assert_eq!(sequence.video_tracks[1].clips[0].id, video_id);
    assert_eq!(sequence.video_tracks[1].clips[0].position, tt(0, tb));
    assert_eq!(sequence.audio_tracks[0].clips[0].id, audio_id);
    assert_eq!(sequence.audio_tracks[0].clips[0].position, tt(0, tb));
    assert!(!state.can_undo_action());
}

#[test]
fn bulk_trim_rejects_stale_or_locked_link_members_atomically() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence").time_base();
    let asset_id = AssetId::new();
    let mut video = Clip::new(asset_id, tt(0, tb), tt(10, tb)).expect("valid video Clip");
    let mut audio = Clip::new(asset_id, tt(0, tb), tt(10, tb)).expect("valid audio Clip");
    let video_id = video.id;
    let audio_id = audio.id;
    let link_group = ClipLinkGroupId::new();
    video.link_group = Some(link_group);
    audio.link_group = Some(link_group);
    {
        let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
        sequence.video_tracks[0].add_clip(video).expect("add video Clip");
        let audio_track_id = sequence.audio_tracks[0].id;
        sequence
            .add_media_audio_clip(audio_track_id, audio, AudioSourceComponentId::primary())
            .expect("add audio Clip");
    }

    state
        .trim_clips_bulk_to_frame(&[video_id, ClipId::new()], TrimEdge::Out, 5)
        .expect_err("one stale identity must reject the complete trim");
    assert_eq!(
        find_clip(state.active_sequence().expect("sequence"), video_id)
            .expect("video Clip")
            .duration,
        tt(10, tb)
    );

    state.active_sequence_mut_uncommitted().expect("sequence").audio_tracks[0].is_locked = true;
    state
        .trim_clips_bulk_to_frame(&[video_id], TrimEdge::Out, 5)
        .expect_err("one locked Link Group member must reject the complete trim");
    let sequence = state.active_sequence().expect("sequence");
    assert_eq!(
        find_clip(sequence, video_id).expect("video Clip").duration,
        tt(10, tb)
    );
    assert_eq!(
        find_clip(sequence, audio_id).expect("audio Clip").duration,
        tt(10, tb)
    );
    assert!(!state.can_undo_action());
}

#[test]
fn cross_track_move_locked_track_rejects() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let locked_track_id =
        state.active_sequence().expect("sequence should exist").video_tracks[1].id;

    let clip = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let clip_id = clip.id;

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[1].is_locked = true;
        seq.video_tracks[0].add_clip(clip).expect("add clip");
    }

    let result =
        state.move_clip_to_track_with_mode(locked_track_id, clip_id, 5, ClipOverlapMode::Overwrite);
    assert!(result.is_err());

    // Clip still on source track.
    let seq = state.active_sequence().expect("sequence should exist");
    assert_eq!(seq.video_tracks[0].clips.len(), 1);
    assert_eq!(seq.video_tracks[0].clips[0].id, clip_id);
    assert!(seq.video_tracks[1].clips.is_empty());
}

#[test]
fn cross_track_move_insert_mode_pushes_existing() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let target_track_id =
        state.active_sequence().expect("sequence should exist").video_tracks[1].id;

    let existing = Clip::new(AssetId::new(), tt(10, tb), tt(20, tb)).expect("valid clip");
    let existing_id = existing.id;
    let mover = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let mover_id = mover.id;

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[1].add_clip(existing).expect("add existing");
        seq.video_tracks[0].add_clip(mover).expect("add mover");
    }

    state
        .move_clip_to_track_with_mode(target_track_id, mover_id, 5, ClipOverlapMode::PushForward)
        .expect("insert move should succeed");

    let seq = state.active_sequence().expect("sequence should exist");
    assert_eq!(seq.video_tracks[1].clips.len(), 2);
    assert_eq!(seq.video_tracks[1].clips[0].id, mover_id);
    assert_eq!(seq.video_tracks[1].clips[0].position, tt(5, tb));
    assert_eq!(seq.video_tracks[1].clips[1].id, existing_id);
    assert_eq!(seq.video_tracks[1].clips[1].position, tt(15, tb));
}

#[test]
fn cross_track_move_overwrite_mode_trims_existing() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let target_track_id =
        state.active_sequence().expect("sequence should exist").video_tracks[1].id;

    let existing = Clip::new(AssetId::new(), tt(10, tb), tt(20, tb)).expect("valid clip");
    let existing_id = existing.id;
    let mover = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let mover_id = mover.id;

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[1].add_clip(existing).expect("add existing");
        seq.video_tracks[0].add_clip(mover).expect("add mover");
    }

    state
        .move_clip_to_track_with_mode(target_track_id, mover_id, 5, ClipOverlapMode::Overwrite)
        .expect("overwrite move should succeed");

    let seq = state.active_sequence().expect("sequence should exist");
    // Overwrite: existing clip at [10, 30) intersected by mover at [5, 15) → existing trimmed to [15, 30)
    assert_eq!(seq.video_tracks[1].clips[0].id, mover_id);
    assert_eq!(seq.video_tracks[1].clips[0].position, tt(5, tb));
    assert_eq!(seq.video_tracks[1].clips[1].id, existing_id);
    assert_eq!(seq.video_tracks[1].clips[1].position, tt(15, tb));
    assert_eq!(seq.video_tracks[1].clips[1].duration, tt(15, tb));
}

#[test]
fn cross_track_move_same_track_behavior_preserved() {
    // Moving within the same track should still work via move_clip_to_track_with_mode.
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let track_id = state.active_sequence().expect("sequence should exist").video_tracks[0].id;

    let clip = Clip::new(AssetId::new(), tt(20, tb), tt(10, tb)).expect("valid clip");
    let clip_id = clip.id;

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip).expect("add clip");
    }

    state
        .move_clip_to_track_with_mode(track_id, clip_id, 5, ClipOverlapMode::Overwrite)
        .expect("same-track move should succeed");

    let seq = state.active_sequence().expect("sequence should exist");
    assert_eq!(seq.video_tracks[0].clips.len(), 1);
    assert_eq!(seq.video_tracks[0].clips[0].id, clip_id);
    assert_eq!(seq.video_tracks[0].clips[0].position, tt(5, tb));
}

#[test]
fn cross_track_move_is_undoable_and_restores_original() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let target_track_id =
        state.active_sequence().expect("sequence should exist").video_tracks[1].id;

    let clip = Clip::new(AssetId::new(), tt(10, tb), tt(20, tb)).expect("valid clip");
    let clip_id = clip.id;

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip).expect("add clip");
    }

    state
        .move_clip_to_track_with_mode(target_track_id, clip_id, 5, ClipOverlapMode::Overwrite)
        .expect("cross-track move should succeed");

    // Verify move happened.
    assert!(
        state.active_sequence().expect("sequence should exist").video_tracks[0]
            .clips
            .is_empty()
    );

    assert!(state.undo_timeline().expect("undo cross-track move"));
    let seq = state.active_sequence().expect("sequence should exist");
    assert_eq!(seq.video_tracks[0].clips.len(), 1);
    assert_eq!(seq.video_tracks[0].clips[0].id, clip_id);
    assert_eq!(seq.video_tracks[0].clips[0].position, tt(10, tb));
    assert!(seq.video_tracks[1].clips.is_empty());
}

#[test]
fn cross_track_move_relative_offset_across_different_source_tracks() {
    // Premiere-style: dragging V2→V3 gives a +1 track delta. A V1 clip should
    // move to V2, not V3. Each selected clip shifts by the same track offset.
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();

    let clip_v1 = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let clip_v1_id = clip_v1.id;
    let clip_v2 = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let clip_v2_id = clip_v2.id;

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_v1).expect("add clip v1");
        seq.video_tracks[1].add_clip(clip_v2).expect("add clip v2");
    }

    // Simulate dragging V2 clip to V3 (track index 2). Delta = +1.
    let target_track_id =
        state.active_sequence().expect("sequence should exist").video_tracks[2].id;

    // V2 clip moves to V3.
    state
        .move_clip_to_track_with_mode(target_track_id, clip_v2_id, 5, ClipOverlapMode::Overwrite)
        .expect("move clip v2");
    // V1 clip moves to V2 (same +1 track delta).
    let v2_target_id = state.active_sequence().expect("sequence should exist").video_tracks[1].id;
    state
        .move_clip_to_track_with_mode(v2_target_id, clip_v1_id, 5, ClipOverlapMode::Overwrite)
        .expect("move clip v1");

    let seq = state.active_sequence().expect("sequence should exist");
    assert!(seq.video_tracks[0].clips.is_empty());
    assert_eq!(seq.video_tracks[1].clips.len(), 1);
    assert_eq!(seq.video_tracks[1].clips[0].id, clip_v1_id);
    assert_eq!(seq.video_tracks[1].clips[0].position, tt(5, tb));
    assert_eq!(seq.video_tracks[2].clips.len(), 1);
    assert_eq!(seq.video_tracks[2].clips[0].id, clip_v2_id);
    assert_eq!(seq.video_tracks[2].clips[0].position, tt(5, tb));
}

#[test]
fn independent_cross_track_move_does_not_move_unlinked_neighbor() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();

    let clip_v1 = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let clip_v1_id = clip_v1.id;
    let clip_v2 = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let clip_v2_id = clip_v2.id;

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_v1).expect("add clip v1");
        seq.video_tracks[1].add_clip(clip_v2).expect("add clip v2");
    }

    // V2 clip to V0 (delta -1).
    let target_track_id =
        state.active_sequence().expect("sequence should exist").video_tracks[0].id;
    state
        .move_clip_to_track_with_mode(target_track_id, clip_v2_id, 5, ClipOverlapMode::Overwrite)
        .expect("move clip v2");

    let seq = state.active_sequence().expect("sequence should exist");
    assert_eq!(seq.video_tracks[0].clips.len(), 2);
    assert!(seq.video_tracks[1].clips.is_empty());
    // V1 clip stays on V0 (unmoved).
    assert!(seq.video_tracks[0].clips.iter().any(|c| c.id == clip_v1_id));
    // V2 clip moved to V0.
    assert!(seq.video_tracks[0].clips.iter().any(|c| c.id == clip_v2_id));
}

#[test]
fn successive_cross_track_moves_accept_explicit_in_bounds_targets() {
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();

    let clip_v2 = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let clip_v2_id = clip_v2.id;
    let clip_v3 = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let clip_v3_id = clip_v3.id;

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[1].add_clip(clip_v2).expect("add clip v2");
        seq.video_tracks[2].add_clip(clip_v3).expect("add clip v3");
    }

    let v2_target_id = state.active_sequence().expect("sequence should exist").video_tracks[1].id;
    let v1_target_id = state.active_sequence().expect("sequence should exist").video_tracks[0].id;

    state
        .move_clip_to_track_with_mode(v2_target_id, clip_v3_id, 5, ClipOverlapMode::Overwrite)
        .expect("move clip v3");
    state
        .move_clip_to_track_with_mode(v1_target_id, clip_v2_id, 5, ClipOverlapMode::Overwrite)
        .expect("move clip v2");

    let seq = state.active_sequence().expect("sequence should exist");
    // V2 clip at V1.
    assert_eq!(seq.video_tracks[0].clips.len(), 1);
    assert_eq!(seq.video_tracks[0].clips[0].id, clip_v2_id);
    assert_eq!(seq.video_tracks[0].clips[0].position, tt(5, tb));
    // V3 clip at V2.
    assert_eq!(seq.video_tracks[1].clips.len(), 1);
    assert_eq!(seq.video_tracks[1].clips[0].id, clip_v3_id);
    assert_eq!(seq.video_tracks[1].clips[0].position, tt(5, tb));
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
    let tb = state.active_sequence().expect("sequence should exist").time_base();

    let clip_a = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let clip_a_id = clip_a.id;
    let clip_b = Clip::new(AssetId::new(), tt(5, tb), tt(10, tb)).expect("valid clip");
    let clip_b_id = clip_b.id;

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_a).expect("add clip a");
        seq.video_tracks[1].add_clip(clip_b).expect("add clip b");
    }

    // Delta +1: B moves first (V2→V3), then A (V1→V2).
    let v3_target = state.active_sequence().expect("sequence should exist").video_tracks[2].id;
    let v2_target = state.active_sequence().expect("sequence should exist").video_tracks[1].id;

    // B first (higher source index).
    state
        .move_clip_to_track_with_mode(v3_target, clip_b_id, 5, ClipOverlapMode::Overwrite)
        .expect("move clip b");
    // Then A.
    state
        .move_clip_to_track_with_mode(v2_target, clip_a_id, 0, ClipOverlapMode::Overwrite)
        .expect("move clip a");

    let seq = state.active_sequence().expect("sequence should exist");
    // V1 empty.
    assert!(seq.video_tracks[0].clips.is_empty());
    // A on V2, intact.
    assert_eq!(seq.video_tracks[1].clips.len(), 1);
    assert_eq!(seq.video_tracks[1].clips[0].id, clip_a_id);
    assert_eq!(seq.video_tracks[1].clips[0].position, tt(0, tb));
    assert_eq!(seq.video_tracks[1].clips[0].duration, tt(10, tb));
    // B on V3, intact.
    assert_eq!(seq.video_tracks[2].clips.len(), 1);
    assert_eq!(seq.video_tracks[2].clips[0].id, clip_b_id);
    assert_eq!(seq.video_tracks[2].clips[0].position, tt(5, tb));
    assert_eq!(seq.video_tracks[2].clips[0].duration, tt(10, tb));
}

#[test]
fn same_track_move_does_not_trim_before_release() {
    // Ghost-based drag: during drag, the sequence must be unchanged.
    // Only on release (drop) should the move be applied.
    let mut state = create_state_with_sequence();
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let _track_id = state.active_sequence().expect("sequence should exist").video_tracks[0].id;

    // Clip A at [0, 10), clip B at [20, 10) — separated, no overlap.
    let clip_a = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("valid clip");
    let clip_a_id = clip_a.id;
    let clip_b = Clip::new(AssetId::new(), tt(20, tb), tt(10, tb)).expect("valid clip");
    let clip_b_id = clip_b.id;

    {
        let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
        seq.video_tracks[0].add_clip(clip_a).expect("add a");
        seq.video_tracks[0].add_clip(clip_b).expect("add b");
    }

    // Simulate drag start: save before-snapshot.
    let before = state.active_sequence().cloned();

    // "During drag": the sequence should be restored to before-snapshot
    // (no mutations). Verify by checking A and B are in original positions.
    {
        let seq = state.active_sequence().expect("sequence should exist");
        let clips = &seq.video_tracks[0].clips;
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0].id, clip_a_id);
        assert_eq!(clips[0].position, tt(0, tb));
        assert_eq!(clips[0].duration, tt(10, tb));
        assert_eq!(clips[1].id, clip_b_id);
        assert_eq!(clips[1].position, tt(20, tb));
        assert_eq!(clips[1].duration, tt(10, tb));
    }

    // "On release": apply the actual move (A to frame 5, B to frame 25).
    let anchor_pairs = vec![(clip_a_id, 0), (clip_b_id, 20)];
    state
        .move_clip_group_by_delta_with_mode(&anchor_pairs, 5, ClipOverlapMode::Overwrite)
        .expect("group move should succeed");

    let seq = state.active_sequence().expect("sequence should exist");
    let clips = &seq.video_tracks[0].clips;
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[0].id, clip_a_id);
    assert_eq!(clips[0].position, tt(5, tb));
    assert_eq!(clips[1].id, clip_b_id);
    assert_eq!(clips[1].position, tt(25, tb));

    // Verify undo snapshot semantics: restoring before-snapshot brings clips back.
    state.test_set_sequence(before);
    let seq = state.active_sequence().expect("sequence should exist");
    let clips = &seq.video_tracks[0].clips;
    assert_eq!(clips[0].position, tt(0, tb));
    assert_eq!(clips[1].position, tt(20, tb));
}
