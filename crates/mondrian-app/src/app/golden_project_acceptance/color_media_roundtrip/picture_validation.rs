//! Preview execution and independent source-to-working picture checks.

use super::super::media_execution::{decode_media, rgba8_at, source_rgba, DecodedMedia};
use super::PREVIEW_RESOLUTION;
use crate::app::preview_cpu_execution::{
    composite_resolved_preview, composite_resolved_preview_working,
};
use crate::app::preview_timeline_execution::{
    resolve_preview_timeline, PreviewTimelineMediaFrame, PreviewTimelineMediaRequest,
    PreviewTimelineResolution, PreviewTimelineTitleFrame,
};
use crate::app::preview_unavailability::{PreviewOutputStage, PreviewUnavailability};
use crate::app::preview_viewer_plan::ResolvedPreviewElement;
use crate::app::AppState;
use anyhow::{bail, ensure, Context};
use mondrian_core::{AssetId, WorkingColorSpace};
use mondrian_media::PreviewDecodeSessionContext;
use mondrian_playback::PreviewResolutionScale;
use mondrian_renderer::{
    execute_cpu_output_boundary, RenderOutputColorBoundary, RenderOutputColorBoundaryTarget,
    TimelineCompositeScratch,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[derive(Debug, Serialize)]
pub(super) struct SourcePatchEvidence {
    hlg_max_encoded_code_error: u8,
    hlg_neutral_luminance: Vec<f32>,
    hlg_channel_dominance_proven: bool,
    srgb_max_encoded_code_error: u8,
    srgb_alpha_codes: Vec<u8>,
    srgb_to_linear_rec2020_max_error: f32,
    transparent_rgb_ignored: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct PreviewEvidence {
    frame: i64,
    width: u32,
    height: u32,
    elements: usize,
    media_decode_layers: u32,
    software_cpu_layers: u32,
    float_linear_composites: u64,
    legacy_rgba8_composites: u64,
    rgba_sha256: String,
    program_output_rgba_sha256: String,
}

pub(super) struct PictureStageResult {
    pub(super) source_patches: SourcePatchEvidence,
    pub(super) preview: PreviewEvidence,
    pub(super) program_output_rgba: Vec<u8>,
}

struct ColorMediaPreviewExecution {
    decoded: HashMap<AssetId, DecodedMedia>,
    viewer_output: crate::app::preview_cpu_execution::PreviewCompositeOutput,
    base_only_rgba: Vec<u8>,
    program_output_rgba: Vec<u8>,
}

fn execute_preview(
    state: &AppState,
    frame: i64,
    decode_context: &mut PreviewDecodeSessionContext,
) -> anyhow::Result<ColorMediaPreviewExecution> {
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let assets = state
        .asset_library()
        .context("Asset Library is absent")?
        .list_assets()?
        .into_iter()
        .map(|asset| (asset.id, asset))
        .collect::<HashMap<_, _>>();
    let mut decoded = HashMap::new();
    let mut adapter_failure = None;
    let mut media_frame = |request: PreviewTimelineMediaRequest| {
        let outcome = assets
            .get(&request.asset_id)
            .with_context(|| format!("Preview asset is absent: {}", request.asset_id))
            .and_then(|asset| decode_media(state, &request, asset, decode_context));
        match outcome {
            Ok(media) => {
                let frame = media.frame.clone();
                decoded.insert(request.asset_id, media);
                PreviewTimelineMediaFrame::Ready(frame)
            }
            Err(error) => {
                adapter_failure = Some(error.to_string());
                PreviewTimelineMediaFrame::Unavailable {
                    reason: PreviewUnavailability::blocked(
                        PreviewOutputStage::MediaResolution,
                        "color-media decode Adapter failed",
                    ),
                }
            }
        }
    };
    let mut title_frame = |_request| PreviewTimelineTitleFrame::Unavailable {
        reason: PreviewUnavailability::blocked(
            PreviewOutputStage::GeneratedSource,
            "color-media slice contains no generated titles",
        ),
    };
    let color_context =
        sequence.settings.root_program_color_context(state.project_color_environment());
    let resolved = match resolve_preview_timeline(
        sequence,
        state.sequences(),
        frame,
        PREVIEW_RESOLUTION,
        PreviewResolutionScale::Full,
        color_context,
        &mut media_frame,
        &mut title_frame,
    ) {
        PreviewTimelineResolution::Ready(resolved) => resolved,
        PreviewTimelineResolution::Empty => bail!("color-media Preview resolved as empty"),
        PreviewTimelineResolution::Pending { .. } => {
            bail!("color-media Preview retained a pending dependency")
        }
        PreviewTimelineResolution::Unavailable { reason } => {
            bail!(
                "color-media Preview unavailable: {reason:?}; Adapter: {}",
                adapter_failure.as_deref().unwrap_or("no Adapter diagnostic")
            )
        }
    };
    ensure!(
        resolved.plan.elements.len() == 2
            && resolved
                .plan
                .elements
                .iter()
                .all(|element| matches!(element, ResolvedPreviewElement::Media { .. })),
        "color-media Preview did not resolve exactly two media layers"
    );
    let mut scratch = TimelineCompositeScratch::default();
    let output = composite_resolved_preview(
        PREVIEW_RESOLUTION.width,
        PREVIEW_RESOLUTION.height,
        &resolved.plan.elements,
        &resolved.plan.color_context,
        &mut scratch,
    )?;
    let mut base_scratch = TimelineCompositeScratch::default();
    let base_only = composite_resolved_preview(
        PREVIEW_RESOLUTION.width,
        PREVIEW_RESOLUTION.height,
        &resolved.plan.elements[..1],
        &resolved.plan.color_context,
        &mut base_scratch,
    )?
    .rgba;
    let mut program_scratch = TimelineCompositeScratch::default();
    let program_working = composite_resolved_preview_working(
        PREVIEW_RESOLUTION.width,
        PREVIEW_RESOLUTION.height,
        &resolved.plan.elements,
        &resolved.plan.color_context,
        &mut program_scratch,
    )?;
    let program_output_color = resolved
        .plan
        .color_context
        .output_color_space
        .color()
        .context("Golden Program Output is not an encoded color space")?;
    let program_boundary = RenderOutputColorBoundary::from_intent(
        RenderOutputColorBoundaryTarget::Export,
        program_output_color,
        &resolved.plan.color_context.output_transform,
        resolved.plan.color_context.output_tone_map,
        resolved.plan.color_context.engine.clone(),
    )?;
    let program_output = execute_cpu_output_boundary(&program_working.frame, &program_boundary)?
        .result
        .frame
        .into_rgba();
    Ok(ColorMediaPreviewExecution {
        decoded,
        viewer_output: output,
        base_only_rgba: base_only,
        program_output_rgba: program_output,
    })
}

fn rgba_f32_at(
    frame: &mondrian_core::WorkingRgbaF32Frame,
    x: u32,
    y: u32,
) -> anyhow::Result<[f32; 4]> {
    ensure!(
        x < frame.width && y < frame.height,
        "working-space sample ({x}, {y}) is outside {}x{}",
        frame.width,
        frame.height
    );
    frame
        .data
        .get(y as usize * frame.width as usize + x as usize)
        .copied()
        .context("working-space sample storage is truncated")
}

fn max_code_error(actual: [u8; 4], expected: [u8; 4]) -> u8 {
    actual
        .into_iter()
        .zip(expected)
        .map(|(actual, expected)| actual.abs_diff(expected))
        .max()
        .unwrap_or(0)
}

fn srgb_to_linear(value: u8) -> f32 {
    let encoded = f32::from(value) / 255.0;
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb_to_linear_rec2020(rgb: [u8; 3]) -> [f32; 3] {
    let linear = [
        srgb_to_linear(rgb[0]),
        srgb_to_linear(rgb[1]),
        srgb_to_linear(rgb[2]),
    ];
    [
        0.627_403_9 * linear[0] + 0.329_283 * linear[1] + 0.043_313_1 * linear[2],
        0.069_097_3 * linear[0] + 0.919_540_4 * linear[1] + 0.011_362_3 * linear[2],
        0.016_391_4 * linear[0] + 0.088_013_3 * linear[1] + 0.895_595_3 * linear[2],
    ]
}

fn validate_source_patches(
    hlg: &DecodedMedia,
    alpha: &DecodedMedia,
    preview_rgba: &[u8],
    base_only_rgba: &[u8],
) -> anyhow::Result<SourcePatchEvidence> {
    let hlg_source = source_rgba(&hlg.frame)?;
    let alpha_source = source_rgba(&alpha.frame)?;
    let hlg_expected_10 = [
        [64, 64, 64],
        [256, 256, 256],
        [512, 512, 512],
        [768, 768, 768],
        [800, 200, 100],
        [200, 800, 100],
        [100, 200, 800],
        [940, 940, 940],
    ];
    let mut hlg_max_encoded_code_error = 0;
    for (index, expected) in hlg_expected_10.into_iter().enumerate() {
        let actual = rgba8_at(&hlg_source, 1920, 120 + index as u32 * 240, 540)?;
        let expected = [
            ((expected[0] * 255 + 511) / 1023) as u8,
            ((expected[1] * 255 + 511) / 1023) as u8,
            ((expected[2] * 255 + 511) / 1023) as u8,
            255,
        ];
        hlg_max_encoded_code_error =
            hlg_max_encoded_code_error.max(max_code_error(actual, expected));
    }
    ensure!(
        hlg_max_encoded_code_error <= 4,
        "HLG decoded patch error exceeded four 8-bit codes: {hlg_max_encoded_code_error}"
    );

    let hlg_working = hlg.frame.working_frame()?.frame;
    ensure!(
        hlg_working.descriptor().color_space == WorkingColorSpace::LinearRec2020.into(),
        "HLG input did not enter the Sequence working space"
    );
    let hlg_working = hlg_working.rgba_f32();
    let neutral_luminance = (0..4)
        .chain(std::iter::once(7))
        .map(|index| {
            let value = rgba_f32_at(hlg_working, 120 + index * 240, 540)?;
            ensure!(
                value[..3].iter().all(|channel| channel.is_finite())
                    && (value[0] - value[1]).abs() < 0.01
                    && (value[1] - value[2]).abs() < 0.01,
                "HLG neutral patch is not finite and neutral in working space"
            );
            Ok((value[0] + value[1] + value[2]) / 3.0)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    ensure!(
        neutral_luminance.windows(2).all(|pair| pair[1] > pair[0]),
        "HLG neutral patches are not strictly monotonic in working space"
    );
    let red = rgba_f32_at(hlg_working, 120 + 4 * 240, 540)?;
    let green = rgba_f32_at(hlg_working, 120 + 5 * 240, 540)?;
    let blue = rgba_f32_at(hlg_working, 120 + 6 * 240, 540)?;
    ensure!(
        red[0] > red[1]
            && red[0] > red[2]
            && green[1] > green[0]
            && green[1] > green[2]
            && blue[2] > blue[0]
            && blue[2] > blue[1],
        "HLG chromatic patches lost channel dominance in working space"
    );

    let alpha_expected = [
        ([255, 0, 255, 0], 240, 270),
        ([255, 0, 0, 64], 240, 810),
        ([0, 255, 0, 128], 720, 810),
        ([0, 0, 255, 192], 1200, 810),
        ([255, 255, 255, 255], 1680, 810),
    ];
    let mut srgb_max_encoded_code_error = 0;
    let mut alpha_codes = Vec::new();
    for (expected, x, y) in alpha_expected {
        let actual = rgba8_at(&alpha_source, 1920, x, y)?;
        srgb_max_encoded_code_error =
            srgb_max_encoded_code_error.max(max_code_error(actual, expected));
        alpha_codes.push(actual[3]);
    }
    ensure!(
        srgb_max_encoded_code_error == 0,
        "sRGB Alpha decoded patches are not byte-exact"
    );

    let alpha_working = alpha.frame.working_frame()?.frame;
    ensure!(
        alpha_working.descriptor().color_space == WorkingColorSpace::LinearRec2020.into(),
        "sRGB Alpha input did not enter the Sequence working space"
    );
    let alpha_working = alpha_working.rgba_f32();
    let mut srgb_to_linear_rec2020_max_error = 0.0_f32;
    for (rgb, alpha_code, x) in [
        ([255, 0, 0], 64_u8, 240),
        ([0, 255, 0], 128_u8, 720),
        ([0, 0, 255], 192_u8, 1200),
        ([255, 255, 255], 255_u8, 1680),
    ] {
        let actual = rgba_f32_at(alpha_working, x, 810)?;
        let expected = srgb_to_linear_rec2020(rgb);
        for channel in 0..3 {
            srgb_to_linear_rec2020_max_error =
                srgb_to_linear_rec2020_max_error.max((actual[channel] - expected[channel]).abs());
        }
        srgb_to_linear_rec2020_max_error =
            srgb_to_linear_rec2020_max_error.max((actual[3] - f32::from(alpha_code) / 255.0).abs());
    }
    ensure!(
        srgb_to_linear_rec2020_max_error <= 0.003,
        "independent sRGB-to-linear-Rec.2020 oracle error is {srgb_to_linear_rec2020_max_error}"
    );

    let transparent_sample = rgba8_at(preview_rgba, PREVIEW_RESOLUTION.width, 240, 270)?;
    let base_sample = rgba8_at(base_only_rgba, PREVIEW_RESOLUTION.width, 240, 270)?;
    ensure!(
        transparent_sample == base_sample,
        "RGB behind zero Alpha changed the composited result"
    );
    ensure!(
        rgba8_at(preview_rgba, PREVIEW_RESOLUTION.width, 240, 810)?
            != rgba8_at(base_only_rgba, PREVIEW_RESOLUTION.width, 240, 810)?,
        "non-zero straight Alpha did not affect the composited result"
    );

    Ok(SourcePatchEvidence {
        hlg_max_encoded_code_error,
        hlg_neutral_luminance: neutral_luminance,
        hlg_channel_dominance_proven: true,
        srgb_max_encoded_code_error,
        srgb_alpha_codes: alpha_codes,
        srgb_to_linear_rec2020_max_error,
        transparent_rgb_ignored: true,
    })
}

fn sha256_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn execute_picture_stage(
    state: &AppState,
    hlg_asset_id: AssetId,
    alpha_asset_id: AssetId,
    evaluation_frame: i64,
    decode_context: &mut PreviewDecodeSessionContext,
) -> anyhow::Result<PictureStageResult> {
    let execution = execute_preview(state, evaluation_frame, decode_context)?;
    let hlg = execution
        .decoded
        .get(&hlg_asset_id)
        .context("Preview did not decode HLG fixture")?;
    let alpha = execution
        .decoded
        .get(&alpha_asset_id)
        .context("Preview did not decode sRGB Alpha fixture")?;
    let source_patches = validate_source_patches(
        hlg,
        alpha,
        &execution.viewer_output.rgba,
        &execution.base_only_rgba,
    )?;
    let decode_summary = execution.decoded.values().fold(
        crate::app::preview_execution::PreviewDecodeExecutionSummary::default(),
        |mut summary, media| {
            summary.accumulate(media.frame.decode_execution());
            summary
        },
    );
    let preview = PreviewEvidence {
        frame: evaluation_frame,
        width: PREVIEW_RESOLUTION.width,
        height: PREVIEW_RESOLUTION.height,
        elements: 2,
        media_decode_layers: decode_summary.media_layers,
        software_cpu_layers: decode_summary.software_cpu_layers,
        float_linear_composites: execution
            .viewer_output
            .composite_diagnostics
            .float_linear_composites,
        legacy_rgba8_composites: execution
            .viewer_output
            .composite_diagnostics
            .legacy_rgba8_composites,
        rgba_sha256: sha256_bytes(&execution.viewer_output.rgba),
        program_output_rgba_sha256: sha256_bytes(&execution.program_output_rgba),
    };
    ensure!(
        preview.media_decode_layers == 2
            && preview.float_linear_composites == 1
            && preview.legacy_rgba8_composites == 0,
        "color-media Preview left the production float-linear path"
    );
    Ok(PictureStageResult {
        source_patches,
        preview,
        program_output_rgba: execution.program_output_rgba,
    })
}
