//! Shared production media resolution and decode helpers for color evidence.

use crate::app::preview_access_mode::{MediaPreviewJob, MediaPreviewRequestPriority};
use crate::app::preview_hardware_admission::PreviewHardwareDecodeAdmissionState;
use crate::app::preview_media_source::{
    resolve_preview_media_source, PreviewMediaSourceOutcome, PreviewMediaSourceRequest,
};
use crate::app::preview_media_task::decode_media_preview_with_context;
use crate::app::preview_timeline_execution::PreviewTimelineMediaRequest;
use crate::app::AppState;
use anyhow::{bail, ensure, Context};
use mondrian_assets::AssetRecord;
use mondrian_media::{
    PreviewDecodeAccessMode, PreviewDecodeAdaptiveHints, PreviewDecodeSessionContext,
    PreviewHardwareDecodeRequest,
};
use mondrian_renderer::CpuSourceColorFrame;
use mondrian_timeline::sequence::InputColorResolutionSource;
use std::time::Instant;

#[derive(Clone)]
pub(super) struct DecodedMedia {
    pub(super) frame: crate::app::preview_media_frame::MediaPreviewFrame,
    pub(super) input_color_resolution: InputColorResolutionSource,
}

pub(super) fn decode_media(
    state: &AppState,
    request: &PreviewTimelineMediaRequest,
    asset: &AssetRecord,
    decode_context: &mut PreviewDecodeSessionContext,
) -> anyhow::Result<DecodedMedia> {
    let proxy_config = state.proxy_config();
    let resolved = match resolve_preview_media_source(PreviewMediaSourceRequest {
        asset,
        color_space_override: request.color_space_override,
        alpha_interpretation: request.alpha_interpretation,
        source_time: request.source_time,
        target_resolution: request.target_resolution,
        input_color: &request.input_color,
        prefer_proxy: false,
        request_missing_proxy_generation: false,
        proxy_config: &proxy_config,
        proxy_color: None,
        hardware_admission: PreviewHardwareDecodeAdmissionState::default(),
    }) {
        PreviewMediaSourceOutcome::Ready(resolved) => resolved,
        PreviewMediaSourceOutcome::ColorRejected(rejected) => {
            bail!("Preview rejected media color: {:?}", rejected.diagnostic)
        }
        PreviewMediaSourceOutcome::Unavailable(unavailable) => {
            bail!("Preview media unavailable: {}", unavailable.reason)
        }
    };
    ensure!(
        resolved.input_color_resolution.source == InputColorResolutionSource::DetectedMetadata,
        "Preview did not use explicit detected color metadata"
    );
    let result = decode_media_preview_with_context(
        MediaPreviewJob {
            key: resolved.key,
            generation: 1,
            priority: MediaPreviewRequestPriority::Current,
            access_mode: PreviewDecodeAccessMode::RandomAccessStillFrame,
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_device_selector: None,
            enqueued_at: Instant::now(),
            deadline_at: None,
            demand_identity: None,
            execution_id: None,
        },
        0,
        decode_context,
        || false,
    );
    ensure!(!result.canceled, "Preview media decode was canceled");
    ensure!(
        result.error.is_none() && result.failure_reason.is_none(),
        "Preview media decode failed: {:?} / {:?}",
        result.error,
        result.failure_reason
    );
    let frame = result.frame.context("Preview media decode produced no frame")?;
    Ok(DecodedMedia {
        frame,
        input_color_resolution: resolved.input_color_resolution.source,
    })
}

pub(super) fn source_rgba(
    frame: &crate::app::preview_media_frame::MediaPreviewFrame,
) -> anyhow::Result<Vec<u8>> {
    let source = frame.gpu_source().context("decoded media did not retain a CPU source frame")?;
    match source.source.as_ref() {
        CpuSourceColorFrame::EncodedRgba8(frame) => Ok(frame.rgba().to_vec()),
        CpuSourceColorFrame::LinearFloat(_) => bail!("color fixture unexpectedly decoded as float"),
    }
}

pub(super) fn rgba8_at(rgba: &[u8], width: u32, x: u32, y: u32) -> anyhow::Result<[u8; 4]> {
    ensure!(
        width > 0 && x < width,
        "RGBA sample x={x} is outside width={width}"
    );
    let index = (y as usize)
        .checked_mul(width as usize)
        .and_then(|row| row.checked_add(x as usize))
        .and_then(|pixel| pixel.checked_mul(4))
        .context("RGBA sample offset overflowed")?;
    let pixel = rgba
        .get(index..index.saturating_add(4))
        .filter(|pixel| pixel.len() == 4)
        .with_context(|| format!("RGBA sample ({x}, {y}) is outside the decoded frame"))?;
    Ok([pixel[0], pixel[1], pixel[2], pixel[3]])
}
