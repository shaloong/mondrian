use crate::EncodedRgbaF32Frame;
use crate::{
    ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding, ColorFrameResidency,
    ColorFrameSpace, CpuColorFrame, CpuEncodedColorFrame, CpuEncodedFloatColorFrame,
    LinearFloatSource, OcioGpuShaderCache, OcioGpuShaderError, OcioGpuShaderRequest,
    OcioGpuWgpuExecutionPlan,
};
use mondrian_core::{
    types::{ColorEngine, ColorSpace},
    GpuLanguage, OcioColorSpaceIdentity, WorkingColorSpace, WorkingRgbaF32Frame,
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
    /// GPU-resident color-managed intermediate used inside the render graph.
    Intermediate,
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
    /// Encoded float display/export frame with output transform applied.
    pub frame: CpuEncodedFloatColorFrame,
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

/// Identity-to-identity transform used for a color-managed render-graph intermediate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderIntermediateColorTransform {
    /// Exact OCIO destination identity.
    pub output_identity: OcioColorSpaceIdentity,
    /// Semantic role carried by the intermediate frame.
    pub output_domain: ColorFrameDomain,
    /// Floating-point encoding produced by the transform.
    pub output_encoding: ColorFrameEncoding,
    /// Color engine used to resolve and execute the OCIO processor.
    pub engine: ColorEngine,
}

/// Preview-only colorimetric adaptation from Program Output to the local monitor.
///
/// This stage never contains a rendering/view transform. It may only convert
/// between display-referred identities in the same SDR or HDR dynamic-range
/// class, ensuring monitor selection cannot change the program rendering.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderMonitorAdaptation {
    program_output_color_space: ColorSpace,
    monitor_color_space: ColorSpace,
    engine: ColorEngine,
}

impl RenderMonitorAdaptation {
    /// Build a fail-closed Program Output to monitor colorimetric adaptation.
    pub fn new(
        program_output_color_space: ColorSpace,
        monitor_color_space: ColorSpace,
        engine: ColorEngine,
    ) -> Result<Self, RenderMonitorAdaptationError> {
        if !program_output_color_space.is_display_referred() {
            return Err(RenderMonitorAdaptationError::UnsupportedProgramOutput {
                color_space: program_output_color_space,
            });
        }
        if !monitor_color_space.is_display_referred() {
            return Err(RenderMonitorAdaptationError::UnsupportedMonitorOutput {
                color_space: monitor_color_space,
            });
        }
        if program_output_color_space.is_hdr() != monitor_color_space.is_hdr() {
            return Err(RenderMonitorAdaptationError::DynamicRangeClassMismatch {
                program_output: program_output_color_space,
                monitor_output: monitor_color_space,
            });
        }
        Ok(Self {
            program_output_color_space,
            monitor_color_space,
            engine,
        })
    }

    /// Program Output identity expected at the adaptation input.
    pub const fn program_output_color_space(&self) -> ColorSpace {
        self.program_output_color_space
    }

    /// Local monitor identity produced by the adaptation.
    pub const fn monitor_color_space(&self) -> ColorSpace {
        self.monitor_color_space
    }

    /// Whether the monitor differs from Program Output and needs one OCIO pass.
    pub fn requires_pass(&self) -> bool {
        self.program_output_color_space != self.monitor_color_space
    }

    /// Build the stock-OCIO identity transform for a required adaptation pass.
    ///
    /// `None` is an intentional zero-pass route when both identities match.
    pub fn gpu_transform(&self) -> Option<RenderIntermediateColorTransform> {
        self.requires_pass().then(|| RenderIntermediateColorTransform {
            output_identity: OcioColorSpaceIdentity::Color(self.monitor_color_space),
            output_domain: ColorFrameDomain::Display,
            output_encoding: ColorFrameEncoding::EncodedFloat,
            engine: self.engine.clone(),
        })
    }
}

/// Invalid monitor-adaptation contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RenderMonitorAdaptationError {
    /// Program Output must already be display-referred.
    #[error("Program Output {color_space:?} is not display-referred")]
    UnsupportedProgramOutput {
        /// Rejected Program Output identity.
        color_space: ColorSpace,
    },
    /// A monitor target must be display-referred.
    #[error("monitor output {color_space:?} is not display-referred")]
    UnsupportedMonitorOutput {
        /// Rejected monitor identity.
        color_space: ColorSpace,
    },
    /// Tone/dynamic-range mapping belongs in an explicit rendering policy.
    #[error(
        "monitor adaptation cannot change dynamic-range class from {program_output:?} to {monitor_output:?}"
    )]
    DynamicRangeClassMismatch {
        /// Program Output identity.
        program_output: ColorSpace,
        /// Requested monitor identity.
        monitor_output: ColorSpace,
    },
}

/// Paired OCIO GPU transforms surrounding one effect processing domain.
#[derive(Debug, Clone)]
pub struct RenderEffectColorDomainGpuPlan {
    /// Working-to-processing transform, absent for scene-linear effects.
    pub to_processing: Option<RenderColorTransformGpuPlan>,
    /// Processing-to-working transform, absent for scene-linear effects.
    pub to_working: Option<RenderColorTransformGpuPlan>,
}

/// Planner that resolves effect-domain semantics into renderer-owned OCIO passes.
pub struct RenderEffectColorDomainGpuPlanner<'a> {
    color: RenderColorTransformGpuPlanner<'a>,
}

impl<'a> RenderEffectColorDomainGpuPlanner<'a> {
    /// Create an effect-domain planner over the shared OCIO shader cache.
    pub fn new(cache: &'a mut OcioGpuShaderCache, options: RenderColorTransformGpuOptions) -> Self {
        Self {
            color: RenderColorTransformGpuPlanner::new(cache, options),
        }
    }

    /// Plan a no-transfer GPU round trip around an effect's declared domain.
    pub fn plan(
        &mut self,
        input: ColorFrameDescriptor,
        domain: mondrian_effects::EffectColorDomain,
        engine: ColorEngine,
    ) -> Result<RenderEffectColorDomainGpuPlan, RenderEffectColorDomainGpuPlanError> {
        let working = match (
            input.domain,
            input.color_space,
            input.encoding,
            input.residency,
        ) {
            (
                ColorFrameDomain::Working,
                ColorFrameSpace::Working(working),
                ColorFrameEncoding::LinearFloat,
                ColorFrameResidency::Gpu,
            ) => working,
            _ => {
                return Err(RenderEffectColorDomainGpuPlanError::InvalidWorkingInput {
                    descriptor: input,
                });
            }
        };
        let (output_identity, output_encoding) = match domain {
            mondrian_effects::EffectColorDomain::SceneLinearRgb => {
                return Ok(RenderEffectColorDomainGpuPlan {
                    to_processing: None,
                    to_working: None,
                });
            }
            mondrian_effects::EffectColorDomain::LogPerceptualRgb { color_space }
            | mondrian_effects::EffectColorDomain::DisplayEncodedRgb { color_space } => (
                OcioColorSpaceIdentity::Color(color_space),
                ColorFrameEncoding::EncodedFloat,
            ),
            mondrian_effects::EffectColorDomain::DisplayLinearRgb { color_space } => (
                OcioColorSpaceIdentity::Color(color_space),
                ColorFrameEncoding::LinearFloat,
            ),
            mondrian_effects::EffectColorDomain::Data
            | mondrian_effects::EffectColorDomain::AlphaMask => {
                return Err(RenderEffectColorDomainGpuPlanError::NonRgbDomain { domain });
            }
        };
        let to_processing = self.color.plan_identity_transform(
            input,
            &RenderIntermediateColorTransform {
                output_identity,
                output_domain: ColorFrameDomain::Effect,
                output_encoding,
                engine: engine.clone(),
            },
        )?;
        let to_working = self.color.plan_identity_transform(
            to_processing.diagnostics.output,
            &RenderIntermediateColorTransform {
                output_identity: OcioColorSpaceIdentity::Working(working),
                output_domain: ColorFrameDomain::Working,
                output_encoding: ColorFrameEncoding::LinearFloat,
                engine,
            },
        )?;
        Ok(RenderEffectColorDomainGpuPlan {
            to_processing: Some(to_processing),
            to_working: Some(to_working),
        })
    }
}

/// Error returned when an effect domain cannot become a GPU OCIO pass pair.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RenderEffectColorDomainGpuPlanError {
    /// The route must begin with a GPU-resident linear working frame.
    #[error("effect GPU color route requires a GPU linear working frame, got {descriptor:?}")]
    InvalidWorkingInput {
        /// Invalid input contract.
        descriptor: ColorFrameDescriptor,
    },
    /// Data and alpha are not color-convertible RGB domains.
    #[error("effect GPU color route cannot convert non-RGB domain {domain:?}")]
    NonRgbDomain {
        /// Non-convertible domain.
        domain: mondrian_effects::EffectColorDomain,
    },
    /// One of the stock-OCIO transforms could not be planned.
    #[error(transparent)]
    ColorTransform(#[from] RenderColorTransformError),
}

/// Input transform requested by a decode/source boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderInputTransform {
    /// Timeline working color space.
    pub working_color_space: WorkingColorSpace,
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
        working_color_space: WorkingColorSpace,
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

    /// Build a source/import -> timeline working-space transform that must be
    /// planned and executed by the native OCIO GPU path.
    pub fn to_working_gpu(
        working_color_space: WorkingColorSpace,
        tone_map: bool,
        engine: ColorEngine,
    ) -> Self {
        Self {
            working_color_space,
            tone_map,
            engine,
            backend: RenderColorTransformBackend::OcioGpuShaderPlan,
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

    /// Build an export/delivery transform through an explicit OCIO
    /// display/view pair.
    ///
    /// Unlike [`Self::display_view`] which targets preview presentation
    /// (`Display` domain), this targets encoded export delivery (`Export`
    /// domain). The OCIO display/view transform (which carries tone mapping)
    /// is applied identically; only the diagnostic domain differs.
    pub fn delivery_view(
        output_color_space: ColorSpace,
        display: impl Into<String>,
        view: impl Into<String>,
        tone_map: bool,
        engine: ColorEngine,
    ) -> Self {
        Self {
            output_color_space,
            output_domain: ColorFrameDomain::Export,
            display_view: Some(RenderOcioDisplayView::new(display, view)),
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
                    color_space: transform.working_color_space.into(),
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
            color_space: transform.working_color_space.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Cpu,
        };

        let mut data = frame.data().to_vec();
        let source_identity = match descriptor.color_space {
            ColorFrameSpace::Color(source) => OcioColorSpaceIdentity::Color(source),
            ColorFrameSpace::Working(source) => OcioColorSpaceIdentity::Working(source),
            ColorFrameSpace::Device(_) => {
                return Err(RenderColorTransformError::ExecutionFailed {
                    direction: RenderColorTransformDirection::InputToWorking,
                    input: descriptor,
                    output: output_descriptor,
                    reason: "linear source frame cannot carry a monitor-device identity"
                        .to_string(),
                });
            }
        };
        transform
            .engine
            .convert_identity_float(
                &mut data,
                source_identity,
                transform.working_color_space.into(),
            )
            .map_err(|reason| RenderColorTransformError::ExecutionFailed {
                direction: RenderColorTransformDirection::InputToWorking,
                input: descriptor,
                output: output_descriptor,
                reason,
            })?;

        // Re-pack flat f32 into the typed working-frame payload.
        let pixels: Vec<[f32; 4]> =
            data.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
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
            color_space: transform.working_color_space.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Cpu,
        };

        let source = descriptor.color_space.color().ok_or_else(|| {
            RenderColorTransformError::ExecutionFailed {
                direction: RenderColorTransformDirection::InputToWorking,
                input: descriptor,
                output: output_descriptor,
                reason: "encoded input frame is missing an encoded color-space identity"
                    .to_string(),
            }
        })?;
        let mut flat = frame
            .rgba()
            .chunks_exact(4)
            .flat_map(|pixel| pixel.iter().map(|channel| *channel as f32 / 255.0))
            .collect::<Vec<_>>();
        transform
            .engine
            .convert_identity_float(
                &mut flat,
                OcioColorSpaceIdentity::Color(source),
                OcioColorSpaceIdentity::Working(transform.working_color_space),
            )
            .map_err(|reason| RenderColorTransformError::ExecutionFailed {
                direction: RenderColorTransformDirection::InputToWorking,
                input: descriptor,
                output: output_descriptor,
                reason,
            })?;

        let pixels = flat
            .chunks_exact(4)
            .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
            .collect();
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
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
            color_space: transform.output_color_space.into(),
            domain: transform.output_domain,
            encoding: ColorFrameEncoding::EncodedRgba8,
            residency: ColorFrameResidency::Cpu,
        };

        let encoded_float = Self::transform_float(frame, transform)?;
        let rgba = encoded_float
            .frame
            .rgba_f32()
            .data
            .iter()
            .flat_map(|pixel| {
                pixel.iter().map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8)
            })
            .collect();

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
    /// quantization. Returns an encoded float boundary frame with the output
    /// transform applied.
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
            color_space: transform.output_color_space.into(),
            domain: transform.output_domain,
            encoding: ColorFrameEncoding::EncodedFloat,
            residency: ColorFrameResidency::Cpu,
        };

        // Flatten borrowed typed pixels into the contiguous f32 buffer OCIO expects.
        let mut flat = flatten_rgba_f32_pixels(&frame.rgba_f32().data);
        let working = descriptor.color_space.working().ok_or_else(|| {
            RenderColorTransformError::ExecutionFailed {
                direction: RenderColorTransformDirection::WorkingToOutput,
                input: descriptor,
                output: output_descriptor,
                reason: "working frame is missing a working-space identity".to_string(),
            }
        })?;
        if let Some(display_view) = &transform.display_view {
            transform
                .engine
                .display_transform_identity_float(
                    &mut flat,
                    working.into(),
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
                .convert_identity_float(
                    &mut flat,
                    working.into(),
                    transform.output_color_space.into(),
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
        let out_frame = CpuEncodedFloatColorFrame::new(
            EncodedRgbaF32Frame {
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

    /// Apply a preview-only Program Output to monitor colorimetric transform on
    /// encoded float data without introducing an RGBA8 boundary.
    pub fn monitor_adaptation_float(
        frame: &CpuEncodedFloatColorFrame,
        adaptation: &RenderMonitorAdaptation,
    ) -> Result<RenderOutputTransformFloatResult, RenderColorTransformError> {
        let descriptor = frame.descriptor();
        let output_descriptor = ColorFrameDescriptor {
            width: descriptor.width,
            height: descriptor.height,
            color_space: adaptation.monitor_color_space.into(),
            domain: ColorFrameDomain::Display,
            encoding: ColorFrameEncoding::EncodedFloat,
            residency: ColorFrameResidency::Cpu,
        };
        if !matches!(
            descriptor.domain,
            ColorFrameDomain::Display | ColorFrameDomain::Export
        ) {
            return Err(RenderColorTransformError::UnsupportedInputDomain {
                domain: descriptor.domain,
            });
        }
        if descriptor.encoding != ColorFrameEncoding::EncodedFloat {
            return Err(RenderColorTransformError::ExecutionFailed {
                direction: RenderColorTransformDirection::Intermediate,
                input: descriptor,
                output: output_descriptor,
                reason: "CPU monitor adaptation requires encoded-float input".to_owned(),
            });
        }
        let source = descriptor.color_space.color().ok_or_else(|| {
            RenderColorTransformError::ExecutionFailed {
                direction: RenderColorTransformDirection::Intermediate,
                input: descriptor,
                output: output_descriptor,
                reason: "CPU monitor adaptation input has no encoded color identity".to_owned(),
            }
        })?;
        if source != adaptation.program_output_color_space {
            return Err(RenderColorTransformError::ExecutionFailed {
                direction: RenderColorTransformDirection::Intermediate,
                input: descriptor,
                output: output_descriptor,
                reason: format!(
                    "Program Output {source:?} does not match monitor adaptation input {:?}",
                    adaptation.program_output_color_space
                ),
            });
        }

        let mut flat = flatten_rgba_f32_pixels(&frame.rgba_f32().data);
        adaptation
            .engine
            .convert_identity_float(
                &mut flat,
                OcioColorSpaceIdentity::Color(source),
                OcioColorSpaceIdentity::Color(adaptation.monitor_color_space),
            )
            .map_err(|reason| RenderColorTransformError::ExecutionFailed {
                direction: RenderColorTransformDirection::Intermediate,
                input: descriptor,
                output: output_descriptor,
                reason,
            })?;
        let pixels = flat.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
        let frame = CpuEncodedFloatColorFrame::new(
            EncodedRgbaF32Frame {
                width: descriptor.width,
                height: descriptor.height,
                data: pixels,
                color_space: adaptation.monitor_color_space,
            },
            ColorFrameDomain::Display,
        );
        Ok(RenderOutputTransformFloatResult {
            frame,
            diagnostics: RenderColorTransformDiagnostics {
                backend: RenderColorTransformBackend::CpuOcioFloat,
                direction: RenderColorTransformDirection::Intermediate,
                input: descriptor,
                output: output_descriptor,
                pixel_count: descriptor.pixel_count(),
                used_rgba8_boundary: false,
            },
        })
    }
}

fn flatten_rgba_f32_pixels(pixels: &[[f32; 4]]) -> Vec<f32> {
    let mut flat = Vec::with_capacity(pixels.len().saturating_mul(4));
    for pixel in pixels {
        flat.extend_from_slice(pixel);
    }
    flat
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
            color_space: transform.working_color_space.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: self.options.output_residency,
        };
        let working = transform.working_color_space;
        let source = input.color_space.color().ok_or_else(|| {
            RenderColorTransformError::ExecutionFailed {
                direction: RenderColorTransformDirection::InputToWorking,
                input,
                output,
                reason: "GPU input frame is missing an external color-space identity".to_string(),
            }
        })?;
        let request = OcioGpuShaderRequest::ColorSpace {
            engine: transform.engine.clone(),
            src: OcioColorSpaceIdentity::Color(source),
            dst: OcioColorSpaceIdentity::Working(working),
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
        self.plan_output_transform_with_encoding(input, transform, ColorFrameEncoding::EncodedRgba8)
    }

    /// Plan GPU output while explicitly retaining the destination sample encoding.
    pub fn plan_output_transform_with_encoding(
        &mut self,
        input: ColorFrameDescriptor,
        transform: &RenderColorTransform,
        output_encoding: ColorFrameEncoding,
    ) -> Result<RenderColorTransformGpuPlan, RenderColorTransformError> {
        if input.domain != ColorFrameDomain::Working {
            return Err(RenderColorTransformError::UnsupportedInputDomain { domain: input.domain });
        }

        let output = ColorFrameDescriptor {
            width: input.width,
            height: input.height,
            color_space: transform.output_color_space.into(),
            domain: transform.output_domain,
            encoding: output_encoding,
            residency: self.options.output_residency,
        };
        let working = input.color_space.working().ok_or_else(|| {
            RenderColorTransformError::ExecutionFailed {
                direction: RenderColorTransformDirection::WorkingToOutput,
                input,
                output,
                reason: "GPU output frame is missing a working-space identity".to_string(),
            }
        })?;
        let request = if let Some(display_view) = &transform.display_view {
            OcioGpuShaderRequest::DisplayView {
                engine: transform.engine.clone(),
                src: OcioColorSpaceIdentity::Working(working),
                display: display_view.display.clone(),
                view: display_view.view.clone(),
                language: self.options.language,
            }
        } else {
            OcioGpuShaderRequest::ColorSpace {
                engine: transform.engine.clone(),
                src: OcioColorSpaceIdentity::Working(working),
                dst: OcioColorSpaceIdentity::Color(transform.output_color_space),
                language: self.options.language,
            }
        };
        self.plan(
            RenderColorTransformDirection::WorkingToOutput,
            input,
            output,
            request,
        )
    }

    /// Plan an arbitrary OCIO identity-to-identity transform between GPU render nodes.
    ///
    /// Unlike input and output boundary planning, this path preserves residency and
    /// never implies an upload or readback. It is used for explicit effect domains.
    pub fn plan_identity_transform(
        &mut self,
        input: ColorFrameDescriptor,
        transform: &RenderIntermediateColorTransform,
    ) -> Result<RenderColorTransformGpuPlan, RenderColorTransformError> {
        let source_identity = frame_space_identity(input.color_space).ok_or_else(|| {
            RenderColorTransformError::ExecutionFailed {
                direction: RenderColorTransformDirection::Intermediate,
                input,
                output: input,
                reason: "GPU intermediate cannot use a monitor-device identity".to_string(),
            }
        })?;
        let output = ColorFrameDescriptor {
            width: input.width,
            height: input.height,
            color_space: identity_frame_space(transform.output_identity),
            domain: transform.output_domain,
            encoding: transform.output_encoding,
            residency: input.residency,
        };
        let request = OcioGpuShaderRequest::ColorSpace {
            engine: transform.engine.clone(),
            src: source_identity,
            dst: transform.output_identity,
            language: self.options.language,
        };
        self.plan(
            RenderColorTransformDirection::Intermediate,
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

fn frame_space_identity(space: ColorFrameSpace) -> Option<OcioColorSpaceIdentity> {
    match space {
        ColorFrameSpace::Color(space) => Some(OcioColorSpaceIdentity::Color(space)),
        ColorFrameSpace::Working(space) => Some(OcioColorSpaceIdentity::Working(space)),
        ColorFrameSpace::Device(_) => None,
    }
}

fn identity_frame_space(identity: OcioColorSpaceIdentity) -> ColorFrameSpace {
    match identity {
        OcioColorSpaceIdentity::Color(space) => ColorFrameSpace::Color(space),
        OcioColorSpaceIdentity::Working(space) => ColorFrameSpace::Working(space),
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
    /// A frame carried an external color identity where a working identity was required.
    #[error("unsupported render color identity for working transform: {identity:?}")]
    UnsupportedWorkingIdentity {
        /// Actual typed frame identity.
        identity: crate::ColorFrameSpace,
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
    /// A camera/display transfer space was selected as a linear working space.
    #[error("invalid render working color space: {0}")]
    InvalidWorkingColorSpace(#[from] mondrian_core::InvalidWorkingColorSpace),
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::types::OcioConfigSource;
    use mondrian_core::{ensure_mondrian_default_ocio_loaded, WorkingRgbaF32Frame};

    fn pinned_custom_engine(source: OcioConfigSource) -> ColorEngine {
        ColorEngine::CustomOcio {
            identity: Box::new(
                mondrian_core::CustomOcioProjectIdentity::from_pinned_parts(
                    source,
                    "0".repeat(64),
                    "test-resolved-config".to_owned(),
                    "0".repeat(64),
                    "Linear Rec.709 (sRGB)".to_owned(),
                    vec![mondrian_core::CustomOcioOutputIdentity::from_pinned_parts(
                        ColorSpace::Rec709,
                        "Test Display".to_owned(),
                        "Test View".to_owned(),
                        "Test Display Color Space".to_owned(),
                        mondrian_core::CustomOcioLookIdentity::None,
                    )
                    .expect("valid Custom OCIO output binding")],
                    Vec::new(),
                    Vec::new(),
                )
                .expect("structurally valid Custom OCIO test identity"),
            ),
        }
    }

    #[test]
    fn cpu_transform_returns_typed_display_boundary_frame() {
        let source = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 1,
            height: 1,
            data: vec![[0.5, 0.25, 0.125, 1.0]],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let transform = RenderColorTransform::display(
            ColorSpace::Srgb,
            false,
            ColorEngine::mondrian_standard(),
        );

        let output =
            CpuColorTransformExecutor::transform(&source, &transform).expect("display transform");

        let descriptor = output.frame.descriptor();
        assert_eq!(descriptor.domain, ColorFrameDomain::Display);
        assert_eq!(descriptor.color_space, ColorSpace::Srgb.into());
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
        let (display, view) = ColorEngine::mondrian_standard()
            .default_display_view()
            .expect("default display/view");
        let source = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 1,
            height: 1,
            data: vec![[0.25, 0.5, 0.75, 1.0]],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let transform = RenderColorTransform::display_view(
            ColorSpace::Srgb,
            display,
            view,
            false,
            ColorEngine::mondrian_standard(),
        );

        let output = CpuColorTransformExecutor::transform(&source, &transform)
            .expect("display/view transform");

        assert_eq!(output.frame.descriptor().domain, ColorFrameDomain::Display);
        assert_eq!(
            output.frame.descriptor().color_space,
            ColorSpace::Srgb.into()
        );
        assert_eq!(output.frame.rgba().len(), 4);
    }

    #[test]
    fn input_transform_returns_typed_working_frame() {
        let source =
            CpuEncodedColorFrame::source_rgba8(1, 1, ColorSpace::Rec709, vec![128, 64, 32, 255]);
        let transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::mondrian_standard(),
        );

        let working = CpuColorTransformExecutor::input_to_working(&source, &transform)
            .expect("input transform");

        let descriptor = working.frame.descriptor();
        assert_eq!(descriptor.domain, ColorFrameDomain::Working);
        assert_eq!(
            descriptor.color_space,
            WorkingColorSpace::LinearRec709.into()
        );
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
        let source = CpuEncodedColorFrame::source_rgba8(
            2,
            1,
            ColorSpace::SonySLog3SGamut3Cine,
            vec![128; 2 * 4],
        );
        let missing_path = std::env::temp_dir().join(format!(
            "mondrian-missing-input-ocio-{}.ocio",
            std::process::id()
        ));
        let transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec709,
            false,
            pinned_custom_engine(OcioConfigSource::Path { path: missing_path }),
        );

        let err = CpuColorTransformExecutor::input_to_working(&source, &transform)
            .expect_err("missing explicit OCIO source must fail");

        match err {
            RenderColorTransformError::ExecutionFailed { direction, input, output, reason } => {
                assert_eq!(direction, RenderColorTransformDirection::InputToWorking);
                assert_eq!(input, source.descriptor());
                assert_eq!(output.domain, ColorFrameDomain::Working);
                assert_eq!(output.color_space, WorkingColorSpace::LinearRec709.into());
                assert_eq!(output.pixel_count(), 2);
                assert!(reason.contains("OCIO config file not found"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn output_transform_failure_carries_boundary_descriptors() {
        let source = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 2,
            height: 2,
            data: vec![[0.5, 0.25, 0.125, 1.0]; 4],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let missing_path = std::env::temp_dir().join(format!(
            "mondrian-missing-output-ocio-{}.ocio",
            std::process::id()
        ));
        let transform = RenderColorTransform::export(
            ColorSpace::Srgb,
            false,
            pinned_custom_engine(OcioConfigSource::Path { path: missing_path }),
        );

        let err = CpuColorTransformExecutor::transform(&source, &transform)
            .expect_err("missing explicit OCIO source must fail");

        match err {
            RenderColorTransformError::ExecutionFailed { direction, input, output, reason } => {
                assert_eq!(direction, RenderColorTransformDirection::WorkingToOutput);
                assert_eq!(input, source.descriptor());
                assert_eq!(output.domain, ColorFrameDomain::Export);
                assert_eq!(output.color_space, ColorSpace::Srgb.into());
                assert_eq!(output.encoding, ColorFrameEncoding::EncodedFloat);
                assert_eq!(output.pixel_count(), 4);
                assert!(reason.contains("OCIO config file not found"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn gpu_planner_builds_input_shader_plan_with_explicit_boundaries() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let source = CpuEncodedColorFrame::source_rgba8(
            2,
            3,
            ColorSpace::SonySLog3SGamut3Cine,
            vec![128; 2 * 3 * 4],
        );
        let transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::mondrian_standard(),
        );
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
            plan.request,
            OcioGpuShaderRequest::ColorSpace {
                engine: ColorEngine::mondrian_standard(),
                src: OcioColorSpaceIdentity::Color(ColorSpace::SonySLog3SGamut3Cine),
                dst: OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec709),
                language: GpuLanguage::Glsl4_0,
            }
        );
    }

    #[test]
    fn gpu_planner_builds_output_shader_plan_from_working_descriptor() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let source = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 4,
            height: 5,
            data: vec![[0.5, 0.25, 0.125, 1.0]; 20],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let transform = RenderColorTransform::display(
            ColorSpace::Srgb,
            false,
            ColorEngine::mondrian_standard(),
        );
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
        assert_eq!(plan.diagnostics.output.color_space, ColorSpace::Srgb.into());
        assert_eq!(plan.diagnostics.output.residency, ColorFrameResidency::Gpu);
        assert_eq!(plan.diagnostics.pixel_count, 20);
        assert!(plan.requires_source_upload);
        assert!(!plan.requires_output_readback);
        assert_eq!(
            plan.request,
            OcioGpuShaderRequest::ColorSpace {
                engine: ColorEngine::mondrian_standard(),
                src: OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec709),
                dst: OcioColorSpaceIdentity::Color(ColorSpace::Srgb),
                language: GpuLanguage::Glsl4_0,
            }
        );
        assert!(plan.wgpu.blockers.is_empty());
        assert!(plan.wgpu.can_execute());
    }

    #[test]
    fn gpu_planner_builds_resident_effect_domain_identity_transform() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let input = ColorFrameDescriptor {
            width: 3840,
            height: 2160,
            color_space: WorkingColorSpace::LinearRec2020.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
        };
        let transform = RenderIntermediateColorTransform {
            output_identity: OcioColorSpaceIdentity::Color(ColorSpace::Rec709),
            output_domain: ColorFrameDomain::Effect,
            output_encoding: ColorFrameEncoding::EncodedFloat,
            engine: ColorEngine::mondrian_standard(),
        };
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderColorTransformGpuPlanner::new(
            &mut cache,
            RenderColorTransformGpuOptions::default(),
        );

        let plan = planner
            .plan_identity_transform(input, &transform)
            .expect("effect-domain GPU plan");

        assert_eq!(plan.direction, RenderColorTransformDirection::Intermediate);
        assert_eq!(plan.diagnostics.input, input);
        assert_eq!(plan.diagnostics.output.domain, ColorFrameDomain::Effect);
        assert_eq!(
            plan.diagnostics.output.color_space,
            ColorFrameSpace::Color(ColorSpace::Rec709)
        );
        assert_eq!(
            plan.diagnostics.output.encoding,
            ColorFrameEncoding::EncodedFloat
        );
        assert_eq!(plan.diagnostics.output.residency, ColorFrameResidency::Gpu);
        assert!(!plan.requires_source_upload);
        assert!(!plan.requires_output_readback);
        assert!(plan.can_execute_in_place_on_gpu());
        assert_eq!(
            plan.request,
            OcioGpuShaderRequest::ColorSpace {
                engine: ColorEngine::mondrian_standard(),
                src: OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
                dst: OcioColorSpaceIdentity::Color(ColorSpace::Rec709),
                language: GpuLanguage::Glsl4_0,
            }
        );
    }

    #[test]
    fn monitor_adaptation_uses_no_pass_for_identical_program_and_monitor_spaces() {
        let adaptation = RenderMonitorAdaptation::new(
            ColorSpace::Rec709,
            ColorSpace::Rec709,
            ColorEngine::mondrian_standard(),
        )
        .expect("matching display-referred spaces");

        assert!(!adaptation.requires_pass());
        assert_eq!(adaptation.gpu_transform(), None);
    }

    #[test]
    fn monitor_adaptation_plans_sdr_colorimetric_pass_without_transfers() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let adaptation = RenderMonitorAdaptation::new(
            ColorSpace::Rec709,
            ColorSpace::DisplayP3,
            ColorEngine::mondrian_standard(),
        )
        .expect("SDR monitor adaptation");
        let input = ColorFrameDescriptor {
            width: 3840,
            height: 2160,
            color_space: ColorSpace::Rec709.into(),
            domain: ColorFrameDomain::Display,
            encoding: ColorFrameEncoding::EncodedFloat,
            residency: ColorFrameResidency::Gpu,
        };
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderColorTransformGpuPlanner::new(
            &mut cache,
            RenderColorTransformGpuOptions::default(),
        );

        let plan = planner
            .plan_identity_transform(
                input,
                &adaptation.gpu_transform().expect("required adaptation pass"),
            )
            .expect("monitor adaptation GPU plan");

        assert!(adaptation.requires_pass());
        assert_eq!(plan.diagnostics.input, input);
        assert_eq!(plan.diagnostics.output.domain, ColorFrameDomain::Display);
        assert_eq!(
            plan.diagnostics.output.color_space,
            ColorFrameSpace::Color(ColorSpace::DisplayP3)
        );
        assert_eq!(
            plan.diagnostics.output.encoding,
            ColorFrameEncoding::EncodedFloat
        );
        assert!(plan.can_execute_in_place_on_gpu());
        assert_eq!(
            plan.request,
            OcioGpuShaderRequest::ColorSpace {
                engine: ColorEngine::mondrian_standard(),
                src: OcioColorSpaceIdentity::Color(ColorSpace::Rec709),
                dst: OcioColorSpaceIdentity::Color(ColorSpace::DisplayP3),
                language: GpuLanguage::Glsl4_0,
            }
        );
    }

    #[test]
    fn monitor_adaptation_rejects_dynamic_range_class_changes() {
        let error = RenderMonitorAdaptation::new(
            ColorSpace::Rec2100Pq,
            ColorSpace::Rec709,
            ColorEngine::mondrian_standard(),
        )
        .expect_err("HDR to SDR requires a rendering transform, not monitor adaptation");

        assert_eq!(
            error,
            RenderMonitorAdaptationError::DynamicRangeClassMismatch {
                program_output: ColorSpace::Rec2100Pq,
                monitor_output: ColorSpace::Rec709,
            }
        );
    }

    #[test]
    fn monitor_adaptation_rejects_non_display_identities() {
        let error = RenderMonitorAdaptation::new(
            ColorSpace::LinearRec2020,
            ColorSpace::Srgb,
            ColorEngine::mondrian_standard(),
        )
        .expect_err("scene-linear program output is invalid");

        assert_eq!(
            error,
            RenderMonitorAdaptationError::UnsupportedProgramOutput {
                color_space: ColorSpace::LinearRec2020,
            }
        );
    }

    #[test]
    fn cpu_monitor_adaptation_converts_encoded_float_without_rgba8_boundary() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let frame = CpuEncodedFloatColorFrame::new(
            EncodedRgbaF32Frame {
                width: 2,
                height: 1,
                data: vec![[0.1, 0.2, 0.3, 1.0], [0.8, 0.6, 0.4, 0.5]],
                color_space: ColorSpace::Rec709,
            },
            ColorFrameDomain::Display,
        );
        let adaptation = RenderMonitorAdaptation::new(
            ColorSpace::Rec709,
            ColorSpace::Srgb,
            ColorEngine::mondrian_standard(),
        )
        .expect("SDR monitor adaptation");

        let result = CpuColorTransformExecutor::monitor_adaptation_float(&frame, &adaptation)
            .expect("CPU monitor adaptation");

        assert_eq!(
            result.diagnostics.direction,
            RenderColorTransformDirection::Intermediate
        );
        assert_eq!(result.diagnostics.input, frame.descriptor());
        assert_eq!(
            result.diagnostics.output.color_space,
            ColorFrameSpace::Color(ColorSpace::Srgb)
        );
        assert_eq!(result.diagnostics.output.domain, ColorFrameDomain::Display);
        assert_eq!(
            result.diagnostics.output.encoding,
            ColorFrameEncoding::EncodedFloat
        );
        assert!(!result.diagnostics.used_rgba8_boundary);
        assert_eq!(result.frame.rgba_f32().data[1][3], 0.5);
    }

    #[test]
    fn effect_domain_planner_builds_gpu_round_trip_without_transfer_nodes() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let input = ColorFrameDescriptor {
            width: 3840,
            height: 2160,
            color_space: WorkingColorSpace::LinearRec2020.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
        };
        let domain = mondrian_effects::EffectColorDomain::DisplayEncodedRgb {
            color_space: ColorSpace::Rec709,
        };
        let mut cache = OcioGpuShaderCache::default();
        let mut planner = RenderEffectColorDomainGpuPlanner::new(
            &mut cache,
            RenderColorTransformGpuOptions::default(),
        );

        let route = planner
            .plan(input, domain, ColorEngine::mondrian_standard())
            .expect("effect color route");
        let to_processing = route.to_processing.expect("working to effect");
        let to_working = route.to_working.expect("effect to working");

        assert_eq!(to_processing.diagnostics.input, input);
        assert_eq!(
            to_processing.diagnostics.output.domain,
            ColorFrameDomain::Effect
        );
        assert_eq!(
            to_processing.diagnostics.output.encoding,
            ColorFrameEncoding::EncodedFloat
        );
        assert_eq!(
            to_working.diagnostics.input,
            to_processing.diagnostics.output
        );
        assert_eq!(
            to_working.diagnostics.output.domain,
            ColorFrameDomain::Working
        );
        assert_eq!(
            to_working.diagnostics.output.color_space,
            WorkingColorSpace::LinearRec2020.into()
        );
        assert_eq!(
            to_working.diagnostics.output.encoding,
            ColorFrameEncoding::LinearFloat
        );
        assert!(to_processing.can_execute_in_place_on_gpu());
        assert!(to_working.can_execute_in_place_on_gpu());
    }

    #[test]
    fn gpu_planner_builds_display_view_shader_plan_from_working_descriptor() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let (display, view) = ColorEngine::mondrian_standard()
            .default_display_view()
            .expect("default display/view");
        let source = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 4,
            height: 5,
            data: vec![[0.5, 0.25, 0.125, 1.0]; 20],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let transform = RenderColorTransform::display_view(
            ColorSpace::Srgb,
            display.clone(),
            view.clone(),
            false,
            ColorEngine::mondrian_standard(),
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
                engine: ColorEngine::mondrian_standard(),
                src: OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec709),
                display: display.clone(),
                view: view.clone(),
                language: GpuLanguage::Glsl4_0,
            }
        );
        assert!(plan.wgpu.blockers.is_empty());
        assert!(plan.wgpu.can_execute());
    }

    #[test]
    fn gpu_planner_can_request_cpu_output_readback_boundary() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let source = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 2,
            height: 2,
            data: vec![[0.5, 0.25, 0.125, 1.0]; 4],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let transform = RenderColorTransform::export(
            ColorSpace::Rec709,
            false,
            ColorEngine::mondrian_standard(),
        );
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
        assert!(matches!(
            plan.request,
            OcioGpuShaderRequest::ColorSpace {
                src: OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec709),
                dst: OcioColorSpaceIdentity::Color(ColorSpace::Rec709),
                ..
            }
        ));
        assert!(plan.wgpu.blockers.is_empty());
        assert!(plan.wgpu.can_execute());
    }

    #[test]
    fn float_input_transform_bypasses_rgba8_boundary() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let source = LinearFloatSource::new(
            2,
            2,
            ColorSpace::Aces2065_1,
            vec![
                0.5, 0.25, 0.125, 1.0, 0.8, 0.6, 0.4, 1.0, 0.2, 0.4, 0.6, 1.0, 1.0, 0.5, 0.0, 1.0,
            ],
        );
        let transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec2020,
            false,
            ColorEngine::mondrian_standard(),
        );

        let result = CpuColorTransformExecutor::input_to_working_float(&source, &transform)
            .expect("float input transform");

        assert_eq!(result.frame.descriptor().domain, ColorFrameDomain::Working);
        assert_eq!(
            result.frame.descriptor().encoding,
            ColorFrameEncoding::LinearFloat
        );
        assert_eq!(result.diagnostics.pixel_count, 4);
        assert!(!result.diagnostics.used_rgba8_boundary);
        assert_eq!(
            result.frame.descriptor().color_space,
            ColorFrameSpace::Working(WorkingColorSpace::LinearRec2020)
        );
        assert_ne!(result.frame.rgba_f32().data[0], [0.5, 0.25, 0.125, 1.0]);
    }

    #[test]
    fn float_output_transform_produces_float_frame() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let source = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 2,
            height: 2,
            data: vec![
                [0.5, 0.25, 0.125, 1.0],
                [0.8, 0.6, 0.4, 1.0],
                [0.2, 0.4, 0.6, 1.0],
                [1.0, 0.5, 0.0, 1.0],
            ],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let transform = RenderColorTransform::export(
            ColorSpace::Rec709,
            false,
            ColorEngine::mondrian_standard(),
        );

        let result = CpuColorTransformExecutor::transform_float(&source, &transform)
            .expect("float output transform");

        // Float output keeps the encoded output graph domain while avoiding u8 quantization.
        assert_eq!(result.frame.descriptor().domain, ColorFrameDomain::Export);
        assert_eq!(
            result.frame.descriptor().encoding,
            ColorFrameEncoding::EncodedFloat
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
    fn flatten_rgba_f32_pixels_preserves_channel_order() {
        let pixels = [[0.25, 0.5, 0.75, 1.0], [1.25, 1.5, 1.75, 0.5]];

        let flat = flatten_rgba_f32_pixels(&pixels);

        assert_eq!(flat, vec![0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 0.5]);
        assert_eq!(flat.capacity(), pixels.len() * 4);
    }

    #[test]
    fn float_pipeline_avoids_transfer_function_round_trip() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        // Create a float source with linear values.
        let linear_data = vec![0.5, 0.25, 0.125, 1.0];
        let float_source = LinearFloatSource::new(1, 1, ColorSpace::LinearRec709, linear_data);

        let transform = RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::mondrian_standard(),
        );

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

    #[test]
    fn delivery_view_constructor_sets_export_domain_with_display_view() {
        let transform = RenderColorTransform::delivery_view(
            ColorSpace::Srgb,
            "sRGB - Display",
            "ACES 2.0 - SDR 100 nits (Rec.709)",
            true,
            ColorEngine::Aces {
                preset: mondrian_core::types::AcesConfigPreset::StudioV4Aces2Ocio25,
            },
        );

        assert_eq!(transform.output_domain, ColorFrameDomain::Export);
        assert!(transform.display_view.is_some());
        let dv = transform.display_view.as_ref().unwrap();
        assert_eq!(dv.display, "sRGB - Display");
        assert_eq!(dv.view, "ACES 2.0 - SDR 100 nits (Rec.709)");
        assert!(transform.tone_map);
        assert_eq!(transform.output_color_space, ColorSpace::Srgb);
    }

    #[test]
    fn delivery_view_float_path_dispatches_to_display_transform() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let source = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 2,
            height: 2,
            data: vec![
                [0.5, 0.25, 0.125, 1.0],
                [0.8, 0.6, 0.4, 1.0],
                [0.2, 0.4, 0.6, 1.0],
                [1.0, 0.5, 0.0, 1.0],
            ],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let transform = RenderColorTransform::delivery_view(
            ColorSpace::Srgb,
            "sRGB - Display",
            "ACES 2.0 - SDR 100 nits (Rec.709)",
            true,
            ColorEngine::Aces {
                preset: mondrian_core::types::AcesConfigPreset::StudioV4Aces2Ocio25,
            },
        );

        let result = CpuColorTransformExecutor::transform_float(&source, &transform)
            .expect("delivery view float transform");

        assert_eq!(result.frame.descriptor().domain, ColorFrameDomain::Export);
        assert_eq!(
            result.frame.descriptor().encoding,
            ColorFrameEncoding::EncodedFloat
        );
        assert!(!result.diagnostics.used_rgba8_boundary);
        assert_eq!(
            result.diagnostics.backend,
            RenderColorTransformBackend::CpuOcioFloat
        );
    }
}
