//! Focused Hero proof for one retime author-to-execution contract.
//!
//! This gate deliberately uses metadata-only media. It proves exact author,
//! persistence, Viewer/Export planning, and dense audio preparation semantics;
//! fixed-corpus Golden stages remain responsible for decode and encode pixels.

use super::harness::{
    author_transition, dispatch_author_transition, new_run_directory, DirectoryCleanup,
};
use super::workflow::GoldenProductWorkflowDriver;
use super::{load_golden_contract, repository_root, sequence_settings_from_contract};
use crate::app::ui_actions::TimelineInsertAssetPayload;
use anyhow::{ensure, Context};
use mondrian_audio::{
    compile_audio_program, AudioCompileRequest, AudioProcessingMode, AudioRenderContract,
    PreparedAudioPlan,
};
use mondrian_core::{
    ClipId, ColorSpace, FramePosition, ProjectColorEnvironment, ProjectSettings, Rational,
    TimeScale, TimelineTime,
};
use mondrian_editor_state::Action;
use mondrian_media::info::{AudioCodec, ChannelLayout, PixelFormat, VideoCodec};
use mondrian_media::{
    AudioStreamInfo, DecodedVideoRange, DetectedColorInterpretation, MediaInfo, VideoCodecProfile,
    VideoColorDetectionMethod, VideoColorInterpretationConfidence, VideoColorSpaceSource,
    VideoStreamInfo,
};
use mondrian_renderer::{
    evaluate_timeline_render_plan, TimelineEvaluationRequest, TimelineRenderPlanElement,
};
use mondrian_timeline::{
    InsertAutomationPolicy, InsertTimelineStatePolicy, InsertTransitionPolicy, Sequence,
};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn golden_hero_retime_is_one_exact_contract_across_author_preview_audio_export_and_reopen(
) -> anyhow::Result<()> {
    let root = repository_root();
    let contract = load_golden_contract(&root)?;
    let directory = new_run_directory(&root, "MONDRIAN_GOLDEN_RETIME_RUN_ROOT", "golden-retime")?;
    let mut cleanup = DirectoryCleanup::default();
    cleanup.track(Some(directory.clone()));
    let mut workflow = GoldenProductWorkflowDriver::create(
        directory.join("retime-hero.mdp"),
        "Golden Retime Hero",
        sequence_settings_from_contract(&contract.timeline)?,
        ProjectColorEnvironment::default(),
        ProjectSettings {
            cache_dir: Some(directory.join("cache")),
            ..ProjectSettings::default()
        },
    )?;
    let hero_sequence_id = workflow.hero_sequence_id();

    let media_path = directory.join("metadata-only-retime-source.mov");
    std::fs::write(&media_path, [0_u8])?;
    let asset_id = workflow
        .app()
        .asset_library()
        .context("Golden Retime Asset Library is absent")?
        .upsert_media_file_with_info(
            &media_path,
            MediaInfo {
                path: media_path.clone(),
                duration: Duration::from_secs(10),
                file_size: 1,
                container: "mov".to_owned(),
                video_streams: vec![VideoStreamInfo {
                    index: 0,
                    codec: VideoCodec::H264,
                    duration: Some(Duration::from_secs(10)),
                    codec_profile: VideoCodecProfile::H264High,
                    width: 1920,
                    height: 1080,
                    frame_rate: Rational::FPS_25,
                    frame_rate_proven: true,
                    pixel_format: PixelFormat::Yuv420p,
                    pixel_format_proven: true,
                    color_range: DecodedVideoRange::Limited,
                    detected_color_space: Some(ColorSpace::Rec709),
                    color_interpretation: DetectedColorInterpretation {
                        color_space: Some(ColorSpace::Rec709),
                        confidence: VideoColorInterpretationConfidence::High,
                        source: VideoColorSpaceSource::Metadata,
                        method: VideoColorDetectionMethod::CicpTags,
                        evidence: Vec::new(),
                        warnings: Vec::new(),
                        user_overridable: true,
                    },
                    color_space_source: VideoColorSpaceSource::Metadata,
                    color_detection_method: VideoColorDetectionMethod::CicpTags,
                    color_metadata: None,
                    color_metadata_hints: Vec::new(),
                    hdr_metadata: Vec::new(),
                    bit_depth: 8,
                    has_alpha: false,
                    avg_bitrate: 8_000_000,
                    total_frames: Some(250),
                }],
                audio_streams: vec![AudioStreamInfo {
                    index: 1,
                    stream_id: Some(1),
                    language: None,
                    title: None,
                    is_default: true,
                    codec: AudioCodec::Pcm { bit_depth: 24 },
                    duration: Some(Duration::from_secs(10)),
                    sample_rate: 48_000,
                    channels: 2,
                    channel_layout: ChannelLayout::Stereo,
                    bit_depth: 24,
                    avg_bitrate: 2_304_000,
                }],
                has_video: true,
                has_audio: true,
            },
        )?;

    let sequence = workflow
        .app()
        .active_sequence()
        .context("Golden Retime Hero Sequence is absent")?;
    let video_track_id = sequence.video_tracks[0].id;
    let audio_track_id = sequence.audio_tracks[0].id;
    let time_base = sequence.time_base();
    let (inserted, _) =
        author_transition(workflow.app_mut(), "insert-linked-retime-source", |state| {
            Ok(state.insert_asset_from_ui(TimelineInsertAssetPayload {
                asset_id,
                insert_frame: 0,
                source_in_frame: 0,
                duration_frames: 48,
                video_target_track_id: Some(video_track_id),
                audio_target_track_id: Some(audio_track_id),
                ripple_track_ids: vec![video_track_id, audio_track_id],
                automation_policy: InsertAutomationPolicy::FollowEditorialContent,
                transition_policy: InsertTransitionPolicy::RejectAffected,
                timeline_state_policy: InsertTimelineStatePolicy::PreserveSequenceTime,
            })?)
        })?;
    ensure!(
        inserted.inserted_clip_ids.len() == 2,
        "Golden Retime Insert did not create one linked picture/audio pair"
    );
    let (video_clip_id, audio_clip_id) = linked_clip_ids(
        workflow.app().active_sequence().context("Hero is absent")?,
        asset_id,
    )?;

    dispatch_author_transition(
        workflow.app_mut(),
        "set-linked-forward-rate",
        Action::SetClipForwardRate {
            clip_id: video_clip_id,
            rate: TimeScale::new(3, 2)?,
            include_linked: true,
        },
    )?;
    assert_clip_scale(
        workflow.app().active_sequence().context("Hero is absent")?,
        video_clip_id,
        TimeScale::new(3, 2)?,
    )?;
    assert_clip_scale(
        workflow.app().active_sequence().context("Hero is absent")?,
        audio_clip_id,
        TimeScale::new(3, 2)?,
    )?;
    let sample_frame = 12;
    let expected_forward_source = frame_time(18, time_base)?;
    assert_visual_source_time(
        workflow.app().active_sequence().context("Hero is absent")?,
        sample_frame,
        expected_forward_source,
    )?;
    assert_prepared_audio_source_time(
        workflow.app().active_sequence().context("Hero is absent")?,
        audio_clip_id,
        frame_time(sample_frame, time_base)?,
        expected_forward_source,
        TimeScale::new(3, 2)?,
    )?;

    dispatch_author_transition(
        workflow.app_mut(),
        "freeze-linked-picture-only",
        Action::FreezeVideoClipAt {
            clip_id: video_clip_id,
            sequence_time: FramePosition::new(sample_frame, time_base),
        },
    )?;
    let held_sequence = workflow.app().active_sequence().context("Hero is absent")?;
    assert_clip_scale(held_sequence, video_clip_id, TimeScale::new(0, 1)?)?;
    assert_clip_scale(held_sequence, audio_clip_id, TimeScale::new(3, 2)?)?;
    assert_visual_source_time(held_sequence, sample_frame, expected_forward_source)?;
    assert_visual_source_time(held_sequence, 36, expected_forward_source)?;
    assert_prepared_audio_source_time(
        held_sequence,
        audio_clip_id,
        frame_time(36, time_base)?,
        frame_time(54, time_base)?,
        TimeScale::new(3, 2)?,
    )?;

    author_transition(workflow.app_mut(), "undo-picture-hold", |state| {
        ensure!(state.undo_timeline()?, "picture hold had no Undo entry");
        Ok(())
    })?;
    assert_clip_scale(
        workflow.app().active_sequence().context("Hero is absent")?,
        video_clip_id,
        TimeScale::new(3, 2)?,
    )?;
    author_transition(workflow.app_mut(), "undo-linked-rate", |state| {
        ensure!(state.undo_timeline()?, "linked rate had no Undo entry");
        Ok(())
    })?;
    let original_sequence = workflow.app().active_sequence().context("Hero is absent")?;
    assert_clip_scale(original_sequence, video_clip_id, TimeScale::ONE)?;
    assert_clip_scale(original_sequence, audio_clip_id, TimeScale::ONE)?;

    author_transition(workflow.app_mut(), "redo-linked-rate", |state| {
        ensure!(state.redo_timeline()?, "linked rate had no Redo entry");
        Ok(())
    })?;
    author_transition(workflow.app_mut(), "redo-picture-hold", |state| {
        ensure!(state.redo_timeline()?, "picture hold had no Redo entry");
        Ok(())
    })?;
    let durable = workflow.durable_save_reopen()?;
    ensure!(
        durable.session_identity_changed && durable.project_identity_preserved,
        "Golden Retime durable reopen did not cross a fresh Session boundary"
    );
    ensure!(
        workflow.hero_sequence_id() == hero_sequence_id
            && workflow.app().active_sequence_id() == Some(hero_sequence_id),
        "Golden Retime durable reopen changed the Hero Sequence"
    );

    let reopened = workflow.app().active_sequence().context("reopened Hero is absent")?;
    assert_clip_scale(reopened, video_clip_id, TimeScale::new(0, 1)?)?;
    assert_clip_scale(reopened, audio_clip_id, TimeScale::new(3, 2)?)?;
    assert_visual_source_time(reopened, 36, expected_forward_source)?;
    assert_prepared_audio_source_time(
        reopened,
        audio_clip_id,
        frame_time(36, time_base)?,
        frame_time(54, time_base)?,
        TimeScale::new(3, 2)?,
    )?;
    Ok(())
}

fn linked_clip_ids(
    sequence: &Sequence,
    asset_id: mondrian_core::AssetId,
) -> anyhow::Result<(ClipId, ClipId)> {
    let video = sequence.video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.media_asset_id() == Some(asset_id))
        .context("linked video Clip is absent")?;
    let audio = sequence.audio_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.media_asset_id() == Some(asset_id))
        .context("linked audio Clip is absent")?;
    ensure!(
        video.link_group.is_some() && video.link_group == audio.link_group,
        "inserted picture/audio Clips do not share one link group"
    );
    Ok((video.id, audio.id))
}

fn assert_clip_scale(
    sequence: &Sequence,
    clip_id: ClipId,
    expected: TimeScale,
) -> anyhow::Result<()> {
    let clip = sequence
        .video_tracks
        .iter()
        .chain(&sequence.audio_tracks)
        .flat_map(|track| &track.clips)
        .find(|clip| clip.id == clip_id)
        .with_context(|| format!("Clip is absent: {clip_id}"))?;
    ensure!(
        clip.source_time_scale() == expected,
        "Clip {clip_id} source scale changed"
    );
    Ok(())
}

fn assert_visual_source_time(
    sequence: &Sequence,
    timeline_frame: i64,
    expected: TimelineTime,
) -> anyhow::Result<()> {
    for request in [
        TimelineEvaluationRequest::preview(timeline_frame, 0.5),
        TimelineEvaluationRequest::export(timeline_frame),
    ] {
        let plan = evaluate_timeline_render_plan(sequence, request)?;
        let media = plan
            .elements
            .iter()
            .filter_map(|element| match element {
                TimelineRenderPlanElement::Media(media) => Some(media),
                _ => None,
            })
            .collect::<Vec<_>>();
        ensure!(
            media.len() == 1 && media[0].source_time == expected,
            "Preview/Export render plan did not preserve the exact source-time map"
        );
    }
    Ok(())
}

fn assert_prepared_audio_source_time(
    sequence: &Sequence,
    clip_id: ClipId,
    sequence_time: TimelineTime,
    expected_source_time: TimelineTime,
    expected_scale: TimeScale,
) -> anyhow::Result<()> {
    let output_id = sequence.audio_program.outputs[0].id;
    let compiled = compile_audio_program(sequence, AudioCompileRequest::program(output_id))?;
    let prepared = PreparedAudioPlan::prepare(
        Arc::new(compiled),
        AudioRenderContract {
            sample_rate: sequence.settings.audio_sample_rate,
            channel_layout: sequence.settings.audio_channel_layout,
            max_block_frames: 1024,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes:
                AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
            public_output_lookahead_budget_frames:
                AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
            compensation_delay_scratch_budget_bytes:
                AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
        },
    )?;
    let contribution = prepared
        .program()
        .contributions()
        .iter()
        .find(|contribution| contribution.clip_id == clip_id)
        .with_context(|| format!("prepared audio contribution is absent: {clip_id}"))?;
    ensure!(
        contribution.source_time_map.scale == expected_scale
            && contribution.source_time_map.map(sequence_time)? == expected_source_time,
        "prepared audio contribution changed the exact source-time map"
    );
    let summary = prepared.schedule_summary();
    ensure!(
        summary.contribution_count == 1
            && summary.track_count == sequence.audio_tracks.len()
            && summary.node_count >= summary.track_count,
        "audio prepare did not lower one contribution through the complete routed Track closure"
    );
    Ok(())
}

fn frame_time(frame: i64, time_base: mondrian_core::Rational) -> anyhow::Result<TimelineTime> {
    Ok(TimelineTime::from_frame_position(FramePosition::new(
        frame, time_base,
    ))?)
}
