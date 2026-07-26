//! Fixed-corpus decode, presentation, export, and reimport evidence for one
//! exact constant-retime operation on the Golden Hero Sequence.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, ensure, Context};
use mondrian_assets::AssetKind;
use mondrian_core::timeline_data::AlphaInterpretation;
use mondrian_core::{AssetId, ClipId, FramePosition, Resolution, TimeScale, TimelineTime};
use mondrian_editor_state::Action;
use mondrian_export::preset::TimelineExportRange;
use mondrian_media::{PreviewDecodeSessionContext, VideoCodecProfile};
use mondrian_playback::PreviewResolutionScale;
use mondrian_renderer::{
    evaluate_timeline_render_plan, execute_cpu_output_boundary, CpuColorFrame,
    RenderOutputColorBoundary, RenderOutputColorBoundaryTarget, TimelineCompositeScratch,
    TimelineEvaluationRequest, TimelineRenderPlanElement,
};
use mondrian_timeline::{Clip, Sequence};
use serde::Serialize;

use super::fixture::{sha256_bytes, sha256_file};
use super::generated_delivery::{export_and_probe, ExportEvidence};
use super::harness::{
    dispatch_author_transition, wait_for_media_imports, AuthorTransitionEvidence,
};
use super::headless_preview::{
    GoldenHeadlessPreview, GoldenHeadlessViewerEvidence, GoldenViewerPresentationEvidence,
};
use super::media_execution::{decode_media, source_rgba};
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
const EXPORT_DURATION_FRAMES: i64 = 1;
const VIEWER_PRESENTATION_TIMEOUT: Duration = Duration::from_secs(30);
const EXPECTED_MEAN_ABSOLUTE_ERROR_MAX: f64 = 20.0;
const EXPECTED_MAX_CHANNEL_ERROR_MAX: u8 = 96;
const COUNTERFACTUAL_MEAN_ERROR_MARGIN_MIN: f64 = 3.0;
const SAMPLE_COLUMNS: u32 = 16;
const SAMPLE_ROWS: u32 = 9;

#[derive(Debug, Serialize)]
pub(super) struct GoldenRetimeMediaEvidence {
    author_step: AuthorTransitionEvidence,
    rate: TimeScale,
    sample_frame: i64,
    expected_source_time: TimelineTime,
    preview_source_time: TimelineTime,
    export_source_time: TimelineTime,
    presentation: GoldenViewerPresentationEvidence,
    viewer: GoldenHeadlessViewerEvidence,
    expected_program: ProgramReferenceEvidence,
    counterfactual_program: ProgramReferenceEvidence,
    export: ExportEvidence,
    reimport: RetimeReimportEvidence,
}

impl GoldenRetimeMediaEvidence {
    pub(super) const fn export_id(&self) -> &'static str {
        RETIME_EXPORT_ID
    }
}

#[derive(Debug, Serialize)]
struct ProgramReferenceEvidence {
    source_time: TimelineTime,
    source_rgba_sha256: String,
    program_rgba_sha256: String,
    decode_execution: crate::app::preview_execution::PreviewDecodeExecutionSummary,
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
    counterfactual_difference: PixelDifferenceEvidence,
    expected_mean_absolute_error_max: f64,
    expected_max_channel_error_max: u8,
    counterfactual_mean_error_margin_min: f64,
}

#[derive(Debug, Serialize)]
struct PixelDifferenceEvidence {
    sampled_pixels: u32,
    sampled_channels: u32,
    mean_absolute_error: f64,
    max_channel_error: u8,
}

pub(super) fn execute_retime_media_evidence(
    state: &mut AppState,
    contract: &GoldenProjectContract,
    output_directory: &Path,
    clip_id: ClipId,
    asset_id: AssetId,
    window: GoldenTimelineWindow,
) -> anyhow::Result<GoldenRetimeMediaEvidence> {
    let rate = TimeScale::new(1, 2)?;
    let author_step = dispatch_author_transition(
        state,
        "set-proxy-relink-half-rate",
        Action::SetClipForwardRate { clip_id, rate, include_linked: true },
    )?;
    let sequence = state.active_sequence().cloned().context("Retime Hero Sequence is absent")?;
    let time_base = sequence.time_base();
    let sample_frame = window
        .start_frame
        .checked_add(SAMPLE_FRAME_OFFSET)
        .context("retime sample frame overflowed")?;
    let export_end_frame = sample_frame
        .checked_add(EXPORT_DURATION_FRAMES)
        .context("retime export range overflowed")?;
    ensure!(
        export_end_frame <= window.end_frame_exclusive,
        "retime evidence range escaped the Proxy/Relink Hero window"
    );
    let clip = find_clip(&sequence, clip_id)?;
    ensure!(
        clip.media_asset_id() == Some(asset_id) && clip.source_time_scale() == rate,
        "retime Action did not preserve the target media identity and exact rate"
    );
    let sample_time =
        TimelineTime::from_frame_position(FramePosition::new(sample_frame, time_base))?;
    let expected_source_time = clip.timeline_to_source_time(sample_time)?;
    ensure!(
        expected_source_time
            == TimelineTime::from_frame_position(FramePosition::new(25, time_base))?,
        "50% Golden retime did not map the sample frame to exact source frame 25"
    );
    let preview_source_time = plan_source_time(
        &sequence,
        asset_id,
        TimelineEvaluationRequest::preview(sample_frame, 1.0),
    )?;
    let export_source_time = plan_source_time(
        &sequence,
        asset_id,
        TimelineEvaluationRequest::export(sample_frame),
    )?;
    ensure!(
        preview_source_time == expected_source_time && export_source_time == expected_source_time,
        "Preview and Export plans disagree with the canonical retime source time"
    );

    let output_resolution = contract
        .exports
        .iter()
        .find(|export| export.id == RETIME_EXPORT_ID)
        .map(|export| Resolution {
            width: export.expected_delivery.width,
            height: export.expected_delivery.height,
        })
        .context("Golden H.264 export contract is absent")?;
    let mut decode_context = PreviewDecodeSessionContext::new();
    let expected_program = render_program_reference(
        state,
        &sequence,
        sample_frame,
        output_resolution,
        asset_id,
        expected_source_time,
        &mut decode_context,
    )?;

    let mut counterfactual_sequence = sequence.clone();
    let counterfactual_clip = find_clip_mut(&mut counterfactual_sequence, clip_id)?;
    counterfactual_clip
        .set_constant_source_time_map(counterfactual_clip.source_origin(), TimeScale::ONE)?;
    let counterfactual_source_time = counterfactual_clip.timeline_to_source_time(sample_time)?;
    ensure!(
        counterfactual_source_time != expected_source_time,
        "counterfactual 100% map did not produce a distinct source time"
    );
    let counterfactual_program = render_program_reference(
        state,
        &counterfactual_sequence,
        sample_frame,
        output_resolution,
        asset_id,
        counterfactual_source_time,
        &mut decode_context,
    )?;
    ensure!(
        expected_program.evidence.source_rgba_sha256
            != counterfactual_program.evidence.source_rgba_sha256
            && expected_program.evidence.program_rgba_sha256
                != counterfactual_program.evidence.program_rgba_sha256,
        "Golden H.264 stimulus does not distinguish 50% source time from 100%"
    );
    decode_context.clear();

    state.seek(sample_frame);
    let mut viewer = GoldenHeadlessPreview::new()?;
    let presentation = viewer.present_current(state, VIEWER_PRESENTATION_TIMEOUT)?;
    let viewer_evidence = viewer.evidence();
    ensure!(
        viewer_evidence.presentations == 1 && viewer_evidence.completed_demands == 1,
        "retimed Headless Viewer did not complete exactly one current demand"
    );

    let export_contract = contract
        .exports
        .iter()
        .find(|export| export.id == RETIME_EXPORT_ID)
        .context("Golden H.264 export contract is absent")?;
    let export = export_and_probe(
        state,
        export_contract,
        output_directory.join("retime-h264-aac-sdr.mp4"),
        TimelineExportRange::WorkArea {
            start_frame: sample_frame,
            end_frame_exclusive: export_end_frame,
        },
        sequence.settings.frame_rate,
        EXPORT_DURATION_FRAMES,
        contract.acceptance.duration_error_max_frames,
        contract.acceptance.av_boundary_error_max_ms,
        sequence.settings.audio_sample_rate,
    )?;
    let reimport =
        reimport_and_compare(state, &export, &expected_program, &counterfactual_program)?;

    Ok(GoldenRetimeMediaEvidence {
        author_step,
        rate,
        sample_frame,
        expected_source_time,
        preview_source_time,
        export_source_time,
        presentation,
        viewer: viewer_evidence,
        expected_program: expected_program.evidence,
        counterfactual_program: counterfactual_program.evidence,
        export,
        reimport,
    })
}

fn plan_source_time(
    sequence: &Sequence,
    asset_id: AssetId,
    request: TimelineEvaluationRequest,
) -> anyhow::Result<TimelineTime> {
    let plan = evaluate_timeline_render_plan(sequence, request)?;
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
    Ok(media[0].source_time)
}

fn render_program_reference(
    state: &AppState,
    sequence: &Sequence,
    frame: i64,
    resolution: Resolution,
    asset_id: AssetId,
    expected_source_time: TimelineTime,
    decode_context: &mut PreviewDecodeSessionContext,
) -> anyhow::Result<ProgramReference> {
    let assets = state
        .asset_library()
        .context("Asset Library is absent")?
        .list_assets()?
        .into_iter()
        .map(|asset| (asset.id, asset))
        .collect::<HashMap<_, _>>();
    let mut request_source_time = None;
    let mut source_rgba_sha256 = None;
    let mut decode_execution = None;
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
            .and_then(|asset| decode_media(state, &request, asset, decode_context));
        match outcome {
            Ok(media) => {
                request_source_time = Some(request.source_time);
                decode_execution = Some(media.frame.decode_execution());
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
    let color_context =
        sequence.settings.root_program_color_context(state.project_color_environment());
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
        request_source_time == Some(expected_source_time),
        "real media Adapter received a source time different from the canonical map"
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
        .output_color_space
        .color()
        .context("Golden Program Output is not an encoded color space")?;
    let boundary = RenderOutputColorBoundary::from_intent(
        RenderOutputColorBoundaryTarget::Export,
        program_output_color,
        &resolved.plan.color_context.output_transform,
        resolved.plan.color_context.output_tone_map,
        resolved.plan.color_context.engine.clone(),
    )?;
    let rgba = execute_cpu_output_boundary(&CpuColorFrame::working(flattened), &boundary)?
        .result
        .frame
        .into_rgba();
    Ok(ProgramReference {
        evidence: ProgramReferenceEvidence {
            source_time: expected_source_time,
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

fn reimport_and_compare(
    state: &mut AppState,
    export: &ExportEvidence,
    expected: &ProgramReference,
    counterfactual: &ProgramReference,
) -> anyhow::Result<RetimeReimportEvidence> {
    ensure!(
        expected.resolution == counterfactual.resolution,
        "retime references use different output extents"
    );
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
        .media_info
        .primary_video()
        .context("retime reimport has no video stream")?;
    ensure!(
        asset.kind == AssetKind::Video
            && asset.path == export.output_path()
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
        .root_program_color_context(state.project_color_environment())
        .media_input(false);
    let request = PreviewTimelineMediaRequest {
        asset_id: asset.id,
        color_space_override: None,
        alpha_interpretation: AlphaInterpretation::Ignore,
        source_time: TimelineTime::ZERO,
        target_resolution: expected.resolution,
        input_color,
    };
    let mut decode_context = PreviewDecodeSessionContext::new();
    let decoded = decode_media(state, &request, asset, &mut decode_context)?;
    let decoded_rgba = source_rgba(&decoded.frame)?;
    let expected_difference =
        sampled_difference(&expected.rgba, &decoded_rgba, expected.resolution)?;
    let counterfactual_difference =
        sampled_difference(&counterfactual.rgba, &decoded_rgba, expected.resolution)?;
    ensure!(
        expected_difference.mean_absolute_error <= EXPECTED_MEAN_ABSOLUTE_ERROR_MAX
            && expected_difference.max_channel_error <= EXPECTED_MAX_CHANNEL_ERROR_MAX,
        "retime export differs from the expected 50% Program reference: mean {:.3}, max {}",
        expected_difference.mean_absolute_error,
        expected_difference.max_channel_error
    );
    ensure!(
        counterfactual_difference.mean_absolute_error
            >= expected_difference.mean_absolute_error
                + COUNTERFACTUAL_MEAN_ERROR_MARGIN_MIN,
        "retime export is not materially closer to the 50% reference than the 100% counterfactual: \
         expected mean {:.3}, counterfactual mean {:.3}",
        expected_difference.mean_absolute_error,
        counterfactual_difference.mean_absolute_error
    );
    Ok(RetimeReimportEvidence {
        asset_id: asset.id,
        path_sha256: sha256_file(export.output_path())?,
        decoded_rgba_sha256: sha256_bytes(&decoded_rgba),
        decode_execution: decoded.frame.decode_execution(),
        expected_difference,
        counterfactual_difference,
        expected_mean_absolute_error_max: EXPECTED_MEAN_ABSOLUTE_ERROR_MAX,
        expected_max_channel_error_max: EXPECTED_MAX_CHANNEL_ERROR_MAX,
        counterfactual_mean_error_margin_min: COUNTERFACTUAL_MEAN_ERROR_MARGIN_MIN,
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
