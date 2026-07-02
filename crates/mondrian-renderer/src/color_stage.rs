use crate::{
    ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding, ColorFrameResidency, CpuColorFrame,
    CpuColorTransformExecutor, CpuEncodedColorFrame, OcioGpuShaderCache, RenderColorTransform,
    RenderColorTransformError, RenderColorTransformGpuOptions, RenderColorTransformGpuPlan,
    RenderColorTransformGpuPlanner, RenderInputTransform, RenderInputTransformResult,
    RenderOutputTransformResult,
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
        plan: RenderColorTransformGpuPlan,
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
            plan,
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

#[cfg(test)]
mod tests {
    use super::*;
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

    fn assert_stage_chain_is_contiguous(plan: &RenderColorStagePlan) {
        for pair in plan.stages.windows(2) {
            assert_eq!(pair[0].output(), pair[1].input());
        }
        assert_eq!(
            plan.stages.last().expect("stage plan should not be empty").output(),
            plan.final_descriptor
        );
    }
}
