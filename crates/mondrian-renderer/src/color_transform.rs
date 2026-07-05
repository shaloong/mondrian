use crate::{
    ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding, ColorFrameResidency, CpuColorFrame,
    CpuEncodedColorFrame, LinearFloatSource, OcioGpuShaderCache, OcioGpuShaderError,
    OcioGpuShaderRequest, OcioGpuWgpuExecutionPlan, OcioGpuWgpuWrapperColorContract,
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
    /// CPU OCIO path operating directly on f32 data without u8 quantization.
    CpuOcioFloat,
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

/// Result of a float output transform that bypasses RGBA8 quantization.
pub struct RenderOutputTransformFloatResult {
    /// Linear float display/export frame with output transform applied.
    pub frame: CpuColorFrame,
    /// Execution diagnostics.
    pub diagnostics: RenderColorTransformDiagnostics,
}

/// OCIO display/view pair used for viewer/display output transforms.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderOcioDisplayView {
    /// OCIO display name.
    pub display: String,
    /// OCIO view name under the display.
    pub view: String,
}

impl RenderOcioDisplayView {
    /// Build an explicit OCIO display/view pair.
    pub fn new(display: impl Into<String>, view: impl Into<String>) -> Self {
        Self { display: display.into(), view: view.into() }
    }
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
    /// OCIO display/view pair for presentation transforms.
    pub display_view: Option<RenderOcioDisplayView>,
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
            display_view: None,
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
            display_view: None,
            tone_map,
            engine,
            backend: RenderColorTransformBackend::CpuOcioRgba8Boundary,
        }
    }

    /// Build a display transform through an explicit OCIO display/view pair.
    pub fn display_view(
        output_color_space: ColorSpace,
        display: impl Into<String>,
        view: impl Into<String>,
        tone_map: bool,
        engine: ColorEngine,
    ) -> Self {
        Self {
            output_color_space,
            output_domain: ColorFrameDomain::Display,
            display_view: Some(RenderOcioDisplayView::new(display, view)),
            tone_map,
            engine,
            backend: RenderColorTransformBackend::CpuOcioRgba8Boundary,
        }
    }
}

/// Execute renderer color transforms for CPU-resident frames.
pub struct CpuColorTransformExecutor;

impl CpuColorTransformExecutor {
    /// Apply a source/import transform from a linear float source and return
    /// frame plus execution diagnostics. This bypasses the RGBA8 quantization
    /// path entirely.
    pub fn input_to_working_float(
        frame: &LinearFloatSource,
        transform: &RenderInputTransform,
    ) -> Result<RenderInputTransformResult, RenderColorTransformError> {
        let descriptor = frame.descriptor();
        if descriptor.domain != ColorFrameDomain::Source {
            return Err(RenderColorTransformError::UnsupportedInputDomain {
                domain: descriptor.domain,
            });
        }
        if descriptor.encoding != ColorFrameEncoding::LinearFloat {
            return Err(RenderColorTransformError::ExecutionFailed {
                direction: RenderColorTransformDirection::InputToWorking,
                input: descriptor,
                output: ColorFrameDescriptor {
                    width: descriptor.width,
                    height: descriptor.height,
                    color_space: transform.working_color_space,
                    domain: ColorFrameDomain::Working,
                    encoding: ColorFrameEncoding::LinearFloat,
                    residency: ColorFrameResidency::Cpu,
                },
                reason: "LinearFloatSource must have LinearFloat encoding".to_string(),
            });
        }

        let output_descriptor = ColorFrameDescriptor {
            width: descriptor.width,
            height: descriptor.height,
            color_space: transform.working_color_space,
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Cpu,
        };

        let mut data = frame.data().to_vec();
        transform
            .engine
            .convert_pipeline_float(
                &mut data,
                descriptor.color_space,
                transform.working_color_space,
                transform.working_color_space,
                false,
            )
            .map_err(|reason| RenderColorTransformError::ExecutionFailed {
                direction: RenderColorTransformDirection::InputToWorking,
                input: descriptor,
                output: output_descriptor,
                reason,
            })?;

        // Re-pack flat f32 into Vec<[f32; 4]> for RgbaF32Frame.
        let pixels: Vec<[f32; 4]> =
            data.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
        let frame = CpuColorFrame::working(RgbaF32Frame {
            width: descriptor.width,
            height: descriptor.height,
            data: pixels,
            color_space: transform.working_color_space,
        });
        let diagnostics = RenderColorTransformDiagnostics {
            backend: transform.backend,
            direction: RenderColorTransformDirection::InputToWorking,
            input: descriptor,
            output: output_descriptor,
            pixel_count: descriptor.width as usize * descriptor.height as usize,
            used_rgba8_boundary: false,
        };

        Ok(RenderInputTransformResult { frame, diagnostics })
    }

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
        let output_descriptor = ColorFrameDescriptor {
            width: descriptor.width,
            height: descriptor.height,
            color_space: transform.working_color_space,
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Cpu,
        };

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
        .map_err(|reason| RenderColorTransformError::ExecutionFailed {
            direction: RenderColorTransformDirection::InputToWorking,
            input: descriptor,
            output: output_descriptor,
            reason,
        })?;

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
            output: output_descriptor,
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
        let output_descriptor = ColorFrameDescriptor {
            width: descriptor.width,
            height: descriptor.height,
            color_space: transform.output_color_space,
            domain: transform.output_domain,
            encoding: ColorFrameEncoding::EncodedRgba8,
            residency: ColorFrameResidency::Cpu,
        };

        let mut rgba = frame.to_output_rgba8(descriptor.color_space, false);
        if let Some(display_view) = &transform.display_view {
            transform
                .engine
                .display_transform(
                    &mut rgba,
                    descriptor.color_space,
                    &display_view.display,
                    &display_view.view,
                )
                .map_err(|reason| RenderColorTransformError::ExecutionFailed {
                    direction: RenderColorTransformDirection::WorkingToOutput,
                    input: descriptor,
                    output: output_descriptor,
                    reason,
                })?;
        } else {
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
            .map_err(|reason| RenderColorTransformError::ExecutionFailed {
                direction: RenderColorTransformDirection::WorkingToOutput,
                input: descriptor,
                output: output_descriptor,
                reason,
            })?;
        }

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
            output: output_descriptor,
            pixel_count: descriptor.width as usize * descriptor.height as usize,
            used_rgba8_boundary: true,
        };

        Ok(RenderOutputTransformResult { frame, diagnostics })
    }

    /// Apply a working -> output transform on f32 data without u8
    /// quantization. Returns a working-space `CpuColorFrame` with the output
    /// transform applied in float.
    ///
    /// This is the precision-preserving alternative to [`Self::transform`].
    /// The caller can then use the frame for GPU upload or further processing
    /// without an intermediate u8 round-trip.
    pub fn transform_float(
        frame: &CpuColorFrame,
        transform: &RenderColorTransform,
    ) -> Result<RenderOutputTransformFloatResult, RenderColorTransformError> {
        let descriptor = frame.descriptor();
        if descriptor.domain != ColorFrameDomain::Working {
            return Err(RenderColorTransformError::UnsupportedInputDomain {
                domain: descriptor.domain,
            });
        }

        let output_descriptor = ColorFrameDescriptor {
            width: descriptor.width,
            height: descriptor.height,
            color_space: transform.output_color_space,
            domain: transform.output_domain,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Cpu,
        };

        let data: Vec<[f32; 4]> = frame.rgba_f32().data.clone();
        // Flatten to contiguous f32 for OCIO processing.
        let mut flat: Vec<f32> = data.iter().flat_map(|p| p.iter().copied()).collect();
        if let Some(display_view) = &transform.display_view {
            transform
                .engine
                .display_transform_float(
                    &mut flat,
                    descriptor.color_space,
                    &display_view.display,
                    &display_view.view,
                )
                .map_err(|reason| RenderColorTransformError::ExecutionFailed {
                    direction: RenderColorTransformDirection::WorkingToOutput,
                    input: descriptor,
                    output: output_descriptor,
                    reason,
                })?;
        } else {
            transform
                .engine
                .convert_pipeline_float(
                    &mut flat,
                    descriptor.color_space,
                    descriptor.color_space,
                    transform.output_color_space,
                    false,
                )
                .map_err(|reason| RenderColorTransformError::ExecutionFailed {
                    direction: RenderColorTransformDirection::WorkingToOutput,
                    input: descriptor,
                    output: output_descriptor,
                    reason,
                })?;
        }

        // Re-pack flat f32 into Vec<[f32; 4]>.
        let pixels: Vec<[f32; 4]> =
            flat.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
        let out_frame = CpuColorFrame::linear(
            RgbaF32Frame {
                width: descriptor.width,
                height: descriptor.height,
                data: pixels,
                color_space: transform.output_color_space,
            },
            transform.output_domain,
        );
        let diagnostics = RenderColorTransformDiagnostics {
            backend: RenderColorTransformBackend::CpuOcioFloat,
            direction: RenderColorTransformDirection::WorkingToOutput,
            input: descriptor,
            output: output_descriptor,
            pixel_count: descriptor.width as usize * descriptor.height as usize,
            used_rgba8_boundary: false,
        };

        Ok(RenderOutputTransformFloatResult { frame: out_frame, diagnostics })
    }
}
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
        let mut plan = self.plan(
            RenderColorTransformDirection::InputToWorking,
            input,
            output,
            request,
        )?;
        if output.encoding == ColorFrameEncoding::LinearFloat {
            plan.wgpu.wrapper_color =
                OcioGpuWgpuWrapperColorContract::encoded_input_to_linear_working(
                    transform.working_color_space,
                );
        }
        Ok(plan)
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
        let request = if let Some(display_view) = &transform.display_view {
            OcioGpuShaderRequest::DisplayView {
                src: input.color_space,
                display: display_view.display.clone(),
                view: display_view.view.clone(),
                language: self.options.language,
            }
        } else {
            OcioGpuShaderRequest::ColorSpace {
                src: input.color_space,
                dst: transform.output_color_space,
                language: self.options.language,
            }
        };
        let mut plan = self.plan(
            RenderColorTransformDirection::WorkingToOutput,
            input,
            output,
            request,
        )?;
        if input.encoding == ColorFrameEncoding::LinearFloat {
            plan.wgpu.wrapper_color =
                OcioGpuWgpuWrapperColorContract::linear_working_to_encoded_output(
                    input.color_space,
                );
        }
        Ok(plan)
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
    #[error("render color transform failed ({direction:?}, {input:?} -> {output:?}): {reason}")]
    ExecutionFailed {
        /// Logical transform direction.
        direction: RenderColorTransformDirection,
        /// Input frame descriptor.
        input: ColorFrameDescriptor,
        /// Intended output frame descriptor.
        output: ColorFrameDescriptor,
        /// Backend diagnostic reason.
        reason: String,
    },
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
    use mondrian_core::types::OcioConfigSource;
    use mondrian_core::{
        ensure_mondrian_default_ocio_loaded, ocio_default_display_view, RgbaF32Frame,
    };

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
    fn cpu_transform_executes_explicit_display_view_boundary() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let (display, view) = ocio_default_display_view().expect("default display/view");
        let source = CpuColorFrame::working(RgbaF32Frame {
            width: 1,
            height: 1,
            data: vec![[0.25, 0.5, 0.75, 1.0]],
            color_space: ColorSpace::Rec709,
        });
        let transform = RenderColorTransform::display_view(
            ColorSpace::Srgb,
            display,
            view,
            false,
            ColorEngine::MondrianSmart,
        );

        let output = CpuColorTransformExecutor::transform(&source, &transform)
            .expect("display/view transform");

        assert_eq!(output.frame.descriptor().domain, ColorFrameDomain::Display);
        assert_eq!(output.frame.descriptor().color_space, ColorSpace::Srgb);
        assert_eq!(output.frame.rgba().len(), 4);
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
    fn input_transform_failure_carries_boundary_descriptors() {
        let source = CpuEncodedColorFrame::source_rgba8(2, 1, ColorSpace::SLog3, vec![128; 2 * 4]);
        let missing_path = std::env::temp_dir().join(format!(
            "mondrian-missing-input-ocio-{}.ocio",
            std::process::id()
        ));
        let transform = RenderInputTransform::to_working(
            ColorSpace::Rec709,
            false,
            ColorEngine::Ocio {
                source: OcioConfigSource::Path { path: missing_path },
            },
        );

        let err = CpuColorTransformExecutor::input_to_working(&source, &transform)
            .expect_err("missing explicit OCIO source must fail");

        match err {
            RenderColorTransformError::ExecutionFailed { direction, input, output, reason } => {
                assert_eq!(direction, RenderColorTransformDirection::InputToWorking);
                assert_eq!(input, source.descriptor());
                assert_eq!(output.domain, ColorFrameDomain::Working);
                assert_eq!(output.color_space, ColorSpace::Rec709);
                assert_eq!(output.pixel_count(), 2);
                assert!(reason.contains("OCIO config file not found"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn output_transform_failure_carries_boundary_descriptors() {
        let source = CpuColorFrame::working(RgbaF32Frame {
            width: 2,
            height: 2,
            data: vec![[0.5, 0.25, 0.125, 1.0]; 4],
            color_space: ColorSpace::Rec709,
        });
        let missing_path = std::env::temp_dir().join(format!(
            "mondrian-missing-output-ocio-{}.ocio",
            std::process::id()
        ));
        let transform = RenderColorTransform::export(
            ColorSpace::Srgb,
            false,
            ColorEngine::Ocio {
                source: OcioConfigSource::Path { path: missing_path },
            },
        );

        let err = CpuColorTransformExecutor::transform(&source, &transform)
            .expect_err("missing explicit OCIO source must fail");

        match err {
            RenderColorTransformError::ExecutionFailed { direction, input, output, reason } => {
                assert_eq!(direction, RenderColorTransformDirection::WorkingToOutput);
                assert_eq!(input, source.descriptor());
                assert_eq!(output.domain, ColorFrameDomain::Export);
                assert_eq!(output.color_space, ColorSpace::Srgb);
                assert_eq!(output.encoding, ColorFrameEncoding::EncodedRgba8);
                assert_eq!(output.pixel_count(), 4);
                assert!(reason.contains("OCIO config file not found"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
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
        assert_eq!(
            plan.wgpu.wrapper_color,
            OcioGpuWgpuWrapperColorContract::encoded_input_to_linear_working(ColorSpace::Rec709)
        );
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
        assert_eq!(
            plan.wgpu.wrapper_color,
            OcioGpuWgpuWrapperColorContract::linear_working_to_encoded_output(ColorSpace::Rec709)
        );
        assert!(plan.wgpu.blockers.is_empty());
        assert!(plan.wgpu.can_execute());
    }

    #[test]
    fn gpu_planner_builds_display_view_shader_plan_from_working_descriptor() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let (display, view) = ocio_default_display_view().expect("default display/view");
        let source = CpuColorFrame::working(RgbaF32Frame {
            width: 4,
            height: 5,
            data: vec![[0.5, 0.25, 0.125, 1.0]; 20],
            color_space: ColorSpace::Rec709,
        });
        let transform = RenderColorTransform::display_view(
            ColorSpace::Srgb,
            display.clone(),
            view.clone(),
            false,
            ColorEngine::MondrianSmart,
        );
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderColorTransformGpuPlanner::new(
            &mut cache,
            RenderColorTransformGpuOptions::default(),
        );

        let plan = planner
            .plan_output_transform(source.descriptor(), &transform)
            .expect("display/view GPU plan");

        assert_eq!(
            plan.request,
            OcioGpuShaderRequest::DisplayView {
                src: ColorSpace::Rec709,
                display: display.clone(),
                view: view.clone(),
                language: GpuLanguage::Glsl4_0,
            }
        );
        assert_eq!(
            plan.wgpu.wrapper_color,
            OcioGpuWgpuWrapperColorContract::linear_working_to_encoded_output(ColorSpace::Rec709)
        );
        assert!(plan.wgpu.blockers.is_empty());
        assert!(plan.wgpu.can_execute());
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
        assert_eq!(
            plan.wgpu.wrapper_color,
            OcioGpuWgpuWrapperColorContract::linear_working_to_encoded_output(ColorSpace::Rec709)
        );
        assert!(plan.wgpu.blockers.is_empty());
        assert!(plan.wgpu.can_execute());
    }

    #[test]
    fn float_input_transform_bypasses_rgba8_boundary() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let source = LinearFloatSource::new(
            2,
            2,
            ColorSpace::Rec709,
            vec![
                0.5, 0.25, 0.125, 1.0, 0.8, 0.6, 0.4, 1.0, 0.2, 0.4, 0.6, 1.0, 1.0, 0.5, 0.0, 1.0,
            ],
        );
        let transform =
            RenderInputTransform::to_working(ColorSpace::Rec709, false, ColorEngine::MondrianSmart);

        let result = CpuColorTransformExecutor::input_to_working_float(&source, &transform)
            .expect("float input transform");

        assert_eq!(result.frame.descriptor().domain, ColorFrameDomain::Working);
        assert_eq!(
            result.frame.descriptor().encoding,
            ColorFrameEncoding::LinearFloat
        );
        assert_eq!(result.diagnostics.pixel_count, 4);
        assert!(!result.diagnostics.used_rgba8_boundary);
    }

    #[test]
    fn float_output_transform_produces_float_frame() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let source = CpuColorFrame::working(RgbaF32Frame {
            width: 2,
            height: 2,
            data: vec![
                [0.5, 0.25, 0.125, 1.0],
                [0.8, 0.6, 0.4, 1.0],
                [0.2, 0.4, 0.6, 1.0],
                [1.0, 0.5, 0.0, 1.0],
            ],
            color_space: ColorSpace::Rec709,
        });
        let transform =
            RenderColorTransform::export(ColorSpace::Rec709, false, ColorEngine::MondrianSmart);

        let result = CpuColorTransformExecutor::transform_float(&source, &transform)
            .expect("float output transform");

        // Float output keeps the output graph domain while avoiding u8 quantization.
        assert_eq!(result.frame.descriptor().domain, ColorFrameDomain::Export);
        assert_eq!(
            result.frame.descriptor().encoding,
            ColorFrameEncoding::LinearFloat
        );
        assert_eq!(result.diagnostics.output.domain, ColorFrameDomain::Export);
        assert_eq!(result.diagnostics.pixel_count, 4);
        assert!(!result.diagnostics.used_rgba8_boundary);
        assert_eq!(
            result.diagnostics.backend,
            RenderColorTransformBackend::CpuOcioFloat
        );
    }

    #[test]
    fn float_pipeline_avoids_transfer_function_round_trip() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        // Create a float source with linear values.
        let linear_data = vec![0.5, 0.25, 0.125, 1.0];
        let float_source = LinearFloatSource::new(1, 1, ColorSpace::Rec709, linear_data);

        let transform =
            RenderInputTransform::to_working(ColorSpace::Rec709, false, ColorEngine::MondrianSmart);

        let result = CpuColorTransformExecutor::input_to_working_float(&float_source, &transform)
            .expect("float input");

        // Verify the float path produces a valid working frame without RGBA8 boundary.
        assert_eq!(
            result.frame.descriptor().encoding,
            ColorFrameEncoding::LinearFloat
        );
        assert!(!result.diagnostics.used_rgba8_boundary);
        assert_eq!(result.diagnostics.pixel_count, 1);
        // The output should be in working space with reasonable values.
        let output_data = &result.frame.rgba_f32().data[0];
        assert!(output_data[0] > 0.0 && output_data[0] < 1.0);
        assert!(output_data[3] > 0.9); // Alpha should be preserved
    }
}
