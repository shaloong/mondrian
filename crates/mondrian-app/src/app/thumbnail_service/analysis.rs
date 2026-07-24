//! Deterministic still decode and thumbnail color execution.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::mpsc;
use std::time::Instant;

use mondrian_assets::AssetRecord;
use mondrian_core::types::{ColorEngine, ColorSpace};
use mondrian_core::{OutputTransformIntent, WorkingColorSpace};
use mondrian_media::{
    decode_preview_frame_cancellable, DecodedVideoRangeContract, PreviewDecodeAccessMode,
    PreviewDecodeOutcome, PreviewDecodeRequest, PreviewSourceColorContract,
};
use mondrian_renderer::{
    execute_cpu_input_stage, execute_cpu_input_stage_float, execute_cpu_output_boundary_rgba8,
    CpuEncodedColorFrame, LinearFloatSource, RenderInputTransform, RenderOutputColorBoundary,
};
use mondrian_timeline::sequence::{ProgramColorContext, ResolvedInputColor};

use crate::app::preview_access_mode::{
    media_preview_access_mode_for_intent, MediaPreviewAccessIntent,
};

use super::state::{ThumbnailJob, ThumbnailResult};
use super::{
    ThumbnailFailure, ThumbnailFailureReason, ThumbnailRasterColorSpace, ThumbnailRasterFrame,
};

const THUMBNAIL_MAX_WIDTH: u32 = 320;
const THUMBNAIL_MAX_HEIGHT: u32 = 180;

pub(super) fn thumbnail_worker(
    jobs: mpsc::Receiver<ThumbnailJob>,
    results: mpsc::SyncSender<ThumbnailResult>,
) {
    for job in jobs {
        let started = Instant::now();
        let result = decode_thumbnail(&job);
        let result = ThumbnailResult {
            key: job.key,
            generation: job.generation,
            result,
            elapsed: started.elapsed(),
        };
        if results.send(result).is_err() {
            break;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ThumbnailColorContract {
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
        let primary_video = asset.media_info.primary_video().ok_or_else(|| {
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
                primary_video.detected_color_space,
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
) -> Result<ThumbnailRasterFrame, ThumbnailFailure> {
    debug_assert_eq!(
        media_preview_access_mode_for_intent(MediaPreviewAccessIntent::DeterministicStill),
        PreviewDecodeAccessMode::RandomAccessStillFrame
    );
    let request = PreviewDecodeRequest::new(
        job.key.path.as_path(),
        mondrian_core::TimelineTime::ZERO,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        PreviewSourceColorContract::new(
            job.key.color.source_color_space,
            job.key.color.source_range,
        ),
    )
    .with_max_size(Some(THUMBNAIL_MAX_WIDTH), Some(THUMBNAIL_MAX_HEIGHT))
    .with_fingerprint(job.key.fingerprint);
    let cancellation = job.cancellation.clone();
    let (width, height, rgba) =
        match decode_preview_frame_cancellable(request, move || cancellation.is_canceled()) {
            Ok(PreviewDecodeOutcome::Frame(frame)) => {
                let width = frame.width;
                let height = frame.height;
                let rgba = color_manage_rgba(width, height, frame.into_data(), &job.key.color)?;
                (width, height, rgba)
            }
            Ok(PreviewDecodeOutcome::FloatFrame(frame)) => {
                let width = frame.width;
                let height = frame.height;
                let rgba = color_manage_float(width, height, frame.into_data(), &job.key.color)?;
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
            Err(error) => {
                return Err(failure(
                    ThumbnailFailureReason::DecodeFailed,
                    error.to_string(),
                ));
            }
        };
    ThumbnailRasterFrame::new(
        thumbnail_key(job, width, height),
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

pub(super) fn color_manage_rgba(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    color: &ThumbnailColorContract,
) -> Result<Vec<u8>, ThumbnailFailure> {
    let source = CpuEncodedColorFrame::source_rgba8(width, height, color.source_color_space, rgba);
    let input =
        RenderInputTransform::to_working(color.working_color_space, false, color.engine.clone());
    let working = execute_cpu_input_stage(&source, &input).map_err(|error| {
        failure(
            ThumbnailFailureReason::InputTransformFailed,
            format!("thumbnail input transform failed: {error}"),
        )
    })?;
    execute_cpu_output_boundary_rgba8(&working.result.frame, &color.output_boundary()?)
        .map(|output| output.rgba)
        .map_err(|error| {
            failure(
                ThumbnailFailureReason::OutputTransformFailed,
                format!("thumbnail display transform failed: {error}"),
            )
        })
}

fn color_manage_float(
    width: u32,
    height: u32,
    rgba: Vec<f32>,
    color: &ThumbnailColorContract,
) -> Result<Vec<u8>, ThumbnailFailure> {
    let source = LinearFloatSource::new(width, height, color.source_color_space, rgba);
    let input =
        RenderInputTransform::to_working(color.working_color_space, false, color.engine.clone());
    let working = execute_cpu_input_stage_float(&source, &input).map_err(|error| {
        failure(
            ThumbnailFailureReason::InputTransformFailed,
            format!("thumbnail float input transform failed: {error}"),
        )
    })?;
    execute_cpu_output_boundary_rgba8(&working.result.frame, &color.output_boundary()?)
        .map(|output| output.rgba)
        .map_err(|error| {
            failure(
                ThumbnailFailureReason::OutputTransformFailed,
                format!("thumbnail float display transform failed: {error}"),
            )
        })
}

pub(super) fn thumbnail_key(job: &ThumbnailJob, width: u32, height: u32) -> String {
    let mut color_hasher = DefaultHasher::new();
    job.key.color.hash(&mut color_hasher);
    let color_signature = color_hasher.finish();
    let len = job
        .key
        .fingerprint
        .len
        .map_or_else(|| "unknown".to_owned(), |len| len.to_string());
    let modified = match (
        job.key.fingerprint.modified_secs,
        job.key.fingerprint.modified_nanos,
    ) {
        (Some(secs), Some(nanos)) => format!("{secs}-{nanos}"),
        _ => "unknown".to_owned(),
    };
    format!(
        "asset-thumb:{}:{width}x{height}:len{len}:mtime{modified}:sig{color_signature:016x}",
        job.key.asset_id
    )
}

fn failure(reason: ThumbnailFailureReason, detail: impl Into<String>) -> ThumbnailFailure {
    ThumbnailFailure::new(reason, detail)
}
