//! 后台渲染队列

use crate::preset::{
    AudioCodecConfig, Container, ExportConfig, ExportInput, TimelineExportInput,
    TimelineExportRange, VideoCodecConfig,
};
use crate::validator::{
    probe_media_summary, validate_export_output, ExpectedVideoConstraints,
    ExportValidationExpectations,
};
use chrono::{DateTime, Utc};
use mondrian_core::types::{AssetId, ColorEngine, ColorSpace, JobId, Rational, TimeCode};
use mondrian_media::audio::{
    AudioBuffer, AudioMixer, AudioSourceCache, AudioTrackConfig, AudioTrackData,
};
use mondrian_media::decode_video_frame_at_time_rgba_scaled;
use mondrian_media::VideoColorDiagnosticIssueAggregate;
use mondrian_renderer::{
    color_report_vocab, composite_timeline_elements_color_frame_with_diagnostics,
    evaluate_timeline_render_plan, execute_cpu_input_stage, execute_cpu_output_boundary_float,
    execute_cpu_output_boundary_rgba8, ColorFrameResidency, CpuColorFrame, CpuEncodedColorFrame,
    GpuColorFrameReadbackPlan, GpuColorFrameTextureFormat, GpuContext, RenderColorStageDiagnostics,
    RenderColorStageGpuBlockerBreakdown, RenderColorTransformGpuOptions,
    RenderGpuOutputBoundaryRuntime, RenderGpuOutputBoundaryRuntimeOwnedBackendContext,
    RenderInputTransform, RenderOutputColorBoundary, TimelineAdjustmentLayer,
    TimelineCompositeColorPathSummary, TimelineCompositeDiagnostics, TimelineCompositeElement,
    TimelineCompositeLegacyBreakdown, TimelineCompositeOptions, TimelineCompositeScratch,
    TimelineEvaluationRequest, TimelineMediaLayer, TimelineRenderPlanElement,
    TimelineSolidColorLayer,
};
use mondrian_timeline::sequence::{
    ColorContext, ExportBitDepth, InputColorResolutionSourceCounts, SequenceSettings, VideoRange,
    MAX_NESTED_SEQUENCE_RENDER_DEPTH,
};
use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;
use tokio::runtime::Builder as TokioRuntimeBuilder;

/// Frame contract for export output, selected based on `ExportBitDepth`.
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
    /// Select the appropriate frame contract from the export bit depth.
    pub fn from_bit_depth(bit_depth: ExportBitDepth) -> Self {
        match bit_depth {
            ExportBitDepth::Eight => Self::Rgba8,
            ExportBitDepth::Ten | ExportBitDepth::SixteenFloat => Self::Rgba16Float,
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
    ExportFrameContract::from_bit_depth(settings.color_management.export_bit_depth)
}

/// Renderer-owned CPU float/high-bit output boundary for export.
///
/// In production this is a transparent pass-through to the renderer's
/// `execute_cpu_output_boundary_float`. Test builds support failure injection
/// via `FORCE_FLOAT_BOUNDARY_FAILURE` so integration tests can exercise the
/// RGBA8 precision-fallback branch without mocking the color engine.
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
    /// `Err`, exercising the RGBA8 precision-fallback branch in real render code.
    static FORCE_FLOAT_BOUNDARY_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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

    Ok(ExportGpuOutputAttemptOutcome { rgba, stage_diagnostics: record.stage_diagnostics })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum JobStatus {
    Pending,
    Rendering { frame: u64, total_frames: u64 },
    Encoding,
    Completed,
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct RenderJob {
    pub id: JobId,
    pub config: ExportConfig,
    pub status: JobStatus,
    pub progress: f32,
    /// Export diagnostics accumulated by the worker while the job runs.
    pub diagnostics: ExportJobDiagnostics,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
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
    /// Final export output precision fallbacks observed while encoding.
    pub output_precision_fallbacks: u64,
    /// Structured final export output precision fallback reasons.
    pub output_precision_fallback_reasons: ExportOutputPrecisionFallbackBreakdown,
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

    /// Record one precision fallback at the final export output contract.
    pub fn record_output_precision_fallback(
        &mut self,
        reason: ExportOutputPrecisionFallbackReason,
    ) {
        self.output_precision_fallbacks = self.output_precision_fallbacks.saturating_add(1);
        self.output_precision_fallback_reasons =
            self.output_precision_fallback_reasons.add_reason(reason);
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

/// Structured reason for final export output precision fallback.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExportOutputPrecisionFallbackReason {
    /// High-bit-depth export fell back to an RGBA8 CPU output boundary before pipe packing.
    CpuRgba8BoundaryPackedToHighBitDepthPipe,
}

/// Structured final export output precision fallback counts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExportOutputPrecisionFallbackBreakdown {
    /// High-bit-depth export used an RGBA8 CPU output boundary before `rgba64le` pipe packing.
    pub cpu_rgba8_boundary_packed_to_high_bit_depth_pipe: u64,
}

impl ExportOutputPrecisionFallbackBreakdown {
    /// Return total fallback count across all recorded reasons.
    pub fn total(&self) -> u64 {
        self.cpu_rgba8_boundary_packed_to_high_bit_depth_pipe
    }

    /// Merge another breakdown in place.
    pub fn accumulate(self, other: Self) -> Self {
        Self {
            cpu_rgba8_boundary_packed_to_high_bit_depth_pipe: self
                .cpu_rgba8_boundary_packed_to_high_bit_depth_pipe
                .saturating_add(other.cpu_rgba8_boundary_packed_to_high_bit_depth_pipe),
        }
    }

    /// Map one reason into a mut accumulator entry.
    pub fn add_reason(mut self, reason: ExportOutputPrecisionFallbackReason) -> Self {
        match reason {
            ExportOutputPrecisionFallbackReason::CpuRgba8BoundaryPackedToHighBitDepthPipe => {
                self.cpu_rgba8_boundary_packed_to_high_bit_depth_pipe =
                    self.cpu_rgba8_boundary_packed_to_high_bit_depth_pipe.saturating_add(1)
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
    /// An export delivery view policy was configured but failed validation/resolution.
    InvalidExportDeliveryView,
}

/// Structured final export output transform semantic issue counts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExportOutputTransformIssueBreakdown {
    /// Tone mapping was requested without an export view/display-view transform.
    pub tone_map_requested_without_export_view_transform: u64,
    /// Export delivery view policy was configured but invalid or unresolved.
    pub invalid_export_delivery_view: u64,
}

impl ExportOutputTransformIssueBreakdown {
    /// Return total issue count across all recorded reasons.
    pub fn total(&self) -> u64 {
        self.tone_map_requested_without_export_view_transform
            .saturating_add(self.invalid_export_delivery_view)
    }

    /// Merge another breakdown in place.
    pub fn accumulate(self, other: Self) -> Self {
        Self {
            tone_map_requested_without_export_view_transform: self
                .tone_map_requested_without_export_view_transform
                .saturating_add(other.tone_map_requested_without_export_view_transform),
            invalid_export_delivery_view: self
                .invalid_export_delivery_view
                .saturating_add(other.invalid_export_delivery_view),
        }
    }

    /// Map one reason into a mut accumulator entry.
    pub fn add_reason(mut self, reason: ExportOutputTransformIssueReason) -> Self {
        match reason {
            ExportOutputTransformIssueReason::ToneMapRequestedWithoutExportViewTransform => {
                self.tone_map_requested_without_export_view_transform =
                    self.tone_map_requested_without_export_view_transform.saturating_add(1)
            }
            ExportOutputTransformIssueReason::InvalidExportDeliveryView => {
                self.invalid_export_delivery_view =
                    self.invalid_export_delivery_view.saturating_add(1)
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
    /// Final export output precision fallbacks.
    pub output_precision_fallbacks: u64,
    /// Structured final export output precision fallback reasons.
    pub output_precision_fallback_reasons: ExportOutputPrecisionFallbackBreakdown,
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
    /// Whether all diagnosed composites stayed in the float/linear path.
    pub fully_float_linear: bool,
    /// Whether native GPU color scheduling was free of upload/readback and blockers.
    pub gpu_path_ready: bool,
}

/// Schema version for export color health reports.
pub const EXPORT_COLOR_HEALTH_REPORT_SCHEMA_VERSION: u32 = 1;

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
            "export_output_precision_fallbacks",
            self.output_precision_fallbacks,
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
    if summary.output_precision_fallbacks > 0
        || summary.output_precision_fallback_reasons.total() > 0
    {
        push_export_root_cause_with_action(
            root_causes,
            actions,
            ExportColorHealthArea::CompositePath,
            "export_output_precision_fallback",
            ExportColorHealthSeverity::Fail,
            format!(
                "output_precision_fallbacks={} cpu_rgba8_boundary_packed_to_high_bit_depth_pipe={}",
                summary.output_precision_fallbacks,
                summary
                    .output_precision_fallback_reasons
                    .cpu_rgba8_boundary_packed_to_high_bit_depth_pipe
            ),
            "replace_export_cpu_rgba8_output_boundary",
            "Replace high-bit-depth export CPU fallback with a renderer-owned float/high-bit output boundary.",
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
                "output_transform_issues={} tone_map_requested_without_export_view_transform={} invalid_export_delivery_view={}",
                summary.output_transform_issues,
                summary
                    .output_transform_issue_reasons
                    .tone_map_requested_without_export_view_transform,
                summary
                    .output_transform_issue_reasons
                    .invalid_export_delivery_view
            ),
            "configure_export_delivery_view",
            "Configure an export delivery view policy in project or sequence display management.",
        );
    }
    if !summary.fully_float_linear || summary.legacy_reason_total > 0 {
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
            && self.output_precision_fallbacks == 0
            && self.output_precision_fallback_reasons.total() == 0
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
            output_precision_fallbacks: self.output_precision_fallbacks,
            output_precision_fallback_reasons: self.output_precision_fallback_reasons,
            output_transform_issues: self.output_transform_issues,
            output_transform_issue_reasons: self.output_transform_issue_reasons,
            float_linear_composites: composite.float_linear_composites,
            legacy_rgba8_composites: composite.legacy_rgba8_composites,
            legacy_reason_total: composite.legacy_breakdown.total(),
            legacy_breakdown: composite.legacy_breakdown,
            fully_float_linear: composite.is_fully_float_linear(),
            gpu_path_ready: {
                if self.gpu_output_attempts == 0 && self.gpu_output_cpu_fallbacks == 0 {
                    self.output_precision_fallbacks == 0
                        && self.output_precision_fallback_reasons.total() == 0
                        && self.output_transform_issues == 0
                        && self.output_transform_issue_reasons.total() == 0
                } else {
                    self.gpu_output_cpu_fallbacks == 0
                        && self.gpu_output_fallback_reasons.total() == 0
                        && self.output_precision_fallbacks == 0
                        && self.output_precision_fallback_reasons.total() == 0
                        && self.output_transform_issues == 0
                        && self.output_transform_issue_reasons.total() == 0
                        && stages.gpu_blockers == 0
                }
            },
        })
    }
}

impl RenderJob {
    pub fn new(config: ExportConfig) -> Self {
        Self {
            id: JobId::new(),
            config,
            status: JobStatus::Pending,
            progress: 0.0,
            diagnostics: ExportJobDiagnostics::default(),
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
        }
    }
}

pub(crate) enum JobExecutionResult {
    Completed,
    Failed(String),
    Cancelled,
}

trait ExportExecutor: Send + Sync + 'static {
    fn execute(
        &self,
        job: &RenderJob,
        cancel: &AtomicBool,
        report: &mut dyn FnMut(JobStatus, f32),
        report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
    ) -> JobExecutionResult;
}

#[derive(Default)]
pub struct FfmpegExportExecutor;

impl ExportExecutor for FfmpegExportExecutor {
    fn execute(
        &self,
        job: &RenderJob,
        cancel: &AtomicBool,
        report: &mut dyn FnMut(JobStatus, f32),
        report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
    ) -> JobExecutionResult {
        if cancel.load(Ordering::Relaxed) {
            return JobExecutionResult::Cancelled;
        }

        if let Some(parent) = job.config.output_path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                return JobExecutionResult::Failed(format!(
                    "无法创建导出目录 {}: {}",
                    parent.display(),
                    err
                ));
            }
        }

        match &job.config.input {
            ExportInput::File { input_path, in_point, out_point } => execute_file_export(
                job,
                input_path,
                in_point.as_deref(),
                out_point.as_deref(),
                cancel,
                report,
            ),
            ExportInput::Timeline(timeline) => {
                execute_timeline_export(job, timeline, cancel, report, report_diagnostics)
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
        channels: u8,
    },
    Silent {
        sample_rate: u32,
        channels: u8,
    },
    Disabled,
}

#[derive(Clone)]
struct DecodedVideoLayer {
    frame: CpuColorFrame,
    stage_diagnostics: RenderColorStageDiagnostics,
}

fn execute_file_export(
    job: &RenderJob,
    input_path: &Path,
    in_point: Option<&str>,
    out_point: Option<&str>,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(JobStatus, f32),
) -> JobExecutionResult {
    if !input_path.exists() {
        return JobExecutionResult::Failed(format!("导出输入不存在：{}", input_path.display()));
    }

    let source_summary = probe_media_summary(input_path).ok();
    report(JobStatus::Encoding, 0.02);

    let duration_ms = probe_duration_ms(input_path, in_point, out_point).unwrap_or(0);

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y")
        .arg("-hide_banner")
        .arg("-progress")
        .arg("pipe:2")
        .arg("-nostats")
        .arg("-loglevel")
        .arg("error");

    if let Some(in_point) = in_point {
        cmd.arg("-ss").arg(in_point);
    }
    cmd.arg("-i").arg(input_path);
    if let Some(out_point) = out_point {
        cmd.arg("-to").arg(out_point);
    }

    if let Some(filter) = build_video_filter(&job.config) {
        cmd.arg("-vf").arg(filter);
    }

    apply_video_codec_args(&mut cmd, &job.config.preset.video);
    apply_audio_codec_args(&mut cmd, &job.config.preset.audio);
    cmd.arg("-f")
        .arg(container_format(&job.config.preset.container))
        .arg(&job.config.output_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            return JobExecutionResult::Failed(format!("无法启动 ffmpeg: {}", err));
        }
    };

    match monitor_ffmpeg_child(child, duration_ms, cancel, report) {
        JobExecutionResult::Completed => {
            let expectations = ExportValidationExpectations {
                require_video_stream: true,
                require_audio_stream: source_summary.map(|s| s.has_audio).unwrap_or(false),
                expected_video: job.config.preset.resolution.as_ref().map(|resolution| {
                    ExpectedVideoConstraints {
                        width: Some(normalize_output_dimension(resolution.width)),
                        height: Some(normalize_output_dimension(resolution.height)),
                        fps_num: None,
                        fps_den: None,
                    }
                }),
                expected_duration_secs: if duration_ms > 0 {
                    Some(duration_ms as f64 / 1000.0)
                } else {
                    None
                },
            };
            match validate_export_output(job.config.output_path.as_path(), &expectations) {
                Ok(()) => JobExecutionResult::Completed,
                Err(err) => JobExecutionResult::Failed(format!("导出结果校验失败: {err}")),
            }
        }
        other => other,
    }
}

fn execute_timeline_export(
    job: &RenderJob,
    timeline: &TimelineExportInput,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(JobStatus, f32),
    report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
) -> JobExecutionResult {
    let mut temp_audio_path_to_cleanup: Option<PathBuf> = None;
    let result = (|| {
        if cancel.load(Ordering::Relaxed) {
            return JobExecutionResult::Cancelled;
        }

        for (asset_id, path) in &timeline.asset_paths {
            if !path.exists() {
                return JobExecutionResult::Failed(format!(
                    "时间线素材离线：asset={} path={}",
                    asset_id,
                    path.display()
                ));
            }
        }
        if let Err(err) = validate_timeline_export_color_compatibility(&job.config, timeline) {
            return JobExecutionResult::Failed(err);
        }

        let range = compute_timeline_render_range(timeline);
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
        let validation_expectations = ExportValidationExpectations {
            require_video_stream: true,
            require_audio_stream: !matches!(&audio_input, TimelineAudioInput::Disabled),
            expected_video: Some(ExpectedVideoConstraints {
                width: Some(width),
                height: Some(height),
                fps_num: Some(range.fps_num),
                fps_den: Some(range.fps_den),
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
            TimelineAudioInput::PcmFile { path, sample_rate, channels } => {
                cmd.arg("-f")
                    .arg("f32le")
                    .arg("-ar")
                    .arg(sample_rate.to_string())
                    .arg("-ac")
                    .arg(channels.to_string())
                    .arg("-i")
                    .arg(path)
                    .arg("-map")
                    .arg("0:v:0")
                    .arg("-map")
                    .arg("1:a:0")
                    .arg("-shortest");
            }
            TimelineAudioInput::Silent { sample_rate, channels } => {
                let channel_layout = ffmpeg_channel_layout(*channels);
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
        apply_sequence_video_format_args(&mut cmd, &timeline.sequence.settings);
        apply_color_tag_args(
            &mut cmd,
            timeline.sequence.settings.color_management.output_color_space,
        );
        if timeline.sequence.settings.color_management.preserve_hdr_metadata {
            if let Err(err) = apply_hdr_metadata_args(&mut cmd, &timeline.sequence.settings) {
                return JobExecutionResult::Failed(err);
            }
        }
        if !matches!(&audio_input, TimelineAudioInput::Disabled) {
            apply_audio_codec_args(&mut cmd, &job.config.preset.audio);
        }
        cmd.arg("-f")
            .arg(container_format(&job.config.preset.container))
            .arg(&job.config.output_path)
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

        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return JobExecutionResult::Cancelled;
        }

        report(JobStatus::Encoding, 0.98);
        match child.wait_with_output() {
            Ok(output) if output.status.success() => {
                match validate_export_output(
                    job.config.output_path.as_path(),
                    &validation_expectations,
                ) {
                    Ok(()) => JobExecutionResult::Completed,
                    Err(err) => JobExecutionResult::Failed(format!("导出结果校验失败: {err}")),
                }
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let reason = stderr
                    .lines()
                    .rev()
                    .find(|line| !line.trim().is_empty())
                    .map(|line| line.trim().to_string())
                    .unwrap_or_else(|| format!("ffmpeg 退出码：{}", output.status));
                JobExecutionResult::Failed(format!("时间线编码失败：{reason}"))
            }
            Err(err) => JobExecutionResult::Failed(format!("等待 ffmpeg 结束失败: {}", err)),
        }
    })();

    if let Some(path) = temp_audio_path_to_cleanup {
        let _ = std::fs::remove_file(path);
    }
    result
}

fn prepare_timeline_audio_input(
    job: &RenderJob,
    timeline: &TimelineExportInput,
    range: TimelineRenderRange,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(JobStatus, f32),
) -> Result<TimelineAudioInput, JobExecutionResult> {
    if matches!(job.config.preset.container, Container::Gif) {
        return Ok(TimelineAudioInput::Disabled);
    }

    let sample_rate = timeline.sequence.settings.audio_sample_rate.max(8_000);
    let channels = timeline.sequence.settings.audio_channel_layout.channels().max(1);
    if !timeline_has_audio_content(timeline, range) {
        return Ok(TimelineAudioInput::Silent { sample_rate, channels });
    }

    let temp_path = std::env::temp_dir().join(format!(
        "mondrian-export-audio-{}-{}.f32",
        job.id,
        Utc::now().timestamp_millis()
    ));

    match render_timeline_audio_to_pcm_f32(
        temp_path.as_path(),
        timeline,
        range,
        sample_rate,
        channels,
        cancel,
        report,
    ) {
        JobExecutionResult::Completed => {
            Ok(TimelineAudioInput::PcmFile { path: temp_path, sample_rate, channels })
        }
        JobExecutionResult::Cancelled => Err(JobExecutionResult::Cancelled),
        JobExecutionResult::Failed(reason) => Err(JobExecutionResult::Failed(reason)),
    }
}

fn timeline_has_audio_content(timeline: &TimelineExportInput, range: TimelineRenderRange) -> bool {
    sequence_has_audio_content(
        timeline,
        &timeline.sequence,
        range.start_frame,
        range.start_frame.saturating_add(range.total_frames as i64),
        0,
    )
}

fn sequence_has_audio_content(
    timeline: &TimelineExportInput,
    seq: &mondrian_timeline::sequence::Sequence,
    start: i64,
    end_exclusive: i64,
    depth: usize,
) -> bool {
    if depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH {
        return false;
    }

    let has_solo = seq.audio_tracks.iter().any(|t| t.is_solo && !t.is_muted);
    for track in &seq.audio_tracks {
        if track.is_muted || (has_solo && !track.is_solo) {
            continue;
        }
        for clip in &track.clips {
            if clip.is_disabled {
                continue;
            }
            if !timeline.asset_paths.contains_key(&clip.asset_id) {
                continue;
            }

            let clip_start = clip.position.frame;
            let clip_end = clip.end_position().frame;
            if clip_end > start && clip_start < end_exclusive {
                return true;
            }
        }
    }

    for track in seq.video_tracks.iter().chain(seq.audio_tracks.iter()) {
        for clip in &track.clips {
            if clip.is_disabled || !clip.is_nested_sequence() {
                continue;
            }
            let clip_start = clip.position.frame;
            let clip_end = clip.end_position().frame;
            if clip_end <= start || clip_start >= end_exclusive {
                continue;
            }
            let Some(nested_sequence_id) = clip.nested_sequence_id else {
                continue;
            };
            let Some(nested_sequence) =
                timeline.sequences.iter().find(|sequence| sequence.id == nested_sequence_id)
            else {
                continue;
            };
            let nested_start = start.saturating_sub(clip_start).max(0);
            let nested_end = end_exclusive.saturating_sub(clip_start).max(nested_start);
            if sequence_has_audio_content(
                timeline,
                nested_sequence,
                nested_start,
                nested_end,
                depth + 1,
            ) {
                return true;
            }
        }
    }
    false
}

fn render_timeline_audio_to_pcm_f32(
    output_path: &Path,
    timeline: &TimelineExportInput,
    range: TimelineRenderRange,
    sample_rate: u32,
    channels: u8,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(JobStatus, f32),
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
    let mut writer = BufWriter::new(file);
    let cache = AudioSourceCache::new(sample_rate, channels);
    let mixer = AudioMixer::new(sample_rate, channels);

    let total_samples = timeline_total_audio_samples(range, sample_rate);
    if total_samples == 0 {
        return JobExecutionResult::Completed;
    }

    let chunk_frames_target = (sample_rate as usize / 5).clamp(1024, 16_384);
    let timeline_start_secs =
        range.start_frame.max(0) as f64 * range.fps_den as f64 / range.fps_num.max(1) as f64;

    let mut rendered_samples = 0usize;
    let mut sample_bytes = Vec::<u8>::with_capacity(chunk_frames_target * channels as usize * 4);

    while rendered_samples < total_samples {
        if cancel.load(Ordering::Relaxed) {
            return JobExecutionResult::Cancelled;
        }

        let remaining = total_samples - rendered_samples;
        let chunk_frames = remaining.min(chunk_frames_target).max(1);
        let chunk_start_secs = timeline_start_secs + rendered_samples as f64 / sample_rate as f64;
        let chunk = match render_timeline_audio_chunk(
            timeline,
            &cache,
            &mixer,
            chunk_start_secs,
            chunk_frames,
            sample_rate,
            channels,
        ) {
            Ok(buffer) => buffer,
            Err(err) => return JobExecutionResult::Failed(err),
        };

        sample_bytes.clear();
        sample_bytes.reserve(chunk.samples.len() * 4);
        for sample in &chunk.samples {
            sample_bytes.extend_from_slice(&sample.to_le_bytes());
        }
        if let Err(err) = writer.write_all(&sample_bytes) {
            return JobExecutionResult::Failed(format!("写入临时音频文件失败: {}", err));
        }

        rendered_samples += chunk_frames;
        let ratio = rendered_samples as f32 / total_samples as f32;
        let progress = (0.02 + 0.14 * ratio).clamp(0.02, 0.16);
        report(JobStatus::Encoding, progress);
    }

    if let Err(err) = writer.flush() {
        return JobExecutionResult::Failed(format!("刷新临时音频文件失败: {}", err));
    }
    JobExecutionResult::Completed
}

fn timeline_total_audio_samples(range: TimelineRenderRange, sample_rate: u32) -> usize {
    if range.total_frames == 0 || sample_rate == 0 {
        return 0;
    }
    let seconds = range.total_frames as f64 * range.fps_den as f64 / range.fps_num.max(1) as f64;
    (seconds * sample_rate as f64).round().max(0.0) as usize
}

fn render_timeline_audio_chunk(
    timeline: &TimelineExportInput,
    cache: &AudioSourceCache,
    mixer: &AudioMixer,
    window_start_secs: f64,
    chunk_frames: usize,
    sample_rate: u32,
    channels: u8,
) -> Result<AudioBuffer, String> {
    render_sequence_audio_chunk(
        timeline,
        &timeline.sequence,
        cache,
        mixer,
        window_start_secs,
        chunk_frames,
        sample_rate,
        channels,
        0,
    )
}

fn render_sequence_audio_chunk(
    timeline: &TimelineExportInput,
    seq: &mondrian_timeline::sequence::Sequence,
    cache: &AudioSourceCache,
    mixer: &AudioMixer,
    window_start_secs: f64,
    chunk_frames: usize,
    sample_rate: u32,
    channels: u8,
    depth: usize,
) -> Result<AudioBuffer, String> {
    if depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH {
        return Ok(AudioBuffer::silent(sample_rate, channels, chunk_frames));
    }

    let chunk_duration_secs = chunk_frames as f64 / sample_rate.max(1) as f64;
    let window_end_secs = window_start_secs + chunk_duration_secs;
    let has_solo = seq.audio_tracks.iter().any(|t| t.is_solo && !t.is_muted);
    let mut tracks = Vec::<AudioTrackData>::new();

    for track in &seq.audio_tracks {
        if track.is_muted || (has_solo && !track.is_solo) {
            continue;
        }

        for clip in &track.clips {
            if clip.is_disabled {
                continue;
            }

            let Some(path) = timeline.asset_paths.get(&clip.asset_id) else {
                continue;
            };
            let clip_start_secs = clip.position.to_secs();
            let clip_end_secs = clip.end_position().to_secs();
            let overlap_start = window_start_secs.max(clip_start_secs);
            let overlap_end = window_end_secs.min(clip_end_secs);
            if overlap_end <= overlap_start {
                continue;
            }

            let decoded = cache.get_or_decode(path.as_path()).map_err(|err| {
                format!(
                    "解码音频失败 asset={} path={} err={}",
                    clip.asset_id,
                    path.display(),
                    err
                )
            })?;

            let overlap_tc = TimeCode::from_secs(overlap_start, seq.settings.frame_rate);
            let source_start_secs = clip.timeline_to_source_time(overlap_tc).to_secs().max(0.0);
            let source_start_frame = (source_start_secs * sample_rate as f64).floor() as usize;
            let segment_frames =
                ((overlap_end - overlap_start) * sample_rate as f64).ceil().max(1.0) as usize;
            let segment = decoded.slice_frames(source_start_frame, segment_frames);
            if segment.samples.is_empty() {
                continue;
            }

            let place_offset = ((overlap_start - window_start_secs) * sample_rate as f64)
                .round()
                .max(0.0) as usize;
            let mut placed = AudioBuffer::silent(sample_rate, channels, chunk_frames);
            let max_place_frames = chunk_frames.saturating_sub(place_offset);
            let copy_frames = segment.frame_count().min(max_place_frames);

            let dst_channels = channels as usize;
            let src_channels = segment.channels as usize;
            for frame in 0..copy_frames {
                let dst_base = (place_offset + frame) * dst_channels;
                let src_base = frame * src_channels;
                for ch in 0..dst_channels {
                    let src_ch = ch.min(src_channels.saturating_sub(1));
                    let sample = segment.samples.get(src_base + src_ch).copied().unwrap_or(0.0);
                    placed.samples[dst_base + ch] = sample;
                }
            }

            tracks.push(AudioTrackData {
                buffer: placed,
                config: AudioTrackConfig {
                    volume: 1.0,
                    pan: 0.0,
                    is_muted: false,
                    is_solo: false,
                },
            });
        }
    }

    for track in seq.video_tracks.iter().chain(seq.audio_tracks.iter()) {
        for clip in &track.clips {
            if clip.is_disabled || !clip.is_nested_sequence() {
                continue;
            }

            let Some(nested_sequence_id) = clip.nested_sequence_id else {
                continue;
            };
            let Some(nested_sequence) =
                timeline.sequences.iter().find(|sequence| sequence.id == nested_sequence_id)
            else {
                continue;
            };

            let clip_start_secs = clip.position.to_secs();
            let clip_end_secs = clip.end_position().to_secs();
            let overlap_start = window_start_secs.max(clip_start_secs);
            let overlap_end = window_end_secs.min(clip_end_secs);
            if overlap_end <= overlap_start {
                continue;
            }

            let nested_start_secs = clip
                .timeline_to_source_time(TimeCode::from_secs(
                    overlap_start,
                    seq.settings.frame_rate,
                ))
                .to_secs()
                .max(0.0);
            let nested_frames =
                ((overlap_end - overlap_start) * sample_rate as f64).ceil().max(1.0) as usize;
            let nested_chunk = render_sequence_audio_chunk(
                timeline,
                nested_sequence,
                cache,
                mixer,
                nested_start_secs,
                nested_frames,
                sample_rate,
                channels,
                depth + 1,
            )?;
            if nested_chunk.samples.is_empty() {
                continue;
            }

            let place_offset = ((overlap_start - window_start_secs) * sample_rate as f64)
                .round()
                .max(0.0) as usize;
            let mut placed = AudioBuffer::silent(sample_rate, channels, chunk_frames);
            let max_place_frames = chunk_frames.saturating_sub(place_offset);
            let copy_frames = nested_chunk.frame_count().min(max_place_frames);
            let channel_count = channels as usize;
            for frame in 0..copy_frames {
                let dst_base = (place_offset + frame) * channel_count;
                let src_base = frame * channel_count;
                for ch in 0..channel_count {
                    placed.samples[dst_base + ch] =
                        nested_chunk.samples.get(src_base + ch).copied().unwrap_or(0.0);
                }
            }

            tracks.push(AudioTrackData {
                buffer: placed,
                config: AudioTrackConfig {
                    volume: 1.0,
                    pan: 0.0,
                    is_muted: false,
                    is_solo: false,
                },
            });
        }
    }

    if tracks.is_empty() {
        return Ok(AudioBuffer::silent(sample_rate, channels, chunk_frames));
    }

    let mut mixed = mixer.mix(&tracks);
    let mixed_frames = mixed.frame_count();
    if mixed_frames < chunk_frames {
        mixed.samples.resize(chunk_frames * channels as usize, 0.0);
    } else if mixed_frames > chunk_frames {
        mixed.samples.truncate(chunk_frames.saturating_mul(channels as usize));
    }
    Ok(mixed)
}

fn write_timeline_frames(
    stdin: ChildStdin,
    timeline: &TimelineExportInput,
    range: TimelineRenderRange,
    width: u32,
    height: u32,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(JobStatus, f32),
    report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
) -> JobExecutionResult {
    let mut writer = BufWriter::new(stdin);
    write_timeline_frames_to_writer(
        &mut writer,
        timeline,
        range,
        width,
        height,
        cancel,
        report,
        report_diagnostics,
    )
}

fn write_timeline_frames_to_writer<W: Write>(
    writer: &mut W,
    timeline: &TimelineExportInput,
    range: TimelineRenderRange,
    width: u32,
    height: u32,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(JobStatus, f32),
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
        if cancel.load(Ordering::Relaxed) {
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
        report(
            JobStatus::Rendering { frame: rendered, total_frames: total },
            progress,
        );
    }

    if let Err(err) = writer.flush() {
        return JobExecutionResult::Failed(format!("刷新编码管道失败: {}", err));
    }

    JobExecutionResult::Completed
}

/// Build the export output boundary from the resolved color context.
///
/// When `tone_map` is requested and an OCIO delivery view is available,
/// returns an [`RenderOutputColorBoundary::export_view`] boundary that
/// carries the OCIO display/view transform (which includes tone mapping).
///
/// When `tone_map` is requested but no delivery view is available, returns
/// a plain [`RenderOutputColorBoundary::export`] boundary. The caller
/// should record `ToneMapRequestedWithoutExportViewTransform` in this case.
///
/// When `tone_map` is not requested, returns a plain export boundary
/// without any view transform.
fn export_output_boundary_from_context(color_context: &ColorContext) -> RenderOutputColorBoundary {
    if color_context.tone_map {
        if let (Some(display), Some(view)) = (&color_context.ocio_display, &color_context.ocio_view)
        {
            return RenderOutputColorBoundary::export_view(
                color_context.output_color_space,
                display.clone(),
                view.clone(),
                true,
                color_context.engine.clone(),
            );
        }
    }
    RenderOutputColorBoundary::export(
        color_context.output_color_space,
        color_context.tone_map,
        color_context.engine.clone(),
    )
}

fn render_timeline_frame_into(
    timeline: &TimelineExportInput,
    timeline_frame: i64,
    width: u32,
    height: u32,
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
        .root_export_color_context(&timeline.project_color_management);

    render_sequence_frame_into(
        timeline,
        &timeline.sequence,
        timeline_frame,
        width,
        height,
        color_context,
        canvas,
        0,
        input_color_counts,
        stage_diagnostics,
        composite_diagnostics,
        export_diagnostics,
    )
}

/// Collect input color-resolution source counts for one export timeline frame.
///
/// This uses the same render-plan evaluation path as timeline export, including
/// nested sequence recursion and sequence color-context inheritance. It is the
/// export-side diagnostic counterpart to preview's per-frame source counters.
pub fn export_input_color_resolution_counts_for_frame(
    timeline: &TimelineExportInput,
    timeline_frame: i64,
) -> Result<InputColorResolutionSourceCounts, String> {
    let color_context = timeline
        .sequence
        .settings
        .root_export_color_context(&timeline.project_color_management);
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
    timeline: &TimelineExportInput,
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
    timeline: &TimelineExportInput,
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
    timeline: &TimelineExportInput,
) -> VideoColorDiagnosticIssueAggregate {
    let mut asset_ids = HashSet::new();
    collect_sequence_asset_ids(timeline, &timeline.sequence, 0, &mut asset_ids);

    let mut summary = VideoColorDiagnosticIssueAggregate::default();
    for asset_id in asset_ids {
        if let Some(diagnostic) = timeline.asset_color_diagnostics.get(&asset_id) {
            summary.observe(diagnostic);
        }
    }
    summary
}

fn collect_sequence_asset_ids(
    timeline: &TimelineExportInput,
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
    timeline: &TimelineExportInput,
    sequence: &mondrian_timeline::sequence::Sequence,
    timeline_frame: i64,
    color_context: ColorContext,
    depth: usize,
) -> Result<InputColorResolutionSourceCounts, String> {
    if depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH {
        return Err("序列嵌套层级过深，已停止统计输入色彩解析以避免循环".to_string());
    }

    let render_plan =
        evaluate_timeline_render_plan(sequence, TimelineEvaluationRequest::export(timeline_frame));
    let mut counts = InputColorResolutionSourceCounts::default();
    for element in &render_plan.elements {
        match element {
            TimelineRenderPlanElement::Media(media) => {
                let detected_color_space =
                    timeline.asset_color_spaces.get(&media.asset_id).copied();
                let asset_interpretation = timeline
                    .asset_interpretations
                    .get(&media.asset_id)
                    .copied()
                    .unwrap_or_default();
                let resolution =
                    color_context.missing_metadata_policy.resolve_asset_input_decision(
                        media.color_space_override,
                        asset_interpretation,
                        detected_color_space,
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
                let nested_frame =
                    TimeCode::from_secs(nested.source_secs, nested_sequence.settings.frame_rate)
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
    timeline: &TimelineExportInput,
    sequence: &mondrian_timeline::sequence::Sequence,
    timeline_frame: i64,
    width: u32,
    height: u32,
    color_context: ColorContext,
    canvas: &mut Vec<u8>,
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
    let required_len = frame_contract.canvas_len(width, height);
    if canvas.len() != required_len {
        canvas.resize(required_len, 0);
    }

    let render_plan =
        evaluate_timeline_render_plan(sequence, TimelineEvaluationRequest::export(timeline_frame));
    if render_plan.is_empty() {
        fill_canvas_black_opaque(canvas, frame_contract, width, height);
        return Ok(());
    }

    let mut decode_cache = (render_plan.len() > 1).then(|| {
        HashMap::<(AssetId, i64, Rational, ColorSpace), Arc<DecodedVideoLayer>>::with_capacity(
            render_plan.len(),
        )
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
        let Some(path) = timeline.asset_paths.get(&media.asset_id) else {
            continue;
        };
        let detected_color_space = timeline.asset_color_spaces.get(&media.asset_id).copied();
        let asset_interpretation =
            timeline.asset_interpretations.get(&media.asset_id).copied().unwrap_or_default();
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
        let input_color_space = input_color_resolution.color_space.ok_or_else(|| {
                let diagnostic = timeline
                    .asset_color_diagnostics
                    .get(&media.asset_id)
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
            })?;
        let cache_key = (
            media.asset_id,
            media.source_frame,
            media.source_time_base,
            input_color_space,
        );
        let decoded = if let Some(cache) = decode_cache.as_mut() {
            if let Some(hit) = cache.get(&cache_key) {
                Arc::clone(hit)
            } else {
                let decoded = decode_video_layer_scaled(
                    media.asset_id,
                    path.as_path(),
                    input_color_space,
                    color_context.working_color_space,
                    &color_context.engine,
                    color_context.tone_map,
                    media.source_secs,
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
                color_context.working_color_space,
                &color_context.engine,
                color_context.tone_map,
                media.source_secs,
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
        let nested_frame =
            TimeCode::from_secs(nested.source_secs, nested_sequence.settings.frame_rate)
                .frame
                .max(0);
        let mut nested_canvas = vec![0u8; nested_width as usize * nested_height as usize * 4];
        let nested_context =
            nested_sequence.settings.nested_render_color_context(color_context.clone());
        let nested_output_color_space = nested_context.output_color_space;
        let nested_engine = nested_context.engine.clone();
        render_sequence_frame_into(
            timeline,
            nested_sequence,
            nested_frame,
            nested_width,
            nested_height,
            nested_context,
            &mut nested_canvas,
            depth + 1,
            input_color_counts.as_deref_mut(),
            stage_diagnostics.as_deref_mut(),
            composite_diagnostics.as_deref_mut(),
            export_diagnostics.as_deref_mut(),
        )?;
        let nested_canvas =
            export_frame_contract(&nested_sequence.settings).to_rgba8_boundary(&nested_canvas);
        let nested_source = CpuEncodedColorFrame::source_rgba8(
            nested_width,
            nested_height,
            nested_output_color_space,
            nested_canvas,
        );
        let nested_input = execute_cpu_input_stage(
            &nested_source,
            &RenderInputTransform::to_working(nested_output_color_space, false, nested_engine),
        )
        .map_err(|err| format!("nested sequence input color transform failed: {err}"))?;
        if let Some(diagnostics) = stage_diagnostics.as_deref_mut() {
            diagnostics.accumulate(nested_input.stage_diagnostics);
        }
        nested_media[index] = Some(nested_input.result.frame);
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
        fill_canvas_black_opaque(canvas, frame_contract, width, height);
        return Ok(());
    }

    let mut scratch = TimelineCompositeScratch::default();
    let rendered = composite_timeline_elements_color_frame_with_diagnostics(
        width,
        height,
        &composite_elements,
        TimelineCompositeOptions::default(),
        color_context.working_color_space,
        &mut scratch,
    );
    if let Some(diagnostics) = composite_diagnostics {
        diagnostics.accumulate(rendered.diagnostics);
    }

    let mut gpu_output_fallback_reasons = ExportGpuOutputFallbackBreakdown::default();
    let mut gpu_output_attempts = 0u64;
    let mut gpu_output_cpu_fallbacks = 0u64;
    let boundary = export_output_boundary_from_context(&color_context);
    if color_context.tone_map && boundary.display_view.is_none() {
        if let Some(diagnostics) = export_diagnostics.as_deref_mut() {
            if color_context.export_delivery_view_error.is_some() {
                diagnostics.record_output_transform_issue(
                    ExportOutputTransformIssueReason::InvalidExportDeliveryView,
                );
            }
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
                    Err(_float_err) => {
                        let encoded = execute_cpu_output_boundary_rgba8(&rendered.frame, &boundary)
                            .map_err(|err| format!("final color transform failed: {err}"))?;
                        if let Some(diagnostics) = stage_diagnostics {
                            diagnostics.accumulate(encoded.stage_diagnostics);
                        }
                        if let Some(diagnostics) = export_diagnostics.as_deref_mut() {
                            diagnostics.record_output_precision_fallback(
                                ExportOutputPrecisionFallbackReason::CpuRgba8BoundaryPackedToHighBitDepthPipe,
                            );
                        }
                        frame_contract.pack_rgba8(&encoded.rgba)
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
    canvas.clear();
    canvas.extend_from_slice(&final_bytes);
    Ok(())
}

fn decode_video_layer_scaled(
    asset_id: AssetId,
    path: &Path,
    input_color_space: ColorSpace,
    working_color_space: ColorSpace,
    engine: &ColorEngine,
    tone_map: bool,
    source_secs: f64,
    width: u32,
    height: u32,
) -> Result<Arc<DecodedVideoLayer>, String> {
    let decoded =
        decode_video_frame_at_time_rgba_scaled(path, source_secs, Some(width), Some(height))
            .map_err(|err| format!("asset={} path={} err={}", asset_id, path.display(), err))?;
    let decoded_width = decoded.width;
    let decoded_height = decoded.height;
    let source = CpuEncodedColorFrame::source_rgba8_shared(
        decoded_width,
        decoded_height,
        input_color_space,
        decoded.into_shared_data(),
    );
    let execution = execute_cpu_input_stage(
        &source,
        &RenderInputTransform::to_working(working_color_space, tone_map, engine.clone()),
    )
    .map_err(|err| format!("asset={asset_id} color transform failed: {err}"))?;
    Ok(Arc::new(DecodedVideoLayer {
        frame: execution.result.frame,
        stage_diagnostics: execution.stage_diagnostics,
    }))
}

fn compute_timeline_render_range(timeline: &TimelineExportInput) -> TimelineRenderRange {
    let sequence = &timeline.sequence;
    let sequence_end_exclusive = sequence.total_duration().frame.max(1);
    let (start, requested_end_exclusive) = match timeline.range {
        TimelineExportRange::EntireSequence => (0, sequence_end_exclusive),
        TimelineExportRange::SequenceInOut => {
            let start = sequence.in_point_frame();
            (
                start,
                sequence
                    .out_point_frame()
                    .map(|frame| frame.saturating_add(1))
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
    TimelineRenderRange { start_frame: start, total_frames, fps_num, fps_den }
}

fn timeline_output_resolution(job: &RenderJob, timeline: &TimelineExportInput) -> (u32, u32) {
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

fn ffmpeg_channel_layout(channels: u8) -> &'static str {
    match channels {
        0 | 1 => "mono",
        2 => "stereo",
        6 => "5.1",
        _ => "stereo",
    }
}

fn normalize_output_dimension(value: u32) -> u32 {
    let mut dim = value.max(1);
    if dim > 1 && dim % 2 == 1 {
        dim = dim.saturating_sub(1);
    }
    dim.max(1)
}

/// 异步后台渲染队列
pub struct RenderQueue {
    jobs: Arc<Mutex<VecDeque<RenderJob>>>,
    wake: Arc<Condvar>,
    shutdown: Arc<AtomicBool>,
    cancel_flags: Arc<Mutex<HashMap<JobId, Arc<AtomicBool>>>>,
    executor: Arc<dyn ExportExecutor>,
}

impl RenderQueue {
    pub fn new() -> Arc<Self> {
        Self::new_with_executor(Arc::new(FfmpegExportExecutor))
    }

    fn new_with_executor(executor: Arc<dyn ExportExecutor>) -> Arc<Self> {
        let queue = Arc::new(Self::with_executor(executor));
        queue.spawn_worker();
        queue
    }

    fn with_executor(executor: Arc<dyn ExportExecutor>) -> Self {
        Self {
            jobs: Arc::new(Mutex::new(VecDeque::new())),
            wake: Arc::new(Condvar::new()),
            shutdown: Arc::new(AtomicBool::new(false)),
            cancel_flags: Arc::new(Mutex::new(HashMap::new())),
            executor,
        }
    }

    fn spawn_worker(&self) {
        let jobs = Arc::clone(&self.jobs);
        let wake = Arc::clone(&self.wake);
        let shutdown = Arc::clone(&self.shutdown);
        let cancel_flags = Arc::clone(&self.cancel_flags);
        let executor = Arc::clone(&self.executor);

        let result = std::thread::Builder::new().name("mondrian-export-worker".to_string()).spawn(
            move || {
                while let Some((job, cancel_flag)) =
                    take_next_pending_job(&jobs, &wake, &shutdown, &cancel_flags)
                {
                    let mut report = |status: JobStatus, progress: f32| {
                        update_job_status(&jobs, job.id, status, progress);
                    };
                    let mut report_diagnostics = |diagnostics: ExportJobDiagnostics| {
                        update_job_diagnostics(&jobs, job.id, diagnostics);
                    };
                    let outcome = executor.execute(
                        &job,
                        cancel_flag.as_ref(),
                        &mut report,
                        &mut report_diagnostics,
                    );

                    match outcome {
                        JobExecutionResult::Completed => {
                            update_job_terminal_state(&jobs, job.id, JobStatus::Completed, 1.0);
                        }
                        JobExecutionResult::Cancelled => {
                            update_job_terminal_state(&jobs, job.id, JobStatus::Cancelled, 0.0);
                        }
                        JobExecutionResult::Failed(reason) => {
                            update_job_terminal_state(
                                &jobs,
                                job.id,
                                JobStatus::Failed(reason),
                                0.0,
                            );
                        }
                    }

                    cancel_flags.lock().remove(&job.id);
                }
            },
        );
        if let Err(e) = result {
            tracing::error!("Failed to spawn export worker thread: {}", e);
        }
    }

    pub fn enqueue(&self, job: RenderJob) -> JobId {
        let id = job.id;
        self.jobs.lock().push_back(job);
        self.wake.notify_one();
        id
    }

    pub fn list_jobs(&self) -> Vec<RenderJob> {
        self.jobs.lock().iter().cloned().collect()
    }

    pub fn cancel(&self, id: JobId) {
        let mut should_wake = false;
        {
            let mut queue = self.jobs.lock();
            if let Some(job) = queue.iter_mut().find(|j| j.id == id) {
                match job.status {
                    JobStatus::Pending => {
                        job.status = JobStatus::Cancelled;
                        job.progress = 0.0;
                        job.completed_at = Some(Utc::now());
                        should_wake = true;
                    }
                    JobStatus::Rendering { .. } | JobStatus::Encoding => {
                        if let Some(flag) = self.cancel_flags.lock().get(&id).cloned() {
                            flag.store(true, Ordering::Relaxed);
                        }
                    }
                    JobStatus::Completed | JobStatus::Failed(_) | JobStatus::Cancelled => {}
                }
            }
        }
        if should_wake {
            self.wake.notify_all();
        }
    }

    pub fn clear_completed(&self) {
        let mut queue = self.jobs.lock();
        queue.retain(|job| !helpers::is_terminal(&job.status));
    }
}

impl Drop for RenderQueue {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.wake.notify_all();
    }
}

impl Default for RenderQueue {
    fn default() -> Self {
        let queue = Self::with_executor(Arc::new(FfmpegExportExecutor));
        queue.spawn_worker();
        queue
    }
}

fn take_next_pending_job(
    jobs: &Mutex<VecDeque<RenderJob>>,
    wake: &Condvar,
    shutdown: &AtomicBool,
    cancel_flags: &Mutex<HashMap<JobId, Arc<AtomicBool>>>,
) -> Option<(RenderJob, Arc<AtomicBool>)> {
    let mut queue = jobs.lock();
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return None;
        }

        if let Some(index) = queue.iter().position(|job| matches!(job.status, JobStatus::Pending)) {
            let Some(job) = queue.get_mut(index) else {
                tracing::error!("Pending job index {index} disappeared from queue");
                continue;
            };
            job.status = JobStatus::Rendering { frame: 0, total_frames: 1000 };
            job.progress = 0.0;
            job.started_at = Some(Utc::now());

            let snapshot = job.clone();
            let cancel_flag = Arc::new(AtomicBool::new(false));
            cancel_flags.lock().insert(snapshot.id, Arc::clone(&cancel_flag));
            return Some((snapshot, cancel_flag));
        }

        wake.wait(&mut queue);
    }
}

fn update_job_status(
    jobs: &Mutex<VecDeque<RenderJob>>,
    job_id: JobId,
    status: JobStatus,
    progress: f32,
) {
    let mut queue = jobs.lock();
    if let Some(job) = queue.iter_mut().find(|job| job.id == job_id) {
        if matches!(job.status, JobStatus::Cancelled) && !matches!(status, JobStatus::Cancelled) {
            return;
        }
        if is_terminal(&job.status) {
            return;
        }
        job.status = status;
        job.progress = progress.clamp(0.0, 1.0);
    }
}

fn update_job_diagnostics(
    jobs: &Mutex<VecDeque<RenderJob>>,
    job_id: JobId,
    diagnostics: ExportJobDiagnostics,
) {
    let mut queue = jobs.lock();
    if let Some(job) = queue.iter_mut().find(|job| job.id == job_id) {
        job.diagnostics = diagnostics;
    }
}

mod helpers;
pub(crate) use helpers::*;

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::timeline_data::{
        AssetColorPayload, AssetMediaInterpretation, MediaColorInterpretation,
    };
    use mondrian_core::types::{AssetId, BlendMode, TimeCode};
    use mondrian_core::{VideoContentLightMetadata, VideoMasteringDisplayMetadata};
    use mondrian_effects::{get_or_compile_scheduled_effect_graph, EffectRenderPlan};
    use mondrian_renderer::RenderOutputColorBoundaryTarget;
    use mondrian_timeline::clip::Clip;
    use mondrian_timeline::sequence::{
        InputColorResolutionSource, MissingColorMetadataPolicy, Sequence,
    };
    use mondrian_timeline::track::Track;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;

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
                ColorSpace::Rec709,
                false,
                ColorEngine::MondrianSmart,
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
            cancel: &AtomicBool,
            report: &mut dyn FnMut(JobStatus, f32),
            _report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
        ) -> JobExecutionResult {
            self.calls.fetch_add(1, Ordering::Relaxed);
            report(JobStatus::Encoding, 0.2);

            let step = 20u64;
            let mut elapsed = 0u64;
            while elapsed < self.delay_ms {
                if cancel.load(Ordering::Relaxed) {
                    return JobExecutionResult::Cancelled;
                }
                std::thread::sleep(Duration::from_millis(step));
                elapsed += step;
            }

            report(
                JobStatus::Rendering { frame: 1000, total_frames: 1000 },
                0.95,
            );
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
            _cancel: &AtomicBool,
            report: &mut dyn FnMut(JobStatus, f32),
            report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
        ) -> JobExecutionResult {
            report(JobStatus::Rendering { frame: 1, total_frames: 1 }, 0.5);
            report_diagnostics(self.diagnostics);
            JobExecutionResult::Completed
        }
    }

    fn dummy_config(output_name: &str) -> ExportConfig {
        ExportConfig {
            preset: crate::preset::ExportPreset::youtube_1080p(),
            input: ExportInput::File {
                input_path: PathBuf::from("dummy-input.mp4"),
                in_point: None,
                out_point: None,
            },
            output_path: PathBuf::from(output_name),
        }
    }

    fn timeline_input_with_output_color(output_color_space: ColorSpace) -> TimelineExportInput {
        let mut sequence = Sequence::new("color-validation");
        sequence.settings.color_management.output_color_space = output_color_space;
        TimelineExportInput {
            sequence,
            sequences: Vec::new(),
            asset_paths: HashMap::new(),
            asset_color_spaces: HashMap::new(),
            asset_interpretations: HashMap::new(),
            asset_color_diagnostics: HashMap::new(),
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
        }
    }

    fn test_color_diagnostic(
        source: mondrian_media::VideoColorSpaceSource,
        method: mondrian_media::VideoColorDetectionMethod,
        warning: Option<mondrian_media::VideoColorInterpretationWarning>,
    ) -> mondrian_media::VideoColorDiagnostic {
        let warnings = warning.into_iter().collect::<Vec<_>>();
        mondrian_media::VideoColorDiagnostic {
            detected_color_space: None,
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

        let job_id = queue.enqueue(RenderJob::new(dummy_config("out-a.mp4")));

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

        let first_id = queue.enqueue(RenderJob::new(dummy_config("out-first.mp4")));
        let second_id = queue.enqueue(RenderJob::new(dummy_config("out-second.mp4")));
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
        let mut completed = RenderJob::new(dummy_config("completed.mp4"));
        completed.status = JobStatus::Completed;
        completed.progress = 1.0;
        let mut failed = RenderJob::new(dummy_config("failed.mp4"));
        failed.status = JobStatus::Failed("disk full".to_owned());
        let mut cancelled = RenderJob::new(dummy_config("cancelled.mp4"));
        cancelled.status = JobStatus::Cancelled;

        queue.enqueue(completed);
        queue.enqueue(failed);
        queue.enqueue(cancelled);

        queue.clear_completed();

        assert!(queue.list_jobs().is_empty());
        assert_eq!(calls.load(Ordering::Relaxed), 0);
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

        let job_id = queue.enqueue(RenderJob::new(dummy_config("diagnostics.mp4")));

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
            missing_or_unsupported_cicp_tags: 1,
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
                missing_or_unsupported_cicp_tags: 1,
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

    /// Exercise the real CPU fallback render branch with float-path failure
    /// injection.
    ///
    /// This test sets `FORCE_FLOAT_BOUNDARY_FAILURE` so that the renderer's
    /// `execute_cpu_output_boundary_float` returns `Err` immediately. The
    /// export pipeline then falls back to the RGBA8 output boundary and packs
    /// into the `rgba64le` pipe contract. We verify:
    ///
    /// 1. The canvas is the correct high-bit `rgba64le` size.
    /// 2. `output_precision_fallback` is recorded (the fallback happened).
    /// 3. The health report verdict is `Fail` with the expected root cause.
    ///
    /// **Scope note:** both the float and RGBA8 paths share the same
    /// `ColorEngine` (OCIO config). We cannot make the float path fail
    /// independently of the RGBA8 path through config alone. The test hook
    /// `cpu_output_boundary_float` bypasses the engine at the
    /// wrapper level, allowing the RGBA8 path to still succeed while the
    /// float path is force-failed. This is the sanctioned injection point
    /// for long-term fallback-path testing.
    #[test]
    fn precision_fallback_path_still_records_fallback_when_float_helper_unavailable() {
        let mut seq = Sequence::new("precision-fallback-injected");
        seq.settings.color_management.export_bit_depth = ExportBitDepth::Ten;
        let tb = seq.time_base();
        seq.video_tracks[0]
            .add_clip(Clip::new_solid_color(
                AssetId::new(),
                mondrian_core::Color::from_rgba8(200, 100, 50, 255),
                TimeCode::new(0, tb),
                TimeCode::new(1, tb),
            ))
            .expect("add solid clip");
        let timeline = TimelineExportInput {
            sequence: seq,
            sequences: Vec::new(),
            asset_paths: HashMap::new(),
            asset_color_spaces: HashMap::new(),
            asset_interpretations: HashMap::new(),
            asset_color_diagnostics: HashMap::new(),
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
        };

        let mut export_diagnostics = ExportJobColorDiagnostics::default();
        let mut canvas = vec![0u8; 2 * 2 * 8];

        let _guard = FloatBoundaryFailureGuard::activate();
        render_timeline_frame_into(
            &timeline,
            0,
            2,
            2,
            &mut canvas,
            None,
            None,
            None,
            Some(&mut export_diagnostics),
        )
        .expect("render should succeed via RGBA8 fallback");

        // Canvas is high-bit rgba64le
        assert_eq!(canvas.len(), 2 * 2 * 8);

        // The precision fallback was recorded through real render code
        assert_eq!(
            export_diagnostics.output_precision_fallbacks, 1,
            "real render fallback path must record output_precision_fallback"
        );
        assert_eq!(
            export_diagnostics
                .output_precision_fallback_reasons
                .cpu_rgba8_boundary_packed_to_high_bit_depth_pipe,
            1
        );

        // Health report reflects the fallback
        let report = export_diagnostics
            .health_report("precision-fallback-real-path")
            .expect("health report");
        assert_eq!(report.verdict, ExportColorHealthVerdict::Fail);
        assert!(report.checks.iter().any(|check| {
            check.code == "export_output_precision_fallbacks"
                && check.severity == ExportColorHealthSeverity::Fail
        }));
        assert!(report
            .root_causes
            .iter()
            .any(|root| root.code == "export_output_precision_fallback"));
        assert!(report
            .actions
            .iter()
            .any(|action| action.code == "replace_export_cpu_rgba8_output_boundary"));
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
            .any(|action| action.code == "configure_export_delivery_view"));
    }

    #[test]
    fn export_output_boundary_from_context_uses_export_view_when_view_present() {
        let ctx = ColorContext {
            working_color_space: ColorSpace::Rec709,
            output_color_space: ColorSpace::Srgb,
            tone_map: true,
            workflow: mondrian_timeline::sequence::ColorWorkflow::DisplayReferred,
            nested_processing:
                mondrian_core::timeline_data::NestedColorProcessing::PreserveChildWorkingSpace,
            engine: ColorEngine::MondrianSmart,
            missing_metadata_policy:
                mondrian_timeline::sequence::MissingColorMetadataPolicy::AssumeRec709,
            display_management: mondrian_core::color_models::DisplayManagementPolicy::default(),
            ocio_display: Some("sRGB - Display".to_string()),
            ocio_view: Some("ACES 2.0 - SDR 100 nits (Rec.709)".to_string()),
            export_delivery_view_error: None,
        };

        let boundary = export_output_boundary_from_context(&ctx);
        assert_eq!(boundary.target, RenderOutputColorBoundaryTarget::Export);
        assert!(boundary.display_view.is_some());
        assert!(boundary.tone_map);
        let dv = boundary.display_view.as_ref().unwrap();
        assert_eq!(dv.display, "sRGB - Display");
        assert_eq!(dv.view, "ACES 2.0 - SDR 100 nits (Rec.709)");
    }

    #[test]
    fn export_output_boundary_from_context_plain_export_when_no_view() {
        let ctx = ColorContext {
            working_color_space: ColorSpace::Rec709,
            output_color_space: ColorSpace::Srgb,
            tone_map: true,
            workflow: mondrian_timeline::sequence::ColorWorkflow::DisplayReferred,
            nested_processing:
                mondrian_core::timeline_data::NestedColorProcessing::PreserveChildWorkingSpace,
            engine: ColorEngine::MondrianSmart,
            missing_metadata_policy:
                mondrian_timeline::sequence::MissingColorMetadataPolicy::AssumeRec709,
            display_management: mondrian_core::color_models::DisplayManagementPolicy::default(),
            ocio_display: None,
            ocio_view: None,
            export_delivery_view_error: None,
        };

        let boundary = export_output_boundary_from_context(&ctx);
        assert_eq!(boundary.target, RenderOutputColorBoundaryTarget::Export);
        assert!(boundary.display_view.is_none());
        assert!(boundary.tone_map);
    }

    #[test]
    fn export_output_boundary_from_context_plain_export_when_no_tone_map() {
        let ctx = ColorContext {
            working_color_space: ColorSpace::Rec709,
            output_color_space: ColorSpace::Rec709,
            tone_map: false,
            workflow: mondrian_timeline::sequence::ColorWorkflow::DisplayReferred,
            nested_processing:
                mondrian_core::timeline_data::NestedColorProcessing::PreserveChildWorkingSpace,
            engine: ColorEngine::MondrianSmart,
            missing_metadata_policy:
                mondrian_timeline::sequence::MissingColorMetadataPolicy::AssumeRec709,
            display_management: mondrian_core::color_models::DisplayManagementPolicy::default(),
            ocio_display: Some("sRGB - Display".to_string()),
            ocio_view: Some("ACES 2.0 - SDR 100 nits (Rec.709)".to_string()),
            export_delivery_view_error: None,
        };

        let boundary = export_output_boundary_from_context(&ctx);
        assert_eq!(boundary.target, RenderOutputColorBoundaryTarget::Export);
        assert!(boundary.display_view.is_none());
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

    /// When an explicit delivery view is configured, the real export render
    /// path produces an export_view boundary and records no transform issue.
    /// This uses a hand-built ColorContext with ocio_display/ocio_view set,
    /// since root_export_color_context intentionally clears them.
    #[test]
    fn export_real_render_with_view_records_no_transform_issue() {
        let mut seq = Sequence::new("explicit-delivery-view");
        seq.settings.color_management.export_bit_depth = ExportBitDepth::Eight;
        let tb = seq.time_base();
        seq.video_tracks[0]
            .add_clip(Clip::new_solid_color(
                AssetId::new(),
                mondrian_core::Color::from_rgba8(128, 128, 128, 255),
                TimeCode::new(0, tb),
                TimeCode::new(1, tb),
            ))
            .expect("add solid clip");
        let timeline = TimelineExportInput {
            sequence: seq,
            sequences: Vec::new(),
            asset_paths: HashMap::new(),
            asset_color_spaces: HashMap::new(),
            asset_interpretations: HashMap::new(),
            asset_color_diagnostics: HashMap::new(),
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
        };
        let ctx = ColorContext {
            working_color_space: ColorSpace::Rec709,
            output_color_space: ColorSpace::Rec709,
            tone_map: true,
            workflow: mondrian_timeline::sequence::ColorWorkflow::DisplayReferred,
            nested_processing:
                mondrian_core::timeline_data::NestedColorProcessing::PreserveChildWorkingSpace,
            engine: ColorEngine::MondrianSmart,
            missing_metadata_policy:
                mondrian_timeline::sequence::MissingColorMetadataPolicy::AssumeRec709,
            display_management: mondrian_core::color_models::DisplayManagementPolicy::default(),
            ocio_display: Some("sRGB - Display".to_string()),
            ocio_view: Some("ACES 2.0 - SDR 100 nits (Rec.709)".to_string()),
            export_delivery_view_error: None,
        };

        let boundary = export_output_boundary_from_context(&ctx);
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
            &mut canvas,
            0,
            None,
            None,
            None,
            Some(&mut diagnostics),
        )
        .expect("render with explicit delivery view");

        assert_eq!(canvas.len(), 2 * 2 * 4);
        assert_eq!(diagnostics.output_transform_issues, 0);
        assert_eq!(diagnostics.output_transform_issue_reasons.total(), 0);
    }

    #[test]
    fn export_real_render_with_invalid_delivery_view_records_invalid_transform_issue() {
        let mut seq = Sequence::new("invalid-delivery-view");
        seq.settings.color_management.export_bit_depth = ExportBitDepth::Eight;
        let tb = seq.time_base();
        seq.video_tracks[0]
            .add_clip(Clip::new_solid_color(
                AssetId::new(),
                mondrian_core::Color::from_rgba8(128, 128, 128, 255),
                TimeCode::new(0, tb),
                TimeCode::new(1, tb),
            ))
            .expect("add solid clip");
        let timeline = TimelineExportInput {
            sequence: seq,
            sequences: Vec::new(),
            asset_paths: HashMap::new(),
            asset_color_spaces: HashMap::new(),
            asset_interpretations: HashMap::new(),
            asset_color_diagnostics: HashMap::new(),
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
        };
        let ctx = ColorContext {
            working_color_space: ColorSpace::Rec709,
            output_color_space: ColorSpace::Rec709,
            tone_map: true,
            workflow: mondrian_timeline::sequence::ColorWorkflow::DisplayReferred,
            nested_processing:
                mondrian_core::timeline_data::NestedColorProcessing::PreserveChildWorkingSpace,
            engine: ColorEngine::MondrianSmart,
            missing_metadata_policy:
                mondrian_timeline::sequence::MissingColorMetadataPolicy::AssumeRec709,
            display_management: mondrian_core::color_models::DisplayManagementPolicy::default(),
            ocio_display: None,
            ocio_view: None,
            export_delivery_view_error: Some("invalid delivery view".to_string()),
        };

        let mut diagnostics = ExportJobColorDiagnostics::default();
        let mut canvas = vec![0u8; 2 * 2 * 4];
        render_sequence_frame_into(
            &timeline,
            &timeline.sequence,
            0,
            2,
            2,
            ctx,
            &mut canvas,
            0,
            None,
            None,
            None,
            Some(&mut diagnostics),
        )
        .expect("render with invalid delivery view should fail closed through diagnostics");

        assert_eq!(canvas.len(), 2 * 2 * 4);
        assert_eq!(diagnostics.output_transform_issues, 2);
        assert_eq!(
            diagnostics
                .output_transform_issue_reasons
                .tone_map_requested_without_export_view_transform,
            1
        );
        assert_eq!(
            diagnostics.output_transform_issue_reasons.invalid_export_delivery_view,
            1
        );
    }

    #[test]
    fn export_color_diagnostics_report_fails_closed_without_frame_evidence() {
        let mut diagnostics = ExportJobColorDiagnostics::default();
        diagnostics.record_asset_issue_summary(VideoColorDiagnosticIssueAggregate {
            diagnostics: 1,
            method_missing_metadata: 1,
            confidence_none: 1,
            missing_or_unsupported_cicp_tags: 1,
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
        assert!(report.root_causes.iter().any(|root| root.code == "legacy_rgba8_composite_path"));
    }

    #[test]
    fn export_color_diagnostics_summary_surfaces_asset_issues_without_frame_evidence() {
        let mut diagnostics = ExportJobColorDiagnostics::default();
        diagnostics.record_asset_issue_summary(VideoColorDiagnosticIssueAggregate {
            diagnostics: 1,
            method_missing_metadata: 1,
            confidence_none: 1,
            missing_or_unsupported_cicp_tags: 1,
            ..VideoColorDiagnosticIssueAggregate::default()
        });

        assert_eq!(
            diagnostics.summary(),
            Some(ExportJobColorDiagnosticsSummary {
                asset_issue_summary: VideoColorDiagnosticIssueAggregate {
                    diagnostics: 1,
                    method_missing_metadata: 1,
                    confidence_none: 1,
                    missing_or_unsupported_cicp_tags: 1,
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
            .add_clip(Clip::new(
                direct_id,
                TimeCode::new(0, tb),
                TimeCode::new(10, tb),
            ))
            .expect("add direct clip");
        let mut nested_track = Track::new_video("nested");
        nested_track
            .add_clip(Clip::new_nested_sequence(
                nested_sequence_id,
                TimeCode::new(0, tb),
                TimeCode::new(10, tb),
                Some("Nested".to_owned()),
            ))
            .expect("add nested clip");
        sequence.video_tracks.push(nested_track);

        let mut nested = Sequence::new("nested-issues");
        nested.id = nested_sequence_id;
        let nested_tb = nested.time_base();
        nested.video_tracks[0]
            .add_clip(Clip::new(
                nested_id,
                TimeCode::new(0, nested_tb),
                TimeCode::new(10, nested_tb),
            ))
            .expect("add nested media clip");

        let mut asset_color_diagnostics = HashMap::new();
        asset_color_diagnostics.insert(
            direct_id,
            test_color_diagnostic(
                mondrian_media::VideoColorSpaceSource::MissingMetadata,
                mondrian_media::VideoColorDetectionMethod::MissingMetadata,
                Some(mondrian_media::VideoColorInterpretationWarning::MissingOrUnsupportedCicpTags),
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

        let timeline = TimelineExportInput {
            sequence,
            sequences: vec![nested],
            asset_paths: HashMap::new(),
            asset_color_spaces: HashMap::new(),
            asset_interpretations: HashMap::new(),
            asset_color_diagnostics,
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
                missing_or_unsupported_cicp_tags: 1,
                decoder_unavailable: 1,
                ..VideoColorDiagnosticIssueAggregate::default()
            }
        );
    }

    #[test]
    fn export_color_validation_rejects_camera_log_consumer_codecs() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::AppleLog);
        timeline.sequence.settings.color_management.export_bit_depth = ExportBitDepth::Ten;
        let config = dummy_config("camera-log.mp4");

        let err = validate_timeline_export_color_compatibility(&config, &timeline)
            .expect_err("camera log should reject H.264/MP4 delivery");
        assert!(err.contains("Camera log"));
        assert!(err.contains("ProRes"));
    }

    #[test]
    fn export_color_validation_allows_camera_log_prores_intermediate() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::AppleLog);
        timeline.sequence.settings.color_management.export_bit_depth = ExportBitDepth::Ten;

        let mut config = dummy_config("camera-log.mov");
        config.preset.container = Container::Mov;
        config.preset.video = VideoCodecConfig::ProRes { variant: "4444xq".to_string() };

        validate_timeline_export_color_compatibility(&config, &timeline)
            .expect("camera log ProRes intermediate should pass");
    }

    #[test]
    fn export_color_validation_rejects_preserve_hdr_without_typed_metadata() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec2100Pq);
        timeline.sequence.settings.color_management.export_bit_depth = ExportBitDepth::Ten;
        timeline.sequence.settings.color_management.preserve_hdr_metadata = true;
        let mut config = dummy_config("hdr-missing-metadata.mp4");
        config.preset.video = VideoCodecConfig::H265 { crf: 20, bitrate_kbps: None };

        let err = validate_timeline_export_color_compatibility(&config, &timeline)
            .expect_err("preserve HDR should require typed metadata");
        assert!(err.contains("SMPTE ST 2086"));
    }

    #[test]
    fn export_color_validation_allows_preserve_hdr_with_typed_metadata() {
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec2100Pq);
        timeline.sequence.settings.color_management.export_bit_depth = ExportBitDepth::Ten;
        timeline.sequence.settings.color_management.preserve_hdr_metadata = true;
        timeline.sequence.settings.color_management.hdr_mastering_display =
            Some(VideoMasteringDisplayMetadata::rec2100_pq_1000_nit_reference());
        timeline.sequence.settings.color_management.hdr_content_light =
            Some(VideoContentLightMetadata::hdr10_1000_nit_reference());
        let mut config = dummy_config("hdr-with-metadata.mp4");
        config.preset.video = VideoCodecConfig::H265 { crf: 20, bitrate_kbps: None };

        validate_timeline_export_color_compatibility(&config, &timeline)
            .expect("typed HDR metadata should pass validation");
    }

    #[test]
    fn timeline_render_range_respects_marked_in_out() {
        let mut seq = Sequence::new("range-test");
        let tb = seq.time_base();
        let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(200, tb));
        seq.video_tracks[0].add_clip(clip).expect("add clip");
        seq.in_point_frame = Some(40);
        seq.out_point_frame = Some(99);

        let timeline = TimelineExportInput {
            sequence: seq,
            sequences: Vec::new(),
            asset_paths: HashMap::new(),
            asset_color_spaces: HashMap::new(),
            asset_interpretations: HashMap::new(),
            asset_color_diagnostics: HashMap::new(),
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
        };

        let range = compute_timeline_render_range(&timeline);
        assert_eq!(range.start_frame, 40);
        assert_eq!(range.total_frames, 60);
    }

    #[test]
    fn timeline_render_range_can_export_entire_sequence() {
        let mut seq = Sequence::new("range-entire-test");
        let tb = seq.time_base();
        let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(200, tb));
        seq.video_tracks[0].add_clip(clip).expect("add clip");
        seq.in_point_frame = Some(40);
        seq.out_point_frame = Some(99);

        let timeline = TimelineExportInput {
            sequence: seq,
            sequences: Vec::new(),
            asset_paths: HashMap::new(),
            asset_color_spaces: HashMap::new(),
            asset_interpretations: HashMap::new(),
            asset_color_diagnostics: HashMap::new(),
            range: TimelineExportRange::EntireSequence,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
        };

        let range = compute_timeline_render_range(&timeline);
        assert_eq!(range.start_frame, 0);
        assert_eq!(range.total_frames, 200);
    }

    #[test]
    fn export_input_color_resolution_counts_for_frame_tracks_media_sources() {
        let mut seq = Sequence::new("export-input-color-counts");
        seq.settings.color_space = ColorSpace::Rec2020;
        seq.settings.color_management.missing_metadata_policy =
            MissingColorMetadataPolicy::AssumeSequenceWorkingSpace;
        let tb = seq.time_base();
        let detected_id = AssetId::new();
        let override_id = AssetId::new();
        let missing_id = AssetId::new();
        let data_id = AssetId::new();

        seq.video_tracks[0]
            .add_clip(Clip::new(
                detected_id,
                TimeCode::new(0, tb),
                TimeCode::new(10, tb),
            ))
            .expect("add detected clip");
        for (name, asset_id) in [
            ("override", override_id),
            ("missing", missing_id),
            ("data", data_id),
        ] {
            let mut track = Track::new_video(name);
            track
                .add_clip(Clip::new(
                    asset_id,
                    TimeCode::new(0, tb),
                    TimeCode::new(10, tb),
                ))
                .expect("add clip");
            seq.video_tracks.push(track);
        }

        let mut timeline = TimelineExportInput {
            sequence: seq,
            sequences: Vec::new(),
            asset_paths: HashMap::new(),
            asset_color_spaces: HashMap::new(),
            asset_interpretations: HashMap::new(),
            asset_color_diagnostics: HashMap::new(),
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
        };
        timeline.asset_color_spaces.insert(detected_id, ColorSpace::Srgb);
        timeline.asset_interpretations.insert(
            override_id,
            AssetMediaInterpretation {
                color: MediaColorInterpretation::Override { color_space: ColorSpace::SLog3 },
                ..AssetMediaInterpretation::default()
            },
        );
        timeline.asset_interpretations.insert(
            data_id,
            AssetMediaInterpretation {
                payload: AssetColorPayload::NonColorData,
                ..AssetMediaInterpretation::default()
            },
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
            counts.count(InputColorResolutionSource::MissingPolicyAssumeSequenceWorkingSpace),
            1
        );
        assert_eq!(counts.count(InputColorResolutionSource::DataTexture), 1);
    }

    #[test]
    fn timeline_has_audio_content_detects_overlap() {
        let mut seq = Sequence::new("audio-range-test");
        let tb = seq.time_base();
        let asset_id = AssetId::new();
        let clip = Clip::new(asset_id, TimeCode::new(25, tb), TimeCode::new(20, tb));
        seq.audio_tracks[0].add_clip(clip).expect("add audio clip");
        seq.in_point_frame = Some(30);
        seq.out_point_frame = Some(40);

        let mut asset_paths = HashMap::new();
        asset_paths.insert(asset_id, PathBuf::from("dummy-audio.wav"));
        let timeline = TimelineExportInput {
            sequence: seq,
            sequences: Vec::new(),
            asset_paths,
            asset_color_spaces: HashMap::new(),
            asset_interpretations: HashMap::new(),
            asset_color_diagnostics: HashMap::new(),
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
        };

        let range = compute_timeline_render_range(&timeline);
        assert!(timeline_has_audio_content(&timeline, range));
    }

    #[test]
    fn timeline_total_audio_samples_matches_frame_duration() {
        let range = TimelineRenderRange {
            start_frame: 0,
            total_frames: 50,
            fps_num: 25,
            fps_den: 1,
        };
        assert_eq!(timeline_total_audio_samples(range, 48_000), 96_000);
    }

    #[test]
    fn sequence_video_format_args_follow_bit_depth_and_range() {
        let mut settings = mondrian_timeline::sequence::SequenceSettings::default();
        settings.color_management.export_bit_depth = ExportBitDepth::Ten;
        settings.color_management.video_range = VideoRange::Legal;

        let mut cmd = Command::new("ffmpeg");
        apply_sequence_video_format_args(&mut cmd, &settings);
        let args = cmd.get_args().map(|arg| arg.to_string_lossy().to_string()).collect::<Vec<_>>();

        assert!(args.windows(2).any(|pair| pair == ["-pix_fmt", "yuv420p10le"]));
        assert!(args.windows(2).any(|pair| pair == ["-color_range", "tv"]));
    }

    #[test]
    fn color_tag_args_use_export_output_color_space() {
        let mut cmd = Command::new("ffmpeg");
        apply_color_tag_args(&mut cmd, ColorSpace::Rec2100Pq);
        let args = cmd.get_args().map(|arg| arg.to_string_lossy().to_string()).collect::<Vec<_>>();

        assert!(args.windows(2).any(|pair| pair == ["-color_primaries", "bt2020"]));
        assert!(args.windows(2).any(|pair| pair == ["-color_trc", "smpte2084"]));
        assert!(args.windows(2).any(|pair| pair == ["-colorspace", "bt2020nc"]));
    }

    #[test]
    fn color_tag_args_skip_camera_log_spaces_without_standard_delivery_tags() {
        let mut cmd = Command::new("ffmpeg");
        apply_color_tag_args(&mut cmd, ColorSpace::AppleLog);
        apply_color_tag_args(&mut cmd, ColorSpace::SLog3);
        apply_color_tag_args(&mut cmd, ColorSpace::ArriLogC4);

        let args = cmd.get_args().map(|arg| arg.to_string_lossy().to_string()).collect::<Vec<_>>();
        assert!(args.is_empty());
    }

    #[test]
    fn render_timeline_frame_into_clears_canvas_when_no_layers() {
        let mut seq = Sequence::new("empty");
        seq.settings.color_management.export_bit_depth = ExportBitDepth::Eight;
        seq.in_point_frame = Some(0);
        seq.out_point_frame = Some(10);
        let timeline = TimelineExportInput {
            sequence: seq,
            sequences: Vec::new(),
            asset_paths: HashMap::new(),
            asset_color_spaces: HashMap::new(),
            asset_interpretations: HashMap::new(),
            asset_color_diagnostics: HashMap::new(),
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
        };

        let mut canvas = vec![77u8; 4 * 2 * 4];
        render_timeline_frame_into(&timeline, 0, 4, 2, &mut canvas, None, None, None, None)
            .expect("render should pass");

        for px in canvas.chunks_exact(4) {
            assert_eq!(px, &[0, 0, 0, 255]);
        }
    }

    #[test]
    fn render_timeline_frame_into_uses_rgba64le_canvas_for_high_bit_depth_no_layers() {
        let mut seq = Sequence::new("empty-high-bit-depth");
        seq.settings.color_management.export_bit_depth = ExportBitDepth::Ten;
        seq.in_point_frame = Some(0);
        seq.out_point_frame = Some(10);
        let timeline = TimelineExportInput {
            sequence: seq,
            sequences: Vec::new(),
            asset_paths: HashMap::new(),
            asset_color_spaces: HashMap::new(),
            asset_interpretations: HashMap::new(),
            asset_color_diagnostics: HashMap::new(),
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
        };

        let mut canvas = vec![77u8; 4 * 2 * 4];
        render_timeline_frame_into(&timeline, 0, 4, 2, &mut canvas, None, None, None, None)
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
            .add_clip(Clip::new_solid_color(
                AssetId::new(),
                mondrian_core::Color::from_rgba8(32, 96, 160, 255),
                TimeCode::new(0, tb),
                TimeCode::new(10, tb),
            ))
            .expect("add solid clip");
        let timeline = TimelineExportInput {
            sequence: seq,
            sequences: Vec::new(),
            asset_paths: HashMap::new(),
            asset_color_spaces: HashMap::new(),
            asset_interpretations: HashMap::new(),
            asset_color_diagnostics: HashMap::new(),
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
        };

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
            .add_clip(Clip::new(
                asset_id,
                TimeCode::new(0, tb),
                TimeCode::new(1, tb),
            ))
            .expect("add clip");

        let temp_path = std::env::temp_dir().join(format!(
            "mondrian-missing-color-{}.mov",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        std::fs::write(&temp_path, []).expect("create placeholder media path");

        let mut asset_paths = HashMap::new();
        asset_paths.insert(asset_id, temp_path.clone());
        let mut asset_color_diagnostics = HashMap::new();
        asset_color_diagnostics.insert(
            asset_id,
            mondrian_media::VideoColorDiagnostic {
                detected_color_space: None,
                interpretation: mondrian_media::DetectedColorInterpretation {
                    color_space: None,
                    confidence: mondrian_media::VideoColorInterpretationConfidence::None,
                    source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
                    method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
                    evidence: vec![mondrian_media::VideoColorInterpretationEvidence::UnsupportedCicpTags {
                        primaries: mondrian_media::VideoColorTag {
                            code: 2,
                            name: None,
                            specified: false,
                        },
                        transfer: mondrian_media::VideoColorTag {
                            code: 2,
                            name: None,
                            specified: false,
                        },
                        matrix: mondrian_media::VideoColorTag {
                            code: 2,
                            name: None,
                            specified: false,
                        },
                    }],
                    warnings: vec![
                        mondrian_media::VideoColorInterpretationWarning::MissingOrUnsupportedCicpTags,
                    ],
                    user_overridable: true,
                },
                source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
                method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
                metadata: Some(mondrian_media::VideoColorMetadata {
                    primaries: mondrian_media::VideoColorTag {
                        code: 2,
                        name: None,
                        specified: false,
                    },
                    transfer: mondrian_media::VideoColorTag {
                        code: 2,
                        name: None,
                        specified: false,
                    },
                    matrix: mondrian_media::VideoColorTag { code: 2, name: None, specified: false },
                }),
                metadata_hints: Vec::new(),
                hdr_metadata: Vec::new(),
            },
        );
        let timeline = TimelineExportInput {
            sequence: seq,
            sequences: Vec::new(),
            asset_paths,
            asset_color_spaces: HashMap::new(),
            asset_interpretations: HashMap::new(),
            asset_color_diagnostics,
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
        );

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
        );

        assert_eq!(&output[0..4], &[54, 54, 54, 255]);
        assert_eq!(&output[4..8], &[0, 255, 0, 255]);
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
    fn high_bit_depth_cpu_fallback_does_not_record_precision_fallback() {
        let mut seq = Sequence::new("high-bit-float-fallback");
        seq.settings.color_management.export_bit_depth = ExportBitDepth::Ten;
        let tb = seq.time_base();
        seq.video_tracks[0]
            .add_clip(Clip::new_solid_color(
                AssetId::new(),
                mondrian_core::Color::from_rgba8(128, 128, 128, 255),
                TimeCode::new(0, tb),
                TimeCode::new(1, tb),
            ))
            .expect("add solid clip");
        let timeline = TimelineExportInput {
            sequence: seq,
            sequences: Vec::new(),
            asset_paths: HashMap::new(),
            asset_color_spaces: HashMap::new(),
            asset_interpretations: HashMap::new(),
            asset_color_diagnostics: HashMap::new(),
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
            &mut canvas,
            None,
            None,
            None,
            Some(&mut export_diagnostics),
        )
        .expect("render high-bit should pass");

        assert_eq!(canvas.len(), 2 * 2 * 8);
        assert_eq!(
            export_diagnostics.output_precision_fallbacks, 0,
            "high-bit float path should not record precision fallback"
        );
        assert_eq!(
            export_diagnostics.output_precision_fallback_reasons.total(),
            0,
            "high-bit float path should have zero precision fallback reasons"
        );
    }

    #[test]
    fn high_bit_depth_cpu_fallback_produces_correct_rgba64le_canvas() {
        let mut seq = Sequence::new("high-bit-canvas-check");
        seq.settings.color_management.export_bit_depth = ExportBitDepth::Ten;
        let tb = seq.time_base();
        seq.video_tracks[0]
            .add_clip(Clip::new_solid_color(
                AssetId::new(),
                mondrian_core::Color::from_rgba8(200, 100, 50, 255),
                TimeCode::new(0, tb),
                TimeCode::new(1, tb),
            ))
            .expect("add solid clip");
        let timeline = TimelineExportInput {
            sequence: seq,
            sequences: Vec::new(),
            asset_paths: HashMap::new(),
            asset_color_spaces: HashMap::new(),
            asset_interpretations: HashMap::new(),
            asset_color_diagnostics: HashMap::new(),
            range: TimelineExportRange::SequenceInOut,
            project_color_management: mondrian_core::ProjectColorManagement::default(),
        };

        let mut canvas = vec![0u8; 2 * 2 * 8];
        render_timeline_frame_into(&timeline, 0, 2, 2, &mut canvas, None, None, None, None)
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
