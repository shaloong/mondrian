//! Fixed-corpus decode, presentation, export, and reimport evidence for one
//! exact constant-retime operation on the Golden Hero Sequence.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, ensure, Context};
use mondrian_assets::AssetKind;
use mondrian_core::timeline_data::AlphaInterpretation;
use mondrian_core::{
    AssetId, ClipId, FramePosition, Resolution, SourceSampleTarget, TimeScale, TimelineTime,
};
use mondrian_editor_state::Action;
use mondrian_export::preset::TimelineExportRange;
use mondrian_media::{
    PreviewDecodeSessionContext, PreviewDecodeTemporalSelection, PreviewTemporalExtentSource,
    VideoCodecProfile,
};
use mondrian_playback::PreviewResolutionScale;
use mondrian_renderer::{
    evaluate_prepared_visual_program, execute_cpu_output_boundary, CpuColorFrame,
    PreparedVisualProgram, RenderOutputColorBoundary, RenderOutputColorBoundaryTarget,
    TimelineCompositeScratch, TimelineEvaluationRequest, TimelineRenderPlanElement,
};
use mondrian_timeline::{Clip, ClipSourceTimeMap, Sequence};
use serde::Serialize;

use super::fixture::{sha256_bytes, sha256_file};
use super::generated_delivery::{export_and_probe, ExportEvidence};
use super::harness::{
    author_transition, dispatch_author_transition, wait_for_media_imports, AuthorTransitionEvidence,
};
use super::headless_preview::{
    GoldenHeadlessPreview, GoldenHeadlessViewerEvidence, GoldenViewerPresentationEvidence,
};
use super::media_execution::{
    decode_media, decode_media_with_preference, source_rgba, source_temporal_selection,
};
use super::proxy_relink::{
    path_resolution_label, resolve_media_path_for_preference, ResolvedPathEvidence,
};
use super::{GoldenProjectContract, GoldenTimelineWindow};
use crate::app::preview_cpu_execution::composite_resolved_preview_working;
use crate::app::preview_timeline_execution::{
    resolve_preview_timeline, PreviewTimelineMediaFrame, PreviewTimelineMediaRequest,
    PreviewTimelineResolution, PreviewTimelineTitleFrame,
};
use crate::app::preview_unavailability::{PreviewOutputStage, PreviewUnavailability};
use crate::app::preview_viewer_plan::ResolvedPreviewElement;
use crate::app::AppState;

const RETIME_EXPORT_ID: &str = "h264-aac-sdr";
const SAMPLE_FRAME_OFFSET: i64 = 50;
const HOLD_FRAME_OFFSET: i64 = 80;
const EXPORT_DURATION_FRAMES: i64 = 1;
const VIEWER_PRESENTATION_TIMEOUT: Duration = Duration::from_secs(30);
const EXPECTED_MEAN_ABSOLUTE_ERROR_MAX: f64 = 20.0;
const EXPECTED_MAX_CHANNEL_ERROR_MAX: u8 = 96;
const COUNTERFACTUAL_MEAN_ERROR_MARGIN_MIN: f64 = 0.5;
const COUNTERFACTUAL_MEAN_ERROR_RATIO_MIN: f64 = 1.5;
const PROXY_EXPECTED_MEAN_ABSOLUTE_ERROR_MAX: f64 = 12.0;
const PROXY_EXPECTED_MAX_CHANNEL_ERROR_MAX: u8 = 160;
const PROXY_COUNTERFACTUAL_MEAN_ERROR_MARGIN_MIN: f64 = 0.25;
const SAMPLE_COLUMNS: u32 = 16;
const SAMPLE_ROWS: u32 = 9;

#[derive(Debug, Serialize)]
pub(super) struct GoldenRetimeMediaEvidence {
    fixtures: Vec<RetimeFixtureEvidence>,
}

#[derive(Debug, Serialize)]
struct RetimeFixtureEvidence {
    fixture_id: &'static str,
    cadence: RetimeCadence,
    author: RetimeAuthorEvidence,
    media_binding: RetimeMediaBindingEvidence,
    forward: RetimeScenarioEvidence,
    reverse: RetimeScenarioEvidence,
    reverse_hold: RetimeScenarioEvidence,
    presentations: Vec<GoldenViewerPresentationEvidence>,
    viewer: GoldenHeadlessViewerEvidence,
    export: ExportEvidence,
    reimport: RetimeReimportEvidence,
}

impl GoldenRetimeMediaEvidence {
    pub(super) const fn export_id(&self) -> &'static str {
        RETIME_EXPORT_ID
    }
}

#[derive(Debug, Serialize)]
struct RetimeAuthorEvidence {
    forward_half: AuthorTransitionEvidence,
    reverse_half: AuthorTransitionEvidence,
    reverse_hold: AuthorTransitionEvidence,
    undo_hold: AuthorTransitionEvidence,
    undo_reverse: AuthorTransitionEvidence,
}

#[derive(Debug, Serialize)]
struct RetimeMediaBindingEvidence {
    preview_proxy: ResolvedPathEvidence,
    immutable_export_source: ResolvedPathEvidence,
}

#[derive(Debug, Serialize)]
struct RetimeScenarioEvidence {
    id: &'static str,
    rate: TimeScale,
    sample_frame: i64,
    expected_source_sample: SourceSampleTarget,
    preview_source_sample: SourceSampleTarget,
    export_source_sample: SourceSampleTarget,
    expected_program: ProgramReferenceEvidence,
    preview_proxy: RetimeProxyProgramEvidence,
    counterfactual_programs: Vec<LabeledProgramReferenceEvidence>,
}

#[derive(Debug, Serialize)]
struct RetimeProxyProgramEvidence {
    program: ProgramReferenceEvidence,
    expected_difference: PixelDifferenceEvidence,
    counterfactual_differences: Vec<LabeledPixelDifferenceEvidence>,
    expected_mean_absolute_error_max: f64,
    expected_max_channel_error_max: u8,
    counterfactual_mean_error_margin_min: f64,
}

#[derive(Debug, Serialize)]
struct LabeledProgramReferenceEvidence {
    id: &'static str,
    program: ProgramReferenceEvidence,
}

#[derive(Debug, Serialize)]
struct ProgramReferenceEvidence {
    source_sample: SourceSampleTarget,
    decode_resolution: &'static str,
    temporal_selection: TemporalSelectionEvidence,
    source_rgba_sha256: String,
    program_rgba_sha256: String,
    decode_execution: crate::app::preview_execution::PreviewDecodeExecutionSummary,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct TemporalSelectionEvidence {
    requested_pts: i64,
    selected_pts: i64,
    selected_duration_pts: i64,
    extent_source: PreviewTemporalExtentSource,
}

struct ProgramReference {
    evidence: ProgramReferenceEvidence,
    resolution: Resolution,
    rgba: Vec<u8>,
}

#[derive(Debug, Serialize)]
struct RetimeReimportEvidence {
    asset_id: AssetId,
    path_sha256: String,
    decoded_rgba_sha256: String,
    decode_execution: crate::app::preview_execution::PreviewDecodeExecutionSummary,
    expected_difference: PixelDifferenceEvidence,
    counterfactual_differences: Vec<LabeledPixelDifferenceEvidence>,
    expected_mean_absolute_error_max: f64,
    expected_max_channel_error_max: u8,
    counterfactual_mean_error_margin_min: f64,
    counterfactual_mean_error_ratio_min: f64,
}

#[derive(Debug, Serialize)]
struct LabeledPixelDifferenceEvidence {
    id: &'static str,
    difference: PixelDifferenceEvidence,
}

#[derive(Debug, Serialize)]
struct PixelDifferenceEvidence {
    sampled_pixels: u32,
    sampled_channels: u32,
    mean_absolute_error: f64,
    max_channel_error: u8,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum RetimeCadence {
    Cfr25,
    VfrAlternating20And60Ms,
}

#[derive(Debug, Clone, Copy)]
struct RetimeFixtureTarget {
    id: &'static str,
    clip_id: ClipId,
    asset_id: AssetId,
    window: GoldenTimelineWindow,
    cadence: RetimeCadence,
    export_file_name: &'static str,
}

pub(super) fn execute_retime_media_evidence(
    state: &mut AppState,
    contract: &GoldenProjectContract,
    output_directory: &Path,
    clip_id: ClipId,
    asset_id: AssetId,
    window: GoldenTimelineWindow,
    vfr_clip_id: ClipId,
    vfr_asset_id: AssetId,
    vfr_window: GoldenTimelineWindow,
) -> anyhow::Result<GoldenRetimeMediaEvidence> {
    let cfr = execute_fixture_retime_media_evidence(
        state,
        contract,
        output_directory,
        RetimeFixtureTarget {
            id: "cfr-25fps",
            clip_id,
            asset_id,
            window,
            cadence: RetimeCadence::Cfr25,
            export_file_name: "cfr-reverse-hold-h264-aac-sdr.mp4",
        },
    )?;
    let vfr = execute_fixture_retime_media_evidence(
        state,
        contract,
        output_directory,
        RetimeFixtureTarget {
            id: "vfr-alternating-20-60ms",
            clip_id: vfr_clip_id,
            asset_id: vfr_asset_id,
            window: vfr_window,
            cadence: RetimeCadence::VfrAlternating20And60Ms,
            export_file_name: "vfr-reverse-hold-h264-aac-sdr.mp4",
        },
    )?;
    Ok(GoldenRetimeMediaEvidence { fixtures: vec![cfr, vfr] })
}

fn execute_fixture_retime_media_evidence(
    state: &mut AppState,
    contract: &GoldenProjectContract,
    output_directory: &Path,
    target: RetimeFixtureTarget,
) -> anyhow::Result<RetimeFixtureEvidence> {
    let RetimeFixtureTarget {
        id: fixture_id,
        clip_id,
        asset_id,
        window,
        cadence,
        export_file_name,
    } = target;
    let forward_rate = TimeScale::new(1, 2)?;
    let reverse_rate = TimeScale::new(-1, 2)?;
    let forward_half = dispatch_author_transition(
        state,
        "set-proxy-relink-half-rate",
        crate::app::ui_actions::clip_set_rate_action(
            crate::app::product_action::ClipSetRatePayload {
                clip_id,
                rate: forward_rate,
                include_linked: true,
            },
        ),
    )?;
    let forward_sequence =
        state.active_sequence().cloned().context("Retime Hero Sequence is absent")?;
    let time_base = forward_sequence.time_base();
    let sample_frame = window
        .start_frame
        .checked_add(SAMPLE_FRAME_OFFSET)
        .context("retime sample frame overflowed")?;
    let hold_frame = window
        .start_frame
        .checked_add(HOLD_FRAME_OFFSET)
        .context("retime hold frame overflowed")?;
    let export_end_frame = hold_frame
        .checked_add(EXPORT_DURATION_FRAMES)
        .context("retime export range overflowed")?;
    ensure!(
        export_end_frame <= window.end_frame_exclusive,
        "retime evidence range escaped the Proxy/Relink Hero window"
    );
    let clip = find_clip(&forward_sequence, clip_id)?;
    ensure!(
        clip.media_asset_id() == Some(asset_id) && clip.source_time_scale() == forward_rate,
        "retime Action did not preserve the target media identity and exact rate"
    );
    let sample_time =
        TimelineTime::from_frame_position(FramePosition::new(sample_frame, time_base))?;
    let expected_forward_sample = clip.timeline_to_source_sample(sample_time)?;
    ensure!(
        expected_forward_sample == SourceSampleTarget::covering(TimelineTime::new(1, 1)?),
        "50% Golden retime did not map the sample frame to exact covering source time 1s"
    );
    let (forward_preview_sample, forward_export_sample) = plan_source_samples(
        &forward_sequence,
        asset_id,
        sample_frame,
        expected_forward_sample,
    )?;

    let output_resolution = contract
        .exports
        .iter()
        .find(|export| export.id == RETIME_EXPORT_ID)
        .map(|export| Resolution {
            width: export.expected_delivery.width,
            height: export.expected_delivery.height,
        })
        .context("Golden H.264 export contract is absent")?;
    let asset = state
        .asset_library()
        .context("Asset Library is absent")?
        .list_assets()?
        .into_iter()
        .find(|asset| asset.id == asset_id)
        .context("retime media Asset is absent")?;
    let preview_proxy = resolve_media_path_for_preference(state, &asset, true)?;
    let immutable_export_source = resolve_media_path_for_preference(state, &asset, false)?;
    ensure!(
        preview_proxy.resolution == "proxy"
            && immutable_export_source.resolution == "source"
            && preview_proxy.path != immutable_export_source.path
            && asset.file_path() == Some(immutable_export_source.path.as_path()),
        "retime media bindings did not distinguish product proxy Preview from immutable original Export"
    );

    let mut decode_context = PreviewDecodeSessionContext::new();
    let forward_expected = render_program_reference(
        state,
        &forward_sequence,
        sample_frame,
        output_resolution,
        asset_id,
        expected_forward_sample,
        &mut decode_context,
    )?;

    let mut counterfactual_sequence = forward_sequence.clone();
    let counterfactual_clip = find_clip_mut(&mut counterfactual_sequence, clip_id)?;
    counterfactual_clip
        .set_constant_source_time_map(counterfactual_clip.source_origin(), TimeScale::ONE)?;
    let counterfactual_source_sample =
        counterfactual_clip.timeline_to_source_sample(sample_time)?;
    ensure!(
        counterfactual_source_sample != expected_forward_sample,
        "counterfactual 100% map did not produce a distinct source sample"
    );
    let forward_counterfactual = render_program_reference(
        state,
        &counterfactual_sequence,
        sample_frame,
        output_resolution,
        asset_id,
        counterfactual_source_sample,
        &mut decode_context,
    )?;
    ensure!(
        forward_expected.evidence.source_rgba_sha256
            != forward_counterfactual.evidence.source_rgba_sha256
            && forward_expected.evidence.program_rgba_sha256
                != forward_counterfactual.evidence.program_rgba_sha256,
        "Golden H.264 stimulus does not distinguish 50% source time from 100%"
    );
    ensure_covering_selection(&forward_expected, cadence)?;
    let forward_proxy = render_program_reference_with_preference(
        state,
        &forward_sequence,
        sample_frame,
        output_resolution,
        asset_id,
        ProgramReferenceDecodeRequest::proxy(expected_forward_sample),
        &mut decode_context,
    )?;
    let forward_proxy_evidence = prove_proxy_program(
        forward_proxy,
        &forward_expected,
        [("full-rate", &forward_counterfactual)],
    )?;

    let reverse_half = dispatch_author_transition(
        state,
        "set-proxy-relink-reverse-half-rate",
        crate::app::ui_actions::clip_set_rate_action(
            crate::app::product_action::ClipSetRatePayload {
                clip_id,
                rate: reverse_rate,
                include_linked: true,
            },
        ),
    )?;
    let reverse_sequence = state
        .active_sequence()
        .cloned()
        .context("reverse Retime Hero Sequence is absent")?;
    let reverse_clip = find_clip(&reverse_sequence, clip_id)?;
    let expected_reverse_sample = reverse_clip.timeline_to_source_sample(sample_time)?;
    ensure!(
        reverse_clip.source_time_scale() == reverse_rate
            && expected_reverse_sample
                == SourceSampleTarget::strict_predecessor(TimelineTime::new(2, 1)?),
        "reverse 50% Golden retime did not preserve the old span or exact strict predecessor"
    );
    let (reverse_preview_sample, reverse_export_sample) = plan_source_samples(
        &reverse_sequence,
        asset_id,
        sample_frame,
        expected_reverse_sample,
    )?;
    let reverse_expected = render_program_reference(
        state,
        &reverse_sequence,
        sample_frame,
        output_resolution,
        asset_id,
        expected_reverse_sample,
        &mut decode_context,
    )?;
    let reverse_adjacent_sequence =
        covering_hold_counterfactual(&reverse_sequence, clip_id, expected_reverse_sample.time())?;
    let reverse_adjacent = render_program_reference(
        state,
        &reverse_adjacent_sequence,
        sample_frame,
        output_resolution,
        asset_id,
        SourceSampleTarget::covering(expected_reverse_sample.time()),
        &mut decode_context,
    )?;
    let reverse_wrong_direction_sample =
        find_clip(&forward_sequence, clip_id)?.timeline_to_source_sample(sample_time)?;
    let reverse_wrong_direction = render_program_reference(
        state,
        &forward_sequence,
        sample_frame,
        output_resolution,
        asset_id,
        reverse_wrong_direction_sample,
        &mut decode_context,
    )?;
    ensure_strict_predecessor_selection(&reverse_expected, &reverse_adjacent, cadence)?;
    ensure_distinct_programs(
        &reverse_expected,
        [
            ("adjacent-covering", &reverse_adjacent),
            ("wrong-direction", &reverse_wrong_direction),
        ],
    )?;
    let reverse_proxy = render_program_reference_with_preference(
        state,
        &reverse_sequence,
        sample_frame,
        output_resolution,
        asset_id,
        ProgramReferenceDecodeRequest::proxy(expected_reverse_sample),
        &mut decode_context,
    )?;
    let reverse_proxy_evidence = prove_proxy_program(
        reverse_proxy,
        &reverse_expected,
        [
            ("adjacent-covering", &reverse_adjacent),
            ("wrong-direction", &reverse_wrong_direction),
        ],
    )?;

    state.seek(sample_frame)?;
    let mut viewer = GoldenHeadlessPreview::new()?;
    let reverse_presentation = viewer.present_current(state, VIEWER_PRESENTATION_TIMEOUT)?;

    let reverse_hold = dispatch_author_transition(
        state,
        "hold-proxy-relink-reverse-frame",
        crate::app::ui_actions::clip_hold_frame_action(
            crate::app::product_action::ClipHoldFramePayload {
                clip_id,
                sequence_time: FramePosition::new(sample_frame, time_base),
            },
        ),
    )?;
    let held_sequence = state
        .active_sequence()
        .cloned()
        .context("held Retime Hero Sequence is absent")?;
    let hold_time = TimelineTime::from_frame_position(FramePosition::new(hold_frame, time_base))?;
    let expected_hold_sample =
        find_clip(&held_sequence, clip_id)?.timeline_to_source_sample(hold_time)?;
    ensure!(
        find_clip(&held_sequence, clip_id)?.source_time_scale() == TimeScale::ZERO
            && expected_hold_sample == expected_reverse_sample,
        "reverse hold did not retain the complete captured source sample"
    );
    let (hold_preview_sample, hold_export_sample) =
        plan_source_samples(&held_sequence, asset_id, hold_frame, expected_hold_sample)?;
    let hold_expected = render_program_reference(
        state,
        &held_sequence,
        hold_frame,
        output_resolution,
        asset_id,
        expected_hold_sample,
        &mut decode_context,
    )?;
    let hold_adjacent_sequence =
        covering_hold_counterfactual(&held_sequence, clip_id, expected_hold_sample.time())?;
    let hold_adjacent = render_program_reference(
        state,
        &hold_adjacent_sequence,
        hold_frame,
        output_resolution,
        asset_id,
        SourceSampleTarget::covering(expected_hold_sample.time()),
        &mut decode_context,
    )?;
    let hold_wrong_direction_sample =
        find_clip(&forward_sequence, clip_id)?.timeline_to_source_sample(hold_time)?;
    let hold_wrong_direction = render_program_reference(
        state,
        &forward_sequence,
        hold_frame,
        output_resolution,
        asset_id,
        hold_wrong_direction_sample,
        &mut decode_context,
    )?;
    ensure_strict_predecessor_selection(&hold_expected, &hold_adjacent, cadence)?;
    ensure!(
        hold_expected.evidence.source_rgba_sha256 == reverse_expected.evidence.source_rgba_sha256,
        "reverse hold did not retain the decoded picture captured before the hold"
    );
    ensure_distinct_programs(
        &hold_expected,
        [
            ("adjacent-covering", &hold_adjacent),
            ("wrong-direction", &hold_wrong_direction),
        ],
    )?;
    let hold_proxy = render_program_reference_with_preference(
        state,
        &held_sequence,
        hold_frame,
        output_resolution,
        asset_id,
        ProgramReferenceDecodeRequest::proxy(expected_hold_sample),
        &mut decode_context,
    )?;
    let hold_proxy_evidence = prove_proxy_program(
        hold_proxy,
        &hold_expected,
        [
            ("adjacent-covering", &hold_adjacent),
            ("wrong-direction", &hold_wrong_direction),
        ],
    )?;
    decode_context.clear();

    state.seek(hold_frame)?;
    let hold_presentation = viewer.present_current(state, VIEWER_PRESENTATION_TIMEOUT)?;
    let viewer_evidence = viewer.evidence();
    ensure!(
        viewer_evidence.presentations == 2 && viewer_evidence.completed_demands == 2,
        "retimed Headless Viewer did not complete reverse and held proxy demands"
    );

    let export_contract = contract
        .exports
        .iter()
        .find(|export| export.id == RETIME_EXPORT_ID)
        .context("Golden H.264 export contract is absent")?;
    let export = export_and_probe(
        state,
        export_contract,
        output_directory.join(export_file_name),
        TimelineExportRange::WorkArea {
            start_frame: hold_frame,
            end_frame_exclusive: export_end_frame,
        },
        held_sequence.settings.frame_rate,
        EXPORT_DURATION_FRAMES,
        contract.acceptance.duration_error_max_frames,
        contract.acceptance.av_boundary_error_max_ms,
        held_sequence.settings.audio_sample_rate,
    )?;
    let (_, undo_hold) = author_transition(state, "undo-proxy-relink-reverse-hold", |state| {
        ensure!(state.undo_timeline()?, "reverse hold had no Undo entry");
        Ok(())
    })?;
    let (_, undo_reverse) = author_transition(state, "undo-proxy-relink-reverse-rate", |state| {
        ensure!(state.undo_timeline()?, "reverse retime had no Undo entry");
        Ok(())
    })?;
    let restored_sequence =
        state.active_sequence().context("restored Retime Hero Sequence is absent")?;
    ensure!(
        find_clip(restored_sequence, clip_id)?.source_time_scale() == forward_rate
            && find_clip(restored_sequence, clip_id)?.timeline_to_source_sample(sample_time)?
                == expected_forward_sample,
        "signed retime evidence did not restore the retained 50% authoring anchor"
    );
    let reimport = reimport_and_compare(
        state,
        &export,
        &hold_expected,
        [
            ("adjacent-covering", &hold_adjacent),
            ("wrong-direction", &hold_wrong_direction),
        ],
    )?;

    Ok(RetimeFixtureEvidence {
        fixture_id,
        cadence,
        author: RetimeAuthorEvidence {
            forward_half,
            reverse_half,
            reverse_hold,
            undo_hold,
            undo_reverse,
        },
        media_binding: RetimeMediaBindingEvidence { preview_proxy, immutable_export_source },
        forward: RetimeScenarioEvidence {
            id: "forward-half",
            rate: forward_rate,
            sample_frame,
            expected_source_sample: expected_forward_sample,
            preview_source_sample: forward_preview_sample,
            export_source_sample: forward_export_sample,
            expected_program: forward_expected.evidence,
            preview_proxy: forward_proxy_evidence,
            counterfactual_programs: vec![LabeledProgramReferenceEvidence {
                id: "full-rate",
                program: forward_counterfactual.evidence,
            }],
        },
        reverse: RetimeScenarioEvidence {
            id: "reverse-half",
            rate: reverse_rate,
            sample_frame,
            expected_source_sample: expected_reverse_sample,
            preview_source_sample: reverse_preview_sample,
            export_source_sample: reverse_export_sample,
            expected_program: reverse_expected.evidence,
            preview_proxy: reverse_proxy_evidence,
            counterfactual_programs: vec![
                LabeledProgramReferenceEvidence {
                    id: "adjacent-covering",
                    program: reverse_adjacent.evidence,
                },
                LabeledProgramReferenceEvidence {
                    id: "wrong-direction",
                    program: reverse_wrong_direction.evidence,
                },
            ],
        },
        reverse_hold: RetimeScenarioEvidence {
            id: "reverse-hold",
            rate: TimeScale::ZERO,
            sample_frame: hold_frame,
            expected_source_sample: expected_hold_sample,
            preview_source_sample: hold_preview_sample,
            export_source_sample: hold_export_sample,
            expected_program: hold_expected.evidence,
            preview_proxy: hold_proxy_evidence,
            counterfactual_programs: vec![
                LabeledProgramReferenceEvidence {
                    id: "adjacent-covering",
                    program: hold_adjacent.evidence,
                },
                LabeledProgramReferenceEvidence {
                    id: "wrong-direction",
                    program: hold_wrong_direction.evidence,
                },
            ],
        },
        presentations: vec![reverse_presentation, hold_presentation],
        viewer: viewer_evidence,
        export,
        reimport,
    })
}

fn plan_source_samples(
    sequence: &Sequence,
    asset_id: AssetId,
    frame: i64,
    expected: SourceSampleTarget,
) -> anyhow::Result<(SourceSampleTarget, SourceSampleTarget)> {
    let position = FramePosition::new(frame, sequence.time_base());
    let preview = plan_source_sample(
        sequence,
        asset_id,
        TimelineEvaluationRequest::preview(position, 1.0),
    )?;
    let export = plan_source_sample(
        sequence,
        asset_id,
        TimelineEvaluationRequest::export(position),
    )?;
    ensure!(
        preview == expected && export == expected,
        "Preview and Export plans disagree with the canonical retime source sample"
    );
    Ok((preview, export))
}

fn plan_source_sample(
    sequence: &Sequence,
    asset_id: AssetId,
    request: TimelineEvaluationRequest,
) -> anyhow::Result<SourceSampleTarget> {
    let program = PreparedVisualProgram::prepare(sequence)?;
    let plan = evaluate_prepared_visual_program(&program, request)?;
    let media = plan
        .elements
        .iter()
        .filter_map(|element| match element {
            TimelineRenderPlanElement::Media(media) if media.asset_id == asset_id => Some(media),
            _ => None,
        })
        .collect::<Vec<_>>();
    ensure!(
        media.len() == 1,
        "retime frame did not resolve exactly one target media layer"
    );
    Ok(media[0].source_sample)
}

fn covering_hold_counterfactual(
    sequence: &Sequence,
    clip_id: ClipId,
    source_time: TimelineTime,
) -> anyhow::Result<Sequence> {
    let mut counterfactual = sequence.clone();
    find_clip_mut(&mut counterfactual, clip_id)?.replace_source_time_map(
        ClipSourceTimeMap::hold(SourceSampleTarget::covering(source_time)),
    )?;
    Ok(counterfactual)
}

fn ensure_covering_selection(
    reference: &ProgramReference,
    cadence: RetimeCadence,
) -> anyhow::Result<()> {
    let selection = reference.evidence.temporal_selection;
    let selected_end = selection
        .selected_pts
        .checked_add(selection.selected_duration_pts)
        .context("covering selection temporal extent overflowed")?;
    ensure!(
        reference.evidence.source_sample.boundary()
            == mondrian_core::SourceSamplingBoundary::Covering
            && selection.selected_duration_pts > 0
            && selection.requested_pts >= selection.selected_pts
            && selection.requested_pts < selected_end,
        "covering target did not select a proven interval containing its requested PTS"
    );
    match cadence {
        RetimeCadence::Cfr25 => ensure!(
            selection.requested_pts == selection.selected_pts
                && reference
                    .evidence
                    .source_sample
                    .to_frame_position(mondrian_core::Rational::FPS_25)?
                    .frame
                    == 25,
            "covering CFR target differs from its independent 25 fps frame oracle"
        ),
        RetimeCadence::VfrAlternating20And60Ms => ensure!(
            selection.requested_pts > selection.selected_pts,
            "VFR covering oracle did not exercise a request inside a presentation interval"
        ),
    }
    Ok(())
}

fn ensure_strict_predecessor_selection(
    expected: &ProgramReference,
    adjacent_covering: &ProgramReference,
    cadence: RetimeCadence,
) -> anyhow::Result<()> {
    let strict = expected.evidence.temporal_selection;
    let adjacent = adjacent_covering.evidence.temporal_selection;
    ensure!(
        expected.evidence.source_sample.boundary()
            == mondrian_core::SourceSamplingBoundary::StrictPredecessor
            && adjacent_covering.evidence.source_sample
                == SourceSampleTarget::covering(expected.evidence.source_sample.time())
            && strict.requested_pts.checked_add(1) == Some(adjacent.requested_pts)
            && strict.selected_pts.checked_add(strict.selected_duration_pts)
                == Some(adjacent.selected_pts)
            && adjacent.requested_pts == adjacent.selected_pts
            && strict.selected_duration_pts > 0
            && adjacent.selected_duration_pts > 0,
        "strict-predecessor decode did not select the interval immediately before the covering boundary"
    );
    match cadence {
        RetimeCadence::Cfr25 => ensure!(
            strict.selected_duration_pts == adjacent.selected_duration_pts
                && expected
                    .evidence
                    .source_sample
                    .to_frame_position(mondrian_core::Rational::FPS_25)?
                    .frame
                    == 49,
            "strict-predecessor CFR target differs from its independent 25 fps frame oracle"
        ),
        RetimeCadence::VfrAlternating20And60Ms => ensure!(
            strict.selected_duration_pts > adjacent.selected_duration_pts,
            "VFR strict-predecessor oracle did not cross unequal 60/20 ms intervals"
        ),
    }
    Ok(())
}

fn ensure_distinct_programs<const N: usize>(
    expected: &ProgramReference,
    counterfactuals: [(&str, &ProgramReference); N],
) -> anyhow::Result<()> {
    for (id, counterfactual) in counterfactuals {
        ensure!(
            expected.evidence.source_rgba_sha256 != counterfactual.evidence.source_rgba_sha256
                && expected.evidence.program_rgba_sha256
                    != counterfactual.evidence.program_rgba_sha256,
            "retime stimulus does not distinguish expected Program from {id} counterfactual"
        );
    }
    Ok(())
}

fn render_program_reference(
    state: &AppState,
    sequence: &Sequence,
    frame: i64,
    resolution: Resolution,
    asset_id: AssetId,
    expected_source_sample: SourceSampleTarget,
    decode_context: &mut PreviewDecodeSessionContext,
) -> anyhow::Result<ProgramReference> {
    render_program_reference_with_preference(
        state,
        sequence,
        frame,
        resolution,
        asset_id,
        ProgramReferenceDecodeRequest::source(expected_source_sample),
        decode_context,
    )
}

#[derive(Debug, Clone, Copy)]
struct ProgramReferenceDecodeRequest {
    expected_source_sample: SourceSampleTarget,
    prefer_proxy: bool,
}

impl ProgramReferenceDecodeRequest {
    const fn source(expected_source_sample: SourceSampleTarget) -> Self {
        Self { expected_source_sample, prefer_proxy: false }
    }

    const fn proxy(expected_source_sample: SourceSampleTarget) -> Self {
        Self { expected_source_sample, prefer_proxy: true }
    }
}

fn render_program_reference_with_preference(
    state: &AppState,
    sequence: &Sequence,
    frame: i64,
    resolution: Resolution,
    asset_id: AssetId,
    decode_request: ProgramReferenceDecodeRequest,
    decode_context: &mut PreviewDecodeSessionContext,
) -> anyhow::Result<ProgramReference> {
    let assets = state
        .asset_library()
        .context("Asset Library is absent")?
        .list_assets()?
        .into_iter()
        .map(|asset| (asset.id, asset))
        .collect::<HashMap<_, _>>();
    let mut request_source_sample = None;
    let mut source_rgba_sha256 = None;
    let mut decode_execution = None;
    let mut decode_resolution = None;
    let mut temporal_selection = None;
    let mut adapter_failure = None;
    let mut media_frame = |request: PreviewTimelineMediaRequest| {
        if request.asset_id != asset_id {
            adapter_failure = Some(format!(
                "unexpected retime media Asset: {}",
                request.asset_id
            ));
            return PreviewTimelineMediaFrame::Unavailable {
                reason: PreviewUnavailability::blocked(
                    PreviewOutputStage::MediaResolution,
                    "retime frame resolved an unrelated media Asset",
                ),
            };
        }
        let outcome = assets
            .get(&request.asset_id)
            .with_context(|| format!("Retime Preview asset is absent: {}", request.asset_id))
            .and_then(|asset| {
                decode_media_with_preference(
                    state,
                    &request,
                    asset,
                    decode_context,
                    decode_request.prefer_proxy,
                )
            });
        match outcome {
            Ok(media) => {
                request_source_sample = Some(request.source_sample);
                decode_execution = Some(media.frame.decode_execution());
                decode_resolution = Some(path_resolution_label(media.path_resolution));
                match source_temporal_selection(&media.frame)
                    .and_then(temporal_selection_from_decode)
                {
                    Ok(selection) => temporal_selection = Some(selection),
                    Err(error) => {
                        adapter_failure = Some(error.to_string());
                        return PreviewTimelineMediaFrame::Unavailable {
                            reason: PreviewUnavailability::blocked(
                                PreviewOutputStage::MediaResolution,
                                "retime temporal-selection evidence is incomplete",
                            ),
                        };
                    }
                }
                match source_rgba(&media.frame) {
                    Ok(rgba) => {
                        source_rgba_sha256 = Some(sha256_bytes(&rgba));
                        PreviewTimelineMediaFrame::Ready(media.frame)
                    }
                    Err(error) => {
                        adapter_failure = Some(error.to_string());
                        PreviewTimelineMediaFrame::Unavailable {
                            reason: PreviewUnavailability::blocked(
                                PreviewOutputStage::MediaResolution,
                                "retime source pixel extraction failed",
                            ),
                        }
                    }
                }
            }
            Err(error) => {
                adapter_failure = Some(error.to_string());
                PreviewTimelineMediaFrame::Unavailable {
                    reason: PreviewUnavailability::blocked(
                        PreviewOutputStage::MediaResolution,
                        "retime media decode Adapter failed",
                    ),
                }
            }
        }
    };
    let mut title_frame = |_request| PreviewTimelineTitleFrame::Unavailable {
        reason: PreviewUnavailability::blocked(
            PreviewOutputStage::GeneratedSource,
            "retime window contains no generated titles",
        ),
    };
    let color_context = sequence
        .settings
        .root_program_color_context(state.project_color_environment())?;
    let resolved = match resolve_preview_timeline(
        sequence,
        std::slice::from_ref(sequence),
        frame,
        resolution,
        PreviewResolutionScale::Full,
        color_context,
        &mut media_frame,
        &mut title_frame,
    ) {
        PreviewTimelineResolution::Ready(resolved) => resolved,
        PreviewTimelineResolution::Empty => bail!("retime Program reference resolved as empty"),
        PreviewTimelineResolution::Pending { .. } => {
            bail!("retime Program reference retained a pending dependency")
        }
        PreviewTimelineResolution::Unavailable { reason } => bail!(
            "retime Program reference unavailable: {reason:?}; Adapter: {}",
            adapter_failure.as_deref().unwrap_or("no Adapter diagnostic")
        ),
    };
    ensure!(
        resolved.plan.elements.len() == 1
            && matches!(
                resolved.plan.elements.first(),
                Some(ResolvedPreviewElement::Media { .. })
            ),
        "retime Program reference did not resolve exactly one target media layer"
    );
    ensure!(
        request_source_sample == Some(decode_request.expected_source_sample),
        "real media Adapter received a source sample different from the canonical map"
    );
    let mut scratch = TimelineCompositeScratch::default();
    let working = composite_resolved_preview_working(
        resolution.width,
        resolution.height,
        &resolved.plan.elements,
        &resolved.plan.color_context,
        &mut scratch,
    )?;
    ensure!(
        working.composite_diagnostics.float_linear_composites == 1
            && working.composite_diagnostics.legacy_rgba8_composites == 0
            && !working.composite_diagnostics.is_color_domain_blocked(),
        "retime Program reference left the working-linear path"
    );
    let mut flattened = working.frame.into_rgba_f32();
    for pixel in &mut flattened.data {
        let coverage = pixel[3].clamp(0.0, 1.0);
        pixel[0] *= coverage;
        pixel[1] *= coverage;
        pixel[2] *= coverage;
        pixel[3] = 1.0;
    }
    let program_output_color = resolved
        .plan
        .color_context
        .output_color_space()
        .color()
        .context("Golden Program Output is not an encoded color space")?;
    let boundary = RenderOutputColorBoundary::from_intent(
        RenderOutputColorBoundaryTarget::Export,
        program_output_color,
        resolved.plan.color_context.output_transform(),
        resolved.plan.color_context.output_tone_map(),
        resolved.plan.color_context.engine().clone(),
    )?;
    let rgba = execute_cpu_output_boundary(&CpuColorFrame::working(flattened), &boundary)?
        .result
        .frame
        .into_rgba();
    Ok(ProgramReference {
        evidence: ProgramReferenceEvidence {
            source_sample: decode_request.expected_source_sample,
            decode_resolution: decode_resolution
                .context("retime decode produced no source/proxy resolution evidence")?,
            temporal_selection: temporal_selection
                .context("retime decode produced no temporal-selection evidence")?,
            source_rgba_sha256: source_rgba_sha256
                .context("retime decode produced no source raster hash")?,
            program_rgba_sha256: sha256_bytes(&rgba),
            decode_execution: decode_execution
                .context("retime decode produced no execution evidence")?,
        },
        resolution,
        rgba,
    })
}

fn temporal_selection_from_decode(
    selection: PreviewDecodeTemporalSelection,
) -> anyhow::Result<TemporalSelectionEvidence> {
    let selected_end = selection
        .selected_pts
        .checked_add(selection.selected_duration_pts)
        .context("retime selected temporal extent overflowed")?;
    ensure!(
        selection.requested_pts >= selection.selected_pts
            && selection.requested_pts < selected_end
            && !selection.temporal_approximation,
        "retime exact decode did not return a proven interval covering its requested PTS"
    );
    Ok(TemporalSelectionEvidence {
        requested_pts: selection.requested_pts,
        selected_pts: selection.selected_pts,
        selected_duration_pts: selection.selected_duration_pts,
        extent_source: selection.extent_source,
    })
}

fn prove_proxy_program<const N: usize>(
    proxy: ProgramReference,
    expected_original: &ProgramReference,
    counterfactuals: [(&'static str, &ProgramReference); N],
) -> anyhow::Result<RetimeProxyProgramEvidence> {
    ensure!(
        proxy.resolution == expected_original.resolution
            && proxy.evidence.source_sample == expected_original.evidence.source_sample,
        "proxy Program does not represent the original Program's exact source target and extent"
    );
    ensure!(
        proxy.evidence.decode_resolution == "proxy"
            && expected_original.evidence.decode_resolution == "source",
        "proxy/original Program comparison did not decode its declared physical bindings"
    );
    let expected_difference =
        sampled_difference(&proxy.rgba, &expected_original.rgba, proxy.resolution)?;
    ensure!(
        expected_difference.mean_absolute_error <= PROXY_EXPECTED_MEAN_ABSOLUTE_ERROR_MAX
            && expected_difference.max_channel_error <= PROXY_EXPECTED_MAX_CHANNEL_ERROR_MAX,
        "proxy Program exceeds its original-source tolerance: mean {:.3}, max {}",
        expected_difference.mean_absolute_error,
        expected_difference.max_channel_error
    );
    let mut counterfactual_differences = Vec::with_capacity(counterfactuals.len());
    for (id, counterfactual) in counterfactuals {
        ensure!(
            proxy.resolution == counterfactual.resolution,
            "proxy {id} counterfactual uses a different output extent"
        );
        let difference = sampled_difference(&proxy.rgba, &counterfactual.rgba, proxy.resolution)?;
        ensure!(
            difference.mean_absolute_error
                >= expected_difference.mean_absolute_error
                    + PROXY_COUNTERFACTUAL_MEAN_ERROR_MARGIN_MIN,
            "proxy Program is not materially closer to the expected original than the {id} counterfactual: expected mean {:.3}, counterfactual mean {:.3}",
            expected_difference.mean_absolute_error,
            difference.mean_absolute_error
        );
        counterfactual_differences.push(LabeledPixelDifferenceEvidence { id, difference });
    }
    Ok(RetimeProxyProgramEvidence {
        program: proxy.evidence,
        expected_difference,
        counterfactual_differences,
        expected_mean_absolute_error_max: PROXY_EXPECTED_MEAN_ABSOLUTE_ERROR_MAX,
        expected_max_channel_error_max: PROXY_EXPECTED_MAX_CHANNEL_ERROR_MAX,
        counterfactual_mean_error_margin_min: PROXY_COUNTERFACTUAL_MEAN_ERROR_MARGIN_MIN,
    })
}

fn reimport_and_compare<const N: usize>(
    state: &mut AppState,
    export: &ExportEvidence,
    expected: &ProgramReference,
    counterfactuals: [(&'static str, &ProgramReference); N],
) -> anyhow::Result<RetimeReimportEvidence> {
    for (id, counterfactual) in &counterfactuals {
        ensure!(
            expected.resolution == counterfactual.resolution,
            "retime {id} counterfactual uses a different output extent"
        );
    }
    let before = state
        .asset_library()
        .context("Asset Library is absent")?
        .list_assets()?
        .into_iter()
        .map(|asset| asset.id)
        .collect::<BTreeSet<_>>();
    state.dispatch_action(Action::ImportMedia(vec![export
        .output_path()
        .to_path_buf()]))?;
    wait_for_media_imports(state)?;
    let imported = state
        .asset_library()
        .context("Asset Library is absent after retime reimport")?
        .list_assets()?
        .into_iter()
        .filter(|asset| !before.contains(&asset.id))
        .collect::<Vec<_>>();
    ensure!(
        imported.len() == 1,
        "retime export reimport created {} Assets instead of one",
        imported.len()
    );
    let asset = &imported[0];
    let video = asset
        .media_probe()
        .context("retime reimport has no coherent media probe")?
        .primary_video()
        .context("retime reimport has no video stream")?;
    ensure!(
        asset.kind == AssetKind::Video
            && asset.file_path() == Some(export.output_path())
            && video.codec == mondrian_media::info::VideoCodec::H264
            && video.codec_profile == VideoCodecProfile::H264High
            && video.width == expected.resolution.width
            && video.height == expected.resolution.height,
        "retime reimport differs from the validated H.264 delivery"
    );
    let input_color = state
        .active_sequence()
        .context("active Sequence is absent")?
        .settings
        .root_program_color_context(state.project_color_environment())?
        .media_input(false);
    let request = PreviewTimelineMediaRequest {
        asset_id: asset.id,
        color_space_override: None,
        alpha_interpretation: AlphaInterpretation::Ignore,
        picture_overrides: Default::default(),
        source_sample: mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
        target_resolution: expected.resolution,
        input_color,
        cpu_working_required: false,
    };
    let mut decode_context = PreviewDecodeSessionContext::new();
    let decoded = decode_media(state, &request, asset, &mut decode_context)?;
    let decoded_rgba = source_rgba(&decoded.frame)?;
    let expected_difference =
        sampled_difference(&expected.rgba, &decoded_rgba, expected.resolution)?;
    let mut counterfactual_differences = Vec::with_capacity(counterfactuals.len());
    for (id, counterfactual) in counterfactuals {
        counterfactual_differences.push(LabeledPixelDifferenceEvidence {
            id,
            difference: sampled_difference(
                &counterfactual.rgba,
                &decoded_rgba,
                expected.resolution,
            )?,
        });
    }
    ensure!(
        expected_difference.mean_absolute_error <= EXPECTED_MEAN_ABSOLUTE_ERROR_MAX
            && expected_difference.max_channel_error <= EXPECTED_MAX_CHANNEL_ERROR_MAX,
        "retime export differs from the expected reverse-hold Program reference: mean {:.3}, max {}",
        expected_difference.mean_absolute_error,
        expected_difference.max_channel_error
    );
    for counterfactual in &counterfactual_differences {
        ensure!(
            counterfactual.difference.mean_absolute_error
                >= expected_difference.mean_absolute_error
                    + COUNTERFACTUAL_MEAN_ERROR_MARGIN_MIN
                && counterfactual.difference.mean_absolute_error
                    >= expected_difference.mean_absolute_error
                        * COUNTERFACTUAL_MEAN_ERROR_RATIO_MIN,
            "retime export is not materially closer to the reverse-hold reference than the {} counterfactual: expected mean {:.3}, counterfactual mean {:.3}, required margin {:.3}, ratio {:.3}",
            counterfactual.id,
            expected_difference.mean_absolute_error,
            counterfactual.difference.mean_absolute_error,
            COUNTERFACTUAL_MEAN_ERROR_MARGIN_MIN,
            COUNTERFACTUAL_MEAN_ERROR_RATIO_MIN
        );
    }
    Ok(RetimeReimportEvidence {
        asset_id: asset.id,
        path_sha256: sha256_file(export.output_path())?,
        decoded_rgba_sha256: sha256_bytes(&decoded_rgba),
        decode_execution: decoded.frame.decode_execution(),
        expected_difference,
        counterfactual_differences,
        expected_mean_absolute_error_max: EXPECTED_MEAN_ABSOLUTE_ERROR_MAX,
        expected_max_channel_error_max: EXPECTED_MAX_CHANNEL_ERROR_MAX,
        counterfactual_mean_error_margin_min: COUNTERFACTUAL_MEAN_ERROR_MARGIN_MIN,
        counterfactual_mean_error_ratio_min: COUNTERFACTUAL_MEAN_ERROR_RATIO_MIN,
    })
}

fn sampled_difference(
    left: &[u8],
    right: &[u8],
    resolution: Resolution,
) -> anyhow::Result<PixelDifferenceEvidence> {
    let expected_len = resolution.width as usize * resolution.height as usize * 4;
    ensure!(
        left.len() == expected_len && right.len() == expected_len,
        "retime comparison raster extent is invalid"
    );
    let crop_x = resolution.width / 4;
    let crop_y = resolution.height / 4;
    let crop_width = resolution.width / 2;
    let crop_height = resolution.height / 2;
    let mut absolute_error_sum = 0_u64;
    let mut max_channel_error = 0_u8;
    let mut sampled_pixels = 0_u32;
    for row in 0..SAMPLE_ROWS {
        let y = crop_y + ((row * 2 + 1) * crop_height) / (SAMPLE_ROWS * 2);
        for column in 0..SAMPLE_COLUMNS {
            let x = crop_x + ((column * 2 + 1) * crop_width) / (SAMPLE_COLUMNS * 2);
            let offset = (y as usize * resolution.width as usize + x as usize) * 4;
            for channel in 0..3 {
                let error = left[offset + channel].abs_diff(right[offset + channel]);
                absolute_error_sum = absolute_error_sum.saturating_add(u64::from(error));
                max_channel_error = max_channel_error.max(error);
            }
            sampled_pixels = sampled_pixels.saturating_add(1);
        }
    }
    let sampled_channels = sampled_pixels
        .checked_mul(3)
        .context("retime sampled-channel count overflowed")?;
    ensure!(sampled_channels > 0, "retime comparison sampled no pixels");
    Ok(PixelDifferenceEvidence {
        sampled_pixels,
        sampled_channels,
        mean_absolute_error: absolute_error_sum as f64 / f64::from(sampled_channels),
        max_channel_error,
    })
}

fn find_clip(sequence: &Sequence, clip_id: ClipId) -> anyhow::Result<&Clip> {
    sequence
        .video_tracks
        .iter()
        .chain(&sequence.audio_tracks)
        .flat_map(|track| &track.clips)
        .find(|clip| clip.id == clip_id)
        .with_context(|| format!("retime Clip is absent: {clip_id}"))
}

fn find_clip_mut(sequence: &mut Sequence, clip_id: ClipId) -> anyhow::Result<&mut Clip> {
    sequence
        .video_tracks
        .iter_mut()
        .chain(&mut sequence.audio_tracks)
        .flat_map(|track| &mut track.clips)
        .find(|clip| clip.id == clip_id)
        .with_context(|| format!("retime Clip is absent: {clip_id}"))
}
