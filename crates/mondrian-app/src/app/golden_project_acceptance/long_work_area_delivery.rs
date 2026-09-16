//! Long Work Area delivery candidate gate.
//!
//! The Golden delivery slice proves the H.264/AAC SDR and HEVC Main10 delivery
//! contracts over short timeline windows. This gate exports the same two
//! contracted targets over the complete multi-minute Hero Work Area through
//! the production export queue, then proves full-window diagnosed coverage,
//! exact stream-local A/V boundaries at the long duration, sampled
//! start/middle/end pixel roundtrip against an independent Rec.709 oracle,
//! and an audio RMS/peak roundtrip over the production Program render path.
//!
//! It is candidate evidence for the M1 export matrix, not a Golden contract
//! slice: `complete_golden_project` stays false and the report never feeds the
//! release repetition obligation.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use anyhow::{ensure, Context};
use mondrian_assets::{AssetKind, AssetRecord};
use mondrian_core::timeline_data::AlphaInterpretation;
use mondrian_core::{
    AssetId, ClipId, FramePosition, Resolution, SourceSampleTarget, TimelineTime, TrackId,
};
use mondrian_editor_state::Action;
use mondrian_export::preset::TimelineExportRange;
use mondrian_media::PreviewDecodeSessionContext;
use mondrian_timeline::clip::Transform2D;
use mondrian_timeline::sequence::{InputColorResolutionSource, MediaInputColorContext};
use serde::Serialize;

use super::fixture::{resolve_fixture, CorpusManifest};
use super::generated_delivery::{
    decode_delivery_audio, export_and_probe, max_rgb_error, rec709_code,
    render_program_audio_reference, AudioRoundtripEvidence, ExportEvidence,
};
use super::harness::{fixture_root, wait_for_media_imports, DurableReopenEvidence};
use super::media_execution::{decode_media, rgba8_at, source_rgba};
use super::workflow::GoldenProductWorkflowDriver;
use super::{load_json, sequence_settings_from_contract, GoldenProjectContract};
use crate::app::ui_actions::{
    clip_write_parameter_values_action, timeline_drop_asset_action, timeline_trim_clips_action,
    ClipParameterValueWrite, ClipWriteParameterValuesPayload, TimelineDropAssetPayload,
    TimelineTrimClipsPayload, TimelineTrimPayloadEdge,
};
use crate::app::{AppState, ClipOverlapMode};
use mondrian_core::automation::PropertyValue;

pub(super) const LONG_WORK_AREA_START_FRAME: i64 = 0;
pub(super) const LONG_WORK_AREA_WINDOW_END_FRAME_EXCLUSIVE: i64 = 7500;
const SAMPLE_FRAME_STARTS: [i64; 3] = [0, 3750, 7499];
const VISUAL_ROUNDTRIP_MAX_RGB_ERROR: u8 = 16;
const AUTHORED_OPACITY: f32 = 0.8;

#[derive(Debug, Serialize)]
pub(super) struct LongWorkAreaDeliveryReport {
    schema_version: u32,
    profile: &'static str,
    contract_id: String,
    status: &'static str,
    complete_golden_project: bool,
    window: LongWorkAreaWindowEvidence,
    authoring: LongWorkAreaAuthorEvidence,
    durability: DurableReopenEvidence,
    exports: Vec<LongWorkAreaExportEvidence>,
}

#[derive(Debug, Serialize)]
struct LongWorkAreaWindowEvidence {
    start_frame: i64,
    end_frame_exclusive: i64,
    duration_frames: i64,
    duration_secs: f64,
}

#[derive(Debug, Serialize)]
struct LongWorkAreaAuthorEvidence {
    video_track_id: TrackId,
    audio_track_id: TrackId,
    solid_asset_id: AssetId,
    solid_clip_id: ClipId,
    pcm_asset_id: AssetId,
    pcm_clip_id: ClipId,
    solid_start_frame: i64,
    solid_end_frame_exclusive: i64,
    pcm_start_frame: i64,
    pcm_end_frame_exclusive: i64,
    expected_center_rgba: [u8; 4],
}

#[derive(Debug, Serialize)]
struct LongWorkAreaExportEvidence {
    export: ExportEvidence,
    reimport: LongWorkAreaReimportEvidence,
}

#[derive(Debug, Serialize)]
struct LongWorkAreaReimportEvidence {
    asset_id: AssetId,
    input_color_resolution: InputColorResolutionSource,
    sampled_frames: Vec<SampledFrameEvidence>,
    audio_roundtrip: AudioRoundtripEvidence,
}

#[derive(Debug, Serialize)]
struct SampledFrameEvidence {
    frame: i64,
    expected_center_rgba: [u8; 4],
    decoded_center_rgba: [u8; 4],
    max_rgb_error: u8,
    input_color_resolution: InputColorResolutionSource,
}

/// Author a full-window solid plus the committed PCM fixture, prove the
/// window survives a durable save/reopen, export both contracted delivery
/// targets over the long Work Area, and reimport each for sampled
/// start/middle/end pixel roundtrip and audio RMS/peak roundtrip.
pub(super) fn execute_long_work_area_delivery(
    root: &Path,
    contract: &GoldenProjectContract,
    workflow: &mut GoldenProductWorkflowDriver,
    output_directory: &Path,
) -> anyhow::Result<LongWorkAreaDeliveryReport> {
    let settings = sequence_settings_from_contract(&contract.timeline)?;
    let manifest: CorpusManifest = load_json(&root.join("tests/validation/corpus-manifest.json"))?;
    let fixture = resolve_fixture(root, &fixture_root(root), contract, &manifest, "pcm-audio")?;

    let state = workflow.app_mut();
    let (time_base, video_track_id, audio_track_id) = {
        let sequence = state.active_sequence().context("active Sequence is absent")?;
        ensure!(
            sequence.settings == settings,
            "long Work Area gate is not bound to the contract-owned Sequence"
        );
        let video_track_id = sequence
            .video_tracks
            .iter()
            .find(|track| track.clips.is_empty())
            .map(|track| track.id)
            .context("long Work Area gate requires a pristine video Track")?;
        let audio_track_id = sequence.audio_tracks[0].id;
        (sequence.time_base(), video_track_id, audio_track_id)
    };
    let window_start = FramePosition::new(LONG_WORK_AREA_START_FRAME, time_base);
    let window_end = FramePosition::new(LONG_WORK_AREA_WINDOW_END_FRAME_EXCLUSIVE, time_base);
    let duration_frames = LONG_WORK_AREA_WINDOW_END_FRAME_EXCLUSIVE - LONG_WORK_AREA_START_FRAME;
    let duration_secs =
        duration_frames as f64 * settings.frame_rate.den as f64 / settings.frame_rate.num as f64;

    state.dispatch_action(Action::ImportMedia(vec![fixture.path.clone()]))?;
    wait_for_media_imports(state)?;
    let pcm_asset = state
        .asset_library()
        .context("Asset Library is absent")?
        .list_assets()?
        .into_iter()
        .find(|asset| asset.file_path() == Some(fixture.path.as_path()))
        .context("long Work Area gate requires the Foundation PCM Asset")?;
    ensure!(
        pcm_asset.kind == AssetKind::Audio,
        "PCM fixture did not import as audio"
    );

    let solid_asset_id = state.create_solid_color_asset_in_folder(None, None)?;
    state.begin_drag_asset(
        solid_asset_id,
        "Long Work Area Solid".to_owned(),
        AssetKind::SolidColor,
        Duration::from_secs_f64(duration_secs),
        false,
    );
    let solid_clip_id = state.drop_dragging_asset_to_video_track_with_mode(
        video_track_id,
        LONG_WORK_AREA_START_FRAME,
        ClipOverlapMode::Overwrite,
    )?;
    let solid_needs_trim = {
        let sequence = state.active_sequence().context("active Sequence is absent")?;
        let solid_clip = sequence
            .video_tracks
            .iter()
            .find(|track| track.id == video_track_id)
            .and_then(|track| track.clips.iter().find(|clip| clip.id == solid_clip_id))
            .context("dropped solid Clip is absent")?;
        solid_clip.end_position()? != TimelineTime::from_frame_position(window_end)?
    };
    if solid_needs_trim {
        state.dispatch_action(timeline_trim_clips_action(TimelineTrimClipsPayload {
            clip_ids: vec![solid_clip_id],
            edge: TimelineTrimPayloadEdge::Out,
            position: window_end,
        }))?;
    }
    let opacity_parameter = {
        let sequence = state.active_sequence().context("active Sequence is absent")?;
        let solid_clip = sequence
            .video_tracks
            .iter()
            .find(|track| track.id == video_track_id)
            .and_then(|track| track.clips.iter().find(|clip| clip.id == solid_clip_id))
            .context("authored solid Clip is absent")?;
        solid_clip
            .intrinsic_parameter_bag()
            .address_for_path(Transform2D::OPACITY_PATH)
            .context("solid Clip opacity parameter is absent")?
    };
    state.dispatch_action(clip_write_parameter_values_action(
        ClipWriteParameterValuesPayload {
            clip_id: solid_clip_id,
            writes: vec![ClipParameterValueWrite {
                parameter: opacity_parameter,
                value: PropertyValue::Float(AUTHORED_OPACITY),
            }],
        },
    ))?;

    let pcm_clip_id = {
        let before = state
            .active_sequence()
            .context("active Sequence is absent")?
            .audio_tracks
            .iter()
            .flat_map(|track| track.clips.iter().map(|clip| clip.id))
            .collect::<BTreeSet<_>>();
        state.dispatch_action(timeline_drop_asset_action(TimelineDropAssetPayload {
            asset_id: pcm_asset.id,
            target_track_id: audio_track_id,
            position: window_start,
        }))?;
        state
            .active_sequence()
            .context("active Sequence is absent")?
            .audio_tracks
            .iter()
            .flat_map(|track| track.clips.iter())
            .find(|clip| !before.contains(&clip.id))
            .map(|clip| clip.id)
            .context("PCM timeline drop created no audio Clip")?
    };
    let pcm_needs_trim = {
        let sequence = state.active_sequence().context("active Sequence is absent")?;
        let pcm_clip = sequence
            .audio_tracks
            .iter()
            .find(|track| track.id == audio_track_id)
            .and_then(|track| track.clips.iter().find(|clip| clip.id == pcm_clip_id))
            .context("dropped PCM Clip is absent")?;
        pcm_clip.end_position()? != TimelineTime::from_frame_position(window_end)?
    };
    if pcm_needs_trim {
        state.dispatch_action(Action::TrimClipEnd {
            clip_id: pcm_clip_id,
            new_source_out: window_end,
        })?;
    }

    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let solid_clip = sequence
        .video_tracks
        .iter()
        .find(|track| track.id == video_track_id)
        .and_then(|track| track.clips.iter().find(|clip| clip.id == solid_clip_id))
        .context("authored solid Clip is absent")?;
    ensure!(
        solid_clip.position == TimelineTime::from_frame_position(window_start)?
            && solid_clip.end_position()? == TimelineTime::from_frame_position(window_end)?,
        "solid Clip does not cover the complete long Work Area"
    );
    let solid_color = solid_clip.content.solid_color().context("solid Clip is not Solid Color")?;
    let opacity = solid_clip.transform.evaluate_opacity(solid_clip.clip_time_in);
    ensure!(
        (opacity - AUTHORED_OPACITY).abs() < 1.0e-6,
        "authored opacity did not survive the product actions"
    );
    let pcm_clip = sequence
        .audio_tracks
        .iter()
        .find(|track| track.id == audio_track_id)
        .and_then(|track| track.clips.iter().find(|clip| clip.id == pcm_clip_id))
        .context("authored PCM Clip is absent")?;
    ensure!(
        pcm_clip.position == TimelineTime::from_frame_position(window_start)?
            && pcm_clip.end_position()? == TimelineTime::from_frame_position(window_end)?,
        "PCM Clip does not cover the complete long Work Area"
    );
    let expected_center_rgba = [
        rec709_code(solid_color.r * solid_color.a * opacity),
        rec709_code(solid_color.g * solid_color.a * opacity),
        rec709_code(solid_color.b * solid_color.a * opacity),
        255,
    ];

    let durability = workflow.durable_save_reopen()?;
    let state = workflow.app_mut();
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let reopened_solid = sequence
        .video_tracks
        .iter()
        .find(|track| track.id == video_track_id)
        .and_then(|track| track.clips.iter().find(|clip| clip.id == solid_clip_id))
        .context("durable reopen lost the solid Clip")?;
    let reopened_pcm = sequence
        .audio_tracks
        .iter()
        .find(|track| track.id == audio_track_id)
        .and_then(|track| track.clips.iter().find(|clip| clip.id == pcm_clip_id))
        .context("durable reopen lost the PCM Clip")?;
    ensure!(
        reopened_solid.end_position()? == TimelineTime::from_frame_position(window_end)?
            && reopened_pcm.end_position()? == TimelineTime::from_frame_position(window_end)?,
        "durable reopen changed the long Work Area coverage"
    );
    let duration_frames = LONG_WORK_AREA_WINDOW_END_FRAME_EXCLUSIVE - LONG_WORK_AREA_START_FRAME;
    let mut long_exports = Vec::new();
    let mut decode_context = PreviewDecodeSessionContext::new();
    for export in &contract.exports {
        let evidence = export_and_probe(
            state,
            export,
            output_directory.join(format!("{}-long-window.mp4", export.id)),
            TimelineExportRange::WorkArea {
                start_frame: LONG_WORK_AREA_START_FRAME,
                end_frame_exclusive: LONG_WORK_AREA_WINDOW_END_FRAME_EXCLUSIVE,
            },
            settings.frame_rate,
            duration_frames,
            contract.acceptance.duration_error_max_frames,
            contract.acceptance.av_boundary_error_max_ms,
            contract.timeline.audio_sample_rate,
        )?;
        let reimport = reimport_and_sample(
            state,
            &evidence,
            contract.timeline.audio_sample_rate,
            expected_center_rgba,
            &mut decode_context,
        )?;
        long_exports.push(LongWorkAreaExportEvidence { export: evidence, reimport });
    }

    Ok(LongWorkAreaDeliveryReport {
        schema_version: 1,
        profile: "long-work-area-delivery-v1",
        contract_id: contract.id.clone(),
        status: "pass",
        complete_golden_project: false,
        window: LongWorkAreaWindowEvidence {
            start_frame: LONG_WORK_AREA_START_FRAME,
            end_frame_exclusive: LONG_WORK_AREA_WINDOW_END_FRAME_EXCLUSIVE,
            duration_frames,
            duration_secs,
        },
        authoring: LongWorkAreaAuthorEvidence {
            video_track_id,
            audio_track_id,
            solid_asset_id,
            solid_clip_id,
            pcm_asset_id: pcm_asset.id,
            pcm_clip_id,
            solid_start_frame: LONG_WORK_AREA_START_FRAME,
            solid_end_frame_exclusive: LONG_WORK_AREA_WINDOW_END_FRAME_EXCLUSIVE,
            pcm_start_frame: LONG_WORK_AREA_START_FRAME,
            pcm_end_frame_exclusive: LONG_WORK_AREA_WINDOW_END_FRAME_EXCLUSIVE,
            expected_center_rgba,
        },
        durability,
        exports: long_exports,
    })
}

fn reimport_and_sample(
    state: &mut AppState,
    evidence: &ExportEvidence,
    audio_sample_rate: u32,
    expected_center_rgba: [u8; 4],
    decode_context: &mut PreviewDecodeSessionContext,
) -> anyhow::Result<LongWorkAreaReimportEvidence> {
    let before = state
        .asset_library()
        .context("Asset Library is absent")?
        .list_assets()?
        .into_iter()
        .map(|asset| asset.id)
        .collect::<BTreeSet<_>>();
    state.dispatch_action(Action::ImportMedia(vec![evidence.output_path.clone()]))?;
    wait_for_media_imports(state)?;
    let imported = state
        .asset_library()
        .context("Asset Library is absent after reimport")?
        .list_assets()?
        .into_iter()
        .filter(|asset| !before.contains(&asset.id))
        .collect::<Vec<_>>();
    ensure!(
        imported.len() == 1,
        "long Work Area reimport created {} assets instead of one",
        imported.len()
    );
    let asset = &imported[0];
    ensure!(
        asset.kind == AssetKind::Video && asset.file_path() == Some(evidence.output_path.as_path()),
        "long Work Area reimport did not retain the finished deliverable identity"
    );
    let media_probe =
        asset.media_probe().context("reimported asset has no coherent media probe")?;
    let video = media_probe.primary_video().context("reimported asset has no video")?;
    ensure!(
        video.width > 0 && video.height > 0,
        "reimported video dimensions are unproven"
    );
    let resolution = Resolution { width: video.width, height: video.height };

    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let time_base = sequence.time_base();
    let input_color = sequence
        .settings
        .root_program_color_context(state.project_color_environment())?
        .media_input(false);

    let mut sampled_frames = Vec::new();
    for frame in SAMPLE_FRAME_STARTS {
        sampled_frames.push(decode_sampled_frame(
            state,
            asset,
            decode_context,
            frame,
            time_base,
            resolution,
            input_color.clone(),
            expected_center_rgba,
        )?);
    }

    let audio_reference =
        render_program_audio_reference(state, TimelineTime::ZERO, audio_sample_rate)?;
    let audio_roundtrip = decode_delivery_audio(state, asset, audio_reference, TimelineTime::ZERO)?;
    Ok(LongWorkAreaReimportEvidence {
        asset_id: asset.id,
        input_color_resolution: sampled_frames
            .first()
            .context("sampled frames are empty")?
            .input_color_resolution,
        sampled_frames,
        audio_roundtrip,
    })
}

fn decode_sampled_frame(
    state: &AppState,
    asset: &AssetRecord,
    decode_context: &mut PreviewDecodeSessionContext,
    frame: i64,
    time_base: mondrian_core::Rational,
    resolution: Resolution,
    input_color: MediaInputColorContext,
    expected_center_rgba: [u8; 4],
) -> anyhow::Result<SampledFrameEvidence> {
    let request = crate::app::preview_timeline_execution::PreviewTimelineMediaRequest {
        asset_id: asset.id,
        color_space_override: None,
        alpha_interpretation: AlphaInterpretation::Ignore,
        picture_overrides: Default::default(),
        source_sample: SourceSampleTarget::covering(TimelineTime::from_frame_position(
            FramePosition::new(frame, time_base),
        )?),
        target_resolution: resolution,
        input_color,
        cpu_working_required: false,
    };
    let decoded = decode_media(state, &request, asset, decode_context)?;
    let rgba = source_rgba(&decoded.frame)?;
    let width = resolution.width;
    let mut max_error = 0;
    for (x, y) in [
        (width / 2, resolution.height / 2),
        (width / 4, resolution.height / 2),
    ] {
        let pixel = rgba8_at(&rgba, width, x, y)?;
        max_error = max_error.max(max_rgb_error(pixel, expected_center_rgba));
    }
    ensure!(
        max_error <= VISUAL_ROUNDTRIP_MAX_RGB_ERROR,
        "long Work Area sampled frame {frame} differs from the Rec.709 oracle by \
         {max_error} encoded codes"
    );
    Ok(SampledFrameEvidence {
        frame,
        expected_center_rgba,
        decoded_center_rgba: rgba8_at(&rgba, width, width / 2, resolution.height / 2)?,
        max_rgb_error: max_error,
        input_color_resolution: decoded.input_color_resolution,
    })
}

#[cfg(test)]
#[test]
#[ignore = "long Work Area delivery candidate gate; requires production FFmpeg encoders and two multi-minute exports"]
fn long_work_area_delivery_candidate_gate() -> anyhow::Result<()> {
    use std::io::Write;

    use mondrian_core::{ProjectColorEnvironment, ProjectSettings};

    use super::harness::new_run_directory;
    use super::{load_golden_contract, repository_root};

    let root = repository_root();
    let contract = load_golden_contract(&root)?;
    let directory = new_run_directory(&root, "MONDRIAN_LONG_WORK_AREA_RUN_ROOT", "long-work-area")?;
    let project_path = directory.join("long-work-area-delivery.mdp");
    let workflow = GoldenProductWorkflowDriver::create(
        project_path.clone(),
        "Long Work Area Delivery",
        sequence_settings_from_contract(&contract.timeline)?,
        ProjectColorEnvironment::default(),
        ProjectSettings::default(),
    )?;
    let report = workflow.run_with(|workflow| {
        execute_long_work_area_delivery(&root, &contract, workflow, &directory)
    })?;
    ensure!(
        report.status == "pass",
        "long Work Area delivery gate failed"
    );
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_LONG_WORK_AREA_JSON={report_json}");
    std::fs::write(directory.join("long-work-area-report.json"), &report_json)?;
    if let Some(output) = std::env::var_os("MONDRIAN_PERF_OUTPUT") {
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(output)?;
        writeln!(file, "{report_json}")?;
    }
    Ok(())
}
