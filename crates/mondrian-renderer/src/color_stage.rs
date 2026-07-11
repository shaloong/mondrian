use crate::{
    ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding, ColorFrameResidency, CpuColorFrame,
    CpuColorTransformExecutor, CpuEncodedColorFrame, GpuColorFrameAllocationPlan,
    GpuColorFrameHandle, GpuColorFrameId, GpuColorFrameIdAllocator, GpuColorFrameReadback,
    GpuColorFrameReadbackError, GpuColorFrameReadbackPlan, GpuColorFrameResource,
    GpuColorFrameResourceTable, GpuColorFrameResourceTableError, GpuColorFrameTextureFormat,
    GpuColorFrameUploadError, GpuColorFrameUploadPlan, GpuColorFrameUploader,
    GpuColorFrameWgpuResource, GpuCompositeError, GpuCompositeRecord, GpuCompositeRequest,
    GpuFrameCompositor, LinearFloatSource, OcioGpuShaderCache, OcioGpuShaderCacheDiagnostics,
    OcioGpuWgpuBackendObjectError, OcioGpuWgpuBackendObjectRuntime,
    OcioGpuWgpuBackendObjectRuntimeDiagnostics, OcioGpuWgpuBackendPrepError,
    OcioGpuWgpuBackendPrepRuntime, OcioGpuWgpuBackendPrepRuntimeDiagnostics,
    OcioGpuWgpuBindGroupLayoutDescriptorPlan, OcioGpuWgpuBindGroupPreparer, OcioGpuWgpuBlocker,
    OcioGpuWgpuColorTargetFormat, OcioGpuWgpuOcioBindGroup, OcioGpuWgpuRenderPassError,
    OcioGpuWgpuRenderPassNodePlan, OcioGpuWgpuRenderPassRecorder, OcioGpuWgpuRenderPassTarget,
    OcioGpuWgpuRenderPipeline, OcioGpuWgpuWrapperBindGroup, OcioGpuWgpuWrapperBindingPlan,
    OcioGpuWgpuWrapperInputResources, RenderColorTransform, RenderColorTransformError,
    RenderColorTransformGpuOptions, RenderColorTransformGpuPlan, RenderColorTransformGpuPlanner,
    RenderInputTransform, RenderInputTransformResult, RenderOcioDisplayView,
    RenderOutputTransformFloatResult, RenderOutputTransformResult,
};
use mondrian_core::types::{ColorEngine, ColorSpace};
use mondrian_core::WorkingColorSpace;
use serde::{Deserialize, Serialize};

/// Preferred execution mode for a renderer color transform stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderColorStageMode {
    /// Always use the CPU transform executor.
    CpuOnly,
    /// Plan a GPU OCIO shader stage and expose required transfer/readback nodes.
    PreferGpu,
}

/// One scheduled step in a renderer color transform boundary.
#[derive(Debug, Clone)]
pub enum RenderColorStage {
    /// Upload a CPU-resident boundary frame to a GPU texture before GPU execution.
    UploadToGpu {
        /// CPU descriptor consumed by the upload.
        input: ColorFrameDescriptor,
        /// GPU descriptor produced by the upload.
        output: ColorFrameDescriptor,
    },
    /// Execute a CPU source/import -> working-space transform.
    CpuInputTransform {
        /// Input descriptor.
        input: ColorFrameDescriptor,
        /// Output descriptor.
        output: ColorFrameDescriptor,
        /// Transform request.
        transform: RenderInputTransform,
    },
    /// Execute a CPU working-space -> display/export transform.
    CpuOutputTransform {
        /// Input descriptor.
        input: ColorFrameDescriptor,
        /// Output descriptor.
        output: ColorFrameDescriptor,
        /// Transform request.
        transform: RenderColorTransform,
    },
    /// Execute or prepare a GPU OCIO color transform.
    GpuColorTransform {
        /// GPU descriptor consumed by the color transform.
        input: ColorFrameDescriptor,
        /// GPU descriptor produced by the color transform.
        output: ColorFrameDescriptor,
        /// GPU transform plan and blockers.
        plan: Box<RenderColorTransformGpuPlan>,
    },
    /// Read back a GPU-resident boundary frame to CPU memory.
    ReadbackToCpu {
        /// GPU descriptor consumed by the readback.
        input: ColorFrameDescriptor,
        /// CPU descriptor produced by the readback.
        output: ColorFrameDescriptor,
    },
}

impl RenderColorStage {
    /// Input descriptor consumed by this stage.
    pub fn input(&self) -> ColorFrameDescriptor {
        match self {
            Self::UploadToGpu { input, .. }
            | Self::CpuInputTransform { input, .. }
            | Self::CpuOutputTransform { input, .. }
            | Self::GpuColorTransform { input, .. }
            | Self::ReadbackToCpu { input, .. } => *input,
        }
    }

    /// Output descriptor produced by this stage.
    pub fn output(&self) -> ColorFrameDescriptor {
        match self {
            Self::UploadToGpu { output, .. }
            | Self::CpuInputTransform { output, .. }
            | Self::CpuOutputTransform { output, .. }
            | Self::GpuColorTransform { output, .. }
            | Self::ReadbackToCpu { output, .. } => *output,
        }
    }
}

/// Scheduled color transform boundary.
#[derive(Debug, Clone)]
pub struct RenderColorStagePlan {
    /// Ordered stages needed to satisfy the transform.
    pub stages: Vec<RenderColorStage>,
    /// Final descriptor produced by the last stage.
    pub final_descriptor: ColorFrameDescriptor,
}

/// Aggregated diagnostics for a color stage plan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderColorStageDiagnostics {
    /// Total stages in the plan.
    pub total_stages: u64,
    /// CPU source/import -> working-space stages.
    pub cpu_input_stages: u64,
    /// CPU working-space -> display/export stages.
    pub cpu_output_stages: u64,
    /// GPU OCIO color transform stages.
    pub gpu_color_stages: u64,
    /// CPU -> GPU upload stages.
    pub upload_stages: u64,
    /// GPU -> CPU readback stages.
    pub readback_stages: u64,
    /// Native GPU blockers exposed by planned GPU stages.
    pub gpu_blockers: u64,
    /// Structured native GPU blocker breakdown.
    pub gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown,
    /// Sum of pixels touched by scheduled stages.
    pub stage_pixels: u64,
}

/// Aggregated native GPU blocker reasons in a color stage plan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderColorStageGpuBlockerBreakdown {
    /// Backend shader module has not been prepared.
    pub shader_module_not_prepared: u64,
    /// OCIO LUT/uniform resources are not connected to a concrete bind group.
    pub ocio_resource_bind_group_not_prepared: u64,
    /// Fullscreen wrapper shader is missing.
    pub fullscreen_wrapper_not_prepared: u64,
    /// Final render pipeline/render-pass node is missing.
    pub render_pipeline_not_prepared: u64,
    /// OCIO config is not loaded or unavailable for GPU shader extraction.
    pub ocio_config_not_loaded: u64,
    /// OCIO processor could not be created for the requested transform.
    pub ocio_processor_unavailable: u64,
    /// OCIO GPU shader extraction failed (transpilation, Naga, or backend error).
    pub ocio_gpu_shader_extraction_failed: u64,
}

impl RenderColorStageGpuBlockerBreakdown {
    /// Total counted native GPU blockers.
    pub fn total(self) -> u64 {
        self.shader_module_not_prepared
            .saturating_add(self.ocio_resource_bind_group_not_prepared)
            .saturating_add(self.fullscreen_wrapper_not_prepared)
            .saturating_add(self.render_pipeline_not_prepared)
            .saturating_add(self.ocio_config_not_loaded)
            .saturating_add(self.ocio_processor_unavailable)
            .saturating_add(self.ocio_gpu_shader_extraction_failed)
    }

    /// Record one native GPU blocker.
    pub fn record(&mut self, blocker: &OcioGpuWgpuBlocker) {
        match blocker {
            OcioGpuWgpuBlocker::ShaderModuleNotPrepared { .. } => {
                self.shader_module_not_prepared = self.shader_module_not_prepared.saturating_add(1);
            }
            OcioGpuWgpuBlocker::OcioResourceBindGroupNotPrepared { .. } => {
                self.ocio_resource_bind_group_not_prepared =
                    self.ocio_resource_bind_group_not_prepared.saturating_add(1);
            }
            OcioGpuWgpuBlocker::FullscreenWrapperNotPrepared => {
                self.fullscreen_wrapper_not_prepared =
                    self.fullscreen_wrapper_not_prepared.saturating_add(1);
            }
            OcioGpuWgpuBlocker::RenderPipelineNotPrepared => {
                self.render_pipeline_not_prepared =
                    self.render_pipeline_not_prepared.saturating_add(1);
            }
            OcioGpuWgpuBlocker::OcioConfigNotLoaded => {
                self.ocio_config_not_loaded = self.ocio_config_not_loaded.saturating_add(1);
            }
            OcioGpuWgpuBlocker::OcioProcessorUnavailable => {
                self.ocio_processor_unavailable = self.ocio_processor_unavailable.saturating_add(1);
            }
            OcioGpuWgpuBlocker::OcioGpuShaderExtractionFailed { .. } => {
                self.ocio_gpu_shader_extraction_failed =
                    self.ocio_gpu_shader_extraction_failed.saturating_add(1);
            }
        }
    }

    /// Add counts from another breakdown.
    pub fn accumulate(&mut self, other: Self) {
        self.shader_module_not_prepared =
            self.shader_module_not_prepared.saturating_add(other.shader_module_not_prepared);
        self.ocio_resource_bind_group_not_prepared = self
            .ocio_resource_bind_group_not_prepared
            .saturating_add(other.ocio_resource_bind_group_not_prepared);
        self.fullscreen_wrapper_not_prepared = self
            .fullscreen_wrapper_not_prepared
            .saturating_add(other.fullscreen_wrapper_not_prepared);
        self.render_pipeline_not_prepared = self
            .render_pipeline_not_prepared
            .saturating_add(other.render_pipeline_not_prepared);
        self.ocio_config_not_loaded =
            self.ocio_config_not_loaded.saturating_add(other.ocio_config_not_loaded);
        self.ocio_processor_unavailable =
            self.ocio_processor_unavailable.saturating_add(other.ocio_processor_unavailable);
        self.ocio_gpu_shader_extraction_failed = self
            .ocio_gpu_shader_extraction_failed
            .saturating_add(other.ocio_gpu_shader_extraction_failed);
    }
}

impl RenderColorStageDiagnostics {
    /// Add another diagnostics summary into this one.
    pub fn accumulate(&mut self, other: Self) {
        self.total_stages = self.total_stages.saturating_add(other.total_stages);
        self.cpu_input_stages = self.cpu_input_stages.saturating_add(other.cpu_input_stages);
        self.cpu_output_stages = self.cpu_output_stages.saturating_add(other.cpu_output_stages);
        self.gpu_color_stages = self.gpu_color_stages.saturating_add(other.gpu_color_stages);
        self.upload_stages = self.upload_stages.saturating_add(other.upload_stages);
        self.readback_stages = self.readback_stages.saturating_add(other.readback_stages);
        self.gpu_blockers = self.gpu_blockers.saturating_add(other.gpu_blockers);
        self.gpu_blocker_breakdown.accumulate(other.gpu_blocker_breakdown);
        self.stage_pixels = self.stage_pixels.saturating_add(other.stage_pixels);
    }
}

impl RenderColorStagePlan {
    /// Whether the plan contains a GPU transform stage.
    pub fn contains_gpu_transform(&self) -> bool {
        self.stages
            .iter()
            .any(|stage| matches!(stage, RenderColorStage::GpuColorTransform { .. }))
    }

    /// Whether the plan contains an explicit CPU/GPU transfer stage.
    pub fn contains_transfer(&self) -> bool {
        self.stages.iter().any(|stage| {
            matches!(
                stage,
                RenderColorStage::UploadToGpu { .. } | RenderColorStage::ReadbackToCpu { .. }
            )
        })
    }

    /// Return an aggregated stage diagnostics summary.
    pub fn diagnostics(&self) -> RenderColorStageDiagnostics {
        let mut diagnostics = RenderColorStageDiagnostics::default();
        for stage in &self.stages {
            diagnostics.total_stages = diagnostics.total_stages.saturating_add(1);
            diagnostics.stage_pixels =
                diagnostics.stage_pixels.saturating_add(stage.output().pixel_count() as u64);
            match stage {
                RenderColorStage::UploadToGpu { .. } => {
                    diagnostics.upload_stages = diagnostics.upload_stages.saturating_add(1);
                }
                RenderColorStage::CpuInputTransform { .. } => {
                    diagnostics.cpu_input_stages = diagnostics.cpu_input_stages.saturating_add(1);
                }
                RenderColorStage::CpuOutputTransform { .. } => {
                    diagnostics.cpu_output_stages = diagnostics.cpu_output_stages.saturating_add(1);
                }
                RenderColorStage::GpuColorTransform { plan, .. } => {
                    diagnostics.gpu_color_stages = diagnostics.gpu_color_stages.saturating_add(1);
                    diagnostics.gpu_blockers =
                        diagnostics.gpu_blockers.saturating_add(plan.wgpu.blockers.len() as u64);
                    for blocker in &plan.wgpu.blockers {
                        diagnostics.gpu_blocker_breakdown.record(blocker);
                    }
                }
                RenderColorStage::ReadbackToCpu { .. } => {
                    diagnostics.readback_stages = diagnostics.readback_stages.saturating_add(1);
                }
            }
        }
        diagnostics
    }
}

/// Plans renderer color transform stages without executing them.
pub struct RenderColorStagePlanner<'a> {
    mode: RenderColorStageMode,
    gpu_cache: Option<&'a mut OcioGpuShaderCache>,
    gpu_options: RenderColorTransformGpuOptions,
}

/// Executes CPU-only color stage plans.
pub struct CpuRenderColorStageExecutor;

/// Result of executing a renderer color stage plan.
#[derive(Debug, Clone)]
pub struct RenderColorStageExecution<T> {
    /// Value produced by the selected executor.
    pub result: T,
    /// Diagnostics for the stage plan that was executed.
    pub stage_diagnostics: RenderColorStageDiagnostics,
}

/// Encoded RGBA8 output plus diagnostics for a final preview/export color boundary.
#[derive(Debug, Clone)]
pub struct RenderOutputColorBoundaryRgba8 {
    /// Encoded RGBA8 pixels produced by the output boundary.
    pub rgba: Vec<u8>,
    /// Color transform diagnostics emitted by the boundary executor.
    pub color_diagnostics: crate::RenderColorTransformDiagnostics,
    /// Stage diagnostics for the executed boundary plan.
    pub stage_diagnostics: RenderColorStageDiagnostics,
    /// Descriptor of the encoded output frame.
    pub output_descriptor: ColorFrameDescriptor,
}

/// Final output boundary requested by preview or export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderOutputColorBoundaryTarget {
    /// Viewer/display presentation output.
    Display,
    /// Encoded delivery/export output.
    Export,
}

/// Renderer-owned description of a working-frame to final-output color boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderOutputColorBoundary {
    /// Caller-visible output target.
    pub target: RenderOutputColorBoundaryTarget,
    /// Destination color space.
    pub output_color_space: ColorSpace,
    /// OCIO display/view pair for presentation output.
    pub display_view: Option<RenderOcioDisplayView>,
    /// Whether tone mapping is requested.
    pub tone_map: bool,
    /// Color engine selected for this output boundary.
    pub engine: ColorEngine,
}

impl RenderOutputColorBoundary {
    /// Build a display/viewer output boundary.
    pub fn display(output_color_space: ColorSpace, tone_map: bool, engine: ColorEngine) -> Self {
        Self {
            target: RenderOutputColorBoundaryTarget::Display,
            output_color_space,
            display_view: None,
            tone_map,
            engine,
        }
    }

    /// Build a display/viewer output boundary through an explicit OCIO display/view pair.
    pub fn display_view(
        output_color_space: ColorSpace,
        display: impl Into<String>,
        view: impl Into<String>,
        tone_map: bool,
        engine: ColorEngine,
    ) -> Self {
        Self {
            target: RenderOutputColorBoundaryTarget::Display,
            output_color_space,
            display_view: Some(RenderOcioDisplayView::new(display, view)),
            tone_map,
            engine,
        }
    }

    /// Build an encoded export output boundary.
    pub fn export(output_color_space: ColorSpace, tone_map: bool, engine: ColorEngine) -> Self {
        Self {
            target: RenderOutputColorBoundaryTarget::Export,
            output_color_space,
            display_view: None,
            tone_map,
            engine,
        }
    }

    /// Build an export delivery boundary through an explicit OCIO
    /// display/view pair.
    ///
    /// This carries the OCIO delivery view transform (which includes tone
    /// mapping) into the export output boundary. Unlike
    /// [`Self::display_view`] which targets preview presentation, this
    /// targets encoded export delivery.
    pub fn export_view(
        output_color_space: ColorSpace,
        display: impl Into<String>,
        view: impl Into<String>,
        tone_map: bool,
        engine: ColorEngine,
    ) -> Self {
        Self {
            target: RenderOutputColorBoundaryTarget::Export,
            output_color_space,
            display_view: Some(RenderOcioDisplayView::new(display, view)),
            tone_map,
            engine,
        }
    }

    fn transform(&self) -> RenderColorTransform {
        match self.target {
            RenderOutputColorBoundaryTarget::Display => match &self.display_view {
                Some(display_view) => RenderColorTransform::display_view(
                    self.output_color_space,
                    display_view.display.clone(),
                    display_view.view.clone(),
                    self.tone_map,
                    self.engine.clone(),
                ),
                None => RenderColorTransform::display(
                    self.output_color_space,
                    self.tone_map,
                    self.engine.clone(),
                ),
            },
            RenderOutputColorBoundaryTarget::Export => match &self.display_view {
                Some(display_view) => RenderColorTransform::delivery_view(
                    self.output_color_space,
                    display_view.display.clone(),
                    display_view.view.clone(),
                    self.tone_map,
                    self.engine.clone(),
                ),
                None => RenderColorTransform::export(
                    self.output_color_space,
                    self.tone_map,
                    self.engine.clone(),
                ),
            },
        }
    }
}

/// Planned stage graph for one final output color boundary.
#[derive(Debug, Clone)]
pub struct RenderOutputColorBoundaryStagePlan {
    /// Boundary that produced this stage plan.
    pub boundary: RenderOutputColorBoundary,
    /// Ordered renderer stages for the boundary.
    pub stage_plan: RenderColorStagePlan,
}

impl RenderOutputColorBoundaryStagePlan {
    /// Return stage diagnostics for this planned boundary.
    pub fn diagnostics(&self) -> RenderColorStageDiagnostics {
        self.stage_plan.diagnostics()
    }

    /// Build GPU resources for a blocker-free GPU output boundary stage plan.
    pub fn gpu_resource_plan(
        &self,
        ids: &mut GpuColorFrameIdAllocator,
        frame: &CpuColorFrame,
        output_texture_format: GpuColorFrameTextureFormat,
    ) -> Result<RenderGpuOutputStageResourcePlan, RenderGpuOutputStageResourcePlanError> {
        RenderGpuOutputStageResourcePlan::from_cpu_working_frame(
            ids,
            frame,
            &self.stage_plan,
            output_texture_format,
        )
    }

    /// Build GPU resources and record this final-output boundary into a command encoder.
    pub fn record_wgpu_output_boundary(
        &self,
        request: RenderGpuOutputBoundaryRecordRequest<'_>,
    ) -> Result<RenderGpuOutputStageRecord, RenderGpuOutputBoundaryRecordError> {
        let RenderGpuOutputBoundaryRecordRequest { ids, frame, output_texture_format, backend } =
            request;
        let resources = self
            .gpu_resource_plan(ids, frame, output_texture_format)
            .map_err(RenderGpuOutputBoundaryRecordError::ResourcePlan)?;
        resources
            .record_wgpu_output_stage(RenderGpuOutputStageRecordRequest { backend: backend.into() })
            .map_err(RenderGpuOutputBoundaryRecordError::Record)
    }
}

/// Borrowed backend context required to record a final-output GPU color boundary.
pub struct RenderGpuOutputBoundaryBackendContext<'a> {
    /// wgpu device used for resource materialization and bind-group creation.
    pub device: &'a wgpu::Device,
    /// wgpu queue used for upload writes.
    pub queue: &'a wgpu::Queue,
    /// Command encoder receiving the color pass and optional readback copy.
    pub encoder: &'a mut wgpu::CommandEncoder,
    /// Prepared OCIO fullscreen render pipeline.
    pub pipeline: &'a OcioGpuWgpuRenderPipeline,
    /// Prepared OCIO resource bind group.
    pub ocio_bind_group: &'a OcioGpuWgpuOcioBindGroup,
    /// Backend render-pass node for this color transform.
    pub pass_node: OcioGpuWgpuRenderPassNodePlan,
    /// Shared GPU color frame resource table.
    pub table: &'a mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
    /// Load operation for the output color attachment.
    pub load_op: wgpu::LoadOp<wgpu::Color>,
}

/// Per-record backend context for runtime-owned OCIO backend objects.
///
/// The output runtime owns OCIO shader extraction, backend preparation, concrete
/// backend-object caches, frame ids, and frame resources. This context only
/// supplies objects whose lifetime is tied to the current command submission.
pub struct RenderGpuOutputBoundaryRuntimeOwnedBackendContext<'a> {
    /// wgpu device used for resource materialization and backend-object creation.
    pub device: &'a wgpu::Device,
    /// wgpu queue used for upload writes.
    pub queue: &'a wgpu::Queue,
    /// Command encoder receiving the color pass and optional readback copy.
    pub encoder: &'a mut wgpu::CommandEncoder,
    /// Load operation for the output color attachment.
    pub load_op: wgpu::LoadOp<wgpu::Color>,
}

/// Borrowed inputs required to record one final-output GPU color boundary.
pub struct RenderGpuOutputBoundaryRecordRequest<'a> {
    /// GPU frame id allocator for upload/output handles.
    pub ids: &'a mut GpuColorFrameIdAllocator,
    /// CPU working frame entering the output boundary.
    pub frame: &'a CpuColorFrame,
    /// Texture format for the GPU output target.
    pub output_texture_format: GpuColorFrameTextureFormat,
    /// Backend context used to materialize resources and record the pass.
    pub backend: RenderGpuOutputBoundaryBackendContext<'a>,
}

/// Error returned when a final-output GPU boundary cannot be recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderGpuOutputBoundaryRecordError {
    /// The boundary stage plan could not produce GPU resources.
    ResourcePlan(RenderGpuOutputStageResourcePlanError),
    /// Resource materialization, pass recording, or readback recording failed.
    Record(RenderGpuOutputStageRecordError),
}

/// Error returned when runtime-owned GPU backend objects cannot record a final boundary.
#[derive(Debug, PartialEq, Eq)]
pub enum RenderGpuOutputBoundaryRuntimeRecordError {
    /// The output boundary could not be planned.
    Plan(RenderColorTransformError),
    /// The planned boundary could not produce GPU resources.
    ResourcePlan(RenderGpuOutputStageResourcePlanError),
    /// Pure OCIO backend contracts could not be prepared.
    BackendPrep(OcioGpuWgpuBackendPrepError),
    /// Concrete wgpu backend objects could not be prepared.
    BackendObjects(OcioGpuWgpuBackendObjectError),
    /// Resource materialization, pass recording, or readback recording failed.
    Record(RenderGpuOutputStageRecordError),
}

/// Error returned when runtime-owned GPU backend objects cannot record an input stage.
#[derive(Debug, PartialEq, Eq)]
pub enum RenderGpuInputStageRuntimeRecordError {
    /// The input transform could not be planned.
    Plan(RenderColorTransformError),
    /// The planned input stage could not produce GPU resources.
    ResourcePlan(RenderGpuInputStageResourcePlanError),
    /// Pure OCIO backend contracts could not be prepared.
    BackendPrep(OcioGpuWgpuBackendPrepError),
    /// Concrete wgpu backend objects could not be prepared.
    BackendObjects(OcioGpuWgpuBackendObjectError),
    /// Resource materialization or pass recording failed.
    Record(RenderGpuOutputStageRecordError),
}

/// Renderer-owned state for native GPU final-output color boundaries.
///
/// App/export code should hold one runtime per render backend lifetime. The
/// runtime owns renderer-internal color resources, OCIO shader extraction,
/// backend-object preparation, and executor-level recording so callers only pass
/// per-submission wgpu objects.
pub struct RenderGpuOutputBoundaryRuntime {
    shader_cache: OcioGpuShaderCache,
    backend_prep: OcioGpuWgpuBackendPrepRuntime,
    backend_objects: OcioGpuWgpuBackendObjectRuntime,
    frame_ids: GpuColorFrameIdAllocator,
    frame_table: GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
}

impl RenderGpuOutputBoundaryRuntime {
    /// Create a runtime with default cache capacity and frame ids starting at 1.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a runtime with default cache capacity and a custom first frame id.
    pub fn with_first_frame_id(first_frame_id: u64) -> Self {
        Self {
            shader_cache: OcioGpuShaderCache::default(),
            backend_prep: OcioGpuWgpuBackendPrepRuntime::default(),
            backend_objects: OcioGpuWgpuBackendObjectRuntime::default(),
            frame_ids: GpuColorFrameIdAllocator::new(first_frame_id),
            frame_table: GpuColorFrameResourceTable::new(),
        }
    }

    /// Return point-in-time runtime diagnostics.
    pub fn diagnostics(&self) -> RenderGpuOutputBoundaryRuntimeDiagnostics {
        RenderGpuOutputBoundaryRuntimeDiagnostics {
            shader_cache: self.shader_cache.diagnostics(),
            backend_prep: self.backend_prep.diagnostics(),
            backend_objects: self.backend_objects.diagnostics(),
            next_frame_id: self.frame_ids.next_raw(),
            frame_table_entries: self.frame_table.len(),
        }
    }

    /// Remove all materialized frame resources owned by this runtime.
    pub fn clear_frame_resources(&mut self) {
        self.frame_table.clear();
    }

    /// Borrow the runtime-owned GPU frame resource table.
    pub fn frame_table(&self) -> &GpuColorFrameResourceTable<GpuColorFrameWgpuResource> {
        &self.frame_table
    }

    /// Mutably borrow the runtime-owned GPU frame resource table.
    pub fn frame_table_mut(
        &mut self,
    ) -> &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource> {
        &mut self.frame_table
    }

    /// Mutably borrow the allocator that owns frame identities for this
    /// runtime's resource table. Native import plans must allocate from this
    /// same namespace before their returned resources enter the table.
    pub fn frame_ids_mut(&mut self) -> &mut GpuColorFrameIdAllocator {
        &mut self.frame_ids
    }

    /// Borrow the runtime-owned OCIO shader cache.
    pub fn shader_cache(&self) -> &OcioGpuShaderCache {
        &self.shader_cache
    }

    /// Mutably borrow the runtime-owned OCIO shader cache.
    pub fn shader_cache_mut(&mut self) -> &mut OcioGpuShaderCache {
        &mut self.shader_cache
    }

    /// Borrow the runtime-owned pure backend-preparation cache.
    pub fn backend_prep(&self) -> &OcioGpuWgpuBackendPrepRuntime {
        &self.backend_prep
    }

    /// Mutably borrow the runtime-owned pure backend-preparation cache.
    pub fn backend_prep_mut(&mut self) -> &mut OcioGpuWgpuBackendPrepRuntime {
        &mut self.backend_prep
    }

    /// Borrow the runtime-owned concrete backend-object cache.
    pub fn backend_objects(&self) -> &OcioGpuWgpuBackendObjectRuntime {
        &self.backend_objects
    }

    /// Mutably borrow the runtime-owned concrete backend-object cache.
    pub fn backend_objects_mut(&mut self) -> &mut OcioGpuWgpuBackendObjectRuntime {
        &mut self.backend_objects
    }

    /// Record a native GPU working-space composite into this runtime's shared
    /// frame resource table.
    ///
    /// The returned GPU working frame can be passed directly to
    /// [`Self::record_wgpu_output_boundary_gpu_frame_owned_backend`], avoiding
    /// the legacy CPU working-frame composite before preview output.
    pub fn record_wgpu_working_composite(
        &mut self,
        compositor: &GpuFrameCompositor,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        request: GpuCompositeRequest<'_>,
    ) -> Result<GpuCompositeRecord, GpuCompositeError> {
        let Self { frame_ids, frame_table, .. } = self;
        compositor.record(device, queue, encoder, frame_ids, frame_table, request)
    }

    /// Plan, prepare runtime-owned backend objects, and record a native GPU
    /// source/input color transform from decoded CPU RGBA8 into GPU working space.
    pub fn record_wgpu_input_stage_owned_backend(
        &mut self,
        transform: &RenderInputTransform,
        frame: &CpuEncodedColorFrame,
        gpu_options: RenderColorTransformGpuOptions,
        backend: RenderGpuOutputBoundaryRuntimeOwnedBackendContext<'_>,
    ) -> Result<RenderGpuInputStageRecord, RenderGpuInputStageRuntimeRecordError> {
        let Self {
            shader_cache,
            backend_prep,
            backend_objects,
            frame_ids,
            frame_table,
        } = self;
        let mut planner = RenderColorStagePlanner::prefer_gpu(shader_cache, gpu_options);
        let stage_plan = planner
            .plan_input_to_working(frame.descriptor(), transform)
            .map_err(RenderGpuInputStageRuntimeRecordError::Plan)?;
        let resources = RenderGpuInputStageResourcePlan::from_cpu_encoded_source_frame(
            frame_ids,
            frame,
            &stage_plan,
        )
        .map_err(RenderGpuInputStageRuntimeRecordError::ResourcePlan)?;
        let output_format = color_target_format_for_gpu_frame(&resources.output);
        let shader_plan = resources.transform.wgpu.shader_plan.clone();
        let static_pipeline = backend_prep
            .prepare_static_pipeline(&shader_plan, output_format)
            .map_err(RenderGpuInputStageRuntimeRecordError::BackendPrep)?;
        let prepared_backend = backend_objects
            .prepare_backend_objects(
                backend.device,
                backend.queue,
                &shader_plan,
                &static_pipeline,
            )
            .map_err(RenderGpuInputStageRuntimeRecordError::BackendObjects)?;
        resources
            .record_wgpu_input_stage(RenderGpuOutputStageRecordRequest {
                backend: RenderGpuOutputStageBackendContext {
                    device: backend.device,
                    queue: backend.queue,
                    encoder: backend.encoder,
                    pipeline: &prepared_backend.render_pipeline,
                    ocio_bind_group: &prepared_backend.ocio_bind_group,
                    pass_node: prepared_backend.pass_node,
                    table: frame_table,
                    load_op: backend.load_op,
                },
            })
            .map_err(RenderGpuInputStageRuntimeRecordError::Record)
    }

    /// Plan and record the OCIO input transform for an encoded-float source
    /// frame that is already present in this runtime's GPU resource table.
    ///
    /// Native YUV backends use this after their sampling pass. Both handles
    /// come from the native import plan, so this method validates and preserves
    /// that plan rather than allocating replacement frame identities.
    pub fn record_wgpu_input_stage_gpu_frame_owned_backend(
        &mut self,
        transform: &RenderInputTransform,
        input: &GpuColorFrameHandle,
        output: &GpuColorFrameHandle,
        gpu_options: RenderColorTransformGpuOptions,
        backend: RenderGpuOutputBoundaryRuntimeOwnedBackendContext<'_>,
    ) -> Result<RenderGpuInputStageRecord, RenderGpuInputStageRuntimeRecordError> {
        let Self {
            shader_cache,
            backend_prep,
            backend_objects,
            frame_table,
            ..
        } = self;
        let mut planner = RenderColorStagePlanner::prefer_gpu(shader_cache, gpu_options);
        let stage_plan = planner
            .plan_input_to_working(input.descriptor(), transform)
            .map_err(RenderGpuInputStageRuntimeRecordError::Plan)?;
        let resources = RenderGpuInputStageResourcePlan::from_gpu_encoded_source_frame(
            input,
            output,
            &stage_plan,
        )
        .map_err(RenderGpuInputStageRuntimeRecordError::ResourcePlan)?;
        let output_format = color_target_format_for_gpu_frame(&resources.output);
        let shader_plan = resources.transform.wgpu.shader_plan.clone();
        let static_pipeline = backend_prep
            .prepare_static_pipeline(&shader_plan, output_format)
            .map_err(RenderGpuInputStageRuntimeRecordError::BackendPrep)?;
        let prepared_backend = backend_objects
            .prepare_backend_objects(
                backend.device,
                backend.queue,
                &shader_plan,
                &static_pipeline,
            )
            .map_err(RenderGpuInputStageRuntimeRecordError::BackendObjects)?;
        resources
            .record_wgpu_input_stage(RenderGpuOutputStageRecordRequest {
                backend: RenderGpuOutputStageBackendContext {
                    device: backend.device,
                    queue: backend.queue,
                    encoder: backend.encoder,
                    pipeline: &prepared_backend.render_pipeline,
                    ocio_bind_group: &prepared_backend.ocio_bind_group,
                    pass_node: prepared_backend.pass_node,
                    table: frame_table,
                    load_op: backend.load_op,
                },
            })
            .map_err(RenderGpuInputStageRuntimeRecordError::Record)
    }

    /// Plan, prepare runtime-owned backend objects, and record a native GPU output boundary.
    pub fn record_wgpu_output_boundary_owned_backend(
        &mut self,
        boundary: &RenderOutputColorBoundary,
        frame: &CpuColorFrame,
        output_texture_format: GpuColorFrameTextureFormat,
        gpu_options: RenderColorTransformGpuOptions,
        backend: RenderGpuOutputBoundaryRuntimeOwnedBackendContext<'_>,
    ) -> Result<RenderGpuOutputStageRecord, RenderGpuOutputBoundaryRuntimeRecordError> {
        let Self {
            shader_cache,
            backend_prep,
            backend_objects,
            frame_ids,
            frame_table,
        } = self;
        let mut planner = RenderOutputColorBoundaryPlanner::prefer_gpu(shader_cache, gpu_options);
        let plan = planner
            .plan_descriptor_for_texture(frame.descriptor(), boundary, output_texture_format)
            .map_err(RenderGpuOutputBoundaryRuntimeRecordError::Plan)?;
        let resources = plan
            .gpu_resource_plan(frame_ids, frame, output_texture_format)
            .map_err(RenderGpuOutputBoundaryRuntimeRecordError::ResourcePlan)?;
        let output_format = color_target_format_for_gpu_frame(&resources.output);
        let shader_plan = resources.transform.wgpu.shader_plan.clone();
        let static_pipeline = backend_prep
            .prepare_static_pipeline(&shader_plan, output_format)
            .map_err(RenderGpuOutputBoundaryRuntimeRecordError::BackendPrep)?;
        let prepared_backend = backend_objects
            .prepare_backend_objects(
                backend.device,
                backend.queue,
                &shader_plan,
                &static_pipeline,
            )
            .map_err(RenderGpuOutputBoundaryRuntimeRecordError::BackendObjects)?;
        resources
            .record_wgpu_output_stage(RenderGpuOutputStageRecordRequest {
                backend: RenderGpuOutputStageBackendContext {
                    device: backend.device,
                    queue: backend.queue,
                    encoder: backend.encoder,
                    pipeline: &prepared_backend.render_pipeline,
                    ocio_bind_group: &prepared_backend.ocio_bind_group,
                    pass_node: prepared_backend.pass_node,
                    table: frame_table,
                    load_op: backend.load_op,
                },
            })
            .map_err(RenderGpuOutputBoundaryRuntimeRecordError::Record)
    }

    /// Plan, prepare runtime-owned backend objects, and record a native GPU
    /// output boundary from an upstream GPU-resident working frame.
    pub fn record_wgpu_output_boundary_gpu_frame_owned_backend(
        &mut self,
        boundary: &RenderOutputColorBoundary,
        input: &GpuColorFrameHandle,
        output_texture_format: GpuColorFrameTextureFormat,
        gpu_options: RenderColorTransformGpuOptions,
        backend: RenderGpuOutputBoundaryRuntimeOwnedBackendContext<'_>,
    ) -> Result<RenderGpuOutputStageRecord, RenderGpuOutputBoundaryRuntimeRecordError> {
        let Self {
            shader_cache,
            backend_prep,
            backend_objects,
            frame_ids,
            frame_table,
        } = self;
        let mut planner = RenderOutputColorBoundaryPlanner::prefer_gpu(shader_cache, gpu_options);
        let plan = planner
            .plan_descriptor_for_texture(input.descriptor(), boundary, output_texture_format)
            .map_err(RenderGpuOutputBoundaryRuntimeRecordError::Plan)?;
        let resources = RenderGpuOutputStageResourcePlan::from_gpu_working_frame(
            frame_ids,
            input,
            &plan.stage_plan,
            output_texture_format,
        )
        .map_err(RenderGpuOutputBoundaryRuntimeRecordError::ResourcePlan)?;
        let output_format = color_target_format_for_gpu_frame(&resources.output);
        let shader_plan = resources.transform.wgpu.shader_plan.clone();
        let static_pipeline = backend_prep
            .prepare_static_pipeline(&shader_plan, output_format)
            .map_err(RenderGpuOutputBoundaryRuntimeRecordError::BackendPrep)?;
        let prepared_backend = backend_objects
            .prepare_backend_objects(
                backend.device,
                backend.queue,
                &shader_plan,
                &static_pipeline,
            )
            .map_err(RenderGpuOutputBoundaryRuntimeRecordError::BackendObjects)?;
        resources
            .record_wgpu_output_stage(RenderGpuOutputStageRecordRequest {
                backend: RenderGpuOutputStageBackendContext {
                    device: backend.device,
                    queue: backend.queue,
                    encoder: backend.encoder,
                    pipeline: &prepared_backend.render_pipeline,
                    ocio_bind_group: &prepared_backend.ocio_bind_group,
                    pass_node: prepared_backend.pass_node,
                    table: frame_table,
                    load_op: backend.load_op,
                },
            })
            .map_err(RenderGpuOutputBoundaryRuntimeRecordError::Record)
    }
}

impl Default for RenderGpuOutputBoundaryRuntime {
    fn default() -> Self {
        Self::with_first_frame_id(1)
    }
}

/// Point-in-time diagnostics for a GPU output boundary runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderGpuOutputBoundaryRuntimeDiagnostics {
    /// OCIO shader extraction cache diagnostics.
    pub shader_cache: OcioGpuShaderCacheDiagnostics,
    /// Pure backend-preparation cache diagnostics.
    pub backend_prep: OcioGpuWgpuBackendPrepRuntimeDiagnostics,
    /// Concrete backend-object cache diagnostics.
    pub backend_objects: OcioGpuWgpuBackendObjectRuntimeDiagnostics,
    /// Next GPU color frame id that will be allocated.
    pub next_frame_id: u64,
    /// Number of materialized frame resources currently retained.
    pub frame_table_entries: usize,
}

/// Schema version for renderer GPU output health reports.
pub const RENDER_GPU_OUTPUT_HEALTH_REPORT_SCHEMA_VERSION: u32 = 1;

/// Serializable frame evidence for renderer GPU output diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RenderGpuOutputFrameReport {
    /// Output frame width in pixels.
    pub width: usize,
    /// Output frame height in pixels.
    pub height: usize,
    /// Total pixel count represented by this boundary sample.
    pub pixel_count: usize,
    /// Working/input color space entering the output boundary.
    pub input_color_space: ColorSpace,
    /// Requested display/export color space leaving the output boundary.
    pub output_color_space: ColorSpace,
}

impl RenderGpuOutputFrameReport {
    /// Return the expected encoded RGBA8 readback byte count for this frame.
    pub fn expected_readback_bytes(&self) -> usize {
        self.pixel_count.saturating_mul(4)
    }
}

/// Serializable stage evidence for renderer GPU output diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderGpuOutputStageDiagnosticsReport {
    /// Total scheduled stages for this output boundary.
    pub total_stages: u64,
    /// CPU->GPU upload stages.
    pub upload_stages: u64,
    /// Native GPU color-transform stages.
    pub gpu_color_stages: u64,
    /// GPU->CPU readback stages.
    pub readback_stages: u64,
    /// Native GPU blockers exposed by the stage plan.
    pub gpu_blockers: u64,
    /// Structured native GPU blocker reasons.
    pub gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown,
    /// Sum of pixels touched by the scheduled stages.
    pub stage_pixels: u64,
}

impl From<RenderColorStageDiagnostics> for RenderGpuOutputStageDiagnosticsReport {
    fn from(diagnostics: RenderColorStageDiagnostics) -> Self {
        Self {
            total_stages: diagnostics.total_stages,
            upload_stages: diagnostics.upload_stages,
            gpu_color_stages: diagnostics.gpu_color_stages,
            readback_stages: diagnostics.readback_stages,
            gpu_blockers: diagnostics.gpu_blockers,
            gpu_blocker_breakdown: diagnostics.gpu_blocker_breakdown,
            stage_pixels: diagnostics.stage_pixels,
        }
    }
}

/// Serializable runtime evidence for renderer GPU output diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderGpuOutputRuntimeDiagnosticsReport {
    /// Number of prepared shader-cache entries.
    pub shader_cache_entries: usize,
    /// Shader-cache hits observed by the runtime.
    pub shader_cache_hits: u64,
    /// Shader-cache misses observed by the runtime.
    pub shader_cache_misses: u64,
    /// Shader extraction failures observed by the runtime.
    pub shader_cache_extraction_failures: u64,
    /// Number of prepared pure backend resource entries.
    pub backend_prep_resource_entries: usize,
    /// Number of prepared concrete backend object entries.
    pub backend_object_entries: usize,
    /// Concrete backend-object cache hits observed by the runtime.
    pub backend_object_hits: u64,
    /// Concrete backend-object cache misses observed by the runtime.
    pub backend_object_misses: u64,
    /// Concrete backend-object preparation failures observed by the runtime.
    pub backend_object_failures: u64,
    /// Number of GPU frame-table entries retained by the runtime.
    pub frame_table_entries: usize,
    /// Next GPU frame id that will be allocated.
    pub next_frame_id: u64,
}

impl From<RenderGpuOutputBoundaryRuntimeDiagnostics> for RenderGpuOutputRuntimeDiagnosticsReport {
    fn from(diagnostics: RenderGpuOutputBoundaryRuntimeDiagnostics) -> Self {
        Self {
            shader_cache_entries: diagnostics.shader_cache.entries,
            shader_cache_hits: diagnostics.shader_cache.hits,
            shader_cache_misses: diagnostics.shader_cache.misses,
            shader_cache_extraction_failures: diagnostics.shader_cache.extraction_failures,
            backend_prep_resource_entries: diagnostics.backend_prep.resources.entries,
            backend_object_entries: diagnostics.backend_objects.entries,
            backend_object_hits: diagnostics.backend_objects.hits,
            backend_object_misses: diagnostics.backend_objects.misses,
            backend_object_failures: diagnostics.backend_objects.failures,
            frame_table_entries: diagnostics.frame_table_entries,
            next_frame_id: diagnostics.next_frame_id,
        }
    }
}

/// High-level health state derived from one renderer GPU output sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderGpuOutputHealthStatus {
    /// The smoke could not exercise a real GPU boundary and was skipped.
    Skipped,
    /// The GPU boundary completed and matched the CPU reference within tolerance.
    Passed,
    /// The GPU boundary produced evidence but violated at least one fail-closed check.
    Failed,
}

/// Derived summary for one renderer GPU output sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RenderGpuOutputHealthSummary {
    /// High-level sample status.
    pub status: RenderGpuOutputHealthStatus,
    /// Whether the real native GPU output boundary was fully ready.
    pub native_gpu_output_ready: bool,
    /// Whether the stage sequence contained upload + GPU color + readback.
    pub complete_stage_sequence: bool,
    /// Whether the stage plan reported zero native GPU blockers.
    pub no_gpu_blockers: bool,
    /// Whether runtime backend preparation/object caches were warmed successfully.
    pub backend_runtime_ready: bool,
    /// Whether the OCIO shader cache warmed without extraction failures.
    pub shader_cache_warmed: bool,
    /// Whether the observed readback matched the expected byte count.
    pub readback_complete: bool,
    /// Whether CPU/GPU parity stayed within the configured tolerance.
    pub parity_within_tolerance: bool,
    /// Expected encoded RGBA8 readback bytes for this boundary sample.
    pub expected_readback_bytes: usize,
}

impl RenderGpuOutputHealthSummary {
    /// Evaluate one renderer GPU output sample into the stable summary contract.
    pub fn evaluate(
        skipped: bool,
        frame: &RenderGpuOutputFrameReport,
        stage: &RenderGpuOutputStageDiagnosticsReport,
        runtime: &RenderGpuOutputRuntimeDiagnosticsReport,
        observed_readback_bytes: usize,
        max_rgba_delta: u8,
        tolerance: u8,
    ) -> Self {
        let expected_readback_bytes = frame.expected_readback_bytes();
        let complete_stage_sequence = stage.total_stages == 3
            && stage.upload_stages == 1
            && stage.gpu_color_stages == 1
            && stage.readback_stages == 1;
        let no_gpu_blockers = stage.gpu_blockers == 0 && stage.gpu_blocker_breakdown.total() == 0;
        let shader_cache_warmed = runtime.shader_cache_entries > 0
            && runtime.shader_cache_misses > 0
            && runtime.shader_cache_extraction_failures == 0;
        let backend_runtime_ready = runtime.backend_prep_resource_entries > 0
            && runtime.backend_object_entries > 0
            && runtime.backend_object_misses > 0
            && runtime.backend_object_failures == 0;
        let readback_complete =
            expected_readback_bytes > 0 && observed_readback_bytes == expected_readback_bytes;
        let parity_within_tolerance = max_rgba_delta <= tolerance;
        let native_gpu_output_ready = !skipped
            && complete_stage_sequence
            && no_gpu_blockers
            && shader_cache_warmed
            && backend_runtime_ready
            && readback_complete;
        let status = if skipped {
            RenderGpuOutputHealthStatus::Skipped
        } else if native_gpu_output_ready && parity_within_tolerance {
            RenderGpuOutputHealthStatus::Passed
        } else {
            RenderGpuOutputHealthStatus::Failed
        };

        Self {
            status,
            native_gpu_output_ready,
            complete_stage_sequence,
            no_gpu_blockers,
            backend_runtime_ready,
            shader_cache_warmed,
            readback_complete,
            parity_within_tolerance,
            expected_readback_bytes,
        }
    }
}

/// Overall verdict for a renderer GPU output health report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RenderGpuOutputHealthVerdict {
    /// The sample satisfied all fail-closed checks.
    Pass,
    /// The sample violated at least one fail-closed check.
    Fail,
}

/// Diagnostic area used by renderer GPU output health checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RenderGpuOutputDiagnosticArea {
    /// Smoke capture completeness and readback integrity.
    CaptureIntegrity,
    /// Renderer GPU stage scheduling and blocker health.
    GpuColorPath,
    /// OCIO shader-cache and backend runtime readiness.
    BackendRuntime,
    /// CPU/GPU parity against the reference boundary.
    Parity,
}

/// Severity for one renderer GPU output health check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RenderGpuOutputHealthSeverity {
    /// The check passed.
    Pass,
    /// The check failed.
    Fail,
}

/// One renderer GPU output health check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderGpuOutputHealthCheck {
    /// Diagnostic area this check belongs to.
    pub area: RenderGpuOutputDiagnosticArea,
    /// Stable machine-readable check code.
    pub code: &'static str,
    /// Check severity.
    pub severity: RenderGpuOutputHealthSeverity,
    /// Observed value.
    pub observed: u64,
    /// Optional threshold or target.
    pub limit: Option<u64>,
}

/// One prioritized renderer GPU output root cause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderGpuOutputHealthRootCause {
    /// Diagnostic area this root cause belongs to.
    pub area: RenderGpuOutputDiagnosticArea,
    /// Stable machine-readable root-cause code.
    pub code: &'static str,
    /// Root-cause severity.
    pub severity: RenderGpuOutputHealthSeverity,
    /// Compact evidence string.
    pub evidence: String,
}

/// One suggested follow-up action for a renderer GPU output sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderGpuOutputHealthAction {
    /// Diagnostic area this action belongs to.
    pub area: RenderGpuOutputDiagnosticArea,
    /// Stable machine-readable action code.
    pub code: &'static str,
    /// Human-readable action.
    pub description: &'static str,
}

/// Compact evidence payload embedded in renderer GPU output health reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderGpuOutputHealthEvidence {
    /// Skip reason when the smoke could not acquire a real adapter.
    pub skipped_reason: Option<String>,
    /// Expected encoded RGBA8 readback byte count.
    pub expected_readback_bytes: usize,
    /// Observed encoded RGBA8 readback byte count.
    pub observed_readback_bytes: usize,
    /// Maximum channel delta observed between CPU and GPU output.
    pub max_rgba_delta: u8,
    /// Allowed parity tolerance used for this sample.
    pub tolerance: u8,
    /// Structured native GPU blocker breakdown for this sample.
    pub gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown,
    /// Number of shader-cache entries retained by the runtime.
    pub shader_cache_entries: usize,
    /// Number of concrete backend-object entries retained by the runtime.
    pub backend_object_entries: usize,
}

/// Versioned renderer GPU output health report for smoke output and tooling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderGpuOutputHealthReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Applied report profile.
    pub profile: String,
    /// Overall health verdict.
    pub verdict: RenderGpuOutputHealthVerdict,
    /// Structured summary derived from the sample.
    pub summary: RenderGpuOutputHealthSummary,
    /// Stable checks that explain the verdict.
    pub checks: Vec<RenderGpuOutputHealthCheck>,
    /// Prioritized machine-readable root causes.
    pub root_causes: Vec<RenderGpuOutputHealthRootCause>,
    /// Suggested engineering follow-up actions.
    pub actions: Vec<RenderGpuOutputHealthAction>,
    /// Compact evidence pointers for dashboards and tooling.
    pub evidence: RenderGpuOutputHealthEvidence,
}

impl RenderGpuOutputHealthReport {
    /// Build the health report for one renderer GPU output sample.
    #[allow(clippy::too_many_arguments)]
    pub fn from_sample(
        profile: impl Into<String>,
        skipped_reason: Option<String>,
        frame: &RenderGpuOutputFrameReport,
        stage: &RenderGpuOutputStageDiagnosticsReport,
        runtime: &RenderGpuOutputRuntimeDiagnosticsReport,
        observed_readback_bytes: usize,
        max_rgba_delta: u8,
        tolerance: u8,
    ) -> Self {
        let summary = RenderGpuOutputHealthSummary::evaluate(
            skipped_reason.is_some(),
            frame,
            stage,
            runtime,
            observed_readback_bytes,
            max_rgba_delta,
            tolerance,
        );
        Self::build(
            profile,
            skipped_reason,
            summary,
            stage,
            runtime,
            observed_readback_bytes,
            max_rgba_delta,
            tolerance,
        )
    }

    /// Build the health report from a precomputed renderer GPU output summary.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        profile: impl Into<String>,
        skipped_reason: Option<String>,
        summary: RenderGpuOutputHealthSummary,
        stage: &RenderGpuOutputStageDiagnosticsReport,
        runtime: &RenderGpuOutputRuntimeDiagnosticsReport,
        observed_readback_bytes: usize,
        max_rgba_delta: u8,
        tolerance: u8,
    ) -> Self {
        let mut checks = Vec::new();
        let mut root_causes = Vec::new();
        let mut actions = Vec::new();

        push_render_gpu_output_bool_check(
            &mut checks,
            RenderGpuOutputDiagnosticArea::CaptureIntegrity,
            "not_skipped",
            summary.status != RenderGpuOutputHealthStatus::Skipped,
        );
        push_render_gpu_output_bool_check(
            &mut checks,
            RenderGpuOutputDiagnosticArea::GpuColorPath,
            "native_gpu_output_ready",
            summary.native_gpu_output_ready,
        );
        push_render_gpu_output_bool_check(
            &mut checks,
            RenderGpuOutputDiagnosticArea::GpuColorPath,
            "complete_stage_sequence",
            summary.complete_stage_sequence,
        );
        push_render_gpu_output_bool_check(
            &mut checks,
            RenderGpuOutputDiagnosticArea::GpuColorPath,
            "no_gpu_blockers",
            summary.no_gpu_blockers,
        );
        push_render_gpu_output_bool_check(
            &mut checks,
            RenderGpuOutputDiagnosticArea::BackendRuntime,
            "backend_runtime_ready",
            summary.backend_runtime_ready,
        );
        push_render_gpu_output_bool_check(
            &mut checks,
            RenderGpuOutputDiagnosticArea::BackendRuntime,
            "shader_cache_warmed",
            summary.shader_cache_warmed,
        );
        push_render_gpu_output_bool_check(
            &mut checks,
            RenderGpuOutputDiagnosticArea::CaptureIntegrity,
            "readback_complete",
            summary.readback_complete,
        );
        checks.push(RenderGpuOutputHealthCheck {
            area: RenderGpuOutputDiagnosticArea::Parity,
            code: "parity_within_tolerance",
            severity: if summary.parity_within_tolerance {
                RenderGpuOutputHealthSeverity::Pass
            } else {
                RenderGpuOutputHealthSeverity::Fail
            },
            observed: u64::from(max_rgba_delta),
            limit: Some(u64::from(tolerance)),
        });

        push_render_gpu_output_root_causes_and_actions(
            skipped_reason.clone(),
            &summary,
            stage,
            runtime,
            observed_readback_bytes,
            max_rgba_delta,
            tolerance,
            &mut root_causes,
            &mut actions,
        );

        let verdict =
            if checks.iter().any(|check| check.severity == RenderGpuOutputHealthSeverity::Fail) {
                RenderGpuOutputHealthVerdict::Fail
            } else {
                RenderGpuOutputHealthVerdict::Pass
            };

        Self {
            schema_version: RENDER_GPU_OUTPUT_HEALTH_REPORT_SCHEMA_VERSION,
            profile: profile.into(),
            verdict,
            summary,
            checks,
            root_causes,
            actions,
            evidence: RenderGpuOutputHealthEvidence {
                skipped_reason,
                expected_readback_bytes: summary.expected_readback_bytes,
                observed_readback_bytes,
                max_rgba_delta,
                tolerance,
                gpu_blocker_breakdown: stage.gpu_blocker_breakdown,
                shader_cache_entries: runtime.shader_cache_entries,
                backend_object_entries: runtime.backend_object_entries,
            },
        }
    }
}

fn push_render_gpu_output_bool_check(
    checks: &mut Vec<RenderGpuOutputHealthCheck>,
    area: RenderGpuOutputDiagnosticArea,
    code: &'static str,
    passed: bool,
) {
    checks.push(RenderGpuOutputHealthCheck {
        area,
        code,
        severity: if passed {
            RenderGpuOutputHealthSeverity::Pass
        } else {
            RenderGpuOutputHealthSeverity::Fail
        },
        observed: if passed { 1 } else { 0 },
        limit: Some(1),
    });
}

#[allow(clippy::too_many_arguments)]
fn push_render_gpu_output_root_causes_and_actions(
    skipped_reason: Option<String>,
    summary: &RenderGpuOutputHealthSummary,
    stage: &RenderGpuOutputStageDiagnosticsReport,
    runtime: &RenderGpuOutputRuntimeDiagnosticsReport,
    observed_readback_bytes: usize,
    max_rgba_delta: u8,
    tolerance: u8,
    root_causes: &mut Vec<RenderGpuOutputHealthRootCause>,
    actions: &mut Vec<RenderGpuOutputHealthAction>,
) {
    if let Some(reason) = skipped_reason {
        push_render_gpu_output_root_cause_with_action(
            root_causes,
            actions,
            RenderGpuOutputDiagnosticArea::CaptureIntegrity,
            "gpu_adapter_unavailable",
            reason,
            "provision_real_wgpu_adapter",
            "Run the smoke on a machine where wgpu can acquire a real adapter.",
        );
    }
    if !summary.complete_stage_sequence {
        push_render_gpu_output_root_cause_with_action(
            root_causes,
            actions,
            RenderGpuOutputDiagnosticArea::GpuColorPath,
            "gpu_stage_sequence_incomplete",
            format!(
                "total_stages={} upload={} gpu={} readback={}",
                stage.total_stages,
                stage.upload_stages,
                stage.gpu_color_stages,
                stage.readback_stages
            ),
            "inspect_gpu_stage_sequence",
            "Inspect upload, GPU color, and readback stage sequencing on the real boundary.",
        );
    }
    if !summary.no_gpu_blockers {
        push_render_gpu_output_root_cause_with_action(
            root_causes,
            actions,
            RenderGpuOutputDiagnosticArea::GpuColorPath,
            "gpu_stage_blocked",
            format!(
                "gpu_blockers={} shader={} ocio_resource={} wrapper={} pipeline={} \
                 ocio_config={} ocio_processor={} shader_extraction={}",
                stage.gpu_blockers,
                stage.gpu_blocker_breakdown.shader_module_not_prepared,
                stage.gpu_blocker_breakdown.ocio_resource_bind_group_not_prepared,
                stage.gpu_blocker_breakdown.fullscreen_wrapper_not_prepared,
                stage.gpu_blocker_breakdown.render_pipeline_not_prepared,
                stage.gpu_blocker_breakdown.ocio_config_not_loaded,
                stage.gpu_blocker_breakdown.ocio_processor_unavailable,
                stage.gpu_blocker_breakdown.ocio_gpu_shader_extraction_failed
            ),
            "inspect_gpu_blocker_breakdown",
            "Inspect shader module, OCIO resource, fullscreen wrapper, render pipeline, OCIO config, processor, and shader extraction blockers.",
        );
    }
    if !summary.backend_runtime_ready {
        push_render_gpu_output_root_cause_with_action(
            root_causes,
            actions,
            RenderGpuOutputDiagnosticArea::BackendRuntime,
            "backend_runtime_not_ready",
            format!(
                "prep_entries={} backend_entries={} backend_misses={} backend_failures={}",
                runtime.backend_prep_resource_entries,
                runtime.backend_object_entries,
                runtime.backend_object_misses,
                runtime.backend_object_failures
            ),
            "inspect_backend_runtime",
            "Inspect backend prep/object caches and runtime object materialization for the smoke boundary.",
        );
    }
    if !summary.shader_cache_warmed {
        push_render_gpu_output_root_cause_with_action(
            root_causes,
            actions,
            RenderGpuOutputDiagnosticArea::BackendRuntime,
            "shader_cache_not_ready",
            format!(
                "shader_cache_entries={} hits={} misses={} extraction_failures={}",
                runtime.shader_cache_entries,
                runtime.shader_cache_hits,
                runtime.shader_cache_misses,
                runtime.shader_cache_extraction_failures
            ),
            "inspect_shader_cache_extraction",
            "Inspect OCIO shader extraction and cache warming for the real GPU output boundary.",
        );
    }
    if !summary.readback_complete {
        push_render_gpu_output_root_cause_with_action(
            root_causes,
            actions,
            RenderGpuOutputDiagnosticArea::CaptureIntegrity,
            "gpu_readback_incomplete",
            format!(
                "observed_readback_bytes={} expected_readback_bytes={}",
                observed_readback_bytes, summary.expected_readback_bytes
            ),
            "inspect_gpu_readback",
            "Inspect GPU readback byte count and output texture/readback plan compatibility.",
        );
    }
    if !summary.parity_within_tolerance {
        push_render_gpu_output_root_cause_with_action(
            root_causes,
            actions,
            RenderGpuOutputDiagnosticArea::Parity,
            "cpu_gpu_parity_out_of_tolerance",
            format!("max_rgba_delta={} tolerance={}", max_rgba_delta, tolerance),
            "inspect_cpu_gpu_parity",
            "Inspect CPU/GPU output parity and verify OCIO + wrapper execution matches the CPU reference path.",
        );
    }
}

fn push_render_gpu_output_root_cause_with_action(
    root_causes: &mut Vec<RenderGpuOutputHealthRootCause>,
    actions: &mut Vec<RenderGpuOutputHealthAction>,
    area: RenderGpuOutputDiagnosticArea,
    root_code: &'static str,
    evidence: String,
    action_code: &'static str,
    action_description: &'static str,
) {
    if !root_causes.iter().any(|root| root.code == root_code) {
        root_causes.push(RenderGpuOutputHealthRootCause {
            area,
            code: root_code,
            severity: RenderGpuOutputHealthSeverity::Fail,
            evidence,
        });
    }
    if !actions.iter().any(|action| action.code == action_code) {
        actions.push(RenderGpuOutputHealthAction {
            area,
            code: action_code,
            description: action_description,
        });
    }
}

/// Strategy-aware planner for preview/export final output color boundaries.
pub struct RenderOutputColorBoundaryPlanner<'a> {
    stage_planner: RenderColorStagePlanner<'a>,
}

impl RenderOutputColorBoundaryPlanner<'_> {
    /// Create a planner that always chooses the CPU correctness path.
    pub fn cpu_only() -> Self {
        Self { stage_planner: RenderColorStagePlanner::cpu_only() }
    }
}

impl<'a> RenderOutputColorBoundaryPlanner<'a> {
    /// Create a planner that asks the renderer GPU backend for an OCIO stage plan.
    pub fn prefer_gpu(
        gpu_cache: &'a mut OcioGpuShaderCache,
        gpu_options: RenderColorTransformGpuOptions,
    ) -> Self {
        Self {
            stage_planner: RenderColorStagePlanner::prefer_gpu(gpu_cache, gpu_options),
        }
    }

    /// Plan the final output boundary for a typed working frame.
    pub fn plan(
        &mut self,
        frame: &CpuColorFrame,
        boundary: &RenderOutputColorBoundary,
    ) -> Result<RenderOutputColorBoundaryStagePlan, RenderColorTransformError> {
        self.plan_descriptor(frame.descriptor(), boundary)
    }

    /// Plan a final output boundary that retains encoded float samples.
    pub fn plan_float(
        &mut self,
        frame: &CpuColorFrame,
        boundary: &RenderOutputColorBoundary,
    ) -> Result<RenderOutputColorBoundaryStagePlan, RenderColorTransformError> {
        let stage_plan = self
            .stage_planner
            .plan_output_transform_float(frame.descriptor(), &boundary.transform())?;
        Ok(RenderOutputColorBoundaryStagePlan { boundary: boundary.clone(), stage_plan })
    }

    /// Plan the final output boundary for an already-described working frame.
    ///
    /// This is used by GPU-resident upstream stages that have a
    /// `GpuColorFrameHandle` but no CPU pixel container.
    pub fn plan_descriptor(
        &mut self,
        descriptor: ColorFrameDescriptor,
        boundary: &RenderOutputColorBoundary,
    ) -> Result<RenderOutputColorBoundaryStagePlan, RenderColorTransformError> {
        let stage_plan =
            self.stage_planner.plan_output_transform(descriptor, &boundary.transform())?;
        Ok(RenderOutputColorBoundaryStagePlan { boundary: boundary.clone(), stage_plan })
    }

    /// Plan a boundary whose descriptor encoding matches its concrete GPU target.
    pub fn plan_descriptor_for_texture(
        &mut self,
        descriptor: ColorFrameDescriptor,
        boundary: &RenderOutputColorBoundary,
        output_texture_format: GpuColorFrameTextureFormat,
    ) -> Result<RenderOutputColorBoundaryStagePlan, RenderColorTransformError> {
        let transform = boundary.transform();
        let stage_plan = match output_texture_format {
            GpuColorFrameTextureFormat::Rgba8Unorm => {
                self.stage_planner.plan_output_transform(descriptor, &transform)?
            }
            GpuColorFrameTextureFormat::Rgba16Float | GpuColorFrameTextureFormat::Rgba32Float => {
                self.stage_planner.plan_output_transform_float(descriptor, &transform)?
            }
        };
        Ok(RenderOutputColorBoundaryStagePlan { boundary: boundary.clone(), stage_plan })
    }
}

/// Renderer-owned executor for preview/export final output color boundaries.
///
/// This is the call-site boundary for app/export code. Construction selects an
/// explicit execution strategy; the executor does not silently fall back between
/// CPU and GPU modes.
pub struct RenderOutputColorBoundaryExecutor<'a> {
    planner: RenderOutputColorBoundaryPlanner<'a>,
}

impl RenderOutputColorBoundaryExecutor<'_> {
    /// Create a final-output executor that always schedules CPU color stages.
    pub fn cpu_only() -> Self {
        Self {
            planner: RenderOutputColorBoundaryPlanner::cpu_only(),
        }
    }
}

impl<'a> RenderOutputColorBoundaryExecutor<'a> {
    /// Create a final-output executor that plans native GPU OCIO output stages.
    pub fn prefer_gpu(
        gpu_cache: &'a mut OcioGpuShaderCache,
        gpu_options: RenderColorTransformGpuOptions,
    ) -> Self {
        Self {
            planner: RenderOutputColorBoundaryPlanner::prefer_gpu(gpu_cache, gpu_options),
        }
    }

    /// Plan and execute a final display/export output boundary.
    pub fn execute(
        &mut self,
        frame: &CpuColorFrame,
        boundary: &RenderOutputColorBoundary,
    ) -> Result<RenderColorStageExecution<RenderOutputTransformResult>, RenderColorTransformError>
    {
        let plan = self.planner.plan(frame, boundary)?;
        CpuRenderColorStageExecutor::output_transform(frame, &plan.stage_plan)
    }

    /// Plan and record a native GPU final display/export output boundary.
    pub fn record_wgpu_output_boundary(
        &mut self,
        boundary: &RenderOutputColorBoundary,
        request: RenderGpuOutputBoundaryRecordRequest<'_>,
    ) -> Result<RenderGpuOutputStageRecord, RenderOutputColorBoundaryGpuRecordError> {
        let RenderGpuOutputBoundaryRecordRequest { ids, frame, output_texture_format, backend } =
            request;
        let plan = self
            .planner
            .plan(frame, boundary)
            .map_err(RenderOutputColorBoundaryGpuRecordError::Plan)?;
        plan.record_wgpu_output_boundary(RenderGpuOutputBoundaryRecordRequest {
            ids,
            frame,
            output_texture_format,
            backend,
        })
        .map_err(RenderOutputColorBoundaryGpuRecordError::Record)
    }
}

/// Error returned when a final-output executor cannot record a GPU boundary.
#[derive(Debug, PartialEq, Eq)]
pub enum RenderOutputColorBoundaryGpuRecordError {
    /// The output boundary could not be planned.
    Plan(RenderColorTransformError),
    /// The planned GPU boundary could not be recorded.
    Record(RenderGpuOutputBoundaryRecordError),
}

/// Schedulable GPU OCIO color pass with resolved source/target frame handles.
#[derive(Debug, Clone)]
pub struct RenderGpuColorPassSchedule {
    /// GPU source frame consumed by the OCIO pass.
    pub input: GpuColorFrameHandle,
    /// GPU target frame produced by the OCIO pass.
    pub output: GpuColorFrameHandle,
    /// Renderer GPU transform plan for this color boundary.
    pub transform: RenderColorTransformGpuPlan,
    /// Backend render-pass node that records the fullscreen OCIO draw.
    pub pass_node: OcioGpuWgpuRenderPassNodePlan,
}

/// GPU input texture view borrowed while materializing a scheduled OCIO color pass.
pub struct RenderGpuColorPassInputView<'a> {
    /// Typed renderer handle this view was resolved from.
    pub frame: &'a GpuColorFrameHandle,
    /// Texture view sampled by the fullscreen wrapper.
    pub texture_view: &'a wgpu::TextureView,
    /// Sampler used to read the input texture.
    pub sampler: &'a wgpu::Sampler,
}

/// GPU target texture view borrowed while recording a scheduled OCIO color pass.
pub struct RenderGpuColorPassTargetView<'a> {
    /// Typed renderer handle this view was resolved from.
    pub frame: &'a GpuColorFrameHandle,
    /// Texture view written by the fullscreen pass.
    pub texture_view: &'a wgpu::TextureView,
    /// Attachment load operation for the output target.
    pub load_op: wgpu::LoadOp<wgpu::Color>,
}

/// Resolved resource-table entries for a scheduled GPU OCIO color pass.
#[derive(Debug)]
pub struct RenderGpuColorPassResolvedResources<'a, R> {
    /// Resolved input frame entry.
    pub input: &'a GpuColorFrameResource<R>,
    /// Resolved output frame entry.
    pub output: &'a GpuColorFrameResource<R>,
}

/// Resource materialization plan for one GPU output color transform stage.
#[derive(Debug, Clone)]
pub struct RenderGpuOutputStageResourcePlan {
    /// Input GPU frame handle consumed by the color pass.
    pub input: GpuColorFrameHandle,
    /// Output GPU frame handle produced by the color pass.
    pub output: GpuColorFrameHandle,
    /// Readback contract when the planned output returns to a CPU encoded boundary.
    pub readback: Option<GpuColorFrameReadbackPlan>,
    /// Optional upload plan that moves a CPU working frame into the input GPU frame.
    ///
    /// This is `None` when the input frame was produced by an upstream GPU
    /// stage, such as native GPU working-space compositing.
    pub input_upload: Option<GpuColorFrameUploadPlan>,
    /// Allocation plan for the output GPU target frame.
    pub output_allocation: GpuColorFrameAllocationPlan,
    /// GPU transform plan that these resources satisfy.
    pub transform: RenderColorTransformGpuPlan,
}

/// Resource materialization plan for one GPU source/input color transform stage.
///
/// This is the renderer-owned path for decoded CPU RGBA8 source frames entering
/// timeline working space on the GPU: CPU encoded source upload ->
/// native OCIO GPU input transform -> GPU-resident linear working frame.
#[derive(Debug, Clone)]
pub struct RenderGpuInputStageResourcePlan {
    /// Uploaded source GPU frame consumed by the input color pass.
    pub input: GpuColorFrameHandle,
    /// GPU working frame produced by the input color pass.
    pub output: GpuColorFrameHandle,
    /// Optional upload plan for CPU-decoded sources. Native GPU sources are
    /// already present in the shared resource table.
    pub input_upload: Option<GpuColorFrameUploadPlan>,
    /// Allocation plan for the output GPU working frame.
    pub output_allocation: GpuColorFrameAllocationPlan,
    /// GPU transform plan that these resources satisfy.
    pub transform: RenderColorTransformGpuPlan,
}

/// Result of recording one materialized GPU input color stage.
pub struct RenderGpuInputStageRecord {
    /// Resources materialized into the shared GPU frame table.
    pub materialized: RenderGpuOutputStageMaterializedResources,
    /// Diagnostics for the GPU color stage shape that was recorded.
    pub stage_diagnostics: RenderColorStageDiagnostics,
}

/// Handles materialized into a GPU frame resource table for one output stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderGpuOutputStageMaterializedResources {
    /// Input GPU frame inserted into the table.
    pub input: GpuColorFrameHandle,
    /// Output GPU frame inserted into the table.
    pub output: GpuColorFrameHandle,
}

/// Result of recording one materialized GPU output color stage.
pub struct RenderGpuOutputStageRecord {
    /// Resources materialized into the shared GPU frame table.
    pub materialized: RenderGpuOutputStageMaterializedResources,
    /// Diagnostics for the GPU color stage shape that was recorded.
    pub stage_diagnostics: RenderColorStageDiagnostics,
    /// Optional readback buffer recorded for CPU output boundaries.
    pub readback_buffer: Option<wgpu::Buffer>,
}

/// Borrowed backend objects required to record one GPU output color stage.
pub struct RenderGpuOutputStageBackendContext<'a> {
    /// wgpu device used for resource materialization and bind-group creation.
    pub device: &'a wgpu::Device,
    /// wgpu queue used for upload writes.
    pub queue: &'a wgpu::Queue,
    /// Command encoder receiving the color pass and optional readback copy.
    pub encoder: &'a mut wgpu::CommandEncoder,
    /// Prepared OCIO fullscreen render pipeline.
    pub pipeline: &'a OcioGpuWgpuRenderPipeline,
    /// Prepared OCIO resource bind group.
    pub ocio_bind_group: &'a OcioGpuWgpuOcioBindGroup,
    /// Backend render-pass node for this color transform.
    pub pass_node: OcioGpuWgpuRenderPassNodePlan,
    /// Shared GPU color frame resource table.
    pub table: &'a mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
    /// Load operation for the output color attachment.
    pub load_op: wgpu::LoadOp<wgpu::Color>,
}

/// Borrowed inputs required to record one GPU output color stage.
pub struct RenderGpuOutputStageRecordRequest<'a> {
    /// Backend context used to materialize resources and record the pass.
    pub backend: RenderGpuOutputStageBackendContext<'a>,
}

impl<'a> From<RenderGpuOutputBoundaryBackendContext<'a>>
    for RenderGpuOutputStageBackendContext<'a>
{
    fn from(context: RenderGpuOutputBoundaryBackendContext<'a>) -> Self {
        Self {
            device: context.device,
            queue: context.queue,
            encoder: context.encoder,
            pipeline: context.pipeline,
            ocio_bind_group: context.ocio_bind_group,
            pass_node: context.pass_node,
            table: context.table,
            load_op: context.load_op,
        }
    }
}

impl RenderGpuOutputStageResourcePlan {
    /// Return the stage diagnostics represented by this executable GPU output resource plan.
    pub fn stage_diagnostics(&self) -> RenderColorStageDiagnostics {
        let mut diagnostics = RenderColorStageDiagnostics {
            total_stages: 1 + u64::from(self.input_upload.is_some()),
            upload_stages: u64::from(self.input_upload.is_some()),
            gpu_color_stages: 1,
            stage_pixels: self.output.descriptor().pixel_count().saturating_add(
                self.input_upload.as_ref().map_or(0, |_| self.input.descriptor().pixel_count()),
            ) as u64,
            ..RenderColorStageDiagnostics::default()
        };
        if let Some(readback) = &self.readback {
            diagnostics.total_stages = diagnostics.total_stages.saturating_add(1);
            diagnostics.readback_stages = diagnostics.readback_stages.saturating_add(1);
            diagnostics.stage_pixels = diagnostics
                .stage_pixels
                .saturating_add(readback.output_descriptor.pixel_count() as u64);
        }
        diagnostics
    }

    /// Build resource plans for a CPU working frame entering a planned native GPU output transform.
    pub fn from_cpu_working_frame(
        ids: &mut GpuColorFrameIdAllocator,
        frame: &CpuColorFrame,
        stage_plan: &RenderColorStagePlan,
        output_texture_format: GpuColorFrameTextureFormat,
    ) -> Result<Self, RenderGpuOutputStageResourcePlanError> {
        let planned = planned_gpu_upload_transform_readback(stage_plan)?;
        if !planned.transform.wgpu.can_execute() {
            let mut breakdown = RenderColorStageGpuBlockerBreakdown::default();
            for blocker in &planned.transform.wgpu.blockers {
                breakdown.record(blocker);
            }
            return Err(
                RenderGpuOutputStageResourcePlanError::NativeBlockersRemaining {
                    blockers: planned.transform.wgpu.blockers.len(),
                    breakdown,
                },
            );
        }
        if planned.upload_input != frame.descriptor() {
            return Err(
                RenderGpuOutputStageResourcePlanError::InputDescriptorMismatch {
                    expected: planned.upload_input,
                    actual: frame.descriptor(),
                },
            );
        }
        validate_output_texture_encoding(planned.gpu_output.encoding, output_texture_format)?;
        let input_upload = GpuColorFrameUploadPlan::from_cpu_color_frame(
            ids.allocate(),
            frame,
            GpuColorFrameTextureFormat::Rgba32Float,
            "color-stage-working-input",
        )
        .map_err(RenderGpuOutputStageResourcePlanError::InputUpload)?;
        if input_upload.handle.descriptor() != planned.gpu_input {
            return Err(
                RenderGpuOutputStageResourcePlanError::UploadOutputDescriptorMismatch {
                    expected: planned.gpu_input,
                    actual: input_upload.handle.descriptor(),
                },
            );
        }
        let output = GpuColorFrameHandle::new(
            ids.allocate(),
            planned.gpu_output,
            output_texture_format,
            "color-stage-output-target",
        )
        .map_err(RenderGpuOutputStageResourcePlanError::OutputHandle)?;
        let readback = if planned.readback_output.is_some() {
            Some(output_readback_plan(output.clone())?)
        } else {
            None
        };
        let output_allocation = GpuColorFrameAllocationPlan::for_handle(output.clone());
        let mut transform = (*planned.transform).clone();
        transform.diagnostics.input = planned.gpu_input;
        transform.diagnostics.output = planned.gpu_output;
        transform.requires_source_upload = false;
        transform.requires_output_readback = false;
        Ok(Self {
            input: input_upload.handle.clone(),
            output,
            readback,
            input_upload: Some(input_upload),
            output_allocation,
            transform,
        })
    }

    /// Build resource plans for an already GPU-resident working frame entering
    /// a planned native GPU output transform.
    pub fn from_gpu_working_frame(
        ids: &mut GpuColorFrameIdAllocator,
        input: &GpuColorFrameHandle,
        stage_plan: &RenderColorStagePlan,
        output_texture_format: GpuColorFrameTextureFormat,
    ) -> Result<Self, RenderGpuOutputStageResourcePlanError> {
        let planned = planned_gpu_transform_readback(stage_plan)?;
        if !planned.transform.wgpu.can_execute() {
            let mut breakdown = RenderColorStageGpuBlockerBreakdown::default();
            for blocker in &planned.transform.wgpu.blockers {
                breakdown.record(blocker);
            }
            return Err(
                RenderGpuOutputStageResourcePlanError::NativeBlockersRemaining {
                    blockers: planned.transform.wgpu.blockers.len(),
                    breakdown,
                },
            );
        }
        if planned.gpu_input != input.descriptor() {
            return Err(
                RenderGpuOutputStageResourcePlanError::InputDescriptorMismatch {
                    expected: planned.gpu_input,
                    actual: input.descriptor(),
                },
            );
        }
        validate_output_texture_encoding(planned.gpu_output.encoding, output_texture_format)?;
        let output = GpuColorFrameHandle::new(
            ids.allocate(),
            planned.gpu_output,
            output_texture_format,
            "color-stage-output-target",
        )
        .map_err(RenderGpuOutputStageResourcePlanError::OutputHandle)?;
        let readback = if planned.readback_output.is_some() {
            Some(output_readback_plan(output.clone())?)
        } else {
            None
        };
        let output_allocation = GpuColorFrameAllocationPlan::for_handle(output.clone());
        let mut transform = (*planned.transform).clone();
        transform.diagnostics.input = planned.gpu_input;
        transform.diagnostics.output = planned.gpu_output;
        transform.requires_source_upload = false;
        transform.requires_output_readback = false;
        Ok(Self {
            input: input.clone(),
            output,
            readback,
            input_upload: None,
            output_allocation,
            transform,
        })
    }

    /// Insert already-materialized resources into a GPU color frame resource table.
    pub fn insert_resources<R>(
        &self,
        table: &mut GpuColorFrameResourceTable<R>,
        input: GpuColorFrameResource<R>,
        output: GpuColorFrameResource<R>,
    ) -> Result<RenderGpuOutputStageMaterializedResources, RenderGpuOutputStageMaterializeError>
    {
        if input.handle() != &self.input {
            return Err(
                RenderGpuOutputStageMaterializeError::InputResourceMismatch {
                    expected: self.input.clone(),
                    actual: input.handle().clone(),
                },
            );
        }
        if output.handle() != &self.output {
            return Err(
                RenderGpuOutputStageMaterializeError::OutputResourceMismatch {
                    expected: self.output.clone(),
                    actual: output.handle().clone(),
                },
            );
        }
        validate_materialization_table_slot(table, &self.input)?;
        validate_materialization_table_slot(table, &self.output)?;
        table
            .insert(input)
            .map_err(RenderGpuOutputStageMaterializeError::ResourceTable)?;
        table
            .insert(output)
            .map_err(RenderGpuOutputStageMaterializeError::ResourceTable)?;
        Ok(RenderGpuOutputStageMaterializedResources {
            input: self.input.clone(),
            output: self.output.clone(),
        })
    }

    /// Upload/allocate this stage's wgpu resources and insert them into a resource table.
    pub fn materialize_wgpu(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
    ) -> Result<RenderGpuOutputStageMaterializedResources, RenderGpuOutputStageMaterializeError>
    {
        validate_materialization_table_slot(table, &self.input)?;
        validate_materialization_table_slot(table, &self.output)?;
        let output = GpuColorFrameUploader::allocate(device, &self.output_allocation);
        if let Some(input_upload) = &self.input_upload {
            let input = GpuColorFrameUploader::upload(device, queue, input_upload);
            self.insert_resources(table, input, output)
        } else {
            table
                .get(&self.input)
                .map_err(RenderGpuOutputStageMaterializeError::ResourceTable)?;
            table
                .insert(output)
                .map_err(RenderGpuOutputStageMaterializeError::ResourceTable)?;
            Ok(RenderGpuOutputStageMaterializedResources {
                input: self.input.clone(),
                output: self.output.clone(),
            })
        }
    }

    /// Build a schedulable color pass for these materialized frame handles.
    pub fn schedule_pass(
        &self,
        pass_node: OcioGpuWgpuRenderPassNodePlan,
    ) -> Result<RenderGpuColorPassSchedule, RenderGpuColorPassScheduleError> {
        RenderGpuColorPassSchedule::new(
            self.input.clone(),
            self.output.clone(),
            self.transform.clone(),
            pass_node,
        )
    }

    /// Materialize resources, record the GPU color pass, then record optional readback.
    pub fn record_wgpu_output_stage(
        &self,
        request: RenderGpuOutputStageRecordRequest<'_>,
    ) -> Result<RenderGpuOutputStageRecord, RenderGpuOutputStageRecordError> {
        let RenderGpuOutputStageRecordRequest { backend } = request;
        let schedule = self
            .schedule_pass(backend.pass_node)
            .map_err(RenderGpuOutputStageRecordError::Schedule)?;
        let materialized = self
            .materialize_wgpu(backend.device, backend.queue, backend.table)
            .map_err(RenderGpuOutputStageRecordError::Materialize)?;
        schedule
            .record_wgpu_from_resources(
                backend.device,
                backend.encoder,
                backend.pipeline,
                backend.ocio_bind_group,
                backend.table,
                backend.load_op,
            )
            .map_err(RenderGpuOutputStageRecordError::Pass)?;
        let readback_buffer = self
            .record_readback_wgpu(backend.device, backend.encoder, backend.table)
            .map_err(RenderGpuOutputStageRecordError::Readback)?;
        Ok(RenderGpuOutputStageRecord {
            materialized,
            stage_diagnostics: self.stage_diagnostics(),
            readback_buffer,
        })
    }

    /// Resolve the materialized output resource used by this stage's optional readback.
    pub fn resolve_readback_resource<'a, R>(
        &self,
        table: &'a GpuColorFrameResourceTable<R>,
    ) -> Result<Option<&'a GpuColorFrameResource<R>>, RenderGpuOutputStageReadbackError> {
        let Some(readback) = &self.readback else {
            return Ok(None);
        };
        table
            .get(&readback.handle)
            .map(Some)
            .map_err(RenderGpuOutputStageReadbackError::ResourceTable)
    }

    /// Record this stage's optional GPU-to-CPU readback copy into an encoder.
    pub fn record_readback_wgpu(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        table: &GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
    ) -> Result<Option<wgpu::Buffer>, RenderGpuOutputStageReadbackError> {
        let Some(readback) = &self.readback else {
            return Ok(None);
        };
        match self.resolve_readback_resource(table)? {
            Some(resource) => Ok(Some(GpuColorFrameReadback::record_copy(
                device,
                encoder,
                readback,
                resource.resource(),
            ))),
            None => Ok(None),
        }
    }
}

fn validate_output_texture_encoding(
    encoding: ColorFrameEncoding,
    texture_format: GpuColorFrameTextureFormat,
) -> Result<(), RenderGpuOutputStageResourcePlanError> {
    let matches = match texture_format {
        GpuColorFrameTextureFormat::Rgba8Unorm => encoding == ColorFrameEncoding::EncodedRgba8,
        GpuColorFrameTextureFormat::Rgba16Float | GpuColorFrameTextureFormat::Rgba32Float => {
            encoding == ColorFrameEncoding::EncodedFloat
        }
    };
    if matches {
        Ok(())
    } else {
        Err(
            RenderGpuOutputStageResourcePlanError::OutputTextureEncodingMismatch {
                encoding,
                texture_format,
            },
        )
    }
}

fn output_readback_plan(
    output: GpuColorFrameHandle,
) -> Result<GpuColorFrameReadbackPlan, RenderGpuOutputStageResourcePlanError> {
    match output.texture_format() {
        GpuColorFrameTextureFormat::Rgba8Unorm => GpuColorFrameReadbackPlan::encoded_rgba8(output),
        GpuColorFrameTextureFormat::Rgba16Float => {
            GpuColorFrameReadbackPlan::encoded_rgba16float(output)
        }
        GpuColorFrameTextureFormat::Rgba32Float => {
            GpuColorFrameReadbackPlan::encoded_rgba32float(output)
        }
    }
    .map_err(RenderGpuOutputStageResourcePlanError::OutputReadback)
}

impl RenderGpuInputStageResourcePlan {
    /// Return the stage diagnostics represented by this executable GPU input resource plan.
    pub fn stage_diagnostics(&self) -> RenderColorStageDiagnostics {
        RenderColorStageDiagnostics {
            total_stages: 1 + u64::from(self.input_upload.is_some()),
            upload_stages: u64::from(self.input_upload.is_some()),
            gpu_color_stages: 1,
            stage_pixels: self.output.descriptor().pixel_count().saturating_add(
                self.input_upload.as_ref().map_or(0, |_| self.input.descriptor().pixel_count()),
            ) as u64,
            ..RenderColorStageDiagnostics::default()
        }
    }

    /// Build resource plans for a decoded CPU encoded source frame entering a
    /// planned native GPU input transform.
    pub fn from_cpu_encoded_source_frame(
        ids: &mut GpuColorFrameIdAllocator,
        frame: &CpuEncodedColorFrame,
        stage_plan: &RenderColorStagePlan,
    ) -> Result<Self, RenderGpuInputStageResourcePlanError> {
        let planned = planned_gpu_source_upload_transform(stage_plan)?;
        if !planned.transform.wgpu.can_execute() {
            let mut breakdown = RenderColorStageGpuBlockerBreakdown::default();
            for blocker in &planned.transform.wgpu.blockers {
                breakdown.record(blocker);
            }
            return Err(
                RenderGpuInputStageResourcePlanError::NativeBlockersRemaining {
                    blockers: planned.transform.wgpu.blockers.len(),
                    breakdown,
                },
            );
        }
        if planned.upload_input != frame.descriptor() {
            return Err(
                RenderGpuInputStageResourcePlanError::InputDescriptorMismatch {
                    expected: planned.upload_input,
                    actual: frame.descriptor(),
                },
            );
        }
        let input_upload = GpuColorFrameUploadPlan::from_cpu_encoded_frame(
            ids.allocate(),
            frame,
            GpuColorFrameTextureFormat::Rgba8Unorm,
            "color-stage-source-input",
        )
        .map_err(RenderGpuInputStageResourcePlanError::InputUpload)?;
        if input_upload.handle.descriptor() != planned.gpu_input {
            return Err(
                RenderGpuInputStageResourcePlanError::UploadOutputDescriptorMismatch {
                    expected: planned.gpu_input,
                    actual: input_upload.handle.descriptor(),
                },
            );
        }
        let output = GpuColorFrameHandle::new(
            ids.allocate(),
            planned.gpu_output,
            GpuColorFrameTextureFormat::Rgba32Float,
            "color-stage-working-output",
        )
        .map_err(RenderGpuInputStageResourcePlanError::OutputHandle)?;
        let output_allocation = GpuColorFrameAllocationPlan::for_handle(output.clone());
        let mut transform = (*planned.transform).clone();
        transform.diagnostics.input = planned.gpu_input;
        transform.diagnostics.output = planned.gpu_output;
        transform.requires_source_upload = false;
        transform.requires_output_readback = false;
        Ok(Self {
            input: input_upload.handle.clone(),
            output,
            input_upload: Some(input_upload),
            output_allocation,
            transform,
        })
    }

    /// Build resource plans for an already GPU-resident encoded source frame
    /// entering the OCIO input transform.
    pub fn from_gpu_encoded_source_frame(
        input: &GpuColorFrameHandle,
        output: &GpuColorFrameHandle,
        stage_plan: &RenderColorStagePlan,
    ) -> Result<Self, RenderGpuInputStageResourcePlanError> {
        let planned = planned_gpu_source_transform(stage_plan)?;
        if !planned.transform.wgpu.can_execute() {
            let mut breakdown = RenderColorStageGpuBlockerBreakdown::default();
            for blocker in &planned.transform.wgpu.blockers {
                breakdown.record(blocker);
            }
            return Err(
                RenderGpuInputStageResourcePlanError::NativeBlockersRemaining {
                    blockers: planned.transform.wgpu.blockers.len(),
                    breakdown,
                },
            );
        }
        if planned.gpu_input != input.descriptor() {
            return Err(
                RenderGpuInputStageResourcePlanError::InputDescriptorMismatch {
                    expected: planned.gpu_input,
                    actual: input.descriptor(),
                },
            );
        }
        if input.descriptor().encoding != ColorFrameEncoding::EncodedFloat {
            return Err(RenderGpuInputStageResourcePlanError::UnsupportedStagePlan {
                reason: "native GPU input transform requires an encoded-float source frame",
            });
        }
        if input.texture_format() != GpuColorFrameTextureFormat::Rgba16Float {
            return Err(RenderGpuInputStageResourcePlanError::UnsupportedStagePlan {
                reason: "native encoded-float source must use Rgba16Float",
            });
        }
        if planned.gpu_output != output.descriptor() {
            return Err(
                RenderGpuInputStageResourcePlanError::OutputDescriptorMismatch {
                    expected: planned.gpu_output,
                    actual: output.descriptor(),
                },
            );
        }
        if output.texture_format() != GpuColorFrameTextureFormat::Rgba32Float {
            return Err(
                RenderGpuInputStageResourcePlanError::UnsupportedOutputTextureFormat {
                    texture_format: output.texture_format(),
                },
            );
        }
        let output_allocation = GpuColorFrameAllocationPlan::for_handle(output.clone());
        let mut transform = (*planned.transform).clone();
        transform.diagnostics.input = planned.gpu_input;
        transform.diagnostics.output = planned.gpu_output;
        transform.requires_source_upload = false;
        transform.requires_output_readback = false;
        Ok(Self {
            input: input.clone(),
            output: output.clone(),
            input_upload: None,
            output_allocation,
            transform,
        })
    }

    /// Upload/allocate this input stage's wgpu resources and insert them into a resource table.
    pub fn materialize_wgpu(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
    ) -> Result<RenderGpuOutputStageMaterializedResources, RenderGpuOutputStageMaterializeError>
    {
        validate_materialization_table_slot(table, &self.input)?;
        validate_materialization_table_slot(table, &self.output)?;
        let output = GpuColorFrameUploader::allocate(device, &self.output_allocation);
        if let Some(input_upload) = &self.input_upload {
            let input = GpuColorFrameUploader::upload(device, queue, input_upload);
            self.insert_resources(table, input, output)
        } else {
            table
                .get(&self.input)
                .map_err(RenderGpuOutputStageMaterializeError::ResourceTable)?;
            table
                .insert(output)
                .map_err(RenderGpuOutputStageMaterializeError::ResourceTable)?;
            Ok(RenderGpuOutputStageMaterializedResources {
                input: self.input.clone(),
                output: self.output.clone(),
            })
        }
    }

    /// Insert already-materialized resources into a GPU color frame resource table.
    pub fn insert_resources<R>(
        &self,
        table: &mut GpuColorFrameResourceTable<R>,
        input: GpuColorFrameResource<R>,
        output: GpuColorFrameResource<R>,
    ) -> Result<RenderGpuOutputStageMaterializedResources, RenderGpuOutputStageMaterializeError>
    {
        if input.handle() != &self.input {
            return Err(
                RenderGpuOutputStageMaterializeError::InputResourceMismatch {
                    expected: self.input.clone(),
                    actual: input.handle().clone(),
                },
            );
        }
        if output.handle() != &self.output {
            return Err(
                RenderGpuOutputStageMaterializeError::OutputResourceMismatch {
                    expected: self.output.clone(),
                    actual: output.handle().clone(),
                },
            );
        }
        validate_materialization_table_slot(table, &self.input)?;
        validate_materialization_table_slot(table, &self.output)?;
        table
            .insert(input)
            .map_err(RenderGpuOutputStageMaterializeError::ResourceTable)?;
        table
            .insert(output)
            .map_err(RenderGpuOutputStageMaterializeError::ResourceTable)?;
        Ok(RenderGpuOutputStageMaterializedResources {
            input: self.input.clone(),
            output: self.output.clone(),
        })
    }

    /// Build a schedulable color pass for these materialized frame handles.
    pub fn schedule_pass(
        &self,
        pass_node: OcioGpuWgpuRenderPassNodePlan,
    ) -> Result<RenderGpuColorPassSchedule, RenderGpuColorPassScheduleError> {
        RenderGpuColorPassSchedule::new(
            self.input.clone(),
            self.output.clone(),
            self.transform.clone(),
            pass_node,
        )
    }

    /// Materialize resources, then record the GPU input color pass.
    pub fn record_wgpu_input_stage(
        &self,
        request: RenderGpuOutputStageRecordRequest<'_>,
    ) -> Result<RenderGpuInputStageRecord, RenderGpuOutputStageRecordError> {
        let RenderGpuOutputStageRecordRequest { backend } = request;
        let schedule = self
            .schedule_pass(backend.pass_node)
            .map_err(RenderGpuOutputStageRecordError::Schedule)?;
        let materialized = self
            .materialize_wgpu(backend.device, backend.queue, backend.table)
            .map_err(RenderGpuOutputStageRecordError::Materialize)?;
        schedule
            .record_wgpu_from_resources(
                backend.device,
                backend.encoder,
                backend.pipeline,
                backend.ocio_bind_group,
                backend.table,
                backend.load_op,
            )
            .map_err(RenderGpuOutputStageRecordError::Pass)?;
        Ok(RenderGpuInputStageRecord {
            materialized,
            stage_diagnostics: self.stage_diagnostics(),
        })
    }
}

/// Error returned when GPU output stage resources cannot be planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderGpuOutputStageResourcePlanError {
    /// Native GPU blockers remain.
    NativeBlockersRemaining {
        /// Number of blockers in the GPU execution plan.
        blockers: usize,
        /// Structured blocker breakdown from the planned GPU transform.
        breakdown: RenderColorStageGpuBlockerBreakdown,
    },
    /// The stage plan is not a supported upload -> GPU transform shape.
    UnsupportedStagePlan {
        /// Human-readable reason.
        reason: &'static str,
    },
    /// The CPU working frame does not match the transform input descriptor.
    InputDescriptorMismatch {
        /// Expected transform input descriptor.
        expected: ColorFrameDescriptor,
        /// Actual CPU frame descriptor.
        actual: ColorFrameDescriptor,
    },
    /// The input upload did not produce the GPU descriptor consumed by the transform.
    UploadOutputDescriptorMismatch {
        /// Expected GPU transform input descriptor.
        expected: ColorFrameDescriptor,
        /// Actual upload output descriptor.
        actual: ColorFrameDescriptor,
    },
    /// The input upload plan failed.
    InputUpload(GpuColorFrameUploadError),
    /// The output GPU handle could not be created.
    OutputHandle(crate::GpuColorFrameHandleError),
    /// The output GPU frame cannot be read back into the requested CPU boundary.
    OutputReadback(GpuColorFrameReadbackError),
    /// The planned encoded sample representation does not match the target texture.
    OutputTextureEncodingMismatch {
        /// Encoding declared by the stage plan.
        encoding: ColorFrameEncoding,
        /// Concrete target texture format.
        texture_format: GpuColorFrameTextureFormat,
    },
}

/// Error returned when GPU input stage resources cannot be planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderGpuInputStageResourcePlanError {
    /// Native GPU blockers remain.
    NativeBlockersRemaining {
        /// Number of blockers in the GPU execution plan.
        blockers: usize,
        /// Structured blocker breakdown from the planned GPU transform.
        breakdown: RenderColorStageGpuBlockerBreakdown,
    },
    /// The stage plan is not a supported source upload -> GPU transform shape.
    UnsupportedStagePlan {
        /// Human-readable reason.
        reason: &'static str,
    },
    /// The CPU source frame does not match the transform input descriptor.
    InputDescriptorMismatch {
        /// Expected transform input descriptor.
        expected: ColorFrameDescriptor,
        /// Actual CPU frame descriptor.
        actual: ColorFrameDescriptor,
    },
    /// The planned GPU working output does not match the supplied output handle.
    OutputDescriptorMismatch {
        /// Descriptor required by the OCIO stage plan.
        expected: ColorFrameDescriptor,
        /// Descriptor carried by the supplied output handle.
        actual: ColorFrameDescriptor,
    },
    /// The input upload did not produce the GPU descriptor consumed by the transform.
    UploadOutputDescriptorMismatch {
        /// Expected GPU transform input descriptor.
        expected: ColorFrameDescriptor,
        /// Actual upload output descriptor.
        actual: ColorFrameDescriptor,
    },
    /// The input upload plan failed.
    InputUpload(GpuColorFrameUploadError),
    /// The output GPU handle could not be created.
    OutputHandle(crate::GpuColorFrameHandleError),
    /// The requested output texture format cannot carry a linear working frame.
    UnsupportedOutputTextureFormat {
        /// Requested texture format.
        texture_format: GpuColorFrameTextureFormat,
    },
}

/// Error returned when planned GPU output stage resources cannot be materialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderGpuOutputStageMaterializeError {
    /// The provided input resource does not match the planned input handle.
    InputResourceMismatch {
        /// Expected planned handle.
        expected: GpuColorFrameHandle,
        /// Actual resource handle.
        actual: GpuColorFrameHandle,
    },
    /// The provided output resource does not match the planned output handle.
    OutputResourceMismatch {
        /// Expected planned handle.
        expected: GpuColorFrameHandle,
        /// Actual resource handle.
        actual: GpuColorFrameHandle,
    },
    /// Resource table rejected one of the materialized resources.
    ResourceTable(GpuColorFrameResourceTableError),
}

/// Error returned when a GPU output stage readback cannot be recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderGpuOutputStageReadbackError {
    /// Resource table rejected the readback target lookup.
    ResourceTable(GpuColorFrameResourceTableError),
}

/// Error returned when a full GPU output stage cannot be recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderGpuOutputStageRecordError {
    /// The pass could not be scheduled from the planned handles and backend node.
    Schedule(RenderGpuColorPassScheduleError),
    /// The stage resources could not be materialized.
    Materialize(RenderGpuOutputStageMaterializeError),
    /// The color pass could not be recorded.
    Pass(RenderGpuColorPassExecutionError),
    /// The optional readback could not be recorded.
    Readback(RenderGpuOutputStageReadbackError),
}

impl RenderGpuColorPassSchedule {
    /// Build a schedulable GPU OCIO color pass after descriptor and backend-node validation.
    pub fn new(
        input: GpuColorFrameHandle,
        output: GpuColorFrameHandle,
        transform: RenderColorTransformGpuPlan,
        pass_node: OcioGpuWgpuRenderPassNodePlan,
    ) -> Result<Self, RenderGpuColorPassScheduleError> {
        if transform.requires_source_upload {
            return Err(RenderGpuColorPassScheduleError::UploadStillRequired);
        }
        if transform.requires_output_readback {
            return Err(RenderGpuColorPassScheduleError::ReadbackStillRequired);
        }
        if !transform.wgpu.can_execute() {
            return Err(RenderGpuColorPassScheduleError::NativeBlockersRemaining {
                blockers: transform.wgpu.blockers.len(),
            });
        }
        if input.descriptor() != transform.diagnostics.input {
            return Err(RenderGpuColorPassScheduleError::InputDescriptorMismatch {
                expected: transform.diagnostics.input,
                actual: input.descriptor(),
            });
        }
        if output.descriptor() != transform.diagnostics.output {
            return Err(RenderGpuColorPassScheduleError::OutputDescriptorMismatch {
                expected: transform.diagnostics.output,
                actual: output.descriptor(),
            });
        }
        if input.descriptor().residency != ColorFrameResidency::Gpu {
            return Err(RenderGpuColorPassScheduleError::InputNotGpuResident {
                actual: input.descriptor().residency,
            });
        }
        if output.descriptor().residency != ColorFrameResidency::Gpu {
            return Err(RenderGpuColorPassScheduleError::OutputNotGpuResident {
                actual: output.descriptor().residency,
            });
        }
        if input.descriptor().width != output.descriptor().width
            || input.descriptor().height != output.descriptor().height
        {
            return Err(RenderGpuColorPassScheduleError::ExtentMismatch {
                input: input.descriptor(),
                output: output.descriptor(),
            });
        }
        if pass_node.resource_key != transform.wgpu.resources.resource_key {
            return Err(RenderGpuColorPassScheduleError::PassResourceKeyMismatch {
                expected: transform.wgpu.resources.resource_key,
                actual: pass_node.resource_key,
            });
        }
        let wrapper_layout =
            OcioGpuWgpuWrapperBindingPlan::for_contract(&transform.wgpu.resources.wrapper_contract);
        let wrapper_layout_hash =
            OcioGpuWgpuBindGroupLayoutDescriptorPlan::for_wrapper_input(&wrapper_layout)
                .layout_hash;
        if pass_node.wrapper_layout_hash != wrapper_layout_hash {
            return Err(
                RenderGpuColorPassScheduleError::PassWrapperLayoutHashMismatch {
                    expected: wrapper_layout_hash,
                    actual: pass_node.wrapper_layout_hash,
                },
            );
        }
        let output_format = color_target_format_for_gpu_frame(&output);
        if pass_node.output_format != output_format {
            return Err(
                RenderGpuColorPassScheduleError::TargetTextureFormatMismatch {
                    expected: pass_node.output_format,
                    actual: output.texture_format(),
                },
            );
        }

        Ok(Self { input, output, transform, pass_node })
    }

    /// Return the wrapper input binding contract used by this scheduled pass.
    pub fn wrapper_binding_plan(&self) -> OcioGpuWgpuWrapperBindingPlan {
        OcioGpuWgpuWrapperBindingPlan::for_contract(&self.transform.wgpu.resources.wrapper_contract)
    }

    /// Validate that a resolved renderer input frame belongs to this scheduled pass.
    pub fn validate_input_frame(
        &self,
        frame: &GpuColorFrameHandle,
    ) -> Result<(), RenderGpuColorPassExecutionError> {
        validate_execution_frame(
            &self.input,
            frame,
            RenderGpuColorPassExecutionFrameRole::Input,
        )
    }

    /// Validate that a resolved renderer output frame belongs to this scheduled pass.
    pub fn validate_output_frame(
        &self,
        frame: &GpuColorFrameHandle,
    ) -> Result<(), RenderGpuColorPassExecutionError> {
        validate_execution_frame(
            &self.output,
            frame,
            RenderGpuColorPassExecutionFrameRole::Output,
        )
    }

    /// Resolve source and target entries from a shared GPU frame resource table.
    pub fn resolve_resources<'a, R>(
        &self,
        resources: &'a GpuColorFrameResourceTable<R>,
    ) -> Result<RenderGpuColorPassResolvedResources<'a, R>, RenderGpuColorPassExecutionError> {
        let input = resources
            .get(&self.input)
            .map_err(RenderGpuColorPassExecutionError::ResourceTable)?;
        let output = resources
            .get(&self.output)
            .map_err(RenderGpuColorPassExecutionError::ResourceTable)?;
        Ok(RenderGpuColorPassResolvedResources { input, output })
    }

    /// Create the wrapper input bind group for this scheduled pass from a resolved input view.
    pub fn prepare_wrapper_bind_group(
        &self,
        device: &wgpu::Device,
        input: RenderGpuColorPassInputView<'_>,
    ) -> Result<OcioGpuWgpuWrapperBindGroup, RenderGpuColorPassExecutionError> {
        self.validate_input_frame(input.frame)?;
        let wrapper_layout = self.wrapper_binding_plan();
        let wrapper_layout_hash =
            OcioGpuWgpuBindGroupLayoutDescriptorPlan::for_wrapper_input(&wrapper_layout)
                .layout_hash;
        if wrapper_layout_hash != self.pass_node.wrapper_layout_hash {
            return Err(
                RenderGpuColorPassExecutionError::WrapperLayoutHashMismatch {
                    expected: self.pass_node.wrapper_layout_hash,
                    actual: wrapper_layout_hash,
                },
            );
        }
        Ok(OcioGpuWgpuBindGroupPreparer::prepare_wrapper_bind_group(
            device,
            &wrapper_layout,
            OcioGpuWgpuWrapperInputResources {
                input_texture_view: input.texture_view,
                input_sampler: input.sampler,
            },
        ))
    }

    /// Record this scheduled pass into a wgpu command encoder.
    pub fn record_wgpu(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &OcioGpuWgpuRenderPipeline,
        ocio_bind_group: &OcioGpuWgpuOcioBindGroup,
        wrapper_bind_group: &OcioGpuWgpuWrapperBindGroup,
        target: RenderGpuColorPassTargetView<'_>,
    ) -> Result<(), RenderGpuColorPassExecutionError> {
        self.validate_output_frame(target.frame)?;
        OcioGpuWgpuRenderPassRecorder::record(
            encoder,
            &self.pass_node,
            pipeline,
            ocio_bind_group,
            wrapper_bind_group,
            OcioGpuWgpuRenderPassTarget {
                resource_key: self.pass_node.resource_key,
                output_format: color_target_format_for_gpu_frame(target.frame),
                view: target.texture_view,
                load_op: target.load_op,
            },
        )
        .map_err(RenderGpuColorPassExecutionError::RenderPass)
    }

    /// Resolve GPU frame resources, prepare the wrapper bind group, and record this pass.
    pub fn record_wgpu_from_resources(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &OcioGpuWgpuRenderPipeline,
        ocio_bind_group: &OcioGpuWgpuOcioBindGroup,
        resources: &GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        load_op: wgpu::LoadOp<wgpu::Color>,
    ) -> Result<(), RenderGpuColorPassExecutionError> {
        let resolved = self.resolve_resources(resources)?;
        let input = resolved.input.resource();
        let wrapper_bind_group = self.prepare_wrapper_bind_group(
            device,
            RenderGpuColorPassInputView {
                frame: resolved.input.handle(),
                texture_view: &input.texture_view,
                sampler: &input.sampler,
            },
        )?;
        let output = resolved.output.resource();
        self.record_wgpu(
            encoder,
            pipeline,
            ocio_bind_group,
            &wrapper_bind_group,
            RenderGpuColorPassTargetView {
                frame: resolved.output.handle(),
                texture_view: &output.texture_view,
                load_op,
            },
        )
    }
}

/// Error returned when a GPU OCIO color pass cannot be scheduled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderGpuColorPassScheduleError {
    /// The stage still requires an upload before GPU execution.
    UploadStillRequired,
    /// The stage still requires a readback after GPU execution.
    ReadbackStillRequired,
    /// Native backend blockers remain.
    NativeBlockersRemaining {
        /// Number of blockers in the GPU execution plan.
        blockers: usize,
    },
    /// The source frame descriptor does not match the transform input.
    InputDescriptorMismatch {
        /// Expected transform input descriptor.
        expected: ColorFrameDescriptor,
        /// Actual source frame descriptor.
        actual: ColorFrameDescriptor,
    },
    /// The target frame descriptor does not match the transform output.
    OutputDescriptorMismatch {
        /// Expected transform output descriptor.
        expected: ColorFrameDescriptor,
        /// Actual target frame descriptor.
        actual: ColorFrameDescriptor,
    },
    /// The source frame is not GPU-resident.
    InputNotGpuResident {
        /// Actual source residency.
        actual: ColorFrameResidency,
    },
    /// The target frame is not GPU-resident.
    OutputNotGpuResident {
        /// Actual target residency.
        actual: ColorFrameResidency,
    },
    /// Source and target frame extents differ.
    ExtentMismatch {
        /// Source frame descriptor.
        input: ColorFrameDescriptor,
        /// Target frame descriptor.
        output: ColorFrameDescriptor,
    },
    /// Backend render-pass node belongs to a different OCIO resource plan.
    PassResourceKeyMismatch {
        /// Expected resource key from the transform's wgpu plan.
        expected: u64,
        /// Actual resource key from the pass node.
        actual: u64,
    },
    /// Backend render-pass node was built for a different wrapper input layout.
    PassWrapperLayoutHashMismatch {
        /// Expected wrapper layout hash from the transform resource contract.
        expected: u64,
        /// Actual wrapper layout hash from the pass node.
        actual: u64,
    },
    /// Backend render-pass target format differs from the target frame format.
    TargetTextureFormatMismatch {
        /// Expected render-pass output format.
        expected: OcioGpuWgpuColorTargetFormat,
        /// Actual target frame texture format.
        actual: GpuColorFrameTextureFormat,
    },
}

/// Error returned when a scheduled GPU color pass cannot bind or record backend resources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderGpuColorPassExecutionError {
    /// The resolved input frame id differs from the scheduled input.
    InputFrameIdMismatch {
        /// Expected scheduled frame id.
        expected: GpuColorFrameId,
        /// Actual resolved frame id.
        actual: GpuColorFrameId,
    },
    /// The resolved output frame id differs from the scheduled output.
    OutputFrameIdMismatch {
        /// Expected scheduled frame id.
        expected: GpuColorFrameId,
        /// Actual resolved frame id.
        actual: GpuColorFrameId,
    },
    /// The resolved input frame descriptor differs from the scheduled input.
    InputDescriptorMismatch {
        /// Expected scheduled descriptor.
        expected: ColorFrameDescriptor,
        /// Actual resolved descriptor.
        actual: ColorFrameDescriptor,
    },
    /// The resolved output frame descriptor differs from the scheduled output.
    OutputDescriptorMismatch {
        /// Expected scheduled descriptor.
        expected: ColorFrameDescriptor,
        /// Actual resolved descriptor.
        actual: ColorFrameDescriptor,
    },
    /// The resolved input frame texture format differs from the scheduled input.
    InputTextureFormatMismatch {
        /// Expected scheduled texture format.
        expected: GpuColorFrameTextureFormat,
        /// Actual resolved texture format.
        actual: GpuColorFrameTextureFormat,
    },
    /// The resolved output frame texture format differs from the scheduled output.
    OutputTextureFormatMismatch {
        /// Expected scheduled texture format.
        expected: GpuColorFrameTextureFormat,
        /// Actual resolved texture format.
        actual: GpuColorFrameTextureFormat,
    },
    /// The wrapper bind-group layout no longer matches the scheduled pass node.
    WrapperLayoutHashMismatch {
        /// Expected wrapper layout hash from the pass node.
        expected: u64,
        /// Actual wrapper layout hash from the transform resource contract.
        actual: u64,
    },
    /// Source or target frame resolution failed against the shared GPU resource table.
    ResourceTable(GpuColorFrameResourceTableError),
    /// The backend render-pass recorder rejected the concrete wgpu resources.
    RenderPass(OcioGpuWgpuRenderPassError),
}

impl CpuRenderColorStageExecutor {
    /// Execute a CPU source/import -> working-space stage plan.
    pub fn input_to_working(
        frame: &CpuEncodedColorFrame,
        plan: &RenderColorStagePlan,
    ) -> Result<RenderColorStageExecution<RenderInputTransformResult>, RenderColorTransformError>
    {
        let [stage] = plan.stages.as_slice() else {
            return Err(RenderColorTransformError::UnsupportedStagePlan {
                reason: "input stage execution requires exactly one CPU stage",
            });
        };
        let RenderColorStage::CpuInputTransform { input, output, transform } = stage else {
            return Err(RenderColorTransformError::UnsupportedStagePlan {
                reason: "input stage execution only supports CPU input transforms",
            });
        };
        validate_descriptor(*input, frame.descriptor())?;
        let result = CpuColorTransformExecutor::input_to_working(frame, transform)?;
        validate_descriptor(*output, result.frame.descriptor())?;
        validate_descriptor(plan.final_descriptor, result.frame.descriptor())?;
        Ok(RenderColorStageExecution { result, stage_diagnostics: plan.diagnostics() })
    }

    /// Execute a CPU working-space -> display/export stage plan.
    pub fn output_transform(
        frame: &CpuColorFrame,
        plan: &RenderColorStagePlan,
    ) -> Result<RenderColorStageExecution<RenderOutputTransformResult>, RenderColorTransformError>
    {
        let [stage] = plan.stages.as_slice() else {
            return Err(RenderColorTransformError::UnsupportedStagePlan {
                reason: "output stage execution requires exactly one CPU stage",
            });
        };
        let RenderColorStage::CpuOutputTransform { input, output, transform } = stage else {
            return Err(RenderColorTransformError::UnsupportedStagePlan {
                reason: "output stage execution only supports CPU output transforms",
            });
        };
        validate_descriptor(*input, frame.descriptor())?;
        let result = CpuColorTransformExecutor::transform(frame, transform)?;
        validate_descriptor(*output, result.frame.descriptor())?;
        validate_descriptor(plan.final_descriptor, result.frame.descriptor())?;
        Ok(RenderColorStageExecution { result, stage_diagnostics: plan.diagnostics() })
    }

    /// Execute a CPU working-space -> display/export stage plan, returning
    /// float output without u8 quantization.
    ///
    /// The stage plan must explicitly declare an `EncodedFloat` output. This
    /// keeps display/export transfer semantics visible to downstream cache,
    /// readback, and delivery code.
    pub fn output_transform_float(
        frame: &CpuColorFrame,
        plan: &RenderColorStagePlan,
    ) -> Result<
        RenderColorStageExecution<RenderOutputTransformFloatResult>,
        RenderColorTransformError,
    > {
        let [stage] = plan.stages.as_slice() else {
            return Err(RenderColorTransformError::UnsupportedStagePlan {
                reason: "output stage execution requires exactly one CPU stage",
            });
        };
        let RenderColorStage::CpuOutputTransform { input, output, transform } = stage else {
            return Err(RenderColorTransformError::UnsupportedStagePlan {
                reason: "output stage execution only supports CPU output transforms",
            });
        };
        validate_descriptor(*input, frame.descriptor())?;
        let result = CpuColorTransformExecutor::transform_float(frame, transform)?;
        validate_descriptor(*output, result.frame.descriptor())?;
        validate_descriptor(plan.final_descriptor, result.frame.descriptor())?;
        Ok(RenderColorStageExecution { result, stage_diagnostics: plan.diagnostics() })
    }

    /// Execute a CPU source/import -> working-space stage plan from a linear
    /// float source, bypassing RGBA8 quantization.
    pub fn input_to_working_float(
        frame: &LinearFloatSource,
        plan: &RenderColorStagePlan,
    ) -> Result<RenderColorStageExecution<RenderInputTransformResult>, RenderColorTransformError>
    {
        let [stage] = plan.stages.as_slice() else {
            return Err(RenderColorTransformError::UnsupportedStagePlan {
                reason: "input stage execution requires exactly one CPU stage",
            });
        };
        let RenderColorStage::CpuInputTransform { input, output, transform } = stage else {
            return Err(RenderColorTransformError::UnsupportedStagePlan {
                reason: "input stage execution only supports CPU input transforms",
            });
        };
        validate_descriptor(*input, frame.descriptor())?;
        let result = CpuColorTransformExecutor::input_to_working_float(frame, transform)?;
        validate_descriptor(*output, result.frame.descriptor())?;
        validate_descriptor(plan.final_descriptor, result.frame.descriptor())?;
        Ok(RenderColorStageExecution { result, stage_diagnostics: plan.diagnostics() })
    }
}

/// Plan and execute a CPU source/import -> working-space color stage.
pub fn execute_cpu_input_stage(
    frame: &CpuEncodedColorFrame,
    transform: &RenderInputTransform,
) -> Result<RenderColorStageExecution<RenderInputTransformResult>, RenderColorTransformError> {
    let mut planner = RenderColorStagePlanner::cpu_only();
    let plan = planner.plan_input_to_working(frame.descriptor(), transform)?;
    CpuRenderColorStageExecutor::input_to_working(frame, &plan)
}

/// Plan and execute a CPU input stage from a linear float source, bypassing
/// RGBA8 quantization entirely.
pub fn execute_cpu_input_stage_float(
    frame: &LinearFloatSource,
    transform: &RenderInputTransform,
) -> Result<RenderColorStageExecution<RenderInputTransformResult>, RenderColorTransformError> {
    let mut planner = RenderColorStagePlanner::cpu_only();
    let plan = planner.plan_input_to_working(frame.descriptor(), transform)?;
    CpuRenderColorStageExecutor::input_to_working_float(frame, &plan)
}

/// Convert a CPU working frame into another linear working identity through OCIO.
pub fn execute_cpu_working_transform(
    frame: &CpuColorFrame,
    destination: WorkingColorSpace,
    engine: ColorEngine,
) -> Result<RenderColorStageExecution<RenderInputTransformResult>, RenderColorTransformError> {
    let descriptor = frame.descriptor();
    let source = descriptor.color_space.working().ok_or(
        RenderColorTransformError::UnsupportedWorkingIdentity { identity: descriptor.color_space },
    )?;
    let data = frame.rgba_f32().data.iter().flat_map(|pixel| pixel.iter().copied()).collect();
    let source = LinearFloatSource::new(descriptor.width, descriptor.height, source, data);
    execute_cpu_input_stage_float(
        &source,
        &RenderInputTransform::to_working(destination, false, engine),
    )
}

/// Plan and execute a CPU working-space -> display/export color stage.
pub fn execute_cpu_output_stage(
    frame: &CpuColorFrame,
    transform: &RenderColorTransform,
) -> Result<RenderColorStageExecution<RenderOutputTransformResult>, RenderColorTransformError> {
    let mut planner = RenderColorStagePlanner::cpu_only();
    let plan = planner.plan_output_transform(frame.descriptor(), transform)?;
    CpuRenderColorStageExecutor::output_transform(frame, &plan)
}

/// Plan and execute a CPU final-output boundary described by preview/export intent.
pub fn execute_cpu_output_boundary(
    frame: &CpuColorFrame,
    boundary: &RenderOutputColorBoundary,
) -> Result<RenderColorStageExecution<RenderOutputTransformResult>, RenderColorTransformError> {
    let mut executor = RenderOutputColorBoundaryExecutor::cpu_only();
    executor.execute(frame, boundary)
}

/// Plan and execute a CPU final-output boundary, returning encoded RGBA8 pixels.
pub fn execute_cpu_output_boundary_rgba8(
    frame: &CpuColorFrame,
    boundary: &RenderOutputColorBoundary,
) -> Result<RenderOutputColorBoundaryRgba8, RenderColorTransformError> {
    let output = execute_cpu_output_boundary(frame, boundary)?;
    let output_descriptor = output.result.frame.descriptor();
    Ok(RenderOutputColorBoundaryRgba8 {
        rgba: output.result.frame.into_rgba(),
        color_diagnostics: output.result.diagnostics,
        stage_diagnostics: output.stage_diagnostics,
        output_descriptor,
    })
}

/// Float output plus diagnostics for a final preview/export color boundary.
///
/// This is the precision-preserving alternative to [`RenderOutputColorBoundaryRgba8`].
/// The caller receives a float display/export frame with the output transform
/// applied, avoiding an intermediate u8 quantization round-trip.
#[derive(Debug)]
pub struct RenderOutputColorBoundaryFloat {
    /// Float display/export frame with output transform applied.
    pub frame: crate::CpuEncodedFloatColorFrame,
    /// Color transform diagnostics emitted by the boundary executor.
    pub color_diagnostics: crate::RenderColorTransformDiagnostics,
    /// Stage diagnostics for the executed boundary plan.
    pub stage_diagnostics: RenderColorStageDiagnostics,
    /// Descriptor of the output frame.
    pub output_descriptor: ColorFrameDescriptor,
}

/// Plan and execute a CPU final-output boundary, returning float output without
/// u8 quantization.
///
/// This is the renderer-owned CPU float/high-bit output boundary for
/// 10-bit delivery. It applies the working -> output color transform
/// through OCIO float processors, preserving HDR/wide-gamut precision.
///
/// For export use: the caller can flatten the float frame into `[f32]` and use
/// `ExportFrameContract::pack_rgba_f32()` to produce `rgba64le` pipe bytes
/// without an intermediate RGBA8 round-trip.
///
/// For display/view use: the caller receives the same float precision without
/// an intermediate u8 boundary, but must still encode for presentation.
pub fn execute_cpu_output_boundary_float(
    frame: &CpuColorFrame,
    boundary: &RenderOutputColorBoundary,
) -> Result<RenderOutputColorBoundaryFloat, RenderColorTransformError> {
    let mut planner = RenderOutputColorBoundaryPlanner::cpu_only();
    let plan = planner.plan_float(frame, boundary)?;
    let output = CpuRenderColorStageExecutor::output_transform_float(frame, &plan.stage_plan)?;
    let output_descriptor = output.result.frame.descriptor();
    Ok(RenderOutputColorBoundaryFloat {
        frame: output.result.frame,
        color_diagnostics: output.result.diagnostics,
        stage_diagnostics: output.stage_diagnostics,
        output_descriptor,
    })
}

impl<'a> RenderColorStagePlanner<'a> {
    /// Create a planner that always schedules CPU color transforms.
    pub fn cpu_only() -> Self {
        Self {
            mode: RenderColorStageMode::CpuOnly,
            gpu_cache: None,
            gpu_options: RenderColorTransformGpuOptions::default(),
        }
    }

    /// Create a planner that prefers GPU OCIO shader stages.
    pub fn prefer_gpu(
        gpu_cache: &'a mut OcioGpuShaderCache,
        gpu_options: RenderColorTransformGpuOptions,
    ) -> Self {
        Self {
            mode: RenderColorStageMode::PreferGpu,
            gpu_cache: Some(gpu_cache),
            gpu_options,
        }
    }

    /// Plan source/import -> timeline working-space color processing.
    pub fn plan_input_to_working(
        &mut self,
        input: ColorFrameDescriptor,
        transform: &RenderInputTransform,
    ) -> Result<RenderColorStagePlan, RenderColorTransformError> {
        if input.domain != ColorFrameDomain::Source {
            return Err(RenderColorTransformError::UnsupportedInputDomain { domain: input.domain });
        }

        match self.mode {
            RenderColorStageMode::CpuOnly => Ok(self.cpu_input_stage(input, transform)),
            RenderColorStageMode::PreferGpu => {
                let plan = self.gpu_planner()?.plan_input_to_working(input, transform)?;
                Ok(Self::gpu_stage_plan(plan))
            }
        }
    }

    /// Plan timeline working-space -> display/export color processing.
    pub fn plan_output_transform(
        &mut self,
        input: ColorFrameDescriptor,
        transform: &RenderColorTransform,
    ) -> Result<RenderColorStagePlan, RenderColorTransformError> {
        if input.domain != ColorFrameDomain::Working {
            return Err(RenderColorTransformError::UnsupportedInputDomain { domain: input.domain });
        }

        match self.mode {
            RenderColorStageMode::CpuOnly => Ok(self.cpu_output_stage(input, transform)),
            RenderColorStageMode::PreferGpu => {
                let plan = self.gpu_planner()?.plan_output_transform(input, transform)?;
                Ok(Self::gpu_stage_plan(plan))
            }
        }
    }

    /// Plan timeline working-space -> display/export processing while
    /// preserving the destination encoding in floating-point samples.
    pub fn plan_output_transform_float(
        &mut self,
        input: ColorFrameDescriptor,
        transform: &RenderColorTransform,
    ) -> Result<RenderColorStagePlan, RenderColorTransformError> {
        if input.domain != ColorFrameDomain::Working {
            return Err(RenderColorTransformError::UnsupportedInputDomain { domain: input.domain });
        }

        match self.mode {
            RenderColorStageMode::CpuOnly => Ok(self.cpu_output_stage_with_encoding(
                input,
                transform,
                ColorFrameEncoding::EncodedFloat,
            )),
            RenderColorStageMode::PreferGpu => {
                let plan = self.gpu_planner()?.plan_output_transform_with_encoding(
                    input,
                    transform,
                    ColorFrameEncoding::EncodedFloat,
                )?;
                Ok(Self::gpu_stage_plan(plan))
            }
        }
    }

    fn gpu_planner(
        &mut self,
    ) -> Result<RenderColorTransformGpuPlanner<'_>, RenderColorTransformError> {
        let cache = self
            .gpu_cache
            .as_deref_mut()
            .ok_or(RenderColorTransformError::GpuPlannerUnavailable)?;
        Ok(RenderColorTransformGpuPlanner::new(cache, self.gpu_options))
    }

    fn cpu_input_stage(
        &self,
        input: ColorFrameDescriptor,
        transform: &RenderInputTransform,
    ) -> RenderColorStagePlan {
        let output = ColorFrameDescriptor {
            width: input.width,
            height: input.height,
            color_space: transform.working_color_space.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Cpu,
        };
        RenderColorStagePlan {
            stages: vec![RenderColorStage::CpuInputTransform {
                input,
                output,
                transform: transform.clone(),
            }],
            final_descriptor: output,
        }
    }

    fn cpu_output_stage(
        &self,
        input: ColorFrameDescriptor,
        transform: &RenderColorTransform,
    ) -> RenderColorStagePlan {
        self.cpu_output_stage_with_encoding(input, transform, ColorFrameEncoding::EncodedRgba8)
    }

    fn cpu_output_stage_with_encoding(
        &self,
        input: ColorFrameDescriptor,
        transform: &RenderColorTransform,
        encoding: ColorFrameEncoding,
    ) -> RenderColorStagePlan {
        let output = ColorFrameDescriptor {
            width: input.width,
            height: input.height,
            color_space: transform.output_color_space.into(),
            domain: transform.output_domain,
            encoding,
            residency: ColorFrameResidency::Cpu,
        };
        RenderColorStagePlan {
            stages: vec![RenderColorStage::CpuOutputTransform {
                input,
                output,
                transform: transform.clone(),
            }],
            final_descriptor: output,
        }
    }

    fn gpu_stage_plan(plan: RenderColorTransformGpuPlan) -> RenderColorStagePlan {
        let mut stages = Vec::with_capacity(3);
        let gpu_input = plan.diagnostics.input.with_residency(ColorFrameResidency::Gpu);
        if plan.requires_source_upload {
            stages.push(RenderColorStage::UploadToGpu {
                input: plan.diagnostics.input,
                output: gpu_input,
            });
        }

        let gpu_output = plan.diagnostics.output.with_residency(ColorFrameResidency::Gpu);
        let final_descriptor = plan.diagnostics.output;
        stages.push(RenderColorStage::GpuColorTransform {
            input: gpu_input,
            output: gpu_output,
            plan: Box::new(plan),
        });

        if final_descriptor.residency == ColorFrameResidency::Cpu {
            stages.push(RenderColorStage::ReadbackToCpu {
                input: gpu_output,
                output: final_descriptor,
            });
        }

        RenderColorStagePlan { stages, final_descriptor }
    }
}

fn validate_descriptor(
    expected: ColorFrameDescriptor,
    actual: ColorFrameDescriptor,
) -> Result<(), RenderColorTransformError> {
    if expected != actual {
        return Err(RenderColorTransformError::StageDescriptorMismatch { expected, actual });
    }
    Ok(())
}

fn color_target_format_for_gpu_frame(frame: &GpuColorFrameHandle) -> OcioGpuWgpuColorTargetFormat {
    match frame.texture_format() {
        GpuColorFrameTextureFormat::Rgba8Unorm => OcioGpuWgpuColorTargetFormat::Rgba8Unorm,
        GpuColorFrameTextureFormat::Rgba16Float => OcioGpuWgpuColorTargetFormat::Rgba16Float,
        GpuColorFrameTextureFormat::Rgba32Float => OcioGpuWgpuColorTargetFormat::Rgba32Float,
    }
}

struct PlannedGpuUploadTransform<'a> {
    upload_input: ColorFrameDescriptor,
    gpu_input: ColorFrameDescriptor,
    gpu_output: ColorFrameDescriptor,
    readback_output: Option<ColorFrameDescriptor>,
    transform: &'a RenderColorTransformGpuPlan,
}

struct PlannedGpuSourceUploadTransform<'a> {
    upload_input: ColorFrameDescriptor,
    gpu_input: ColorFrameDescriptor,
    gpu_output: ColorFrameDescriptor,
    transform: &'a RenderColorTransformGpuPlan,
}

struct PlannedGpuSourceTransform<'a> {
    gpu_input: ColorFrameDescriptor,
    gpu_output: ColorFrameDescriptor,
    transform: &'a RenderColorTransformGpuPlan,
}

struct PlannedGpuTransform<'a> {
    gpu_input: ColorFrameDescriptor,
    gpu_output: ColorFrameDescriptor,
    readback_output: Option<ColorFrameDescriptor>,
    transform: &'a RenderColorTransformGpuPlan,
}

fn planned_gpu_source_upload_transform(
    stage_plan: &RenderColorStagePlan,
) -> Result<PlannedGpuSourceUploadTransform<'_>, RenderGpuInputStageResourcePlanError> {
    let stages = stage_plan.stages.as_slice();
    let [upload @ RenderColorStage::UploadToGpu { .. }, transform @ RenderColorStage::GpuColorTransform { .. }] =
        stages
    else {
        return Err(RenderGpuInputStageResourcePlanError::UnsupportedStagePlan {
            reason: "GPU input resources require UploadToGpu -> GpuColorTransform",
        });
    };

    let RenderColorStage::UploadToGpu { input: upload_input, output: upload_output } = upload
    else {
        unreachable!("matched upload stage")
    };
    let RenderColorStage::GpuColorTransform {
        input: gpu_input,
        output: gpu_output,
        plan: transform,
    } = transform
    else {
        unreachable!("matched GPU transform stage")
    };
    if upload_output != gpu_input {
        return Err(RenderGpuInputStageResourcePlanError::UnsupportedStagePlan {
            reason: "upload output descriptor must equal GPU transform input descriptor",
        });
    }
    if gpu_input.residency != ColorFrameResidency::Gpu
        || gpu_output.residency != ColorFrameResidency::Gpu
    {
        return Err(RenderGpuInputStageResourcePlanError::UnsupportedStagePlan {
            reason: "GPU transform input and output descriptors must be GPU-resident",
        });
    }
    if upload_input.domain != ColorFrameDomain::Source
        || upload_input.encoding != ColorFrameEncoding::EncodedRgba8
        || upload_input.residency != ColorFrameResidency::Cpu
    {
        return Err(RenderGpuInputStageResourcePlanError::UnsupportedStagePlan {
            reason: "GPU input upload must consume a CPU encoded source frame",
        });
    }
    if gpu_output.domain != ColorFrameDomain::Working
        || gpu_output.encoding != ColorFrameEncoding::LinearFloat
    {
        return Err(RenderGpuInputStageResourcePlanError::UnsupportedStagePlan {
            reason: "GPU input transform must produce a linear working frame",
        });
    }
    if stage_plan.final_descriptor != *gpu_output {
        return Err(RenderGpuInputStageResourcePlanError::UnsupportedStagePlan {
            reason: "GPU input final descriptor must be the GPU working output descriptor",
        });
    }
    Ok(PlannedGpuSourceUploadTransform {
        upload_input: *upload_input,
        gpu_input: *gpu_input,
        gpu_output: *gpu_output,
        transform,
    })
}

fn planned_gpu_source_transform(
    stage_plan: &RenderColorStagePlan,
) -> Result<PlannedGpuSourceTransform<'_>, RenderGpuInputStageResourcePlanError> {
    let [RenderColorStage::GpuColorTransform {
        input: gpu_input,
        output: gpu_output,
        plan: transform,
    }] = stage_plan.stages.as_slice()
    else {
        return Err(RenderGpuInputStageResourcePlanError::UnsupportedStagePlan {
            reason: "GPU-resident input resources require one GpuColorTransform stage",
        });
    };
    if gpu_input.residency != ColorFrameResidency::Gpu
        || gpu_input.domain != ColorFrameDomain::Source
        || gpu_input.encoding != ColorFrameEncoding::EncodedFloat
    {
        return Err(RenderGpuInputStageResourcePlanError::UnsupportedStagePlan {
            reason: "GPU input transform must consume a GPU encoded-float source frame",
        });
    }
    if gpu_output.residency != ColorFrameResidency::Gpu
        || gpu_output.domain != ColorFrameDomain::Working
        || gpu_output.encoding != ColorFrameEncoding::LinearFloat
    {
        return Err(RenderGpuInputStageResourcePlanError::UnsupportedStagePlan {
            reason: "GPU input transform must produce a GPU linear working frame",
        });
    }
    if stage_plan.final_descriptor != *gpu_output {
        return Err(RenderGpuInputStageResourcePlanError::UnsupportedStagePlan {
            reason: "GPU input final descriptor must be the GPU working output descriptor",
        });
    }
    Ok(PlannedGpuSourceTransform {
        gpu_input: *gpu_input,
        gpu_output: *gpu_output,
        transform,
    })
}

fn planned_gpu_upload_transform_readback(
    stage_plan: &RenderColorStagePlan,
) -> Result<PlannedGpuUploadTransform<'_>, RenderGpuOutputStageResourcePlanError> {
    let stages = stage_plan.stages.as_slice();
    let (upload, transform, readback) = match stages {
        [upload @ RenderColorStage::UploadToGpu { .. }, transform @ RenderColorStage::GpuColorTransform { .. }] => {
            (upload, transform, None)
        }
        [upload @ RenderColorStage::UploadToGpu { .. }, transform @ RenderColorStage::GpuColorTransform { .. }, readback @ RenderColorStage::ReadbackToCpu { .. }] => {
            (upload, transform, Some(readback))
        }
        _ => {
            return Err(RenderGpuOutputStageResourcePlanError::UnsupportedStagePlan {
                reason: "GPU output resources require UploadToGpu -> GpuColorTransform with optional ReadbackToCpu",
            });
        }
    };

    let RenderColorStage::UploadToGpu { input: upload_input, output: upload_output } = upload
    else {
        unreachable!("matched upload stage")
    };
    let RenderColorStage::GpuColorTransform {
        input: gpu_input,
        output: gpu_output,
        plan: transform,
    } = transform
    else {
        unreachable!("matched GPU transform stage")
    };
    if upload_output != gpu_input {
        return Err(
            RenderGpuOutputStageResourcePlanError::UnsupportedStagePlan {
                reason: "upload output descriptor must equal GPU transform input descriptor",
            },
        );
    }
    if gpu_input.residency != ColorFrameResidency::Gpu
        || gpu_output.residency != ColorFrameResidency::Gpu
    {
        return Err(
            RenderGpuOutputStageResourcePlanError::UnsupportedStagePlan {
                reason: "GPU transform input and output descriptors must be GPU-resident",
            },
        );
    }
    match readback {
        Some(RenderColorStage::ReadbackToCpu { input, output }) => {
            if input != gpu_output || *output != stage_plan.final_descriptor {
                return Err(
                    RenderGpuOutputStageResourcePlanError::UnsupportedStagePlan {
                        reason: "readback descriptors must connect GPU output to final descriptor",
                    },
                );
            }
        }
        None => {
            if *gpu_output != stage_plan.final_descriptor {
                return Err(RenderGpuOutputStageResourcePlanError::UnsupportedStagePlan {
                    reason: "GPU output descriptor must equal final descriptor when there is no readback",
                });
            }
        }
        Some(_) => unreachable!("matched optional readback stage"),
    }

    Ok(PlannedGpuUploadTransform {
        upload_input: *upload_input,
        gpu_input: *gpu_input,
        gpu_output: *gpu_output,
        readback_output: readback.map(|stage| match stage {
            RenderColorStage::ReadbackToCpu { output, .. } => *output,
            _ => unreachable!("matched optional readback stage"),
        }),
        transform,
    })
}

fn planned_gpu_transform_readback(
    stage_plan: &RenderColorStagePlan,
) -> Result<PlannedGpuTransform<'_>, RenderGpuOutputStageResourcePlanError> {
    let stages = stage_plan.stages.as_slice();
    let (transform, readback) = match stages {
        [transform @ RenderColorStage::GpuColorTransform { .. }] => (transform, None),
        [transform @ RenderColorStage::GpuColorTransform { .. }, readback @ RenderColorStage::ReadbackToCpu { .. }] => {
            (transform, Some(readback))
        }
        _ => {
            return Err(RenderGpuOutputStageResourcePlanError::UnsupportedStagePlan {
                reason: "GPU-resident output resources require GpuColorTransform with optional ReadbackToCpu",
            });
        }
    };

    let RenderColorStage::GpuColorTransform {
        input: gpu_input,
        output: gpu_output,
        plan: transform,
    } = transform
    else {
        unreachable!("matched GPU transform stage")
    };
    if gpu_input.residency != ColorFrameResidency::Gpu
        || gpu_output.residency != ColorFrameResidency::Gpu
    {
        return Err(
            RenderGpuOutputStageResourcePlanError::UnsupportedStagePlan {
                reason: "GPU transform input and output descriptors must be GPU-resident",
            },
        );
    }
    match readback {
        Some(RenderColorStage::ReadbackToCpu { input, output }) => {
            if input != gpu_output || *output != stage_plan.final_descriptor {
                return Err(
                    RenderGpuOutputStageResourcePlanError::UnsupportedStagePlan {
                        reason: "readback descriptors must connect GPU output to final descriptor",
                    },
                );
            }
        }
        None => {
            if *gpu_output != stage_plan.final_descriptor {
                return Err(RenderGpuOutputStageResourcePlanError::UnsupportedStagePlan {
                    reason: "GPU output descriptor must equal final descriptor when there is no readback",
                });
            }
        }
        Some(_) => unreachable!("matched optional readback stage"),
    }

    Ok(PlannedGpuTransform {
        gpu_input: *gpu_input,
        gpu_output: *gpu_output,
        readback_output: readback.map(|stage| match stage {
            RenderColorStage::ReadbackToCpu { output, .. } => *output,
            _ => unreachable!("matched optional readback stage"),
        }),
        transform,
    })
}

fn validate_materialization_table_slot<R>(
    table: &GpuColorFrameResourceTable<R>,
    handle: &GpuColorFrameHandle,
) -> Result<(), RenderGpuOutputStageMaterializeError> {
    match table.get(handle) {
        Ok(_) | Err(GpuColorFrameResourceTableError::MissingFrame { .. }) => Ok(()),
        Err(err) => Err(RenderGpuOutputStageMaterializeError::ResourceTable(err)),
    }
}

#[derive(Debug, Clone, Copy)]
enum RenderGpuColorPassExecutionFrameRole {
    Input,
    Output,
}

fn validate_execution_frame(
    expected: &GpuColorFrameHandle,
    actual: &GpuColorFrameHandle,
    role: RenderGpuColorPassExecutionFrameRole,
) -> Result<(), RenderGpuColorPassExecutionError> {
    if expected.id() != actual.id() {
        return Err(match role {
            RenderGpuColorPassExecutionFrameRole::Input => {
                RenderGpuColorPassExecutionError::InputFrameIdMismatch {
                    expected: expected.id(),
                    actual: actual.id(),
                }
            }
            RenderGpuColorPassExecutionFrameRole::Output => {
                RenderGpuColorPassExecutionError::OutputFrameIdMismatch {
                    expected: expected.id(),
                    actual: actual.id(),
                }
            }
        });
    }
    if expected.descriptor() != actual.descriptor() {
        return Err(match role {
            RenderGpuColorPassExecutionFrameRole::Input => {
                RenderGpuColorPassExecutionError::InputDescriptorMismatch {
                    expected: expected.descriptor(),
                    actual: actual.descriptor(),
                }
            }
            RenderGpuColorPassExecutionFrameRole::Output => {
                RenderGpuColorPassExecutionError::OutputDescriptorMismatch {
                    expected: expected.descriptor(),
                    actual: actual.descriptor(),
                }
            }
        });
    }
    if expected.texture_format() != actual.texture_format() {
        return Err(match role {
            RenderGpuColorPassExecutionFrameRole::Input => {
                RenderGpuColorPassExecutionError::InputTextureFormatMismatch {
                    expected: expected.texture_format(),
                    actual: actual.texture_format(),
                }
            }
            RenderGpuColorPassExecutionFrameRole::Output => {
                RenderGpuColorPassExecutionError::OutputTextureFormatMismatch {
                    expected: expected.texture_format(),
                    actual: actual.texture_format(),
                }
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        GpuColorFrameId, GpuColorFrameIdAllocator, GpuColorFrameReadbackPlan,
        GpuColorFrameTextureFormat, GpuContext, OcioGpuShaderRequest, OcioGpuWgpuBlocker,
        RenderColorTransformBackend,
    };
    use mondrian_core::types::{ColorEngine, ColorSpace};
    use mondrian_core::WorkingColorSpace;
    use mondrian_core::WorkingRgbaF32Frame;
    use mondrian_core::{
        ensure_mondrian_default_ocio_loaded, ocio_default_display_view, GpuLanguage,
    };
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::PathBuf;

    #[derive(Debug, serde::Serialize)]
    struct GpuOutputBoundarySmokeReport {
        scenario: &'static str,
        skipped: Option<String>,
        adapter: Option<GpuOutputAdapterReport>,
        frame: RenderGpuOutputFrameReport,
        output_texture_format: &'static str,
        health_report: RenderGpuOutputHealthReport,
        stage: RenderGpuOutputStageDiagnosticsReport,
        runtime: RenderGpuOutputRuntimeDiagnosticsReport,
        readback_bytes: usize,
        max_rgba_delta: u8,
        tolerance: u8,
    }

    #[derive(Debug, serde::Serialize)]
    struct GpuOutputAdapterReport {
        name: String,
        backend: String,
        device_type: String,
        driver: String,
        driver_info: String,
    }

    #[test]
    fn gpu_output_health_report_classifies_native_path() {
        let frame = RenderGpuOutputFrameReport {
            width: 2,
            height: 2,
            pixel_count: 4,
            input_color_space: ColorSpace::Rec709,
            output_color_space: ColorSpace::Srgb,
        };
        let stage = RenderGpuOutputStageDiagnosticsReport {
            total_stages: 3,
            upload_stages: 1,
            gpu_color_stages: 1,
            readback_stages: 1,
            gpu_blockers: 0,
            gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown::default(),
            stage_pixels: 12,
        };
        let runtime = RenderGpuOutputRuntimeDiagnosticsReport {
            shader_cache_entries: 1,
            shader_cache_hits: 0,
            shader_cache_misses: 1,
            shader_cache_extraction_failures: 0,
            backend_prep_resource_entries: 1,
            backend_object_entries: 1,
            backend_object_hits: 0,
            backend_object_misses: 1,
            backend_object_failures: 0,
            frame_table_entries: 2,
            next_frame_id: 3,
        };

        let passed =
            RenderGpuOutputHealthSummary::evaluate(false, &frame, &stage, &runtime, 16, 2, 3);
        let passed_report = RenderGpuOutputHealthReport::build(
            "renderer_gpu_output_boundary",
            None,
            passed,
            &stage,
            &runtime,
            16,
            2,
            3,
        );
        assert_eq!(passed_report.verdict, RenderGpuOutputHealthVerdict::Pass);
        assert_eq!(
            passed_report.summary.status,
            RenderGpuOutputHealthStatus::Passed
        );
        assert!(passed_report.summary.native_gpu_output_ready);
        assert!(passed_report.summary.parity_within_tolerance);
        assert_eq!(passed_report.summary.expected_readback_bytes, 16);
        assert!(passed_report.root_causes.is_empty());
        assert!(passed_report.actions.is_empty());

        let incomplete_readback =
            RenderGpuOutputHealthSummary::evaluate(false, &frame, &stage, &runtime, 12, 2, 3);
        let incomplete_readback_report = RenderGpuOutputHealthReport::build(
            "renderer_gpu_output_boundary",
            None,
            incomplete_readback,
            &stage,
            &runtime,
            12,
            2,
            3,
        );
        assert_eq!(
            incomplete_readback_report.verdict,
            RenderGpuOutputHealthVerdict::Fail
        );
        assert_eq!(
            incomplete_readback_report.summary.status,
            RenderGpuOutputHealthStatus::Failed
        );
        assert!(!incomplete_readback_report.summary.readback_complete);
        assert!(!incomplete_readback_report.summary.native_gpu_output_ready);
        assert!(incomplete_readback_report
            .root_causes
            .iter()
            .any(|root| root.code == "gpu_readback_incomplete"));

        let skipped =
            RenderGpuOutputHealthSummary::evaluate(true, &frame, &stage, &runtime, 16, 2, 3);
        let skipped_report = RenderGpuOutputHealthReport::build(
            "renderer_gpu_output_boundary",
            Some("no GPU adapter available".to_owned()),
            skipped,
            &stage,
            &runtime,
            16,
            2,
            3,
        );
        assert_eq!(skipped_report.verdict, RenderGpuOutputHealthVerdict::Fail);
        assert_eq!(
            skipped_report.summary.status,
            RenderGpuOutputHealthStatus::Skipped
        );
        assert!(!skipped_report.summary.native_gpu_output_ready);
        assert!(skipped_report
            .root_causes
            .iter()
            .any(|root| root.code == "gpu_adapter_unavailable"));
    }

    #[test]
    fn gpu_output_smoke_report_serializes_health_report() {
        let frame = RenderGpuOutputFrameReport {
            width: 2,
            height: 2,
            pixel_count: 4,
            input_color_space: ColorSpace::Rec709,
            output_color_space: ColorSpace::Srgb,
        };
        let stage = RenderGpuOutputStageDiagnosticsReport {
            total_stages: 3,
            upload_stages: 1,
            gpu_color_stages: 1,
            readback_stages: 1,
            gpu_blockers: 0,
            gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown::default(),
            stage_pixels: 12,
        };
        let runtime = RenderGpuOutputRuntimeDiagnosticsReport {
            shader_cache_entries: 1,
            shader_cache_hits: 0,
            shader_cache_misses: 1,
            shader_cache_extraction_failures: 0,
            backend_prep_resource_entries: 1,
            backend_object_entries: 1,
            backend_object_hits: 0,
            backend_object_misses: 1,
            backend_object_failures: 0,
            frame_table_entries: 2,
            next_frame_id: 3,
        };
        let health =
            RenderGpuOutputHealthSummary::evaluate(false, &frame, &stage, &runtime, 16, 2, 3);
        let report = GpuOutputBoundarySmokeReport {
            scenario: "renderer_gpu_output_boundary",
            skipped: None,
            adapter: None,
            frame,
            output_texture_format: "Rgba8Unorm",
            health_report: RenderGpuOutputHealthReport::build(
                "renderer_gpu_output_boundary",
                None,
                health,
                &stage,
                &runtime,
                16,
                2,
                3,
            ),
            stage,
            runtime,
            readback_bytes: 16,
            max_rgba_delta: 2,
            tolerance: 3,
        };

        let json = serde_json::to_value(&report).expect("serialize renderer smoke report");
        assert!(json.get("health").is_none());
        assert!(json.get("health_failures").is_none());
        assert!(json.get("passed").is_none());
        assert_eq!(json["health_report"]["schema_version"], 1);
        assert_eq!(json["health_report"]["verdict"], "Pass");
        assert_eq!(
            json["health_report"]["summary"]["expected_readback_bytes"],
            16
        );
    }

    fn source_descriptor(residency: ColorFrameResidency) -> ColorFrameDescriptor {
        ColorFrameDescriptor {
            width: 1280,
            height: 720,
            color_space: ColorSpace::SLog3.into(),
            domain: ColorFrameDomain::Source,
            encoding: ColorFrameEncoding::EncodedRgba8,
            residency,
        }
    }

    fn working_descriptor(residency: ColorFrameResidency) -> ColorFrameDescriptor {
        ColorFrameDescriptor {
            width: 1920,
            height: 1080,
            color_space: WorkingColorSpace::LinearRec709.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency,
        }
    }

    #[test]
    fn cpu_input_plan_uses_single_cpu_stage() {
        let transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::MondrianSmart,
        );
        let mut planner = RenderColorStagePlanner::cpu_only();

        let plan = planner
            .plan_input_to_working(source_descriptor(ColorFrameResidency::Cpu), &transform)
            .expect("CPU input plan");

        assert_eq!(plan.stages.len(), 1);
        assert!(!plan.contains_gpu_transform());
        assert!(!plan.contains_transfer());
        assert_eq!(plan.final_descriptor.residency, ColorFrameResidency::Cpu);
        assert_eq!(plan.final_descriptor.domain, ColorFrameDomain::Working);
    }

    #[test]
    fn cpu_stage_executor_runs_input_plan_and_validates_descriptors() {
        let source = CpuEncodedColorFrame::source_rgba8(
            1280,
            720,
            ColorSpace::SLog3,
            vec![128; 1280 * 720 * 4],
        );
        let transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::MondrianSmart,
        );
        let mut planner = RenderColorStagePlanner::cpu_only();
        let plan = planner
            .plan_input_to_working(source.descriptor(), &transform)
            .expect("CPU input plan");

        let result = CpuRenderColorStageExecutor::input_to_working(&source, &plan)
            .expect("execute CPU input plan");

        assert_eq!(result.result.frame.descriptor(), plan.final_descriptor);
        assert_eq!(result.result.diagnostics.input, source.descriptor());
        assert_eq!(result.result.diagnostics.output, plan.final_descriptor);
        assert_eq!(result.stage_diagnostics, plan.diagnostics());
    }

    #[test]
    fn cpu_stage_executor_rejects_gpu_plan() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let source = CpuEncodedColorFrame::source_rgba8(
            1280,
            720,
            ColorSpace::SLog3,
            vec![128; 1280 * 720 * 4],
        );
        let transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::MondrianSmart,
        );
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderColorStagePlanner::prefer_gpu(
            &mut cache,
            RenderColorTransformGpuOptions::default(),
        );
        let plan = planner
            .plan_input_to_working(source.descriptor(), &transform)
            .expect("GPU input plan");

        let err = CpuRenderColorStageExecutor::input_to_working(&source, &plan)
            .expect_err("CPU executor must reject GPU plans");

        assert!(matches!(
            err,
            RenderColorTransformError::UnsupportedStagePlan { .. }
        ));
    }

    #[test]
    fn cpu_stage_executor_runs_output_plan_and_validates_descriptors() {
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 1920,
            height: 1080,
            data: vec![[0.25, 0.5, 0.75, 1.0]; 1920 * 1080],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let transform =
            RenderColorTransform::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);
        let mut planner = RenderColorStagePlanner::cpu_only();
        let plan = planner
            .plan_output_transform(frame.descriptor(), &transform)
            .expect("CPU output plan");

        let result = CpuRenderColorStageExecutor::output_transform(&frame, &plan)
            .expect("execute CPU output plan");

        assert_eq!(result.result.frame.descriptor(), plan.final_descriptor);
        assert_eq!(result.result.diagnostics.input, frame.descriptor());
        assert_eq!(result.result.diagnostics.output, plan.final_descriptor);
        assert_eq!(result.stage_diagnostics, plan.diagnostics());
    }

    #[test]
    fn cpu_stage_helpers_plan_and_execute_transforms() {
        let source =
            CpuEncodedColorFrame::source_rgba8(2, 2, ColorSpace::Rec709, vec![96; 2 * 2 * 4]);
        let input_transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::MondrianSmart,
        );
        let working = execute_cpu_input_stage(&source, &input_transform)
            .expect("helper should execute input stage");
        assert_eq!(
            working.result.frame.descriptor().domain,
            ColorFrameDomain::Working
        );
        assert_eq!(working.stage_diagnostics.cpu_input_stages, 1);

        let output_transform =
            RenderColorTransform::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);
        let output = execute_cpu_output_stage(&working.result.frame, &output_transform)
            .expect("helper should execute output stage");
        assert_eq!(
            output.result.frame.descriptor().domain,
            ColorFrameDomain::Display
        );
        assert_eq!(
            output.result.frame.descriptor().residency,
            ColorFrameResidency::Cpu
        );
        assert_eq!(output.stage_diagnostics.cpu_output_stages, 1);
    }

    #[test]
    fn cpu_output_boundary_executes_display_target() {
        let frame = cpu_working_frame();
        let boundary =
            RenderOutputColorBoundary::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);

        let output = execute_cpu_output_boundary(&frame, &boundary)
            .expect("display boundary should execute");

        assert_eq!(
            output.result.frame.descriptor().domain,
            ColorFrameDomain::Display
        );
        assert_eq!(
            output.result.diagnostics.direction,
            crate::RenderColorTransformDirection::WorkingToOutput
        );
        assert_eq!(output.stage_diagnostics.cpu_output_stages, 1);
    }

    #[test]
    fn cpu_output_boundary_rgba8_helper_returns_pixels_and_diagnostics() {
        let frame = cpu_working_frame();
        let boundary =
            RenderOutputColorBoundary::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);

        let output = execute_cpu_output_boundary_rgba8(&frame, &boundary)
            .expect("display boundary should encode RGBA8");

        assert_eq!(output.output_descriptor.domain, ColorFrameDomain::Display);
        assert_eq!(
            output.output_descriptor.encoding,
            ColorFrameEncoding::EncodedRgba8
        );
        assert_eq!(
            output.rgba.len(),
            output.output_descriptor.pixel_count() * 4
        );
        assert_eq!(output.color_diagnostics.input, frame.descriptor());
        assert_eq!(output.color_diagnostics.output, output.output_descriptor);
        assert_eq!(output.stage_diagnostics.cpu_output_stages, 1);
        assert_eq!(output.stage_diagnostics.gpu_color_stages, 0);
    }

    #[test]
    fn output_boundary_executor_cpu_only_executes_display_target() {
        let frame = cpu_working_frame();
        let boundary =
            RenderOutputColorBoundary::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);
        let mut executor = RenderOutputColorBoundaryExecutor::cpu_only();

        let output = executor.execute(&frame, &boundary).expect("display boundary should execute");

        assert_eq!(
            output.result.frame.descriptor().domain,
            ColorFrameDomain::Display
        );
        assert_eq!(
            output.result.diagnostics.direction,
            crate::RenderColorTransformDirection::WorkingToOutput
        );
        assert_eq!(output.stage_diagnostics.cpu_output_stages, 1);
        assert_eq!(output.stage_diagnostics.gpu_color_stages, 0);
    }

    #[test]
    fn gpu_output_boundary_runtime_owns_shader_cache_and_frame_ids() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let mut runtime = RenderGpuOutputBoundaryRuntime::with_first_frame_id(900);

        assert_eq!(
            runtime.diagnostics(),
            RenderGpuOutputBoundaryRuntimeDiagnostics {
                shader_cache: OcioGpuShaderCache::default().diagnostics(),
                backend_prep: OcioGpuWgpuBackendPrepRuntime::default().diagnostics(),
                backend_objects: OcioGpuWgpuBackendObjectRuntime::default().diagnostics(),
                next_frame_id: 900,
                frame_table_entries: 0
            }
        );

        runtime
            .shader_cache_mut()
            .get_or_extract(OcioGpuShaderRequest::ColorSpace {
                src: ColorSpace::SLog3.into(),
                dst: ColorSpace::Rec709.into(),
                language: GpuLanguage::Glsl4_0,
            })
            .expect("shader extraction");

        let diagnostics = runtime.diagnostics();
        assert_eq!(diagnostics.shader_cache.entries, 1);
        assert_eq!(diagnostics.shader_cache.misses, 1);
        assert_eq!(diagnostics.backend_prep.resources.entries, 0);
        assert_eq!(diagnostics.backend_objects.entries, 0);
        assert_eq!(diagnostics.next_frame_id, 900);
        assert_eq!(diagnostics.frame_table_entries, 0);

        runtime.clear_frame_resources();
        assert_eq!(runtime.diagnostics().frame_table_entries, 0);
    }

    #[test]
    fn gpu_output_boundary_plans_explicit_working_to_encoded_processor() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let frame = cpu_working_frame();
        let boundary =
            RenderOutputColorBoundary::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);
        let mut runtime = RenderGpuOutputBoundaryRuntime::with_first_frame_id(1_000);
        let mut planner = RenderOutputColorBoundaryPlanner::prefer_gpu(
            runtime.shader_cache_mut(),
            RenderColorTransformGpuOptions {
                output_residency: ColorFrameResidency::Cpu,
                ..RenderColorTransformGpuOptions::default()
            },
        );

        let plan = planner
            .plan(&frame, &boundary)
            .expect("linear working GPU output boundary should plan diagnostics");
        assert_eq!(
            plan.stage_plan.diagnostics().gpu_color_stages,
            1,
            "the GPU output stage remains visible with its wrapper color contract"
        );
        let RenderColorStage::GpuColorTransform { plan: gpu_plan, .. } = &plan.stage_plan.stages[1]
        else {
            panic!("expected upload -> GPU transform -> readback stage shape");
        };
        assert!(gpu_plan.wgpu.blockers.is_empty());
        assert!(gpu_plan.wgpu.can_execute());

        let mut ids = GpuColorFrameIdAllocator::new(1_000);
        let resources = plan
            .gpu_resource_plan(&mut ids, &frame, GpuColorFrameTextureFormat::Rgba8Unorm)
            .expect("linear-aware GPU output resources should materialize");
        assert!(matches!(
            resources.transform.wgpu.shader_plan.request,
            crate::OcioGpuShaderRequest::ColorSpace {
                src: mondrian_core::OcioColorSpaceIdentity::Working(
                    mondrian_core::WorkingColorSpace::LinearRec709
                ),
                dst: mondrian_core::OcioColorSpaceIdentity::Encoded(ColorSpace::Srgb),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn gpu_output_boundary_runtime_matches_cpu_output_on_real_wgpu_device() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping real wgpu output parity test: no GPU adapter available");
            return;
        };
        let frame = cpu_working_frame();
        let boundary =
            RenderOutputColorBoundary::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);
        let expected = execute_cpu_output_boundary_rgba8(&frame, &boundary)
            .expect("CPU display boundary should encode RGBA8");
        let mut runtime = RenderGpuOutputBoundaryRuntime::with_first_frame_id(1_000);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-test-gpu-output-boundary-parity"),
        });

        let record = runtime
            .record_wgpu_output_boundary_owned_backend(
                &boundary,
                &frame,
                GpuColorFrameTextureFormat::Rgba8Unorm,
                RenderColorTransformGpuOptions {
                    output_residency: ColorFrameResidency::Cpu,
                    ..RenderColorTransformGpuOptions::default()
                },
                RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                    device: &context.device,
                    queue: &context.queue,
                    encoder: &mut encoder,
                    load_op: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                },
            )
            .expect("runtime-owned GPU output boundary should record");
        let readback_buffer = record
            .readback_buffer
            .expect("CPU-resident GPU output boundary should record readback");

        context.queue.submit(std::iter::once(encoder.finish()));
        let readback_plan = GpuColorFrameReadbackPlan::encoded_rgba8(record.materialized.output)
            .expect("GPU output should be readable as RGBA8");
        let mapped = map_readback_buffer(&context.device, &readback_buffer);
        let actual = readback_plan
            .unpack_mapped_rgba8(&mapped)
            .expect("readback should unpack into encoded frame");
        readback_buffer.unmap();

        assert_srgb_display_accurate(&expected.rgba, actual.rgba());
        assert_eq!(record.stage_diagnostics.total_stages, 3);
        assert_eq!(record.stage_diagnostics.upload_stages, 1);
        assert_eq!(record.stage_diagnostics.gpu_color_stages, 1);
        assert_eq!(record.stage_diagnostics.readback_stages, 1);
    }

    #[tokio::test]
    async fn gpu_input_stage_runtime_records_source_upload_and_working_output_on_real_wgpu_device()
    {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping real wgpu input stage test: no GPU adapter available");
            return;
        };
        let source = cpu_source_frame();
        let transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::MondrianSmart,
        );
        let mut runtime = RenderGpuOutputBoundaryRuntime::with_first_frame_id(1_300);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-test-gpu-input-stage"),
        });

        let record = runtime
            .record_wgpu_input_stage_owned_backend(
                &transform,
                &source,
                RenderColorTransformGpuOptions::default(),
                RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                    device: &context.device,
                    queue: &context.queue,
                    encoder: &mut encoder,
                    load_op: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                },
            )
            .expect("runtime-owned GPU input stage should record");
        context.queue.submit(std::iter::once(encoder.finish()));

        assert_eq!(record.materialized.input.id().raw(), 1_300);
        assert_eq!(record.materialized.output.id().raw(), 1_301);
        assert_eq!(record.stage_diagnostics.total_stages, 2);
        assert_eq!(record.stage_diagnostics.upload_stages, 1);
        assert_eq!(record.stage_diagnostics.gpu_color_stages, 1);
        assert_eq!(record.stage_diagnostics.readback_stages, 0);
        assert_eq!(
            record.materialized.output.descriptor().domain,
            ColorFrameDomain::Working
        );
        assert_eq!(
            record.materialized.output.descriptor().residency,
            ColorFrameResidency::Gpu
        );
        assert!(runtime.frame_table().get(&record.materialized.input).is_ok());
        assert!(runtime.frame_table().get(&record.materialized.output).is_ok());

        let diagnostics = runtime.diagnostics();
        assert_eq!(diagnostics.next_frame_id, 1_302);
        assert_eq!(diagnostics.frame_table_entries, 2);
        assert_eq!(diagnostics.shader_cache.entries, 1);
        assert_eq!(diagnostics.backend_prep.resources.entries, 1);
        assert_eq!(diagnostics.backend_objects.entries, 1);
    }

    #[tokio::test]
    async fn gpu_input_stage_output_can_feed_gpu_compositor_on_real_wgpu_device() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping real wgpu input-to-composite test: no GPU adapter available");
            return;
        };
        let source = cpu_source_frame();
        let transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::MondrianSmart,
        );
        let compositor = GpuFrameCompositor::new(&context.device);
        let mut effect_graph = mondrian_effects::EffectGraphBuilderState::new();
        effect_graph.append_unary(mondrian_effects::EffectRenderOp::ColorAdjust {
            exposure: 0.25,
            contrast: 1.1,
            saturation: 0.9,
        });
        effect_graph.append_unary(mondrian_effects::EffectRenderOp::Grain { amount: 0.05 });
        let compiled_effect_graph =
            mondrian_effects::get_or_compile_scheduled_render_graph(effect_graph.finish())
                .expect("valid GPU effect graph");
        let effect_plan = mondrian_effects::lower_effect_graph_to_gpu_plan(&compiled_effect_graph)
            .expect("supported GPU effect graph");
        let mut runtime = RenderGpuOutputBoundaryRuntime::with_first_frame_id(1_400);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-test-gpu-input-to-composite"),
        });

        let input_record = runtime
            .record_wgpu_input_stage_owned_backend(
                &transform,
                &source,
                RenderColorTransformGpuOptions::default(),
                RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                    device: &context.device,
                    queue: &context.queue,
                    encoder: &mut encoder,
                    load_op: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                },
            )
            .expect("runtime-owned GPU input stage should record");
        let media_handle = input_record.materialized.output.clone();
        let layers = [crate::GpuCompositeLayer {
            source: crate::GpuCompositeLayerSource::GpuFrame(&media_handle),
            opacity: 1.0,
            blend_mode: mondrian_core::types::BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&effect_plan),
            frame_seed: 17,
        }];

        let composite = runtime
            .record_wgpu_working_composite(
                &compositor,
                &context.device,
                &context.queue,
                &mut encoder,
                GpuCompositeRequest {
                    width: source.descriptor().width,
                    height: source.descriptor().height,
                    working_color_space: WorkingColorSpace::LinearRec709,
                    layers: &layers,
                },
            )
            .expect("GPU input output should feed GPU compositor");
        context.queue.submit(std::iter::once(encoder.finish()));

        assert_eq!(composite.diagnostics.gpu_native_composites, 1);
        assert_eq!(composite.diagnostics.gpu_with_upload_composites, 0);
        assert_eq!(composite.diagnostics.cpu_fallback_composites, 0);
        assert_eq!(
            composite.output.descriptor().domain,
            ColorFrameDomain::Working
        );
        assert_eq!(
            composite.output.descriptor().residency,
            ColorFrameResidency::Gpu
        );
        assert_eq!(runtime.diagnostics().frame_table_entries, 4);
    }

    #[tokio::test]
    async fn gpu_output_boundary_runtime_matches_cpu_display_view_on_real_wgpu_device() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping real wgpu display/view parity test: no GPU adapter available");
            return;
        };
        let (display, view) = ocio_default_display_view().expect("default display/view");
        let frame = cpu_working_frame();
        let boundary = RenderOutputColorBoundary::display_view(
            ColorSpace::Srgb,
            display,
            view,
            false,
            ColorEngine::MondrianSmart,
        );
        let expected = execute_cpu_output_boundary_rgba8(&frame, &boundary)
            .expect("CPU display/view boundary should encode RGBA8");
        let mut runtime = RenderGpuOutputBoundaryRuntime::with_first_frame_id(1_200);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-test-gpu-display-view-boundary-parity"),
        });

        let record = runtime
            .record_wgpu_output_boundary_owned_backend(
                &boundary,
                &frame,
                GpuColorFrameTextureFormat::Rgba8Unorm,
                RenderColorTransformGpuOptions {
                    output_residency: ColorFrameResidency::Cpu,
                    ..RenderColorTransformGpuOptions::default()
                },
                RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                    device: &context.device,
                    queue: &context.queue,
                    encoder: &mut encoder,
                    load_op: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                },
            )
            .expect("runtime-owned GPU display/view boundary should record");
        let readback_buffer = record
            .readback_buffer
            .expect("CPU-resident GPU display/view boundary should record readback");

        context.queue.submit(std::iter::once(encoder.finish()));
        let readback_plan = GpuColorFrameReadbackPlan::encoded_rgba8(record.materialized.output)
            .expect("GPU display/view output should be readable as RGBA8");
        let mapped = map_readback_buffer(&context.device, &readback_buffer);
        let actual = readback_plan
            .unpack_mapped_rgba8(&mapped)
            .expect("display/view readback should unpack into encoded frame");
        readback_buffer.unmap();

        assert_srgb_display_accurate(&expected.rgba, actual.rgba());
        assert_eq!(record.stage_diagnostics.gpu_color_stages, 1);
        assert_eq!(record.stage_diagnostics.readback_stages, 1);
    }

    #[tokio::test]
    async fn gpu_pq_display_view_meets_delta_e_itp_budget_on_real_wgpu_device() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping real wgpu PQ display/view accuracy test: no GPU adapter available");
            return;
        };
        let frame = cpu_working_frame();
        let boundary = RenderOutputColorBoundary::display_view(
            ColorSpace::Rec2100Pq,
            "Rec.2100-PQ - Display",
            "ACES 2.0 - HDR 1000 nits (Rec.2020)",
            false,
            ColorEngine::MondrianSmart,
        );
        let expected = execute_cpu_output_boundary_float(&frame, &boundary)
            .expect("CPU PQ display/view boundary");
        let mut runtime = RenderGpuOutputBoundaryRuntime::with_first_frame_id(1_250);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-test-gpu-pq-display-view-accuracy"),
        });

        let record = runtime
            .record_wgpu_output_boundary_owned_backend(
                &boundary,
                &frame,
                GpuColorFrameTextureFormat::Rgba16Float,
                RenderColorTransformGpuOptions {
                    output_residency: ColorFrameResidency::Cpu,
                    ..RenderColorTransformGpuOptions::default()
                },
                RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                    device: &context.device,
                    queue: &context.queue,
                    encoder: &mut encoder,
                    load_op: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                },
            )
            .expect("runtime-owned GPU PQ display/view boundary should record");
        let readback_buffer = record.readback_buffer.expect("PQ float readback buffer");
        assert_eq!(
            record.materialized.output.descriptor().encoding,
            ColorFrameEncoding::EncodedFloat
        );

        context.queue.submit(std::iter::once(encoder.finish()));
        let readback_plan =
            GpuColorFrameReadbackPlan::encoded_rgba16float(record.materialized.output)
                .expect("PQ output should be readable as RGBA16F");
        let mapped = map_readback_buffer(&context.device, &readback_buffer);
        let actual_flat = readback_plan
            .unpack_mapped_rgba16float(&mapped)
            .expect("PQ readback should unpack");
        readback_buffer.unmap();
        let actual: Vec<[f32; 4]> = actual_flat
            .chunks_exact(4)
            .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
            .collect();
        let report = crate::compare_pq_hdr_display_rgba(
            &expected.frame.rgba_f32().data,
            &actual,
            crate::PqHdrDisplayAccuracyBudget::new(0.5, 0.2, 0.5, 0.001),
        )
        .expect("valid PQ display accuracy report");

        assert!(
            report.within_budget,
            "{report:#?}\nexpected={:?}\nactual={actual:?}",
            expected.frame.rgba_f32().data
        );
        assert_eq!(record.stage_diagnostics.gpu_color_stages, 1);
        assert_eq!(record.stage_diagnostics.readback_stages, 1);
    }

    #[tokio::test]
    #[ignore = "manual renderer GPU output boundary smoke report; requires a real wgpu adapter"]
    async fn gpu_output_boundary_runtime_smoke_report_on_real_wgpu_device() -> anyhow::Result<()> {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let tolerance = 3;
        let context = match GpuContext::new().await {
            Ok(context) => context,
            Err(err) => {
                let skipped_runtime = RenderGpuOutputBoundaryRuntime::with_first_frame_id(1_000);
                let frame_report = RenderGpuOutputFrameReport {
                    width: 0,
                    height: 0,
                    pixel_count: 0,
                    input_color_space: ColorSpace::Rec709,
                    output_color_space: ColorSpace::Srgb,
                };
                let stage_report: RenderGpuOutputStageDiagnosticsReport =
                    RenderColorStageDiagnostics::default().into();
                let runtime_report: RenderGpuOutputRuntimeDiagnosticsReport =
                    skipped_runtime.diagnostics().into();
                let skipped_reason = format!("no GPU adapter available: {err}");
                let report = GpuOutputBoundarySmokeReport {
                    scenario: "renderer_gpu_output_boundary",
                    skipped: Some(skipped_reason.clone()),
                    adapter: None,
                    frame: frame_report,
                    output_texture_format: "Rgba8Unorm",
                    health_report: RenderGpuOutputHealthReport::from_sample(
                        "renderer_gpu_output_boundary",
                        Some(skipped_reason),
                        &frame_report,
                        &stage_report,
                        &runtime_report,
                        0,
                        0,
                        tolerance,
                    ),
                    stage: stage_report,
                    runtime: runtime_report,
                    readback_bytes: 0,
                    max_rgba_delta: 0,
                    tolerance,
                };
                emit_gpu_output_smoke_report(&report)?;
                return Ok(());
            }
        };
        let adapter_info = context.adapter.get_info();
        let frame = cpu_working_frame();
        let boundary =
            RenderOutputColorBoundary::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);
        let expected = execute_cpu_output_boundary_rgba8(&frame, &boundary)
            .expect("CPU display boundary should encode RGBA8");
        let mut runtime = RenderGpuOutputBoundaryRuntime::with_first_frame_id(1_000);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-smoke-gpu-output-boundary"),
        });

        let record = runtime
            .record_wgpu_output_boundary_owned_backend(
                &boundary,
                &frame,
                GpuColorFrameTextureFormat::Rgba8Unorm,
                RenderColorTransformGpuOptions {
                    output_residency: ColorFrameResidency::Cpu,
                    ..RenderColorTransformGpuOptions::default()
                },
                RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                    device: &context.device,
                    queue: &context.queue,
                    encoder: &mut encoder,
                    load_op: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                },
            )
            .map_err(|err| anyhow::anyhow!("{err:?}"))?;
        let readback_buffer = record
            .readback_buffer
            .as_ref()
            .expect("CPU-resident GPU output boundary should record readback");
        context.queue.submit(std::iter::once(encoder.finish()));
        let readback_plan =
            GpuColorFrameReadbackPlan::encoded_rgba8(record.materialized.output.clone())
                .expect("GPU output should be readable as RGBA8");
        let mapped = map_readback_buffer(&context.device, readback_buffer);
        let actual = readback_plan
            .unpack_mapped_rgba8(&mapped)
            .expect("readback should unpack into encoded frame");
        readback_buffer.unmap();

        let max_rgba_delta = max_rgba_delta(&expected.rgba, actual.rgba());
        let stage_diagnostics = record.stage_diagnostics;
        let runtime_diagnostics = runtime.diagnostics();
        let frame_report = RenderGpuOutputFrameReport {
            width: frame.descriptor().width as usize,
            height: frame.descriptor().height as usize,
            pixel_count: frame.descriptor().pixel_count(),
            input_color_space: frame
                .descriptor()
                .color_space
                .encoded()
                .expect("GPU output boundary input must be encoded"),
            output_color_space: boundary.output_color_space,
        };
        let stage_report = RenderGpuOutputStageDiagnosticsReport::from(stage_diagnostics);
        let runtime_report = RenderGpuOutputRuntimeDiagnosticsReport::from(runtime_diagnostics);
        let health_report = RenderGpuOutputHealthReport::from_sample(
            "renderer_gpu_output_boundary",
            None,
            &frame_report,
            &stage_report,
            &runtime_report,
            actual.rgba().len(),
            max_rgba_delta,
            tolerance,
        );
        let passed = health_report.verdict == RenderGpuOutputHealthVerdict::Pass;
        let report = GpuOutputBoundarySmokeReport {
            scenario: "renderer_gpu_output_boundary",
            skipped: None,
            adapter: Some(GpuOutputAdapterReport {
                name: adapter_info.name,
                backend: format!("{:?}", adapter_info.backend),
                device_type: format!("{:?}", adapter_info.device_type),
                driver: adapter_info.driver,
                driver_info: adapter_info.driver_info,
            }),
            frame: frame_report,
            output_texture_format: "Rgba8Unorm",
            health_report,
            stage: stage_report,
            runtime: runtime_report,
            readback_bytes: actual.rgba().len(),
            max_rgba_delta,
            tolerance,
        };
        emit_gpu_output_smoke_report(&report)?;

        assert!(passed, "GPU output boundary smoke failed: {report:?}");
        Ok(())
    }

    #[test]
    fn output_boundary_cpu_planner_produces_cpu_stage_plan() {
        let frame = cpu_working_frame();
        let boundary =
            RenderOutputColorBoundary::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);
        let mut planner = RenderOutputColorBoundaryPlanner::cpu_only();

        let plan = planner.plan(&frame, &boundary).expect("CPU output boundary plan");

        assert_eq!(plan.boundary, boundary);
        assert_eq!(plan.stage_plan.stages.len(), 1);
        assert!(matches!(
            plan.stage_plan.stages[0],
            RenderColorStage::CpuOutputTransform { .. }
        ));
        assert_eq!(plan.diagnostics().cpu_output_stages, 1);
        assert_eq!(plan.diagnostics().gpu_color_stages, 0);
    }

    #[test]
    fn output_boundary_prefer_gpu_planner_builds_backend_ready_stage_plan() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let frame = cpu_working_frame();
        let boundary =
            RenderOutputColorBoundary::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderOutputColorBoundaryPlanner::prefer_gpu(
            &mut cache,
            RenderColorTransformGpuOptions {
                output_residency: ColorFrameResidency::Cpu,
                ..RenderColorTransformGpuOptions::default()
            },
        );

        let plan = planner.plan(&frame, &boundary).expect("GPU output boundary plan");
        let diagnostics = plan.diagnostics();

        assert_eq!(plan.boundary, boundary);
        assert!(plan.stage_plan.contains_gpu_transform());
        assert!(plan.stage_plan.contains_transfer());
        assert_eq!(diagnostics.cpu_output_stages, 0);
        assert_eq!(diagnostics.upload_stages, 1);
        assert_eq!(diagnostics.gpu_color_stages, 1);
        assert_eq!(diagnostics.readback_stages, 1);
        assert_eq!(diagnostics.gpu_blockers, 0);
    }

    #[test]
    fn color_stage_diagnostics_break_down_gpu_blocker_reasons() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let frame = cpu_working_frame();
        let mut plan = gpu_output_stage_plan_for_frame(&frame);
        add_gpu_stage_blocker_reason(
            &mut plan,
            OcioGpuWgpuBlocker::ShaderModuleNotPrepared { language: GpuLanguage::Glsl4_0 },
        );
        add_gpu_stage_blocker_reason(
            &mut plan,
            OcioGpuWgpuBlocker::OcioResourceBindGroupNotPrepared {
                texture_2d_count: 2,
                texture_3d_count: 1,
                uniform_buffers: 1,
            },
        );
        add_gpu_stage_blocker_reason(&mut plan, OcioGpuWgpuBlocker::FullscreenWrapperNotPrepared);
        add_gpu_stage_blocker_reason(&mut plan, OcioGpuWgpuBlocker::RenderPipelineNotPrepared);

        let diagnostics = plan.diagnostics();

        assert_eq!(diagnostics.gpu_blockers, 4);
        assert_eq!(diagnostics.gpu_blocker_breakdown.total(), 4);
        assert_eq!(
            diagnostics.gpu_blocker_breakdown,
            RenderColorStageGpuBlockerBreakdown {
                shader_module_not_prepared: 1,
                ocio_resource_bind_group_not_prepared: 1,
                fullscreen_wrapper_not_prepared: 1,
                render_pipeline_not_prepared: 1,
                ..RenderColorStageGpuBlockerBreakdown::default()
            }
        );
    }

    #[test]
    fn output_boundary_cpu_stage_plan_rejects_gpu_resource_bridge() {
        let frame = cpu_working_frame();
        let boundary =
            RenderOutputColorBoundary::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);
        let mut planner = RenderOutputColorBoundaryPlanner::cpu_only();
        let plan = planner.plan(&frame, &boundary).expect("CPU output boundary plan");
        let mut ids = GpuColorFrameIdAllocator::new(700);

        let err = plan
            .gpu_resource_plan(&mut ids, &frame, GpuColorFrameTextureFormat::Rgba16Float)
            .expect_err("CPU output boundary cannot build GPU resources");

        assert!(matches!(
            err,
            RenderGpuOutputStageResourcePlanError::UnsupportedStagePlan { .. }
        ));
    }

    #[test]
    fn output_boundary_gpu_stage_plan_reports_blockers_before_resource_bridge() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let frame = cpu_working_frame();
        let boundary =
            RenderOutputColorBoundary::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderOutputColorBoundaryPlanner::prefer_gpu(
            &mut cache,
            RenderColorTransformGpuOptions {
                output_residency: ColorFrameResidency::Cpu,
                ..RenderColorTransformGpuOptions::default()
            },
        );
        let mut plan = planner.plan(&frame, &boundary).expect("GPU output boundary plan");
        add_gpu_stage_blocker(&mut plan.stage_plan);
        let mut ids = GpuColorFrameIdAllocator::new(710);

        let err = plan
            .gpu_resource_plan(&mut ids, &frame, GpuColorFrameTextureFormat::Rgba8Unorm)
            .expect_err("blocked GPU output boundary cannot build resources");

        match err {
            RenderGpuOutputStageResourcePlanError::NativeBlockersRemaining {
                blockers,
                breakdown,
            } => {
                assert!(blockers > 0);
                assert_eq!(breakdown.total(), blockers as u64);
                assert!(breakdown.render_pipeline_not_prepared > 0);
            }
            other => panic!("expected native blockers, got {other:?}"),
        }
    }

    #[test]
    fn output_boundary_gpu_stage_plan_builds_resource_plan_when_blockers_clear() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let frame = cpu_working_frame();
        let boundary =
            RenderOutputColorBoundary::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderOutputColorBoundaryPlanner::prefer_gpu(
            &mut cache,
            RenderColorTransformGpuOptions {
                output_residency: ColorFrameResidency::Cpu,
                ..RenderColorTransformGpuOptions::default()
            },
        );
        let plan = planner.plan(&frame, &boundary).expect("GPU output boundary plan");
        let mut ids = GpuColorFrameIdAllocator::new(720);

        let resources = plan
            .gpu_resource_plan(&mut ids, &frame, GpuColorFrameTextureFormat::Rgba8Unorm)
            .expect("blocker-free GPU output boundary resources");

        assert_eq!(
            resources.transform.diagnostics.input,
            resources.input.descriptor()
        );
        assert_eq!(
            resources.transform.diagnostics.output,
            resources.output.descriptor()
        );
        assert_eq!(
            resources.output.texture_format(),
            GpuColorFrameTextureFormat::Rgba8Unorm
        );
        assert!(resources.readback.is_some());
    }

    #[test]
    fn cpu_output_boundary_executes_export_target() {
        let frame = cpu_working_frame();
        let boundary = RenderOutputColorBoundary::export(
            ColorSpace::Rec709,
            false,
            ColorEngine::MondrianSmart,
        );

        let output =
            execute_cpu_output_boundary(&frame, &boundary).expect("export boundary should execute");

        assert_eq!(
            output.result.frame.descriptor().domain,
            ColorFrameDomain::Export
        );
        assert_eq!(
            output.result.frame.descriptor().color_space,
            ColorSpace::Rec709.into()
        );
        assert_eq!(output.stage_diagnostics.cpu_output_stages, 1);
    }

    #[test]
    fn cpu_output_boundary_display_and_export_targets_match_pixels_for_same_transform() {
        let frame = cpu_working_frame();
        let display = execute_cpu_output_boundary(
            &frame,
            &RenderOutputColorBoundary::display(
                ColorSpace::Srgb,
                false,
                ColorEngine::MondrianSmart,
            ),
        )
        .expect("display output boundary");
        let export = execute_cpu_output_boundary(
            &frame,
            &RenderOutputColorBoundary::export(ColorSpace::Srgb, false, ColorEngine::MondrianSmart),
        )
        .expect("export output boundary");

        assert_eq!(display.result.frame.rgba(), export.result.frame.rgba());
        assert_eq!(
            display.result.frame.descriptor().domain,
            ColorFrameDomain::Display
        );
        assert_eq!(
            export.result.frame.descriptor().domain,
            ColorFrameDomain::Export
        );
        assert_eq!(
            display.result.diagnostics.output.color_space,
            export.result.diagnostics.output.color_space
        );
        assert_eq!(
            display.stage_diagnostics.cpu_output_stages,
            export.stage_diagnostics.cpu_output_stages
        );
        assert_eq!(
            display.stage_diagnostics.stage_pixels,
            export.stage_diagnostics.stage_pixels
        );
    }

    #[test]
    fn gpu_input_plan_adds_upload_for_cpu_source() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::MondrianSmart,
        );
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderColorStagePlanner::prefer_gpu(
            &mut cache,
            RenderColorTransformGpuOptions::default(),
        );

        let plan = planner
            .plan_input_to_working(source_descriptor(ColorFrameResidency::Cpu), &transform)
            .expect("GPU input plan");

        assert_eq!(plan.stages.len(), 2);
        assert!(matches!(
            plan.stages[0],
            RenderColorStage::UploadToGpu { .. }
        ));
        assert!(matches!(
            plan.stages[1],
            RenderColorStage::GpuColorTransform { .. }
        ));
        assert_stage_chain_is_contiguous(&plan);
        assert!(plan.contains_gpu_transform());
        assert!(plan.contains_transfer());
        assert_eq!(plan.final_descriptor.residency, ColorFrameResidency::Gpu);
    }

    #[test]
    fn gpu_output_plan_adds_readback_when_cpu_output_is_requested() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let transform =
            RenderColorTransform::export(ColorSpace::Rec709, false, ColorEngine::MondrianSmart);
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderColorStagePlanner::prefer_gpu(
            &mut cache,
            RenderColorTransformGpuOptions {
                output_residency: ColorFrameResidency::Cpu,
                ..RenderColorTransformGpuOptions::default()
            },
        );

        let plan = planner
            .plan_output_transform(working_descriptor(ColorFrameResidency::Gpu), &transform)
            .expect("GPU output plan");

        assert_eq!(plan.stages.len(), 2);
        assert!(matches!(
            plan.stages[0],
            RenderColorStage::GpuColorTransform { .. }
        ));
        assert!(matches!(
            plan.stages[1],
            RenderColorStage::ReadbackToCpu { .. }
        ));
        assert_stage_chain_is_contiguous(&plan);
        assert!(plan.contains_gpu_transform());
        assert!(plan.contains_transfer());
        assert_eq!(plan.final_descriptor.residency, ColorFrameResidency::Cpu);
        assert_eq!(plan.final_descriptor.domain, ColorFrameDomain::Export);
    }

    #[test]
    fn gpu_color_pass_schedule_binds_handles_transform_and_pass_node() {
        let transform = executable_gpu_output_plan();
        let input = gpu_handle(1, transform.diagnostics.input, "working-input");
        let output = gpu_handle(2, transform.diagnostics.output, "display-output");
        let pass_node = pass_node_for_transform(&transform);

        let schedule = RenderGpuColorPassSchedule::new(
            input.clone(),
            output.clone(),
            transform.clone(),
            pass_node,
        )
        .expect("schedulable GPU color pass");

        assert_eq!(schedule.input, input);
        assert_eq!(schedule.output, output);
        assert_eq!(schedule.transform.diagnostics, transform.diagnostics);
        assert_eq!(
            schedule.pass_node.resource_key,
            transform.wgpu.resources.resource_key
        );
    }

    #[test]
    fn gpu_color_pass_schedule_rejects_remaining_native_blockers() {
        let transform = blocked_gpu_output_plan();
        let input = gpu_handle(3, transform.diagnostics.input, "working-input");
        let output = gpu_handle(4, transform.diagnostics.output, "display-output");
        let pass_node = pass_node_for_transform(&transform);

        let err = RenderGpuColorPassSchedule::new(input, output, transform, pass_node)
            .expect_err("blocked GPU plan must not schedule");

        assert!(matches!(
            err,
            RenderGpuColorPassScheduleError::NativeBlockersRemaining { blockers } if blockers > 0
        ));
    }

    #[test]
    fn gpu_color_pass_schedule_rejects_mismatched_target_descriptor() {
        let transform = executable_gpu_output_plan();
        let input = gpu_handle(5, transform.diagnostics.input, "working-input");
        let mut wrong_output_descriptor = transform.diagnostics.output;
        wrong_output_descriptor.color_space = ColorSpace::Rec2020.into();
        let output = gpu_handle(6, wrong_output_descriptor, "wrong-display-output");
        let pass_node = pass_node_for_transform(&transform);

        let err = RenderGpuColorPassSchedule::new(input, output, transform, pass_node)
            .expect_err("output descriptor mismatch must fail");

        assert!(matches!(
            err,
            RenderGpuColorPassScheduleError::OutputDescriptorMismatch { .. }
        ));
    }

    #[test]
    fn gpu_color_pass_schedule_rejects_mismatched_pass_resource_key() {
        let transform = executable_gpu_output_plan();
        let input = gpu_handle(7, transform.diagnostics.input, "working-input");
        let output = gpu_handle(8, transform.diagnostics.output, "display-output");
        let mut pass_node = pass_node_for_transform(&transform);
        pass_node.resource_key = transform.wgpu.resources.resource_key.wrapping_add(1);

        let err = RenderGpuColorPassSchedule::new(input, output, transform, pass_node)
            .expect_err("pass resource mismatch must fail");

        assert!(matches!(
            err,
            RenderGpuColorPassScheduleError::PassResourceKeyMismatch { .. }
        ));
    }

    #[test]
    fn gpu_color_pass_schedule_rejects_mismatched_target_texture_format() {
        let transform = executable_gpu_output_plan();
        let input = gpu_handle(9, transform.diagnostics.input, "working-input");
        let output = gpu_handle_with_format(
            10,
            transform.diagnostics.output,
            GpuColorFrameTextureFormat::Rgba8Unorm,
            "display-output-rgba8",
        );
        let pass_node = pass_node_for_transform(&transform);

        let err = RenderGpuColorPassSchedule::new(input, output, transform, pass_node)
            .expect_err("target texture format mismatch must fail");

        assert!(matches!(
            err,
            RenderGpuColorPassScheduleError::TargetTextureFormatMismatch {
                expected: crate::OcioGpuWgpuColorTargetFormat::Rgba16Float,
                actual: GpuColorFrameTextureFormat::Rgba8Unorm
            }
        ));
    }

    #[test]
    fn gpu_color_pass_schedule_rejects_mismatched_wrapper_layout_hash() {
        let transform = executable_gpu_output_plan();
        let input = gpu_handle(11, transform.diagnostics.input, "working-input");
        let output = gpu_handle(12, transform.diagnostics.output, "display-output");
        let mut pass_node = pass_node_for_transform(&transform);
        pass_node.wrapper_layout_hash = pass_node.wrapper_layout_hash.wrapping_add(1);

        let err = RenderGpuColorPassSchedule::new(input, output, transform, pass_node)
            .expect_err("wrapper layout mismatch must fail");

        assert!(matches!(
            err,
            RenderGpuColorPassScheduleError::PassWrapperLayoutHashMismatch { .. }
        ));
    }

    #[test]
    fn gpu_color_pass_schedule_validates_resolved_input_frame() {
        let schedule = executable_gpu_output_schedule(13, 14);
        let wrong_input = gpu_handle(15, schedule.input.descriptor(), "wrong-input");

        let err = schedule
            .validate_input_frame(&wrong_input)
            .expect_err("wrong input frame id must fail");

        assert!(matches!(
            err,
            RenderGpuColorPassExecutionError::InputFrameIdMismatch { .. }
        ));
    }

    #[test]
    fn gpu_color_pass_schedule_validates_resolved_output_frame() {
        let schedule = executable_gpu_output_schedule(16, 17);
        let wrong_output = gpu_handle_with_format(
            17,
            schedule.output.descriptor(),
            GpuColorFrameTextureFormat::Rgba32Float,
            "wrong-output-format",
        );

        let err = schedule
            .validate_output_frame(&wrong_output)
            .expect_err("wrong output texture format must fail");

        assert!(matches!(
            err,
            RenderGpuColorPassExecutionError::OutputTextureFormatMismatch { .. }
        ));
    }

    #[test]
    fn gpu_color_pass_schedule_resolves_matching_frame_resources() {
        let schedule = executable_gpu_output_schedule(18, 19);
        let mut table = GpuColorFrameResourceTable::new();
        table
            .insert(GpuColorFrameResource::new(
                schedule.input.clone(),
                "input-texture",
            ))
            .expect("insert input");
        table
            .insert(GpuColorFrameResource::new(
                schedule.output.clone(),
                "output-texture",
            ))
            .expect("insert output");

        let resolved = schedule.resolve_resources(&table).expect("resolve scheduled resources");

        assert_eq!(resolved.input.resource(), &"input-texture");
        assert_eq!(resolved.output.resource(), &"output-texture");
        assert_eq!(resolved.input.handle(), &schedule.input);
        assert_eq!(resolved.output.handle(), &schedule.output);
    }

    #[test]
    fn gpu_color_pass_schedule_reports_missing_resource_table_frame() {
        let schedule = executable_gpu_output_schedule(20, 21);
        let mut table = GpuColorFrameResourceTable::new();
        table
            .insert(GpuColorFrameResource::new(schedule.input.clone(), "input"))
            .expect("insert input");

        let err = schedule
            .resolve_resources(&table)
            .expect_err("missing output must fail before recording");

        assert!(matches!(
            err,
            RenderGpuColorPassExecutionError::ResourceTable(
                GpuColorFrameResourceTableError::MissingFrame { id }
            ) if id == schedule.output.id()
        ));
    }

    #[test]
    fn gpu_color_pass_schedule_reports_stale_resource_table_contract() {
        let schedule = executable_gpu_output_schedule(22, 23);
        let stale_output = gpu_handle_with_format(
            23,
            schedule.output.descriptor(),
            GpuColorFrameTextureFormat::Rgba32Float,
            "stale-output",
        );
        let mut table = GpuColorFrameResourceTable::new();
        table
            .insert(GpuColorFrameResource::new(schedule.input.clone(), "input"))
            .expect("insert input");
        table
            .insert(GpuColorFrameResource::new(
                stale_output.clone(),
                "stale-output",
            ))
            .expect("insert stale output");

        let err = schedule
            .resolve_resources(&table)
            .expect_err("stale output contract must fail before recording");

        assert_eq!(
            err,
            RenderGpuColorPassExecutionError::ResourceTable(
                GpuColorFrameResourceTableError::ContractMismatch {
                    id: schedule.output.id(),
                    expected: schedule.output.contract(),
                    actual: stale_output.contract()
                }
            )
        );
    }

    #[test]
    fn gpu_output_stage_resource_plan_materializes_upload_allocation_and_schedulable_transform() {
        let frame = cpu_working_frame();
        let stage_plan = gpu_output_stage_plan_for_frame(&frame);
        let mut ids = GpuColorFrameIdAllocator::new(500);

        let resources = RenderGpuOutputStageResourcePlan::from_cpu_working_frame(
            &mut ids,
            &frame,
            &stage_plan,
            GpuColorFrameTextureFormat::Rgba16Float,
        )
        .expect("GPU output stage resources");

        assert_eq!(resources.input.id().raw(), 500);
        assert_eq!(resources.output.id().raw(), 501);
        assert_eq!(ids.next_raw(), 502);
        let input_upload = resources
            .input_upload
            .as_ref()
            .expect("CPU working frame path must include upload");
        assert_eq!(input_upload.handle, resources.input);
        assert_eq!(
            input_upload.texture_format,
            GpuColorFrameTextureFormat::Rgba32Float
        );
        assert_eq!(resources.output_allocation.handle, resources.output);
        assert_eq!(
            resources.output_allocation.texture_format,
            GpuColorFrameTextureFormat::Rgba16Float
        );
        assert!(!resources.transform.requires_source_upload);
        assert!(!resources.transform.requires_output_readback);
        assert_eq!(resources.stage_diagnostics(), stage_plan.diagnostics());
        assert_eq!(
            resources.transform.diagnostics.input,
            resources.input.descriptor()
        );
        assert_eq!(
            resources.transform.diagnostics.output,
            resources.output.descriptor()
        );

        let pass_node = pass_node_for_transform(&resources.transform);
        RenderGpuColorPassSchedule::new(
            resources.input,
            resources.output,
            resources.transform,
            pass_node,
        )
        .expect("resource-planned transform should schedule");
    }

    #[test]
    fn gpu_output_stage_resource_plan_accepts_gpu_resident_working_input_without_upload() {
        let frame = cpu_working_frame();
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let input_descriptor = frame.descriptor().with_residency(ColorFrameResidency::Gpu);
        let input = gpu_handle_with_format(
            600,
            input_descriptor,
            GpuColorFrameTextureFormat::Rgba32Float,
            "gpu-composited-working",
        );
        let transform =
            RenderColorTransform::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderColorStagePlanner::prefer_gpu(
            &mut cache,
            RenderColorTransformGpuOptions::default(),
        );
        let stage_plan = planner
            .plan_output_transform(input_descriptor, &transform)
            .expect("GPU-resident output stage plan");
        let mut ids = GpuColorFrameIdAllocator::new(601);

        let resources = RenderGpuOutputStageResourcePlan::from_gpu_working_frame(
            &mut ids,
            &input,
            &stage_plan,
            GpuColorFrameTextureFormat::Rgba8Unorm,
        )
        .expect("GPU-resident working frame output resources");

        assert_eq!(resources.input, input);
        assert!(resources.input_upload.is_none());
        assert_eq!(resources.output.id().raw(), 601);
        assert_eq!(ids.next_raw(), 602);
        let diagnostics = resources.stage_diagnostics();
        assert_eq!(diagnostics.upload_stages, 0);
        assert_eq!(diagnostics.gpu_color_stages, 1);
        assert_eq!(diagnostics.readback_stages, 0);
    }

    #[test]
    fn gpu_input_stage_resource_plan_uploads_cpu_source_and_outputs_gpu_working_frame() {
        let source = cpu_source_frame();
        let stage_plan = gpu_input_stage_plan_for_source(&source);
        let mut ids = GpuColorFrameIdAllocator::new(700);

        let resources = RenderGpuInputStageResourcePlan::from_cpu_encoded_source_frame(
            &mut ids,
            &source,
            &stage_plan,
        )
        .expect("GPU input stage resources");

        assert_eq!(resources.input.id().raw(), 700);
        assert_eq!(resources.output.id().raw(), 701);
        assert_eq!(ids.next_raw(), 702);
        let input_upload = resources.input_upload.as_ref().expect("CPU source upload");
        assert_eq!(input_upload.handle, resources.input);
        assert_eq!(
            input_upload.texture_format,
            GpuColorFrameTextureFormat::Rgba8Unorm
        );
        assert_eq!(resources.output_allocation.handle, resources.output);
        assert_eq!(
            resources.output_allocation.texture_format,
            GpuColorFrameTextureFormat::Rgba32Float
        );
        assert_eq!(
            resources.output.descriptor().domain,
            ColorFrameDomain::Working
        );
        assert_eq!(
            resources.output.descriptor().encoding,
            ColorFrameEncoding::LinearFloat
        );
        assert_eq!(
            resources.output.descriptor().residency,
            ColorFrameResidency::Gpu
        );
        assert!(!resources.transform.requires_source_upload);
        assert!(!resources.transform.requires_output_readback);
        assert_eq!(resources.stage_diagnostics(), stage_plan.diagnostics());

        let mut pass_node = pass_node_for_transform(&resources.transform);
        pass_node.output_format = OcioGpuWgpuColorTargetFormat::Rgba32Float;
        RenderGpuColorPassSchedule::new(
            resources.input,
            resources.output,
            resources.transform,
            pass_node,
        )
        .expect("resource-planned GPU input transform should schedule");
    }

    #[test]
    fn gpu_input_stage_resource_plan_accepts_encoded_float_gpu_source_without_upload() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let input_descriptor = ColorFrameDescriptor {
            width: 1920,
            height: 1080,
            color_space: ColorSpace::Rec2100Pq.into(),
            domain: ColorFrameDomain::Source,
            encoding: ColorFrameEncoding::EncodedFloat,
            residency: ColorFrameResidency::Gpu,
        };
        let input = gpu_handle_with_format(
            710,
            input_descriptor,
            GpuColorFrameTextureFormat::Rgba16Float,
            "native-yuv-encoded-source",
        );
        let transform = RenderInputTransform::to_working_gpu(
            WorkingColorSpace::LinearRec2020,
            true,
            ColorEngine::MondrianSmart,
        );
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderColorStagePlanner::prefer_gpu(
            &mut cache,
            RenderColorTransformGpuOptions::default(),
        );
        let stage_plan = planner
            .plan_input_to_working(input_descriptor, &transform)
            .expect("GPU-resident encoded source input plan");
        assert!(!stage_plan.contains_transfer());
        let output = gpu_handle_with_format(
            711,
            stage_plan.final_descriptor,
            GpuColorFrameTextureFormat::Rgba32Float,
            "native-yuv-working-output",
        );

        let resources = RenderGpuInputStageResourcePlan::from_gpu_encoded_source_frame(
            &input,
            &output,
            &stage_plan,
        )
        .expect("GPU-resident encoded source resources");

        assert_eq!(resources.input, input);
        assert!(resources.input_upload.is_none());
        assert_eq!(resources.output, output);
        assert_eq!(resources.stage_diagnostics(), stage_plan.diagnostics());
        assert_eq!(resources.stage_diagnostics().upload_stages, 0);
        assert_eq!(
            resources.output.descriptor().domain,
            ColorFrameDomain::Working
        );
        assert_eq!(
            resources.output.descriptor().encoding,
            ColorFrameEncoding::LinearFloat
        );
    }

    #[test]
    fn gpu_input_stage_resource_plan_rejects_cpu_only_plan() {
        let source = cpu_source_frame();
        let transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::MondrianSmart,
        );
        let mut planner = RenderColorStagePlanner::cpu_only();
        let stage_plan = planner
            .plan_input_to_working(source.descriptor(), &transform)
            .expect("CPU input stage plan");
        let mut ids = GpuColorFrameIdAllocator::new(710);

        let err = RenderGpuInputStageResourcePlan::from_cpu_encoded_source_frame(
            &mut ids,
            &source,
            &stage_plan,
        )
        .expect_err("CPU-only input plan cannot materialize GPU resources");

        assert!(matches!(
            err,
            RenderGpuInputStageResourcePlanError::UnsupportedStagePlan { .. }
        ));
    }

    #[test]
    fn gpu_input_stage_resource_plan_rejects_native_blockers() {
        let source = cpu_source_frame();
        let mut stage_plan = gpu_input_stage_plan_for_source(&source);
        add_gpu_stage_blocker(&mut stage_plan);
        let mut ids = GpuColorFrameIdAllocator::new(720);

        let err = RenderGpuInputStageResourcePlan::from_cpu_encoded_source_frame(
            &mut ids,
            &source,
            &stage_plan,
        )
        .expect_err("blocked GPU input stage cannot materialize executable resources");

        assert!(matches!(
            err,
            RenderGpuInputStageResourcePlanError::NativeBlockersRemaining { blockers, .. } if blockers > 0
        ));
    }

    #[test]
    fn gpu_input_stage_resource_plan_inserts_materialized_resources() {
        let resources = executable_gpu_input_stage_resources(730);
        let mut table = GpuColorFrameResourceTable::new();

        let materialized = resources
            .insert_resources(
                &mut table,
                GpuColorFrameResource::new(resources.input.clone(), "source-input"),
                GpuColorFrameResource::new(resources.output.clone(), "working-output"),
            )
            .expect("insert materialized input resources");

        assert_eq!(materialized.input, resources.input);
        assert_eq!(materialized.output, resources.output);
        assert_eq!(
            table.get(&resources.input).expect("input").resource(),
            &"source-input"
        );
        assert_eq!(
            table.get(&resources.output).expect("output").resource(),
            &"working-output"
        );
    }

    #[test]
    fn gpu_output_stage_resource_plan_rejects_cpu_only_stage_plan() {
        let frame = cpu_working_frame();
        let transform =
            RenderColorTransform::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);
        let mut planner = RenderColorStagePlanner::cpu_only();
        let stage_plan = planner
            .plan_output_transform(frame.descriptor(), &transform)
            .expect("CPU stage plan");
        let mut ids = GpuColorFrameIdAllocator::default();

        let err = RenderGpuOutputStageResourcePlan::from_cpu_working_frame(
            &mut ids,
            &frame,
            &stage_plan,
            GpuColorFrameTextureFormat::Rgba16Float,
        )
        .expect_err("CPU-only plan cannot materialize GPU resources");

        assert!(matches!(
            err,
            RenderGpuOutputStageResourcePlanError::UnsupportedStagePlan { .. }
        ));
    }

    #[test]
    fn gpu_output_stage_resource_plan_rejects_native_blockers() {
        let frame = cpu_working_frame();
        let mut stage_plan = gpu_output_stage_plan_for_frame(&frame);
        add_gpu_stage_blocker(&mut stage_plan);
        let mut ids = GpuColorFrameIdAllocator::default();

        let err = RenderGpuOutputStageResourcePlan::from_cpu_working_frame(
            &mut ids,
            &frame,
            &stage_plan,
            GpuColorFrameTextureFormat::Rgba16Float,
        )
        .expect_err("blocked GPU stage cannot materialize executable resources");

        assert!(matches!(
            err,
            RenderGpuOutputStageResourcePlanError::NativeBlockersRemaining { blockers, .. } if blockers > 0
        ));
    }

    #[test]
    fn gpu_output_stage_resource_plan_inserts_materialized_resources() {
        let resources = executable_gpu_output_stage_resources(600);
        let mut table = GpuColorFrameResourceTable::new();

        let materialized = resources
            .insert_resources(
                &mut table,
                GpuColorFrameResource::new(resources.input.clone(), "input-resource"),
                GpuColorFrameResource::new(resources.output.clone(), "output-resource"),
            )
            .expect("insert materialized resources");

        assert_eq!(materialized.input, resources.input);
        assert_eq!(materialized.output, resources.output);
        assert_eq!(
            table.get(&resources.input).expect("input").resource(),
            &"input-resource"
        );
        assert_eq!(
            table.get(&resources.output).expect("output").resource(),
            &"output-resource"
        );
    }

    #[test]
    fn gpu_output_stage_resource_plan_builds_pass_schedule() {
        let resources = executable_gpu_output_stage_resources(605);
        let pass_node = pass_node_for_transform(&resources.transform);

        let schedule = resources
            .schedule_pass(pass_node)
            .expect("stage resources should schedule pass");

        assert_eq!(schedule.input, resources.input);
        assert_eq!(schedule.output, resources.output);
        assert_eq!(
            schedule.transform.wgpu.resources.resource_key,
            resources.transform.wgpu.resources.resource_key
        );
    }

    #[test]
    fn gpu_output_stage_resource_plan_rejects_mismatched_pass_schedule() {
        let resources = executable_gpu_output_stage_resources(606);
        let mut pass_node = pass_node_for_transform(&resources.transform);
        pass_node.resource_key = pass_node.resource_key.wrapping_add(1);

        let err = resources
            .schedule_pass(pass_node)
            .expect_err("mismatched pass node must not schedule");

        assert!(matches!(
            err,
            RenderGpuColorPassScheduleError::PassResourceKeyMismatch { .. }
        ));
    }

    #[test]
    fn gpu_output_stage_resource_plan_rejects_mismatched_materialized_input() {
        let resources = executable_gpu_output_stage_resources(610);
        let wrong_input = gpu_handle(
            999,
            resources.input.descriptor(),
            "wrong-materialized-input",
        );
        let mut table = GpuColorFrameResourceTable::new();

        let err = resources
            .insert_resources(
                &mut table,
                GpuColorFrameResource::new(wrong_input.clone(), "wrong-input"),
                GpuColorFrameResource::new(resources.output.clone(), "output-resource"),
            )
            .expect_err("wrong input handle must fail");

        assert_eq!(
            err,
            RenderGpuOutputStageMaterializeError::InputResourceMismatch {
                expected: resources.input,
                actual: wrong_input
            }
        );
        assert!(table.is_empty());
    }

    #[test]
    fn gpu_output_stage_resource_plan_surfaces_resource_table_conflicts() {
        let resources = executable_gpu_output_stage_resources(620);
        let mut stale_descriptor = resources.output.descriptor();
        stale_descriptor.color_space = ColorSpace::Rec2020.into();
        let stale_output = gpu_handle_with_format(
            resources.output.id().raw(),
            stale_descriptor,
            resources.output.texture_format(),
            "stale-output",
        );
        let mut table = GpuColorFrameResourceTable::new();
        table
            .insert(GpuColorFrameResource::new(stale_output.clone(), "stale"))
            .expect("insert stale output");

        let err = resources
            .insert_resources(
                &mut table,
                GpuColorFrameResource::new(resources.input.clone(), "input-resource"),
                GpuColorFrameResource::new(resources.output.clone(), "output-resource"),
            )
            .expect_err("table conflict must surface");

        assert_eq!(
            err,
            RenderGpuOutputStageMaterializeError::ResourceTable(
                GpuColorFrameResourceTableError::ContractMismatch {
                    id: resources.output.id(),
                    expected: resources.output.contract(),
                    actual: stale_output.contract()
                }
            )
        );
        assert!(matches!(
            table.get(&resources.input),
            Err(GpuColorFrameResourceTableError::MissingFrame { .. })
        ));
    }

    #[test]
    fn gpu_output_stage_resource_plan_carries_cpu_boundary_readback_plan() {
        let frame = cpu_working_frame();
        let stage_plan = gpu_output_stage_plan_for_cpu_output(&frame);
        let mut ids = GpuColorFrameIdAllocator::new(630);

        let resources = RenderGpuOutputStageResourcePlan::from_cpu_working_frame(
            &mut ids,
            &frame,
            &stage_plan,
            GpuColorFrameTextureFormat::Rgba8Unorm,
        )
        .expect("GPU output resources with readback");

        let readback = resources.readback.as_ref().expect("CPU boundary readback");
        assert_eq!(readback.handle, resources.output);
        assert_eq!(readback.output_descriptor, stage_plan.final_descriptor);
        assert_eq!(
            readback.texture_format,
            GpuColorFrameTextureFormat::Rgba8Unorm
        );
        assert_eq!(
            resources.output.descriptor().residency,
            ColorFrameResidency::Gpu
        );
        assert_eq!(
            stage_plan.final_descriptor.residency,
            ColorFrameResidency::Cpu
        );
        assert!(!resources.transform.requires_output_readback);
        assert_eq!(resources.stage_diagnostics(), stage_plan.diagnostics());
    }

    #[test]
    fn gpu_output_stage_resource_plan_rejects_unsupported_cpu_boundary_texture_format() {
        let frame = cpu_working_frame();
        let stage_plan = gpu_output_stage_plan_for_cpu_output(&frame);
        let mut ids = GpuColorFrameIdAllocator::new(640);

        let err = RenderGpuOutputStageResourcePlan::from_cpu_working_frame(
            &mut ids,
            &frame,
            &stage_plan,
            GpuColorFrameTextureFormat::Rgba16Float,
        )
        .expect_err("CPU boundary readback requires an encoded RGBA8 target");

        assert!(matches!(
            err,
            RenderGpuOutputStageResourcePlanError::OutputTextureEncodingMismatch {
                encoding: ColorFrameEncoding::EncodedRgba8,
                texture_format: GpuColorFrameTextureFormat::Rgba16Float
            }
        ));
    }

    #[test]
    fn gpu_output_stage_readback_resource_resolves_from_materialized_table() {
        let resources = executable_gpu_output_stage_resources_with_readback(650);
        let mut table = GpuColorFrameResourceTable::new();
        table
            .insert(GpuColorFrameResource::new(
                resources.output.clone(),
                "readback-target",
            ))
            .expect("insert output resource");

        let resolved = resources
            .resolve_readback_resource(&table)
            .expect("resolve readback resource")
            .expect("readback resource");

        assert_eq!(resolved.handle(), &resources.output);
        assert_eq!(resolved.resource(), &"readback-target");
    }

    #[test]
    fn gpu_output_stage_readback_resource_reports_missing_output() {
        let resources = executable_gpu_output_stage_resources_with_readback(660);
        let table = GpuColorFrameResourceTable::<&'static str>::new();

        let err = resources
            .resolve_readback_resource(&table)
            .expect_err("missing readback target must surface");

        assert_eq!(
            err,
            RenderGpuOutputStageReadbackError::ResourceTable(
                GpuColorFrameResourceTableError::MissingFrame { id: resources.output.id() }
            )
        );
    }

    #[test]
    fn gpu_output_stage_without_cpu_boundary_has_no_readback_resource() {
        let resources = executable_gpu_output_stage_resources(670);
        let table = GpuColorFrameResourceTable::<&'static str>::new();

        assert!(resources
            .resolve_readback_resource(&table)
            .expect("resolve no-readback resource")
            .is_none());
    }

    fn assert_stage_chain_is_contiguous(plan: &RenderColorStagePlan) {
        for pair in plan.stages.windows(2) {
            assert_eq!(pair[0].output(), pair[1].input());
        }
        assert_eq!(
            plan.stages.last().expect("stage plan should not be empty").output(),
            plan.final_descriptor
        );
    }

    fn blocked_gpu_output_plan() -> RenderColorTransformGpuPlan {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let transform =
            RenderColorTransform::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderColorTransformGpuPlanner::new(
            &mut cache,
            RenderColorTransformGpuOptions::default(),
        );
        let mut plan = planner
            .plan_output_transform(working_descriptor(ColorFrameResidency::Gpu), &transform)
            .expect("GPU output plan");
        plan.wgpu.blockers.push(OcioGpuWgpuBlocker::RenderPipelineNotPrepared);
        plan
    }

    fn executable_gpu_output_plan() -> RenderColorTransformGpuPlan {
        let mut plan = blocked_gpu_output_plan();
        plan.wgpu.blockers.clear();
        plan
    }

    fn executable_gpu_output_schedule(input_id: u64, output_id: u64) -> RenderGpuColorPassSchedule {
        let transform = executable_gpu_output_plan();
        let input = gpu_handle(input_id, transform.diagnostics.input, "working-input");
        let output = gpu_handle(output_id, transform.diagnostics.output, "display-output");
        let pass_node = pass_node_for_transform(&transform);
        RenderGpuColorPassSchedule::new(input, output, transform, pass_node)
            .expect("schedulable GPU color pass")
    }

    fn cpu_working_frame() -> CpuColorFrame {
        CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 4,
            height: 2,
            data: vec![[0.25, 0.5, 0.75, 1.0]; 8],
            color_space: WorkingColorSpace::LinearRec709,
        })
    }

    fn cpu_source_frame() -> CpuEncodedColorFrame {
        CpuEncodedColorFrame::source_rgba8(4, 2, ColorSpace::SLog3, [96, 128, 160, 255].repeat(8))
    }

    fn gpu_input_stage_plan_for_source(source: &CpuEncodedColorFrame) -> RenderColorStagePlan {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::MondrianSmart,
        );
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderColorStagePlanner::prefer_gpu(
            &mut cache,
            RenderColorTransformGpuOptions::default(),
        );
        planner
            .plan_input_to_working(source.descriptor(), &transform)
            .expect("GPU input stage plan")
    }

    fn gpu_output_stage_plan_for_frame(frame: &CpuColorFrame) -> RenderColorStagePlan {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let transform =
            RenderColorTransform::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderColorStagePlanner::prefer_gpu(
            &mut cache,
            RenderColorTransformGpuOptions::default(),
        );
        planner
            .plan_output_transform_float(frame.descriptor(), &transform)
            .expect("GPU output stage plan")
    }

    fn gpu_output_stage_plan_for_cpu_output(frame: &CpuColorFrame) -> RenderColorStagePlan {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let transform =
            RenderColorTransform::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderColorStagePlanner::prefer_gpu(
            &mut cache,
            RenderColorTransformGpuOptions {
                output_residency: ColorFrameResidency::Cpu,
                ..RenderColorTransformGpuOptions::default()
            },
        );
        planner
            .plan_output_transform(frame.descriptor(), &transform)
            .expect("GPU output stage plan")
    }

    fn add_gpu_stage_blocker(stage_plan: &mut RenderColorStagePlan) {
        add_gpu_stage_blocker_reason(stage_plan, OcioGpuWgpuBlocker::RenderPipelineNotPrepared);
    }

    fn add_gpu_stage_blocker_reason(
        stage_plan: &mut RenderColorStagePlan,
        blocker: OcioGpuWgpuBlocker,
    ) {
        for stage in &mut stage_plan.stages {
            if let RenderColorStage::GpuColorTransform { plan, .. } = stage {
                plan.wgpu.blockers.push(blocker.clone());
            }
        }
    }

    fn map_readback_buffer(device: &wgpu::Device, readback: &wgpu::Buffer) -> Vec<u8> {
        let (tx, rx) = std::sync::mpsc::channel();
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        let _ = device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
        rx.recv()
            .expect("readback map callback should run")
            .expect("readback map should succeed");
        slice.get_mapped_range().expect("gpu readback mapped range").to_vec()
    }

    fn max_rgba_delta(expected: &[u8], actual: &[u8]) -> u8 {
        expected
            .iter()
            .zip(actual)
            .map(|(&expected, &actual)| expected.abs_diff(actual))
            .max()
            .unwrap_or(0)
    }

    fn assert_srgb_display_accurate(expected: &[u8], actual: &[u8]) {
        let report = crate::compare_srgb_display_rgba8(
            expected,
            actual,
            crate::SrgbDisplayAccuracyBudget::new(0.75, 0.20, 0.50, 0),
        )
        .expect("valid encoded sRGB output buffers");
        assert!(
            report.within_budget,
            "sRGB display accuracy budget exceeded: {report:#?}"
        );
    }

    #[test]
    fn cpu_float_output_boundary_returns_float_frame_without_rgba8_quantization() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let frame = cpu_working_frame();
        let boundary = RenderOutputColorBoundary::export(
            ColorSpace::Rec709,
            false,
            ColorEngine::MondrianSmart,
        );

        let result = execute_cpu_output_boundary_float(&frame, &boundary)
            .expect("float output boundary should execute");

        assert_eq!(
            result.frame.descriptor().encoding,
            crate::ColorFrameEncoding::EncodedFloat
        );
        assert_eq!(result.frame.descriptor().domain, ColorFrameDomain::Export);
        assert_eq!(result.output_descriptor.domain, ColorFrameDomain::Export);
        assert_eq!(
            result.color_diagnostics.input.domain,
            ColorFrameDomain::Working
        );
        assert_eq!(
            result.color_diagnostics.output.domain,
            ColorFrameDomain::Export
        );
        assert_eq!(result.color_diagnostics.pixel_count, 8);
        assert!(!result.color_diagnostics.used_rgba8_boundary);
        assert_eq!(
            result.color_diagnostics.backend,
            RenderColorTransformBackend::CpuOcioFloat
        );
        let pixels = &result.frame.rgba_f32().data;
        assert_eq!(pixels.len(), 8);
        for px in pixels {
            let [r, g, b, a] = *px;
            assert!((0.0..=1.0).contains(&r));
            assert!((0.0..=1.0).contains(&g));
            assert!((0.0..=1.0).contains(&b));
            assert!((0.0..=1.0).contains(&a));
        }
    }

    #[test]
    fn cpu_float_output_boundary_display_view_returns_float_frame() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let (display, view) = ocio_default_display_view().expect("default display/view");
        let frame = cpu_working_frame();
        let boundary = RenderOutputColorBoundary::display_view(
            ColorSpace::Srgb,
            display,
            view,
            false,
            ColorEngine::MondrianSmart,
        );

        let result = execute_cpu_output_boundary_float(&frame, &boundary)
            .expect("float display/view output boundary");

        assert_eq!(
            result.frame.descriptor().encoding,
            crate::ColorFrameEncoding::EncodedFloat
        );
        assert_eq!(result.color_diagnostics.pixel_count, 8);
        assert!(!result.color_diagnostics.used_rgba8_boundary);
    }

    #[test]
    fn export_view_boundary_target_is_export_with_display_view() {
        let boundary = RenderOutputColorBoundary::export_view(
            ColorSpace::Srgb,
            "sRGB - Display",
            "ACES 2.0 - SDR 100 nits (Rec.709)",
            true,
            ColorEngine::MondrianSmart,
        );

        assert_eq!(boundary.target, RenderOutputColorBoundaryTarget::Export);
        assert!(boundary.display_view.is_some());
        let dv = boundary.display_view.as_ref().unwrap();
        assert_eq!(dv.display, "sRGB - Display");
        assert_eq!(dv.view, "ACES 2.0 - SDR 100 nits (Rec.709)");
        assert!(boundary.tone_map);
    }

    #[test]
    fn export_view_boundary_uses_delivery_view_transform() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let boundary = RenderOutputColorBoundary::export_view(
            ColorSpace::Srgb,
            "sRGB - Display",
            "ACES 2.0 - SDR 100 nits (Rec.709)",
            true,
            ColorEngine::MondrianSmart,
        );

        let transform = boundary.transform();
        assert_eq!(transform.output_domain, crate::ColorFrameDomain::Export);
        assert!(transform.display_view.is_some());
        let dv = transform.display_view.as_ref().unwrap();
        assert_eq!(dv.display, "sRGB - Display");
        assert_eq!(dv.view, "ACES 2.0 - SDR 100 nits (Rec.709)");
        assert!(transform.tone_map);
    }

    #[test]
    fn export_view_boundary_float_path_uses_display_transform() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let frame = cpu_working_frame();
        let boundary = RenderOutputColorBoundary::export_view(
            ColorSpace::Srgb,
            "sRGB - Display",
            "ACES 2.0 - SDR 100 nits (Rec.709)",
            true,
            ColorEngine::MondrianSmart,
        );

        let result = execute_cpu_output_boundary_float(&frame, &boundary)
            .expect("export view float boundary should execute");

        assert_eq!(
            result.frame.descriptor().encoding,
            crate::ColorFrameEncoding::EncodedFloat
        );
        assert_eq!(result.color_diagnostics.pixel_count, 8);
        assert_eq!(
            result.color_diagnostics.backend,
            crate::RenderColorTransformBackend::CpuOcioFloat
        );
        assert!(!result.color_diagnostics.used_rgba8_boundary);
    }

    #[test]
    fn export_plain_boundary_no_view_uses_pipeline_transform() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let frame = cpu_working_frame();
        let boundary = RenderOutputColorBoundary::export(
            ColorSpace::Rec709,
            false,
            ColorEngine::MondrianSmart,
        );

        let result = execute_cpu_output_boundary_float(&frame, &boundary)
            .expect("plain export boundary should execute");

        assert_eq!(
            result.frame.descriptor().encoding,
            crate::ColorFrameEncoding::EncodedFloat
        );
        assert!(!result.color_diagnostics.used_rgba8_boundary);
    }

    #[test]
    fn preview_display_view_boundary_uses_display_target() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let (display, view) = ocio_default_display_view().expect("default display/view");
        let boundary = RenderOutputColorBoundary::display_view(
            ColorSpace::Srgb,
            display,
            view,
            false,
            ColorEngine::MondrianSmart,
        );

        assert_eq!(boundary.target, RenderOutputColorBoundaryTarget::Display);
        let transform = boundary.transform();
        assert_eq!(transform.output_domain, crate::ColorFrameDomain::Display);
    }

    fn emit_gpu_output_smoke_report(report: &GpuOutputBoundarySmokeReport) -> anyhow::Result<()> {
        let report_json = serde_json::to_string(report)?;
        eprintln!("MONDRIAN_RENDERER_GPU_OUTPUT_JSON={report_json}");
        if let Some(path) = renderer_gpu_output_smoke_output_path() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = OpenOptions::new().create(true).append(true).open(path)?;
            writeln!(file, "{report_json}")?;
        }
        Ok(())
    }

    fn renderer_gpu_output_smoke_output_path() -> Option<PathBuf> {
        std::env::var_os("MONDRIAN_RENDERER_GPU_OUTPUT_SMOKE_OUTPUT").map(PathBuf::from)
    }

    fn executable_gpu_output_stage_resources(first_id: u64) -> RenderGpuOutputStageResourcePlan {
        let frame = cpu_working_frame();
        let stage_plan = gpu_output_stage_plan_for_frame(&frame);
        let mut ids = GpuColorFrameIdAllocator::new(first_id);
        RenderGpuOutputStageResourcePlan::from_cpu_working_frame(
            &mut ids,
            &frame,
            &stage_plan,
            GpuColorFrameTextureFormat::Rgba16Float,
        )
        .expect("GPU output stage resources")
    }

    fn executable_gpu_output_stage_resources_with_readback(
        first_id: u64,
    ) -> RenderGpuOutputStageResourcePlan {
        let frame = cpu_working_frame();
        let stage_plan = gpu_output_stage_plan_for_cpu_output(&frame);
        let mut ids = GpuColorFrameIdAllocator::new(first_id);
        RenderGpuOutputStageResourcePlan::from_cpu_working_frame(
            &mut ids,
            &frame,
            &stage_plan,
            GpuColorFrameTextureFormat::Rgba8Unorm,
        )
        .expect("GPU output stage resources with readback")
    }

    fn executable_gpu_input_stage_resources(first_id: u64) -> RenderGpuInputStageResourcePlan {
        let source = cpu_source_frame();
        let stage_plan = gpu_input_stage_plan_for_source(&source);
        let mut ids = GpuColorFrameIdAllocator::new(first_id);
        RenderGpuInputStageResourcePlan::from_cpu_encoded_source_frame(
            &mut ids,
            &source,
            &stage_plan,
        )
        .expect("GPU input stage resources")
    }

    fn gpu_handle(
        id: u64,
        descriptor: ColorFrameDescriptor,
        label: &'static str,
    ) -> GpuColorFrameHandle {
        gpu_handle_with_format(
            id,
            descriptor,
            GpuColorFrameTextureFormat::Rgba16Float,
            label,
        )
    }

    fn gpu_handle_with_format(
        id: u64,
        descriptor: ColorFrameDescriptor,
        texture_format: GpuColorFrameTextureFormat,
        label: &'static str,
    ) -> GpuColorFrameHandle {
        GpuColorFrameHandle::new(
            GpuColorFrameId::from_raw(id),
            descriptor,
            texture_format,
            label,
        )
        .expect("GPU frame handle")
    }

    fn pass_node_for_transform(
        transform: &RenderColorTransformGpuPlan,
    ) -> OcioGpuWgpuRenderPassNodePlan {
        let wrapper_layout =
            OcioGpuWgpuWrapperBindingPlan::for_contract(&transform.wgpu.resources.wrapper_contract);
        let wrapper_layout_descriptor =
            OcioGpuWgpuBindGroupLayoutDescriptorPlan::for_wrapper_input(&wrapper_layout);
        OcioGpuWgpuRenderPassNodePlan {
            resource_key: transform.wgpu.resources.resource_key,
            render_pipeline_cache_key: 11,
            render_descriptor_hash: 12,
            ocio_layout_hash: 13,
            wrapper_layout_hash: wrapper_layout_descriptor.layout_hash,
            output_format: crate::OcioGpuWgpuColorTargetFormat::Rgba16Float,
            vertex_count: 4,
            node_hash: 15,
        }
    }

    #[test]
    fn classify_ocio_shader_error_maps_known_error_messages() {
        // Verify the error classifier correctly maps known OCIO error messages
        // to typed blockers.
        assert!(matches!(
            crate::ocio_gpu::classify_ocio_shader_error(
                "no OCIO config loaded (call ensure_ocio_loaded first)"
            ),
            OcioGpuWgpuBlocker::OcioConfigNotLoaded
        ));
        assert!(matches!(
            crate::ocio_gpu::classify_ocio_shader_error(
                "OCIO processor 'srgb' -> 'rec709': processor not found"
            ),
            OcioGpuWgpuBlocker::OcioProcessorUnavailable
        ));
        assert!(matches!(
            crate::ocio_gpu::classify_ocio_shader_error(
                "OCIO display processor 'srgb' -> 'srgb/view': failed"
            ),
            OcioGpuWgpuBlocker::OcioProcessorUnavailable
        ));
        assert!(matches!(
            crate::ocio_gpu::classify_ocio_shader_error(
                "OCIO GPU shader extraction returned empty shader text"
            ),
            OcioGpuWgpuBlocker::OcioGpuShaderExtractionFailed { .. }
        ));
        // Unknown error messages fall through to ExtractionFailed.
        assert!(matches!(
            crate::ocio_gpu::classify_ocio_shader_error("some unknown error"),
            OcioGpuWgpuBlocker::OcioGpuShaderExtractionFailed { .. }
        ));
    }

    #[test]
    fn prepare_wgpu_execution_never_propagates_extraction_error() {
        // Core invariant: prepare_wgpu_execution must always return Ok,
        // converting extraction failures into blocked plans with typed
        // blockers. This ensures the stage plan is always produced and
        // diagnostics always have evidence.
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let mut cache = OcioGpuShaderCache::default();
        // Use a nonexistent display/view to trigger extraction failure.
        let result = cache.prepare_wgpu_execution(OcioGpuShaderRequest::DisplayView {
            src: ColorSpace::Rec709.into(),
            display: "nonexistent_display_for_structural_test".to_owned(),
            view: "nonexistent_view_for_structural_test".to_owned(),
            language: GpuLanguage::Glsl4_0,
        });
        // Must always be Ok - extraction errors become blocked plans.
        let plan = result.expect("prepare_wgpu_execution must never return Err");
        // The display/view doesn't exist, so we should have blockers.
        assert!(
            !plan.can_execute(),
            "nonexistent display/view should produce blockers"
        );
        assert!(!plan.blockers.is_empty());
        assert!(matches!(
            plan.blockers[0],
            OcioGpuWgpuBlocker::OcioProcessorUnavailable
                | OcioGpuWgpuBlocker::OcioGpuShaderExtractionFailed { .. }
        ));
    }

    #[test]
    fn gpu_shader_extraction_failure_does_not_mask_cpu_fallback_path() {
        // When the GPU path fails with a blocker, the CPU path should still
        // be available and produce a clean result without GPU stages.
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let frame = cpu_working_frame();
        let boundary =
            RenderOutputColorBoundary::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);

        // CPU-only path should work regardless of GPU state.
        let cpu_output =
            execute_cpu_output_boundary(&frame, &boundary).expect("CPU boundary should succeed");
        assert_eq!(cpu_output.stage_diagnostics.gpu_color_stages, 0);
        assert_eq!(cpu_output.stage_diagnostics.gpu_blockers, 0);
        assert_eq!(
            cpu_output.stage_diagnostics.gpu_blocker_breakdown.total(),
            0
        );

        // The GPU path with a nonexistent display/view should produce a blocked plan.
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderOutputColorBoundaryPlanner::prefer_gpu(
            &mut cache,
            RenderColorTransformGpuOptions::default(),
        );
        let boundary_blocked = RenderOutputColorBoundary::display_view(
            ColorSpace::Srgb,
            "nonexistent_display_for_cpu_mask_test",
            "nonexistent_view_for_cpu_mask_test",
            false,
            ColorEngine::MondrianSmart,
        );
        let gpu_plan = planner.plan(&frame, &boundary_blocked).expect("GPU plan");
        // If it contains GPU transform, it must have blockers.
        if gpu_plan.stage_plan.contains_gpu_transform() {
            let d = gpu_plan.stage_plan.diagnostics();
            assert!(d.gpu_blockers > 0, "blocked GPU plan should have blockers");
        }

        // The CPU path diagnostics must not show GPU stages.
        assert_eq!(cpu_output.stage_diagnostics.gpu_color_stages, 0);
    }

    #[test]
    fn gpu_plan_blocker_breakdown_records_all_renderer_blocker_types() {
        // Verify that the breakdown correctly accumulates all 7 renderer-level
        // blocker types through the record() method.
        let mut breakdown = RenderColorStageGpuBlockerBreakdown::default();

        breakdown.record(&OcioGpuWgpuBlocker::OcioConfigNotLoaded);
        breakdown.record(&OcioGpuWgpuBlocker::OcioProcessorUnavailable);
        breakdown.record(&OcioGpuWgpuBlocker::OcioGpuShaderExtractionFailed {
            reason: "test".to_owned(),
        });
        breakdown.record(&OcioGpuWgpuBlocker::ShaderModuleNotPrepared {
            language: GpuLanguage::Glsl4_0,
        });
        breakdown.record(&OcioGpuWgpuBlocker::OcioResourceBindGroupNotPrepared {
            texture_2d_count: 0,
            texture_3d_count: 0,
            uniform_buffers: 0,
        });
        breakdown.record(&OcioGpuWgpuBlocker::FullscreenWrapperNotPrepared);
        breakdown.record(&OcioGpuWgpuBlocker::RenderPipelineNotPrepared);

        assert_eq!(breakdown.total(), 7);
        assert_eq!(breakdown.ocio_config_not_loaded, 1);
        assert_eq!(breakdown.ocio_processor_unavailable, 1);
        assert_eq!(breakdown.ocio_gpu_shader_extraction_failed, 1);
        assert_eq!(breakdown.shader_module_not_prepared, 1);
        assert_eq!(breakdown.ocio_resource_bind_group_not_prepared, 1);
        assert_eq!(breakdown.fullscreen_wrapper_not_prepared, 1);
        assert_eq!(breakdown.render_pipeline_not_prepared, 1);
    }

    #[test]
    fn gpu_output_health_report_classifies_extraction_failure() {
        // Verify that the health report correctly classifies a sample with
        // OcioGpuShaderExtractionFailed as Failed.
        let frame = RenderGpuOutputFrameReport {
            width: 64,
            height: 64,
            pixel_count: 4096,
            input_color_space: ColorSpace::Rec709,
            output_color_space: ColorSpace::Srgb,
        };
        let stage = RenderGpuOutputStageDiagnosticsReport {
            total_stages: 1,
            gpu_color_stages: 1,
            gpu_blockers: 1,
            gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown {
                ocio_gpu_shader_extraction_failed: 1,
                ..RenderColorStageGpuBlockerBreakdown::default()
            },
            ..RenderGpuOutputStageDiagnosticsReport::default()
        };
        let runtime = RenderGpuOutputRuntimeDiagnosticsReport::default();

        let report = RenderGpuOutputHealthReport::from_sample(
            "test-extraction-failure",
            None,
            &frame,
            &stage,
            &runtime,
            0,
            0,
            0,
        );

        assert_eq!(report.verdict, RenderGpuOutputHealthVerdict::Fail);
        assert!(!report.summary.native_gpu_output_ready);
        assert!(!report.summary.no_gpu_blockers);
        assert!(report.evidence.gpu_blocker_breakdown.ocio_gpu_shader_extraction_failed > 0);
        assert!(report.root_causes.iter().any(|rc| rc.code == "gpu_stage_blocked"));
    }
}
