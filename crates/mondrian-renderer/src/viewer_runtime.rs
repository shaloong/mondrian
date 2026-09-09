//! Stateful GPU resources for one Viewer preview execution context.
//!
//! Windowing is an Adapter concern. The resources below instead belong to the
//! Viewer GPU execution lifetime and must be shared by every production or
//! headless Adapter that executes the same preview path.

#[cfg(test)]
#[path = "viewer_runtime/retirement_tests.rs"]
mod retirement_tests;

use std::sync::Arc;
use std::time::Instant;

use crate::viewer_spatial::{GpuViewerSpatialRecord, GpuViewerSpatialRuntime};
use crate::{
    estimate_viewer_gpu_active_working_set, native_source_texture_format_from_decoded,
    native_video_sampling_from_decoded, CpuColorFrame,
    GpuColorFrameBindGroupCacheKeyAllocationError, GpuColorFrameHandle, GpuColorFrameId,
    GpuColorFrameIdAllocationError, GpuColorFrameResourceTableError, GpuColorFrameTextureFormat,
    GpuColorFrameWgpuResourcePool, GpuCompositeLayer, GpuCompositeLayerSource, GpuCompositeRequest,
    GpuCompositingDiagnostics, GpuDisplayCalibrationRuntime, GpuFrameCompositor,
    GpuNativeDecodedFrameImportError, GpuNativeDecodedFrameImportSupport,
    GpuNativeDecodedFrameTextureFormat, GpuNativeDecodedFrameVideoSampling, GpuProgramScopesError,
    GpuProgramScopesRecord, GpuProgramScopesRequest, GpuProgramScopesRuntime,
    GpuProgramScopesRuntimeDiagnostics, GpuSignalMonitorError, GpuSignalMonitorRequest,
    GpuSignalMonitorRuntime, GpuViewerSpatialRuntimeDiagnostics, GpuWorkingFloatDecision,
    HeterogeneousGpuCompletedContinuation, HeterogeneousGpuContinuationError,
    HeterogeneousGpuRecordResources, HeterogeneousGpuRecordedContinuation,
    HeterogeneousGpuSubmittedContinuation, NativeVideoImportCandidateTimingReceipt,
    NativeVideoImportCpuTimings, NativeVideoImportGpuTimingDiagnostics,
    NativeVideoImportGpuTimingPolicy, NativeVideoImportGpuTimingSample,
    RenderColorStageDiagnostics, RenderColorTransformGpuOptions,
    RenderGpuColorTransformRuntimeRecordError, RenderGpuCompositeGraphRecordError,
    RenderGpuInputStageRecord, RenderGpuInputStageRuntimeRecordError,
    RenderGpuOutputBoundaryRuntime, RenderGpuOutputBoundaryRuntimeOwnedBackendContext,
    RenderGpuOutputBoundaryRuntimeRecordError, RenderMonitorAdaptation, RenderOutputColorBoundary,
    ViewerGpuActiveWorkingSetAdmissionError, ViewerGpuActiveWorkingSetEstimate,
    ViewerGpuActiveWorkingSetEstimateError, ViewerGpuExecutionLayer,
    ViewerGpuExecutionResourceGrant, ViewerGpuMediaSource, ViewerGpuNativeSource,
    ViewerGpuPresentationOutputLease, ViewerHeterogeneousGpuInput, ViewerNativeVideoImportRuntime,
    ViewerSourceRect, PRODUCT_GPU_WORKING_FLOAT_DECISION,
};
use mondrian_core::display_calibration::DisplayCalibrationLut3d;
use mondrian_core::types::{BlendMode, Color, SequenceId};
use mondrian_core::{ProgramScopesTap, WorkingColorSpace};
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
    /// Move-only CPU-prefix completions addressed by heterogeneous media
    /// layers.
    ///
    /// Each addressed entry is consumed exactly once during recording. Entries
    /// not referenced by a contributing layer are discarded without recording
    /// or submission.
    pub heterogeneous_inputs: Vec<ViewerHeterogeneousGpuInput>,
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
    /// Optional fused false-color, zebra, and gamut warning pass.
    pub signal_monitoring: Option<GpuSignalMonitorRequest>,
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
    /// Preview-only monitor-adaptation commands are complete.
    MonitorAdaptation,
    /// Optional Program/Monitor scopes commands are complete.
    ProgramScopes,
    /// Optional fused signal-monitoring commands are complete.
    SignalMonitoring,
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
/// Candidate-owned frame resources are cleared between records; a presentation
/// output explicitly detached into [`ViewerGpuPresentationOutputLease`] is no
/// longer candidate-owned and remains valid across ordinary clears. `reset`
/// invalidates the shared pool generation so a lease may remain readable while
/// its later drop cannot repopulate a retired device/runtime epoch. Pipelines
/// and backend capabilities otherwise remain resident for this object's
/// lifetime.
pub struct ViewerGpuExecutionRuntime {
    resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
    resource_grant: ViewerGpuExecutionResourceGrant,
    last_active_working_set: Option<ViewerGpuActiveWorkingSetEstimate>,
    native_video_import: ViewerNativeVideoImportRuntime,
    cpu_yuv_upload: crate::cpu_yuv::CpuYuvUploadRuntime,
    color_output: RenderGpuOutputBoundaryRuntime,
    spatial: GpuViewerSpatialRuntime,
    display_calibration: GpuDisplayCalibrationRuntime,
    program_scopes: GpuProgramScopesRuntime,
    signal_monitor: GpuSignalMonitorRuntime,
    working_compositor: GpuFrameCompositor,
}

/// Error creating one Viewer GPU execution context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ViewerGpuExecutionRuntimeCreateError {
    /// Renderer frame identity allocation is exhausted.
    #[error(transparent)]
    FrameId(#[from] GpuColorFrameIdAllocationError),
    /// Renderer bind-group cache identity allocation is exhausted.
    #[error(transparent)]
    BindGroupCacheKey(#[from] GpuColorFrameBindGroupCacheKeyAllocationError),
    /// Renderer-owned compact CPU YUV upload worker could not start.
    #[error("compact CPU YUV upload worker could not start")]
    CpuYuvUploadWorker,
}

impl ViewerGpuExecutionRuntime {
    /// Create one execution context for a renderer device.
    pub fn new(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Self, ViewerGpuExecutionRuntimeCreateError> {
        Self::new_with_native_import_gpu_timing_policy(
            adapter,
            device,
            queue,
            NativeVideoImportGpuTimingPolicy::default(),
        )
    }

    /// Create one execution context with an explicit native-import timing policy.
    pub fn new_with_native_import_gpu_timing_policy(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        native_import_gpu_timing_policy: NativeVideoImportGpuTimingPolicy,
    ) -> Result<Self, ViewerGpuExecutionRuntimeCreateError> {
        let resource_grant = ViewerGpuExecutionResourceGrant::default();
        let resource_pool = Arc::new(GpuColorFrameWgpuResourcePool::new(
            resource_grant.output_pool,
        ));
        // Prepare every fallible GPU component before starting an upload worker.
        // An early construction error then owns no detached upload execution.
        let native_video_import =
            ViewerNativeVideoImportRuntime::new_with_resource_pool_and_gpu_timing_policy(
                adapter,
                device,
                queue,
                Arc::clone(&resource_pool),
                native_import_gpu_timing_policy,
            );
        let color_output =
            RenderGpuOutputBoundaryRuntime::with_resource_pool(Arc::clone(&resource_pool))?;
        let spatial = GpuViewerSpatialRuntime::with_resource_pool(Arc::clone(&resource_pool));
        let display_calibration =
            GpuDisplayCalibrationRuntime::with_resource_pool(Arc::clone(&resource_pool))?;
        let program_scopes = GpuProgramScopesRuntime::default();
        let signal_monitor =
            GpuSignalMonitorRuntime::with_resource_pool(Arc::clone(&resource_pool))?;
        let working_compositor = GpuFrameCompositor::new(device)?;
        let cpu_yuv_upload = crate::cpu_yuv::CpuYuvUploadRuntime::new(device)
            .map_err(|_| ViewerGpuExecutionRuntimeCreateError::CpuYuvUploadWorker)?;
        Ok(Self {
            resource_pool,
            resource_grant,
            last_active_working_set: None,
            native_video_import,
            cpu_yuv_upload,
            color_output,
            spatial,
            display_calibration,
            program_scopes,
            signal_monitor,
            working_compositor,
        })
    }

    /// Close upload admission and transfer all resources to a poll-only owner.
    ///
    /// The Adapter must retain the returned owner through both its Renderer
    /// receipt and the independent submission-lifecycle/whole-queue barriers.
    /// This transition never joins a running worker on the caller thread.
    pub fn into_retirement(self) -> crate::ViewerGpuExecutionRetirement {
        let Self {
            resource_pool,
            resource_grant: _,
            last_active_working_set: _,
            native_video_import,
            cpu_yuv_upload,
            color_output,
            spatial,
            display_calibration,
            program_scopes,
            signal_monitor,
            working_compositor,
        } = self;
        crate::ViewerGpuExecutionRetirement {
            native_video_import,
            cpu_yuv_upload: cpu_yuv_upload.into_retirement(),
            terminal: None,
            native_device_removed: false,
            _resource_pool: resource_pool,
            _color_output: color_output,
            _spatial: spatial,
            _display_calibration: display_calibration,
            _program_scopes: program_scopes,
            _signal_monitor: signal_monitor,
            _working_compositor: working_compositor,
        }
    }

    /// Return the Viewer owner's idle-retention and active-frame grant.
    pub const fn resource_grant(&self) -> ViewerGpuExecutionResourceGrant {
        self.resource_grant
    }

    /// Return the hard grant and most recent admitted request estimate.
    pub const fn active_working_set_diagnostics(
        &self,
    ) -> crate::ViewerGpuActiveWorkingSetDiagnostics {
        crate::ViewerGpuActiveWorkingSetDiagnostics {
            grant: self.resource_grant,
            last_admitted: self.last_active_working_set,
        }
    }

    /// Apply a changed Viewer resource grant without rebuilding GPU pipelines.
    ///
    /// Idle resources above a reduced idle grant are retired synchronously.
    /// Checked-out resources remain valid and observe that idle grant when they
    /// return to the shared device-scoped pool. Changed active limits apply to
    /// the next request admission and never invalidate an already-recording
    /// candidate; product policy keeps those limits stable across pressure.
    pub fn reconfigure_resource_grant(&mut self, grant: ViewerGpuExecutionResourceGrant) -> bool {
        if self.resource_grant == grant {
            return false;
        }
        self.resource_pool.reconfigure(grant.output_pool);
        self.resource_grant = grant;
        true
    }

    /// Release all idle Viewer color-frame textures while retaining pipelines,
    /// exact execution contracts, and resources owned by an active candidate.
    pub fn clear_idle_resources(&self) {
        self.resource_pool.clear();
        self.cpu_yuv_upload.clear();
    }

    /// Native decoder import capability exposed to preview scheduling.
    pub fn native_import_support(&self) -> GpuNativeDecodedFrameImportSupport {
        self.native_video_import.support()
    }

    /// Renderer-qualified decoder device root paired with native import support.
    pub fn native_decode_device_root(
        &self,
    ) -> Option<mondrian_media::RendererHwAccelDeviceContext> {
        self.native_video_import.decoder_device_root()
    }

    /// Install the payload-free wake edge emitted when a compact CPU YUV
    /// transfer buffer becomes ready for candidate recording.
    pub fn install_cpu_yuv_upload_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        self.cpu_yuv_upload.install_completion_waker(waker);
    }

    /// Start compact CPU YUV transfer preparation without recording or
    /// reserving a GPU submission.
    ///
    /// The operation visits every contributing ordinary or Transition input,
    /// schedules each distinct retained media frame, and leaves completed
    /// preparations unconsumed for the later exact Viewer candidate. `true`
    /// means every compact input is ready to record now; requests without such
    /// inputs are trivially ready.
    pub fn prepare_cpu_yuv_uploads(
        &self,
        layers: &[ViewerGpuExecutionLayer],
    ) -> Result<bool, ViewerGpuExecutionError> {
        let mut seen = Vec::with_capacity(layers.len().saturating_mul(2));
        let mut all_ready = true;
        for layer in layers {
            match layer {
                ViewerGpuExecutionLayer::Source(source) => {
                    prepare_source_cpu_yuv_upload(
                        source,
                        &self.cpu_yuv_upload,
                        &mut seen,
                        &mut all_ready,
                    )?;
                }
                ViewerGpuExecutionLayer::Adjustment { .. } => {}
                ViewerGpuExecutionLayer::CrossDissolve(transition) => {
                    if !transition.progress.is_finite() {
                        continue;
                    }
                    let progress = transition.progress.clamp(0.0, 1.0);
                    for (input, weight) in [
                        (&transition.left, 1.0 - progress),
                        (&transition.right, progress),
                    ] {
                        if weight <= 0.0 {
                            continue;
                        }
                        if let crate::ViewerGpuTransitionInput::Source(source) = input {
                            prepare_source_cpu_yuv_upload(
                                source,
                                &self.cpu_yuv_upload,
                                &mut seen,
                                &mut all_ready,
                            )?;
                        }
                    }
                }
            }
        }
        Ok(all_ready)
    }

    /// Prepare contract-specific native-video input color objects without
    /// adopting decoder surfaces or recording a Viewer candidate.
    ///
    /// This is a bounded preroll seam. The exact later candidate still owns
    /// the native surface transition and all frame-local GPU resources.
    pub fn prepare_native_video_imports(
        &mut self,
        layers: &[ViewerGpuExecutionLayer],
    ) -> Result<(), ViewerGpuExecutionError> {
        if !self.native_video_import.support().renderer_backend_ready {
            return Ok(());
        }
        let mut seen = Vec::with_capacity(layers.len().saturating_mul(2));
        for layer in layers {
            match layer {
                ViewerGpuExecutionLayer::Source(source) => {
                    prepare_source_native_video_import(
                        source,
                        &mut self.native_video_import,
                        &mut seen,
                    )?;
                }
                ViewerGpuExecutionLayer::Adjustment { .. } => {}
                ViewerGpuExecutionLayer::CrossDissolve(transition) => {
                    if !transition.progress.is_finite() {
                        continue;
                    }
                    let progress = transition.progress.clamp(0.0, 1.0);
                    for (input, weight) in [
                        (&transition.left, 1.0 - progress),
                        (&transition.right, progress),
                    ] {
                        if weight <= 0.0 {
                            continue;
                        }
                        if let crate::ViewerGpuTransitionInput::Source(source) = input {
                            prepare_source_native_video_import(
                                source,
                                &mut self.native_video_import,
                                &mut seen,
                            )?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Collect native-import callbacks after the owner has polled the device.
    pub fn collect_native_import_gpu_timings_after_device_poll(&mut self) {
        self.native_video_import.collect_gpu_timings_after_device_poll();
    }

    /// Drain completed native-import GPU timing samples.
    pub fn take_completed_native_import_gpu_timings(
        &mut self,
    ) -> Vec<NativeVideoImportGpuTimingSample> {
        self.native_video_import.take_completed_gpu_timings()
    }

    /// Cumulative native-import GPU timing coverage and availability.
    pub fn native_import_gpu_timing_diagnostics(&self) -> NativeVideoImportGpuTimingDiagnostics {
        self.native_video_import.gpu_timing_diagnostics()
    }

    /// Current bounded native-import contract and compatibility bridge residency.
    pub fn native_import_pool_residency(&self) -> (usize, usize) {
        self.native_video_import.pool_residency()
    }

    /// Native decoder surfaces retained until the renderer proves its final read complete.
    pub fn native_import_retained_source_count(&self) -> usize {
        self.native_video_import.retained_source_count()
    }

    /// Non-blockingly retire decoder sources whose renderer use completed.
    pub fn retire_completed_native_import_sources(
        &mut self,
    ) -> Result<usize, GpuNativeDecodedFrameImportError> {
        self.native_video_import.retire_completed_source_residency()
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

    /// Point-in-time evidence for creative-LUT device residency and reuse.
    pub fn compositor_creative_lut_diagnostics(&self) -> crate::GpuCreativeLutCacheDiagnostics {
        self.working_compositor.creative_lut_diagnostics()
    }

    /// Point-in-time evidence that hidden scopes perform no work and visible
    /// scopes reuse retained pipelines and display resources.
    pub fn program_scopes_diagnostics(&self) -> GpuProgramScopesRuntimeDiagnostics {
        self.program_scopes.diagnostics()
    }

    /// Return point-in-time evidence for device-scoped Viewer texture reuse.
    ///
    /// Qualification takes a snapshot after warmup and after the measured
    /// interval. The difference proves whether the production runtime stayed
    /// allocation-free without exposing resource-table ownership.
    pub fn resource_pool_diagnostics(&self) -> crate::GpuColorFrameWgpuResourcePoolDiagnostics {
        self.resource_pool.diagnostics()
    }

    /// Release resources scoped to the current candidate, retaining pipelines.
    pub fn clear_frame_resources(&mut self) {
        self.cpu_yuv_upload.begin_frame();
        self.working_compositor.clear_frame_resources();
        self.color_output.clear_frame_resources();
        self.spatial.clear_frame_resources();
        self.display_calibration.clear_frame_resources();
        self.signal_monitor.clear_frame_resources();
    }

    /// Record one current Viewer frame through the shared GPU execution path.
    ///
    /// The returned handle remains owned by this runtime until the next frame
    /// clear/reset or an exact call to [`Self::take_presentation_output`].
    /// Presentation registration and publication are Adapter work.
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
        stage_marker: Option<&mut dyn ViewerGpuExecutionStageMarker>,
    ) -> Result<ViewerGpuExecutionRecord, ViewerGpuExecutionError> {
        let candidate_token = self.native_video_import.begin_viewer_candidate();
        let result =
            self.record_candidate_with_stage_marker(device, queue, encoder, request, stage_marker);
        if result.is_ok() {
            self.cpu_yuv_upload.finish_candidate(encoder);
        } else {
            self.cpu_yuv_upload.discard_candidate();
        }
        let receipt =
            self.native_video_import.end_viewer_candidate(candidate_token, result.is_ok());
        match result {
            Ok(mut record) => {
                record.native_video_import_timing_receipt = receipt;
                Ok(record)
            }
            Err(error) => {
                debug_assert!(receipt.is_none());
                Err(error)
            }
        }
    }

    fn record_candidate_with_stage_marker(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        mut request: ViewerGpuExecutionRequest<'_>,
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
        validate_program_scopes_contract(
            request.program_output_boundary,
            request.monitor_adaptation,
            request.program_scopes,
        )?;
        validate_signal_monitor_contract(
            request.program_output_boundary,
            request.monitor_adaptation,
            request.signal_monitoring,
        )?;
        if !self.prepare_cpu_yuv_uploads(request.layers)? {
            return Err(ViewerGpuExecutionError::Backpressure(
                "compact CPU YUV transfer preparation is still running".to_owned(),
            ));
        }
        let mut active_working_set =
            estimate_viewer_gpu_active_working_set(&request).map_err(|error| match error {
                ViewerGpuActiveWorkingSetEstimateError::InvalidHeterogeneousInput { reason } => {
                    ViewerGpuExecutionError::InvalidHeterogeneousInput { reason }
                }
                error => ViewerGpuExecutionError::ActiveWorkingSet(
                    ViewerGpuActiveWorkingSetAdmissionError::Estimate(error),
                ),
            })?;
        let (detached_presentation_textures, detached_presentation_bytes) = self
            .resource_pool
            .detached_presentation_demand()
            .ok_or(ViewerGpuExecutionError::ActiveWorkingSet(
                ViewerGpuActiveWorkingSetAdmissionError::Estimate(
                    ViewerGpuActiveWorkingSetEstimateError::ArithmeticOverflow {
                        stage: crate::ViewerGpuActiveWorkingSetStage::DetachedPresentations,
                    },
                ),
            ))?;
        active_working_set
            .include_presentation_residency(
                detached_presentation_textures,
                detached_presentation_bytes,
            )
            .map_err(|error| match error {
                ViewerGpuActiveWorkingSetEstimateError::PresentationCapacityExceeded {
                    live_outputs,
                } => ViewerGpuExecutionError::Backpressure(format!(
                    "capacity-one presentation owner still has {live_outputs} live outputs"
                )),
                error => ViewerGpuExecutionError::ActiveWorkingSet(
                    ViewerGpuActiveWorkingSetAdmissionError::Estimate(error),
                ),
            })?;
        self.resource_grant
            .admit_active_working_set(active_working_set)
            .map_err(ViewerGpuExecutionError::ActiveWorkingSet)?;
        self.last_active_working_set = Some(active_working_set);
        let input_prepare_started = Instant::now();
        let mut heterogeneous_inputs = std::mem::take(&mut request.heterogeneous_inputs)
            .into_iter()
            .map(Some)
            .collect::<Vec<_>>();
        let mut prepared = prepare_composite(
            &request,
            &mut heterogeneous_inputs,
            &mut self.color_output,
            &mut self.native_video_import,
            &self.cpu_yuv_upload,
            &self.working_compositor,
            &self.resource_pool,
            device,
            queue,
            encoder,
        )?;
        let input_prepare_us = elapsed_us(input_prepare_started);
        let working_composite_started = Instant::now();
        let (prepared_layers, mut compositing_diagnostics) = execute_prepared_composite_nodes(
            &mut prepared,
            &request,
            &self.working_compositor,
            &mut self.color_output,
            device,
            queue,
            encoder,
        )?;
        let gpu_layers = composite_layers(&prepared_layers, &prepared.gpu_input_handles);
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
                request.program_output_boundary.engine().clone(),
                RenderColorTransformGpuOptions::default(),
            )
            .map_err(|error| ViewerGpuExecutionError::WorkingComposite(Box::new(error)))?;
        validate_product_working_handle("composite", &composite.output)?;
        compositing_diagnostics.accumulate(composite.compositing_diagnostics);
        let residency = prepared.residency;
        let fallback_reasons = prepared.fallback_reasons;
        let mut stage_diagnostics = prepared.input_stage_diagnostics;
        stage_diagnostics.accumulate(composite.color_stage_diagnostics);
        let working_composite_us = elapsed_us(working_composite_started);
        mark_gpu_stage(
            &mut stage_marker,
            encoder,
            ViewerGpuExecutionGpuStage::WorkingComposite,
        )?;
        let spatial_started = Instant::now();
        let spatial_record = {
            let (frame_table, frame_ids) = self.color_output.frame_table_and_ids_mut();
            let working = frame_table.get(&composite.output).map_err(|error| {
                ViewerGpuExecutionError::WorkingOutputMissing(format!("{error:?}"))
            })?;
            self.spatial
                .record_for_presentation(
                    device,
                    encoder,
                    frame_ids,
                    working,
                    request.source_rect,
                    request.output_width,
                    request.output_height,
                )
                .map_err(|error| ViewerGpuExecutionError::Spatial(error.to_string()))?
        };
        let spatial_diagnostics = self.spatial.diagnostics();
        let spatial_output = spatial_record.output().clone();
        validate_product_working_handle("spatial", &spatial_output)?;
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
        let program_output_texture_format =
            if request.monitor_adaptation.requires_pass() || request.signal_monitoring.is_some() {
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
            .map_err(|error| ViewerGpuExecutionError::ProgramOutputBoundary(Box::new(error)))?;
        let program_output_boundary_us = elapsed_us(program_output_boundary_started);
        mark_gpu_stage(
            &mut stage_marker,
            encoder,
            ViewerGpuExecutionGpuStage::ProgramOutputBoundary,
        )?;
        stage_diagnostics.accumulate(program_output_record.stage_diagnostics);
        let program_output = program_output_record.materialized.output;
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
                .map_err(|error| ViewerGpuExecutionError::MonitorAdaptation(Box::new(error)))?;
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
        let program_scopes_started = Instant::now();
        let program_scopes = if let Some(scopes_request) = request.program_scopes {
            let scope_input = match scopes_request.tap() {
                ProgramScopesTap::ProgramOutput => &program_output,
                ProgramScopesTap::MonitorOutput => &output,
            };
            let scope_input_view = self
                .color_output
                .frame_table()
                .get(scope_input)
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
                        &scope_input_view,
                        request.output_width,
                        request.output_height,
                        scopes_request,
                    )
                    .map_err(|error| ViewerGpuExecutionError::ProgramScopes(Box::new(error)))?,
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
        let signal_monitoring_started = Instant::now();
        let (output, output_owner) = if let Some(monitoring) = request.signal_monitoring {
            let signal = match monitoring.tap {
                ProgramScopesTap::ProgramOutput => &program_output,
                ProgramScopesTap::MonitorOutput => &output,
            };
            let signal_resource = self.color_output.frame_table().get(signal).map_err(|error| {
                ViewerGpuExecutionError::ProgramOutputMissing(format!("{error:?}"))
            })?;
            let presentation_resource =
                self.color_output.frame_table().get(&output).map_err(|error| {
                    ViewerGpuExecutionError::DisplayOutputMissing(format!("{error:?}"))
                })?;
            let monitored = self
                .signal_monitor
                .record(
                    device,
                    queue,
                    encoder,
                    signal_resource,
                    presentation_resource,
                    monitoring,
                )
                .map_err(|error| ViewerGpuExecutionError::SignalMonitoring(Box::new(error)))?;
            (monitored, ViewerGpuExecutionOutputOwner::SignalMonitor)
        } else {
            (output, ViewerGpuExecutionOutputOwner::ColorOutput)
        };
        let signal_monitoring_us = elapsed_us(signal_monitoring_started);
        mark_gpu_stage(
            &mut stage_marker,
            encoder,
            ViewerGpuExecutionGpuStage::SignalMonitoring,
        )?;
        let calibration_started = Instant::now();
        let (output, output_owner) = if let Some(calibration) = request.display_calibration {
            let output_resource = match output_owner {
                ViewerGpuExecutionOutputOwner::ColorOutput => {
                    self.color_output.frame_table().get(&output).map_err(|error| {
                        ViewerGpuExecutionError::DisplayOutputMissing(format!("{error:?}"))
                    })?
                }
                ViewerGpuExecutionOutputOwner::SignalMonitor => {
                    self.signal_monitor.output(&output).ok_or_else(|| {
                        ViewerGpuExecutionError::DisplayOutputMissing(
                            "signal-monitoring output is missing".to_owned(),
                        )
                    })?
                }
                ViewerGpuExecutionOutputOwner::DisplayCalibration => {
                    unreachable!("display calibration cannot own output before its own stage")
                }
            };
            let calibrated = self
                .display_calibration
                .record(
                    device,
                    queue,
                    encoder,
                    output_resource,
                    calibration,
                    GpuColorFrameTextureFormat::Rgba16Float,
                )
                .map_err(|error| ViewerGpuExecutionError::Calibration(error.to_string()))?;
            (
                calibrated,
                ViewerGpuExecutionOutputOwner::DisplayCalibration,
            )
        } else {
            (output, output_owner)
        };
        let display_calibration_us = elapsed_us(calibration_started);
        let heterogeneous_continuations = std::mem::take(&mut prepared.heterogeneous_continuations);
        Ok(ViewerGpuExecutionRecord {
            program_output,
            program_scopes,
            output,
            working_float_decision: PRODUCT_GPU_WORKING_FLOAT_DECISION,
            output_owner: Some(output_owner),
            stage_diagnostics,
            compositing_diagnostics,
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
                signal_monitoring_us,
                display_calibration_us,
            },
            native_video_import_timing_receipt: None,
            heterogeneous_continuations,
        })
    }

    /// Move the exact recorded presentation output into an Adapter-owned lease.
    ///
    /// This is the only production ownership transfer for a Viewer output. The
    /// record's private owner authority is consumed before the concrete
    /// resource is detached, so a missing resource fails closed and the same
    /// record can never acquire a later allocation under a reused handle.
    ///
    /// Callers must submit the command buffer containing this record before
    /// publishing the lease. If a candidate is abandoned before submission,
    /// its encoder must be discarded before the lease is dropped.
    pub fn take_presentation_output(
        &mut self,
        record: &mut ViewerGpuExecutionRecord,
    ) -> Result<ViewerGpuPresentationOutputLease, ViewerGpuPresentationOutputTakeError> {
        let owner = record
            .output_owner
            .take()
            .ok_or(ViewerGpuPresentationOutputTakeError::AlreadyTaken { id: record.output.id() })?;
        let resource = match owner {
            ViewerGpuExecutionOutputOwner::ColorOutput => self
                .color_output
                .take_frame_resource(&record.output)
                .map_err(ViewerGpuPresentationOutputTakeError::ColorOutput)?,
            ViewerGpuExecutionOutputOwner::DisplayCalibration => {
                self.display_calibration.take_output(&record.output).ok_or(
                    ViewerGpuPresentationOutputTakeError::DisplayCalibrationMissing {
                        id: record.output.id(),
                    },
                )?
            }
            ViewerGpuExecutionOutputOwner::SignalMonitor => self
                .signal_monitor
                .take_output(&record.output)
                .ok_or(ViewerGpuPresentationOutputTakeError::SignalMonitorMissing {
                    id: record.output.id(),
                })?,
        };
        Ok(ViewerGpuPresentationOutputLease::new(
            resource,
            Arc::clone(&self.resource_pool),
        ))
    }

    /// Resolve the recorded presentation texture without exposing resource tables.
    ///
    /// This compatibility borrow is valid only before
    /// [`Self::take_presentation_output`]. Presentation Adapters should retain
    /// the move-only lease instead of a cloned view.
    pub fn output_texture_view(
        &self,
        record: &ViewerGpuExecutionRecord,
    ) -> Result<wgpu::TextureView, ViewerGpuExecutionError> {
        match record.output_owner {
            Some(ViewerGpuExecutionOutputOwner::ColorOutput) => self
                .color_output
                .frame_table()
                .get(&record.output)
                .map(|resource| resource.resource().texture_view.clone())
                .map_err(|error| {
                    ViewerGpuExecutionError::DisplayOutputMissing(format!("{error:?}"))
                }),
            Some(ViewerGpuExecutionOutputOwner::DisplayCalibration) => self
                .display_calibration
                .output(&record.output)
                .map(|resource| resource.resource().texture_view.clone())
                .ok_or_else(|| {
                    ViewerGpuExecutionError::DisplayOutputMissing(
                        "calibrated output disappeared before presentation".to_owned(),
                    )
                }),
            Some(ViewerGpuExecutionOutputOwner::SignalMonitor) => self
                .signal_monitor
                .output(&record.output)
                .map(|resource| resource.resource().texture_view.clone())
                .ok_or_else(|| {
                    ViewerGpuExecutionError::DisplayOutputMissing(
                        "signal-monitoring output disappeared before presentation".to_owned(),
                    )
                }),
            None => Err(ViewerGpuExecutionError::DisplayOutputMissing(
                "presentation output ownership was already transferred".to_owned(),
            )),
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
        self.signal_monitor.clear();
        self.program_scopes.clear();
        self.color_output.clear_frame_resources();
        self.cpu_yuv_upload.clear();
        self.resource_pool.invalidate();
        self.last_active_working_set = None;
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
    if boundary.output_color_space() != adaptation.program_output_color_space() {
        return Err(ViewerGpuExecutionError::ProgramMonitorBoundaryMismatch {
            program_boundary: boundary.output_color_space(),
            adaptation_input: adaptation.program_output_color_space(),
        });
    }
    Ok(())
}

fn validate_program_scopes_contract(
    boundary: &RenderOutputColorBoundary,
    adaptation: &RenderMonitorAdaptation,
    request: Option<GpuProgramScopesRequest>,
) -> Result<(), ViewerGpuExecutionError> {
    let Some(request) = request else {
        return Ok(());
    };
    let expected = match request.tap() {
        ProgramScopesTap::ProgramOutput => boundary.output_color_space(),
        ProgramScopesTap::MonitorOutput => adaptation.monitor_color_space(),
    };
    if expected != request.signal_color_space() {
        return Err(ViewerGpuExecutionError::ProgramScopesBoundaryMismatch {
            tap: request.tap(),
            expected_signal: expected,
            scopes_signal: request.signal_color_space(),
        });
    }
    Ok(())
}

fn validate_signal_monitor_contract(
    boundary: &RenderOutputColorBoundary,
    adaptation: &RenderMonitorAdaptation,
    request: Option<GpuSignalMonitorRequest>,
) -> Result<(), ViewerGpuExecutionError> {
    let Some(request) = request else {
        return Ok(());
    };
    let request = request
        .validate()
        .map_err(|error| ViewerGpuExecutionError::SignalMonitoring(Box::new(error)))?;
    let expected = match request.tap {
        ProgramScopesTap::ProgramOutput => boundary.output_color_space(),
        ProgramScopesTap::MonitorOutput => adaptation.monitor_color_space(),
    };
    if expected != request.compliance.signal_color_space {
        return Err(ViewerGpuExecutionError::SignalMonitoringBoundaryMismatch {
            tap: request.tap,
            expected_signal: expected,
            monitoring_signal: request.compliance.signal_color_space,
        });
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
    /// Renderer output handle whose private ownership authority can be consumed once.
    pub output: GpuColorFrameHandle,
    /// Product working-format decision shared with prepared Export execution.
    pub working_float_decision: GpuWorkingFloatDecision,
    output_owner: Option<ViewerGpuExecutionOutputOwner>,
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
    native_video_import_timing_receipt: Option<NativeVideoImportCandidateTimingReceipt>,
    heterogeneous_continuations: Vec<HeterogeneousGpuRecordedContinuation>,
}

impl ViewerGpuExecutionRecord {
    /// Move the exact native-import candidate receipt out at most once.
    ///
    /// `None` means timing was inactive/unsupported, the record did not retain
    /// a receipt, or a prior call already consumed it. It is not permission to
    /// infer expected sample coverage from layer counts.
    pub fn take_native_video_import_timing_receipt(
        &mut self,
    ) -> Option<NativeVideoImportCandidateTimingReceipt> {
        self.native_video_import_timing_receipt.take()
    }

    /// Number of CPU-prefix → GPU-suffix continuations recorded into the same
    /// caller-owned encoder as this Viewer frame.
    pub fn heterogeneous_continuation_count(&self) -> usize {
        self.heterogeneous_continuations.len()
    }

    /// Bind every recorded heterogeneous continuation to the exact Adapter
    /// submission containing this Viewer frame.
    ///
    /// Call this once, immediately after `Queue::submit`, before either
    /// publishing the candidate or clearing the Viewer runtime.
    pub fn assert_adapter_submission(
        &mut self,
        submission_index: wgpu::SubmissionIndex,
    ) -> ViewerHeterogeneousGpuSubmissionBatch {
        let continuations = std::mem::take(&mut self.heterogeneous_continuations)
            .into_iter()
            .map(|continuation| continuation.assert_adapter_submission(submission_index.clone()))
            .collect();
        ViewerHeterogeneousGpuSubmissionBatch { continuations }
    }
}

/// Heterogeneous parts of one Viewer frame bound to its exact queue
/// submission.
///
/// This value proves submission, not completion. Window adapters register its
/// non-blocking callback; Headless adapters may wait for the exact submission.
pub struct ViewerHeterogeneousGpuSubmissionBatch {
    continuations: Vec<HeterogeneousGpuSubmittedContinuation>,
}

impl ViewerHeterogeneousGpuSubmissionBatch {
    /// Whether this Viewer frame contained no heterogeneous continuation.
    pub fn is_empty(&self) -> bool {
        self.continuations.is_empty()
    }

    /// Number of continuations covered by the same submission.
    pub fn len(&self) -> usize {
        self.continuations.len()
    }

    /// Wait for the exact Viewer submission before one non-renewing monotonic
    /// deadline and produce completion evidence for every continuation.
    ///
    /// Every continuation in this batch is bound to the same Adapter
    /// submission. Observing the first continuation's submission therefore
    /// proves completion for the whole batch; the remaining values must not
    /// renew the deadline with additional waits.
    pub fn wait_until(
        self,
        device: &wgpu::Device,
        deadline: std::time::Instant,
    ) -> Result<ViewerHeterogeneousGpuCompletedBatch, HeterogeneousGpuContinuationError> {
        let mut continuations = self.continuations.into_iter();
        let Some(first) = continuations.next() else {
            return Ok(ViewerHeterogeneousGpuCompletedBatch { continuations: Vec::new() });
        };
        let mut completed = Vec::with_capacity(continuations.len().saturating_add(1));
        completed.push(first.wait_until(device, deadline)?);
        completed.extend(
            continuations
                .map(HeterogeneousGpuSubmittedContinuation::complete_after_observed_submission),
        );
        Ok(ViewerHeterogeneousGpuCompletedBatch { continuations: completed })
    }

    /// Register one short queue-completion callback for this whole Viewer
    /// submission.
    ///
    /// The callback is driven by a later `Queue::submit` or poll call and must
    /// only hand the completed batch to an Adapter inbox. It must not perform
    /// publication or other long-running UI work.
    pub fn register_completion_callback(
        self,
        queue: &wgpu::Queue,
        callback: impl FnOnce(ViewerHeterogeneousGpuCompletedBatch) + Send + 'static,
    ) {
        queue.on_submitted_work_done(move || {
            let continuations = self
                .continuations
                .into_iter()
                .map(HeterogeneousGpuSubmittedContinuation::complete_after_observed_submission)
                .collect();
            callback(ViewerHeterogeneousGpuCompletedBatch { continuations });
        });
    }
}

/// Exact completion evidence for every heterogeneous continuation in one
/// Viewer candidate.
#[derive(Default)]
pub struct ViewerHeterogeneousGpuCompletedBatch {
    continuations: Vec<HeterogeneousGpuCompletedContinuation>,
}

impl ViewerHeterogeneousGpuCompletedBatch {
    /// Completed continuations in deterministic layer-recording order.
    pub fn continuations(&self) -> &[HeterogeneousGpuCompletedContinuation] {
        &self.continuations
    }

    /// Number of completed continuations.
    pub fn len(&self) -> usize {
        self.continuations.len()
    }

    /// Whether the batch contains no heterogeneous work.
    pub fn is_empty(&self) -> bool {
        self.continuations.is_empty()
    }
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
    /// Optional fused false-color/zebra/gamut-alarm command preparation.
    pub signal_monitoring_us: u64,
    /// Optional display-calibration command preparation.
    pub display_calibration_us: u64,
}

fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewerGpuExecutionOutputOwner {
    ColorOutput,
    SignalMonitor,
    DisplayCalibration,
}

/// Fail-closed ownership-transfer error for one recorded Viewer output.
#[derive(Debug, thiserror::Error)]
pub enum ViewerGpuPresentationOutputTakeError {
    /// This record's private output authority was already consumed.
    #[error("Viewer presentation output {id:?} was already taken")]
    AlreadyTaken {
        /// Renderer resource identity that cannot be transferred twice.
        id: GpuColorFrameId,
    },
    /// The color-output owner no longer held the record's exact typed resource.
    #[error("Viewer color output could not be detached: {0}")]
    ColorOutput(#[source] GpuColorFrameResourceTableError),
    /// The calibration owner no longer held the record's exact typed resource.
    #[error("Viewer display-calibration output {id:?} is missing")]
    DisplayCalibrationMissing {
        /// Renderer resource identity expected from display calibration.
        id: GpuColorFrameId,
    },
    /// The signal-monitoring owner no longer held the exact output.
    #[error("Viewer signal-monitoring output {id:?} is missing")]
    SignalMonitorMissing { id: GpuColorFrameId },
}

/// Stage-specific failures from the shared Viewer GPU execution Interface.
#[derive(Debug, thiserror::Error)]
pub enum ViewerGpuExecutionError {
    /// The complete request could not fit this owner's pressure-stable active
    /// texture grant. This is returned before any frame texture is created.
    #[error(transparent)]
    ActiveWorkingSet(#[from] ViewerGpuActiveWorkingSetAdmissionError),
    /// Bounded native/GPU resources are still in flight. The presentation
    /// adapter should retain its current output and retry or discard this candidate.
    #[error("Viewer GPU execution is backpressured: {0}")]
    Backpressure(String),
    #[error("Viewer GPU input preparation failed: {0}")]
    InputPreparation(String),
    /// A move-only CPU-prefix completion was absent, duplicated, ambiguous, or
    /// bound to a different Viewer frame.
    #[error("Viewer heterogeneous GPU input is invalid: {reason}")]
    InvalidHeterogeneousInput {
        /// Stable rejected invariant.
        reason: &'static str,
    },
    /// Recording the exact GPU suffix failed. Once this route is selected the
    /// Viewer must discard the candidate; it may not reinterpret the whole
    /// Effect graph on CPU.
    #[error("Viewer heterogeneous GPU continuation failed: {0}")]
    HeterogeneousContinuation(#[source] Box<HeterogeneousGpuContinuationError>),
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
    /// Scope signal identity did not match the selected Viewer tap.
    #[error("Viewer scopes tap {tap:?} expects {expected_signal:?}, not {scopes_signal:?}")]
    ProgramScopesBoundaryMismatch {
        tap: ProgramScopesTap,
        expected_signal: mondrian_core::types::ColorSpace,
        scopes_signal: mondrian_core::types::ColorSpace,
    },
    /// Monitoring signal identity did not match its selected Viewer tap.
    #[error(
        "Viewer monitoring tap {tap:?} expects {expected_signal:?}, not {monitoring_signal:?}"
    )]
    SignalMonitoringBoundaryMismatch {
        tap: ProgramScopesTap,
        expected_signal: mondrian_core::types::ColorSpace,
        monitoring_signal: mondrian_core::types::ColorSpace,
    },
    #[error("Viewer GPU working composite graph failed: {0:?}")]
    WorkingComposite(Box<RenderGpuCompositeGraphRecordError>),
    /// A typed two-input Transition could not be materialized exactly.
    #[error("Viewer GPU Transition execution failed: {0}")]
    Transition(#[source] Box<crate::GpuCompositeError>),
    #[error("Viewer GPU effect-domain processing failed: {0}")]
    EffectDomain(String),
    #[error("Viewer GPU working output is missing: {0}")]
    WorkingOutputMissing(String),
    /// Actual GPU working storage disagreed with the policy used for admission.
    #[error("Viewer GPU {stage} working output must use {expected:?}, got {actual:?}")]
    WorkingFloatPolicyMismatch {
        /// Stable working stage identity.
        stage: &'static str,
        /// Policy-selected format.
        expected: GpuColorFrameTextureFormat,
        /// Actual recorded output format.
        actual: GpuColorFrameTextureFormat,
    },
    #[error("Viewer GPU spatial processing failed: {0}")]
    Spatial(String),
    #[error("Viewer GPU spatial output disappeared before the display boundary")]
    SpatialOutputMissing,
    #[error("Viewer GPU spatial resource transfer failed: {0}")]
    SpatialTransfer(String),
    #[error("Viewer GPU Program Output boundary failed: {0:?}")]
    ProgramOutputBoundary(Box<RenderGpuOutputBoundaryRuntimeRecordError>),
    #[error("Viewer GPU Program Output scopes failed: {0}")]
    ProgramScopes(#[source] Box<GpuProgramScopesError>),
    /// False-color/zebra/gamut monitoring failed without changing Program Output.
    #[error("Viewer GPU signal monitoring failed: {0}")]
    SignalMonitoring(#[source] Box<GpuSignalMonitorError>),
    #[error("Viewer GPU monitor adaptation failed: {0:?}")]
    MonitorAdaptation(Box<RenderGpuColorTransformRuntimeRecordError>),
    #[error("Viewer GPU Program Output is missing: {0}")]
    ProgramOutputMissing(String),
    #[error("Viewer GPU display output is missing: {0}")]
    DisplayOutputMissing(String),
    #[error("Viewer GPU display calibration failed: {0}")]
    Calibration(String),
    #[error("Viewer GPU profiling stage marker failed: {0}")]
    StageMarker(String),
}

impl ViewerGpuExecutionError {
    /// Return the stable compositor blocker represented by a working-graph failure.
    pub fn working_composite_blocker(&self) -> Option<crate::GpuCompositingBlockerReason> {
        let Self::WorkingComposite(error) = self else {
            return None;
        };
        match error.as_ref() {
            RenderGpuCompositeGraphRecordError::Composite(crate::GpuCompositeError::Blocked {
                reason,
            }) => Some(*reason),
            _ => Some(crate::GpuCompositingBlockerReason::GpuUnavailable),
        }
    }

    /// Whether failure occurred while recording the Program Output Module.
    pub const fn is_program_output_failure(&self) -> bool {
        matches!(self, Self::ProgramOutputBoundary(_))
    }

    /// Return deterministic native Program Output blockers without exposing stage IR.
    pub fn program_output_blocker_breakdown(
        &self,
    ) -> Option<crate::color_stage::RenderColorStageGpuBlockerBreakdown> {
        let Self::ProgramOutputBoundary(error) = self else {
            return None;
        };
        match error.as_ref() {
            RenderGpuOutputBoundaryRuntimeRecordError::ResourcePlan(
                crate::color_stage::RenderGpuOutputStageResourcePlanError::NativeBlockersRemaining {
                    breakdown,
                    ..
                },
            ) => Some(*breakdown),
            _ => None,
        }
    }
}

fn validate_product_working_handle(
    stage: &'static str,
    handle: &GpuColorFrameHandle,
) -> Result<(), ViewerGpuExecutionError> {
    let expected = PRODUCT_GPU_WORKING_FLOAT_DECISION.format().texture_format();
    let actual = handle.texture_format();
    if actual != expected {
        return Err(ViewerGpuExecutionError::WorkingFloatPolicyMismatch {
            stage,
            expected,
            actual,
        });
    }
    Ok(())
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
    heterogeneous_continuations: Vec<HeterogeneousGpuRecordedContinuation>,
    nodes: Vec<PreparedCompositeNode<'a>>,
    residency: ViewerGpuExecutionResidency,
    input_stage_diagnostics: RenderColorStageDiagnostics,
    pre_compositing_diagnostics: GpuCompositingDiagnostics,
    fallback_reasons: Vec<String>,
}

#[derive(Clone, Copy)]
struct PreparedCompositeLayer<'a> {
    source: PreparedCompositeLayerSource<'a>,
    opacity: f32,
    blend_mode: BlendMode,
    transform: [f32; 6],
    effect_plan: Option<&'a mondrian_effects::CompiledEffectGpuPlan>,
    frame_seed: i64,
}

#[derive(Clone, Copy)]
enum PreparedCompositeLayerSource<'a> {
    CpuFrame(&'a CpuColorFrame),
    CpuDataTexture(&'a CpuColorFrame),
    GpuFrame(usize),
    SolidColor(Color),
    Adjustment,
}

enum PreparedCompositeNode<'a> {
    Layer(PreparedCompositeLayer<'a>),
    CrossDissolve {
        left: Option<PreparedCompositeLayer<'a>>,
        right: Option<PreparedCompositeLayer<'a>>,
        progress: f32,
    },
}

fn prepare_composite<'a>(
    request: &ViewerGpuExecutionRequest<'a>,
    heterogeneous_inputs: &mut [Option<ViewerHeterogeneousGpuInput>],
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    native_runtime: &mut ViewerNativeVideoImportRuntime,
    cpu_yuv_upload: &crate::cpu_yuv::CpuYuvUploadRuntime,
    compositor: &GpuFrameCompositor,
    resource_pool: &Arc<GpuColorFrameWgpuResourcePool>,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
) -> Result<PreparedComposite<'a>, ViewerGpuExecutionError> {
    let mut prepared = PreparedComposite {
        gpu_input_handles: Vec::new(),
        heterogeneous_continuations: Vec::new(),
        nodes: Vec::with_capacity(request.layers.len()),
        residency: ViewerGpuExecutionResidency::default(),
        input_stage_diagnostics: RenderColorStageDiagnostics::default(),
        pre_compositing_diagnostics: GpuCompositingDiagnostics::default(),
        fallback_reasons: Vec::new(),
    };

    for node in request.layers {
        match node {
            ViewerGpuExecutionLayer::Source(source) => {
                if source_layer_has_zero_contribution(source) {
                    continue;
                }
                let layer = prepare_source_layer(
                    source,
                    request,
                    heterogeneous_inputs,
                    &mut prepared,
                    runtime,
                    native_runtime,
                    cpu_yuv_upload,
                    compositor,
                    resource_pool,
                    device,
                    queue,
                    encoder,
                )?;
                prepared.nodes.push(PreparedCompositeNode::Layer(layer));
            }
            ViewerGpuExecutionLayer::Adjustment {
                effect_plan,
                opacity,
                blend_mode,
                frame_seed,
            } => {
                if opacity.clamp(0.0, 1.0) == 0.0 {
                    continue;
                }
                prepared.residency.procedural_layers =
                    prepared.residency.procedural_layers.saturating_add(1);
                prepared.nodes.push(PreparedCompositeNode::Layer(PreparedCompositeLayer {
                    source: PreparedCompositeLayerSource::Adjustment,
                    opacity: *opacity,
                    blend_mode: *blend_mode,
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_plan: Some(effect_plan),
                    frame_seed: *frame_seed,
                }));
            }
            ViewerGpuExecutionLayer::CrossDissolve(transition) => {
                let crate::ViewerGpuCrossDissolveLayer { left, right, progress } =
                    transition.as_ref();
                if !progress.is_finite() {
                    return Err(ViewerGpuExecutionError::Transition(Box::new(
                        crate::GpuCompositeError::NonFiniteTransitionProgress {
                            progress_bits: progress.to_bits(),
                        },
                    )));
                }
                let progress = progress.clamp(0.0, 1.0);
                let left = prepare_transition_input(
                    left,
                    1.0 - progress,
                    request,
                    heterogeneous_inputs,
                    &mut prepared,
                    runtime,
                    native_runtime,
                    cpu_yuv_upload,
                    compositor,
                    resource_pool,
                    device,
                    queue,
                    encoder,
                )?;
                let right = prepare_transition_input(
                    right,
                    progress,
                    request,
                    heterogeneous_inputs,
                    &mut prepared,
                    runtime,
                    native_runtime,
                    cpu_yuv_upload,
                    compositor,
                    resource_pool,
                    device,
                    queue,
                    encoder,
                )?;
                if left.is_some() || right.is_some() {
                    prepared.nodes.push(PreparedCompositeNode::CrossDissolve {
                        left,
                        right,
                        progress,
                    });
                }
            }
        }
    }

    Ok(prepared)
}

#[allow(clippy::too_many_arguments)]
fn prepare_transition_input<'a>(
    input: &'a crate::ViewerGpuTransitionInput,
    weight: f32,
    request: &ViewerGpuExecutionRequest<'a>,
    heterogeneous_inputs: &mut [Option<ViewerHeterogeneousGpuInput>],
    prepared: &mut PreparedComposite<'a>,
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    native_runtime: &mut ViewerNativeVideoImportRuntime,
    cpu_yuv_upload: &crate::cpu_yuv::CpuYuvUploadRuntime,
    compositor: &GpuFrameCompositor,
    resource_pool: &Arc<GpuColorFrameWgpuResourcePool>,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
) -> Result<Option<PreparedCompositeLayer<'a>>, ViewerGpuExecutionError> {
    if weight <= 0.0 {
        return Ok(None);
    }
    match input {
        crate::ViewerGpuTransitionInput::Transparent => Ok(None),
        crate::ViewerGpuTransitionInput::Source(source)
            if source_layer_has_zero_contribution(source) =>
        {
            Ok(None)
        }
        crate::ViewerGpuTransitionInput::Source(source) => prepare_source_layer(
            source,
            request,
            heterogeneous_inputs,
            prepared,
            runtime,
            native_runtime,
            cpu_yuv_upload,
            compositor,
            resource_pool,
            device,
            queue,
            encoder,
        )
        .map(Some),
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_source_layer<'a>(
    source_layer: &'a crate::ViewerGpuSourceLayer,
    request: &ViewerGpuExecutionRequest<'a>,
    heterogeneous_inputs: &mut [Option<ViewerHeterogeneousGpuInput>],
    prepared: &mut PreparedComposite<'a>,
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    native_runtime: &mut ViewerNativeVideoImportRuntime,
    cpu_yuv_upload: &crate::cpu_yuv::CpuYuvUploadRuntime,
    compositor: &GpuFrameCompositor,
    resource_pool: &Arc<GpuColorFrameWgpuResourcePool>,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
) -> Result<PreparedCompositeLayer<'a>, ViewerGpuExecutionError> {
    match source_layer {
        crate::ViewerGpuSourceLayer::Media {
            frame,
            is_data_texture,
            gpu_source,
            native_source,
            cpu_yuv_source,
            heterogeneous_input,
            opacity,
            blend_mode,
            transform,
            effect_plan,
            frame_seed,
        } => {
            prepared.residency.media_layers = prepared.residency.media_layers.saturating_add(1);
            if *is_data_texture
                && (frame.is_none()
                    || gpu_source.is_some()
                    || native_source.is_some()
                    || cpu_yuv_source.is_some()
                    || heterogeneous_input.is_some())
            {
                return Err(ViewerGpuExecutionError::InputPreparation(
                    "DataTexture media must provide exactly one typed CPU numeric payload"
                        .to_owned(),
                ));
            }
            if let Some(address) = heterogeneous_input {
                if frame.is_some()
                    || gpu_source.is_some()
                    || native_source.is_some()
                    || cpu_yuv_source.is_some()
                {
                    return Err(ViewerGpuExecutionError::InvalidHeterogeneousInput {
                        reason: "heterogeneous media source is not exclusive",
                    });
                }
                if !effect_plan.node_ids().is_empty() {
                    return Err(ViewerGpuExecutionError::InvalidHeterogeneousInput {
                        reason: "heterogeneous media source retained a second Effect plan",
                    });
                }
                let address = usize::try_from(*address).map_err(|_| {
                    ViewerGpuExecutionError::InvalidHeterogeneousInput {
                        reason: "heterogeneous media input address is not representable",
                    }
                })?;
                let input = heterogeneous_inputs.get_mut(address).and_then(Option::take).ok_or(
                    ViewerGpuExecutionError::InvalidHeterogeneousInput {
                        reason: "heterogeneous media input address is missing or duplicated",
                    },
                )?;
                let binding = input.request.binding();
                if binding.working_color_space() != request.working_color_space
                    || binding.frame_seed() != *frame_seed
                {
                    return Err(ViewerGpuExecutionError::InvalidHeterogeneousInput {
                        reason:
                            "heterogeneous media input does not match the Viewer working contract",
                    });
                }
                let recorded = {
                    let (frame_table, frame_ids) = runtime.frame_table_and_ids_mut();
                    HeterogeneousGpuRecordResources::new(
                        device,
                        queue,
                        encoder,
                        frame_ids,
                        frame_table,
                        compositor,
                        Some(resource_pool),
                    )
                    .record(input.request, input.completion)
                    .map_err(|error| {
                        ViewerGpuExecutionError::HeterogeneousContinuation(Box::new(error))
                    })?
                };
                let index = prepared.gpu_input_handles.len();
                prepared.gpu_input_handles.push(recorded.output().clone());
                prepared.heterogeneous_continuations.push(recorded);
                prepared.residency.gpu_input_layers =
                    prepared.residency.gpu_input_layers.saturating_add(1);
                return Ok(PreparedCompositeLayer {
                    source: PreparedCompositeLayerSource::GpuFrame(index),
                    opacity: *opacity,
                    blend_mode: *blend_mode,
                    transform: *transform,
                    effect_plan: None,
                    frame_seed: *frame_seed,
                });
            }
            prepared.residency.record_source(
                gpu_source.as_ref(),
                native_source.as_ref(),
                cpu_yuv_source.as_ref(),
            );
            let cpu_yuv_handle = match cpu_yuv_source.as_ref() {
                Some(source) => Some(record_cpu_yuv_video_layer(
                    source,
                    native_runtime,
                    cpu_yuv_upload,
                    runtime,
                    device,
                    queue,
                    encoder,
                )?),
                None => None,
            };
            let mut native_import_error = None;
            let native_handle = match native_source.as_ref() {
                Some(source) => match record_native_video_layer(source, native_runtime, runtime) {
                    Ok(handle) => Some(handle),
                    Err(error) if error.is_backpressure() => {
                        return Err(ViewerGpuExecutionError::Backpressure(error.to_string()));
                    }
                    Err(error) => {
                        prepared.residency.gpu_input_failures =
                            prepared.residency.gpu_input_failures.saturating_add(1);
                        prepared
                            .fallback_reasons
                            .push(format!("viewer native video import failed: {error}"));
                        runtime.log_persistent_failure_warn(|| {
                            format!(
                                "viewer native video import failed: {error} (sequence_id={:?}, frame={})",
                                request.sequence_id, request.timeline_frame
                            )
                        });
                        native_import_error = Some(error.to_string());
                        None
                    }
                },
                None => None,
            };
            let source = if let Some(handle) = cpu_yuv_handle.or(native_handle) {
                let index = prepared.gpu_input_handles.len();
                prepared.gpu_input_handles.push(handle);
                if cpu_yuv_source.is_some() {
                    prepared.residency.gpu_input_layers =
                        prepared.residency.gpu_input_layers.saturating_add(1);
                }
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
                                prepared
                                    .fallback_reasons
                                    .push(format!("viewer GPU input transform failed: {error:?}"));
                                if let Some(frame) = frame.as_ref() {
                                    prepared.residency.cpu_upload_layers =
                                        prepared.residency.cpu_upload_layers.saturating_add(1);
                                    runtime.log_persistent_failure_warn(|| {
                                        format!(
                                            "viewer GPU input transform failed; using CPU working layer upload: {error:?} (sequence_id={:?}, frame={})",
                                            request.sequence_id, request.timeline_frame
                                        )
                                    });
                                    if *is_data_texture {
                                        PreparedCompositeLayerSource::CpuDataTexture(frame)
                                    } else {
                                        PreparedCompositeLayerSource::CpuFrame(frame)
                                    }
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
                        if *is_data_texture {
                            PreparedCompositeLayerSource::CpuDataTexture(frame)
                        } else {
                            PreparedCompositeLayerSource::CpuFrame(frame)
                        }
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
                    PreparedCompositeLayerSource::CpuDataTexture(frame) => {
                        let layer = GpuCompositeLayer {
                            source: GpuCompositeLayerSource::CpuDataTexture(frame),
                            opacity: 1.0,
                            blend_mode: BlendMode::Normal,
                            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                            effect_plan: None,
                            frame_seed: *frame_seed,
                        };
                        let descriptor = frame.descriptor();
                        let materialized = runtime
                            .record_wgpu_working_composite(
                                compositor,
                                device,
                                queue,
                                encoder,
                                GpuCompositeRequest {
                                    width: descriptor.width,
                                    height: descriptor.height,
                                    working_color_space: request.working_color_space,
                                    layers: std::slice::from_ref(&layer),
                                },
                            )
                            .map_err(|error| {
                                ViewerGpuExecutionError::EffectDomain(format!(
                                    "DataTexture numeric bypass failed: {error:?}"
                                ))
                            })?;
                        prepared.pre_compositing_diagnostics.accumulate(materialized.diagnostics);
                        materialized.output
                    }
                    PreparedCompositeLayerSource::SolidColor(_)
                    | PreparedCompositeLayerSource::Adjustment => {
                        return Err(ViewerGpuExecutionError::EffectDomain(
                            "media effect received a non-media prepared source".to_owned(),
                        ));
                    }
                };
                let index = record_external_domain_effect(
                    prepared,
                    runtime,
                    compositor,
                    effect_plan,
                    input,
                    request.program_output_boundary.engine().clone(),
                    *frame_seed,
                    device,
                    queue,
                    encoder,
                )?;
                (PreparedCompositeLayerSource::GpuFrame(index), None)
            };
            Ok(PreparedCompositeLayer {
                source,
                opacity: *opacity,
                blend_mode: *blend_mode,
                transform: *transform,
                effect_plan,
                frame_seed: *frame_seed,
            })
        }
        crate::ViewerGpuSourceLayer::SolidColor { layer, effect_plan } => {
            prepared.residency.procedural_layers =
                prepared.residency.procedural_layers.saturating_add(1);
            let scene_linear = effect_plan.processing_domain() == EffectColorDomain::SceneLinearRgb;
            if scene_linear {
                Ok(PreparedCompositeLayer {
                    source: PreparedCompositeLayerSource::SolidColor(layer.color),
                    opacity: layer.opacity,
                    blend_mode: layer.blend_mode,
                    transform: layer.transform,
                    effect_plan: Some(effect_plan),
                    frame_seed: layer.frame_seed,
                })
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
                let index = record_external_domain_effect(
                    prepared,
                    runtime,
                    compositor,
                    effect_plan,
                    materialized.output,
                    request.program_output_boundary.engine().clone(),
                    layer.frame_seed,
                    device,
                    queue,
                    encoder,
                )?;
                Ok(PreparedCompositeLayer {
                    source: PreparedCompositeLayerSource::GpuFrame(index),
                    opacity: layer.opacity,
                    blend_mode: layer.blend_mode,
                    transform: layer.transform,
                    effect_plan: None,
                    frame_seed: layer.frame_seed,
                })
            }
        }
    }
}

fn source_layer_has_zero_contribution(layer: &crate::ViewerGpuSourceLayer) -> bool {
    let opacity = match layer {
        crate::ViewerGpuSourceLayer::Media { opacity, .. } => *opacity,
        crate::ViewerGpuSourceLayer::SolidColor { layer, .. } => layer.opacity,
    };
    opacity.clamp(0.0, 1.0) == 0.0
}

fn prepare_source_cpu_yuv_upload(
    layer: &crate::ViewerGpuSourceLayer,
    uploads: &crate::cpu_yuv::CpuYuvUploadRuntime,
    seen: &mut Vec<usize>,
    all_ready: &mut bool,
) -> Result<(), ViewerGpuExecutionError> {
    if source_layer_has_zero_contribution(layer) {
        return Ok(());
    }
    let crate::ViewerGpuSourceLayer::Media { cpu_yuv_source: Some(source), .. } = layer else {
        return Ok(());
    };
    let identity = Arc::as_ptr(&source.frame) as usize;
    if seen.contains(&identity) {
        return Ok(());
    }
    seen.push(identity);
    *all_ready &= uploads.prepare(&source.frame).map_err(|error| match error {
        crate::cpu_yuv::CpuYuvMaterializationError::UploadPending => {
            ViewerGpuExecutionError::Backpressure(
                "compact CPU YUV transfer preparation is still running".to_owned(),
            )
        }
        error => ViewerGpuExecutionError::InputPreparation(format!(
            "compact CPU YUV upload preparation failed: {error}"
        )),
    })?;
    Ok(())
}

fn prepare_source_native_video_import(
    layer: &crate::ViewerGpuSourceLayer,
    runtime: &mut crate::ViewerNativeVideoImportRuntime,
    seen: &mut Vec<usize>,
) -> Result<(), ViewerGpuExecutionError> {
    if source_layer_has_zero_contribution(layer) {
        return Ok(());
    }
    let crate::ViewerGpuSourceLayer::Media { native_source: Some(source), .. } = layer else {
        return Ok(());
    };
    let identity = Arc::as_ptr(&source.native_frame) as usize;
    if seen.contains(&identity) {
        return Ok(());
    }
    seen.push(identity);
    runtime
        .prepare_import_backend_objects(
            source.source_color_space,
            &source.input_transform,
            source.materialization_width,
            source.materialization_height,
            &source.native_frame,
        )
        .map_err(|error| {
            ViewerGpuExecutionError::InputPreparation(format!(
                "native video input backend preparation failed: {error}"
            ))
        })
}

#[allow(clippy::too_many_arguments)]
fn execute_prepared_composite_nodes<'a>(
    prepared: &mut PreparedComposite<'a>,
    request: &ViewerGpuExecutionRequest<'a>,
    compositor: &GpuFrameCompositor,
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
) -> Result<(Vec<PreparedCompositeLayer<'a>>, GpuCompositingDiagnostics), ViewerGpuExecutionError> {
    let nodes = std::mem::take(&mut prepared.nodes);
    let mut layers = Vec::with_capacity(nodes.len());
    let mut diagnostics = std::mem::take(&mut prepared.pre_compositing_diagnostics);

    for node in nodes {
        match node {
            PreparedCompositeNode::Layer(layer) => layers.push(layer),
            PreparedCompositeNode::CrossDissolve { left, right, progress } => {
                let left = materialize_transition_source(
                    left,
                    prepared,
                    request,
                    compositor,
                    runtime,
                    device,
                    queue,
                    encoder,
                    &mut diagnostics,
                )?;
                let right = materialize_transition_source(
                    right,
                    prepared,
                    request,
                    compositor,
                    runtime,
                    device,
                    queue,
                    encoder,
                    &mut diagnostics,
                )?;

                let (output, opacity) = match (left, right) {
                    (None, None) => continue,
                    (Some(left), None) => (left, 1.0 - progress),
                    (None, Some(right)) => (right, progress),
                    (Some(left), Some(right)) => {
                        let record = runtime
                            .record_wgpu_cross_dissolve(
                                compositor,
                                device,
                                queue,
                                encoder,
                                &left,
                                &right,
                                progress,
                                request.working_color_space,
                            )
                            .map_err(|error| {
                                ViewerGpuExecutionError::Transition(Box::new(error))
                            })?;
                        diagnostics.accumulate(record.diagnostics);
                        (record.output, 1.0)
                    }
                };
                layers.push(push_prepared_gpu_layer(prepared, output, opacity));
            }
        }
    }

    Ok((layers, diagnostics))
}

#[allow(clippy::too_many_arguments)]
fn materialize_transition_source<'a>(
    layer: Option<PreparedCompositeLayer<'a>>,
    prepared: &PreparedComposite<'a>,
    request: &ViewerGpuExecutionRequest<'a>,
    compositor: &GpuFrameCompositor,
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
    diagnostics: &mut GpuCompositingDiagnostics,
) -> Result<Option<GpuColorFrameHandle>, ViewerGpuExecutionError> {
    let Some(layer) = layer else {
        return Ok(None);
    };
    let gpu_layer = composite_layer(&layer, &prepared.gpu_input_handles);
    let record = runtime
        .record_wgpu_working_composite(
            compositor,
            device,
            queue,
            encoder,
            GpuCompositeRequest {
                width: request.width,
                height: request.height,
                working_color_space: request.working_color_space,
                layers: std::slice::from_ref(&gpu_layer),
            },
        )
        .map_err(|error| ViewerGpuExecutionError::Transition(Box::new(error)))?;
    diagnostics.accumulate(record.diagnostics);
    Ok(Some(record.output))
}

fn push_prepared_gpu_layer<'a>(
    prepared: &mut PreparedComposite<'a>,
    output: GpuColorFrameHandle,
    opacity: f32,
) -> PreparedCompositeLayer<'a> {
    let index = prepared.gpu_input_handles.len();
    prepared.gpu_input_handles.push(output);
    PreparedCompositeLayer {
        source: PreparedCompositeLayerSource::GpuFrame(index),
        opacity,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_plan: None,
        frame_seed: 0,
    }
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
        source.materialization_width,
        source.materialization_height,
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

#[allow(clippy::too_many_arguments)]
fn record_cpu_yuv_video_layer(
    source: &crate::ViewerGpuCpuYuvSource,
    native_runtime: &mut ViewerNativeVideoImportRuntime,
    cpu_yuv_upload: &crate::cpu_yuv::CpuYuvUploadRuntime,
    color_runtime: &mut RenderGpuOutputBoundaryRuntime,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
) -> Result<GpuColorFrameHandle, ViewerGpuExecutionError> {
    crate::cpu_yuv::record_cpu_yuv_frame(
        &native_runtime.cpu_yuv_decoder,
        cpu_yuv_upload,
        &source.frame,
        &source.input_transform,
        source.materialization_width,
        source.materialization_height,
        color_runtime,
        device,
        queue,
        encoder,
    )
    .map_err(|error| match error {
        crate::cpu_yuv::CpuYuvMaterializationError::UploadPending => {
            ViewerGpuExecutionError::Backpressure(
                "compact CPU YUV transfer preparation is still running".to_owned(),
            )
        }
        error => ViewerGpuExecutionError::InputPreparation(format!(
            "compact CPU YUV GPU materialization failed: {error}"
        )),
    })
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
    layers.iter().map(|layer| composite_layer(layer, gpu_input_handles)).collect()
}

fn composite_layer<'a>(
    layer: &PreparedCompositeLayer<'a>,
    gpu_input_handles: &'a [GpuColorFrameHandle],
) -> GpuCompositeLayer<'a> {
    GpuCompositeLayer {
        source: match layer.source {
            PreparedCompositeLayerSource::CpuFrame(frame) => {
                GpuCompositeLayerSource::CpuFrame(frame)
            }
            PreparedCompositeLayerSource::CpuDataTexture(frame) => {
                GpuCompositeLayerSource::CpuDataTexture(frame)
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
    }
}

impl ViewerGpuExecutionResidency {
    fn record_source(
        &mut self,
        media_source: Option<&ViewerGpuMediaSource>,
        native_source: Option<&ViewerGpuNativeSource>,
        cpu_yuv_source: Option<&crate::ViewerGpuCpuYuvSource>,
    ) {
        let facts = native_source
            .map(ViewerGpuNativeVideoFacts::from_native_source)
            .or_else(|| cpu_yuv_source.map(ViewerGpuNativeVideoFacts::from_cpu_yuv_source))
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
    fn from_cpu_yuv_source(source: &crate::ViewerGpuCpuYuvSource) -> Self {
        let source_texture_format = Some(match source.frame.sample_format {
            mondrian_media::CpuYuvSampleFormat::Unorm8 => GpuNativeDecodedFrameTextureFormat::Nv12,
            mondrian_media::CpuYuvSampleFormat::Unorm16Lsb10 => {
                GpuNativeDecodedFrameTextureFormat::P010
            }
        });
        let source_video_sampling = source_texture_format.and_then(|format| {
            source.frame.source_color.color_space().and_then(|color_space| {
                native_video_sampling_from_decoded(color_space, format, source.frame.video_sampling)
            })
        });
        Self {
            decoder_residency: DecodedFrameResidency::CpuYuv,
            decoder_handle_kind: None,
            source_texture_format,
            source_video_sampling,
        }
    }

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
    use std::time::Duration;

    use crate::{
        ColorFrameAlpha, ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding,
        ColorFrameResidency, ColorFrameSpace, CpuEncodedColorFrame, CpuSourceColorFrame,
        GpuColorFrameId, GpuContext, HeterogeneousCpuPrefixBatchExecutor,
        HeterogeneousCpuPrefixBatchGrant, HeterogeneousCpuPrefixBatchItem,
        HeterogeneousCpuPrefixBatchRequest, HeterogeneousGpuContinuationBinding,
        HeterogeneousGpuContinuationRequest, HeterogeneousGpuResourceGrant,
        PreparedHeterogeneousEffectRoute, RenderInputTransform, TimelineSolidColorLayer,
    };
    use mondrian_core::automation::{PropertyHost, PropertyMutation, PropertyValue};
    use mondrian_core::display_calibration::{
        IccProfileFingerprint, DEFAULT_DISPLAY_CALIBRATION_LUT_EDGE,
    };
    use mondrian_core::{
        effect_data::{EffectNode, EffectType},
        ensure_mondrian_default_ocio_loaded, ColorEngine, ColorSpace, TimelineTime, WaveformMode,
        WorkingRgbaF32Frame,
    };
    use mondrian_effects::{
        compile_reference_effect_graph_in_domain, compile_reference_render_graph,
        lower_effect_graph_to_gpu_plan, EffectColorDomain, EffectColorDomainContract,
        EffectExecutionSessionConfig, EffectFrameExtent, EffectGraphBuilderState,
        EffectGraphExecutionBudget, EffectNodeExt, EffectRenderOp, EffectRenderPlan,
        PreparedEffectProgram,
    };
    use mondrian_media::{
        DecodedGpuFrameHandleKind, DecodedVideoSampling, DecodedVideoSurfaceFormat,
    };

    fn identity_rec709_display_calibration() -> Arc<DisplayCalibrationLut3d> {
        let edge = DEFAULT_DISPLAY_CALIBRATION_LUT_EDGE;
        let denominator = f32::from(edge - 1);
        let mut samples = Vec::with_capacity(usize::from(edge).pow(3) * 4);
        for blue in 0..edge {
            for green in 0..edge {
                for red in 0..edge {
                    samples.extend_from_slice(&[
                        f32::from(red) / denominator,
                        f32::from(green) / denominator,
                        f32::from(blue) / denominator,
                        1.0,
                    ]);
                }
            }
        }
        Arc::new(
            DisplayCalibrationLut3d::from_rgba32f_samples(
                ColorSpace::Rec709,
                IccProfileFingerprint::from_bytes(b"viewer-presentation-lease-test"),
                edge,
                samples,
            )
            .expect("identity Rec.709 display calibration"),
        )
    }

    fn record_empty_viewer_frame(
        context: &GpuContext,
        runtime: &mut ViewerGpuExecutionRuntime,
        timeline_frame: i64,
        display_calibration: Option<Arc<DisplayCalibrationLut3d>>,
    ) -> ViewerGpuExecutionRecord {
        try_record_empty_viewer_frame(context, runtime, timeline_frame, display_calibration)
            .expect("record Viewer presentation lease fixture")
    }

    fn try_record_empty_viewer_frame(
        context: &GpuContext,
        runtime: &mut ViewerGpuExecutionRuntime,
        timeline_frame: i64,
        display_calibration: Option<Arc<DisplayCalibrationLut3d>>,
    ) -> Result<ViewerGpuExecutionRecord, ViewerGpuExecutionError> {
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
        let output_precision = if display_calibration.is_some() {
            ViewerGpuOutputPrecision::EncodedFloat16
        } else {
            ViewerGpuOutputPrecision::Encoded8
        };
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewer-presentation-output-lease-test"),
        });
        let record = runtime.record(
            &context.device,
            &context.queue,
            &mut encoder,
            ViewerGpuExecutionRequest {
                sequence_id: SequenceId::new(),
                timeline_frame,
                width: 4,
                height: 4,
                working_color_space: WorkingColorSpace::LinearRec709,
                layers: &[],
                heterogeneous_inputs: Vec::new(),
                program_output_boundary: &output_boundary,
                monitor_adaptation: &monitor_adaptation,
                source_rect: ViewerSourceRect::FULL,
                output_width: 4,
                output_height: 4,
                output_precision,
                display_calibration,
                program_scopes: None,
                signal_monitoring: None,
            },
        )?;
        context.queue.submit(std::iter::once(encoder.finish()));
        Ok(record)
    }

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
    fn viewer_execution_error_keeps_typed_sources_without_large_result_payloads() {
        assert!(std::mem::size_of::<ViewerGpuExecutionError>() <= 64);

        let error = ViewerGpuExecutionError::Transition(Box::new(
            crate::GpuCompositeError::NonFiniteTransitionProgress {
                progress_bits: f32::NAN.to_bits(),
            },
        ));

        assert!(matches!(
            &error,
            ViewerGpuExecutionError::Transition(source)
                if matches!(
                    source.as_ref(),
                    crate::GpuCompositeError::NonFiniteTransitionProgress { .. }
                )
        ));
        let source = std::error::Error::source(&error).expect("typed transition source");
        let source = source
            .downcast_ref::<Box<crate::GpuCompositeError>>()
            .expect("boxed source retains the concrete transition error");
        assert!(matches!(
            source.as_ref(),
            crate::GpuCompositeError::NonFiniteTransitionProgress { .. }
        ));
        assert!(error.to_string().contains("progress must be finite"));
    }

    #[test]
    fn viewer_record_moves_native_import_candidate_receipt_out_exactly_once() {
        let output = GpuColorFrameHandle::new(
            GpuColorFrameId::from_raw(7),
            ColorFrameDescriptor {
                width: 1,
                height: 1,
                color_space: ColorFrameSpace::Working(WorkingColorSpace::LinearRec709),
                domain: ColorFrameDomain::Working,
                encoding: ColorFrameEncoding::LinearFloat,
                residency: ColorFrameResidency::Gpu,
                alpha: ColorFrameAlpha::StraightCoverage,
            },
            GpuColorFrameTextureFormat::Rgba16Float,
            "candidate-receipt-fixture",
        )
        .expect("valid GPU output fixture");
        let mut record = ViewerGpuExecutionRecord {
            program_output: output.clone(),
            program_scopes: None,
            output,
            working_float_decision: PRODUCT_GPU_WORKING_FLOAT_DECISION,
            output_owner: Some(ViewerGpuExecutionOutputOwner::ColorOutput),
            stage_diagnostics: RenderColorStageDiagnostics::default(),
            compositing_diagnostics: GpuCompositingDiagnostics::default(),
            spatial_diagnostics: GpuViewerSpatialRuntimeDiagnostics::default(),
            residency: ViewerGpuExecutionResidency::default(),
            fallback_reasons: Vec::new(),
            cpu_stage_timings: ViewerGpuExecutionCpuStageTimings::default(),
            native_video_import_timing_receipt: Some(
                NativeVideoImportCandidateTimingReceipt::fixture(9, 3, 1, 1, 1),
            ),
            heterogeneous_continuations: Vec::new(),
        };

        let receipt = record
            .take_native_video_import_timing_receipt()
            .expect("first take owns the receipt");
        assert_eq!(receipt.candidate_token().get(), 9);
        assert_eq!(receipt.submitted_imports(), 3);
        assert_eq!(receipt.scheduled_samples(), 1);
        assert_eq!(receipt.missing_samples(), 1);
        assert_eq!(receipt.dropped_samples(), 1);
        assert!(record.take_native_video_import_timing_receipt().is_none());
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

        let adaptation = crate::RenderMonitorAdaptation::new(
            ColorSpace::Rec709,
            ColorSpace::Srgb,
            ColorEngine::mondrian_standard(),
        )
        .expect("valid monitor adaptation");

        assert!(matches!(
            validate_program_scopes_contract(&boundary, &adaptation, Some(scopes)),
            Err(ViewerGpuExecutionError::ProgramScopesBoundaryMismatch {
                tap: ProgramScopesTap::ProgramOutput,
                expected_signal: ColorSpace::Rec709,
                scopes_signal: ColorSpace::DisplayP3,
            })
        ));
        assert!(validate_program_scopes_contract(&boundary, &adaptation, None).is_ok());
    }

    #[test]
    fn viewer_monitor_scopes_require_the_monitor_boundary() {
        let boundary = RenderOutputColorBoundary::display(
            ColorSpace::Rec709,
            false,
            ColorEngine::mondrian_standard(),
        );
        let adaptation = crate::RenderMonitorAdaptation::new(
            ColorSpace::Rec709,
            ColorSpace::DisplayP3,
            ColorEngine::mondrian_standard(),
        )
        .expect("valid monitor adaptation");
        let valid = GpuProgramScopesRequest::with_controls(
            ColorSpace::DisplayP3,
            WaveformMode::Luma,
            mondrian_core::ProgramScopeScale::Ire,
            ProgramScopesTap::MonitorOutput,
            256,
            512,
        )
        .expect("monitor scopes");
        assert!(validate_program_scopes_contract(&boundary, &adaptation, Some(valid)).is_ok());

        let invalid = GpuProgramScopesRequest::with_controls(
            ColorSpace::Rec709,
            WaveformMode::Luma,
            mondrian_core::ProgramScopeScale::Ire,
            ProgramScopesTap::MonitorOutput,
            256,
            512,
        )
        .expect("mismatched monitor scopes");
        assert!(matches!(
            validate_program_scopes_contract(&boundary, &adaptation, Some(invalid)),
            Err(ViewerGpuExecutionError::ProgramScopesBoundaryMismatch {
                tap: ProgramScopesTap::MonitorOutput,
                expected_signal: ColorSpace::DisplayP3,
                scopes_signal: ColorSpace::Rec709,
            })
        ));
    }

    #[tokio::test]
    async fn viewer_resource_grant_shrinks_the_live_idle_texture_pool() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping Viewer resource-grant test: no GPU adapter available");
            return;
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
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
                .expect("Viewer GPU runtime");
        let record_frame = |runtime: &mut ViewerGpuExecutionRuntime, timeline_frame| {
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("viewer-resource-grant-integration"),
                });
            runtime
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
                        layers: &[],
                        heterogeneous_inputs: Vec::new(),
                        program_output_boundary: &output_boundary,
                        monitor_adaptation: &monitor_adaptation,
                        source_rect: ViewerSourceRect::FULL,
                        output_width: 4,
                        output_height: 4,
                        output_precision: ViewerGpuOutputPrecision::Encoded8,
                        display_calibration: None,
                        program_scopes: None,
                        signal_monitoring: None,
                    },
                )
                .expect("Viewer GPU frame");
            context.queue.submit(std::iter::once(encoder.finish()));
        };

        record_frame(&mut runtime, 0);
        runtime.clear_frame_resources();
        let retained_before = runtime.color_output_diagnostics().resource_pool.retained_resources;
        assert!(retained_before > 0);

        record_frame(&mut runtime, 1);
        let active_before = runtime.color_output_diagnostics();
        assert!(active_before.frame_table_entries > 0);
        let zero_idle = ViewerGpuExecutionResourceGrant::new(0, 0);
        assert!(runtime.reconfigure_resource_grant(zero_idle));
        assert_eq!(runtime.resource_grant(), zero_idle);
        assert_eq!(
            runtime.color_output_diagnostics().resource_pool.retained_resources,
            0
        );
        runtime.clear_frame_resources();
        assert_eq!(
            runtime.color_output_diagnostics().resource_pool.retained_resources,
            0,
            "resources checked out under the old grant must obey the new grant on release"
        );
        assert!(!runtime.reconfigure_resource_grant(zero_idle));
        runtime.clear_idle_resources();
        assert_eq!(
            runtime.color_output_diagnostics().resource_pool.retained_resources,
            0
        );
    }

    #[tokio::test]
    async fn viewer_color_output_lease_survives_clear_and_returns_exactly_once() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping Viewer color-output lease test: no GPU adapter available");
            return;
        };
        let mut runtime =
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
                .expect("Viewer GPU runtime");
        let mut record = record_empty_viewer_frame(&context, &mut runtime, 10, None);
        let expected = record.output.clone();

        let lease = runtime
            .take_presentation_output(&mut record)
            .expect("take color-output presentation resource");
        let detached = runtime.color_output_diagnostics().resource_pool;
        assert_eq!(detached.detached_presentation_resources, 1);
        assert_eq!(detached.detached_presentation_bytes, 4 * 4 * 4);
        assert_eq!(detached.detached_presentation_high_water_resources, 1);
        assert_eq!(detached.detached_presentation_high_water_bytes, 4 * 4 * 4);
        assert!(!detached.detached_presentation_accounting_overflowed);
        assert_eq!(lease.handle(), &expected);
        assert_eq!(lease.texture().width(), 4);
        assert!(matches!(
            runtime.take_presentation_output(&mut record),
            Err(ViewerGpuPresentationOutputTakeError::AlreadyTaken { id })
                if id == expected.id()
        ));
        assert!(runtime.output_texture_view(&record).is_err());

        runtime.clear_frame_resources();
        assert_eq!(lease.handle(), &expected);
        assert_eq!(lease.texture().height(), 4);
        let releases_before_drop = runtime.color_output_diagnostics().resource_pool.releases;
        drop(lease);
        let after_drop = runtime.color_output_diagnostics().resource_pool;
        assert_eq!(after_drop.releases, releases_before_drop + 1);
        assert_eq!(after_drop.detached_presentation_resources, 0);
        assert_eq!(after_drop.detached_presentation_bytes, 0);
        assert_eq!(after_drop.detached_presentation_high_water_resources, 1);
        assert_eq!(after_drop.detached_presentation_high_water_bytes, 4 * 4 * 4);
    }

    #[tokio::test]
    async fn viewer_active_grant_is_stable_before_and_after_first_presentation() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping Viewer detached-output admission test: no GPU adapter available");
            return;
        };
        let mut runtime =
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
                .expect("Viewer GPU runtime");

        let warmup = record_empty_viewer_frame(&context, &mut runtime, 20, None);
        let steady_state = runtime
            .active_working_set_diagnostics()
            .last_admitted
            .expect("warmup admission");
        assert_eq!(
            steady_state.detached_presentations,
            crate::ViewerGpuActiveTextureDemand::default()
        );
        assert_eq!(
            steady_state.presentation_continuity_reserve,
            steady_state.presentation_output
        );
        runtime.clear_frame_resources();
        drop(warmup);

        let original_grant = runtime.resource_grant();
        let exact_steady_state_grant = ViewerGpuExecutionResourceGrant::new(
            original_grant.max_idle_per_contract(),
            original_grant.max_idle_bytes(),
        )
        .with_active_limits(steady_state.total().bytes, steady_state.total().textures);
        assert!(runtime.reconfigure_resource_grant(exact_steady_state_grant));

        let mut first = try_record_empty_viewer_frame(&context, &mut runtime, 21, None)
            .expect("first frame fits its exact steady-state grant");
        let lease = runtime
            .take_presentation_output(&mut first)
            .expect("detach first candidate output");
        runtime.clear_frame_resources();

        let second = try_record_empty_viewer_frame(&context, &mut runtime, 22, None)
            .expect("identical second frame fits while the first output remains current");
        let second_admission = runtime
            .active_working_set_diagnostics()
            .last_admitted
            .expect("second-frame admission");
        assert_eq!(
            second_admission.detached_presentations,
            steady_state.presentation_output
        );
        assert_eq!(
            second_admission.presentation_continuity_reserve,
            crate::ViewerGpuActiveTextureDemand::default()
        );
        assert_eq!(second_admission.total(), steady_state.total());
        drop(second);
        runtime.clear_frame_resources();

        drop(lease);
        assert_eq!(
            runtime.color_output_diagnostics().resource_pool.detached_presentation_resources,
            0
        );
        let third = try_record_empty_viewer_frame(&context, &mut runtime, 23, None)
            .expect("the reserve returns after the current lease drops");
        let third_admission = runtime
            .active_working_set_diagnostics()
            .last_admitted
            .expect("third-frame admission");
        assert_eq!(
            third_admission.detached_presentations,
            crate::ViewerGpuActiveTextureDemand::default()
        );
        assert_eq!(
            third_admission.presentation_continuity_reserve,
            steady_state.presentation_output
        );
        assert_eq!(third_admission.total(), steady_state.total());
        drop(third);
        runtime.clear_frame_resources();
    }

    #[tokio::test]
    async fn viewer_multiple_detached_outputs_backpressure_before_recording() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping Viewer capacity-one admission test: no GPU adapter available");
            return;
        };
        let mut runtime =
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
                .expect("Viewer GPU runtime");

        let mut first = record_empty_viewer_frame(&context, &mut runtime, 30, None);
        let first_lease =
            runtime.take_presentation_output(&mut first).expect("detach first output");
        runtime.clear_frame_resources();
        let mut second = record_empty_viewer_frame(&context, &mut runtime, 31, None);
        let second_lease = runtime
            .take_presentation_output(&mut second)
            .expect("detach second output to simulate a broken multi-slot Adapter");
        runtime.clear_frame_resources();
        assert_eq!(
            runtime.color_output_diagnostics().resource_pool.detached_presentation_resources,
            2
        );

        assert!(matches!(
            try_record_empty_viewer_frame(&context, &mut runtime, 32, None),
            Err(ViewerGpuExecutionError::Backpressure(reason))
                if reason.contains("2 live outputs")
        ));

        drop(second_lease);
        let recovered = try_record_empty_viewer_frame(&context, &mut runtime, 33, None)
            .expect("capacity-one admission resumes after one excess lease drops");
        drop(recovered);
        runtime.clear_frame_resources();
        drop(first_lease);
    }

    #[tokio::test]
    async fn viewer_calibrated_output_lease_survives_reset_without_repopulating_old_generation() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping Viewer calibrated-output lease test: no GPU adapter available");
            return;
        };
        let mut runtime =
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
                .expect("Viewer GPU runtime");
        let mut record = record_empty_viewer_frame(
            &context,
            &mut runtime,
            11,
            Some(identity_rec709_display_calibration()),
        );
        let expected = record.output.clone();
        let lease = runtime
            .take_presentation_output(&mut record)
            .expect("take calibrated presentation resource");
        assert_eq!(lease.handle(), &expected);

        let before_reset = runtime.color_output_diagnostics().resource_pool;
        assert_eq!(before_reset.detached_presentation_resources, 1);
        assert_eq!(before_reset.detached_presentation_bytes, 4 * 4 * 8);
        runtime.reset();
        let after_reset = runtime.color_output_diagnostics().resource_pool;
        assert_eq!(after_reset.invalidations, before_reset.invalidations + 1);
        assert_eq!(after_reset.detached_presentation_resources, 1);
        assert_eq!(after_reset.detached_presentation_bytes, 4 * 4 * 8);
        assert_eq!(lease.handle(), &expected);
        assert_eq!(lease.texture().width(), 4);
        let releases_before_drop = after_reset.releases;
        let stale_before_drop = after_reset.stale_generation_releases;
        drop(lease);
        let after_drop = runtime.color_output_diagnostics().resource_pool;
        assert_eq!(after_drop.releases, releases_before_drop);
        assert_eq!(after_drop.stale_generation_releases, stale_before_drop + 1);
        assert_eq!(after_drop.detached_presentation_resources, 0);
        assert_eq!(after_drop.detached_presentation_bytes, 0);
        assert_eq!(after_drop.retained_resources, 0);
    }

    #[tokio::test]
    async fn viewer_missing_presentation_output_consumes_record_authority_fail_closed() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping Viewer missing-output lease test: no GPU adapter available");
            return;
        };
        let mut runtime =
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
                .expect("Viewer GPU runtime");
        let mut record = record_empty_viewer_frame(&context, &mut runtime, 12, None);
        let expected_id = record.output.id();
        runtime.clear_frame_resources();

        assert!(matches!(
            runtime.take_presentation_output(&mut record),
            Err(ViewerGpuPresentationOutputTakeError::ColorOutput(
                GpuColorFrameResourceTableError::MissingFrame { id }
            )) if id == expected_id
        ));
        assert!(matches!(
            runtime.take_presentation_output(&mut record),
            Err(ViewerGpuPresentationOutputTakeError::AlreadyTaken { id })
                if id == expected_id
        ));
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
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
                .expect("Viewer GPU runtime");
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
                    heterogeneous_inputs: Vec::new(),
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
                    signal_monitoring: None,
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

        let mut monitor_scopes_encoder =
            context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("viewer-monitor-scopes-integration"),
            });
        let monitor_scopes_record = runtime
            .record(
                &context.device,
                &context.queue,
                &mut monitor_scopes_encoder,
                ViewerGpuExecutionRequest {
                    sequence_id: SequenceId::new(),
                    timeline_frame: 1,
                    width: 4,
                    height: 4,
                    working_color_space: WorkingColorSpace::LinearRec709,
                    layers: &[],
                    heterogeneous_inputs: Vec::new(),
                    program_output_boundary: &program_output_boundary,
                    monitor_adaptation: &monitor_adaptation,
                    source_rect: ViewerSourceRect::FULL,
                    output_width: 4,
                    output_height: 4,
                    output_precision: ViewerGpuOutputPrecision::Encoded8,
                    display_calibration: None,
                    program_scopes: Some(
                        GpuProgramScopesRequest::with_controls(
                            ColorSpace::DisplayP3,
                            WaveformMode::RgbParade,
                            mondrian_core::ProgramScopeScale::Nits100,
                            ProgramScopesTap::MonitorOutput,
                            256,
                            512,
                        )
                        .expect("monitor scope request"),
                    ),
                    signal_monitoring: None,
                },
            )
            .expect("Viewer GPU Monitor Output scopes frame");
        context.queue.submit(std::iter::once(monitor_scopes_encoder.finish()));
        let monitor_scopes =
            monitor_scopes_record.program_scopes.as_ref().expect("Monitor Output scopes");
        assert_eq!(
            monitor_scopes.request.signal_color_space(),
            ColorSpace::DisplayP3
        );
        assert_eq!(
            monitor_scopes.request.tap(),
            ProgramScopesTap::MonitorOutput
        );
        assert_eq!(
            monitor_scopes.request.scale(),
            mondrian_core::ProgramScopeScale::Nits100
        );
        assert_eq!(runtime.program_scopes_diagnostics().frames_recorded, 2);
    }

    #[tokio::test]
    async fn viewer_records_gpu_media_effect_domain_before_working_composite() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping Viewer effect-domain integration test: no GPU adapter available");
            return;
        };
        let domain = EffectColorDomain::DisplayEncodedRgb { color_space: ColorSpace::Rec709 };
        let graph = compile_reference_effect_graph_in_domain(
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
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
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
                .expect("Viewer GPU runtime");
        for timeline_frame in 7..10 {
            let layer =
                ViewerGpuExecutionLayer::Source(Box::new(crate::ViewerGpuSourceLayer::Media {
                    frame: None,
                    is_data_texture: false,
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
                    cpu_yuv_source: None,
                    heterogeneous_input: None,
                    opacity: 1.0,
                    blend_mode: BlendMode::Normal,
                    transform: [0.5, 0.0, 1.0, 0.0, 0.5, 1.0],
                    effect_plan: Arc::clone(&effect_plan),
                    frame_seed: timeline_frame,
                }));
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
                        heterogeneous_inputs: Vec::new(),
                        program_output_boundary: &output_boundary,
                        monitor_adaptation: &monitor_adaptation,
                        source_rect: ViewerSourceRect::FULL,
                        output_width: 4,
                        output_height: 4,
                        output_precision: ViewerGpuOutputPrecision::Encoded8,
                        display_calibration: None,
                        program_scopes: None,
                        signal_monitoring: None,
                    },
                )
                .expect("Viewer GPU effect-domain frame");
            context.queue.submit(std::iter::once(encoder.finish()));

            assert_eq!(record.stage_diagnostics.gpu_color_stages, 4);
            assert_eq!(record.stage_diagnostics.upload_stages, 1);
            assert_eq!(record.stage_diagnostics.readback_stages, 0);
            assert_eq!(record.compositing_diagnostics.gpu_passthrough_frames, 0);
            assert_eq!(record.compositing_diagnostics.gpu_native_composites, 1);
            assert!(record.compositing_diagnostics.execution.render_passes >= 1);
            assert!(record.compositing_diagnostics.execution.avoided_shader_pixels > 0);
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
        let graph = compile_reference_effect_graph_in_domain(
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
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
        let layer = ViewerGpuExecutionLayer::Source(Box::new(crate::ViewerGpuSourceLayer::Media {
            frame: Some(frame),
            is_data_texture: false,
            gpu_source: None,
            native_source: None,
            cpu_yuv_source: None,
            heterogeneous_input: None,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan,
            frame_seed: 9,
        }));
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
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
                .expect("Viewer GPU runtime");
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
                    heterogeneous_inputs: Vec::new(),
                    program_output_boundary: &output_boundary,
                    monitor_adaptation: &monitor_adaptation,
                    source_rect: ViewerSourceRect::FULL,
                    output_width: 4,
                    output_height: 4,
                    output_precision: ViewerGpuOutputPrecision::Encoded8,
                    display_calibration: None,
                    program_scopes: None,
                    signal_monitoring: None,
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
        let graph = compile_reference_effect_graph_in_domain(
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
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
        let layer = ViewerGpuExecutionLayer::Source(Box::new(crate::ViewerGpuSourceLayer::Media {
            frame: Some(frame),
            is_data_texture: false,
            gpu_source: None,
            native_source: None,
            cpu_yuv_source: None,
            heterogeneous_input: None,
            opacity: 0.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan,
            frame_seed: 9,
        }));
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
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
                .expect("Viewer GPU runtime");
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
                    heterogeneous_inputs: Vec::new(),
                    program_output_boundary: &output_boundary,
                    monitor_adaptation: &monitor_adaptation,
                    source_rect: ViewerSourceRect::FULL,
                    output_width: 4,
                    output_height: 4,
                    output_precision: ViewerGpuOutputPrecision::Encoded8,
                    display_calibration: None,
                    program_scopes: None,
                    signal_monitoring: None,
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
        let graph = compile_reference_effect_graph_in_domain(
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                }],
            },
            EffectColorDomainContract::preserving(domain),
        )
        .expect("valid display-domain graph");
        let effect_plan =
            Arc::new(lower_effect_graph_to_gpu_plan(&graph).expect("GPU effect plan"));
        let layer =
            ViewerGpuExecutionLayer::Source(Box::new(crate::ViewerGpuSourceLayer::SolidColor {
                layer: TimelineSolidColorLayer {
                    color: Color { r: 0.18, g: 0.08, b: 0.02, a: 0.75 },
                    opacity: 1.0,
                    blend_mode: BlendMode::Normal,
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_graph: Arc::new(graph),
                    frame_seed: 11,
                },
                effect_plan,
            }));
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
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
                .expect("Viewer GPU runtime");
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
                    heterogeneous_inputs: Vec::new(),
                    program_output_boundary: &output_boundary,
                    monitor_adaptation: &monitor_adaptation,
                    source_rect: ViewerSourceRect::FULL,
                    output_width: 4,
                    output_height: 4,
                    output_precision: ViewerGpuOutputPrecision::Encoded8,
                    display_calibration: None,
                    program_scopes: None,
                    signal_monitoring: None,
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
        let graph = compile_reference_render_graph(EffectGraphBuilderState::new().finish())
            .expect("valid scene-linear identity graph");
        let effect_plan =
            Arc::new(lower_effect_graph_to_gpu_plan(&graph).expect("GPU identity plan"));
        let layer =
            ViewerGpuExecutionLayer::Source(Box::new(crate::ViewerGpuSourceLayer::SolidColor {
                layer: TimelineSolidColorLayer {
                    color: Color { r: 0.18, g: 0.08, b: 0.02, a: 1.0 },
                    opacity: 1.0,
                    blend_mode: BlendMode::Normal,
                    transform: [0.75, 0.0, 0.125, 0.0, 0.75, 0.125],
                    effect_graph: Arc::clone(&graph),
                    frame_seed: 0,
                },
                effect_plan,
            }));
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
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
                .expect("Viewer GPU runtime");
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
                    heterogeneous_inputs: Vec::new(),
                    program_output_boundary: &output_boundary,
                    monitor_adaptation: &monitor_adaptation,
                    source_rect: ViewerSourceRect::FULL,
                    output_width: 4,
                    output_height: 4,
                    output_precision: ViewerGpuOutputPrecision::EncodedFloat16,
                    display_calibration: None,
                    program_scopes: None,
                    signal_monitoring: None,
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
    async fn viewer_executes_cross_dissolve_as_one_typed_working_node() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping Viewer Cross Dissolve test: no GPU adapter available");
            return;
        };
        let graph = compile_reference_render_graph(EffectGraphBuilderState::new().finish())
            .expect("valid scene-linear identity graph");
        let effect_plan =
            Arc::new(lower_effect_graph_to_gpu_plan(&graph).expect("GPU identity plan"));
        let source = |color, frame_seed| {
            crate::ViewerGpuTransitionInput::Source(Box::new(
                crate::ViewerGpuSourceLayer::SolidColor {
                    layer: TimelineSolidColorLayer {
                        color,
                        opacity: 1.0,
                        blend_mode: BlendMode::Normal,
                        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                        effect_graph: Arc::clone(&graph),
                        frame_seed,
                    },
                    effect_plan: Arc::clone(&effect_plan),
                },
            ))
        };
        let layer =
            ViewerGpuExecutionLayer::CrossDissolve(Box::new(crate::ViewerGpuCrossDissolveLayer {
                left: source(Color { r: 1.0, g: 0.0, b: 0.0, a: 0.25 }, 7),
                right: source(Color { r: 0.0, g: 0.0, b: 1.0, a: 0.75 }, 8),
                progress: 0.4,
            }));
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
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
                .expect("Viewer GPU runtime");
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewer-cross-dissolve"),
        });

        let record = runtime
            .record(
                &context.device,
                &context.queue,
                &mut encoder,
                ViewerGpuExecutionRequest {
                    sequence_id: SequenceId::new(),
                    timeline_frame: 8,
                    width: 4,
                    height: 4,
                    working_color_space: WorkingColorSpace::LinearRec709,
                    layers: &[layer],
                    heterogeneous_inputs: Vec::new(),
                    program_output_boundary: &output_boundary,
                    monitor_adaptation: &monitor_adaptation,
                    source_rect: ViewerSourceRect::FULL,
                    output_width: 4,
                    output_height: 4,
                    output_precision: ViewerGpuOutputPrecision::Encoded8,
                    display_calibration: None,
                    program_scopes: None,
                    signal_monitoring: None,
                },
            )
            .expect("Viewer typed Cross Dissolve frame");
        context.queue.submit(std::iter::once(encoder.finish()));

        assert_eq!(record.compositing_diagnostics.gpu_cross_dissolve_passes, 1);
        assert_eq!(record.residency.procedural_layers, 2);
        assert_eq!(record.residency.cpu_upload_layers, 0);
        assert_eq!(record.stage_diagnostics.readback_stages, 0);
    }

    #[tokio::test]
    async fn viewer_records_tracer_cpu_prefix_and_gpu_suffix_in_one_submission() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping Viewer heterogeneous tracer test: no GPU adapter available");
            return;
        };
        const GENERATION: u64 = 41;
        const FRAME_SEED: i64 = 73;
        const WIDTH: u32 = 4;
        const HEIGHT: u32 = 3;
        const WORKING_SPACE: WorkingColorSpace = WorkingColorSpace::LinearRec2020;
        let blur = EffectNode::with_defaults(EffectType::GaussianBlur);
        let mut correction = EffectNode::with_defaults(EffectType::BasicCorrection);
        correction
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::BasicCorrection.property_path("exposure"),
                value: PropertyValue::Float(0.25),
            })
            .expect("set Basic Correction exposure");
        let mut grain = EffectNode::with_defaults(EffectType::Grain);
        grain
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Grain.property_path("amount"),
                value: PropertyValue::Float(0.1),
            })
            .expect("set Grain amount");
        let graph = PreparedEffectProgram::prepare(&[blur, correction, grain], &[], WORKING_SPACE)
            .expect("prepare heterogeneous tracer")
            .evaluate(TimelineTime::ZERO)
            .expect("compile heterogeneous tracer");
        let graph_budget = EffectGraphExecutionBudget::new(16 << 20, 16 << 20, 16 << 20, 32, 64);
        let cpu_grant = HeterogeneousCpuPrefixBatchGrant::new(
            EffectExecutionSessionConfig::uncached(16 << 20),
            graph_budget,
            1,
            16 << 20,
        );
        let cpu_input = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: WIDTH,
            height: HEIGHT,
            color_space: WORKING_SPACE,
            data: (0..WIDTH * HEIGHT)
                .map(|index| {
                    let value = index as f32 / (WIDTH * HEIGHT - 1) as f32;
                    [value, 1.0 - value, 0.25 + 0.5 * value, 1.0]
                })
                .collect(),
        });
        let prepared_route = PreparedHeterogeneousEffectRoute::prepare(
            Arc::clone(&graph),
            EffectFrameExtent::new(WIDTH, HEIGHT),
            graph_budget,
        )
        .expect("prepare tracer route");
        let cpu_output = HeterogeneousCpuPrefixBatchExecutor::default()
            .execute(
                HeterogeneousCpuPrefixBatchRequest::new(
                    cpu_grant,
                    vec![HeterogeneousCpuPrefixBatchItem::new(
                        0,
                        prepared_route,
                        cpu_input,
                        WORKING_SPACE,
                        FRAME_SEED,
                    )],
                ),
                GENERATION,
                || None,
            )
            .expect("execute tracer CPU prefix");
        let mut completions = cpu_output.into_completions().into_vec();
        let (_, completion) = completions.pop().expect("one CPU prefix completion").into_parts();
        let gpu_request = HeterogeneousGpuContinuationRequest::new(
            HeterogeneousGpuContinuationBinding::new(
                graph.semantic_fingerprint(),
                GENERATION,
                EffectFrameExtent::new(WIDTH, HEIGHT),
                FRAME_SEED,
                WORKING_SPACE,
            ),
            HeterogeneousGpuResourceGrant::new(16 << 20, 16 << 20, 64, 0),
        );
        let identity_graph =
            compile_reference_render_graph(EffectGraphBuilderState::new().finish())
                .expect("compile Viewer identity graph");
        let identity_plan = Arc::new(
            lower_effect_graph_to_gpu_plan(&identity_graph).expect("lower Viewer identity plan"),
        );
        let layer = ViewerGpuExecutionLayer::Source(Box::new(crate::ViewerGpuSourceLayer::Media {
            frame: None,
            is_data_texture: false,
            gpu_source: None,
            native_source: None,
            cpu_yuv_source: None,
            heterogeneous_input: Some(0),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: identity_plan,
            frame_seed: FRAME_SEED,
        }));
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
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
                .expect("Viewer GPU runtime");
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewer-heterogeneous-tracer"),
        });
        let mut record = runtime
            .record(
                &context.device,
                &context.queue,
                &mut encoder,
                ViewerGpuExecutionRequest {
                    sequence_id: SequenceId::new(),
                    timeline_frame: FRAME_SEED,
                    // Source-domain extent deliberately differs from the
                    // Viewer canvas. Nested placements project through the
                    // existing normalized layer transform.
                    width: WIDTH * 2,
                    height: HEIGHT * 2,
                    working_color_space: WORKING_SPACE,
                    layers: &[layer],
                    heterogeneous_inputs: vec![ViewerHeterogeneousGpuInput {
                        request: gpu_request,
                        completion,
                    }],
                    program_output_boundary: &output_boundary,
                    monitor_adaptation: &monitor_adaptation,
                    source_rect: ViewerSourceRect::FULL,
                    output_width: WIDTH * 2,
                    output_height: HEIGHT * 2,
                    output_precision: ViewerGpuOutputPrecision::Encoded8,
                    display_calibration: None,
                    program_scopes: None,
                    signal_monitoring: None,
                },
            )
            .expect("record Viewer heterogeneous tracer");
        assert_eq!(record.heterogeneous_continuation_count(), 1);
        let submission_index = context.queue.submit(std::iter::once(encoder.finish()));
        let submitted = record.assert_adapter_submission(submission_index);
        assert_eq!(submitted.len(), 1);
        let completed = submitted
            .wait_until(
                &context.device,
                std::time::Instant::now() + Duration::from_secs(5),
            )
            .expect("wait for exact heterogeneous Viewer submission");
        assert_eq!(completed.len(), 1);
        let evidence = completed.continuations()[0].evidence();
        assert_eq!(evidence.recorded().generation(), GENERATION);
        assert_eq!(
            evidence.recorded().graph_fingerprint(),
            graph.semantic_fingerprint()
        );
        assert_eq!(
            evidence.submitted().authority(),
            crate::HeterogeneousGpuSubmissionAuthority::TrustedAdapterAssertion
        );
        assert_eq!(evidence.batch_id(), evidence.recorded().batch_id());
        assert_eq!(evidence.completed_uploads(), evidence.recorded().uploads());
        assert_eq!(
            evidence.completed_output_token(),
            evidence.recorded().output_signal()
        );
        assert_eq!(evidence.recorded().gpu_nodes().len(), 2);
        let admitted = runtime
            .active_working_set_diagnostics()
            .last_admitted
            .expect("Viewer admitted heterogeneous working set");
        assert_eq!(
            admitted.source_preparation.bytes,
            evidence.recorded().recorded_device_bytes(),
            "Viewer must admit the Adapter's physical recording bytes"
        );
        assert_eq!(
            admitted.source_preparation.textures,
            evidence.recorded().recorded_device_materializations(),
            "Viewer must admit the Adapter's physical recording texture count"
        );
    }

    #[tokio::test]
    async fn viewer_composite_to_program_output_matches_cpu_on_real_wgpu_device() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping Viewer composite-to-output parity test: no GPU adapter");
            return;
        };
        let graph = compile_reference_render_graph(EffectGraphBuilderState::new().finish())
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
        let layer = ViewerGpuExecutionLayer::Source(Box::new(crate::ViewerGpuSourceLayer::Media {
            frame: None,
            is_data_texture: false,
            gpu_source: Some(ViewerGpuMediaSource {
                source: Arc::clone(&source),
                input_transform: gpu_input_transform,
                decoder_residency: DecodedFrameResidency::CpuRgba,
                decoder_handle_kind: None,
                decoded_surface_format: DecodedVideoSurfaceFormat::Rgba8,
                decoded_video_sampling: DecodedVideoSampling::default(),
            }),
            native_source: None,
            cpu_yuv_source: None,
            heterogeneous_input: None,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Arc::new(
                lower_effect_graph_to_gpu_plan(&graph).expect("GPU identity plan"),
            ),
            frame_seed: 0,
        }));
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
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
                .expect("Viewer GPU runtime");
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
                    heterogeneous_inputs: Vec::new(),
                    program_output_boundary: &boundary,
                    monitor_adaptation: &monitor_adaptation,
                    source_rect: ViewerSourceRect::FULL,
                    output_width: 4,
                    output_height: 4,
                    output_precision: ViewerGpuOutputPrecision::Encoded8,
                    display_calibration: None,
                    program_scopes: None,
                    signal_monitoring: None,
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
            output,
        )
        .expect("record Viewer output readback");
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
        let scene_graph = compile_reference_render_graph(EffectGraphBuilderState::new().finish())
            .expect("valid scene-linear identity graph");
        let scene_plan = Arc::new(
            lower_effect_graph_to_gpu_plan(&scene_graph).expect("scene-linear GPU identity plan"),
        );
        let domain = EffectColorDomain::DisplayEncodedRgb { color_space: ColorSpace::Rec709 };
        let adjustment_graph = compile_reference_effect_graph_in_domain(
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                }],
            },
            EffectColorDomainContract::preserving(domain),
        )
        .expect("valid display-domain adjustment graph");
        let adjustment_plan = Arc::new(
            lower_effect_graph_to_gpu_plan(&adjustment_graph).expect("GPU adjustment plan"),
        );
        let layers = [
            ViewerGpuExecutionLayer::Source(Box::new(crate::ViewerGpuSourceLayer::SolidColor {
                layer: TimelineSolidColorLayer {
                    color: Color { r: 0.18, g: 0.08, b: 0.02, a: 1.0 },
                    opacity: 1.0,
                    blend_mode: BlendMode::Normal,
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_graph: Arc::clone(&scene_graph),
                    frame_seed: 0,
                },
                effect_plan: scene_plan,
            })),
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
            ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
                .expect("Viewer GPU runtime");
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
                    heterogeneous_inputs: Vec::new(),
                    program_output_boundary: &output_boundary,
                    monitor_adaptation: &monitor_adaptation,
                    source_rect: ViewerSourceRect::FULL,
                    output_width: 4,
                    output_height: 4,
                    output_precision: ViewerGpuOutputPrecision::Encoded8,
                    display_calibration: None,
                    program_scopes: None,
                    signal_monitoring: None,
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
