use crate::{
    ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding, ColorFrameResidency, CpuColorFrame,
    CpuColorTransformExecutor, CpuEncodedColorFrame, GpuColorFrameAllocationPlan,
    GpuColorFrameHandle, GpuColorFrameId, GpuColorFrameIdAllocator, GpuColorFrameReadback,
    GpuColorFrameReadbackError, GpuColorFrameReadbackPlan, GpuColorFrameResource,
    GpuColorFrameResourceTable, GpuColorFrameResourceTableError, GpuColorFrameTextureFormat,
    GpuColorFrameUploadError, GpuColorFrameUploadPlan, GpuColorFrameUploader,
    GpuColorFrameWgpuResource, OcioGpuShaderCache, OcioGpuShaderCacheDiagnostics,
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
    RenderOutputTransformResult,
};
use mondrian_core::types::{ColorEngine, ColorSpace};
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
}

impl RenderColorStageGpuBlockerBreakdown {
    /// Total counted native GPU blockers.
    pub fn total(self) -> u64 {
        self.shader_module_not_prepared
            .saturating_add(self.ocio_resource_bind_group_not_prepared)
            .saturating_add(self.fullscreen_wrapper_not_prepared)
            .saturating_add(self.render_pipeline_not_prepared)
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
            RenderOutputColorBoundaryTarget::Export => RenderColorTransform::export(
                self.output_color_space,
                self.tone_map,
                self.engine.clone(),
            ),
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
            .plan(frame, boundary)
            .map_err(RenderGpuOutputBoundaryRuntimeRecordError::Plan)?;
        let resources = plan
            .gpu_resource_plan(frame_ids, frame, output_texture_format)
            .map_err(RenderGpuOutputBoundaryRuntimeRecordError::ResourcePlan)?;
        let output_format = color_target_format_for_gpu_frame(&resources.output);
        let shader_plan = resources.transform.wgpu.shader_plan.clone();
        let wrapper_color = resources.transform.wgpu.wrapper_color;
        let static_pipeline = backend_prep
            .prepare_static_pipeline(&shader_plan, wrapper_color, output_format)
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
        let stage_plan = self
            .stage_planner
            .plan_output_transform(frame.descriptor(), &boundary.transform())?;
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
    /// Upload plan that moves the CPU working frame into the input GPU frame.
    pub input_upload: GpuColorFrameUploadPlan,
    /// Allocation plan for the output GPU target frame.
    pub output_allocation: GpuColorFrameAllocationPlan,
    /// GPU transform plan that these resources satisfy.
    pub transform: RenderColorTransformGpuPlan,
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
            total_stages: 2,
            upload_stages: 1,
            gpu_color_stages: 1,
            stage_pixels: self
                .input
                .descriptor()
                .pixel_count()
                .saturating_add(self.output.descriptor().pixel_count())
                as u64,
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
            return Err(
                RenderGpuOutputStageResourcePlanError::NativeBlockersRemaining {
                    blockers: planned.transform.wgpu.blockers.len(),
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
            Some(
                GpuColorFrameReadbackPlan::encoded_rgba8(output.clone())
                    .map_err(RenderGpuOutputStageResourcePlanError::OutputReadback)?,
            )
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
            input_upload,
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
        let input = GpuColorFrameUploader::upload(device, queue, &self.input_upload);
        let output = GpuColorFrameUploader::allocate(device, &self.output_allocation);
        self.insert_resources(table, input, output)
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

/// Error returned when GPU output stage resources cannot be planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderGpuOutputStageResourcePlanError {
    /// Native GPU blockers remain.
    NativeBlockersRemaining {
        /// Number of blockers in the GPU execution plan.
        blockers: usize,
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
            color_space: transform.working_color_space,
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
        let output = ColorFrameDescriptor {
            width: input.width,
            height: input.height,
            color_space: transform.output_color_space,
            domain: transform.output_domain,
            encoding: ColorFrameEncoding::EncodedRgba8,
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
        OcioGpuWgpuWrapperColorContract,
    };
    use mondrian_core::types::{ColorEngine, ColorSpace};
    use mondrian_core::RgbaF32Frame;
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
        frame: GpuOutputFrameReport,
        output_texture_format: &'static str,
        health: GpuOutputHealthSummary,
        stage: GpuOutputStageDiagnosticsReport,
        runtime: GpuOutputRuntimeDiagnosticsReport,
        readback_bytes: usize,
        max_rgba_delta: u8,
        tolerance: u8,
        passed: bool,
    }

    #[derive(Debug, serde::Serialize)]
    struct GpuOutputAdapterReport {
        name: String,
        backend: String,
        device_type: String,
        driver: String,
        driver_info: String,
    }

    #[derive(Debug, serde::Serialize)]
    struct GpuOutputFrameReport {
        width: usize,
        height: usize,
        pixel_count: usize,
        input_color_space: ColorSpace,
        output_color_space: ColorSpace,
    }

    #[derive(Debug, serde::Serialize)]
    struct GpuOutputStageDiagnosticsReport {
        total_stages: u64,
        upload_stages: u64,
        gpu_color_stages: u64,
        readback_stages: u64,
        gpu_blockers: u64,
        gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown,
        stage_pixels: u64,
    }

    impl From<RenderColorStageDiagnostics> for GpuOutputStageDiagnosticsReport {
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

    #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
    struct GpuOutputHealthSummary {
        status: &'static str,
        native_gpu_output_ready: bool,
        complete_stage_sequence: bool,
        no_gpu_blockers: bool,
        backend_runtime_ready: bool,
        shader_cache_warmed: bool,
        readback_complete: bool,
        parity_within_tolerance: bool,
        expected_readback_bytes: usize,
    }

    impl GpuOutputHealthSummary {
        fn evaluate(
            skipped: bool,
            frame: &GpuOutputFrameReport,
            stage: &GpuOutputStageDiagnosticsReport,
            runtime: &GpuOutputRuntimeDiagnosticsReport,
            readback_bytes: usize,
            max_rgba_delta: u8,
            tolerance: u8,
        ) -> Self {
            let expected_readback_bytes = frame.pixel_count.saturating_mul(4);
            let complete_stage_sequence = stage.total_stages == 3
                && stage.upload_stages == 1
                && stage.gpu_color_stages == 1
                && stage.readback_stages == 1;
            let no_gpu_blockers =
                stage.gpu_blockers == 0 && stage.gpu_blocker_breakdown.total() == 0;
            let shader_cache_warmed = runtime.shader_cache_entries > 0
                && runtime.shader_cache_misses > 0
                && runtime.shader_cache_extraction_failures == 0;
            let backend_runtime_ready = runtime.backend_prep_resource_entries > 0
                && runtime.backend_object_entries > 0
                && runtime.backend_object_misses > 0
                && runtime.backend_object_failures == 0;
            let readback_complete =
                expected_readback_bytes > 0 && readback_bytes == expected_readback_bytes;
            let parity_within_tolerance = max_rgba_delta <= tolerance;
            let native_gpu_output_ready = !skipped
                && complete_stage_sequence
                && no_gpu_blockers
                && shader_cache_warmed
                && backend_runtime_ready
                && readback_complete;
            let status = if skipped {
                "skipped"
            } else if native_gpu_output_ready && parity_within_tolerance {
                "passed"
            } else {
                "failed"
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

    #[derive(Debug, serde::Serialize)]
    struct GpuOutputRuntimeDiagnosticsReport {
        shader_cache_entries: usize,
        shader_cache_hits: u64,
        shader_cache_misses: u64,
        shader_cache_extraction_failures: u64,
        backend_prep_resource_entries: usize,
        backend_object_entries: usize,
        backend_object_hits: u64,
        backend_object_misses: u64,
        backend_object_failures: u64,
        frame_table_entries: usize,
        next_frame_id: u64,
    }

    impl From<RenderGpuOutputBoundaryRuntimeDiagnostics> for GpuOutputRuntimeDiagnosticsReport {
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

    #[test]
    fn gpu_output_smoke_health_summary_classifies_native_path() {
        let frame = GpuOutputFrameReport {
            width: 2,
            height: 2,
            pixel_count: 4,
            input_color_space: ColorSpace::Rec709,
            output_color_space: ColorSpace::Srgb,
        };
        let stage = GpuOutputStageDiagnosticsReport {
            total_stages: 3,
            upload_stages: 1,
            gpu_color_stages: 1,
            readback_stages: 1,
            gpu_blockers: 0,
            gpu_blocker_breakdown: RenderColorStageGpuBlockerBreakdown::default(),
            stage_pixels: 12,
        };
        let runtime = GpuOutputRuntimeDiagnosticsReport {
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

        let passed = GpuOutputHealthSummary::evaluate(false, &frame, &stage, &runtime, 16, 2, 3);
        assert_eq!(passed.status, "passed");
        assert!(passed.native_gpu_output_ready);
        assert!(passed.parity_within_tolerance);
        assert_eq!(passed.expected_readback_bytes, 16);

        let incomplete_readback =
            GpuOutputHealthSummary::evaluate(false, &frame, &stage, &runtime, 12, 2, 3);
        assert_eq!(incomplete_readback.status, "failed");
        assert!(!incomplete_readback.readback_complete);
        assert!(!incomplete_readback.native_gpu_output_ready);

        let skipped = GpuOutputHealthSummary::evaluate(true, &frame, &stage, &runtime, 16, 2, 3);
        assert_eq!(skipped.status, "skipped");
        assert!(!skipped.native_gpu_output_ready);
    }

    fn source_descriptor(residency: ColorFrameResidency) -> ColorFrameDescriptor {
        ColorFrameDescriptor {
            width: 1280,
            height: 720,
            color_space: ColorSpace::SLog3,
            domain: ColorFrameDomain::Source,
            encoding: ColorFrameEncoding::EncodedRgba8,
            residency,
        }
    }

    fn working_descriptor(residency: ColorFrameResidency) -> ColorFrameDescriptor {
        ColorFrameDescriptor {
            width: 1920,
            height: 1080,
            color_space: ColorSpace::Rec709,
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency,
        }
    }

    #[test]
    fn cpu_input_plan_uses_single_cpu_stage() {
        let transform =
            RenderInputTransform::to_working(ColorSpace::Rec709, false, ColorEngine::MondrianSmart);
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
        let transform =
            RenderInputTransform::to_working(ColorSpace::Rec709, false, ColorEngine::MondrianSmart);
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
        let transform =
            RenderInputTransform::to_working(ColorSpace::Rec709, false, ColorEngine::MondrianSmart);
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
        let frame = CpuColorFrame::working(RgbaF32Frame {
            width: 1920,
            height: 1080,
            data: vec![[0.25, 0.5, 0.75, 1.0]; 1920 * 1080],
            color_space: ColorSpace::Rec709,
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
        let input_transform =
            RenderInputTransform::to_working(ColorSpace::Rec709, false, ColorEngine::MondrianSmart);
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
                src: ColorSpace::SLog3,
                dst: ColorSpace::Rec709,
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
    fn gpu_output_boundary_plans_linear_aware_wrapper_for_working_output() {
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
        assert_eq!(
            gpu_plan.wgpu.wrapper_color,
            OcioGpuWgpuWrapperColorContract::linear_working_to_encoded_output(ColorSpace::Rec709)
        );
        assert!(gpu_plan.wgpu.blockers.is_empty());
        assert!(gpu_plan.wgpu.can_execute());

        let mut ids = GpuColorFrameIdAllocator::new(1_000);
        let resources = plan
            .gpu_resource_plan(&mut ids, &frame, GpuColorFrameTextureFormat::Rgba8Unorm)
            .expect("linear-aware GPU output resources should materialize");
        assert_eq!(
            resources.transform.wgpu.wrapper_color,
            OcioGpuWgpuWrapperColorContract::linear_working_to_encoded_output(ColorSpace::Rec709)
        );
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

        assert_rgba_close(&expected.rgba, actual.rgba(), 3);
        assert_eq!(record.stage_diagnostics.total_stages, 3);
        assert_eq!(record.stage_diagnostics.upload_stages, 1);
        assert_eq!(record.stage_diagnostics.gpu_color_stages, 1);
        assert_eq!(record.stage_diagnostics.readback_stages, 1);
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

        assert_rgba_close(&expected.rgba, actual.rgba(), 3);
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
                let frame_report = GpuOutputFrameReport {
                    width: 0,
                    height: 0,
                    pixel_count: 0,
                    input_color_space: ColorSpace::Rec709,
                    output_color_space: ColorSpace::Srgb,
                };
                let stage_report: GpuOutputStageDiagnosticsReport =
                    RenderColorStageDiagnostics::default().into();
                let runtime_report: GpuOutputRuntimeDiagnosticsReport =
                    skipped_runtime.diagnostics().into();
                let health = GpuOutputHealthSummary::evaluate(
                    true,
                    &frame_report,
                    &stage_report,
                    &runtime_report,
                    0,
                    0,
                    tolerance,
                );
                let report = GpuOutputBoundarySmokeReport {
                    scenario: "renderer_gpu_output_boundary",
                    skipped: Some(format!("no GPU adapter available: {err}")),
                    adapter: None,
                    frame: frame_report,
                    output_texture_format: "Rgba8Unorm",
                    health,
                    stage: stage_report,
                    runtime: runtime_report,
                    readback_bytes: 0,
                    max_rgba_delta: 0,
                    tolerance,
                    passed: false,
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
        let frame_report = GpuOutputFrameReport {
            width: frame.descriptor().width as usize,
            height: frame.descriptor().height as usize,
            pixel_count: frame.descriptor().pixel_count(),
            input_color_space: frame.descriptor().color_space,
            output_color_space: boundary.output_color_space,
        };
        let stage_report = stage_diagnostics.into();
        let runtime_report = runtime_diagnostics.into();
        let health = GpuOutputHealthSummary::evaluate(
            false,
            &frame_report,
            &stage_report,
            &runtime_report,
            actual.rgba().len(),
            max_rgba_delta,
            tolerance,
        );
        let passed = health.status == "passed";
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
            health,
            stage: stage_report,
            runtime: runtime_report,
            readback_bytes: actual.rgba().len(),
            max_rgba_delta,
            tolerance,
            passed,
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

        assert!(matches!(
            err,
            RenderGpuOutputStageResourcePlanError::NativeBlockersRemaining { blockers } if blockers > 0
        ));
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
            ColorSpace::Rec709
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
        let transform =
            RenderInputTransform::to_working(ColorSpace::Rec709, false, ColorEngine::MondrianSmart);
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
        wrong_output_descriptor.color_space = ColorSpace::Rec2020;
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
        assert_eq!(resources.input_upload.handle, resources.input);
        assert_eq!(
            resources.input_upload.texture_format,
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
            RenderGpuOutputStageResourcePlanError::NativeBlockersRemaining { blockers } if blockers > 0
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
        stale_descriptor.color_space = ColorSpace::Rec2020;
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
            RenderGpuOutputStageResourcePlanError::OutputReadback(
                GpuColorFrameReadbackError::UnsupportedTextureFormat {
                    texture_format: GpuColorFrameTextureFormat::Rgba16Float
                }
            )
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
        CpuColorFrame::working(RgbaF32Frame {
            width: 4,
            height: 2,
            data: vec![[0.25, 0.5, 0.75, 1.0]; 8],
            color_space: ColorSpace::Rec709,
        })
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
            .plan_output_transform(frame.descriptor(), &transform)
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

    fn assert_rgba_close(expected: &[u8], actual: &[u8], tolerance: u8) {
        assert_eq!(expected.len(), actual.len());
        for (index, (&expected, &actual)) in expected.iter().zip(actual).enumerate() {
            let delta = expected.abs_diff(actual);
            assert!(
                delta <= tolerance,
                "rgba byte {index}: expected {expected}, got {actual}, tolerance {tolerance}"
            );
        }
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
}
