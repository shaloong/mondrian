//! Stateful GPU resources for one Viewer preview execution context.
//!
//! Windowing is an Adapter concern. The resources below instead belong to the
//! Viewer GPU execution lifetime and must be shared by every production or
//! headless Adapter that executes the same preview path.

use std::sync::Arc;
use std::time::Instant;

use crate::{
    native_source_texture_format_from_decoded, native_video_sampling_from_decoded, CpuColorFrame,
    GpuColorFrameHandle, GpuColorFrameTextureFormat, GpuColorFrameWgpuResourcePool,
    GpuCompositeLayer, GpuCompositeLayerSource, GpuCompositeRequest, GpuCompositingDiagnostics,
    GpuDisplayCalibrationRuntime, GpuFrameCompositor, GpuNativeDecodedFrameImportSupport,
    GpuNativeDecodedFrameTextureFormat, GpuNativeDecodedFrameVideoSampling, GpuProgramScopesError,
    GpuProgramScopesRecord, GpuProgramScopesRequest, GpuProgramScopesRuntime,
    GpuProgramScopesRuntimeDiagnostics, GpuViewerSpatialRecord, GpuViewerSpatialRuntime,
    GpuViewerSpatialRuntimeDiagnostics, NativeVideoImportCpuTimings, RenderColorStageDiagnostics,
    RenderColorTransformGpuOptions, RenderGpuColorTransformRuntimeRecordError,
    RenderGpuCompositeGraphRecordError, RenderGpuInputStageRecord,
    RenderGpuInputStageRuntimeRecordError, RenderGpuOutputBoundaryRuntime,
    RenderGpuOutputBoundaryRuntimeOwnedBackendContext, RenderGpuOutputBoundaryRuntimeRecordError,
    RenderMonitorAdaptation, RenderOutputColorBoundary, ViewerGpuExecutionLayer,
    ViewerGpuMediaSource, ViewerGpuNativeSource, ViewerNativeVideoImportRuntime, ViewerSourceRect,
};
use mondrian_core::display_calibration::DisplayCalibrationLut3d;
use mondrian_core::types::{BlendMode, Color, SequenceId};
use mondrian_core::WorkingColorSpace;
use mondrian_effects::EffectColorDomain;
use mondrian_media::{DecodedFrameResidency, DecodedGpuFrameHandleKind};

/// Immutable renderer input for one Viewer GPU execution.
pub struct ViewerGpuExecutionRequest<'a> {
    /// Sequence identity used only to correlate renderer diagnostics.
    pub sequence_id: SequenceId,
    /// Timeline frame used only to correlate renderer diagnostics.
    pub timeline_frame: i64,
    /// Working-frame width before Viewer crop and resize.
    pub width: u32,
    /// Working-frame height before Viewer crop and resize.
    pub height: u32,
    /// Timeline working color space represented by all input layers.
    pub working_color_space: WorkingColorSpace,
    /// Bottom-to-top layer stack entering working-linear compositing.
    pub layers: &'a [ViewerGpuExecutionLayer],
    /// Exact Program Output transform shared with delivery/export.
    pub program_output_boundary: &'a RenderOutputColorBoundary,
    /// Preview-only Program Output to local-monitor adaptation.
    pub monitor_adaptation: &'a RenderMonitorAdaptation,
    /// Normalized crop in the working composite.
    pub source_rect: ViewerSourceRect,
    /// Output width after Viewer spatial processing.
    pub output_width: u32,
    /// Output height after Viewer spatial processing.
    pub output_height: u32,
    /// Encoded output precision required by the presentation Adapter.
    pub output_precision: ViewerGpuOutputPrecision,
    /// Optional proven display calibration applied after the output boundary.
    pub display_calibration: Option<Arc<DisplayCalibrationLut3d>>,
    /// Optional demand-driven analysis of Program Output before monitor adaptation.
    pub program_scopes: Option<GpuProgramScopesRequest>,
}

/// Precision contract between a presentation Adapter and Viewer GPU execution.
///
/// The renderer cannot infer this from the OCIO output alone: SDR swapchains
/// normally use an 8-bit carrier, while HDR surfaces, high-bit validation, and
/// display calibration require a float carrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerGpuOutputPrecision {
    /// Normalized 8-bit encoded display code values.
    Encoded8,
    /// Half-float encoded display code values without an 8-bit quantization boundary.
    EncodedFloat16,
}

impl ViewerGpuOutputPrecision {
    /// Return the minimum safe carrier for a display target and calibration path.
    pub fn minimum_for_display(
        output_color_space: mondrian_core::types::ColorSpace,
        requires_display_calibration: bool,
    ) -> Self {
        if requires_display_calibration || output_color_space.is_hdr() {
            Self::EncodedFloat16
        } else {
            Self::Encoded8
        }
    }

    const fn texture_format(self) -> GpuColorFrameTextureFormat {
        match self {
            Self::Encoded8 => GpuColorFrameTextureFormat::Rgba8Unorm,
            Self::EncodedFloat16 => GpuColorFrameTextureFormat::Rgba16Float,
        }
    }
}

/// Ordered GPU command boundary exposed to an optional profiling Adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerGpuExecutionGpuStage {
    /// Working composite commands are complete.
    WorkingComposite,
    /// Viewer spatial commands are complete.
    Spatial,
    /// Program Output boundary commands are complete.
    ProgramOutputBoundary,
    /// Optional Program Output scopes commands are complete.
    ProgramScopes,
    /// Preview-only monitor-adaptation commands are complete.
    MonitorAdaptation,
}

/// Adapter hook for writing GPU markers without coupling execution to a profiler.
pub trait ViewerGpuExecutionStageMarker {
    /// Write one ordered marker into the active Viewer command encoder.
    fn mark(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        stage: ViewerGpuExecutionGpuStage,
    ) -> Result<(), String>;
}

/// Long-lived GPU state for a single Viewer preview execution context.
///
/// Frame resources are cleared between candidates; pipelines and backend
/// capabilities remain resident for the lifetime of this object. Fields are
/// temporarily visible to the sibling Window Adapter while execution is moved
/// behind this module's stable interface.
pub struct ViewerGpuExecutionRuntime {
    native_video_import: ViewerNativeVideoImportRuntime,
    color_output: RenderGpuOutputBoundaryRuntime,
    spatial: GpuViewerSpatialRuntime,
    display_calibration: GpuDisplayCalibrationRuntime,
    program_scopes: GpuProgramScopesRuntime,
    working_compositor: GpuFrameCompositor,
}

impl ViewerGpuExecutionRuntime {
    /// Create one execution context for a renderer device.
    pub fn new(adapter: &wgpu::Adapter, device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let resource_pool = Arc::new(GpuColorFrameWgpuResourcePool::default());
        Self {
            native_video_import: ViewerNativeVideoImportRuntime::new_with_resource_pool(
                adapter,
                device,
                queue,
                Arc::clone(&resource_pool),
            ),
            color_output: RenderGpuOutputBoundaryRuntime::with_resource_pool(Arc::clone(
                &resource_pool,
            )),
            spatial: GpuViewerSpatialRuntime::with_resource_pool(Arc::clone(&resource_pool)),
            display_calibration: GpuDisplayCalibrationRuntime::with_resource_pool(resource_pool),
            program_scopes: GpuProgramScopesRuntime::default(),
            working_compositor: GpuFrameCompositor::new(device),
        }
    }

    /// Native decoder import capability exposed to preview scheduling.
    pub fn native_import_support(&self) -> GpuNativeDecodedFrameImportSupport {
        self.native_video_import.support()
    }

    /// Current bounded native-import contract-pool and bridge-entry residency.
    pub fn native_import_pool_residency(&self) -> (usize, usize) {
        self.native_video_import.pool_residency()
    }

    /// Aggregate output-stage diagnostics without exposing the resource table.
    pub fn color_output_diagnostics(&self) -> crate::RenderGpuOutputBoundaryRuntimeDiagnostics {
        self.color_output.diagnostics()
    }

    /// Point-in-time evidence for persistent compositor uniform reuse.
    pub fn compositor_uniform_arena_diagnostics(
        &self,
    ) -> crate::GpuCompositorUniformArenaDiagnostics {
        self.working_compositor.uniform_arena_diagnostics()
    }

    /// Point-in-time evidence for compositor texture-binding reuse.
    pub fn compositor_texture_binding_diagnostics(
        &self,
    ) -> crate::GpuCompositorTextureBindingDiagnostics {
        self.working_compositor.texture_binding_diagnostics()
    }

    /// Point-in-time evidence that hidden scopes perform no work and visible
    /// scopes reuse retained pipelines and display resources.
    pub fn program_scopes_diagnostics(&self) -> GpuProgramScopesRuntimeDiagnostics {
        self.program_scopes.diagnostics()
    }

    /// Release resources scoped to the current candidate, retaining pipelines.
    pub fn clear_frame_resources(&mut self) {
        self.working_compositor.clear_frame_resources();
        self.color_output.clear_frame_resources();
        self.spatial.clear_frame_resources();
        self.display_calibration.clear_frame_resources();
    }

    /// Record one current Viewer frame through the shared GPU execution path.
    ///
    /// The returned handle remains owned by this runtime until the next frame
    /// clear/reset. Presentation registration and publication are Adapter work.
    pub fn record(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        request: ViewerGpuExecutionRequest<'_>,
    ) -> Result<ViewerGpuExecutionRecord, ViewerGpuExecutionError> {
        self.record_with_stage_marker(device, queue, encoder, request, None)
    }

    /// Record one Viewer frame with optional ordered hardware profiling markers.
    pub fn record_with_stage_marker(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        request: ViewerGpuExecutionRequest<'_>,
        mut stage_marker: Option<&mut dyn ViewerGpuExecutionStageMarker>,
    ) -> Result<ViewerGpuExecutionRecord, ViewerGpuExecutionError> {
        validate_output_precision(
            request.output_precision,
            request.display_calibration.is_some(),
        )?;
        validate_program_monitor_contract(
            request.program_output_boundary,
            request.monitor_adaptation,
        )?;
        validate_program_scopes_contract(request.program_output_boundary, request.program_scopes)?;
        self.native_video_import.reset_frame_cpu_timings();
        let input_prepare_started = Instant::now();
        let prepared = prepare_composite(
            &request,
            &mut self.color_output,
            &mut self.native_video_import,
            &self.working_compositor,
            device,
            queue,
            encoder,
        )?;
        let input_prepare_us = elapsed_us(input_prepare_started);
        let residency = prepared.residency;
        let fallback_reasons = prepared.fallback_reasons;
        let mut stage_diagnostics = prepared.input_stage_diagnostics;
        let gpu_layers = composite_layers(&prepared.layers, &prepared.gpu_input_handles);
        let working_composite_started = Instant::now();
        let composite = self
            .color_output
            .record_wgpu_composite_graph(
                &self.working_compositor,
                device,
                queue,
                encoder,
                GpuCompositeRequest {
                    width: request.width,
                    height: request.height,
                    working_color_space: request.working_color_space,
                    layers: &gpu_layers,
                },
                request.program_output_boundary.engine.clone(),
                RenderColorTransformGpuOptions::default(),
            )
            .map_err(ViewerGpuExecutionError::WorkingComposite)?;
        stage_diagnostics.accumulate(composite.color_stage_diagnostics);
        let working_composite_us = elapsed_us(working_composite_started);
        mark_gpu_stage(
            &mut stage_marker,
            encoder,
            ViewerGpuExecutionGpuStage::WorkingComposite,
        )?;
        let working_view = self
            .color_output
            .frame_table()
            .get(&composite.output)
            .map_err(|error| ViewerGpuExecutionError::WorkingOutputMissing(format!("{error:?}")))?
            .resource()
            .texture_view
            .clone();
        let spatial_started = Instant::now();
        let spatial_record = self
            .spatial
            .record_for_presentation(
                device,
                encoder,
                self.color_output.frame_ids_mut(),
                composite.output,
                &working_view,
                request.source_rect,
                request.output_width,
                request.output_height,
            )
            .map_err(|error| ViewerGpuExecutionError::Spatial(error.to_string()))?;
        let spatial_diagnostics = self.spatial.diagnostics();
        let spatial_output = spatial_record.output().clone();
        if matches!(spatial_record, GpuViewerSpatialRecord::Materialized(_)) {
            let spatial_resource = self
                .spatial
                .take_output(&spatial_output)
                .ok_or(ViewerGpuExecutionError::SpatialOutputMissing)?;
            self.color_output
                .frame_table_mut()
                .insert(spatial_resource)
                .map_err(|error| ViewerGpuExecutionError::SpatialTransfer(format!("{error:?}")))?;
        }
        let spatial_us = elapsed_us(spatial_started);
        mark_gpu_stage(
            &mut stage_marker,
            encoder,
            ViewerGpuExecutionGpuStage::Spatial,
        )?;
        let program_output_boundary_started = Instant::now();
        let program_output_texture_format = if request.monitor_adaptation.requires_pass() {
            GpuColorFrameTextureFormat::Rgba16Float
        } else {
            request.output_precision.texture_format()
        };
        let program_output_record = self
            .color_output
            .record_wgpu_output_boundary_gpu_frame_owned_backend(
                request.program_output_boundary,
                &spatial_output,
                program_output_texture_format,
                RenderColorTransformGpuOptions::default(),
                RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                    device,
                    queue,
                    encoder,
                    load_op: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                },
            )
            .map_err(ViewerGpuExecutionError::ProgramOutputBoundary)?;
        let program_output_boundary_us = elapsed_us(program_output_boundary_started);
        mark_gpu_stage(
            &mut stage_marker,
            encoder,
            ViewerGpuExecutionGpuStage::ProgramOutputBoundary,
        )?;
        stage_diagnostics.accumulate(program_output_record.stage_diagnostics);
        let program_output = program_output_record.materialized.output;
        let program_scopes_started = Instant::now();
        let program_scopes = if let Some(scopes_request) = request.program_scopes {
            let program_output_view = self
                .color_output
                .frame_table()
                .get(&program_output)
                .map_err(|error| {
                    ViewerGpuExecutionError::ProgramOutputMissing(format!("{error:?}"))
                })?
                .resource()
                .texture_view
                .clone();
            Some(
                self.program_scopes
                    .record(
                        device,
                        queue,
                        encoder,
                        &program_output_view,
                        request.output_width,
                        request.output_height,
                        scopes_request,
                    )
                    .map_err(ViewerGpuExecutionError::ProgramScopes)?,
            )
        } else {
            None
        };
        let program_scopes_us = elapsed_us(program_scopes_started);
        mark_gpu_stage(
            &mut stage_marker,
            encoder,
            ViewerGpuExecutionGpuStage::ProgramScopes,
        )?;
        let monitor_adaptation_started = Instant::now();
        let output = if let Some(transform) = request.monitor_adaptation.gpu_transform() {
            let monitor_record = self
                .color_output
                .record_wgpu_intermediate_color_transform_owned_backend(
                    &transform,
                    &program_output,
                    GpuColorFrameTextureFormat::Rgba16Float,
                    "viewer-monitor-adaptation",
                    RenderColorTransformGpuOptions::default(),
                    RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                        device,
                        queue,
                        encoder,
                        load_op: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    },
                )
                .map_err(ViewerGpuExecutionError::MonitorAdaptation)?;
            stage_diagnostics.accumulate(monitor_record.stage_diagnostics);
            monitor_record.materialized.output
        } else {
            program_output.clone()
        };
        let monitor_adaptation_us = elapsed_us(monitor_adaptation_started);
        mark_gpu_stage(
            &mut stage_marker,
            encoder,
            ViewerGpuExecutionGpuStage::MonitorAdaptation,
        )?;
        let calibration_started = Instant::now();
        let (output, output_owner) = if let Some(calibration) = request.display_calibration {
            let output_view = self
                .color_output
                .frame_table()
                .get(&output)
                .map_err(|error| {
                    ViewerGpuExecutionError::DisplayOutputMissing(format!("{error:?}"))
                })?
                .resource()
                .texture_view
                .clone();
            let calibrated = self
                .display_calibration
                .record(
                    device,
                    queue,
                    encoder,
                    output,
                    &output_view,
                    calibration,
                    GpuColorFrameTextureFormat::Rgba16Float,
                )
                .map_err(|error| ViewerGpuExecutionError::Calibration(error.to_string()))?;
            (
                calibrated,
                ViewerGpuExecutionOutputOwner::DisplayCalibration,
            )
        } else {
            (output, ViewerGpuExecutionOutputOwner::ColorOutput)
        };
        let display_calibration_us = elapsed_us(calibration_started);
        Ok(ViewerGpuExecutionRecord {
            program_output,
            program_scopes,
            output,
            output_owner,
            stage_diagnostics,
            compositing_diagnostics: composite.compositing_diagnostics,
            spatial_diagnostics,
            residency,
            fallback_reasons,
            cpu_stage_timings: ViewerGpuExecutionCpuStageTimings {
                input_prepare_us,
                native_video_import: self.native_video_import.frame_cpu_timings(),
                working_composite_us,
                spatial_us,
                program_output_boundary_us,
                program_scopes_us,
                monitor_adaptation_us,
                display_calibration_us,
            },
        })
    }

    /// Resolve the recorded presentation texture without exposing resource tables.
    pub fn output_texture_view(
        &self,
        record: &ViewerGpuExecutionRecord,
    ) -> Result<wgpu::TextureView, ViewerGpuExecutionError> {
        match record.output_owner {
            ViewerGpuExecutionOutputOwner::ColorOutput => self
                .color_output
                .frame_table()
                .get(&record.output)
                .map(|resource| resource.resource().texture_view.clone())
                .map_err(|error| {
                    ViewerGpuExecutionError::DisplayOutputMissing(format!("{error:?}"))
                }),
            ViewerGpuExecutionOutputOwner::DisplayCalibration => self
                .display_calibration
                .output(&record.output)
                .map(|resource| resource.resource().texture_view.clone())
                .ok_or_else(|| {
                    ViewerGpuExecutionError::DisplayOutputMissing(
                        "calibrated output disappeared before presentation".to_owned(),
                    )
                }),
        }
    }

    /// Resolve the Program Output texture before local monitor adaptation.
    ///
    /// Scopes and program-output diagnostics must consume this view so local
    /// display policy cannot change measured program values.
    pub fn program_output_texture_view(
        &self,
        record: &ViewerGpuExecutionRecord,
    ) -> Result<wgpu::TextureView, ViewerGpuExecutionError> {
        self.color_output
            .frame_table()
            .get(&record.program_output)
            .map(|resource| resource.resource().texture_view.clone())
            .map_err(|error| ViewerGpuExecutionError::ProgramOutputMissing(format!("{error:?}")))
    }

    /// Reset all retained execution resources after a device/surface transition.
    pub fn reset(&mut self) {
        self.working_compositor.clear_frame_resources();
        self.spatial.clear();
        self.display_calibration.clear();
        self.program_scopes.clear();
        self.color_output.clear_frame_resources();
        self.color_output.resource_pool().clear();
    }
}

fn validate_output_precision(
    output_precision: ViewerGpuOutputPrecision,
    has_display_calibration: bool,
) -> Result<(), ViewerGpuExecutionError> {
    if has_display_calibration && output_precision != ViewerGpuOutputPrecision::EncodedFloat16 {
        return Err(ViewerGpuExecutionError::DisplayCalibrationRequiresFloatOutput);
    }
    Ok(())
}

fn validate_program_monitor_contract(
    boundary: &RenderOutputColorBoundary,
    adaptation: &RenderMonitorAdaptation,
) -> Result<(), ViewerGpuExecutionError> {
    if boundary.output_color_space != adaptation.program_output_color_space() {
        return Err(ViewerGpuExecutionError::ProgramMonitorBoundaryMismatch {
            program_boundary: boundary.output_color_space,
            adaptation_input: adaptation.program_output_color_space(),
        });
    }
    Ok(())
}

fn validate_program_scopes_contract(
    boundary: &RenderOutputColorBoundary,
    request: Option<GpuProgramScopesRequest>,
) -> Result<(), ViewerGpuExecutionError> {
    if let Some(request) = request {
        if boundary.output_color_space != request.signal_color_space() {
            return Err(ViewerGpuExecutionError::ProgramScopesBoundaryMismatch {
                program_boundary: boundary.output_color_space,
                scopes_signal: request.signal_color_space(),
            });
        }
    }
    Ok(())
}

fn mark_gpu_stage(
    marker: &mut Option<&mut dyn ViewerGpuExecutionStageMarker>,
    encoder: &mut wgpu::CommandEncoder,
    stage: ViewerGpuExecutionGpuStage,
) -> Result<(), ViewerGpuExecutionError> {
    if let Some(marker) = marker.as_deref_mut() {
        marker.mark(encoder, stage).map_err(ViewerGpuExecutionError::StageMarker)?;
    }
    Ok(())
}

/// Successful GPU recording evidence consumed by presentation Adapters.
pub struct ViewerGpuExecutionRecord {
    /// Program Output retained before preview-only monitor adaptation.
    pub program_output: GpuColorFrameHandle,
    /// GPU-only scope display products when the active UI requested analysis.
    pub program_scopes: Option<GpuProgramScopesRecord>,
    /// Renderer-owned output handle retained until frame resources clear.
    pub output: GpuColorFrameHandle,
    output_owner: ViewerGpuExecutionOutputOwner,
    /// Accumulated input and output color-stage diagnostics.
    pub stage_diagnostics: RenderColorStageDiagnostics,
    /// Working compositor execution diagnostics.
    pub compositing_diagnostics: GpuCompositingDiagnostics,
    /// Viewer crop/resize execution diagnostics.
    pub spatial_diagnostics: GpuViewerSpatialRuntimeDiagnostics,
    /// Exact media residency used for this execution.
    pub residency: ViewerGpuExecutionResidency,
    /// Explicit reasons for native/GPU-input correctness fallbacks.
    pub fallback_reasons: Vec<String>,
    /// CPU wall time spent recording each renderer stage before queue submission.
    pub cpu_stage_timings: ViewerGpuExecutionCpuStageTimings,
}

/// CPU command-recording attribution for one successful Viewer frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct ViewerGpuExecutionCpuStageTimings {
    /// Input source import and input-color transform command preparation.
    pub input_prepare_us: u64,
    /// Native-video detail nested within `input_prepare_us` when hardware import ran.
    pub native_video_import: NativeVideoImportCpuTimings,
    /// Working-linear layer composite command preparation.
    pub working_composite_us: u64,
    /// Viewer crop/resize command preparation.
    pub spatial_us: u64,
    /// Program Output color-boundary command preparation.
    pub program_output_boundary_us: u64,
    /// Optional Program Output scopes command preparation.
    pub program_scopes_us: u64,
    /// Preview-only monitor-adaptation command preparation.
    pub monitor_adaptation_us: u64,
    /// Optional display-calibration command preparation.
    pub display_calibration_us: u64,
}

fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewerGpuExecutionOutputOwner {
    ColorOutput,
    DisplayCalibration,
}

/// Stage-specific failures from the shared Viewer GPU execution Interface.
#[derive(Debug, thiserror::Error)]
pub enum ViewerGpuExecutionError {
    /// Bounded native/GPU resources are still in flight. The presentation
    /// adapter should retain its current output and retry or discard this candidate.
    #[error("Viewer GPU execution is backpressured: {0}")]
    Backpressure(String),
    #[error("Viewer GPU input preparation failed: {0}")]
    InputPreparation(String),
    /// Display calibration was paired with a quantized output carrier.
    #[error("Viewer display calibration requires an encoded float16 output carrier")]
    DisplayCalibrationRequiresFloatOutput,
    /// Program Output and monitor adaptation were wired from different roots.
    #[error(
        "Viewer Program Output boundary {program_boundary:?} does not match monitor adaptation input {adaptation_input:?}"
    )]
    ProgramMonitorBoundaryMismatch {
        /// Program Output boundary destination.
        program_boundary: mondrian_core::types::ColorSpace,
        /// Identity expected by monitor adaptation.
        adaptation_input: mondrian_core::types::ColorSpace,
    },
    /// Scope signal identity did not match the measured Program Output.
    #[error(
        "Viewer Program Output boundary {program_boundary:?} does not match scope signal {scopes_signal:?}"
    )]
    ProgramScopesBoundaryMismatch {
        program_boundary: mondrian_core::types::ColorSpace,
        scopes_signal: mondrian_core::types::ColorSpace,
    },
    #[error("Viewer GPU working composite graph failed: {0:?}")]
    WorkingComposite(RenderGpuCompositeGraphRecordError),
    #[error("Viewer GPU effect-domain processing failed: {0}")]
    EffectDomain(String),
    #[error("Viewer GPU working output is missing: {0}")]
    WorkingOutputMissing(String),
    #[error("Viewer GPU spatial processing failed: {0}")]
    Spatial(String),
    #[error("Viewer GPU spatial output disappeared before the display boundary")]
    SpatialOutputMissing,
    #[error("Viewer GPU spatial resource transfer failed: {0}")]
    SpatialTransfer(String),
    #[error("Viewer GPU Program Output boundary failed: {0:?}")]
    ProgramOutputBoundary(RenderGpuOutputBoundaryRuntimeRecordError),
    #[error("Viewer GPU Program Output scopes failed: {0}")]
    ProgramScopes(GpuProgramScopesError),
    #[error("Viewer GPU monitor adaptation failed: {0:?}")]
    MonitorAdaptation(RenderGpuColorTransformRuntimeRecordError),
    #[error("Viewer GPU Program Output is missing: {0}")]
    ProgramOutputMissing(String),
    #[error("Viewer GPU display output is missing: {0}")]
    DisplayOutputMissing(String),
    #[error("Viewer GPU display calibration failed: {0}")]
    Calibration(String),
    #[error("Viewer GPU profiling stage marker failed: {0}")]
    StageMarker(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewerGpuExecutionResidency {
    /// Media layer count.
    pub media_layers: u32,
    /// Procedural layer count.
    pub procedural_layers: u32,
    /// Media layers backed by native decoder GPU surfaces.
    pub native_decoder_gpu_layers: u32,
    /// Media layers transformed through the GPU input path.
    pub gpu_input_layers: u32,
    /// Media layers uploaded from CPU working frames.
    pub cpu_upload_layers: u32,
    /// Native/GPU-input attempts that fell back.
    pub gpu_input_failures: u32,
    /// Sampling and residency facts for native-video admission evidence.
    pub native_video_import: Option<ViewerGpuNativeVideoFacts>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewerGpuNativeVideoFacts {
    /// Actual decoder output residency.
    pub decoder_residency: DecodedFrameResidency,
    /// Native handle family if retained by media.
    pub decoder_handle_kind: Option<DecodedGpuFrameHandleKind>,
    /// Renderer import texture format derived from media facts.
    pub source_texture_format: Option<GpuNativeDecodedFrameTextureFormat>,
    /// Fail-closed renderer sampling contract derived from media facts.
    pub source_video_sampling: Option<GpuNativeDecodedFrameVideoSampling>,
}

impl Default for ViewerGpuNativeVideoFacts {
    fn default() -> Self {
        Self {
            decoder_residency: DecodedFrameResidency::CpuRgba,
            decoder_handle_kind: None,
            source_texture_format: None,
            source_video_sampling: None,
        }
    }
}

struct PreparedComposite<'a> {
    gpu_input_handles: Vec<GpuColorFrameHandle>,
    layers: Vec<PreparedCompositeLayer<'a>>,
    residency: ViewerGpuExecutionResidency,
    input_stage_diagnostics: RenderColorStageDiagnostics,
    fallback_reasons: Vec<String>,
}

struct PreparedCompositeLayer<'a> {
    source: PreparedCompositeLayerSource<'a>,
    opacity: f32,
    blend_mode: BlendMode,
    transform: [f32; 6],
    effect_plan: Option<&'a mondrian_effects::CompiledEffectGpuPlan>,
    frame_seed: i64,
}

enum PreparedCompositeLayerSource<'a> {
    CpuFrame(&'a CpuColorFrame),
    GpuFrame(usize),
    SolidColor(Color),
    Adjustment,
}

fn prepare_composite<'a>(
    request: &ViewerGpuExecutionRequest<'a>,
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    native_runtime: &mut ViewerNativeVideoImportRuntime,
    compositor: &GpuFrameCompositor,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
) -> Result<PreparedComposite<'a>, ViewerGpuExecutionError> {
    let mut prepared = PreparedComposite {
        gpu_input_handles: Vec::new(),
        layers: Vec::with_capacity(request.layers.len()),
        residency: ViewerGpuExecutionResidency::default(),
        input_stage_diagnostics: RenderColorStageDiagnostics::default(),
        fallback_reasons: Vec::new(),
    };

    for layer in request.layers {
        if viewer_layer_has_zero_contribution(layer) {
            continue;
        }
        match layer {
            ViewerGpuExecutionLayer::Media {
                frame,
                gpu_source,
                native_source,
                opacity,
                transform,
                effect_plan,
                frame_seed,
            } => {
                prepared.residency.media_layers = prepared.residency.media_layers.saturating_add(1);
                prepared.residency.record_source(gpu_source.as_ref(), native_source.as_ref());
                let mut native_import_error = None;
                let native_handle = match native_source.as_ref() {
                    Some(source) => {
                        match record_native_video_layer(source, native_runtime, runtime) {
                            Ok(handle) => Some(handle),
                            Err(error) if error.is_backpressure() => {
                                return Err(ViewerGpuExecutionError::Backpressure(
                                    error.to_string(),
                                ));
                            }
                            Err(error) => {
                                prepared.residency.gpu_input_failures =
                                    prepared.residency.gpu_input_failures.saturating_add(1);
                                prepared
                                    .fallback_reasons
                                    .push(format!("viewer native video import failed: {error}"));
                                tracing::warn!(
                                    sequence_id = %request.sequence_id,
                                    frame = request.timeline_frame,
                                    width = request.width,
                                    height = request.height,
                                    "viewer native video import failed: {error}"
                                );
                                native_import_error = Some(error.to_string());
                                None
                            }
                        }
                    }
                    None => None,
                };
                let source = if let Some(handle) = native_handle {
                    let index = prepared.gpu_input_handles.len();
                    prepared.gpu_input_handles.push(handle);
                    PreparedCompositeLayerSource::GpuFrame(index)
                } else {
                    match gpu_source.as_ref() {
                        Some(source) => {
                            match record_gpu_input_layer(source, runtime, device, queue, encoder) {
                                Ok(record) => {
                                    prepared
                                        .input_stage_diagnostics
                                        .accumulate(record.stage_diagnostics);
                                    let index = prepared.gpu_input_handles.len();
                                    prepared.gpu_input_handles.push(record.materialized.output);
                                    prepared.residency.gpu_input_layers =
                                        prepared.residency.gpu_input_layers.saturating_add(1);
                                    PreparedCompositeLayerSource::GpuFrame(index)
                                }
                                Err(error) => {
                                    prepared.residency.gpu_input_failures =
                                        prepared.residency.gpu_input_failures.saturating_add(1);
                                    prepared.fallback_reasons.push(format!(
                                        "viewer GPU input transform failed: {error:?}"
                                    ));
                                    if let Some(frame) = frame.as_ref() {
                                        prepared.residency.cpu_upload_layers =
                                            prepared.residency.cpu_upload_layers.saturating_add(1);
                                        tracing::warn!(
                                            sequence_id = %request.sequence_id,
                                            frame = request.timeline_frame,
                                            width = request.width,
                                            height = request.height,
                                            "viewer GPU input transform failed; using CPU working layer upload: {error:?}"
                                        );
                                        PreparedCompositeLayerSource::CpuFrame(frame)
                                    } else {
                                        return Err(ViewerGpuExecutionError::InputPreparation(
                                        format!(
                                            "GPU input transform failed without a CPU working fallback: {error:?}"
                                        ),
                                    ));
                                    }
                                }
                            }
                        }
                        None => {
                            let Some(frame) = frame.as_ref() else {
                                let reason = native_source.as_ref().map_or_else(
                                    || {
                                        "media layer has no GPU source or CPU working fallback"
                                            .to_owned()
                                    },
                                    |source| {
                                        native_import_failure_without_cpu_fallback(
                                            source.native_frame.handle_kind(),
                                            source.native_frame.surface_format,
                                            native_import_error.as_deref(),
                                        )
                                    },
                                );
                                return Err(ViewerGpuExecutionError::InputPreparation(reason));
                            };
                            prepared.residency.cpu_upload_layers =
                                prepared.residency.cpu_upload_layers.saturating_add(1);
                            PreparedCompositeLayerSource::CpuFrame(frame)
                        }
                    }
                };
                let (source, effect_plan) = if effect_plan.processing_domain()
                    == EffectColorDomain::SceneLinearRgb
                {
                    (source, Some(effect_plan.as_ref()))
                } else {
                    let input = match source {
                        PreparedCompositeLayerSource::GpuFrame(index) => {
                            prepared.gpu_input_handles[index].clone()
                        }
                        PreparedCompositeLayerSource::CpuFrame(frame) => {
                            let upload = runtime
                                .upload_wgpu_working_frame(device, queue, frame)
                                .map_err(|error| {
                                    ViewerGpuExecutionError::EffectDomain(format!(
                                        "CPU working source upload failed: {error:?}"
                                    ))
                                })?;
                            prepared.input_stage_diagnostics.accumulate(upload.stage_diagnostics);
                            upload.output
                        }
                        PreparedCompositeLayerSource::SolidColor(_)
                        | PreparedCompositeLayerSource::Adjustment => {
                            return Err(ViewerGpuExecutionError::EffectDomain(
                                "media effect received a non-media prepared source".to_owned(),
                            ));
                        }
                    };
                    let index = record_external_domain_effect(
                        &mut prepared,
                        runtime,
                        compositor,
                        effect_plan,
                        input,
                        request.program_output_boundary.engine.clone(),
                        *frame_seed,
                        device,
                        queue,
                        encoder,
                    )?;
                    (PreparedCompositeLayerSource::GpuFrame(index), None)
                };
                prepared.layers.push(PreparedCompositeLayer {
                    source,
                    opacity: *opacity,
                    blend_mode: BlendMode::Normal,
                    transform: *transform,
                    effect_plan,
                    frame_seed: *frame_seed,
                });
            }
            ViewerGpuExecutionLayer::SolidColor { layer, effect_plan } => {
                prepared.residency.procedural_layers =
                    prepared.residency.procedural_layers.saturating_add(1);
                let scene_linear =
                    effect_plan.processing_domain() == EffectColorDomain::SceneLinearRgb;
                if scene_linear {
                    prepared.layers.push(PreparedCompositeLayer {
                        source: PreparedCompositeLayerSource::SolidColor(layer.color),
                        opacity: layer.opacity,
                        blend_mode: layer.blend_mode,
                        transform: layer.transform,
                        effect_plan: Some(effect_plan),
                        frame_seed: layer.frame_seed,
                    });
                } else {
                    let materialized = runtime
                        .record_wgpu_solid_source(
                            compositor,
                            device,
                            queue,
                            encoder,
                            request.width,
                            request.height,
                            request.working_color_space,
                            layer.color,
                        )
                        .map_err(|error| {
                            ViewerGpuExecutionError::EffectDomain(format!(
                                "solid source materialization failed: {error:?}"
                            ))
                        })?;
                    let (index, prepared_effect_plan) = if scene_linear {
                        let index = prepared.gpu_input_handles.len();
                        prepared.gpu_input_handles.push(materialized.output);
                        (index, Some(effect_plan.as_ref()))
                    } else {
                        (
                            record_external_domain_effect(
                                &mut prepared,
                                runtime,
                                compositor,
                                effect_plan,
                                materialized.output,
                                request.program_output_boundary.engine.clone(),
                                layer.frame_seed,
                                device,
                                queue,
                                encoder,
                            )?,
                            None,
                        )
                    };
                    prepared.layers.push(PreparedCompositeLayer {
                        source: PreparedCompositeLayerSource::GpuFrame(index),
                        opacity: layer.opacity,
                        blend_mode: layer.blend_mode,
                        transform: layer.transform,
                        effect_plan: prepared_effect_plan,
                        frame_seed: layer.frame_seed,
                    });
                }
            }
            ViewerGpuExecutionLayer::Adjustment {
                effect_plan,
                opacity,
                blend_mode,
                frame_seed,
            } => {
                prepared.layers.push(PreparedCompositeLayer {
                    source: PreparedCompositeLayerSource::Adjustment,
                    opacity: *opacity,
                    blend_mode: *blend_mode,
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_plan: Some(effect_plan),
                    frame_seed: *frame_seed,
                });
            }
        }
    }

    Ok(prepared)
}

fn viewer_layer_has_zero_contribution(layer: &ViewerGpuExecutionLayer) -> bool {
    let opacity = match layer {
        ViewerGpuExecutionLayer::Media { opacity, .. }
        | ViewerGpuExecutionLayer::Adjustment { opacity, .. } => *opacity,
        ViewerGpuExecutionLayer::SolidColor { layer, .. } => layer.opacity,
    };
    opacity.clamp(0.0, 1.0) == 0.0
}

#[allow(clippy::too_many_arguments)]
fn record_external_domain_effect(
    prepared: &mut PreparedComposite<'_>,
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    compositor: &GpuFrameCompositor,
    effect_plan: &mondrian_effects::CompiledEffectGpuPlan,
    input: GpuColorFrameHandle,
    engine: mondrian_core::ColorEngine,
    frame_seed: i64,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
) -> Result<usize, ViewerGpuExecutionError> {
    let record = runtime
        .record_wgpu_effect_domain_round_trip(
            compositor,
            effect_plan,
            &input,
            engine,
            frame_seed,
            RenderColorTransformGpuOptions::default(),
            RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                device,
                queue,
                encoder,
                load_op: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            },
        )
        .map_err(|error| ViewerGpuExecutionError::EffectDomain(format!("{error:?}")))?;
    prepared
        .input_stage_diagnostics
        .accumulate(record.to_processing.stage_diagnostics);
    prepared.input_stage_diagnostics.accumulate(record.to_working.stage_diagnostics);
    let index = prepared.gpu_input_handles.len();
    prepared.gpu_input_handles.push(record.to_working.materialized.output);
    Ok(index)
}

fn native_import_failure_without_cpu_fallback(
    handle_kind: mondrian_media::DecodedGpuFrameHandleKind,
    surface_format: mondrian_media::DecodedVideoSurfaceFormat,
    error: Option<&str>,
) -> String {
    format!(
        "native GPU import failed for {} {surface_format:?} without a CPU working fallback: {}",
        handle_kind.as_str(),
        error.unwrap_or("native import returned no error detail")
    )
}

fn record_native_video_layer(
    source: &ViewerGpuNativeSource,
    native_runtime: &mut ViewerNativeVideoImportRuntime,
    color_runtime: &mut RenderGpuOutputBoundaryRuntime,
) -> Result<GpuColorFrameHandle, crate::GpuNativeDecodedFrameImportError> {
    let resource = native_runtime.import(
        color_runtime.frame_ids_mut(),
        source.source_color_space,
        &source.input_transform,
        &source.native_frame,
    )?;
    let handle = resource.handle().clone();
    if color_runtime
        .frame_table_mut()
        .insert(resource)
        .map_err(
            |error| crate::GpuNativeDecodedFrameImportError::BackendRejected {
                reason: format!("native working resource insertion failed: {error:?}"),
            },
        )?
        .is_some()
    {
        return Err(crate::GpuNativeDecodedFrameImportError::BackendRejected {
            reason: "native working frame unexpectedly replaced a live resource".to_owned(),
        });
    }
    Ok(handle)
}

fn record_gpu_input_layer(
    source: &ViewerGpuMediaSource,
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
) -> Result<RenderGpuInputStageRecord, RenderGpuInputStageRuntimeRecordError> {
    runtime.record_wgpu_input_stage_owned_backend(
        &source.input_transform,
        &source.source,
        RenderColorTransformGpuOptions::default(),
        RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
            device,
            queue,
            encoder,
            load_op: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
        },
    )
}

fn composite_layers<'a>(
    layers: &'a [PreparedCompositeLayer<'a>],
    gpu_input_handles: &'a [GpuColorFrameHandle],
) -> Vec<GpuCompositeLayer<'a>> {
    layers
        .iter()
        .map(|layer| GpuCompositeLayer {
            source: match layer.source {
                PreparedCompositeLayerSource::CpuFrame(frame) => {
                    GpuCompositeLayerSource::CpuFrame(frame)
                }
                PreparedCompositeLayerSource::GpuFrame(index) => {
                    GpuCompositeLayerSource::GpuFrame(&gpu_input_handles[index])
                }
                PreparedCompositeLayerSource::SolidColor(color) => {
                    GpuCompositeLayerSource::SolidColor(color)
                }
                PreparedCompositeLayerSource::Adjustment => GpuCompositeLayerSource::Adjustment,
            },
            opacity: layer.opacity,
            blend_mode: layer.blend_mode,
            transform: layer.transform,
            effect_plan: layer.effect_plan,
            frame_seed: layer.frame_seed,
        })
        .collect()
}

impl ViewerGpuExecutionResidency {
    fn record_source(
        &mut self,
        media_source: Option<&ViewerGpuMediaSource>,
        native_source: Option<&ViewerGpuNativeSource>,
    ) {
        let facts = native_source
            .map(ViewerGpuNativeVideoFacts::from_native_source)
            .or_else(|| media_source.map(ViewerGpuNativeVideoFacts::from_media_source))
            .unwrap_or_default();
        if facts.decoder_residency == DecodedFrameResidency::GpuTexture {
            self.native_decoder_gpu_layers = self.native_decoder_gpu_layers.saturating_add(1);
        }
        let should_replace = self
            .native_video_import
            .map(|current| {
                current.decoder_residency != DecodedFrameResidency::GpuTexture
                    && facts.decoder_residency == DecodedFrameResidency::GpuTexture
            })
            .unwrap_or(true);
        if should_replace {
            self.native_video_import = Some(facts);
        }
    }
}

impl ViewerGpuNativeVideoFacts {
    fn from_media_source(source: &ViewerGpuMediaSource) -> Self {
        let source_texture_format = (source.decoder_residency == DecodedFrameResidency::GpuTexture)
            .then(|| native_source_texture_format_from_decoded(source.decoded_surface_format))
            .flatten();
        let source_video_sampling = source_texture_format.and_then(|format| {
            source.source.descriptor().color_space.color().and_then(|encoded| {
                native_video_sampling_from_decoded(encoded, format, source.decoded_video_sampling)
            })
        });
        Self {
            decoder_residency: source.decoder_residency,
            decoder_handle_kind: source.decoder_handle_kind,
            source_texture_format,
            source_video_sampling,
        }
    }

    fn from_native_source(source: &ViewerGpuNativeSource) -> Self {
        let source_texture_format =
            native_source_texture_format_from_decoded(source.native_frame.surface_format);
        let source_video_sampling = source_texture_format.and_then(|format| {
            native_video_sampling_from_decoded(
                source.source_color_space,
                format,
                source.native_frame.diagnostics.decoded_video_sampling,
            )
        });
        Self {
            decoder_residency: DecodedFrameResidency::GpuTexture,
            decoder_handle_kind: Some(source.native_frame.handle_kind()),
            source_texture_format,
            source_video_sampling,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CpuEncodedColorFrame, CpuSourceColorFrame, GpuContext, RenderInputTransform,
        TimelineSolidColorLayer,
    };
    use mondrian_core::{
        ensure_mondrian_default_ocio_loaded, ColorEngine, ColorSpace, WaveformMode,
        WorkingRgbaF32Frame,
    };
    use mondrian_effects::{
        compile_scheduled_effect_graph_in_domain, get_or_compile_scheduled_render_graph,
        lower_effect_graph_to_gpu_plan, EffectColorDomain, EffectColorDomainContract,
        EffectGraphBuilderState, EffectRenderOp, EffectRenderPlan,
    };
    use mondrian_media::{
        DecodedGpuFrameHandleKind, DecodedVideoSampling, DecodedVideoSurfaceFormat,
    };

    #[test]
    fn terminal_native_import_failure_preserves_backend_error_detail() {
        let message = native_import_failure_without_cpu_fallback(
            DecodedGpuFrameHandleKind::D3D11Texture2D,
            DecodedVideoSurfaceFormat::P010,
            Some("adapter LUID mismatch"),
        );

        assert!(message.contains("D3D11Texture2D P010"));
        assert!(message.contains("adapter LUID mismatch"));
    }

    #[test]
    fn viewer_output_precision_preserves_hdr_and_calibration_carriers() {
        assert_eq!(
            ViewerGpuOutputPrecision::minimum_for_display(ColorSpace::Srgb, false),
            ViewerGpuOutputPrecision::Encoded8
        );
        assert_eq!(
            ViewerGpuOutputPrecision::minimum_for_display(ColorSpace::DisplayP3, false),
            ViewerGpuOutputPrecision::Encoded8
        );
        for output in [ColorSpace::Rec2100Hlg, ColorSpace::Rec2100Pq] {
            assert_eq!(
                ViewerGpuOutputPrecision::minimum_for_display(output, false),
                ViewerGpuOutputPrecision::EncodedFloat16
            );
        }
        assert_eq!(
            ViewerGpuOutputPrecision::minimum_for_display(ColorSpace::Srgb, true),
            ViewerGpuOutputPrecision::EncodedFloat16
        );
        assert_eq!(
            ViewerGpuOutputPrecision::EncodedFloat16.texture_format(),
            GpuColorFrameTextureFormat::Rgba16Float
        );
        assert!(matches!(
            validate_output_precision(ViewerGpuOutputPrecision::Encoded8, true),
            Err(ViewerGpuExecutionError::DisplayCalibrationRequiresFloatOutput)
        ));
    }

    #[test]
    fn viewer_rejects_monitor_adaptation_for_a_different_program_boundary() {
        let boundary = RenderOutputColorBoundary::display(
            ColorSpace::Rec709,
            false,
            ColorEngine::mondrian_standard(),
        );
        let adaptation = crate::RenderMonitorAdaptation::new(
            ColorSpace::Srgb,
            ColorSpace::DisplayP3,
            ColorEngine::mondrian_standard(),
        )
        .expect("valid standalone SDR adaptation");

        assert!(matches!(
            validate_program_monitor_contract(&boundary, &adaptation),
            Err(ViewerGpuExecutionError::ProgramMonitorBoundaryMismatch {
                program_boundary: ColorSpace::Rec709,
                adaptation_input: ColorSpace::Srgb,
            })
        ));
    }

    #[test]
    fn viewer_rejects_scopes_for_a_different_program_boundary_before_recording() {
        let boundary = RenderOutputColorBoundary::display(
            ColorSpace::Rec709,
            false,
            ColorEngine::mondrian_standard(),
        );
        let scopes =
            GpuProgramScopesRequest::new(ColorSpace::DisplayP3, WaveformMode::Luma, 256, 512)
                .expect("valid standalone P3 scopes");

        assert!(matches!(
            validate_program_scopes_contract(&boundary, Some(scopes)),
            Err(ViewerGpuExecutionError::ProgramScopesBoundaryMismatch {
                program_boundary: ColorSpace::Rec709,
                scopes_signal: ColorSpace::DisplayP3,
            })
        ));
        assert!(validate_program_scopes_contract(&boundary, None).is_ok());
    }

    #[tokio::test]
    async fn viewer_retains_program_output_before_gpu_monitor_adaptation() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping Viewer monitor-adaptation test: no GPU adapter available");
            return;
        };
        let program_output_boundary = RenderOutputColorBoundary::display(
            ColorSpace::Rec709,
            false,
            ColorEngine::mondrian_standard(),
        );
        let monitor_adaptation = RenderMonitorAdaptation::new(
            ColorSpace::Rec709,
            ColorSpace::DisplayP3,
            ColorEngine::mondrian_standard(),
        )
        .expect("SDR monitor adaptation");
        let mut runtime =
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewer-monitor-adaptation-integration"),
        });

        let record = runtime
            .record(
                &context.device,
                &context.queue,
                &mut encoder,
                ViewerGpuExecutionRequest {
                    sequence_id: SequenceId::new(),
                    timeline_frame: 0,
                    width: 4,
                    height: 4,
                    working_color_space: WorkingColorSpace::LinearRec709,
                    layers: &[],
                    program_output_boundary: &program_output_boundary,
                    monitor_adaptation: &monitor_adaptation,
                    source_rect: ViewerSourceRect::FULL,
                    output_width: 4,
                    output_height: 4,
                    output_precision: ViewerGpuOutputPrecision::Encoded8,
                    display_calibration: None,
                    program_scopes: Some(
                        GpuProgramScopesRequest::new(
                            ColorSpace::Rec709,
                            WaveformMode::Luma,
                            256,
                            512,
                        )
                        .expect("scope request"),
                    ),
                },
            )
            .expect("Viewer GPU monitor adaptation frame");
        context.queue.submit(std::iter::once(encoder.finish()));

        assert_eq!(
            record.program_output.descriptor().color_space,
            crate::ColorFrameSpace::Color(ColorSpace::Rec709)
        );
        assert_eq!(
            record.output.descriptor().color_space,
            crate::ColorFrameSpace::Color(ColorSpace::DisplayP3)
        );
        assert_eq!(record.stage_diagnostics.gpu_color_stages, 2);
        assert_eq!(record.stage_diagnostics.upload_stages, 0);
        assert_eq!(record.stage_diagnostics.readback_stages, 0);
        let scopes = record.program_scopes.as_ref().expect("Program Output scopes");
        assert_eq!(scopes.request.signal_color_space(), ColorSpace::Rec709);
        assert_eq!(runtime.program_scopes_diagnostics().frames_recorded, 1);
        runtime
            .program_output_texture_view(&record)
            .expect("retained Program Output texture");
        runtime.output_texture_view(&record).expect("retained monitor output texture");
    }

    #[tokio::test]
    async fn viewer_records_gpu_media_effect_domain_before_working_composite() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping Viewer effect-domain integration test: no GPU adapter available");
            return;
        };
        let domain = EffectColorDomain::DisplayEncodedRgb { color_space: ColorSpace::Rec709 };
        let graph = compile_scheduled_effect_graph_in_domain(
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                }],
            },
            EffectColorDomainContract::preserving(domain),
        )
        .expect("valid display-domain graph");
        let effect_plan =
            Arc::new(lower_effect_graph_to_gpu_plan(&graph).expect("GPU effect plan"));
        let source = Arc::new(CpuSourceColorFrame::from(
            CpuEncodedColorFrame::source_rgba8(4, 4, ColorSpace::Rec709, vec![96; 4 * 4 * 4]),
        ));
        let output_boundary = RenderOutputColorBoundary::display(
            ColorSpace::Rec709,
            false,
            ColorEngine::mondrian_standard(),
        );
        let monitor_adaptation = RenderMonitorAdaptation::new(
            ColorSpace::Rec709,
            ColorSpace::Rec709,
            ColorEngine::mondrian_standard(),
        )
        .expect("matching monitor adaptation");
        let mut runtime =
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue);
        for timeline_frame in 7..10 {
            let layer = ViewerGpuExecutionLayer::Media {
                frame: None,
                gpu_source: Some(ViewerGpuMediaSource {
                    source: Arc::clone(&source),
                    input_transform: RenderInputTransform::to_working_gpu(
                        WorkingColorSpace::LinearRec709,
                        false,
                        ColorEngine::mondrian_standard(),
                    ),
                    decoder_residency: DecodedFrameResidency::CpuRgba,
                    decoder_handle_kind: None,
                    decoded_surface_format: DecodedVideoSurfaceFormat::Rgba8,
                    decoded_video_sampling: DecodedVideoSampling::default(),
                }),
                native_source: None,
                opacity: 1.0,
                transform: [0.5, 0.0, 1.0, 0.0, 0.5, 1.0],
                effect_plan: Arc::clone(&effect_plan),
                frame_seed: timeline_frame,
            };
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("viewer-effect-domain-integration-resource-reuse"),
                });

            let record = runtime
                .record(
                    &context.device,
                    &context.queue,
                    &mut encoder,
                    ViewerGpuExecutionRequest {
                        sequence_id: SequenceId::new(),
                        timeline_frame,
                        width: 4,
                        height: 4,
                        working_color_space: WorkingColorSpace::LinearRec709,
                        layers: &[layer],
                        program_output_boundary: &output_boundary,
                        monitor_adaptation: &monitor_adaptation,
                        source_rect: ViewerSourceRect::FULL,
                        output_width: 4,
                        output_height: 4,
                        output_precision: ViewerGpuOutputPrecision::Encoded8,
                        display_calibration: None,
                        program_scopes: None,
                    },
                )
                .expect("Viewer GPU effect-domain frame");
            context.queue.submit(std::iter::once(encoder.finish()));

            assert_eq!(record.stage_diagnostics.gpu_color_stages, 4);
            assert_eq!(record.stage_diagnostics.upload_stages, 1);
            assert_eq!(record.stage_diagnostics.readback_stages, 0);
            assert_eq!(record.compositing_diagnostics.gpu_passthrough_frames, 0);
            assert_eq!(record.compositing_diagnostics.gpu_native_composites, 1);
            assert_eq!(runtime.program_scopes_diagnostics().frames_recorded, 0);
            assert_eq!(
                record.output.descriptor().domain,
                crate::ColorFrameDomain::Display
            );
            runtime.clear_frame_resources();
        }
    }

    #[tokio::test]
    async fn viewer_uploads_cpu_working_media_for_external_effect_domain() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping Viewer CPU effect-domain upload test: no GPU adapter available");
            return;
        };
        let domain = EffectColorDomain::DisplayEncodedRgb { color_space: ColorSpace::Rec709 };
        let graph = compile_scheduled_effect_graph_in_domain(
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                }],
            },
            EffectColorDomainContract::preserving(domain),
        )
        .expect("valid display-domain graph");
        let effect_plan =
            Arc::new(lower_effect_graph_to_gpu_plan(&graph).expect("GPU effect plan"));
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 4,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![[0.18, 0.08, 0.02, 1.0]; 16],
        });
        let layer = ViewerGpuExecutionLayer::Media {
            frame: Some(frame),
            gpu_source: None,
            native_source: None,
            opacity: 1.0,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan,
            frame_seed: 9,
        };
        let output_boundary = RenderOutputColorBoundary::display(
            ColorSpace::Rec709,
            false,
            ColorEngine::mondrian_standard(),
        );
        let monitor_adaptation = RenderMonitorAdaptation::new(
            ColorSpace::Rec709,
            ColorSpace::Rec709,
            ColorEngine::mondrian_standard(),
        )
        .expect("matching monitor adaptation");
        let mut runtime =
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewer-cpu-working-effect-domain"),
        });

        let record = runtime
            .record(
                &context.device,
                &context.queue,
                &mut encoder,
                ViewerGpuExecutionRequest {
                    sequence_id: SequenceId::new(),
                    timeline_frame: 9,
                    width: 4,
                    height: 4,
                    working_color_space: WorkingColorSpace::LinearRec709,
                    layers: &[layer],
                    program_output_boundary: &output_boundary,
                    monitor_adaptation: &monitor_adaptation,
                    source_rect: ViewerSourceRect::FULL,
                    output_width: 4,
                    output_height: 4,
                    output_precision: ViewerGpuOutputPrecision::Encoded8,
                    display_calibration: None,
                    program_scopes: None,
                },
            )
            .expect("Viewer CPU working effect-domain frame");
        context.queue.submit(std::iter::once(encoder.finish()));

        assert_eq!(record.stage_diagnostics.gpu_color_stages, 3);
        assert_eq!(record.stage_diagnostics.upload_stages, 1);
        assert_eq!(record.stage_diagnostics.readback_stages, 0);
        assert_eq!(record.residency.cpu_upload_layers, 1);
        assert_eq!(record.compositing_diagnostics.gpu_passthrough_frames, 1);
    }

    #[tokio::test]
    async fn viewer_skips_zero_opacity_media_before_upload_and_effect_domain() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping Viewer zero-opacity media test: no GPU adapter available");
            return;
        };
        let domain = EffectColorDomain::DisplayEncodedRgb { color_space: ColorSpace::Rec709 };
        let graph = compile_scheduled_effect_graph_in_domain(
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                }],
            },
            EffectColorDomainContract::preserving(domain),
        )
        .expect("valid display-domain graph");
        let effect_plan =
            Arc::new(lower_effect_graph_to_gpu_plan(&graph).expect("GPU effect plan"));
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 4,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![[0.18, 0.08, 0.02, 1.0]; 16],
        });
        let layer = ViewerGpuExecutionLayer::Media {
            frame: Some(frame),
            gpu_source: None,
            native_source: None,
            opacity: 0.0,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan,
            frame_seed: 9,
        };
        let output_boundary = RenderOutputColorBoundary::display(
            ColorSpace::Rec709,
            false,
            ColorEngine::mondrian_standard(),
        );
        let monitor_adaptation = RenderMonitorAdaptation::new(
            ColorSpace::Rec709,
            ColorSpace::Rec709,
            ColorEngine::mondrian_standard(),
        )
        .expect("matching monitor adaptation");
        let mut runtime =
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewer-zero-opacity-media"),
        });

        let record = runtime
            .record(
                &context.device,
                &context.queue,
                &mut encoder,
                ViewerGpuExecutionRequest {
                    sequence_id: SequenceId::new(),
                    timeline_frame: 9,
                    width: 4,
                    height: 4,
                    working_color_space: WorkingColorSpace::LinearRec709,
                    layers: &[layer],
                    program_output_boundary: &output_boundary,
                    monitor_adaptation: &monitor_adaptation,
                    source_rect: ViewerSourceRect::FULL,
                    output_width: 4,
                    output_height: 4,
                    output_precision: ViewerGpuOutputPrecision::Encoded8,
                    display_calibration: None,
                    program_scopes: None,
                },
            )
            .expect("Viewer zero-opacity media frame");
        context.queue.submit(std::iter::once(encoder.finish()));

        assert_eq!(record.stage_diagnostics.gpu_color_stages, 1);
        assert_eq!(record.stage_diagnostics.upload_stages, 0);
        assert_eq!(record.stage_diagnostics.readback_stages, 0);
        assert_eq!(record.residency.media_layers, 0);
        assert_eq!(record.residency.cpu_upload_layers, 0);
        assert_eq!(
            runtime.compositor_uniform_arena_diagnostics().uniform_writes,
            0
        );
    }

    #[tokio::test]
    async fn viewer_materializes_solid_before_external_effect_domain() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping Viewer solid effect-domain test: no GPU adapter available");
            return;
        };
        let domain = EffectColorDomain::DisplayEncodedRgb { color_space: ColorSpace::Rec709 };
        let graph = compile_scheduled_effect_graph_in_domain(
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                }],
            },
            EffectColorDomainContract::preserving(domain),
        )
        .expect("valid display-domain graph");
        let effect_plan =
            Arc::new(lower_effect_graph_to_gpu_plan(&graph).expect("GPU effect plan"));
        let layer = ViewerGpuExecutionLayer::SolidColor {
            layer: TimelineSolidColorLayer {
                color: Color { r: 0.18, g: 0.08, b: 0.02, a: 0.75 },
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: Arc::new(graph),
                frame_seed: 11,
            },
            effect_plan,
        };
        let output_boundary = RenderOutputColorBoundary::display(
            ColorSpace::Rec709,
            false,
            ColorEngine::mondrian_standard(),
        );
        let monitor_adaptation = RenderMonitorAdaptation::new(
            ColorSpace::Rec709,
            ColorSpace::Rec709,
            ColorEngine::mondrian_standard(),
        )
        .expect("matching monitor adaptation");
        let mut runtime =
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewer-solid-effect-domain"),
        });

        let record = runtime
            .record(
                &context.device,
                &context.queue,
                &mut encoder,
                ViewerGpuExecutionRequest {
                    sequence_id: SequenceId::new(),
                    timeline_frame: 11,
                    width: 4,
                    height: 4,
                    working_color_space: WorkingColorSpace::LinearRec709,
                    layers: &[layer],
                    program_output_boundary: &output_boundary,
                    monitor_adaptation: &monitor_adaptation,
                    source_rect: ViewerSourceRect::FULL,
                    output_width: 4,
                    output_height: 4,
                    output_precision: ViewerGpuOutputPrecision::Encoded8,
                    display_calibration: None,
                    program_scopes: None,
                },
            )
            .expect("Viewer GPU solid effect-domain frame");
        context.queue.submit(std::iter::once(encoder.finish()));

        assert_eq!(record.stage_diagnostics.gpu_color_stages, 3);
        assert_eq!(record.stage_diagnostics.upload_stages, 0);
        assert_eq!(record.stage_diagnostics.readback_stages, 0);
        assert_eq!(record.compositing_diagnostics.gpu_passthrough_frames, 1);
        assert_eq!(record.compositing_diagnostics.gpu_native_composites, 0);
        assert_eq!(record.residency.procedural_layers, 1);
    }

    #[tokio::test]
    async fn viewer_keeps_affine_scene_linear_solid_procedural() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping Viewer affine solid test: no GPU adapter available");
            return;
        };
        let graph = get_or_compile_scheduled_render_graph(EffectGraphBuilderState::new().finish())
            .expect("valid scene-linear identity graph");
        let effect_plan =
            Arc::new(lower_effect_graph_to_gpu_plan(&graph).expect("GPU identity plan"));
        let layer = ViewerGpuExecutionLayer::SolidColor {
            layer: TimelineSolidColorLayer {
                color: Color { r: 0.18, g: 0.08, b: 0.02, a: 1.0 },
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [0.75, 0.0, 0.125, 0.0, 0.75, 0.125],
                effect_graph: Arc::clone(&graph),
                frame_seed: 0,
            },
            effect_plan,
        };
        let output_boundary = RenderOutputColorBoundary::display(
            ColorSpace::Rec709,
            false,
            ColorEngine::mondrian_standard(),
        );
        let monitor_adaptation = RenderMonitorAdaptation::new(
            ColorSpace::Rec709,
            ColorSpace::Rec709,
            ColorEngine::mondrian_standard(),
        )
        .expect("matching monitor adaptation");
        let mut runtime =
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewer-affine-scene-solid"),
        });

        let record = runtime
            .record(
                &context.device,
                &context.queue,
                &mut encoder,
                ViewerGpuExecutionRequest {
                    sequence_id: SequenceId::new(),
                    timeline_frame: 0,
                    width: 4,
                    height: 4,
                    working_color_space: WorkingColorSpace::LinearRec709,
                    layers: &[layer],
                    program_output_boundary: &output_boundary,
                    monitor_adaptation: &monitor_adaptation,
                    source_rect: ViewerSourceRect::FULL,
                    output_width: 4,
                    output_height: 4,
                    output_precision: ViewerGpuOutputPrecision::EncodedFloat16,
                    display_calibration: None,
                    program_scopes: None,
                },
            )
            .expect("Viewer affine scene-linear solid frame");
        context.queue.submit(std::iter::once(encoder.finish()));

        assert_eq!(record.stage_diagnostics.gpu_color_stages, 1);
        assert_eq!(record.stage_diagnostics.upload_stages, 0);
        assert_eq!(record.compositing_diagnostics.gpu_native_composites, 1);
        assert_eq!(record.residency.procedural_layers, 1);
        assert_eq!(
            record.output.texture_format(),
            GpuColorFrameTextureFormat::Rgba16Float
        );
    }

    #[tokio::test]
    async fn viewer_composite_to_program_output_matches_cpu_on_real_wgpu_device() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping Viewer composite-to-output parity test: no GPU adapter");
            return;
        };
        let graph = get_or_compile_scheduled_render_graph(EffectGraphBuilderState::new().finish())
            .expect("valid scene-linear identity graph");
        let source = Arc::new(CpuSourceColorFrame::from(
            CpuEncodedColorFrame::source_rgba8(
                4,
                4,
                ColorSpace::Rec2100Hlg,
                [64_u8, 96, 128, 255].repeat(16),
            ),
        ));
        let gpu_input_transform = RenderInputTransform::to_working_gpu(
            WorkingColorSpace::LinearRec2020,
            false,
            ColorEngine::mondrian_standard(),
        );
        let layer = ViewerGpuExecutionLayer::Media {
            frame: None,
            gpu_source: Some(ViewerGpuMediaSource {
                source: Arc::clone(&source),
                input_transform: gpu_input_transform,
                decoder_residency: DecodedFrameResidency::CpuRgba,
                decoder_handle_kind: None,
                decoded_surface_format: DecodedVideoSurfaceFormat::Rgba8,
                decoded_video_sampling: DecodedVideoSampling::default(),
            }),
            native_source: None,
            opacity: 1.0,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Arc::new(
                lower_effect_graph_to_gpu_plan(&graph).expect("GPU identity plan"),
            ),
            frame_seed: 0,
        };
        let boundary = RenderOutputColorBoundary::from_intent(
            crate::RenderOutputColorBoundaryTarget::Display,
            ColorSpace::Rec709,
            &mondrian_core::OutputTransformIntent::mondrian_standard(),
            true,
            ColorEngine::mondrian_standard(),
        )
        .expect("Standard Rec.709 display boundary");
        let monitor_adaptation = RenderMonitorAdaptation::new(
            ColorSpace::Rec709,
            ColorSpace::Rec709,
            ColorEngine::mondrian_standard(),
        )
        .expect("matching monitor adaptation");
        let cpu_input_transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec2020,
            false,
            ColorEngine::mondrian_standard(),
        );
        let working = crate::execute_cpu_source_input_stage(&source, &cpu_input_transform)
            .expect("CPU HLG input reference");
        let expected = crate::execute_cpu_output_boundary_rgba8(&working.result.frame, &boundary)
            .expect("CPU Program Output reference");
        let mut runtime =
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewer-composite-to-program-output-parity"),
        });

        let record = runtime
            .record(
                &context.device,
                &context.queue,
                &mut encoder,
                ViewerGpuExecutionRequest {
                    sequence_id: SequenceId::new(),
                    timeline_frame: 0,
                    width: 4,
                    height: 4,
                    working_color_space: WorkingColorSpace::LinearRec2020,
                    layers: &[layer],
                    program_output_boundary: &boundary,
                    monitor_adaptation: &monitor_adaptation,
                    source_rect: ViewerSourceRect::FULL,
                    output_width: 4,
                    output_height: 4,
                    output_precision: ViewerGpuOutputPrecision::Encoded8,
                    display_calibration: None,
                    program_scopes: None,
                },
            )
            .expect("Viewer composite-to-output frame");
        let output = runtime
            .color_output
            .frame_table()
            .get(&record.output)
            .expect("Viewer output resource");
        let readback_plan = crate::GpuColorFrameReadbackPlan::encoded_rgba8(record.output.clone())
            .expect("Viewer output should be RGBA8");
        let readback = crate::GpuColorFrameReadback::record_copy(
            &context.device,
            &mut encoder,
            &readback_plan,
            output.resource(),
        );
        context.queue.submit(std::iter::once(encoder.finish()));
        let (sender, receiver) = std::sync::mpsc::channel();
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        let _ = context
            .device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
        receiver.recv().expect("Viewer readback callback").expect("Viewer readback map");
        let mapped = slice.get_mapped_range().expect("Viewer mapped readback");
        let actual = readback_plan.unpack_mapped_rgba8(&mapped).expect("Viewer RGBA8 readback");
        drop(mapped);
        readback.unmap();

        let max_delta = expected
            .rgba
            .iter()
            .zip(actual.rgba())
            .map(|(&expected, &actual)| expected.abs_diff(actual))
            .max()
            .unwrap_or(0);
        assert!(
            max_delta <= 1,
            "Viewer Program Output max RGBA delta {max_delta}"
        );
    }

    #[tokio::test]
    async fn viewer_interleaves_external_adjustment_with_working_composite() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping Viewer adjustment effect-domain test: no GPU adapter available");
            return;
        };
        let scene_graph =
            get_or_compile_scheduled_render_graph(EffectGraphBuilderState::new().finish())
                .expect("valid scene-linear identity graph");
        let scene_plan = Arc::new(
            lower_effect_graph_to_gpu_plan(&scene_graph).expect("scene-linear GPU identity plan"),
        );
        let domain = EffectColorDomain::DisplayEncodedRgb { color_space: ColorSpace::Rec709 };
        let adjustment_graph = compile_scheduled_effect_graph_in_domain(
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                }],
            },
            EffectColorDomainContract::preserving(domain),
        )
        .expect("valid display-domain adjustment graph");
        let adjustment_plan = Arc::new(
            lower_effect_graph_to_gpu_plan(&adjustment_graph).expect("GPU adjustment plan"),
        );
        let layers = [
            ViewerGpuExecutionLayer::SolidColor {
                layer: TimelineSolidColorLayer {
                    color: Color { r: 0.18, g: 0.08, b: 0.02, a: 1.0 },
                    opacity: 1.0,
                    blend_mode: BlendMode::Normal,
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_graph: Arc::clone(&scene_graph),
                    frame_seed: 0,
                },
                effect_plan: scene_plan,
            },
            ViewerGpuExecutionLayer::Adjustment {
                effect_plan: adjustment_plan,
                opacity: 0.6,
                blend_mode: BlendMode::Normal,
                frame_seed: 13,
            },
        ];
        let output_boundary = RenderOutputColorBoundary::display(
            ColorSpace::Rec709,
            false,
            ColorEngine::mondrian_standard(),
        );
        let monitor_adaptation = RenderMonitorAdaptation::new(
            ColorSpace::Rec709,
            ColorSpace::Rec709,
            ColorEngine::mondrian_standard(),
        )
        .expect("matching monitor adaptation");
        let mut runtime =
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewer-adjustment-effect-domain"),
        });

        let record = runtime
            .record(
                &context.device,
                &context.queue,
                &mut encoder,
                ViewerGpuExecutionRequest {
                    sequence_id: SequenceId::new(),
                    timeline_frame: 13,
                    width: 4,
                    height: 4,
                    working_color_space: WorkingColorSpace::LinearRec709,
                    layers: &layers,
                    program_output_boundary: &output_boundary,
                    monitor_adaptation: &monitor_adaptation,
                    source_rect: ViewerSourceRect::FULL,
                    output_width: 4,
                    output_height: 4,
                    output_precision: ViewerGpuOutputPrecision::Encoded8,
                    display_calibration: None,
                    program_scopes: None,
                },
            )
            .expect("Viewer GPU external-domain adjustment frame");
        context.queue.submit(std::iter::once(encoder.finish()));

        assert_eq!(record.stage_diagnostics.gpu_color_stages, 3);
        assert_eq!(record.stage_diagnostics.upload_stages, 0);
        assert_eq!(record.stage_diagnostics.readback_stages, 0);
        assert!(record.compositing_diagnostics.gpu_native_composites >= 2);
    }
}
