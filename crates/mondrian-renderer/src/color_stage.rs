use crate::{
    ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding, ColorFrameResidency, CpuColorFrame,
    CpuColorTransformExecutor, CpuEncodedColorFrame, GpuColorFrameAllocationPlan,
    GpuColorFrameHandle, GpuColorFrameId, GpuColorFrameIdAllocator, GpuColorFrameResource,
    GpuColorFrameResourceTable, GpuColorFrameResourceTableError, GpuColorFrameTextureFormat,
    GpuColorFrameUploadError, GpuColorFrameUploadPlan, GpuColorFrameUploader,
    GpuColorFrameWgpuResource, OcioGpuShaderCache, OcioGpuWgpuBindGroupPreparer,
    OcioGpuWgpuColorTargetFormat, OcioGpuWgpuOcioBindGroup, OcioGpuWgpuRenderPassError,
    OcioGpuWgpuRenderPassNodePlan, OcioGpuWgpuRenderPassRecorder, OcioGpuWgpuRenderPassTarget,
    OcioGpuWgpuRenderPipeline, OcioGpuWgpuWrapperBindGroup, OcioGpuWgpuWrapperBindingPlan,
    OcioGpuWgpuWrapperInputResources, RenderColorTransform, RenderColorTransformError,
    RenderColorTransformGpuOptions, RenderColorTransformGpuPlan, RenderColorTransformGpuPlanner,
    RenderInputTransform, RenderInputTransformResult, RenderOutputTransformResult,
};

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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
    /// Sum of pixels touched by scheduled stages.
    pub stage_pixels: u64,
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

impl RenderGpuOutputStageResourcePlan {
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
        let output_allocation = GpuColorFrameAllocationPlan::for_handle(output.clone());
        let mut transform = (*planned.transform).clone();
        transform.diagnostics.input = planned.gpu_input;
        transform.diagnostics.output = planned.gpu_output;
        transform.requires_source_upload = false;
        transform.requires_output_readback = false;
        Ok(Self {
            input: input_upload.handle.clone(),
            output,
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
        if pass_node.wrapper_layout_hash != wrapper_layout.layout_hash {
            return Err(
                RenderGpuColorPassScheduleError::PassWrapperLayoutHashMismatch {
                    expected: wrapper_layout.layout_hash,
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
        if wrapper_layout.layout_hash != self.pass_node.wrapper_layout_hash {
            return Err(
                RenderGpuColorPassExecutionError::WrapperLayoutHashMismatch {
                    expected: self.pass_node.wrapper_layout_hash,
                    actual: wrapper_layout.layout_hash,
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
    use crate::{GpuColorFrameId, GpuColorFrameIdAllocator, GpuColorFrameTextureFormat};
    use mondrian_core::ensure_mondrian_default_ocio_loaded;
    use mondrian_core::types::{ColorEngine, ColorSpace};
    use mondrian_core::RgbaF32Frame;

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
        let mut stage_plan = gpu_output_stage_plan_for_frame(&frame);
        clear_gpu_stage_blockers(&mut stage_plan);
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
        let stage_plan = gpu_output_stage_plan_for_frame(&frame);
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
        planner
            .plan_output_transform(working_descriptor(ColorFrameResidency::Gpu), &transform)
            .expect("GPU output plan")
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

    fn clear_gpu_stage_blockers(stage_plan: &mut RenderColorStagePlan) {
        for stage in &mut stage_plan.stages {
            if let RenderColorStage::GpuColorTransform { plan, .. } = stage {
                plan.wgpu.blockers.clear();
            }
        }
    }

    fn executable_gpu_output_stage_resources(first_id: u64) -> RenderGpuOutputStageResourcePlan {
        let frame = cpu_working_frame();
        let mut stage_plan = gpu_output_stage_plan_for_frame(&frame);
        clear_gpu_stage_blockers(&mut stage_plan);
        let mut ids = GpuColorFrameIdAllocator::new(first_id);
        RenderGpuOutputStageResourcePlan::from_cpu_working_frame(
            &mut ids,
            &frame,
            &stage_plan,
            GpuColorFrameTextureFormat::Rgba16Float,
        )
        .expect("GPU output stage resources")
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
        OcioGpuWgpuRenderPassNodePlan {
            resource_key: transform.wgpu.resources.resource_key,
            render_pipeline_cache_key: 11,
            render_descriptor_hash: 12,
            ocio_layout_hash: 13,
            wrapper_layout_hash: wrapper_layout.layout_hash,
            output_format: crate::OcioGpuWgpuColorTargetFormat::Rgba16Float,
            vertex_count: 4,
            node_hash: 15,
        }
    }
}
