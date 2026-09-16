//! Production export, strict probe, reimport, and sampled roundtrip evidence.

use super::super::media_execution::{decode_media, rgba8_at, source_rgba};
use super::{EXPORT_TIMEOUT, PREVIEW_RESOLUTION};
use crate::app::golden_project_acceptance::builtin_preset;
use crate::app::golden_project_acceptance::fixture::sha256_file;
use crate::app::golden_project_acceptance::harness::{
    execute_export_job, wait_for_media_imports, CompletedExportEvidence,
};
use crate::app::preview_timeline_execution::PreviewTimelineMediaRequest;
use crate::app::AppState;
use anyhow::{ensure, Context};
use mondrian_assets::AssetRecord;
use mondrian_core::timeline_data::AlphaInterpretation;
use mondrian_core::{AssetId, TimelineTime};
use mondrian_editor_state::Action;
use mondrian_export::preset::TimelineExportRange;
use mondrian_export::validator::{probe_export_output, ExportOutputProbe};
use mondrian_media::PreviewDecodeSessionContext;
use mondrian_timeline::sequence::InputColorResolutionSource;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, Serialize)]
pub(super) struct ExportRoundtripEvidence {
    execution: CompletedExportEvidence,
    output_sha256: String,
    probe: ExportOutputProbe,
    reimported_asset_id: AssetId,
    input_color_resolution: InputColorResolutionSource,
    sampled_pixels: usize,
    max_channel_error: u8,
}

fn reimport_export(state: &mut AppState, output_path: &Path) -> anyhow::Result<AssetRecord> {
    let before = state
        .asset_library()
        .context("Asset Library is absent")?
        .list_assets()?
        .into_iter()
        .map(|asset| asset.id)
        .collect::<BTreeSet<_>>();
    state.dispatch_action(Action::ImportMedia(vec![output_path.to_path_buf()]))?;
    wait_for_media_imports(state)?;
    let imported = state
        .asset_library()
        .context("Asset Library is absent after export reimport")?
        .list_assets()?
        .into_iter()
        .filter(|asset| !before.contains(&asset.id))
        .collect::<Vec<_>>();
    ensure!(
        imported.len() == 1,
        "export reimport created {} assets",
        imported.len()
    );
    imported.into_iter().next().context("export reimport count was proven nonzero")
}

pub(super) fn execute_export_roundtrip(
    state: &mut AppState,
    output_directory: &Path,
    program_output_rgba: &[u8],
    start_frame: i64,
    decode_context: &mut PreviewDecodeSessionContext,
) -> anyhow::Result<ExportRoundtripEvidence> {
    let preset = builtin_preset("h264-aac-sdr")?.preset();
    let end_frame_exclusive =
        start_frame.checked_add(1).context("Color Media export frame overflowed")?;
    let execution = execute_export_job(
        state,
        preset,
        state.active_sequence().map(|sequence| sequence.id),
        TimelineExportRange::WorkArea { start_frame, end_frame_exclusive },
        output_directory.join("color-media-roundtrip-h264.mp4"),
        EXPORT_TIMEOUT,
    )?;
    let output_sha256 = sha256_file(&execution.output_path)?;
    let probe = probe_export_output(&execution.output_path)?;
    let video = probe.video.as_ref().context("color-media export has no video stream")?;
    ensure!(
        video.codec_name.as_deref() == Some("h264")
            && video.width == Some(PREVIEW_RESOLUTION.width)
            && video.height == Some(PREVIEW_RESOLUTION.height)
            && video.pixel_format.as_deref() == Some("yuv420p")
            && video.color_primaries.as_deref() == Some("bt709")
            && video.color_transfer.as_deref() == Some("bt709")
            && video.color_matrix.as_deref() == Some("bt709"),
        "color-media export does not satisfy the H.264 Rec.709 roundtrip contract"
    );
    let asset = reimport_export(state, &execution.output_path)?;
    let input_color = state
        .active_sequence()
        .context("active Sequence is absent")?
        .settings
        .root_program_color_context(state.project_color_environment())?
        .media_input(false);
    let request = PreviewTimelineMediaRequest {
        asset_id: asset.id,
        color_space_override: None,
        alpha_interpretation: AlphaInterpretation::Straight,
        picture_overrides: Default::default(),
        source_sample: mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
        target_resolution: PREVIEW_RESOLUTION,
        input_color,
        cpu_working_required: false,
    };
    let decoded = decode_media(state, &request, &asset, decode_context)?;
    let export_rgba = source_rgba(&decoded.frame)?;
    let samples = [(240, 270), (240, 810), (720, 810), (1200, 810), (1680, 810)];
    let mut max_channel_error = 0;
    for (x, y) in samples {
        let preview = rgba8_at(program_output_rgba, PREVIEW_RESOLUTION.width, x, y)?;
        let export = rgba8_at(&export_rgba, PREVIEW_RESOLUTION.width, x, y)?;
        for channel in 0..3 {
            max_channel_error = max_channel_error.max(preview[channel].abs_diff(export[channel]));
        }
    }
    ensure!(
        max_channel_error <= 16,
        "Preview/export sampled-channel disagreement is {max_channel_error} codes"
    );
    Ok(ExportRoundtripEvidence {
        execution,
        output_sha256,
        probe,
        reimported_asset_id: asset.id,
        input_color_resolution: decoded.input_color_resolution,
        sampled_pixels: samples.len(),
        max_channel_error,
    })
}
