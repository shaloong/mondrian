use crate::{
    ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding, ColorFrameResidency, CpuColorFrame,
    CpuEncodedColorFrame, OcioGpuShaderCache, OcioGpuShaderError, OcioGpuShaderRequest,
    OcioGpuWgpuExecutionPlan,
};
use mondrian_core::{
    convert_rgba8_in_place,
    types::{ColorEngine, ColorSpace},
    ColorPipeline, GpuLanguage, RgbaF32Frame,
};

/// Backend used to execute a render color transform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderColorTransformBackend {
    /// CPU OCIO path via an explicit RGBA8 boundary.
    CpuOcioRgba8Boundary,
    /// Planned OCIO GPU shader path before native wgpu upload/execution.
    OcioGpuShaderPlan,
}

/// Logical transform direction used for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderColorTransformDirection {
    /// Source/import encoded pixels entering timeline working space.
    InputToWorking,
    /// Timeline working pixels leaving to display/export.
    WorkingToOutput,
}

/// Diagnostics emitted by renderer color transform execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderColorTransformDiagnostics {
    /// Execution backend used.
    pub backend: RenderColorTransformBackend,
    /// Logical transform direction.
    pub direction: RenderColorTransformDirection,
    /// Input frame contract.
    pub input: ColorFrameDescriptor,
    /// Output frame contract.
    pub output: ColorFrameDescriptor,
    /// Number of pixels transformed.
    pub pixel_count: usize,
    /// Whether this execution crossed an RGBA8 CPU boundary.
    pub used_rgba8_boundary: bool,
}

/// Detailed result for source/import input transforms.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderInputTransformResult {
    /// Transformed working-space frame.
    pub frame: CpuColorFrame,
    /// Execution diagnostics.
    pub diagnostics: RenderColorTransformDiagnostics,
}

/// Detailed result for display/export output transforms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderOutputTransformResult {
    /// Encoded boundary frame.
    pub frame: CpuEncodedColorFrame,
    /// Execution diagnostics.
    pub diagnostics: RenderColorTransformDiagnostics,
}

/// GPU color-transform planning options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderColorTransformGpuOptions {
    /// OCIO shader language to extract for the renderer backend.
    pub language: GpuLanguage,
    /// Desired residency for the planned output descriptor.
    pub output_residency: ColorFrameResidency,
}

impl Default for RenderColorTransformGpuOptions {
    fn default() -> Self {
        Self {
            language: GpuLanguage::Glsl4_0,
            output_residency: ColorFrameResidency::Gpu,
        }
    }
}

/// Renderer-side GPU planning result for a color-transform boundary.
#[derive(Debug, Clone)]
pub struct RenderColorTransformGpuPlan {
    /// Logical transform direction.
    pub direction: RenderColorTransformDirection,
    /// OCIO shader extraction request used for this plan.
    pub request: OcioGpuShaderRequest,
    /// Prepared native wgpu execution plan and its explicit blockers.
    pub wgpu: OcioGpuWgpuExecutionPlan,
    /// Descriptor-level diagnostics for the planned transform.
    pub diagnostics: RenderColorTransformDiagnostics,
    /// Whether the current source descriptor is CPU-resident and needs an upload node.
    pub requires_source_upload: bool,
    /// Whether the requested output descriptor is CPU-resident and needs a readback node.
    pub requires_output_readback: bool,
}

impl RenderColorTransformGpuPlan {
    /// Whether native wgpu execution can be scheduled without CPU upload/readback nodes.
    pub fn can_execute_in_place_on_gpu(&self) -> bool {
        self.wgpu.can_execute() && !self.requires_source_upload && !self.requires_output_readback
    }
}

/// Color transform requested by a render graph boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderColorTransform {
    /// Destination color space.
    pub output_color_space: ColorSpace,
    /// Destination frame domain.
    pub output_domain: ColorFrameDomain,
    /// Whether HDR/scene data should be tone-mapped for the destination.
    pub tone_map: bool,
    /// Color engine used to execute the transform.
    pub engine: ColorEngine,
    /// Execution backend selected for this transform.
    pub backend: RenderColorTransformBackend,
}

/// Input transform requested by a decode/source boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderInputTransform {
    /// Timeline working color space.
    pub working_color_space: ColorSpace,
    /// Whether input HDR/scene data should be tone-mapped while entering working space.
    pub tone_map: bool,
    /// Color engine used to execute the input transform.
    pub engine: ColorEngine,
    /// Execution backend selected for this transform.
    pub backend: RenderColorTransformBackend,
}

impl RenderInputTransform {
    /// Build a source/import -> timeline working-space transform.
    pub fn to_working(
        working_color_space: ColorSpace,
        tone_map: bool,
        engine: ColorEngine,
    ) -> Self {
        Self {
            working_color_space,
            tone_map,
            engine,
            backend: RenderColorTransformBackend::CpuOcioRgba8Boundary,
        }
    }
}

impl RenderColorTransform {
    /// Build a display/presentation transform.
    pub fn display(output_color_space: ColorSpace, tone_map: bool, engine: ColorEngine) -> Self {
        Self {
            output_color_space,
            output_domain: ColorFrameDomain::Display,
            tone_map,
            engine,
            backend: RenderColorTransformBackend::CpuOcioRgba8Boundary,
        }
    }

    /// Build an export/delivery transform.
    pub fn export(output_color_space: ColorSpace, tone_map: bool, engine: ColorEngine) -> Self {
        Self {
            output_color_space,
            output_domain: ColorFrameDomain::Export,
            tone_map,
            engine,
            backend: RenderColorTransformBackend::CpuOcioRgba8Boundary,
        }
    }
}

/// Execute renderer color transforms for CPU-resident frames.
pub struct CpuColorTransformExecutor;

impl CpuColorTransformExecutor {
    /// Apply a source/import transform and return frame plus execution diagnostics.
    pub fn input_to_working(
        frame: &CpuEncodedColorFrame,
        transform: &RenderInputTransform,
    ) -> Result<RenderInputTransformResult, RenderColorTransformError> {
        let descriptor = frame.descriptor();
        if descriptor.domain != ColorFrameDomain::Source {
            return Err(RenderColorTransformError::UnsupportedInputDomain {
                domain: descriptor.domain,
            });
        }

        let mut rgba = frame.rgba().to_vec();
        convert_rgba8_in_place(
            &mut rgba,
            ColorPipeline::new(
                descriptor.color_space,
                transform.working_color_space,
                transform.working_color_space,
                transform.tone_map,
            )
            .with_engine(transform.engine.clone()),
        )
        .map_err(RenderColorTransformError::ExecutionFailed)?;

        let frame = CpuColorFrame::working(RgbaF32Frame::from_rgba8(
            descriptor.width,
            descriptor.height,
            &rgba,
            transform.working_color_space,
            transform.working_color_space,
            false,
        ));
        let diagnostics = RenderColorTransformDiagnostics {
            backend: transform.backend,
            direction: RenderColorTransformDirection::InputToWorking,
            input: descriptor,
            output: frame.descriptor(),
            pixel_count: descriptor.width as usize * descriptor.height as usize,
            used_rgba8_boundary: true,
        };

        Ok(RenderInputTransformResult { frame, diagnostics })
    }

    /// Apply a render color transform and return frame plus execution diagnostics.
    pub fn transform(
        frame: &CpuColorFrame,
        transform: &RenderColorTransform,
    ) -> Result<RenderOutputTransformResult, RenderColorTransformError> {
        let descriptor = frame.descriptor();
        if descriptor.domain != ColorFrameDomain::Working {
            return Err(RenderColorTransformError::UnsupportedInputDomain {
                domain: descriptor.domain,
            });
        }

        let mut rgba = frame.to_output_rgba8(descriptor.color_space, false);
        convert_rgba8_in_place(
            &mut rgba,
            ColorPipeline::new(
                descriptor.color_space,
                descriptor.color_space,
                transform.output_color_space,
                transform.tone_map,
            )
            .with_engine(transform.engine.clone()),
        )
        .map_err(RenderColorTransformError::ExecutionFailed)?;

        let frame = CpuEncodedColorFrame::rgba8(
            descriptor.width,
            descriptor.height,
            transform.output_color_space,
            transform.output_domain,
            rgba,
        );
        let diagnostics = RenderColorTransformDiagnostics {
            backend: transform.backend,
            direction: RenderColorTransformDirection::WorkingToOutput,
            input: descriptor,
            output: frame.descriptor(),
            pixel_count: descriptor.width as usize * descriptor.height as usize,
            used_rgba8_boundary: true,
        };

        Ok(RenderOutputTransformResult { frame, diagnostics })
    }
}

/// Plans OCIO GPU shader execution for renderer color-transform boundaries.
pub struct RenderColorTransformGpuPlanner<'a> {
    cache: &'a mut OcioGpuShaderCache,
    options: RenderColorTransformGpuOptions,
}

impl<'a> RenderColorTransformGpuPlanner<'a> {
    /// Create a planner backed by the shared renderer OCIO GPU shader cache.
    pub fn new(cache: &'a mut OcioGpuShaderCache, options: RenderColorTransformGpuOptions) -> Self {
        Self { cache, options }
    }

    /// Plan source/import -> timeline working-space GPU execution.
    pub fn plan_input_to_working(
        &mut self,
        input: ColorFrameDescriptor,
        transform: &RenderInputTransform,
    ) -> Result<RenderColorTransformGpuPlan, RenderColorTransformError> {
        if input.domain != ColorFrameDomain::Source {
            return Err(RenderColorTransformError::UnsupportedInputDomain { domain: input.domain });
        }

        let output = ColorFrameDescriptor {
            width: input.width,
            height: input.height,
            color_space: transform.working_color_space,
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: self.options.output_residency,
        };
        let request = OcioGpuShaderRequest::ColorSpace {
            src: input.color_space,
            dst: transform.working_color_space,
            language: self.options.language,
        };
        self.plan(
            RenderColorTransformDirection::InputToWorking,
            input,
            output,
            request,
        )
    }

    /// Plan timeline working-space -> display/export GPU execution.
    pub fn plan_output_transform(
        &mut self,
        input: ColorFrameDescriptor,
        transform: &RenderColorTransform,
    ) -> Result<RenderColorTransformGpuPlan, RenderColorTransformError> {
        if input.domain != ColorFrameDomain::Working {
            return Err(RenderColorTransformError::UnsupportedInputDomain { domain: input.domain });
        }

        let output = ColorFrameDescriptor {
            width: input.width,
            height: input.height,
            color_space: transform.output_color_space,
            domain: transform.output_domain,
            encoding: ColorFrameEncoding::EncodedRgba8,
            residency: self.options.output_residency,
        };
        let request = OcioGpuShaderRequest::ColorSpace {
            src: input.color_space,
            dst: transform.output_color_space,
            language: self.options.language,
        };
        self.plan(
            RenderColorTransformDirection::WorkingToOutput,
            input,
            output,
            request,
        )
    }

    fn plan(
        &mut self,
        direction: RenderColorTransformDirection,
        input: ColorFrameDescriptor,
        output: ColorFrameDescriptor,
        request: OcioGpuShaderRequest,
    ) -> Result<RenderColorTransformGpuPlan, RenderColorTransformError> {
        let wgpu = self.cache.prepare_wgpu_execution(request.clone())?;
        let diagnostics = RenderColorTransformDiagnostics {
            backend: RenderColorTransformBackend::OcioGpuShaderPlan,
            direction,
            input,
            output,
            pixel_count: input.pixel_count(),
            used_rgba8_boundary: false,
        };
        Ok(RenderColorTransformGpuPlan {
            direction,
            request,
            wgpu,
            diagnostics,
            requires_source_upload: input.residency == ColorFrameResidency::Cpu,
            requires_output_readback: output.residency == ColorFrameResidency::Cpu,
        })
    }
}

/// Error returned by render color transform execution.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RenderColorTransformError {
    /// The executor was asked to transform a frame from an unsupported domain.
    #[error("unsupported render color transform input domain: {domain:?}")]
    UnsupportedInputDomain {
        /// Input frame domain.
        domain: ColorFrameDomain,
    },
    /// The selected color engine failed.
    #[error("render color transform failed: {0}")]
    ExecutionFailed(String),
    /// GPU shader planning failed before native execution could be scheduled.
    #[error("render GPU color transform planning failed: {0}")]
    GpuPlanningFailed(#[from] OcioGpuShaderError),
    /// GPU planning was requested without an OCIO GPU shader cache.
    #[error("render GPU color transform planning requested without a GPU planner")]
    GpuPlannerUnavailable,
    /// A color stage plan cannot be executed by the selected executor.
    #[error("render color stage plan cannot execute on this executor: {reason}")]
    UnsupportedStagePlan {
        /// Diagnostic reason.
        reason: &'static str,
    },
    /// A color stage descriptor did not match the executor input/output.
    #[error("render color stage descriptor mismatch: expected {expected:?}, actual {actual:?}")]
    StageDescriptorMismatch {
        /// Expected descriptor.
        expected: ColorFrameDescriptor,
        /// Actual descriptor.
        actual: ColorFrameDescriptor,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{ensure_mondrian_default_ocio_loaded, RgbaF32Frame};

    #[test]
    fn cpu_transform_returns_typed_display_boundary_frame() {
        let source = CpuColorFrame::working(RgbaF32Frame {
            width: 1,
            height: 1,
            data: vec![[0.5, 0.25, 0.125, 1.0]],
            color_space: ColorSpace::Rec709,
        });
        let transform =
            RenderColorTransform::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);

        let output =
            CpuColorTransformExecutor::transform(&source, &transform).expect("display transform");

        let descriptor = output.frame.descriptor();
        assert_eq!(descriptor.domain, ColorFrameDomain::Display);
        assert_eq!(descriptor.color_space, ColorSpace::Srgb);
        assert_eq!(descriptor.encoding, crate::ColorFrameEncoding::EncodedRgba8);
        assert_eq!(output.frame.rgba().len(), 4);
        assert_eq!(
            output.diagnostics.direction,
            RenderColorTransformDirection::WorkingToOutput
        );
        assert_eq!(output.diagnostics.input.domain, ColorFrameDomain::Working);
        assert_eq!(output.diagnostics.output.domain, ColorFrameDomain::Display);
        assert_eq!(output.diagnostics.pixel_count, 1);
        assert!(output.diagnostics.used_rgba8_boundary);
    }

    #[test]
    fn input_transform_returns_typed_working_frame() {
        let source =
            CpuEncodedColorFrame::source_rgba8(1, 1, ColorSpace::Rec709, vec![128, 64, 32, 255]);
        let transform =
            RenderInputTransform::to_working(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);

        let working = CpuColorTransformExecutor::input_to_working(&source, &transform)
            .expect("input transform");

        let descriptor = working.frame.descriptor();
        assert_eq!(descriptor.domain, ColorFrameDomain::Working);
        assert_eq!(descriptor.color_space, ColorSpace::Srgb);
        assert_eq!(descriptor.encoding, crate::ColorFrameEncoding::LinearFloat);
        assert_eq!(working.frame.rgba_f32().data.len(), 1);
        assert_eq!(
            working.diagnostics.direction,
            RenderColorTransformDirection::InputToWorking
        );
        assert_eq!(working.diagnostics.input.domain, ColorFrameDomain::Source);
        assert_eq!(working.diagnostics.output.domain, ColorFrameDomain::Working);
        assert_eq!(working.diagnostics.pixel_count, 1);
        assert!(working.diagnostics.used_rgba8_boundary);
    }

    #[test]
    fn gpu_planner_builds_input_shader_plan_with_explicit_boundaries() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let source =
            CpuEncodedColorFrame::source_rgba8(2, 3, ColorSpace::SLog3, vec![128; 2 * 3 * 4]);
        let transform =
            RenderInputTransform::to_working(ColorSpace::Rec709, false, ColorEngine::MondrianSmart);
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderColorTransformGpuPlanner::new(
            &mut cache,
            RenderColorTransformGpuOptions::default(),
        );

        let plan = planner
            .plan_input_to_working(source.descriptor(), &transform)
            .expect("input GPU plan");

        assert_eq!(
            plan.direction,
            RenderColorTransformDirection::InputToWorking
        );
        assert_eq!(
            plan.diagnostics.backend,
            RenderColorTransformBackend::OcioGpuShaderPlan
        );
        assert_eq!(plan.diagnostics.input.domain, ColorFrameDomain::Source);
        assert_eq!(plan.diagnostics.output.domain, ColorFrameDomain::Working);
        assert_eq!(plan.diagnostics.output.residency, ColorFrameResidency::Gpu);
        assert_eq!(plan.diagnostics.pixel_count, 6);
        assert!(!plan.diagnostics.used_rgba8_boundary);
        assert!(plan.requires_source_upload);
        assert!(!plan.requires_output_readback);
        assert!(!plan.can_execute_in_place_on_gpu());
        assert!(plan.wgpu.blockers.is_empty());
        assert!(plan.wgpu.can_execute());
    }

    #[test]
    fn gpu_planner_builds_output_shader_plan_from_working_descriptor() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let source = CpuColorFrame::working(RgbaF32Frame {
            width: 4,
            height: 5,
            data: vec![[0.5, 0.25, 0.125, 1.0]; 20],
            color_space: ColorSpace::Rec709,
        });
        let transform =
            RenderColorTransform::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderColorTransformGpuPlanner::new(
            &mut cache,
            RenderColorTransformGpuOptions::default(),
        );

        let plan = planner
            .plan_output_transform(source.descriptor(), &transform)
            .expect("output GPU plan");

        assert_eq!(
            plan.direction,
            RenderColorTransformDirection::WorkingToOutput
        );
        assert_eq!(plan.diagnostics.input.domain, ColorFrameDomain::Working);
        assert_eq!(plan.diagnostics.output.domain, ColorFrameDomain::Display);
        assert_eq!(plan.diagnostics.output.color_space, ColorSpace::Srgb);
        assert_eq!(plan.diagnostics.output.residency, ColorFrameResidency::Gpu);
        assert_eq!(plan.diagnostics.pixel_count, 20);
        assert!(plan.requires_source_upload);
        assert!(!plan.requires_output_readback);
    }

    #[test]
    fn gpu_planner_can_request_cpu_output_readback_boundary() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let source = CpuColorFrame::working(RgbaF32Frame {
            width: 2,
            height: 2,
            data: vec![[0.5, 0.25, 0.125, 1.0]; 4],
            color_space: ColorSpace::Rec709,
        });
        let transform =
            RenderColorTransform::export(ColorSpace::Rec709, false, ColorEngine::MondrianSmart);
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderColorTransformGpuPlanner::new(
            &mut cache,
            RenderColorTransformGpuOptions {
                output_residency: ColorFrameResidency::Cpu,
                ..RenderColorTransformGpuOptions::default()
            },
        );

        let plan = planner
            .plan_output_transform(source.descriptor(), &transform)
            .expect("output GPU plan");

        assert_eq!(plan.diagnostics.output.residency, ColorFrameResidency::Cpu);
        assert!(plan.requires_output_readback);
    }
}
