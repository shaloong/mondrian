//! Deterministic still decode and thumbnail color execution.

use std::fmt::Write as _;
use std::sync::mpsc;
use std::time::Instant;

use mondrian_assets::AssetRecord;
use mondrian_core::types::{ColorEngine, ColorSpace};
use mondrian_core::{OutputTransformIntent, WorkingColorSpace};
use mondrian_media::{
    DecodedRgbaEncoding, DecodedVideoRangeContract, PreviewDecodeAccessMode, PreviewDecodeOutcome,
    PreviewDecodeRequest, PreviewDecodeSessionContext, PreviewSourceColorContract,
};
use mondrian_renderer::{
    execute_cpu_input_stage_with_session, execute_cpu_output_boundary_rgba8_with_session,
    execute_cpu_source_input_stage_with_session, CpuEncodedColorFrame, CpuEncodedFloatColorFrame,
    CpuSourceColorFrame, LinearFloatSource, RenderCpuColorExecutionSession, RenderInputTransform,
    RenderOutputColorBoundary,
};
use mondrian_timeline::sequence::{ProgramColorContext, ResolvedInputColor};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::app::preview_access_mode::{
    media_preview_access_mode_for_intent, MediaPreviewAccessIntent,
};
use crate::app::single_worker_activity::SingleWorkerActivity;

use super::state::{ThumbnailJob, ThumbnailResult, ThumbnailWorkerIdentity};
use super::{
    ThumbnailDispatchGate, ThumbnailFailure, ThumbnailFailureReason, ThumbnailRasterColorSpace,
    ThumbnailRasterFrame,
};

const THUMBNAIL_MAX_WIDTH: u32 = 320;
const THUMBNAIL_MAX_HEIGHT: u32 = 180;

pub(super) fn thumbnail_worker(
    jobs: mpsc::Receiver<ThumbnailJob>,
    results: mpsc::SyncSender<ThumbnailResult>,
    dispatch_gate: std::sync::Arc<ThumbnailDispatchGate>,
    activity: std::sync::Arc<SingleWorkerActivity<ThumbnailWorkerIdentity>>,
) {
    let mut decode_context = PreviewDecodeSessionContext::new();
    let mut color_session = RenderCpuColorExecutionSession::default();
    for job in jobs {
        let identity = job.worker_identity();
        let mut activity_lease = activity.begin(identity);
        let started = Instant::now();
        let result = if dispatch_gate.wait_until_enabled(&job.cancellation) {
            activity_lease.mark_running();
            decode_thumbnail(&job, &mut decode_context, &mut color_session)
        } else {
            Err(ThumbnailFailure::new(
                ThumbnailFailureReason::DecodeCanceled,
                "thumbnail dispatch was canceled before execution",
            ))
        };
        let result = ThumbnailResult {
            key: job.key,
            generation: job.generation,
            result,
            elapsed: started.elapsed(),
        };
        activity_lease.finish_for_publication();
        match results.send(result) {
            Ok(()) => activity_lease.commit_publication(),
            Err(_) => break,
        }
    }
    decode_context.clear();
    color_session.clear();
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub(super) struct ThumbnailColorContract {
    pub(super) video_stream_index: u32,
    pub(super) source_color_space: ColorSpace,
    pub(super) source_range: DecodedVideoRangeContract,
    pub(super) working_color_space: WorkingColorSpace,
    pub(super) output_color_space: ColorSpace,
    pub(super) tone_map: bool,
    pub(super) engine: ColorEngine,
    pub(super) output_transform: OutputTransformIntent,
}

impl ThumbnailColorContract {
    pub(super) fn resolve(
        asset: &AssetRecord,
        context: &ProgramColorContext,
    ) -> Result<Self, ThumbnailFailure> {
        let primary_video =
            asset.media_probe().and_then(|probe| probe.primary_video()).ok_or_else(|| {
                failure(
                    ThumbnailFailureReason::MissingVideoStreamContract,
                    "video asset has no probed primary video stream",
                )
            })?;
        let source_color_space = match context
            .missing_metadata_policy
            .resolve_asset_input_decision(
                None,
                asset.interpretation,
                primary_video.executable_color_space(),
                context.working_color_space,
            )
            .resolved
        {
            ResolvedInputColor::Color(color_space) => color_space,
            ResolvedInputColor::Data => {
                return Err(failure(
                    ThumbnailFailureReason::NonColorDataUnsupported,
                    "thumbnail cannot interpret non-color YUV data",
                ));
            }
            ResolvedInputColor::Rejected => {
                return Err(failure(
                    ThumbnailFailureReason::InputColorRejected,
                    "input color resolution rejected media metadata",
                ));
            }
        };
        let source_range = DecodedVideoRangeContract::from_interpretation(
            asset.interpretation.range,
            primary_video.color_range,
        );
        let output_color_space = context.output_color_space.color().ok_or_else(|| {
            failure(
                ThumbnailFailureReason::InternalOutputIdentity,
                "thumbnail presentation requires an encoded output identity",
            )
        })?;
        if output_color_space != ColorSpace::Srgb {
            return Err(failure(
                ThumbnailFailureReason::UnsupportedRasterOutput,
                format!("thumbnail raster contract does not support {output_color_space:?}"),
            ));
        }
        Ok(Self {
            video_stream_index: primary_video.index,
            source_color_space,
            source_range,
            working_color_space: context.working_color_space,
            output_color_space,
            tone_map: context.output_tone_map,
            engine: context.engine.clone(),
            output_transform: context.output_transform.clone(),
        })
    }

    pub(super) fn output_boundary(&self) -> Result<RenderOutputColorBoundary, ThumbnailFailure> {
        RenderOutputColorBoundary::from_intent(
            mondrian_renderer::RenderOutputColorBoundaryTarget::Display,
            self.output_color_space,
            &self.output_transform,
            self.tone_map,
            self.engine.clone(),
        )
        .map_err(|error| {
            failure(
                ThumbnailFailureReason::OutputTransformFailed,
                format!("thumbnail output intent resolution failed: {error}"),
            )
        })
    }
}

pub(super) fn decode_thumbnail(
    job: &ThumbnailJob,
    decode_context: &mut PreviewDecodeSessionContext,
    color_session: &mut RenderCpuColorExecutionSession,
) -> Result<ThumbnailRasterFrame, ThumbnailFailure> {
    debug_assert_eq!(
        media_preview_access_mode_for_intent(MediaPreviewAccessIntent::DeterministicStill),
        PreviewDecodeAccessMode::RandomAccessStillFrame
    );
    let request = PreviewDecodeRequest::new(
        job.key.path.as_path(),
        mondrian_core::SourceSampleTarget::covering(mondrian_core::TimelineTime::ZERO),
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        PreviewSourceColorContract::new(
            job.key.color.source_color_space,
            job.key.color.source_range,
        ),
    )
    .with_video_stream_index(job.key.color.video_stream_index)
    .with_max_size(Some(THUMBNAIL_MAX_WIDTH), Some(THUMBNAIL_MAX_HEIGHT))
    .with_fingerprint(job.key.fingerprint);
    let cancellation = job.cancellation.clone();
    let (width, height, rgba) =
        match decode_context.decode_cancellable(request, move || cancellation.is_canceled()) {
            Ok(PreviewDecodeOutcome::Frame(frame)) => {
                let width = frame.width;
                let height = frame.height;
                let rgba = color_manage_rgba_with_session(
                    width,
                    height,
                    frame.into_data(),
                    &job.key.color,
                    color_session,
                )?;
                (width, height, rgba)
            }
            Ok(PreviewDecodeOutcome::FloatFrame(frame)) => {
                let width = frame.width;
                let height = frame.height;
                let encoding = frame.color_contract.encoding;
                let rgba = color_manage_float_with_session(
                    width,
                    height,
                    frame.into_data(),
                    encoding,
                    &job.key.color,
                    color_session,
                )?;
                (width, height, rgba)
            }
            Ok(PreviewDecodeOutcome::Canceled(_)) => {
                return Err(failure(
                    ThumbnailFailureReason::DecodeCanceled,
                    "thumbnail still-frame decode was canceled",
                ));
            }
            Ok(PreviewDecodeOutcome::NativeGpuFrame(frame)) => {
                return Err(failure(
                    ThumbnailFailureReason::UnexpectedGpuFrame,
                    format!(
                        "thumbnail requires CPU RGBA, got native GPU {} {:?}",
                        frame.handle_kind().as_str(),
                        frame.surface_format
                    ),
                ));
            }
            Ok(PreviewDecodeOutcome::CpuYuvFrame(frame)) => {
                return Err(failure(
                    ThumbnailFailureReason::UnexpectedGpuFrame,
                    format!(
                        "thumbnail requires CPU RGBA, got compact CPU YUV {:?} {:?}",
                        frame.chroma_subsampling, frame.sample_format
                    ),
                ));
            }
            Err(error) => {
                return Err(failure(
                    ThumbnailFailureReason::DecodeFailed,
                    error.to_string(),
                ));
            }
        };
    ThumbnailRasterFrame::new(
        thumbnail_key(job, width, height)?,
        width,
        height,
        ThumbnailRasterColorSpace::Srgb,
        rgba,
    )
    .ok_or_else(|| {
        failure(
            ThumbnailFailureReason::InvalidRasterPayload,
            "thumbnail raster payload is invalid",
        )
    })
}

fn color_manage_rgba_with_session(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    color: &ThumbnailColorContract,
    color_session: &mut RenderCpuColorExecutionSession,
) -> Result<Vec<u8>, ThumbnailFailure> {
    let source = CpuEncodedColorFrame::source_rgba8(width, height, color.source_color_space, rgba);
    let input =
        RenderInputTransform::to_working(color.working_color_space, false, color.engine.clone());
    let working =
        execute_cpu_input_stage_with_session(&source, &input, color_session).map_err(|error| {
            failure(
                ThumbnailFailureReason::InputTransformFailed,
                format!("thumbnail input transform failed: {error}"),
            )
        })?;
    execute_cpu_output_boundary_rgba8_with_session(
        &working.result.frame,
        &color.output_boundary()?,
        color_session,
    )
    .map(|output| output.rgba)
    .map_err(|error| {
        failure(
            ThumbnailFailureReason::OutputTransformFailed,
            format!("thumbnail display transform failed: {error}"),
        )
    })
}

fn color_manage_float_with_session(
    width: u32,
    height: u32,
    rgba: Vec<f32>,
    encoding: DecodedRgbaEncoding,
    color: &ThumbnailColorContract,
    color_session: &mut RenderCpuColorExecutionSession,
) -> Result<Vec<u8>, ThumbnailFailure> {
    let source =
        match encoding {
            DecodedRgbaEncoding::SourceEncodedRgb => {
                CpuSourceColorFrame::from(CpuEncodedFloatColorFrame::source_flat_rgba_f32(
                    width,
                    height,
                    color.source_color_space,
                    rgba,
                ))
            }
            DecodedRgbaEncoding::SourceLinearRgb => CpuSourceColorFrame::from(
                LinearFloatSource::new(width, height, color.source_color_space, rgba),
            ),
        };
    let input =
        RenderInputTransform::to_working(color.working_color_space, false, color.engine.clone());
    let working = execute_cpu_source_input_stage_with_session(&source, &input, color_session)
        .map_err(|error| {
            failure(
                ThumbnailFailureReason::InputTransformFailed,
                format!("thumbnail float input transform failed: {error}"),
            )
        })?;
    execute_cpu_output_boundary_rgba8_with_session(
        &working.result.frame,
        &color.output_boundary()?,
        color_session,
    )
    .map(|output| output.rgba)
    .map_err(|error| {
        failure(
            ThumbnailFailureReason::OutputTransformFailed,
            format!("thumbnail float display transform failed: {error}"),
        )
    })
}

#[cfg(test)]
pub(super) fn color_manage_rgba(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    color: &ThumbnailColorContract,
) -> Result<Vec<u8>, ThumbnailFailure> {
    color_manage_rgba_with_session(
        width,
        height,
        rgba,
        color,
        &mut RenderCpuColorExecutionSession::new(0),
    )
}

pub(super) fn thumbnail_key(
    job: &ThumbnailJob,
    width: u32,
    height: u32,
) -> Result<String, ThumbnailFailure> {
    #[derive(Serialize)]
    struct ThumbnailRasterIdentity<'a> {
        schema_version: u8,
        asset_id: mondrian_core::AssetId,
        source_revision: &'a mondrian_core::MediaFileFingerprint,
        color: &'a ThumbnailColorContract,
        width: u32,
        height: u32,
    }

    let identity = ThumbnailRasterIdentity {
        schema_version: 1,
        asset_id: job.key.asset_id,
        source_revision: &job.key.fingerprint,
        color: &job.key.color,
        width,
        height,
    };
    let canonical = serde_json::to_vec(&identity).map_err(|error| {
        failure(
            ThumbnailFailureReason::OutputTransformFailed,
            format!("thumbnail raster identity serialization failed: {error}"),
        )
    })?;
    let digest = Sha256::digest(canonical);
    let mut key = String::with_capacity("asset-thumb:".len() + digest.len() * 2);
    key.push_str("asset-thumb:");
    for byte in digest {
        write!(&mut key, "{byte:02x}").map_err(|error| {
            failure(
                ThumbnailFailureReason::OutputTransformFailed,
                format!("thumbnail raster identity formatting failed: {error}"),
            )
        })?;
    }
    Ok(key)
}

fn failure(reason: ThumbnailFailureReason, detail: impl Into<String>) -> ThumbnailFailure {
    ThumbnailFailure::new(reason, detail)
}
