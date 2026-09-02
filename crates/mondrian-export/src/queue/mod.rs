//! 后台渲染队列

use crate::audio_stems::{
    stem_file_name, stem_path, validate_and_write_audio_stem_manifest, AudioStemExpectation,
    AudioStemValidationContract,
};
use crate::delivery::{
    ffmpeg_audio_channel_layout, ResolvedExportArtifactEncoding, ResolvedExportDeliveryContract,
};
pub use crate::frame_contract::{ExportFrameContract, ExportFramePackingError};
use crate::image_sequence::{
    apply_ffmpeg_image_encoder_args, ffmpeg_frame_pattern, frame_file_name,
    image_sequence_frame_contract, resolve_image_sequence_encoding, validate_and_write_manifest,
    validate_image_sequence_frame_samples, write_native_tiff_float_frame,
    ImageSequenceEncoderAdapter, ImageSequenceValidationContract,
};
#[cfg(test)]
use crate::preset::TimelineExportRange;
use crate::preset::{
    AudioCodecConfig, Container, ExportAlphaMode, ExportChromaSampling, ExportConfig,
    ExportOutputPolicy, HevcProfile, ProfessionalDeliveryProfile, TimelineExportSnapshot,
    VideoCodecConfig,
};
use crate::professional_delivery::{
    build_and_validate_package, encode_linear_rec709_as_dcdm_xyz12le,
    resolve_professional_delivery, DeliverableLayout, ImfTrackMetadata, PackageAssetId,
    PackageAssetRole, PackageElementId, PackageRelativePath, ProfessionalDeliveryToolchain,
    ProfessionalPackageBuildRequest, ProfessionalPackageDocumentIds, ProfessionalPackageTrack,
};
use crate::validator::{
    delivery_bit_depth_value, expected_audio_constraints, expected_video_encoding,
    validate_export_output_cancellable, ExpectedStream, ExpectedVideoConstraints,
    ExportValidationExpectations,
};
use crate::{
    PreparedTimelineAudioOutputSnapshot, PreparedTimelineAudioSnapshot,
    PreparedTimelineVisualSnapshot,
};
use chrono::Utc;
use mondrian_audio::{
    AudioContinuityEpoch, AudioDecodedSource, AudioLoudnessAnalyzer, AudioLoudnessReport,
    AudioMediaResolver, AudioProcessingMode, AudioProgramDeliveryRuntime, AudioProgramRuntime,
    AudioRenderContract, AudioRenderRequest, AudioRuntimeResourceFootprint,
    AudioRuntimeResourceGrant, ResolvedAudioSource,
};
use mondrian_core::timeline_data::{AlphaInterpretation, TimelineClipExecutionRef};
use mondrian_core::types::{AssetId, ColorEngine, ColorSpace, FramePosition, Rational};
use mondrian_core::{
    legalize_encoded_rgba_f32, AudioChannelLayout, AudioSamplePosition, AudioSampleRate,
    AudioSampleRounding, AudioSourceComponentId, ExecutionCancellationToken, FrameRounding,
    Resolution, ResolvedPictureGeometry, SequenceId, SignalComplianceContract, SignalLegalizer,
    TimelineTime, TimelineTimeRange, WorkingColorSpace, WorkingRgbaF32Frame,
};
use mondrian_effects::{
    identity_compiled_effect_graph, EffectExecutionContinuity, EffectExecutionSessionConfig,
    EffectFrameExtent, EffectFrameTileF32, EffectTemporalSourceIdentity, PreparedTemporalFrameSet,
};
#[cfg(test)]
use mondrian_media::PreviewDecodeSessionDisposition;
use mondrian_media::{
    run_supervised_command, D3D12ResidentHevcEncoderSession, DecodedVideoRange,
    DecodedVideoRangeContract, MediaFileFingerprint, PreviewDecodeAccessMode,
    PreviewDecodeDiagnostics, PreviewDecodeOutcome, PreviewDecodeRequest,
    PreviewDecodeSessionContext, PreviewSourceColorContract, ResidentEncodeBitDepth,
    ResidentEncodeColorimetry, ResidentHevcEncoderConfig, SupervisedChild, SupervisedProcessError,
    SupervisedProcessPolicy, SupervisedStreamCapture, VideoColorDiagnosticIssueAggregate,
};
use mondrian_media::{AudioSourceCache, AudioSourceCacheConfig, AudioSourceCacheShutdownEvidence};
use mondrian_renderer::{
    color::{
        GpuColorBackendContext, GpuColorExecutionSession, GpuProgramInput, GpuProgramOutputError,
        ProgramOutputBoundary, ProgramOutputModule, ProgramOutputRole, RenderColorStageDiagnostics,
        RenderColorStageGpuBlockerBreakdown, SourceColorModule, WorkingColorModule,
    },
    color_report_vocab, composite_timeline_elements_color_frame_with_diagnostics,
    execute_prepared_visual_closure, prepare_decoded_cpu_source_frame,
    prepare_visual_frame_closure, project_affine_to_sampled_extents, project_basic_title_transform,
    BasicTitleRasterizer, ColorFrameResidency, CpuColorFrame, GpuColorFrameHandle,
    GpuColorFrameReadbackPlan, GpuColorFrameTextureFormat, GpuColorFrameWgpuResourcePool,
    GpuColorFrameWgpuResourcePoolOptions, GpuContext, GpuResidentEncoderInputLease,
    GpuVisualFrameElement, GpuVisualFrameExecutionResourceGrant, GpuVisualFrameExecutor,
    GpuVisualFrameRecord, GpuVisualFrameRequest, GpuVisualFrameSource, GpuVisualSourceLayer,
    GpuVisualTransitionInput, GpuWorkingFloatDecision, HeterogeneousCpuPrefixSource,
    HeterogeneousGpuCompletedEvidence, HeterogeneousGpuCompletedFrame,
    HeterogeneousGpuContinuationError, HeterogeneousGpuContinuationRequest,
    HeterogeneousGpuContinuationRuntime, PreparedSourceFrame, PreparedVisualChildCanvasPolicy,
    PreparedVisualExecutionAdapter, PreparedVisualExecutionError,
    PreparedVisualExecutionNodeInputs, PreparedVisualFrameClosure,
    PreparedVisualFrameClosureRequest, PreparedVisualFrameEvaluation, PreparedVisualFrameNode,
    PreparedVisualFrameNodeId, PreparedVisualMaterializationContract, PreparedVisualNestedSample,
    PreparedVisualProgram, RenderColorTransformGpuOptions, RenderGpuOutputExecutionResourceGrant,
    SourceFramePreparationIntent, TimelineAdjustmentLayer, TimelineBasicTitlePlan,
    TimelineCompositeColorPathSummary, TimelineCompositeDiagnostics,
    TimelineCompositeDomainBlockerBreakdown, TimelineCompositeElement,
    TimelineCompositeLegacyBreakdown, TimelineCompositeOptions, TimelineCompositeScratch,
    TimelineCpuCompositePrecision, TimelineCrossDissolveLayer, TimelineEffectColorRuntime,
    TimelineEvaluationRequest, TimelineFrameExecutionRequest, TimelineMediaLayer,
    TimelineMediaPlan, TimelineRenderPlanElement, TimelineSolidColorLayer,
    TimelineTemporalDemandBatch, TimelineTemporalSource, TimelineTransitionInput,
    TimelineTransitionInputPlan,
};
#[cfg(test)]
use mondrian_renderer::{
    PreparedVisualProgramCache, PreparedVisualProgramCacheConfig, RenderInputTransform,
};
use mondrian_storage::{
    DirectoryPublicationEvidence, DirectoryPublicationFailure, FilePublicationEvidence,
    FilePublicationFailure, FilePublicationMode, OwnedPublicationDirectory, OwnedPublicationFile,
};
use mondrian_timeline::sequence::{
    DeliveryBitDepth, InputColorResolutionSource, InputColorResolutionSourceCounts,
    ProgramColorContext, ResolvedInputColor, SequenceSettings, VideoRange,
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

/// Resolve the export frame contract from the admitted delivery sample depth.
fn export_frame_contract(delivery: &ResolvedExportDeliveryContract) -> ExportFrameContract {
    match delivery.artifact {
        ResolvedExportArtifactEncoding::ImageSequence { format } => {
            image_sequence_frame_contract(format)
        }
        ResolvedExportArtifactEncoding::MediaFile { .. }
        | ResolvedExportArtifactEncoding::AudioStems { .. }
        | ResolvedExportArtifactEncoding::ProfessionalDelivery { .. } => {
            ExportFrameContract::from_bit_depth(delivery.bit_depth)
        }
    }
}

/// Renderer-owned CPU float/high-bit output boundary for export.
///
/// In production this is a transparent pass-through to the renderer's
/// `execute_cpu_output_boundary_float`. Test builds support failure injection
/// via `FORCE_FLOAT_BOUNDARY_FAILURE` so integration tests can exercise the
/// high-precision fail-closed branch without mocking the color engine.
fn cpu_output_boundary_float(
    frame: &CpuColorFrame,
    boundary: &ProgramOutputBoundary,
    session: &mut mondrian_renderer::RenderCpuColorExecutionSession,
) -> Result<
    mondrian_renderer::color::ProgramOutputFloat,
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
    ProgramOutputModule::execute_cpu_float(frame, boundary, session)
}

#[cfg(test)]
thread_local! {
    /// Test-only flag that forces `cpu_output_boundary_float` to return
    /// `Err`, exercising the high-precision fail-closed branch in real render code.
    static FORCE_FLOAT_BOUNDARY_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Test-only flag that forces export GPU output scheduling to fail before runtime access.
    static FORCE_GPU_BOUNDARY_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// GPU visual execution is opt-in per test so the parallel unit suite does
    /// not compile the large compositor shader on dozens of D3D devices at once.
    static ENABLE_GPU_VISUAL_EXECUTION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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

#[cfg(test)]
struct GpuVisualExecutionGuard;

#[cfg(test)]
impl GpuVisualExecutionGuard {
    fn activate() -> Self {
        ENABLE_GPU_VISUAL_EXECUTION.with(|cell| cell.set(true));
        Self
    }
}

#[cfg(test)]
impl Drop for GpuVisualExecutionGuard {
    fn drop(&mut self) {
        ENABLE_GPU_VISUAL_EXECUTION.with(|cell| cell.set(false));
    }
}

struct ExportGpuOutputBackend {
    context: Arc<GpuContext>,
    runtime: GpuColorExecutionSession,
    visual: Option<GpuVisualFrameExecutor>,
    heterogeneous_runtime: HeterogeneousGpuContinuationRuntime,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
struct ExportGpuOutputAttemptOutcome {
    pipe_bytes: Vec<u8>,
    stage_diagnostics: RenderColorStageDiagnostics,
}

struct ExportGpuResidentOutputAttemptOutcome {
    source: GpuResidentEncoderInputLease,
    stage_diagnostics: RenderColorStageDiagnostics,
}

const EXPORT_GPU_READBACK_TIMEOUT: Duration = Duration::from_secs(30);
const EXPORT_GPU_READBACK_POLL_SLICE: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExportGpuOutputExecutionError {
    Fallback(ExportGpuOutputFallbackReason),
    Packing(ExportFramePackingError),
    Canceled,
    DeviceTimedOut,
}

impl From<ExportGpuOutputFallbackReason> for ExportGpuOutputExecutionError {
    fn from(reason: ExportGpuOutputFallbackReason) -> Self {
        Self::Fallback(reason)
    }
}

impl From<ExportFramePackingError> for ExportGpuOutputExecutionError {
    fn from(error: ExportFramePackingError) -> Self {
        Self::Packing(error)
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
    visual_active_grant: GpuVisualFrameExecutionResourceGrant,
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
            visual_active_grant: GpuVisualFrameExecutionResourceGrant::new(
                2 * 1024 * 1024 * 1024,
                128,
            ),
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
        let visual_active_grant = policy.gpu_visual_active;
        self.active_output_grant = policy.gpu_output_active;
        if self.resource_pool_options == options && self.visual_active_grant == visual_active_grant
        {
            return;
        }
        self.resource_pool_options = options;
        self.visual_active_grant = visual_active_grant;
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

    fn active_adapter_identity(
        &mut self,
    ) -> Option<crate::hardware_encoding::ActiveGraphicsAdapterIdentity> {
        self.ensure_ready().ok()?;
        match &self.state {
            ExportGpuExecutionRuntimeState::Ready { backend, .. } => Some(
                crate::hardware_encoding::ActiveGraphicsAdapterIdentity::from(
                    &backend.context.adapter.get_info(),
                ),
            ),
            ExportGpuExecutionRuntimeState::Cold
            | ExportGpuExecutionRuntimeState::Backoff { .. } => None,
        }
    }

    fn begin_visual_frame(&mut self) -> Result<(), ExportGpuOutputFallbackReason> {
        #[cfg(test)]
        if !ENABLE_GPU_VISUAL_EXECUTION.with(|cell| cell.get()) {
            return Err(ExportGpuOutputFallbackReason::ContextUnavailable);
        }
        self.ensure_ready()?;
        if let ExportGpuExecutionRuntimeState::Ready { backend, .. } = &mut self.state {
            if backend.visual.is_none() {
                backend.visual = Some(
                    GpuVisualFrameExecutor::with_resource_grant(
                        &backend.context.device,
                        self.visual_active_grant,
                    )
                    .map_err(|_| ExportGpuOutputFallbackReason::ContextUnavailable)?,
                );
            }
            if let Some(visual) = backend.visual.as_ref() {
                visual.clear_frame_resources();
            }
            backend.runtime.clear_frame_resources();
            return Ok(());
        }
        Err(ExportGpuOutputFallbackReason::ContextUnavailable)
    }

    fn record_visual_node(
        &mut self,
        request: GpuVisualFrameRequest<'_>,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<GpuVisualFrameRecord, String> {
        if cancellation.is_canceled() {
            return Err("export GPU visual execution canceled".to_owned());
        }
        self.ensure_ready()
            .map_err(|reason| format!("export GPU visual backend is unavailable: {reason:?}"))?;
        let backend = match &mut self.state {
            ExportGpuExecutionRuntimeState::Ready { backend, .. } => backend,
            ExportGpuExecutionRuntimeState::Cold
            | ExportGpuExecutionRuntimeState::Backoff { .. } => {
                return Err("export GPU visual backend is unavailable".to_owned());
            }
        };
        let mut encoder =
            backend.context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mondrian-export-gpu-visual-node"),
            });
        let visual = backend.visual.as_ref().ok_or_else(|| {
            "export GPU visual executor was not prepared before recording".to_owned()
        })?;
        let record = visual
            .record(
                &mut backend.runtime,
                &backend.context.device,
                &backend.context.queue,
                &mut encoder,
                request,
            )
            .map_err(|error| format!("export GPU visual node failed: {error}"))?;
        backend.context.queue.submit(std::iter::once(encoder.finish()));
        if cancellation.is_canceled() {
            return Err("export GPU visual execution canceled".to_owned());
        }
        Ok(record)
    }

    fn convert_gpu_working_frame(
        &mut self,
        frame: &GpuColorFrameHandle,
        target: WorkingColorSpace,
        engine: ColorEngine,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(GpuColorFrameHandle, RenderColorStageDiagnostics), String> {
        if frame.descriptor().color_space.working() == Some(target) {
            return Ok((frame.clone(), RenderColorStageDiagnostics::default()));
        }
        if cancellation.is_canceled() {
            return Err("export nested GPU working transform canceled".to_owned());
        }
        self.ensure_ready().map_err(|reason| {
            format!("export nested GPU working transform backend unavailable: {reason:?}")
        })?;
        let backend = match &mut self.state {
            ExportGpuExecutionRuntimeState::Ready { backend, .. } => backend,
            ExportGpuExecutionRuntimeState::Cold
            | ExportGpuExecutionRuntimeState::Backoff { .. } => {
                return Err("export nested GPU working transform backend unavailable".to_owned());
            }
        };
        let mut encoder =
            backend.context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mondrian-export-gpu-nested-working-transform"),
            });
        let record = WorkingColorModule::record_gpu(
            &mut backend.runtime,
            frame,
            target,
            engine,
            RenderColorTransformGpuOptions::default(),
            GpuColorBackendContext {
                device: &backend.context.device,
                queue: &backend.context.queue,
                encoder: &mut encoder,
                load_op: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            },
        )
        .map_err(|error| format!("nested GPU working-space transform failed: {error:?}"))?;
        let output = record.output().clone();
        let diagnostics = record.stage_diagnostics();
        backend.context.queue.submit(std::iter::once(encoder.finish()));
        Ok((output, diagnostics))
    }

    fn execute(
        &mut self,
        frame: &CpuColorFrame,
        boundary: &ProgramOutputBoundary,
        frame_contract: ExportFrameContract,
        legalizer: SignalLegalizer,
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
                    ExportGpuBoundaryInput::Cpu(frame),
                    boundary,
                    frame_contract,
                    legalizer,
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
        if result.as_ref().is_err_and(export_gpu_error_requires_backend_backoff) {
            self.state = ExportGpuExecutionRuntimeState::Backoff {
                attempt_generation: self.attempt_generation,
            };
        }
        result
    }

    fn execute_gpu_frame(
        &mut self,
        frame: &GpuColorFrameHandle,
        boundary: &ProgramOutputBoundary,
        frame_contract: ExportFrameContract,
        legalizer: SignalLegalizer,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<ExportGpuOutputAttemptOutcome, ExportGpuOutputExecutionError> {
        self.ensure_ready().map_err(ExportGpuOutputExecutionError::from)?;
        let result = match &mut self.state {
            ExportGpuExecutionRuntimeState::Ready { backend, .. } => {
                execute_export_gpu_output_boundary_with_backend(
                    backend,
                    ExportGpuBoundaryInput::Gpu(frame),
                    boundary,
                    frame_contract,
                    legalizer,
                    self.active_output_grant,
                    cancellation,
                )
            }
            ExportGpuExecutionRuntimeState::Cold
            | ExportGpuExecutionRuntimeState::Backoff { .. } => {
                Err(ExportGpuOutputFallbackReason::ContextUnavailable.into())
            }
        };
        if result.as_ref().is_err_and(export_gpu_error_requires_backend_backoff) {
            self.state = ExportGpuExecutionRuntimeState::Backoff {
                attempt_generation: self.attempt_generation,
            };
        }
        result
    }

    fn execute_resident(
        &mut self,
        frame: ExportGpuBoundaryInput<'_>,
        boundary: &ProgramOutputBoundary,
        boundary_texture_format: GpuColorFrameTextureFormat,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<ExportGpuResidentOutputAttemptOutcome, ExportGpuOutputExecutionError> {
        self.ensure_ready().map_err(ExportGpuOutputExecutionError::from)?;
        match &mut self.state {
            ExportGpuExecutionRuntimeState::Ready { backend, .. } => {
                let result = execute_export_gpu_output_boundary_resident_with_backend(
                    backend,
                    frame,
                    boundary,
                    boundary_texture_format,
                    self.active_output_grant,
                    cancellation,
                );
                if result.is_err() {
                    if let Some(visual) = backend.visual.as_ref() {
                        visual.clear_frame_resources();
                    }
                    backend.runtime.clear_frame_resources();
                }
                result
            }
            ExportGpuExecutionRuntimeState::Cold
            | ExportGpuExecutionRuntimeState::Backoff { .. } => {
                Err(ExportGpuOutputFallbackReason::ContextUnavailable.into())
            }
        }
    }

    #[cfg(target_os = "windows")]
    fn create_resident_encode_adapter(
        &mut self,
        contract: mondrian_renderer::D3D12ResidentEncodeAdapterContract,
    ) -> Result<mondrian_renderer::D3D12ResidentEncodeAdapter, String> {
        self.ensure_ready()
            .map_err(|reason| format!("export GPU runtime unavailable: {reason:?}"))?;
        let ExportGpuExecutionRuntimeState::Ready { backend, .. } = &self.state else {
            return Err("export GPU runtime unavailable".to_owned());
        };
        mondrian_renderer::D3D12ResidentEncodeAdapter::new(
            &backend.context.device,
            &backend.context.queue,
            contract,
        )
        .map_err(|error| error.to_string())
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

const fn export_gpu_error_requires_backend_backoff(error: &ExportGpuOutputExecutionError) -> bool {
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
        visual: None,
        context,
        runtime: GpuColorExecutionSession::with_resource_pool(resource_pool)
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

#[derive(Clone, Copy)]
enum ExportGpuBoundaryInput<'a> {
    Cpu(&'a CpuColorFrame),
    Gpu(&'a GpuColorFrameHandle),
}

fn execute_export_gpu_output_boundary_with_backend(
    backend: &mut ExportGpuOutputBackend,
    frame: ExportGpuBoundaryInput<'_>,
    boundary: &ProgramOutputBoundary,
    frame_contract: ExportFrameContract,
    legalizer: SignalLegalizer,
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

    let boundary_texture_format = if legalizer.is_active()
        && matches!(frame_contract, ExportFrameContract::EncodedRgba8Unorm)
    {
        GpuColorFrameTextureFormat::Rgba16Float
    } else {
        frame_contract.gpu_boundary_texture_format()
    };
    let gpu_options = RenderColorTransformGpuOptions {
        output_residency: ColorFrameResidency::Cpu,
        ..RenderColorTransformGpuOptions::default()
    };
    let input = match frame {
        ExportGpuBoundaryInput::Cpu(frame) => GpuProgramInput::Cpu(frame),
        ExportGpuBoundaryInput::Gpu(frame) => GpuProgramInput::Gpu(frame),
    };
    let mut record = backend
        .runtime
        .record_program_output(
            boundary,
            input,
            boundary_texture_format,
            gpu_options,
            active_grant,
            GpuColorBackendContext {
                device: &backend.context.device,
                queue: &backend.context.queue,
                encoder: &mut encoder,
                load_op: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
            },
        )
        .map_err(|error| match error {
            GpuProgramOutputError::ActiveWorkingSet => ExportGpuOutputExecutionError::Fallback(
                ExportGpuOutputFallbackReason::ActiveWorkingSetRejected,
            ),
            _ => ExportGpuOutputExecutionError::Fallback(
                ExportGpuOutputFallbackReason::RecordBoundaryFailed,
            ),
        })?;

    let submission_index = backend.context.queue.submit(std::iter::once(encoder.finish()));
    let readback_buffer =
        record.take_readback_buffer().ok_or(ExportGpuOutputExecutionError::Fallback(
            ExportGpuOutputFallbackReason::MissingReadbackBuffer,
        ))?;
    let readback_plan = match boundary_texture_format {
        GpuColorFrameTextureFormat::Rgba8Unorm => {
            GpuColorFrameReadbackPlan::encoded_rgba8(record.output().clone())
                .map_err(|_| ExportGpuOutputFallbackReason::ReadbackUnpackFailed)?
        }
        GpuColorFrameTextureFormat::Rgba16Float => {
            GpuColorFrameReadbackPlan::encoded_rgba16float(record.output().clone())
                .map_err(|_| ExportGpuOutputFallbackReason::ReadbackUnpackFailed)?
        }
        GpuColorFrameTextureFormat::Rgba32Float => {
            GpuColorFrameReadbackPlan::encoded_rgba32float(record.output().clone())
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
    let pipe_bytes = match boundary_texture_format {
        GpuColorFrameTextureFormat::Rgba8Unorm => {
            let actual = readback_plan
                .unpack_mapped_rgba8(&mapped)
                .map_err(|_| ExportGpuOutputFallbackReason::ReadbackUnpackFailed)?;
            frame_contract.pack_rgba8(actual.rgba())?
        }
        GpuColorFrameTextureFormat::Rgba16Float => {
            let mut f32_data = readback_plan
                .unpack_mapped_rgba16float(&mapped)
                .map_err(|_| ExportGpuOutputFallbackReason::ReadbackUnpackFailed)?;
            if legalizer.is_active() {
                let compliance =
                    SignalComplianceContract::normalized_rgb(boundary.output_color_space())
                        .map_err(|_| ExportGpuOutputFallbackReason::RecordBoundaryFailed)?;
                legalize_encoded_rgba_f32(
                    bytemuck::cast_slice_mut(&mut f32_data),
                    compliance,
                    legalizer,
                )
                .map_err(|_| ExportGpuOutputFallbackReason::ReadbackUnpackFailed)?;
            }
            frame_contract.pack_rgba_f32(&f32_data)?
        }
        GpuColorFrameTextureFormat::Rgba32Float => {
            let mut f32_data = readback_plan
                .unpack_mapped_rgba32float(&mapped)
                .map_err(|_| ExportGpuOutputFallbackReason::ReadbackUnpackFailed)?;
            if legalizer.is_active() {
                let compliance =
                    SignalComplianceContract::normalized_rgb(boundary.output_color_space())
                        .map_err(|_| ExportGpuOutputFallbackReason::RecordBoundaryFailed)?;
                legalize_encoded_rgba_f32(
                    bytemuck::cast_slice_mut(&mut f32_data),
                    compliance,
                    legalizer,
                )
                .map_err(|_| ExportGpuOutputFallbackReason::ReadbackUnpackFailed)?;
            }
            frame_contract.pack_rgba_f32(&f32_data)?
        }
    };
    if cancellation.is_canceled() {
        return Err(ExportGpuOutputExecutionError::Canceled);
    }
    if let Some(visual) = backend.visual.as_ref() {
        visual.clear_frame_resources();
    }
    backend.runtime.clear_frame_resources();

    Ok(ExportGpuOutputAttemptOutcome {
        pipe_bytes,
        stage_diagnostics: record.stage_diagnostics(),
    })
}

fn execute_export_gpu_output_boundary_resident_with_backend(
    backend: &mut ExportGpuOutputBackend,
    frame: ExportGpuBoundaryInput<'_>,
    boundary: &ProgramOutputBoundary,
    boundary_texture_format: GpuColorFrameTextureFormat,
    active_grant: RenderGpuOutputExecutionResourceGrant,
    cancellation: &ExecutionCancellationToken,
) -> Result<ExportGpuResidentOutputAttemptOutcome, ExportGpuOutputExecutionError> {
    if cancellation.is_canceled() {
        return Err(ExportGpuOutputExecutionError::Canceled);
    }
    let mut encoder =
        backend.context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-export-resident-output-boundary"),
        });
    let options = RenderColorTransformGpuOptions {
        output_residency: ColorFrameResidency::Gpu,
        ..RenderColorTransformGpuOptions::default()
    };
    let input = match frame {
        ExportGpuBoundaryInput::Cpu(frame) => GpuProgramInput::Cpu(frame),
        ExportGpuBoundaryInput::Gpu(frame) => GpuProgramInput::Gpu(frame),
    };
    let record = backend
        .runtime
        .record_program_output(
            boundary,
            input,
            boundary_texture_format,
            options,
            active_grant,
            GpuColorBackendContext {
                device: &backend.context.device,
                queue: &backend.context.queue,
                encoder: &mut encoder,
                load_op: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
            },
        )
        .map_err(|error| match error {
            GpuProgramOutputError::ActiveWorkingSet => ExportGpuOutputExecutionError::Fallback(
                ExportGpuOutputFallbackReason::ActiveWorkingSetRejected,
            ),
            _ => ExportGpuOutputExecutionError::Fallback(
                ExportGpuOutputFallbackReason::RecordBoundaryFailed,
            ),
        })?;
    backend.context.queue.submit(std::iter::once(encoder.finish()));
    let source = backend
        .runtime
        .take_resident_encoder_input(record.output())
        .map_err(|_| ExportGpuOutputFallbackReason::RecordBoundaryFailed)?;
    if let Some(visual) = backend.visual.as_ref() {
        visual.clear_frame_resources();
    }
    backend.runtime.clear_frame_resources();
    Ok(ExportGpuResidentOutputAttemptOutcome {
        source,
        stage_diagnostics: record.stage_diagnostics(),
    })
}

/// Diagnostics accumulated for one export job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ExportJobDiagnostics {
    /// Color-management diagnostics observed while rendering this job.
    pub color: ExportJobColorDiagnostics,
    /// Visual execution diagnostics observed by the immutable export attempt.
    pub visual: ExportJobVisualDiagnostics,
    /// Standards loudness/true-peak observation of the rendered Program audio.
    pub audio: Option<AudioLoudnessReport>,
    /// Independently verified encoded-essence reuse, when Smart Render won.
    pub smart_render: Option<ExportSmartRenderEvidence>,
    /// Byte-identical original Dynamic HDR file preservation evidence.
    pub dynamic_hdr_preservation: Option<ExportDynamicHdrPreservationEvidence>,
    /// Broadcaster-profile Program Output observation, when requested.
    pub broadcast_qc: Option<mondrian_broadcast::BroadcastQcReport>,
}

/// Dynamic metadata family proved on one byte-identical preserved source file.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExportDynamicHdrKind {
    /// Public SMPTE ST 2094-40 Application #4 syntax; not a brand-certification claim.
    St2094_40Application4,
    /// Dolby Vision metadata; licensing/qualification remains separately required.
    DolbyVision,
}

/// Bounded evidence that an original Dynamic HDR source file was preserved exactly.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExportDynamicHdrPreservationEvidence {
    /// Sole source Asset whose complete file was copied.
    pub source_asset_id: AssetId,
    /// Dynamic metadata family proved before and after the copy.
    pub kind: ExportDynamicHdrKind,
    /// Exact source artifact byte length.
    pub source_bytes: u64,
    /// SHA-256 of the frozen source artifact.
    pub source_sha256: [u8; 32],
    /// SHA-256 of the staged output artifact.
    pub output_sha256: [u8; 32],
    /// Whether source and output digests matched exactly.
    pub byte_identity_verified: bool,
    /// Whether the copied output was independently probed for the requested family.
    pub output_metadata_reprobed: bool,
}

/// Bounded proof that Smart Render reused the exact admitted source video.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExportSmartRenderEvidence {
    /// Sole source Asset whose encoded video packets were reused.
    pub source_asset_id: AssetId,
    /// Ordered packet count matched before and after remux.
    pub packet_count: u64,
    /// Encoded payload bytes matched before and after remux.
    pub payload_bytes: u64,
    /// Whether the independently captured packet digests were identical.
    pub packet_identity_verified: bool,
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
    /// Prepared visual nodes completed as GPU-resident working frames.
    pub gpu_visual_nodes_completed: u64,
    /// Non-root Sequence outputs handed to their parent without CPU readback.
    pub gpu_visual_nested_outputs: u64,
    /// Typed non-color DataTexture uploads consumed by the GPU numeric bypass.
    pub gpu_visual_data_texture_uploads: u64,
    /// Final GPU visual outputs read back exactly at the encoder-pipe boundary.
    pub gpu_visual_output_readbacks: u64,
    /// High-water logical active texture bytes admitted by the GPU Visual Module.
    pub gpu_visual_peak_active_bytes: u64,
    /// High-water active texture count admitted by the GPU Visual Module.
    pub gpu_visual_peak_active_textures: u64,
    /// Working-float selection shared by every completed GPU visual node.
    pub gpu_visual_working_float_decision: Option<GpuWorkingFloatDecision>,
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
    /// Resident D3D12 HEVC admission attempts.
    pub resident_encode_admission_attempts: u64,
    /// Resident in-process HEVC Sessions that passed exact-device qualification.
    pub resident_encode_sessions: u64,
    /// Frames submitted without a host pixel boundary.
    pub resident_encode_frames: u64,
    /// D3D12 Video Process RGB-to-YCbCr conversions.
    pub resident_encode_video_process_submissions: u64,
    /// Encoded packets written to the video-only temporary artifact.
    pub resident_encode_packets: u64,
    /// Host pixel readbacks on the resident route; must remain zero.
    pub resident_encode_cpu_pixel_readbacks: u64,
    /// Rawvideo pipe bytes on the resident route; must remain zero.
    pub resident_encode_rawvideo_pipe_bytes: u64,
    /// CPU-to-encoder pixel uploads on the resident route; must remain zero.
    pub resident_encode_cpu_pixel_uploads: u64,
    /// Final muxes that stream-copied resident video rather than decoding pixels.
    pub resident_encode_video_stream_copy_muxes: u64,
    /// Most recent pre-start resident route blocker, if rawvideo fallback won.
    pub resident_encode_blocker: Option<ExportResidentEncodeBlocker>,
}

/// Stable pre-start blocker for the narrow qualified resident HEVC route.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExportResidentEncodeBlocker {
    /// Artifact or codec is outside HEVC Main/Main10 4:2:0.
    UnsupportedCodec,
    /// Alpha preservation cannot enter a subsampled hardware surface.
    AlphaPreservation,
    /// The CPU-only legalizer has no resident lowering.
    Legalizer,
    /// The authored signal cannot be represented exactly by D3D12 Video Process.
    Signal,
    /// Static HDR metadata has no exact in-process lowering.
    StaticHdrMetadata,
    /// VBV/constrained rate control is not implemented by this Adapter.
    RateControl,
    /// GOP semantics are outside the fixed closed-GOP route.
    CodingStructure,
    /// Frozen resident-surface grant cannot admit the codec pool.
    ResourceGrant,
    /// Requested QC needs a decoded delivery-picture observation before publication.
    BroadcastQcObservationRequired,
    /// Platform, driver, exact-device conversion, or encoder Session was unavailable.
    BackendUnavailable,
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

    fn from_directory_storage(evidence: DirectoryPublicationEvidence) -> Self {
        Self { output_path: evidence.path().to_path_buf() }
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

    fn from_directory_storage(
        failure: DirectoryPublicationFailure,
        output_path: &Path,
        retained_partial_path: Option<PathBuf>,
    ) -> Self {
        match failure {
            DirectoryPublicationFailure::BeforeNamespace(error) => Self::BeforeNamespace {
                output_path: output_path.to_path_buf(),
                retained_partial_path,
                detail: format!("{error:#}"),
            },
            DirectoryPublicationFailure::DurabilityUnconfirmed { path, source } => {
                Self::DurabilityUnconfirmed { output_path: path, detail: source.to_string() }
            }
            DirectoryPublicationFailure::NamespaceIndeterminate {
                intended_path,
                retained_staging_path,
                source,
            } => Self::NamespaceIndeterminate {
                output_path: intended_path,
                retained_partial_path: retained_staging_path.or(retained_partial_path),
                detail: source.to_string(),
            },
        }
    }
}

enum ProducedArtifactValidation {
    MediaFile(Box<ExportValidationExpectations>),
    ImageSequence(ImageSequenceValidationContract),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExportExecutionOwnerEvent {
    AudioSourceStarted,
    AudioSourceClosed { all_resources_released: bool },
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
        report_owner: &mut dyn FnMut(ExportExecutionOwnerEvent),
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
        report_owner: &mut dyn FnMut(ExportExecutionOwnerEvent),
    ) -> JobExecutionResult {
        let mut audio_owner = ExportAudioSourceOwner::new(
            execution_gate.resource_policy().audio_source_cache,
            report_owner,
        );
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute_ffmpeg_export_job(
                job,
                cancel,
                execution_gate,
                report,
                report_diagnostics,
                &mut audio_owner,
            )
        }));
        let deadline = Instant::now()
            .checked_add(EXPORT_AUDIO_SOURCE_SHUTDOWN_TIMEOUT)
            .unwrap_or_else(Instant::now);
        let audio_closure = audio_owner.shutdown_until(deadline);
        match outcome {
            Ok(outcome) => finish_export_with_audio_closure(outcome, audio_closure),
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }
}

fn execute_ffmpeg_export_job(
    job: &RenderJob,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    report: &mut dyn FnMut(ExportProgress),
    report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
    audio_owner: &mut ExportAudioSourceOwner<'_>,
) -> JobExecutionResult {
    if !execution_gate.wait_at_boundary(ExportProgressPhase::Preparing, cancel) {
        return JobExecutionResult::Cancelled;
    }
    report(ExportProgress::preparing(0.01));

    let final_output = job.config.output_path.as_path();
    if job.config.preset.audio_stem_format().is_some() {
        return execute_audio_stems_export(
            job,
            final_output,
            cancel,
            execution_gate,
            report,
            report_diagnostics,
            audio_owner,
        );
    }
    if job.config.preset.image_sequence_format().is_some() {
        return execute_image_sequence_export(
            job,
            final_output,
            cancel,
            execution_gate,
            report,
            report_diagnostics,
            audio_owner,
        );
    }
    if job.config.preset.professional_delivery().is_some() {
        return execute_professional_delivery_export(
            job,
            final_output,
            cancel,
            execution_gate,
            report,
            report_diagnostics,
            audio_owner,
        );
    }
    let staging =
        match OwnedPublicationFile::create_sibling(final_output, &format!("export-{}", job.id())) {
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
    let mut validation_contract = None;
    let outcome = execute_timeline_export(
        job,
        &job.config.timeline,
        partial_output.as_path(),
        &mut validation_contract,
        cancel,
        execution_gate,
        report,
        report_diagnostics,
        audio_owner,
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
    let Some(ProducedArtifactValidation::MediaFile(validation_expectations)) = validation_contract
    else {
        return JobExecutionResult::Failed(
            "encoded media export completed without its media-file validation contract".to_owned(),
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

fn execute_professional_delivery_export(
    job: &RenderJob,
    final_output: &Path,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    report: &mut dyn FnMut(ExportProgress),
    report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
    audio_owner: &mut ExportAudioSourceOwner<'_>,
) -> JobExecutionResult {
    let Some(author_output) = job.config.preset.professional_delivery().cloned() else {
        return JobExecutionResult::Failed(
            "professional delivery executor received a non-professional preset".to_owned(),
        );
    };
    if let Err(reason) = validate_snapshot_media_revisions(&job.config.timeline) {
        return JobExecutionResult::Failed(reason);
    }
    let delivery = match crate::delivery::resolve_export_delivery(
        &job.config.preset,
        &job.config.timeline.sequence.settings,
        &job.config.timeline.color_environment,
    ) {
        Ok(delivery) => delivery,
        Err(error) => return JobExecutionResult::Failed(error.to_string()),
    };
    let contract = match resolve_professional_delivery(
        &author_output,
        delivery.resolution,
        delivery.frame_rate,
        delivery.bit_depth,
        delivery.video_range,
        delivery.chroma_sampling,
        delivery.color_target.color_space,
        job.config.timeline.sequence.settings.audio_channel_layout,
    ) {
        Ok(contract) => contract,
        Err(error) => return JobExecutionResult::Failed(error.to_string()),
    };
    if job.config.timeline.sequence.settings.field_order
        != mondrian_core::timeline_data::FieldOrder::Progressive
    {
        return JobExecutionResult::Failed(
            "qualified IMF/AS-11/DCP rows require progressive Program Output".to_owned(),
        );
    }
    if contract.layout == DeliverableLayout::ImmutableDirectory
        && job.config.output_policy != ExportOutputPolicy::CreateNew
    {
        return JobExecutionResult::Failed(
            "IMF and DCP packages support immutable CreateNew publication only".to_owned(),
        );
    }
    let range = match compute_timeline_render_range_for_delivery(&job.config.timeline, &delivery) {
        Ok(range) if range.total_frames > 0 => range,
        Ok(_) => {
            return JobExecutionResult::Failed("professional delivery range is empty".to_owned())
        }
        Err(error) => return JobExecutionResult::Failed(error),
    };
    let toolchain = match ProfessionalDeliveryToolchain::discover(author_output.profile) {
        Ok(toolchain) => toolchain,
        Err(error) => return JobExecutionResult::Failed(error.to_string()),
    };
    if let Err(error) = toolchain.qualify_for(author_output.profile) {
        return JobExecutionResult::Failed(error.to_string());
    }
    let parent = final_output.parent().unwrap_or_else(|| Path::new("."));
    let work = match tempfile::Builder::new()
        .prefix("mondrian-professional-delivery-")
        .tempdir_in(parent)
    {
        Ok(work) => work,
        Err(error) => {
            return JobExecutionResult::Failed(format!(
                "failed to allocate professional delivery work directory: {error}"
            ));
        }
    };

    let Some(prepared_visual) =
        job.config.timeline.prepared_execution().map(|execution| execution.visual())
    else {
        return JobExecutionResult::Failed(
            "professional delivery has no admitted visual execution closure".to_owned(),
        );
    };
    let resource_policy = execution_gate.resource_policy();
    let mut visual_session = match ExportVisualRenderSession::for_execution_generation(
        execution_gate.attempt_generation(),
        resource_policy,
        prepared_visual,
    ) {
        Ok(session) => session,
        Err(error) => return JobExecutionResult::Failed(error),
    };
    let export_color_context = match resolved_export_color_context(&job.config.timeline, &delivery)
    {
        Ok(context) => context,
        Err(error) => return JobExecutionResult::Failed(error),
    };
    if let Err(outcome) = preflight_timeline_visual_range_at_resolution(
        &job.config.timeline,
        range,
        Resolution {
            width: delivery.resolution.width,
            height: delivery.resolution.height,
        },
        export_color_context,
        cancel,
        execution_gate,
        &mut visual_session,
    ) {
        return outcome;
    }
    let media_diagnostics = match export_media_diagnostic_set(&job.config.timeline) {
        Ok(diagnostics) => diagnostics,
        Err(error) => return JobExecutionResult::Failed(error),
    };
    if let Err(error) = validate_timeline_dynamic_hdr_delivery(
        &job.config.timeline,
        media_diagnostics.issue_summary,
    ) {
        return JobExecutionResult::Failed(error);
    }
    let (wave_path, audio_analysis) = match render_professional_pcm24_wave(
        work.path(),
        &job.config.timeline,
        range,
        contract.audio_sample_rate,
        cancel,
        execution_gate,
        report,
        audio_owner,
    ) {
        Ok(value) => value,
        Err(outcome) => return outcome,
    };
    let mca_labels = work.path().join("stereo-mca-labels.txt");
    if let Err(error) = write_stereo_mca_labels(&mca_labels, &author_output.metadata.language) {
        return JobExecutionResult::Failed(error);
    }
    let initial_diagnostics = ExportRenderInitialDiagnostics {
        asset_issue_summary: media_diagnostics.issue_summary,
        audio_analysis: Some(audio_analysis),
    };
    let video_output = match render_professional_picture_essence(
        work.path(),
        author_output.profile,
        &job.config.timeline,
        range,
        &delivery,
        job.config.broadcast_qc.as_ref(),
        cancel,
        execution_gate,
        report,
        report_diagnostics,
        &mut visual_session,
        initial_diagnostics,
    ) {
        Ok(path) => path,
        Err(outcome) => return outcome,
    };
    if !execution_gate.wait_at_boundary(ExportProgressPhase::Encoding, cancel) {
        return JobExecutionResult::Cancelled;
    }
    report(ExportProgress::encoding(0.94));
    if !execution_gate.wait_at_boundary(ExportProgressPhase::Packaging, cancel) {
        return JobExecutionResult::Cancelled;
    }
    report(ExportProgress::packaging(0.95));

    match author_output.profile {
        ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 => {
            let staging = match OwnedPublicationDirectory::create_sibling(
                final_output,
                &format!("export-{}", job.id()),
            ) {
                Ok(staging) => staging,
                Err(error) => {
                    return JobExecutionResult::Failed(format!(
                        "failed to reserve IMF package staging directory: {error:#}"
                    ));
                }
            };
            let picture_pattern = staging.path().join("picture_{fp_uuid}.mxf");
            let mut picture_command = toolchain.imf_picture_command(
                &video_output,
                &picture_pattern,
                &author_output.metadata.title,
            );
            if let Err(outcome) =
                run_professional_tool(&mut picture_command, "IMF picture wrapping", cancel)
            {
                return outcome;
            }
            let audio_pattern = staging.path().join("audio_{fp_uuid}.mxf");
            let mut audio_command =
                toolchain.imf_audio_command(&wave_path, &mca_labels, &audio_pattern);
            if let Err(outcome) =
                run_professional_tool(&mut audio_command, "IMF audio wrapping", cancel)
            {
                return outcome;
            }
            let (picture_path, picture_id) = match find_bmx_track(staging.path(), "picture_") {
                Ok(value) => value,
                Err(error) => return JobExecutionResult::Failed(error),
            };
            let (audio_path, audio_id) = match find_bmx_track(staging.path(), "audio_") {
                Ok(value) => value,
                Err(error) => return JobExecutionResult::Failed(error),
            };
            for path in [&picture_path, &audio_path] {
                let mut command = toolchain.bmx_reimport_command(path, false);
                if let Err(outcome) =
                    run_professional_tool(&mut command, "IMF MXF reimport", cancel)
                {
                    return outcome;
                }
            }
            let picture_descriptor_xml = match extract_imf_descriptor_xml(
                &toolchain,
                &picture_path,
                &work.path().join("photon-picture"),
                "IMF picture",
                cancel,
            ) {
                Ok(xml) => xml,
                Err(outcome) => return outcome,
            };
            let audio_descriptor_xml = match extract_imf_descriptor_xml(
                &toolchain,
                &audio_path,
                &work.path().join("photon-audio"),
                "IMF audio",
                cancel,
            ) {
                Ok(xml) => xml,
                Err(outcome) => return outcome,
            };
            let picture_relative = match PackageRelativePath::new(
                picture_path.file_name().and_then(|name| name.to_str()).unwrap_or_default(),
            ) {
                Ok(path) => path,
                Err(error) => return JobExecutionResult::Failed(error.to_string()),
            };
            let audio_relative = match PackageRelativePath::new(
                audio_path.file_name().and_then(|name| name.to_str()).unwrap_or_default(),
            ) {
                Ok(path) => path,
                Err(error) => return JobExecutionResult::Failed(error.to_string()),
            };
            let Some(audio_duration) = range.total_frames.checked_mul(1_920) else {
                return JobExecutionResult::Failed(
                    "IMF audio resource duration exceeds the supported integer range".to_owned(),
                );
            };
            let request = ProfessionalPackageBuildRequest {
                profile: author_output.profile,
                metadata: author_output.metadata.clone(),
                composition_id: crate::professional_delivery::CompositionPlaylistId::new(),
                packing_list_id: crate::professional_delivery::PackingListId::new(),
                issued_at: Utc::now(),
                document_ids: ProfessionalPackageDocumentIds::new(),
                picture: ProfessionalPackageTrack {
                    id: picture_id,
                    path: picture_relative,
                    role: PackageAssetRole::PictureTrack,
                    imf: Some(ImfTrackMetadata {
                        essence_descriptor_id: PackageElementId::new(),
                        essence_descriptor_xml: picture_descriptor_xml,
                        edit_rate: Rational::FPS_25,
                        intrinsic_duration: range.total_frames,
                        source_duration: range.total_frames,
                    }),
                },
                audio: Some(ProfessionalPackageTrack {
                    id: audio_id,
                    path: audio_relative,
                    role: PackageAssetRole::AudioTrack,
                    imf: Some(ImfTrackMetadata {
                        essence_descriptor_id: PackageElementId::new(),
                        essence_descriptor_xml: audio_descriptor_xml,
                        edit_rate: Rational::new(48_000, 1),
                        intrinsic_duration: audio_duration,
                        source_duration: audio_duration,
                    }),
                }),
                edit_rate: contract.edit_rate,
                duration: range.total_frames,
            };
            if let Err(error) = build_and_validate_package(staging.path(), &request) {
                return JobExecutionResult::Failed(format!(
                    "IMF package validation failed: {error}"
                ));
            }
            let mut photon = toolchain.photon_imp_validation_command(staging.path());
            let photon_output = match run_professional_tool_capture(
                &mut photon,
                "IMF Photon package validation",
                cancel,
            ) {
                Ok(output) => output,
                Err(outcome) => return outcome,
            };
            if photon_output.contains("FATAL") || photon_output.contains("ERROR") {
                return JobExecutionResult::Failed(
                    "IMF Photon package validation reported an error".to_owned(),
                );
            }
            if !execution_gate.wait_at_boundary(ExportProgressPhase::Validating, cancel) {
                return JobExecutionResult::Cancelled;
            }
            report(ExportProgress::validating(0.975));
            publish_professional_directory(staging, final_output, cancel, execution_gate, report)
        }
        ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => {
            let staging = match OwnedPublicationFile::create_sibling(
                final_output,
                &format!("export-{}", job.id()),
            ) {
                Ok(staging) => staging,
                Err(error) => {
                    return JobExecutionResult::Failed(format!(
                        "failed to reserve AS-11 staging file: {error:#}"
                    ));
                }
            };
            let partial_output = staging.path().to_path_buf();
            let reservation = staging.release_for_external_writer();
            let mut command = toolchain.as11_x9_command(
                &video_output,
                &wave_path,
                &mca_labels,
                &partial_output,
                &author_output.metadata.title,
            );
            if let Err(outcome) = run_professional_tool(&mut command, "AS-11 X9 wrapping", cancel) {
                return outcome;
            }
            let mut inspect = toolchain.bmx_reimport_command(&partial_output, true);
            let inspection =
                match run_professional_tool_capture(&mut inspect, "AS-11 X9 reimport", cancel) {
                    Ok(output) => output,
                    Err(outcome) => return outcome,
                };
            for required in [
                "op_label        : OP1A",
                "edit_rate       : 60000/1001",
                "essence_type    : AVC_High_422",
                "component_depth : 10",
                "channel_count        : 2",
                "bits_per_sample      : 24",
                "spec_identifier : urn:smpte:ul:060e2b34.04010101.0d010801.05090000",
                "is_complete     : true",
                "last_frame      : true",
            ] {
                if !inspection.contains(required) {
                    return JobExecutionResult::Failed(format!(
                        "AS-11 X9 reimport evidence is missing {required:?}"
                    ));
                }
            }
            if !execution_gate.wait_at_boundary(ExportProgressPhase::Validating, cancel) {
                return JobExecutionResult::Cancelled;
            }
            report(ExportProgress::validating(0.98));
            if let Err(reason) = validate_snapshot_media_revisions(&job.config.timeline) {
                return JobExecutionResult::Failed(reason);
            }
            let staging = match reservation.reclaim() {
                Ok(staging) => staging,
                Err(error) => {
                    return JobExecutionResult::Failed(format!(
                        "AS-11 staging object identity changed: {error:#}"
                    ));
                }
            };
            if !execution_gate.wait_at_boundary(ExportProgressPhase::Publishing, cancel) {
                return JobExecutionResult::Cancelled;
            }
            report(ExportProgress::publishing(0.995));
            match finalize_export_output(staging, final_output, job.config.output_policy) {
                Ok(evidence) => JobExecutionResult::Published(evidence),
                Err(failure) => JobExecutionResult::PublicationFailed(failure),
            }
        }
        ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => {
            let staging = match OwnedPublicationDirectory::create_sibling(
                final_output,
                &format!("export-{}", job.id()),
            ) {
                Ok(staging) => staging,
                Err(error) => {
                    return JobExecutionResult::Failed(format!(
                        "failed to reserve DCP package staging directory: {error:#}"
                    ));
                }
            };
            let picture_id = PackageAssetId::new();
            let audio_id = PackageAssetId::new();
            let picture_path = staging.path().join("picture.mxf");
            let audio_path = staging.path().join("audio.mxf");
            let mut picture_command = toolchain.dcp_picture_command(
                &video_output,
                &picture_path,
                picture_id,
                range.total_frames,
            );
            if let Err(outcome) =
                run_professional_tool(&mut picture_command, "DCP picture wrapping", cancel)
            {
                return outcome;
            }
            let mut audio_command = toolchain.dcp_audio_command(
                &wave_path,
                &audio_path,
                audio_id,
                range.total_frames,
                &author_output.metadata.language,
            );
            if let Err(outcome) =
                run_professional_tool(&mut audio_command, "DCP audio wrapping", cancel)
            {
                return outcome;
            }
            for path in [&picture_path, &audio_path] {
                let mut inspect = toolchain.asdcp_reimport_command(path);
                if let Err(outcome) =
                    run_professional_tool(&mut inspect, "DCP AS-DCP reimport", cancel)
                {
                    return outcome;
                }
            }
            let picture_relative = match PackageRelativePath::new("picture.mxf") {
                Ok(path) => path,
                Err(error) => return JobExecutionResult::Failed(error.to_string()),
            };
            let audio_relative = match PackageRelativePath::new("audio.mxf") {
                Ok(path) => path,
                Err(error) => return JobExecutionResult::Failed(error.to_string()),
            };
            let request = ProfessionalPackageBuildRequest {
                profile: author_output.profile,
                metadata: author_output.metadata,
                composition_id: crate::professional_delivery::CompositionPlaylistId::new(),
                packing_list_id: crate::professional_delivery::PackingListId::new(),
                issued_at: Utc::now(),
                document_ids: ProfessionalPackageDocumentIds::new(),
                picture: ProfessionalPackageTrack {
                    id: picture_id,
                    path: picture_relative,
                    role: PackageAssetRole::PictureTrack,
                    imf: None,
                },
                audio: Some(ProfessionalPackageTrack {
                    id: audio_id,
                    path: audio_relative,
                    role: PackageAssetRole::AudioTrack,
                    imf: None,
                }),
                edit_rate: contract.edit_rate,
                duration: range.total_frames,
            };
            if let Err(error) = build_and_validate_package(staging.path(), &request) {
                return JobExecutionResult::Failed(format!(
                    "DCP package validation failed: {error}"
                ));
            }
            if !execution_gate.wait_at_boundary(ExportProgressPhase::Validating, cancel) {
                return JobExecutionResult::Cancelled;
            }
            report(ExportProgress::validating(0.975));
            let mut verify = toolchain.dcp_package_validation_command(staging.path());
            if let Err(outcome) = run_dcp_package_validator(&mut verify, cancel) {
                return outcome;
            }
            publish_professional_directory(staging, final_output, cancel, execution_gate, report)
        }
    }
}

fn render_professional_pcm24_wave(
    work: &Path,
    timeline: &TimelineExportSnapshot,
    range: TimelineRenderRange,
    sample_rate: u32,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    report: &mut dyn FnMut(ExportProgress),
    audio_owner: &mut ExportAudioSourceOwner<'_>,
) -> Result<(PathBuf, AudioLoudnessReport), JobExecutionResult> {
    let raw_path = work.join("primary-audio.f32le");
    let wave_path = work.join("primary-audio.wav");
    let prepared_audio = timeline
        .prepared_execution()
        .and_then(|execution| execution.audio())
        .ok_or_else(|| {
            JobExecutionResult::Failed(
                "professional delivery has no admitted primary Program Output audio closure"
                    .to_owned(),
            )
        })?;
    let (_, sample_frames) =
        timeline_audio_sample_range(range, sample_rate).map_err(JobExecutionResult::Failed)?;
    let analysis = if prepared_audio.execution_demand().requires_execution() {
        let mut analysis = None;
        match render_timeline_audio_to_pcm_f32(
            &raw_path,
            timeline,
            prepared_audio.primary_output(),
            range,
            sample_rate,
            AudioChannelLayout::Stereo,
            audio_owner,
            cancel,
            execution_gate,
            report,
            &mut analysis,
        ) {
            JobExecutionResult::ReversibleWorkCompleted => analysis.ok_or_else(|| {
                JobExecutionResult::Failed(
                    "professional delivery audio completed without analysis evidence".to_owned(),
                )
            })?,
            other => return Err(other),
        }
    } else {
        write_silent_pcm_f32(&raw_path, sample_frames, 2).map_err(JobExecutionResult::Failed)?;
        AudioLoudnessReport::digital_silence(u64::try_from(sample_frames).map_err(|_| {
            JobExecutionResult::Failed(
                "professional delivery audio duration exceeds evidence capacity".to_owned(),
            )
        })?)
    };
    let mut command = mondrian_media::ffmpeg_command();
    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-y")
        .arg("-f")
        .arg("f32le")
        .arg("-ar")
        .arg(sample_rate.to_string())
        .arg("-ac")
        .arg("2")
        .arg("-i")
        .arg(&raw_path)
        .arg("-map")
        .arg("0:a:0")
        .arg("-c:a")
        .arg("pcm_s24le")
        .arg("-f")
        .arg("wav")
        .arg(&wave_path);
    run_professional_tool(&mut command, "professional PCM24 WAV encoding", cancel)?;
    Ok((wave_path, analysis))
}

fn write_silent_pcm_f32(path: &Path, frames: usize, channels: usize) -> Result<(), String> {
    let file = std::fs::File::create(path)
        .map_err(|error| format!("failed to create silent PCM staging file: {error}"))?;
    let mut writer = BufWriter::new(file);
    let chunk = vec![0_u8; 16 * 1024 * channels * std::mem::size_of::<f32>()];
    let mut bytes_remaining = frames
        .checked_mul(channels)
        .and_then(|samples| samples.checked_mul(std::mem::size_of::<f32>()))
        .ok_or_else(|| "silent PCM staging byte count overflow".to_owned())?;
    while bytes_remaining > 0 {
        let count = bytes_remaining.min(chunk.len());
        writer
            .write_all(&chunk[..count])
            .map_err(|error| format!("failed to write silent PCM staging file: {error}"))?;
        bytes_remaining -= count;
    }
    writer
        .flush()
        .map_err(|error| format!("failed to flush silent PCM staging file: {error}"))
}

#[allow(clippy::too_many_arguments)]
fn render_professional_picture_essence(
    work: &Path,
    profile: ProfessionalDeliveryProfile,
    timeline: &TimelineExportSnapshot,
    range: TimelineRenderRange,
    delivery: &ResolvedExportDeliveryContract,
    broadcast_qc_profile: Option<&mondrian_broadcast::BroadcastQcProfile>,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    report: &mut dyn FnMut(ExportProgress),
    report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
    visual_session: &mut ExportVisualRenderSession,
    initial_diagnostics: ExportRenderInitialDiagnostics,
) -> Result<PathBuf, JobExecutionResult> {
    let (width, height) = (delivery.resolution.width, delivery.resolution.height);
    let input_pixel_format = match profile {
        ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => "xyz12le",
        ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25
        | ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => {
            export_frame_contract(delivery).ffmpeg_pix_fmt()
        }
    };
    let output = match profile {
        ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 => work.join("picture.prores"),
        ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => work.join("picture.h264"),
        ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => {
            let directory = work.join("j2c");
            std::fs::create_dir(&directory).map_err(|error| {
                JobExecutionResult::Failed(format!("failed to create DCP J2C staging: {error}"))
            })?;
            directory
        }
    };
    let mut command = mondrian_media::ffmpeg_command();
    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-y")
        .arg("-f")
        .arg("rawvideo")
        .arg("-pix_fmt")
        .arg(input_pixel_format)
        .arg("-s:v")
        .arg(format!("{width}x{height}"))
        .arg("-r")
        .arg(format!("{}/{}", range.fps_num, range.fps_den))
        .arg("-i")
        .arg("pipe:0")
        .arg("-an");
    match profile {
        ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 => {
            command
                .arg("-vf")
                .arg("format=yuv422p10le")
                .arg("-c:v")
                .arg("prores_ks")
                .arg("-profile:v")
                .arg("3")
                .arg("-vendor")
                .arg("apl0")
                .arg("-f")
                .arg("rawvideo")
                .arg(&output);
        }
        ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => {
            command
                .arg("-vf")
                .arg("format=yuv422p10le")
                .arg("-c:v")
                .arg("libx264")
                .arg("-profile:v")
                .arg("high422")
                .arg("-level:v")
                .arg("4.1")
                .arg("-g")
                .arg("1")
                .arg("-keyint_min")
                .arg("1")
                .arg("-sc_threshold")
                .arg("0")
                .arg("-bf")
                .arg("0")
                .arg("-f")
                .arg("h264")
                .arg(&output);
        }
        ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => {
            command
                .arg("-c:v")
                .arg("libopenjpeg")
                .arg("-format")
                .arg("j2k")
                .arg("-profile:v")
                .arg("cinema2k")
                .arg("-cinema_mode")
                .arg("2k_24")
                .arg("-pix_fmt")
                .arg("xyz12le")
                .arg("-start_number")
                .arg("0")
                .arg("-frames:v")
                .arg(range.total_frames.to_string())
                .arg("-f")
                .arg("image2")
                .arg(output.join("frame_%06d.j2c"));
        }
    }
    let policy = SupervisedProcessPolicy {
        pipe_stdin: true,
        stdout: SupervisedStreamCapture::Drain,
        stderr: SupervisedStreamCapture::Tail { limit_bytes: 64 * 1024 },
        deadline: None,
        ..SupervisedProcessPolicy::default()
    };
    let mut child = SupervisedChild::spawn(&mut command, policy).map_err(|error| {
        process_supervision_failure("start professional picture encoder", error)
    })?;
    let observations = ExportRenderObservations { broadcast_qc_profile, initial_diagnostics };
    let render_outcome = if profile == ProfessionalDeliveryProfile::SmpteDcp2kFlat24 {
        let frame_contract = export_frame_contract(delivery);
        render_timeline_frames_with_sink(
            timeline,
            range,
            width,
            height,
            ExportAlphaMode::FlattenBlack,
            delivery,
            cancel,
            execution_gate,
            report,
            report_diagnostics,
            visual_session,
            observations,
            &mut |_index, canvas| {
                let rgba = frame_contract
                    .to_rgba_f32(canvas)
                    .map_err(|error| JobExecutionResult::Failed(error.to_string()))?;
                let xyz = encode_linear_rec709_as_dcdm_xyz12le(&rgba)
                    .map_err(|error| JobExecutionResult::Failed(error.to_string()))?;
                child.write_owned(xyz, cancel).map(|_| ()).map_err(|error| {
                    process_supervision_failure("write DCDM XYZ picture pipe", error)
                })
            },
        )
    } else {
        write_timeline_frames(
            &mut child,
            timeline,
            range,
            width,
            height,
            ExportAlphaMode::FlattenBlack,
            delivery,
            cancel,
            execution_gate,
            report,
            report_diagnostics,
            visual_session,
            observations,
        )
    };
    if !matches!(render_outcome, JobExecutionResult::ReversibleWorkCompleted) {
        return Err(render_outcome);
    }
    let process = child.finish(cancel).map_err(|error| {
        process_supervision_failure("finish professional picture encoder", error)
    })?;
    if !process.status.success() {
        return Err(JobExecutionResult::Failed(format!(
            "professional picture encoder failed: {}",
            bounded_process_reason(&process)
        )));
    }
    Ok(output)
}

fn write_stereo_mca_labels(path: &Path, language: &str) -> Result<(), String> {
    let body = format!(
        "0\nchL, chan=0\nchR, chan=1\nsgST, lang={language}, mcaaudiocontentkind=PRM, mcaaudioelementkind=FCMP, mcatitle=Mondrian, mcatitleversion=1\n"
    );
    std::fs::write(path, body)
        .map_err(|error| format!("failed to write stereo MCA label contract: {error}"))
}

fn run_professional_tool(
    command: &mut Command,
    operation: &str,
    cancel: &ExecutionCancellationToken,
) -> Result<(), JobExecutionResult> {
    run_professional_tool_capture(command, operation, cancel).map(|_| ())
}

fn run_professional_tool_capture(
    command: &mut Command,
    operation: &str,
    cancel: &ExecutionCancellationToken,
) -> Result<String, JobExecutionResult> {
    let policy = SupervisedProcessPolicy {
        stdout: SupervisedStreamCapture::Head { limit_bytes: 2 * 1024 * 1024, reject_excess: true },
        stderr: SupervisedStreamCapture::Tail { limit_bytes: 2 * 1024 * 1024 },
        deadline: Some(Instant::now() + Duration::from_secs(6 * 60 * 60)),
        ..SupervisedProcessPolicy::default()
    };
    let output = run_supervised_command(command, None, policy, cancel)
        .map_err(|error| process_supervision_failure(operation, error))?;
    if !output.status.success() {
        return Err(JobExecutionResult::Failed(format!(
            "{operation} failed: {}",
            bounded_process_reason(&output)
        )));
    }
    let mut combined = output.stdout.clone();
    combined.extend_from_slice(&output.stderr);
    Ok(String::from_utf8_lossy(&combined).into_owned())
}

fn run_dcp_package_validator(
    command: &mut Command,
    cancel: &ExecutionCancellationToken,
) -> Result<(), JobExecutionResult> {
    let policy = SupervisedProcessPolicy {
        stdout: SupervisedStreamCapture::Head { limit_bytes: 2 * 1024 * 1024, reject_excess: true },
        stderr: SupervisedStreamCapture::Tail { limit_bytes: 2 * 1024 * 1024 },
        deadline: Some(Instant::now() + Duration::from_secs(6 * 60 * 60)),
        ..SupervisedProcessPolicy::default()
    };
    let output = run_supervised_command(command, None, policy, cancel)
        .map_err(|error| process_supervision_failure("DCP package verification", error))?;
    let mut combined = output.stdout.clone();
    combined.extend_from_slice(&output.stderr);
    let report = String::from_utf8_lossy(&combined);
    if report.lines().any(|line| line.trim_start().starts_with("Error:")) {
        return Err(JobExecutionResult::Failed(
            "DCP package verification reported a SMPTE interoperability error".to_owned(),
        ));
    }
    if !output.status.success()
        && !report.lines().any(|line| {
            let line = line.trim_start();
            line.starts_with("Bv2.1 error:") || line.starts_with("Warning:")
        })
    {
        return Err(JobExecutionResult::Failed(format!(
            "DCP package verification failed: {}",
            bounded_process_reason(&output)
        )));
    }
    Ok(())
}

fn extract_imf_descriptor_xml(
    toolchain: &ProfessionalDeliveryToolchain,
    track: &Path,
    work: &Path,
    label: &str,
    cancel: &ExecutionCancellationToken,
) -> Result<String, JobExecutionResult> {
    std::fs::create_dir(work).map_err(|error| {
        JobExecutionResult::Failed(format!(
            "failed to create {label} Photon work directory: {error}"
        ))
    })?;
    let mut command = toolchain.photon_track_descriptor_command(track, work);
    let output = run_professional_tool_capture(
        &mut command,
        &format!("{label} Photon descriptor extraction"),
        cancel,
    )?;
    if !output.contains("No errors were detected in the IMFTrackFile") {
        return Err(JobExecutionResult::Failed(format!(
            "{label} Photon descriptor extraction did not return zero-error evidence"
        )));
    }
    let descriptor_path = work.join("EssenceDescriptor.xml");
    let metadata = std::fs::symlink_metadata(&descriptor_path).map_err(|error| {
        JobExecutionResult::Failed(format!(
            "failed to inspect {label} Photon descriptor: {error}"
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 2 * 1024 * 1024
    {
        return Err(JobExecutionResult::Failed(format!(
            "{label} Photon descriptor is not one bounded direct file"
        )));
    }
    std::fs::read_to_string(&descriptor_path).map_err(|error| {
        JobExecutionResult::Failed(format!("failed to read {label} Photon descriptor: {error}"))
    })
}

fn bounded_process_reason(output: &mondrian_media::SupervisedProcessOutput) -> String {
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_owned())
        .unwrap_or_else(|| format!("exit status {}", output.status))
}

fn find_bmx_track(root: &Path, prefix: &str) -> Result<(PathBuf, PackageAssetId), String> {
    let mut matches = Vec::new();
    for entry in std::fs::read_dir(root)
        .map_err(|error| format!("failed to inspect BMX output directory: {error}"))?
    {
        let entry = entry.map_err(|error| format!("failed to inspect BMX output: {error}"))?;
        let metadata = entry
            .metadata()
            .map_err(|error| format!("failed to inspect BMX output metadata: {error}"))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if metadata.is_file() && name.starts_with(prefix) && name.ends_with(".mxf") {
            let uuid = name
                .strip_prefix(prefix)
                .and_then(|value| value.strip_suffix(".mxf"))
                .and_then(|value| uuid::Uuid::parse_str(value).ok())
                .ok_or_else(|| format!("BMX output does not carry a parseable fp_uuid: {name}"))?;
            matches.push((entry.path(), PackageAssetId::from_uuid(uuid)));
        }
    }
    if matches.len() != 1 {
        return Err(format!(
            "expected exactly one BMX {prefix} Track File, found {}",
            matches.len()
        ));
    }
    Ok(matches.remove(0))
}

fn publish_professional_directory(
    staging: OwnedPublicationDirectory,
    final_output: &Path,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    report: &mut dyn FnMut(ExportProgress),
) -> JobExecutionResult {
    if !execution_gate.wait_at_boundary(ExportProgressPhase::Publishing, cancel) {
        return JobExecutionResult::Cancelled;
    }
    report(ExportProgress::publishing(0.995));
    let partial_output = staging.path().to_path_buf();
    match staging.preserve_source_on_before_namespace_failure().publish_create_new() {
        Ok(evidence) => JobExecutionResult::Published(
            DurableExportPublication::from_directory_storage(evidence),
        ),
        Err(failure) => {
            JobExecutionResult::PublicationFailed(ExportPublicationFailure::from_directory_storage(
                failure,
                final_output,
                Some(partial_output),
            ))
        }
    }
}

fn execute_audio_stems_export(
    job: &RenderJob,
    final_output: &Path,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    report: &mut dyn FnMut(ExportProgress),
    report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
    audio_owner: &mut ExportAudioSourceOwner<'_>,
) -> JobExecutionResult {
    if job.config.output_policy != ExportOutputPolicy::CreateNew {
        return JobExecutionResult::Failed(
            "audio-stem packages support CreateNew publication only".to_owned(),
        );
    }
    if let Err(reason) = validate_snapshot_media_revisions(&job.config.timeline) {
        return JobExecutionResult::Failed(reason);
    }
    let delivery = match crate::delivery::resolve_export_delivery(
        &job.config.preset,
        &job.config.timeline.sequence.settings,
        &job.config.timeline.color_environment,
    ) {
        Ok(delivery) => delivery,
        Err(error) => return JobExecutionResult::Failed(error.to_string()),
    };
    let range = match compute_timeline_render_range_for_delivery(&job.config.timeline, &delivery) {
        Ok(range) => range,
        Err(error) => return JobExecutionResult::Failed(error),
    };
    let Some(format) = job.config.preset.audio_stem_format() else {
        return JobExecutionResult::Failed(
            "audio-stem execution selected a non-stem preset".to_owned(),
        );
    };
    let Some(prepared_audio) =
        job.config.timeline.prepared_execution().and_then(|execution| execution.audio())
    else {
        return JobExecutionResult::Failed(
            "audio-stem export has no immutable Program Output closure".to_owned(),
        );
    };
    let staging = match OwnedPublicationDirectory::create_sibling(
        final_output,
        &format!("export-{}", job.id()),
    ) {
        Ok(staging) => staging,
        Err(error) => {
            return JobExecutionResult::Failed(format!(
                "failed to reserve audio-stem sibling directory for {}: {error:#}",
                final_output.display()
            ));
        }
    };
    let partial_output = staging.path().to_path_buf();
    let sample_rate = job.config.timeline.sequence.settings.audio_sample_rate.max(8_000);
    let channel_layout = job.config.timeline.sequence.settings.audio_channel_layout;
    let total_sample_frames =
        match timeline_audio_sample_range(range, sample_rate).and_then(|(_, frames)| {
            u64::try_from(frames)
                .map_err(|_| "audio-stem sample-frame count exceeds u64".to_owned())
        }) {
            Ok(frames) => frames,
            Err(error) => return JobExecutionResult::Failed(error),
        };
    let output_count = prepared_audio.output_count();
    let mut stems = Vec::with_capacity(output_count);
    let mut primary_analysis = None;
    let rendered_stems = match render_audio_stems_to_pcm_f32(
        &job.config.timeline,
        prepared_audio,
        range,
        sample_rate,
        channel_layout,
        audio_owner,
        cancel,
        execution_gate,
        report,
    ) {
        Ok(rendered) => rendered,
        Err(outcome) => return outcome,
    };
    for (index, (output, rendered)) in prepared_audio.outputs().zip(rendered_stems).enumerate() {
        if index == 0 {
            primary_analysis = Some(rendered.loudness);
        }
        let file_name = stem_file_name(index, output.output_id());
        let output_path = stem_path(staging.path(), index, output.output_id());
        let Some(ffmpeg_layout) = ffmpeg_audio_channel_layout(channel_layout) else {
            return JobExecutionResult::Failed(format!(
                "audio-stem layout {channel_layout} has no explicit WAV lowering"
            ));
        };
        let mut command = mondrian_media::ffmpeg_command();
        command
            .arg("-y")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-f")
            .arg("f32le")
            .arg("-ar")
            .arg(sample_rate.to_string())
            .arg("-channel_layout")
            .arg(ffmpeg_layout)
            .arg("-ac")
            .arg(channel_layout.channel_count().to_string())
            .arg("-i")
            .arg(&rendered.path)
            .arg("-map")
            .arg("0:a:0")
            .arg("-c:a")
            .arg("pcm_s24le")
            .arg("-rf64")
            .arg("auto")
            .arg("-f")
            .arg("wav")
            .arg(&output_path);
        let encoded = mondrian_media::run_supervised_command(
            &mut command,
            None,
            SupervisedProcessPolicy {
                stdout: SupervisedStreamCapture::Drain,
                stderr: SupervisedStreamCapture::Tail { limit_bytes: 64 * 1024 },
                ..SupervisedProcessPolicy::default()
            },
            cancel,
        );
        let encoded = match encoded {
            Ok(encoded) => encoded,
            Err(_) if cancel.is_canceled() => return JobExecutionResult::Cancelled,
            Err(error) => {
                return JobExecutionResult::Failed(format!(
                    "audio-stem encoder process failed for {}: {error}",
                    output.output_id()
                ));
            }
        };
        if !encoded.status.success() {
            let detail = String::from_utf8_lossy(&encoded.stderr).trim().to_owned();
            return JobExecutionResult::Failed(format!(
                "audio-stem encoder failed for {}: {}",
                output.output_id(),
                if detail.is_empty() {
                    encoded.status.to_string()
                } else {
                    detail
                }
            ));
        }
        stems.push(AudioStemExpectation {
            output_id: output.output_id(),
            name: output.name().to_owned(),
            file_name,
            loudness: rendered.loudness,
        });
    }
    if let Some(audio) = primary_analysis {
        report_diagnostics(ExportJobDiagnostics {
            audio: Some(audio),
            ..ExportJobDiagnostics::default()
        });
    }
    if !execution_gate.wait_at_boundary(ExportProgressPhase::Validating, cancel) {
        return JobExecutionResult::Cancelled;
    }
    report(ExportProgress::validating(0.95));
    if let Err(error) = validate_and_write_audio_stem_manifest(
        staging.path(),
        AudioStemValidationContract {
            format,
            sample_rate,
            channel_layout,
            sample_frames: total_sample_frames,
            stems,
        },
        cancel,
    ) {
        return if cancel.is_canceled() {
            JobExecutionResult::Cancelled
        } else {
            JobExecutionResult::Failed(format!("audio-stem validation failed: {error}"))
        };
    }
    if let Err(reason) = validate_snapshot_media_revisions(&job.config.timeline) {
        return JobExecutionResult::Failed(reason);
    }
    if !execution_gate.wait_at_boundary(ExportProgressPhase::Publishing, cancel) {
        return JobExecutionResult::Cancelled;
    }
    report(ExportProgress::publishing(0.995));
    match staging.preserve_source_on_before_namespace_failure().publish_create_new() {
        Ok(evidence) => JobExecutionResult::Published(
            DurableExportPublication::from_directory_storage(evidence),
        ),
        Err(failure) => {
            JobExecutionResult::PublicationFailed(ExportPublicationFailure::from_directory_storage(
                failure,
                final_output,
                Some(partial_output),
            ))
        }
    }
}

fn execute_image_sequence_export(
    job: &RenderJob,
    final_output: &Path,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    report: &mut dyn FnMut(ExportProgress),
    report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
    audio_owner: &mut ExportAudioSourceOwner<'_>,
) -> JobExecutionResult {
    if job.config.output_policy != ExportOutputPolicy::CreateNew {
        return JobExecutionResult::Failed(
            "图像序列当前仅支持 CreateNew 发布；不会递归替换已有目录".to_owned(),
        );
    }
    let staging = match OwnedPublicationDirectory::create_sibling(
        final_output,
        &format!("export-{}", job.id()),
    ) {
        Ok(staging) => staging,
        Err(error) => {
            return JobExecutionResult::Failed(format!(
                "failed to reserve an exact sibling image-sequence directory for {}: {error:#}",
                final_output.display()
            ));
        }
    };
    let partial_output = staging.path().to_path_buf();
    let Some(format) = job.config.preset.image_sequence_format() else {
        return JobExecutionResult::Failed(
            "image-sequence execution selected a non-sequence preset".to_owned(),
        );
    };
    let output_pattern = ffmpeg_frame_pattern(staging.path(), format);
    let mut validation_contract = None;
    let outcome = execute_timeline_export(
        job,
        &job.config.timeline,
        &output_pattern,
        &mut validation_contract,
        cancel,
        execution_gate,
        report,
        report_diagnostics,
        audio_owner,
    );
    if !matches!(outcome, JobExecutionResult::ReversibleWorkCompleted) {
        return outcome;
    }
    let Some(ProducedArtifactValidation::ImageSequence(validation_contract)) = validation_contract
    else {
        return JobExecutionResult::Failed(
            "image-sequence export completed without its validation contract".to_owned(),
        );
    };
    if !execution_gate.wait_at_boundary(ExportProgressPhase::Validating, cancel) {
        return JobExecutionResult::Cancelled;
    }
    report(ExportProgress::validating(0.99));
    if let Err(error) = validate_and_write_manifest(staging.path(), validation_contract, cancel) {
        return if cancel.is_canceled() {
            JobExecutionResult::Cancelled
        } else {
            JobExecutionResult::Failed(format!("图像序列校验失败: {error}"))
        };
    }
    if let Err(reason) = validate_snapshot_media_revisions(&job.config.timeline) {
        return JobExecutionResult::Failed(reason);
    }
    if !execution_gate.wait_at_boundary(ExportProgressPhase::Publishing, cancel) {
        return JobExecutionResult::Cancelled;
    }
    report(ExportProgress::publishing(0.995));
    match staging.preserve_source_on_before_namespace_failure().publish_create_new() {
        Ok(evidence) => JobExecutionResult::Published(
            DurableExportPublication::from_directory_storage(evidence),
        ),
        Err(failure) => {
            JobExecutionResult::PublicationFailed(ExportPublicationFailure::from_directory_storage(
                failure,
                final_output,
                Some(partial_output),
            ))
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TimelineRenderRange {
    source_start: TimelineTime,
    total_frames: u64,
    fps_num: i64,
    fps_den: i64,
    sequence_frame_rate: Rational,
    frame_sampling: crate::preset::ExportFrameSampling,
}

impl TimelineRenderRange {
    fn time_range(self) -> Result<TimelineTimeRange, String> {
        let frame_count = i64::try_from(self.total_frames)
            .map_err(|_| "export frame count exceeds signed time capacity".to_owned())?;
        let duration = TimelineTime::from_frame_position(FramePosition::new(
            frame_count,
            Rational::new(self.fps_den, self.fps_num),
        ))
        .map_err(|error| error.to_string())?;
        TimelineTimeRange::new(self.source_start, duration).map_err(|error| error.to_string())
    }

    fn evaluation_frame(self, output_index: u64) -> Result<i64, String> {
        let output_index = i64::try_from(output_index)
            .map_err(|_| "export frame index exceeds signed time capacity".to_owned())?;
        let offset = TimelineTime::from_frame_position(FramePosition::new(
            output_index,
            Rational::new(self.fps_den, self.fps_num),
        ))
        .map_err(|error| error.to_string())?;
        let sample_time =
            self.source_start.checked_add(offset).map_err(|error| error.to_string())?;
        match self.frame_sampling {
            crate::preset::ExportFrameSampling::FrameHold => sample_time
                .to_frame_position(self.sequence_frame_rate, FrameRounding::Floor)
                .map(|position| position.frame)
                .map_err(|error| error.to_string()),
        }
    }

    fn interlaced_evaluation_position(
        self,
        output_index: u64,
        second_field: bool,
    ) -> Result<FramePosition, String> {
        if self.sequence_frame_rate != Rational::new(self.fps_num, self.fps_den) {
            return Err(format!(
                "interlaced Program Output requires export cadence {}:{} to match Sequence cadence {}",
                self.fps_num, self.fps_den, self.sequence_frame_rate
            ));
        }
        let output_index = i64::try_from(output_index)
            .map_err(|_| "export frame index exceeds signed time capacity".to_owned())?;
        let field_index = output_index
            .checked_mul(2)
            .and_then(|value| value.checked_add(i64::from(second_field)))
            .ok_or_else(|| "export field index exceeds signed time capacity".to_owned())?;
        let field_rate = Rational::new(
            self.sequence_frame_rate
                .num
                .checked_mul(2)
                .ok_or_else(|| "Sequence field cadence overflowed".to_owned())?,
            self.sequence_frame_rate.den,
        );
        let offset = TimelineTime::from_frame_position(FramePosition::new(
            field_index,
            Rational::new(field_rate.den, field_rate.num),
        ))
        .map_err(|error| error.to_string())?;
        let sample_time =
            self.source_start.checked_add(offset).map_err(|error| error.to_string())?;
        let position = sample_time
            .to_frame_position(field_rate, FrameRounding::Floor)
            .map_err(|error| error.to_string())?;
        if TimelineTime::from_frame_position(position).map_err(|error| error.to_string())?
            != sample_time
        {
            return Err(format!(
                "interlaced export sample time {sample_time} is not exactly representable on the Sequence field grid"
            ));
        }
        Ok(position)
    }
}

enum TimelineAudioInput {
    PcmFile {
        path: PathBuf,
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
        analysis: AudioLoudnessReport,
    },
    Silent {
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
        analysis: AudioLoudnessReport,
    },
    Disabled,
}

impl TimelineAudioInput {
    const fn analysis(&self) -> Option<AudioLoudnessReport> {
        match self {
            Self::PcmFile { analysis, .. } | Self::Silent { analysis, .. } => Some(*analysis),
            Self::Disabled => None,
        }
    }
}

#[derive(Debug, Clone)]
struct DecodedVideoLayer {
    frame: CpuColorFrame,
    is_data_texture: bool,
    source_resolution: Resolution,
    picture_geometry: ResolvedPictureGeometry,
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
    validation_contract_out: &mut Option<ProducedArtifactValidation>,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    report: &mut dyn FnMut(ExportProgress),
    report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
    audio_owner: &mut ExportAudioSourceOwner<'_>,
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
        let delivery = match crate::delivery::resolve_export_delivery(
            &job.config.preset,
            &timeline.sequence.settings,
            &timeline.color_environment,
        ) {
            Ok(delivery) => delivery,
            Err(error) => return JobExecutionResult::Failed(error.to_string()),
        };
        let range = match compute_timeline_render_range_for_delivery(timeline, &delivery) {
            Ok(range) => range,
            Err(error) => return JobExecutionResult::Failed(error),
        };
        if range.total_frames == 0 {
            return JobExecutionResult::Failed("时间线导出范围为空".to_string());
        }
        let selected_time_range = match range.time_range() {
            Ok(range) => range,
            Err(error) => return JobExecutionResult::Failed(error),
        };
        let dynamic_hdr_delivery = match crate::dynamic_hdr::resolve_dynamic_hdr_delivery(
            timeline,
            &delivery,
            selected_time_range,
        ) {
            Ok(delivery) => delivery,
            Err(error) => return JobExecutionResult::Failed(error),
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
        let export_color_context = match resolved_export_color_context(timeline, &delivery) {
            Ok(context) => context,
            Err(error) => return JobExecutionResult::Failed(error),
        };
        if let Err(outcome) = preflight_timeline_visual_range_at_resolution(
            timeline,
            range,
            Resolution {
                width: delivery.resolution.width,
                height: delivery.resolution.height,
            },
            export_color_context,
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

        let audio_codec = match &delivery.artifact {
            ResolvedExportArtifactEncoding::MediaFile { audio, .. } => audio,
            ResolvedExportArtifactEncoding::ImageSequence { .. }
            | ResolvedExportArtifactEncoding::AudioStems { .. }
            | ResolvedExportArtifactEncoding::ProfessionalDelivery { .. } => {
                &AudioCodecConfig::Disabled
            }
        };
        let audio_input = prepare_timeline_audio_input(
            audio_codec,
            timeline,
            range,
            cancel,
            execution_gate,
            report,
            audio_owner,
        );
        let audio_input = match audio_input {
            Ok(input) => input,
            Err(outcome) => return outcome,
        };
        let audio_analysis = audio_input.analysis();
        if let TimelineAudioInput::PcmFile { path, .. } = &audio_input {
            temp_audio_path_to_cleanup = Some(path.clone());
        }

        let (width, height) = (delivery.resolution.width, delivery.resolution.height);
        let mut expected_video_signal =
            match expected_export_video_signal(&timeline.sequence.settings, &delivery) {
                Ok(signal) => signal,
                Err(error) => return JobExecutionResult::Failed(error),
            };
        if let ResolvedExportArtifactEncoding::MediaFile { video, .. } = &delivery.artifact
            && crate::mezzanine::professional_mezzanine_contract(video).is_some()
        {
            // Qualified progressive DNxHR and raw MOV/MXF streams may omit a
            // stream-level field_order; decoded-frame evidence proves that row.
            // Interlaced v210 must retain both the explicit `tt` tag and the
            // decoded-frame dominance contract.
            if delivery.field_order == mondrian_core::timeline_data::FieldOrder::Progressive {
                expected_video_signal.field_order = None;
            }
            if !crate::mezzanine::requires_stream_range_tag(video) {
                expected_video_signal.color_range = None;
            }
        }
        let validation_contract = match &delivery.artifact {
            ResolvedExportArtifactEncoding::MediaFile { container, video, audio } => {
                let expected_audio = match &audio_input {
                    TimelineAudioInput::Disabled => None,
                    TimelineAudioInput::PcmFile { sample_rate, channel_layout, .. }
                    | TimelineAudioInput::Silent { sample_rate, channel_layout, .. } => {
                        match expected_audio_constraints(audio, *sample_rate, *channel_layout) {
                            Some(expected) => Some(expected),
                            None => {
                                return JobExecutionResult::Failed(
                                    "已启用的音频输出没有可证明的编码合同".to_string(),
                                );
                            }
                        }
                    }
                };
                ProducedArtifactValidation::MediaFile(Box::new(ExportValidationExpectations {
                    container: *container,
                    video: ExpectedStream::Required(ExpectedVideoConstraints {
                        encoding: Some(expected_video_encoding(video)),
                        codec_tag: (*container == Container::Mov)
                            .then(|| crate::mezzanine::expected_mov_codec_tag(video))
                            .flatten()
                            .map(str::to_owned),
                        bit_depth: Some(delivery_bit_depth_value(delivery.bit_depth)),
                        width: Some(width),
                        height: Some(height),
                        fps_num: Some(range.fps_num),
                        fps_den: Some(range.fps_den),
                        signal: Some(expected_video_signal),
                        coding: Some(delivery.video_coding),
                        require_progressive_frame: delivery.field_order
                            == mondrian_core::timeline_data::FieldOrder::Progressive,
                        require_interlaced_top_field_first: match delivery.field_order {
                            mondrian_core::timeline_data::FieldOrder::Progressive => None,
                            mondrian_core::timeline_data::FieldOrder::UpperFirst => Some(true),
                            mondrian_core::timeline_data::FieldOrder::LowerFirst => Some(false),
                        },
                    }),
                    audio: expected_audio
                        .map(ExpectedStream::Required)
                        .unwrap_or(ExpectedStream::Forbidden),
                    expected_duration_secs: Some(
                        range.total_frames as f64 * range.fps_den as f64
                            / range.fps_num.max(1) as f64,
                    ),
                }))
            }
            ResolvedExportArtifactEncoding::ImageSequence { format } => {
                ProducedArtifactValidation::ImageSequence(ImageSequenceValidationContract {
                    format: *format,
                    frame_count: range.total_frames,
                    width,
                    height,
                    frame_rate: delivery.frame_rate,
                    color_space: delivery.color_target.color_space,
                    alpha_mode: job.config.preset.alpha_mode,
                })
            }
            ResolvedExportArtifactEncoding::AudioStems { .. } => {
                return JobExecutionResult::Failed(
                    "audio-stem package entered the visual export executor".to_owned(),
                );
            }
            ResolvedExportArtifactEncoding::ProfessionalDelivery { .. } => {
                return JobExecutionResult::Failed(
                    "professional delivery entered the generic visual export executor".to_owned(),
                );
            }
        };
        match crate::dynamic_hdr::execute_dynamic_hdr_delivery(
            &dynamic_hdr_delivery,
            &job.config,
            timeline,
            &delivery,
            selected_time_range,
            range.total_frames,
            output_path,
            cancel,
        ) {
            Ok(Some(evidence)) => {
                if job.config.broadcast_qc.is_some() {
                    return JobExecutionResult::Failed(
                        "broadcast QC requires decoded delivery-picture observation; byte-preserved Dynamic HDR export cannot bypass analysis"
                            .to_owned(),
                    );
                }
                report_diagnostics(ExportJobDiagnostics {
                    audio: audio_analysis,
                    dynamic_hdr_preservation: Some(evidence),
                    ..ExportJobDiagnostics::default()
                });
                *validation_contract_out = Some(validation_contract);
                return JobExecutionResult::ReversibleWorkCompleted;
            }
            Ok(None) => {}
            Err(error) => return JobExecutionResult::Failed(error),
        }
        match try_execute_smart_render(
            job,
            timeline,
            &delivery,
            range,
            &audio_input,
            &validation_contract,
            output_path,
            cancel,
            execution_gate,
            report,
        ) {
            Ok(Some(evidence)) => {
                report_diagnostics(ExportJobDiagnostics {
                    audio: audio_analysis,
                    smart_render: Some(evidence),
                    ..ExportJobDiagnostics::default()
                });
                *validation_contract_out = Some(validation_contract);
                return JobExecutionResult::ReversibleWorkCompleted;
            }
            Ok(None) => {}
            Err(outcome) => return outcome,
        }
        visual_session.visual_diagnostics.resident_encode_admission_attempts = visual_session
            .visual_diagnostics
            .resident_encode_admission_attempts
            .saturating_add(1);
        let resident_qualification = if job.config.broadcast_qc.is_some() {
            Err(ExportResidentEncodeBlocker::BroadcastQcObservationRequired)
        } else {
            qualify_resident_hevc_export(
                timeline,
                &delivery,
                job.config.preset.alpha_mode,
                resource_policy,
            )
        };
        match resident_qualification {
            Ok(plan) => {
                #[cfg(target_os = "windows")]
                match execute_resident_hevc_export(
                    timeline,
                    &delivery,
                    range,
                    &audio_input,
                    output_path,
                    cancel,
                    execution_gate,
                    report,
                    report_diagnostics,
                    &mut visual_session,
                    ExportRenderInitialDiagnostics {
                        asset_issue_summary: media_diagnostics.issue_summary,
                        audio_analysis,
                    },
                    plan,
                ) {
                    ResidentExportAttemptOutcome::Completed => {
                        *validation_contract_out = Some(validation_contract);
                        return JobExecutionResult::ReversibleWorkCompleted;
                    }
                    ResidentExportAttemptOutcome::NotStarted => {
                        visual_session.visual_diagnostics.resident_encode_blocker =
                            Some(ExportResidentEncodeBlocker::BackendUnavailable);
                    }
                    ResidentExportAttemptOutcome::Cancelled => {
                        return JobExecutionResult::Cancelled;
                    }
                    ResidentExportAttemptOutcome::Failed(reason) => {
                        return JobExecutionResult::Failed(reason);
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = plan;
                    visual_session.visual_diagnostics.resident_encode_blocker =
                        Some(ExportResidentEncodeBlocker::BackendUnavailable);
                }
            }
            Err(blocker) => {
                visual_session.visual_diagnostics.resident_encode_blocker = Some(blocker);
            }
        }
        let resolved_video_encoder = match &delivery.artifact {
            ResolvedExportArtifactEncoding::MediaFile { video, .. } => {
                let adapter = visual_session.active_adapter_identity();
                match crate::hardware_encoding::resolve_video_encoder(
                    video,
                    delivery.pixel_format,
                    adapter.as_ref(),
                    timeline
                        .sequence
                        .settings
                        .delivery
                        .static_hdr_metadata_policy
                        .writes_authored_metadata(),
                    cancel,
                ) {
                    Ok(encoder) => Some(encoder),
                    Err(_) if cancel.is_canceled() => return JobExecutionResult::Cancelled,
                    Err(error) => return JobExecutionResult::Failed(error),
                }
            }
            ResolvedExportArtifactEncoding::ImageSequence { .. } => None,
            ResolvedExportArtifactEncoding::AudioStems { .. } => {
                return JobExecutionResult::Failed(
                    "audio-stem package entered video encoder resolution".to_owned(),
                );
            }
            ResolvedExportArtifactEncoding::ProfessionalDelivery { .. } => {
                return JobExecutionResult::Failed(
                    "professional delivery entered generic video encoder resolution".to_owned(),
                );
            }
        };
        let image_encoding = match delivery.artifact {
            ResolvedExportArtifactEncoding::ImageSequence { format } => {
                match resolve_image_sequence_encoding(format, job.config.preset.alpha_mode) {
                    Ok(contract) => Some((format, contract)),
                    Err(error) => return JobExecutionResult::Failed(error.to_owned()),
                }
            }
            ResolvedExportArtifactEncoding::MediaFile { .. }
            | ResolvedExportArtifactEncoding::AudioStems { .. }
            | ResolvedExportArtifactEncoding::ProfessionalDelivery { .. } => None,
        };
        if let Some((format, image_contract)) = image_encoding
            && image_contract.adapter == ImageSequenceEncoderAdapter::NativeTiffFloat
        {
            let Some(directory) = output_path.parent() else {
                return JobExecutionResult::Failed(
                    "native image-sequence output pattern has no parent directory".to_owned(),
                );
            };
            let outcome = write_native_image_sequence_frames(
                directory,
                format,
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
                ExportRenderObservations {
                    broadcast_qc_profile: job.config.broadcast_qc.as_ref(),
                    initial_diagnostics: ExportRenderInitialDiagnostics {
                        asset_issue_summary: media_diagnostics.issue_summary,
                        audio_analysis,
                    },
                },
            );
            if matches!(outcome, JobExecutionResult::ReversibleWorkCompleted) {
                *validation_contract_out = Some(validation_contract);
            }
            return outcome;
        }
        let mut cmd = mondrian_media::ffmpeg_command();
        let frame_contract = export_frame_contract(&delivery);
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
            TimelineAudioInput::PcmFile { path, sample_rate, channel_layout, .. } => {
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
            TimelineAudioInput::Silent { sample_rate, channel_layout, .. } => {
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

        match &delivery.artifact {
            ResolvedExportArtifactEncoding::MediaFile { container, video, audio } => {
                let Some(encoder) = resolved_video_encoder else {
                    return JobExecutionResult::Failed(
                        "media-file export has no resolved video encoder".to_owned(),
                    );
                };
                apply_video_codec_args(&mut cmd, video, delivery.video_coding, encoder);
                apply_export_video_signal_args(&mut cmd, &timeline.sequence.settings, &delivery);
                if let Err(err) = apply_encoder_signal_params(
                    &mut cmd,
                    video,
                    encoder,
                    &timeline.sequence.settings,
                    &delivery,
                ) {
                    return JobExecutionResult::Failed(err);
                }
                if !matches!(&audio_input, TimelineAudioInput::Disabled) {
                    apply_audio_codec_args(&mut cmd, audio);
                }
                cmd.arg("-f").arg(container_format(container)).arg(output_path);
            }
            ResolvedExportArtifactEncoding::ImageSequence { format } => {
                apply_export_video_signal_args(&mut cmd, &timeline.sequence.settings, &delivery);
                let Some((_, image_contract)) = image_encoding else {
                    return JobExecutionResult::Failed(
                        "image-sequence encoder lost its resolved contract".to_owned(),
                    );
                };
                if let Err(error) = apply_ffmpeg_image_encoder_args(
                    &mut cmd,
                    *format,
                    image_contract.output_pixel_format,
                ) {
                    return JobExecutionResult::Failed(error.to_owned());
                }
                cmd.arg("-start_number")
                    .arg("0")
                    .arg("-frames:v")
                    .arg(range.total_frames.to_string())
                    .arg("-f")
                    .arg("image2")
                    .arg(output_path);
            }
            ResolvedExportArtifactEncoding::AudioStems { .. } => {
                return JobExecutionResult::Failed(
                    "audio-stem package entered FFmpeg video command construction".to_owned(),
                );
            }
            ResolvedExportArtifactEncoding::ProfessionalDelivery { .. } => {
                return JobExecutionResult::Failed(
                    "professional delivery entered generic FFmpeg command construction".to_owned(),
                );
            }
        }

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
            ExportRenderObservations {
                broadcast_qc_profile: job.config.broadcast_qc.as_ref(),
                initial_diagnostics: ExportRenderInitialDiagnostics {
                    asset_issue_summary: media_diagnostics.issue_summary,
                    audio_analysis,
                },
            },
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
                *validation_contract_out = Some(validation_contract);
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

#[cfg(target_os = "windows")]
enum ResidentExportAttemptOutcome {
    Completed,
    NotStarted,
    Cancelled,
    Failed(String),
}

#[cfg(target_os = "windows")]
#[allow(clippy::too_many_arguments)]
fn execute_resident_hevc_export(
    timeline: &TimelineExportSnapshot,
    delivery: &ResolvedExportDeliveryContract,
    range: TimelineRenderRange,
    audio_input: &TimelineAudioInput,
    output_path: &Path,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    report: &mut dyn FnMut(ExportProgress),
    report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
    visual_session: &mut ExportVisualRenderSession,
    initial_diagnostics: ExportRenderInitialDiagnostics,
    plan: ResidentHevcExportPlan,
) -> ResidentExportAttemptOutcome {
    let width = delivery.resolution.width;
    let height = delivery.resolution.height;
    let frame_rate_num = match u32::try_from(delivery.frame_rate.num) {
        Ok(value) if value > 0 => value,
        _ => return ResidentExportAttemptOutcome::NotStarted,
    };
    let frame_rate_den = match u32::try_from(delivery.frame_rate.den) {
        Ok(value) if value > 0 => value,
        _ => return ResidentExportAttemptOutcome::NotStarted,
    };
    let adapter_contract = mondrian_renderer::D3D12ResidentEncodeAdapterContract {
        width,
        height,
        frame_rate_num,
        frame_rate_den,
        bit_depth: plan.bit_depth,
        colorimetry: plan.colorimetry,
        full_range: plan.full_range,
        max_frames_in_flight: usize::try_from(plan.surface_pool_size).unwrap_or(usize::MAX),
    };
    let mut adapter =
        match visual_session.gpu_output.create_resident_encode_adapter(adapter_contract) {
            Ok(adapter) => adapter,
            Err(error) => {
                tracing::debug!(%error, "resident HEVC Adapter qualification rejected");
                return ResidentExportAttemptOutcome::NotStarted;
            }
        };
    let temp_dir = match tempfile::Builder::new().prefix("mondrian-resident-hevc-").tempdir() {
        Ok(directory) => directory,
        Err(error) => {
            tracing::debug!(%error, "resident HEVC temporary directory unavailable");
            return ResidentExportAttemptOutcome::NotStarted;
        }
    };
    let resident_video_path = temp_dir.path().join("resident-video.mkv");
    let config = ResidentHevcEncoderConfig {
        output_path: resident_video_path.clone(),
        width,
        height,
        frame_rate_num,
        frame_rate_den,
        bit_depth: plan.bit_depth,
        colorimetry: plan.colorimetry,
        full_range: plan.full_range,
        keyframe_interval_frames: plan.keyframe_interval_frames,
        max_b_frames: plan.max_b_frames,
        quantizer: plan.quantizer,
        surface_pool_size: plan.surface_pool_size,
    };
    let mut encoder =
        match D3D12ResidentHevcEncoderSession::open(&adapter.encoder_device_root(), config) {
            Ok(encoder) => encoder,
            Err(error) => {
                tracing::debug!(%error, "resident HEVC encoder Session rejected");
                return ResidentExportAttemptOutcome::NotStarted;
            }
        };
    visual_session.visual_diagnostics.resident_encode_sessions =
        visual_session.visual_diagnostics.resident_encode_sessions.saturating_add(1);
    let root_color_context = match resolved_export_color_context(timeline, delivery) {
        Ok(context) => context,
        Err(error) => return ResidentExportAttemptOutcome::Failed(error),
    };
    let frame_contract = export_frame_contract(delivery);
    let total = range.total_frames.max(1);
    let mut diagnostics = ExportJobDiagnostics::default();
    diagnostics
        .color
        .record_asset_issue_summary(initial_diagnostics.asset_issue_summary);
    diagnostics.audio = initial_diagnostics.audio_analysis;
    let mut submitted = 0_u64;

    for index in 0..total {
        if !execution_gate.wait_at_boundary(ExportProgressPhase::Rendering, cancel) {
            return ResidentExportAttemptOutcome::Cancelled;
        }
        let timeline_frame = match range.evaluation_frame(index) {
            Ok(frame) => frame,
            Err(error) => return ResidentExportAttemptOutcome::Failed(error),
        };
        let mut frame_color_counts = InputColorResolutionSourceCounts::default();
        let mut frame_stage_diagnostics = RenderColorStageDiagnostics::default();
        let mut frame_composite_diagnostics = TimelineCompositeDiagnostics::default();
        let source = match render_timeline_frame_resident_with_session_cancellable(
            timeline,
            timeline_frame,
            width,
            height,
            ExportAlphaMode::FlattenBlack,
            root_color_context.clone(),
            ExportDeliveryPixelContract::new(frame_contract, delivery.legalizer),
            Some(&mut frame_color_counts),
            Some(&mut frame_stage_diagnostics),
            Some(&mut frame_composite_diagnostics),
            Some(&mut diagnostics.color),
            visual_session,
            cancel,
        ) {
            Ok(source) => source,
            Err(_error) if cancel.is_canceled() => {
                return ResidentExportAttemptOutcome::Cancelled;
            }
            Err(error) if submitted == 0 => {
                tracing::debug!(%error, "resident HEVC first frame rejected before submission");
                return ResidentExportAttemptOutcome::NotStarted;
            }
            Err(error) => {
                return ResidentExportAttemptOutcome::Failed(format!(
                    "resident HEVC render failed after {submitted} submitted frames: {error}"
                ));
            }
        };
        let destination = match encoder.acquire_input_frame() {
            Ok(frame) => frame,
            Err(error) if submitted == 0 => {
                tracing::debug!(%error, "resident HEVC first input surface unavailable");
                return ResidentExportAttemptOutcome::NotStarted;
            }
            Err(error) => {
                return ResidentExportAttemptOutcome::Failed(format!(
                    "resident HEVC surface acquisition failed after {submitted} frames: {error}"
                ));
            }
        };
        let ready = match adapter.process(source, destination) {
            Ok(ready) => ready,
            Err(error) if submitted == 0 => {
                tracing::debug!(%error, "resident HEVC first Video Process submission rejected");
                return ResidentExportAttemptOutcome::NotStarted;
            }
            Err(error) => {
                return ResidentExportAttemptOutcome::Failed(format!(
                    "resident HEVC Video Process failed after {submitted} frames: {error}"
                ));
            }
        };
        if let Err(error) = encoder.submit_input_frame(ready, index) {
            return ResidentExportAttemptOutcome::Failed(format!(
                "resident HEVC encoder rejected frame {index}: {error}"
            ));
        }
        submitted = submitted.saturating_add(1);
        diagnostics.color.record_frame_diagnostics(
            frame_color_counts,
            frame_stage_diagnostics,
            frame_composite_diagnostics,
        );
        visual_session.visual_diagnostics.resident_encode_frames = submitted;
        visual_session.visual_diagnostics.resident_encode_video_process_submissions =
            adapter.diagnostics().video_process_submissions;
        diagnostics.visual = visual_session.visual_diagnostics();
        report_diagnostics(diagnostics.clone());
        let ratio = submitted as f32 / total as f32;
        report(ExportProgress::rendering(
            (0.18 + 0.72 * ratio).clamp(0.18, 0.92),
            submitted,
            total,
        ));
    }

    if cancel.is_canceled() {
        return ResidentExportAttemptOutcome::Cancelled;
    }
    if let Err(error) = encoder.finish() {
        return ResidentExportAttemptOutcome::Failed(format!(
            "resident HEVC encoder finalization failed: {error}"
        ));
    }
    let media_evidence = encoder.diagnostics();
    let renderer_evidence = adapter.diagnostics();
    visual_session.visual_diagnostics.resident_encode_packets = media_evidence.packets_written;
    visual_session.visual_diagnostics.resident_encode_cpu_pixel_readbacks = media_evidence
        .cpu_pixel_readbacks
        .saturating_add(renderer_evidence.cpu_pixel_readbacks);
    visual_session.visual_diagnostics.resident_encode_rawvideo_pipe_bytes = media_evidence
        .rawvideo_pipe_bytes
        .saturating_add(renderer_evidence.rawvideo_pipe_bytes);
    visual_session.visual_diagnostics.resident_encode_cpu_pixel_uploads = media_evidence
        .cpu_pixel_uploads
        .saturating_add(renderer_evidence.cpu_pixel_uploads);

    if !execution_gate.wait_at_boundary(ExportProgressPhase::Encoding, cancel) {
        return ResidentExportAttemptOutcome::Cancelled;
    }
    let ResolvedExportArtifactEncoding::MediaFile { container, audio, .. } = &delivery.artifact
    else {
        return ResidentExportAttemptOutcome::Failed(
            "resident HEVC route lost its media-file contract".to_owned(),
        );
    };
    let mut command = mondrian_media::ffmpeg_command();
    command
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(&resident_video_path)
        .arg("-map")
        .arg("0:v:0")
        .arg("-c:v")
        .arg("copy");
    match audio_input {
        TimelineAudioInput::PcmFile { path, sample_rate, channel_layout, .. } => {
            let Some(layout) = ffmpeg_audio_channel_layout(*channel_layout) else {
                return ResidentExportAttemptOutcome::Failed(
                    "resident HEVC audio layout has no FFmpeg lowering".to_owned(),
                );
            };
            command
                .arg("-f")
                .arg("f32le")
                .arg("-ar")
                .arg(sample_rate.to_string())
                .arg("-channel_layout")
                .arg(layout)
                .arg("-ac")
                .arg(channel_layout.channel_count().to_string())
                .arg("-i")
                .arg(path)
                .arg("-map")
                .arg("1:a:0")
                .arg("-shortest");
            apply_audio_codec_args(&mut command, audio);
        }
        TimelineAudioInput::Silent { sample_rate, channel_layout, .. } => {
            let Some(layout) = ffmpeg_audio_channel_layout(*channel_layout) else {
                return ResidentExportAttemptOutcome::Failed(
                    "resident HEVC audio layout has no FFmpeg lowering".to_owned(),
                );
            };
            command
                .arg("-f")
                .arg("lavfi")
                .arg("-i")
                .arg(format!(
                    "anullsrc=channel_layout={layout}:sample_rate={sample_rate}"
                ))
                .arg("-map")
                .arg("1:a:0")
                .arg("-shortest");
            apply_audio_codec_args(&mut command, audio);
        }
        TimelineAudioInput::Disabled => {
            command.arg("-an");
        }
    }
    apply_export_video_signal_args(&mut command, &timeline.sequence.settings, delivery);
    command.arg("-f").arg(container_format(container)).arg(output_path);
    report(ExportProgress::encoding(0.98));
    let output = mondrian_media::run_supervised_command(
        &mut command,
        None,
        SupervisedProcessPolicy {
            stdout: SupervisedStreamCapture::Drain,
            stderr: SupervisedStreamCapture::Tail { limit_bytes: 64 * 1024 },
            ..SupervisedProcessPolicy::default()
        },
        cancel,
    );
    match output {
        Ok(output) if output.status.success() => {
            visual_session.visual_diagnostics.resident_encode_video_stream_copy_muxes =
                visual_session
                    .visual_diagnostics
                    .resident_encode_video_stream_copy_muxes
                    .saturating_add(1);
            diagnostics.visual = visual_session.visual_diagnostics();
            report_diagnostics(diagnostics.clone());
            ResidentExportAttemptOutcome::Completed
        }
        Ok(output) => {
            let reason = String::from_utf8_lossy(&output.stderr)
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("unknown FFmpeg remux failure")
                .to_owned();
            ResidentExportAttemptOutcome::Failed(format!(
                "resident HEVC final stream-copy mux failed: {reason}"
            ))
        }
        Err(error) if error.is_canceled() => ResidentExportAttemptOutcome::Cancelled,
        Err(error) => ResidentExportAttemptOutcome::Failed(format!(
            "resident HEVC final stream-copy mux failed: {error}"
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn try_execute_smart_render(
    job: &RenderJob,
    timeline: &TimelineExportSnapshot,
    delivery: &ResolvedExportDeliveryContract,
    range: TimelineRenderRange,
    audio_input: &TimelineAudioInput,
    validation_contract: &ProducedArtifactValidation,
    output_path: &Path,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    report: &mut dyn FnMut(ExportProgress),
) -> Result<Option<ExportSmartRenderEvidence>, JobExecutionResult> {
    if job.config.broadcast_qc.is_some() {
        return Ok(None);
    }
    if delivery.field_order != mondrian_core::timeline_data::FieldOrder::Progressive {
        tracing::debug!("Smart Render is not qualified for field-woven output");
        return Ok(None);
    }
    let selected_range = range.time_range().map_err(JobExecutionResult::Failed)?;
    let plan = match crate::smart_render::qualify_smart_render(
        &job.config,
        timeline,
        delivery,
        selected_range,
        range.total_frames,
    ) {
        Ok(plan) => plan,
        Err(blocker) => {
            tracing::debug!(
                ?blocker,
                "Smart Render eligibility rejected; using pixel render"
            );
            return Ok(None);
        }
    };
    let ResolvedExportArtifactEncoding::MediaFile { container, audio, .. } = &delivery.artifact
    else {
        return Ok(None);
    };
    let ProducedArtifactValidation::MediaFile(expectations) = validation_contract else {
        return Ok(None);
    };
    if !execution_gate.wait_at_boundary(ExportProgressPhase::Preparing, cancel) {
        return Err(JobExecutionResult::Cancelled);
    }
    let source_packets = match mondrian_media::capture_video_packet_identity_cancellable(
        &plan.path,
        Some(plan.video_stream_index),
        cancel,
    ) {
        Ok(identity) if identity.first_packet_is_key => identity,
        Ok(_) => {
            tracing::debug!(
                asset_id = %plan.asset_id,
                "Smart Render source does not begin on an independently decodable packet"
            );
            return Ok(None);
        }
        Err(mondrian_media::VideoPacketIdentityError::Canceled) => {
            return Err(JobExecutionResult::Cancelled);
        }
        Err(error) => {
            tracing::debug!(%error, "Smart Render source packet identity unavailable");
            return Ok(None);
        }
    };
    if MediaFileFingerprint::capture(&plan.path) != plan.source_fingerprint {
        return Ok(None);
    }

    let mut command = mondrian_media::ffmpeg_command();
    command
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(&plan.path);
    match audio_input {
        TimelineAudioInput::PcmFile { path, sample_rate, channel_layout, .. } => {
            let Some(layout) = ffmpeg_audio_channel_layout(*channel_layout) else {
                return Ok(None);
            };
            command
                .arg("-f")
                .arg("f32le")
                .arg("-ar")
                .arg(sample_rate.to_string())
                .arg("-channel_layout")
                .arg(layout)
                .arg("-ac")
                .arg(channel_layout.channel_count().to_string())
                .arg("-i")
                .arg(path);
        }
        TimelineAudioInput::Silent { sample_rate, channel_layout, .. } => {
            let Some(layout) = ffmpeg_audio_channel_layout(*channel_layout) else {
                return Ok(None);
            };
            command.arg("-f").arg("lavfi").arg("-i").arg(format!(
                "anullsrc=channel_layout={layout}:sample_rate={sample_rate}"
            ));
        }
        TimelineAudioInput::Disabled => {}
    }
    command
        .arg("-map")
        .arg(format!("0:{}", plan.video_stream_index))
        .arg("-c:v")
        .arg("copy");
    match audio_input {
        TimelineAudioInput::PcmFile { .. } | TimelineAudioInput::Silent { .. } => {
            command.arg("-map").arg("1:a:0").arg("-shortest");
            apply_audio_codec_args(&mut command, audio);
        }
        TimelineAudioInput::Disabled => {
            command.arg("-an");
        }
    }
    command.arg("-f").arg(container_format(container)).arg(output_path);

    if !execution_gate.wait_at_boundary(ExportProgressPhase::Encoding, cancel) {
        return Err(JobExecutionResult::Cancelled);
    }
    report(ExportProgress::encoding(0.90));
    let output = mondrian_media::run_supervised_command(
        &mut command,
        None,
        SupervisedProcessPolicy {
            stdout: SupervisedStreamCapture::Drain,
            stderr: SupervisedStreamCapture::Tail { limit_bytes: 64 * 1024 },
            ..SupervisedProcessPolicy::default()
        },
        cancel,
    );
    let output = match output {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            tracing::debug!(
                status = %output.status,
                stderr = %String::from_utf8_lossy(&output.stderr),
                "Smart Render remux failed; using pixel render"
            );
            return Ok(None);
        }
        Err(_) if cancel.is_canceled() => return Err(JobExecutionResult::Cancelled),
        Err(error) => {
            tracing::debug!(%error, "Smart Render remux supervision failed");
            return Ok(None);
        }
    };
    drop(output);
    let copied_packets = match mondrian_media::capture_video_packet_identity_cancellable(
        output_path,
        None,
        cancel,
    ) {
        Ok(identity) => identity,
        Err(mondrian_media::VideoPacketIdentityError::Canceled) => {
            return Err(JobExecutionResult::Cancelled);
        }
        Err(error) => {
            tracing::debug!(%error, "Smart Render output packet identity unavailable");
            return Ok(None);
        }
    };
    if source_packets.packet_count != copied_packets.packet_count
        || source_packets.payload_bytes != copied_packets.payload_bytes
        || source_packets.payload_digest != copied_packets.payload_digest
    {
        tracing::debug!(
            source_packets = source_packets.packet_count,
            output_packets = copied_packets.packet_count,
            "Smart Render packet identity changed during remux"
        );
        return Ok(None);
    }
    match validate_export_output_cancellable(output_path, expectations, cancel) {
        Ok(_) => {}
        Err(_) if cancel.is_canceled() => return Err(JobExecutionResult::Cancelled),
        Err(error) => {
            tracing::debug!(%error, "Smart Render output contract validation failed");
            return Ok(None);
        }
    }
    if let Err(reason) = validate_snapshot_media_revisions(timeline) {
        tracing::debug!(%reason, "Smart Render source revision changed before completion");
        return Ok(None);
    }
    Ok(Some(ExportSmartRenderEvidence {
        source_asset_id: plan.asset_id,
        packet_count: source_packets.packet_count,
        payload_bytes: source_packets.payload_bytes,
        packet_identity_verified: true,
    }))
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
    audio_codec: &AudioCodecConfig,
    timeline: &TimelineExportSnapshot,
    range: TimelineRenderRange,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    report: &mut dyn FnMut(ExportProgress),
    audio_owner: &mut ExportAudioSourceOwner<'_>,
) -> Result<TimelineAudioInput, JobExecutionResult> {
    if !execution_gate.wait_at_boundary(ExportProgressPhase::Preparing, cancel) {
        return Err(JobExecutionResult::Cancelled);
    }
    if matches!(audio_codec, AudioCodecConfig::Disabled) {
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
        let sample_frames = u64::try_from(
            timeline_audio_sample_range(range, sample_rate)
                .map_err(JobExecutionResult::Failed)?
                .1,
        )
        .map_err(|_| {
            JobExecutionResult::Failed(
                "audio sample-frame count exceeds loudness evidence capacity".to_owned(),
            )
        })?;
        return Ok(TimelineAudioInput::Silent {
            sample_rate,
            channel_layout,
            analysis: AudioLoudnessReport::digital_silence(sample_frames),
        });
    }

    let temp = tempfile::Builder::new()
        .prefix("mondrian-export-audio-")
        .suffix(".f32")
        .tempfile()
        .map_err(|error| {
            JobExecutionResult::Failed(format!("分配唯一导出音频临时文件失败: {error}"))
        })?;
    let temp_path = temp.path().to_path_buf();
    // The render helper creates the file itself, so drop the reserved handle
    // first; the unique path keeps concurrent jobs and reruns isolated.
    drop(temp);
    let mut temp_guard = ExportAudioTempFile::armed(&temp_path);
    let mut analysis = None;
    match render_timeline_audio_to_pcm_f32(
        temp_path.as_path(),
        timeline,
        prepared_audio.primary_output(),
        range,
        sample_rate,
        channel_layout,
        audio_owner,
        cancel,
        execution_gate,
        report,
        &mut analysis,
    ) {
        JobExecutionResult::ReversibleWorkCompleted => {
            temp_guard.defuse();
            let Some(analysis) = analysis else {
                return Err(JobExecutionResult::Failed(
                    "rendered audio completed without loudness/true-peak evidence".to_owned(),
                ));
            };
            Ok(TimelineAudioInput::PcmFile {
                path: temp_path,
                sample_rate,
                channel_layout,
                analysis,
            })
        }
        JobExecutionResult::Published(_) | JobExecutionResult::PublicationFailed(_) => {
            Err(JobExecutionResult::Failed(
                "audio preparation crossed publication authority inside reversible export work"
                    .to_owned(),
            ))
        }
        JobExecutionResult::Cancelled => Err(JobExecutionResult::Cancelled),
        JobExecutionResult::Failed(reason) => Err(JobExecutionResult::Failed(reason)),
    }
}

/// RAII cleanup for one export audio PCM artifact.
///
/// On any non-success path (or a panic) the guard removes the artifact and
/// logs the deletion failure instead of silently leaking a potentially
/// multi-gigabyte interleaved PCM file in the shared temp directory.
struct ExportAudioTempFile {
    path: Option<PathBuf>,
}

const EXPORT_AUDIO_SOURCE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const EXPORT_AUDIO_SOURCE_WINDOW_SECONDS: usize = 10;

#[derive(Debug, Clone, Copy)]
struct ExportAudioSourceClosureEvidence {
    external_cache_references: usize,
    cache: AudioSourceCacheShutdownEvidence,
}

impl ExportAudioSourceClosureEvidence {
    fn all_resources_released(self) -> bool {
        self.external_cache_references == 0 && self.cache.all_resources_released()
    }
}

struct ExportAudioSourceOwner<'a> {
    config: AudioSourceCacheConfig,
    sample_rate: Option<u32>,
    cache: Option<Arc<AudioSourceCache>>,
    report_owner: &'a mut dyn FnMut(ExportExecutionOwnerEvent),
}

impl<'a> ExportAudioSourceOwner<'a> {
    fn new(
        config: AudioSourceCacheConfig,
        report_owner: &'a mut dyn FnMut(ExportExecutionOwnerEvent),
    ) -> Self {
        Self {
            config,
            sample_rate: None,
            cache: None,
            report_owner,
        }
    }

    fn cache(&mut self, sample_rate: u32) -> Result<Arc<AudioSourceCache>, String> {
        if let Some(existing_rate) = self.sample_rate
            && existing_rate != sample_rate
        {
            return Err(format!(
                "export job attempted to reuse one audio source owner at {existing_rate} Hz and {sample_rate} Hz"
            ));
        }
        if let Some(cache) = self.cache.as_ref() {
            return Ok(Arc::clone(cache));
        }
        let cache = Arc::new(AudioSourceCache::new_bounded_with_sessions(
            sample_rate,
            EXPORT_AUDIO_SOURCE_WINDOW_SECONDS,
            self.config.entry_capacity,
            self.config.byte_budget,
            self.config.decoder_session_capacity,
        ));
        self.sample_rate = Some(sample_rate);
        self.cache = Some(Arc::clone(&cache));
        (self.report_owner)(ExportExecutionOwnerEvent::AudioSourceStarted);
        Ok(cache)
    }

    fn shutdown_until(mut self, deadline: Instant) -> Option<ExportAudioSourceClosureEvidence> {
        let cache = self.cache.take()?;
        cache.begin_shutdown();
        let evidence = match Arc::try_unwrap(cache) {
            Ok(cache) => ExportAudioSourceClosureEvidence {
                external_cache_references: 0,
                cache: cache.shutdown_until(deadline),
            },
            Err(cache) => {
                let external_cache_references = Arc::strong_count(&cache).saturating_sub(1);
                drop(cache);
                ExportAudioSourceClosureEvidence {
                    external_cache_references,
                    cache: AudioSourceCacheShutdownEvidence::default(),
                }
            }
        };
        (self.report_owner)(ExportExecutionOwnerEvent::AudioSourceClosed {
            all_resources_released: evidence.all_resources_released(),
        });
        Some(evidence)
    }
}

fn finish_export_with_audio_closure(
    outcome: JobExecutionResult,
    audio_closure: Option<ExportAudioSourceClosureEvidence>,
) -> JobExecutionResult {
    let Some(audio_closure) = audio_closure else {
        return outcome;
    };
    if audio_closure.all_resources_released() {
        return outcome;
    }
    let detail = format!("export audio source closure was incomplete: {audio_closure:?}");
    match outcome {
        JobExecutionResult::Published(_) | JobExecutionResult::PublicationFailed(_) => outcome,
        JobExecutionResult::Failed(reason) => {
            JobExecutionResult::Failed(format!("{reason}; {detail}"))
        }
        JobExecutionResult::ReversibleWorkCompleted | JobExecutionResult::Cancelled => {
            JobExecutionResult::Failed(detail)
        }
    }
}

impl ExportAudioTempFile {
    fn armed(path: &Path) -> Self {
        Self { path: Some(path.to_path_buf()) }
    }

    fn defuse(&mut self) {
        self.path = None;
    }
}

impl Drop for ExportAudioTempFile {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else { return };
        if let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                "failed to remove export audio temporary artifact {}: {error}",
                path.display()
            );
        }
    }
}

fn prepare_timeline_audio_delivery(
    timeline: &TimelineExportSnapshot,
    prepared_audio: &PreparedTimelineAudioOutputSnapshot,
    range: TimelineRenderRange,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    cache: &Arc<AudioSourceCache>,
    resource_grant: AudioRuntimeResourceGrant,
) -> Result<AudioProgramDeliveryRuntime, String> {
    let resolver = ExportAudioMediaResolver { timeline, cache: Arc::clone(cache) };
    let contract = AudioRenderContract {
        sample_rate,
        channel_layout: timeline.sequence.settings.audio_channel_layout,
        max_block_frames: 16_384,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    };
    let public_time_range = range.time_range()?;
    let runtime =
        AudioProgramRuntime::build_from_precompiled_closure_for_range_with_resource_grant(
            &timeline.sequence,
            &timeline.sequences,
            &resolver,
            contract,
            Some(prepared_audio.root_program().output_id()),
            public_time_range,
            prepared_audio.closure(),
            resource_grant,
        )
        .map_err(|error| format!("编译导出音频 Program 失败（未使用降级混音）: {error}"))?;
    if runtime.execution_demand() != prepared_audio.execution_demand() {
        return Err(
            "prepared audio Runtime execution demand differs from admitted root Program evidence"
                .to_owned(),
        );
    }
    AudioProgramDeliveryRuntime::prepare_standard(runtime, channel_layout)
        .map_err(|error| format!("导出音频输出布局映射不可用: {error}"))
}

fn add_audio_runtime_footprint(
    total: AudioRuntimeResourceFootprint,
    next: AudioRuntimeResourceFootprint,
) -> Option<AudioRuntimeResourceFootprint> {
    Some(AudioRuntimeResourceFootprint {
        runtime_occurrences: total.runtime_occurrences.checked_add(next.runtime_occurrences)?,
        fixed_resident_bytes: total.fixed_resident_bytes.checked_add(next.fixed_resident_bytes)?,
        prepared_logical_bytes: total
            .prepared_logical_bytes
            .checked_add(next.prepared_logical_bytes)?,
        render_scratch_bytes: total.render_scratch_bytes.checked_add(next.render_scratch_bytes)?,
        processor_session_bytes: total
            .processor_session_bytes
            .checked_add(next.processor_session_bytes)?,
        compensation_delay_bytes: total
            .compensation_delay_bytes
            .checked_add(next.compensation_delay_bytes)?,
        parameter_event_bytes: total
            .parameter_event_bytes
            .checked_add(next.parameter_event_bytes)?,
        media_window_bytes: total.media_window_bytes.checked_add(next.media_window_bytes)?,
        nested_window_bytes: total.nested_window_bytes.checked_add(next.nested_window_bytes)?,
    })
}

fn validate_audio_runtime_footprint_grant(
    footprint: AudioRuntimeResourceFootprint,
    grant: AudioRuntimeResourceGrant,
) -> Result<(), String> {
    for (category, required, granted) in [
        (
            "runtime occurrences",
            footprint.runtime_occurrences,
            grant.max_runtime_occurrences,
        ),
        (
            "fixed resident bytes",
            footprint.fixed_resident_bytes,
            grant.max_fixed_resident_bytes,
        ),
        (
            "prepared logical bytes",
            footprint.prepared_logical_bytes,
            grant.max_prepared_logical_bytes,
        ),
    ] {
        if required > granted {
            return Err(format!(
                "audio-stem aggregate {category} require {required}, exceeding job grant {granted}"
            ));
        }
    }
    Ok(())
}

struct RenderedAudioStemPcm {
    path: PathBuf,
    _guard: ExportAudioTempFile,
    loudness: AudioLoudnessReport,
}

fn render_audio_stems_to_pcm_f32(
    timeline: &TimelineExportSnapshot,
    prepared_audio: &PreparedTimelineAudioSnapshot,
    range: TimelineRenderRange,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    audio_owner: &mut ExportAudioSourceOwner<'_>,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    report: &mut dyn FnMut(ExportProgress),
) -> Result<Vec<RenderedAudioStemPcm>, JobExecutionResult> {
    struct ActiveStemPcm {
        path: PathBuf,
        guard: ExportAudioTempFile,
        writer: BufWriter<std::fs::File>,
        delivery: AudioProgramDeliveryRuntime,
        loudness: AudioLoudnessAnalyzer,
    }

    if !execution_gate.wait_at_boundary(ExportProgressPhase::Preparing, cancel) {
        return Err(JobExecutionResult::Cancelled);
    }
    let cache = audio_owner.cache(sample_rate).map_err(JobExecutionResult::Failed)?;
    let outputs = prepared_audio.outputs().collect::<Vec<_>>();
    let resource_grant = execution_gate.resource_policy().audio_runtime_grant;

    // Measure each immutable Runtime without retaining the others, then admit
    // the aggregate before the simultaneously-live stem set is constructed.
    let mut aggregate_footprint = AudioRuntimeResourceFootprint::default();
    for output in &outputs {
        let delivery = prepare_timeline_audio_delivery(
            timeline,
            output,
            range,
            sample_rate,
            channel_layout,
            &cache,
            resource_grant,
        )
        .map_err(JobExecutionResult::Failed)?;
        aggregate_footprint =
            add_audio_runtime_footprint(aggregate_footprint, delivery.resource_footprint())
                .ok_or_else(|| {
                    JobExecutionResult::Failed(
                        "audio-stem aggregate Runtime footprint exceeds addressable capacity"
                            .to_owned(),
                    )
                })?;
    }
    validate_audio_runtime_footprint_grant(aggregate_footprint, resource_grant)
        .map_err(JobExecutionResult::Failed)?;

    let (start_sample, total_samples) =
        timeline_audio_sample_range(range, sample_rate).map_err(JobExecutionResult::Failed)?;
    let mut active = Vec::with_capacity(outputs.len());
    for output in &outputs {
        if !execution_gate.wait_at_boundary(ExportProgressPhase::Preparing, cancel) {
            return Err(JobExecutionResult::Cancelled);
        }
        let temp = tempfile::Builder::new()
            .prefix("mondrian-export-stem-")
            .suffix(".f32")
            .tempfile()
            .map_err(|error| {
                JobExecutionResult::Failed(format!(
                    "failed to allocate stem PCM temporary file: {error}"
                ))
            })?;
        let path = temp.path().to_path_buf();
        drop(temp);
        let guard = ExportAudioTempFile::armed(&path);
        let file = std::fs::File::create(&path).map_err(|error| {
            JobExecutionResult::Failed(format!(
                "failed to create stem PCM temporary file {}: {error}",
                path.display()
            ))
        })?;
        let mut delivery = prepare_timeline_audio_delivery(
            timeline,
            output,
            range,
            sample_rate,
            channel_layout,
            &cache,
            resource_grant,
        )
        .map_err(JobExecutionResult::Failed)?;
        if delivery.requires_state_entry() {
            delivery
                .enter_state(AudioContinuityEpoch::new(1), start_sample)
                .map_err(|error| {
                    JobExecutionResult::Failed(format!(
                        "failed to enter audio-stem continuity state: {error}"
                    ))
                })?;
        }
        let loudness =
            AudioLoudnessAnalyzer::new(sample_rate, channel_layout).map_err(|error| {
                JobExecutionResult::Failed(format!(
                    "audio-stem loudness/true-peak contract is unavailable: {error}"
                ))
            })?;
        active.push(ActiveStemPcm {
            path,
            guard,
            writer: BufWriter::new(file),
            delivery,
            loudness,
        });
    }

    let channels = channel_layout.channel_count();
    let chunk_frames_target = (sample_rate as usize / 5).clamp(1_024, 16_384);
    let mut pcm = vec![0.0_f32; chunk_frames_target * channels];
    let mut sample_bytes = Vec::<u8>::with_capacity(chunk_frames_target * channels * 4);
    let mut rendered_samples = 0usize;
    let cache_window_frames = usize::try_from(sample_rate)
        .unwrap_or(usize::MAX)
        .saturating_mul(EXPORT_AUDIO_SOURCE_WINDOW_SECONDS)
        .max(1);
    let cache_window_frames_i64 = i64::try_from(cache_window_frames).unwrap_or(i64::MAX);
    while rendered_samples < total_samples {
        if !execution_gate.wait_at_boundary(ExportProgressPhase::Preparing, cancel) {
            return Err(JobExecutionResult::Cancelled);
        }
        let rendered_samples_i64 = i64::try_from(rendered_samples).map_err(|_| {
            JobExecutionResult::Failed("audio-stem sample position exceeds i64".to_owned())
        })?;
        let chunk_start = start_sample.checked_add(rendered_samples_i64).ok_or_else(|| {
            JobExecutionResult::Failed("audio-stem sample position exceeds i64".to_owned())
        })?;
        let frames_to_cache_boundary = usize::try_from(
            cache_window_frames_i64 - chunk_start.rem_euclid(cache_window_frames_i64),
        )
        .unwrap_or(usize::MAX)
        .max(1);
        let chunk_frames = (total_samples - rendered_samples)
            .min(chunk_frames_target)
            .min(frames_to_cache_boundary)
            .max(1);
        let chunk_samples = chunk_frames * channels;
        for stem in &mut active {
            stem.delivery
                .render_into_cancellable(
                    AudioRenderRequest { start_sample: chunk_start, frames: chunk_frames },
                    &mut pcm[..chunk_samples],
                    cancel,
                )
                .map_err(|error| {
                    if cancel.is_canceled() {
                        JobExecutionResult::Cancelled
                    } else {
                        JobExecutionResult::Failed(format!(
                            "audio-stem Program execution failed: {error}"
                        ))
                    }
                })?;
            stem.loudness.observe_interleaved(&pcm[..chunk_samples]).map_err(|error| {
                JobExecutionResult::Failed(format!(
                    "audio-stem loudness/true-peak analysis failed: {error}"
                ))
            })?;
            sample_bytes.clear();
            for sample in &pcm[..chunk_samples] {
                sample_bytes.extend_from_slice(&sample.to_le_bytes());
            }
            stem.writer.write_all(&sample_bytes).map_err(|error| {
                JobExecutionResult::Failed(format!("failed to write stem PCM: {error}"))
            })?;
        }
        rendered_samples += chunk_frames;
        let ratio = rendered_samples as f32 / total_samples.max(1) as f32;
        report(ExportProgress::preparing(
            (0.02 + 0.70 * ratio).clamp(0.02, 0.72),
        ));
    }

    let mut rendered = Vec::with_capacity(active.len());
    for mut stem in active {
        stem.writer.flush().map_err(|error| {
            JobExecutionResult::Failed(format!("failed to flush stem PCM: {error}"))
        })?;
        let loudness = stem.loudness.finish().map_err(|error| {
            JobExecutionResult::Failed(format!(
                "failed to finish audio-stem loudness/true-peak analysis: {error}"
            ))
        })?;
        rendered.push(RenderedAudioStemPcm { path: stem.path, _guard: stem.guard, loudness });
    }
    Ok(rendered)
}

fn render_timeline_audio_to_pcm_f32(
    output_path: &Path,
    timeline: &TimelineExportSnapshot,
    prepared_audio: &PreparedTimelineAudioOutputSnapshot,
    range: TimelineRenderRange,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    audio_owner: &mut ExportAudioSourceOwner<'_>,
    cancel: &ExecutionCancellationToken,
    execution_gate: &service::ExportExecutionGate,
    report: &mut dyn FnMut(ExportProgress),
    analysis_out: &mut Option<AudioLoudnessReport>,
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
    let cache = match audio_owner.cache(sample_rate) {
        Ok(cache) => cache,
        Err(error) => return JobExecutionResult::Failed(error),
    };
    {
        let mut delivery = match prepare_timeline_audio_delivery(
            timeline,
            prepared_audio,
            range,
            sample_rate,
            channel_layout,
            &cache,
            execution_gate.resource_policy().audio_runtime_grant,
        ) {
            Ok(delivery) => delivery,
            Err(error) => return JobExecutionResult::Failed(error),
        };

        let (start_sample, total_samples) = match timeline_audio_sample_range(range, sample_rate) {
            Ok(sample_range) => sample_range,
            Err(error) => return JobExecutionResult::Failed(error),
        };
        let mut loudness = match AudioLoudnessAnalyzer::new(sample_rate, channel_layout) {
            Ok(analyzer) => analyzer,
            Err(error) => {
                return JobExecutionResult::Failed(format!(
                    "导出音频响度/真峰值分析合同不可用: {error}"
                ));
            }
        };
        if total_samples == 0 {
            return match loudness.finish() {
                Ok(report) => {
                    *analysis_out = Some(report);
                    JobExecutionResult::ReversibleWorkCompleted
                }
                Err(error) => JobExecutionResult::Failed(format!(
                    "完成空导出音频响度/真峰值分析失败: {error}"
                )),
            };
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
            if let Err(error) = loudness.observe_interleaved(&pcm[..chunk_samples]) {
                return JobExecutionResult::Failed(format!("导出音频响度/真峰值分析失败: {error}"));
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
        *analysis_out = match loudness.finish() {
            Ok(report) => Some(report),
            Err(error) => {
                return JobExecutionResult::Failed(format!(
                    "完成导出音频响度/真峰值分析失败: {error}"
                ));
            }
        };
        JobExecutionResult::ReversibleWorkCompleted
    }
}

fn timeline_audio_sample_range(
    range: TimelineRenderRange,
    sample_rate: u32,
) -> Result<(i64, usize), String> {
    if range.total_frames == 0 || sample_rate == 0 {
        return Ok((0, 0));
    }
    let rate = AudioSampleRate::new(sample_rate).map_err(|error| error.to_string())?;
    let time_range = range.time_range()?;
    let start_time = time_range.start;
    let end_time = time_range.end().map_err(|error| error.to_string())?;
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
        let progressive_position = || {
            range
                .evaluation_frame(index)
                .map(|frame| FramePosition::new(frame, timeline.sequence.time_base()))
        };
        let positions = if timeline.sequence.settings.field_order
            == mondrian_core::timeline_data::FieldOrder::Progressive
        {
            [
                Some(progressive_position().map_err(JobExecutionResult::Failed)?),
                None,
            ]
        } else {
            [
                Some(
                    range
                        .interlaced_evaluation_position(index, false)
                        .map_err(JobExecutionResult::Failed)?,
                ),
                Some(
                    range
                        .interlaced_evaluation_position(index, true)
                        .map_err(JobExecutionResult::Failed)?,
                ),
            ]
        };
        for position in positions.into_iter().flatten() {
            let closure = prepare_export_visual_frame_closure(
                timeline,
                visual_session,
                cancel,
                &timeline.sequence,
                position,
                root_resolution,
                root_color_context.clone(),
            )
            .map_err(|reason| {
                if cancel.is_canceled() {
                    JobExecutionResult::Cancelled
                } else {
                    JobExecutionResult::Failed(format!(
                        "export visual closure preflight failed at root sample {position:?}: {reason}"
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
                        "export visual closure exceeds its CPU working-set grant at root sample {position:?}: {error}"
                    ))
                })?;
            if cancel.is_canceled() {
                return Err(JobExecutionResult::Cancelled);
            }
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
    observations: ExportRenderObservations<'_>,
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
        observations,
        &mut |_index, canvas| {
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

#[allow(clippy::too_many_arguments)]
fn write_native_image_sequence_frames(
    directory: &Path,
    format: crate::preset::ImageSequenceFormat,
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
    observations: ExportRenderObservations<'_>,
) -> JobExecutionResult {
    let frame_contract = export_frame_contract(delivery);
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
        observations,
        &mut |index, canvas| {
            let path = directory.join(frame_file_name(index, format));
            write_native_tiff_float_frame(&path, width, height, alpha_mode, frame_contract, canvas)
                .map_err(JobExecutionResult::Failed)
        },
    )
}

#[derive(Debug, Clone, Copy)]
struct ExportRenderInitialDiagnostics {
    asset_issue_summary: VideoColorDiagnosticIssueAggregate,
    audio_analysis: Option<AudioLoudnessReport>,
}

#[derive(Debug, Clone, Copy)]
struct ExportRenderObservations<'a> {
    broadcast_qc_profile: Option<&'a mondrian_broadcast::BroadcastQcProfile>,
    initial_diagnostics: ExportRenderInitialDiagnostics,
}

#[derive(Debug, Clone, Copy)]
struct ExportDeliveryPixelContract {
    frame: ExportFrameContract,
    legalizer: SignalLegalizer,
}

type ExportFrameSink<'a> = dyn FnMut(u64, &mut Vec<u8>) -> Result<(), JobExecutionResult> + 'a;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResidentHevcExportPlan {
    bit_depth: ResidentEncodeBitDepth,
    colorimetry: ResidentEncodeColorimetry,
    full_range: bool,
    keyframe_interval_frames: u32,
    max_b_frames: u32,
    quantizer: u8,
    surface_pool_size: u32,
}

fn qualify_resident_hevc_export(
    timeline: &TimelineExportSnapshot,
    delivery: &ResolvedExportDeliveryContract,
    alpha_mode: ExportAlphaMode,
    policy: service::ExportExecutionResourcePolicy,
) -> Result<ResidentHevcExportPlan, ExportResidentEncodeBlocker> {
    if delivery.field_order != mondrian_core::timeline_data::FieldOrder::Progressive {
        return Err(ExportResidentEncodeBlocker::Signal);
    }
    let ResolvedExportArtifactEncoding::MediaFile { video, .. } = &delivery.artifact else {
        return Err(ExportResidentEncodeBlocker::UnsupportedCodec);
    };
    let (profile, rate_control) = match video {
        VideoCodecConfig::Hevc { profile, rate_control } => (*profile, *rate_control),
        _ => return Err(ExportResidentEncodeBlocker::UnsupportedCodec),
    };
    if delivery.chroma_sampling != ExportChromaSampling::Yuv420 {
        return Err(ExportResidentEncodeBlocker::UnsupportedCodec);
    }
    if alpha_mode != ExportAlphaMode::FlattenBlack {
        return Err(ExportResidentEncodeBlocker::AlphaPreservation);
    }
    if delivery.legalizer.is_active() {
        return Err(ExportResidentEncodeBlocker::Legalizer);
    }
    if timeline
        .sequence
        .settings
        .delivery
        .static_hdr_metadata_policy
        .writes_authored_metadata()
    {
        return Err(ExportResidentEncodeBlocker::StaticHdrMetadata);
    }
    if rate_control.max_bitrate_kbps.is_some() || rate_control.buffer_size_kbits.is_some() {
        return Err(ExportResidentEncodeBlocker::RateControl);
    }
    let bit_depth = match (profile, delivery.bit_depth) {
        (HevcProfile::Main, DeliveryBitDepth::Eight) => ResidentEncodeBitDepth::Eight,
        (HevcProfile::Main10, DeliveryBitDepth::Ten) => ResidentEncodeBitDepth::Ten,
        _ => return Err(ExportResidentEncodeBlocker::UnsupportedCodec),
    };
    let colorimetry = match delivery.color_target.color_space {
        ColorSpace::Rec709 => ResidentEncodeColorimetry::Rec709,
        ColorSpace::Rec2100Pq => ResidentEncodeColorimetry::Rec2100Pq,
        _ => return Err(ExportResidentEncodeBlocker::Signal),
    };
    let full_range = match delivery.video_range {
        VideoRange::Full => true,
        VideoRange::Legal => false,
    };
    if full_range && colorimetry == ResidentEncodeColorimetry::Rec2100Pq {
        return Err(ExportResidentEncodeBlocker::Signal);
    }
    let (keyframe_interval_frames, max_b_frames) = match delivery.video_coding {
        crate::video_encoding::ResolvedVideoCodingStructure::H26xLongGop {
            keyframe_interval_frames,
            max_b_frames,
            closed_gop: true,
            scene_cut: crate::video_encoding::VideoSceneCutPolicy::Disabled,
        } => (keyframe_interval_frames, u32::from(max_b_frames)),
        _ => return Err(ExportResidentEncodeBlocker::CodingStructure),
    };
    if policy.resident_encoder_surfaces < 2 {
        return Err(ExportResidentEncodeBlocker::ResourceGrant);
    }
    let bytes_per_pixel_x2 = match bit_depth {
        ResidentEncodeBitDepth::Eight => 3_u128,
        ResidentEncodeBitDepth::Ten => 6_u128,
    };
    let pool_bytes_x2 = u128::from(delivery.resolution.width)
        .checked_mul(u128::from(delivery.resolution.height))
        .and_then(|pixels| pixels.checked_mul(bytes_per_pixel_x2))
        .and_then(|bytes| bytes.checked_mul(u128::from(policy.resident_encoder_surfaces)))
        .ok_or(ExportResidentEncodeBlocker::ResourceGrant)?;
    if pool_bytes_x2 > u128::from(policy.resident_encoder_surface_bytes).saturating_mul(2) {
        return Err(ExportResidentEncodeBlocker::ResourceGrant);
    }
    Ok(ResidentHevcExportPlan {
        bit_depth,
        colorimetry,
        full_range,
        keyframe_interval_frames,
        max_b_frames,
        quantizer: rate_control.crf,
        surface_pool_size: policy.resident_encoder_surfaces,
    })
}

impl ExportDeliveryPixelContract {
    const fn new(frame: ExportFrameContract, legalizer: SignalLegalizer) -> Self {
        Self { frame, legalizer }
    }

    const fn unmodified(frame: ExportFrameContract) -> Self {
        Self::new(frame, SignalLegalizer::Off)
    }
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
    observations: ExportRenderObservations<'_>,
    write_frame: &mut ExportFrameSink<'_>,
) -> JobExecutionResult {
    let frame_contract = export_frame_contract(delivery);
    let root_color_context = match resolved_export_color_context(timeline, delivery) {
        Ok(context) => context,
        Err(error) => return JobExecutionResult::Failed(error),
    };
    let total = range.total_frames.max(1);
    let mut canvas = vec![0u8; frame_contract.canvas_len(width, height)];
    let mut first_field_canvas = Vec::new();
    let mut second_field_canvas = Vec::new();
    let picture_sampling = mondrian_renderer::picture_sampling::ProgramPictureSampling::new(
        timeline.sequence.time_base(),
        delivery.field_order,
    );
    let mut diagnostics = ExportJobDiagnostics::default();
    diagnostics
        .color
        .record_asset_issue_summary(observations.initial_diagnostics.asset_issue_summary);
    diagnostics.audio = observations.initial_diagnostics.audio_analysis;
    let mut broadcast_qc = match prepare_export_broadcast_qc(
        observations.broadcast_qc_profile,
        width,
        height,
        delivery.color_target.color_space,
    ) {
        Ok(session) => session,
        Err(error) => return JobExecutionResult::Failed(error),
    };

    for index in 0..total {
        if !execution_gate.wait_at_boundary(ExportProgressPhase::Rendering, cancel) {
            finish_incomplete_export_broadcast_qc(
                &mut broadcast_qc,
                &mut diagnostics,
                report_diagnostics,
            );
            return JobExecutionResult::Cancelled;
        }

        let mut frame_color_counts = InputColorResolutionSourceCounts::default();
        let mut frame_stage_diagnostics = RenderColorStageDiagnostics::default();
        let mut frame_composite_diagnostics = TimelineCompositeDiagnostics::default();
        let samples = match picture_sampling.samples(index) {
            Ok(samples) => samples,
            Err(error) => return JobExecutionResult::Failed(error.to_string()),
        };
        let render_result = match samples {
            mondrian_renderer::picture_sampling::ProgramPictureSamples::Progressive(_) => {
                let timeline_frame = match range.evaluation_frame(index) {
                    Ok(frame) => frame,
                    Err(error) => return JobExecutionResult::Failed(error),
                };
                render_timeline_frame_into_with_session_cancellable(
                    timeline,
                    timeline_frame,
                    width,
                    height,
                    alpha_mode,
                    root_color_context.clone(),
                    ExportDeliveryPixelContract::new(frame_contract, delivery.legalizer),
                    &mut canvas,
                    Some(&mut frame_color_counts),
                    Some(&mut frame_stage_diagnostics),
                    Some(&mut frame_composite_diagnostics),
                    Some(&mut diagnostics.color),
                    visual_session,
                    cancel,
                )
            }
            mondrian_renderer::picture_sampling::ProgramPictureSamples::Interlaced {
                first,
                second,
            } => {
                let first = match range.interlaced_evaluation_position(index, false) {
                    Ok(position) => first.with_position(position),
                    Err(error) => return JobExecutionResult::Failed(error),
                };
                let second = match range.interlaced_evaluation_position(index, true) {
                    Ok(position) => second.with_position(position),
                    Err(error) => return JobExecutionResult::Failed(error),
                };
                let render_first = render_timeline_sample_into_with_session_cancellable(
                    timeline,
                    first.position(),
                    width,
                    height,
                    alpha_mode,
                    root_color_context.clone(),
                    ExportDeliveryPixelContract::new(frame_contract, delivery.legalizer),
                    &mut first_field_canvas,
                    Some(&mut frame_color_counts),
                    Some(&mut frame_stage_diagnostics),
                    Some(&mut frame_composite_diagnostics),
                    Some(&mut diagnostics.color),
                    visual_session,
                    cancel,
                );
                render_first
                    .and_then(|()| {
                        render_timeline_sample_into_with_session_cancellable(
                            timeline,
                            second.position(),
                            width,
                            height,
                            alpha_mode,
                            root_color_context.clone(),
                            ExportDeliveryPixelContract::new(frame_contract, delivery.legalizer),
                            &mut second_field_canvas,
                            Some(&mut frame_color_counts),
                            Some(&mut frame_stage_diagnostics),
                            Some(&mut frame_composite_diagnostics),
                            Some(&mut diagnostics.color),
                            visual_session,
                            cancel,
                        )
                    })
                    .and_then(|()| {
                        crate::interlaced_delivery::assemble_interlaced_program_frame(
                            frame_contract,
                            width,
                            height,
                            first,
                            &first_field_canvas,
                            second,
                            &second_field_canvas,
                            &mut canvas,
                        )
                        .map_err(|error| error.to_string())
                    })
            }
        };
        diagnostics.color.record_frame_diagnostics(
            frame_color_counts,
            frame_stage_diagnostics,
            frame_composite_diagnostics,
        );
        diagnostics.visual = visual_session.visual_diagnostics();
        report_diagnostics(diagnostics.clone());
        match render_result {
            Ok(()) => {}
            Err(_) if cancel.is_canceled() => {
                finish_incomplete_export_broadcast_qc(
                    &mut broadcast_qc,
                    &mut diagnostics,
                    report_diagnostics,
                );
                return JobExecutionResult::Cancelled;
            }
            Err(err) => {
                finish_incomplete_export_broadcast_qc(
                    &mut broadcast_qc,
                    &mut diagnostics,
                    report_diagnostics,
                );
                return JobExecutionResult::Failed(format!(
                    "渲染时间线图像失败（output_frame={}）: {}",
                    index, err
                ));
            }
        }

        if let Some(session) = broadcast_qc.as_mut() {
            let rgba = match frame_contract.to_rgba_f32(&canvas) {
                Ok(rgba) => rgba,
                Err(error) => {
                    finish_incomplete_export_broadcast_qc(
                        &mut broadcast_qc,
                        &mut diagnostics,
                        report_diagnostics,
                    );
                    return JobExecutionResult::Failed(format!(
                        "broadcast QC delivery-picture readback failed: {error}"
                    ));
                }
            };
            if let Err(error) = session.push(mondrian_broadcast::BroadcastQcFrame {
                frame_index: index,
                rgba: bytemuck::cast_slice(&rgba),
            }) {
                finish_incomplete_export_broadcast_qc(
                    &mut broadcast_qc,
                    &mut diagnostics,
                    report_diagnostics,
                );
                return JobExecutionResult::Failed(format!(
                    "broadcast QC frame analysis failed: {error}"
                ));
            }
        }

        if let ResolvedExportArtifactEncoding::ImageSequence { format } = delivery.artifact
            && let Err(error) =
                validate_image_sequence_frame_samples(format, frame_contract, &canvas)
        {
            return JobExecutionResult::Failed(error);
        }
        if let Err(outcome) = write_frame(index, &mut canvas) {
            finish_incomplete_export_broadcast_qc(
                &mut broadcast_qc,
                &mut diagnostics,
                report_diagnostics,
            );
            return outcome;
        }

        let rendered = index + 1;
        let ratio = rendered as f32 / total as f32;
        let progress = (0.18 + 0.72 * ratio).clamp(0.18, 0.92);
        report(ExportProgress::rendering(progress, rendered, total));
    }

    if let Some(session) = broadcast_qc.take() {
        let report = match session.finish(true) {
            Ok(report) => report,
            Err(error) => {
                return JobExecutionResult::Failed(format!(
                    "broadcast QC report finalization failed: {error}"
                ));
            }
        };
        let verdict = report.verdict;
        diagnostics.broadcast_qc = Some(report);
        report_diagnostics(diagnostics.clone());
        if matches!(
            verdict,
            mondrian_broadcast::BroadcastQcVerdict::Fail
                | mondrian_broadcast::BroadcastQcVerdict::Incomplete
        ) {
            return JobExecutionResult::Failed(format!(
                "broadcast QC publication gate rejected profile {} with verdict {verdict:?}",
                diagnostics
                    .broadcast_qc
                    .as_ref()
                    .map(|report| report.profile_id.as_str())
                    .unwrap_or("<missing>")
            ));
        }
    }

    JobExecutionResult::ReversibleWorkCompleted
}

fn prepare_export_broadcast_qc(
    profile: Option<&mondrian_broadcast::BroadcastQcProfile>,
    width: u32,
    height: u32,
    color_space: ColorSpace,
) -> Result<Option<mondrian_broadcast::BroadcastQcSession>, String> {
    let Some(profile) = profile else {
        return Ok(None);
    };
    if profile.observation_tap
        != mondrian_broadcast::BroadcastQcObservationTap::DeliveryPictureAfterLegalizer
    {
        return Err(
            "Export currently admits broadcast QC only at the post-Legalizer delivery-picture tap"
                .to_owned(),
        );
    }
    if profile.active_picture.raster_width != width
        || profile.active_picture.raster_height != height
    {
        return Err(format!(
            "broadcast QC profile raster {}x{} differs from delivery {}x{}",
            profile.active_picture.raster_width,
            profile.active_picture.raster_height,
            width,
            height
        ));
    }
    if profile.signal_color_space != color_space {
        return Err(format!(
            "broadcast QC profile color {:?} differs from delivery {:?}",
            profile.signal_color_space, color_space
        ));
    }
    mondrian_broadcast::BroadcastQcSession::new(profile.clone())
        .map(Some)
        .map_err(|error| format!("broadcast QC profile is invalid: {error}"))
}

fn finish_incomplete_export_broadcast_qc(
    session: &mut Option<mondrian_broadcast::BroadcastQcSession>,
    diagnostics: &mut ExportJobDiagnostics,
    report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
) {
    let Some(session) = session.take() else {
        return;
    };
    if let Ok(report) = session.finish(false) {
        diagnostics.broadcast_qc = Some(report);
        report_diagnostics(diagnostics.clone());
    }
}

/// Build the export output boundary from the resolved color context.
///
/// The renderer resolves the product-level output-transform intent. Preview
/// and export therefore cannot independently reinterpret Mondrian Standard,
/// an explicit OCIO view, or a colorimetric delivery.
fn export_output_boundary_from_context(
    color_context: &ProgramColorContext,
) -> Result<ProgramOutputBoundary, String> {
    ProgramOutputModule::boundary(ProgramOutputRole::Export, color_context)
        .map_err(|error| error.to_string())
}

fn resolved_export_color_context(
    timeline: &TimelineExportSnapshot,
    delivery: &ResolvedExportDeliveryContract,
) -> Result<ProgramColorContext, String> {
    let context = timeline
        .sequence
        .settings
        .root_program_color_context(&timeline.color_environment)
        .map_err(|error| format!("invalid root Program color context: {error}"))?;
    context
        .for_export_output(
            delivery.color_target.color_space,
            delivery.color_target.tone_map,
            delivery.color_target.output_transform.clone(),
        )
        .map_err(|error| format!("invalid export Program color context: {error}"))
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
    let frame_contract =
        ExportFrameContract::from_bit_depth(timeline.sequence.settings.delivery.bit_depth);
    let color_context = timeline
        .sequence
        .settings
        .root_program_color_context(&timeline.color_environment)
        .map_err(|error| format!("invalid root Program color context: {error}"))?;
    render_timeline_frame_into_with_session(
        timeline,
        timeline_frame,
        width,
        height,
        alpha_mode,
        color_context,
        ExportDeliveryPixelContract::unmodified(frame_contract),
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
    delivery_pixels: ExportDeliveryPixelContract,
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
        delivery_pixels,
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
    delivery_pixels: ExportDeliveryPixelContract,
    canvas: &mut Vec<u8>,
    input_color_counts: Option<&mut InputColorResolutionSourceCounts>,
    stage_diagnostics: Option<&mut RenderColorStageDiagnostics>,
    composite_diagnostics: Option<&mut TimelineCompositeDiagnostics>,
    export_diagnostics: Option<&mut ExportJobColorDiagnostics>,
    visual_session: &mut ExportVisualRenderSession,
    cancellation: &ExecutionCancellationToken,
) -> Result<(), String> {
    render_timeline_sample_into_with_session_cancellable(
        timeline,
        FramePosition::new(timeline_frame, timeline.sequence.time_base()),
        width,
        height,
        alpha_mode,
        color_context,
        delivery_pixels,
        canvas,
        input_color_counts,
        stage_diagnostics,
        composite_diagnostics,
        export_diagnostics,
        visual_session,
        cancellation,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_timeline_sample_into_with_session_cancellable(
    timeline: &TimelineExportSnapshot,
    timeline_position: FramePosition,
    width: u32,
    height: u32,
    alpha_mode: ExportAlphaMode,
    color_context: ProgramColorContext,
    delivery_pixels: ExportDeliveryPixelContract,
    canvas: &mut Vec<u8>,
    input_color_counts: Option<&mut InputColorResolutionSourceCounts>,
    stage_diagnostics: Option<&mut RenderColorStageDiagnostics>,
    composite_diagnostics: Option<&mut TimelineCompositeDiagnostics>,
    export_diagnostics: Option<&mut ExportJobColorDiagnostics>,
    visual_session: &mut ExportVisualRenderSession,
    cancellation: &ExecutionCancellationToken,
) -> Result<(), String> {
    let required_len = delivery_pixels.frame.canvas_len(width, height);
    if canvas.len() != required_len {
        canvas.resize(required_len, 0);
    }

    let mut render_context = ExportFrameRenderContext {
        media: &timeline.media,
        color_environment: &timeline.color_environment,
        alpha_mode,
        delivery_pixels,
        input_color_counts,
        stage_diagnostics,
        composite_diagnostics,
        export_diagnostics,
        visual_session,
        cancellation,
    };

    render_sequence_sample_into(
        timeline,
        &mut render_context,
        &timeline.sequence,
        timeline_position,
        Resolution { width, height },
        color_context,
        SequenceRenderTarget::Deliverable(canvas),
    )
}

#[allow(clippy::too_many_arguments)]
fn render_timeline_frame_resident_with_session_cancellable(
    timeline: &TimelineExportSnapshot,
    timeline_frame: i64,
    width: u32,
    height: u32,
    alpha_mode: ExportAlphaMode,
    color_context: ProgramColorContext,
    delivery_pixels: ExportDeliveryPixelContract,
    input_color_counts: Option<&mut InputColorResolutionSourceCounts>,
    stage_diagnostics: Option<&mut RenderColorStageDiagnostics>,
    composite_diagnostics: Option<&mut TimelineCompositeDiagnostics>,
    export_diagnostics: Option<&mut ExportJobColorDiagnostics>,
    visual_session: &mut ExportVisualRenderSession,
    cancellation: &ExecutionCancellationToken,
) -> Result<GpuResidentEncoderInputLease, String> {
    let mut output = None;
    let mut render_context = ExportFrameRenderContext {
        media: &timeline.media,
        color_environment: &timeline.color_environment,
        alpha_mode,
        delivery_pixels,
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
        SequenceRenderTarget::Resident(&mut output),
    )?;
    output.ok_or_else(|| {
        "resident export root produced no GPU output (empty-frame lowering unavailable)".to_owned()
    })
}

enum SequenceRenderTarget<'a> {
    Working(&'a mut Option<CpuColorFrame>),
    Deliverable(&'a mut Vec<u8>),
    Resident(&'a mut Option<GpuResidentEncoderInputLease>),
}

#[derive(Clone)]
struct PreparedExportTemporalLayer {
    frame: CpuColorFrame,
    source_resolution: Resolution,
    source_to_display_affine: [f32; 6],
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

    fn active_adapter_identity(
        &mut self,
    ) -> Option<crate::hardware_encoding::ActiveGraphicsAdapterIdentity> {
        self.gpu_output.active_adapter_identity()
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
        let start_frame = range
            .source_start
            .to_frame_position(range.sequence_frame_rate, FrameRounding::Floor)
            .map_err(|error| error.to_string())?
            .frame;
        let end_frame_exclusive = range
            .time_range()?
            .end()
            .map_err(|error| error.to_string())?
            .to_frame_position(range.sequence_frame_rate, FrameRounding::Ceil)
            .map_err(|error| error.to_string())?
            .frame;
        let dependencies = crate::prepare_timeline_export_dependencies(
            root,
            sequences,
            TimelineExportRange::WorkArea { start_frame, end_frame_exclusive },
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
    delivery_pixels: ExportDeliveryPixelContract,
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
    source_contract: PreviewSourceColorContract,
    alpha_interpretation: AlphaInterpretation,
    preparation_intent: SourceFramePreparationIntent,
    decode_resolution: Resolution,
    source_resolution: Resolution,
    picture_geometry: ResolvedPictureGeometry,
    camera_raw: Option<mondrian_media::CameraRawDecodeIntent>,
}

impl ExportDecodeCacheKey {
    #[allow(clippy::too_many_arguments)]
    fn new(
        asset_id: AssetId,
        dependency: &crate::preset::ExportMediaDependency,
        source_sample: mondrian_core::SourceSampleTarget,
        source_contract: PreviewSourceColorContract,
        preparation_intent: SourceFramePreparationIntent,
        alpha_interpretation: AlphaInterpretation,
        decode_resolution: Resolution,
        source_resolution: Resolution,
        picture_geometry: ResolvedPictureGeometry,
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
        let camera_raw = dependency
            .source_video_stream
            .as_ref()
            .and_then(|stream| stream.camera_raw.as_ref())
            .map(|raw| {
                mondrian_media::CameraRawDecodeIntent::new(
                    raw.adapter,
                    dependency.interpretation.camera_raw,
                )
                .map_err(|error| {
                    format!(
                        "asset={} path={} invalid camera RAW export contract: {error}",
                        asset_id,
                        dependency.path.display()
                    )
                })
            })
            .transpose()?;
        Ok(Self {
            asset_id,
            source_path: dependency.path.clone(),
            source_fingerprint: dependency.source_fingerprint,
            video_stream_index,
            source_sample,
            source_contract,
            alpha_interpretation,
            preparation_intent,
            decode_resolution,
            source_resolution,
            picture_geometry,
            camera_raw,
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
        .root_program_color_context(&timeline.color_environment)
        .map_err(|error| format!("invalid root Program color context: {error}"))?;
    let closure = prepare_export_visual_frame_closure(
        timeline,
        &mut visual_session,
        &cancellation,
        &timeline.sequence,
        FramePosition::new(timeline_frame, timeline.sequence.time_base()),
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
                | TimelineRenderPlanElement::TimelineGrade(_)
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
    let resolution = color_context.missing_metadata_policy().resolve_asset_input_decision(
        media.color_space_override,
        dependency.interpretation,
        dependency
            .color_diagnostic
            .as_ref()
            .and_then(mondrian_media::VideoColorDiagnostic::executable_color_space),
        color_context.working_color_space(),
    );
    counts.record(resolution.source);
    Ok(())
}

type PreparedExportVisualClosure =
    PreparedVisualFrameClosure<Vec<PreparedExportHeterogeneousElement>>;

enum PreparedExportVisualOutput {
    Root,
    NestedCpu(CpuColorFrame),
    NestedGpu(GpuColorFrameHandle),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExportPreparedVisualMode {
    Cpu,
    Gpu,
}

type ExportNodeInputs<'a> = PreparedVisualExecutionNodeInputs<
    'a,
    Vec<PreparedExportHeterogeneousElement>,
    PreparedExportVisualOutput,
>;

struct ExportPreparedVisualAdapter<'context, 'resources, 'target> {
    context: &'context mut ExportFrameRenderContext<'resources>,
    root_target: Option<SequenceRenderTarget<'target>>,
    mode: ExportPreparedVisualMode,
}

impl PreparedVisualExecutionAdapter<Vec<PreparedExportHeterogeneousElement>>
    for ExportPreparedVisualAdapter<'_, '_, '_>
{
    type Output = PreparedExportVisualOutput;
    type Error = String;

    fn materialize_node(
        &mut self,
        inputs: ExportNodeInputs<'_>,
    ) -> Result<Self::Output, Self::Error> {
        if inputs.is_root() {
            let target = self
                .root_target
                .take()
                .ok_or_else(|| "prepared export root target was already consumed".to_owned())?;
            match self.mode {
                ExportPreparedVisualMode::Cpu => {
                    render_prepared_visual_node_into(self.context, &inputs, target)?;
                }
                ExportPreparedVisualMode::Gpu => {
                    render_prepared_visual_node_gpu_into(self.context, &inputs, target)?;
                }
            }
            return Ok(PreparedExportVisualOutput::Root);
        }

        let inbound = inputs.inbound_binding().ok_or_else(|| {
            format!(
                "prepared export nested node {} has no inbound binding",
                inputs.node().id().index()
            )
        })?;
        if self.mode == ExportPreparedVisualMode::Gpu {
            let output = render_prepared_visual_node_gpu(self.context, &inputs, false)?;
            let parent_working = inbound.parent_working_color_space();
            let (output, stage_diagnostics) =
                self.context.visual_session.gpu_output.convert_gpu_working_frame(
                    &output,
                    parent_working,
                    inputs.node().color_context().engine().clone(),
                    self.context.cancellation,
                )?;
            if let Some(diagnostics) = self.context.stage_diagnostics.as_deref_mut() {
                diagnostics.accumulate(stage_diagnostics);
            }
            return Ok(PreparedExportVisualOutput::NestedGpu(output));
        }

        let mut output = None;
        render_prepared_visual_node_into(
            self.context,
            &inputs,
            SequenceRenderTarget::Working(&mut output),
        )?;
        let mut frame = output.ok_or_else(|| {
            format!(
                "nested sequence produced no working frame: {}",
                inputs.node().sequence_id()
            )
        })?;
        let parent_working = inbound.parent_working_color_space();
        if frame.descriptor().color_space.working() != Some(parent_working) {
            let converted = WorkingColorModule::execute_cpu(
                &frame,
                parent_working,
                inputs.node().color_context().engine().clone(),
                self.context.visual_session.composite_scratch.color_execution_mut(),
            )
            .map_err(|error| format!("nested working-space transform failed: {error}"))?;
            if let Some(diagnostics) = self.context.stage_diagnostics.as_deref_mut() {
                diagnostics.accumulate(converted.stage_diagnostics());
            }
            frame = converted.into_frame();
        }
        Ok(PreparedExportVisualOutput::NestedCpu(frame))
    }
}

fn prepared_export_nested_output<'a>(
    inputs: &'a ExportNodeInputs<'_>,
    placement: TimelineClipExecutionRef,
    sample: PreparedVisualNestedSample,
) -> Result<&'a CpuColorFrame, String> {
    match inputs.nested_output(placement, sample) {
        Some(PreparedExportVisualOutput::NestedCpu(frame)) => Ok(frame),
        Some(PreparedExportVisualOutput::NestedGpu(_)) => Err(format!(
            "prepared export child for Clip {} returned a GPU output to the CPU Adapter",
            placement.clip_id
        )),
        Some(PreparedExportVisualOutput::Root) => Err(format!(
            "prepared export child for Clip {} returned the root output",
            placement.clip_id
        )),
        None => Err(format!(
            "prepared export child output is unavailable for Clip {} ({sample:?})",
            placement.clip_id
        )),
    }
}

fn prepared_export_nested_gpu_output(
    inputs: &ExportNodeInputs<'_>,
    placement: TimelineClipExecutionRef,
    sample: PreparedVisualNestedSample,
) -> Result<GpuColorFrameHandle, String> {
    match inputs.nested_output(placement, sample) {
        Some(PreparedExportVisualOutput::NestedGpu(frame)) => Ok(frame.clone()),
        Some(PreparedExportVisualOutput::NestedCpu(_)) => Err(format!(
            "prepared export child for Clip {} returned a CPU output to the GPU Adapter",
            placement.clip_id
        )),
        Some(PreparedExportVisualOutput::Root) => Err(format!(
            "prepared export child for Clip {} returned the root output",
            placement.clip_id
        )),
        None => Err(format!(
            "prepared export child GPU output is unavailable for Clip {} ({sample:?})",
            placement.clip_id
        )),
    }
}

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
    root_position: FramePosition,
    root_resolution: Resolution,
    root_color_context: ProgramColorContext,
) -> Result<PreparedExportVisualClosure, String> {
    let visual_session = std::cell::RefCell::new(visual_session);
    prepare_visual_frame_closure(
        PreparedVisualFrameClosureRequest {
            root_sequence,
            sequences: &timeline.sequences,
            root_position,
            root_resolution,
            root_color_context,
            child_canvas_policy: PreparedVisualChildCanvasPolicy::Authored,
        },
        |sequence| visual_session.borrow_mut().prepare_program(sequence),
        |program, position, resolution, _color_context, _normalized_preview_resolution_scale| {
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
                        TimelineEvaluationRequest::export(position),
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
    render_sequence_sample_into(
        timeline,
        context,
        sequence,
        FramePosition::new(timeline_frame, sequence.time_base()),
        resolution,
        color_context,
        target,
    )
}

fn render_sequence_sample_into(
    timeline: &TimelineExportSnapshot,
    context: &mut ExportFrameRenderContext<'_>,
    sequence: &mondrian_timeline::sequence::Sequence,
    timeline_position: FramePosition,
    resolution: Resolution,
    color_context: ProgramColorContext,
    target: SequenceRenderTarget<'_>,
) -> Result<(), String> {
    let closure = prepare_export_visual_frame_closure(
        timeline,
        context.visual_session,
        context.cancellation,
        sequence,
        timeline_position,
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
    let use_gpu = matches!(
        &target,
        SequenceRenderTarget::Deliverable(_) | SequenceRenderTarget::Resident(_)
    ) && prepared_export_visual_closure_supports_gpu(
        &closure,
        &mut context.visual_session.composite_scratch,
    ) && context.visual_session.gpu_output.begin_visual_frame().is_ok();
    let mode = if use_gpu {
        ExportPreparedVisualMode::Gpu
    } else {
        ExportPreparedVisualMode::Cpu
    };
    let mut adapter = ExportPreparedVisualAdapter { context, root_target: Some(target), mode };
    match execute_prepared_visual_closure(&closure, &mut adapter) {
        Ok(PreparedExportVisualOutput::Root) => Ok(()),
        Ok(PreparedExportVisualOutput::NestedCpu(_) | PreparedExportVisualOutput::NestedGpu(_)) => {
            Err("prepared export execution returned a nested frame for the root".to_owned())
        }
        Err(PreparedVisualExecutionError::Structure(error)) => Err(format!(
            "prepared export visual execution failed closed: {error}"
        )),
        Err(PreparedVisualExecutionError::Adapter(error)) => Err(error),
    }
}

fn prepared_export_visual_closure_supports_gpu(
    closure: &PreparedExportVisualClosure,
    scratch: &mut TimelineCompositeScratch,
) -> bool {
    closure.nodes().iter().all(|node| {
        node.evaluation().payload().is_empty()
            && node.evaluation().temporal_batches().is_empty()
            && node
                .evaluation()
                .plan()
                .elements
                .iter()
                .all(|element| prepared_export_element_supports_gpu(element, scratch))
    })
}

fn prepared_export_element_supports_gpu(
    element: &TimelineRenderPlanElement,
    scratch: &mut TimelineCompositeScratch,
) -> bool {
    let graph_supported = |graph: &Arc<mondrian_effects::CompiledEffectGraph>,
                           scratch: &mut TimelineCompositeScratch| {
        scratch.get_or_lower_effect_gpu_plan(graph).is_ok()
    };
    match element {
        TimelineRenderPlanElement::Media(media) => graph_supported(&media.effect_graph, scratch),
        TimelineRenderPlanElement::BasicTitle(title) => {
            graph_supported(&title.effect_graph, scratch)
        }
        TimelineRenderPlanElement::NestedSequence(nested) => {
            graph_supported(&nested.effect_graph, scratch)
        }
        TimelineRenderPlanElement::SolidColor(solid) => {
            graph_supported(&solid.effect_graph, scratch)
        }
        TimelineRenderPlanElement::Adjustment(adjustment) => {
            graph_supported(&adjustment.effect_graph, scratch)
        }
        TimelineRenderPlanElement::TimelineGrade(grade) => {
            graph_supported(&grade.effect_graph, scratch)
        }
        TimelineRenderPlanElement::CrossDissolve(transition) => {
            prepared_export_transition_input_supports_gpu(&transition.left, scratch)
                && prepared_export_transition_input_supports_gpu(&transition.right, scratch)
        }
    }
}

fn prepared_export_transition_input_supports_gpu(
    input: &TimelineTransitionInputPlan,
    scratch: &mut TimelineCompositeScratch,
) -> bool {
    match input {
        TimelineTransitionInputPlan::Transparent => true,
        TimelineTransitionInputPlan::Media(media) => {
            scratch.get_or_lower_effect_gpu_plan(&media.effect_graph).is_ok()
        }
        TimelineTransitionInputPlan::BasicTitle(title) => {
            scratch.get_or_lower_effect_gpu_plan(&title.effect_graph).is_ok()
        }
        TimelineTransitionInputPlan::NestedSequence(nested) => {
            scratch.get_or_lower_effect_gpu_plan(&nested.effect_graph).is_ok()
        }
        TimelineTransitionInputPlan::SolidColor(solid) => {
            scratch.get_or_lower_effect_gpu_plan(&solid.effect_graph).is_ok()
        }
    }
}

fn render_prepared_visual_node_gpu_into(
    context: &mut ExportFrameRenderContext<'_>,
    inputs: &ExportNodeInputs<'_>,
    target: SequenceRenderTarget<'_>,
) -> Result<(), String> {
    let flatten_black = matches!(
        &target,
        SequenceRenderTarget::Deliverable(_) | SequenceRenderTarget::Resident(_)
    ) && context.alpha_mode == ExportAlphaMode::FlattenBlack;
    let output = render_prepared_visual_node_gpu(context, inputs, flatten_black)?;
    match target {
        SequenceRenderTarget::Deliverable(canvas) => {
            finish_export_gpu_visual_output(context, inputs.node().color_context(), output, canvas)
        }
        SequenceRenderTarget::Resident(resident) => finish_export_gpu_visual_output_resident(
            context,
            inputs.node().color_context(),
            ExportGpuBoundaryInput::Gpu(&output),
            resident,
        ),
        SequenceRenderTarget::Working(_) => {
            Err("GPU visual root requires an output-boundary target".to_owned())
        }
    }
}

fn render_prepared_visual_node_gpu(
    context: &mut ExportFrameRenderContext<'_>,
    inputs: &ExportNodeInputs<'_>,
    flatten_black: bool,
) -> Result<GpuColorFrameHandle, String> {
    if context.cancellation.is_canceled() {
        return Err("export GPU visual execution canceled".to_owned());
    }
    let node = inputs.node();
    let materialization = node.materialization_contract();
    let author_resolution = materialization.author_resolution();
    let resolution = node.execution_resolution();
    let Resolution { width, height } = resolution;
    let color_context = node.color_context().clone();
    let render_plan = node.evaluation().plan();
    if !node.evaluation().payload().is_empty() || !node.evaluation().temporal_batches().is_empty() {
        return Err(
            "heterogeneous or temporal work escaped Export GPU visual preflight".to_owned(),
        );
    }

    let mut decode_cache = HashMap::<ExportDecodeCacheKey, Arc<DecodedVideoLayer>>::with_capacity(
        render_plan.len().saturating_mul(2),
    );
    let mut decoded_media =
        std::iter::repeat_with(|| None).take(render_plan.len()).collect::<Vec<_>>();
    let mut title_media = std::iter::repeat_with(|| None)
        .take(render_plan.len())
        .collect::<Vec<Option<ResolvedExportTitle>>>();
    let mut nested_media = std::iter::repeat_with(|| None)
        .take(render_plan.len())
        .collect::<Vec<Option<GpuColorFrameHandle>>>();

    for (index, element) in render_plan.elements.iter().enumerate() {
        match element {
            TimelineRenderPlanElement::Media(media) => {
                decoded_media[index] = Some(decode_export_media_plan(
                    context.media,
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
            TimelineRenderPlanElement::BasicTitle(title) => {
                title_media[index] = Some(render_export_basic_title_plan(
                    context.visual_session,
                    materialization,
                    title,
                    width,
                    height,
                    color_context.working_color_space(),
                )?);
            }
            TimelineRenderPlanElement::NestedSequence(nested) => {
                nested_media[index] = Some(prepared_export_nested_gpu_output(
                    inputs,
                    nested.placement,
                    PreparedVisualNestedSample::Current,
                )?);
            }
            TimelineRenderPlanElement::Adjustment(_)
            | TimelineRenderPlanElement::TimelineGrade(_)
            | TimelineRenderPlanElement::SolidColor(_)
            | TimelineRenderPlanElement::CrossDissolve(_) => {}
        }
    }

    let mut gpu_elements =
        Vec::with_capacity(render_plan.len().saturating_add(usize::from(flatten_black)));
    if flatten_black {
        gpu_elements.push(GpuVisualFrameElement::Source(Box::new(
            GpuVisualSourceLayer {
                source: GpuVisualFrameSource::Solid(mondrian_core::Color::BLACK),
                opacity: 1.0,
                blend_mode: mondrian_core::BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: lower_export_gpu_effect_plan(
                    context.visual_session,
                    &identity_compiled_effect_graph().ok_or_else(|| {
                        "renderer could not prepare the identity Effect graph".to_owned()
                    })?,
                )?,
                frame_seed: 0,
            },
        )));
    }

    for (index, element) in render_plan.elements.iter().enumerate() {
        let gpu_element = match element {
            TimelineRenderPlanElement::Media(media) => {
                let decoded = decoded_media[index].as_ref().ok_or_else(|| {
                    "GPU media plan was not resolved before compositing".to_owned()
                })?;
                let transform = project_export_picture_affine(
                    media.transform,
                    decoded.picture_geometry.source_to_display_affine(),
                    decoded.source_resolution,
                    decoded_frame_resolution(&decoded.frame),
                    author_resolution,
                    resolution,
                    "GPU media",
                )?;
                GpuVisualFrameElement::Source(Box::new(GpuVisualSourceLayer {
                    source: export_gpu_cpu_source(&decoded.frame, decoded.is_data_texture),
                    opacity: media.opacity,
                    blend_mode: media.blend_mode,
                    transform,
                    effect_plan: lower_export_gpu_effect_plan(
                        context.visual_session,
                        &media.effect_graph,
                    )?,
                    frame_seed: media.frame_seed,
                }))
            }
            TimelineRenderPlanElement::BasicTitle(title) => {
                let resolved = title_media[index].as_ref().ok_or_else(|| {
                    "GPU Basic Title plan was not resolved before compositing".to_owned()
                })?;
                GpuVisualFrameElement::Source(Box::new(GpuVisualSourceLayer {
                    source: export_gpu_cpu_source(&resolved.frame, false),
                    opacity: title.opacity,
                    blend_mode: title.blend_mode,
                    transform: resolved.transform,
                    effect_plan: lower_export_gpu_effect_plan(
                        context.visual_session,
                        &title.effect_graph,
                    )?,
                    frame_seed: title.frame_seed,
                }))
            }
            TimelineRenderPlanElement::NestedSequence(nested) => {
                let frame = nested_media[index].as_ref().ok_or_else(|| {
                    "GPU nested Sequence plan was not resolved before compositing".to_owned()
                })?;
                let child_id = export_nested_child(
                    inputs.closure(),
                    node.id(),
                    nested.placement,
                    PreparedVisualNestedSample::Current,
                )?;
                let source_resolution =
                    export_visual_node(inputs.closure(), child_id)?.author_resolution();
                let descriptor = frame.descriptor();
                let transform = project_export_affine(
                    nested.transform,
                    source_resolution,
                    Resolution { width: descriptor.width, height: descriptor.height },
                    author_resolution,
                    resolution,
                    "GPU nested Sequence",
                )?;
                GpuVisualFrameElement::Source(Box::new(GpuVisualSourceLayer {
                    source: GpuVisualFrameSource::GpuWorking(frame.clone()),
                    opacity: nested.opacity,
                    blend_mode: nested.blend_mode,
                    transform,
                    effect_plan: lower_export_gpu_effect_plan(
                        context.visual_session,
                        &nested.effect_graph,
                    )?,
                    frame_seed: nested.frame_seed,
                }))
            }
            TimelineRenderPlanElement::SolidColor(solid) => {
                let transform = project_export_affine(
                    solid.transform,
                    author_resolution,
                    resolution,
                    author_resolution,
                    resolution,
                    "GPU solid color",
                )?;
                GpuVisualFrameElement::Source(Box::new(GpuVisualSourceLayer {
                    source: GpuVisualFrameSource::Solid(solid.color),
                    opacity: solid.opacity,
                    blend_mode: solid.blend_mode,
                    transform,
                    effect_plan: lower_export_gpu_effect_plan(
                        context.visual_session,
                        &solid.effect_graph,
                    )?,
                    frame_seed: solid.frame_seed,
                }))
            }
            TimelineRenderPlanElement::Adjustment(adjustment) => {
                GpuVisualFrameElement::Adjustment {
                    effect_plan: lower_export_gpu_effect_plan(
                        context.visual_session,
                        &adjustment.effect_graph,
                    )?,
                    opacity: adjustment.opacity,
                    blend_mode: adjustment.blend_mode,
                    frame_seed: adjustment.frame_seed,
                }
            }
            TimelineRenderPlanElement::TimelineGrade(grade) => GpuVisualFrameElement::Adjustment {
                effect_plan: lower_export_gpu_effect_plan(
                    context.visual_session,
                    &grade.effect_graph,
                )?,
                opacity: 1.0,
                blend_mode: mondrian_core::BlendMode::Normal,
                frame_seed: grade.frame_seed,
            },
            TimelineRenderPlanElement::CrossDissolve(transition) => {
                GpuVisualFrameElement::CrossDissolve {
                    left: lower_export_gpu_transition_input(
                        context,
                        inputs,
                        materialization,
                        resolution,
                        &color_context,
                        &transition.left,
                        &mut decode_cache,
                    )?,
                    right: lower_export_gpu_transition_input(
                        context,
                        inputs,
                        materialization,
                        resolution,
                        &color_context,
                        &transition.right,
                        &mut decode_cache,
                    )?,
                    progress: transition.progress,
                }
            }
        };
        gpu_elements.push(gpu_element);
    }

    let record = context.visual_session.gpu_output.record_visual_node(
        GpuVisualFrameRequest {
            width,
            height,
            working_color_space: color_context.working_color_space(),
            color_engine: color_context.engine().clone(),
            elements: &gpu_elements,
        },
        context.cancellation,
    )?;
    match context.visual_session.visual_diagnostics.gpu_visual_working_float_decision {
        Some(existing) if existing != record.working_float_decision => {
            return Err("Export GPU visual nodes disagreed on working-float policy".to_owned());
        }
        Some(_) => {}
        None => {
            context.visual_session.visual_diagnostics.gpu_visual_working_float_decision =
                Some(record.working_float_decision);
        }
    }
    context.visual_session.visual_diagnostics.gpu_visual_nodes_completed = context
        .visual_session
        .visual_diagnostics
        .gpu_visual_nodes_completed
        .saturating_add(1);
    if !inputs.is_root() {
        context.visual_session.visual_diagnostics.gpu_visual_nested_outputs = context
            .visual_session
            .visual_diagnostics
            .gpu_visual_nested_outputs
            .saturating_add(1);
    }
    context.visual_session.visual_diagnostics.gpu_visual_data_texture_uploads = context
        .visual_session
        .visual_diagnostics
        .gpu_visual_data_texture_uploads
        .saturating_add(record.compositing_diagnostics.data_texture_uploads);
    let active = record.active_working_set.total();
    context.visual_session.visual_diagnostics.gpu_visual_peak_active_bytes = context
        .visual_session
        .visual_diagnostics
        .gpu_visual_peak_active_bytes
        .max(active.bytes);
    context.visual_session.visual_diagnostics.gpu_visual_peak_active_textures = context
        .visual_session
        .visual_diagnostics
        .gpu_visual_peak_active_textures
        .max(active.textures);
    if let Some(diagnostics) = context.stage_diagnostics.as_deref_mut() {
        diagnostics.accumulate(record.color_stage_diagnostics);
    }
    if let Some(diagnostics) = context.composite_diagnostics.as_deref_mut() {
        diagnostics.accumulate(TimelineCompositeDiagnostics {
            elements: render_plan.len() as u64,
            float_linear_composites: 1,
            effect_gpu_executed: gpu_elements.len() as u64,
            ..TimelineCompositeDiagnostics::default()
        });
    }
    Ok(record.output)
}

fn export_gpu_cpu_source(frame: &CpuColorFrame, is_data_texture: bool) -> GpuVisualFrameSource {
    if is_data_texture {
        GpuVisualFrameSource::DataTexture(Arc::new(frame.clone()))
    } else {
        GpuVisualFrameSource::Working(Arc::new(frame.clone()))
    }
}

fn lower_export_gpu_effect_plan(
    visual_session: &mut ExportVisualRenderSession,
    graph: &Arc<mondrian_effects::CompiledEffectGraph>,
) -> Result<Arc<mondrian_effects::CompiledEffectGpuPlan>, String> {
    visual_session
        .composite_scratch
        .get_or_lower_effect_gpu_plan(graph)
        .map_err(|blocker| format!("Effect graph escaped Export GPU preflight: {blocker:?}"))
}

#[allow(clippy::too_many_arguments)]
fn lower_export_gpu_transition_input(
    context: &mut ExportFrameRenderContext<'_>,
    inputs: &ExportNodeInputs<'_>,
    materialization: PreparedVisualMaterializationContract,
    resolution: Resolution,
    color_context: &ProgramColorContext,
    plan: &TimelineTransitionInputPlan,
    decode_cache: &mut HashMap<ExportDecodeCacheKey, Arc<DecodedVideoLayer>>,
) -> Result<GpuVisualTransitionInput, String> {
    let author_resolution = materialization.author_resolution();
    let Resolution { width, height } = resolution;
    Ok(match plan {
        TimelineTransitionInputPlan::Transparent => GpuVisualTransitionInput::Transparent,
        TimelineTransitionInputPlan::Media(media) => {
            let decoded = decode_export_media_plan(
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
            )?;
            let transform = project_export_picture_affine(
                media.transform,
                decoded.picture_geometry.source_to_display_affine(),
                decoded.source_resolution,
                decoded_frame_resolution(&decoded.frame),
                author_resolution,
                resolution,
                "GPU Transition media",
            )?;
            GpuVisualTransitionInput::Source(Box::new(GpuVisualSourceLayer {
                source: export_gpu_cpu_source(&decoded.frame, decoded.is_data_texture),
                opacity: media.opacity,
                blend_mode: media.blend_mode,
                transform,
                effect_plan: lower_export_gpu_effect_plan(
                    context.visual_session,
                    &media.effect_graph,
                )?,
                frame_seed: media.frame_seed,
            }))
        }
        TimelineTransitionInputPlan::BasicTitle(title) => {
            let resolved = render_export_basic_title_plan(
                context.visual_session,
                materialization,
                title,
                width,
                height,
                color_context.working_color_space(),
            )?;
            GpuVisualTransitionInput::Source(Box::new(GpuVisualSourceLayer {
                source: export_gpu_cpu_source(&resolved.frame, false),
                opacity: title.opacity,
                blend_mode: title.blend_mode,
                transform: resolved.transform,
                effect_plan: lower_export_gpu_effect_plan(
                    context.visual_session,
                    &title.effect_graph,
                )?,
                frame_seed: title.frame_seed,
            }))
        }
        TimelineTransitionInputPlan::NestedSequence(nested) => {
            let frame = prepared_export_nested_gpu_output(
                inputs,
                nested.placement,
                PreparedVisualNestedSample::Current,
            )?;
            let child_id = export_nested_child(
                inputs.closure(),
                inputs.node().id(),
                nested.placement,
                PreparedVisualNestedSample::Current,
            )?;
            let source_resolution =
                export_visual_node(inputs.closure(), child_id)?.author_resolution();
            let descriptor = frame.descriptor();
            let transform = project_export_affine(
                nested.transform,
                source_resolution,
                Resolution { width: descriptor.width, height: descriptor.height },
                author_resolution,
                resolution,
                "GPU Transition nested Sequence",
            )?;
            GpuVisualTransitionInput::Source(Box::new(GpuVisualSourceLayer {
                source: GpuVisualFrameSource::GpuWorking(frame),
                opacity: nested.opacity,
                blend_mode: nested.blend_mode,
                transform,
                effect_plan: lower_export_gpu_effect_plan(
                    context.visual_session,
                    &nested.effect_graph,
                )?,
                frame_seed: nested.frame_seed,
            }))
        }
        TimelineTransitionInputPlan::SolidColor(solid) => {
            let transform = project_export_affine(
                solid.transform,
                author_resolution,
                resolution,
                author_resolution,
                resolution,
                "GPU Transition solid color",
            )?;
            GpuVisualTransitionInput::Source(Box::new(GpuVisualSourceLayer {
                source: GpuVisualFrameSource::Solid(solid.color),
                opacity: solid.opacity,
                blend_mode: solid.blend_mode,
                transform,
                effect_plan: lower_export_gpu_effect_plan(
                    context.visual_session,
                    &solid.effect_graph,
                )?,
                frame_seed: solid.frame_seed,
            }))
        }
    })
}

fn finish_export_gpu_visual_output(
    context: &mut ExportFrameRenderContext<'_>,
    color_context: &ProgramColorContext,
    output: GpuColorFrameHandle,
    canvas: &mut Vec<u8>,
) -> Result<(), String> {
    let boundary = export_output_boundary_from_context(color_context)?;
    if color_context.output_tone_map()
        && boundary.ocio_display_view().is_none()
        && let Some(diagnostics) = context.export_diagnostics.as_deref_mut()
    {
        diagnostics.record_output_transform_issue(
            ExportOutputTransformIssueReason::ToneMapRequestedWithoutExportViewTransform,
        );
    }
    let attempt = context
        .visual_session
        .gpu_output
        .execute_gpu_frame(
            &output,
            &boundary,
            context.delivery_pixels.frame,
            context.delivery_pixels.legalizer,
            context.cancellation,
        )
        .map_err(|error| match error {
            ExportGpuOutputExecutionError::Canceled => {
                "export GPU visual output readback canceled".to_owned()
            }
            ExportGpuOutputExecutionError::DeviceTimedOut => {
                "export GPU visual output readback timed out".to_owned()
            }
            ExportGpuOutputExecutionError::Fallback(reason) => {
                format!("export GPU visual output failed closed: {reason:?}")
            }
            ExportGpuOutputExecutionError::Packing(error) => {
                format!("export GPU visual output cannot enter the declared FFmpeg pipe: {error}")
            }
        })?;
    context.visual_session.visual_diagnostics.gpu_visual_output_readbacks = context
        .visual_session
        .visual_diagnostics
        .gpu_visual_output_readbacks
        .saturating_add(1);
    if let Some(diagnostics) = context.stage_diagnostics.as_deref_mut() {
        diagnostics.accumulate(attempt.stage_diagnostics);
    }
    if let Some(diagnostics) = context.export_diagnostics.as_deref_mut() {
        diagnostics.record_export_output_boundary(
            1,
            0,
            ExportGpuOutputFallbackBreakdown::default(),
        );
    }
    canvas.clear();
    canvas.extend_from_slice(&attempt.pipe_bytes);
    Ok(())
}

fn finish_export_gpu_visual_output_resident(
    context: &mut ExportFrameRenderContext<'_>,
    color_context: &ProgramColorContext,
    input: ExportGpuBoundaryInput<'_>,
    resident: &mut Option<GpuResidentEncoderInputLease>,
) -> Result<(), String> {
    if context.delivery_pixels.legalizer.is_active() {
        return Err(
            "resident encode requires GPU legalizer lowering; CPU legalizer is active".to_owned(),
        );
    }
    let boundary = export_output_boundary_from_context(color_context)?;
    let attempt = context
        .visual_session
        .gpu_output
        .execute_resident(
            input,
            &boundary,
            context.delivery_pixels.frame.gpu_boundary_texture_format(),
            context.cancellation,
        )
        .map_err(|error| format!("resident export output boundary failed: {error:?}"))?;
    if let Some(diagnostics) = context.stage_diagnostics.as_deref_mut() {
        diagnostics.accumulate(attempt.stage_diagnostics);
    }
    *resident = Some(attempt.source);
    Ok(())
}

fn render_prepared_visual_node_into(
    context: &mut ExportFrameRenderContext<'_>,
    inputs: &ExportNodeInputs<'_>,
    mut target: SequenceRenderTarget<'_>,
) -> Result<(), String> {
    if context.cancellation.is_canceled() {
        return Err("export visual execution canceled".to_owned());
    }
    let closure = inputs.closure();
    let node = inputs.node();
    let node_id = node.id();
    let materialization = node.materialization_contract();
    let author_resolution = materialization.author_resolution();
    let resolution = node.execution_resolution();
    let color_context = node.color_context().clone();
    let Resolution { width, height } = resolution;
    let media_dependencies = context.media;

    let frame_contract = context.delivery_pixels.frame;
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
            color_context.working_color_space(),
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
    let temporal_layers =
        resolve_export_temporal_batches(context, inputs, temporal_batches, &mut decode_cache)?;
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
                    color_context.working_color_space(),
                )?);
            }
            TimelineRenderPlanElement::CrossDissolve(transition) => {
                let left = resolve_export_transition_input(
                    context,
                    inputs,
                    materialization,
                    &transition.left,
                    resolution,
                    &color_context,
                    &mut decode_cache,
                    &temporal_layers,
                )?;
                let right = resolve_export_transition_input(
                    context,
                    inputs,
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
            | TimelineRenderPlanElement::TimelineGrade(_)
            | TimelineRenderPlanElement::SolidColor(_)
            | TimelineRenderPlanElement::NestedSequence(_) => {}
        }
    }

    for (index, element) in render_plan.elements.iter().enumerate() {
        let TimelineRenderPlanElement::NestedSequence(nested) = element else {
            continue;
        };
        if !temporal_layers.contains_key(&nested.placement) {
            nested_media[index] = Some(
                prepared_export_nested_output(
                    inputs,
                    nested.placement,
                    PreparedVisualNestedSample::Current,
                )?
                .clone(),
            );
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
            | TimelineRenderPlanElement::TimelineGrade(_)
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
                color_context.working_color_space(),
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
            TimelineRenderPlanElement::TimelineGrade(grade) => {
                composite_elements.push(TimelineCompositeElement::Adjustment(
                    TimelineAdjustmentLayer {
                        effect_graph: grade.effect_graph.clone(),
                        opacity: 1.0,
                        blend_mode: None,
                        frame_seed: grade.frame_seed,
                    },
                ));
            }
            TimelineRenderPlanElement::Media(media) => {
                let (resolved_frame, source_resolution, source_to_display_affine) =
                    if let Some(temporal) = temporal_layers.get(&media.placement) {
                        (
                            &temporal.frame,
                            temporal.source_resolution,
                            temporal.source_to_display_affine,
                        )
                    } else {
                        let decoded = decoded_media[index].as_ref().ok_or_else(|| {
                            "media plan was not resolved before compositing".to_owned()
                        })?;
                        (
                            &decoded.frame,
                            decoded.source_resolution,
                            decoded.picture_geometry.source_to_display_affine(),
                        )
                    };
                let frame = heterogeneous_media[index].as_ref().unwrap_or(resolved_frame);
                let transform = project_export_picture_affine(
                    media.transform,
                    source_to_display_affine,
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
            color_context.working_color_space(),
            context.alpha_mode,
        );
        return Ok(());
    }

    let effect_execution_generation = context.visual_session.effect_execution_generation;
    context
        .visual_session
        .composite_scratch
        .bind_effect_execution_generation(effect_execution_generation);
    let composite_options = if matches!(
        &target,
        SequenceRenderTarget::Deliverable(_) | SequenceRenderTarget::Resident(_)
    ) && context.alpha_mode == ExportAlphaMode::FlattenBlack
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
        TimelineEffectColorRuntime::new(
            color_context.engine(),
            color_context.working_color_space(),
        ),
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

    let output_target = match target {
        SequenceRenderTarget::Working(output) => {
            *output = Some(rendered.frame);
            return Ok(());
        }
        SequenceRenderTarget::Resident(resident) => {
            return finish_export_gpu_visual_output_resident(
                context,
                &color_context,
                ExportGpuBoundaryInput::Cpu(&rendered.frame),
                resident,
            );
        }
        SequenceRenderTarget::Deliverable(canvas) => canvas,
    };

    let mut gpu_output_fallback_reasons = ExportGpuOutputFallbackBreakdown::default();
    let mut gpu_output_attempts = 0u64;
    let mut gpu_output_cpu_fallbacks = 0u64;
    let boundary = export_output_boundary_from_context(&color_context)?;
    if color_context.output_tone_map()
        && boundary.ocio_display_view().is_none()
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
        context.delivery_pixels.legalizer,
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
        Err(ExportGpuOutputExecutionError::Packing(error)) => {
            return Err(format!(
                "export GPU output cannot enter the declared FFmpeg pipe: {error}"
            ));
        }
    };
    gpu_output_attempts = gpu_output_attempts.saturating_add(1);

    let final_bytes = match attempt {
        Some(attempt) => {
            if let Some(diagnostics) = context.stage_diagnostics.as_deref_mut() {
                diagnostics.accumulate(attempt.stage_diagnostics);
            }
            attempt.pipe_bytes
        }
        None => {
            if frame_contract.requires_float_output_boundary()
                || context.delivery_pixels.legalizer.is_active()
            {
                match cpu_output_boundary_float(
                    &rendered.frame,
                    &boundary,
                    context.visual_session.composite_scratch.color_execution_mut(),
                ) {
                    Ok(float_result) => {
                        if let Some(diagnostics) = context.stage_diagnostics.as_deref_mut() {
                            diagnostics.accumulate(float_result.stage_diagnostics);
                        }
                        let mut encoded = float_result.frame.into_rgba_f32();
                        if context.delivery_pixels.legalizer.is_active() {
                            let compliance = SignalComplianceContract::normalized_rgb(
                                boundary.output_color_space(),
                            )
                            .map_err(|error| {
                                format!("export legalizer contract failed: {error}")
                            })?;
                            legalize_encoded_rgba_f32(
                                &mut encoded.data,
                                compliance,
                                context.delivery_pixels.legalizer,
                            )
                            .map_err(|error| format!("export legalizer failed: {error}"))?;
                        }
                        let flat: &[f32] = bytemuck::cast_slice(&encoded.data);
                        frame_contract
                            .pack_rgba_f32(flat)
                            .map_err(|error| format!("export CPU float output cannot enter the declared FFmpeg pipe: {error}"))?
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
                let encoded = ProgramOutputModule::execute_cpu_rgba8(
                    &rendered.frame,
                    &boundary,
                    context.visual_session.composite_scratch.color_execution_mut(),
                )
                .map_err(|err| format!("final color transform failed: {err}"))?;
                if let Some(diagnostics) = context.stage_diagnostics.as_deref_mut() {
                    diagnostics.accumulate(encoded.stage_diagnostics);
                }
                frame_contract.pack_rgba8(&encoded.rgba).map_err(|error| {
                    format!("export RGBA8 output cannot enter the declared FFmpeg pipe: {error}")
                })?
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
    output_target.clear();
    output_target.extend_from_slice(&final_bytes);
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
    let input_video_range = resolve_export_input_video_range(
        media_dependencies,
        media.asset_id,
        dependency.interpretation,
    );
    let source_preparation = resolve_export_source_preparation(
        ExportSourcePreparationRequest {
            asset_id: media.asset_id,
            color_space_override: media.color_space_override,
            auto_tone_map: media.auto_tone_map,
            dependency,
            input_video_range,
            color_context,
        },
        input_color_counts,
    )?;
    let ExportSourcePreparation { source_contract, preparation_intent, .. } = source_preparation;
    let source_resolution = dependency.source_resolution.ok_or_else(|| {
        format!(
            "asset={} export snapshot has no source extent",
            media.asset_id
        )
    })?;
    let picture_metadata = dependency.picture.ok_or_else(|| {
        format!(
            "asset={} export snapshot has no source picture interpretation",
            media.asset_id
        )
    })?;
    let picture_geometry = ResolvedPictureGeometry::resolve(
        source_resolution,
        picture_metadata,
        media.pixel_aspect_ratio_override,
        media.field_order_override,
    )
    .map_err(|error| {
        format!(
            "asset={} export picture interpretation is unsupported: {error}",
            media.asset_id
        )
    })?;
    let key = ExportDecodeCacheKey::new(
        media.asset_id,
        dependency,
        media.source_sample,
        source_contract,
        preparation_intent.clone(),
        media.alpha_interpretation,
        Resolution { width, height },
        source_resolution,
        picture_geometry,
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
            picture_geometry,
            source_color: source_contract,
            alpha_interpretation: media.alpha_interpretation,
            preparation_intent,
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

/// Exact decode and renderer-preparation contract selected for one export source.
///
/// Keeping resolution behind this narrow Interface gives Preview-independent
/// Export callers one closed decision for color-managed, DataTexture, and
/// rejected inputs. Decode and cache code consume the result without
/// reinterpreting authored or probe evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExportSourcePreparation {
    source_contract: PreviewSourceColorContract,
    preparation_intent: SourceFramePreparationIntent,
    resolution_source: InputColorResolutionSource,
}

struct ExportSourcePreparationRequest<'a> {
    asset_id: AssetId,
    color_space_override: Option<ColorSpace>,
    auto_tone_map: bool,
    dependency: &'a crate::preset::ExportMediaDependency,
    input_video_range: DecodedVideoRangeContract,
    color_context: &'a ProgramColorContext,
}

fn resolve_export_source_preparation(
    request: ExportSourcePreparationRequest<'_>,
    input_color_counts: Option<&mut InputColorResolutionSourceCounts>,
) -> Result<ExportSourcePreparation, String> {
    let ExportSourcePreparationRequest {
        asset_id,
        color_space_override,
        auto_tone_map,
        dependency,
        input_video_range,
        color_context,
    } = request;
    let input_color_resolution =
        color_context.missing_metadata_policy().resolve_asset_input_decision(
            color_space_override,
            dependency.interpretation,
            dependency
                .color_diagnostic
                .as_ref()
                .and_then(mondrian_media::VideoColorDiagnostic::executable_color_space)
                .or_else(|| {
                    dependency
                        .source_video_stream
                        .as_ref()
                        .and_then(mondrian_media::VideoStreamInfo::executable_color_space)
                }),
            color_context.working_color_space(),
        );
    if let Some(counts) = input_color_counts {
        counts.record(input_color_resolution.source);
    }
    let (source_contract, preparation_intent) = match input_color_resolution.resolved {
        ResolvedInputColor::Color(color_space) => (
            PreviewSourceColorContract::new(color_space, input_video_range),
            SourceFramePreparationIntent::ColorManaged(SourceColorModule::cpu_intent(
                &color_context.media_input(auto_tone_map),
            )),
        ),
        ResolvedInputColor::Data => {
            let sampling = dependency
                .color_diagnostic
                .as_ref()
                .and_then(|diagnostic| diagnostic.sampling)
                .ok_or_else(|| {
                    format!(
                        "asset={} path={} data-texture export requires proven source sampling",
                        asset_id,
                        dependency.path.display()
                    )
                })?;
            if !sampling.pixel_format.is_rgb() {
                return Err(format!(
                    "asset={} path={} data-texture export requires RGB source sampling, got {:?}",
                    asset_id,
                    dependency.path.display(),
                    sampling.pixel_format
                ));
            }
            (
                PreviewSourceColorContract::data_texture(input_video_range),
                SourceFramePreparationIntent::data_texture(color_context.working_color_space()),
            )
        }
        ResolvedInputColor::Rejected => {
            let diagnostic = dependency
                .color_diagnostic
                .as_ref()
                .map(mondrian_media::VideoColorDiagnostic::summary)
                .unwrap_or_else(|| "unavailable".to_string());
            return Err(format!(
                "asset={} path={} missing color metadata rejected by sequence policy {:?}; resolution={:?} override={:?} detected={:?} working={:?}; {}",
                asset_id,
                dependency.path.display(),
                color_context.missing_metadata_policy(),
                input_color_resolution.source,
                input_color_resolution.override_color_space,
                input_color_resolution.executable_color_space,
                input_color_resolution.working_color_space,
                diagnostic
            ));
        }
    };
    Ok(ExportSourcePreparation {
        source_contract,
        preparation_intent,
        resolution_source: input_color_resolution.source,
    })
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
    inputs: &ExportNodeInputs<'_>,
    batches: &[TimelineTemporalDemandBatch],
    decode_cache: &mut HashMap<ExportDecodeCacheKey, Arc<DecodedVideoLayer>>,
) -> Result<HashMap<TimelineClipExecutionRef, PreparedExportTemporalLayer>, String> {
    let node = inputs.node();
    let materialization = node.materialization_contract();
    let color_context = node.color_context().clone();
    let resolution = node.execution_resolution();
    let mut layers = HashMap::with_capacity(batches.len());
    for batch in batches {
        if context.cancellation.is_canceled() {
            return Err("export temporal dependency preparation was canceled".to_owned());
        }
        let mut source_resolution = None;
        let mut source_to_display_affine = None;
        let mut resolved = Vec::with_capacity(batch.source_demands().len());
        for demand in batch.source_demands() {
            let (frame, current_source_resolution, current_source_to_display_affine) =
                resolve_export_temporal_source(
                    context,
                    inputs,
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
            if source_to_display_affine
                .replace(current_source_to_display_affine)
                .is_some_and(|previous| previous != current_source_to_display_affine)
            {
                return Err(format!(
                    "Clip {} temporal requests resolved with inconsistent picture geometry",
                    batch.placement().clip_id
                ));
            }
            resolved.push((
                demand.effect_request,
                export_temporal_tile(&frame, demand.effect_request)?,
            ));
        }
        let source_identity =
            export_temporal_source_identity(context, inputs.closure(), batch, &color_context)?;
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
            color_space: color_context.working_color_space(),
        });
        let layer = PreparedExportTemporalLayer {
            frame,
            source_resolution: source_resolution.unwrap_or(resolution),
            source_to_display_affine: source_to_display_affine
                .unwrap_or([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]),
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
    inputs: &ExportNodeInputs<'_>,
    materialization: PreparedVisualMaterializationContract,
    color_context: &ProgramColorContext,
    resolution: Resolution,
    batch: &TimelineTemporalDemandBatch,
    demand: &mondrian_renderer::TimelineTemporalSourceDemand,
    decode_cache: &mut HashMap<ExportDecodeCacheKey, Arc<DecodedVideoLayer>>,
) -> Result<(CpuColorFrame, Resolution, [f32; 6]), String> {
    if context.cancellation.is_canceled() {
        return Err("export temporal source resolution was canceled".to_owned());
    }
    let identity = identity_compiled_effect_graph()
        .ok_or_else(|| "renderer could not prepare the identity Effect graph".to_owned())?;
    let (frame, source_resolution, source_to_display_affine) = match &demand.source {
        TimelineTemporalSource::Media {
            asset_id,
            source_sample,
            color_space_override,
            picture_overrides,
            alpha_interpretation,
            auto_tone_map,
        } => {
            let plan = TimelineMediaPlan {
                placement: demand.placement,
                asset_id: *asset_id,
                color_space_override: *color_space_override,
                pixel_aspect_ratio_override: picture_overrides.pixel_aspect_ratio,
                field_order_override: picture_overrides.field_order,
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
            (
                decoded.frame.clone(),
                decoded.source_resolution,
                decoded.picture_geometry.source_to_display_affine(),
            )
        }
        TimelineTemporalSource::NestedSequence { sequence_id, .. } => {
            let child_id = export_nested_child(
                inputs.closure(),
                inputs.node().id(),
                demand.placement,
                PreparedVisualNestedSample::Temporal(demand.effect_request),
            )?;
            let child_node = export_visual_node(inputs.closure(), child_id)?;
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
                prepared_export_nested_output(
                    inputs,
                    demand.placement,
                    PreparedVisualNestedSample::Temporal(demand.effect_request),
                )?
                .clone(),
                child_author_resolution,
                [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
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
                    color_space: color_context.working_color_space(),
                }),
                materialization.author_resolution(),
                [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
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
    if descriptor.color_space.working() != Some(color_context.working_color_space())
        || descriptor.alpha != mondrian_renderer::ColorFrameAlpha::StraightCoverage
    {
        return Err(format!(
            "Clip {} temporal source did not resolve to parent working-space straight alpha",
            batch.placement().clip_id
        ));
    }
    Ok((frame, source_resolution, source_to_display_affine))
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
        serde_json::to_vec(&color_context.working_color_space())
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
                picture_overrides,
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
                    serde_json::to_vec(picture_overrides).map_err(|error| error.to_string())?,
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
    inputs: &ExportNodeInputs<'_>,
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
                color_context.working_color_space(),
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
            ResolvedExportTransitionInput::Nested(
                prepared_export_nested_output(
                    inputs,
                    nested.placement,
                    PreparedVisualNestedSample::Current,
                )?
                .clone(),
            )
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

fn project_export_picture_affine(
    transform: [f32; 6],
    source_to_display_affine: [f32; 6],
    source_authoring: Resolution,
    source_sampled: Resolution,
    output_authoring: Resolution,
    output_sampled: Resolution,
    source_kind: &str,
) -> Result<[f32; 6], String> {
    let interpreted = mondrian_core::compose_picture_affine(transform, source_to_display_affine)
        .ok_or_else(|| format!("{source_kind} picture interpretation geometry is invalid"))?;
    project_export_affine(
        interpreted,
        source_authoring,
        source_sampled,
        output_authoring,
        output_sampled,
        source_kind,
    )
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
            let transform = project_export_picture_affine(
                media.transform,
                frame.picture_geometry.source_to_display_affine(),
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
            let transform = project_export_picture_affine(
                transform,
                temporal.source_to_display_affine,
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
    if media_dependencies
        .get(&asset_id)
        .and_then(|dependency| dependency.source_video_stream.as_ref())
        .and_then(|stream| stream.camera_raw.as_ref())
        .is_some()
    {
        return DecodedVideoRangeContract::Automatic { probed_range: DecodedVideoRange::Full };
    }
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
                frame_contract.fill_black_opaque(canvas, width, height);
            }
            ExportAlphaMode::Preserve => canvas.fill(0),
        },
        SequenceRenderTarget::Resident(output) => {
            **output = None;
        }
    }
}

/// Immutable author and delivery facts for one export still-frame decode.
///
/// The request owns the renderer source-preparation intent so media decode
/// cannot reinterpret color-managed versus DataTexture execution while a job
/// is running. Source revision and physical stream authority remain frozen in
/// `dependency`.
struct ExportVideoLayerDecodeRequest<'a> {
    asset_id: AssetId,
    dependency: &'a crate::preset::ExportMediaDependency,
    source_sample: mondrian_core::SourceSampleTarget,
    decode_resolution: Resolution,
    source_resolution: Resolution,
    picture_geometry: ResolvedPictureGeometry,
    source_color: PreviewSourceColorContract,
    alpha_interpretation: AlphaInterpretation,
    preparation_intent: SourceFramePreparationIntent,
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
        picture_geometry,
        source_color,
        alpha_interpretation,
        preparation_intent,
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
    let mut media_request = PreviewDecodeRequest::new(
        path,
        source_sample,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        source_color,
    )
    .with_field_processing(
        mondrian_media::PreviewSourceFieldProcessing::from_picture_scan(picture_geometry.scan()),
    )
    .with_max_size(
        Some(decode_resolution.width),
        Some(decode_resolution.height),
    )
    .with_video_stream_index(video_stream_index)
    .with_fingerprint(dependency.source_fingerprint);
    if let Some(raw) = dependency
        .source_video_stream
        .as_ref()
        .and_then(|stream| stream.camera_raw.as_ref())
    {
        let intent = mondrian_media::CameraRawDecodeIntent::new(
            raw.adapter,
            dependency.interpretation.camera_raw,
        )
        .map_err(|error| {
            format!(
                "asset={} path={} invalid camera RAW export contract: {error}",
                asset_id,
                path.display()
            )
        })?;
        media_request = media_request.with_camera_raw(intent);
    }
    let decode_cancellation = cancellation.clone();
    let outcome =
        decode_context.decode_cancellable(media_request, move || decode_cancellation.is_canceled());
    let (source, decode_diagnostics): (PreparedSourceFrame, PreviewDecodeDiagnostics) =
        match outcome {
            Ok(PreviewDecodeOutcome::Frame(frame)) => {
                let diagnostics = frame.diagnostics;
                let source = prepare_decoded_cpu_source_frame(
                    frame,
                    alpha_interpretation,
                    preparation_intent.clone(),
                )
                .map_err(|err| {
                    format!(
                        "asset={} path={} source frame preparation failed: {}",
                        asset_id,
                        path.display(),
                        err
                    )
                })?;
                (source, diagnostics)
            }
            Ok(PreviewDecodeOutcome::FloatFrame(frame)) => {
                let diagnostics = frame.diagnostics;
                let source = prepare_decoded_cpu_source_frame(
                    frame,
                    alpha_interpretation,
                    preparation_intent,
                )
                .map_err(|err| {
                    format!(
                        "asset={} path={} source frame preparation failed: {}",
                        asset_id,
                        path.display(),
                        err
                    )
                })?;
                (source, diagnostics)
            }
            Ok(PreviewDecodeOutcome::CpuYuvFrame(_)) => {
                return Err(format!(
                    "asset={} path={} err=export still-frame CPU fallback requires CPU-addressable RGB, got compact GPU-materialization YUV",
                    asset_id,
                    path.display()
                ));
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
    let is_data_texture = source.is_data_texture();
    let execution = source
        .execute_cpu_with_session(color_session)
        .map_err(|err| format!("asset={asset_id} color transform failed: {err}"))?;
    Ok(Arc::new(DecodedVideoLayer {
        frame: execution.frame,
        is_data_texture,
        source_resolution,
        picture_geometry,
        source_fingerprint: dependency.source_fingerprint,
        video_stream_index,
        decode_diagnostics: Some(decode_diagnostics),
        stage_diagnostics: execution.stage_diagnostics,
    }))
}

#[cfg(test)]
fn compute_timeline_render_range(
    timeline: &TimelineExportSnapshot,
) -> Result<TimelineRenderRange, String> {
    compute_timeline_render_range_with_cadence(
        timeline,
        timeline.sequence.settings.frame_rate,
        crate::preset::ExportFrameSampling::FrameHold,
    )
}

fn compute_timeline_render_range_for_delivery(
    timeline: &TimelineExportSnapshot,
    delivery: &ResolvedExportDeliveryContract,
) -> Result<TimelineRenderRange, String> {
    compute_timeline_render_range_with_cadence(
        timeline,
        delivery.frame_rate,
        delivery.frame_sampling,
    )
}

fn compute_timeline_render_range_with_cadence(
    timeline: &TimelineExportSnapshot,
    output_frame_rate: Rational,
    frame_sampling: crate::preset::ExportFrameSampling,
) -> Result<TimelineRenderRange, String> {
    let resolved = timeline.range.resolve(&timeline.sequence).map_err(|error| error.to_string())?;
    let selected_time = resolved.time_range().map_err(|error| error.to_string())?;
    let output_frame_count = selected_time
        .duration
        .to_frame_position(output_frame_rate, FrameRounding::Ceil)
        .map_err(|error| error.to_string())?
        .frame;
    let total_frames = u64::try_from(output_frame_count.max(1))
        .map_err(|_| "export frame count exceeds unsigned capacity".to_owned())?;
    Ok(TimelineRenderRange {
        source_start: selected_time.start,
        total_frames,
        fps_num: output_frame_rate.num,
        fps_den: output_frame_rate.den,
        sequence_frame_rate: timeline.sequence.settings.frame_rate,
        frame_sampling,
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
    use mondrian_renderer::color::ProgramOutputRole;
    use mondrian_timeline::clip::Clip;
    use mondrian_timeline::sequence::{
        MissingColorMetadataPolicy, Sequence, StaticHdrMetadataPolicy,
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

    #[test]
    fn export_cpu_rgba_decode_delegates_to_source_frame_preparation() {
        let queue_source = include_str!("mod.rs");
        let decode = queue_source
            .split("fn decode_video_layer_scaled")
            .nth(1)
            .and_then(|suffix| suffix.split("fn compute_timeline_render_range").next())
            .expect("export video decode implementation");

        assert_eq!(
            decode.matches("prepare_decoded_cpu_source_frame(").count(),
            2
        );
        assert!(!decode.contains("CpuEncodedFloatColorFrame"));
        assert!(!decode.contains("LinearFloatSource"));
        assert!(!decode.contains("normalize_alpha"));
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
                .root_program_color_context(&timeline.color_environment)
                .map_err(|error| {
                    JobExecutionResult::Failed(format!(
                        "invalid root Program color context: {error}"
                    ))
                })?,
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
            visual_active_grant: GpuVisualFrameExecutionResourceGrant::default(),
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
                &ExportGpuOutputExecutionError::Fallback(reason)
            ));
        }
        assert!(!export_gpu_error_requires_backend_backoff(
            &ExportGpuOutputExecutionError::Packing(
                ExportFramePackingError::InvalidRgbaComponentCount { components: 3 }
            )
        ));
        assert!(!export_gpu_error_requires_backend_backoff(
            &ExportGpuOutputExecutionError::Canceled
        ));
        assert!(export_gpu_error_requires_backend_backoff(
            &ExportGpuOutputExecutionError::DeviceTimedOut
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
        let visual_active_grant = ExportExecutionResourcePolicy::default().gpu_visual_active;
        let mut runtime = ExportGpuExecutionRuntime {
            attempt_generation: 7,
            next_device_generation: 3,
            resource_pool_options: GpuColorFrameWgpuResourcePoolOptions::default(),
            visual_active_grant,
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
    fn export_gpu_visual_grant_change_rebuilds_the_backend_before_next_frame() {
        let policy = ExportExecutionResourcePolicy::default();
        let mut runtime = ExportGpuExecutionRuntime {
            attempt_generation: 7,
            next_device_generation: 3,
            resource_pool_options: GpuColorFrameWgpuResourcePoolOptions {
                max_per_contract: policy.gpu_output_idle_per_contract,
                max_retained_bytes: policy.gpu_output_idle_bytes,
            },
            visual_active_grant: GpuVisualFrameExecutionResourceGrant::new(1, 1),
            active_output_grant: policy.gpu_output_active,
            state: ExportGpuExecutionRuntimeState::Backoff { attempt_generation: 7 },
        };

        runtime.configure(policy);

        assert_eq!(runtime.visual_active_grant, policy.gpu_visual_active);
        assert!(matches!(
            runtime.state,
            ExportGpuExecutionRuntimeState::Cold
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
        let settings = mondrian_timeline::sequence::SequenceSettings::default();
        let context = settings
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
            .expect("default Program color context");
        let mut session = mondrian_renderer::RenderCpuColorExecutionSession::new(4);
        mondrian_renderer::color::SourceColorModule::execute_cpu(
            &mondrian_renderer::CpuSourceColorFrame::from(source),
            &context.media_input(false),
            &mut session,
        )
        .expect("test input transform")
        .into_frame()
    }

    fn test_working_color_space() -> WorkingColorSpace {
        mondrian_timeline::sequence::SequenceSettings::default()
            .color
            .working_color_space
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
        PreparedEffectProgram::prepare(&effects, &[], test_working_color_space())
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
                test_working_color_space(),
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
                test_working_color_space(),
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
        let working_color_space = mondrian_timeline::sequence::SequenceSettings::default()
            .color
            .working_color_space;
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
                working_color_space,
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
            working_color_space
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
                test_working_color_space(),
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
                test_working_color_space(),
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
            _report_owner: &mut dyn FnMut(ExportExecutionOwnerEvent),
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
            _report_owner: &mut dyn FnMut(ExportExecutionOwnerEvent),
        ) -> JobExecutionResult {
            if !execution_gate.wait_at_boundary(ExportProgressPhase::Rendering, cancel) {
                return JobExecutionResult::Cancelled;
            }
            report(ExportProgress::rendering(0.5, 1, 1));
            report_diagnostics(self.diagnostics.clone());
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
            _report_owner: &mut dyn FnMut(ExportExecutionOwnerEvent),
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
            smart_render: crate::preset::ExportSmartRenderPolicy::Automatic,
            broadcast_qc: None,
        }
    }

    #[test]
    fn ffmpeg_executor_publishes_png_sequence_only_after_manifest_validation() {
        if mondrian_media::ffmpeg_command().arg("-version").output().is_err() {
            eprintln!("skipping PNG sequence integration test: FFmpeg unavailable");
            return;
        }
        let directory = tempfile::tempdir().expect("temporary export parent");
        let output = directory.path().join("sequence.pngseq");
        let mut config = dummy_config(&output.to_string_lossy());
        config.preset = crate::preset::ExportPreset::png_sequence();
        config.preset.resolution = Some(crate::preset::Resolution { width: 16, height: 16 });
        refresh_test_execution_snapshot(&mut config.timeline, false);
        let job = RenderJob::new(config);
        let result = FfmpegExportExecutor.execute(
            &job,
            &ExecutionCancellationToken::new(),
            &open_execution_gate(),
            &mut |_| {},
            &mut |_| {},
            &mut |_| {},
        );

        assert!(matches!(result, JobExecutionResult::Published(_)));
        assert!(output.join(crate::image_sequence::MANIFEST_FILE_NAME).is_file());
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(output.join(crate::image_sequence::MANIFEST_FILE_NAME))
                .expect("read published manifest"),
        )
        .expect("parse published manifest");
        assert_eq!(manifest["frame_count"], 1);
        assert!(output
            .join(crate::image_sequence::frame_file_name(
                0,
                crate::preset::ImageSequenceFormat::Png8,
            ))
            .is_file());
    }

    #[test]
    fn executor_publishes_every_high_precision_image_master_after_exact_validation() {
        if mondrian_media::ffmpeg_command().arg("-version").output().is_err() {
            eprintln!("skipping high-precision image-master test: FFmpeg unavailable");
            return;
        }
        let cases = [
            crate::preset::ExportPreset::png16_sequence(),
            crate::preset::ExportPreset::open_exr_half_sequence(),
            crate::preset::ExportPreset::open_exr_float_sequence(),
            crate::preset::ExportPreset::dpx16_sequence(),
            crate::preset::ExportPreset::tiff16_sequence(),
            crate::preset::ExportPreset::tiff_float_sequence(),
        ];
        let parent = tempfile::tempdir().expect("temporary export parent");

        for mut preset in cases {
            let format = preset.image_sequence_format().expect("image representation");
            let output = parent.path().join(format!("{:?}.sequence", format));
            let mut config = dummy_config(&output.to_string_lossy());
            preset.resolution = Some(crate::preset::Resolution { width: 16, height: 16 });
            config.preset = preset;
            refresh_test_execution_snapshot(&mut config.timeline, false);
            let job = RenderJob::new(config);
            let result = FfmpegExportExecutor.execute(
                &job,
                &ExecutionCancellationToken::new(),
                &open_execution_gate(),
                &mut |_| {},
                &mut |_| {},
                &mut |_| {},
            );

            assert!(
                matches!(result, JobExecutionResult::Published(_)),
                "{format:?} failed: {result:?}"
            );
            let manifest: serde_json::Value = serde_json::from_slice(
                &std::fs::read(output.join(crate::image_sequence::MANIFEST_FILE_NAME))
                    .expect("read published manifest"),
            )
            .expect("parse published manifest");
            assert_eq!(manifest["schema_version"], 2);
            assert_eq!(manifest["frame_count"], 1);
            assert!(manifest["frame_contract"].is_string());
            assert!(manifest["output_pixel_format"].is_string());
            assert!(output.join(crate::image_sequence::frame_file_name(0, format)).is_file());
        }
    }

    #[test]
    fn ffmpeg_executor_publishes_all_program_outputs_as_validated_audio_stems() {
        if mondrian_media::ffmpeg_command().arg("-version").output().is_err() {
            eprintln!("skipping audio-stem integration test: FFmpeg unavailable");
            return;
        }
        let directory = tempfile::tempdir().expect("temporary export parent");
        let output = directory.path().join("programs.wavstems");
        let mut config = dummy_config(&output.to_string_lossy());
        config.preset = crate::preset::ExportPreset::audio_stems_pcm24();
        let alternate_output = mondrian_core::ProgramOutputId::new();
        config.timeline.sequence.audio_program.outputs.push(
            mondrian_timeline::audio::AudioProgramOutput {
                id: alternate_output,
                name: "Dialogue & Effects".to_owned(),
                main_source: mondrian_timeline::audio::ProgramOutputMainSource::RoutedInputs,
                strip: mondrian_timeline::audio::AudioChannelStrip::default(),
            },
        );
        refresh_test_execution_snapshot_with_audio_selection(
            &mut config.timeline,
            crate::preset::ExportAudioProgramSelection::All,
        );
        let expected_ids = config
            .timeline
            .sequence
            .audio_program
            .outputs
            .iter()
            .map(|output| output.id)
            .collect::<Vec<_>>();
        let job = RenderJob::new(config);
        let result = FfmpegExportExecutor.execute(
            &job,
            &ExecutionCancellationToken::new(),
            &open_execution_gate(),
            &mut |_| {},
            &mut |_| {},
            &mut |_| {},
        );

        assert!(
            matches!(result, JobExecutionResult::Published(_)),
            "{result:?}"
        );
        let manifest_path = output.join(crate::audio_stems::AUDIO_STEM_MANIFEST_FILE_NAME);
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&manifest_path).expect("read published stem manifest"),
        )
        .expect("parse published stem manifest");
        let stems = manifest["stems"].as_array().expect("stem manifest entries");
        assert_eq!(stems.len(), expected_ids.len());
        for (index, output_id) in expected_ids.into_iter().enumerate() {
            let path = output.join(crate::audio_stems::stem_file_name(index, output_id));
            assert!(path.is_file(), "{}", path.display());
            let probe = crate::validator::probe_export_output(&path).expect("probe published stem");
            assert_eq!(
                probe.audio.and_then(|audio| audio.codec_name),
                Some("pcm_s24le".to_owned())
            );
        }
    }

    #[test]
    fn audio_stem_block_interleave_reuses_bounded_decode_windows_across_outputs() {
        if mondrian_media::ffmpeg_command().arg("-version").output().is_err() {
            eprintln!("skipping audio-stem decode-sharing test: FFmpeg unavailable");
            return;
        }
        let directory = tempfile::tempdir().expect("temporary stem source parent");
        let source_path = directory.path().join("twenty-one-seconds.wav");
        let sample_rate = 48_000_u32;
        let sample_frames = usize::try_from(sample_rate).expect("sample rate") * 21;
        let data_bytes = u32::try_from(sample_frames * 2 * 4).expect("short WAV payload");
        let mut wave = BufWriter::new(
            std::fs::File::create(&source_path).expect("create test float WAV source"),
        );
        wave.write_all(b"RIFF").expect("write RIFF tag");
        wave.write_all(&(36_u32 + data_bytes).to_le_bytes()).expect("write RIFF extent");
        wave.write_all(b"WAVEfmt ").expect("write WAVE/fmt tags");
        wave.write_all(&16_u32.to_le_bytes()).expect("write fmt size");
        wave.write_all(&3_u16.to_le_bytes()).expect("write IEEE-float format");
        wave.write_all(&2_u16.to_le_bytes()).expect("write channel count");
        wave.write_all(&sample_rate.to_le_bytes()).expect("write sample rate");
        wave.write_all(&(sample_rate * 8).to_le_bytes()).expect("write byte rate");
        wave.write_all(&8_u16.to_le_bytes()).expect("write block align");
        wave.write_all(&32_u16.to_le_bytes()).expect("write bit depth");
        wave.write_all(b"data").expect("write data tag");
        wave.write_all(&data_bytes.to_le_bytes()).expect("write data extent");
        let silence = vec![0_u8; 64 * 1024];
        let mut remaining = usize::try_from(data_bytes).expect("payload usize");
        while remaining > 0 {
            let count = remaining.min(silence.len());
            wave.write_all(&silence[..count]).expect("write PCM payload");
            remaining -= count;
        }
        wave.flush().expect("flush float WAV source");
        drop(wave);

        let asset_id = AssetId::new();
        let component_id = AudioSourceComponentId::primary();
        let mut timeline = timeline_input_with_output_color(ColorSpace::Rec709);
        timeline.sequence.settings.audio_sample_rate = sample_rate;
        let time_base = timeline.sequence.time_base();
        let track_id = timeline.sequence.audio_tracks[0].id;
        timeline
            .sequence
            .add_media_audio_clip(
                track_id,
                Clip::new(asset_id, TimelineTime::ZERO, tt(525, time_base))
                    .expect("21-second audio Clip"),
                component_id,
            )
            .expect("add media audio Clip");
        let alternate_output = mondrian_core::ProgramOutputId::new();
        timeline.sequence.audio_program.outputs.push(
            mondrian_timeline::audio::AudioProgramOutput {
                id: alternate_output,
                name: "Shared Decode Stem".to_owned(),
                main_source: mondrian_timeline::audio::ProgramOutputMainSource::RoutedInputs,
                strip: mondrian_timeline::audio::AudioChannelStrip::default(),
            },
        );
        timeline
            .sequence
            .audio_program
            .routes
            .push(mondrian_timeline::audio::AudioRoute::new(
                mondrian_timeline::audio::AudioRouteSource::Track {
                    track_id,
                    port: mondrian_timeline::audio::AudioChannelStripOutputPort::PostMute,
                },
                mondrian_timeline::audio::AudioRouteDestination::Output(alternate_output),
            ));
        let fingerprint = MediaFileFingerprint::capture(&source_path);
        let mut dependency =
            test_media_dependency(source_path, None, AssetMediaInterpretation::default(), None);
        dependency.audio_components.insert(
            component_id,
            mondrian_media::AudioSourceSelection::new(
                0,
                mondrian_media::info::ChannelLayout::Stereo,
                fingerprint,
            ),
        );
        timeline.media.insert(asset_id, dependency);
        refresh_test_execution_snapshot_with_audio_selection(
            &mut timeline,
            crate::preset::ExportAudioProgramSelection::All,
        );
        let delivery = crate::delivery::resolve_export_delivery(
            &crate::preset::ExportPreset::audio_stems_pcm24(),
            &timeline.sequence.settings,
            &timeline.color_environment,
        )
        .expect("resolve stem delivery");
        let range = compute_timeline_render_range_for_delivery(&timeline, &delivery)
            .expect("resolve stem range");
        let prepared_audio = timeline
            .prepared_execution()
            .and_then(|execution| execution.audio())
            .expect("prepared all-output audio closure");
        assert_eq!(prepared_audio.output_count(), 2);

        let mut events = Vec::new();
        let (diagnostics, closure) = {
            let mut report_owner = |event| events.push(event);
            let mut owner = ExportAudioSourceOwner::new(
                // Two retained windows cover the current/next boundary while the
                // 21-second source still exceeds the complete cache working set.
                AudioSourceCacheConfig::new(2, 8 * 1024 * 1024, 1),
                &mut report_owner,
            );
            let rendered = render_audio_stems_to_pcm_f32(
                &timeline,
                prepared_audio,
                range,
                sample_rate,
                AudioChannelLayout::Stereo,
                &mut owner,
                &ExecutionCancellationToken::new(),
                &open_execution_gate(),
                &mut |_| {},
            )
            .expect("render interleaved stem PCM");
            assert_eq!(rendered.len(), 2);
            let cache = owner.cache(sample_rate).expect("inspect shared job cache");
            let diagnostics = cache.diagnostics();
            drop(cache);
            drop(rendered);
            let closure = owner
                .shutdown_until(Instant::now() + Duration::from_secs(2))
                .expect("close used stem owner");
            (diagnostics, closure)
        };

        assert!(diagnostics.hits > 0, "{diagnostics:?}");
        assert!(
            diagnostics.decode_successes <= 4,
            "21 seconds span three ten-second windows; two outputs must not decode each window independently: {diagnostics:?}"
        );
        assert!(closure.all_resources_released(), "{closure:?}");
        assert_eq!(
            events.last(),
            Some(&ExportExecutionOwnerEvent::AudioSourceClosed { all_resources_released: true })
        );
    }

    #[test]
    fn ffmpeg_executor_publishes_probe_qualified_h264_media() {
        if mondrian_media::ffmpeg_command().arg("-version").output().is_err() {
            eprintln!("skipping H.264 integration test: FFmpeg unavailable");
            return;
        }
        let directory = tempfile::tempdir().expect("temporary export parent");
        let output = directory.path().join("qualified.mp4");
        let mut config = dummy_config(&output.to_string_lossy());
        config.preset.resolution = Some(crate::preset::Resolution { width: 256, height: 256 });
        refresh_test_execution_snapshot(&mut config.timeline, true);
        let job = RenderJob::new(config);
        let mut latest_diagnostics = None;
        let result = FfmpegExportExecutor.execute(
            &job,
            &ExecutionCancellationToken::new(),
            &open_execution_gate(),
            &mut |_| {},
            &mut |diagnostics| latest_diagnostics = Some(diagnostics),
            &mut |_| {},
        );

        assert!(
            matches!(result, JobExecutionResult::Published(_)),
            "{result:?}"
        );
        assert!(output.is_file());
        let analysis = latest_diagnostics
            .and_then(|diagnostics| diagnostics.audio)
            .expect("Program audio analysis evidence");
        assert_eq!(analysis.integrated_lufs, None);
        assert_eq!(analysis.true_peak_dbtp, None);
        assert!(analysis.sample_frames > 0);
    }

    #[test]
    fn ffmpeg_export_freezes_and_reports_broadcast_qc_before_publication() {
        if mondrian_media::ffmpeg_command().arg("-version").output().is_err() {
            eprintln!("skipping broadcast QC export integration test: FFmpeg unavailable");
            return;
        }
        let directory = tempfile::tempdir().expect("temporary broadcast QC parent");
        let output = directory.path().join("broadcast-qc.mp4");
        let mut config = dummy_config(&output.to_string_lossy());
        config.preset.resolution = Some(crate::preset::Resolution { width: 256, height: 256 });
        let profile = mondrian_broadcast::BroadcastQcProfile {
            id: "test-broadcaster".to_owned(),
            edition: "2026-01".to_owned(),
            source_sha256: [9; 32],
            signal_color_space: ColorSpace::Rec709,
            observation_tap:
                mondrian_broadcast::BroadcastQcObservationTap::DeliveryPictureAfterLegalizer,
            active_picture: mondrian_broadcast::QcActivePicture::full(256, 256),
            rules: vec![mondrian_broadcast::BroadcastQcRule::Black {
                rule_id: "black-program".to_owned(),
                maximum_encoded_luma: 0.01,
                minimum_coverage_ppm: 1_000_000,
                minimum_frames: 1,
                severity: mondrian_broadcast::BroadcastQcSeverity::Warn,
            }],
            maximum_retained_findings: 8,
            require_regulatory_flash_analysis: false,
            require_encoded_artifact_revalidation: false,
        };
        config.broadcast_qc = Some(profile.clone());
        refresh_test_execution_snapshot(&mut config.timeline, true);
        let job = RenderJob::new(config);
        let mut latest_diagnostics = None;
        let result = FfmpegExportExecutor.execute(
            &job,
            &ExecutionCancellationToken::new(),
            &open_execution_gate(),
            &mut |_| {},
            &mut |diagnostics| latest_diagnostics = Some(diagnostics),
            &mut |_| {},
        );

        assert!(
            matches!(result, JobExecutionResult::Published(_)),
            "{result:?}"
        );
        let report = latest_diagnostics
            .and_then(|diagnostics| diagnostics.broadcast_qc)
            .expect("broadcast QC report");
        assert!(report.complete);
        assert_eq!(report.profile_id, "test-broadcaster");
        assert_eq!(report.verdict, mondrian_broadcast::BroadcastQcVerdict::Warn);
        assert!(report.findings.iter().any(|finding| {
            finding.kind == mondrian_broadcast::BroadcastQcFindingKind::BlackSegment
        }));

        let blocked_output = directory.path().join("broadcast-qc-blocked.mp4");
        let mut blocked_config = dummy_config(&blocked_output.to_string_lossy());
        blocked_config.preset.resolution =
            Some(crate::preset::Resolution { width: 256, height: 256 });
        let mut blocking_profile = profile;
        if let mondrian_broadcast::BroadcastQcRule::Black { severity, .. } =
            &mut blocking_profile.rules[0]
        {
            *severity = mondrian_broadcast::BroadcastQcSeverity::Fail;
        }
        blocked_config.broadcast_qc = Some(blocking_profile);
        refresh_test_execution_snapshot(&mut blocked_config.timeline, true);
        let blocked_job = RenderJob::new(blocked_config);
        let mut blocked_diagnostics = None;
        let blocked = FfmpegExportExecutor.execute(
            &blocked_job,
            &ExecutionCancellationToken::new(),
            &open_execution_gate(),
            &mut |_| {},
            &mut |diagnostics| blocked_diagnostics = Some(diagnostics),
            &mut |_| {},
        );
        assert!(
            matches!(blocked, JobExecutionResult::Failed(_)),
            "{blocked:?}"
        );
        assert!(
            !blocked_output.exists(),
            "fatal QC must not publish the deliverable"
        );
        assert_eq!(
            blocked_diagnostics
                .and_then(|diagnostics| diagnostics.broadcast_qc)
                .map(|report| report.verdict),
            Some(mondrian_broadcast::BroadcastQcVerdict::Fail)
        );
    }

    #[test]
    fn ffmpeg_executor_publishes_each_professional_mezzanine_family() {
        if mondrian_media::ffmpeg_command().arg("-version").output().is_err() {
            eprintln!("skipping professional mezzanine integration test: FFmpeg unavailable");
            return;
        }
        let directory = tempfile::tempdir().expect("temporary mezzanine export parent");
        let cases = [
            (
                crate::preset::ExportPreset::dnxhr_hqx_intermediate(),
                "dnxhr-hqx.mov",
                Some(crate::preset::Resolution { width: 256, height: 128 }),
                "dnxhd",
            ),
            (
                crate::preset::ExportPreset::avc_intra_100_intermediate(),
                "avc-intra-100.mxf",
                None,
                "h264",
            ),
            (
                crate::preset::ExportPreset::uncompressed_v210_master(),
                "uncompressed-v210.mov",
                Some(crate::preset::Resolution { width: 256, height: 128 }),
                "v210",
            ),
            (
                crate::preset::ExportPreset::uncompressed_r210_master(),
                "uncompressed-r210.mov",
                Some(crate::preset::Resolution { width: 256, height: 128 }),
                "r210",
            ),
        ];

        for (mut preset, output_name, test_resolution, expected_codec) in cases {
            if let Some(resolution) = test_resolution {
                preset.resolution = Some(resolution);
            }
            let output = directory.path().join(output_name);
            let mut config = dummy_config(&output.to_string_lossy());
            config.preset = preset;
            refresh_test_execution_snapshot(&mut config.timeline, true);
            let job = RenderJob::new(config);
            let result = FfmpegExportExecutor.execute(
                &job,
                &ExecutionCancellationToken::new(),
                &open_execution_gate(),
                &mut |_| {},
                &mut |_| {},
                &mut |_| {},
            );

            assert!(
                matches!(result, JobExecutionResult::Published(_)),
                "{output_name} failed: {result:?}"
            );
            let probe = crate::validator::probe_export_output(&output)
                .unwrap_or_else(|error| panic!("probe {output_name}: {error}"));
            assert_eq!(
                probe.video.and_then(|video| video.codec_name),
                Some(expected_codec.to_owned())
            );
        }
    }

    #[test]
    fn ffmpeg_executor_smart_renders_full_identity_video_with_packet_proof() {
        if mondrian_media::ffmpeg_command().arg("-version").output().is_err() {
            eprintln!("skipping Smart Render integration test: FFmpeg unavailable");
            return;
        }
        let directory = tempfile::tempdir().expect("temporary Smart Render parent");
        let source = directory.path().join("source.mp4");
        let output = directory.path().join("smart-rendered.mp4");
        let mut make_source = mondrian_media::ffmpeg_command();
        let generated = make_source
            .arg("-y")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-f")
            .arg("lavfi")
            .arg("-i")
            .arg("testsrc2=size=32x32:rate=25:duration=1")
            .arg("-an")
            .arg("-c:v")
            .arg("libx264")
            .arg("-profile:v")
            .arg("high")
            .arg("-pix_fmt")
            .arg("yuv420p")
            .arg("-x264-params")
            .arg("keyint=50:min-keyint=50:bframes=3:scenecut=0:open-gop=0")
            .arg("-color_range")
            .arg("tv")
            .arg("-color_primaries")
            .arg("bt709")
            .arg("-color_trc")
            .arg("bt709")
            .arg("-colorspace")
            .arg("bt709")
            .arg(&source)
            .output()
            .expect("run Smart Render source encoder");
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
        let probe = mondrian_media::probe_media_info(&source).expect("probe Smart Render source");
        let stream = probe.primary_video().expect("source video stream").clone();
        assert_eq!(stream.total_frames, Some(25));
        let source_duration = stream.duration.expect("source stream duration");
        let duration_nanos = i64::try_from(source_duration.as_nanos()).expect("short duration");
        let source_range = TimelineTimeRange::new(
            TimelineTime::ZERO,
            TimelineTime::new(duration_nanos, 1_000_000_000).expect("exact duration"),
        )
        .expect("source range");
        assert_eq!(
            source_range.duration,
            TimelineTime::new(1, 1).expect("one second")
        );

        let asset_id = AssetId::new();
        let mut sequence = Sequence::new("Smart Render identity");
        sequence.settings.resolution = Resolution { width: 32, height: 32 };
        sequence.settings.frame_rate = Rational::FPS_25;
        sequence.settings.color.input.auto_tone_map_media = false;
        sequence.in_point = Some(TimelineTime::ZERO);
        sequence.out_point = Some(TimelineTime::new(24, 25).expect("last source frame"));
        sequence.video_tracks[0]
            .add_clip(
                Clip::new(asset_id, TimelineTime::ZERO, source_range.duration)
                    .expect("identity source Clip"),
            )
            .expect("add identity source Clip");
        let fingerprint = MediaFileFingerprint::capture(&source);
        assert!(fingerprint.authorizes_reuse());
        let dependency = crate::preset::ExportMediaDependency {
            path: source.clone(),
            source_fingerprint: fingerprint,
            source_container: probe.container.clone(),
            source_video_stream: Some(stream.clone()),
            video_stream_index: Some(stream.index),
            picture_source_extent: Some(mondrian_timeline::PictureSourceExtent::TimelineRange(
                source_range,
            )),
            source_resolution: Some(Resolution { width: stream.width, height: stream.height }),
            picture: Some(stream.picture),
            audio_components: HashMap::new(),
            interpretation: AssetMediaInterpretation::default(),
            color_diagnostic: Some(mondrian_media::VideoColorDiagnostic::from_stream(&stream)),
        };
        let mut timeline = TimelineExportSnapshot {
            sequence,
            sequences: Vec::new(),
            media: HashMap::from([(asset_id, dependency)]),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        };
        refresh_test_execution_snapshot(&mut timeline, true);
        let mut preset = crate::preset::ExportPreset::h264_aac_sdr_1080p();
        preset.resolution = None;
        preset.color_target = crate::preset::ExportColorTarget::Colorimetric(ColorSpace::Rec709);
        let job = RenderJob::new(ExportConfig {
            preset,
            timeline: Box::new(timeline),
            output_path: output.clone(),
            output_policy: ExportOutputPolicy::CreateNew,
            smart_render: crate::preset::ExportSmartRenderPolicy::Automatic,
            broadcast_qc: None,
        });
        let mut latest_diagnostics = None;
        let result = FfmpegExportExecutor.execute(
            &job,
            &ExecutionCancellationToken::new(),
            &open_execution_gate(),
            &mut |_| {},
            &mut |diagnostics| latest_diagnostics = Some(diagnostics),
            &mut |_| {},
        );

        assert!(
            matches!(result, JobExecutionResult::Published(_)),
            "{result:?}"
        );
        let evidence = latest_diagnostics
            .and_then(|diagnostics| diagnostics.smart_render)
            .expect("Smart Render evidence");
        assert_eq!(evidence.source_asset_id, asset_id);
        assert!(evidence.packet_identity_verified);
        assert!(evidence.packet_count > 0);
        assert!(evidence.payload_bytes > 0);
        let source_identity =
            mondrian_media::capture_video_packet_identity(&source, Some(stream.index))
                .expect("source packet identity");
        let output_identity = mondrian_media::capture_video_packet_identity(&output, None)
            .expect("output packet identity");
        assert_eq!(source_identity.packet_count, output_identity.packet_count);
        assert_eq!(source_identity.payload_bytes, output_identity.payload_bytes);
        assert_eq!(
            source_identity.payload_digest,
            output_identity.payload_digest
        );
    }

    fn test_delivery_contract(
        bit_depth: DeliveryBitDepth,
        video_range: VideoRange,
        chroma_sampling: ExportChromaSampling,
        pixel_format: &'static str,
    ) -> ResolvedExportDeliveryContract {
        ResolvedExportDeliveryContract {
            artifact: ResolvedExportArtifactEncoding::MediaFile {
                container: Container::Mp4,
                video: VideoCodecConfig::H264 {
                    profile: crate::preset::H264Profile::High,
                    rate_control: VideoRateControl::constant_quality(18),
                },
                audio: AudioCodecConfig::Aac { bitrate_kbps: 192 },
            },
            resolution: crate::preset::Resolution { width: 1_920, height: 1_080 },
            frame_rate: Rational::FPS_25,
            frame_sampling: crate::preset::ExportFrameSampling::FrameHold,
            video_coding: crate::video_encoding::ResolvedVideoCodingStructure::H26xLongGop {
                keyframe_interval_frames: 50,
                max_b_frames: 3,
                closed_gop: true,
                scene_cut: crate::video_encoding::VideoSceneCutPolicy::Disabled,
            },
            sample_aspect_ratio: mondrian_core::SampleAspectRatio::SQUARE,
            field_order: mondrian_core::timeline_data::FieldOrder::Progressive,
            bit_depth,
            video_range,
            chroma_sampling,
            pixel_format,
            color_target: crate::delivery::ResolvedExportColorTarget {
                color_space: ColorSpace::Rec709,
                tone_map: true,
                output_transform: mondrian_core::OutputTransformIntent::mondrian_standard(),
            },
            legalizer: SignalLegalizer::Off,
        }
    }

    #[test]
    fn resident_hevc_admission_is_exact_and_resource_bounded() {
        let timeline = timeline_input_with_output_color(ColorSpace::Rec709);
        let mut delivery = test_delivery_contract(
            DeliveryBitDepth::Ten,
            VideoRange::Legal,
            ExportChromaSampling::Yuv420,
            "yuv420p10le",
        );
        delivery.artifact = ResolvedExportArtifactEncoding::MediaFile {
            container: Container::Mkv,
            video: VideoCodecConfig::Hevc {
                profile: HevcProfile::Main10,
                rate_control: VideoRateControl::constant_quality(18),
            },
            audio: AudioCodecConfig::Disabled,
        };
        let plan = qualify_resident_hevc_export(
            &timeline,
            &delivery,
            ExportAlphaMode::FlattenBlack,
            ExportExecutionResourcePolicy::default(),
        )
        .expect("qualified Main10 resident route");
        assert_eq!(plan.bit_depth, ResidentEncodeBitDepth::Ten);
        assert_eq!(plan.colorimetry, ResidentEncodeColorimetry::Rec709);
        assert!(!plan.full_range);
        assert_eq!(plan.surface_pool_size, 8);
    }

    #[test]
    fn resident_hevc_rejects_alpha_legalizer_and_vbv_before_backend_access() {
        let timeline = timeline_input_with_output_color(ColorSpace::Rec709);
        let mut delivery = test_delivery_contract(
            DeliveryBitDepth::Eight,
            VideoRange::Legal,
            ExportChromaSampling::Yuv420,
            "yuv420p",
        );
        delivery.artifact = ResolvedExportArtifactEncoding::MediaFile {
            container: Container::Mkv,
            video: VideoCodecConfig::Hevc {
                profile: HevcProfile::Main,
                rate_control: VideoRateControl::constant_quality(20),
            },
            audio: AudioCodecConfig::Disabled,
        };
        let policy = ExportExecutionResourcePolicy::default();
        assert_eq!(
            qualify_resident_hevc_export(&timeline, &delivery, ExportAlphaMode::Preserve, policy,),
            Err(ExportResidentEncodeBlocker::AlphaPreservation)
        );
        delivery.legalizer = SignalLegalizer::ClampRgb;
        assert_eq!(
            qualify_resident_hevc_export(
                &timeline,
                &delivery,
                ExportAlphaMode::FlattenBlack,
                policy,
            ),
            Err(ExportResidentEncodeBlocker::Legalizer)
        );
        delivery.legalizer = SignalLegalizer::Off;
        let ResolvedExportArtifactEncoding::MediaFile { video, .. } = &mut delivery.artifact else {
            unreachable!();
        };
        *video = VideoCodecConfig::Hevc {
            profile: HevcProfile::Main,
            rate_control: VideoRateControl::constrained_quality(20, 20_000, 40_000),
        };
        assert_eq!(
            qualify_resident_hevc_export(
                &timeline,
                &delivery,
                ExportAlphaMode::FlattenBlack,
                policy,
            ),
            Err(ExportResidentEncodeBlocker::RateControl)
        );
    }

    #[test]
    fn resident_hevc_rejects_unrepresentable_hdr_and_surface_grant() {
        let timeline = timeline_input_with_output_color(ColorSpace::Rec2100Pq);
        let mut delivery = test_delivery_contract(
            DeliveryBitDepth::Ten,
            VideoRange::Full,
            ExportChromaSampling::Yuv420,
            "yuv420p10le",
        );
        delivery.color_target.color_space = ColorSpace::Rec2100Pq;
        delivery.artifact = ResolvedExportArtifactEncoding::MediaFile {
            container: Container::Mkv,
            video: VideoCodecConfig::Hevc {
                profile: HevcProfile::Main10,
                rate_control: VideoRateControl::constant_quality(18),
            },
            audio: AudioCodecConfig::Disabled,
        };
        assert_eq!(
            qualify_resident_hevc_export(
                &timeline,
                &delivery,
                ExportAlphaMode::FlattenBlack,
                ExportExecutionResourcePolicy::default(),
            ),
            Err(ExportResidentEncodeBlocker::Signal)
        );
        delivery.video_range = VideoRange::Legal;
        let policy = ExportExecutionResourcePolicy {
            resident_encoder_surface_bytes: 1,
            ..ExportExecutionResourcePolicy::default()
        };
        assert_eq!(
            qualify_resident_hevc_export(
                &timeline,
                &delivery,
                ExportAlphaMode::FlattenBlack,
                policy,
            ),
            Err(ExportResidentEncodeBlocker::ResourceGrant)
        );
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
        refresh_test_execution_snapshot_with_audio_selection(
            timeline,
            if include_audio {
                crate::preset::ExportAudioProgramSelection::Primary
            } else {
                crate::preset::ExportAudioProgramSelection::Disabled
            },
        );
    }

    fn refresh_test_execution_snapshot_with_audio_selection(
        timeline: &mut TimelineExportSnapshot,
        audio_selection: crate::preset::ExportAudioProgramSelection,
    ) {
        let resource_policy = service::ExportExecutionResourcePolicy::default();
        let mut execution = crate::prepare_timeline_export_dependencies_with_audio_selection(
            &timeline.sequence,
            &timeline.sequences,
            timeline.range,
            audio_selection,
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
            source_container: String::new(),
            source_video_stream: None,
            video_stream_index: Some(0),
            picture_source_extent: Some(mondrian_timeline::PictureSourceExtent::Still),
            source_resolution: Some(Resolution { width: 1, height: 1 }),
            picture: Some(Default::default()),
            audio_components: HashMap::new(),
            interpretation,
            color_diagnostic,
        }
    }

    fn square_picture_geometry(resolution: Resolution) -> ResolvedPictureGeometry {
        ResolvedPictureGeometry::square(resolution).expect("non-empty test picture geometry")
    }

    fn data_texture_interpretation() -> AssetMediaInterpretation {
        AssetMediaInterpretation {
            payload: AssetColorPayload::NonColorData,
            ..AssetMediaInterpretation::default()
        }
    }

    #[test]
    fn export_source_preparation_resolves_rgb_data_texture_without_color_identity() {
        let source = tempfile::NamedTempFile::new().expect("temporary RGB data source");
        std::fs::write(source.path(), b"rgb data identity").expect("write source identity");
        let dependency = test_media_dependency(
            source.path().to_path_buf(),
            Some(ColorSpace::Srgb),
            data_texture_interpretation(),
            None,
        );
        let mut sequence = Sequence::new("data texture export");
        sequence.settings.color.input.missing_metadata_policy =
            MissingColorMetadataPolicy::RejectMedia;
        let color_context = sequence
            .settings
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
            .expect("valid rejecting context");
        let preparation = resolve_export_source_preparation(
            ExportSourcePreparationRequest {
                asset_id: AssetId::new(),
                color_space_override: Some(ColorSpace::Rec2100Pq),
                auto_tone_map: true,
                dependency: &dependency,
                input_video_range: DecodedVideoRangeContract::OverrideFull,
                color_context: &color_context,
            },
            None,
        )
        .expect("proven RGB data texture must bypass color management");

        assert_eq!(
            preparation.source_contract,
            PreviewSourceColorContract::data_texture(DecodedVideoRangeContract::OverrideFull)
        );
        assert_eq!(
            preparation.preparation_intent,
            SourceFramePreparationIntent::data_texture(color_context.working_color_space())
        );
        assert_eq!(
            preparation.resolution_source,
            InputColorResolutionSource::DataTexture
        );

        let different_environment =
            mondrian_core::ProjectColorEnvironment::new(ColorEngine::Aces {
                preset: mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
            });
        let different_engine = sequence
            .settings
            .root_program_color_context(&different_environment)
            .expect("valid ACES context");
        let under_different_engine = resolve_export_source_preparation(
            ExportSourcePreparationRequest {
                asset_id: AssetId::new(),
                color_space_override: None,
                auto_tone_map: false,
                dependency: &dependency,
                input_video_range: DecodedVideoRangeContract::OverrideFull,
                color_context: &different_engine,
            },
            None,
        )
        .expect("DataTexture identity is independent of OCIO engine and tone mapping");
        assert_eq!(preparation, under_different_engine);
    }

    #[test]
    fn export_source_preparation_rejects_yuv_data_texture_before_decode() {
        let source = tempfile::NamedTempFile::new().expect("temporary YUV data source");
        std::fs::write(source.path(), b"yuv data identity").expect("write source identity");
        let dependency = test_media_dependency(
            source.path().to_path_buf(),
            Some(ColorSpace::Rec709),
            data_texture_interpretation(),
            None,
        );
        let color_context = Sequence::new("rejected YUV data texture")
            .settings
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
            .expect("valid test context");
        let error = resolve_export_source_preparation(
            ExportSourcePreparationRequest {
                asset_id: AssetId::new(),
                color_space_override: None,
                auto_tone_map: false,
                dependency: &dependency,
                input_video_range: DecodedVideoRangeContract::OverrideLimited,
                color_context: &color_context,
            },
            None,
        )
        .expect_err("YCbCr conversion would alter technical channels and must fail closed");

        assert!(error.contains("data-texture export requires RGB source sampling"));
        assert!(error.contains("Yuv420p"));
    }

    #[test]
    fn rgb_data_texture_decodes_and_materializes_for_export_without_color_stages() {
        let root = tempfile::tempdir().expect("temporary RGB data directory");
        let source = root.path().join("technical.ppm");
        let mut ppm = b"P6\n1 1\n255\n".to_vec();
        ppm.extend_from_slice(&[17, 64, 255]);
        std::fs::write(&source, ppm).expect("write one-pixel RGB data image");
        let dependency = test_media_dependency(
            source,
            Some(ColorSpace::Srgb),
            data_texture_interpretation(),
            None,
        );
        let color_context = Sequence::new("RGB data texture vertical slice")
            .settings
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
            .expect("valid test context");
        let preparation = resolve_export_source_preparation(
            ExportSourcePreparationRequest {
                asset_id: AssetId::new(),
                color_space_override: None,
                auto_tone_map: false,
                dependency: &dependency,
                input_video_range: DecodedVideoRangeContract::OverrideFull,
                color_context: &color_context,
            },
            None,
        )
        .expect("resolve RGB data texture");
        let mut decode_context = PreviewDecodeSessionContext::new();
        let mut color_session = mondrian_renderer::RenderCpuColorExecutionSession::new(0);
        let cancellation = ExecutionCancellationToken::new();
        let decoded = decode_video_layer_scaled(
            ExportVideoLayerDecodeRequest {
                asset_id: AssetId::new(),
                dependency: &dependency,
                source_sample: mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
                decode_resolution: Resolution { width: 1, height: 1 },
                source_resolution: Resolution { width: 1, height: 1 },
                picture_geometry: square_picture_geometry(Resolution { width: 1, height: 1 }),
                source_color: preparation.source_contract,
                alpha_interpretation: AlphaInterpretation::Straight,
                preparation_intent: preparation.preparation_intent,
            },
            ExportVideoLayerDecodeExecutionContext {
                color_session: &mut color_session,
                decode_context: &mut decode_context,
                cancellation: &cancellation,
            },
        )
        .expect("decode and prepare RGB data texture");

        assert!(decoded.is_data_texture);
        assert_eq!(
            decoded.stage_diagnostics,
            RenderColorStageDiagnostics::default()
        );
        let pixel = decoded.frame.rgba_f32().data[0];
        assert!((pixel[0] - 17.0 / 255.0).abs() < 1.0e-6);
        assert!((pixel[1] - 64.0 / 255.0).abs() < 1.0e-6);
        assert_eq!(pixel[2], 1.0);
        assert_eq!(pixel[3], 1.0);
        assert_eq!(
            decoded.frame.rgba_f32().color_space,
            color_context.working_color_space()
        );
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
        let root_environment = mondrian_core::ProjectColorEnvironment::new(ColorEngine::Aces {
            preset: mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
        });
        let root_context = root_sequence
            .settings
            .root_program_color_context(&root_environment)
            .expect("valid root context");
        let mut nested_working_sequence = Sequence::new("alternate working context");
        nested_working_sequence.settings.color.working_color_space =
            match root_context.working_color_space() {
                WorkingColorSpace::LinearRec709 => WorkingColorSpace::LinearRec2020,
                _ => WorkingColorSpace::LinearRec709,
            };
        let nested_working_context = nested_working_sequence
            .settings
            .root_program_color_context(&root_environment)
            .expect("valid alternate working context");
        let nested_engine_environment = mondrian_core::ProjectColorEnvironment::default();
        let nested_engine_context = root_sequence
            .settings
            .root_program_color_context(&nested_engine_environment)
            .expect("valid alternate engine context");
        let source_resolution = Resolution { width: 3_840, height: 2_160 };
        let decode_resolution = Resolution { width: 1_920, height: 1_080 };
        let build_key = |context: &ProgramColorContext, auto_tone_map: bool| {
            let source_contract = PreviewSourceColorContract::new(
                ColorSpace::Rec709,
                DecodedVideoRangeContract::OverrideFull,
            );
            let preparation_intent = RenderInputTransform::to_working(
                context.working_color_space(),
                auto_tone_map,
                context.engine().clone(),
            )
            .into();
            ExportDecodeCacheKey::new(
                asset_id,
                &dependency,
                mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
                source_contract,
                preparation_intent,
                AlphaInterpretation::Straight,
                decode_resolution,
                source_resolution,
                square_picture_geometry(source_resolution),
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
                picture_geometry: square_picture_geometry(Resolution { width: 16, height: 16 }),
                source_color: PreviewSourceColorContract::new(
                    ColorSpace::Rec709,
                    DecodedVideoRangeContract::OverrideLimited,
                ),
                alpha_interpretation: AlphaInterpretation::Straight,
                preparation_intent: RenderInputTransform::to_working(
                    WorkingColorSpace::LinearRec709,
                    false,
                    ColorEngine::mondrian_standard(),
                )
                .into(),
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
                    picture_geometry: square_picture_geometry(Resolution { width: 16, height: 16 }),
                    source_color: PreviewSourceColorContract::new(
                        ColorSpace::Rec709,
                        DecodedVideoRangeContract::OverrideLimited,
                    ),
                    alpha_interpretation: AlphaInterpretation::Straight,
                    preparation_intent: RenderInputTransform::to_working(
                        WorkingColorSpace::LinearRec709,
                        false,
                        ColorEngine::mondrian_standard(),
                    )
                    .into(),
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
                picture_geometry: square_picture_geometry(Resolution { width: 16, height: 16 }),
                source_color: PreviewSourceColorContract::new(
                    ColorSpace::Rec709,
                    DecodedVideoRangeContract::OverrideLimited,
                ),
                alpha_interpretation: AlphaInterpretation::Straight,
                preparation_intent: RenderInputTransform::to_working(
                    WorkingColorSpace::LinearRec709,
                    false,
                    ColorEngine::mondrian_standard(),
                )
                .into(),
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
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
            .expect("valid test context");
        let source_contract = PreviewSourceColorContract::new(
            ColorSpace::Rec709,
            DecodedVideoRangeContract::OverrideFull,
        );
        let preparation_intent = RenderInputTransform::to_working(
            context.working_color_space(),
            false,
            context.engine().clone(),
        )
        .into();
        let original = ExportDecodeCacheKey::new(
            asset_id,
            &dependency,
            mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
            source_contract,
            preparation_intent,
            AlphaInterpretation::Straight,
            Resolution { width: 1_920, height: 1_080 },
            Resolution { width: 3_840, height: 2_160 },
            square_picture_geometry(Resolution { width: 3_840, height: 2_160 }),
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
        changed.source_contract = PreviewSourceColorContract::new(
            ColorSpace::Srgb,
            DecodedVideoRangeContract::OverrideFull,
        );
        assert_ne!(original, changed);
        let mut changed = original.clone();
        changed.source_contract = PreviewSourceColorContract::new(
            ColorSpace::Rec709,
            DecodedVideoRangeContract::OverrideLimited,
        );
        assert_ne!(original, changed);
        let mut changed = original.clone();
        changed.preparation_intent =
            SourceFramePreparationIntent::data_texture(WorkingColorSpace::LinearRec709);
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
        let mut changed = original.clone();
        changed.camera_raw = Some(
            mondrian_media::CameraRawDecodeIntent::new(
                mondrian_core::CameraRawAdapter::Dng,
                mondrian_core::CameraRawInterpretation {
                    exposure_millistops: 1_000,
                    ..mondrian_core::CameraRawInterpretation::default()
                },
            )
            .expect("valid RAW cache identity"),
        );
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
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
            .expect("valid test context");
        let source_contract = PreviewSourceColorContract::new(
            ColorSpace::Rec709,
            DecodedVideoRangeContract::OverrideLimited,
        );
        let preparation_intent = RenderInputTransform::to_working(
            context.working_color_space(),
            false,
            context.engine().clone(),
        )
        .into();

        let error = ExportDecodeCacheKey::new(
            asset_id,
            &dependency,
            mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
            source_contract,
            preparation_intent,
            AlphaInterpretation::Straight,
            Resolution { width: 1, height: 1 },
            Resolution { width: 1, height: 1 },
            square_picture_geometry(Resolution { width: 1, height: 1 }),
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
        let queue = RenderQueue::new_with_executor(Arc::new(DiagnosticExecutor {
            diagnostics: diagnostics.clone(),
        }));

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
        let root = SequenceSettings::default()
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
            .expect("valid root context");
        let ctx = root
            .for_rendering_view_output(ColorSpace::Srgb)
            .expect("valid sRGB rendering View context");

        let boundary = export_output_boundary_from_context(&ctx).expect("encoded output");
        assert_eq!(boundary.target(), ProgramOutputRole::Export);
        assert!(boundary.ocio_display_view().is_some());
        assert!(boundary.tone_map());
        let dv = boundary.ocio_display_view().expect("resolved display/view");
        assert_eq!(dv.display, "sRGB - Display");
        assert_eq!(dv.view, "Mondrian Standard SDR v2");
    }

    #[test]
    fn export_context_rejects_engine_intent_without_tone_flag() {
        let root = SequenceSettings::default()
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
            .expect("valid root context");
        let error = root
            .for_export_output(
                ColorSpace::Rec709,
                false,
                mondrian_core::OutputTransformIntent::mondrian_standard(),
            )
            .expect_err("rendering View without tone-map mode must be rejected");
        assert!(error.to_string().contains("disagrees"), "{error:#}");
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
        let ctx = timeline
            .sequence
            .settings
            .root_program_color_context(&timeline.color_environment)
            .expect("valid root context");

        let boundary = export_output_boundary_from_context(&ctx).expect("encoded output");
        // The boundary has a view -> no issue should be recorded.
        assert!(boundary.ocio_display_view().is_some());
        assert!(boundary.tone_map());
        assert_eq!(boundary.target(), ProgramOutputRole::Export);

        let mut diagnostics = ExportJobColorDiagnostics::default();
        let mut canvas = vec![0u8; 2 * 2 * 4];
        let mut visual_session = ExportVisualRenderSession::default();
        let cancellation = ExecutionCancellationToken::new();
        let mut render_context = ExportFrameRenderContext {
            media: &timeline.media,
            color_environment: &timeline.color_environment,
            alpha_mode: ExportAlphaMode::FlattenBlack,
            delivery_pixels: ExportDeliveryPixelContract::unmodified(
                ExportFrameContract::EncodedRgba8Unorm,
            ),
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
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
            .expect("valid test context");
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
            delivery_pixels: ExportDeliveryPixelContract::unmodified(
                ExportFrameContract::EncodedRgba8Unorm,
            ),
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
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
            .expect("valid test context");
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
            delivery_pixels: ExportDeliveryPixelContract::unmodified(
                ExportFrameContract::EncodedRgba8Unorm,
            ),
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
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
            .expect("valid test context");
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
            delivery_pixels: ExportDeliveryPixelContract::unmodified(
                ExportFrameContract::EncodedRgba8Unorm,
            ),
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
    #[ignore = "real GPU Export nested residency qualification; run independently"]
    fn export_gpu_visual_path_keeps_nested_output_resident_until_final_readback() {
        let _gpu_visual = GpuVisualExecutionGuard::activate();
        let mut child = Sequence::new("GPU nested child");
        child.settings.resolution = Resolution { width: 4, height: 2 };
        let child_time_base = child.time_base();
        child.video_tracks[0]
            .add_clip(
                Clip::new_solid_color(
                    AssetId::new(),
                    mondrian_core::Color::from_rgba8(64, 128, 255, 255),
                    tt(0, child_time_base),
                    tt(1, child_time_base),
                )
                .expect("GPU nested solid"),
            )
            .expect("place GPU nested solid");

        let mut root = Sequence::new("GPU nested root");
        root.settings.resolution = Resolution { width: 4, height: 2 };
        let root_time_base = root.time_base();
        root.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(
                    child.id,
                    tt(0, root_time_base),
                    tt(1, root_time_base),
                    None,
                )
                .expect("GPU nested Sequence"),
            )
            .expect("place GPU nested Sequence");
        let color_environment = mondrian_core::ProjectColorEnvironment::default();
        let color_context = root
            .settings
            .root_program_color_context(&color_environment)
            .expect("GPU nested color context");
        let mut timeline = TimelineExportSnapshot {
            sequence: root,
            sequences: vec![child],
            media: HashMap::new(),
            color_environment,
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        };
        let mut visual_session = captured_visual_session_for_test(&mut timeline);
        if visual_session.gpu_output.begin_visual_frame().is_err() {
            eprintln!("skipping Export GPU nested test: no GPU adapter available");
            return;
        }
        let cancellation = ExecutionCancellationToken::new();
        let mut canvas = Vec::new();
        {
            let mut render_context = ExportFrameRenderContext {
                media: &timeline.media,
                color_environment: &timeline.color_environment,
                alpha_mode: ExportAlphaMode::Preserve,
                delivery_pixels: ExportDeliveryPixelContract::unmodified(
                    ExportFrameContract::EncodedRgba8Unorm,
                ),
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
                Resolution { width: 4, height: 2 },
                color_context,
                SequenceRenderTarget::Deliverable(&mut canvas),
            )
            .expect("Export GPU nested render");
        }

        let diagnostics = visual_session.visual_diagnostics();
        assert_eq!(diagnostics.gpu_visual_nodes_completed, 2);
        assert_eq!(diagnostics.gpu_visual_nested_outputs, 1);
        assert_eq!(diagnostics.gpu_visual_output_readbacks, 1);
        assert!(diagnostics.gpu_visual_peak_active_bytes > 0);
        assert!(diagnostics.gpu_visual_peak_active_textures >= 3);
        assert_eq!(canvas.len(), 4 * 2 * 4);
        assert!(canvas.chunks_exact(4).all(|pixel| pixel[3] == 255));
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
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
            .expect("valid test context");
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
            delivery_pixels: ExportDeliveryPixelContract::unmodified(
                ExportFrameContract::EncodedRgba8Unorm,
            ),
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
        let media = config.preset.media_file_mut().expect("media preset");
        media.container = Container::Mov;
        media.video = VideoCodecConfig::ProRes { profile: ProResProfile::Hq };
        media.video_coding = crate::video_encoding::VideoCodingStructure::IntraOnly;
        config.preset.video_signal = ExportVideoSignal {
            bit_depth: ExportParameter::Explicit(DeliveryBitDepth::Twelve),
            range: ExportParameter::Explicit(VideoRange::Full),
            chroma_sampling: ExportChromaSampling::Yuv422,
        };
        let err = resolve_timeline_export_delivery(&config, &timeline)
            .expect_err("ProRes HQ is a 10-bit profile");
        assert!(err.contains("profile"));

        config.preset.media_file_mut().expect("media preset").video =
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
            let media = config.preset.media_file_mut().expect("media preset");
            media.container = container;
            media.video_coding = match codec {
                VideoCodecConfig::Av1 { .. } => {
                    crate::video_encoding::VideoCodingStructure::av1_delivery()
                }
                VideoCodecConfig::ProRes { .. } => {
                    crate::video_encoding::VideoCodingStructure::IntraOnly
                }
                _ => unreachable!("test matrix contains only AV1 and ProRes"),
            };
            media.video = codec;
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
    fn export_color_validation_requires_explicit_dynamic_hdr_delivery_intent() {
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

        resolve_timeline_export_delivery(&config, &timeline)
            .expect("Omit explicitly authorizes rendered output without dynamic metadata");

        timeline
            .sequence
            .dynamic_hdr
            .apply(
                mondrian_timeline::DynamicHdrAuthorEdit::SetDeliveryIntent {
                    intent: mondrian_timeline::DynamicHdrDeliveryIntent::PreserveSourceExact {
                        family: mondrian_core::DynamicHdrMetadataFamily::St2094_40Application4,
                    },
                },
                timeline.sequence.settings.frame_rate,
            )
            .expect("select exact preservation");
        resolve_timeline_export_delivery(&config, &timeline)
            .expect("detected ST 2094-40 family admits the explicit preservation intent");

        timeline
            .sequence
            .dynamic_hdr
            .apply(
                mondrian_timeline::DynamicHdrAuthorEdit::SetDeliveryIntent {
                    intent: mondrian_timeline::DynamicHdrDeliveryIntent::PreserveSourceExact {
                        family: mondrian_core::DynamicHdrMetadataFamily::DolbyVision,
                    },
                },
                timeline.sequence.settings.frame_rate,
            )
            .expect("select mismatched preservation family");
        let error = resolve_timeline_export_delivery(&config, &timeline)
            .expect_err("preservation family must be proven by frozen diagnostics");
        assert!(error.contains("Dolby Vision metadata"));
        assert!(error.contains("do not detect that metadata family"));
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
        let media = config.preset.media_file_mut().expect("media preset");
        media.container = Container::Gif;
        media.video = VideoCodecConfig::Gif { colors: 256, dither: true };
        media.video_coding = crate::video_encoding::VideoCodingStructure::IntraOnly;
        media.audio = AudioCodecConfig::Disabled;
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
        assert_eq!(range.evaluation_frame(0), Ok(40));
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
        assert_eq!(range.evaluation_frame(0), Ok(0));
        assert_eq!(range.total_frames, 200);
    }

    #[test]
    fn timeline_render_range_converts_cadence_without_duration_drift() {
        let mut seq = Sequence::new("cadence-conversion");
        seq.settings.frame_rate = Rational::FPS_24;
        let tb = seq.time_base();
        let clip = Clip::new(AssetId::new(), tt(0, tb), tt(24, tb)).expect("one second clip");
        seq.video_tracks[0].add_clip(clip).expect("add clip");
        let timeline = TimelineExportSnapshot {
            sequence: seq,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::EntireSequence,
        };
        let mut delivery = test_delivery_contract(
            DeliveryBitDepth::Eight,
            VideoRange::Legal,
            ExportChromaSampling::Yuv420,
            "yuv420p",
        );
        delivery.frame_rate = Rational::FPS_30;

        let range = compute_timeline_render_range_for_delivery(&timeline, &delivery)
            .expect("exact cadence conversion");

        assert_eq!(range.total_frames, 30);
        assert_eq!(range.evaluation_frame(0), Ok(0));
        assert_eq!(range.evaluation_frame(1), Ok(0));
        assert_eq!(range.evaluation_frame(2), Ok(1));
        assert_eq!(range.evaluation_frame(29), Ok(23));
        assert_eq!(
            range.time_range().expect("exact range").duration,
            tt(24, tb)
        );
    }

    #[test]
    fn timeline_render_range_preserves_marked_source_offset_across_cadences() {
        let mut seq = Sequence::new("offset-cadence-conversion");
        seq.settings.frame_rate = Rational::FPS_25;
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
        let mut delivery = test_delivery_contract(
            DeliveryBitDepth::Eight,
            VideoRange::Legal,
            ExportChromaSampling::Yuv420,
            "yuv420p",
        );
        delivery.frame_rate = Rational::FPS_30;

        let range = compute_timeline_render_range_for_delivery(&timeline, &delivery)
            .expect("offset cadence conversion");

        assert_eq!(range.total_frames, 72);
        assert_eq!(range.evaluation_frame(0), Ok(40));
        assert_eq!(range.evaluation_frame(1), Ok(40));
        assert_eq!(range.evaluation_frame(71), Ok(99));
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
            .root_program_color_context(&timeline.color_environment)
            .expect("valid test context");
        let mut canvas = Vec::new();
        let error = render_timeline_frame_into_with_session(
            &timeline,
            1,
            16,
            16,
            ExportAlphaMode::FlattenBlack,
            color_context,
            ExportDeliveryPixelContract::unmodified(ExportFrameContract::EncodedRgba8Unorm),
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
            source_start: TimelineTime::ZERO,
            total_frames: 50,
            fps_num: 25,
            fps_den: 1,
            sequence_frame_rate: Rational::FPS_25,
            frame_sampling: crate::preset::ExportFrameSampling::FrameHold,
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
            crate::video_encoding::ResolvedVideoCodingStructure::H26xLongGop {
                keyframe_interval_frames: 60,
                max_b_frames: 3,
                closed_gop: true,
                scene_cut: crate::video_encoding::VideoSceneCutPolicy::Disabled,
            },
            crate::hardware_encoding::ResolvedVideoEncoder::Libx264,
        );
        let h264_args = h264
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(h264_args.windows(2).any(|pair| pair == ["-profile:v", "high"]));
        assert!(h264_args.windows(2).any(|pair| pair == ["-maxrate", "8000k"]));
        assert!(h264_args.windows(2).any(|pair| pair == ["-bufsize", "16000k"]));
        assert!(!h264_args.iter().any(|arg| arg == "-b:v"));
        assert!(h264_args.windows(2).any(|pair| pair == ["-g", "60"]));
        assert!(h264_args.windows(2).any(|pair| pair == ["-bf", "3"]));
        assert!(h264_args.windows(2).any(|pair| pair == ["-flags", "+cgop"]));

        let mut hevc = Command::new("ffmpeg");
        apply_video_codec_args(
            &mut hevc,
            &VideoCodecConfig::Hevc {
                profile: HevcProfile::Main10,
                rate_control: VideoRateControl::constant_quality(20),
            },
            crate::video_encoding::ResolvedVideoCodingStructure::H26xLongGop {
                keyframe_interval_frames: 50,
                max_b_frames: 2,
                closed_gop: true,
                scene_cut: crate::video_encoding::VideoSceneCutPolicy::Adaptive,
            },
            crate::hardware_encoding::ResolvedVideoEncoder::Libx265,
        );
        let hevc_args = hevc
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(hevc_args.windows(2).any(|pair| pair == ["-c:v", "libx265"]));
        assert!(hevc_args.windows(2).any(|pair| pair == ["-profile:v", "main10"]));
        assert!(hevc_args.windows(2).any(|pair| pair == ["-crf", "20"]));
        assert!(hevc_args.windows(2).any(|pair| pair == ["-g", "50"]));
        assert!(hevc_args.windows(2).any(|pair| pair == ["-bf", "2"]));
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
                && pair[1]
                    == "scale=iw:ih:in_range=full:out_range=limited:out_color_matrix=bt2020,setsar=1/1"
        }));
        assert!(args.windows(2).any(|pair| pair == ["-field_order", "progressive"]));
    }

    #[test]
    fn interlaced_signal_args_and_validation_share_tff_contract() {
        let settings = SequenceSettings {
            frame_rate: Rational::FPS_25,
            field_order: mondrian_core::timeline_data::FieldOrder::UpperFirst,
            ..SequenceSettings::default()
        };
        let mut delivery = test_delivery_contract(
            DeliveryBitDepth::Ten,
            VideoRange::Legal,
            ExportChromaSampling::Yuv422,
            "yuv422p10le",
        );
        delivery.field_order = mondrian_core::timeline_data::FieldOrder::UpperFirst;
        delivery.artifact = ResolvedExportArtifactEncoding::MediaFile {
            container: Container::Mov,
            video: VideoCodecConfig::ProRes { profile: crate::preset::ProResProfile::Hq },
            audio: AudioCodecConfig::Pcm { bit_depth: 24 },
        };

        let mut cmd = Command::new("ffmpeg");
        apply_export_video_signal_args(&mut cmd, &settings, &delivery);
        let args = cmd.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert!(args.windows(2).any(|pair| pair == ["-field_order", "tt"]));
        assert!(args.windows(2).any(|pair| pair == ["-top", "1"]));
        assert!(args.windows(2).any(|pair| pair == ["-flags", "+ildct+ilme"]));
        assert!(args.windows(2).any(|pair| {
            pair[0] == "-vf" && pair[1].contains("out_color_matrix=bt709:interl=1")
        }));

        let expected = expected_export_video_signal(&settings, &delivery)
            .expect("qualified TFF signal expectation");
        assert_eq!(expected.field_order.as_deref(), Some("tt"));
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
            crate::hardware_encoding::ResolvedVideoEncoder::Libx265,
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
        assert!(args[1].contains("keyint=50:min-keyint=50:bframes=3"));
        assert!(args[1].contains("scenecut=0:open-gop=0"));
        assert!(args[1].contains("master-display="));
        assert!(args[1].contains(":max-cll="));

        let mut cmd = Command::new("ffmpeg");
        apply_encoder_signal_params(
            &mut cmd,
            &crate::preset::VideoCodecConfig::H264 {
                profile: crate::preset::H264Profile::High,
                rate_control: VideoRateControl::constant_quality(20),
            },
            crate::hardware_encoding::ResolvedVideoEncoder::Libx264,
            &settings,
            &delivery,
        )
        .expect("valid encoder signal params");
        let args = cmd.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "-x264-params");
        assert!(args[1].contains("keyint=50:min-keyint=50:bframes=3"));
        assert!(args[1].contains("scenecut=0:open-gop=0"));
        assert!(args[1].contains("colorprim=bt2020"));
        assert!(args[1].contains("transfer=smpte2084"));
        assert!(args[1].contains("colormatrix=bt2020nc"));
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
        assert_eq!(
            expected.sample_aspect_ratio,
            Some(mondrian_core::SampleAspectRatio::SQUARE)
        );
        assert_eq!(expected.field_order.as_deref(), Some("progressive"));
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
                                working_color_space: test_working_color_space(),
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

        assert_eq!(output[0], output[1]);
        assert_eq!(output[1], output[2]);
        assert!(output[0] > 0);
        assert_eq!(output[3], 255);
        assert!(output[5] > output[4]);
        assert!(output[5] > output[6]);
        assert_eq!(output[7], 255);
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
mod audio_source_owner_tests {
    use super::*;

    #[test]
    fn unused_owner_emits_no_lifecycle_and_needs_no_receipt() {
        let mut events = Vec::new();
        let closure = {
            let mut report = |event| events.push(event);
            ExportAudioSourceOwner::new(
                AudioSourceCacheConfig::new(16, 64 * 1024 * 1024, 2),
                &mut report,
            )
            .shutdown_until(Instant::now() + Duration::from_secs(1))
        };

        assert!(closure.is_none());
        assert!(events.is_empty());
    }

    #[test]
    fn job_owner_closes_nested_audio_cache_with_exact_lifecycle_events() {
        let mut events = Vec::new();
        let closure = {
            let mut report = |event| events.push(event);
            let mut owner = ExportAudioSourceOwner::new(
                AudioSourceCacheConfig::new(16, 64 * 1024 * 1024, 2),
                &mut report,
            );
            let cache = owner.cache(48_000).expect("create job audio cache");
            drop(cache);
            owner
                .shutdown_until(Instant::now() + Duration::from_secs(1))
                .expect("used owner closure")
        };

        assert_eq!(closure.cache.schema_version, 5);
        assert!(closure.all_resources_released(), "{closure:?}");
        assert_eq!(
            events,
            vec![
                ExportExecutionOwnerEvent::AudioSourceStarted,
                ExportExecutionOwnerEvent::AudioSourceClosed { all_resources_released: true },
            ]
        );
    }

    #[test]
    fn retained_cache_reference_is_reported_dirty_and_cannot_be_hidden_by_outcome() {
        let mut events = Vec::new();
        let retained_cache;
        let closure = {
            let mut report = |event| events.push(event);
            let mut owner = ExportAudioSourceOwner::new(
                AudioSourceCacheConfig::new(16, 64 * 1024 * 1024, 2),
                &mut report,
            );
            retained_cache = owner.cache(48_000).expect("create retained job audio cache");
            owner
                .shutdown_until(Instant::now() + Duration::from_secs(1))
                .expect("used owner closure")
        };

        assert_eq!(closure.external_cache_references, 1);
        assert!(!closure.all_resources_released());
        assert_eq!(
            events,
            vec![
                ExportExecutionOwnerEvent::AudioSourceStarted,
                ExportExecutionOwnerEvent::AudioSourceClosed { all_resources_released: false },
            ]
        );
        assert!(matches!(
            finish_export_with_audio_closure(
                JobExecutionResult::ReversibleWorkCompleted,
                Some(closure),
            ),
            JobExecutionResult::Failed(_)
        ));
        drop(retained_cache);
    }
}

#[cfg(test)]
#[path = "../queue_perf_tests.rs"]
mod perf_tests;
