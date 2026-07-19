//! 后台渲染队列

use crate::preset::{
    AudioCodecConfig, Container, ExportAlphaMode, ExportConfig, TimelineExportRange,
    TimelineExportSnapshot, VideoCodecConfig,
};
use crate::validator::{
    validate_export_output, ExpectedVideoConstraints, ExportValidationExpectations,
};
use mondrian_audio::{
    compile_audio_program, AudioCompileRequest, AudioContinuityEpoch, AudioDecodedSource,
    AudioMediaResolver, AudioProcessingMode, AudioProgramRuntime, AudioRenderContract,
    AudioRenderRequest, AudioStateEntry,
};
use mondrian_core::timeline_data::AlphaInterpretation;
use mondrian_core::types::{AssetId, ColorEngine, ColorSpace, FramePosition, JobId, Rational};
use mondrian_core::{
    AudioChannelLayout, AudioSamplePosition, AudioSampleRate, AudioSampleRounding,
    AudioSourceComponentId, ExecutionCancellationToken, FrameRounding, TimelineTime,
    WorkingColorSpace, WorkingRgbaF32Frame,
};
use mondrian_media::AudioSourceCache;
use mondrian_media::{
    decode_preview_frame_cancellable, DecodedVideoRange, DecodedVideoRangeContract,
    MediaFileFingerprint, PreviewDecodeAccessMode, PreviewDecodeOutcome, PreviewDecodeRequest,
    PreviewSourceColorContract, VideoColorDiagnosticIssueAggregate,
};
use mondrian_renderer::{
    color_report_vocab, composite_timeline_elements_color_frame_with_diagnostics,
    evaluate_timeline_render_plan, execute_cpu_output_boundary_float,
    execute_cpu_output_boundary_rgba8, execute_cpu_source_input_stage,
    execute_cpu_working_transform, ColorFrameResidency, CpuColorFrame, CpuEncodedColorFrame,
    CpuSourceColorFrame, GpuColorFrameReadbackPlan, GpuColorFrameTextureFormat, GpuContext,
    LinearFloatSource, RenderColorStageDiagnostics, RenderColorStageGpuBlockerBreakdown,
    RenderColorTransformGpuOptions, RenderGpuOutputBoundaryRuntime,
    RenderGpuOutputBoundaryRuntimeOwnedBackendContext, RenderInputTransform,
    RenderOutputColorBoundary, TimelineAdjustmentLayer, TimelineCompositeColorPathSummary,
    TimelineCompositeDiagnostics, TimelineCompositeDomainBlockerBreakdown,
    TimelineCompositeElement, TimelineCompositeLegacyBreakdown, TimelineCompositeOptions,
    TimelineCompositeScratch, TimelineEffectColorRuntime, TimelineEvaluationRequest,
    TimelineMediaLayer, TimelineRenderPlanElement, TimelineSolidColorLayer,
};
use mondrian_timeline::sequence::{
    ColorContext, DeliveryBitDepth, InputColorResolutionSourceCounts, ResolvedInputColor,
    SequenceSettings, VideoRange, MAX_NESTED_SEQUENCE_RENDER_DEPTH,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex as StdMutex, OnceLock};
use tokio::runtime::Builder as TokioRuntimeBuilder;

mod service;
pub use service::*;

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

/// Resolve the export frame contract from sequence settings.
fn export_frame_contract(settings: &SequenceSettings) -> ExportFrameContract {
    ExportFrameContract::from_bit_depth(settings.color_management.delivery_bit_depth)
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
    execute_cpu_output_boundary_float(frame, boundary)
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
    runtime: StdMutex<RenderGpuOutputBoundaryRuntime>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
struct ExportGpuOutputAttemptOutcome {
    rgba: Vec<u8>,
    stage_diagnostics: RenderColorStageDiagnostics,
}

static EXPORT_GPU_OUTPUT_RUNTIME: OnceLock<Result<Arc<ExportGpuOutputBackend>, String>> =
    OnceLock::new();

fn build_export_gpu_output_runtime() -> Result<Arc<ExportGpuOutputBackend>, String> {
    let runtime = TokioRuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("build gpu runtime failed: {err}"))?;
    let context = runtime
        .block_on(GpuContext::new())
        .map_err(|err| format!("create gpu context failed: {err}"))?;
    Ok(Arc::new(ExportGpuOutputBackend {
        context,
        runtime: StdMutex::new(RenderGpuOutputBoundaryRuntime::default()),
    }))
}

fn export_gpu_output_runtime() -> Result<&'static Arc<ExportGpuOutputBackend>, String> {
    if let Some(result) = EXPORT_GPU_OUTPUT_RUNTIME.get() {
        return result.as_ref().map_err(|err| err.clone());
    }

    let result = match EXPORT_GPU_OUTPUT_RUNTIME.set(build_export_gpu_output_runtime()) {
        Ok(()) => EXPORT_GPU_OUTPUT_RUNTIME.get().expect("runtime init must be set"),
        Err(_already_set) => EXPORT_GPU_OUTPUT_RUNTIME
            .get()
            .expect("runtime state must be available after concurrent init"),
    };
    result.as_ref().map_err(|err| err.clone())
}

fn map_readback_buffer_sync(
    device: &wgpu::Device,
    readback: &wgpu::Buffer,
) -> Result<Vec<u8>, String> {
    let slice = readback.slice(..);
    let (tx, rx) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    let _ = device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });

    let _ = rx
        .recv()
        .map_err(|err| format!("readback map callback channel closed: {err}"))?;
    let mapped = slice
        .get_mapped_range()
        .map_err(|err| format!("readback mapped range unavailable: {err:?}"))?;
    Ok(mapped.to_vec())
}

fn execute_export_gpu_output_boundary(
    frame: &CpuColorFrame,
    boundary: &RenderOutputColorBoundary,
    frame_contract: ExportFrameContract,
) -> Result<ExportGpuOutputAttemptOutcome, ExportGpuOutputFallbackReason> {
    #[cfg(test)]
    if FORCE_GPU_BOUNDARY_FAILURE.with(|cell| cell.get()) {
        return Err(ExportGpuOutputFallbackReason::ContextUnavailable);
    }
    let backend = export_gpu_output_runtime()
        .map_err(|_| ExportGpuOutputFallbackReason::ContextUnavailable)?;
    let mut runtime = backend
        .runtime
        .lock()
        .map_err(|_| ExportGpuOutputFallbackReason::ContextUnavailable)?;

    let mut encoder =
        backend.context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-export-gpu-output-boundary"),
        });

    let record = runtime
        .record_wgpu_output_boundary_owned_backend(
            boundary,
            frame,
            frame_contract.gpu_texture_format(),
            RenderColorTransformGpuOptions {
                output_residency: ColorFrameResidency::Cpu,
                ..RenderColorTransformGpuOptions::default()
            },
            RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                device: &backend.context.device,
                queue: &backend.context.queue,
                encoder: &mut encoder,
                load_op: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
            },
        )
        .map_err(|_| ExportGpuOutputFallbackReason::RecordBoundaryFailed)?;

    backend.context.queue.submit(std::iter::once(encoder.finish()));
    let readback_buffer = record
        .readback_buffer
        .ok_or(ExportGpuOutputFallbackReason::MissingReadbackBuffer)?;
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
    let mapped = map_readback_buffer_sync(&backend.context.device, &readback_buffer)
        .map_err(|_| ExportGpuOutputFallbackReason::ReadbackMapFailed)?;
    let rgba = match frame_contract.gpu_texture_format() {
        GpuColorFrameTextureFormat::Rgba8Unorm => {
            let actual = readback_plan
                .unpack_mapped_rgba8(&mapped)
                .map_err(|_| ExportGpuOutputFallbackReason::ReadbackUnpackFailed)?;
            frame_contract.pack_rgba8(actual.rgba())
        }
        GpuColorFrameTextureFormat::Rgba16Float | GpuColorFrameTextureFormat::Rgba32Float => {
            let f32_data = readback_plan
                .unpack_mapped_rgba16float(&mapped)
                .map_err(|_| ExportGpuOutputFallbackReason::ReadbackUnpackFailed)?;
            frame_contract.pack_rgba_f32(&f32_data)
        }
    };
    readback_buffer.unmap();
    runtime.clear_frame_resources();

    Ok(ExportGpuOutputAttemptOutcome { rgba, stage_diagnostics: record.stage_diagnostics })
}

/// Diagnostics accumulated for one export job.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExportJobDiagnostics {
    /// Color-management diagnostics observed while rendering this job.
    pub color: ExportJobColorDiagnostics,
}

impl ExportJobDiagnostics {
    /// Build the versioned export color report for this job when color evidence exists.
    pub fn color_report(self, profile: impl Into<String>) -> Option<ExportColorHealthReport> {
        self.color.health_report(profile)
    }
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
    /// GPU output path lacked an explicit readback buffer.
    MissingReadbackBuffer,
    /// GPU output readback map failed.
    ReadbackMapFailed,
    /// GPU output readback unpacking failed.
    ReadbackUnpackFailed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExportGpuOutputFallbackBreakdown {
    /// GPU context was unavailable or initialization failed.
    pub context_unavailable: u64,
    /// GPU recording failed before submission.
    pub record_boundary_failed: u64,
    /// GPU output planned readback buffer missing.
    pub missing_readback_buffer: u64,
    /// Readback map failed.
    pub readback_map_failed: u64,
    /// Unpacking readback bytes failed.
    pub readback_unpack_failed: u64,
}

impl ExportGpuOutputFallbackBreakdown {
    /// Return total fallback count across all recorded reasons.
    pub fn total(&self) -> u64 {
        self.context_unavailable
            .saturating_add(self.record_boundary_failed)
            .saturating_add(self.missing_readback_buffer)
            .saturating_add(self.readback_map_failed)
            .saturating_add(self.readback_unpack_failed)
    }

    /// Merge another breakdown in place.
    pub fn accumulate(self, other: Self) -> Self {
        Self {
            context_unavailable: self.context_unavailable.saturating_add(other.context_unavailable),
            record_boundary_failed: self
                .record_boundary_failed
                .saturating_add(other.record_boundary_failed),
            missing_readback_buffer: self
                .missing_readback_buffer
                .saturating_add(other.missing_readback_buffer),
            readback_map_failed: self.readback_map_failed.saturating_add(other.readback_map_failed),
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
            ExportGpuOutputFallbackReason::MissingReadbackBuffer => {
                self.missing_readback_buffer = self.missing_readback_buffer.saturating_add(1)
            }
            ExportGpuOutputFallbackReason::ReadbackMapFailed => {
                self.readback_map_failed = self.readback_map_failed.saturating_add(1)
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
pub const EXPORT_COLOR_HEALTH_REPORT_SCHEMA_VERSION: u32 = 4;

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
                "gpu_output_attempts={} cpu_fallbacks={} context_unavailable={} record_failed={} missing_readback_buffer={} readback_map_failed={} readback_unpack_failed={}",
                summary.gpu_output_attempts,
                summary.gpu_output_cpu_fallbacks,
                summary.gpu_output_fallback_reasons.context_unavailable,
                summary.gpu_output_fallback_reasons.record_boundary_failed,
                summary.gpu_output_fallback_reasons.missing_readback_buffer,
                summary.gpu_output_fallback_reasons.readback_map_failed,
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

pub(crate) enum JobExecutionResult {
    /// The validated deliverable crossed its irreversible publication point.
    Completed,
    /// Execution ended without publishing a new deliverable.
    Failed(String),
    /// Cancellation was observed before the irreversible publication point.
    Cancelled,
}

pub(crate) trait ExportExecutor: Send + Sync + 'static {
    fn execute(
        &self,
        job: &RenderJob,
        cancel: &ExecutionCancellationToken,
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
        report: &mut dyn FnMut(ExportProgress),
        report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
    ) -> JobExecutionResult {
        if cancel.is_canceled() {
            return JobExecutionResult::Cancelled;
        }
        report(ExportProgress::preparing(0.01));

        let final_output = job.config.output_path.as_path();
        if let Some(parent) = final_output.parent().filter(|parent| !parent.as_os_str().is_empty())
        {
            if let Err(err) = std::fs::create_dir_all(parent) {
                return JobExecutionResult::Failed(format!(
                    "无法创建导出目录 {}: {}",
                    parent.display(),
                    err
                ));
            }
        }

        let partial_output = export_partial_output_path(final_output, job.id());
        let _ = std::fs::remove_file(&partial_output);
        let outcome = execute_timeline_export(
            job,
            &job.config.timeline,
            partial_output.as_path(),
            cancel,
            report,
            report_diagnostics,
        );
        if !matches!(outcome, JobExecutionResult::Completed) {
            let _ = std::fs::remove_file(&partial_output);
            return outcome;
        }
        if cancel.is_canceled() {
            let _ = std::fs::remove_file(&partial_output);
            return JobExecutionResult::Cancelled;
        }
        if let Err(reason) = validate_snapshot_media_revisions(&job.config.timeline) {
            let _ = std::fs::remove_file(&partial_output);
            return JobExecutionResult::Failed(reason);
        }
        report(ExportProgress::publishing(0.995));
        match finalize_export_output(partial_output.as_path(), final_output) {
            Ok(()) => JobExecutionResult::Completed,
            Err(reason) => {
                let _ = std::fs::remove_file(&partial_output);
                JobExecutionResult::Failed(reason)
            }
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

#[derive(Clone)]
struct DecodedVideoLayer {
    frame: CpuColorFrame,
    stage_diagnostics: RenderColorStageDiagnostics,
}

fn execute_timeline_export(
    job: &RenderJob,
    timeline: &TimelineExportSnapshot,
    output_path: &Path,
    cancel: &ExecutionCancellationToken,
    report: &mut dyn FnMut(ExportProgress),
    report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
) -> JobExecutionResult {
    let mut temp_audio_path_to_cleanup: Option<PathBuf> = None;
    let result = (|| {
        if cancel.is_canceled() {
            return JobExecutionResult::Cancelled;
        }

        if let Err(reason) = validate_snapshot_media_revisions(timeline) {
            return JobExecutionResult::Failed(reason);
        }
        if let Err(err) = validate_timeline_export_color_compatibility(&job.config, timeline) {
            return JobExecutionResult::Failed(err);
        }

        let range = match compute_timeline_render_range(timeline) {
            Ok(range) => range,
            Err(error) => return JobExecutionResult::Failed(error),
        };
        if range.total_frames == 0 {
            return JobExecutionResult::Failed("时间线导出范围为空".to_string());
        }

        let audio_input = prepare_timeline_audio_input(job, timeline, range, cancel, report);
        let audio_input = match audio_input {
            Ok(input) => input,
            Err(outcome) => return outcome,
        };
        if let TimelineAudioInput::PcmFile { path, .. } = &audio_input {
            temp_audio_path_to_cleanup = Some(path.clone());
        }

        let (width, height) = timeline_output_resolution(job, timeline);
        let expected_video_signal = match expected_export_video_signal(
            &timeline.sequence.settings,
            &job.config.preset.video,
            job.config.preset.alpha_mode,
        ) {
            Ok(signal) => signal,
            Err(error) => return JobExecutionResult::Failed(error),
        };
        let validation_expectations = ExportValidationExpectations {
            require_video_stream: true,
            require_audio_stream: !matches!(&audio_input, TimelineAudioInput::Disabled),
            expected_video: Some(ExpectedVideoConstraints {
                width: Some(width),
                height: Some(height),
                fps_num: Some(range.fps_num),
                fps_den: Some(range.fps_den),
                signal: Some(expected_video_signal),
            }),
            expected_duration_secs: Some(
                range.total_frames as f64 * range.fps_den as f64 / range.fps_num.max(1) as f64,
            ),
        };
        let mut cmd = Command::new("ffmpeg");
        let pix_fmt = export_frame_contract(&timeline.sequence.settings).ffmpeg_pix_fmt();
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
                let Some(ffmpeg_layout) = ffmpeg_channel_layout(*channel_layout) else {
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
                let Some(channel_layout) = ffmpeg_channel_layout(*channel_layout) else {
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
        apply_export_video_signal_args(
            &mut cmd,
            &timeline.sequence.settings,
            &job.config.preset.video,
            job.config.preset.alpha_mode,
        );
        if timeline
            .sequence
            .settings
            .color_management
            .static_hdr_metadata_policy
            .writes_authored_metadata()
        {
            if let Err(err) = apply_h265_hdr_metadata_args(&mut cmd, &timeline.sequence.settings) {
                return JobExecutionResult::Failed(err);
            }
        }
        if !matches!(&audio_input, TimelineAudioInput::Disabled) {
            apply_audio_codec_args(&mut cmd, &job.config.preset.audio);
        }
        cmd.arg("-f")
            .arg(container_format(&job.config.preset.container))
            .arg(output_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) => {
                return JobExecutionResult::Failed(format!("无法启动 ffmpeg: {}", err));
            }
        };

        let Some(stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return JobExecutionResult::Failed("ffmpeg stdin 管道不可用".to_string());
        };

        match write_timeline_frames(
            stdin,
            timeline,
            range,
            width,
            height,
            job.config.preset.alpha_mode,
            cancel,
            report,
            report_diagnostics,
        ) {
            JobExecutionResult::Completed => {}
            JobExecutionResult::Cancelled => {
                let _ = child.kill();
                let _ = child.wait();
                return JobExecutionResult::Cancelled;
            }
            JobExecutionResult::Failed(reason) => {
                let _ = child.kill();
                let _ = child.wait();
                return JobExecutionResult::Failed(reason);
            }
        }

        if cancel.is_canceled() {
            let _ = child.kill();
            let _ = child.wait();
            return JobExecutionResult::Cancelled;
        }

        report(ExportProgress::encoding(0.98));
        match wait_for_ffmpeg_child(child, cancel) {
            Ok(output) if output.status.success() => {
                report(ExportProgress::validating(0.99));
                match validate_export_output(output_path, &validation_expectations) {
                    Ok(()) => JobExecutionResult::Completed,
                    Err(err) => JobExecutionResult::Failed(format!("导出结果校验失败: {err}")),
                }
            }
            Ok(output) => {
                let reason = output
                    .stderr_tail
                    .lines()
                    .rev()
                    .find(|line| !line.trim().is_empty())
                    .map(|line| line.trim().to_string())
                    .unwrap_or_else(|| format!("ffmpeg 退出码：{}", output.status));
                JobExecutionResult::Failed(format!("时间线编码失败：{reason}"))
            }
            Err(outcome) => outcome,
        }
    })();

    if let Some(path) = temp_audio_path_to_cleanup {
        let _ = std::fs::remove_file(path);
    }
    result
}

fn validate_snapshot_media_revisions(timeline: &TimelineExportSnapshot) -> Result<(), String> {
    for (asset_id, dependency) in &timeline.media {
        let actual = MediaFileFingerprint::capture(dependency.path.as_path());
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

fn export_partial_output_path(final_output: &Path, job_id: JobId) -> PathBuf {
    let mut file_name = final_output
        .file_name()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "mondrian-export".into());
    file_name.push(format!(".mondrian-{job_id}.partial"));
    final_output.with_file_name(file_name)
}

fn finalize_export_output(partial_output: &Path, final_output: &Path) -> Result<(), String> {
    if !partial_output.is_file() {
        return Err(format!(
            "validated export temporary output is unavailable: {}",
            partial_output.display()
        ));
    }

    std::fs::OpenOptions::new()
        .write(true)
        .open(partial_output)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            format!(
                "failed to durably flush validated export {} before publication: {error}",
                partial_output.display()
            )
        })?;
    replace_validated_output(partial_output, final_output).map_err(|error| {
        format!(
            "failed to publish validated export {}: {error}",
            final_output.display()
        )
    })?;

    if let Err(error) = sync_output_directory(final_output) {
        tracing::warn!(
            path = %final_output.display(),
            %error,
            "export was atomically published but its directory durability sync failed"
        );
    }
    Ok(())
}

#[cfg(windows)]
fn replace_validated_output(partial_output: &Path, final_output: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, ReplaceFileW, MOVEFILE_WRITE_THROUGH, REPLACEFILE_WRITE_THROUGH,
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    }

    let partial = wide(partial_output);
    let final_path = wide(final_output);
    let succeeded = if final_output.exists() {
        // SAFETY: All pointers reference live, NUL-terminated UTF-16 buffers for
        // the duration of the call. The reserved pointers are required to be null.
        unsafe {
            ReplaceFileW(
                final_path.as_ptr(),
                partial.as_ptr(),
                std::ptr::null(),
                REPLACEFILE_WRITE_THROUGH,
                std::ptr::null(),
                std::ptr::null(),
            )
        }
    } else {
        // SAFETY: Both pointers reference live, NUL-terminated UTF-16 buffers.
        unsafe {
            MoveFileExW(
                partial.as_ptr(),
                final_path.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        }
    };
    if succeeded == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_validated_output(partial_output: &Path, final_output: &Path) -> std::io::Result<()> {
    std::fs::rename(partial_output, final_output)
}

#[cfg(unix)]
fn sync_output_directory(final_output: &Path) -> std::io::Result<()> {
    let Some(parent) = final_output.parent().filter(|parent| !parent.as_os_str().is_empty()) else {
        return Ok(());
    };
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_output_directory(_final_output: &Path) -> std::io::Result<()> {
    Ok(())
}

fn prepare_timeline_audio_input(
    job: &RenderJob,
    timeline: &TimelineExportSnapshot,
    range: TimelineRenderRange,
    cancel: &ExecutionCancellationToken,
    report: &mut dyn FnMut(ExportProgress),
) -> Result<TimelineAudioInput, JobExecutionResult> {
    if matches!(job.config.preset.container, Container::Gif) {
        return Ok(TimelineAudioInput::Disabled);
    }

    let sample_rate = timeline.sequence.settings.audio_sample_rate.max(8_000);
    let channel_layout = timeline.sequence.settings.audio_channel_layout;
    if !timeline_has_audio_content(timeline, range).map_err(JobExecutionResult::Failed)? {
        return Ok(TimelineAudioInput::Silent { sample_rate, channel_layout });
    }

    let temp_path = std::env::temp_dir().join(format!(
        "mondrian-export-audio-{}-{}.f32",
        job.id(),
        chrono::Utc::now().timestamp_millis()
    ));

    match render_timeline_audio_to_pcm_f32(
        temp_path.as_path(),
        timeline,
        range,
        sample_rate,
        channel_layout,
        cancel,
        report,
    ) {
        JobExecutionResult::Completed => {
            Ok(TimelineAudioInput::PcmFile { path: temp_path, sample_rate, channel_layout })
        }
        JobExecutionResult::Cancelled => Err(JobExecutionResult::Cancelled),
        JobExecutionResult::Failed(reason) => Err(JobExecutionResult::Failed(reason)),
    }
}

fn timeline_has_audio_content(
    timeline: &TimelineExportSnapshot,
    range: TimelineRenderRange,
) -> Result<bool, String> {
    let time_base = timeline.sequence.time_base();
    let start = TimelineTime::from_frame_position(FramePosition::new(range.start_frame, time_base))
        .map_err(|error| error.to_string())?;
    let end = TimelineTime::from_frame_position(FramePosition::new(
        range.start_frame.saturating_add(range.total_frames as i64),
        time_base,
    ))
    .map_err(|error| error.to_string())?;
    let output_id = timeline
        .sequence
        .audio_program
        .outputs
        .first()
        .map(|output| output.id)
        .ok_or_else(|| "Sequence has no audio Program Output".to_owned())?;
    let program =
        compile_audio_program(&timeline.sequence, AudioCompileRequest::program(output_id))
            .map_err(|error| error.to_string())?;
    Ok(program.contributions().iter().any(|contribution| {
        contribution
            .sequence_range
            .end()
            .is_ok_and(|clip_end| clip_end > start && contribution.sequence_range.start < end)
    }))
}

fn render_timeline_audio_to_pcm_f32(
    output_path: &Path,
    timeline: &TimelineExportSnapshot,
    range: TimelineRenderRange,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    cancel: &ExecutionCancellationToken,
    report: &mut dyn FnMut(ExportProgress),
) -> JobExecutionResult {
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
    let cache = Arc::new(AudioSourceCache::new(sample_rate, channel_layout));
    let resolver = ExportAudioMediaResolver { timeline, cache: Arc::clone(&cache) };
    let contract = AudioRenderContract {
        sample_rate,
        channel_layout,
        max_block_frames: 16_384,
        processing_mode: AudioProcessingMode::Offline,
    };
    let mut runtime = match AudioProgramRuntime::build(
        &timeline.sequence,
        &timeline.sequences,
        &resolver,
        contract,
        None,
    ) {
        Ok(runtime) => runtime,
        Err(error) => {
            return JobExecutionResult::Failed(format!(
                "编译导出音频 Program 失败（未使用降级混音）: {error}"
            ));
        }
    };

    let (start_sample, total_samples) = match timeline_audio_sample_range(range, sample_rate) {
        Ok(sample_range) => sample_range,
        Err(error) => return JobExecutionResult::Failed(error),
    };
    if total_samples == 0 {
        return JobExecutionResult::Completed;
    }
    if runtime.requires_state_entry() {
        if let Err(error) = runtime
            .enter_state(AudioStateEntry { epoch: AudioContinuityEpoch::new(1), start_sample })
        {
            return JobExecutionResult::Failed(format!("进入导出音频连续性状态失败: {error}"));
        }
    }

    let chunk_frames_target = (sample_rate as usize / 5).clamp(1024, 16_384);
    let mut writer = BufWriter::new(file);
    let mut rendered_samples = 0usize;
    let channels = channel_layout.channel_count();
    let mut sample_bytes = Vec::<u8>::with_capacity(chunk_frames_target * channels * 4);
    let mut pcm = vec![0.0_f32; chunk_frames_target * channels];

    while rendered_samples < total_samples {
        if cancel.is_canceled() {
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
        if let Err(error) = runtime.render_into(
            AudioRenderRequest { start_sample: chunk_start, frames: chunk_frames },
            &mut pcm[..chunk_samples],
        ) {
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
    JobExecutionResult::Completed
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
        contract: AudioRenderContract,
    ) -> Result<Arc<dyn AudioDecodedSource>, String> {
        if self.cache.channel_layout() != contract.channel_layout {
            return Err(format!(
                "audio source cache layout {:?} does not match export render layout {:?}",
                self.cache.channel_layout(),
                contract.channel_layout
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
        Ok(Arc::new(ExportDecodedAudioSource(source)))
    }
}

struct ExportDecodedAudioSource(mondrian_media::AudioSourceReader);

impl AudioDecodedSource for ExportDecodedAudioSource {
    fn read_interleaved(
        &self,
        start_frame: i64,
        frames: usize,
        destination: &mut [f32],
        _cancellation: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<(), String> {
        self.0
            .read_interleaved(start_frame, frames, destination)
            .map_err(|error| error.to_string())
    }
}

fn write_timeline_frames(
    stdin: ChildStdin,
    timeline: &TimelineExportSnapshot,
    range: TimelineRenderRange,
    width: u32,
    height: u32,
    alpha_mode: ExportAlphaMode,
    cancel: &ExecutionCancellationToken,
    report: &mut dyn FnMut(ExportProgress),
    report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
) -> JobExecutionResult {
    let mut writer = BufWriter::new(stdin);
    write_timeline_frames_to_writer(
        &mut writer,
        timeline,
        range,
        width,
        height,
        alpha_mode,
        cancel,
        report,
        report_diagnostics,
    )
}

fn write_timeline_frames_to_writer<W: Write>(
    writer: &mut W,
    timeline: &TimelineExportSnapshot,
    range: TimelineRenderRange,
    width: u32,
    height: u32,
    alpha_mode: ExportAlphaMode,
    cancel: &ExecutionCancellationToken,
    report: &mut dyn FnMut(ExportProgress),
    report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
) -> JobExecutionResult {
    let total = range.total_frames.max(1);
    let frame_contract = export_frame_contract(&timeline.sequence.settings);
    let mut canvas = vec![0u8; frame_contract.canvas_len(width, height)];
    let mut diagnostics = ExportJobDiagnostics::default();
    diagnostics
        .color
        .record_asset_issue_summary(export_asset_issue_summary(timeline));

    for index in 0..total {
        if cancel.is_canceled() {
            return JobExecutionResult::Cancelled;
        }

        let timeline_frame = range.start_frame + index as i64;
        let mut frame_color_counts = InputColorResolutionSourceCounts::default();
        let mut frame_stage_diagnostics = RenderColorStageDiagnostics::default();
        let mut frame_composite_diagnostics = TimelineCompositeDiagnostics::default();
        let render_result = render_timeline_frame_into(
            timeline,
            timeline_frame,
            width,
            height,
            alpha_mode,
            &mut canvas,
            Some(&mut frame_color_counts),
            Some(&mut frame_stage_diagnostics),
            Some(&mut frame_composite_diagnostics),
            Some(&mut diagnostics.color),
        );
        diagnostics.color.record_frame_diagnostics(
            frame_color_counts,
            frame_stage_diagnostics,
            frame_composite_diagnostics,
        );
        report_diagnostics(diagnostics);
        match render_result {
            Ok(()) => {}
            Err(err) => {
                return JobExecutionResult::Failed(format!(
                    "渲染时间线帧失败（frame={}）: {}",
                    timeline_frame, err
                ));
            }
        }

        if let Err(err) = writer.write_all(&canvas) {
            return JobExecutionResult::Failed(format!("写入编码管道失败: {}", err));
        }

        let rendered = index + 1;
        let ratio = rendered as f32 / total as f32;
        let progress = (0.18 + 0.72 * ratio).clamp(0.18, 0.92);
        report(ExportProgress::rendering(progress, rendered, total));
    }

    if let Err(err) = writer.flush() {
        return JobExecutionResult::Failed(format!("刷新编码管道失败: {}", err));
    }

    JobExecutionResult::Completed
}

/// Build the export output boundary from the resolved color context.
///
/// The renderer resolves the product-level output-transform intent. Preview
/// and export therefore cannot independently reinterpret Mondrian Standard,
/// an explicit OCIO view, or a colorimetric delivery.
fn export_output_boundary_from_context(
    color_context: &ColorContext,
) -> Result<RenderOutputColorBoundary, String> {
    let output_color_space = color_context.output_color_space.color().ok_or_else(|| {
        "deliverable output boundary requires an encoded output color space".to_owned()
    })?;
    RenderOutputColorBoundary::from_intent(
        mondrian_renderer::RenderOutputColorBoundaryTarget::Export,
        output_color_space,
        &color_context.output_transform,
        color_context.tone_map,
        color_context.engine.clone(),
    )
    .map_err(|error| error.to_string())
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
    let frame_contract = export_frame_contract(&timeline.sequence.settings);
    let required_len = frame_contract.canvas_len(width, height);
    if canvas.len() != required_len {
        canvas.resize(required_len, 0);
    }

    let color_context = timeline
        .sequence
        .settings
        .root_program_color_context(&timeline.project_color_management);

    render_sequence_frame_into(
        timeline,
        &timeline.sequence,
        timeline_frame,
        width,
        height,
        color_context,
        alpha_mode,
        SequenceRenderTarget::Deliverable(canvas),
        0,
        input_color_counts,
        stage_diagnostics,
        composite_diagnostics,
        export_diagnostics,
    )
}

enum SequenceRenderTarget<'a> {
    Working(&'a mut Option<CpuColorFrame>),
    Deliverable(&'a mut Vec<u8>),
}

/// Collect input color-resolution source counts for one export timeline frame.
///
/// This uses the same render-plan evaluation path as timeline export, including
/// nested sequence recursion and sequence color-context inheritance. It is the
/// export-side diagnostic counterpart to preview's per-frame source counters.
pub fn export_input_color_resolution_counts_for_frame(
    timeline: &TimelineExportSnapshot,
    timeline_frame: i64,
) -> Result<InputColorResolutionSourceCounts, String> {
    let color_context = timeline
        .sequence
        .settings
        .root_program_color_context(&timeline.project_color_management);
    export_sequence_input_color_resolution_counts(
        timeline,
        &timeline.sequence,
        timeline_frame,
        color_context,
        0,
    )
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

/// Aggregate media color-diagnostic issues for assets actually referenced by this export timeline.
pub fn export_asset_issue_summary(
    timeline: &TimelineExportSnapshot,
) -> VideoColorDiagnosticIssueAggregate {
    let mut asset_ids = HashSet::new();
    collect_sequence_asset_ids(timeline, &timeline.sequence, 0, &mut asset_ids);

    let mut summary = VideoColorDiagnosticIssueAggregate::default();
    for asset_id in asset_ids {
        if let Some(diagnostic) = timeline
            .media
            .get(&asset_id)
            .and_then(|dependency| dependency.color_diagnostic.as_ref())
        {
            summary.observe(diagnostic);
        }
    }
    summary
}

fn collect_sequence_asset_ids(
    timeline: &TimelineExportSnapshot,
    sequence: &mondrian_timeline::sequence::Sequence,
    depth: usize,
    asset_ids: &mut HashSet<AssetId>,
) {
    if depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH {
        return;
    }

    for track in &sequence.video_tracks {
        for clip in &track.clips {
            if clip.is_disabled {
                continue;
            }
            if clip.is_nested_sequence() {
                let Some(nested_sequence_id) = clip.nested_sequence_id else {
                    continue;
                };
                let Some(nested_sequence) =
                    timeline.sequences.iter().find(|sequence| sequence.id == nested_sequence_id)
                else {
                    continue;
                };
                collect_sequence_asset_ids(timeline, nested_sequence, depth + 1, asset_ids);
                continue;
            }
            asset_ids.insert(clip.asset_id);
        }
    }
}

fn export_sequence_input_color_resolution_counts(
    timeline: &TimelineExportSnapshot,
    sequence: &mondrian_timeline::sequence::Sequence,
    timeline_frame: i64,
    color_context: ColorContext,
    depth: usize,
) -> Result<InputColorResolutionSourceCounts, String> {
    if depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH {
        return Err("序列嵌套层级过深，已停止统计输入色彩解析以避免循环".to_string());
    }

    let render_plan =
        evaluate_timeline_render_plan(sequence, TimelineEvaluationRequest::export(timeline_frame))
            .map_err(|error| error.to_string())?;
    let mut counts = InputColorResolutionSourceCounts::default();
    for element in &render_plan.elements {
        match element {
            TimelineRenderPlanElement::Media(media) => {
                let dependency = timeline
                    .media
                    .get(&media.asset_id)
                    .ok_or_else(|| format!("导出快照缺少素材依赖: {}", media.asset_id))?;
                let resolution =
                    color_context.missing_metadata_policy.resolve_asset_input_decision(
                        media.color_space_override,
                        dependency.interpretation,
                        dependency.detected_color_space,
                        color_context.working_color_space,
                    );
                counts.record(resolution.source);
            }
            TimelineRenderPlanElement::NestedSequence(nested) => {
                let Some(nested_sequence) =
                    timeline.sequences.iter().find(|sequence| sequence.id == nested.sequence_id)
                else {
                    return Err(format!("嵌套序列不存在: {}", nested.sequence_id));
                };
                let nested_frame = nested
                    .source_time
                    .to_frame_position(nested_sequence.settings.frame_rate, FrameRounding::Floor)
                    .map_err(|error| error.to_string())?
                    .frame
                    .max(0);
                let nested_context =
                    nested_sequence.settings.nested_render_color_context(color_context.clone());
                let nested_counts = export_sequence_input_color_resolution_counts(
                    timeline,
                    nested_sequence,
                    nested_frame,
                    nested_context,
                    depth + 1,
                )?;
                counts.accumulate(nested_counts);
            }
            TimelineRenderPlanElement::Adjustment(_) | TimelineRenderPlanElement::SolidColor(_) => {
            }
        }
    }
    Ok(counts)
}

fn render_sequence_frame_into(
    timeline: &TimelineExportSnapshot,
    sequence: &mondrian_timeline::sequence::Sequence,
    timeline_frame: i64,
    width: u32,
    height: u32,
    color_context: ColorContext,
    alpha_mode: ExportAlphaMode,
    mut target: SequenceRenderTarget<'_>,
    depth: usize,
    mut input_color_counts: Option<&mut InputColorResolutionSourceCounts>,
    mut stage_diagnostics: Option<&mut RenderColorStageDiagnostics>,
    mut composite_diagnostics: Option<&mut TimelineCompositeDiagnostics>,
    mut export_diagnostics: Option<&mut ExportJobColorDiagnostics>,
) -> Result<(), String> {
    if depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH {
        return Err("序列嵌套层级过深，已停止渲染以避免循环".to_string());
    }

    let frame_contract = export_frame_contract(&sequence.settings);
    if let SequenceRenderTarget::Deliverable(canvas) = &mut target {
        let required_len = frame_contract.canvas_len(width, height);
        if canvas.len() != required_len {
            canvas.resize(required_len, 0);
        }
    }

    let render_plan =
        evaluate_timeline_render_plan(sequence, TimelineEvaluationRequest::export(timeline_frame))
            .map_err(|error| error.to_string())?;
    if render_plan.is_empty() {
        finish_empty_sequence_target(
            &mut target,
            frame_contract,
            width,
            height,
            color_context.working_color_space,
            alpha_mode,
        );
        return Ok(());
    }

    let mut decode_cache = (render_plan.len() > 1).then(|| {
        HashMap::<
            (
                AssetId,
                TimelineTime,
                ColorSpace,
                DecodedVideoRangeContract,
                AlphaInterpretation,
            ),
            Arc<DecodedVideoLayer>,
        >::with_capacity(render_plan.len())
    });
    let mut decoded_media =
        std::iter::repeat_with(|| None).take(render_plan.len()).collect::<Vec<_>>();
    let mut nested_media = std::iter::repeat_with(|| None)
        .take(render_plan.len())
        .collect::<Vec<Option<CpuColorFrame>>>();

    for (index, element) in render_plan.elements.iter().enumerate() {
        let TimelineRenderPlanElement::Media(media) = element else {
            continue;
        };
        let dependency = timeline
            .media
            .get(&media.asset_id)
            .ok_or_else(|| format!("导出快照缺少素材依赖: {}", media.asset_id))?;
        let path = &dependency.path;
        let detected_color_space = dependency.detected_color_space;
        let asset_interpretation = dependency.interpretation;
        let input_color_resolution =
            color_context.missing_metadata_policy.resolve_asset_input_decision(
                media.color_space_override,
                asset_interpretation,
                detected_color_space,
                color_context.working_color_space,
            );
        if let Some(counts) = input_color_counts.as_deref_mut() {
            counts.record(input_color_resolution.source);
        }
        let input_color_space = match input_color_resolution.resolved {
            ResolvedInputColor::Color(color_space) => color_space,
            ResolvedInputColor::Data | ResolvedInputColor::Rejected => {
                return Err({
                    let diagnostic = dependency
                        .color_diagnostic
                        .as_ref()
                        .map(mondrian_media::VideoColorDiagnostic::summary)
                        .unwrap_or_else(|| "unavailable".to_string());
                    format!(
                    "asset={} path={} missing color metadata rejected by sequence policy {:?}; resolution={:?} override={:?} detected={:?} working={:?}; {}",
                    media.asset_id,
                    path.display(),
                    color_context.missing_metadata_policy,
                    input_color_resolution.source,
                    input_color_resolution.override_color_space,
                    input_color_resolution.detected_color_space,
                    input_color_resolution.working_color_space,
                    diagnostic
                )
                })
            }
        };
        let input_video_range =
            resolve_export_input_video_range(timeline, media.asset_id, asset_interpretation);
        let cache_key = (
            media.asset_id,
            media.source_time,
            input_color_space,
            input_video_range,
            media.alpha_interpretation,
        );
        let decoded = if let Some(cache) = decode_cache.as_mut() {
            if let Some(hit) = cache.get(&cache_key) {
                Arc::clone(hit)
            } else {
                let decoded = decode_video_layer_scaled(
                    media.asset_id,
                    path.as_path(),
                    input_color_space,
                    input_video_range,
                    media.alpha_interpretation,
                    color_context.working_color_space,
                    &color_context.engine,
                    color_context.tone_map,
                    media.source_time,
                    width,
                    height,
                )?;
                if let Some(diagnostics) = stage_diagnostics.as_deref_mut() {
                    diagnostics.accumulate(decoded.stage_diagnostics);
                }
                cache.insert(cache_key, Arc::clone(&decoded));
                decoded
            }
        } else {
            let decoded = decode_video_layer_scaled(
                media.asset_id,
                path.as_path(),
                input_color_space,
                input_video_range,
                media.alpha_interpretation,
                color_context.working_color_space,
                &color_context.engine,
                color_context.tone_map,
                media.source_time,
                width,
                height,
            )?;
            if let Some(diagnostics) = stage_diagnostics.as_deref_mut() {
                diagnostics.accumulate(decoded.stage_diagnostics);
            }
            decoded
        };
        decoded_media[index] = Some(decoded);
    }

    for (index, element) in render_plan.elements.iter().enumerate() {
        let TimelineRenderPlanElement::NestedSequence(nested) = element else {
            continue;
        };
        let Some(nested_sequence) =
            timeline.sequences.iter().find(|sequence| sequence.id == nested.sequence_id)
        else {
            return Err(format!("嵌套序列不存在: {}", nested.sequence_id));
        };
        let nested_width = normalize_output_dimension(nested_sequence.settings.resolution.width);
        let nested_height = normalize_output_dimension(nested_sequence.settings.resolution.height);
        let nested_frame = nested
            .source_time
            .to_frame_position(nested_sequence.settings.frame_rate, FrameRounding::Floor)
            .map_err(|error| error.to_string())?
            .frame
            .max(0);
        let mut nested_frame_output = None;
        let nested_context =
            nested_sequence.settings.nested_render_color_context(color_context.clone());
        render_sequence_frame_into(
            timeline,
            nested_sequence,
            nested_frame,
            nested_width,
            nested_height,
            nested_context,
            alpha_mode,
            SequenceRenderTarget::Working(&mut nested_frame_output),
            depth + 1,
            input_color_counts.as_deref_mut(),
            stage_diagnostics.as_deref_mut(),
            composite_diagnostics.as_deref_mut(),
            export_diagnostics.as_deref_mut(),
        )?;
        let mut nested_frame = nested_frame_output.ok_or_else(|| {
            format!(
                "nested sequence produced no working frame: {}",
                nested.sequence_id
            )
        })?;
        if nested_frame.descriptor().color_space.working()
            != Some(color_context.working_color_space)
        {
            let converted = execute_cpu_working_transform(
                &nested_frame,
                color_context.working_color_space,
                color_context.engine.clone(),
            )
            .map_err(|err| format!("nested working-space transform failed: {err}"))?;
            if let Some(diagnostics) = stage_diagnostics.as_deref_mut() {
                diagnostics.accumulate(converted.stage_diagnostics);
            }
            nested_frame = converted.result.frame;
        }
        nested_media[index] = Some(nested_frame);
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
                let Some(decoded) = decoded_media[index].as_ref() else {
                    continue;
                };
                composite_elements.push(TimelineCompositeElement::Media(TimelineMediaLayer {
                    frame: &decoded.frame,
                    opacity: media.opacity,
                    blend_mode: media.blend_mode,
                    transform: media.transform,
                    effect_graph: media.effect_graph.clone(),
                    frame_seed: media.frame_seed,
                }));
            }
            TimelineRenderPlanElement::NestedSequence(nested) => {
                let Some(frame) = nested_media[index].as_ref() else {
                    continue;
                };
                composite_elements.push(TimelineCompositeElement::Media(TimelineMediaLayer {
                    frame,
                    opacity: nested.opacity,
                    blend_mode: nested.blend_mode,
                    transform: nested.transform,
                    effect_graph: nested.effect_graph.clone(),
                    frame_seed: nested.frame_seed,
                }));
            }
            TimelineRenderPlanElement::SolidColor(solid) => {
                composite_elements.push(TimelineCompositeElement::SolidColor(
                    TimelineSolidColorLayer {
                        color: solid.color,
                        opacity: solid.opacity,
                        blend_mode: solid.blend_mode,
                        transform: solid.transform,
                        effect_graph: solid.effect_graph.clone(),
                        frame_seed: solid.frame_seed,
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
            alpha_mode,
        );
        return Ok(());
    }

    let mut scratch = TimelineCompositeScratch::default();
    let composite_options = if matches!(&target, SequenceRenderTarget::Deliverable(_))
        && alpha_mode == ExportAlphaMode::FlattenBlack
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
        &mut scratch,
    );
    if let Some(diagnostics) = composite_diagnostics {
        diagnostics.accumulate(rendered.diagnostics);
    }

    if let SequenceRenderTarget::Working(output) = target {
        *output = Some(rendered.frame);
        return Ok(());
    }

    let mut gpu_output_fallback_reasons = ExportGpuOutputFallbackBreakdown::default();
    let mut gpu_output_attempts = 0u64;
    let mut gpu_output_cpu_fallbacks = 0u64;
    let boundary = export_output_boundary_from_context(&color_context)?;
    if color_context.tone_map && boundary.display_view.is_none() {
        if let Some(diagnostics) = export_diagnostics.as_deref_mut() {
            diagnostics.record_output_transform_issue(
                ExportOutputTransformIssueReason::ToneMapRequestedWithoutExportViewTransform,
            );
        }
    }
    let attempt = execute_export_gpu_output_boundary(&rendered.frame, &boundary, frame_contract)
        .inspect_err(|reason| {
            gpu_output_cpu_fallbacks = gpu_output_cpu_fallbacks.saturating_add(1);
            gpu_output_fallback_reasons = gpu_output_fallback_reasons.add_reason(*reason);
        })
        .ok();
    gpu_output_attempts = gpu_output_attempts.saturating_add(1);

    let final_bytes = match attempt {
        Some(attempt) => {
            if let Some(diagnostics) = stage_diagnostics.as_deref_mut() {
                diagnostics.accumulate(attempt.stage_diagnostics);
            }
            attempt.rgba
        }
        None => {
            if frame_contract.requires_high_precision_boundary() {
                match cpu_output_boundary_float(&rendered.frame, &boundary) {
                    Ok(float_result) => {
                        if let Some(diagnostics) = stage_diagnostics {
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
                        if let Some(diagnostics) = export_diagnostics.as_deref_mut() {
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
                let encoded = execute_cpu_output_boundary_rgba8(&rendered.frame, &boundary)
                    .map_err(|err| format!("final color transform failed: {err}"))?;
                if let Some(diagnostics) = stage_diagnostics {
                    diagnostics.accumulate(encoded.stage_diagnostics);
                }
                frame_contract.pack_rgba8(&encoded.rgba)
            }
        }
    };

    if let Some(diagnostics) = export_diagnostics {
        diagnostics.record_export_output_boundary(
            gpu_output_attempts,
            gpu_output_cpu_fallbacks,
            gpu_output_fallback_reasons,
        );
    }
    let SequenceRenderTarget::Deliverable(canvas) = target else {
        unreachable!("working target returned before output boundary");
    };
    canvas.clear();
    canvas.extend_from_slice(&final_bytes);
    Ok(())
}

fn resolve_export_input_video_range(
    timeline: &TimelineExportSnapshot,
    asset_id: AssetId,
    interpretation: mondrian_core::timeline_data::AssetMediaInterpretation,
) -> DecodedVideoRangeContract {
    let detected = timeline
        .media
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

fn decode_video_layer_scaled(
    asset_id: AssetId,
    path: &Path,
    input_color_space: ColorSpace,
    input_video_range: DecodedVideoRangeContract,
    alpha_interpretation: AlphaInterpretation,
    working_color_space: WorkingColorSpace,
    engine: &ColorEngine,
    tone_map: bool,
    source_time: TimelineTime,
    width: u32,
    height: u32,
) -> Result<Arc<DecodedVideoLayer>, String> {
    let request = PreviewDecodeRequest::new(
        path,
        source_time,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        PreviewSourceColorContract::new(input_color_space, input_video_range),
    )
    .with_max_size(Some(width), Some(height));
    let input_transform =
        RenderInputTransform::to_working(working_color_space, tone_map, engine.clone());
    let source: CpuSourceColorFrame = match decode_preview_frame_cancellable(request, || false) {
        Ok(PreviewDecodeOutcome::Frame(frame)) => CpuEncodedColorFrame::source_rgba8_shared(
            frame.width,
            frame.height,
            input_color_space,
            frame.into_shared_data(),
        )
        .into(),
        Ok(PreviewDecodeOutcome::FloatFrame(frame)) => LinearFloatSource::new(
            frame.width,
            frame.height,
            input_color_space,
            frame.into_data(),
        )
        .into(),
        Ok(PreviewDecodeOutcome::Canceled) => {
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
    let execution = execute_cpu_source_input_stage(&source, &input_transform)
        .map_err(|err| format!("asset={asset_id} color transform failed: {err}"))?;
    Ok(Arc::new(DecodedVideoLayer {
        frame: execution.result.frame,
        stage_diagnostics: execution.stage_diagnostics,
    }))
}

fn compute_timeline_render_range(
    timeline: &TimelineExportSnapshot,
) -> Result<TimelineRenderRange, String> {
    let sequence = &timeline.sequence;
    let frame_rate = sequence.settings.frame_rate;
    let sequence_end_exclusive = sequence
        .total_duration()
        .map_err(|error| error.to_string())?
        .to_frame_position(frame_rate, FrameRounding::Ceil)
        .map_err(|error| error.to_string())?
        .frame
        .max(1);
    let (start, requested_end_exclusive) = match timeline.range {
        TimelineExportRange::EntireSequence => (0, sequence_end_exclusive),
        TimelineExportRange::SequenceInOut => {
            let start = sequence
                .in_point()
                .to_frame_position(frame_rate, FrameRounding::Floor)
                .map_err(|error| error.to_string())?
                .frame;
            (
                start,
                sequence
                    .out_point()
                    .map(|time| {
                        time.to_frame_position(frame_rate, FrameRounding::Floor)
                            .map(|frame| frame.frame.saturating_add(1))
                    })
                    .transpose()
                    .map_err(|error| error.to_string())?
                    .unwrap_or(sequence_end_exclusive),
            )
        }
        TimelineExportRange::WorkArea { start_frame, end_frame_exclusive } => {
            (start_frame.max(0), end_frame_exclusive.max(0))
        }
    };
    let max_end_exclusive = sequence_end_exclusive.max(start.saturating_add(1));
    let end_exclusive = requested_end_exclusive.max(start.saturating_add(1)).min(max_end_exclusive);
    let total_frames = end_exclusive.saturating_sub(start) as u64;

    let fps_num = sequence.settings.frame_rate.num.max(1);
    let fps_den = sequence.settings.frame_rate.den.max(1);
    Ok(TimelineRenderRange { start_frame: start, total_frames, fps_num, fps_den })
}

fn timeline_output_resolution(job: &RenderJob, timeline: &TimelineExportSnapshot) -> (u32, u32) {
    if let Some(resolution) = &job.config.preset.resolution {
        return (
            normalize_output_dimension(resolution.width),
            normalize_output_dimension(resolution.height),
        );
    }

    (
        normalize_output_dimension(timeline.sequence.settings.resolution.width),
        normalize_output_dimension(timeline.sequence.settings.resolution.height),
    )
}

fn ffmpeg_channel_layout(layout: AudioChannelLayout) -> Option<&'static str> {
    match layout {
        AudioChannelLayout::Mono => Some("mono"),
        AudioChannelLayout::Stereo => Some("stereo"),
        AudioChannelLayout::Surround51Side => Some("5.1(side)"),
        AudioChannelLayout::Speakers(_) | AudioChannelLayout::Discrete(_) => None,
    }
}

fn normalize_output_dimension(value: u32) -> u32 {
    let mut dim = value.max(1);
    if dim > 1 && dim % 2 == 1 {
        dim = dim.saturating_sub(1);
    }
    dim.max(1)
}

mod helpers;
pub(crate) use helpers::*;

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::timeline_data::{
        AssetColorPayload, AssetMediaInterpretation, MediaColorInterpretation,
        MediaRangeInterpretation, MediaSignalRange,
    };
    use mondrian_core::types::{AssetId, BlendMode, FramePosition};
    use mondrian_core::{VideoContentLightMetadata, VideoMasteringDisplayMetadata};
    use mondrian_effects::{get_or_compile_scheduled_effect_graph, EffectRenderPlan};
    use mondrian_renderer::RenderOutputColorBoundaryTarget;
    use mondrian_timeline::clip::Clip;
    use mondrian_timeline::sequence::{
        InputColorResolutionSource, MissingColorMetadataPolicy, Sequence, StaticHdrMetadataPolicy,
    };
    use mondrian_timeline::track::Track;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn tt(frame: i64, time_base: Rational) -> TimelineTime {
        TimelineTime::from_frame_position(FramePosition::new(frame, time_base))
            .expect("valid test time")
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

    struct FakeExecutor {
        calls: Arc<AtomicUsize>,
        delay_ms: u64,
    }

    impl ExportExecutor for FakeExecutor {
        fn execute(
            &self,
            _job: &RenderJob,
            cancel: &ExecutionCancellationToken,
            report: &mut dyn FnMut(ExportProgress),
            _report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
        ) -> JobExecutionResult {
            self.calls.fetch_add(1, Ordering::Relaxed);
            report(ExportProgress::encoding(0.2));

            let step = 20u64;
            let mut elapsed = 0u64;
            while elapsed < self.delay_ms {
                if cancel.is_canceled() {
                    return JobExecutionResult::Cancelled;
                }
                std::thread::sleep(Duration::from_millis(step));
                elapsed += step;
            }

            report(ExportProgress::rendering(0.95, 1_000, 1_000));
            JobExecutionResult::Completed
        }
    }

    struct DiagnosticExecutor {
        diagnostics: ExportJobDiagnostics,
    }

    impl ExportExecutor for DiagnosticExecutor {
        fn execute(
            &self,
            _job: &RenderJob,
            _cancel: &ExecutionCancellationToken,
            report: &mut dyn FnMut(ExportProgress),
            report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
        ) -> JobExecutionResult {
            report(ExportProgress::rendering(0.5, 1, 1));
            report_diagnostics(self.diagnostics);
            JobExecutionResult::Completed
        }
    }

    fn dummy_config(output_name: &str) -> ExportConfig {
        ExportConfig {
            preset: crate::preset::ExportPreset::youtube_1080p(),
            timeline: Box::new(timeline_input_with_output_color(ColorSpace::Rec709)),
            output_path: PathBuf::from(output_name),
        }
    }

    fn timeline_input_with_output_color(output_color_space: ColorSpace) -> TimelineExportSnapshot {
        let mut sequence = Sequence::new("color-validation");
        sequence.settings.color_management.output_color_space = output_color_space;
        TimelineExportSnapshot {
            sequence,
            sequences: Vec::new(),
            media: HashMap::new(),
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
        }
    }

    fn test_media_dependency(
        path: PathBuf,
        detected_color_space: Option<ColorSpace>,
        interpretation: AssetMediaInterpretation,
        color_diagnostic: Option<mondrian_media::VideoColorDiagnostic>,
    ) -> crate::preset::ExportMediaDependency {
        crate::preset::ExportMediaDependency {
            source_fingerprint: MediaFileFingerprint::capture(path.as_path()),
            path,
            audio_components: HashMap::new(),
            detected_color_space,
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
        std::fs::write(&source, b"source revision changed").expect("replace source");

        let error = validate_snapshot_media_revisions(&timeline)
            .expect_err("changed source revision must fail closed");
        assert!(error.contains(&asset_id.to_string()));
        assert!(error.contains(source.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn validated_export_publication_replaces_final_atomically() {
        let root = std::env::temp_dir().join(format!("mondrian-export-publish-{}", JobId::new()));
        std::fs::create_dir_all(&root).expect("create export publication root");
        let final_output = root.join("deliverable.mp4");
        let job_id = JobId::new();
        let partial_output = export_partial_output_path(&final_output, job_id);
        std::fs::write(&final_output, b"prior deliverable").expect("write prior output");
        std::fs::write(&partial_output, b"validated deliverable").expect("write partial output");

        finalize_export_output(&partial_output, &final_output).expect("publish validated output");

        assert_eq!(
            std::fs::read(&final_output).expect("read published output"),
            b"validated deliverable"
        );
        assert!(!partial_output.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_partial_never_disturbs_existing_deliverable() {
        let root = std::env::temp_dir().join(format!("mondrian-export-preserve-{}", JobId::new()));
        std::fs::create_dir_all(&root).expect("create export preservation root");
        let final_output = root.join("deliverable.mp4");
        let job_id = JobId::new();
        std::fs::write(&final_output, b"prior deliverable").expect("write prior output");

        finalize_export_output(
            export_partial_output_path(&final_output, job_id).as_path(),
            &final_output,
        )
        .expect_err("missing partial must fail");

        assert_eq!(
            std::fs::read(&final_output).expect("read preserved output"),
            b"prior deliverable"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    fn test_color_diagnostic(
        source: mondrian_media::VideoColorSpaceSource,
        method: mondrian_media::VideoColorDetectionMethod,
        warning: Option<mondrian_media::VideoColorInterpretationWarning>,
    ) -> mondrian_media::VideoColorDiagnostic {
        let warnings = warning.into_iter().collect::<Vec<_>>();
        mondrian_media::VideoColorDiagnostic {
            detected_color_space: None,
            color_range: mondrian_media::DecodedVideoRange::Unknown,
            interpretation: mondrian_media::DetectedColorInterpretation {
                color_space: None,
                confidence: mondrian_media::VideoColorInterpretationConfidence::None,
                source,
                method,
                evidence: Vec::new(),
                warnings: warnings.clone(),
                user_overridable: true,
            },
            source,
            method,
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
    fn clear_completed_removes_all_terminal_jobs() {
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

        queue.clear_completed();

        assert!(queue.list_jobs().is_empty());
        assert_eq!(calls.load(Ordering::Relaxed), 2);
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
        seq.settings.color_management.delivery_bit_depth = DeliveryBitDepth::Ten;
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
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
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
        let ctx = ColorContext {
            working_color_space: WorkingColorSpace::LinearRec709,
            output_color_space: ColorSpace::Srgb.into(),
            tone_map: true,
            workflow: mondrian_timeline::sequence::ColorWorkflow::DisplayReferred,
            nested_processing:
                mondrian_core::timeline_data::NestedColorProcessing::PreserveChildWorkingSpace,
            engine: ColorEngine::mondrian_standard(),
            missing_metadata_policy:
                mondrian_timeline::sequence::MissingColorMetadataPolicy::AssumeRec709,
            display_management: mondrian_core::color_models::DisplayManagementPolicy::default(),
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
        let ctx = ColorContext {
            working_color_space: WorkingColorSpace::LinearRec709,
            output_color_space: ColorSpace::Rec709.into(),
            tone_map: false,
            workflow: mondrian_timeline::sequence::ColorWorkflow::DisplayReferred,
            nested_processing:
                mondrian_core::timeline_data::NestedColorProcessing::PreserveChildWorkingSpace,
            engine: ColorEngine::mondrian_standard(),
            missing_metadata_policy:
                mondrian_timeline::sequence::MissingColorMetadataPolicy::AssumeRec709,
            display_management: mondrian_core::color_models::DisplayManagementPolicy::default(),
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
        seq.settings.color_management.delivery_bit_depth = DeliveryBitDepth::Eight;
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
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
        };
        let ctx = ColorContext {
            working_color_space: WorkingColorSpace::LinearRec709,
            output_color_space: ColorSpace::Rec709.into(),
            tone_map: true,
            workflow: mondrian_timeline::sequence::ColorWorkflow::DisplayReferred,
            nested_processing:
                mondrian_core::timeline_data::NestedColorProcessing::PreserveChildWorkingSpace,
            engine: ColorEngine::mondrian_standard(),
            missing_metadata_policy:
                mondrian_timeline::sequence::MissingColorMetadataPolicy::AssumeRec709,
            display_management: mondrian_core::color_models::DisplayManagementPolicy::default(),
            output_transform: mondrian_core::OutputTransformIntent::mondrian_standard(),
        };

        let boundary = export_output_boundary_from_context(&ctx).expect("encoded output");
        // The boundary has a view -> no issue should be recorded.
        assert!(boundary.display_view.is_some());
        assert!(boundary.tone_map);
        assert_eq!(boundary.target, RenderOutputColorBoundaryTarget::Export);

        let mut diagnostics = ExportJobColorDiagnostics::default();
        let mut canvas = vec![0u8; 2 * 2 * 4];
        render_sequence_frame_into(
            &timeline,
            &timeline.sequence,
            0,
            2,
            2,
            ctx,
            ExportAlphaMode::FlattenBlack,
            SequenceRenderTarget::Deliverable(&mut canvas),
            0,
            None,
            None,
            None,
            Some(&mut diagnostics),
        )
        .expect("render with engine-owned output intent");

        assert_eq!(canvas.len(), 2 * 2 * 4);
        assert_eq!(diagnostics.output_transform_issues, 0);
        assert_eq!(diagnostics.output_transform_issue_reasons.total(), 0);
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
    fn export_asset_issue_summary_scopes_to_referenced_assets_only() {
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
        let timeline = TimelineExportSnapshot {
            sequence,
            sequences: vec![nested],
            media,
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
        };

        assert_eq!(
            export_asset_issue_summary(&timeline),
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
    fn export_color_validation_rejects_camera_log_consumer_codecs() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::AppleLogBt2020);
        timeline.sequence.settings.color_management.delivery_bit_depth = DeliveryBitDepth::Ten;
        let config = dummy_config("camera-log.mp4");

        let err = validate_timeline_export_color_compatibility(&config, &timeline)
            .expect_err("camera log should reject H.264/MP4 delivery");
        assert!(err.contains("Camera log"));
        assert!(err.contains("ProRes"));
    }

    #[test]
    fn export_color_validation_allows_camera_log_prores_intermediate() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::AppleLogBt2020);
        timeline.sequence.settings.color_management.delivery_bit_depth = DeliveryBitDepth::Twelve;

        let mut config = dummy_config("camera-log.mov");
        config.preset.container = Container::Mov;
        config.preset.video = VideoCodecConfig::ProRes { variant: "4444xq".to_string() };

        validate_timeline_export_color_compatibility(&config, &timeline)
            .expect("camera log ProRes intermediate should pass");
    }

    #[test]
    fn export_alpha_validation_rejects_opaque_delivery_codec() {
        let timeline = timeline_input_with_output_color(ColorSpace::Rec709);
        let mut config = dummy_config("alpha.mp4");
        config.preset.alpha_mode = ExportAlphaMode::Preserve;

        let err = validate_timeline_export_color_compatibility(&config, &timeline)
            .expect_err("H.264 must not pretend to preserve alpha");

        assert!(err.contains("ProRes 4444"));
    }

    #[test]
    fn export_alpha_validation_allows_mov_prores_4444_xq() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec709);
        timeline.sequence.settings.color_management.delivery_bit_depth = DeliveryBitDepth::Twelve;
        let mut config = dummy_config("alpha.mov");
        config.preset.container = Container::Mov;
        config.preset.video = VideoCodecConfig::ProRes { variant: "4444xq".to_owned() };
        config.preset.alpha_mode = ExportAlphaMode::Preserve;

        validate_timeline_export_color_compatibility(&config, &timeline)
            .expect("MOV ProRes 4444 XQ should preserve alpha");
    }

    #[test]
    fn export_color_validation_binds_prores_profile_to_real_sample_depth() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec709);
        let mut config = dummy_config("prores.mov");
        config.preset.container = Container::Mov;

        config.preset.video = VideoCodecConfig::ProRes { variant: "hq".to_owned() };
        timeline.sequence.settings.color_management.delivery_bit_depth = DeliveryBitDepth::Twelve;
        let err = validate_timeline_export_color_compatibility(&config, &timeline)
            .expect_err("ProRes HQ is a 10-bit profile");
        assert!(err.contains("必须声明 10-bit"));

        config.preset.video = VideoCodecConfig::ProRes { variant: "4444xq".to_owned() };
        timeline.sequence.settings.color_management.delivery_bit_depth = DeliveryBitDepth::Ten;
        let err = validate_timeline_export_color_compatibility(&config, &timeline)
            .expect_err("ProRes 4444 XQ is a 12-bit profile");
        assert!(err.contains("必须声明 12-bit"));
    }

    #[test]
    fn export_color_validation_rejects_static_hdr_write_without_typed_metadata() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec2100Pq);
        timeline.sequence.settings.color_management.delivery_bit_depth = DeliveryBitDepth::Ten;
        timeline.sequence.settings.color_management.static_hdr_metadata_policy =
            StaticHdrMetadataPolicy::WriteAuthored;
        let mut config = dummy_config("hdr-missing-metadata.mp4");
        config.preset.video = VideoCodecConfig::H265 { crf: 20, bitrate_kbps: None };

        let err = validate_timeline_export_color_compatibility(&config, &timeline)
            .expect_err("static HDR writing should require typed metadata");
        assert!(err.contains("SMPTE ST 2086"));
    }

    #[test]
    fn export_color_validation_allows_static_hdr_write_with_typed_metadata() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec2100Pq);
        timeline.sequence.settings.color_management.delivery_bit_depth = DeliveryBitDepth::Ten;
        timeline.sequence.settings.color_management.static_hdr_metadata_policy =
            StaticHdrMetadataPolicy::WriteAuthored;
        timeline.sequence.settings.color_management.hdr_mastering_display =
            Some(VideoMasteringDisplayMetadata::rec2100_1000_nit_reference());
        timeline.sequence.settings.color_management.hdr_content_light =
            Some(VideoContentLightMetadata::rec2100_1000_nit_reference());
        let mut config = dummy_config("hdr-with-metadata.mp4");
        config.preset.video = VideoCodecConfig::H265 { crf: 20, bitrate_kbps: None };

        validate_timeline_export_color_compatibility(&config, &timeline)
            .expect("typed HDR metadata should pass validation");
    }

    #[test]
    fn export_color_validation_binds_content_light_to_standard_view_peak() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec2100Pq);
        timeline.sequence.settings.color_management.delivery_bit_depth = DeliveryBitDepth::Ten;
        timeline.sequence.settings.color_management.static_hdr_metadata_policy =
            StaticHdrMetadataPolicy::WriteAuthored;
        let mut mastering = VideoMasteringDisplayMetadata::rec2100_1000_nit_reference();
        mastering.luminance.as_mut().expect("reference luminance").max =
            mondrian_core::VideoHdrRational::new(4000, 1);
        timeline.sequence.settings.color_management.hdr_mastering_display = Some(mastering);
        timeline.sequence.settings.color_management.hdr_content_light =
            Some(VideoContentLightMetadata::rec2100_1000_nit_reference());
        let mut config = dummy_config("hdr-content-light-contract.mp4");
        config.preset.video = VideoCodecConfig::H265 { crf: 20, bitrate_kbps: None };

        validate_timeline_export_color_compatibility(&config, &timeline)
            .expect("mastering-display capability may exceed the Standard View's content peak");

        timeline.sequence.settings.color_management.hdr_mastering_display =
            Some(VideoMasteringDisplayMetadata::rec2100_1000_nit_reference());
        timeline.sequence.settings.color_management.hdr_content_light =
            Some(VideoContentLightMetadata {
                max_content_light_level: 1200,
                max_frame_average_light_level: 400,
            });
        let err = validate_timeline_export_color_compatibility(&config, &timeline)
            .expect_err("MaxCLL must not exceed the fixed Standard View peak");
        assert!(err.contains("峰值为 1000 nit"));
        assert!(err.contains("MaxCLL 声明 1200 nit"));
    }

    #[test]
    fn export_color_validation_rejects_unimplemented_hdr_metadata_backends() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec2100Pq);
        timeline.sequence.settings.color_management.delivery_bit_depth = DeliveryBitDepth::Ten;
        timeline.sequence.settings.color_management.static_hdr_metadata_policy =
            StaticHdrMetadataPolicy::WriteAuthored;
        timeline.sequence.settings.color_management.hdr_mastering_display =
            Some(VideoMasteringDisplayMetadata::rec2100_1000_nit_reference());
        timeline.sequence.settings.color_management.hdr_content_light =
            Some(VideoContentLightMetadata::rec2100_1000_nit_reference());

        for codec in [
            VideoCodecConfig::Av1 { crf: 24 },
            VideoCodecConfig::ProRes { variant: "hq".to_owned() },
        ] {
            let mut config = dummy_config("hdr-unsupported-metadata.mov");
            config.preset.container = Container::Mov;
            config.preset.video = codec;

            let err = validate_timeline_export_color_compatibility(&config, &timeline)
                .expect_err("metadata preservation needs a verified encoder backend");
            assert!(err.contains("H.265/libx265"));
            assert!(err.contains("metadata backend"));
        }
    }

    #[test]
    fn export_color_validation_rejects_dynamic_hdr_passthrough_claim() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec2100Pq);
        timeline.sequence.settings.color_management.delivery_bit_depth = DeliveryBitDepth::Ten;
        timeline.sequence.settings.color_management.static_hdr_metadata_policy =
            StaticHdrMetadataPolicy::WriteAuthored;
        timeline.sequence.settings.color_management.hdr_mastering_display =
            Some(VideoMasteringDisplayMetadata::rec2100_1000_nit_reference());
        timeline.sequence.settings.color_management.hdr_content_light =
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
        let mut config = dummy_config("hdr-dynamic-passthrough.mp4");
        config.preset.video = VideoCodecConfig::H265 { crf: 20, bitrate_kbps: None };

        let error = validate_timeline_export_color_compatibility(&config, &timeline)
            .expect_err("rendered export must not claim dynamic HDR passthrough");
        assert!(error.contains("HDR10+ 动态 metadata（1 个）"));
        assert!(error.contains("不能安全透传"));
        assert!(error.contains("动态 HDR 重新制作流程"));
    }

    #[test]
    fn export_color_validation_requires_explicit_srgb_for_untagged_gif() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec709);
        timeline.sequence.settings.color_management.delivery_bit_depth = DeliveryBitDepth::Eight;
        let mut config = dummy_config("untagged.gif");
        config.preset.container = Container::Gif;
        config.preset.video = VideoCodecConfig::Gif { colors: 256, dither: true };

        let err = validate_timeline_export_color_compatibility(&config, &timeline)
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
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
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
            range: TimelineExportRange::EntireSequence,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
        };

        let range = compute_timeline_render_range(&timeline).expect("valid render range");
        assert_eq!(range.start_frame, 0);
        assert_eq!(range.total_frames, 200);
    }

    #[test]
    fn export_input_color_resolution_counts_for_frame_tracks_media_sources() {
        let mut seq = Sequence::new("export-input-color-counts");
        seq.settings.working_color_space = WorkingColorSpace::LinearRec2020;
        seq.settings.color_management.missing_metadata_policy =
            MissingColorMetadataPolicy::AssumeRec709;
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
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
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
            resolve_export_input_video_range(&timeline, asset_id, interpretation),
            DecodedVideoRangeContract::OverrideFull
        );
        assert_eq!(
            resolve_export_input_video_range(
                &timeline,
                asset_id,
                AssetMediaInterpretation::default()
            ),
            DecodedVideoRangeContract::Automatic { probed_range: DecodedVideoRange::Limited }
        );
    }

    #[test]
    fn timeline_has_audio_content_detects_overlap() {
        let mut seq = Sequence::new("audio-range-test");
        let tb = seq.time_base();
        let asset_id = AssetId::new();
        let clip = Clip::new(asset_id, tt(25, tb), tt(20, tb)).expect("valid clip");
        let track_id = seq.audio_tracks[0].id;
        seq.add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
            .expect("add audio clip");
        seq.in_point = Some(tt(30, tb));
        seq.out_point = Some(tt(40, tb));

        let mut media = HashMap::new();
        media.insert(
            asset_id,
            test_media_dependency(
                PathBuf::from("dummy-audio.wav"),
                None,
                AssetMediaInterpretation::default(),
                None,
            ),
        );
        let timeline = TimelineExportSnapshot {
            sequence: seq,
            sequences: Vec::new(),
            media,
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
        };

        let range = compute_timeline_render_range(&timeline).expect("valid render range");
        assert!(timeline_has_audio_content(&timeline, range).expect("audio presence"));
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
        let cache = Arc::new(AudioSourceCache::new(48_000, AudioChannelLayout::Stereo));
        let resolver = ExportAudioMediaResolver { timeline: &timeline, cache };

        assert!(resolver
            .resolve(
                asset_id,
                component_id,
                AudioRenderContract {
                    sample_rate: 48_000,
                    channel_layout: AudioChannelLayout::Stereo,
                    max_block_frames: 1_024,
                    processing_mode: AudioProcessingMode::Offline,
                },
            )
            .is_ok());
        assert!(resolver
            .resolve(
                asset_id,
                AudioSourceComponentId::new(),
                AudioRenderContract {
                    sample_rate: 48_000,
                    channel_layout: AudioChannelLayout::Stereo,
                    max_block_frames: 1_024,
                    processing_mode: AudioProcessingMode::Offline,
                },
            )
            .is_err());
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
            ffmpeg_channel_layout(AudioChannelLayout::Mono),
            Some("mono")
        );
        assert_eq!(
            ffmpeg_channel_layout(AudioChannelLayout::Stereo),
            Some("stereo")
        );
        assert_eq!(
            ffmpeg_channel_layout(AudioChannelLayout::Surround51Side),
            Some("5.1(side)")
        );
        assert_eq!(
            ffmpeg_channel_layout(AudioChannelLayout::Surround51Back),
            None
        );
        assert_eq!(
            ffmpeg_channel_layout(AudioChannelLayout::discrete(8).expect("discrete layout")),
            None
        );
    }

    #[test]
    fn export_video_signal_args_bind_bit_depth_range_and_matrix_conversion() {
        let mut settings = mondrian_timeline::sequence::SequenceSettings::default();
        settings.color_management.delivery_bit_depth = DeliveryBitDepth::Ten;
        settings.color_management.video_range = VideoRange::Legal;
        settings.color_management.output_color_space = ColorSpace::Rec2100Pq;
        let codec = VideoCodecConfig::H265 { crf: 20, bitrate_kbps: None };

        let mut cmd = Command::new("ffmpeg");
        apply_export_video_signal_args(&mut cmd, &settings, &codec, ExportAlphaMode::FlattenBlack);
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
        settings.color_management.output_color_space = ColorSpace::Rec2100Pq;
        let codec = VideoCodecConfig::H265 { crf: 20, bitrate_kbps: None };
        let mut cmd = Command::new("ffmpeg");
        apply_export_video_signal_args(&mut cmd, &settings, &codec, ExportAlphaMode::FlattenBlack);
        let args = cmd.get_args().map(|arg| arg.to_string_lossy().to_string()).collect::<Vec<_>>();

        assert!(args.windows(2).any(|pair| pair == ["-color_primaries", "bt2020"]));
        assert!(args.windows(2).any(|pair| pair == ["-color_trc", "smpte2084"]));
        assert!(args.windows(2).any(|pair| pair == ["-colorspace", "bt2020nc"]));
    }

    #[test]
    fn color_tag_args_skip_camera_log_spaces_without_standard_delivery_tags() {
        let codec = VideoCodecConfig::ProRes { variant: "hq".to_owned() };
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
            let mut settings = SequenceSettings::default();
            settings.color_management.output_color_space = color_space;
            settings.color_management.delivery_bit_depth = DeliveryBitDepth::Ten;
            let mut cmd = Command::new("ffmpeg");
            apply_export_video_signal_args(
                &mut cmd,
                &settings,
                &codec,
                ExportAlphaMode::FlattenBlack,
            );

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
        settings.color_management.output_color_space = ColorSpace::Srgb;
        let codec = VideoCodecConfig::H264 { crf: 20, bitrate_kbps: None };
        let mut cmd = Command::new("ffmpeg");

        apply_export_video_signal_args(&mut cmd, &settings, &codec, ExportAlphaMode::FlattenBlack);

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
        settings.color_management.output_color_space = ColorSpace::Rec2100Pq;
        settings.color_management.delivery_bit_depth = DeliveryBitDepth::Ten;
        settings.color_management.video_range = VideoRange::Legal;
        let codec = VideoCodecConfig::H265 { crf: 20, bitrate_kbps: None };

        let expected =
            expected_export_video_signal(&settings, &codec, ExportAlphaMode::FlattenBlack)
                .expect("valid PQ signal contract");

        assert_eq!(expected.pixel_format.as_deref(), Some("yuv420p10le"));
        assert_eq!(expected.color_range.as_deref(), Some("tv"));
        assert_eq!(expected.color_primaries.as_deref(), Some("bt2020"));
        assert_eq!(expected.color_transfer.as_deref(), Some("smpte2084"));
        assert_eq!(expected.color_matrix.as_deref(), Some("bt2020nc"));
        assert!(!expected.require_color_tags_absent);
        assert!(expected.static_hdr_metadata.is_none());

        settings.color_management.static_hdr_metadata_policy =
            StaticHdrMetadataPolicy::WriteAuthored;
        settings.color_management.hdr_mastering_display =
            Some(VideoMasteringDisplayMetadata::rec2100_1000_nit_reference());
        settings.color_management.hdr_content_light =
            Some(VideoContentLightMetadata::rec2100_1000_nit_reference());
        let expected =
            expected_export_video_signal(&settings, &codec, ExportAlphaMode::FlattenBlack)
                .expect("valid static HDR metadata contract");
        let expected_static_hdr = expected
            .static_hdr_metadata
            .expect("post-encode contract must retain authored static HDR metadata");
        assert_eq!(
            expected_static_hdr.content_light,
            VideoContentLightMetadata::rec2100_1000_nit_reference()
        );
        settings.color_management.static_hdr_metadata_policy = StaticHdrMetadataPolicy::Omit;

        settings.color_management.output_color_space = ColorSpace::AppleLogBt2020;
        settings.color_management.delivery_bit_depth = DeliveryBitDepth::Twelve;
        let prores = VideoCodecConfig::ProRes { variant: "4444xq".to_owned() };
        let expected =
            expected_export_video_signal(&settings, &prores, ExportAlphaMode::FlattenBlack)
                .expect("valid ProRes signal contract");
        assert_eq!(expected.pixel_format.as_deref(), Some("yuv444p12le"));
        let alpha_expected =
            expected_export_video_signal(&settings, &prores, ExportAlphaMode::Preserve)
                .expect("valid ProRes alpha signal contract");
        assert_eq!(alpha_expected.pixel_format.as_deref(), Some("yuva444p12le"));
        assert!(expected.require_color_tags_absent);
    }

    #[test]
    fn rec601_delivery_preserves_pal_and_ntsc_signal_tags() {
        let codec = VideoCodecConfig::H264 { crf: 20, bitrate_kbps: None };
        for (color_space, primaries, transfer, matrix) in [
            (ColorSpace::Rec601Pal, "bt470bg", "bt470bg", "bt470bg"),
            (
                ColorSpace::Rec601Ntsc,
                "smpte170m",
                "smpte170m",
                "smpte170m",
            ),
        ] {
            let mut settings = SequenceSettings::default();
            settings.color_management.output_color_space = color_space;
            let expected =
                expected_export_video_signal(&settings, &codec, ExportAlphaMode::FlattenBlack)
                    .expect("valid Rec.601 signal contract");
            assert_eq!(expected.color_primaries.as_deref(), Some(primaries));
            assert_eq!(expected.color_transfer.as_deref(), Some(transfer));
            assert_eq!(expected.color_matrix.as_deref(), Some(matrix));

            let mut cmd = Command::new("ffmpeg");
            apply_export_video_signal_args(
                &mut cmd,
                &settings,
                &codec,
                ExportAlphaMode::FlattenBlack,
            );
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
        seq.settings.color_management.delivery_bit_depth = DeliveryBitDepth::Eight;
        let tb = seq.time_base();
        seq.in_point = Some(tt(0, tb));
        seq.out_point = Some(tt(10, tb));
        let timeline = TimelineExportSnapshot {
            sequence: seq,
            sequences: Vec::new(),
            media: HashMap::new(),
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
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
    }

    #[test]
    fn render_timeline_frame_into_preserves_or_flattens_alpha_explicitly() {
        let mut seq = Sequence::new("alpha-delivery");
        seq.settings.color_management.delivery_bit_depth = DeliveryBitDepth::Eight;
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
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
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
    fn render_timeline_frame_into_uses_rgba64le_canvas_for_ten_bit_no_layers() {
        let mut seq = Sequence::new("empty-ten-bit");
        seq.settings.color_management.delivery_bit_depth = DeliveryBitDepth::Ten;
        let tb = seq.time_base();
        seq.in_point = Some(tt(0, tb));
        seq.out_point = Some(tt(10, tb));
        let timeline = TimelineExportSnapshot {
            sequence: seq,
            sequences: Vec::new(),
            media: HashMap::new(),
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
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
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
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
        seq.settings.color_management.missing_metadata_policy =
            MissingColorMetadataPolicy::RejectMedia;
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
            detected_color_space: None,
            color_range: mondrian_media::DecodedVideoRange::Unknown,
            interpretation: mondrian_media::DetectedColorInterpretation {
                color_space: None,
                confidence: mondrian_media::VideoColorInterpretationConfidence::None,
                source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
                method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
                evidence: Vec::new(),
                warnings: vec![mondrian_media::VideoColorInterpretationWarning::MissingCicpTags],
                user_overridable: true,
            },
            source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
            method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
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
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
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
                    effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
                        ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                            exposure: 0.0,
                            contrast: 1.0,
                            saturation: 0.0,
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
                        effect_graph: get_or_compile_scheduled_effect_graph(
                            &EffectRenderPlan::default(),
                        )
                        .expect("compile identity graph"),
                        frame_seed: 0,
                    },
                ),
                mondrian_renderer::TimelineCompositeElement::Adjustment(
                    mondrian_renderer::TimelineAdjustmentLayer {
                        effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
                            ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                                exposure: 0.0,
                                contrast: 1.0,
                                saturation: 0.0,
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
                        effect_graph: get_or_compile_scheduled_effect_graph(
                            &EffectRenderPlan::default(),
                        )
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
        seq.settings.color_management.delivery_bit_depth = DeliveryBitDepth::Ten;
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
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
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
        seq.settings.color_management.delivery_bit_depth = DeliveryBitDepth::Ten;
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
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
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
