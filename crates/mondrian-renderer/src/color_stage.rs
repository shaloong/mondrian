use crate::{
    ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding, ColorFrameResidency,
    OcioGpuShaderCache, RenderColorTransform, RenderColorTransformError,
    RenderColorTransformGpuOptions, RenderColorTransformGpuPlan, RenderColorTransformGpuPlanner,
    RenderInputTransform,
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
}

/// Plans renderer color transform stages without executing them.
pub struct RenderColorStagePlanner<'a> {
    mode: RenderColorStageMode,
    gpu_cache: Option<&'a mut OcioGpuShaderCache>,
    gpu_options: RenderColorTransformGpuOptions,
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

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::ensure_mondrian_default_ocio_loaded;
    use mondrian_core::types::{ColorEngine, ColorSpace};

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
