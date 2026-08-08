//! 后台渲染队列

use crate::delivery::{ffmpeg_audio_channel_layout, ResolvedExportDeliveryContract};
#[cfg(test)]
use crate::preset::TimelineExportRange;
use crate::preset::{
    AudioCodecConfig, Container, ExportAlphaMode, ExportConfig, ExportOutputPolicy,
    ResolvedTimelineExportRange, TimelineExportSnapshot, VideoCodecConfig,
};
use crate::validator::{
    delivery_bit_depth_value, expected_audio_constraints, expected_video_encoding,
    validate_export_output_cancellable, ExpectedStream, ExpectedVideoConstraints,
    ExportValidationExpectations,
};
use crate::{PreparedTimelineAudioSnapshot, PreparedTimelineVisualSnapshot};
use mondrian_audio::{
    AudioContinuityEpoch, AudioDecodedSource, AudioMediaResolver, AudioProcessingMode,
    AudioProgramDeliveryRuntime, AudioProgramRuntime, AudioRenderContract, AudioRenderRequest,
    ResolvedAudioSource,
};
use mondrian_core::timeline_data::{AlphaInterpretation, TimelineClipExecutionRef};
#[cfg(test)]
use mondrian_core::types::ColorEngine;
use mondrian_core::types::{AssetId, ColorSpace, FramePosition, Rational};
use mondrian_core::{
    AudioChannelLayout, AudioSamplePosition, AudioSampleRate, AudioSampleRounding,
    AudioSourceComponentId, ExecutionCancellationToken, Resolution, SequenceId, TimelineTime,
    TimelineTimeRange, WorkingColorSpace, WorkingRgbaF32Frame,
};
use mondrian_effects::{
    identity_compiled_effect_graph, EffectExecutionContinuity, EffectExecutionSessionConfig,
    EffectFrameExtent, EffectFrameTileF32, EffectTemporalSourceIdentity, PreparedTemporalFrameSet,
};
use mondrian_media::AudioSourceCache;
#[cfg(test)]
use mondrian_media::PreviewDecodeSessionDisposition;
use mondrian_media::{
    DecodedVideoRange, DecodedVideoRangeContract, MediaFileFingerprint, PreviewDecodeAccessMode,
    PreviewDecodeDiagnostics, PreviewDecodeOutcome, PreviewDecodeRequest,
    PreviewDecodeSessionContext, PreviewSourceColorContract, SupervisedChild,
    SupervisedProcessError, SupervisedProcessPolicy, SupervisedStreamCapture,
    VideoColorDiagnosticIssueAggregate,
};
use mondrian_renderer::{
    color_report_vocab, composite_timeline_elements_color_frame_with_diagnostics,
    execute_cpu_output_boundary_float_with_session, execute_cpu_output_boundary_rgba8_with_session,
    execute_cpu_source_input_stage_with_session, execute_cpu_working_transform_with_session,
    prepare_visual_frame_closure, project_affine_to_sampled_extents, project_basic_title_transform,
    BasicTitleRasterizer, ColorFrameResidency, CpuColorFrame, CpuEncodedColorFrame,
    CpuSourceColorFrame, GpuColorFrameReadbackPlan, GpuColorFrameTextureFormat,
    GpuColorFrameWgpuResourcePool, GpuColorFrameWgpuResourcePoolOptions, GpuContext,
    HeterogeneousCpuPrefixSource, HeterogeneousGpuCompletedEvidence,
    HeterogeneousGpuCompletedFrame, HeterogeneousGpuContinuationError,
    HeterogeneousGpuContinuationRequest, HeterogeneousGpuContinuationRuntime, LinearFloatSource,
    PreparedVisualChildCanvasPolicy, PreparedVisualFrameClosure, PreparedVisualFrameClosureRequest,
    PreparedVisualFrameEvaluation, PreparedVisualFrameNode, PreparedVisualFrameNodeId,
    PreparedVisualMaterializationContract, PreparedVisualNestedSample, PreparedVisualProgram,
    RenderColorStageDiagnostics, RenderColorStageGpuBlockerBreakdown,
    RenderColorTransformGpuOptions, RenderGpuOutputBoundaryRuntime,
    RenderGpuOutputBoundaryRuntimeOwnedBackendContext, RenderGpuOutputBoundaryRuntimeRecordError,
    RenderGpuOutputExecutionResourceGrant, RenderInputTransform, RenderOutputColorBoundary,
    TimelineAdjustmentLayer, TimelineBasicTitlePlan, TimelineCompositeColorPathSummary,
    TimelineCompositeDiagnostics, TimelineCompositeDomainBlockerBreakdown,
    TimelineCompositeElement, TimelineCompositeLegacyBreakdown, TimelineCompositeOptions,
    TimelineCompositeScratch, TimelineCpuCompositePrecision, TimelineCrossDissolveLayer,
    TimelineEffectColorRuntime, TimelineEvaluationRequest, TimelineFrameExecutionRequest,
    TimelineMediaLayer, TimelineMediaPlan, TimelineRenderPlanElement, TimelineSolidColorLayer,
    TimelineTemporalDemandBatch, TimelineTemporalSource, TimelineTransitionInput,
    TimelineTransitionInputPlan,
};
#[cfg(test)]
use mondrian_renderer::{PreparedVisualProgramCache, PreparedVisualProgramCacheConfig};
use mondrian_storage::{
    FilePublicationEvidence, FilePublicationFailure, FilePublicationMode, OwnedPublicationFile,
};
use mondrian_timeline::sequence::{
    DeliveryBitDepth, InputColorResolutionSourceCounts, ProgramColorContext, ResolvedInputColor,
    SequenceSettings, VideoRange,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
#[cfg(test)]
use std::collections::HashSet;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use tokio::runtime::Builder as TokioRuntimeBuilder;

mod service;
#[cfg(feature = "validation")]
mod validation;
mod visual_effect_execution;
pub use service::*;
#[cfg(feature = "validation")]
pub use validation::{export_visual_frame_validation, ExportVisualFrameValidation};
use visual_effect_execution::{
    prepare_export_effect_frame_plan, ExportHeterogeneousRouteContract,
    PreparedExportHeterogeneousElement,
};
#[cfg(test)]
use visual_effect_execution::{ExportHeterogeneousEffectError, ExportHeterogeneousPlacement};

/// Internal pipe contract selected from the requested delivery bit depth.
///
/// This determines the GPU texture format, FFmpeg input pixel format, and
/// canvas allocation strategy for the export pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFrameContract {
    /// 8-bit RGBA legacy boundary. Used for SDR 8-bit delivery.
    Rgba8,
    /// 16-bit float RGBA boundary. Used for 10-bit ProRes/HDR delivery
    /// and camera-log intermediates.
    Rgba16Float,
}

impl ExportFrameContract {
    /// Select the internal pipe contract from the delivery sample depth.
    pub fn from_bit_depth(bit_depth: DeliveryBitDepth) -> Self {
        match bit_depth {
            DeliveryBitDepth::Eight => Self::Rgba8,
            DeliveryBitDepth::Ten | DeliveryBitDepth::Twelve => Self::Rgba16Float,
        }
    }

    /// GPU texture format for this contract.
    pub fn gpu_texture_format(&self) -> GpuColorFrameTextureFormat {
        match self {
            Self::Rgba8 => GpuColorFrameTextureFormat::Rgba8Unorm,
            Self::Rgba16Float => GpuColorFrameTextureFormat::Rgba16Float,
        }
    }

    /// FFmpeg input pixel format string for the raw video pipe.
    pub fn ffmpeg_pix_fmt(&self) -> &'static str {
        match self {
            Self::Rgba8 => "rgba",
            Self::Rgba16Float => "rgba64le",
        }
    }

    /// Bytes per pixel for canvas allocation.
    pub fn bytes_per_pixel(&self) -> usize {
        match self {
            Self::Rgba8 => 4,
            Self::Rgba16Float => 8, // 4 channels × 2 bytes (f16)
        }
    }

    /// Canvas byte length for given dimensions.
    pub fn canvas_len(&self, width: u32, height: u32) -> usize {
        width as usize * height as usize * self.bytes_per_pixel()
    }

    /// Whether this output contract requires more precision than an RGBA8 CPU boundary provides.
    pub fn requires_high_precision_boundary(&self) -> bool {
        matches!(self, Self::Rgba16Float)
    }

    /// Pack RGBA8 pixels into the raw-video pipe format described by this contract.
    pub fn pack_rgba8(&self, rgba: &[u8]) -> Vec<u8> {
        match self {
            Self::Rgba8 => rgba.to_vec(),
            Self::Rgba16Float => pack_rgba8_to_rgba64le(rgba),
        }
    }

    /// Pack normalized float RGBA pixels into the raw-video pipe format.
    pub fn pack_rgba_f32(&self, rgba: &[f32]) -> Vec<u8> {
        match self {
            Self::Rgba8 => {
                rgba.iter().map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8).collect()
            }
            Self::Rgba16Float => pack_rgba_f32_to_rgba64le(rgba),
        }
    }

    /// Convert this contract's raw-video pipe bytes back to an RGBA8 source boundary.
    pub fn to_rgba8_boundary(&self, pixels: &[u8]) -> Vec<u8> {
        match self {
            Self::Rgba8 => pixels.to_vec(),
            Self::Rgba16Float => unpack_rgba64le_to_rgba8(pixels),
        }
    }
}

/// Resolve the export frame contract from the admitted delivery sample depth.
fn export_frame_contract(bit_depth: DeliveryBitDepth) -> ExportFrameContract {
    ExportFrameContract::from_bit_depth(bit_depth)
}

/// Renderer-owned CPU float/high-bit output boundary for export.
///
/// In production this is a transparent pass-through to the renderer's
/// `execute_cpu_output_boundary_float`. Test builds support failure injection
/// via `FORCE_FLOAT_BOUNDARY_FAILURE` so integration tests can exercise the
/// high-precision fail-closed branch without mocking the color engine.
fn cpu_output_boundary_float(
    frame: &CpuColorFrame,
    boundary: &RenderOutputColorBoundary,
    session: &mut mondrian_renderer::RenderCpuColorExecutionSession,
) -> Result<
    mondrian_renderer::RenderOutputColorBoundaryFloat,
    mondrian_renderer::RenderColorTransformError,
> {
    #[cfg(test)]
    {
        if FORCE_FLOAT_BOUNDARY_FAILURE.with(|cell| cell.get()) {
            return Err(
                mondrian_renderer::RenderColorTransformError::UnsupportedStagePlan {
                    reason: "test-injected float boundary failure",
                },
            );
        }
    }
    execute_cpu_output_boundary_float_with_session(frame, boundary, session)
}

#[cfg(test)]
thread_local! {
    /// Test-only flag that forces `cpu_output_boundary_float` to return
    /// `Err`, exercising the high-precision fail-closed branch in real render code.
    static FORCE_FLOAT_BOUNDARY_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Test-only flag that forces export GPU output scheduling to fail before runtime access.
    static FORCE_GPU_BOUNDARY_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Guard that sets and clears `FORCE_FLOAT_BOUNDARY_FAILURE` for the duration
/// of a scope, ensuring the flag is always reset even on panic.
#[cfg(test)]
struct FloatBoundaryFailureGuard;

#[cfg(test)]
impl FloatBoundaryFailureGuard {
    fn activate() -> Self {
        FORCE_FLOAT_BOUNDARY_FAILURE.with(|cell| cell.set(true));
        FloatBoundaryFailureGuard
    }
}

#[cfg(test)]
impl Drop for FloatBoundaryFailureGuard {
    fn drop(&mut self) {
        FORCE_FLOAT_BOUNDARY_FAILURE.with(|cell| cell.set(false));
    }
}

#[cfg(test)]
struct GpuBoundaryFailureGuard;

#[cfg(test)]
impl GpuBoundaryFailureGuard {
    fn activate() -> Self {
        FORCE_GPU_BOUNDARY_FAILURE.with(|cell| cell.set(true));
        Self
    }
}

#[cfg(test)]
impl Drop for GpuBoundaryFailureGuard {
    fn drop(&mut self) {
        FORCE_GPU_BOUNDARY_FAILURE.with(|cell| cell.set(false));
    }
}

fn pack_rgba8_to_rgba64le(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len() * 2);
    for channel in rgba {
        out.extend_from_slice(&u16::from(*channel).saturating_mul(257).to_le_bytes());
    }
    out
}

fn pack_rgba_f32_to_rgba64le(rgba: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len() * 2);
    for channel in rgba {
        let value = (channel.clamp(0.0, 1.0) * 65_535.0).round() as u16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

fn unpack_rgba64le_to_rgba8(rgba64le: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba64le.len() / 2);
    for channel in rgba64le.chunks_exact(2) {
        let value = u16::from_le_bytes([channel[0], channel[1]]);
        out.push((value / 257) as u8);
    }
    out
}

fn fill_canvas_black_opaque(
    canvas: &mut Vec<u8>,
    contract: ExportFrameContract,
    width: u32,
    height: u32,
) {
    canvas.clear();
    match contract {
        ExportFrameContract::Rgba8 => {
            canvas.resize(contract.canvas_len(width, height), 0);
            for px in canvas.chunks_exact_mut(4) {
                px[3] = 255;
            }
        }
        ExportFrameContract::Rgba16Float => {
            canvas.reserve(contract.canvas_len(width, height));
            for _ in 0..width as usize * height as usize {
                canvas.extend_from_slice(&0u16.to_le_bytes());
                canvas.extend_from_slice(&0u16.to_le_bytes());
                canvas.extend_from_slice(&0u16.to_le_bytes());
                canvas.extend_from_slice(&u16::MAX.to_le_bytes());
            }
        }
    }
}

struct ExportGpuOutputBackend {
    context: Arc<GpuContext>,
    runtime: RenderGpuOutputBoundaryRuntime,
    heterogeneous_runtime: HeterogeneousGpuContinuationRuntime,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
struct ExportGpuOutputAttemptOutcome {
    rgba: Vec<u8>,
    stage_diagnostics: RenderColorStageDiagnostics,
}

const EXPORT_GPU_READBACK_TIMEOUT: Duration = Duration::from_secs(30);
const EXPORT_GPU_READBACK_POLL_SLICE: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportGpuOutputExecutionError {
    Fallback(ExportGpuOutputFallbackReason),
    Canceled,
    DeviceTimedOut,
}

impl From<ExportGpuOutputFallbackReason> for ExportGpuOutputExecutionError {
    fn from(reason: ExportGpuOutputFallbackReason) -> Self {
        Self::Fallback(reason)
    }
}

#[derive(Debug, thiserror::Error)]
enum ExportHeterogeneousGpuExecutionError {
    #[error("export heterogeneous GPU backend is unavailable: {reason:?}")]
    BackendUnavailable {
        reason: ExportGpuOutputFallbackReason,
    },
    #[error(transparent)]
    Continuation(Box<HeterogeneousGpuContinuationError>),
}

impl From<HeterogeneousGpuContinuationError> for ExportHeterogeneousGpuExecutionError {
    fn from(error: HeterogeneousGpuContinuationError) -> Self {
        Self::Continuation(Box::new(error))
    }
}

enum ExportGpuExecutionRuntimeState {
    Cold,
    Ready {
        device_generation: u64,
        backend: Box<ExportGpuOutputBackend>,
    },
    Backoff {
        attempt_generation: u64,
    },
}

struct ExportGpuExecutionRuntime {
    attempt_generation: u64,
    next_device_generation: u64,
    resource_pool_options: GpuColorFrameWgpuResourcePoolOptions,
    active_output_grant: RenderGpuOutputExecutionResourceGrant,
    state: ExportGpuExecutionRuntimeState,
}

impl Default for ExportGpuExecutionRuntime {
    fn default() -> Self {
        Self {
            attempt_generation: 0,
            next_device_generation: 1,
            resource_pool_options: GpuColorFrameWgpuResourcePoolOptions {
                max_per_contract: 1,
                max_retained_bytes: 96 * 1024 * 1024,
            },
            active_output_grant: RenderGpuOutputExecutionResourceGrant::new(1024 * 1024 * 1024, 4),
            state: ExportGpuExecutionRuntimeState::Cold,
        }
    }
}

impl ExportGpuExecutionRuntime {
    fn configure(&mut self, policy: service::ExportExecutionResourcePolicy) {
        let options = GpuColorFrameWgpuResourcePoolOptions {
            max_per_contract: policy.gpu_output_idle_per_contract,
            max_retained_bytes: policy.gpu_output_idle_bytes,
        };
        self.active_output_grant = policy.gpu_output_active;
        if self.resource_pool_options == options {
            return;
        }
        self.resource_pool_options = options;
        self.state = ExportGpuExecutionRuntimeState::Cold;
    }

    fn begin_attempt(&mut self, attempt_generation: u64) {
        if self.attempt_generation == attempt_generation {
            return;
        }
        self.attempt_generation = attempt_generation;
        self.state = ExportGpuExecutionRuntimeState::Cold;
    }

    fn ensure_ready(&mut self) -> Result<(), ExportGpuOutputFallbackReason> {
        match self.state {
            ExportGpuExecutionRuntimeState::Ready { .. } => return Ok(()),
            ExportGpuExecutionRuntimeState::Backoff { attempt_generation }
                if attempt_generation == self.attempt_generation =>
            {
                return Err(ExportGpuOutputFallbackReason::ContextUnavailable);
            }
            ExportGpuExecutionRuntimeState::Cold
            | ExportGpuExecutionRuntimeState::Backoff { .. } => {}
        }

        let Some(next_device_generation) = self.next_device_generation.checked_add(1) else {
            self.state = ExportGpuExecutionRuntimeState::Backoff {
                attempt_generation: self.attempt_generation,
            };
            return Err(ExportGpuOutputFallbackReason::ContextUnavailable);
        };
        let device_generation = self.next_device_generation;
        match build_export_gpu_output_runtime(self.resource_pool_options) {
            Ok(backend) => {
                self.next_device_generation = next_device_generation;
                self.state = ExportGpuExecutionRuntimeState::Ready { device_generation, backend };
                Ok(())
            }
            Err(_) => {
                self.state = ExportGpuExecutionRuntimeState::Backoff {
                    attempt_generation: self.attempt_generation,
                };
                Err(ExportGpuOutputFallbackReason::ContextUnavailable)
            }
        }
    }

    fn execute(
        &mut self,
        frame: &CpuColorFrame,
        boundary: &RenderOutputColorBoundary,
        frame_contract: ExportFrameContract,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<ExportGpuOutputAttemptOutcome, ExportGpuOutputExecutionError> {
        #[cfg(test)]
        if FORCE_GPU_BOUNDARY_FAILURE.with(|cell| cell.get()) {
            return Err(ExportGpuOutputFallbackReason::ContextUnavailable.into());
        }
        self.ensure_ready().map_err(ExportGpuOutputExecutionError::from)?;
        let result = match &mut self.state {
            ExportGpuExecutionRuntimeState::Ready { device_generation, backend } => {
                let _device_generation = *device_generation;
                let result = execute_export_gpu_output_boundary_with_backend(
                    backend,
                    frame,
                    boundary,
                    frame_contract,
                    self.active_output_grant,
                    cancellation,
                );
                if result.is_err() {
                    // Output-boundary recording owns a distinct frame table but
                    // shares the device texture pool with required
                    // heterogeneous execution. A route-local failure must not
                    // leave partially materialized output resources visible to
                    // the next required-GPU request.
                    backend.runtime.clear_frame_resources();
                }
                result
            }
            ExportGpuExecutionRuntimeState::Cold
            | ExportGpuExecutionRuntimeState::Backoff { .. } => {
                Err(ExportGpuOutputFallbackReason::ContextUnavailable.into())
            }
        };
        if result
            .as_ref()
            .is_err_and(|error| export_gpu_error_requires_backend_backoff(*error))
        {
            self.state = ExportGpuExecutionRuntimeState::Backoff {
                attempt_generation: self.attempt_generation,
            };
        }
        result
    }

    fn execute_heterogeneous(
        &mut self,
        request: HeterogeneousGpuContinuationRequest,
        completion: mondrian_effects::PreparedHeterogeneousCpuCompletion,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<HeterogeneousGpuCompletedFrame, ExportHeterogeneousGpuExecutionError> {
        #[cfg(test)]
        if FORCE_GPU_BOUNDARY_FAILURE.with(|cell| cell.get()) {
            return Err(ExportHeterogeneousGpuExecutionError::BackendUnavailable {
                reason: ExportGpuOutputFallbackReason::ContextUnavailable,
            });
        }
        self.ensure_ready().map_err(|reason| {
            ExportHeterogeneousGpuExecutionError::BackendUnavailable { reason }
        })?;
        let deadline = Instant::now().checked_add(EXPORT_GPU_READBACK_TIMEOUT).ok_or(
            ExportHeterogeneousGpuExecutionError::from(
                HeterogeneousGpuContinuationError::DeadlineExceeded,
            ),
        )?;
        match &mut self.state {
            ExportGpuExecutionRuntimeState::Ready { backend, .. } => backend
                .heterogeneous_runtime
                .execute_to_cpu(request, completion, cancellation, deadline)
                .map_err(Into::into),
            ExportGpuExecutionRuntimeState::Cold
            | ExportGpuExecutionRuntimeState::Backoff { .. } => {
                Err(ExportHeterogeneousGpuExecutionError::BackendUnavailable {
                    reason: ExportGpuOutputFallbackReason::ContextUnavailable,
                })
            }
        }
    }
}

const fn export_gpu_error_requires_backend_backoff(error: ExportGpuOutputExecutionError) -> bool {
    matches!(error, ExportGpuOutputExecutionError::DeviceTimedOut)
}

fn build_export_gpu_output_runtime(
    resource_pool_options: GpuColorFrameWgpuResourcePoolOptions,
) -> Result<Box<ExportGpuOutputBackend>, String> {
    let runtime = TokioRuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("build gpu runtime failed: {err}"))?;
    let context = runtime
        .block_on(GpuContext::new())
        .map_err(|err| format!("create gpu context failed: {err}"))?;
    let resource_pool = Arc::new(GpuColorFrameWgpuResourcePool::new(resource_pool_options));
    let heterogeneous_runtime = HeterogeneousGpuContinuationRuntime::with_resource_pool(
        Arc::clone(&context),
        Arc::clone(&resource_pool),
    )
    .map_err(|err| format!("create heterogeneous GPU continuation runtime failed: {err}"))?;
    Ok(Box::new(ExportGpuOutputBackend {
        context,
        runtime: RenderGpuOutputBoundaryRuntime::with_resource_pool(resource_pool)
            .map_err(|err| format!("create GPU output runtime failed: {err}"))?,
        heterogeneous_runtime,
    }))
}

struct ExportGpuReadbackMapLease<'a> {
    buffer: &'a wgpu::Buffer,
}

impl<'a> ExportGpuReadbackMapLease<'a> {
    fn new(buffer: &'a wgpu::Buffer) -> Self {
        Self { buffer }
    }
}

impl Drop for ExportGpuReadbackMapLease<'_> {
    fn drop(&mut self) {
        self.buffer.unmap();
    }
}

fn map_readback_buffer_sync(
    device: &wgpu::Device,
    readback: &wgpu::Buffer,
    submission_index: &wgpu::SubmissionIndex,
    cancellation: &ExecutionCancellationToken,
    deadline: Instant,
) -> Result<Vec<u8>, ExportGpuOutputExecutionError> {
    let slice = readback.slice(..);
    let (tx, rx) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    let _map_lease = ExportGpuReadbackMapLease::new(readback);

    loop {
        match rx.try_recv() {
            Ok(Ok(())) => break,
            Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                return Err(ExportGpuOutputFallbackReason::ReadbackMapFailed.into());
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let poll_timeout =
            export_gpu_readback_poll_timeout(cancellation.is_canceled(), Instant::now(), deadline)?;
        accept_export_gpu_readback_poll(device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission_index.clone()),
            timeout: Some(poll_timeout),
        }))?;
    }
    let mapped = slice
        .get_mapped_range()
        .map_err(|_| ExportGpuOutputFallbackReason::ReadbackMapFailed)?;
    let bytes = mapped.to_vec();
    drop(mapped);
    if cancellation.is_canceled() {
        return Err(ExportGpuOutputExecutionError::Canceled);
    }
    Ok(bytes)
}

fn accept_export_gpu_readback_poll(
    result: Result<wgpu::PollStatus, wgpu::PollError>,
) -> Result<(), ExportGpuOutputExecutionError> {
    match result {
        Ok(_) | Err(wgpu::PollError::Timeout) => Ok(()),
        Err(wgpu::PollError::WrongSubmissionIndex(_, _)) => {
            Err(ExportGpuOutputFallbackReason::ReadbackMapFailed.into())
        }
    }
}

fn export_gpu_readback_poll_timeout(
    canceled: bool,
    now: Instant,
    deadline: Instant,
) -> Result<Duration, ExportGpuOutputExecutionError> {
    if canceled {
        return Err(ExportGpuOutputExecutionError::Canceled);
    }
    let remaining = deadline
        .checked_duration_since(now)
        .ok_or(ExportGpuOutputExecutionError::DeviceTimedOut)?;
    let poll_timeout = remaining.min(EXPORT_GPU_READBACK_POLL_SLICE);
    if poll_timeout.is_zero() {
        return Err(ExportGpuOutputExecutionError::DeviceTimedOut);
    }
    Ok(poll_timeout)
}

fn execute_export_gpu_output_boundary_with_backend(
    backend: &mut ExportGpuOutputBackend,
    frame: &CpuColorFrame,
    boundary: &RenderOutputColorBoundary,
    frame_contract: ExportFrameContract,
    active_grant: RenderGpuOutputExecutionResourceGrant,
    cancellation: &ExecutionCancellationToken,
) -> Result<ExportGpuOutputAttemptOutcome, ExportGpuOutputExecutionError> {
    if cancellation.is_canceled() {
        return Err(ExportGpuOutputExecutionError::Canceled);
    }
    let mut encoder =
        backend.context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-export-gpu-output-boundary"),
        });

    let record = backend
        .runtime
        .record_wgpu_output_boundary_owned_backend_with_grant(
            boundary,
            frame,
            frame_contract.gpu_texture_format(),
            RenderColorTransformGpuOptions {
                output_residency: ColorFrameResidency::Cpu,
                ..RenderColorTransformGpuOptions::default()
            },
            active_grant,
            RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                device: &backend.context.device,
                queue: &backend.context.queue,
                encoder: &mut encoder,
                load_op: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
            },
        )
        .map_err(|error| match error {
            RenderGpuOutputBoundaryRuntimeRecordError::ActiveWorkingSet(_) => {
                ExportGpuOutputExecutionError::Fallback(
                    ExportGpuOutputFallbackReason::ActiveWorkingSetRejected,
                )
            }
            _ => ExportGpuOutputExecutionError::Fallback(
                ExportGpuOutputFallbackReason::RecordBoundaryFailed,
            ),
        })?;

    let submission_index = backend.context.queue.submit(std::iter::once(encoder.finish()));
    let readback_buffer = record.readback_buffer.ok_or(ExportGpuOutputExecutionError::Fallback(
        ExportGpuOutputFallbackReason::MissingReadbackBuffer,
    ))?;
    let readback_plan = match frame_contract.gpu_texture_format() {
        GpuColorFrameTextureFormat::Rgba8Unorm => {
            GpuColorFrameReadbackPlan::encoded_rgba8(record.materialized.output)
                .map_err(|_| ExportGpuOutputFallbackReason::ReadbackUnpackFailed)?
        }
        GpuColorFrameTextureFormat::Rgba16Float => {
            GpuColorFrameReadbackPlan::encoded_rgba16float(record.materialized.output)
                .map_err(|_| ExportGpuOutputFallbackReason::ReadbackUnpackFailed)?
        }
        GpuColorFrameTextureFormat::Rgba32Float => {
            GpuColorFrameReadbackPlan::encoded_rgba16float(record.materialized.output)
                .map_err(|_| ExportGpuOutputFallbackReason::ReadbackUnpackFailed)?
        }
    };
    let deadline = Instant::now()
        .checked_add(EXPORT_GPU_READBACK_TIMEOUT)
        .ok_or(ExportGpuOutputExecutionError::DeviceTimedOut)?;
    let mapped = map_readback_buffer_sync(
        &backend.context.device,
        &readback_buffer,
        &submission_index,
        cancellation,
        deadline,
    )?;
    let rgba = match frame_contract.gpu_texture_format() {
        GpuColorFrameTextureFormat::Rgba8Unorm => {
            let actual = readback_plan
                .unpack_mapped_rgba8(&mapped)
                .map_err(|_| ExportGpuOutputFallbackReason::ReadbackUnpackFailed);
            actual.map(|actual| frame_contract.pack_rgba8(actual.rgba()))
        }
        GpuColorFrameTextureFormat::Rgba16Float | GpuColorFrameTextureFormat::Rgba32Float => {
            let f32_data = readback_plan
                .unpack_mapped_rgba16float(&mapped)
                .map_err(|_| ExportGpuOutputFallbackReason::ReadbackUnpackFailed);
            f32_data.map(|f32_data| frame_contract.pack_rgba_f32(&f32_data))
        }
    };
    let rgba = rgba?;
    if cancellation.is_canceled() {
        return Err(ExportGpuOutputExecutionError::Canceled);
    }
    backend.runtime.clear_frame_resources();

    Ok(ExportGpuOutputAttemptOutcome { rgba, stage_diagnostics: record.stage_diagnostics })
}

/// Diagnostics accumulated for one export job.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExportJobDiagnostics {
    /// Color-management diagnostics observed while rendering this job.
    pub color: ExportJobColorDiagnostics,
    /// Visual execution diagnostics observed by the immutable export attempt.
    pub visual: ExportJobVisualDiagnostics,
}

impl ExportJobDiagnostics {
    /// Build the versioned export color report for this job when color evidence exists.
    pub fn color_report(self, profile: impl Into<String>) -> Option<ExportColorHealthReport> {
        self.color.health_report(profile)
    }
}

/// Bounded diagnostic summary for the most recent completed heterogeneous
/// Effect frame. The renderer completion remains the exact token-list
/// authority.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExportHeterogeneousCompletionEvidence {
    /// Complete compiled Effect graph fingerprint.
    pub graph_fingerprint: [u8; 32],
    /// Queue-owned immutable attempt generation.
    pub generation: u64,
    /// Exact Effect input/output width.
    pub width: u32,
    /// Exact Effect input/output height.
    pub height: u32,
    /// Deterministic frame seed bound to the GPU suffix.
    pub frame_seed: i64,
    /// Exact linear working-space identity proved across CPU and GPU execution.
    pub working_color_space: WorkingColorSpace,
    /// Number of CPU-frontier uploads proved complete by the renderer.
    pub completed_upload_count: u32,
    /// Last upload completion token in deterministic transfer order.
    pub last_completed_upload_token: u32,
    /// Final graph-output completion token proved by the renderer.
    pub completed_output_token: u32,
    /// Raw CPU-to-GPU transfer bytes proved by the graph-value plan.
    pub upload_bytes: u64,
    /// Padded GPU-to-CPU readback bytes that completed.
    pub readback_bytes: u64,
}

impl ExportHeterogeneousCompletionEvidence {
    fn from_renderer(
        evidence: &HeterogeneousGpuCompletedEvidence,
        working_color_space: WorkingColorSpace,
    ) -> Option<Self> {
        let recorded = evidence.recorded();
        let completed_uploads = evidence.completed_uploads();
        Some(Self {
            graph_fingerprint: recorded.graph_fingerprint(),
            generation: recorded.generation(),
            width: recorded.frame_extent().width(),
            height: recorded.frame_extent().height(),
            frame_seed: recorded.frame_seed(),
            working_color_space,
            completed_upload_count: u32::try_from(completed_uploads.len()).ok()?,
            last_completed_upload_token: completed_uploads.last()?.pending_gpu_input_token().get(),
            completed_output_token: evidence.completed_output_token().get(),
            upload_bytes: recorded.upload_bytes(),
            readback_bytes: evidence.readback_bytes()?,
        })
    }
}

/// UI-independent visual execution evidence for one Export attempt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExportJobVisualDiagnostics {
    /// Distinct conservative heterogeneous route contracts frozen by preflight.
    pub heterogeneous_route_contracts: u64,
    /// Frames whose heterogeneous attempt crossed into CPU-prefix execution.
    pub heterogeneous_frames_started: u64,
    /// Frames whose upload, exact GPU suffix, wait, and readback all completed.
    pub heterogeneous_frames_completed: u64,
    /// Terminal post-start heterogeneous failures.
    pub heterogeneous_terminal_failures: u64,
    /// Frames retained on a complete CPU route before heterogeneous work started.
    pub cpu_routes_selected_before_start: u64,
    /// Aggregate completed CPU-to-GPU bytes.
    pub heterogeneous_upload_bytes: u64,
    /// Aggregate completed GPU-to-CPU readback bytes.
    pub heterogeneous_readback_bytes: u64,
    /// Most recent bounded completion summary. Exact upload tokens remain in
    /// the renderer-owned completion for the live attempt.
    pub last_heterogeneous_completion: Option<ExportHeterogeneousCompletionEvidence>,
}

/// Export color diagnostics observed on the real render path.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExportJobColorDiagnostics {
    /// Aggregated media color-diagnostic issues carried in the export input.
    pub asset_issue_summary: VideoColorDiagnosticIssueAggregate,
    /// Aggregated input color-resolution branches across rendered frames.
    pub input_resolution_source_counts: InputColorResolutionSourceCounts,
    /// Aggregated color-stage scheduling diagnostics across rendered frames.
    pub stage_diagnostics: RenderColorStageDiagnostics,
    /// Aggregated timeline composite color-path diagnostics across rendered frames.
    pub composite_diagnostics: TimelineCompositeDiagnostics,
    /// Number of timeline video frames that contributed color diagnostics.
    pub diagnosed_frames: u64,
    /// GPU export boundary attempts.
    pub gpu_output_attempts: u64,
    /// GPU export boundary attempts that fell back to CPU output transform.
    pub gpu_output_cpu_fallbacks: u64,
    /// Structured GPU export fallback reasons.
    pub gpu_output_fallback_reasons: ExportGpuOutputFallbackBreakdown,
    /// High-precision export output boundary failures observed while encoding.
    pub output_precision_failures: u64,
    /// Structured high-precision export output boundary failure reasons.
    pub output_precision_failure_reasons: ExportOutputPrecisionFailureBreakdown,
    /// Final export output transform semantic issues observed while encoding.
    pub output_transform_issues: u64,
    /// Structured final export output transform semantic issue reasons.
    pub output_transform_issue_reasons: ExportOutputTransformIssueBreakdown,
}

impl ExportJobColorDiagnostics {
    /// Record one export output boundary execution outcome.
    pub fn record_export_output_boundary(
        &mut self,
        attempts: u64,
        cpu_fallbacks: u64,
        fallback_reasons: ExportGpuOutputFallbackBreakdown,
    ) {
        self.gpu_output_attempts = self.gpu_output_attempts.saturating_add(attempts);
        self.gpu_output_cpu_fallbacks = self.gpu_output_cpu_fallbacks.saturating_add(cpu_fallbacks);
        self.gpu_output_fallback_reasons =
            self.gpu_output_fallback_reasons.accumulate(fallback_reasons);
    }

    /// Record one fail-closed high-precision export output boundary failure.
    pub fn record_output_precision_failure(&mut self, reason: ExportOutputPrecisionFailureReason) {
        self.output_precision_failures = self.output_precision_failures.saturating_add(1);
        self.output_precision_failure_reasons =
            self.output_precision_failure_reasons.add_reason(reason);
    }

    /// Record one semantic issue at the final export output transform.
    pub fn record_output_transform_issue(&mut self, reason: ExportOutputTransformIssueReason) {
        self.output_transform_issues = self.output_transform_issues.saturating_add(1);
        self.output_transform_issue_reasons =
            self.output_transform_issue_reasons.add_reason(reason);
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExportGpuOutputFallbackReason {
    /// GPU context could not be created and color pipeline stayed on CPU.
    ContextUnavailable,
    /// GPU recording failed before command submission.
    RecordBoundaryFailed,
    /// Exact active texture/readback demand exceeded the frozen Export grant.
    ActiveWorkingSetRejected,
    /// GPU output path lacked an explicit readback buffer.
    MissingReadbackBuffer,
    /// GPU output readback map failed.
    ReadbackMapFailed,
    /// GPU output readback exceeded its monotonic bounded wait.
    ReadbackTimedOut,
    /// GPU output readback unpacking failed.
    ReadbackUnpackFailed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExportGpuOutputFallbackBreakdown {
    /// GPU context was unavailable or initialization failed.
    pub context_unavailable: u64,
    /// GPU recording failed before submission.
    pub record_boundary_failed: u64,
    /// Active texture/readback admission rejected the exact output plan.
    pub active_working_set_rejected: u64,
    /// GPU output planned readback buffer missing.
    pub missing_readback_buffer: u64,
    /// Readback map failed.
    pub readback_map_failed: u64,
    /// Readback exceeded its monotonic deadline.
    pub readback_timed_out: u64,
    /// Unpacking readback bytes failed.
    pub readback_unpack_failed: u64,
}

impl ExportGpuOutputFallbackBreakdown {
    /// Return total fallback count across all recorded reasons.
    pub fn total(&self) -> u64 {
        self.context_unavailable
            .saturating_add(self.record_boundary_failed)
            .saturating_add(self.active_working_set_rejected)
            .saturating_add(self.missing_readback_buffer)
            .saturating_add(self.readback_map_failed)
            .saturating_add(self.readback_timed_out)
            .saturating_add(self.readback_unpack_failed)
    }

    /// Merge another breakdown in place.
    pub fn accumulate(self, other: Self) -> Self {
        Self {
            context_unavailable: self.context_unavailable.saturating_add(other.context_unavailable),
            record_boundary_failed: self
                .record_boundary_failed
                .saturating_add(other.record_boundary_failed),
            active_working_set_rejected: self
                .active_working_set_rejected
                .saturating_add(other.active_working_set_rejected),
            missing_readback_buffer: self
                .missing_readback_buffer
                .saturating_add(other.missing_readback_buffer),
            readback_map_failed: self.readback_map_failed.saturating_add(other.readback_map_failed),
            readback_timed_out: self.readback_timed_out.saturating_add(other.readback_timed_out),
            readback_unpack_failed: self
                .readback_unpack_failed
                .saturating_add(other.readback_unpack_failed),
        }
    }

    /// Map one reason into a mut accumulator entry.
    pub fn add_reason(mut self, reason: ExportGpuOutputFallbackReason) -> Self {
        match reason {
            ExportGpuOutputFallbackReason::ContextUnavailable => {
                self.context_unavailable = self.context_unavailable.saturating_add(1)
            }
            ExportGpuOutputFallbackReason::RecordBoundaryFailed => {
                self.record_boundary_failed = self.record_boundary_failed.saturating_add(1)
            }
            ExportGpuOutputFallbackReason::ActiveWorkingSetRejected => {
                self.active_working_set_rejected =
                    self.active_working_set_rejected.saturating_add(1)
            }
            ExportGpuOutputFallbackReason::MissingReadbackBuffer => {
                self.missing_readback_buffer = self.missing_readback_buffer.saturating_add(1)
            }
            ExportGpuOutputFallbackReason::ReadbackMapFailed => {
                self.readback_map_failed = self.readback_map_failed.saturating_add(1)
            }
            ExportGpuOutputFallbackReason::ReadbackTimedOut => {
                self.readback_timed_out = self.readback_timed_out.saturating_add(1)
            }
            ExportGpuOutputFallbackReason::ReadbackUnpackFailed => {
                self.readback_unpack_failed = self.readback_unpack_failed.saturating_add(1)
            }
        }
        self
    }
}

/// Structured reason that a high-precision export output boundary failed closed.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExportOutputPrecisionFailureReason {
    /// GPU output failed and the renderer-owned CPU float boundary was unavailable.
    FloatBoundaryUnavailable,
}

/// Structured high-precision export output boundary failure counts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExportOutputPrecisionFailureBreakdown {
    /// GPU output failed and the renderer-owned CPU float boundary was unavailable.
    pub float_boundary_unavailable: u64,
}

impl ExportOutputPrecisionFailureBreakdown {
    /// Return total failure count across all recorded reasons.
    pub fn total(&self) -> u64 {
        self.float_boundary_unavailable
    }

    /// Merge another breakdown in place.
    pub fn accumulate(self, other: Self) -> Self {
        Self {
            float_boundary_unavailable: self
                .float_boundary_unavailable
                .saturating_add(other.float_boundary_unavailable),
        }
    }

    /// Map one reason into a mut accumulator entry.
    pub fn add_reason(mut self, reason: ExportOutputPrecisionFailureReason) -> Self {
        match reason {
            ExportOutputPrecisionFailureReason::FloatBoundaryUnavailable => {
                self.float_boundary_unavailable = self.float_boundary_unavailable.saturating_add(1)
            }
        }
        self
    }
}

/// Structured reason for final export output transform semantic issues.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExportOutputTransformIssueReason {
    /// Tone mapping was requested but the export output boundary did not carry an OCIO view transform.
    ToneMapRequestedWithoutExportViewTransform,
}

/// Structured final export output transform semantic issue counts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExportOutputTransformIssueBreakdown {
    /// Tone mapping was requested without an export view/display-view transform.
    pub tone_map_requested_without_export_view_transform: u64,
}

impl ExportOutputTransformIssueBreakdown {
    /// Return total issue count across all recorded reasons.
    pub fn total(&self) -> u64 {
        self.tone_map_requested_without_export_view_transform
    }

    /// Merge another breakdown in place.
    pub fn accumulate(self, other: Self) -> Self {
        Self {
            tone_map_requested_without_export_view_transform: self
                .tone_map_requested_without_export_view_transform
                .saturating_add(other.tone_map_requested_without_export_view_transform),
        }
    }

    /// Map one reason into a mut accumulator entry.
    pub fn add_reason(mut self, reason: ExportOutputTransformIssueReason) -> Self {
        match reason {
            ExportOutputTransformIssueReason::ToneMapRequestedWithoutExportViewTransform => {
                self.tone_map_requested_without_export_view_transform =
                    self.tone_map_requested_without_export_view_transform.saturating_add(1)
            }
        }
        self
    }
}

/// Stable summary of export color-path diagnostics for UI, telemetry, and reports.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExportJobColorDiagnosticsSummary {
    /// Aggregated media color-diagnostic issues carried in the export input.
    pub asset_issue_summary: VideoColorDiagnosticIssueAggregate,
    /// Timeline video frames that contributed color diagnostics.
    pub diagnosed_frames: u64,
    /// Inputs resolved from detected media metadata.
    pub detected_metadata: u64,
    /// Inputs resolved from user overrides.
    pub override_count: u64,
    /// Inputs resolved by missing-metadata policy assumptions.
    pub policy_assumptions: u64,
    /// Inputs bypassing color management as data/utility textures.
    pub data_textures: u64,
    /// Inputs rejected by missing-metadata policy.
    pub policy_rejections: u64,
    /// Inputs resolved from explicit metadata or user overrides.
    pub explicit_metadata_or_override: u64,
    /// CPU input color-transform stages.
    pub cpu_input_stages: u64,
    /// CPU output/display/export color-transform stages.
    pub cpu_output_stages: u64,
    /// Native GPU color-transform stages.
    pub gpu_color_stages: u64,
    /// GPU scheduling blockers across native GPU color stages.
    pub gpu_blockers: u64,
    /// Structured native GPU blocker reasons.
    pub gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown,
    /// Upload/readback transfer stages around color work.
    pub transfer_stages: u64,
    /// Export output attempts through GPU final-output boundary recording.
    pub gpu_output_attempts: u64,
    /// GPU output attempts that fell back to CPU output transform.
    pub gpu_output_cpu_fallbacks: u64,
    /// Structured GPU output fallback reasons for export output boundary.
    pub gpu_output_fallback_reasons: ExportGpuOutputFallbackBreakdown,
    /// Fail-closed high-precision export output boundary failures.
    pub output_precision_failures: u64,
    /// Structured high-precision export output boundary failure reasons.
    pub output_precision_failure_reasons: ExportOutputPrecisionFailureBreakdown,
    /// Final export output transform semantic issues.
    pub output_transform_issues: u64,
    /// Structured final export output transform semantic issue reasons.
    pub output_transform_issue_reasons: ExportOutputTransformIssueBreakdown,
    /// Float/linear timeline composites.
    pub float_linear_composites: u64,
    /// Legacy RGBA8 timeline composites.
    pub legacy_rgba8_composites: u64,
    /// Structured legacy RGBA8 fallback reason count.
    pub legacy_reason_total: u64,
    /// Structured legacy RGBA8 fallback reasons.
    pub legacy_breakdown: TimelineCompositeLegacyBreakdown,
    /// Composite plans blocked on unresolved effect-domain semantics.
    pub blocked_color_domain_composites: u64,
    /// Structured unresolved effect-domain reasons.
    pub domain_blockers: TimelineCompositeDomainBlockerBreakdown,
    /// Whether all diagnosed composites stayed in the float/linear path.
    pub fully_float_linear: bool,
    /// Whether native GPU color scheduling was free of upload/readback and blockers.
    pub gpu_path_ready: bool,
}

/// Schema version for export color health reports.
pub const EXPORT_COLOR_HEALTH_REPORT_SCHEMA_VERSION: u32 = 6;

/// Versioned export color health report for UI, telemetry, perf, and job artifacts.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExportColorHealthReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Applied report profile.
    pub profile: String,
    /// Overall export color health verdict.
    pub verdict: ExportColorHealthVerdict,
    /// Stable export color diagnostics summary used as report evidence.
    pub summary: ExportJobColorDiagnosticsSummary,
    /// Structured checks by export color-pipeline area.
    pub checks: Vec<ExportColorHealthCheck>,
    /// Prioritized machine-readable root causes.
    pub root_causes: Vec<ExportColorHealthRootCause>,
    /// Suggested engineering or operator actions.
    pub actions: Vec<ExportColorHealthAction>,
}

/// Overall export color health verdict.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum ExportColorHealthVerdict {
    /// Export color path met all fail-closed checks.
    Pass,
    /// Export color path passed hard checks but has warning evidence.
    Warn,
    /// Export color path violated a fail-closed check.
    Fail,
}

/// Export color diagnostic area.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum ExportColorHealthArea {
    /// Evidence capture and frame coverage.
    CaptureIntegrity,
    /// Input media metadata and policy handling.
    InputColorPolicy,
    /// Renderer color-stage scheduling.
    StageScheduling,
    /// Timeline compositing precision and legacy paths.
    CompositePath,
}

/// Export color health check severity.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum ExportColorHealthSeverity {
    /// Check passed.
    Pass,
    /// Check produced warning evidence.
    Warn,
    /// Check failed.
    Fail,
}

/// One export color health check.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExportColorHealthCheck {
    /// Diagnostic area for this check.
    pub area: ExportColorHealthArea,
    /// Stable check code.
    pub code: &'static str,
    /// Check severity.
    pub severity: ExportColorHealthSeverity,
    /// Observed value.
    pub observed: u64,
    /// Optional target or threshold.
    pub limit: Option<u64>,
}

/// One export color health root cause.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExportColorHealthRootCause {
    /// Diagnostic area for this root cause.
    pub area: ExportColorHealthArea,
    /// Stable root-cause code.
    pub code: &'static str,
    /// Root-cause severity.
    pub severity: ExportColorHealthSeverity,
    /// Compact evidence string.
    pub evidence: String,
}

/// One export color health action.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExportColorHealthAction {
    /// Diagnostic area for this action.
    pub area: ExportColorHealthArea,
    /// Stable action code.
    pub code: &'static str,
    /// Human-readable action.
    pub description: &'static str,
}

impl ExportJobColorDiagnosticsSummary {
    /// Build the versioned export color health report for this summary.
    pub fn health_report(self, profile: impl Into<String>) -> ExportColorHealthReport {
        let mut checks = Vec::new();
        let mut root_causes = Vec::new();
        let mut actions = Vec::new();

        push_export_min_check(
            &mut checks,
            ExportColorHealthArea::CaptureIntegrity,
            "diagnosed_frames_present",
            self.diagnosed_frames,
            1,
        );
        push_export_bool_check(
            &mut checks,
            ExportColorHealthArea::CompositePath,
            color_report_vocab::check::FULLY_FLOAT_LINEAR,
            self.fully_float_linear,
        );
        push_export_bool_check(
            &mut checks,
            ExportColorHealthArea::StageScheduling,
            color_report_vocab::check::GPU_PATH_READY,
            self.gpu_path_ready,
        );
        push_export_max_check(
            &mut checks,
            ExportColorHealthArea::StageScheduling,
            color_report_vocab::check::GPU_BLOCKERS,
            self.gpu_blockers,
            0,
        );
        push_export_max_check(
            &mut checks,
            ExportColorHealthArea::StageScheduling,
            color_report_vocab::check::TRANSFER_STAGES,
            self.transfer_stages,
            0,
        );
        push_export_max_check(
            &mut checks,
            ExportColorHealthArea::StageScheduling,
            "export_gpu_output_cpu_fallbacks",
            self.gpu_output_cpu_fallbacks,
            0,
        );
        push_export_max_check(
            &mut checks,
            ExportColorHealthArea::CompositePath,
            "export_output_precision_failures",
            self.output_precision_failures,
            0,
        );
        push_export_max_check(
            &mut checks,
            ExportColorHealthArea::CompositePath,
            "export_output_transform_issues",
            self.output_transform_issues,
            0,
        );
        push_export_max_check(
            &mut checks,
            ExportColorHealthArea::CompositePath,
            color_report_vocab::check::LEGACY_REASON_TOTAL,
            self.legacy_reason_total,
            0,
        );
        push_export_max_check(
            &mut checks,
            ExportColorHealthArea::CompositePath,
            color_report_vocab::check::EFFECT_DOMAIN_BLOCKERS,
            self.domain_blockers.total().max(self.blocked_color_domain_composites),
            0,
        );
        push_export_max_check(
            &mut checks,
            ExportColorHealthArea::InputColorPolicy,
            color_report_vocab::check::POLICY_REJECTIONS,
            self.policy_rejections,
            0,
        );
        let warning_count = self.asset_issue_summary.diagnostics_with_warnings;
        checks.push(ExportColorHealthCheck {
            area: ExportColorHealthArea::InputColorPolicy,
            code: "media_warnings",
            severity: if warning_count > 0 {
                ExportColorHealthSeverity::Warn
            } else {
                ExportColorHealthSeverity::Pass
            },
            observed: warning_count,
            limit: Some(0),
        });
        let dynamic_hdr_sources = self
            .asset_issue_summary
            .diagnostics_with_dynamic_hdr10_plus
            .saturating_add(self.asset_issue_summary.diagnostics_with_dolby_vision_config);
        checks.push(ExportColorHealthCheck {
            area: ExportColorHealthArea::InputColorPolicy,
            code: "dynamic_hdr_metadata_sources",
            severity: if dynamic_hdr_sources > 0 {
                ExportColorHealthSeverity::Warn
            } else {
                ExportColorHealthSeverity::Pass
            },
            observed: dynamic_hdr_sources,
            limit: Some(0),
        });

        push_export_root_causes_and_actions(self, &mut root_causes, &mut actions);

        let has_failures =
            checks.iter().any(|check| check.severity == ExportColorHealthSeverity::Fail);
        let has_warnings =
            checks.iter().any(|check| check.severity == ExportColorHealthSeverity::Warn);
        let verdict = if has_failures {
            ExportColorHealthVerdict::Fail
        } else if has_warnings {
            ExportColorHealthVerdict::Warn
        } else {
            ExportColorHealthVerdict::Pass
        };

        ExportColorHealthReport {
            schema_version: EXPORT_COLOR_HEALTH_REPORT_SCHEMA_VERSION,
            profile: profile.into(),
            verdict,
            summary: self,
            checks,
            root_causes,
            actions,
        }
    }
}

fn push_export_min_check(
    checks: &mut Vec<ExportColorHealthCheck>,
    area: ExportColorHealthArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(ExportColorHealthCheck {
        area,
        code,
        severity: if observed < limit {
            ExportColorHealthSeverity::Fail
        } else {
            ExportColorHealthSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_export_max_check(
    checks: &mut Vec<ExportColorHealthCheck>,
    area: ExportColorHealthArea,
    code: &'static str,
    observed: u64,
    limit: u64,
) {
    checks.push(ExportColorHealthCheck {
        area,
        code,
        severity: if observed > limit {
            ExportColorHealthSeverity::Fail
        } else {
            ExportColorHealthSeverity::Pass
        },
        observed,
        limit: Some(limit),
    });
}

fn push_export_bool_check(
    checks: &mut Vec<ExportColorHealthCheck>,
    area: ExportColorHealthArea,
    code: &'static str,
    passed: bool,
) {
    checks.push(ExportColorHealthCheck {
        area,
        code,
        severity: if passed {
            ExportColorHealthSeverity::Pass
        } else {
            ExportColorHealthSeverity::Fail
        },
        observed: if passed { 1 } else { 0 },
        limit: Some(1),
    });
}

fn push_export_root_causes_and_actions(
    summary: ExportJobColorDiagnosticsSummary,
    root_causes: &mut Vec<ExportColorHealthRootCause>,
    actions: &mut Vec<ExportColorHealthAction>,
) {
    if summary.diagnosed_frames == 0 {
        push_export_root_cause_with_action(
            root_causes,
            actions,
            ExportColorHealthArea::CaptureIntegrity,
            "missing_export_color_evidence",
            ExportColorHealthSeverity::Fail,
            "diagnosed_frames=0".to_owned(),
            "inspect_export_render_path",
            "Ensure export jobs record frame color diagnostics from the real render path.",
        );
    }
    if summary.asset_issue_summary.diagnostics_with_warnings > 0 {
        push_export_root_cause_with_action(
            root_causes,
            actions,
            ExportColorHealthArea::InputColorPolicy,
            "asset_color_diagnostics_warning",
            ExportColorHealthSeverity::Warn,
            format!(
                "diagnostics_with_warnings={}",
                summary.asset_issue_summary.diagnostics_with_warnings
            ),
            "inspect_asset_color_warning_evidence",
            "Inspect source media color diagnostic warnings before trusting export color policy.",
        );
    }
    if summary.asset_issue_summary.diagnostics_with_dynamic_hdr10_plus > 0
        || summary.asset_issue_summary.diagnostics_with_dolby_vision_config > 0
    {
        push_export_root_cause_with_action(
            root_causes,
            actions,
            ExportColorHealthArea::InputColorPolicy,
            "dynamic_hdr_metadata_not_preserved",
            ExportColorHealthSeverity::Warn,
            format!(
                "hdr10_plus_sources={} dolby_vision_sources={}",
                summary
                    .asset_issue_summary
                    .diagnostics_with_dynamic_hdr10_plus,
                summary
                    .asset_issue_summary
                    .diagnostics_with_dolby_vision_config
            ),
            "use_validated_dynamic_hdr_authoring",
            "Rendered export strips source HDR10+/Dolby Vision metadata; use a validated dynamic-HDR authoring workflow for dynamic delivery.",
        );
    }
    if summary.policy_rejections > 0 {
        push_export_root_cause_with_action(
            root_causes,
            actions,
            ExportColorHealthArea::InputColorPolicy,
            color_report_vocab::root_cause::INPUT_COLOR_POLICY_REJECTED_SOURCE,
            ExportColorHealthSeverity::Fail,
            format!("policy_rejections={}", summary.policy_rejections),
            color_report_vocab::action::INSPECT_ASSET_COLOR_DIAGNOSTICS,
            "Inspect per-asset color diagnostics and missing-metadata policy before export.",
        );
    }
    if summary.gpu_blockers > 0 {
        push_export_root_cause_with_action(
            root_causes,
            actions,
            ExportColorHealthArea::StageScheduling,
            "export_gpu_color_stage_blocked",
            ExportColorHealthSeverity::Fail,
            format!(
                "gpu_blockers={} shader={} ocio_resource={} wrapper={} pipeline={} \
                 ocio_config={} ocio_processor={} shader_extraction={}",
                summary.gpu_blockers,
                summary.gpu_blocker_breakdown.shader_module_not_prepared,
                summary.gpu_blocker_breakdown.ocio_resource_bind_group_not_prepared,
                summary.gpu_blocker_breakdown.fullscreen_wrapper_not_prepared,
                summary.gpu_blocker_breakdown.render_pipeline_not_prepared,
                summary.gpu_blocker_breakdown.ocio_config_not_loaded,
                summary.gpu_blocker_breakdown.ocio_processor_unavailable,
                summary.gpu_blocker_breakdown.ocio_gpu_shader_extraction_failed
            ),
            color_report_vocab::action::INSPECT_GPU_BLOCKERS,
            "Inspect renderer GPU color blocker breakdown before relying on export GPU scheduling.",
        );
    }
    if summary.gpu_output_cpu_fallbacks > 0 || summary.gpu_output_fallback_reasons.total() > 0 {
        push_export_root_cause_with_action(
            root_causes,
            actions,
            ExportColorHealthArea::StageScheduling,
            "export_gpu_output_fallback",
            ExportColorHealthSeverity::Fail,
            format!(
                "gpu_output_attempts={} cpu_fallbacks={} context_unavailable={} record_failed={} active_working_set_rejected={} missing_readback_buffer={} readback_map_failed={} readback_timed_out={} readback_unpack_failed={}",
                summary.gpu_output_attempts,
                summary.gpu_output_cpu_fallbacks,
                summary.gpu_output_fallback_reasons.context_unavailable,
                summary.gpu_output_fallback_reasons.record_boundary_failed,
                summary.gpu_output_fallback_reasons.active_working_set_rejected,
                summary.gpu_output_fallback_reasons.missing_readback_buffer,
                summary.gpu_output_fallback_reasons.readback_map_failed,
                summary.gpu_output_fallback_reasons.readback_timed_out,
                summary.gpu_output_fallback_reasons.readback_unpack_failed
            ),
            "inspect_export_gpu_output_fallback",
            "Trace export GPU output attempts and keep CPU fallback reasons explicit.",
        );
    }
    if summary.output_precision_failures > 0 || summary.output_precision_failure_reasons.total() > 0
    {
        push_export_root_cause_with_action(
            root_causes,
            actions,
            ExportColorHealthArea::CompositePath,
            "export_output_precision_failure",
            ExportColorHealthSeverity::Fail,
            format!(
                "output_precision_failures={} float_boundary_unavailable={}",
                summary.output_precision_failures,
                summary
                    .output_precision_failure_reasons
                    .float_boundary_unavailable
            ),
            "inspect_export_float_output_boundary",
            "Inspect why both the GPU output path and renderer-owned CPU float output boundary failed.",
        );
    }
    if summary.output_transform_issues > 0 || summary.output_transform_issue_reasons.total() > 0 {
        push_export_root_cause_with_action(
            root_causes,
            actions,
            ExportColorHealthArea::CompositePath,
            "export_output_transform_issue",
            ExportColorHealthSeverity::Fail,
            format!(
                "output_transform_issues={} tone_map_requested_without_export_view_transform={}",
                summary.output_transform_issues,
                summary
                    .output_transform_issue_reasons
                    .tone_map_requested_without_export_view_transform,
            ),
            "inspect_export_output_intent",
            "Inspect why the engine-owned output intent did not resolve an OCIO View.",
        );
    }
    if summary.legacy_rgba8_composites > 0 || summary.legacy_reason_total > 0 {
        push_export_root_cause_with_action(
            root_causes,
            actions,
            ExportColorHealthArea::CompositePath,
            color_report_vocab::root_cause::LEGACY_RGBA8_COMPOSITE_PATH,
            ExportColorHealthSeverity::Fail,
            format!(
                "fully_float_linear={} legacy_reason_total={}",
                summary.fully_float_linear, summary.legacy_reason_total
            ),
            color_report_vocab::action::MIGRATE_LEGACY_COMPOSITE_REASON,
            "Use structured legacy RGBA8 reasons to migrate export composites back to float/linear.",
        );
    }
    if summary.blocked_color_domain_composites > 0 || !summary.domain_blockers.is_empty() {
        push_export_root_cause_with_action(
            root_causes,
            actions,
            ExportColorHealthArea::CompositePath,
            color_report_vocab::root_cause::EFFECT_DOMAIN_UNRESOLVED,
            ExportColorHealthSeverity::Fail,
            format!(
                "blocked_composites={} media={} solid={} adjustment={}",
                summary.blocked_color_domain_composites,
                summary.domain_blockers.media_effect,
                summary.domain_blockers.solid_effect,
                summary.domain_blockers.adjustment_effect
            ),
            color_report_vocab::action::RESOLVE_EFFECT_DOMAIN_TRANSITIONS,
            "Resolve every effect-domain edge through the renderer OCIO planner; never run it as scene-linear or RGBA8.",
        );
    }
}

fn push_export_root_cause_with_action(
    root_causes: &mut Vec<ExportColorHealthRootCause>,
    actions: &mut Vec<ExportColorHealthAction>,
    area: ExportColorHealthArea,
    root_code: &'static str,
    severity: ExportColorHealthSeverity,
    evidence: String,
    action_code: &'static str,
    action_description: &'static str,
) {
    if !root_causes.iter().any(|root| root.code == root_code) {
        root_causes.push(ExportColorHealthRootCause { area, code: root_code, severity, evidence });
    }
    if !actions.iter().any(|action| action.code == action_code) {
        actions.push(ExportColorHealthAction {
            area,
            code: action_code,
            description: action_description,
        });
    }
}

impl ExportJobColorDiagnostics {
    /// Record the aggregated media color-diagnostic issues for this export job.
    pub fn record_asset_issue_summary(&mut self, summary: VideoColorDiagnosticIssueAggregate) {
        self.asset_issue_summary = summary;
    }

    /// Record color-resolution counts observed while rendering one frame.
    pub fn record_input_resolution_counts(&mut self, counts: InputColorResolutionSourceCounts) {
        self.input_resolution_source_counts.accumulate(counts);
        self.diagnosed_frames = self.diagnosed_frames.saturating_add(1);
    }

    /// Record all color diagnostics observed while rendering one frame.
    pub fn record_frame_diagnostics(
        &mut self,
        input_counts: InputColorResolutionSourceCounts,
        stage_diagnostics: RenderColorStageDiagnostics,
        composite_diagnostics: TimelineCompositeDiagnostics,
    ) {
        self.input_resolution_source_counts.accumulate(input_counts);
        self.stage_diagnostics.accumulate(stage_diagnostics);
        self.composite_diagnostics.accumulate(composite_diagnostics);
        self.diagnosed_frames = self.diagnosed_frames.saturating_add(1);
    }

    /// Return the renderer-owned composite color-path summary for this export job.
    pub fn composite_color_path_summary(self) -> TimelineCompositeColorPathSummary {
        self.composite_diagnostics.color_path_summary()
    }

    /// Build the versioned export color report when this diagnostic set has color evidence.
    pub fn health_report(self, profile: impl Into<String>) -> Option<ExportColorHealthReport> {
        self.summary().map(|summary| summary.health_report(profile))
    }

    /// Return a stable export color diagnostics summary when this job has color evidence.
    pub fn summary(self) -> Option<ExportJobColorDiagnosticsSummary> {
        let counts = self.input_resolution_source_counts;
        let stages = self.stage_diagnostics;
        let composite = self.composite_color_path_summary();
        if self.asset_issue_summary.diagnostics == 0
            && self.diagnosed_frames == 0
            && counts.total() == 0
            && stages.total_stages == 0
            && composite.composite_plans() == 0
            && self.output_precision_failures == 0
            && self.output_precision_failure_reasons.total() == 0
            && self.output_transform_issues == 0
            && self.output_transform_issue_reasons.total() == 0
        {
            return None;
        }
        Some(ExportJobColorDiagnosticsSummary {
            asset_issue_summary: self.asset_issue_summary,
            diagnosed_frames: self.diagnosed_frames,
            detected_metadata: counts.detected_metadata,
            override_count: counts.override_count,
            policy_assumptions: counts.policy_assumptions(),
            data_textures: counts.data_textures(),
            policy_rejections: counts.policy_rejections(),
            explicit_metadata_or_override: counts.explicit_metadata_or_override(),
            cpu_input_stages: stages.cpu_input_stages,
            cpu_output_stages: stages.cpu_output_stages,
            gpu_color_stages: stages.gpu_color_stages,
            gpu_blockers: stages.gpu_blockers,
            gpu_blocker_breakdown: stages.gpu_blocker_breakdown,
            transfer_stages: stages.upload_stages.saturating_add(stages.readback_stages),
            gpu_output_attempts: self.gpu_output_attempts,
            gpu_output_cpu_fallbacks: self.gpu_output_cpu_fallbacks,
            gpu_output_fallback_reasons: self.gpu_output_fallback_reasons,
            output_precision_failures: self.output_precision_failures,
            output_precision_failure_reasons: self.output_precision_failure_reasons,
            output_transform_issues: self.output_transform_issues,
            output_transform_issue_reasons: self.output_transform_issue_reasons,
            float_linear_composites: composite.float_linear_composites,
            legacy_rgba8_composites: composite.legacy_rgba8_composites,
            legacy_reason_total: composite.legacy_breakdown.total(),
            legacy_breakdown: composite.legacy_breakdown,
            blocked_color_domain_composites: composite.blocked_composites,
            domain_blockers: composite.domain_blockers,
            fully_float_linear: composite.is_fully_float_linear(),
            gpu_path_ready: {
                if self.gpu_output_attempts == 0 && self.gpu_output_cpu_fallbacks == 0 {
                    self.output_precision_failures == 0
                        && self.output_precision_failure_reasons.total() == 0
                        && self.output_transform_issues == 0
                        && self.output_transform_issue_reasons.total() == 0
                } else {
                    self.gpu_output_cpu_fallbacks == 0
                        && self.gpu_output_fallback_reasons.total() == 0
                        && self.output_precision_failures == 0
                        && self.output_precision_failure_reasons.total() == 0
                        && self.output_transform_issues == 0
                        && self.output_transform_issue_reasons.total() == 0
                        && stages.gpu_blockers == 0
                }
            },
        })
    }
}

#[derive(Debug)]
pub(crate) struct DurableExportPublication {
    output_path: PathBuf,
}

impl DurableExportPublication {
    fn from_storage(evidence: FilePublicationEvidence) -> Self {
        Self {
            output_path: evidence.published_path().to_path_buf(),
        }
    }

    #[cfg(test)]
    fn synthetic(output_path: &Path) -> Self {
        Self {
            output_path: std::path::absolute(output_path)
                .unwrap_or_else(|_| output_path.to_path_buf()),
        }
    }

    fn into_terminal_evidence(self) -> ExportArtifactPublicationEvidence {
        ExportArtifactPublicationEvidence::Durable { output_path: self.output_path }
    }
}

#[derive(Debug)]
pub(crate) enum ExportPublicationFailure {
    BeforeNamespace {
        output_path: PathBuf,
        retained_partial_path: Option<PathBuf>,
        detail: String,
    },
    DurabilityUnconfirmed {
        output_path: PathBuf,
        detail: String,
    },
    NamespaceIndeterminate {
        output_path: PathBuf,
        retained_partial_path: Option<PathBuf>,
        detail: String,
    },
}

impl ExportPublicationFailure {
    fn from_storage(
        failure: FilePublicationFailure,
        output_path: &Path,
        retained_partial_path: Option<PathBuf>,
    ) -> Self {
        match failure {
            FilePublicationFailure::BeforeNamespace(error) => Self::BeforeNamespace {
                output_path: output_path.to_path_buf(),
                retained_partial_path,
                detail: format!("{error:#}"),
            },
            FilePublicationFailure::DurabilityUnconfirmed(error) => Self::DurabilityUnconfirmed {
                output_path: error.published_path().to_path_buf(),
                detail: error.to_string(),
            },
            FilePublicationFailure::NamespaceIndeterminate(error) => Self::NamespaceIndeterminate {
                output_path: error.intended_path().to_path_buf(),
                retained_partial_path: error
                    .retained_new_path()
                    .map(Path::to_path_buf)
                    .or(retained_partial_path),
                detail: error.to_string(),
            },
        }
    }
}

#[derive(Debug)]
pub(crate) enum JobExecutionResult {
    /// A reversible preparation/render/encode sub-operation succeeded.
    ///
    /// This is never a terminal queue outcome and cannot authorize a
    /// deliverable or `Completed` job state.
    ReversibleWorkCompleted,
    /// The validated deliverable was durably published. The contained
    /// evidence is the only authority for a successful terminal state.
    Published(DurableExportPublication),
    /// Publication reached one typed non-success terminal result.
    PublicationFailed(ExportPublicationFailure),
    /// Execution ended without publishing a new deliverable.
    Failed(String),
    /// Cancellation was observed before the irreversible publication point.
    Cancelled,
}

/// Queue-internal execution adapter.
///
/// Implementations must visit the supplied execution Gate at every declared
/// safe frame, audio-block, and phase boundary. After the Gate admits
/// `Publishing`, cancellation must no longer change the terminal disposition
/// from the result of irreversible publication.
pub(crate) trait ExportExecutor: Send + Sync + 'static {
    fn execute(
        &self,
        job: &RenderJob,
        cancel: &ExecutionCancellationToken,
        execution_gate: &service::ExportExecutionGate,
        report: &mut dyn FnMut(ExportProgress),
        report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
    ) -> JobExecutionResult;
}

#[derive(Default)]
pub struct FfmpegExportExecutor;

impl ExportExecutor for FfmpegExportExecutor {
    fn execute(
        &self,
        job: &RenderJob,
        cancel: &ExecutionCancellationToken,
        execution_gate: &service::ExportExecutionGate,
        report: &mut dyn FnMut(ExportProgress),
        report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
    ) -> JobExecutionResult {
        if !execution_gate.wait_at_boundary(ExportProgressPhase::Preparing, cancel) {
            return JobExecutionResult::Cancelled;
        }
        report(ExportProgress::preparing(0.01));

        let final_output = job.config.output_path.as_path();
        let staging = match OwnedPublicationFile::create_sibling(
            final_output,
            &format!("export-{}", job.id()),
        ) {
            Ok(staging) => staging,
            Err(error) => {
                return JobExecutionResult::Failed(format!(
                    "failed to reserve an exact sibling export object for {}: {error:#}",
                    final_output.display()
                ));
            }
        };
        let partial_output = staging.path().to_path_buf();
        let reservation = staging.release_for_external_writer();
        let mut validation_expectations = None;
        let outcome = execute_timeline_export(
            job,
            &job.config.timeline,
            partial_output.as_path(),
            &mut validation_expectations,
            cancel,
            execution_gate,
            report,
            report_diagnostics,
        );
        if !matches!(outcome, JobExecutionResult::ReversibleWorkCompleted) {
            return outcome;
        }
        if cancel.is_canceled() {
            return JobExecutionResult::Cancelled;
        }
        let staging = match reservation.reclaim() {
            Ok(staging) => staging,
            Err(error) => {
                return JobExecutionResult::Failed(format!(
                    "encoded export no longer names its reserved partial object {}: {error:#}",
                    partial_output.display()
                ));
            }
        };
        let Some(validation_expectations) = validation_expectations else {
            return JobExecutionResult::Failed(
                "encoded export completed without a validation contract".to_owned(),
            );
        };
        if !execution_gate.wait_at_boundary(ExportProgressPhase::Validating, cancel) {
            return JobExecutionResult::Cancelled;
        }
        report(ExportProgress::validating(0.99));
        match validate_export_output_cancellable(staging.path(), &validation_expectations, cancel) {
            Ok(_) => {}
            Err(_) if cancel.is_canceled() => return JobExecutionResult::Cancelled,
            Err(error) => {
                return JobExecutionResult::Failed(format!("导出结果校验失败: {error}"));
            }
        }
        if let Err(reason) = validate_snapshot_media_revisions(&job.config.timeline) {
            return JobExecutionResult::Failed(reason);
        }
        if !execution_gate.wait_at_boundary(ExportProgressPhase::Publishing, cancel) {
            return JobExecutionResult::Cancelled;
        }
        report(ExportProgress::publishing(0.995));
        match finalize_export_output(staging, final_output, job.config.output_policy) {
            Ok(evidence) => JobExecutionResult::Published(evidence),
            Err(failure) => JobExecutionResult::PublicationFailed(failure),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TimelineRenderRange {
    start_frame: i64,
    total_frames: u64,
    fps_num: i64,
    fps_den: i64,
}

impl TimelineRenderRange {
    fn resolved(self) -> ResolvedTimelineExportRange {
        ResolvedTimelineExportRange {
            start_frame: self.start_frame,
            total_frames: self.total_frames,
            fps_num: self.fps_num,
            fps_den: self.fps_den,
        }
    }

    fn time_range(self) -> Result<TimelineTimeRange, String> {
        self.resolved().time_range().map_err(|error| error.to_string())
    }
}

enum TimelineAudioInput {
    PcmFile {
        path: PathBuf,
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
    },
    Silent {
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
    },
    Disabled,
}

#[derive(Debug, Clone)]
struct DecodedVideoLayer {
    frame: CpuColorFrame,
    source_resolution: Resolution,
    source_fingerprint: MediaFileFingerprint,
    video_stream_index: u32,
    #[cfg_attr(not(test), allow(dead_code))]
    decode_diagnostics: Option<PreviewDecodeDiagnostics>,
    stage_diagnostics: RenderColorStageDiagnostics,
}

fn execute_timeline_export(
    job: &RenderJob,
    timeline: &TimelineExportSnapshot,
    output_path: &Path,
    validation_expectations_out: &mut Option<ExportValidationExpectations>,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    report: &mut dyn FnMut(ExportProgress),
    report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
) -> JobExecutionResult {
    let mut temp_audio_path_to_cleanup: Option<PathBuf> = None;
    let result = (|| {
        if !execution_gate.wait_at_boundary(ExportProgressPhase::Preparing, cancel) {
            return JobExecutionResult::Cancelled;
        }
        let resource_policy = execution_gate.resource_policy();

        if let Err(reason) = validate_snapshot_media_revisions(timeline) {
            return JobExecutionResult::Failed(reason);
        }
        let range = match compute_timeline_render_range(timeline) {
            Ok(range) => range,
            Err(error) => return JobExecutionResult::Failed(error),
        };
        if range.total_frames == 0 {
            return JobExecutionResult::Failed("时间线导出范围为空".to_string());
        }
        let delivery = match crate::delivery::resolve_export_delivery(
            &job.config.preset,
            &timeline.sequence.settings,
            &timeline.color_environment,
        ) {
            Ok(delivery) => delivery,
            Err(error) => return JobExecutionResult::Failed(error.to_string()),
        };

        let Some(prepared_visual) =
            timeline.prepared_execution().map(|execution| execution.visual())
        else {
            return JobExecutionResult::Failed(
                "immutable export visual execution snapshot was not admitted".to_owned(),
            );
        };
        let mut visual_session = match ExportVisualRenderSession::for_execution_generation(
            execution_gate.attempt_generation(),
            resource_policy,
            prepared_visual,
        ) {
            Ok(session) => session,
            Err(error) => return JobExecutionResult::Failed(error),
        };
        if let Err(outcome) = preflight_timeline_visual_range_at_resolution(
            timeline,
            range,
            Resolution {
                width: delivery.resolution.width,
                height: delivery.resolution.height,
            },
            resolved_export_color_context(timeline, &delivery),
            cancel,
            execution_gate,
            &mut visual_session,
        ) {
            return outcome;
        }
        let media_diagnostics = match export_media_diagnostic_set(timeline) {
            Ok(diagnostics) => diagnostics,
            Err(error) => return JobExecutionResult::Failed(error),
        };
        if let Err(error) =
            validate_timeline_dynamic_hdr_delivery(timeline, media_diagnostics.issue_summary)
        {
            return JobExecutionResult::Failed(error);
        }

        let audio_input =
            prepare_timeline_audio_input(job, timeline, range, cancel, execution_gate, report);
        let audio_input = match audio_input {
            Ok(input) => input,
            Err(outcome) => return outcome,
        };
        if let TimelineAudioInput::PcmFile { path, .. } = &audio_input {
            temp_audio_path_to_cleanup = Some(path.clone());
        }

        let (width, height) = (delivery.resolution.width, delivery.resolution.height);
        let expected_video_signal =
            match expected_export_video_signal(&timeline.sequence.settings, &delivery) {
                Ok(signal) => signal,
                Err(error) => return JobExecutionResult::Failed(error),
            };
        let expected_audio = match &audio_input {
            TimelineAudioInput::Disabled => None,
            TimelineAudioInput::PcmFile { sample_rate, channel_layout, .. }
            | TimelineAudioInput::Silent { sample_rate, channel_layout } => {
                match expected_audio_constraints(
                    &job.config.preset.audio,
                    *sample_rate,
                    *channel_layout,
                ) {
                    Some(expected) => Some(expected),
                    None => {
                        return JobExecutionResult::Failed(
                            "已启用的音频输出没有可证明的编码合同".to_string(),
                        );
                    }
                }
            }
        };
        let validation_expectations = ExportValidationExpectations {
            container: job.config.preset.container,
            video: ExpectedStream::Required(ExpectedVideoConstraints {
                encoding: Some(expected_video_encoding(&job.config.preset.video)),
                bit_depth: Some(delivery_bit_depth_value(delivery.bit_depth)),
                width: Some(width),
                height: Some(height),
                fps_num: Some(range.fps_num),
                fps_den: Some(range.fps_den),
                signal: Some(expected_video_signal),
            }),
            audio: expected_audio
                .map(ExpectedStream::Required)
                .unwrap_or(ExpectedStream::Forbidden),
            expected_duration_secs: Some(
                range.total_frames as f64 * range.fps_den as f64 / range.fps_num.max(1) as f64,
            ),
        };
        let mut cmd = mondrian_media::ffmpeg_command();
        let frame_contract = export_frame_contract(delivery.bit_depth);
        let pix_fmt = frame_contract.ffmpeg_pix_fmt();
        cmd.arg("-y")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-f")
            .arg("rawvideo")
            .arg("-pix_fmt")
            .arg(pix_fmt)
            .arg("-s")
            .arg(format!("{width}x{height}"))
            .arg("-r")
            .arg(format!("{}/{}", range.fps_num, range.fps_den))
            .arg("-i")
            .arg("pipe:0");

        match &audio_input {
            TimelineAudioInput::PcmFile { path, sample_rate, channel_layout } => {
                let Some(ffmpeg_layout) = ffmpeg_audio_channel_layout(*channel_layout) else {
                    return JobExecutionResult::Failed(format!(
                        "audio output layout {channel_layout:?} has no explicit FFmpeg lowering"
                    ));
                };
                cmd.arg("-f")
                    .arg("f32le")
                    .arg("-ar")
                    .arg(sample_rate.to_string())
                    .arg("-channel_layout")
                    .arg(ffmpeg_layout)
                    .arg("-ac")
                    .arg(channel_layout.channel_count().to_string())
                    .arg("-i")
                    .arg(path)
                    .arg("-map")
                    .arg("0:v:0")
                    .arg("-map")
                    .arg("1:a:0")
                    .arg("-shortest");
            }
            TimelineAudioInput::Silent { sample_rate, channel_layout } => {
                let Some(channel_layout) = ffmpeg_audio_channel_layout(*channel_layout) else {
                    return JobExecutionResult::Failed(format!(
                        "audio output layout {channel_layout:?} has no explicit FFmpeg lowering"
                    ));
                };
                cmd.arg("-f")
                    .arg("lavfi")
                    .arg("-i")
                    .arg(format!(
                        "anullsrc=channel_layout={channel_layout}:sample_rate={sample_rate}"
                    ))
                    .arg("-map")
                    .arg("0:v:0")
                    .arg("-map")
                    .arg("1:a:0")
                    .arg("-shortest");
            }
            TimelineAudioInput::Disabled => {
                cmd.arg("-an");
            }
        }

        apply_video_codec_args(&mut cmd, &job.config.preset.video);
        apply_export_video_signal_args(&mut cmd, &timeline.sequence.settings, &delivery);
        if let Err(err) = apply_encoder_signal_params(
            &mut cmd,
            &job.config.preset.video,
            &timeline.sequence.settings,
            &delivery,
        ) {
            return JobExecutionResult::Failed(err);
        }
        if !matches!(&audio_input, TimelineAudioInput::Disabled) {
            apply_audio_codec_args(&mut cmd, &job.config.preset.audio);
        }
        cmd.arg("-f")
            .arg(container_format(&job.config.preset.container))
            .arg(output_path);

        if !execution_gate.wait_at_boundary(ExportProgressPhase::Preparing, cancel) {
            return JobExecutionResult::Cancelled;
        }
        let process_policy = SupervisedProcessPolicy {
            pipe_stdin: true,
            stdout: SupervisedStreamCapture::Drain,
            stderr: SupervisedStreamCapture::Tail { limit_bytes: 64 * 1024 },
            deadline: None,
            ..SupervisedProcessPolicy::default()
        };
        let mut child = match SupervisedChild::spawn(&mut cmd, process_policy) {
            Ok(child) => child,
            Err(err) => {
                return process_supervision_failure("启动 ffmpeg", err);
            }
        };

        match write_timeline_frames(
            &mut child,
            timeline,
            range,
            width,
            height,
            job.config.preset.alpha_mode,
            &delivery,
            cancel,
            execution_gate,
            report,
            report_diagnostics,
            &mut visual_session,
            media_diagnostics.issue_summary,
        ) {
            JobExecutionResult::ReversibleWorkCompleted => {}
            JobExecutionResult::Published(_) | JobExecutionResult::PublicationFailed(_) => {
                return JobExecutionResult::Failed(
                    "frame writer crossed publication authority inside reversible export work"
                        .to_owned(),
                );
            }
            JobExecutionResult::Cancelled => {
                return JobExecutionResult::Cancelled;
            }
            JobExecutionResult::Failed(reason) => {
                return JobExecutionResult::Failed(reason);
            }
        }

        if !execution_gate.wait_at_boundary(ExportProgressPhase::Encoding, cancel) {
            return JobExecutionResult::Cancelled;
        }

        report(ExportProgress::encoding(0.98));
        match child.finish(cancel) {
            Ok(output) if output.status.success() => {
                *validation_expectations_out = Some(validation_expectations);
                JobExecutionResult::ReversibleWorkCompleted
            }
            Ok(output) => {
                let stderr_tail = String::from_utf8_lossy(&output.stderr);
                let reason = stderr_tail
                    .lines()
                    .rev()
                    .find(|line| !line.trim().is_empty())
                    .map(|line| line.trim().to_string())
                    .unwrap_or_else(|| format!("ffmpeg 退出码：{}", output.status));
                JobExecutionResult::Failed(format!("时间线编码失败：{reason}"))
            }
            Err(error) => process_supervision_failure("等待 ffmpeg 编码完成", error),
        }
    })();

    if let Some(path) = temp_audio_path_to_cleanup {
        let _ = std::fs::remove_file(path);
    }
    result
}

fn validate_snapshot_media_revisions(timeline: &TimelineExportSnapshot) -> Result<(), String> {
    for (asset_id, dependency) in &timeline.media {
        if !dependency.source_fingerprint.authorizes_reuse() {
            return Err(format!(
                "export source revision evidence admitted for the snapshot is incomplete: asset={} path={} admitted={:?}",
                asset_id,
                dependency.path.display(),
                dependency.source_fingerprint
            ));
        }
        let actual = MediaFileFingerprint::capture(dependency.path.as_path());
        if !actual.authorizes_reuse() {
            return Err(format!(
                "export source revision evidence cannot be observed completely: asset={} path={} actual={:?}",
                asset_id,
                dependency.path.display(),
                actual
            ));
        }
        if actual != dependency.source_fingerprint {
            return Err(format!(
                "export source revision changed: asset={} path={} admitted={:?} actual={:?}",
                asset_id,
                dependency.path.display(),
                dependency.source_fingerprint,
                actual
            ));
        }
    }
    Ok(())
}

fn finalize_export_output(
    staging: OwnedPublicationFile,
    final_output: &Path,
    output_policy: ExportOutputPolicy,
) -> Result<DurableExportPublication, ExportPublicationFailure> {
    let partial_output = staging.path().to_path_buf();
    let publication_mode = match output_policy {
        ExportOutputPolicy::CreateNew => FilePublicationMode::CreateNew,
        ExportOutputPolicy::OverwriteExisting => FilePublicationMode::ReplaceExisting,
    };
    match staging.preserve_source_on_before_namespace_failure().publish(publication_mode) {
        Ok(evidence) => Ok(DurableExportPublication::from_storage(evidence)),
        Err(failure) => {
            let retained_partial_path =
                matches!(&failure, FilePublicationFailure::BeforeNamespace(_))
                    .then_some(partial_output);
            Err(ExportPublicationFailure::from_storage(
                failure,
                final_output,
                retained_partial_path,
            ))
        }
    }
}

fn prepare_timeline_audio_input(
    job: &RenderJob,
    timeline: &TimelineExportSnapshot,
    range: TimelineRenderRange,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    report: &mut dyn FnMut(ExportProgress),
) -> Result<TimelineAudioInput, JobExecutionResult> {
    if !execution_gate.wait_at_boundary(ExportProgressPhase::Preparing, cancel) {
        return Err(JobExecutionResult::Cancelled);
    }
    if matches!(job.config.preset.audio, AudioCodecConfig::Disabled) {
        return Ok(TimelineAudioInput::Disabled);
    }

    let sample_rate = timeline.sequence.settings.audio_sample_rate.max(8_000);
    let channel_layout = timeline.sequence.settings.audio_channel_layout;
    let prepared_audio = timeline
        .prepared_execution()
        .and_then(|execution| execution.audio())
        .ok_or_else(|| {
            JobExecutionResult::Failed(
                "immutable export audio Program evidence was not admitted".to_owned(),
            )
        })?;
    if !prepared_audio.execution_demand().requires_execution() {
        return Ok(TimelineAudioInput::Silent { sample_rate, channel_layout });
    }

    let temp_path = std::env::temp_dir().join(format!(
        "mondrian-export-audio-{}-{}.f32",
        job.id(),
        chrono::Utc::now().timestamp_millis()
    ));
    let resource_policy = execution_gate.resource_policy();

    match render_timeline_audio_to_pcm_f32(
        temp_path.as_path(),
        timeline,
        prepared_audio,
        range,
        sample_rate,
        channel_layout,
        resource_policy,
        cancel,
        execution_gate,
        report,
    ) {
        JobExecutionResult::ReversibleWorkCompleted => {
            Ok(TimelineAudioInput::PcmFile { path: temp_path, sample_rate, channel_layout })
        }
        JobExecutionResult::Published(_) | JobExecutionResult::PublicationFailed(_) => {
            let _ = std::fs::remove_file(&temp_path);
            Err(JobExecutionResult::Failed(
                "audio preparation crossed publication authority inside reversible export work"
                    .to_owned(),
            ))
        }
        JobExecutionResult::Cancelled => {
            let _ = std::fs::remove_file(&temp_path);
            Err(JobExecutionResult::Cancelled)
        }
        JobExecutionResult::Failed(reason) => {
            let _ = std::fs::remove_file(&temp_path);
            Err(JobExecutionResult::Failed(reason))
        }
    }
}

fn render_timeline_audio_to_pcm_f32(
    output_path: &Path,
    timeline: &TimelineExportSnapshot,
    prepared_audio: &PreparedTimelineAudioSnapshot,
    range: TimelineRenderRange,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    resource_policy: service::ExportExecutionResourcePolicy,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    report: &mut dyn FnMut(ExportProgress),
) -> JobExecutionResult {
    if !execution_gate.wait_at_boundary(ExportProgressPhase::Preparing, cancel) {
        return JobExecutionResult::Cancelled;
    }
    let file = match std::fs::File::create(output_path) {
        Ok(file) => file,
        Err(err) => {
            return JobExecutionResult::Failed(format!(
                "创建临时音频文件失败 {}: {}",
                output_path.display(),
                err
            ));
        }
    };
    let audio_cache = resource_policy.audio_source_cache;
    let cache = Arc::new(AudioSourceCache::new_bounded_with_sessions(
        sample_rate,
        10,
        audio_cache.entry_capacity,
        audio_cache.byte_budget,
        audio_cache.decoder_session_capacity,
    ));
    let resolver = ExportAudioMediaResolver { timeline, cache: Arc::clone(&cache) };
    let program_channel_layout = timeline.sequence.settings.audio_channel_layout;
    let contract = AudioRenderContract {
        sample_rate,
        channel_layout: program_channel_layout,
        max_block_frames: 16_384,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    };
    let public_time_range = match range.time_range() {
        Ok(range) => range,
        Err(error) => return JobExecutionResult::Failed(error),
    };
    let runtime =
        match AudioProgramRuntime::build_from_precompiled_closure_for_range_with_resource_grant(
            &timeline.sequence,
            &timeline.sequences,
            &resolver,
            contract,
            Some(prepared_audio.root_program().output_id()),
            public_time_range,
            prepared_audio.closure(),
            resource_policy.audio_runtime_grant,
        ) {
            Ok(runtime) => runtime,
            Err(error) => {
                return JobExecutionResult::Failed(format!(
                    "编译导出音频 Program 失败（未使用降级混音）: {error}"
                ));
            }
        };
    if runtime.execution_demand() != prepared_audio.execution_demand() {
        return JobExecutionResult::Failed(
            "prepared audio Runtime execution demand differs from admitted root Program evidence"
                .to_owned(),
        );
    }
    let mut delivery = match AudioProgramDeliveryRuntime::prepare_standard(runtime, channel_layout)
    {
        Ok(delivery) => delivery,
        Err(error) => {
            return JobExecutionResult::Failed(format!("导出音频输出布局映射不可用: {error}"));
        }
    };

    let (start_sample, total_samples) = match timeline_audio_sample_range(range, sample_rate) {
        Ok(sample_range) => sample_range,
        Err(error) => return JobExecutionResult::Failed(error),
    };
    if total_samples == 0 {
        return JobExecutionResult::ReversibleWorkCompleted;
    }
    if delivery.requires_state_entry()
        && let Err(error) = delivery.enter_state(AudioContinuityEpoch::new(1), start_sample)
    {
        return JobExecutionResult::Failed(format!("进入导出音频连续性状态失败: {error}"));
    }

    let chunk_frames_target = (sample_rate as usize / 5).clamp(1024, 16_384);
    let mut writer = BufWriter::new(file);
    let mut rendered_samples = 0usize;
    let channels = channel_layout.channel_count();
    let mut sample_bytes = Vec::<u8>::with_capacity(chunk_frames_target * channels * 4);
    let mut pcm = vec![0.0_f32; chunk_frames_target * channels];

    while rendered_samples < total_samples {
        if !execution_gate.wait_at_boundary(ExportProgressPhase::Preparing, cancel) {
            return JobExecutionResult::Cancelled;
        }

        let remaining = total_samples - rendered_samples;
        let chunk_frames = remaining.min(chunk_frames_target).max(1);
        let rendered_samples_i64 = match i64::try_from(rendered_samples) {
            Ok(value) => value,
            Err(_) => {
                return JobExecutionResult::Failed("导出音频样本位置超出支持范围".to_owned());
            }
        };
        let chunk_start = match start_sample.checked_add(rendered_samples_i64) {
            Some(value) => value,
            None => {
                return JobExecutionResult::Failed("导出音频样本位置超出支持范围".to_owned());
            }
        };
        let chunk_samples = chunk_frames * channels;
        if let Err(error) = delivery.render_into_cancellable(
            AudioRenderRequest { start_sample: chunk_start, frames: chunk_frames },
            &mut pcm[..chunk_samples],
            cancel,
        ) {
            if cancel.is_canceled() {
                return JobExecutionResult::Cancelled;
            }
            return JobExecutionResult::Failed(format!("执行导出音频 Program 失败: {error}"));
        }
        sample_bytes.clear();
        sample_bytes.reserve(chunk_samples * 4);
        for sample in &pcm[..chunk_samples] {
            sample_bytes.extend_from_slice(&sample.to_le_bytes());
        }
        if let Err(err) = writer.write_all(&sample_bytes) {
            return JobExecutionResult::Failed(format!("写入临时音频文件失败: {}", err));
        }

        rendered_samples += chunk_frames;
        let ratio = rendered_samples as f32 / total_samples as f32;
        let progress = (0.02 + 0.14 * ratio).clamp(0.02, 0.16);
        report(ExportProgress::preparing(progress));
    }

    if let Err(err) = writer.flush() {
        return JobExecutionResult::Failed(format!("刷新临时音频文件失败: {}", err));
    }
    JobExecutionResult::ReversibleWorkCompleted
}

fn timeline_audio_sample_range(
    range: TimelineRenderRange,
    sample_rate: u32,
) -> Result<(i64, usize), String> {
    if range.total_frames == 0 || sample_rate == 0 {
        return Ok((0, 0));
    }
    let time_base = Rational::new(range.fps_den, range.fps_num);
    let rate = AudioSampleRate::new(sample_rate).map_err(|error| error.to_string())?;
    let start_time =
        TimelineTime::from_frame_position(FramePosition::new(range.start_frame, time_base))
            .map_err(|error| error.to_string())?;
    let frame_count =
        i64::try_from(range.total_frames).map_err(|_| "导出音频帧范围超出支持范围".to_owned())?;
    let end_time = TimelineTime::from_frame_position(FramePosition::new(
        range.start_frame.saturating_add(frame_count),
        time_base,
    ))
    .map_err(|error| error.to_string())?;
    let start =
        AudioSamplePosition::from_timeline_time(start_time, rate, AudioSampleRounding::Nearest)
            .map_err(|error| error.to_string())?;
    let end = AudioSamplePosition::from_timeline_time(end_time, rate, AudioSampleRounding::Nearest)
        .map_err(|error| error.to_string())?;
    let samples = end.samples_since(start).map_err(|error| error.to_string())?;
    Ok((
        start.sample(),
        usize::try_from(samples.max(0)).map_err(|_| "导出音频样本范围超出支持范围".to_owned())?,
    ))
}

struct ExportAudioMediaResolver<'a> {
    timeline: &'a TimelineExportSnapshot,
    cache: Arc<AudioSourceCache>,
}

impl AudioMediaResolver for ExportAudioMediaResolver<'_> {
    fn resolve(
        &self,
        asset_id: AssetId,
        component_id: AudioSourceComponentId,
        sample_rate: u32,
    ) -> Result<ResolvedAudioSource, String> {
        if self.cache.sample_rate() != sample_rate {
            return Err(format!(
                "audio source cache rate {} does not match export render rate {}",
                self.cache.sample_rate(),
                sample_rate
            ));
        }
        let dependency = self
            .timeline
            .media
            .get(&asset_id)
            .ok_or_else(|| format!("Asset {asset_id} has no export media dependency"))?;
        let selection = dependency.audio_components.get(&component_id).ok_or_else(|| {
            format!(
                "audio Component {component_id} has no frozen stream binding for Asset {asset_id}"
            )
        })?;
        let source =
            self.cache.open(dependency.path.as_path(), selection.clone()).map_err(|error| {
                format!(
                    "failed to open bounded audio source Asset {asset_id} at {}: {error}",
                    dependency.path.display()
                )
            })?;
        Ok(ResolvedAudioSource::new(
            source.channel_layout(),
            Arc::new(ExportDecodedAudioSource(source)),
        ))
    }
}

struct ExportDecodedAudioSource(mondrian_media::AudioSourceReader);

impl AudioDecodedSource for ExportDecodedAudioSource {
    fn read_interleaved(
        &self,
        start_frame: i64,
        frames: usize,
        destination: &mut [f32],
        cancellation: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<(), String> {
        self.0
            .read_interleaved_cancellable(start_frame, frames, destination, cancellation)
            .map_err(|error| error.to_string())
    }
}

fn preflight_timeline_visual_range_at_resolution(
    timeline: &TimelineExportSnapshot,
    range: TimelineRenderRange,
    root_resolution: Resolution,
    root_color_context: ProgramColorContext,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    visual_session: &mut ExportVisualRenderSession,
) -> Result<(), JobExecutionResult> {
    if visual_session.route_contracts_sealed {
        return preflight_timeline_visual_range_once(
            timeline,
            range,
            root_resolution,
            root_color_context,
            cancel,
            execution_gate,
            visual_session,
        );
    }
    preflight_timeline_visual_range_once(
        timeline,
        range,
        root_resolution,
        root_color_context,
        cancel,
        execution_gate,
        visual_session,
    )?;
    visual_session.route_contracts_sealed = true;
    Ok(())
}

fn preflight_timeline_visual_range_once(
    timeline: &TimelineExportSnapshot,
    range: TimelineRenderRange,
    root_resolution: Resolution,
    root_color_context: ProgramColorContext,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    visual_session: &mut ExportVisualRenderSession,
) -> Result<(), JobExecutionResult> {
    for index in 0..range.total_frames {
        if !execution_gate.wait_at_boundary(ExportProgressPhase::Preparing, cancel) {
            return Err(JobExecutionResult::Cancelled);
        }
        let index = i64::try_from(index).map_err(|_| {
            JobExecutionResult::Failed(
                "export visual preflight frame range exceeds signed coordinate capacity".to_owned(),
            )
        })?;
        let timeline_frame = range.start_frame.checked_add(index).ok_or_else(|| {
            JobExecutionResult::Failed(
                "export visual preflight frame coordinate overflowed signed capacity".to_owned(),
            )
        })?;
        let closure = prepare_export_visual_frame_closure(
            timeline,
            visual_session,
            cancel,
            &timeline.sequence,
            timeline_frame,
            root_resolution,
            root_color_context.clone(),
        )
        .map_err(|reason| {
            if cancel.is_canceled() {
                JobExecutionResult::Cancelled
            } else {
                JobExecutionResult::Failed(format!(
                    "export visual closure preflight failed at root frame {timeline_frame}: {reason}"
                ))
            }
        })?;
        let materialization_bytes = closure
            .conservative_cpu_materialization_active_bytes()
            .map_err(|error| JobExecutionResult::Failed(error.to_string()))?;
        visual_session
            .composite_scratch
            .admit_cpu_active_working_set(
                materialization_bytes,
                TimelineCpuCompositePrecision::Float32,
            )
            .map_err(|error| {
                JobExecutionResult::Failed(format!(
                    "export visual closure exceeds its CPU working-set grant at root frame {timeline_frame}: {error}"
                ))
            })?;
        if cancel.is_canceled() {
            return Err(JobExecutionResult::Cancelled);
        }
    }
    Ok(())
}

fn process_supervision_failure(
    operation: &str,
    error: SupervisedProcessError,
) -> JobExecutionResult {
    if error.is_canceled() {
        JobExecutionResult::Cancelled
    } else {
        JobExecutionResult::Failed(format!("{operation}失败: {error}"))
    }
}

fn write_timeline_frames(
    child: &mut SupervisedChild,
    timeline: &TimelineExportSnapshot,
    range: TimelineRenderRange,
    width: u32,
    height: u32,
    alpha_mode: ExportAlphaMode,
    delivery: &ResolvedExportDeliveryContract,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    report: &mut dyn FnMut(ExportProgress),
    report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
    visual_session: &mut ExportVisualRenderSession,
    asset_issue_summary: VideoColorDiagnosticIssueAggregate,
) -> JobExecutionResult {
    render_timeline_frames_with_sink(
        timeline,
        range,
        width,
        height,
        alpha_mode,
        delivery,
        cancel,
        execution_gate,
        report,
        report_diagnostics,
        visual_session,
        asset_issue_summary,
        &mut |canvas| {
            let owned = std::mem::take(canvas);
            match child.write_owned(owned, cancel) {
                Ok(returned) => {
                    *canvas = returned;
                    Ok(())
                }
                Err(error) => Err(process_supervision_failure("写入 ffmpeg 视频管道", error)),
            }
        },
    )
}

fn render_timeline_frames_with_sink(
    timeline: &TimelineExportSnapshot,
    range: TimelineRenderRange,
    width: u32,
    height: u32,
    alpha_mode: ExportAlphaMode,
    delivery: &ResolvedExportDeliveryContract,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    report: &mut dyn FnMut(ExportProgress),
    report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
    visual_session: &mut ExportVisualRenderSession,
    asset_issue_summary: VideoColorDiagnosticIssueAggregate,
    write_frame: &mut dyn FnMut(&mut Vec<u8>) -> Result<(), JobExecutionResult>,
) -> JobExecutionResult {
    let frame_contract = export_frame_contract(delivery.bit_depth);
    let root_color_context = resolved_export_color_context(timeline, delivery);
    let total = range.total_frames.max(1);
    let mut canvas = vec![0u8; frame_contract.canvas_len(width, height)];
    let mut diagnostics = ExportJobDiagnostics::default();
    diagnostics.color.record_asset_issue_summary(asset_issue_summary);

    for index in 0..total {
        if !execution_gate.wait_at_boundary(ExportProgressPhase::Rendering, cancel) {
            return JobExecutionResult::Cancelled;
        }

        let timeline_frame = range.start_frame + index as i64;
        let mut frame_color_counts = InputColorResolutionSourceCounts::default();
        let mut frame_stage_diagnostics = RenderColorStageDiagnostics::default();
        let mut frame_composite_diagnostics = TimelineCompositeDiagnostics::default();
        let render_result = render_timeline_frame_into_with_session_cancellable(
            timeline,
            timeline_frame,
            width,
            height,
            alpha_mode,
            root_color_context.clone(),
            frame_contract,
            &mut canvas,
            Some(&mut frame_color_counts),
            Some(&mut frame_stage_diagnostics),
            Some(&mut frame_composite_diagnostics),
            Some(&mut diagnostics.color),
            visual_session,
            cancel,
        );
        diagnostics.color.record_frame_diagnostics(
            frame_color_counts,
            frame_stage_diagnostics,
            frame_composite_diagnostics,
        );
        diagnostics.visual = visual_session.visual_diagnostics();
        report_diagnostics(diagnostics);
        match render_result {
            Ok(()) => {}
            Err(_) if cancel.is_canceled() => {
                return JobExecutionResult::Cancelled;
            }
            Err(err) => {
                return JobExecutionResult::Failed(format!(
                    "渲染时间线帧失败（frame={}）: {}",
                    timeline_frame, err
                ));
            }
        }

        if let Err(outcome) = write_frame(&mut canvas) {
            return outcome;
        }

        let rendered = index + 1;
        let ratio = rendered as f32 / total as f32;
        let progress = (0.18 + 0.72 * ratio).clamp(0.18, 0.92);
        report(ExportProgress::rendering(progress, rendered, total));
    }

    JobExecutionResult::ReversibleWorkCompleted
}

/// Build the export output boundary from the resolved color context.
///
/// The renderer resolves the product-level output-transform intent. Preview
/// and export therefore cannot independently reinterpret Mondrian Standard,
/// an explicit OCIO view, or a colorimetric delivery.
fn export_output_boundary_from_context(
    color_context: &ProgramColorContext,
) -> Result<RenderOutputColorBoundary, String> {
    let output_color_space = color_context.output_color_space.color().ok_or_else(|| {
        "deliverable output boundary requires an encoded output color space".to_owned()
    })?;
    RenderOutputColorBoundary::from_intent(
        mondrian_renderer::RenderOutputColorBoundaryTarget::Export,
        output_color_space,
        &color_context.output_transform,
        color_context.output_tone_map,
        color_context.engine.clone(),
    )
    .map_err(|error| error.to_string())
}

fn resolved_export_color_context(
    timeline: &TimelineExportSnapshot,
    delivery: &ResolvedExportDeliveryContract,
) -> ProgramColorContext {
    let mut context = timeline
        .sequence
        .settings
        .root_program_color_context(&timeline.color_environment);
    context.output_color_space = delivery.color_target.color_space.into();
    context.output_tone_map = delivery.color_target.tone_map;
    context.output_transform = delivery.color_target.output_transform.clone();
    context
}

fn render_timeline_frame_into(
    timeline: &TimelineExportSnapshot,
    timeline_frame: i64,
    width: u32,
    height: u32,
    alpha_mode: ExportAlphaMode,
    canvas: &mut Vec<u8>,
    input_color_counts: Option<&mut InputColorResolutionSourceCounts>,
    stage_diagnostics: Option<&mut RenderColorStageDiagnostics>,
    composite_diagnostics: Option<&mut TimelineCompositeDiagnostics>,
    export_diagnostics: Option<&mut ExportJobColorDiagnostics>,
) -> Result<(), String> {
    let mut visual_session = ExportVisualRenderSession::for_timeline(
        0,
        service::ExportExecutionResourcePolicy::default(),
        timeline,
    )?;
    let frame_contract = export_frame_contract(timeline.sequence.settings.delivery.bit_depth);
    let color_context = timeline
        .sequence
        .settings
        .root_program_color_context(&timeline.color_environment);
    render_timeline_frame_into_with_session(
        timeline,
        timeline_frame,
        width,
        height,
        alpha_mode,
        color_context,
        frame_contract,
        canvas,
        input_color_counts,
        stage_diagnostics,
        composite_diagnostics,
        export_diagnostics,
        &mut visual_session,
    )
}

fn render_timeline_frame_into_with_session(
    timeline: &TimelineExportSnapshot,
    timeline_frame: i64,
    width: u32,
    height: u32,
    alpha_mode: ExportAlphaMode,
    color_context: ProgramColorContext,
    frame_contract: ExportFrameContract,
    canvas: &mut Vec<u8>,
    input_color_counts: Option<&mut InputColorResolutionSourceCounts>,
    stage_diagnostics: Option<&mut RenderColorStageDiagnostics>,
    composite_diagnostics: Option<&mut TimelineCompositeDiagnostics>,
    export_diagnostics: Option<&mut ExportJobColorDiagnostics>,
    visual_session: &mut ExportVisualRenderSession,
) -> Result<(), String> {
    render_timeline_frame_into_with_session_cancellable(
        timeline,
        timeline_frame,
        width,
        height,
        alpha_mode,
        color_context,
        frame_contract,
        canvas,
        input_color_counts,
        stage_diagnostics,
        composite_diagnostics,
        export_diagnostics,
        visual_session,
        &ExecutionCancellationToken::new(),
    )
}

#[allow(clippy::too_many_arguments)]
fn render_timeline_frame_into_with_session_cancellable(
    timeline: &TimelineExportSnapshot,
    timeline_frame: i64,
    width: u32,
    height: u32,
    alpha_mode: ExportAlphaMode,
    color_context: ProgramColorContext,
    frame_contract: ExportFrameContract,
    canvas: &mut Vec<u8>,
    input_color_counts: Option<&mut InputColorResolutionSourceCounts>,
    stage_diagnostics: Option<&mut RenderColorStageDiagnostics>,
    composite_diagnostics: Option<&mut TimelineCompositeDiagnostics>,
    export_diagnostics: Option<&mut ExportJobColorDiagnostics>,
    visual_session: &mut ExportVisualRenderSession,
    cancellation: &ExecutionCancellationToken,
) -> Result<(), String> {
    let required_len = frame_contract.canvas_len(width, height);
    if canvas.len() != required_len {
        canvas.resize(required_len, 0);
    }

    let mut render_context = ExportFrameRenderContext {
        media: &timeline.media,
        color_environment: &timeline.color_environment,
        alpha_mode,
        frame_contract,
        input_color_counts,
        stage_diagnostics,
        composite_diagnostics,
        export_diagnostics,
        visual_session,
        cancellation,
    };

    render_sequence_frame_into(
        timeline,
        &mut render_context,
        &timeline.sequence,
        timeline_frame,
        Resolution { width, height },
        color_context,
        SequenceRenderTarget::Deliverable(canvas),
    )
}

enum SequenceRenderTarget<'a> {
    Working(&'a mut Option<CpuColorFrame>),
    Deliverable(&'a mut Vec<u8>),
}

#[derive(Clone)]
struct PreparedExportTemporalLayer {
    frame: CpuColorFrame,
    source_resolution: Resolution,
}

enum ResolvedExportTransitionInput {
    Transparent,
    Decoded(Arc<DecodedVideoLayer>),
    Nested(CpuColorFrame),
    BasicTitle(ResolvedExportTitle),
    SolidColor,
    Temporal(PreparedExportTemporalLayer),
}

struct ResolvedExportTitle {
    frame: CpuColorFrame,
    transform: [f32; 6],
}

struct ExportVisualRenderSession {
    title_rasterizer: BasicTitleRasterizer,
    decode_context: PreviewDecodeSessionContext,
    prepared_visual: PreparedTimelineVisualSnapshot,
    #[cfg(test)]
    reference_visual_programs: Option<PreparedVisualProgramCache>,
    #[cfg(test)]
    reference_programs_by_sequence: HashMap<SequenceId, Arc<PreparedVisualProgram>>,
    composite_scratch: TimelineCompositeScratch,
    gpu_output: ExportGpuExecutionRuntime,
    heterogeneous_route_contracts: Vec<ExportHeterogeneousRouteContract>,
    visual_diagnostics: ExportJobVisualDiagnostics,
    resource_policy: service::ExportExecutionResourcePolicy,
    effect_execution_generation: u64,
    route_contracts_sealed: bool,
}

impl ExportVisualRenderSession {
    fn for_execution_generation(
        effect_execution_generation: u64,
        resource_policy: service::ExportExecutionResourcePolicy,
        prepared_visual: &PreparedTimelineVisualSnapshot,
    ) -> Result<Self, String> {
        let program_count = prepared_visual.program_count();
        let retained_bytes = prepared_visual.retained_bytes();
        if program_count > resource_policy.visual_program_entries
            || retained_bytes > resource_policy.visual_program_bytes
        {
            return Err(format!(
                "prepared export visual closure exceeds its frozen grant: programs={program_count}/{} logical_bytes={retained_bytes}/{}",
                resource_policy.visual_program_entries,
                resource_policy.visual_program_bytes
            ));
        }
        let mut composite_scratch = TimelineCompositeScratch::default();
        composite_scratch.reconfigure_effect_execution(EffectExecutionSessionConfig {
            max_cache_entries: resource_policy.effect_cache_entries,
            max_cache_bytes: resource_policy.effect_cache_bytes,
            max_working_bytes: resource_policy.effect_working_bytes,
            max_gpu_plan_entries: resource_policy.effect_gpu_plan_entries,
            max_gpu_plan_bytes: resource_policy.effect_gpu_plan_bytes,
        });
        composite_scratch.reconfigure_cpu_working_set(resource_policy.cpu_composite_working_set);
        composite_scratch.reconfigure_color_execution(resource_policy.cpu_color_processor_capacity);
        let mut gpu_output = ExportGpuExecutionRuntime::default();
        gpu_output.configure(resource_policy);
        gpu_output.begin_attempt(effect_execution_generation);
        let title_fonts = prepared_visual.title_fonts().ok_or_else(|| {
            "immutable export Basic Title font dependency closure is unavailable".to_owned()
        })?;
        if title_fonts.retained_bytes() > resource_policy.title_font_bytes {
            return Err(format!(
                "prepared Basic Title font closure exceeds its frozen grant: bytes={}/{}",
                title_fonts.retained_bytes(),
                resource_policy.title_font_bytes
            ));
        }
        Ok(Self {
            title_rasterizer: BasicTitleRasterizer::with_prepared_font_set(
                resource_policy.title_cache_entries,
                resource_policy.title_cache_bytes,
                title_fonts,
            )
            .map_err(|error| format!("failed to load frozen Basic Title font closure: {error}"))?,
            decode_context: PreviewDecodeSessionContext::default(),
            prepared_visual: prepared_visual.clone(),
            #[cfg(test)]
            reference_visual_programs: None,
            #[cfg(test)]
            reference_programs_by_sequence: HashMap::new(),
            composite_scratch,
            gpu_output,
            heterogeneous_route_contracts: Vec::new(),
            visual_diagnostics: ExportJobVisualDiagnostics::default(),
            resource_policy,
            effect_execution_generation,
            route_contracts_sealed: false,
        })
    }

    #[cfg(test)]
    fn for_reference_generation(
        effect_execution_generation: u64,
        resource_policy: service::ExportExecutionResourcePolicy,
    ) -> Self {
        let sequence = mondrian_timeline::sequence::Sequence::new("export visual test reference");
        let prepared = crate::prepare_timeline_export_dependencies(
            &sequence,
            &[],
            TimelineExportRange::EntireSequence,
            false,
        )
        .expect("prepare Export test reference Program");
        let mut prepared_visual = prepared.execution_snapshot().visual().clone();
        prepared_visual
            .install_title_fonts(mondrian_renderer::PreparedBasicTitleFontSet::default())
            .expect("seal empty Export test title-font closure");
        let mut session = Self::for_execution_generation(
            effect_execution_generation,
            resource_policy,
            &prepared_visual,
        )
        .expect("admit Export test reference Program");
        session.reference_visual_programs = Some(PreparedVisualProgramCache::with_config(
            PreparedVisualProgramCacheConfig::new(
                resource_policy.visual_program_entries,
                resource_policy.visual_program_bytes,
            )
            .with_lut_cache(mondrian_effects::LutPreparationCacheConfig::new(
                resource_policy.lut_cache_entries,
                resource_policy.lut_cache_bytes,
            )),
        ));
        session
    }

    fn for_timeline(
        effect_execution_generation: u64,
        resource_policy: service::ExportExecutionResourcePolicy,
        timeline: &TimelineExportSnapshot,
    ) -> Result<Self, String> {
        if let Some(prepared_visual) =
            timeline.prepared_execution().map(|execution| execution.visual())
        {
            let admitted_visual = if prepared_visual.title_fonts().is_none() {
                let mut candidate = prepared_visual.clone();
                candidate
                    .freeze_title_fonts(resource_policy.title_font_bytes)
                    .map_err(|error| error.to_string())?;
                Some(candidate)
            } else {
                None
            };
            return Self::for_execution_generation(
                effect_execution_generation,
                resource_policy,
                admitted_visual.as_ref().unwrap_or(prepared_visual),
            );
        }
        #[cfg(test)]
        {
            Ok(Self::for_reference_generation(
                effect_execution_generation,
                resource_policy,
            ))
        }
        #[cfg(not(test))]
        {
            Err("immutable export visual execution snapshot is unavailable".to_owned())
        }
    }

    /// Release every decoder Session owned by this exact export job.
    ///
    /// Export never borrows the media convenience thread-local. A job-local
    /// context travels through every prepared root, child, Transition, and
    /// temporal materialization and is retired before the executor returns to
    /// terminal publication.
    fn release_decode_sessions(&mut self) {
        self.decode_context.clear();
    }

    fn visual_diagnostics(&self) -> ExportJobVisualDiagnostics {
        self.visual_diagnostics
    }

    fn prepare_program(
        &mut self,
        sequence: &mondrian_timeline::sequence::Sequence,
    ) -> Result<Arc<PreparedVisualProgram>, String> {
        if let Some(program) = self.prepared_visual.program(sequence.id, sequence.revision) {
            return Ok(program);
        }
        #[cfg(test)]
        if let Some(programs) = self.reference_visual_programs.as_mut() {
            let program = programs.prepare(sequence).map_err(|error| error.to_string())?;
            self.reference_programs_by_sequence.insert(sequence.id, Arc::clone(&program));
            return Ok(program);
        }
        Err(format!(
            "Sequence {} revision {:?} was not captured by the immutable export visual snapshot",
            sequence.id, sequence.revision
        ))
    }

    fn materialization_contract_for_sequence(
        &self,
        sequence_id: SequenceId,
    ) -> Result<PreparedVisualMaterializationContract, String> {
        if let Some(program) = self.prepared_visual.program_by_id(sequence_id) {
            return Ok(program.materialization_contract());
        }
        #[cfg(test)]
        if let Some(program) = self.reference_programs_by_sequence.get(&sequence_id) {
            return Ok(program.materialization_contract());
        }
        Err(format!(
            "Sequence {sequence_id} has no frozen visual materialization contract"
        ))
    }

    #[cfg(test)]
    fn prepare_reference_range(
        &mut self,
        root: &mondrian_timeline::sequence::Sequence,
        sequences: &[mondrian_timeline::sequence::Sequence],
        range: TimelineRenderRange,
    ) -> Result<(), String> {
        if self.reference_visual_programs.is_none() {
            return Ok(());
        }
        let total_frames = i64::try_from(range.total_frames)
            .map_err(|_| "reference visual range exceeds signed frame capacity".to_owned())?;
        let end_frame_exclusive = range
            .start_frame
            .checked_add(total_frames)
            .ok_or_else(|| "reference visual range end exceeds signed frame capacity".to_owned())?;
        let dependencies = crate::prepare_timeline_export_dependencies(
            root,
            sequences,
            TimelineExportRange::WorkArea {
                start_frame: range.start_frame,
                end_frame_exclusive,
            },
            false,
        )
        .map_err(|error| error.to_string())?;
        self.prepared_visual = dependencies.execution_snapshot().visual().clone();
        self.reference_programs_by_sequence.clear();
        Ok(())
    }
}

#[cfg(test)]
impl Default for ExportVisualRenderSession {
    fn default() -> Self {
        Self::for_reference_generation(0, service::ExportExecutionResourcePolicy::default())
    }
}

impl Drop for ExportVisualRenderSession {
    fn drop(&mut self) {
        self.release_decode_sessions();
    }
}

/// Frame-scoped execution dependencies shared by root, nested, and Transition
/// rendering.
///
/// The admitted physical media/color facts and alpha policy are immutable for
/// one frame. Raw Sequence snapshots remain outside this materialization
/// context at the closure-preparation Seam. Mutable diagnostics and the
/// job-owned visual Session travel through the renderer-owned prepared closure,
/// so nested Sequences cannot create a parallel semantic evaluator.
struct ExportFrameRenderContext<'a> {
    media: &'a HashMap<AssetId, crate::preset::ExportMediaDependency>,
    color_environment: &'a mondrian_core::ProjectColorEnvironment,
    alpha_mode: ExportAlphaMode,
    frame_contract: ExportFrameContract,
    input_color_counts: Option<&'a mut InputColorResolutionSourceCounts>,
    stage_diagnostics: Option<&'a mut RenderColorStageDiagnostics>,
    composite_diagnostics: Option<&'a mut TimelineCompositeDiagnostics>,
    export_diagnostics: Option<&'a mut ExportJobColorDiagnostics>,
    visual_session: &'a mut ExportVisualRenderSession,
    cancellation: &'a ExecutionCancellationToken,
}

/// Complete identity of one decoded-and-transformed media layer.
///
/// This key is cache authority, not a diagnostic fingerprint. It therefore
/// retains every exact input that can change either the decoded source sample
/// or the source-to-working result and relies on `Eq` to resolve ordinary
/// `HashMap` hash collisions.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ExportDecodeCacheKey {
    asset_id: AssetId,
    source_path: PathBuf,
    source_fingerprint: MediaFileFingerprint,
    video_stream_index: u32,
    source_sample: mondrian_core::SourceSampleTarget,
    input_color_space: ColorSpace,
    input_video_range: DecodedVideoRangeContract,
    alpha_interpretation: AlphaInterpretation,
    media_input_color: mondrian_timeline::sequence::MediaInputColorContext,
    decode_resolution: Resolution,
    source_resolution: Resolution,
}

impl ExportDecodeCacheKey {
    #[allow(clippy::too_many_arguments)]
    fn new(
        asset_id: AssetId,
        dependency: &crate::preset::ExportMediaDependency,
        source_sample: mondrian_core::SourceSampleTarget,
        input_color_space: ColorSpace,
        input_video_range: DecodedVideoRangeContract,
        alpha_interpretation: AlphaInterpretation,
        color_context: &ProgramColorContext,
        auto_tone_map: bool,
        decode_resolution: Resolution,
        source_resolution: Resolution,
    ) -> Result<Self, String> {
        if !dependency.source_fingerprint.authorizes_reuse() {
            return Err(format!(
                "asset={} path={} cannot enter the export decode cache because its admitted source revision evidence is incomplete",
                asset_id,
                dependency.path.display()
            ));
        }
        let video_stream_index = dependency.video_stream_index.ok_or_else(|| {
            format!(
                "asset={} path={} cannot enter the export decode cache because no physical video stream was frozen",
                asset_id,
                dependency.path.display()
            )
        })?;
        Ok(Self {
            asset_id,
            source_path: dependency.path.clone(),
            source_fingerprint: dependency.source_fingerprint,
            video_stream_index,
            source_sample,
            input_color_space,
            input_video_range,
            alpha_interpretation,
            media_input_color: color_context.media_input(auto_tone_map),
            decode_resolution,
            source_resolution,
        })
    }
}

/// Collect input color-resolution source counts for one export timeline frame.
///
/// This iterates the same renderer-owned prepared closure as Timeline export,
/// including each child's bound Program color context. It is the export-side
/// diagnostic counterpart to Preview's per-frame source counters.
pub fn export_input_color_resolution_counts_for_frame(
    timeline: &TimelineExportSnapshot,
    timeline_frame: i64,
) -> Result<InputColorResolutionSourceCounts, String> {
    let cancellation = ExecutionCancellationToken::new();
    let mut visual_session = ExportVisualRenderSession::for_timeline(
        0,
        service::ExportExecutionResourcePolicy::default(),
        timeline,
    )?;
    let color_context = timeline
        .sequence
        .settings
        .root_program_color_context(&timeline.color_environment);
    let closure = prepare_export_visual_frame_closure(
        timeline,
        &mut visual_session,
        &cancellation,
        &timeline.sequence,
        timeline_frame,
        timeline.sequence.settings.resolution,
        color_context,
    )?;
    let mut counts = InputColorResolutionSourceCounts::default();
    for node in closure.nodes() {
        let color_context = node.color_context();
        for element in &node.evaluation().plan().elements {
            match element {
                TimelineRenderPlanElement::Media(media) => {
                    record_export_media_input_color_count(
                        timeline,
                        media,
                        color_context,
                        &mut counts,
                    )?;
                }
                TimelineRenderPlanElement::CrossDissolve(transition) => {
                    if let TimelineTransitionInputPlan::Media(media) = &transition.left {
                        record_export_media_input_color_count(
                            timeline,
                            media,
                            color_context,
                            &mut counts,
                        )?;
                    }
                    if let TimelineTransitionInputPlan::Media(media) = &transition.right {
                        record_export_media_input_color_count(
                            timeline,
                            media,
                            color_context,
                            &mut counts,
                        )?;
                    }
                }
                TimelineRenderPlanElement::Adjustment(_)
                | TimelineRenderPlanElement::SolidColor(_)
                | TimelineRenderPlanElement::BasicTitle(_)
                | TimelineRenderPlanElement::NestedSequence(_) => {}
            }
        }
    }
    Ok(counts)
}

/// Collect timeline composite color-path diagnostics for one export frame.
///
/// This executes the same frame render path used by export jobs and is intended
/// for preview/export parity tests, telemetry probes, and performance budgets.
pub fn export_composite_diagnostics_for_frame(
    timeline: &TimelineExportSnapshot,
    timeline_frame: i64,
    width: u32,
    height: u32,
) -> Result<TimelineCompositeDiagnostics, String> {
    let mut canvas = Vec::new();
    let mut composite_diagnostics = TimelineCompositeDiagnostics::default();
    render_timeline_frame_into(
        timeline,
        timeline_frame,
        width,
        height,
        ExportAlphaMode::FlattenBlack,
        &mut canvas,
        None,
        None,
        Some(&mut composite_diagnostics),
        None,
    )?;
    Ok(composite_diagnostics)
}

/// Collect renderer color-stage scheduling diagnostics for one export frame.
///
/// This executes the same frame render path used by export jobs and records the
/// actual input, nested, and final output stage executions.
pub fn export_color_stage_diagnostics_for_frame(
    timeline: &TimelineExportSnapshot,
    timeline_frame: i64,
    width: u32,
    height: u32,
) -> Result<RenderColorStageDiagnostics, String> {
    let mut canvas = Vec::new();
    let mut stage_diagnostics = RenderColorStageDiagnostics::default();
    render_timeline_frame_into(
        timeline,
        timeline_frame,
        width,
        height,
        ExportAlphaMode::FlattenBlack,
        &mut canvas,
        None,
        Some(&mut stage_diagnostics),
        None,
        None,
    )?;
    Ok(stage_diagnostics)
}

/// Immutable media diagnostics selected by the exact export range.
///
/// This set is frozen beside the exact prepared visual Programs rather than
/// reconstructed from the author model or live Effect registry.
#[derive(Debug, Clone)]
pub struct ExportMediaDiagnosticSet {
    /// Stable, deduplicated file-backed media identities that may contribute.
    pub asset_ids: Arc<[AssetId]>,
    /// Frozen aggregate consumed by both delivery validation and job reports.
    pub issue_summary: VideoColorDiagnosticIssueAggregate,
}

/// Prepare selected-range media diagnostics without enumerating program frames.
pub fn export_media_diagnostic_set(
    timeline: &TimelineExportSnapshot,
) -> Result<ExportMediaDiagnosticSet, String> {
    let prepared = timeline
        .prepared_execution()
        .map(|execution| execution.visual())
        .ok_or_else(|| "immutable export visual execution snapshot is unavailable".to_owned())?;
    let asset_ids = prepared.media_asset_ids().iter().copied().collect::<Vec<_>>();
    let mut issue_summary = VideoColorDiagnosticIssueAggregate::default();
    for asset_id in &asset_ids {
        if let Some(diagnostic) = timeline
            .media
            .get(asset_id)
            .and_then(|dependency| dependency.color_diagnostic.as_ref())
        {
            issue_summary.observe(diagnostic);
        }
    }
    Ok(ExportMediaDiagnosticSet { asset_ids: Arc::from(asset_ids), issue_summary })
}

fn record_export_media_input_color_count(
    timeline: &TimelineExportSnapshot,
    media: &TimelineMediaPlan,
    color_context: &ProgramColorContext,
    counts: &mut InputColorResolutionSourceCounts,
) -> Result<(), String> {
    let dependency = timeline
        .media
        .get(&media.asset_id)
        .ok_or_else(|| format!("导出快照缺少素材依赖: {}", media.asset_id))?;
    let resolution = color_context.missing_metadata_policy.resolve_asset_input_decision(
        media.color_space_override,
        dependency.interpretation,
        dependency
            .color_diagnostic
            .as_ref()
            .and_then(mondrian_media::VideoColorDiagnostic::executable_color_space),
        color_context.working_color_space,
    );
    counts.record(resolution.source);
    Ok(())
}

type PreparedExportVisualClosure =
    PreparedVisualFrameClosure<Vec<PreparedExportHeterogeneousElement>>;

fn export_visual_node(
    closure: &PreparedExportVisualClosure,
    node_id: PreparedVisualFrameNodeId,
) -> Result<&PreparedVisualFrameNode<Vec<PreparedExportHeterogeneousElement>>, String> {
    closure.node(node_id).ok_or_else(|| {
        format!(
            "prepared export visual closure references missing node {}",
            node_id.index()
        )
    })
}

fn export_nested_child(
    closure: &PreparedExportVisualClosure,
    parent_id: PreparedVisualFrameNodeId,
    placement: TimelineClipExecutionRef,
    sample: PreparedVisualNestedSample,
) -> Result<PreparedVisualFrameNodeId, String> {
    export_visual_node(closure, parent_id)?
        .nested_child(placement, sample)
        .ok_or_else(|| {
            format!(
                "prepared export visual closure has no {sample:?} child binding for Clip {}",
                placement.clip_id
            )
        })
}

fn prepare_export_visual_frame_closure(
    timeline: &TimelineExportSnapshot,
    visual_session: &mut ExportVisualRenderSession,
    cancellation: &ExecutionCancellationToken,
    root_sequence: &mondrian_timeline::sequence::Sequence,
    root_frame: i64,
    root_resolution: Resolution,
    root_color_context: ProgramColorContext,
) -> Result<PreparedExportVisualClosure, String> {
    let visual_session = std::cell::RefCell::new(visual_session);
    prepare_visual_frame_closure(
        PreparedVisualFrameClosureRequest {
            root_sequence,
            sequences: &timeline.sequences,
            root_frame,
            root_resolution,
            root_color_context,
            child_canvas_policy: PreparedVisualChildCanvasPolicy::Authored,
        },
        |sequence| visual_session.borrow_mut().prepare_program(sequence),
        |program, frame, resolution, _color_context, _normalized_preview_resolution_scale| {
            if cancellation.is_canceled() {
                return Err("export visual execution canceled".to_owned());
            }
            let mut visual_session = visual_session.borrow_mut();
            let effect_execution_generation = visual_session.effect_execution_generation;
            let extent = EffectFrameExtent::new(resolution.width, resolution.height);
            let prepared_frame = visual_session
                .composite_scratch
                .prepare_timeline_frame_execution(
                    program.as_ref(),
                    TimelineFrameExecutionRequest::new(
                        TimelineEvaluationRequest::export(FramePosition::new(
                            frame,
                            program.evaluation_time_base(),
                        )),
                        effect_execution_generation,
                        EffectExecutionContinuity::Discontinuous,
                        extent,
                        extent.full_frame_roi(),
                        cancellation.clone(),
                    ),
                )
                .map_err(|error| error.to_string())?;
            if prepared_frame.execution_plan().is_empty() {
                let (render_plan, temporal_batches) = prepared_frame.into_parts();
                return Ok(PreparedVisualFrameEvaluation::new(
                    render_plan,
                    temporal_batches,
                    Vec::new(),
                ));
            }
            let temporal_source_coverage_bytes = prepared_frame.source_coverage_bytes();
            let (temporal_render_plan, temporal_batches) = prepared_frame.into_parts();
            visual_session
                .composite_scratch
                .admit_cpu_active_working_set(
                    temporal_source_coverage_bytes,
                    TimelineCpuCompositePrecision::Float32,
                )
                .map_err(|error| {
                    format!(
                        "export temporal source coverage exceeds the CPU working-set grant: {error}"
                    )
                })?;
            let effect_frame_plan = prepare_export_effect_frame_plan(
                program.as_ref(),
                &temporal_render_plan,
                resolution,
                &mut visual_session,
            )
            .map_err(|error| error.to_string())?;
            let (render_plan, heterogeneous) = effect_frame_plan.into_parts();
            Ok(PreparedVisualFrameEvaluation::new(
                render_plan,
                temporal_batches,
                heterogeneous,
            ))
        },
    )
    .map_err(|error| error.to_string())
}

fn render_sequence_frame_into(
    timeline: &TimelineExportSnapshot,
    context: &mut ExportFrameRenderContext<'_>,
    sequence: &mondrian_timeline::sequence::Sequence,
    timeline_frame: i64,
    resolution: Resolution,
    color_context: ProgramColorContext,
    target: SequenceRenderTarget<'_>,
) -> Result<(), String> {
    let closure = prepare_export_visual_frame_closure(
        timeline,
        context.visual_session,
        context.cancellation,
        sequence,
        timeline_frame,
        resolution,
        color_context,
    )?;
    let materialization_bytes = closure
        .conservative_cpu_materialization_active_bytes()
        .map_err(|error| error.to_string())?;
    context
        .visual_session
        .composite_scratch
        .admit_cpu_active_working_set(
            materialization_bytes,
            TimelineCpuCompositePrecision::Float32,
        )
        .map_err(|error| {
            format!("export visual closure exceeds its CPU working-set grant: {error}")
        })?;
    render_prepared_visual_node_into(context, &closure, closure.root(), target)
}

fn render_prepared_visual_node_into(
    context: &mut ExportFrameRenderContext<'_>,
    closure: &PreparedExportVisualClosure,
    node_id: PreparedVisualFrameNodeId,
    mut target: SequenceRenderTarget<'_>,
) -> Result<(), String> {
    if context.cancellation.is_canceled() {
        return Err("export visual execution canceled".to_owned());
    }
    let node = export_visual_node(closure, node_id)?;
    let materialization = node.materialization_contract();
    let author_resolution = materialization.author_resolution();
    let resolution = node.execution_resolution();
    let color_context = node.color_context().clone();
    let Resolution { width, height } = resolution;
    let media_dependencies = context.media;

    let frame_contract = context.frame_contract;
    if let SequenceRenderTarget::Deliverable(canvas) = &mut target {
        let required_len = frame_contract.canvas_len(width, height);
        if canvas.len() != required_len {
            canvas.resize(required_len, 0);
        }
    }

    let render_plan = node.evaluation().plan();
    if render_plan.is_empty() {
        finish_empty_sequence_target(
            &mut target,
            frame_contract,
            width,
            height,
            color_context.working_color_space,
            context.alpha_mode,
        );
        return Ok(());
    }
    let temporal_batches = node.evaluation().temporal_batches();
    let heterogeneous = node.evaluation().payload();

    let mut decode_cache = HashMap::<ExportDecodeCacheKey, Arc<DecodedVideoLayer>>::with_capacity(
        render_plan.len().saturating_mul(2),
    );
    let temporal_generation = context.visual_session.effect_execution_generation;
    context
        .visual_session
        .composite_scratch
        .bind_effect_execution_generation(temporal_generation);
    let temporal_layers = resolve_export_temporal_batches(
        context,
        closure,
        node_id,
        temporal_batches,
        &mut decode_cache,
    )?;
    let mut decoded_media =
        std::iter::repeat_with(|| None).take(render_plan.len()).collect::<Vec<_>>();
    let mut nested_media = std::iter::repeat_with(|| None)
        .take(render_plan.len())
        .collect::<Vec<Option<CpuColorFrame>>>();
    let mut title_media = std::iter::repeat_with(|| None)
        .take(render_plan.len())
        .collect::<Vec<Option<ResolvedExportTitle>>>();
    let mut transition_inputs = std::iter::repeat_with(|| None)
        .take(render_plan.len())
        .collect::<Vec<Option<(ResolvedExportTransitionInput, ResolvedExportTransitionInput)>>>();

    for (index, element) in render_plan.elements.iter().enumerate() {
        match element {
            TimelineRenderPlanElement::Media(media) => {
                if !temporal_layers.contains_key(&media.placement) {
                    decoded_media[index] = Some(decode_export_media_plan(
                        media_dependencies,
                        media,
                        width,
                        height,
                        &color_context,
                        &mut decode_cache,
                        context.input_color_counts.as_deref_mut(),
                        context.stage_diagnostics.as_deref_mut(),
                        context.visual_session,
                        context.cancellation,
                    )?);
                }
            }
            TimelineRenderPlanElement::BasicTitle(title) => {
                title_media[index] = Some(render_export_basic_title_plan(
                    context.visual_session,
                    materialization,
                    title,
                    width,
                    height,
                    color_context.working_color_space,
                )?);
            }
            TimelineRenderPlanElement::CrossDissolve(transition) => {
                let left = resolve_export_transition_input(
                    context,
                    closure,
                    node_id,
                    materialization,
                    &transition.left,
                    resolution,
                    &color_context,
                    &mut decode_cache,
                    &temporal_layers,
                )?;
                let right = resolve_export_transition_input(
                    context,
                    closure,
                    node_id,
                    materialization,
                    &transition.right,
                    resolution,
                    &color_context,
                    &mut decode_cache,
                    &temporal_layers,
                )?;
                transition_inputs[index] = Some((left, right));
            }
            TimelineRenderPlanElement::Adjustment(_)
            | TimelineRenderPlanElement::SolidColor(_)
            | TimelineRenderPlanElement::NestedSequence(_) => {}
        }
    }

    for (index, element) in render_plan.elements.iter().enumerate() {
        let TimelineRenderPlanElement::NestedSequence(nested) = element else {
            continue;
        };
        if !temporal_layers.contains_key(&nested.placement) {
            nested_media[index] = Some(materialize_export_nested_node(
                context,
                closure,
                node_id,
                nested.placement,
                PreparedVisualNestedSample::Current,
                &color_context,
            )?);
        }
    }

    let mut heterogeneous_media = std::iter::repeat_with(|| None)
        .take(render_plan.len())
        .collect::<Vec<Option<CpuColorFrame>>>();
    for route in heterogeneous {
        let element = render_plan.elements.get(route.element_index).ok_or_else(|| {
            format!(
                "heterogeneous route references missing render-plan element {}",
                route.element_index
            )
        })?;
        let input = match element {
            TimelineRenderPlanElement::Media(media) => {
                if let Some(temporal) = temporal_layers.get(&media.placement) {
                    HeterogeneousCpuPrefixSource::working_frame(temporal.frame.clone())
                } else {
                    HeterogeneousCpuPrefixSource::working_frame(
                        decoded_media[route.element_index]
                            .as_ref()
                            .ok_or_else(|| {
                                "heterogeneous media plan was not resolved before Effect execution"
                                    .to_owned()
                            })?
                            .frame
                            .clone(),
                    )
                }
            }
            TimelineRenderPlanElement::BasicTitle(_) => {
                HeterogeneousCpuPrefixSource::working_frame(
                    title_media[route.element_index]
                        .as_ref()
                        .ok_or_else(|| {
                            "heterogeneous Basic Title was not resolved before Effect execution"
                                .to_owned()
                        })?
                        .frame
                        .clone(),
                )
            }
            TimelineRenderPlanElement::NestedSequence(nested) => {
                if let Some(temporal) = temporal_layers.get(&nested.placement) {
                    HeterogeneousCpuPrefixSource::working_frame(temporal.frame.clone())
                } else {
                    HeterogeneousCpuPrefixSource::working_frame(
                        nested_media[route.element_index].as_ref().ok_or_else(|| {
                            "heterogeneous nested Sequence was not resolved before Effect execution"
                                .to_owned()
                        })?.clone(),
                    )
                }
            }
            TimelineRenderPlanElement::SolidColor(solid) => {
                HeterogeneousCpuPrefixSource::solid_color(route.route.frame_extent(), solid.color)
            }
            TimelineRenderPlanElement::Adjustment(_)
            | TimelineRenderPlanElement::CrossDissolve(_) => {
                return Err(format!(
                    "unsupported heterogeneous route escaped preflight at {}",
                    route.placement.label()
                ));
            }
        };
        let output = context
            .visual_session
            .execute_heterogeneous_element(
                route,
                input,
                color_context.working_color_space,
                context.cancellation,
            )
            .map_err(|error| error.to_string())?;
        let slot = heterogeneous_media.get_mut(route.element_index).ok_or_else(|| {
            format!(
                "heterogeneous output references missing render-plan element {}",
                route.element_index
            )
        })?;
        *slot = Some(output);
    }

    let mut composite_elements = Vec::with_capacity(render_plan.len());
    for (index, element) in render_plan.elements.iter().enumerate() {
        match element {
            TimelineRenderPlanElement::Adjustment(adjustment) => {
                composite_elements.push(TimelineCompositeElement::Adjustment(
                    TimelineAdjustmentLayer {
                        effect_graph: adjustment.effect_graph.clone(),
                        opacity: adjustment.opacity,
                        blend_mode: Some(adjustment.blend_mode),
                        frame_seed: adjustment.frame_seed,
                    },
                ));
            }
            TimelineRenderPlanElement::Media(media) => {
                let (resolved_frame, source_resolution) =
                    if let Some(temporal) = temporal_layers.get(&media.placement) {
                        (&temporal.frame, temporal.source_resolution)
                    } else {
                        let decoded = decoded_media[index].as_ref().ok_or_else(|| {
                            "media plan was not resolved before compositing".to_owned()
                        })?;
                        (&decoded.frame, decoded.source_resolution)
                    };
                let frame = heterogeneous_media[index].as_ref().unwrap_or(resolved_frame);
                let transform = project_export_affine(
                    media.transform,
                    source_resolution,
                    decoded_frame_resolution(frame),
                    author_resolution,
                    resolution,
                    "media",
                )?;
                composite_elements.push(TimelineCompositeElement::Media(TimelineMediaLayer {
                    frame,
                    opacity: media.opacity,
                    blend_mode: media.blend_mode,
                    transform,
                    effect_graph: media.effect_graph.clone(),
                    frame_seed: media.frame_seed,
                }));
            }
            TimelineRenderPlanElement::BasicTitle(title) => {
                let resolved = title_media[index].as_ref().ok_or_else(|| {
                    "Basic Title plan was not resolved before compositing".to_owned()
                })?;
                let frame = heterogeneous_media[index].as_ref().unwrap_or(&resolved.frame);
                composite_elements.push(TimelineCompositeElement::Media(TimelineMediaLayer {
                    frame,
                    opacity: title.opacity,
                    blend_mode: title.blend_mode,
                    transform: resolved.transform,
                    effect_graph: title.effect_graph.clone(),
                    frame_seed: title.frame_seed,
                }));
            }
            TimelineRenderPlanElement::NestedSequence(nested) => {
                let (resolved_frame, source_resolution) =
                    if let Some(temporal) = temporal_layers.get(&nested.placement) {
                        (&temporal.frame, temporal.source_resolution)
                    } else {
                        let frame = nested_media[index].as_ref().ok_or_else(|| {
                            "nested-Sequence plan was not resolved before compositing".to_owned()
                        })?;
                        let child_id = export_nested_child(
                            closure,
                            node_id,
                            nested.placement,
                            PreparedVisualNestedSample::Current,
                        )?;
                        let source_resolution =
                            export_visual_node(closure, child_id)?.author_resolution();
                        (frame, source_resolution)
                    };
                let frame = heterogeneous_media[index].as_ref().unwrap_or(resolved_frame);
                let transform = project_export_affine(
                    nested.transform,
                    source_resolution,
                    decoded_frame_resolution(frame),
                    author_resolution,
                    resolution,
                    "nested Sequence",
                )?;
                composite_elements.push(TimelineCompositeElement::Media(TimelineMediaLayer {
                    frame,
                    opacity: nested.opacity,
                    blend_mode: nested.blend_mode,
                    transform,
                    effect_graph: nested.effect_graph.clone(),
                    frame_seed: nested.frame_seed,
                }));
            }
            TimelineRenderPlanElement::SolidColor(solid) => {
                let transform = project_export_affine(
                    solid.transform,
                    author_resolution,
                    resolution,
                    author_resolution,
                    resolution,
                    "solid color",
                )?;
                if let Some(temporal) = temporal_layers.get(&solid.placement) {
                    composite_elements.push(TimelineCompositeElement::Media(TimelineMediaLayer {
                        frame: &temporal.frame,
                        opacity: solid.opacity,
                        blend_mode: solid.blend_mode,
                        transform,
                        effect_graph: solid.effect_graph.clone(),
                        frame_seed: solid.frame_seed,
                    }));
                } else {
                    composite_elements.push(TimelineCompositeElement::SolidColor(
                        TimelineSolidColorLayer {
                            color: solid.color,
                            opacity: solid.opacity,
                            blend_mode: solid.blend_mode,
                            transform,
                            effect_graph: solid.effect_graph.clone(),
                            frame_seed: solid.frame_seed,
                        },
                    ));
                }
            }
            TimelineRenderPlanElement::CrossDissolve(transition) => {
                let Some((left, right)) = transition_inputs[index].as_ref() else {
                    return Err("Cross Dissolve inputs were not resolved".to_owned());
                };
                composite_elements.push(TimelineCompositeElement::CrossDissolve(
                    TimelineCrossDissolveLayer {
                        left: lower_export_transition_input(
                            closure,
                            node_id,
                            materialization,
                            resolution,
                            &transition.left,
                            left,
                        )?,
                        right: lower_export_transition_input(
                            closure,
                            node_id,
                            materialization,
                            resolution,
                            &transition.right,
                            right,
                        )?,
                        progress: transition.progress,
                    },
                ));
            }
        }
    }

    if composite_elements.is_empty() {
        finish_empty_sequence_target(
            &mut target,
            frame_contract,
            width,
            height,
            color_context.working_color_space,
            context.alpha_mode,
        );
        return Ok(());
    }

    let effect_execution_generation = context.visual_session.effect_execution_generation;
    context
        .visual_session
        .composite_scratch
        .bind_effect_execution_generation(effect_execution_generation);
    let composite_options = if matches!(&target, SequenceRenderTarget::Deliverable(_))
        && context.alpha_mode == ExportAlphaMode::FlattenBlack
    {
        TimelineCompositeOptions::opaque_black()
    } else {
        TimelineCompositeOptions::default()
    };
    let rendered = composite_timeline_elements_color_frame_with_diagnostics(
        width,
        height,
        &composite_elements,
        composite_options,
        TimelineEffectColorRuntime::new(&color_context.engine, color_context.working_color_space),
        &mut context.visual_session.composite_scratch,
    )
    .map_err(|error| format!("timeline composite failed: {error}"))?;
    if let Some(diagnostics) = context.composite_diagnostics.as_deref_mut() {
        diagnostics.accumulate(rendered.diagnostics);
    }
    if rendered.diagnostics.uses_legacy_rgba8() {
        let breakdown = rendered.diagnostics.legacy_breakdown();
        return Err(format!(
            "final export working composite failed closed; renderer selected the legacy RGBA8 route (media_effect={}, solid_effect={}, adjustment_effect={}, other={})",
            breakdown.media_effect,
            breakdown.solid_effect,
            breakdown.adjustment_effect,
            breakdown
                .total()
                .saturating_sub(breakdown.media_effect)
                .saturating_sub(breakdown.solid_effect)
                .saturating_sub(breakdown.adjustment_effect),
        ));
    }

    let canvas = match target {
        SequenceRenderTarget::Working(output) => {
            *output = Some(rendered.frame);
            return Ok(());
        }
        SequenceRenderTarget::Deliverable(canvas) => canvas,
    };

    let mut gpu_output_fallback_reasons = ExportGpuOutputFallbackBreakdown::default();
    let mut gpu_output_attempts = 0u64;
    let mut gpu_output_cpu_fallbacks = 0u64;
    let boundary = export_output_boundary_from_context(&color_context)?;
    if color_context.output_tone_map
        && boundary.display_view.is_none()
        && let Some(diagnostics) = context.export_diagnostics.as_deref_mut()
    {
        diagnostics.record_output_transform_issue(
            ExportOutputTransformIssueReason::ToneMapRequestedWithoutExportViewTransform,
        );
    }
    let attempt = match context.visual_session.gpu_output.execute(
        &rendered.frame,
        &boundary,
        frame_contract,
        context.cancellation,
    ) {
        Ok(attempt) => Some(attempt),
        Err(ExportGpuOutputExecutionError::Canceled) => {
            return Err("export GPU output readback canceled".to_owned());
        }
        Err(ExportGpuOutputExecutionError::DeviceTimedOut) => {
            gpu_output_cpu_fallbacks = gpu_output_cpu_fallbacks.saturating_add(1);
            gpu_output_fallback_reasons = gpu_output_fallback_reasons
                .add_reason(ExportGpuOutputFallbackReason::ReadbackTimedOut);
            None
        }
        Err(ExportGpuOutputExecutionError::Fallback(reason)) => {
            gpu_output_cpu_fallbacks = gpu_output_cpu_fallbacks.saturating_add(1);
            gpu_output_fallback_reasons = gpu_output_fallback_reasons.add_reason(reason);
            None
        }
    };
    gpu_output_attempts = gpu_output_attempts.saturating_add(1);

    let final_bytes = match attempt {
        Some(attempt) => {
            if let Some(diagnostics) = context.stage_diagnostics.as_deref_mut() {
                diagnostics.accumulate(attempt.stage_diagnostics);
            }
            attempt.rgba
        }
        None => {
            if frame_contract.requires_high_precision_boundary() {
                match cpu_output_boundary_float(
                    &rendered.frame,
                    &boundary,
                    context.visual_session.composite_scratch.color_execution_mut(),
                ) {
                    Ok(float_result) => {
                        if let Some(diagnostics) = context.stage_diagnostics.as_deref_mut() {
                            diagnostics.accumulate(float_result.stage_diagnostics);
                        }
                        let flat: Vec<f32> = float_result
                            .frame
                            .rgba_f32()
                            .data
                            .iter()
                            .flat_map(|px| px.iter().copied())
                            .collect();
                        frame_contract.pack_rgba_f32(&flat)
                    }
                    Err(float_err) => {
                        if let Some(diagnostics) = context.export_diagnostics.as_deref_mut() {
                            diagnostics.record_export_output_boundary(
                                gpu_output_attempts,
                                gpu_output_cpu_fallbacks,
                                gpu_output_fallback_reasons,
                            );
                            diagnostics.record_output_precision_failure(
                                ExportOutputPrecisionFailureReason::FloatBoundaryUnavailable,
                            );
                        }
                        return Err(format!(
                            "high-precision export output boundary failed closed; refusing RGBA8 downgrade: {float_err}"
                        ));
                    }
                }
            } else {
                let encoded = execute_cpu_output_boundary_rgba8_with_session(
                    &rendered.frame,
                    &boundary,
                    context.visual_session.composite_scratch.color_execution_mut(),
                )
                .map_err(|err| format!("final color transform failed: {err}"))?;
                if let Some(diagnostics) = context.stage_diagnostics.as_deref_mut() {
                    diagnostics.accumulate(encoded.stage_diagnostics);
                }
                frame_contract.pack_rgba8(&encoded.rgba)
            }
        }
    };

    if let Some(diagnostics) = context.export_diagnostics.as_deref_mut() {
        diagnostics.record_export_output_boundary(
            gpu_output_attempts,
            gpu_output_cpu_fallbacks,
            gpu_output_fallback_reasons,
        );
    }
    canvas.clear();
    canvas.extend_from_slice(&final_bytes);
    Ok(())
}

fn decode_export_media_plan(
    media_dependencies: &HashMap<AssetId, crate::preset::ExportMediaDependency>,
    media: &TimelineMediaPlan,
    width: u32,
    height: u32,
    color_context: &ProgramColorContext,
    cache: &mut HashMap<ExportDecodeCacheKey, Arc<DecodedVideoLayer>>,
    input_color_counts: Option<&mut InputColorResolutionSourceCounts>,
    stage_diagnostics: Option<&mut RenderColorStageDiagnostics>,
    visual_session: &mut ExportVisualRenderSession,
    cancellation: &ExecutionCancellationToken,
) -> Result<Arc<DecodedVideoLayer>, String> {
    let dependency = media_dependencies
        .get(&media.asset_id)
        .ok_or_else(|| format!("导出快照缺少素材依赖: {}", media.asset_id))?;
    let input_color_resolution =
        color_context.missing_metadata_policy.resolve_asset_input_decision(
            media.color_space_override,
            dependency.interpretation,
            dependency
                .color_diagnostic
                .as_ref()
                .and_then(mondrian_media::VideoColorDiagnostic::executable_color_space),
            color_context.working_color_space,
        );
    if let Some(counts) = input_color_counts {
        counts.record(input_color_resolution.source);
    }
    let input_color_space = match input_color_resolution.resolved {
        ResolvedInputColor::Color(color_space) => color_space,
        ResolvedInputColor::Data | ResolvedInputColor::Rejected => {
            let diagnostic = dependency
                .color_diagnostic
                .as_ref()
                .map(mondrian_media::VideoColorDiagnostic::summary)
                .unwrap_or_else(|| "unavailable".to_string());
            return Err(format!(
                "asset={} path={} missing color metadata rejected by sequence policy {:?}; resolution={:?} override={:?} detected={:?} working={:?}; {}",
                media.asset_id,
                dependency.path.display(),
                color_context.missing_metadata_policy,
                input_color_resolution.source,
                input_color_resolution.override_color_space,
                input_color_resolution.executable_color_space,
                input_color_resolution.working_color_space,
                diagnostic
            ));
        }
    };
    let input_video_range = resolve_export_input_video_range(
        media_dependencies,
        media.asset_id,
        dependency.interpretation,
    );
    let source_resolution = dependency.source_resolution.ok_or_else(|| {
        format!(
            "asset={} export snapshot has no source extent",
            media.asset_id
        )
    })?;
    let key = ExportDecodeCacheKey::new(
        media.asset_id,
        dependency,
        media.source_sample,
        input_color_space,
        input_video_range,
        media.alpha_interpretation,
        color_context,
        media.auto_tone_map,
        Resolution { width, height },
        source_resolution,
    )?;
    if let Some(decoded) = cache.get(&key) {
        if decoded.source_fingerprint != dependency.source_fingerprint
            || decoded.video_stream_index != key.video_stream_index
        {
            return Err(format!(
                "asset={} export decode cache evidence diverged from admitted source revision",
                media.asset_id
            ));
        }
        return Ok(Arc::clone(decoded));
    }
    let decoded = decode_video_layer_scaled(
        ExportVideoLayerDecodeRequest {
            asset_id: media.asset_id,
            dependency,
            source_sample: media.source_sample,
            decode_resolution: Resolution { width, height },
            source_resolution,
            source_color: PreviewSourceColorContract::new(input_color_space, input_video_range),
            alpha_interpretation: media.alpha_interpretation,
            input_transform: RenderInputTransform::to_working(
                color_context.working_color_space,
                media.auto_tone_map,
                color_context.engine.clone(),
            ),
        },
        ExportVideoLayerDecodeExecutionContext {
            color_session: visual_session.composite_scratch.color_execution_mut(),
            decode_context: &mut visual_session.decode_context,
            cancellation,
        },
    )?;
    if decoded.source_fingerprint != key.source_fingerprint
        || decoded.video_stream_index != key.video_stream_index
    {
        return Err(format!(
            "asset={} export decode result did not prove the cache-authorizing source revision",
            media.asset_id
        ));
    }
    if let Some(diagnostics) = stage_diagnostics {
        diagnostics.accumulate(decoded.stage_diagnostics);
    }
    cache.insert(key, Arc::clone(&decoded));
    Ok(decoded)
}

fn materialize_export_nested_node(
    context: &mut ExportFrameRenderContext<'_>,
    closure: &PreparedExportVisualClosure,
    parent_node_id: PreparedVisualFrameNodeId,
    placement: TimelineClipExecutionRef,
    sample: PreparedVisualNestedSample,
    parent_color_context: &ProgramColorContext,
) -> Result<CpuColorFrame, String> {
    let child_id = export_nested_child(closure, parent_node_id, placement, sample)?;
    let child_node = export_visual_node(closure, child_id)?;
    let child_sequence_id = child_node.sequence_id();
    let mut output = None;
    render_prepared_visual_node_into(
        context,
        closure,
        child_id,
        SequenceRenderTarget::Working(&mut output),
    )?;
    let mut frame = output.ok_or_else(|| {
        format!(
            "nested sequence produced no working frame: {}",
            child_sequence_id
        )
    })?;
    if frame.descriptor().color_space.working() != Some(parent_color_context.working_color_space) {
        let converted = execute_cpu_working_transform_with_session(
            &frame,
            parent_color_context.working_color_space,
            parent_color_context.engine.clone(),
            context.visual_session.composite_scratch.color_execution_mut(),
        )
        .map_err(|error| format!("nested working-space transform failed: {error}"))?;
        if let Some(diagnostics) = context.stage_diagnostics.as_deref_mut() {
            diagnostics.accumulate(converted.stage_diagnostics);
        }
        frame = converted.result.frame;
    }
    Ok(frame)
}

fn render_export_basic_title_plan(
    visual_session: &mut ExportVisualRenderSession,
    materialization: PreparedVisualMaterializationContract,
    title: &TimelineBasicTitlePlan,
    width: u32,
    height: u32,
    working_color_space: WorkingColorSpace,
) -> Result<ResolvedExportTitle, String> {
    let target_resolution = mondrian_core::Resolution { width, height };
    let author_resolution = materialization.author_resolution();
    let raster = visual_session
        .title_rasterizer
        .rasterize(
            &title.title,
            author_resolution,
            materialization.title_safe_margin(),
            target_resolution,
            working_color_space,
        )
        .map_err(|error| format!("Basic Title generation failed closed: {error}"))?;
    let transform = project_basic_title_transform(
        title.transform,
        raster.sampled_source_to_author(),
        author_resolution,
        target_resolution,
    )
    .ok_or_else(|| "Basic Title export transform geometry is invalid".to_owned())?;
    Ok(ResolvedExportTitle { frame: raster.into_frame(), transform })
}

fn resolve_export_temporal_batches(
    context: &mut ExportFrameRenderContext<'_>,
    closure: &PreparedExportVisualClosure,
    node_id: PreparedVisualFrameNodeId,
    batches: &[TimelineTemporalDemandBatch],
    decode_cache: &mut HashMap<ExportDecodeCacheKey, Arc<DecodedVideoLayer>>,
) -> Result<HashMap<TimelineClipExecutionRef, PreparedExportTemporalLayer>, String> {
    let node = export_visual_node(closure, node_id)?;
    let materialization = node.materialization_contract();
    let color_context = node.color_context().clone();
    let resolution = node.execution_resolution();
    let mut layers = HashMap::with_capacity(batches.len());
    for batch in batches {
        if context.cancellation.is_canceled() {
            return Err("export temporal dependency preparation was canceled".to_owned());
        }
        let mut source_resolution = None;
        let mut resolved = Vec::with_capacity(batch.source_demands().len());
        for demand in batch.source_demands() {
            let (frame, current_source_resolution) = resolve_export_temporal_source(
                context,
                closure,
                node_id,
                materialization,
                &color_context,
                resolution,
                batch,
                demand,
                decode_cache,
            )?;
            if source_resolution
                .replace(current_source_resolution)
                .is_some_and(|previous| previous != current_source_resolution)
            {
                return Err(format!(
                    "Clip {} temporal requests resolved with inconsistent authoring extents",
                    batch.placement().clip_id
                ));
            }
            resolved.push((
                demand.effect_request,
                export_temporal_tile(&frame, demand.effect_request)?,
            ));
        }
        let source_identity =
            export_temporal_source_identity(context, closure, batch, &color_context)?;
        let mut prepared = PreparedTemporalFrameSet::prepare(
            source_identity,
            batch.effect_demands().clone(),
            resolved,
        )
        .map_err(|error| format!("export temporal frame set is invalid: {error}"))?;
        let output = context
            .visual_session
            .composite_scratch
            .execute_prepared_temporal_batch(batch, &mut prepared)
            .map_err(|error| format!("export temporal Effect execution failed: {error}"))?;
        let tile = output.tile();
        if tile.roi() != tile.frame_extent().full_frame_roi() {
            return Err(
                "export temporal execution did not produce the required full-frame result"
                    .to_owned(),
            );
        }
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: tile.frame_extent().width(),
            height: tile.frame_extent().height(),
            data: tile.pixels().to_vec(),
            color_space: color_context.working_color_space,
        });
        let layer = PreparedExportTemporalLayer {
            frame,
            source_resolution: source_resolution.unwrap_or(resolution),
        };
        if layers.insert(batch.placement(), layer).is_some() {
            return Err(format!(
                "Clip {} produced more than one temporal execution batch",
                batch.placement().clip_id
            ));
        }
    }
    Ok(layers)
}

#[allow(clippy::too_many_arguments)]
fn resolve_export_temporal_source(
    context: &mut ExportFrameRenderContext<'_>,
    closure: &PreparedExportVisualClosure,
    parent_node_id: PreparedVisualFrameNodeId,
    materialization: PreparedVisualMaterializationContract,
    color_context: &ProgramColorContext,
    resolution: Resolution,
    batch: &TimelineTemporalDemandBatch,
    demand: &mondrian_renderer::TimelineTemporalSourceDemand,
    decode_cache: &mut HashMap<ExportDecodeCacheKey, Arc<DecodedVideoLayer>>,
) -> Result<(CpuColorFrame, Resolution), String> {
    if context.cancellation.is_canceled() {
        return Err("export temporal source resolution was canceled".to_owned());
    }
    let identity = identity_compiled_effect_graph()
        .ok_or_else(|| "renderer could not prepare the identity Effect graph".to_owned())?;
    let (frame, source_resolution) = match &demand.source {
        TimelineTemporalSource::Media {
            asset_id,
            source_sample,
            color_space_override,
            alpha_interpretation,
            auto_tone_map,
        } => {
            let plan = TimelineMediaPlan {
                placement: demand.placement,
                asset_id: *asset_id,
                color_space_override: *color_space_override,
                pixel_aspect_ratio_override: None,
                field_order_override: None,
                alpha_interpretation: *alpha_interpretation,
                frame_rate_override: None,
                source_sample: *source_sample,
                opacity: 1.0,
                blend_mode: mondrian_core::BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: Arc::clone(&identity),
                frame_seed: demand.effect_request.time().numerator(),
                auto_tone_map: *auto_tone_map,
            };
            let decoded = decode_export_media_plan(
                context.media,
                &plan,
                resolution.width,
                resolution.height,
                color_context,
                decode_cache,
                context.input_color_counts.as_deref_mut(),
                context.stage_diagnostics.as_deref_mut(),
                context.visual_session,
                context.cancellation,
            )?;
            (decoded.frame.clone(), decoded.source_resolution)
        }
        TimelineTemporalSource::NestedSequence { sequence_id, .. } => {
            let child_id = export_nested_child(
                closure,
                parent_node_id,
                demand.placement,
                PreparedVisualNestedSample::Temporal(demand.effect_request),
            )?;
            let child_node = export_visual_node(closure, child_id)?;
            if child_node.sequence_id() != *sequence_id {
                return Err(format!(
                    "prepared temporal binding for Clip {} resolved Sequence {}, expected {sequence_id}",
                    demand.placement.clip_id,
                    child_node.sequence_id()
                ));
            }
            let child_resolution = child_node.execution_resolution();
            let child_author_resolution = child_node.author_resolution();
            if child_resolution != resolution {
                return Err(format!(
                    "nested Sequence {sequence_id} temporal source extent {:?} differs from admitted Effect extent {:?}; resampling before Effects is not permitted",
                    child_resolution, resolution
                ));
            }
            (
                materialize_export_nested_node(
                    context,
                    closure,
                    parent_node_id,
                    demand.placement,
                    PreparedVisualNestedSample::Temporal(demand.effect_request),
                    color_context,
                )?,
                child_author_resolution,
            )
        }
        TimelineTemporalSource::SolidColor { color } => {
            let extent = demand.effect_request.frame_extent();
            let pixel = [color.r, color.g, color.b, color.a];
            (
                CpuColorFrame::working(WorkingRgbaF32Frame {
                    width: extent.width(),
                    height: extent.height(),
                    data: vec![pixel; extent.width() as usize * extent.height() as usize],
                    color_space: color_context.working_color_space,
                }),
                materialization.author_resolution(),
            )
        }
    };
    let descriptor = frame.descriptor();
    let expected = demand.effect_request.frame_extent();
    if descriptor.width != expected.width() || descriptor.height != expected.height() {
        return Err(format!(
            "Clip {} temporal source resolved to {}x{}, expected {}x{}",
            batch.placement().clip_id,
            descriptor.width,
            descriptor.height,
            expected.width(),
            expected.height()
        ));
    }
    if descriptor.color_space.working() != Some(color_context.working_color_space)
        || descriptor.alpha != mondrian_renderer::ColorFrameAlpha::StraightCoverage
    {
        return Err(format!(
            "Clip {} temporal source did not resolve to parent working-space straight alpha",
            batch.placement().clip_id
        ));
    }
    Ok((frame, source_resolution))
}

fn export_temporal_tile(
    frame: &CpuColorFrame,
    request: mondrian_effects::EffectTemporalFrameRequest,
) -> Result<EffectFrameTileF32, String> {
    let roi = request.input_roi().region();
    let source = frame.rgba_f32();
    let mut pixels = Vec::with_capacity(roi.width() as usize * roi.height() as usize);
    for y in roi.y()..roi.y().saturating_add(roi.height()) {
        let start = y as usize * source.width as usize + roi.x() as usize;
        let end = start.saturating_add(roi.width() as usize);
        let row = source
            .data
            .get(start..end)
            .ok_or_else(|| "temporal ROI exceeded the resolved export frame".to_owned())?;
        pixels.extend_from_slice(row);
    }
    EffectFrameTileF32::new(
        request.time(),
        request.frame_extent(),
        roi,
        request.time().numerator(),
        pixels,
    )
    .map_err(|error| error.to_string())
}

fn export_temporal_source_identity(
    context: &ExportFrameRenderContext<'_>,
    closure: &PreparedExportVisualClosure,
    batch: &TimelineTemporalDemandBatch,
    color_context: &ProgramColorContext,
) -> Result<EffectTemporalSourceIdentity, String> {
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.export-temporal-source.v2");
    hasher.update(batch.graph().semantic_fingerprint());
    hasher.update(batch.placement().sequence_id.0.as_bytes());
    hasher.update(batch.placement().sequence_revision.get().to_le_bytes());
    hasher.update(batch.placement().clip_id.0.as_bytes());
    hasher.update(batch.placement().clip_time.numerator().to_le_bytes());
    hasher.update(batch.placement().clip_time.denominator().to_le_bytes());
    match batch.placement().endpoint {
        mondrian_core::timeline_data::TimelineClipEndpointContext::Ordinary => {
            hasher.update([0]);
        }
        mondrian_core::timeline_data::TimelineClipEndpointContext::TransitionLeft {
            transition_id,
        } => {
            hasher.update([1]);
            hasher.update(transition_id.0.as_bytes());
        }
        mondrian_core::timeline_data::TimelineClipEndpointContext::TransitionRight {
            transition_id,
        } => {
            hasher.update([2]);
            hasher.update(transition_id.0.as_bytes());
        }
    }
    hasher.update(batch.execution_request().output_frame_seed().to_le_bytes());
    hasher.update(
        serde_json::to_vec(&color_context.working_color_space)
            .map_err(|error| error.to_string())?,
    );
    hasher
        .update(serde_json::to_vec(context.color_environment).map_err(|error| error.to_string())?);

    let mut programs = closure
        .nodes()
        .iter()
        .map(|node| {
            (
                node.sequence_id(),
                node.sequence_revision(),
                node.program().visual_author_fingerprint(),
                node.program().effect_registry_revision(),
            )
        })
        .collect::<Vec<_>>();
    programs.sort_by_key(|(sequence_id, revision, fingerprint, registry_revision)| {
        (
            sequence_id.to_string(),
            revision.get(),
            *fingerprint,
            *registry_revision,
        )
    });
    programs.dedup();
    for (sequence_id, revision, fingerprint, registry_revision) in programs {
        hasher.update(sequence_id.0.as_bytes());
        hasher.update(revision.get().to_le_bytes());
        hasher.update(fingerprint);
        hasher.update(registry_revision.to_le_bytes());
    }
    let mut media = context.media.iter().collect::<Vec<_>>();
    media.sort_by_key(|(asset_id, _)| asset_id.to_string());
    for (asset_id, dependency) in media {
        hasher.update(asset_id.0.as_bytes());
        hasher.update(serde_json::to_vec(dependency).map_err(|error| error.to_string())?);
    }
    for demand in batch.source_demands() {
        hasher.update(demand.effect_request.time().numerator().to_le_bytes());
        hasher.update(demand.effect_request.time().denominator().to_le_bytes());
        match &demand.source {
            TimelineTemporalSource::Media {
                asset_id,
                source_sample,
                color_space_override,
                alpha_interpretation,
                auto_tone_map,
            } => {
                hasher.update([0]);
                hasher.update(asset_id.0.as_bytes());
                hasher.update(source_sample.time().numerator().to_le_bytes());
                hasher.update(source_sample.time().denominator().to_le_bytes());
                hasher.update([match source_sample.boundary() {
                    mondrian_core::SourceSamplingBoundary::Covering => 0,
                    mondrian_core::SourceSamplingBoundary::StrictPredecessor => 1,
                }]);
                hasher.update(
                    serde_json::to_vec(color_space_override).map_err(|error| error.to_string())?,
                );
                hasher.update(
                    serde_json::to_vec(alpha_interpretation).map_err(|error| error.to_string())?,
                );
                hasher.update([u8::from(*auto_tone_map)]);
            }
            TimelineTemporalSource::NestedSequence {
                sequence_id,
                source_sample,
                color_processing,
            } => {
                hasher.update([1]);
                hasher.update(sequence_id.0.as_bytes());
                hasher.update(source_sample.time().numerator().to_le_bytes());
                hasher.update(source_sample.time().denominator().to_le_bytes());
                hasher.update([match source_sample.boundary() {
                    mondrian_core::SourceSamplingBoundary::Covering => 0,
                    mondrian_core::SourceSamplingBoundary::StrictPredecessor => 1,
                }]);
                hasher.update(
                    serde_json::to_vec(color_processing).map_err(|error| error.to_string())?,
                );
            }
            TimelineTemporalSource::SolidColor { color } => {
                hasher.update([2]);
                for component in [color.r, color.g, color.b, color.a] {
                    hasher.update(component.to_bits().to_le_bytes());
                }
            }
        }
    }
    Ok(EffectTemporalSourceIdentity::from_complete_semantic_fingerprint(hasher.finalize().into()))
}

fn resolve_export_transition_input(
    context: &mut ExportFrameRenderContext<'_>,
    closure: &PreparedExportVisualClosure,
    parent_node_id: PreparedVisualFrameNodeId,
    materialization: PreparedVisualMaterializationContract,
    input: &TimelineTransitionInputPlan,
    resolution: Resolution,
    color_context: &ProgramColorContext,
    decode_cache: &mut HashMap<ExportDecodeCacheKey, Arc<DecodedVideoLayer>>,
    temporal_layers: &HashMap<TimelineClipExecutionRef, PreparedExportTemporalLayer>,
) -> Result<ResolvedExportTransitionInput, String> {
    let Resolution { width, height } = resolution;
    if let Some(placement) = transition_input_placement(input)
        && let Some(temporal) = temporal_layers.get(&placement)
    {
        return Ok(ResolvedExportTransitionInput::Temporal(temporal.clone()));
    }
    Ok(match input {
        TimelineTransitionInputPlan::Transparent => ResolvedExportTransitionInput::Transparent,
        TimelineTransitionInputPlan::SolidColor(_) => ResolvedExportTransitionInput::SolidColor,
        TimelineTransitionInputPlan::BasicTitle(title) => {
            ResolvedExportTransitionInput::BasicTitle(render_export_basic_title_plan(
                context.visual_session,
                materialization,
                title,
                width,
                height,
                color_context.working_color_space,
            )?)
        }
        TimelineTransitionInputPlan::Media(media) => {
            ResolvedExportTransitionInput::Decoded(decode_export_media_plan(
                context.media,
                media,
                width,
                height,
                color_context,
                decode_cache,
                context.input_color_counts.as_deref_mut(),
                context.stage_diagnostics.as_deref_mut(),
                context.visual_session,
                context.cancellation,
            )?)
        }
        TimelineTransitionInputPlan::NestedSequence(nested) => {
            ResolvedExportTransitionInput::Nested(materialize_export_nested_node(
                context,
                closure,
                parent_node_id,
                nested.placement,
                PreparedVisualNestedSample::Current,
                color_context,
            )?)
        }
    })
}

fn transition_input_placement(
    input: &TimelineTransitionInputPlan,
) -> Option<TimelineClipExecutionRef> {
    match input {
        TimelineTransitionInputPlan::Transparent => None,
        TimelineTransitionInputPlan::Media(media) => Some(media.placement),
        TimelineTransitionInputPlan::SolidColor(solid) => Some(solid.placement),
        TimelineTransitionInputPlan::BasicTitle(title) => Some(title.placement),
        TimelineTransitionInputPlan::NestedSequence(nested) => Some(nested.placement),
    }
}

fn decoded_frame_resolution(frame: &CpuColorFrame) -> Resolution {
    let descriptor = frame.descriptor();
    Resolution { width: descriptor.width, height: descriptor.height }
}

fn project_export_affine(
    transform: [f32; 6],
    source_authoring: Resolution,
    source_sampled: Resolution,
    output_authoring: Resolution,
    output_sampled: Resolution,
    source_kind: &str,
) -> Result<[f32; 6], String> {
    project_affine_to_sampled_extents(
        transform,
        source_authoring,
        source_sampled,
        output_authoring,
        output_sampled,
    )
    .ok_or_else(|| format!("{source_kind} export transform geometry is invalid"))
}

fn lower_export_transition_input<'a>(
    closure: &PreparedExportVisualClosure,
    parent_node_id: PreparedVisualFrameNodeId,
    materialization: PreparedVisualMaterializationContract,
    resolution: Resolution,
    plan: &'a TimelineTransitionInputPlan,
    resolved: &'a ResolvedExportTransitionInput,
) -> Result<TimelineTransitionInput<'a>, String> {
    let author_resolution = materialization.author_resolution();
    Ok(match (plan, resolved) {
        (TimelineTransitionInputPlan::Transparent, ResolvedExportTransitionInput::Transparent) => {
            TimelineTransitionInput::Transparent
        }
        (
            TimelineTransitionInputPlan::Media(media),
            ResolvedExportTransitionInput::Decoded(frame),
        ) => {
            let transform = project_export_affine(
                media.transform,
                frame.source_resolution,
                decoded_frame_resolution(&frame.frame),
                author_resolution,
                resolution,
                "Transition media",
            )?;
            TimelineTransitionInput::Media(TimelineMediaLayer {
                frame: &frame.frame,
                opacity: media.opacity,
                blend_mode: media.blend_mode,
                transform,
                effect_graph: media.effect_graph.clone(),
                frame_seed: media.frame_seed,
            })
        }
        (
            TimelineTransitionInputPlan::NestedSequence(nested),
            ResolvedExportTransitionInput::Nested(frame),
        ) => {
            let child_id = export_nested_child(
                closure,
                parent_node_id,
                nested.placement,
                PreparedVisualNestedSample::Current,
            )?;
            let source_resolution = export_visual_node(closure, child_id)?.author_resolution();
            let transform = project_export_affine(
                nested.transform,
                source_resolution,
                decoded_frame_resolution(frame),
                author_resolution,
                resolution,
                "Transition nested Sequence",
            )?;
            TimelineTransitionInput::Media(TimelineMediaLayer {
                frame,
                opacity: nested.opacity,
                blend_mode: nested.blend_mode,
                transform,
                effect_graph: nested.effect_graph.clone(),
                frame_seed: nested.frame_seed,
            })
        }
        (
            TimelineTransitionInputPlan::BasicTitle(title),
            ResolvedExportTransitionInput::BasicTitle(resolved),
        ) => TimelineTransitionInput::Media(TimelineMediaLayer {
            frame: &resolved.frame,
            opacity: title.opacity,
            blend_mode: title.blend_mode,
            transform: resolved.transform,
            effect_graph: title.effect_graph.clone(),
            frame_seed: title.frame_seed,
        }),
        (
            TimelineTransitionInputPlan::SolidColor(solid),
            ResolvedExportTransitionInput::SolidColor,
        ) => {
            let transform = project_export_affine(
                solid.transform,
                author_resolution,
                resolution,
                author_resolution,
                resolution,
                "Transition solid color",
            )?;
            TimelineTransitionInput::SolidColor(TimelineSolidColorLayer {
                color: solid.color,
                opacity: solid.opacity,
                blend_mode: solid.blend_mode,
                transform,
                effect_graph: solid.effect_graph.clone(),
                frame_seed: solid.frame_seed,
            })
        }
        (plan, ResolvedExportTransitionInput::Temporal(temporal)) => {
            let (opacity, blend_mode, transform) = match plan {
                TimelineTransitionInputPlan::Media(media) => {
                    (media.opacity, media.blend_mode, media.transform)
                }
                TimelineTransitionInputPlan::NestedSequence(nested) => {
                    (nested.opacity, nested.blend_mode, nested.transform)
                }
                TimelineTransitionInputPlan::SolidColor(solid) => {
                    (solid.opacity, solid.blend_mode, solid.transform)
                }
                TimelineTransitionInputPlan::BasicTitle(title) => {
                    (title.opacity, title.blend_mode, title.transform)
                }
                TimelineTransitionInputPlan::Transparent => {
                    return Err("transparent Transition input cannot own temporal pixels".to_owned())
                }
            };
            let transform = project_export_affine(
                transform,
                temporal.source_resolution,
                decoded_frame_resolution(&temporal.frame),
                author_resolution,
                resolution,
                "Transition temporal source",
            )?;
            TimelineTransitionInput::Media(TimelineMediaLayer {
                frame: &temporal.frame,
                opacity,
                blend_mode,
                transform,
                effect_graph: identity_compiled_effect_graph().ok_or_else(|| {
                    "renderer could not prepare the identity Effect graph".to_owned()
                })?,
                frame_seed: 0,
            })
        }
        _ => {
            return Err("Transition plan and resolved input diverged before compositing".to_owned())
        }
    })
}

fn resolve_export_input_video_range(
    media_dependencies: &HashMap<AssetId, crate::preset::ExportMediaDependency>,
    asset_id: AssetId,
    interpretation: mondrian_core::timeline_data::AssetMediaInterpretation,
) -> DecodedVideoRangeContract {
    let detected = media_dependencies
        .get(&asset_id)
        .and_then(|dependency| dependency.color_diagnostic.as_ref())
        .map(|diagnostic| diagnostic.color_range)
        .unwrap_or(DecodedVideoRange::Unknown);
    DecodedVideoRangeContract::from_interpretation(interpretation.range, detected)
}

fn finish_empty_sequence_target(
    target: &mut SequenceRenderTarget<'_>,
    frame_contract: ExportFrameContract,
    width: u32,
    height: u32,
    working_color_space: WorkingColorSpace,
    alpha_mode: ExportAlphaMode,
) {
    match target {
        SequenceRenderTarget::Working(output) => {
            **output = Some(CpuColorFrame::working(WorkingRgbaF32Frame {
                width,
                height,
                data: vec![[0.0, 0.0, 0.0, 0.0]; width as usize * height as usize],
                color_space: working_color_space,
            }));
        }
        SequenceRenderTarget::Deliverable(canvas) => match alpha_mode {
            ExportAlphaMode::FlattenBlack => {
                fill_canvas_black_opaque(canvas, frame_contract, width, height);
            }
            ExportAlphaMode::Preserve => canvas.fill(0),
        },
    }
}

/// Immutable author and delivery facts for one export still-frame decode.
///
/// The request owns the renderer input transform so media decode cannot
/// reinterpret working-space, tone-map, or color-engine policy while a job is
/// executing. Source revision and physical stream authority remain frozen in
/// `dependency`.
struct ExportVideoLayerDecodeRequest<'a> {
    asset_id: AssetId,
    dependency: &'a crate::preset::ExportMediaDependency,
    source_sample: mondrian_core::SourceSampleTarget,
    decode_resolution: Resolution,
    source_resolution: Resolution,
    source_color: PreviewSourceColorContract,
    alpha_interpretation: AlphaInterpretation,
    input_transform: RenderInputTransform,
}

/// Job-owned mutable execution services for one export still-frame decode.
///
/// Keeping these borrows outside `ExportVideoLayerDecodeRequest` makes the
/// immutable cache-authorizing request reusable without obscuring mutation of
/// the decoder and CPU color sessions.
struct ExportVideoLayerDecodeExecutionContext<'a> {
    color_session: &'a mut mondrian_renderer::RenderCpuColorExecutionSession,
    decode_context: &'a mut PreviewDecodeSessionContext,
    cancellation: &'a ExecutionCancellationToken,
}

fn decode_video_layer_scaled(
    request: ExportVideoLayerDecodeRequest<'_>,
    execution: ExportVideoLayerDecodeExecutionContext<'_>,
) -> Result<Arc<DecodedVideoLayer>, String> {
    let ExportVideoLayerDecodeRequest {
        asset_id,
        dependency,
        source_sample,
        decode_resolution,
        source_resolution,
        source_color,
        alpha_interpretation,
        input_transform,
    } = request;
    let ExportVideoLayerDecodeExecutionContext { color_session, decode_context, cancellation } =
        execution;
    let path = dependency.path.as_path();
    if !dependency.source_fingerprint.authorizes_reuse() {
        return Err(format!(
            "asset={} path={} err=export decode request has incomplete source revision evidence",
            asset_id,
            path.display()
        ));
    }
    let video_stream_index = dependency.video_stream_index.ok_or_else(|| {
        format!(
            "asset={} path={} err=export decode request has no frozen physical video stream",
            asset_id,
            path.display()
        )
    })?;
    let media_request = PreviewDecodeRequest::new(
        path,
        source_sample,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        source_color,
    )
    .with_max_size(
        Some(decode_resolution.width),
        Some(decode_resolution.height),
    )
    .with_video_stream_index(video_stream_index)
    .with_fingerprint(dependency.source_fingerprint);
    let decode_cancellation = cancellation.clone();
    let outcome =
        decode_context.decode_cancellable(media_request, move || decode_cancellation.is_canceled());
    let (source, decode_diagnostics): (CpuSourceColorFrame, PreviewDecodeDiagnostics) =
        match outcome {
            Ok(PreviewDecodeOutcome::Frame(frame)) => {
                let diagnostics = frame.diagnostics;
                (
                    CpuEncodedColorFrame::source_rgba8_shared(
                        frame.width,
                        frame.height,
                        source_color.color_space,
                        frame.into_shared_data(),
                    )
                    .into(),
                    diagnostics,
                )
            }
            Ok(PreviewDecodeOutcome::FloatFrame(frame)) => {
                let diagnostics = frame.diagnostics;
                (
                    LinearFloatSource::new(
                        frame.width,
                        frame.height,
                        source_color.color_space,
                        frame.into_data(),
                    )
                    .into(),
                    diagnostics,
                )
            }
            Ok(PreviewDecodeOutcome::Canceled(_)) => {
                return Err(format!(
                    "asset={} path={} err=export still-frame decode canceled unexpectedly",
                    asset_id,
                    path.display()
                ));
            }
            Ok(PreviewDecodeOutcome::NativeGpuFrame(frame)) => {
                return Err(format!(
                "asset={} path={} err=export still-frame CPU fallback requires CPU RGBA, got native GPU {} {:?}",
                asset_id,
                path.display(),
                frame.handle_kind().as_str(),
                frame.surface_format
            ));
            }
            Err(err) => {
                return Err(format!(
                    "asset={} path={} err={}",
                    asset_id,
                    path.display(),
                    err
                ));
            }
        };
    let source = source.normalize_alpha(alpha_interpretation).map_err(|err| {
        format!(
            "asset={} path={} alpha interpretation failed: {}",
            asset_id,
            path.display(),
            err
        )
    })?;
    let execution =
        execute_cpu_source_input_stage_with_session(&source, &input_transform, color_session)
            .map_err(|err| format!("asset={asset_id} color transform failed: {err}"))?;
    Ok(Arc::new(DecodedVideoLayer {
        frame: execution.result.frame,
        source_resolution,
        source_fingerprint: dependency.source_fingerprint,
        video_stream_index,
        decode_diagnostics: Some(decode_diagnostics),
        stage_diagnostics: execution.stage_diagnostics,
    }))
}

fn compute_timeline_render_range(
    timeline: &TimelineExportSnapshot,
) -> Result<TimelineRenderRange, String> {
    let resolved = timeline.range.resolve(&timeline.sequence).map_err(|error| error.to_string())?;
    Ok(TimelineRenderRange {
        start_frame: resolved.start_frame,
        total_frames: resolved.total_frames,
        fps_num: resolved.fps_num,
        fps_den: resolved.fps_den,
    })
}

mod helpers;
pub use helpers::expected_export_video_signal;
pub(crate) use helpers::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset::{
        Av1Profile, ExportChromaSampling, ExportParameter, ExportVideoSignal, HevcProfile,
        ProResProfile, TimelineExportRange, VideoRateControl,
    };
    use mondrian_core::automation::{PropertyHost, PropertyMutation, PropertyValue};
    use mondrian_core::timeline_data::{
        AssetColorPayload, AssetMediaInterpretation, MediaColorInterpretation,
        MediaRangeInterpretation, MediaSignalRange,
    };
    use mondrian_core::types::{AssetId, BlendMode, FramePosition};
    use mondrian_core::{JobId, VideoContentLightMetadata, VideoMasteringDisplayMetadata};
    use mondrian_effects::{
        apply_compiled_effect_graph_rgba_f32, compile_reference_effect_graph,
        compile_reference_render_graph,
        compiled_effect_graph_supports_rgba_f32_with_domain_processor, CompiledEffectGraph,
        EffectColorDomain, EffectGraphBuilderState, EffectNodeExt, EffectRenderOp,
        EffectRenderPlan, MaskOp, MaskShape, PreparedEffectProgram,
    };
    use mondrian_renderer::RenderOutputColorBoundaryTarget;
    use mondrian_timeline::clip::Clip;
    use mondrian_timeline::sequence::{
        InputColorResolutionSource, MissingColorMetadataPolicy, Sequence, StaticHdrMetadataPolicy,
    };
    use mondrian_timeline::track::Track;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Barrier, Mutex as StdMutex};
    use std::time::Duration;

    fn tt(frame: i64, time_base: Rational) -> TimelineTime {
        TimelineTime::from_frame_position(FramePosition::new(frame, time_base))
            .expect("valid test time")
    }

    fn open_execution_gate() -> service::ExportExecutionGate {
        service::ExportExecutionGate::always_open_for_test()
    }

    fn preflight_timeline_visual_range(
        timeline: &TimelineExportSnapshot,
        range: TimelineRenderRange,
        cancel: &ExecutionCancellationToken,
        execution_gate: &service::ExportExecutionGate,
        visual_session: &mut ExportVisualRenderSession,
    ) -> Result<(), JobExecutionResult> {
        visual_session
            .prepare_reference_range(&timeline.sequence, &timeline.sequences, range)
            .map_err(JobExecutionResult::Failed)?;
        preflight_timeline_visual_range_at_resolution(
            timeline,
            range,
            timeline.sequence.settings.resolution,
            timeline
                .sequence
                .settings
                .root_program_color_context(&timeline.color_environment),
            cancel,
            execution_gate,
            visual_session,
        )
    }

    #[test]
    fn export_materializer_has_no_raw_sequence_relookup_seam() {
        let queue_source = include_str!("mod.rs");
        let context = queue_source
            .split("struct ExportFrameRenderContext")
            .nth(1)
            .and_then(|suffix| suffix.split("struct ExportDecodeCacheKey").next())
            .expect("Export materialization context source");
        let materializer = queue_source
            .split("fn render_prepared_visual_node_into")
            .nth(1)
            .and_then(|suffix| suffix.split("fn resolve_export_input_video_range").next())
            .expect("Export materializer source");
        let effect_planner = include_str!("visual_effect_execution.rs");

        for source in [context, materializer, effect_planner] {
            assert!(!source.contains("TimelineExportSnapshot"));
            assert!(!source.contains("mondrian_timeline::sequence::Sequence"));
            assert!(!source.contains("context.timeline"));
            assert!(!source.contains("export_sequence_by_id"));
            assert!(!source.contains(".settings.resolution"));
            assert!(!source.contains(".settings.preview.resolution_scale"));
            assert!(!source.contains(".settings.title_safe_margin"));
        }
    }

    #[test]
    fn export_gpu_backoff_is_scoped_to_one_queue_attempt() {
        let mut runtime = ExportGpuExecutionRuntime {
            attempt_generation: 7,
            next_device_generation: 3,
            resource_pool_options: GpuColorFrameWgpuResourcePoolOptions::default(),
            active_output_grant: RenderGpuOutputExecutionResourceGrant::default(),
            state: ExportGpuExecutionRuntimeState::Backoff { attempt_generation: 7 },
        };

        runtime.begin_attempt(7);
        assert!(matches!(
            runtime.state,
            ExportGpuExecutionRuntimeState::Backoff { attempt_generation: 7 }
        ));

        runtime.begin_attempt(8);
        assert_eq!(runtime.attempt_generation, 8);
        assert_eq!(runtime.next_device_generation, 3);
        assert!(matches!(
            runtime.state,
            ExportGpuExecutionRuntimeState::Cold
        ));
    }

    #[test]
    fn export_gpu_active_working_set_rejection_remains_explicit() {
        let breakdown = ExportGpuOutputFallbackBreakdown::default()
            .add_reason(ExportGpuOutputFallbackReason::ActiveWorkingSetRejected);

        assert_eq!(breakdown.active_working_set_rejected, 1);
        assert_eq!(breakdown.record_boundary_failed, 0);
        assert_eq!(breakdown.total(), 1);
    }

    #[test]
    fn route_local_gpu_output_failures_do_not_poison_required_gpu_execution() {
        for reason in [
            ExportGpuOutputFallbackReason::RecordBoundaryFailed,
            ExportGpuOutputFallbackReason::ActiveWorkingSetRejected,
            ExportGpuOutputFallbackReason::MissingReadbackBuffer,
            ExportGpuOutputFallbackReason::ReadbackMapFailed,
            ExportGpuOutputFallbackReason::ReadbackUnpackFailed,
        ] {
            assert!(!export_gpu_error_requires_backend_backoff(
                ExportGpuOutputExecutionError::Fallback(reason)
            ));
        }
        assert!(!export_gpu_error_requires_backend_backoff(
            ExportGpuOutputExecutionError::Canceled
        ));
        assert!(export_gpu_error_requires_backend_backoff(
            ExportGpuOutputExecutionError::DeviceTimedOut
        ));
    }

    #[test]
    fn gpu_readback_wait_is_cancellable_and_bounded_by_monotonic_deadline() {
        assert_eq!(EXPORT_GPU_READBACK_TIMEOUT, Duration::from_secs(30));
        let now = Instant::now();
        assert_eq!(
            export_gpu_readback_poll_timeout(true, now, now + Duration::from_secs(1)),
            Err(ExportGpuOutputExecutionError::Canceled)
        );
        assert_eq!(
            export_gpu_readback_poll_timeout(false, now, now),
            Err(ExportGpuOutputExecutionError::DeviceTimedOut)
        );
        assert_eq!(
            export_gpu_readback_poll_timeout(false, now, now + EXPORT_GPU_READBACK_POLL_SLICE * 2,),
            Ok(EXPORT_GPU_READBACK_POLL_SLICE)
        );
        assert_eq!(
            export_gpu_readback_poll_timeout(false, now, now + EXPORT_GPU_READBACK_POLL_SLICE / 2,),
            Ok(EXPORT_GPU_READBACK_POLL_SLICE / 2)
        );
        assert_eq!(
            accept_export_gpu_readback_poll(Err(wgpu::PollError::Timeout)),
            Ok(()),
            "one bounded device-poll slice timing out is not the 30-second readback deadline"
        );
        assert_eq!(
            accept_export_gpu_readback_poll(Err(wgpu::PollError::WrongSubmissionIndex(2, 1))),
            Err(ExportGpuOutputExecutionError::Fallback(
                ExportGpuOutputFallbackReason::ReadbackMapFailed
            ))
        );
    }

    #[test]
    fn export_gpu_active_grant_reconfigures_without_reclassifying_backend_state() {
        let mut runtime = ExportGpuExecutionRuntime {
            attempt_generation: 7,
            next_device_generation: 3,
            resource_pool_options: GpuColorFrameWgpuResourcePoolOptions::default(),
            active_output_grant: RenderGpuOutputExecutionResourceGrant::default(),
            state: ExportGpuExecutionRuntimeState::Backoff { attempt_generation: 7 },
        };
        let policy = ExportExecutionResourcePolicy {
            gpu_output_active: RenderGpuOutputExecutionResourceGrant::new(512 * 1024 * 1024, 4),
            gpu_output_idle_per_contract: runtime.resource_pool_options.max_per_contract,
            gpu_output_idle_bytes: runtime.resource_pool_options.max_retained_bytes,
            ..ExportExecutionResourcePolicy::default()
        };

        runtime.configure(policy);

        assert_eq!(runtime.active_output_grant, policy.gpu_output_active);
        assert!(matches!(
            runtime.state,
            ExportGpuExecutionRuntimeState::Backoff { attempt_generation: 7 }
        ));
    }

    #[test]
    fn export_visual_closure_fails_closed_when_one_program_exceeds_frozen_grant() {
        let policy = service::ExportExecutionResourcePolicy {
            visual_program_entries: 1,
            visual_program_bytes: 1,
            ..service::ExportExecutionResourcePolicy::default()
        };
        let sequence = Sequence::new("oversized visual closure");
        let prepared = crate::prepare_timeline_export_dependencies(
            &sequence,
            &[],
            TimelineExportRange::EntireSequence,
            false,
        )
        .expect("prepare oversized visual closure");
        let error = match ExportVisualRenderSession::for_execution_generation(
            11,
            policy,
            prepared.execution_snapshot().visual(),
        ) {
            Ok(_) => panic!("one oversized visual program must not bypass the attempt grant"),
            Err(error) => error,
        };

        assert!(error.contains("frozen grant"));
    }

    fn test_working_frame(
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> mondrian_renderer::CpuColorFrame {
        let source = mondrian_renderer::CpuEncodedColorFrame::source_rgba8(
            width,
            height,
            ColorSpace::Rec709,
            rgba.to_vec(),
        );
        mondrian_renderer::execute_cpu_input_stage(
            &source,
            &mondrian_renderer::RenderInputTransform::to_working(
                WorkingColorSpace::LinearRec709,
                false,
                ColorEngine::mondrian_standard(),
            ),
        )
        .expect("test input transform")
        .result
        .frame
    }

    fn heterogeneous_tracer_graph(
        middle: mondrian_effects::EffectType,
    ) -> Arc<CompiledEffectGraph> {
        let blur =
            mondrian_effects::EffectNode::with_defaults(mondrian_effects::EffectType::GaussianBlur);
        let mut middle_node = mondrian_effects::EffectNode::with_defaults(middle.clone());
        let middle_property = match &middle {
            mondrian_effects::EffectType::BasicCorrection => Some(("exposure", 0.25)),
            mondrian_effects::EffectType::Vignette => Some(("intensity", 0.45)),
            _ => None,
        };
        if let Some((property, value)) = middle_property {
            middle_node
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: middle.property_path(property),
                    value: PropertyValue::Float(value),
                })
                .expect("configure heterogeneous Export middle Effect");
        }
        let mut grain =
            mondrian_effects::EffectNode::with_defaults(mondrian_effects::EffectType::Grain);
        grain
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: mondrian_effects::EffectType::Grain.property_path("amount"),
                value: PropertyValue::Float(0.1),
            })
            .expect("configure heterogeneous Export Grain");
        let effects = [blur, middle_node, grain];
        PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec709)
            .expect("prepare heterogeneous Export test program")
            .evaluate(TimelineTime::ZERO)
            .expect("compile heterogeneous Export test graph")
    }

    fn heterogeneous_cpu_dag_graph() -> Arc<CompiledEffectGraph> {
        static NEXT_DEFINITION_ID: AtomicUsize = AtomicUsize::new(1);
        let definition_id = NEXT_DEFINITION_ID.fetch_add(1, Ordering::Relaxed);
        let cpu_type = mondrian_effects::EffectType::Plugin(format!(
            "test.export.heterogeneous.cpu-dag.{definition_id}.cpu"
        ));
        let gpu_type = mondrian_effects::EffectType::Plugin(format!(
            "test.export.heterogeneous.cpu-dag.{definition_id}.gpu"
        ));
        mondrian_effects::register_effect_definition(
            mondrian_effects::EffectDefinition::new(
                cpu_type.key(),
                "Export CPU DAG",
                Default::default(),
                mondrian_effects::EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(mondrian_effects::EffectExecutionContract {
                execution_modes: mondrian_effects::EffectExecutionModes::CPU_F32,
                ..mondrian_effects::EffectExecutionContract::IDENTITY
            })
            .with_branching_graph_builder(Arc::new(|_, _, graph| {
                let source = graph.current_output();
                let left = graph.add_unary_from(
                    source,
                    mondrian_effects::EffectRenderOp::ColorAdjust {
                        exposure: 0.25,
                        contrast: 1.0,
                        saturation: 1.0,
                        working_color_space: WorkingColorSpace::LinearRec709,
                    },
                );
                let right = graph.add_unary_from(
                    source,
                    mondrian_effects::EffectRenderOp::Vignette { intensity: 0.2, feather: 0.75 },
                );
                let output = graph.add_blend(left, right, BlendMode::Screen, 0.35);
                graph.set_current_output(output);
                Ok(())
            })),
        )
        .expect("register Export CPU-DAG definition");
        mondrian_effects::register_effect_definition(
            mondrian_effects::EffectDefinition::new(
                gpu_type.key(),
                "Export GPU tail",
                Default::default(),
                mondrian_effects::EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(mondrian_effects::EffectExecutionContract {
                execution_modes: mondrian_effects::EffectExecutionModes::GPU_F32,
                determinism: mondrian_effects::EffectDeterminism::FrameSeeded,
                ..mondrian_effects::EffectExecutionContract::IDENTITY
            })
            .with_graph_builder(Arc::new(|_, _, graph| {
                graph.append_unary(mondrian_effects::EffectRenderOp::Grain { amount: 0.1 });
                Ok(())
            })),
        )
        .expect("register Export GPU-tail definition");
        PreparedEffectProgram::prepare(
            &[
                mondrian_effects::EffectNode::new(cpu_type),
                mondrian_effects::EffectNode::new(gpu_type),
            ],
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .expect("prepare heterogeneous Export CPU-DAG program")
        .evaluate(TimelineTime::ZERO)
        .expect("compile heterogeneous Export CPU-DAG graph")
    }

    fn heterogeneous_gpu_mask_graph() -> Arc<CompiledEffectGraph> {
        let mut graph = EffectGraphBuilderState::new();
        let source = graph.source();
        let filtered = graph.add_unary_from(source, EffectRenderOp::GaussianBlur { radius: 1.0 });
        let mask = graph.add_mask_source(
            MaskShape::Rectangle {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                corner_radius: 0.0,
            },
            0.0,
            0.0,
            0.35,
        );
        let output = graph.add_mask(filtered, mask, false, MaskOp::Add);
        graph.set_current_output(output);
        compile_reference_render_graph(graph.finish()).expect("compile heterogeneous GPU Mask")
    }

    fn freeze_test_heterogeneous_route(
        session: &mut ExportVisualRenderSession,
        graph: &Arc<CompiledEffectGraph>,
        placement: ExportHeterogeneousPlacement,
        extent: EffectFrameExtent,
    ) -> mondrian_renderer::PreparedHeterogeneousEffectRoute {
        let route = session
            .prepare_heterogeneous_route(graph, extent, placement)
            .expect("prepare heterogeneous Export test route");
        session
            .register_or_validate_route_contract(&route, placement, extent)
            .expect("freeze heterogeneous Export test route");
        route
    }

    #[test]
    fn export_route_contract_accepts_the_prepared_cpu_dag_shape() {
        let graph = heterogeneous_cpu_dag_graph();
        let extent = EffectFrameExtent::new(4, 3);
        let mut session = ExportVisualRenderSession::for_reference_generation(
            70,
            service::ExportExecutionResourcePolicy::default(),
        );
        let route = freeze_test_heterogeneous_route(
            &mut session,
            &graph,
            ExportHeterogeneousPlacement::Media,
            extent,
        );

        assert_eq!(route.prepared_work().cpu_nodes().len(), 3);
        assert_eq!(route.prepared_work().gpu_suffix().node_ids().len(), 1);
        assert_eq!(session.heterogeneous_route_contracts.len(), 1);
    }

    #[test]
    fn export_route_contract_accepts_typed_alpha_mask_frontier() {
        let graph = heterogeneous_gpu_mask_graph();
        let extent = EffectFrameExtent::new(4, 3);
        let mut session = ExportVisualRenderSession::for_reference_generation(
            74,
            service::ExportExecutionResourcePolicy::default(),
        );
        let route = freeze_test_heterogeneous_route(
            &mut session,
            &graph,
            ExportHeterogeneousPlacement::Media,
            extent,
        );

        assert!(
            route.prepared_work().plan().materializations().iter().any(|materialization| {
                materialization.residency().format().domain() == EffectColorDomain::AlphaMask
            })
        );
        assert_eq!(route.cpu_frontier_retained_bytes(), 2 * 4 * 3 * 16);
        assert_eq!(session.heterogeneous_route_contracts.len(), 1);
    }

    #[test]
    fn export_complete_cpu_route_wins_when_heterogeneous_route_is_also_preparable() {
        let graph = heterogeneous_tracer_graph(mondrian_effects::EffectType::BasicCorrection);
        let extent = EffectFrameExtent::new(4, 3);
        let mut session = ExportVisualRenderSession::for_reference_generation(
            71,
            service::ExportExecutionResourcePolicy::default(),
        );

        assert!(compiled_effect_graph_supports_rgba_f32_with_domain_processor(&graph));
        session
            .prepare_heterogeneous_route(&graph, extent, ExportHeterogeneousPlacement::Media)
            .expect("the same graph also has a heterogeneous route");
        assert!(
            session
                .select_heterogeneous_route(&graph, ExportHeterogeneousPlacement::Media, extent,)
                .expect("select complete CPU route")
                .is_none(),
            "a complete exact CPU route must win before any heterogeneous work starts"
        );
        assert!(session.heterogeneous_route_contracts.is_empty());
        assert_eq!(
            session.visual_diagnostics().cpu_routes_selected_before_start,
            1
        );
    }

    #[test]
    fn export_heterogeneous_transition_endpoint_remains_preflight_blocked() {
        let graph = heterogeneous_cpu_dag_graph();
        let extent = EffectFrameExtent::new(4, 3);
        let mut session = ExportVisualRenderSession::for_reference_generation(
            78,
            service::ExportExecutionResourcePolicy::default(),
        );
        let error = session
            .select_heterogeneous_route(
                &graph,
                ExportHeterogeneousPlacement::TransitionInput,
                extent,
            )
            .expect_err("Export must not borrow Preview endpoint assembly semantics");
        assert!(matches!(
            error,
            ExportHeterogeneousEffectError::UnsupportedPlacement { placement: "transition_input" }
        ));
        assert!(session.heterogeneous_route_contracts.is_empty());
    }

    #[test]
    fn export_heterogeneous_route_admission_is_independent_of_gpu_plan_cache_pressure() {
        let graph = heterogeneous_tracer_graph(mondrian_effects::EffectType::BasicCorrection);
        let extent = EffectFrameExtent::new(4, 3);
        let nominal_policy = service::ExportExecutionResourcePolicy::default();
        let elevated_policy = service::ExportExecutionResourcePolicy {
            effect_gpu_plan_entries: 1,
            effect_gpu_plan_bytes: 1,
            ..nominal_policy
        };

        assert_eq!(
            elevated_policy.heterogeneous_route_contract_entries,
            nominal_policy.heterogeneous_route_contract_entries
        );
        assert_eq!(
            elevated_policy.heterogeneous_route_contract_bytes,
            nominal_policy.heterogeneous_route_contract_bytes
        );
        assert!(elevated_policy.effect_gpu_plan_entries < nominal_policy.effect_gpu_plan_entries);
        assert!(elevated_policy.effect_gpu_plan_bytes < nominal_policy.effect_gpu_plan_bytes);

        for (generation, policy) in [(72, nominal_policy), (73, elevated_policy)] {
            let mut session =
                ExportVisualRenderSession::for_reference_generation(generation, policy);
            freeze_test_heterogeneous_route(
                &mut session,
                &graph,
                ExportHeterogeneousPlacement::Media,
                extent,
            );

            assert_eq!(session.heterogeneous_route_contracts.len(), 1);
            assert_eq!(
                session.heterogeneous_route_logical_bytes(),
                service::EXPORT_HETEROGENEOUS_ROUTE_CONTRACT_LOGICAL_BYTES
            );
        }
    }

    #[test]
    fn export_heterogeneous_route_contract_rejects_unpreflighted_shape_drift() {
        let extent = EffectFrameExtent::new(4, 3);
        let admitted = heterogeneous_tracer_graph(mondrian_effects::EffectType::BasicCorrection);
        let drifted = heterogeneous_tracer_graph(mondrian_effects::EffectType::Vignette);
        let mut session = ExportVisualRenderSession::for_reference_generation(
            72,
            service::ExportExecutionResourcePolicy::default(),
        );
        freeze_test_heterogeneous_route(
            &mut session,
            &admitted,
            ExportHeterogeneousPlacement::Media,
            extent,
        );
        session.route_contracts_sealed = true;
        let drifted_route = session
            .prepare_heterogeneous_route(&drifted, extent, ExportHeterogeneousPlacement::Media)
            .expect("prepare shape-drift probe");

        let error = session
            .register_or_validate_route_contract(
                &drifted_route,
                ExportHeterogeneousPlacement::Media,
                extent,
            )
            .expect_err("runtime route shape not frozen by preflight must fail closed");
        assert!(matches!(
            error,
            ExportHeterogeneousEffectError::RouteContractNotPreflighted { .. }
        ));
        assert_eq!(session.heterogeneous_route_contracts.len(), 1);
    }

    #[test]
    fn export_heterogeneous_suffix_failure_is_terminal_after_cpu_prefix() {
        let generation = 73;
        let extent = EffectFrameExtent::new(4, 3);
        let frame_seed = 19;
        let graph = heterogeneous_tracer_graph(mondrian_effects::EffectType::BasicCorrection);
        let mut session = ExportVisualRenderSession::for_reference_generation(
            generation,
            service::ExportExecutionResourcePolicy::default(),
        );
        let prepared_route = freeze_test_heterogeneous_route(
            &mut session,
            &graph,
            ExportHeterogeneousPlacement::Media,
            extent,
        );
        session.route_contracts_sealed = true;
        session.composite_scratch.bind_effect_execution_generation(generation);
        let input = test_working_frame(
            &[64, 128, 192, 255].repeat(12),
            extent.width(),
            extent.height(),
        );
        let route = PreparedExportHeterogeneousElement {
            element_index: 0,
            placement: ExportHeterogeneousPlacement::Media,
            route: prepared_route,
            frame_seed,
        };
        let _failure = GpuBoundaryFailureGuard::activate();

        let error = session
            .execute_heterogeneous_element(
                &route,
                HeterogeneousCpuPrefixSource::working_frame(input.clone()),
                WorkingColorSpace::LinearRec709,
                &ExecutionCancellationToken::new(),
            )
            .expect_err("a selected suffix failure must not restart the complete graph on CPU");
        assert!(matches!(
            error,
            ExportHeterogeneousEffectError::GpuContinuation { .. }
        ));
        let diagnostics = session.visual_diagnostics();
        assert_eq!(diagnostics.heterogeneous_frames_started, 1);
        assert_eq!(diagnostics.heterogeneous_frames_completed, 0);
        assert_eq!(diagnostics.heterogeneous_terminal_failures, 1);
        assert!(diagnostics.last_heterogeneous_completion.is_none());
    }

    #[test]
    fn export_heterogeneous_prestart_cancellation_never_starts_cpu_prefix() {
        let generation = 75;
        let extent = EffectFrameExtent::new(4, 3);
        let graph = heterogeneous_tracer_graph(mondrian_effects::EffectType::BasicCorrection);
        let mut session = ExportVisualRenderSession::for_reference_generation(
            generation,
            service::ExportExecutionResourcePolicy::default(),
        );
        let prepared_route = freeze_test_heterogeneous_route(
            &mut session,
            &graph,
            ExportHeterogeneousPlacement::Media,
            extent,
        );
        session.route_contracts_sealed = true;
        session.composite_scratch.bind_effect_execution_generation(generation);
        let input = test_working_frame(
            &[48, 112, 208, 255].repeat(12),
            extent.width(),
            extent.height(),
        );
        let route = PreparedExportHeterogeneousElement {
            element_index: 0,
            placement: ExportHeterogeneousPlacement::Media,
            route: prepared_route,
            frame_seed: 29,
        };
        let cancellation = ExecutionCancellationToken::new();
        cancellation.cancel();

        let error = session
            .execute_heterogeneous_element(
                &route,
                HeterogeneousCpuPrefixSource::working_frame(input.clone()),
                WorkingColorSpace::LinearRec709,
                &cancellation,
            )
            .expect_err("pre-start cancellation must stop before Effect pixels");
        assert!(matches!(
            error,
            ExportHeterogeneousEffectError::Canceled { checkpoint: "before_cpu_prefix" }
        ));
        let diagnostics = session.visual_diagnostics();
        assert_eq!(diagnostics.heterogeneous_frames_started, 0);
        assert_eq!(diagnostics.heterogeneous_terminal_failures, 0);
    }

    #[test]
    fn export_heterogeneous_completion_preserves_frame_contract_and_matches_cpu_reference() {
        let generation = 74;
        let extent = EffectFrameExtent::new(4, 3);
        let frame_seed = 23;
        let graph = heterogeneous_tracer_graph(mondrian_effects::EffectType::BasicCorrection);
        let mut session = ExportVisualRenderSession::for_reference_generation(
            generation,
            service::ExportExecutionResourcePolicy::default(),
        );
        if session.gpu_output.ensure_ready().is_err() {
            eprintln!("skipping Export heterogeneous integration test: no GPU adapter available");
            return;
        }
        let prepared_route = freeze_test_heterogeneous_route(
            &mut session,
            &graph,
            ExportHeterogeneousPlacement::Media,
            extent,
        );
        session.route_contracts_sealed = true;
        session.composite_scratch.bind_effect_execution_generation(generation);
        let input = test_working_frame(
            &[32, 96, 224, 255].repeat(12),
            extent.width(),
            extent.height(),
        );
        let expected = apply_compiled_effect_graph_rgba_f32(
            &input.rgba_f32().data,
            extent.width(),
            extent.height(),
            &graph,
            frame_seed,
        )
        .expect("complete CPU reference");
        let route = PreparedExportHeterogeneousElement {
            element_index: 0,
            placement: ExportHeterogeneousPlacement::Media,
            route: prepared_route,
            frame_seed,
        };

        let output = session
            .execute_heterogeneous_element(
                &route,
                HeterogeneousCpuPrefixSource::working_frame(input.clone()),
                WorkingColorSpace::LinearRec709,
                &ExecutionCancellationToken::new(),
            )
            .expect("complete Export heterogeneous route");
        assert_eq!(output.descriptor(), input.descriptor());
        for (actual, expected) in output.rgba_f32().data.iter().zip(expected.iter()) {
            for channel in 0..4 {
                assert!(
                    (actual[channel] - expected[channel]).abs() <= 2.0e-5,
                    "channel {channel}: actual={} expected={}",
                    actual[channel],
                    expected[channel]
                );
            }
        }
        let diagnostics = session.visual_diagnostics();
        assert_eq!(diagnostics.heterogeneous_frames_started, 1);
        assert_eq!(diagnostics.heterogeneous_frames_completed, 1);
        assert_eq!(diagnostics.heterogeneous_terminal_failures, 0);
        assert!(diagnostics.heterogeneous_upload_bytes > 0);
        assert!(diagnostics.heterogeneous_readback_bytes > 0);
        assert_eq!(
            diagnostics
                .last_heterogeneous_completion
                .expect("bounded completion evidence")
                .working_color_space,
            WorkingColorSpace::LinearRec709
        );
    }

    #[test]
    fn export_procedural_solid_heterogeneous_completion_matches_cpu_reference() {
        let generation = 76;
        let extent = EffectFrameExtent::new(4, 3);
        let frame_seed = 37;
        let color = mondrian_core::Color { r: 0.2, g: 0.4, b: 0.7, a: 0.75 };
        let graph = heterogeneous_tracer_graph(mondrian_effects::EffectType::BasicCorrection);
        let mut session = ExportVisualRenderSession::for_reference_generation(
            generation,
            service::ExportExecutionResourcePolicy::default(),
        );
        if session.gpu_output.ensure_ready().is_err() {
            eprintln!(
                "skipping Export procedural heterogeneous integration test: no GPU adapter available"
            );
            return;
        }
        let prepared_route = freeze_test_heterogeneous_route(
            &mut session,
            &graph,
            ExportHeterogeneousPlacement::SolidColor,
            extent,
        );
        session.route_contracts_sealed = true;
        session.composite_scratch.bind_effect_execution_generation(generation);
        let source = vec![[color.r, color.g, color.b, color.a]; 12];
        let expected = apply_compiled_effect_graph_rgba_f32(
            &source,
            extent.width(),
            extent.height(),
            &graph,
            frame_seed,
        )
        .expect("complete procedural CPU reference");
        let route = PreparedExportHeterogeneousElement {
            element_index: 0,
            placement: ExportHeterogeneousPlacement::SolidColor,
            route: prepared_route,
            frame_seed,
        };

        let output = session
            .execute_heterogeneous_element(
                &route,
                HeterogeneousCpuPrefixSource::solid_color(extent, color),
                WorkingColorSpace::LinearRec709,
                &ExecutionCancellationToken::new(),
            )
            .expect("complete procedural Export heterogeneous route");
        assert_eq!(output.descriptor().width, extent.width());
        assert_eq!(output.descriptor().height, extent.height());
        for (actual, expected) in output.rgba_f32().data.iter().zip(expected.iter()) {
            for channel in 0..4 {
                assert!(
                    (actual[channel] - expected[channel]).abs() <= 2.0e-5,
                    "channel {channel}: actual={} expected={}",
                    actual[channel],
                    expected[channel]
                );
            }
        }
        assert_eq!(
            session.visual_diagnostics().heterogeneous_frames_completed,
            1
        );
    }

    #[test]
    fn export_procedural_solid_rejects_insufficient_source_grant_before_start() {
        let generation = 77;
        let extent = EffectFrameExtent::new(4, 3);
        let graph = heterogeneous_tracer_graph(mondrian_effects::EffectType::BasicCorrection);
        let mut session = ExportVisualRenderSession::for_reference_generation(
            generation,
            service::ExportExecutionResourcePolicy::default(),
        );
        let prepared_route = freeze_test_heterogeneous_route(
            &mut session,
            &graph,
            ExportHeterogeneousPlacement::SolidColor,
            extent,
        );
        session.route_contracts_sealed = true;
        session.resource_policy.effect_working_bytes = 1;
        let route = PreparedExportHeterogeneousElement {
            element_index: 0,
            placement: ExportHeterogeneousPlacement::SolidColor,
            route: prepared_route,
            frame_seed: 41,
        };

        let error = session
            .execute_heterogeneous_element(
                &route,
                HeterogeneousCpuPrefixSource::solid_color(
                    extent,
                    mondrian_core::Color { r: 0.2, g: 0.4, b: 0.7, a: 1.0 },
                ),
                WorkingColorSpace::LinearRec709,
                &ExecutionCancellationToken::new(),
            )
            .expect_err("procedural source allocation must remain inside its attempt grant");
        assert!(matches!(
            error,
            ExportHeterogeneousEffectError::ProceduralSourceGrantExceeded { limit: 1, .. }
        ));
        assert_eq!(session.visual_diagnostics().heterogeneous_frames_started, 0);
    }

    struct FakeExecutor {
        calls: Arc<AtomicUsize>,
        delay_ms: u64,
    }

    impl ExportExecutor for FakeExecutor {
        fn execute(
            &self,
            job: &RenderJob,
            cancel: &ExecutionCancellationToken,
            execution_gate: &service::ExportExecutionGate,
            report: &mut dyn FnMut(ExportProgress),
            _report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
        ) -> JobExecutionResult {
            self.calls.fetch_add(1, Ordering::Relaxed);
            report(ExportProgress::encoding(0.2));

            let step = 20u64;
            let mut elapsed = 0u64;
            while elapsed < self.delay_ms {
                if !execution_gate.wait_at_boundary(ExportProgressPhase::Encoding, cancel) {
                    return JobExecutionResult::Cancelled;
                }
                std::thread::sleep(Duration::from_millis(step));
                elapsed += step;
            }

            report(ExportProgress::rendering(0.95, 1_000, 1_000));
            if execution_gate.wait_at_boundary(ExportProgressPhase::Publishing, cancel) {
                JobExecutionResult::Published(DurableExportPublication::synthetic(
                    &job.config.output_path,
                ))
            } else {
                JobExecutionResult::Cancelled
            }
        }
    }

    struct DiagnosticExecutor {
        diagnostics: ExportJobDiagnostics,
    }

    impl ExportExecutor for DiagnosticExecutor {
        fn execute(
            &self,
            job: &RenderJob,
            cancel: &ExecutionCancellationToken,
            execution_gate: &service::ExportExecutionGate,
            report: &mut dyn FnMut(ExportProgress),
            report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
        ) -> JobExecutionResult {
            if !execution_gate.wait_at_boundary(ExportProgressPhase::Rendering, cancel) {
                return JobExecutionResult::Cancelled;
            }
            report(ExportProgress::rendering(0.5, 1, 1));
            report_diagnostics(self.diagnostics);
            if execution_gate.wait_at_boundary(ExportProgressPhase::Publishing, cancel) {
                JobExecutionResult::Published(DurableExportPublication::synthetic(
                    &job.config.output_path,
                ))
            } else {
                JobExecutionResult::Cancelled
            }
        }
    }

    type ResourcePolicyObservation = (
        service::ExportExecutionResourcePolicy,
        service::ExportExecutionResourcePolicy,
    );

    struct ResourcePolicyProbeExecutor {
        calls: AtomicUsize,
        observations: Arc<StdMutex<Vec<ResourcePolicyObservation>>>,
        first_started: Arc<Barrier>,
        first_continue: Arc<Barrier>,
    }

    impl ExportExecutor for ResourcePolicyProbeExecutor {
        fn execute(
            &self,
            job: &RenderJob,
            cancel: &ExecutionCancellationToken,
            execution_gate: &service::ExportExecutionGate,
            _report: &mut dyn FnMut(ExportProgress),
            _report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
        ) -> JobExecutionResult {
            let call = self.calls.fetch_add(1, Ordering::Relaxed);
            let before = execution_gate.resource_policy();
            if call == 0 {
                self.first_started.wait();
                self.first_continue.wait();
            }
            let after = execution_gate.resource_policy();
            self.observations
                .lock()
                .expect("resource-policy observations")
                .push((before, after));
            if execution_gate.wait_at_boundary(ExportProgressPhase::Publishing, cancel) {
                JobExecutionResult::Published(DurableExportPublication::synthetic(
                    &job.config.output_path,
                ))
            } else {
                JobExecutionResult::Cancelled
            }
        }
    }

    fn dummy_config(output_name: &str) -> ExportConfig {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec709);
        // Queue tests exercise admission itself. Leave the attachment absent so
        // the AAC preset freezes exact root/nested audio Programs, including
        // ProvenSilent root evidence, from the same delivery configuration.
        timeline.prepared_execution = None;
        ExportConfig {
            preset: crate::preset::ExportPreset::h264_aac_sdr_1080p(),
            timeline: Box::new(timeline),
            output_path: PathBuf::from(output_name),
            output_policy: ExportOutputPolicy::CreateNew,
        }
    }

    fn test_delivery_contract(
        bit_depth: DeliveryBitDepth,
        video_range: VideoRange,
        chroma_sampling: ExportChromaSampling,
        pixel_format: &'static str,
    ) -> ResolvedExportDeliveryContract {
        ResolvedExportDeliveryContract {
            resolution: crate::preset::Resolution { width: 1_920, height: 1_080 },
            bit_depth,
            video_range,
            chroma_sampling,
            pixel_format,
            color_target: crate::delivery::ResolvedExportColorTarget {
                color_space: ColorSpace::Rec709,
                tone_map: true,
                output_transform: mondrian_core::OutputTransformIntent::mondrian_standard(),
            },
        }
    }

    fn timeline_input_with_output_color(output_color_space: ColorSpace) -> TimelineExportSnapshot {
        let mut sequence = Sequence::new("color-validation");
        sequence.settings.color.program_output.color_space = output_color_space;
        let mut prepared_execution = crate::prepare_timeline_export_dependencies(
            &sequence,
            &[],
            TimelineExportRange::SequenceInOut,
            false,
        )
        .expect("prepare test visual snapshot")
        .execution_snapshot()
        .clone();
        prepared_execution
            .visual_mut()
            .install_title_fonts(mondrian_renderer::PreparedBasicTitleFontSet::default())
            .expect("seal empty test title-font closure");
        TimelineExportSnapshot {
            sequence,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: Some(prepared_execution),
            range: TimelineExportRange::SequenceInOut,
        }
    }

    fn refresh_test_execution_snapshot(timeline: &mut TimelineExportSnapshot, include_audio: bool) {
        let resource_policy = service::ExportExecutionResourcePolicy::default();
        let mut execution = crate::prepare_timeline_export_dependencies(
            &timeline.sequence,
            &timeline.sequences,
            timeline.range,
            include_audio,
        )
        .expect("prepare exact test execution snapshot")
        .execution_snapshot()
        .clone();
        let title_fonts = mondrian_renderer::PreparedBasicTitleFontSet::prepare(
            execution.visual().basic_title_font_queries().iter().cloned(),
            resource_policy.title_font_bytes,
        )
        .expect("freeze exact test Basic Title font closure");
        execution
            .visual_mut()
            .install_title_fonts(title_fonts)
            .expect("seal exact test Basic Title font closure");
        timeline.install_prepared_execution(execution);
    }

    fn captured_visual_session_for_test(
        timeline: &mut TimelineExportSnapshot,
    ) -> ExportVisualRenderSession {
        let resource_policy = service::ExportExecutionResourcePolicy::default();
        refresh_test_execution_snapshot(timeline, false);
        ExportVisualRenderSession::for_timeline(0, resource_policy, timeline)
            .expect("admit exact test visual execution snapshot")
    }

    #[derive(Clone, Copy)]
    enum TestCicpTagKind {
        Primaries,
        Transfer,
        Matrix,
    }

    fn exact_test_cicp_tag(
        kind: TestCicpTagKind,
        ffmpeg_name: &'static str,
    ) -> mondrian_media::VideoColorTag {
        let (code, canonical_name) = match (kind, ffmpeg_name) {
            (TestCicpTagKind::Primaries, "bt709") => (1, "bt709"),
            (TestCicpTagKind::Primaries, "bt470bg") => (5, "bt470bg"),
            (TestCicpTagKind::Primaries, "smpte170m") => (6, "smpte170m"),
            (TestCicpTagKind::Primaries, "bt2020") => (9, "bt2020"),
            (TestCicpTagKind::Primaries, "smpte432") => (12, "smpte432"),
            (TestCicpTagKind::Transfer, "bt709") => (1, "bt709"),
            (TestCicpTagKind::Transfer, "bt470bg") => (5, "bt470bg"),
            (TestCicpTagKind::Transfer, "smpte170m") => (6, "smpte170m"),
            (TestCicpTagKind::Transfer, "iec61966-2-1") => (13, "iec61966-2-1"),
            (TestCicpTagKind::Transfer, "smpte2084") => (16, "smpte2084"),
            (TestCicpTagKind::Transfer, "arib-std-b67") => (18, "arib-std-b67"),
            (TestCicpTagKind::Matrix, "rgb" | "gbr") => (0, "gbr"),
            (TestCicpTagKind::Matrix, "bt709") => (1, "bt709"),
            (TestCicpTagKind::Matrix, "fcc") => (4, "fcc"),
            (TestCicpTagKind::Matrix, "bt470bg") => (5, "bt470bg"),
            (TestCicpTagKind::Matrix, "smpte170m") => (6, "smpte170m"),
            (TestCicpTagKind::Matrix, "smpte240m") => (7, "smpte240m"),
            (TestCicpTagKind::Matrix, "bt2020nc") => (9, "bt2020nc"),
            _ => panic!("unsupported exact test CICP {ffmpeg_name}"),
        };
        mondrian_media::VideoColorTag {
            code,
            name: Some(canonical_name.to_owned()),
            specified: true,
        }
    }

    fn test_media_dependency(
        path: PathBuf,
        executable_color_space: Option<ColorSpace>,
        interpretation: AssetMediaInterpretation,
        color_diagnostic: Option<mondrian_media::VideoColorDiagnostic>,
    ) -> crate::preset::ExportMediaDependency {
        let color_diagnostic = color_diagnostic.or_else(|| {
            executable_color_space.map(|color_space| {
                let tags = color_space
                    .ffmpeg_tags()
                    .expect("test executable source must have exact standardized tags");
                let metadata = mondrian_media::VideoColorMetadata {
                    primaries: exact_test_cicp_tag(
                        TestCicpTagKind::Primaries,
                        tags.color_primaries,
                    ),
                    transfer: exact_test_cicp_tag(TestCicpTagKind::Transfer, tags.color_trc),
                    matrix: exact_test_cicp_tag(TestCicpTagKind::Matrix, tags.colorspace),
                };
                let pixel_format = if matches!(tags.colorspace, "rgb" | "gbr") {
                    mondrian_media::info::PixelFormat::Rgb24
                } else {
                    mondrian_media::info::PixelFormat::Yuv420p
                };
                let sampling = mondrian_core::ProvenVideoSampling {
                    pixel_format,
                    bit_depth: 8,
                    has_alpha: false,
                };
                let interpretation =
                    mondrian_media::interpret_video_color_metadata(&metadata, Some(sampling), &[]);
                assert_eq!(
                    interpretation.executable_color_space_from_probe(
                        Some(sampling),
                        Some(&metadata),
                        &[],
                    ),
                    Some(color_space),
                    "test dependency must carry closed executable CICP evidence"
                );
                mondrian_media::VideoColorDiagnostic {
                    color_range: mondrian_media::DecodedVideoRange::Unknown,
                    sampling: Some(sampling),
                    interpretation,
                    metadata: Some(metadata),
                    metadata_hints: Vec::new(),
                    hdr_metadata: Vec::new(),
                }
            })
        });
        crate::preset::ExportMediaDependency {
            source_fingerprint: MediaFileFingerprint::capture(path.as_path()),
            path,
            video_stream_index: Some(0),
            picture_source_extent: Some(mondrian_timeline::PictureSourceExtent::Still),
            source_resolution: Some(Resolution { width: 1, height: 1 }),
            audio_components: HashMap::new(),
            interpretation,
            color_diagnostic,
        }
    }

    #[test]
    fn export_source_revision_change_is_fail_closed() {
        let root = std::env::temp_dir().join(format!("mondrian-export-revision-{}", JobId::new()));
        std::fs::create_dir_all(&root).expect("create export revision root");
        let source = root.join("source.mov");
        std::fs::write(&source, b"admitted").expect("write admitted source");
        let asset_id = AssetId::new();
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec709);
        timeline.media.insert(
            asset_id,
            test_media_dependency(
                source.clone(),
                Some(ColorSpace::Rec709),
                AssetMediaInterpretation::default(),
                None,
            ),
        );
        let replacement = root.join("replacement.mov");
        std::fs::write(&replacement, b"replaced").expect("write same-length replacement");
        std::fs::remove_file(&source).expect("unlink admitted source");
        std::fs::rename(&replacement, &source).expect("install same-length replacement");

        let error = validate_snapshot_media_revisions(&timeline)
            .expect_err("changed source revision must fail closed");
        assert!(error.contains(&asset_id.to_string()));
        assert!(error.contains(source.to_string_lossy().as_ref()));
        assert_eq!(
            std::fs::metadata(&source).expect("replacement metadata").len(),
            b"admitted".len() as u64,
            "the revision gate must not rely on length changes"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn export_source_revision_validation_rejects_incomplete_evidence() {
        let root = std::env::temp_dir().join(format!(
            "mondrian-export-incomplete-revision-{}",
            JobId::new()
        ));
        std::fs::create_dir_all(&root).expect("create export revision root");
        let source = root.join("source.mov");
        std::fs::write(&source, b"admitted source").expect("write admitted source");
        let asset_id = AssetId::new();
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec709);
        let mut dependency = test_media_dependency(
            source.clone(),
            Some(ColorSpace::Rec709),
            AssetMediaInterpretation::default(),
            None,
        );
        dependency.source_fingerprint = MediaFileFingerprint::default();
        timeline.media.insert(asset_id, dependency);

        let admitted_error = validate_snapshot_media_revisions(&timeline)
            .expect_err("incomplete admitted revision must fail closed");
        assert!(admitted_error.contains("admitted"));
        assert!(admitted_error.contains("incomplete"));
        assert!(admitted_error.contains(&asset_id.to_string()));

        timeline.media.get_mut(&asset_id).expect("dependency").source_fingerprint =
            MediaFileFingerprint::capture(&source);
        std::fs::remove_file(&source).expect("remove admitted source");
        let actual_error = validate_snapshot_media_revisions(&timeline)
            .expect_err("unobservable current revision must fail closed");
        assert!(actual_error.contains("cannot be observed completely"));
        assert!(actual_error.contains(&asset_id.to_string()));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn export_decode_cache_does_not_cross_root_and_nested_color_contracts() {
        let source = tempfile::NamedTempFile::new().expect("temporary media source");
        std::fs::write(source.path(), b"decode identity").expect("write media source");
        let asset_id = AssetId::new();
        let dependency = test_media_dependency(
            source.path().to_path_buf(),
            Some(ColorSpace::Rec709),
            AssetMediaInterpretation::default(),
            None,
        );
        let root_sequence = Sequence::new("root color context");
        let root_context = root_sequence
            .settings
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default());
        let mut nested_working_context = root_context.clone();
        nested_working_context.working_color_space = match root_context.working_color_space {
            WorkingColorSpace::LinearRec709 => WorkingColorSpace::LinearRec2020,
            _ => WorkingColorSpace::LinearRec709,
        };
        let mut nested_engine_context = root_context.clone();
        nested_engine_context.engine = ColorEngine::Aces {
            preset: mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
        };
        let source_resolution = Resolution { width: 3_840, height: 2_160 };
        let decode_resolution = Resolution { width: 1_920, height: 1_080 };
        let build_key = |context: &ProgramColorContext, auto_tone_map: bool| {
            ExportDecodeCacheKey::new(
                asset_id,
                &dependency,
                mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
                ColorSpace::Rec709,
                DecodedVideoRangeContract::OverrideFull,
                AlphaInterpretation::Straight,
                context,
                auto_tone_map,
                decode_resolution,
                source_resolution,
            )
            .expect("complete cache identity")
        };
        let root_key = build_key(&root_context, false);
        let nested_working_key = build_key(&nested_working_context, false);
        let nested_engine_key = build_key(&nested_engine_context, false);
        let nested_tone_map_key = build_key(&root_context, true);
        let mut cache = HashMap::new();
        cache.insert(root_key.clone(), "root working pixels");

        assert_eq!(cache.get(&root_key), Some(&"root working pixels"));
        assert_ne!(root_key, nested_working_key);
        assert_ne!(root_key, nested_engine_key);
        assert_ne!(root_key, nested_tone_map_key);
        assert!(!cache.contains_key(&nested_working_key));
        assert!(!cache.contains_key(&nested_engine_key));
        assert!(!cache.contains_key(&nested_tone_map_key));
    }

    #[test]
    fn canceled_export_video_decode_yields_before_opening_the_source() {
        let source = tempfile::NamedTempFile::new().expect("temporary canceled media source");
        std::fs::write(source.path(), b"not opened").expect("write canceled media source");
        let dependency = test_media_dependency(
            source.path().to_path_buf(),
            Some(ColorSpace::Rec709),
            AssetMediaInterpretation::default(),
            None,
        );
        let mut decode_context = PreviewDecodeSessionContext::new();
        let mut color_session = mondrian_renderer::RenderCpuColorExecutionSession::new(0);
        let cancellation = ExecutionCancellationToken::new();
        cancellation.cancel();
        let result = decode_video_layer_scaled(
            ExportVideoLayerDecodeRequest {
                asset_id: AssetId::new(),
                dependency: &dependency,
                source_sample: mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
                decode_resolution: Resolution { width: 16, height: 16 },
                source_resolution: Resolution { width: 16, height: 16 },
                source_color: PreviewSourceColorContract::new(
                    ColorSpace::Rec709,
                    DecodedVideoRangeContract::OverrideLimited,
                ),
                alpha_interpretation: AlphaInterpretation::Straight,
                input_transform: RenderInputTransform::to_working(
                    WorkingColorSpace::LinearRec709,
                    false,
                    ColorEngine::mondrian_standard(),
                ),
            },
            ExportVideoLayerDecodeExecutionContext {
                color_session: &mut color_session,
                decode_context: &mut decode_context,
                cancellation: &cancellation,
            },
        );
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("canceled export decode must not open the missing source"),
        };

        assert!(error.contains("canceled"));
        assert_eq!(decode_context.resident_session_count(), 0);
    }

    #[test]
    fn export_decode_context_reuses_across_frames_and_retires_at_job_boundary() {
        const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/small/h264-bframes.mp4");
        let root =
            std::env::temp_dir().join(format!("mondrian-export-decode-context-{}", JobId::new()));
        std::fs::create_dir_all(&root).expect("create export decode context root");
        let source = root.join("two-frames.mp4");
        std::fs::write(&source, FIXTURE).expect("write synthetic H.264 fixture");
        let dependency = test_media_dependency(
            source,
            Some(ColorSpace::Rec709),
            AssetMediaInterpretation::default(),
            None,
        );
        let asset_id = AssetId::new();
        let mut session = ExportVisualRenderSession::default();
        let cancellation = ExecutionCancellationToken::new();
        let decode = |source_time: TimelineTime,
                      session: &mut ExportVisualRenderSession|
         -> Arc<DecodedVideoLayer> {
            decode_video_layer_scaled(
                ExportVideoLayerDecodeRequest {
                    asset_id,
                    dependency: &dependency,
                    source_sample: mondrian_core::SourceSampleTarget::covering(source_time),
                    decode_resolution: Resolution { width: 16, height: 16 },
                    source_resolution: Resolution { width: 16, height: 16 },
                    source_color: PreviewSourceColorContract::new(
                        ColorSpace::Rec709,
                        DecodedVideoRangeContract::OverrideLimited,
                    ),
                    alpha_interpretation: AlphaInterpretation::Straight,
                    input_transform: RenderInputTransform::to_working(
                        WorkingColorSpace::LinearRec709,
                        false,
                        ColorEngine::mondrian_standard(),
                    ),
                },
                ExportVideoLayerDecodeExecutionContext {
                    color_session: session.composite_scratch.color_execution_mut(),
                    decode_context: &mut session.decode_context,
                    cancellation: &cancellation,
                },
            )
            .unwrap_or_else(|error| {
                panic!("decode export frame at source time {source_time:?}: {error}")
            })
        };

        let first = decode(TimelineTime::ZERO, &mut session);
        let second = decode(tt(1, Rational::new(1, 25)), &mut session);
        assert_eq!(
            first
                .decode_diagnostics
                .as_ref()
                .expect("real decode diagnostics")
                .session_disposition,
            PreviewDecodeSessionDisposition::Opened
        );
        assert_eq!(
            second
                .decode_diagnostics
                .as_ref()
                .expect("real decode diagnostics")
                .session_disposition,
            PreviewDecodeSessionDisposition::Reused
        );
        assert_eq!(second.source_fingerprint, dependency.source_fingerprint);
        assert_eq!(second.video_stream_index, 0);
        assert_eq!(session.decode_context.resident_session_count(), 1);

        session.release_decode_sessions();
        assert_eq!(session.decode_context.resident_session_count(), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn export_decode_rejects_same_length_source_replacement_before_session_open() {
        let root =
            std::env::temp_dir().join(format!("mondrian-export-decode-revision-{}", JobId::new()));
        std::fs::create_dir_all(&root).expect("create export decode revision root");
        let source = root.join("source.mov");
        std::fs::write(&source, b"old-revision").expect("write admitted revision");
        let dependency = test_media_dependency(
            source.clone(),
            Some(ColorSpace::Rec709),
            AssetMediaInterpretation::default(),
            None,
        );
        let replacement = root.join("replacement.mov");
        std::fs::write(&replacement, b"new-revision").expect("write equal-length replacement");
        std::fs::remove_file(&source).expect("unlink admitted revision");
        std::fs::rename(&replacement, &source).expect("install equal-length replacement");
        assert_eq!(
            Some(std::fs::metadata(&source).expect("replacement metadata").len()),
            dependency.source_fingerprint.len
        );

        let mut decode_context = PreviewDecodeSessionContext::new();
        let mut color_session = mondrian_renderer::RenderCpuColorExecutionSession::new(0);
        let cancellation = ExecutionCancellationToken::new();
        let error = decode_video_layer_scaled(
            ExportVideoLayerDecodeRequest {
                asset_id: AssetId::new(),
                dependency: &dependency,
                source_sample: mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
                decode_resolution: Resolution { width: 16, height: 16 },
                source_resolution: Resolution { width: 16, height: 16 },
                source_color: PreviewSourceColorContract::new(
                    ColorSpace::Rec709,
                    DecodedVideoRangeContract::OverrideLimited,
                ),
                alpha_interpretation: AlphaInterpretation::Straight,
                input_transform: RenderInputTransform::to_working(
                    WorkingColorSpace::LinearRec709,
                    false,
                    ColorEngine::mondrian_standard(),
                ),
            },
            ExportVideoLayerDecodeExecutionContext {
                color_session: &mut color_session,
                decode_context: &mut decode_context,
                cancellation: &cancellation,
            },
        )
        .expect_err("stale export dependency must fail before decoder setup");
        assert!(error.contains("revision"));
        assert_eq!(decode_context.resident_session_count(), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn export_decode_cache_identity_covers_physical_decode_inputs() {
        let source = tempfile::NamedTempFile::new().expect("temporary media source");
        std::fs::write(source.path(), b"decode identity").expect("write media source");
        let asset_id = AssetId::new();
        let dependency = test_media_dependency(
            source.path().to_path_buf(),
            Some(ColorSpace::Rec709),
            AssetMediaInterpretation::default(),
            None,
        );
        let context = Sequence::new("cache identity")
            .settings
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default());
        let original = ExportDecodeCacheKey::new(
            asset_id,
            &dependency,
            mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
            ColorSpace::Rec709,
            DecodedVideoRangeContract::OverrideFull,
            AlphaInterpretation::Straight,
            &context,
            false,
            Resolution { width: 1_920, height: 1_080 },
            Resolution { width: 3_840, height: 2_160 },
        )
        .expect("complete cache identity");

        let mut changed = original.clone();
        changed.asset_id = AssetId::new();
        assert_ne!(original, changed);
        let mut changed = original.clone();
        changed.source_path = source.path().with_extension("proxy.mov");
        assert_ne!(original, changed);
        let mut changed = original.clone();
        changed.source_fingerprint.modified_nanos =
            changed.source_fingerprint.modified_nanos.map(|value| value.wrapping_add(1));
        assert_ne!(original, changed);
        let mut changed = original.clone();
        changed.video_stream_index = changed.video_stream_index.saturating_add(1);
        assert_ne!(original, changed);
        let mut changed = original.clone();
        changed.source_sample =
            mondrian_core::SourceSampleTarget::covering(tt(1, Rational::new(1, 25)));
        assert_ne!(original, changed);
        let mut changed = original.clone();
        changed.source_sample =
            mondrian_core::SourceSampleTarget::strict_predecessor(TimelineTime::ZERO);
        assert_ne!(original, changed);
        let mut changed = original.clone();
        changed.input_color_space = ColorSpace::Srgb;
        assert_ne!(original, changed);
        let mut changed = original.clone();
        changed.input_video_range = DecodedVideoRangeContract::OverrideLimited;
        assert_ne!(original, changed);
        let mut changed = original.clone();
        changed.alpha_interpretation = AlphaInterpretation::Premultiplied;
        assert_ne!(original, changed);
        let mut changed = original.clone();
        changed.decode_resolution.width /= 2;
        assert_ne!(original, changed);
        let mut changed = original.clone();
        changed.source_resolution.width /= 2;
        assert_ne!(original, changed);
    }

    #[test]
    fn export_picture_dependency_without_physical_stream_fails_closed() {
        let source = tempfile::NamedTempFile::new().expect("temporary media source");
        std::fs::write(source.path(), b"decode identity").expect("write media source");
        let asset_id = AssetId::new();
        let mut dependency = test_media_dependency(
            source.path().to_path_buf(),
            Some(ColorSpace::Rec709),
            AssetMediaInterpretation::default(),
            None,
        );
        dependency.video_stream_index = None;
        let context = Sequence::new("missing physical stream")
            .settings
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default());

        let error = ExportDecodeCacheKey::new(
            asset_id,
            &dependency,
            mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
            ColorSpace::Rec709,
            DecodedVideoRangeContract::OverrideLimited,
            AlphaInterpretation::Straight,
            &context,
            false,
            Resolution { width: 1, height: 1 },
            Resolution { width: 1, height: 1 },
        )
        .expect_err("picture dependency without exact stream must be rejected");
        assert!(error.contains("physical video stream"));
        assert!(error.contains(&asset_id.to_string()));
    }

    #[test]
    fn validated_export_publication_replaces_final_atomically() {
        let root = std::env::temp_dir().join(format!("mondrian-export-publish-{}", JobId::new()));
        std::fs::create_dir_all(&root).expect("create export publication root");
        let final_output = root.join("deliverable.mp4");
        std::fs::write(&final_output, b"prior deliverable").expect("write prior output");
        let mut staging = OwnedPublicationFile::create_sibling(&final_output, "export-test")
            .expect("reserve partial output");
        let partial_output = staging.path().to_path_buf();
        staging
            .file_mut()
            .expect("partial writer")
            .write_all(b"validated deliverable")
            .expect("write partial output");

        let evidence = finalize_export_output(
            staging,
            &final_output,
            ExportOutputPolicy::OverwriteExisting,
        )
        .expect("publish validated output");

        assert_eq!(
            std::fs::read(&final_output).expect("read published output"),
            b"validated deliverable"
        );
        assert_eq!(
            evidence.output_path,
            std::path::absolute(&final_output).expect("absolute")
        );
        assert!(!partial_output.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn publication_reservation_does_not_overwrite_colliding_legacy_partial() {
        let root = std::env::temp_dir().join(format!("mondrian-export-preserve-{}", JobId::new()));
        std::fs::create_dir_all(&root).expect("create export preservation root");
        let final_output = root.join("deliverable.mp4");
        let colliding_partial = root.join("deliverable.mp4.mondrian-collision.partial");
        std::fs::write(&colliding_partial, b"unowned collision").expect("write collision");
        let mut staging = OwnedPublicationFile::create_sibling(&final_output, "export-test")
            .expect("reserve unique partial output");
        assert_ne!(staging.path(), colliding_partial);
        staging
            .file_mut()
            .expect("partial writer")
            .write_all(b"validated deliverable")
            .expect("write partial output");
        finalize_export_output(staging, &final_output, ExportOutputPolicy::CreateNew)
            .expect("publish exact reserved object");

        assert_eq!(
            std::fs::read(&colliding_partial).expect("read preserved collision"),
            b"unowned collision"
        );
        assert_eq!(
            std::fs::read(&final_output).expect("read published output"),
            b"validated deliverable"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn before_namespace_failure_retains_validated_partial_object() {
        let root = std::env::temp_dir().join(format!("mondrian-export-retained-{}", JobId::new()));
        std::fs::create_dir_all(&root).expect("create export preservation root");
        let final_output = root.join("deliverable.mp4");
        std::fs::create_dir(&final_output).expect("create invalid target directory");
        let mut staging = OwnedPublicationFile::create_sibling(&final_output, "export-test")
            .expect("reserve partial output");
        let partial_output = staging.path().to_path_buf();
        staging
            .file_mut()
            .expect("partial writer")
            .write_all(b"validated deliverable")
            .expect("write partial output");

        let failure = finalize_export_output(staging, &final_output, ExportOutputPolicy::CreateNew)
            .expect_err("directory target must fail before namespace publication");
        let ExportPublicationFailure::BeforeNamespace {
            output_path, retained_partial_path, ..
        } = failure
        else {
            panic!("expected typed pre-namespace publication failure");
        };
        assert_eq!(
            output_path,
            std::path::absolute(&final_output).expect("absolute")
        );
        assert_eq!(
            retained_partial_path.as_deref(),
            Some(partial_output.as_path())
        );
        assert_eq!(
            std::fs::read(&partial_output).expect("read retained validated partial"),
            b"validated deliverable"
        );
        assert!(final_output.is_dir());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn create_only_publication_preserves_file_created_during_export() {
        let root =
            std::env::temp_dir().join(format!("mondrian-export-late-collision-{}", JobId::new()));
        std::fs::create_dir_all(&root).expect("create export collision root");
        let final_output = root.join("deliverable.mp4");
        let mut staging = OwnedPublicationFile::create_sibling(&final_output, "export-test")
            .expect("reserve partial output");
        let partial_output = staging.path().to_path_buf();
        staging
            .file_mut()
            .expect("partial writer")
            .write_all(b"validated deliverable")
            .expect("write partial output");
        std::fs::write(&final_output, b"external deliverable").expect("create competing output");

        let failure = finalize_export_output(staging, &final_output, ExportOutputPolicy::CreateNew)
            .expect_err("create-only publication must not overwrite a late collision");
        let ExportPublicationFailure::BeforeNamespace { retained_partial_path, .. } = failure
        else {
            panic!("late create-only collision must fail before namespace mutation");
        };
        assert_eq!(
            retained_partial_path.as_deref(),
            Some(partial_output.as_path())
        );
        assert_eq!(
            std::fs::read(&final_output).expect("read competing output"),
            b"external deliverable"
        );
        assert_eq!(
            std::fs::read(&partial_output).expect("read retained validated output"),
            b"validated deliverable"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn replaced_reserved_partial_is_rejected_without_deleting_replacement() {
        let root = std::env::temp_dir().join(format!("mondrian-export-identity-{}", JobId::new()));
        std::fs::create_dir_all(&root).expect("create export identity root");
        let final_output = root.join("deliverable.mp4");
        std::fs::write(&final_output, b"prior deliverable").expect("write prior output");
        let staging = OwnedPublicationFile::create_sibling(&final_output, "export-test")
            .expect("reserve partial output");
        let reservation = staging.release_for_external_writer();
        let partial_output = reservation.path().to_path_buf();
        std::fs::remove_file(&partial_output).expect("remove reserved name");
        std::fs::write(&partial_output, b"foreign replacement").expect("write replacement");

        assert!(
            reservation.reclaim().is_err(),
            "replacement identity must be rejected"
        );
        assert_eq!(
            std::fs::read(&partial_output).expect("read foreign replacement"),
            b"foreign replacement"
        );
        assert_eq!(
            std::fs::read(&final_output).expect("read preserved prior output"),
            b"prior deliverable"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn export_module_has_no_private_windows_publication_implementation() {
        let source = include_str!("mod.rs");
        let forbidden = ["Replace", "FileW"].concat();
        assert!(!source.contains(&forbidden));
        let forbidden = ["REPLACEFILE", "_WRITE_THROUGH"].concat();
        assert!(!source.contains(&forbidden));
    }

    fn test_color_diagnostic(
        source: mondrian_media::VideoColorSpaceSource,
        method: mondrian_media::VideoColorDetectionMethod,
        warning: Option<mondrian_media::VideoColorInterpretationWarning>,
    ) -> mondrian_media::VideoColorDiagnostic {
        let warnings = warning.into_iter().collect::<Vec<_>>();
        mondrian_media::VideoColorDiagnostic {
            color_range: mondrian_media::DecodedVideoRange::Unknown,
            sampling: None,
            interpretation: mondrian_media::DetectedColorInterpretation {
                candidate_color_space: None,
                confidence: mondrian_media::VideoColorInterpretationConfidence::None,
                source,
                method,
                evidence: Vec::new(),
                warnings: warnings.clone(),
                user_overridable: true,
            },
            metadata: None,
            metadata_hints: Vec::new(),
            hdr_metadata: Vec::new(),
        }
    }

    fn wait_until(timeout_ms: u64, mut predicate: impl FnMut() -> bool) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed().as_millis() < timeout_ms as u128 {
            if predicate() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn queue_executes_jobs_and_marks_completed() {
        let calls = Arc::new(AtomicUsize::new(0));
        let queue = RenderQueue::new_with_executor(Arc::new(FakeExecutor {
            calls: Arc::clone(&calls),
            delay_ms: 100,
        }));

        let job_id =
            queue.enqueue(RenderJob::new(dummy_config("out-a.mp4"))).expect("admit export");

        let done = wait_until(2_000, || {
            queue
                .list_jobs()
                .iter()
                .find(|job| job.id == job_id)
                .map(|job| matches!(job.status, JobStatus::Completed))
                .unwrap_or(false)
        });

        assert!(done, "job should complete within timeout");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn queue_freezes_resource_policy_per_attempt_and_next_attempt_observes_update() {
        let observations = Arc::new(StdMutex::new(Vec::new()));
        let first_started = Arc::new(Barrier::new(2));
        let first_continue = Arc::new(Barrier::new(2));
        let queue = RenderQueue::new_with_executor(Arc::new(ResourcePolicyProbeExecutor {
            calls: AtomicUsize::new(0),
            observations: Arc::clone(&observations),
            first_started: Arc::clone(&first_started),
            first_continue: Arc::clone(&first_continue),
        }));
        let first_policy = service::ExportExecutionResourcePolicy {
            effect_cache_entries: 7,
            ..service::ExportExecutionResourcePolicy::default()
        };
        let second_policy = service::ExportExecutionResourcePolicy {
            effect_cache_entries: 19,
            ..service::ExportExecutionResourcePolicy::default()
        };
        queue.set_resource_policy(first_policy);
        let first_id = queue
            .enqueue(RenderJob::new(dummy_config("resource-policy-first.mp4")))
            .expect("admit first export");
        first_started.wait();
        queue.set_resource_policy(second_policy);
        first_continue.wait();
        assert!(wait_until(2_000, || {
            queue
                .list_jobs()
                .iter()
                .find(|job| job.id == first_id)
                .is_some_and(|job| job.status.is_terminal())
        }));

        let second_id = queue
            .enqueue(RenderJob::new(dummy_config("resource-policy-second.mp4")))
            .expect("admit second export");
        assert!(wait_until(2_000, || {
            queue
                .list_jobs()
                .iter()
                .find(|job| job.id == second_id)
                .is_some_and(|job| job.status.is_terminal())
        }));

        assert_eq!(
            observations.lock().expect("resource-policy observations").as_slice(),
            &[(first_policy, first_policy), (second_policy, second_policy)]
        );
        assert_eq!(queue.diagnostics().resource_policy, second_policy);
    }

    #[test]
    fn cancelling_pending_job_skips_execution() {
        let calls = Arc::new(AtomicUsize::new(0));
        let queue = RenderQueue::new_with_executor(Arc::new(FakeExecutor {
            calls: Arc::clone(&calls),
            delay_ms: 300,
        }));

        let first_id = queue
            .enqueue(RenderJob::new(dummy_config("out-first.mp4")))
            .expect("admit first export");
        let second_id = queue
            .enqueue(RenderJob::new(dummy_config("out-second.mp4")))
            .expect("admit second export");
        queue.cancel(second_id);

        let done = wait_until(3_000, || {
            let jobs = queue.list_jobs();
            let first_done = jobs
                .iter()
                .find(|job| job.id == first_id)
                .map(|job| matches!(job.status, JobStatus::Completed))
                .unwrap_or(false);
            let second_cancelled = jobs
                .iter()
                .find(|job| job.id == second_id)
                .map(|job| matches!(job.status, JobStatus::Cancelled))
                .unwrap_or(false);
            first_done && second_cancelled
        });

        assert!(done, "first should complete and second should cancel");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn clear_terminal_history_removes_all_terminal_jobs() {
        let calls = Arc::new(AtomicUsize::new(0));
        let queue = RenderQueue::new_with_executor(Arc::new(FakeExecutor {
            calls: Arc::clone(&calls),
            delay_ms: 100,
        }));
        queue
            .enqueue(RenderJob::new(dummy_config("completed.mp4")))
            .expect("admit first terminal");
        queue
            .enqueue(RenderJob::new(dummy_config("second-completed.mp4")))
            .expect("admit second terminal");
        let cancelled = queue
            .enqueue(RenderJob::new(dummy_config("cancelled.mp4")))
            .expect("admit canceled terminal");
        queue.cancel(cancelled);
        assert!(wait_until(2_000, || queue
            .list_jobs()
            .iter()
            .all(|job| job.status.is_terminal())));

        assert!(queue.has_terminal_history());
        assert_eq!(queue.clear_terminal_history(), 3);

        assert!(queue.list_jobs().is_empty());
        assert!(!queue.has_terminal_history());
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn cancellation_availability_is_non_allocating_guidance_not_an_outcome() {
        let queue = RenderQueue::new_with_executor(Arc::new(FakeExecutor {
            calls: Arc::new(AtomicUsize::new(0)),
            delay_ms: 0,
        }));
        queue.set_dispatch_enabled(false);
        let job_id = queue
            .enqueue(RenderJob::new(dummy_config("pending-cancel.mp4")))
            .expect("admit pending export");

        assert!(queue.can_cancel(job_id));
        assert!(!queue.can_cancel(JobId::new()));
        assert_eq!(queue.cancel(job_id), ExportCancelOutcome::Requested);
        assert!(!queue.can_cancel(job_id));
        assert!(queue.has_terminal_history());
        assert_eq!(queue.clear_terminal_history(), 1);
    }

    #[test]
    fn queue_exposes_export_job_diagnostics_from_worker() {
        let mut diagnostics = ExportJobDiagnostics::default();
        let mut counts = InputColorResolutionSourceCounts::default();
        counts.record(InputColorResolutionSource::DetectedMetadata);
        counts.record(InputColorResolutionSource::Override);
        diagnostics.color.record_frame_diagnostics(
            counts,
            RenderColorStageDiagnostics {
                total_stages: 2,
                cpu_input_stages: 1,
                cpu_output_stages: 1,
                gpu_blockers: 1,
                gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown {
                    render_pipeline_not_prepared: 1,
                    ..RenderColorStageGpuBlockerBreakdown::default()
                },
                stage_pixels: 8,
                ..RenderColorStageDiagnostics::default()
            },
            TimelineCompositeDiagnostics {
                elements: 3,
                float_linear_composites: 1,
                legacy_rgba8_composites: 1,
                legacy_media_transform: 1,
                ..TimelineCompositeDiagnostics::default()
            },
        );
        let queue = RenderQueue::new_with_executor(Arc::new(DiagnosticExecutor { diagnostics }));

        let job_id = queue
            .enqueue(RenderJob::new(dummy_config("diagnostics.mp4")))
            .expect("admit diagnostic export");

        let done = wait_until(2_000, || {
            queue
                .list_jobs()
                .iter()
                .find(|job| job.id == job_id)
                .map(|job| matches!(job.status, JobStatus::Completed))
                .unwrap_or(false)
        });

        assert!(done, "job should complete within timeout");
        let job = queue
            .list_jobs()
            .into_iter()
            .find(|job| job.id == job_id)
            .expect("diagnostic job");
        assert_eq!(job.diagnostics, diagnostics);
        assert_eq!(job.diagnostics.color.diagnosed_frames, 1);
        assert_eq!(job.diagnostics.color.stage_diagnostics.total_stages, 2);
        assert_eq!(job.diagnostics.color.stage_diagnostics.cpu_input_stages, 1);
        assert_eq!(job.diagnostics.color.stage_diagnostics.cpu_output_stages, 1);
        assert_eq!(job.diagnostics.color.stage_diagnostics.gpu_blockers, 1);
        assert_eq!(
            job.diagnostics
                .color
                .stage_diagnostics
                .gpu_blocker_breakdown
                .render_pipeline_not_prepared,
            1
        );
        assert_eq!(job.diagnostics.color.stage_diagnostics.stage_pixels, 8);
        let composite_summary = job.diagnostics.color.composite_color_path_summary();
        assert_eq!(composite_summary.elements, 3);
        assert_eq!(composite_summary.float_linear_composites, 1);
        assert_eq!(composite_summary.legacy_rgba8_composites, 1);
        assert_eq!(composite_summary.legacy_breakdown.media_transform, 1);
        assert_eq!(
            job.diagnostics.color.summary(),
            Some(ExportJobColorDiagnosticsSummary {
                diagnosed_frames: 1,
                detected_metadata: 1,
                override_count: 1,
                explicit_metadata_or_override: 2,
                gpu_blockers: 1,
                gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown {
                    render_pipeline_not_prepared: 1,
                    ..RenderColorStageGpuBlockerBreakdown::default()
                },
                cpu_input_stages: 1,
                cpu_output_stages: 1,
                float_linear_composites: 1,
                legacy_rgba8_composites: 1,
                legacy_reason_total: 1,
                legacy_breakdown: TimelineCompositeLegacyBreakdown {
                    media_transform: 1,
                    ..TimelineCompositeLegacyBreakdown::default()
                },
                gpu_path_ready: true,
                ..ExportJobColorDiagnosticsSummary::default()
            })
        );
        assert_eq!(
            job.diagnostics
                .color
                .input_resolution_source_counts
                .explicit_metadata_or_override(),
            2
        );
        let color_report = job
            .diagnostics
            .color_report("export-job-diagnostics")
            .expect("export job color report");
        assert_eq!(
            color_report.schema_version,
            EXPORT_COLOR_HEALTH_REPORT_SCHEMA_VERSION
        );
        assert_eq!(color_report.profile, "export-job-diagnostics");
        assert_eq!(color_report.verdict, ExportColorHealthVerdict::Fail);
        assert_eq!(color_report.summary.diagnosed_frames, 1);
        assert_eq!(color_report.summary.gpu_blockers, 1);
        assert_eq!(color_report.summary.legacy_reason_total, 1);
        assert!(color_report
            .root_causes
            .iter()
            .any(|root| root.code == "export_gpu_color_stage_blocked"));
        assert!(color_report
            .root_causes
            .iter()
            .any(|root| root.code == "legacy_rgba8_composite_path"));
    }

    #[test]
    fn export_color_diagnostics_summary_reports_health_contract() {
        assert_eq!(ExportJobColorDiagnostics::default().summary(), None);

        let mut diagnostics = ExportJobColorDiagnostics::default();
        diagnostics.record_asset_issue_summary(VideoColorDiagnosticIssueAggregate {
            diagnostics: 2,
            diagnostics_with_warnings: 1,
            method_missing_metadata: 1,
            method_decoder_unavailable: 1,
            confidence_none: 1,
            confidence_medium: 1,
            missing_cicp_tags: 1,
            unsupported_cicp_tags: 0,
            decoder_unavailable: 1,
            ..VideoColorDiagnosticIssueAggregate::default()
        });
        let mut counts = InputColorResolutionSourceCounts::default();
        counts.record(InputColorResolutionSource::DetectedMetadata);
        counts.record(InputColorResolutionSource::Override);
        counts.record(InputColorResolutionSource::DataTexture);
        counts.record(InputColorResolutionSource::MissingPolicyAssumeRec709);
        counts.record(InputColorResolutionSource::MissingPolicyRejectMedia);
        diagnostics.record_frame_diagnostics(
            counts,
            RenderColorStageDiagnostics {
                total_stages: 2,
                gpu_color_stages: 2,
                gpu_blockers: 2,
                gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown {
                    shader_module_not_prepared: 1,
                    ocio_resource_bind_group_not_prepared: 1,
                    ..RenderColorStageGpuBlockerBreakdown::default()
                },
                stage_pixels: 16,
                ..RenderColorStageDiagnostics::default()
            },
            TimelineCompositeDiagnostics {
                elements: 4,
                float_linear_composites: 2,
                ..TimelineCompositeDiagnostics::default()
            },
        );

        let expected_summary = ExportJobColorDiagnosticsSummary {
            asset_issue_summary: VideoColorDiagnosticIssueAggregate {
                diagnostics: 2,
                diagnostics_with_warnings: 1,
                method_missing_metadata: 1,
                method_decoder_unavailable: 1,
                confidence_none: 1,
                confidence_medium: 1,
                missing_cicp_tags: 1,
                unsupported_cicp_tags: 0,
                decoder_unavailable: 1,
                ..VideoColorDiagnosticIssueAggregate::default()
            },
            diagnosed_frames: 1,
            detected_metadata: 1,
            override_count: 1,
            policy_assumptions: 1,
            data_textures: 1,
            policy_rejections: 1,
            explicit_metadata_or_override: 2,
            gpu_color_stages: 2,
            gpu_blockers: 2,
            gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown {
                shader_module_not_prepared: 1,
                ocio_resource_bind_group_not_prepared: 1,
                ..RenderColorStageGpuBlockerBreakdown::default()
            },
            float_linear_composites: 2,
            fully_float_linear: true,
            gpu_path_ready: true,
            ..ExportJobColorDiagnosticsSummary::default()
        };
        assert_eq!(diagnostics.summary(), Some(expected_summary));

        let report = diagnostics
            .health_report("export-health-contract")
            .expect("export health report");
        assert_eq!(
            report.schema_version,
            EXPORT_COLOR_HEALTH_REPORT_SCHEMA_VERSION
        );
        assert_eq!(report.profile, "export-health-contract");
        assert_eq!(report.summary, expected_summary);
        assert_eq!(report.verdict, ExportColorHealthVerdict::Fail);
        assert!(report.checks.iter().any(|check| {
            check.code == "media_warnings" && check.severity == ExportColorHealthSeverity::Warn
        }));
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "input_color_policy_rejected_source"));
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "export_gpu_color_stage_blocked"));
    }

    #[test]
    fn export_color_health_warns_when_dynamic_hdr_metadata_will_be_stripped() {
        let summary = ExportJobColorDiagnosticsSummary {
            asset_issue_summary: VideoColorDiagnosticIssueAggregate {
                diagnostics: 2,
                diagnostics_with_hdr_metadata: 2,
                hdr_side_data_count: 2,
                diagnostics_with_dynamic_hdr10_plus: 1,
                diagnostics_with_dolby_vision_config: 1,
                ..VideoColorDiagnosticIssueAggregate::default()
            },
            diagnosed_frames: 1,
            fully_float_linear: true,
            gpu_path_ready: true,
            ..ExportJobColorDiagnosticsSummary::default()
        };

        let report = summary.health_report("dynamic-hdr-source");

        assert_eq!(report.verdict, ExportColorHealthVerdict::Warn);
        assert!(report.checks.iter().any(|check| {
            check.code == "dynamic_hdr_metadata_sources"
                && check.severity == ExportColorHealthSeverity::Warn
                && check.observed == 2
        }));
        assert!(report.root_causes.iter().any(|root| {
            root.code == "dynamic_hdr_metadata_not_preserved"
                && root.severity == ExportColorHealthSeverity::Warn
        }));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "use_validated_dynamic_hdr_authoring"));
    }

    #[test]
    fn unresolved_effect_domain_is_a_distinct_fail_closed_export_failure() {
        let mut diagnostics = ExportJobColorDiagnostics::default();
        diagnostics.record_frame_diagnostics(
            InputColorResolutionSourceCounts::default(),
            RenderColorStageDiagnostics::default(),
            TimelineCompositeDiagnostics {
                elements: 1,
                blocked_color_domain_composites: 1,
                blocked_adjustment_effect_domain: 1,
                ..TimelineCompositeDiagnostics::default()
            },
        );

        let summary = diagnostics.summary().expect("export color health");
        assert_eq!(summary.legacy_rgba8_composites, 0);
        assert_eq!(summary.blocked_color_domain_composites, 1);
        assert_eq!(summary.domain_blockers.adjustment_effect, 1);

        let report = summary.health_report("effect-domain-blocker");
        assert_eq!(report.verdict, ExportColorHealthVerdict::Fail);
        assert!(report.checks.iter().any(|check| {
            check.code == color_report_vocab::check::EFFECT_DOMAIN_BLOCKERS
                && check.severity == ExportColorHealthSeverity::Fail
                && check.observed == 1
        }));
        assert!(report
            .root_causes
            .iter()
            .any(|root| { root.code == color_report_vocab::root_cause::EFFECT_DOMAIN_UNRESOLVED }));
        assert!(report.actions.iter().any(|action| {
            action.code == color_report_vocab::action::RESOLVE_EFFECT_DOMAIN_TRANSITIONS
        }));
        assert!(!report.root_causes.iter().any(|root| {
            root.code == color_report_vocab::root_cause::LEGACY_RGBA8_COMPOSITE_PATH
        }));
    }

    /// Exercise the real high-bit CPU render branch with float-path failure
    /// injection.
    ///
    /// This test sets `FORCE_FLOAT_BOUNDARY_FAILURE` so that the renderer's
    /// `execute_cpu_output_boundary_float` returns `Err` immediately. The
    /// export pipeline must refuse to manufacture an `rgba64le` payload from
    /// an RGBA8 boundary. We verify:
    ///
    /// 1. Rendering returns an explicit fail-closed error.
    /// 2. The GPU failure remains diagnosed independently.
    /// 3. No RGBA8 precision fallback is accepted as successful output.
    ///
    /// **Scope note:** the test hook fails `cpu_output_boundary_float` at its
    /// wrapper boundary without corrupting the shared `ColorEngine` or OCIO
    /// config. This is the sanctioned injection point for long-term
    /// fail-closed-path testing.
    #[test]
    fn high_bit_export_fails_closed_when_float_helper_is_unavailable() {
        let mut seq = Sequence::new("precision-failure-injected");
        seq.settings.delivery.bit_depth = DeliveryBitDepth::Ten;
        let tb = seq.time_base();
        seq.video_tracks[0]
            .add_clip(
                Clip::new_solid_color(
                    AssetId::new(),
                    mondrian_core::Color::from_rgba8(200, 100, 50, 255),
                    tt(0, tb),
                    tt(1, tb),
                )
                .expect("valid clip"),
            )
            .expect("add solid clip");
        let timeline = TimelineExportSnapshot {
            sequence: seq,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        };

        let mut export_diagnostics = ExportJobColorDiagnostics::default();
        let mut canvas = vec![0u8; 2 * 2 * 8];

        let _gpu_guard = GpuBoundaryFailureGuard::activate();
        let _guard = FloatBoundaryFailureGuard::activate();
        let error = render_timeline_frame_into(
            &timeline,
            0,
            2,
            2,
            ExportAlphaMode::FlattenBlack,
            &mut canvas,
            None,
            None,
            None,
            Some(&mut export_diagnostics),
        )
        .expect_err("10-bit export must reject RGBA8 precision downgrade");

        assert!(error.contains("high-precision export output boundary failed closed"));
        assert!(error.contains("refusing RGBA8 downgrade"));
        assert_eq!(export_diagnostics.gpu_output_cpu_fallbacks, 1);
        assert_eq!(export_diagnostics.output_precision_failures, 1);
        assert_eq!(
            export_diagnostics.output_precision_failure_reasons.float_boundary_unavailable,
            1
        );

        let report = export_diagnostics
            .health_report("precision-failure-real-path")
            .expect("precision failure health report");
        assert_eq!(
            report.schema_version,
            EXPORT_COLOR_HEALTH_REPORT_SCHEMA_VERSION
        );
        assert_eq!(report.verdict, ExportColorHealthVerdict::Fail);
        assert!(report.checks.iter().any(|check| {
            check.code == "export_output_precision_failures"
                && check.severity == ExportColorHealthSeverity::Fail
        }));
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "export_output_precision_failure"));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "inspect_export_float_output_boundary"));
    }

    #[test]
    fn export_color_report_fails_tone_map_without_export_view_transform() {
        let mut diagnostics = ExportJobColorDiagnostics::default();
        diagnostics.record_frame_diagnostics(
            InputColorResolutionSourceCounts::default(),
            RenderColorStageDiagnostics::default(),
            TimelineCompositeDiagnostics {
                elements: 1,
                float_linear_composites: 1,
                ..TimelineCompositeDiagnostics::default()
            },
        );
        diagnostics.record_output_transform_issue(
            ExportOutputTransformIssueReason::ToneMapRequestedWithoutExportViewTransform,
        );

        let summary = diagnostics.summary().expect("summary");
        assert_eq!(summary.output_transform_issues, 1);
        assert_eq!(
            summary
                .output_transform_issue_reasons
                .tone_map_requested_without_export_view_transform,
            1
        );
        assert!(!summary.gpu_path_ready);

        let report = diagnostics.health_report("export-output-transform").expect("health report");
        assert_eq!(report.verdict, ExportColorHealthVerdict::Fail);
        assert!(report.checks.iter().any(|check| {
            check.code == "export_output_transform_issues"
                && check.severity == ExportColorHealthSeverity::Fail
        }));
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "export_output_transform_issue"));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "inspect_export_output_intent"));
    }

    #[test]
    fn export_output_boundary_from_context_uses_export_view_when_view_present() {
        let ctx = ProgramColorContext {
            working_color_space: WorkingColorSpace::LinearRec709,
            output_color_space: ColorSpace::Srgb.into(),
            output_tone_map: true,
            workflow: mondrian_timeline::sequence::ColorWorkflow::DisplayReferred,
            engine: ColorEngine::mondrian_standard(),
            missing_metadata_policy:
                mondrian_timeline::sequence::MissingColorMetadataPolicy::AssumeRec709,
            output_transform: mondrian_core::OutputTransformIntent::mondrian_standard(),
        };

        let boundary = export_output_boundary_from_context(&ctx).expect("encoded output");
        assert_eq!(boundary.target, RenderOutputColorBoundaryTarget::Export);
        assert!(boundary.display_view.is_some());
        assert!(boundary.tone_map);
        let dv = boundary.display_view.as_ref().unwrap();
        assert_eq!(dv.display, "sRGB - Display");
        assert_eq!(dv.view, "Mondrian Standard SDR v2");
    }

    #[test]
    fn export_output_boundary_preserves_engine_intent_without_tone_flag() {
        let ctx = ProgramColorContext {
            working_color_space: WorkingColorSpace::LinearRec709,
            output_color_space: ColorSpace::Rec709.into(),
            output_tone_map: false,
            workflow: mondrian_timeline::sequence::ColorWorkflow::DisplayReferred,
            engine: ColorEngine::mondrian_standard(),
            missing_metadata_policy:
                mondrian_timeline::sequence::MissingColorMetadataPolicy::AssumeRec709,
            output_transform: mondrian_core::OutputTransformIntent::mondrian_standard(),
        };

        let boundary = export_output_boundary_from_context(&ctx).expect("encoded output");
        assert_eq!(boundary.target, RenderOutputColorBoundaryTarget::Export);
        assert!(boundary.display_view.is_some());
        assert!(!boundary.tone_map);
    }

    #[test]
    fn export_health_report_no_issue_when_view_present_with_tone_map() {
        let mut diagnostics = ExportJobColorDiagnostics::default();
        diagnostics.record_frame_diagnostics(
            InputColorResolutionSourceCounts::default(),
            RenderColorStageDiagnostics::default(),
            TimelineCompositeDiagnostics {
                elements: 1,
                float_linear_composites: 1,
                ..TimelineCompositeDiagnostics::default()
            },
        );
        // No record_output_transform_issue call — the view was present.

        let summary = diagnostics.summary().expect("summary");
        assert_eq!(summary.output_transform_issues, 0);
        assert_eq!(summary.output_transform_issue_reasons.total(), 0);
        assert_eq!(
            summary
                .output_transform_issue_reasons
                .tone_map_requested_without_export_view_transform,
            0
        );

        let report = diagnostics.health_report("export-with-view").expect("health report");
        assert_eq!(report.verdict, ExportColorHealthVerdict::Pass);
        assert!(!report
            .root_causes
            .iter()
            .any(|root| root.code == "export_output_transform_issue"));
    }

    /// A versioned engine-owned output intent produces an export-view boundary
    /// and records no transform issue; the queue exposes no authoring override.
    #[test]
    fn export_real_render_with_engine_output_intent_records_no_transform_issue() {
        let mut seq = Sequence::new("engine-output-intent");
        seq.settings.delivery.bit_depth = DeliveryBitDepth::Eight;
        let tb = seq.time_base();
        seq.video_tracks[0]
            .add_clip(
                Clip::new_solid_color(
                    AssetId::new(),
                    mondrian_core::Color::from_rgba8(128, 128, 128, 255),
                    tt(0, tb),
                    tt(1, tb),
                )
                .expect("valid clip"),
            )
            .expect("add solid clip");
        let timeline = TimelineExportSnapshot {
            sequence: seq,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        };
        let ctx = ProgramColorContext {
            working_color_space: WorkingColorSpace::LinearRec709,
            output_color_space: ColorSpace::Rec709.into(),
            output_tone_map: true,
            workflow: mondrian_timeline::sequence::ColorWorkflow::DisplayReferred,
            engine: ColorEngine::mondrian_standard(),
            missing_metadata_policy:
                mondrian_timeline::sequence::MissingColorMetadataPolicy::AssumeRec709,
            output_transform: mondrian_core::OutputTransformIntent::mondrian_standard(),
        };

        let boundary = export_output_boundary_from_context(&ctx).expect("encoded output");
        // The boundary has a view -> no issue should be recorded.
        assert!(boundary.display_view.is_some());
        assert!(boundary.tone_map);
        assert_eq!(boundary.target, RenderOutputColorBoundaryTarget::Export);

        let mut diagnostics = ExportJobColorDiagnostics::default();
        let mut canvas = vec![0u8; 2 * 2 * 4];
        let mut visual_session = ExportVisualRenderSession::default();
        let cancellation = ExecutionCancellationToken::new();
        let mut render_context = ExportFrameRenderContext {
            media: &timeline.media,
            color_environment: &timeline.color_environment,
            alpha_mode: ExportAlphaMode::FlattenBlack,
            frame_contract: ExportFrameContract::Rgba8,
            input_color_counts: None,
            stage_diagnostics: None,
            composite_diagnostics: None,
            export_diagnostics: Some(&mut diagnostics),
            visual_session: &mut visual_session,
            cancellation: &cancellation,
        };
        render_sequence_frame_into(
            &timeline,
            &mut render_context,
            &timeline.sequence,
            0,
            Resolution { width: 2, height: 2 },
            ctx,
            SequenceRenderTarget::Deliverable(&mut canvas),
        )
        .expect("render with engine-owned output intent");

        assert_eq!(canvas.len(), 2 * 2 * 4);
        assert_eq!(diagnostics.output_transform_issues, 0);
        assert_eq!(diagnostics.output_transform_issue_reasons.total(), 0);
    }

    #[test]
    fn export_executes_cross_dissolve_through_shared_working_compositor() {
        let mut sequence = Sequence::new("export Cross Dissolve");
        let time_base = sequence.time_base();
        let left = Clip::new_solid_color(
            AssetId::new(),
            mondrian_core::Color::from_rgba8(255, 0, 0, 255),
            tt(0, time_base),
            tt(2, time_base),
        )
        .expect("left solid");
        let right = Clip::new_solid_color(
            AssetId::new(),
            mondrian_core::Color::from_rgba8(0, 0, 255, 255),
            tt(2, time_base),
            tt(2, time_base),
        )
        .expect("right solid");
        let (left_id, right_id) = (left.id, right.id);
        sequence.video_tracks[0].add_clip(left).expect("left placement");
        sequence.video_tracks[0].add_clip(right).expect("right placement");
        sequence
            .video_transitions
            .push(mondrian_timeline::VideoTransition::cross_dissolve(
                left_id,
                right_id,
                mondrian_core::TimelineTimeRange::new(tt(1, time_base), tt(2, time_base))
                    .expect("transition range"),
            ));
        sequence.validate_author_identities().expect("valid author graph");
        let color_context = sequence
            .settings
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default());
        let mut timeline = TimelineExportSnapshot {
            sequence,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        };
        let mut output = None;
        let mut composite_diagnostics = TimelineCompositeDiagnostics::default();
        let mut visual_session = captured_visual_session_for_test(&mut timeline);
        let cancellation = ExecutionCancellationToken::new();
        let mut render_context = ExportFrameRenderContext {
            media: &timeline.media,
            color_environment: &timeline.color_environment,
            alpha_mode: ExportAlphaMode::Preserve,
            frame_contract: ExportFrameContract::Rgba8,
            input_color_counts: None,
            stage_diagnostics: None,
            composite_diagnostics: Some(&mut composite_diagnostics),
            export_diagnostics: None,
            visual_session: &mut visual_session,
            cancellation: &cancellation,
        };

        render_sequence_frame_into(
            &timeline,
            &mut render_context,
            &timeline.sequence,
            2,
            Resolution { width: 1, height: 1 },
            color_context,
            SequenceRenderTarget::Working(&mut output),
        )
        .expect("render Cross Dissolve");

        let frame = output.expect("working output");
        let pixel = frame.rgba_f32().data[0];
        assert!((pixel[0] - 0.5).abs() < 1.0e-6, "unexpected red: {pixel:?}");
        assert_eq!(pixel[1], 0.0);
        assert!(
            (pixel[2] - 0.5).abs() < 1.0e-6,
            "unexpected blue: {pixel:?}"
        );
        assert_eq!(pixel[3], 1.0);
        assert_eq!(composite_diagnostics.float_linear_composites, 1);
        assert_eq!(composite_diagnostics.legacy_rgba8_composites, 0);
    }

    #[test]
    fn export_executes_basic_title_through_shared_working_compositor() {
        let mut sequence = Sequence::new("export Basic Title");
        sequence.settings.resolution = mondrian_core::Resolution { width: 320, height: 180 };
        let time_base = sequence.time_base();
        sequence.video_tracks[0]
            .add_clip(
                Clip::new_basic_title(
                    "Mondrian",
                    mondrian_core::default_basic_title_font_family(),
                    tt(0, time_base),
                    tt(24, time_base),
                )
                .expect("Basic Title"),
            )
            .expect("title placement");
        sequence.validate_author_identities().expect("valid author graph");
        let color_context = sequence
            .settings
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default());
        let mut timeline = TimelineExportSnapshot {
            sequence,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        };
        let mut output = None;
        let mut composite_diagnostics = TimelineCompositeDiagnostics::default();
        let mut visual_session = captured_visual_session_for_test(&mut timeline);
        let cancellation = ExecutionCancellationToken::new();
        let mut render_context = ExportFrameRenderContext {
            media: &timeline.media,
            color_environment: &timeline.color_environment,
            alpha_mode: ExportAlphaMode::Preserve,
            frame_contract: ExportFrameContract::Rgba8,
            input_color_counts: None,
            stage_diagnostics: None,
            composite_diagnostics: Some(&mut composite_diagnostics),
            export_diagnostics: None,
            visual_session: &mut visual_session,
            cancellation: &cancellation,
        };

        render_sequence_frame_into(
            &timeline,
            &mut render_context,
            &timeline.sequence,
            0,
            Resolution { width: 320, height: 180 },
            color_context,
            SequenceRenderTarget::Working(&mut output),
        )
        .expect("render Basic Title");

        let frame = output.expect("working output");
        assert_eq!(
            frame.descriptor().alpha,
            mondrian_renderer::ColorFrameAlpha::StraightCoverage
        );
        assert!(frame.rgba_f32().data.iter().any(|pixel| pixel[3] > 0.0));
        assert_eq!(composite_diagnostics.float_linear_composites, 1);
        assert_eq!(composite_diagnostics.legacy_rgba8_composites, 0);
    }

    #[test]
    fn nested_basic_title_preserves_child_canvas_and_shared_visual_session() {
        let mut child = Sequence::new("nested Basic Title");
        child.settings.resolution = mondrian_core::Resolution { width: 640, height: 360 };
        let child_time_base = child.time_base();
        child.video_tracks[0]
            .add_clip(
                Clip::new_basic_title(
                    "Nested",
                    mondrian_core::default_basic_title_font_family(),
                    tt(0, child_time_base),
                    tt(24, child_time_base),
                )
                .expect("nested Basic Title"),
            )
            .expect("nested title placement");

        let mut root = Sequence::new("root");
        root.settings.resolution = mondrian_core::Resolution { width: 320, height: 180 };
        let root_time_base = root.time_base();
        root.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(
                    child.id,
                    tt(0, root_time_base),
                    tt(24, root_time_base),
                    None,
                )
                .expect("nested Sequence clip"),
            )
            .expect("nested Sequence placement");
        root.validate_author_identities().expect("valid root author graph");
        child.validate_author_identities().expect("valid child author graph");
        let color_context = root
            .settings
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default());
        let mut timeline = TimelineExportSnapshot {
            sequence: root,
            sequences: vec![child],
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        };
        let mut output = None;
        let mut visual_session = captured_visual_session_for_test(&mut timeline);
        let cancellation = ExecutionCancellationToken::new();
        let mut render_context = ExportFrameRenderContext {
            media: &timeline.media,
            color_environment: &timeline.color_environment,
            alpha_mode: ExportAlphaMode::Preserve,
            frame_contract: ExportFrameContract::Rgba8,
            input_color_counts: None,
            stage_diagnostics: None,
            composite_diagnostics: None,
            export_diagnostics: None,
            visual_session: &mut visual_session,
            cancellation: &cancellation,
        };

        render_sequence_frame_into(
            &timeline,
            &mut render_context,
            &timeline.sequence,
            0,
            Resolution { width: 320, height: 180 },
            color_context,
            SequenceRenderTarget::Working(&mut output),
        )
        .expect("render nested Basic Title");

        let frame = output.expect("nested working output");
        assert!(frame.rgba_f32().data.iter().any(|pixel| pixel[3] > 0.0));
    }

    #[test]
    fn export_rejects_missing_basic_title_font_instead_of_substituting() {
        let mut sequence = Sequence::new("missing Basic Title font");
        sequence.settings.resolution = mondrian_core::Resolution { width: 320, height: 180 };
        let time_base = sequence.time_base();
        sequence.video_tracks[0]
            .add_clip(
                Clip::new_basic_title(
                    "Mondrian",
                    "Mondrian Font That Must Never Exist 8E43D879",
                    tt(0, time_base),
                    tt(24, time_base),
                )
                .expect("valid author title"),
            )
            .expect("title placement");
        let color_context = sequence
            .settings
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default());
        let timeline = TimelineExportSnapshot {
            sequence,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        };
        let mut output = None;
        let mut visual_session = ExportVisualRenderSession::default();
        let cancellation = ExecutionCancellationToken::new();
        let mut render_context = ExportFrameRenderContext {
            media: &timeline.media,
            color_environment: &timeline.color_environment,
            alpha_mode: ExportAlphaMode::Preserve,
            frame_contract: ExportFrameContract::Rgba8,
            input_color_counts: None,
            stage_diagnostics: None,
            composite_diagnostics: None,
            export_diagnostics: None,
            visual_session: &mut visual_session,
            cancellation: &cancellation,
        };

        let error = render_sequence_frame_into(
            &timeline,
            &mut render_context,
            &timeline.sequence,
            0,
            Resolution { width: 320, height: 180 },
            color_context,
            SequenceRenderTarget::Working(&mut output),
        )
        .expect_err("missing font must fail closed");

        assert!(error.contains("Basic Title generation failed closed"));
        assert!(error.contains("Mondrian Font That Must Never Exist 8E43D879"));
        assert!(output.is_none());
    }

    #[test]
    fn direct_export_probe_admits_unresolved_title_font_dependencies() {
        let missing_family = "Mondrian Probe Font That Must Never Exist 73A4146C";
        let mut sequence = Sequence::new("direct probe title-font admission");
        sequence.settings.resolution = mondrian_core::Resolution { width: 320, height: 180 };
        let time_base = sequence.time_base();
        sequence.video_tracks[0]
            .add_clip(
                Clip::new_basic_title(
                    "Mondrian",
                    missing_family,
                    tt(0, time_base),
                    tt(24, time_base),
                )
                .expect("valid author title"),
            )
            .expect("title placement");
        let dependencies = crate::prepare_timeline_export_dependencies(
            &sequence,
            &[],
            TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 1 },
            false,
        )
        .expect("prepare unresolved title dependency");
        assert!(dependencies.execution_snapshot().visual().title_fonts().is_none());
        let timeline = TimelineExportSnapshot::captured(
            mondrian_core::ProjectColorEnvironment::default(),
            sequence,
            Vec::new(),
            HashMap::new(),
            TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 1 },
            dependencies.execution_snapshot().clone(),
        );

        let error = export_composite_diagnostics_for_frame(&timeline, 0, 320, 180)
            .expect_err("missing selected font must fail one direct probe admission");

        assert!(
            error.contains(missing_family),
            "unexpected diagnostic: {error}"
        );
        assert!(
            !error.contains("font dependency closure is unavailable"),
            "direct probe skipped font dependency admission: {error}"
        );
    }

    #[test]
    fn export_color_diagnostics_report_fails_closed_without_frame_evidence() {
        let mut diagnostics = ExportJobColorDiagnostics::default();
        diagnostics.record_asset_issue_summary(VideoColorDiagnosticIssueAggregate {
            diagnostics: 1,
            method_missing_metadata: 1,
            confidence_none: 1,
            missing_cicp_tags: 1,
            unsupported_cicp_tags: 0,
            ..VideoColorDiagnosticIssueAggregate::default()
        });

        let report = diagnostics
            .health_report("export-missing-evidence")
            .expect("export health report");
        assert_eq!(report.verdict, ExportColorHealthVerdict::Fail);
        assert_eq!(report.summary.diagnosed_frames, 0);
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "missing_export_color_evidence"));
        assert!(!report.root_causes.iter().any(|root| {
            root.code == color_report_vocab::root_cause::LEGACY_RGBA8_COMPOSITE_PATH
        }));
    }

    #[test]
    fn export_color_diagnostics_summary_surfaces_asset_issues_without_frame_evidence() {
        let mut diagnostics = ExportJobColorDiagnostics::default();
        diagnostics.record_asset_issue_summary(VideoColorDiagnosticIssueAggregate {
            diagnostics: 1,
            method_missing_metadata: 1,
            confidence_none: 1,
            missing_cicp_tags: 1,
            unsupported_cicp_tags: 0,
            ..VideoColorDiagnosticIssueAggregate::default()
        });

        assert_eq!(
            diagnostics.summary(),
            Some(ExportJobColorDiagnosticsSummary {
                asset_issue_summary: VideoColorDiagnosticIssueAggregate {
                    diagnostics: 1,
                    method_missing_metadata: 1,
                    confidence_none: 1,
                    missing_cicp_tags: 1,
                    unsupported_cicp_tags: 0,
                    ..VideoColorDiagnosticIssueAggregate::default()
                },
                gpu_path_ready: true,
                ..ExportJobColorDiagnosticsSummary::default()
            })
        );
    }

    #[test]
    fn export_media_diagnostic_set_scopes_to_referenced_assets_only() {
        let mut sequence = Sequence::new("export-asset-issue-scope");
        let tb = sequence.time_base();
        let direct_id = AssetId::new();
        let nested_id = AssetId::new();
        let unused_id = AssetId::new();
        let nested_sequence_id = mondrian_core::types::SequenceId::new();

        sequence.video_tracks[0]
            .add_clip(Clip::new(direct_id, tt(0, tb), tt(10, tb)).expect("valid clip"))
            .expect("add direct clip");
        let mut nested_track = Track::new_video("nested");
        nested_track
            .add_clip(
                Clip::new_nested_sequence(
                    nested_sequence_id,
                    tt(0, tb),
                    tt(10, tb),
                    Some("Nested".to_owned()),
                )
                .expect("valid clip"),
            )
            .expect("add nested clip");
        sequence.video_tracks.push(nested_track);

        let mut nested = Sequence::new("nested-issues");
        nested.id = nested_sequence_id;
        let nested_tb = nested.time_base();
        nested.video_tracks[0]
            .add_clip(
                Clip::new(nested_id, tt(0, nested_tb), tt(10, nested_tb)).expect("valid clip"),
            )
            .expect("add nested media clip");

        let mut asset_color_diagnostics = HashMap::new();
        asset_color_diagnostics.insert(
            direct_id,
            test_color_diagnostic(
                mondrian_media::VideoColorSpaceSource::MissingMetadata,
                mondrian_media::VideoColorDetectionMethod::MissingMetadata,
                Some(mondrian_media::VideoColorInterpretationWarning::MissingCicpTags),
            ),
        );
        asset_color_diagnostics.insert(
            nested_id,
            test_color_diagnostic(
                mondrian_media::VideoColorSpaceSource::DecoderUnavailable,
                mondrian_media::VideoColorDetectionMethod::DecoderUnavailable,
                Some(mondrian_media::VideoColorInterpretationWarning::DecoderUnavailable),
            ),
        );
        asset_color_diagnostics.insert(
            unused_id,
            test_color_diagnostic(
                mondrian_media::VideoColorSpaceSource::Metadata,
                mondrian_media::VideoColorDetectionMethod::MetadataHint,
                Some(
                    mondrian_media::VideoColorInterpretationWarning::PartialCicpTags {
                        detected_color_space: ColorSpace::Rec709,
                    },
                ),
            ),
        );

        let media = asset_color_diagnostics
            .into_iter()
            .map(|(asset_id, diagnostic)| {
                (
                    asset_id,
                    test_media_dependency(
                        PathBuf::from(format!("diagnostic-{asset_id}.mov")),
                        None,
                        AssetMediaInterpretation::default(),
                        Some(diagnostic),
                    ),
                )
            })
            .collect();
        let mut timeline = TimelineExportSnapshot {
            sequence,
            sequences: vec![nested],
            media,
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        };
        timeline.prepared_execution = Some(
            crate::prepare_timeline_export_dependencies(
                &timeline.sequence,
                &timeline.sequences,
                timeline.range,
                false,
            )
            .expect("prepare immutable visual diagnostic snapshot")
            .execution_snapshot()
            .clone(),
        );

        assert_eq!(
            export_media_diagnostic_set(&timeline)
                .expect("prepare selected media diagnostics")
                .issue_summary,
            VideoColorDiagnosticIssueAggregate {
                diagnostics: 2,
                diagnostics_with_warnings: 2,
                method_missing_metadata: 1,
                method_decoder_unavailable: 1,
                confidence_none: 2,
                warning_count: 2,
                missing_cicp_tags: 1,
                unsupported_cicp_tags: 0,
                decoder_unavailable: 1,
                ..VideoColorDiagnosticIssueAggregate::default()
            }
        );
    }

    #[test]
    fn export_media_diagnostics_exclude_hidden_muted_and_out_of_range_dependencies() {
        let mut sequence = Sequence::new("selected media diagnostics");
        let rate = sequence.time_base();
        let selected_asset = AssetId::new();
        let range_outside_asset = AssetId::new();
        let hidden_asset = AssetId::new();
        let muted_asset = AssetId::new();
        let nested_selected_asset = AssetId::new();
        let nested_outside_asset = AssetId::new();
        let nested_id = SequenceId::new();

        sequence.video_tracks[0]
            .add_clip(Clip::new(selected_asset, tt(0, rate), tt(10, rate)).expect("selected Clip"))
            .expect("add selected Clip");
        sequence.video_tracks[0]
            .add_clip(
                Clip::new(range_outside_asset, tt(20, rate), tt(10, rate))
                    .expect("range-outside Clip"),
            )
            .expect("add range-outside Clip");
        sequence.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(
                    nested_id,
                    tt(0, rate),
                    tt(10, rate),
                    Some("nested".to_owned()),
                )
                .expect("nested Clip"),
            )
            .expect("add nested Clip");

        let mut hidden = Track::new_video("hidden");
        hidden.is_visible = false;
        hidden
            .add_clip(Clip::new(hidden_asset, tt(0, rate), tt(10, rate)).expect("hidden Clip"))
            .expect("add hidden Clip");
        sequence.video_tracks.push(hidden);
        let mut muted = Track::new_video("muted");
        muted.is_muted = true;
        muted
            .add_clip(Clip::new(muted_asset, tt(0, rate), tt(10, rate)).expect("muted Clip"))
            .expect("add muted Clip");
        sequence.video_tracks.push(muted);

        let mut nested = Sequence::new("nested selected diagnostics");
        nested.id = nested_id;
        let nested_rate = nested.time_base();
        nested.video_tracks[0]
            .add_clip(
                Clip::new(
                    nested_selected_asset,
                    tt(0, nested_rate),
                    tt(10, nested_rate),
                )
                .expect("nested selected Clip"),
            )
            .expect("add nested selected Clip");
        nested.video_tracks[0]
            .add_clip(
                Clip::new(
                    nested_outside_asset,
                    tt(20, nested_rate),
                    tt(10, nested_rate),
                )
                .expect("nested outside Clip"),
            )
            .expect("add nested outside Clip");

        let mut timeline = TimelineExportSnapshot {
            sequence,
            sequences: vec![nested],
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 10 },
        };
        timeline.prepared_execution = Some(
            crate::prepare_timeline_export_dependencies(
                &timeline.sequence,
                &timeline.sequences,
                timeline.range,
                false,
            )
            .expect("prepare immutable selected-range diagnostic snapshot")
            .execution_snapshot()
            .clone(),
        );
        let diagnostics =
            export_media_diagnostic_set(&timeline).expect("selected media diagnostic set");
        let actual = diagnostics.asset_ids.iter().copied().collect::<HashSet<_>>();

        assert_eq!(
            actual,
            HashSet::from([selected_asset, nested_selected_asset])
        );
        assert!(!actual.contains(&range_outside_asset));
        assert!(!actual.contains(&hidden_asset));
        assert!(!actual.contains(&muted_asset));
        assert!(!actual.contains(&nested_outside_asset));
    }

    #[test]
    fn export_color_validation_rejects_camera_log_consumer_codecs() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec709);
        timeline.sequence.settings.delivery.bit_depth = DeliveryBitDepth::Ten;
        let mut config = dummy_config("camera-log.mp4");
        config.preset.color_target =
            crate::preset::ExportColorTarget::Colorimetric(ColorSpace::AppleLogBt2020);

        let err = resolve_timeline_export_delivery(&config, &timeline)
            .expect_err("camera log should reject H.264/MP4 delivery");
        assert!(err.contains("Camera log"));
    }

    #[test]
    fn export_color_validation_allows_camera_log_prores_intermediate() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec709);
        timeline.sequence.settings.delivery.bit_depth = DeliveryBitDepth::Twelve;

        let mut config = dummy_config("camera-log.mov");
        config.preset = crate::preset::ExportPreset::prores_4444_alpha();
        config.preset.alpha_mode = ExportAlphaMode::FlattenBlack;
        config.preset.color_target =
            crate::preset::ExportColorTarget::Colorimetric(ColorSpace::AppleLogBt2020);

        resolve_timeline_export_delivery(&config, &timeline)
            .expect("camera log ProRes intermediate should pass");
    }

    #[test]
    fn export_alpha_validation_rejects_opaque_delivery_codec() {
        let timeline = timeline_input_with_output_color(ColorSpace::Rec709);
        let mut config = dummy_config("alpha.mp4");
        config.preset.alpha_mode = ExportAlphaMode::Preserve;

        let err = resolve_timeline_export_delivery(&config, &timeline)
            .expect_err("H.264 must not pretend to preserve alpha");

        assert!(err.contains("ProRes 4444"));
    }

    #[test]
    fn export_alpha_validation_allows_mov_prores_4444_xq() {
        let timeline = timeline_input_with_output_color(ColorSpace::Rec709);
        let mut config = dummy_config("alpha.mov");
        config.preset = crate::preset::ExportPreset::prores_4444_alpha();

        resolve_timeline_export_delivery(&config, &timeline)
            .expect("MOV ProRes 4444 XQ should preserve alpha");
    }

    #[test]
    fn export_color_validation_binds_prores_profile_to_real_sample_depth() {
        let timeline = timeline_input_with_output_color(ColorSpace::Rec709);
        let mut config = dummy_config("prores.mov");
        config.preset.container = Container::Mov;

        config.preset.video = VideoCodecConfig::ProRes { profile: ProResProfile::Hq };
        config.preset.video_signal = ExportVideoSignal {
            bit_depth: ExportParameter::Explicit(DeliveryBitDepth::Twelve),
            range: ExportParameter::Explicit(VideoRange::Full),
            chroma_sampling: ExportChromaSampling::Yuv422,
        };
        let err = resolve_timeline_export_delivery(&config, &timeline)
            .expect_err("ProRes HQ is a 10-bit profile");
        assert!(err.contains("profile"));

        config.preset.video =
            VideoCodecConfig::ProRes { profile: ProResProfile::FourFourFourFourXq };
        config.preset.video_signal = ExportVideoSignal {
            bit_depth: ExportParameter::Explicit(DeliveryBitDepth::Ten),
            range: ExportParameter::Explicit(VideoRange::Full),
            chroma_sampling: ExportChromaSampling::Yuv444,
        };
        let err = resolve_timeline_export_delivery(&config, &timeline)
            .expect_err("ProRes 4444 XQ is a 12-bit profile");
        assert!(err.contains("profile"));
    }

    #[test]
    fn export_color_validation_rejects_static_hdr_write_without_typed_metadata() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec2100Pq);
        timeline.sequence.settings.delivery.bit_depth = DeliveryBitDepth::Ten;
        timeline.sequence.settings.delivery.static_hdr_metadata_policy =
            StaticHdrMetadataPolicy::WriteAuthored;
        let mut config = dummy_config("hdr-missing-metadata.mp4");
        config.preset = crate::preset::ExportPreset::hevc_main10_aac();

        let err = resolve_timeline_export_delivery(&config, &timeline)
            .expect_err("static HDR writing should require typed metadata");
        assert!(err.contains("SMPTE ST 2086"));
    }

    #[test]
    fn export_color_validation_allows_static_hdr_write_with_typed_metadata() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec2100Pq);
        timeline.sequence.settings.delivery.bit_depth = DeliveryBitDepth::Ten;
        timeline.sequence.settings.delivery.static_hdr_metadata_policy =
            StaticHdrMetadataPolicy::WriteAuthored;
        timeline.sequence.settings.delivery.hdr_mastering_display =
            Some(VideoMasteringDisplayMetadata::rec2100_1000_nit_reference());
        timeline.sequence.settings.delivery.hdr_content_light =
            Some(VideoContentLightMetadata::rec2100_1000_nit_reference());
        let mut config = dummy_config("hdr-with-metadata.mp4");
        config.preset = crate::preset::ExportPreset::hevc_main10_aac();

        resolve_timeline_export_delivery(&config, &timeline)
            .expect("typed HDR metadata should pass validation");
    }

    #[test]
    fn export_color_validation_binds_content_light_to_standard_view_peak() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec2100Pq);
        timeline.sequence.settings.delivery.bit_depth = DeliveryBitDepth::Ten;
        timeline.sequence.settings.delivery.static_hdr_metadata_policy =
            StaticHdrMetadataPolicy::WriteAuthored;
        let mut mastering = VideoMasteringDisplayMetadata::rec2100_1000_nit_reference();
        mastering.luminance.as_mut().expect("reference luminance").max =
            mondrian_core::VideoHdrRational::new(4000, 1);
        timeline.sequence.settings.delivery.hdr_mastering_display = Some(mastering);
        timeline.sequence.settings.delivery.hdr_content_light =
            Some(VideoContentLightMetadata::rec2100_1000_nit_reference());
        let mut config = dummy_config("hdr-content-light-contract.mp4");
        config.preset = crate::preset::ExportPreset::hevc_main10_aac();

        resolve_timeline_export_delivery(&config, &timeline)
            .expect("mastering-display capability may exceed the Standard View's content peak");

        timeline.sequence.settings.delivery.hdr_mastering_display =
            Some(VideoMasteringDisplayMetadata::rec2100_1000_nit_reference());
        timeline.sequence.settings.delivery.hdr_content_light = Some(VideoContentLightMetadata {
            max_content_light_level: 1200,
            max_frame_average_light_level: 400,
        });
        let err = resolve_timeline_export_delivery(&config, &timeline)
            .expect_err("MaxCLL must not exceed the fixed Standard View peak");
        assert!(err.contains("峰值为 1000 nit"));
        assert!(err.contains("MaxCLL 声明 1200 nit"));
    }

    #[test]
    fn export_color_validation_rejects_unimplemented_hdr_metadata_backends() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec2100Pq);
        timeline.sequence.settings.delivery.bit_depth = DeliveryBitDepth::Ten;
        timeline.sequence.settings.delivery.static_hdr_metadata_policy =
            StaticHdrMetadataPolicy::WriteAuthored;
        timeline.sequence.settings.delivery.hdr_mastering_display =
            Some(VideoMasteringDisplayMetadata::rec2100_1000_nit_reference());
        timeline.sequence.settings.delivery.hdr_content_light =
            Some(VideoContentLightMetadata::rec2100_1000_nit_reference());

        for (codec, container, chroma) in [
            (
                VideoCodecConfig::Av1 {
                    profile: Av1Profile::Main,
                    rate_control: VideoRateControl::constant_quality(24),
                },
                Container::Mp4,
                ExportChromaSampling::Yuv420,
            ),
            (
                VideoCodecConfig::ProRes { profile: ProResProfile::Hq },
                Container::Mov,
                ExportChromaSampling::Yuv422,
            ),
        ] {
            let mut config = dummy_config("hdr-unsupported-metadata.mov");
            config.preset.container = container;
            config.preset.video = codec;
            config.preset.video_signal = ExportVideoSignal {
                bit_depth: ExportParameter::Explicit(DeliveryBitDepth::Ten),
                range: ExportParameter::Explicit(VideoRange::Legal),
                chroma_sampling: chroma,
            };
            config.preset.color_target =
                crate::preset::ExportColorTarget::RenderingView(ColorSpace::Rec2100Pq);

            let err = resolve_timeline_export_delivery(&config, &timeline)
                .expect_err("metadata preservation needs a verified encoder backend");
            assert!(err.contains("HEVC Main10/libx265"));
        }
    }

    #[test]
    fn export_color_validation_rejects_dynamic_hdr_passthrough_claim() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec2100Pq);
        timeline.sequence.settings.delivery.bit_depth = DeliveryBitDepth::Ten;
        timeline.sequence.settings.delivery.static_hdr_metadata_policy =
            StaticHdrMetadataPolicy::WriteAuthored;
        timeline.sequence.settings.delivery.hdr_mastering_display =
            Some(VideoMasteringDisplayMetadata::rec2100_1000_nit_reference());
        timeline.sequence.settings.delivery.hdr_content_light =
            Some(VideoContentLightMetadata::rec2100_1000_nit_reference());
        let asset_id = AssetId::new();
        let tb = timeline.sequence.time_base();
        timeline.sequence.video_tracks[0]
            .add_clip(Clip::new(asset_id, tt(0, tb), tt(10, tb)).expect("valid HDR clip"))
            .expect("add HDR clip");
        let mut diagnostic = test_color_diagnostic(
            mondrian_media::VideoColorSpaceSource::Metadata,
            mondrian_media::VideoColorDetectionMethod::CicpTags,
            None,
        );
        diagnostic.hdr_metadata.push(mondrian_media::VideoHdrMetadataSummary {
            kind: mondrian_media::VideoHdrSideDataKind::DynamicHdr10Plus,
            payload_size: 32,
            payload: None,
        });
        timeline.media.insert(
            asset_id,
            test_media_dependency(
                PathBuf::from("dynamic-hdr.mov"),
                None,
                AssetMediaInterpretation::default(),
                Some(diagnostic),
            ),
        );
        refresh_test_execution_snapshot(&mut timeline, false);
        let mut config = dummy_config("hdr-dynamic-passthrough.mp4");
        config.preset = crate::preset::ExportPreset::hevc_main10_aac();

        let error = resolve_timeline_export_delivery(&config, &timeline)
            .expect_err("rendered export must not claim dynamic HDR passthrough");
        assert!(error.contains("HDR10+ 动态 metadata（1 个）"));
        assert!(error.contains("不能安全透传"));
        assert!(error.contains("动态 HDR 重新制作流程"));
    }

    #[test]
    fn static_hdr_delivery_ignores_dynamic_metadata_that_cannot_contribute() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec2100Pq);
        timeline.sequence.settings.delivery.bit_depth = DeliveryBitDepth::Ten;
        timeline.sequence.settings.delivery.static_hdr_metadata_policy =
            StaticHdrMetadataPolicy::WriteAuthored;
        timeline.sequence.settings.delivery.hdr_mastering_display =
            Some(VideoMasteringDisplayMetadata::rec2100_1000_nit_reference());
        timeline.sequence.settings.delivery.hdr_content_light =
            Some(VideoContentLightMetadata::rec2100_1000_nit_reference());
        let rate = timeline.sequence.time_base();
        timeline.sequence.video_tracks[0]
            .add_clip(
                Clip::new(AssetId::new(), tt(0, rate), tt(10, rate))
                    .expect("selected ordinary Clip"),
            )
            .expect("add selected ordinary Clip");

        let outside_id = AssetId::new();
        timeline.sequence.video_tracks[0]
            .add_clip(
                Clip::new(outside_id, tt(20, rate), tt(10, rate)).expect("range-outside HDR Clip"),
            )
            .expect("add range-outside HDR Clip");
        let hidden_id = AssetId::new();
        let mut hidden = Track::new_video("hidden HDR");
        hidden.is_visible = false;
        hidden
            .add_clip(Clip::new(hidden_id, tt(0, rate), tt(10, rate)).expect("hidden HDR Clip"))
            .expect("add hidden HDR Clip");
        timeline.sequence.video_tracks.push(hidden);
        let muted_id = AssetId::new();
        let mut muted = Track::new_video("muted HDR");
        muted.is_muted = true;
        muted
            .add_clip(Clip::new(muted_id, tt(0, rate), tt(10, rate)).expect("muted HDR Clip"))
            .expect("add muted HDR Clip");
        timeline.sequence.video_tracks.push(muted);

        for asset_id in [outside_id, hidden_id, muted_id] {
            let mut diagnostic = test_color_diagnostic(
                mondrian_media::VideoColorSpaceSource::Metadata,
                mondrian_media::VideoColorDetectionMethod::CicpTags,
                None,
            );
            diagnostic.hdr_metadata.push(mondrian_media::VideoHdrMetadataSummary {
                kind: mondrian_media::VideoHdrSideDataKind::DynamicHdr10Plus,
                payload_size: 32,
                payload: None,
            });
            timeline.media.insert(
                asset_id,
                test_media_dependency(
                    PathBuf::from(format!("{asset_id}-dynamic-hdr.mov")),
                    None,
                    AssetMediaInterpretation::default(),
                    Some(diagnostic),
                ),
            );
        }
        timeline.range = TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 10 };
        refresh_test_execution_snapshot(&mut timeline, false);
        let mut config = dummy_config("hdr-selected-range.mp4");
        config.preset = crate::preset::ExportPreset::hevc_main10_aac();

        resolve_timeline_export_delivery(&config, &timeline)
            .expect("non-contributing dynamic HDR metadata must not block static HDR delivery");
    }

    #[test]
    fn export_color_validation_requires_explicit_srgb_for_untagged_gif() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec709);
        timeline.sequence.settings.delivery.bit_depth = DeliveryBitDepth::Eight;
        let mut config = dummy_config("untagged.gif");
        config.preset.container = Container::Gif;
        config.preset.video = VideoCodecConfig::Gif { colors: 256, dither: true };
        config.preset.audio = AudioCodecConfig::Disabled;
        config.preset.video_signal = ExportVideoSignal {
            bit_depth: ExportParameter::Explicit(DeliveryBitDepth::Eight),
            range: ExportParameter::Explicit(VideoRange::Full),
            chroma_sampling: ExportChromaSampling::Rgb,
        };

        let err = resolve_timeline_export_delivery(&config, &timeline)
            .expect_err("untagged GIF must not imply sRGB");

        assert!(err.contains("显式 sRGB"));
    }

    #[test]
    fn timeline_render_range_respects_marked_in_out() {
        let mut seq = Sequence::new("range-test");
        let tb = seq.time_base();
        let clip = Clip::new(AssetId::new(), tt(0, tb), tt(200, tb)).expect("valid clip");
        seq.video_tracks[0].add_clip(clip).expect("add clip");
        seq.in_point = Some(tt(40, tb));
        seq.out_point = Some(tt(99, tb));

        let timeline = TimelineExportSnapshot {
            sequence: seq,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        };

        let range = compute_timeline_render_range(&timeline).expect("valid render range");
        assert_eq!(range.start_frame, 40);
        assert_eq!(range.total_frames, 60);
    }

    #[test]
    fn timeline_render_range_can_export_entire_sequence() {
        let mut seq = Sequence::new("range-entire-test");
        let tb = seq.time_base();
        let clip = Clip::new(AssetId::new(), tt(0, tb), tt(200, tb)).expect("valid clip");
        seq.video_tracks[0].add_clip(clip).expect("add clip");
        seq.in_point = Some(tt(40, tb));
        seq.out_point = Some(tt(99, tb));

        let timeline = TimelineExportSnapshot {
            sequence: seq,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::EntireSequence,
        };

        let range = compute_timeline_render_range(&timeline).expect("valid render range");
        assert_eq!(range.start_frame, 0);
        assert_eq!(range.total_frames, 200);
    }

    #[test]
    fn export_visual_preflight_follows_only_selected_nested_frame_closure() {
        let mut child = Sequence::new("preflight child");
        let child_time_base = child.time_base();
        child.video_tracks[0]
            .add_clip(
                Clip::new_solid_color(
                    AssetId::new(),
                    mondrian_core::Color::BLACK,
                    tt(0, child_time_base),
                    tt(10, child_time_base),
                )
                .expect("safe child Clip"),
            )
            .expect("add safe child Clip");
        let mut blocked = Clip::new_solid_color(
            AssetId::new(),
            mondrian_core::Color::WHITE,
            tt(20, child_time_base),
            tt(10, child_time_base),
        )
        .expect("blocked child Clip");
        let blocked_id = blocked.id;
        blocked.add_effect_node(mondrian_effects::EffectNode::new(
            mondrian_effects::EffectType::Plugin("vendor.missing.child.effect".to_owned()),
        ));
        child.video_tracks[0].add_clip(blocked).expect("add blocked child Clip");

        let mut root = Sequence::new("preflight root");
        let root_time_base = root.time_base();
        root.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(
                    child.id,
                    tt(0, root_time_base),
                    tt(30, root_time_base),
                    None,
                )
                .expect("nested Clip"),
            )
            .expect("add nested Clip");
        let mut timeline = TimelineExportSnapshot {
            sequence: root,
            sequences: vec![child],
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 10 },
        };
        let cancel = ExecutionCancellationToken::new();
        let execution_gate = open_execution_gate();
        let mut session = ExportVisualRenderSession::default();
        let safe_range = compute_timeline_render_range(&timeline).expect("safe export range");
        let safe_result = preflight_timeline_visual_range(
            &timeline,
            safe_range,
            &cancel,
            &execution_gate,
            &mut session,
        );
        assert!(
            safe_result.is_ok(),
            "range outside child blocker must preflight: {safe_result:?}"
        );

        timeline.range = TimelineExportRange::WorkArea { start_frame: 20, end_frame_exclusive: 21 };
        let blocked_range = compute_timeline_render_range(&timeline).expect("blocked export range");
        let result = preflight_timeline_visual_range(
            &timeline,
            blocked_range,
            &cancel,
            &execution_gate,
            &mut session,
        );
        let Err(JobExecutionResult::Failed(reason)) = result else {
            panic!("reachable child blocker must fail export visual preflight");
        };
        assert!(reason.contains(&blocked_id.to_string()), "{reason}");
    }

    #[test]
    fn export_visual_preflight_ignores_unavailable_transition_outside_range() {
        let mut sequence = Sequence::new("range-scoped Transition preflight");
        let time_base = sequence.time_base();
        sequence.video_tracks[0]
            .add_clip(
                Clip::new_solid_color(
                    AssetId::new(),
                    mondrian_core::Color::BLACK,
                    tt(0, time_base),
                    tt(10, time_base),
                )
                .expect("safe Clip"),
            )
            .expect("add safe Clip");
        let left = Clip::new_solid_color(
            AssetId::new(),
            mondrian_core::Color::BLACK,
            tt(20, time_base),
            tt(10, time_base),
        )
        .expect("left Clip");
        let right = Clip::new_solid_color(
            AssetId::new(),
            mondrian_core::Color::WHITE,
            tt(30, time_base),
            tt(10, time_base),
        )
        .expect("right Clip");
        let (left_id, right_id) = (left.id, right.id);
        sequence.video_tracks[0].add_clip(left).expect("add left Clip");
        sequence.video_tracks[0].add_clip(right).expect("add right Clip");
        let mut transition = mondrian_timeline::VideoTransition::cross_dissolve(
            left_id,
            right_id,
            mondrian_core::TimelineTimeRange::new(tt(28, time_base), tt(4, time_base))
                .expect("Transition range"),
        );
        transition.transition_type = mondrian_timeline::VideoTransitionType::Plugin {
            definition_id: "vendor.missing.transition".to_owned(),
        };
        let transition_id = transition.id;
        sequence.video_transitions.push(transition);

        let mut timeline = TimelineExportSnapshot {
            sequence,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 10 },
        };
        let cancel = ExecutionCancellationToken::new();
        let execution_gate = open_execution_gate();
        let mut session = ExportVisualRenderSession::default();
        let safe_range = compute_timeline_render_range(&timeline).expect("safe range");
        assert!(
            preflight_timeline_visual_range(
                &timeline,
                safe_range,
                &cancel,
                &execution_gate,
                &mut session,
            )
            .is_ok(),
            "range before unavailable Transition must preflight"
        );

        timeline.range = TimelineExportRange::WorkArea { start_frame: 29, end_frame_exclusive: 30 };
        let blocked_range = compute_timeline_render_range(&timeline).expect("blocked range");
        let result = preflight_timeline_visual_range(
            &timeline,
            blocked_range,
            &cancel,
            &execution_gate,
            &mut session,
        );
        let Err(JobExecutionResult::Failed(reason)) = result else {
            panic!("reachable unavailable Transition must fail export visual preflight");
        };
        assert!(reason.contains(&transition_id.to_string()), "{reason}");
    }

    #[test]
    fn export_visual_preflight_does_not_prepare_unrelated_sequence() {
        use mondrian_effects::{
            register_effect_definition, EffectColorDomainContract, EffectDefinition,
            EffectDeterminism, EffectExecutionContract, EffectExecutionModes, EffectGraphTopology,
            EffectNode, EffectResourceLifetime, EffectRoiPropagation, EffectStateModel,
            EffectTemporalInputExtent, EffectType,
        };

        let evaluations = Arc::new(AtomicUsize::new(0));
        let evaluations_for_builder = Arc::clone(&evaluations);
        let effect_type = EffectType::Plugin(format!(
            "test.export.preflight.unrelated.{}",
            AssetId::new()
        ));
        register_effect_definition(
            EffectDefinition::new(
                effect_type.key(),
                "Unrelated Sequence probe",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                determinism: EffectDeterminism::Deterministic,
                state_model: EffectStateModel::Stateless,
                temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
                roi_propagation: EffectRoiPropagation::PixelLocal,
                resource_lifetime: EffectResourceLifetime::Frame,
                topology: EffectGraphTopology::LinearChain,
            })
            .with_graph_builder(Arc::new(move |_, _, _| {
                evaluations_for_builder.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })),
        )
        .expect("register unrelated Sequence probe");

        let mut root = Sequence::new("selected root");
        let root_time_base = root.time_base();
        root.video_tracks[0]
            .add_clip(
                Clip::new_solid_color(
                    AssetId::new(),
                    mondrian_core::Color::BLACK,
                    tt(0, root_time_base),
                    tt(10, root_time_base),
                )
                .expect("root solid"),
            )
            .expect("add root solid");

        let mut unrelated = Sequence::new("unrelated Sequence");
        let unrelated_time_base = unrelated.time_base();
        let mut unrelated_clip = Clip::new_solid_color(
            AssetId::new(),
            mondrian_core::Color::WHITE,
            tt(0, unrelated_time_base),
            tt(10, unrelated_time_base),
        )
        .expect("unrelated solid");
        unrelated_clip.add_effect_node(EffectNode::new(effect_type));
        unrelated.video_tracks[0].add_clip(unrelated_clip).expect("add unrelated solid");

        let timeline = TimelineExportSnapshot {
            sequence: root,
            sequences: vec![unrelated],
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 1 },
        };
        let range = compute_timeline_render_range(&timeline).expect("selected range");
        let execution_gate = open_execution_gate();
        let mut session = ExportVisualRenderSession::default();
        assert!(
            preflight_timeline_visual_range(
                &timeline,
                range,
                &ExecutionCancellationToken::new(),
                &execution_gate,
                &mut session,
            )
            .is_ok(),
            "selected range must preflight"
        );
        assert_eq!(
            evaluations.load(Ordering::SeqCst),
            0,
            "the range-local capture must not bind Effects from an unrelated Sequence"
        );
    }

    #[test]
    fn admitted_visual_snapshot_survives_live_definition_replacement() {
        use mondrian_effects::{
            register_effect_definition, EffectColorDomainContract, EffectDefinition,
            EffectDeterminism, EffectExecutionContract, EffectExecutionModes, EffectGraphTopology,
            EffectNode, EffectResourceLifetime, EffectRoiPropagation, EffectStateModel,
            EffectTemporalInputExtent, EffectTemporalSpan, EffectType,
        };

        let first_builds = Arc::new(AtomicUsize::new(0));
        let first_builds_for_definition = Arc::clone(&first_builds);
        let replacement_builds = Arc::new(AtomicUsize::new(0));
        let replacement_builds_for_definition = Arc::clone(&replacement_builds);
        let effect_type =
            EffectType::Plugin(format!("test.export.frozen-definition.{}", AssetId::new()));
        let definition = |temporal_input, builder: mondrian_effects::EffectGraphBuilder| {
            EffectDefinition::new(
                effect_type.key(),
                "Frozen export definition",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                determinism: EffectDeterminism::Deterministic,
                state_model: EffectStateModel::Stateless,
                temporal_input,
                roi_propagation: EffectRoiPropagation::PixelLocal,
                resource_lifetime: EffectResourceLifetime::Frame,
                topology: EffectGraphTopology::LinearChain,
            })
            .with_graph_builder(builder)
        };
        register_effect_definition(definition(
            EffectTemporalInputExtent::CURRENT_FRAME,
            Arc::new(move |_, _, _| {
                first_builds_for_definition.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        ))
        .expect("register admitted definition");

        let mut sequence = Sequence::new("frozen visual definition");
        sequence.video_tracks.clear();
        let time_base = sequence.time_base();
        let outside_asset = AssetId::new();
        let selected_asset = AssetId::new();
        let mut child = Sequence::new("frozen definition child");
        child.video_tracks.clear();
        let child_time_base = child.time_base();
        let mut child_track = Track::new_video("V1");
        child_track
            .add_clip(
                Clip::new(
                    outside_asset,
                    tt(0, child_time_base),
                    tt(5, child_time_base),
                )
                .expect("outside child Clip"),
            )
            .expect("add outside child Clip");
        child_track
            .add_clip(
                Clip::new(
                    selected_asset,
                    tt(10, child_time_base),
                    tt(5, child_time_base),
                )
                .expect("selected child Clip"),
            )
            .expect("add selected child Clip");
        child.video_tracks.push(child_track);
        let mut track = Track::new_video("V1");
        let mut nested = Clip::new_nested_sequence(
            child.id,
            tt(0, time_base),
            tt(15, time_base),
            Some("child".to_owned()),
        )
        .expect("nested Clip");
        nested.add_effect_node(EffectNode::new(effect_type.clone()));
        track.add_clip(nested).expect("add nested Clip");
        sequence.video_tracks.push(track);
        let range = TimelineExportRange::WorkArea { start_frame: 10, end_frame_exclusive: 11 };

        let admitted = crate::prepare_timeline_export_dependencies(
            &sequence,
            std::slice::from_ref(&child),
            range,
            false,
        )
        .expect("capture admitted visual closure");
        assert!(admitted.media_components().contains_key(&selected_asset));
        assert!(!admitted.media_components().contains_key(&outside_asset));
        assert_eq!(first_builds.load(Ordering::SeqCst), 1);
        let admitted_program = admitted
            .execution_snapshot()
            .visual()
            .program(sequence.id, sequence.revision)
            .expect("admitted root Program");

        register_effect_definition(definition(
            EffectTemporalInputExtent {
                past: EffectTemporalSpan::Finite(tt(10, time_base)),
                future: EffectTemporalSpan::None,
            },
            Arc::new(move |_, _, _| {
                replacement_builds_for_definition.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        ))
        .expect("replace live definition");
        let live = crate::prepare_timeline_export_dependencies(
            &sequence,
            std::slice::from_ref(&child),
            range,
            false,
        )
        .expect("capture replacement visual closure");
        assert!(
            live.media_components().contains_key(&outside_asset),
            "replacement temporal extent must prove that live reinterpretation differs"
        );
        let replacement_build_count = replacement_builds.load(Ordering::SeqCst);
        assert!(replacement_build_count > 0);

        let mut admitted_execution = admitted.execution_snapshot().clone();
        admitted_execution
            .visual_mut()
            .install_title_fonts(mondrian_renderer::PreparedBasicTitleFontSet::default())
            .expect("seal empty title-font closure");
        let mut timeline = TimelineExportSnapshot {
            sequence,
            sequences: vec![child],
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: Some(admitted_execution),
            range,
        };
        let mut session = ExportVisualRenderSession::for_execution_generation(
            0,
            service::ExportExecutionResourcePolicy::default(),
            timeline
                .prepared_execution
                .as_ref()
                .expect("frozen execution snapshot")
                .visual(),
        )
        .expect("install admitted Programs");
        let worker_program =
            session.prepare_program(&timeline.sequence).expect("worker frozen Program");
        assert!(Arc::ptr_eq(&worker_program, &admitted_program));
        assert_eq!(
            replacement_builds.load(Ordering::SeqCst),
            replacement_build_count,
            "worker must not rebuild from the replacement registry definition"
        );
        let diagnostic_assets = export_media_diagnostic_set(&timeline)
            .expect("frozen diagnostic identities")
            .asset_ids;
        assert_eq!(diagnostic_assets.as_ref(), &[selected_asset]);

        timeline.prepared_execution = Some(live.execution_snapshot().clone());
        assert!(
            export_media_diagnostic_set(&timeline)
                .expect("replacement diagnostic identities")
                .asset_ids
                .contains(&outside_asset),
            "control snapshot must expose replacement temporal reachability"
        );
    }

    #[test]
    fn export_visual_preflight_rejects_dynamic_identity_execution_before_rendering() {
        use mondrian_effects::{
            register_effect_definition, EffectColorDomainContract, EffectDefinition,
            EffectDeterminism, EffectExecutionContract, EffectExecutionModes, EffectGraphTopology,
            EffectNode, EffectResourceLifetime, EffectRoiPropagation, EffectStateModel,
            EffectTemporalInputExtent, EffectType,
        };

        let effect_type = EffectType::Plugin(format!(
            "test.export.preflight.stateful_identity.{}",
            AssetId::new()
        ));
        register_effect_definition(
            EffectDefinition::new(
                effect_type.key(),
                "Stateful identity",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                determinism: EffectDeterminism::Deterministic,
                state_model: EffectStateModel::StatefulSequential,
                temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
                roi_propagation: EffectRoiPropagation::PixelLocal,
                resource_lifetime: EffectResourceLifetime::ContinuitySession,
                topology: EffectGraphTopology::LinearChain,
            })
            .with_graph_builder(Arc::new(|_, _, _| Ok(()))),
        )
        .expect("register stateful identity");

        let mut sequence = Sequence::new("dynamic execution preflight");
        let time_base = sequence.time_base();
        let mut blocked = Clip::new_solid_color(
            AssetId::new(),
            mondrian_core::Color::BLACK,
            tt(20, time_base),
            tt(10, time_base),
        )
        .expect("blocked Clip");
        blocked.add_effect_node(EffectNode::new(effect_type));
        sequence.video_tracks[0].add_clip(blocked).expect("add blocked Clip");
        let mut timeline = TimelineExportSnapshot {
            sequence,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 10 },
        };
        let cancel = ExecutionCancellationToken::new();
        let execution_gate = open_execution_gate();
        let mut session = ExportVisualRenderSession::default();
        let safe_range = compute_timeline_render_range(&timeline).expect("safe range");
        assert!(
            preflight_timeline_visual_range(
                &timeline,
                safe_range,
                &cancel,
                &execution_gate,
                &mut session,
            )
            .is_ok(),
            "unreached dynamic execution obligation must not block a selected range"
        );

        timeline.range = TimelineExportRange::WorkArea { start_frame: 20, end_frame_exclusive: 21 };
        let blocked_range = compute_timeline_render_range(&timeline).expect("blocked range");
        let Err(JobExecutionResult::Failed(reason)) = preflight_timeline_visual_range(
            &timeline,
            blocked_range,
            &cancel,
            &execution_gate,
            &mut session,
        ) else {
            panic!("reachable stateful identity must fail CPU single-frame admission");
        };
        assert!(
            reason.contains("current export compositor"),
            "unexpected preflight diagnostic: {reason}"
        );
    }

    #[test]
    fn ten_bit_export_preflight_rejects_u8_only_working_composite() {
        use mondrian_effects::{
            register_effect_definition, EffectColorDomainContract, EffectDefinition,
            EffectDeterminism, EffectExecutionContract, EffectExecutionModes, EffectGraphTopology,
            EffectNode, EffectResourceLifetime, EffectRoiPropagation, EffectStateModel,
            EffectTemporalInputExtent, EffectType,
        };

        let effect_type = EffectType::Plugin(format!(
            "test.export.preflight.u8_only_identity.{}",
            AssetId::new()
        ));
        register_effect_definition(
            EffectDefinition::new(
                effect_type.key(),
                "U8-only identity",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_U8,
                determinism: EffectDeterminism::Deterministic,
                state_model: EffectStateModel::Stateless,
                temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
                roi_propagation: EffectRoiPropagation::PixelLocal,
                resource_lifetime: EffectResourceLifetime::Frame,
                topology: EffectGraphTopology::LinearChain,
            })
            .with_graph_builder(Arc::new(|_, _, _| Ok(()))),
        )
        .expect("register U8-only identity");

        let mut sequence = Sequence::new("ten-bit U8 working-composite rejection");
        sequence.settings.delivery.bit_depth = DeliveryBitDepth::Ten;
        let time_base = sequence.time_base();
        let mut clip = Clip::new_solid_color(
            AssetId::new(),
            mondrian_core::Color::BLACK,
            tt(0, time_base),
            tt(1, time_base),
        )
        .expect("solid Clip");
        clip.add_effect_node(EffectNode::new(effect_type));
        sequence.video_tracks[0].add_clip(clip).expect("add solid Clip");
        let timeline = TimelineExportSnapshot {
            sequence,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 1 },
        };

        let range = compute_timeline_render_range(&timeline).expect("selected range");
        let result = preflight_timeline_visual_range(
            &timeline,
            range,
            &ExecutionCancellationToken::new(),
            &open_execution_gate(),
            &mut ExportVisualRenderSession::default(),
        );
        let Err(JobExecutionResult::Failed(reason)) = result else {
            panic!("ten-bit export must fail before an implicit RGBA8 working composite");
        };
        assert!(
            reason.contains("ExecutionModeNotAdmitted") && reason.contains("Float32"),
            "unexpected high-precision rejection: {reason}"
        );
    }

    #[test]
    fn export_visual_preflight_evaluates_animated_builders_before_audio_or_encoder_work() {
        use mondrian_effects::{
            register_effect_definition, EffectColorDomainContract, EffectDefinition,
            EffectDeterminism, EffectExecutionContract, EffectExecutionModes, EffectGraphTopology,
            EffectNode, EffectResourceLifetime, EffectRoiPropagation, EffectStateModel,
            EffectTemporalInputExtent, EffectType,
        };

        let effect_type = EffectType::Plugin(format!(
            "test.export.preflight.dynamic_panic.{}",
            AssetId::new()
        ));
        register_effect_definition(
            EffectDefinition::new(
                effect_type.key(),
                "Dynamic builder failure",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                determinism: EffectDeterminism::Deterministic,
                state_model: EffectStateModel::Stateless,
                temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
                roi_propagation: EffectRoiPropagation::PixelLocal,
                resource_lifetime: EffectResourceLifetime::Frame,
                topology: EffectGraphTopology::LinearChain,
            })
            .with_graph_builder(Arc::new(|_, context, _| {
                if context.time != TimelineTime::ZERO {
                    panic!("intentional dynamic builder failure");
                }
                Ok(())
            })),
        )
        .expect("register dynamic builder");

        let mut sequence = Sequence::new("dynamic builder preflight");
        let time_base = sequence.time_base();
        let mut clip = Clip::new_solid_color(
            AssetId::new(),
            mondrian_core::Color::BLACK,
            tt(0, time_base),
            tt(10, time_base),
        )
        .expect("dynamic Clip");
        clip.add_effect_node(EffectNode::new(effect_type));
        sequence.video_tracks[0].add_clip(clip).expect("add dynamic Clip");
        let mut timeline = TimelineExportSnapshot {
            sequence,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 1 },
        };
        let cancel = ExecutionCancellationToken::new();
        let execution_gate = open_execution_gate();
        let mut session = ExportVisualRenderSession::default();
        let zero_range = compute_timeline_render_range(&timeline).expect("frame-zero range");
        assert!(
            preflight_timeline_visual_range(
                &timeline,
                zero_range,
                &cancel,
                &execution_gate,
                &mut session,
            )
            .is_ok(),
            "the canonical zero-time graph is valid"
        );

        timeline.range = TimelineExportRange::WorkArea { start_frame: 1, end_frame_exclusive: 2 };
        let dynamic_range = compute_timeline_render_range(&timeline).expect("dynamic range");
        let Err(JobExecutionResult::Failed(reason)) = preflight_timeline_visual_range(
            &timeline,
            dynamic_range,
            &cancel,
            &execution_gate,
            &mut session,
        ) else {
            panic!("animated builder panic must fail visual preflight");
        };
        assert!(
            reason.contains("visual evaluation failed")
                && reason.contains("Effect evaluation failed")
                && reason.contains("panicked"),
            "unexpected preflight diagnostic: {reason}"
        );
    }

    #[test]
    fn export_render_readmits_dynamic_plan_before_media_resolution() {
        use mondrian_effects::{
            register_effect_definition, EffectColorDomain, EffectColorDomainContract,
            EffectDefinition, EffectDeterminism, EffectExecutionContract, EffectExecutionModes,
            EffectGraphTopology, EffectNode, EffectRenderOp, EffectResourceLifetime,
            EffectRoiPropagation, EffectStateModel, EffectTemporalInputExtent, EffectType,
        };

        let evaluations = Arc::new(AtomicUsize::new(0));
        let evaluations_for_builder = Arc::clone(&evaluations);
        let emit_blocked_domain = Arc::new(AtomicBool::new(false));
        let emit_blocked_domain_for_builder = Arc::clone(&emit_blocked_domain);
        let effect_type = EffectType::Plugin(format!(
            "test.export.render.dynamic_readmission.{}",
            AssetId::new()
        ));
        register_effect_definition(
            EffectDefinition::new(
                effect_type.key(),
                "Dynamic readmission probe",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                determinism: EffectDeterminism::Deterministic,
                state_model: EffectStateModel::Stateless,
                temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
                roi_propagation: EffectRoiPropagation::PixelLocal,
                resource_lifetime: EffectResourceLifetime::Frame,
                topology: EffectGraphTopology::LinearChain,
            })
            .with_graph_builder(Arc::new(move |_, _, graph| {
                let operation = EffectRenderOp::ColorAdjust {
                    exposure: 0.0,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: WorkingColorSpace::LinearRec709,
                };
                evaluations_for_builder.fetch_add(1, Ordering::SeqCst);
                if !emit_blocked_domain_for_builder.load(Ordering::SeqCst) {
                    graph.append_unary(operation);
                } else {
                    graph.append_unary_in_domain(
                        operation,
                        EffectColorDomainContract::preserving(EffectColorDomain::Data),
                    );
                }
                Ok(())
            })),
        )
        .expect("register dynamic readmission probe");

        let asset_id = AssetId::new();
        let mut sequence = Sequence::new("dynamic render readmission");
        let time_base = sequence.time_base();
        let mut clip =
            Clip::new(asset_id, tt(0, time_base), tt(10, time_base)).expect("media Clip");
        clip.add_effect_node(EffectNode::new(effect_type));
        sequence.video_tracks[0].add_clip(clip).expect("add media Clip");
        let timeline = TimelineExportSnapshot {
            sequence,
            sequences: Vec::new(),
            // Deliberately absent: reaching media resolution would fail with a
            // different diagnostic and prove admission happened too late.
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::WorkArea { start_frame: 1, end_frame_exclusive: 2 },
        };
        let range = compute_timeline_render_range(&timeline).expect("single-frame range");
        let execution_gate = open_execution_gate();
        let mut session = ExportVisualRenderSession::default();
        preflight_timeline_visual_range(
            &timeline,
            range,
            &ExecutionCancellationToken::new(),
            &execution_gate,
            &mut session,
        )
        .expect("first dynamic graph is admitted");
        let preflight_evaluations = evaluations.load(Ordering::SeqCst);
        assert!(
            preflight_evaluations >= 2,
            "preparation and selected-frame preflight must both evaluate the dynamic graph"
        );
        emit_blocked_domain.store(true, Ordering::SeqCst);

        let color_context = timeline
            .sequence
            .settings
            .root_program_color_context(&timeline.color_environment);
        let mut canvas = Vec::new();
        let error = render_timeline_frame_into_with_session(
            &timeline,
            1,
            16,
            16,
            ExportAlphaMode::FlattenBlack,
            color_context,
            ExportFrameContract::Rgba8,
            &mut canvas,
            None,
            None,
            None,
            None,
            &mut session,
        )
        .expect_err("changed dynamic graph must be re-admitted");
        assert!(
            error.contains("cannot enter the current export compositor"),
            "unexpected render diagnostic: {error}"
        );
        assert!(
            !error.contains("缺少素材依赖"),
            "media resolution ran before dynamic-plan admission: {error}"
        );
        assert!(
            evaluations.load(Ordering::SeqCst) > preflight_evaluations,
            "render must re-evaluate the pinned dynamic program before media resolution"
        );
    }

    #[test]
    fn export_input_color_resolution_counts_for_frame_tracks_media_sources() {
        let mut seq = Sequence::new("export-input-color-counts");
        seq.settings.color.working_color_space = WorkingColorSpace::LinearRec2020;
        seq.settings.color.input.missing_metadata_policy = MissingColorMetadataPolicy::AssumeRec709;
        let tb = seq.time_base();
        let detected_id = AssetId::new();
        let override_id = AssetId::new();
        let missing_id = AssetId::new();
        let data_id = AssetId::new();

        seq.video_tracks[0]
            .add_clip(Clip::new(detected_id, tt(0, tb), tt(10, tb)).expect("valid clip"))
            .expect("add detected clip");
        for (name, asset_id) in [
            ("override", override_id),
            ("missing", missing_id),
            ("data", data_id),
        ] {
            let mut track = Track::new_video(name);
            track
                .add_clip(Clip::new(asset_id, tt(0, tb), tt(10, tb)).expect("valid clip"))
                .expect("add clip");
            seq.video_tracks.push(track);
        }

        let mut timeline = TimelineExportSnapshot {
            sequence: seq,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        };
        timeline.media.insert(
            detected_id,
            test_media_dependency(
                PathBuf::from("detected.mov"),
                Some(ColorSpace::Srgb),
                AssetMediaInterpretation::default(),
                None,
            ),
        );
        timeline.media.insert(
            override_id,
            test_media_dependency(
                PathBuf::from("override.mov"),
                None,
                AssetMediaInterpretation {
                    color: MediaColorInterpretation::Override {
                        color_space: ColorSpace::SonySLog3SGamut3Cine,
                    },
                    ..AssetMediaInterpretation::default()
                },
                None,
            ),
        );
        timeline.media.insert(
            missing_id,
            test_media_dependency(
                PathBuf::from("missing.mov"),
                None,
                AssetMediaInterpretation::default(),
                None,
            ),
        );
        timeline.media.insert(
            data_id,
            test_media_dependency(
                PathBuf::from("data.exr"),
                None,
                AssetMediaInterpretation {
                    payload: AssetColorPayload::NonColorData,
                    ..AssetMediaInterpretation::default()
                },
                None,
            ),
        );

        let counts = export_input_color_resolution_counts_for_frame(&timeline, 0)
            .expect("collect export color source counts");

        assert_eq!(counts.total(), 4);
        assert_eq!(
            counts.count(InputColorResolutionSource::DetectedMetadata),
            1
        );
        assert_eq!(counts.count(InputColorResolutionSource::Override), 1);
        assert_eq!(
            counts.count(InputColorResolutionSource::MissingPolicyAssumeRec709),
            1
        );
        assert_eq!(counts.count(InputColorResolutionSource::DataTexture), 1);
    }

    #[test]
    fn export_input_range_honors_asset_override_over_probe_diagnostic() {
        let asset_id = AssetId::new();
        let mut timeline = timeline_input_with_output_color(ColorSpace::Srgb);
        let mut diagnostic = test_color_diagnostic(
            mondrian_media::VideoColorSpaceSource::Metadata,
            mondrian_media::VideoColorDetectionMethod::CicpTags,
            None,
        );
        diagnostic.color_range = DecodedVideoRange::Limited;
        timeline.media.insert(
            asset_id,
            test_media_dependency(
                PathBuf::from("range.mov"),
                None,
                AssetMediaInterpretation::default(),
                Some(diagnostic),
            ),
        );
        let interpretation = AssetMediaInterpretation {
            range: MediaRangeInterpretation::Override { range: MediaSignalRange::Full },
            ..AssetMediaInterpretation::default()
        };

        assert_eq!(
            resolve_export_input_video_range(&timeline.media, asset_id, interpretation),
            DecodedVideoRangeContract::OverrideFull
        );
        assert_eq!(
            resolve_export_input_video_range(
                &timeline.media,
                asset_id,
                AssetMediaInterpretation::default()
            ),
            DecodedVideoRangeContract::Automatic { probed_range: DecodedVideoRange::Limited }
        );
    }

    #[test]
    fn admitted_audio_demand_preserves_mute_and_visibility_semantics() {
        let mut seq = Sequence::new("audio-range-test");
        let tb = seq.time_base();
        let asset_id = AssetId::new();
        let clip = Clip::new(asset_id, tt(25, tb), tt(20, tb)).expect("valid clip");
        let track_id = seq.audio_tracks[0].id;
        seq.add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
            .expect("add audio clip");
        seq.in_point = Some(tt(30, tb));
        seq.out_point = Some(tt(40, tb));
        let demand = |sequence: &Sequence| {
            crate::prepare_timeline_export_dependencies(
                sequence,
                &[],
                TimelineExportRange::SequenceInOut,
                true,
            )
            .expect("prepare exact audio closure")
            .execution_snapshot()
            .audio()
            .expect("audio evidence")
            .execution_demand()
        };

        assert!(demand(&seq).requires_execution());
        seq.audio_tracks[0].is_muted = true;
        assert!(
            !demand(&seq).requires_execution(),
            "a muted post-mute source is proven silent"
        );
        seq.audio_tracks[0].is_muted = false;
        seq.audio_tracks[0].is_visible = false;
        assert!(
            demand(&seq).requires_execution(),
            "audio Track UI visibility must not gate Program signal"
        );
    }

    #[test]
    fn admitted_audio_demand_retains_muted_pre_mute_send_and_processor_only_bus() {
        let mut sequence = Sequence::new("muted pre-mute Export demand");
        let time_base = sequence.time_base();
        let track_id = sequence.audio_tracks[0].id;
        sequence
            .add_media_audio_clip(
                track_id,
                Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base)).expect("audio Clip"),
                AudioSourceComponentId::primary(),
            )
            .expect("add audio Clip");
        sequence.audio_tracks[0].is_muted = true;
        sequence.audio_program.routes[0].source = mondrian_timeline::AudioRouteSource::Track {
            track_id,
            port: mondrian_timeline::AudioChannelStripOutputPort::PreFader,
        };
        let pre_mute = crate::prepare_timeline_export_dependencies(
            &sequence,
            &[],
            TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 10 },
            true,
        )
        .expect("prepare pre-mute Export closure");
        assert!(
            pre_mute
                .execution_snapshot()
                .audio()
                .expect("pre-mute audio evidence")
                .execution_demand()
                .requires_execution(),
            "a muted Track's selected pre-mute Route still requires canonical execution"
        );

        let mut processor_only = Sequence::new("processor-only Bus Export demand");
        let output_id = processor_only.audio_program.outputs[0].id;
        let bus_id = mondrian_core::MixBusId::new();
        let mut strip = mondrian_timeline::AudioChannelStrip::default();
        strip.pre_fader.processors.push(mondrian_timeline::AudioProcessorInstance {
            id: mondrian_core::AudioProcessorInstanceId::new(),
            definition: mondrian_timeline::AudioProcessorDefinitionRef::Clap {
                plugin_id: "test.mondrian.generator-capable".to_owned(),
                schema_version: 1,
            },
            bypassed: false,
            parameters: Default::default(),
            opaque_state: None,
        });
        processor_only.audio_program.buses.push(mondrian_timeline::AudioMixBus {
            id: bus_id,
            name: "Generator-capable Bus".to_owned(),
            strip,
        });
        processor_only.audio_program.routes.push(mondrian_timeline::AudioRoute::new(
            mondrian_timeline::AudioRouteSource::Bus {
                bus_id,
                port: mondrian_timeline::AudioChannelStripOutputPort::PreFader,
            },
            mondrian_timeline::AudioRouteDestination::Output(output_id),
        ));
        let bus = crate::prepare_timeline_export_dependencies(
            &processor_only,
            &[],
            TimelineExportRange::EntireSequence,
            true,
        )
        .expect("prepare processor-only Bus closure");
        let audio = bus.execution_snapshot().audio().expect("processor-only audio evidence");
        assert!(audio.media_components().is_empty());
        assert!(
            audio.execution_demand().requires_execution(),
            "empty media reachability cannot authorize silence when a selected processor may generate signal or tail"
        );
    }

    #[test]
    fn export_audio_resolver_accepts_frozen_non_primary_stream_binding() {
        let source = tempfile::NamedTempFile::new().expect("temporary audio source");
        let asset_id = AssetId::new();
        let component_id = AudioSourceComponentId::new();
        let fingerprint = MediaFileFingerprint::capture(source.path());
        let mut dependency = test_media_dependency(
            source.path().to_path_buf(),
            None,
            AssetMediaInterpretation::default(),
            None,
        );
        dependency.audio_components.insert(
            component_id,
            mondrian_media::AudioSourceSelection::new(
                3,
                mondrian_media::info::ChannelLayout::Stereo,
                fingerprint,
            ),
        );
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec709);
        timeline.media.insert(asset_id, dependency);
        let cache = Arc::new(AudioSourceCache::new(48_000));
        let resolver = ExportAudioMediaResolver { timeline: &timeline, cache };

        assert!(resolver.resolve(asset_id, component_id, 48_000,).is_ok());
        assert!(resolver.resolve(asset_id, AudioSourceComponentId::new(), 48_000,).is_err());
    }

    #[test]
    fn timeline_audio_sample_range_matches_frame_duration() {
        let range = TimelineRenderRange {
            start_frame: 0,
            total_frames: 50,
            fps_num: 25,
            fps_den: 1,
        };
        assert_eq!(timeline_audio_sample_range(range, 48_000), Ok((0, 96_000)));
    }

    #[test]
    fn ffmpeg_audio_output_lowering_rejects_unnegotiated_layouts() {
        assert_eq!(
            ffmpeg_audio_channel_layout(AudioChannelLayout::Mono),
            Some("mono")
        );
        assert_eq!(
            ffmpeg_audio_channel_layout(AudioChannelLayout::Stereo),
            Some("stereo")
        );
        assert_eq!(
            ffmpeg_audio_channel_layout(AudioChannelLayout::Surround51Side),
            Some("5.1(side)")
        );
        assert_eq!(
            ffmpeg_audio_channel_layout(AudioChannelLayout::Surround51Back),
            Some("5.1")
        );
        assert_eq!(
            ffmpeg_audio_channel_layout(AudioChannelLayout::Surround71),
            Some("7.1")
        );
        assert_eq!(
            ffmpeg_audio_channel_layout(AudioChannelLayout::discrete(8).expect("discrete layout")),
            None
        );
    }

    #[test]
    fn ffmpeg_codec_args_lower_profiles_and_vbv_without_ambiguous_bitrate_mode() {
        let mut h264 = Command::new("ffmpeg");
        apply_video_codec_args(
            &mut h264,
            &VideoCodecConfig::H264 {
                profile: crate::preset::H264Profile::High,
                rate_control: VideoRateControl::constrained_quality(18, 8_000, 16_000),
            },
        );
        let h264_args = h264
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(h264_args.windows(2).any(|pair| pair == ["-profile:v", "high"]));
        assert!(h264_args.windows(2).any(|pair| pair == ["-maxrate", "8000k"]));
        assert!(h264_args.windows(2).any(|pair| pair == ["-bufsize", "16000k"]));
        assert!(!h264_args.iter().any(|arg| arg == "-b:v"));

        let mut hevc = Command::new("ffmpeg");
        apply_video_codec_args(
            &mut hevc,
            &VideoCodecConfig::Hevc {
                profile: HevcProfile::Main10,
                rate_control: VideoRateControl::constant_quality(20),
            },
        );
        let hevc_args = hevc
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(hevc_args.windows(2).any(|pair| pair == ["-c:v", "libx265"]));
        assert!(hevc_args.windows(2).any(|pair| pair == ["-profile:v", "main10"]));
        assert!(hevc_args.windows(2).any(|pair| pair == ["-crf", "20"]));
    }

    #[test]
    fn export_video_signal_args_bind_bit_depth_range_and_matrix_conversion() {
        let mut settings = mondrian_timeline::sequence::SequenceSettings::default();
        settings.color.program_output.color_space = ColorSpace::Rec2100Pq;
        let mut delivery = test_delivery_contract(
            DeliveryBitDepth::Ten,
            VideoRange::Legal,
            ExportChromaSampling::Yuv420,
            "yuv420p10le",
        );
        delivery.color_target.color_space = ColorSpace::Rec2100Pq;

        let mut cmd = Command::new("ffmpeg");
        apply_export_video_signal_args(&mut cmd, &settings, &delivery);
        let args = cmd.get_args().map(|arg| arg.to_string_lossy().to_string()).collect::<Vec<_>>();

        assert!(args.windows(2).any(|pair| pair == ["-pix_fmt", "yuv420p10le"]));
        assert!(args.windows(2).any(|pair| pair == ["-color_range", "tv"]));
        assert!(args.windows(2).any(|pair| {
            pair[0] == "-vf"
                && pair[1] == "scale=iw:ih:in_range=full:out_range=limited:out_color_matrix=bt2020"
        }));
    }

    #[test]
    fn color_tag_args_use_export_output_color_space() {
        let mut settings = SequenceSettings::default();
        settings.color.program_output.color_space = ColorSpace::Rec2100Pq;
        let mut delivery = test_delivery_contract(
            DeliveryBitDepth::Ten,
            VideoRange::Legal,
            ExportChromaSampling::Yuv420,
            "yuv420p10le",
        );
        delivery.color_target.color_space = ColorSpace::Rec2100Pq;
        let mut cmd = Command::new("ffmpeg");
        apply_export_video_signal_args(&mut cmd, &settings, &delivery);
        let args = cmd.get_args().map(|arg| arg.to_string_lossy().to_string()).collect::<Vec<_>>();

        assert!(args.windows(2).any(|pair| pair == ["-color_primaries", "bt2020"]));
        assert!(args.windows(2).any(|pair| pair == ["-color_trc", "smpte2084"]));
        assert!(args.windows(2).any(|pair| pair == ["-colorspace", "bt2020nc"]));
    }

    #[test]
    fn encoder_signal_params_write_vui_tags_and_merge_hdr_metadata() {
        let mut settings = SequenceSettings::default();
        settings.color.program_output.color_space = ColorSpace::Rec2100Pq;
        settings.delivery.hdr_mastering_display =
            Some(mondrian_core::VideoMasteringDisplayMetadata::rec2100_1000_nit_reference());
        settings.delivery.hdr_content_light =
            Some(mondrian_core::VideoContentLightMetadata::rec2100_1000_nit_reference());
        settings.delivery.static_hdr_metadata_policy = StaticHdrMetadataPolicy::WriteAuthored;
        let mut delivery = test_delivery_contract(
            DeliveryBitDepth::Ten,
            VideoRange::Legal,
            ExportChromaSampling::Yuv420,
            "yuv420p10le",
        );
        delivery.color_target.color_space = ColorSpace::Rec2100Pq;

        let mut cmd = Command::new("ffmpeg");
        apply_encoder_signal_params(
            &mut cmd,
            &crate::preset::VideoCodecConfig::Hevc {
                profile: crate::preset::HevcProfile::Main10,
                rate_control: VideoRateControl::constant_quality(20),
            },
            &settings,
            &delivery,
        )
        .expect("valid encoder signal params");
        let args = cmd.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "-x265-params");
        assert!(
            args[1].contains("colorprim=bt2020"),
            "VUI primaries must reach the bitstream"
        );
        assert!(args[1].contains("transfer=smpte2084"));
        assert!(args[1].contains("colormatrix=bt2020nc"));
        assert!(args[1].contains("master-display="));
        assert!(args[1].contains(":max-cll="));

        let mut cmd = Command::new("ffmpeg");
        apply_encoder_signal_params(
            &mut cmd,
            &crate::preset::VideoCodecConfig::H264 {
                profile: crate::preset::H264Profile::High,
                rate_control: VideoRateControl::constant_quality(20),
            },
            &settings,
            &delivery,
        )
        .expect("valid encoder signal params");
        let args = cmd.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                "-x264-params",
                "colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc"
            ]
        );
    }

    #[test]
    fn color_tag_args_skip_camera_log_spaces_without_standard_delivery_tags() {
        let mut delivery = test_delivery_contract(
            DeliveryBitDepth::Ten,
            VideoRange::Full,
            ExportChromaSampling::Yuv422,
            "yuv422p10le",
        );
        for color_space in [
            ColorSpace::AppleLogBt2020,
            ColorSpace::SonySLog3SGamut3,
            ColorSpace::SonySLog3SGamut3Cine,
            ColorSpace::ArriLogC3WideGamut3,
            ColorSpace::ArriLogC4WideGamut4,
            ColorSpace::CanonLog2CinemaGamutD55,
            ColorSpace::CanonLog3CinemaGamutD55,
            ColorSpace::PanasonicVLogVGamut,
            ColorSpace::RedLog3G10WideGamutRgb,
            ColorSpace::BlackmagicFilmWideGamutGen5,
            ColorSpace::DjiDLogDGamut,
            ColorSpace::DavinciIntermediateWideGamut,
        ] {
            delivery.color_target.color_space = color_space;
            let mut settings = SequenceSettings::default();
            settings.color.program_output.color_space = color_space;
            let mut cmd = Command::new("ffmpeg");
            apply_export_video_signal_args(&mut cmd, &settings, &delivery);

            let args =
                cmd.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>();
            assert!(!args.iter().any(|arg| {
                matches!(
                    arg.as_str(),
                    "-color_primaries" | "-color_trc" | "-colorspace"
                )
            }));
            assert!(args.iter().any(|arg| arg == "-vf"));
        }
    }

    #[test]
    fn srgb_yuv_delivery_uses_bt709_matrix_without_relabeling_transfer() {
        let mut settings = SequenceSettings::default();
        settings.color.program_output.color_space = ColorSpace::Srgb;
        let mut delivery = test_delivery_contract(
            DeliveryBitDepth::Eight,
            VideoRange::Legal,
            ExportChromaSampling::Yuv420,
            "yuv420p",
        );
        delivery.color_target.color_space = ColorSpace::Srgb;
        let mut cmd = Command::new("ffmpeg");

        apply_export_video_signal_args(&mut cmd, &settings, &delivery);

        let args = cmd.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert!(args.windows(2).any(|pair| pair == ["-color_trc", "iec61966-2-1"]));
        assert!(args.windows(2).any(|pair| pair == ["-colorspace", "bt709"]));
        assert!(args
            .windows(2)
            .any(|pair| { pair[0] == "-vf" && pair[1].contains("out_color_matrix=bt709") }));
    }

    #[test]
    fn post_encode_expectations_come_from_the_same_signal_contract() {
        let mut settings = SequenceSettings::default();
        settings.color.program_output.color_space = ColorSpace::Rec2100Pq;
        let mut delivery = test_delivery_contract(
            DeliveryBitDepth::Ten,
            VideoRange::Legal,
            ExportChromaSampling::Yuv420,
            "yuv420p10le",
        );
        delivery.color_target.color_space = ColorSpace::Rec2100Pq;

        let expected =
            expected_export_video_signal(&settings, &delivery).expect("valid PQ signal contract");

        assert_eq!(expected.pixel_format.as_deref(), Some("yuv420p10le"));
        assert_eq!(expected.color_range.as_deref(), Some("tv"));
        assert_eq!(expected.color_primaries.as_deref(), Some("bt2020"));
        assert_eq!(expected.color_transfer.as_deref(), Some("smpte2084"));
        assert_eq!(expected.color_matrix.as_deref(), Some("bt2020nc"));
        assert!(!expected.require_color_tags_absent);
        assert_eq!(
            expected.static_hdr_metadata,
            crate::validator::ExpectedStaticHdrMetadata::Absent
        );

        settings.delivery.static_hdr_metadata_policy = StaticHdrMetadataPolicy::WriteAuthored;
        settings.delivery.hdr_mastering_display =
            Some(VideoMasteringDisplayMetadata::rec2100_1000_nit_reference());
        settings.delivery.hdr_content_light =
            Some(VideoContentLightMetadata::rec2100_1000_nit_reference());
        let expected = expected_export_video_signal(&settings, &delivery)
            .expect("valid static HDR metadata contract");
        let crate::validator::ExpectedStaticHdrMetadata::Exact(expected_static_hdr) =
            expected.static_hdr_metadata
        else {
            panic!("post-encode contract must retain authored static HDR metadata");
        };
        assert_eq!(
            expected_static_hdr.content_light,
            VideoContentLightMetadata::rec2100_1000_nit_reference()
        );
        settings.delivery.static_hdr_metadata_policy = StaticHdrMetadataPolicy::Omit;

        settings.color.program_output.color_space = ColorSpace::AppleLogBt2020;
        let mut prores = test_delivery_contract(
            DeliveryBitDepth::Twelve,
            VideoRange::Full,
            ExportChromaSampling::Yuv444,
            "yuv444p12le",
        );
        prores.color_target.color_space = ColorSpace::AppleLogBt2020;
        let expected =
            expected_export_video_signal(&settings, &prores).expect("valid ProRes signal contract");
        assert_eq!(expected.pixel_format.as_deref(), Some("yuv444p12le"));
        let prores_alpha =
            ResolvedExportDeliveryContract { pixel_format: "yuva444p12le", ..prores };
        let alpha_expected = expected_export_video_signal(&settings, &prores_alpha)
            .expect("valid ProRes alpha signal contract");
        assert_eq!(alpha_expected.pixel_format.as_deref(), Some("yuva444p12le"));
        assert!(expected.require_color_tags_absent);
    }

    #[test]
    fn rec601_delivery_preserves_pal_and_ntsc_signal_tags() {
        let mut delivery = test_delivery_contract(
            DeliveryBitDepth::Eight,
            VideoRange::Legal,
            ExportChromaSampling::Yuv420,
            "yuv420p",
        );
        for (color_space, primaries, transfer, matrix) in [
            (ColorSpace::Rec601Pal, "bt470bg", "bt470bg", "bt470bg"),
            (
                ColorSpace::Rec601Ntsc,
                "smpte170m",
                "smpte170m",
                "smpte170m",
            ),
        ] {
            delivery.color_target.color_space = color_space;
            let mut settings = SequenceSettings::default();
            settings.color.program_output.color_space = color_space;
            let expected = expected_export_video_signal(&settings, &delivery)
                .expect("valid Rec.601 signal contract");
            assert_eq!(expected.color_primaries.as_deref(), Some(primaries));
            assert_eq!(expected.color_transfer.as_deref(), Some(transfer));
            assert_eq!(expected.color_matrix.as_deref(), Some(matrix));

            let mut cmd = Command::new("ffmpeg");
            apply_export_video_signal_args(&mut cmd, &settings, &delivery);
            let args =
                cmd.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>();
            assert!(args.windows(2).any(|pair| {
                pair[0] == "-vf" && pair[1].contains(&format!("out_color_matrix={matrix}"))
            }));
        }
    }

    #[test]
    fn render_timeline_frame_into_clears_canvas_when_no_layers() {
        let mut seq = Sequence::new("empty");
        seq.settings.delivery.bit_depth = DeliveryBitDepth::Eight;
        let tb = seq.time_base();
        seq.in_point = Some(tt(0, tb));
        seq.out_point = Some(tt(10, tb));
        let timeline = TimelineExportSnapshot {
            sequence: seq,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        };

        let mut canvas = vec![77u8; 4 * 2 * 4];
        render_timeline_frame_into(
            &timeline,
            0,
            4,
            2,
            ExportAlphaMode::FlattenBlack,
            &mut canvas,
            None,
            None,
            None,
            None,
        )
        .expect("render should pass");

        for px in canvas.chunks_exact(4) {
            assert_eq!(px, &[0, 0, 0, 255]);
        }

        let mut alpha_canvas = vec![77u8; 4 * 2 * 4];
        render_timeline_frame_into(
            &timeline,
            0,
            4,
            2,
            ExportAlphaMode::Preserve,
            &mut alpha_canvas,
            None,
            None,
            None,
            None,
        )
        .expect("alpha-preserving empty render should pass");
        assert!(alpha_canvas.chunks_exact(4).all(|pixel| pixel == [0, 0, 0, 0]));
    }

    #[test]
    fn render_timeline_frame_into_preserves_or_flattens_alpha_explicitly() {
        let mut seq = Sequence::new("alpha-delivery");
        seq.settings.delivery.bit_depth = DeliveryBitDepth::Eight;
        let tb = seq.time_base();
        seq.video_tracks[0]
            .add_clip(
                Clip::new_solid_color(
                    AssetId::new(),
                    mondrian_core::Color::from_rgba8(255, 0, 0, 128),
                    tt(0, tb),
                    tt(1, tb),
                )
                .expect("alpha solid"),
            )
            .expect("add alpha solid");
        seq.in_point = Some(tt(0, tb));
        seq.out_point = Some(tt(1, tb));
        let timeline = TimelineExportSnapshot {
            sequence: seq,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        };

        let mut preserved = Vec::new();
        render_timeline_frame_into(
            &timeline,
            0,
            2,
            2,
            ExportAlphaMode::Preserve,
            &mut preserved,
            None,
            None,
            None,
            None,
        )
        .expect("preserve alpha render");
        assert!(preserved.chunks_exact(4).all(|pixel| pixel[3] == 128));

        let mut flattened = Vec::new();
        render_timeline_frame_into(
            &timeline,
            0,
            2,
            2,
            ExportAlphaMode::FlattenBlack,
            &mut flattened,
            None,
            None,
            None,
            None,
        )
        .expect("flatten alpha render");
        assert!(flattened.chunks_exact(4).all(|pixel| pixel[3] == 255));
    }

    #[test]
    fn export_projects_authoring_transform_to_delivery_extents() {
        let projected = project_export_affine(
            [2.0, 0.0, 0.0, 0.0, 2.0, 0.0],
            Resolution { width: 1920, height: 1080 },
            Resolution { width: 1920, height: 1080 },
            Resolution { width: 3840, height: 2160 },
            Resolution { width: 1920, height: 1080 },
            "test media",
        )
        .expect("valid export projection");

        assert_eq!(projected, [1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    }

    #[test]
    fn render_timeline_frame_into_uses_rgba64le_canvas_for_ten_bit_no_layers() {
        let mut seq = Sequence::new("empty-ten-bit");
        seq.settings.delivery.bit_depth = DeliveryBitDepth::Ten;
        let tb = seq.time_base();
        seq.in_point = Some(tt(0, tb));
        seq.out_point = Some(tt(10, tb));
        let timeline = TimelineExportSnapshot {
            sequence: seq,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        };

        let mut canvas = vec![77u8; 4 * 2 * 4];
        render_timeline_frame_into(
            &timeline,
            0,
            4,
            2,
            ExportAlphaMode::FlattenBlack,
            &mut canvas,
            None,
            None,
            None,
            None,
        )
        .expect("render should pass");

        assert_eq!(canvas.len(), 4 * 2 * 8);
        for px in canvas.chunks_exact(8) {
            assert_eq!(px, &[0, 0, 0, 0, 0, 0, 255, 255]);
        }
    }

    #[test]
    fn export_frame_contract_packs_rgba8_and_float_to_pipe_format() {
        let rgba8 = [0, 128, 255, 64];
        assert_eq!(ExportFrameContract::Rgba8.pack_rgba8(&rgba8), rgba8);
        assert_eq!(
            ExportFrameContract::Rgba16Float.pack_rgba8(&rgba8),
            vec![0, 0, 128, 128, 255, 255, 64, 64]
        );

        assert_eq!(
            ExportFrameContract::Rgba16Float.pack_rgba_f32(&[0.0, 0.5, 1.0, 1.5]),
            vec![0, 0, 0, 128, 255, 255, 255, 255]
        );
        assert_eq!(
            ExportFrameContract::Rgba16Float.to_rgba8_boundary(&[0, 0, 128, 128, 255, 255, 64, 64]),
            rgba8
        );
    }

    #[test]
    fn export_color_stage_diagnostics_for_frame_tracks_output_boundary() {
        let mut seq = Sequence::new("export-stage-diagnostics");
        let tb = seq.time_base();
        seq.video_tracks[0]
            .add_clip(
                Clip::new_solid_color(
                    AssetId::new(),
                    mondrian_core::Color::from_rgba8(32, 96, 160, 255),
                    tt(0, tb),
                    tt(10, tb),
                )
                .expect("valid clip"),
            )
            .expect("add solid clip");
        let timeline = TimelineExportSnapshot {
            sequence: seq,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        };

        let _gpu_guard = GpuBoundaryFailureGuard::activate();
        let diagnostics = export_color_stage_diagnostics_for_frame(&timeline, 0, 2, 2)
            .expect("export stage diagnostics");

        assert_eq!(diagnostics.total_stages, 1);
        assert_eq!(diagnostics.cpu_input_stages, 0);
        assert_eq!(diagnostics.cpu_output_stages, 1);
        assert_eq!(diagnostics.gpu_color_stages, 0);
        assert_eq!(diagnostics.upload_stages, 0);
        assert_eq!(diagnostics.readback_stages, 0);
        assert_eq!(diagnostics.gpu_blockers, 0);
        assert_eq!(diagnostics.stage_pixels, 4);
    }

    #[test]
    fn render_timeline_frame_respects_reject_missing_media_color_metadata() {
        let mut seq = Sequence::new("missing-media-color");
        seq.settings.color.input.missing_metadata_policy = MissingColorMetadataPolicy::RejectMedia;
        let tb = seq.time_base();
        let asset_id = AssetId::new();
        seq.video_tracks[0]
            .add_clip(Clip::new(asset_id, tt(0, tb), tt(1, tb)).expect("valid clip"))
            .expect("add clip");

        let temp_path = std::env::temp_dir().join(format!(
            "mondrian-missing-color-{}.mov",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        std::fs::write(&temp_path, []).expect("create placeholder media path");

        let color_diagnostic = mondrian_media::VideoColorDiagnostic {
            color_range: mondrian_media::DecodedVideoRange::Unknown,
            sampling: None,
            interpretation: mondrian_media::DetectedColorInterpretation {
                candidate_color_space: None,
                confidence: mondrian_media::VideoColorInterpretationConfidence::None,
                source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
                method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
                evidence: Vec::new(),
                warnings: vec![mondrian_media::VideoColorInterpretationWarning::MissingCicpTags],
                user_overridable: true,
            },
            metadata: Some(mondrian_media::VideoColorMetadata {
                primaries: mondrian_media::VideoColorTag { code: 2, name: None, specified: false },
                transfer: mondrian_media::VideoColorTag { code: 2, name: None, specified: false },
                matrix: mondrian_media::VideoColorTag { code: 2, name: None, specified: false },
            }),
            metadata_hints: Vec::new(),
            hdr_metadata: Vec::new(),
        };
        let mut media = HashMap::new();
        media.insert(
            asset_id,
            test_media_dependency(
                temp_path.clone(),
                None,
                AssetMediaInterpretation::default(),
                Some(color_diagnostic),
            ),
        );
        let timeline = TimelineExportSnapshot {
            sequence: seq,
            sequences: Vec::new(),
            media,
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        };

        let mut canvas = Vec::new();
        let mut counts = InputColorResolutionSourceCounts::default();
        let err = render_timeline_frame_into(
            &timeline,
            0,
            1,
            1,
            ExportAlphaMode::FlattenBlack,
            &mut canvas,
            Some(&mut counts),
            None,
            None,
            None,
        )
        .expect_err("missing color metadata should be rejected before decode");

        assert!(err.contains("missing color metadata"));
        assert!(err.contains(temp_path.to_string_lossy().as_ref()));
        assert!(err.contains("source=MissingMetadata"));
        assert!(err.contains("primaries=unspecified"));
        assert_eq!(
            counts.count(InputColorResolutionSource::MissingPolicyRejectMedia),
            1
        );
        let _ = std::fs::remove_file(temp_path);
    }

    #[test]
    fn filename_log_suggestion_cannot_change_export_pixels_but_override_does() {
        let root = std::env::temp_dir().join(format!(
            "mondrian-export-filename-color-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create filename-color root");
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/small/h264-bframes.mp4");
        let plain_path = root.join("camera-original.mp4");
        let suggested_path = root.join("camera-S-Log3_S-Gamut3.Cine.mp4");
        std::fs::copy(&fixture, &plain_path).expect("copy plain synthetic source");
        std::fs::copy(&fixture, &suggested_path).expect("copy renamed synthetic source");
        assert_eq!(
            std::fs::read(&plain_path).expect("read plain source"),
            std::fs::read(&suggested_path).expect("read renamed source"),
            "the regression must vary only the file name"
        );

        let missing_metadata = mondrian_media::VideoColorMetadata {
            primaries: mondrian_media::VideoColorTag { code: 2, name: None, specified: false },
            transfer: mondrian_media::VideoColorTag { code: 2, name: None, specified: false },
            matrix: mondrian_media::VideoColorTag { code: 2, name: None, specified: false },
        };
        let suggestion = mondrian_media::parse_video_color_metadata_hint(
            mondrian_media::VideoColorMetadataHintScope::FileName,
            "filename",
            suggested_path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("UTF-8 test name"),
        )
        .expect("complete camera pair remains visible as a suggestion");
        let plain_interpretation =
            mondrian_media::interpret_video_color_metadata(&missing_metadata, None, &[]);
        let suggested_interpretation = mondrian_media::interpret_video_color_metadata(
            &missing_metadata,
            None,
            std::slice::from_ref(&suggestion),
        );
        assert_eq!(
            suggested_interpretation.candidate_color_space,
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );
        assert_eq!(
            suggested_interpretation.executable_color_space_from_probe(
                Some(mondrian_media::ProvenVideoSampling {
                    pixel_format: mondrian_core::PixelFormat::Yuv420p,
                    bit_depth: 8,
                    has_alpha: false,
                }),
                Some(&missing_metadata),
                std::slice::from_ref(&suggestion),
            ),
            None
        );

        let diagnostic =
            |interpretation: mondrian_media::DetectedColorInterpretation,
             hints: Vec<mondrian_media::VideoColorMetadataHint>| {
                mondrian_media::VideoColorDiagnostic {
                    color_range: mondrian_media::DecodedVideoRange::Limited,
                    sampling: None,
                    interpretation,
                    metadata: Some(missing_metadata.clone()),
                    metadata_hints: hints,
                    hdr_metadata: Vec::new(),
                }
            };
        let snapshot = |path: PathBuf,
                        color_diagnostic: mondrian_media::VideoColorDiagnostic,
                        override_color_space: Option<ColorSpace>| {
            let mut sequence = Sequence::new("filename color authority");
            let time_base = sequence.time_base();
            let asset_id = AssetId::new();
            let mut clip =
                Clip::new(asset_id, tt(0, time_base), tt(10, time_base)).expect("valid media clip");
            clip.media_interpretation_mut()
                .expect("media interpretation")
                .color_space_override = override_color_space;
            sequence.video_tracks[0].add_clip(clip).expect("add media clip");
            sequence.in_point = Some(tt(0, time_base));
            sequence.out_point = Some(tt(10, time_base));
            let mut dependency = test_media_dependency(
                path,
                None,
                AssetMediaInterpretation::default(),
                Some(color_diagnostic),
            );
            dependency.source_resolution = Some(Resolution { width: 64, height: 64 });
            TimelineExportSnapshot {
                sequence,
                sequences: Vec::new(),
                media: HashMap::from([(asset_id, dependency)]),
                color_environment: mondrian_core::ProjectColorEnvironment::default(),
                prepared_execution: None,
                range: TimelineExportRange::SequenceInOut,
            }
        };

        let plain = snapshot(
            plain_path,
            diagnostic(plain_interpretation, Vec::new()),
            None,
        );
        let suggested = snapshot(
            suggested_path.clone(),
            diagnostic(suggested_interpretation.clone(), vec![suggestion.clone()]),
            None,
        );
        let overridden = snapshot(
            suggested_path,
            diagnostic(suggested_interpretation, vec![suggestion]),
            Some(ColorSpace::SonySLog3SGamut3Cine),
        );
        let render = |timeline: &TimelineExportSnapshot| {
            let mut canvas = Vec::new();
            let mut counts = InputColorResolutionSourceCounts::default();
            render_timeline_frame_into(
                timeline,
                0,
                16,
                16,
                ExportAlphaMode::FlattenBlack,
                &mut canvas,
                Some(&mut counts),
                None,
                None,
                None,
            )
            .expect("render synthetic source");
            (canvas, counts)
        };

        let (plain_pixels, plain_counts) = render(&plain);
        let (suggested_pixels, suggested_counts) = render(&suggested);
        let (override_pixels, override_counts) = render(&overridden);
        assert_eq!(plain_pixels, suggested_pixels);
        assert_eq!(plain_counts, suggested_counts);
        assert_eq!(
            plain_counts.count(InputColorResolutionSource::MissingPolicyAssumeRec709),
            1
        );
        assert_ne!(override_pixels, plain_pixels);
        assert_eq!(
            override_counts.count(InputColorResolutionSource::Override),
            1
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shared_compositor_applies_media_effects_for_export() {
        let mut scratch = mondrian_renderer::TimelineCompositeScratch::default();
        let media = test_working_frame(&[120, 80, 40, 255], 1, 1);
        let output = mondrian_renderer::composite_timeline_elements(
            1,
            1,
            &[mondrian_renderer::TimelineCompositeElement::Media(
                mondrian_renderer::TimelineMediaLayer {
                    frame: &media,
                    opacity: 1.0,
                    blend_mode: BlendMode::Normal,
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_graph: compile_reference_effect_graph(&EffectRenderPlan {
                        ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                            exposure: 0.0,
                            contrast: 1.0,
                            saturation: 0.0,
                            working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                        }],
                    })
                    .expect("compile media effect graph"),
                    frame_seed: 0,
                },
            )],
            mondrian_renderer::TimelineCompositeOptions::default(),
            &mut scratch,
        )
        .expect("composite export media effect");

        assert_eq!(output[0], output[1]);
        assert_eq!(output[1], output[2]);
        assert_eq!(output[3], 255);
    }

    #[test]
    fn shared_compositor_respects_adjustment_order_for_export() {
        let mut scratch = mondrian_renderer::TimelineCompositeScratch::default();
        let lower = test_working_frame(&[255, 0, 0, 255, 255, 0, 0, 255], 2, 1);
        let upper = test_working_frame(&[0, 0, 0, 0, 0, 255, 0, 255], 2, 1);
        let output = mondrian_renderer::composite_timeline_elements(
            2,
            1,
            &[
                mondrian_renderer::TimelineCompositeElement::Media(
                    mondrian_renderer::TimelineMediaLayer {
                        frame: &lower,
                        opacity: 1.0,
                        blend_mode: BlendMode::Normal,
                        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                        effect_graph: compile_reference_effect_graph(&EffectRenderPlan::default())
                            .expect("compile identity graph"),
                        frame_seed: 0,
                    },
                ),
                mondrian_renderer::TimelineCompositeElement::Adjustment(
                    mondrian_renderer::TimelineAdjustmentLayer {
                        effect_graph: compile_reference_effect_graph(&EffectRenderPlan {
                            ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                                exposure: 0.0,
                                contrast: 1.0,
                                saturation: 0.0,
                                working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                            }],
                        })
                        .expect("compile adjustment graph"),
                        opacity: 1.0,
                        blend_mode: Some(BlendMode::Normal),
                        frame_seed: 0,
                    },
                ),
                mondrian_renderer::TimelineCompositeElement::Media(
                    mondrian_renderer::TimelineMediaLayer {
                        frame: &upper,
                        opacity: 1.0,
                        blend_mode: BlendMode::Normal,
                        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                        effect_graph: compile_reference_effect_graph(&EffectRenderPlan::default())
                            .expect("compile identity graph"),
                        frame_seed: 0,
                    },
                ),
            ],
            mondrian_renderer::TimelineCompositeOptions::default(),
            &mut scratch,
        )
        .expect("composite export adjustment stack");

        assert_eq!(&output[0..4], &[54, 54, 54, 255]);
        assert_eq!(&output[4..8], &[0, 255, 0, 255]);
    }

    #[test]
    fn delivery_depth_selects_internal_pipe_precision_without_float_delivery_mode() {
        assert_eq!(
            ExportFrameContract::from_bit_depth(DeliveryBitDepth::Eight),
            ExportFrameContract::Rgba8
        );
        assert_eq!(
            ExportFrameContract::from_bit_depth(DeliveryBitDepth::Ten),
            ExportFrameContract::Rgba16Float
        );
        assert_eq!(
            ExportFrameContract::from_bit_depth(DeliveryBitDepth::Twelve),
            ExportFrameContract::Rgba16Float
        );
    }

    #[test]
    fn export_frame_contract_rgba64le_packing_preserves_precision() {
        let f32_input = [0.0f32, 0.5, 1.0, 0.75];
        let packed = ExportFrameContract::Rgba16Float.pack_rgba_f32(&f32_input);
        assert_eq!(packed.len(), 8);

        let r = u16::from_le_bytes([packed[0], packed[1]]);
        let g = u16::from_le_bytes([packed[2], packed[3]]);
        let b = u16::from_le_bytes([packed[4], packed[5]]);
        let a = u16::from_le_bytes([packed[6], packed[7]]);

        assert_eq!(r, 0);
        assert_eq!(g, 32768);
        assert_eq!(b, 65535);
        assert_eq!(a, 49151);
        assert_eq!(packed.len(), 8);
    }

    #[test]
    fn export_frame_contract_rgba8_canvas_len_is_correct() {
        assert_eq!(ExportFrameContract::Rgba8.canvas_len(4, 3), 48);
        assert_eq!(ExportFrameContract::Rgba16Float.canvas_len(4, 3), 96);
    }

    #[test]
    fn ten_bit_cpu_float_fallback_does_not_record_precision_failure() {
        let mut seq = Sequence::new("high-bit-float-fallback");
        seq.settings.delivery.bit_depth = DeliveryBitDepth::Ten;
        let tb = seq.time_base();
        seq.video_tracks[0]
            .add_clip(
                Clip::new_solid_color(
                    AssetId::new(),
                    mondrian_core::Color::from_rgba8(128, 128, 128, 255),
                    tt(0, tb),
                    tt(1, tb),
                )
                .expect("valid clip"),
            )
            .expect("add solid clip");
        let timeline = TimelineExportSnapshot {
            sequence: seq,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        };

        let mut export_diagnostics = ExportJobColorDiagnostics::default();
        let mut canvas = vec![0u8; 2 * 2 * 8];
        render_timeline_frame_into(
            &timeline,
            0,
            2,
            2,
            ExportAlphaMode::FlattenBlack,
            &mut canvas,
            None,
            None,
            None,
            Some(&mut export_diagnostics),
        )
        .expect("render high-bit should pass");

        assert_eq!(canvas.len(), 2 * 2 * 8);
        assert_eq!(
            export_diagnostics.output_precision_failures, 0,
            "high-bit float path should not record precision failure"
        );
        assert_eq!(
            export_diagnostics.output_precision_failure_reasons.total(),
            0,
            "high-bit float path should have zero precision failure reasons"
        );
    }

    #[test]
    fn ten_bit_cpu_fallback_produces_correct_rgba64le_canvas() {
        let mut seq = Sequence::new("high-bit-canvas-check");
        seq.settings.delivery.bit_depth = DeliveryBitDepth::Ten;
        let tb = seq.time_base();
        seq.video_tracks[0]
            .add_clip(
                Clip::new_solid_color(
                    AssetId::new(),
                    mondrian_core::Color::from_rgba8(200, 100, 50, 255),
                    tt(0, tb),
                    tt(1, tb),
                )
                .expect("valid clip"),
            )
            .expect("add solid clip");
        let timeline = TimelineExportSnapshot {
            sequence: seq,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        };

        let mut canvas = vec![0u8; 2 * 2 * 8];
        render_timeline_frame_into(
            &timeline,
            0,
            2,
            2,
            ExportAlphaMode::FlattenBlack,
            &mut canvas,
            None,
            None,
            None,
            None,
        )
        .expect("render high-bit canvas");

        assert_eq!(canvas.len(), 2 * 2 * 8);
        for px in canvas.chunks_exact(8) {
            let r = u16::from_le_bytes([px[0], px[1]]);
            let g = u16::from_le_bytes([px[2], px[3]]);
            let b = u16::from_le_bytes([px[4], px[5]]);
            let a = u16::from_le_bytes([px[6], px[7]]);
            assert!(r > 0, "R channel should be nonzero in rgba64le");
            assert!(g > 0, "G channel should be nonzero in rgba64le");
            assert!(b > 0, "B channel should be nonzero in rgba64le");
            assert_eq!(a, u16::MAX, "alpha should be 1.0 in rgba64le");
        }
    }
}

#[cfg(test)]
#[path = "../queue_perf_tests.rs"]
mod perf_tests;
