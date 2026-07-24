//! AAC editorial and Transport Golden slice over production App Interfaces.

use super::fixture::{resolve_fixture, CorpusManifest, FixtureEvidence};
use super::harness::{
    dispatch_author_transition, ensure_exact_requirement_evidence, fixture_root, new_run_directory,
    rooted_env_path, wait_for_media_imports, write_report, AuthorTransitionEvidence,
};
use super::workflow::{GoldenProductWorkflowDriver, GoldenSequenceStageEvidence};
use super::{
    load_golden_contract, load_json, repository_root, sequence_settings_from_contract,
    GoldenProjectContract,
};
use crate::app::playback::PlaybackAdvanceStatus;
use crate::app::ui_actions::{
    timeline_drop_asset_action, timeline_seek_with_source_action, timeline_trim_clips_action,
    TimelineDropAssetPayload, TimelineSeekSource, TimelineTrimClipsPayload,
    TimelineTrimPayloadEdge,
};
use crate::app::AppState;
use anyhow::{ensure, Context};
use mondrian_assets::AssetKind;
use mondrian_core::{ClipId, FrameRounding, TrackId};
use mondrian_editor_state::action::SelectionTarget;
use mondrian_editor_state::Action;
use mondrian_media::info::{AudioCodec, ChannelLayout};
use mondrian_playback::{ClockMaster, FrameDeliveryKind};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

const EDITORIAL_SLICE_ID: &str = "editorial-transport-v1";
const RUN_ROOT_ENV: &str = "MONDRIAN_GOLDEN_EDITORIAL_RUN_ROOT";
const OUTPUT_ENV: &str = "MONDRIAN_GOLDEN_EDITORIAL_OUTPUT";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct ClipRangeObservation {
    start_frame: i64,
    end_frame_exclusive: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "id")]
enum OperationEvidence {
    #[serde(rename = "overwrite")]
    Overwrite {
        author_step: AuthorTransitionEvidence,
        retained_left_clip_id: String,
        retained_left_range: ClipRangeObservation,
        replacement_clip_id: String,
        replacement_range: ClipRangeObservation,
    },
    #[serde(rename = "split")]
    Split {
        author_step: AuthorTransitionEvidence,
        left_clip_id: String,
        left_range: ClipRangeObservation,
        right_clip_id: String,
        right_range: ClipRangeObservation,
    },
    #[serde(rename = "ripple")]
    Ripple {
        author_step: AuthorTransitionEvidence,
        removed_clip_id: String,
        downstream_clip_id: String,
        downstream_before: ClipRangeObservation,
        downstream_after: ClipRangeObservation,
    },
    #[serde(rename = "scrub")]
    Scrub {
        target_frames: Vec<i64>,
        final_frame: i64,
        ready_deliveries: u64,
        warm_seek_count: u64,
    },
    #[serde(rename = "accurate-seek")]
    AccurateSeek {
        target_frame: i64,
        final_frame: i64,
        ready_deliveries: u64,
        accurate_seek_count: u64,
    },
    #[serde(rename = "play")]
    Play {
        start_frame: i64,
        final_frame: i64,
        frames_advanced: i64,
        clock_master: &'static str,
        synthetic_clock_residency_us: u64,
    },
}

impl OperationEvidence {
    const fn id(&self) -> &'static str {
        match self {
            Self::Overwrite { .. } => "overwrite",
            Self::Split { .. } => "split",
            Self::Ripple { .. } => "ripple",
            Self::Scrub { .. } => "scrub",
            Self::AccurateSeek { .. } => "accurate-seek",
            Self::Play { .. } => "play",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct ImportedAacObservation {
    stream_index: u32,
    sample_rate: u32,
    channels: u8,
    channel_layout: ChannelLayout,
    average_bitrate: u64,
}

#[derive(Debug, Clone, Serialize)]
struct EditorialSetupEvidence {
    stage: GoldenSequenceStageEvidence,
    audio_track_id: String,
    asset_id: String,
    imported_audio: ImportedAacObservation,
    setup_steps: Vec<AuthorTransitionEvidence>,
}

#[derive(Debug, Serialize)]
pub(super) struct GoldenEditorialReport {
    schema_version: u32,
    profile: &'static str,
    contract_id: String,
    corpus_revision: String,
    status: GoldenRunStatus,
    complete_golden_project: bool,
    fixture: FixtureEvidence,
    setup: EditorialSetupEvidence,
    operations: Vec<OperationEvidence>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum GoldenRunStatus {
    Passed,
}

#[derive(Debug)]
struct GoldenRunPaths {
    directory: PathBuf,
    project: PathBuf,
    report: PathBuf,
}

fn new_run_paths(root: &Path) -> anyhow::Result<GoldenRunPaths> {
    let directory = new_run_directory(root, RUN_ROOT_ENV, "golden-editorial")?;
    let report = rooted_env_path(root, OUTPUT_ENV, || {
        directory.join("golden-editorial-report.json")
    });
    Ok(GoldenRunPaths {
        project: directory.join("windows-alpha-golden-editorial.mdp"),
        directory,
        report,
    })
}

fn audio_track(state: &AppState, track_id: TrackId) -> anyhow::Result<&mondrian_timeline::Track> {
    state
        .active_sequence()
        .context("active Sequence is absent")?
        .audio_tracks
        .iter()
        .find(|track| track.id == track_id)
        .context("Golden audio Track is absent")
}

fn clip_range(
    state: &AppState,
    track_id: TrackId,
    clip_id: ClipId,
) -> anyhow::Result<ClipRangeObservation> {
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let clip = audio_track(state, track_id)?
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .with_context(|| format!("audio Clip is absent: {clip_id}"))?;
    let start_frame = clip
        .position
        .to_frame_position(sequence.settings.frame_rate, FrameRounding::Nearest)?
        .frame;
    let end_frame_exclusive = clip
        .end_position()?
        .to_frame_position(sequence.settings.frame_rate, FrameRounding::Nearest)?
        .frame;
    Ok(ClipRangeObservation { start_frame, end_frame_exclusive })
}

fn new_audio_clip_after(
    state: &AppState,
    track_id: TrackId,
    before: &BTreeSet<ClipId>,
) -> anyhow::Result<ClipId> {
    let created = audio_track(state, track_id)?
        .clips
        .iter()
        .filter(|clip| !before.contains(&clip.id))
        .map(|clip| clip.id)
        .collect::<Vec<_>>();
    ensure!(
        created.len() == 1,
        "timeline placement created {} new audio Clips",
        created.len()
    );
    Ok(created[0])
}

fn drop_audio(
    state: &mut AppState,
    asset_id: mondrian_core::AssetId,
    track_id: TrackId,
    frame: i64,
    intent: &'static str,
) -> anyhow::Result<(ClipId, AuthorTransitionEvidence)> {
    let before = audio_track(state, track_id)?
        .clips
        .iter()
        .map(|clip| clip.id)
        .collect::<BTreeSet<_>>();
    let step = dispatch_author_transition(
        state,
        intent,
        timeline_drop_asset_action(TimelineDropAssetPayload {
            asset_id,
            target_track_id: track_id,
            is_video_track: false,
            frame,
        }),
    )?;
    Ok((new_audio_clip_after(state, track_id, &before)?, step))
}

fn trim_audio_out(
    state: &mut AppState,
    clip_id: ClipId,
    end_frame_exclusive: i64,
    intent: &'static str,
) -> anyhow::Result<AuthorTransitionEvidence> {
    dispatch_author_transition(
        state,
        intent,
        timeline_trim_clips_action(TimelineTrimClipsPayload {
            clip_ids: vec![clip_id],
            edge: TimelineTrimPayloadEdge::Out,
            frame: end_frame_exclusive,
        }),
    )
}

fn observe_ready_delivery(state: &mut AppState) {
    let _ = state.observe_viewer_frame_delivery(FrameDeliveryKind::Ready);
}

pub(super) fn execute_editorial_stage(
    root: &Path,
    contract: &GoldenProjectContract,
    workflow: &mut GoldenProductWorkflowDriver,
) -> anyhow::Result<GoldenEditorialReport> {
    let slice = contract
        .execution_slices
        .iter()
        .find(|slice| slice.id == EDITORIAL_SLICE_ID)
        .context("editorial Golden execution slice is missing")?;
    ensure!(
        slice.required_fixture_roles == ["aac-audio"],
        "editorial slice fixture contract drifted"
    );
    ensure!(
        slice.required_operations
            == [
                "play",
                "accurate-seek",
                "scrub",
                "overwrite",
                "ripple",
                "split"
            ],
        "editorial slice operation contract drifted"
    );
    ensure!(
        slice.required_content.is_empty() && slice.required_exports.is_empty(),
        "editorial slice may not claim content or export coverage"
    );
    let window = slice.timeline_window.context("editorial slice has no timeline window")?;
    ensure!(
        window.start_frame == 0 && window.end_frame_exclusive == 150,
        "editorial slice timeline window drifted"
    );

    let manifest: CorpusManifest = load_json(&root.join("tests/validation/corpus-manifest.json"))?;
    let fixture = resolve_fixture(root, &fixture_root(root), contract, &manifest, "aac-audio")?;
    let stage = workflow.create_sequence_stage("editorial-transport")?;
    let state = workflow.app_mut();
    state.dispatch_action(Action::ImportMedia(vec![fixture.path.clone()]))?;
    wait_for_media_imports(state)?;
    let asset = state
        .asset_library()
        .context("Asset Library is absent after AAC import")?
        .list_assets()?
        .into_iter()
        .find(|asset| asset.path == fixture.path)
        .context("imported AAC fixture is absent from the Asset Library")?;
    ensure!(
        asset.kind == AssetKind::Audio,
        "AAC fixture imported as non-audio media"
    );
    let audio = asset.media_info.primary_audio().context("AAC fixture has no audio stream")?;
    ensure!(
        audio.codec == AudioCodec::Aac
            && audio.sample_rate == contract.timeline.audio_sample_rate
            && audio.channels == 2
            && audio.channel_layout == ChannelLayout::Stereo,
        "AAC stream contract differs from the Golden timeline"
    );
    let imported_audio = ImportedAacObservation {
        stream_index: audio.index,
        sample_rate: audio.sample_rate,
        channels: audio.channels,
        channel_layout: audio.channel_layout.clone(),
        average_bitrate: audio.avg_bitrate,
    };
    let track_id = state
        .active_sequence()
        .context("editorial Sequence is absent")?
        .audio_tracks
        .first()
        .context("editorial Sequence has no audio Track")?
        .id;

    let mut setup_steps = Vec::new();
    let (left_clip_id, left_drop) = drop_audio(state, asset.id, track_id, 0, "drop-left-aac")?;
    setup_steps.push(left_drop);
    setup_steps.push(trim_audio_out(state, left_clip_id, 100, "trim-left-aac")?);

    let (replacement_clip_id, overwrite_step) =
        drop_audio(state, asset.id, track_id, 25, "overwrite-left-aac")?;
    setup_steps.push(trim_audio_out(
        state,
        replacement_clip_id,
        75,
        "trim-overwrite-aac",
    )?);
    let retained_left_range = clip_range(state, track_id, left_clip_id)?;
    let replacement_range = clip_range(state, track_id, replacement_clip_id)?;
    ensure!(
        retained_left_range == (ClipRangeObservation { start_frame: 0, end_frame_exclusive: 25 })
            && replacement_range
                == (ClipRangeObservation { start_frame: 25, end_frame_exclusive: 75 }),
        "overwrite did not retain only the non-overlapped left fragment"
    );
    let overwrite_evidence = OperationEvidence::Overwrite {
        author_step: overwrite_step,
        retained_left_clip_id: left_clip_id.to_string(),
        retained_left_range,
        replacement_clip_id: replacement_clip_id.to_string(),
        replacement_range,
    };

    let (downstream_clip_id, downstream_drop) =
        drop_audio(state, asset.id, track_id, 100, "drop-downstream-aac")?;
    setup_steps.push(downstream_drop);
    setup_steps.push(trim_audio_out(
        state,
        downstream_clip_id,
        125,
        "trim-downstream-aac",
    )?);
    let downstream_before = clip_range(state, track_id, downstream_clip_id)?;

    state.dispatch_action(Action::Select(SelectionTarget::Clip(replacement_clip_id)))?;
    state.seek(50);
    let split_step =
        dispatch_author_transition(state, "split-overwrite-aac", Action::SplitClipAtPlayhead)?;
    let replacement_range_after_split = clip_range(state, track_id, replacement_clip_id)?;
    let right_clip = audio_track(state, track_id)?
        .clips
        .iter()
        .find(|clip| {
            clip.id != replacement_clip_id
                && clip_range(state, track_id, clip.id)
                    .is_ok_and(|range| range.start_frame == 50 && range.end_frame_exclusive == 75)
        })
        .context("split did not create the expected right audio Clip")?;
    let right_clip_id = right_clip.id;
    let right_range = clip_range(state, track_id, right_clip_id)?;
    ensure!(
        replacement_range_after_split
            == (ClipRangeObservation { start_frame: 25, end_frame_exclusive: 50 }),
        "split did not retain the expected left range"
    );
    let split_evidence = OperationEvidence::Split {
        author_step: split_step,
        left_clip_id: replacement_clip_id.to_string(),
        left_range: replacement_range_after_split,
        right_clip_id: right_clip_id.to_string(),
        right_range,
    };

    state.dispatch_action(Action::Select(SelectionTarget::Clip(right_clip_id)))?;
    let ripple_step = dispatch_author_transition(
        state,
        "ripple-delete-split-aac",
        Action::RippleDeleteSelection,
    )?;
    ensure!(
        audio_track(state, track_id)?.clips.iter().all(|clip| clip.id != right_clip_id),
        "Ripple Delete retained the selected right Clip"
    );
    let downstream_after = clip_range(state, track_id, downstream_clip_id)?;
    ensure!(
        downstream_before == (ClipRangeObservation { start_frame: 100, end_frame_exclusive: 125 })
            && downstream_after
                == (ClipRangeObservation { start_frame: 75, end_frame_exclusive: 100 }),
        "Ripple Delete did not shift downstream material by the removed duration"
    );
    let ripple_evidence = OperationEvidence::Ripple {
        author_step: ripple_step,
        removed_clip_id: right_clip_id.to_string(),
        downstream_clip_id: downstream_clip_id.to_string(),
        downstream_before,
        downstream_after,
    };

    let target_frames = vec![5, 15, 30];
    for frame in &target_frames {
        state.dispatch_action(timeline_seek_with_source_action(
            *frame,
            TimelineSeekSource::PointerDrag,
        ))?;
        observe_ready_delivery(state);
    }
    let scrub_report = state.playback_evidence_report();
    ensure!(
        state.current_frame() == 30
            && scrub_report.warm_seek_latency.count >= target_frames.len() as u64,
        "pointer-drag seeks did not produce warm scrub evidence"
    );
    let scrub_evidence = OperationEvidence::Scrub {
        target_frames,
        final_frame: state.current_frame(),
        ready_deliveries: scrub_report.deliveries.ready,
        warm_seek_count: scrub_report.warm_seek_latency.count,
    };

    let accurate_target = 40;
    state.dispatch_action(timeline_seek_with_source_action(
        accurate_target,
        TimelineSeekSource::Settled,
    ))?;
    observe_ready_delivery(state);
    let accurate_report = state.playback_evidence_report();
    ensure!(
        state.current_frame() == accurate_target
            && accurate_report.accurate_seek_latency.count >= 1,
        "settled seek did not produce accurate-seek evidence"
    );
    let accurate_evidence = OperationEvidence::AccurateSeek {
        target_frame: accurate_target,
        final_frame: state.current_frame(),
        ready_deliveries: accurate_report.deliveries.ready,
        accurate_seek_count: accurate_report.accurate_seek_latency.count,
    };

    let play_start = state.current_frame();
    state.dispatch_action(Action::Play)?;
    observe_ready_delivery(state);
    let _ = state.observe_video_preroll(0, 0);
    let advance = state.advance_playback_clock(Duration::from_millis(80));
    ensure!(
        advance.status == PlaybackAdvanceStatus::Advanced
            && advance.current_frame > play_start
            && state.playback_clock_master() == Some(ClockMaster::Synthetic),
        "production Transport did not advance from the Synthetic Clock Master"
    );
    state.dispatch_action(Action::Pause)?;
    let play_report = state.playback_evidence_report();
    ensure!(
        play_report.clock_residency.synthetic_us > 0,
        "playback evidence retained no Synthetic Clock residency"
    );
    let play_evidence = OperationEvidence::Play {
        start_frame: play_start,
        final_frame: advance.current_frame,
        frames_advanced: advance.frames_advanced,
        clock_master: "synthetic",
        synthetic_clock_residency_us: play_report.clock_residency.synthetic_us,
    };

    let operations = vec![
        play_evidence,
        accurate_evidence,
        scrub_evidence,
        overwrite_evidence,
        ripple_evidence,
        split_evidence,
    ];
    ensure_exact_requirement_evidence(
        &slice.required_operations,
        operations.iter().map(OperationEvidence::id),
        "operation",
    )?;
    workflow.verify_binding()?;

    Ok(GoldenEditorialReport {
        schema_version: 1,
        profile: EDITORIAL_SLICE_ID,
        contract_id: contract.id.clone(),
        corpus_revision: manifest.corpus_revision,
        status: GoldenRunStatus::Passed,
        complete_golden_project: false,
        fixture,
        setup: EditorialSetupEvidence {
            stage,
            audio_track_id: track_id.to_string(),
            asset_id: asset.id.to_string(),
            imported_audio,
            setup_steps,
        },
        operations,
    })
}

fn execute_editorial_slice(
    root: &Path,
    paths: &GoldenRunPaths,
) -> anyhow::Result<GoldenEditorialReport> {
    let contract = load_golden_contract(root)?;
    let settings = sequence_settings_from_contract(&contract.timeline)?;
    let mut workflow = GoldenProductWorkflowDriver::create(
        paths.project.clone(),
        "Windows Alpha Golden Editorial",
        settings,
        mondrian_core::ProjectColorEnvironment::default(),
        mondrian_core::ProjectSettings::default(),
    )?;
    execute_editorial_stage(root, &contract, &mut workflow)
}

#[test]
#[ignore = "Golden editorial gate requires the generated canonical AAC fixture"]
fn golden_project_editorial_transport_gate() -> anyhow::Result<()> {
    let root = repository_root();
    let paths = new_run_paths(&root)?;
    match execute_editorial_slice(&root, &paths) {
        Ok(report) => {
            write_report(&paths.report, &report)?;
            eprintln!(
                "MONDRIAN_GOLDEN_EDITORIAL_REPORT_JSON={}",
                serde_json::to_string(&report)?
            );
            eprintln!(
                "MONDRIAN_GOLDEN_EDITORIAL_REPORT_PATH={}",
                paths.report.display()
            );
            eprintln!(
                "MONDRIAN_GOLDEN_EDITORIAL_RUN_DIRECTORY={}",
                paths.directory.display()
            );
            Ok(())
        }
        Err(error) => {
            let failure = serde_json::json!({
                "schema_version": 1,
                "profile": EDITORIAL_SLICE_ID,
                "status": "failed",
                "complete_golden_project": false,
                "error": format!("{error:#}")
            });
            write_report(&paths.report, &failure)?;
            Err(error)
        }
    }
}
