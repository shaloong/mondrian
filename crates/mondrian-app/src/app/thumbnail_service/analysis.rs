//! Deterministic still decode and thumbnail color execution.

use std::fmt::Write as _;
use std::sync::mpsc;
use std::time::Instant;

use mondrian_assets::AssetRecord;
use mondrian_core::types::{ColorEngine, ColorSpace};
use mondrian_core::{OutputTransformIntent, WorkingColorSpace};
use mondrian_media::{
    DecodedRgbaEncoding, DecodedVideoMatrix, DecodedVideoRange, DecodedVideoRangeContract,
    PreviewDecodeAccessMode, PreviewDecodeOutcome, PreviewDecodeRequest,
    PreviewDecodeSessionContext, PreviewSourceColorContract,
};
use mondrian_renderer::{
    color::{ProgramOutputBoundary, ProgramOutputModule, ProgramOutputRole, SourceColorModule},
    CpuEncodedColorFrame, CpuEncodedFloatColorFrame, CpuSourceColorFrame, LinearFloatSource,
    RenderCpuColorExecutionSession,
};
use mondrian_timeline::sequence::{
    MediaInputColorContext, MissingColorMetadataPolicy, ProgramColorContext, ResolvedInputColor,
};
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
    #[serde(serialize_with = "serialize_picture_geometry")]
    pub(super) picture_geometry: mondrian_core::ResolvedPictureGeometry,
    pub(super) source_color_space: ColorSpace,
    pub(super) source_range: DecodedVideoRangeContract,
    pub(super) yuv_matrix_fallback: Option<DecodedVideoMatrix>,
    pub(super) matrix_interpretation: mondrian_core::timeline_data::MediaMatrixInterpretation,
    pub(super) working_color_space: WorkingColorSpace,
    pub(super) missing_metadata_policy: MissingColorMetadataPolicy,
    pub(super) output_color_space: ColorSpace,
    pub(super) tone_map: bool,
    pub(super) engine: ColorEngine,
    pub(super) output_transform: OutputTransformIntent,
    pub(super) camera_raw: Option<mondrian_media::CameraRawDecodeIntent>,
}

// Hash the frozen checked geometry through its public semantic projections;
// the Core value intentionally does not expose a reconstructible wire model.
fn serialize_picture_geometry<S: serde::Serializer>(
    geometry: &mondrian_core::ResolvedPictureGeometry,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    (
        geometry.encoded_resolution(),
        geometry.sample_aspect_ratio(),
        geometry.orientation(),
        geometry.scan(),
    )
        .serialize(serializer)
}

impl ThumbnailColorContract {
    fn media_input_context(&self) -> MediaInputColorContext {
        MediaInputColorContext {
            working_color_space: self.working_color_space,
            input_tone_map: false,
            engine: self.engine.clone(),
            missing_metadata_policy: self.missing_metadata_policy,
        }
    }

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
        let input_color_resolution =
            context.missing_metadata_policy().resolve_asset_input_decision(
                None,
                asset.interpretation,
                primary_video.executable_color_space(),
                context.working_color_space(),
            );
        let source_color_space = match input_color_resolution.resolved {
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
        let source_range = if primary_video.camera_raw.is_some() {
            DecodedVideoRangeContract::Automatic { probed_range: DecodedVideoRange::Full }
        } else {
            DecodedVideoRangeContract::from_video_stream(asset.interpretation.range, primary_video)
        };
        let output_color_space = context.output_color_space().color().ok_or_else(|| {
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
        let camera_raw = primary_video
            .camera_raw
            .as_ref()
            .map(|raw| {
                mondrian_media::CameraRawDecodeIntent::new(
                    raw.adapter,
                    asset.interpretation.camera_raw,
                )
                .map_err(|error| {
                    failure(
                        ThumbnailFailureReason::DecodeFailed,
                        format!("invalid camera RAW thumbnail contract: {error}"),
                    )
                })
            })
            .transpose()?;
        Ok(Self {
            video_stream_index: primary_video.index,
            picture_geometry: mondrian_core::ResolvedPictureGeometry::resolve(
                mondrian_core::Resolution {
                    width: primary_video.width,
                    height: primary_video.height,
                },
                primary_video.picture,
                None,
                None,
            )
            .map_err(|error| {
                failure(
                    ThumbnailFailureReason::InvalidRasterPayload,
                    error.to_string(),
                )
            })?,
            source_color_space,
            source_range,
            yuv_matrix_fallback: SourceColorModule::decode_contract(
                source_color_space,
                source_range,
                input_color_resolution.source,
            )
            .with_video_stream_sampling(primary_video)
            .yuv_matrix_fallback,
            matrix_interpretation: asset.interpretation.matrix,
            working_color_space: context.working_color_space(),
            missing_metadata_policy: context.missing_metadata_policy(),
            output_color_space,
            tone_map: context.output_tone_map(),
            engine: context.engine().clone(),
            output_transform: context.output_transform().clone(),
            camera_raw,
        })
    }

    pub(super) fn output_boundary(&self) -> Result<ProgramOutputBoundary, ThumbnailFailure> {
        ProgramOutputBoundary::from_intent(
            ProgramOutputRole::Display,
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
    let mut source_color = PreviewSourceColorContract::new(
        job.key.color.source_color_space,
        job.key.color.source_range,
    );
    if let Some(matrix) = job.key.color.yuv_matrix_fallback {
        source_color = source_color.with_yuv_matrix_fallback(matrix);
    }
    source_color = source_color.with_matrix_interpretation(job.key.color.matrix_interpretation);
    let mut request = PreviewDecodeRequest::new(
        job.key.path.as_path(),
        job.key.source_sample,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        source_color,
    )
    .with_video_stream_index(job.key.color.video_stream_index)
    .with_max_size(Some(THUMBNAIL_MAX_WIDTH), Some(THUMBNAIL_MAX_HEIGHT))
    .with_fingerprint(job.key.fingerprint);
    if let Some(camera_raw) = job.key.color.camera_raw {
        request = request.with_camera_raw(camera_raw);
    }
    let cancellation = job.cancellation.clone();
    let first = decode_context.decode_cancellable(request, move || cancellation.is_canceled());
    let zero = mondrian_core::SourceSampleTarget::covering(mondrian_core::TimelineTime::ZERO);
    let (outcome, sample_fallback) = match first {
        Err(error @ mondrian_core::MondrianError::DecodeTemporalMismatch { .. })
            if job.key.source_sample != zero && !job.cancellation.is_canceled() =>
        {
            // One bounded alternate representative sample, never a Still,
            // Playback or Export rule. Color/decoder failures do not enter
            // this path. The original target remains in request identity.
            request.source_sample = zero;
            let cancellation = job.cancellation.clone();
            let outcome =
                decode_context.decode_cancellable(request, move || cancellation.is_canceled());
            (
                outcome,
                Some(super::ThumbnailSampleFallback {
                    requested: job.key.source_sample,
                    selected: zero,
                    failure: failure(ThumbnailFailureReason::DecodeFailed, error.to_string()),
                }),
            )
        }
        result => (result, None),
    };
    let (width, height, rgba) = match outcome {
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
        Ok(PreviewDecodeOutcome::Unorm16Frame(frame)) => {
            let width = frame.width;
            let height = frame.height;
            let encoding = frame.color_contract.encoding;
            let rgba = color_manage_float_with_session(
                width,
                height,
                frame.into_normalized_f32(),
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
    let selected_sample =
        sample_fallback.as_ref().map_or(job.key.source_sample, |value| value.selected);
    let resource_key = if selected_sample == job.key.source_sample {
        thumbnail_key(job, width, height)?
    } else {
        thumbnail_key_at_sample(job, width, height, selected_sample)?
    };
    let mut frame = ThumbnailRasterFrame::new(
        resource_key,
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
    })?;
    frame.sample_fallback = sample_fallback;
    Ok(frame)
}

fn color_manage_rgba_with_session(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    color: &ThumbnailColorContract,
    color_session: &mut RenderCpuColorExecutionSession,
) -> Result<Vec<u8>, ThumbnailFailure> {
    let source = CpuSourceColorFrame::from(CpuEncodedColorFrame::source_rgba8(
        width,
        height,
        color.source_color_space,
        rgba,
    ));
    let working = SourceColorModule::execute_cpu(
        &source,
        &color.media_input_context(),
        color
            .picture_geometry
            .image_sampling(mondrian_core::Resolution { width, height })
            .map_err(|error| {
                failure(
                    ThumbnailFailureReason::InvalidRasterPayload,
                    error.to_string(),
                )
            })?,
        color_session,
    )
    .map_err(|error| {
        failure(
            ThumbnailFailureReason::InputTransformFailed,
            format!("thumbnail input transform failed: {error}"),
        )
    })?;
    ProgramOutputModule::execute_cpu_rgba8(
        working.frame(),
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
            DecodedRgbaEncoding::DataTexture => {
                return Err(failure(
                    ThumbnailFailureReason::NonColorDataUnsupported,
                    "thumbnail cannot color-manage a decoded data texture",
                ));
            }
        };
    let working = SourceColorModule::execute_cpu(
        &source,
        &color.media_input_context(),
        color
            .picture_geometry
            .image_sampling(mondrian_core::Resolution { width, height })
            .map_err(|error| {
                failure(
                    ThumbnailFailureReason::InvalidRasterPayload,
                    error.to_string(),
                )
            })?,
        color_session,
    )
    .map_err(|error| {
        failure(
            ThumbnailFailureReason::InputTransformFailed,
            format!("thumbnail float input transform failed: {error}"),
        )
    })?;
    ProgramOutputModule::execute_cpu_rgba8(
        working.frame(),
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

/// One deterministic representative position: ten percent of a known video,
/// capped at thirty seconds. Proven stills and unknown durations stay at zero.
/// This selects a sample, never judges brightness or changes color semantics.
pub(super) fn thumbnail_source_sample(asset: &AssetRecord) -> mondrian_core::SourceSampleTarget {
    use mondrian_core::{SourceSampleTarget, TimelineTime};
    let nanos = asset
        .media_probe()
        .and_then(|probe| probe.primary_video().map(|video| (probe, video)))
        .filter(|(_, video)| video.total_frames != Some(1))
        .map(|(probe, video)| video.duration.unwrap_or(probe.duration).as_nanos() / 10)
        .unwrap_or(0)
        .min(30_000_000_000);
    SourceSampleTarget::covering(
        TimelineTime::new(nanos as i64, 1_000_000_000).expect("bounded thumbnail timestamp"),
    )
}

pub(super) fn thumbnail_key(
    job: &ThumbnailJob,
    width: u32,
    height: u32,
) -> Result<String, ThumbnailFailure> {
    thumbnail_key_at_sample(job, width, height, job.key.source_sample)
}

fn thumbnail_key_at_sample(
    job: &ThumbnailJob,
    width: u32,
    height: u32,
    selected_sample: mondrian_core::SourceSampleTarget,
) -> Result<String, ThumbnailFailure> {
    #[derive(Serialize)]
    struct ThumbnailRasterIdentity<'a> {
        schema_version: u8,
        asset_id: mondrian_core::AssetId,
        source_revision: &'a mondrian_core::MediaFileFingerprint,
        color: &'a ThumbnailColorContract,
        source_sample: mondrian_core::SourceSampleTarget,
        selected_sample: mondrian_core::SourceSampleTarget,
        width: u32,
        height: u32,
    }

    let identity = ThumbnailRasterIdentity {
        schema_version: 3,
        asset_id: job.key.asset_id,
        source_revision: &job.key.fingerprint,
        color: &job.key.color,
        source_sample: job.key.source_sample,
        selected_sample,
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
