//! GPU-resident execution of one prepared visual frame.
//!
//! This Module is the renderer-owned Seam between product Adapters and the
//! lower-level color/compositor Implementations. It accepts already-resolved
//! working sources, typed non-color data textures, adjustments, and visual
//! transitions; records their complete working-space graph; and returns one
//! GPU-resident working frame. Decode, nested traversal, output encoding, and
//! publication remain Adapter responsibilities.

use std::sync::Arc;

use mondrian_core::types::{BlendMode, Color, ColorEngine};
use mondrian_core::WorkingColorSpace;
use mondrian_effects::{CompiledEffectGpuPlan, EffectColorDomain};

use crate::{
    product_gpu_working_bytes_per_pixel, CpuColorFrame, GpuColorFrameHandle, GpuCompositeError,
    GpuCompositeLayer, GpuCompositeLayerSource, GpuCompositeRequest, GpuCompositingDiagnostics,
    GpuFrameCompositor, GpuWorkingFloatDecision, RenderColorStageDiagnostics,
    RenderColorTransformGpuOptions, RenderGpuCompositeGraphRecordError,
    RenderGpuEffectDomainRecordError, RenderGpuOutputBoundaryRuntime,
    RenderGpuOutputBoundaryRuntimeOwnedBackendContext, PRODUCT_GPU_WORKING_FLOAT_DECISION,
};

/// Typed pixels or procedural values entering GPU visual execution.
#[derive(Clone)]
pub enum GpuVisualFrameSource {
    /// CPU working-linear color pixels uploaded without another color transform.
    Working(Arc<CpuColorFrame>),
    /// CPU RGBA numeric channels uploaded with `NonColorData + DataTexture` identity.
    ///
    /// The compositor is the only allowed bypass into a working-domain result;
    /// no OCIO stage may consume this source directly.
    DataTexture(Arc<CpuColorFrame>),
    /// An already materialized GPU working frame owned by the supplied frame table.
    GpuWorking(GpuColorFrameHandle),
    /// Procedural full-frame working-linear color.
    Solid(Color),
}

impl GpuVisualFrameSource {
    fn extent(&self, canvas_width: u32, canvas_height: u32) -> (u32, u32) {
        match self {
            Self::Working(frame) | Self::DataTexture(frame) => {
                let descriptor = frame.descriptor();
                (descriptor.width, descriptor.height)
            }
            Self::GpuWorking(frame) => {
                let descriptor = frame.descriptor();
                (descriptor.width, descriptor.height)
            }
            Self::Solid(_) => (canvas_width, canvas_height),
        }
    }
}

/// One resolved source branch before stack compositing.
#[derive(Clone)]
pub struct GpuVisualSourceLayer {
    /// Typed source payload.
    pub source: GpuVisualFrameSource,
    /// Straight-alpha opacity.
    pub opacity: f32,
    /// Canonical Timeline blend mode.
    pub blend_mode: BlendMode,
    /// Timeline affine transform.
    pub transform: [f32; 6],
    /// Exact homogeneous GPU Effect plan.
    pub effect_plan: Arc<CompiledEffectGpuPlan>,
    /// Stable Timeline seed.
    pub frame_seed: i64,
}

/// One input to a two-source visual transition.
#[derive(Clone)]
pub enum GpuVisualTransitionInput {
    /// Explicit transparent endpoint.
    Transparent,
    /// Independently prepared source endpoint.
    Source(Box<GpuVisualSourceLayer>),
}

/// Bottom-to-top visual graph element.
#[derive(Clone)]
pub enum GpuVisualFrameElement {
    /// Ordinary media, title, nested, or solid source.
    Source(Box<GpuVisualSourceLayer>),
    /// Full-frame adjustment over the lower accumulator.
    Adjustment {
        /// Exact homogeneous GPU Effect plan.
        effect_plan: Arc<CompiledEffectGpuPlan>,
        /// Authored adjustment opacity.
        opacity: f32,
        /// Authored adjustment blend mode.
        blend_mode: BlendMode,
        /// Stable Timeline seed.
        frame_seed: i64,
    },
    /// Exact working-linear Cross Dissolve between independently prepared endpoints.
    CrossDissolve {
        /// Earlier endpoint.
        left: GpuVisualTransitionInput,
        /// Later endpoint.
        right: GpuVisualTransitionInput,
        /// Normalized interpolation coefficient.
        progress: f32,
    },
}

/// Logical demand for active GPU visual textures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GpuVisualFrameActiveTextureDemand {
    /// Conservative logical texture bytes, excluding driver padding.
    pub bytes: u64,
    /// Independently allocated textures.
    pub textures: u64,
}

impl GpuVisualFrameActiveTextureDemand {
    fn checked_add(
        &mut self,
        other: Self,
        stage: GpuVisualFrameActiveWorkingSetStage,
    ) -> Result<(), GpuVisualFrameActiveWorkingSetEstimateError> {
        self.bytes = self
            .bytes
            .checked_add(other.bytes)
            .ok_or(GpuVisualFrameActiveWorkingSetEstimateError::ArithmeticOverflow { stage })?;
        self.textures = self
            .textures
            .checked_add(other.textures)
            .ok_or(GpuVisualFrameActiveWorkingSetEstimateError::ArithmeticOverflow { stage })?;
        Ok(())
    }
}

/// Stable stage attribution for GPU visual active-resource evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVisualFrameActiveWorkingSetStage {
    /// Textures already retained by earlier child nodes in the same closure.
    RetainedClosure,
    /// CPU working or typed DataTexture uploads.
    SourceUploads,
    /// Source-domain and adjustment-domain intermediate textures.
    Effects,
    /// Independently materialized Transition endpoints and outputs.
    Transitions,
    /// Working accumulators and Adjustment blend outputs.
    WorkingComposite,
    /// Aggregate checked demand.
    Total,
}

/// Conservative active texture estimate before one visual node records.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GpuVisualFrameActiveWorkingSetEstimate {
    /// Existing closure resources retained by the shared frame table.
    pub retained_closure: GpuVisualFrameActiveTextureDemand,
    /// New CPU-source uploads.
    pub source_uploads: GpuVisualFrameActiveTextureDemand,
    /// New Effect-domain intermediates.
    pub effects: GpuVisualFrameActiveTextureDemand,
    /// New Transition intermediates.
    pub transitions: GpuVisualFrameActiveTextureDemand,
    /// New working-composite intermediates.
    pub working_composite: GpuVisualFrameActiveTextureDemand,
    total: GpuVisualFrameActiveTextureDemand,
}

impl GpuVisualFrameActiveWorkingSetEstimate {
    /// Aggregate active demand across existing closure residency and this node.
    pub const fn total(self) -> GpuVisualFrameActiveTextureDemand {
        self.total
    }
}

/// Failure to derive a checked GPU visual working-set estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GpuVisualFrameActiveWorkingSetEstimateError {
    /// Pixel, byte, texture, or aggregate arithmetic overflowed.
    #[error("GPU visual active working-set arithmetic overflowed in {stage:?}")]
    ArithmeticOverflow {
        /// First stage whose demand could not be represented.
        stage: GpuVisualFrameActiveWorkingSetStage,
    },
    /// The request carried invalid raster geometry.
    #[error("GPU visual frame dimensions must both be non-zero")]
    EmptyCanvas,
    /// Existing renderer frame-table residency could not be represented.
    #[error("GPU visual retained frame-table residency overflowed")]
    RetainedResidencyOverflow,
}

/// Hard owner-scoped grant for one GPU visual frame closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuVisualFrameExecutionResourceGrant {
    max_active_bytes: u64,
    max_active_textures: u64,
}

impl GpuVisualFrameExecutionResourceGrant {
    /// Construct exact logical-byte and texture-count limits.
    pub const fn new(max_active_bytes: u64, max_active_textures: u64) -> Self {
        Self { max_active_bytes, max_active_textures }
    }

    /// Maximum logical active texture bytes.
    pub const fn max_active_bytes(self) -> u64 {
        self.max_active_bytes
    }

    /// Maximum active texture resources.
    pub const fn max_active_textures(self) -> u64 {
        self.max_active_textures
    }

    fn admit(
        self,
        estimate: GpuVisualFrameActiveWorkingSetEstimate,
    ) -> Result<(), GpuVisualFrameActiveWorkingSetAdmissionError> {
        let required = estimate.total();
        if required.bytes > self.max_active_bytes || required.textures > self.max_active_textures {
            return Err(
                GpuVisualFrameActiveWorkingSetAdmissionError::GrantExceeded {
                    required_bytes: required.bytes,
                    granted_bytes: self.max_active_bytes,
                    required_textures: required.textures,
                    granted_textures: self.max_active_textures,
                },
            );
        }
        Ok(())
    }
}

impl Default for GpuVisualFrameExecutionResourceGrant {
    fn default() -> Self {
        Self::new(u64::MAX, u64::MAX)
    }
}

/// Failure to admit one complete GPU visual node before recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GpuVisualFrameActiveWorkingSetAdmissionError {
    /// The exact estimate could not be represented.
    #[error(transparent)]
    Estimate(#[from] GpuVisualFrameActiveWorkingSetEstimateError),
    /// The frozen owner grant cannot cover this node plus retained children.
    #[error(
        "GPU visual active working set requires {required_bytes} bytes across {required_textures} textures, but the owner grants {granted_bytes} bytes across {granted_textures} textures"
    )]
    GrantExceeded {
        /// Required logical bytes.
        required_bytes: u64,
        /// Granted logical bytes.
        granted_bytes: u64,
        /// Required texture resources.
        required_textures: u64,
        /// Granted texture resources.
        granted_textures: u64,
    },
}

/// Immutable request for one GPU-resident prepared visual frame.
pub struct GpuVisualFrameRequest<'a> {
    /// Output canvas width.
    pub width: u32,
    /// Output canvas height.
    pub height: u32,
    /// Linear working identity shared by every color source and result.
    pub working_color_space: WorkingColorSpace,
    /// Stock color engine used only for explicit external Effect domains.
    pub color_engine: ColorEngine,
    /// Bottom-to-top graph elements.
    pub elements: &'a [GpuVisualFrameElement],
}

/// Evidence returned for one recorded GPU visual frame.
#[derive(Debug)]
pub struct GpuVisualFrameRecord {
    /// GPU-resident working result retained in the shared frame table.
    pub output: GpuColorFrameHandle,
    /// Product working-format decision shared with Viewer execution.
    pub working_float_decision: GpuWorkingFloatDecision,
    /// GPU compositor path evidence.
    pub compositing_diagnostics: GpuCompositingDiagnostics,
    /// Explicit OCIO stage evidence. DataTexture input bypasses contribute zero stages.
    pub color_stage_diagnostics: RenderColorStageDiagnostics,
    /// External-domain adjustment passes interleaved into the stack.
    pub external_adjustment_passes: u64,
    /// Frozen active-resource admission that authorized recording.
    pub active_working_set: GpuVisualFrameActiveWorkingSetEstimate,
}

/// Failure while recording a GPU visual frame.
#[derive(Debug, thiserror::Error)]
pub enum GpuVisualFrameExecutionError {
    /// Active GPU residency could not be admitted before recording.
    #[error(transparent)]
    ActiveWorkingSet(#[from] GpuVisualFrameActiveWorkingSetAdmissionError),
    /// Source/transition working compositing failed.
    #[error("GPU visual source composite failed: {0}")]
    Composite(#[from] GpuCompositeError),
    /// A source Effect required an external color-domain round trip that failed.
    #[error("GPU visual source Effect-domain execution failed: {0:?}")]
    SourceEffectDomain(Box<RenderGpuEffectDomainRecordError>),
    /// Final stack/adjustment graph recording failed.
    #[error("GPU visual composite graph failed: {0:?}")]
    CompositeGraph(Box<RenderGpuCompositeGraphRecordError>),
    /// Actual GPU working storage disagreed with the policy used for admission.
    #[error("GPU visual working output must use {expected:?}, got {actual:?}")]
    WorkingFloatPolicyMismatch {
        /// Policy-selected format.
        expected: crate::GpuColorFrameTextureFormat,
        /// Actual recorded output format.
        actual: crate::GpuColorFrameTextureFormat,
    },
}

/// Long-lived renderer Implementation for prepared GPU visual frames.
///
/// Frame ids, OCIO caches, and texture ownership remain in the caller-supplied
/// [`RenderGpuOutputBoundaryRuntime`] so this executor composes directly with
/// Viewer or Export output stages without an intermediate transfer.
pub struct GpuVisualFrameExecutor {
    compositor: GpuFrameCompositor,
    resource_grant: GpuVisualFrameExecutionResourceGrant,
}

impl GpuVisualFrameExecutor {
    /// Create the retained compositor pipelines for one device lifetime.
    pub fn new(
        device: &wgpu::Device,
    ) -> Result<Self, crate::GpuColorFrameBindGroupCacheKeyAllocationError> {
        Self::with_resource_grant(device, GpuVisualFrameExecutionResourceGrant::default())
    }

    /// Create a retained executor with a hard closure-wide active texture grant.
    pub fn with_resource_grant(
        device: &wgpu::Device,
        resource_grant: GpuVisualFrameExecutionResourceGrant,
    ) -> Result<Self, crate::GpuColorFrameBindGroupCacheKeyAllocationError> {
        Ok(Self {
            compositor: GpuFrameCompositor::new(device)?,
            resource_grant,
        })
    }

    /// Release frame-local uniform slots after the caller has submitted all
    /// commands that reference them.
    pub fn clear_frame_resources(&self) {
        self.compositor.clear_frame_resources();
    }

    /// Record a complete working-space visual graph without readback.
    pub fn record(
        &self,
        runtime: &mut RenderGpuOutputBoundaryRuntime,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        request: GpuVisualFrameRequest<'_>,
    ) -> Result<GpuVisualFrameRecord, GpuVisualFrameExecutionError> {
        let retained_textures = u64::try_from(runtime.frame_table().len()).map_err(|_| {
            GpuVisualFrameActiveWorkingSetAdmissionError::Estimate(
                GpuVisualFrameActiveWorkingSetEstimateError::RetainedResidencyOverflow,
            )
        })?;
        let retained_bytes = runtime.frame_table().logical_texture_bytes().ok_or(
            GpuVisualFrameActiveWorkingSetAdmissionError::Estimate(
                GpuVisualFrameActiveWorkingSetEstimateError::RetainedResidencyOverflow,
            ),
        )?;
        let active_working_set = estimate_gpu_visual_frame_active_working_set(
            &request,
            GpuVisualFrameActiveTextureDemand {
                bytes: retained_bytes,
                textures: retained_textures,
            },
        )
        .map_err(GpuVisualFrameActiveWorkingSetAdmissionError::from)?;
        self.resource_grant.admit(active_working_set)?;
        let mut prepared = Vec::with_capacity(request.elements.len());
        let mut compositing_diagnostics = GpuCompositingDiagnostics::default();
        let mut color_stage_diagnostics = RenderColorStageDiagnostics::default();

        for element in request.elements {
            match element {
                GpuVisualFrameElement::Source(source) => {
                    prepared.push(PreparedElement::Source(self.prepare_source(
                        runtime,
                        device,
                        queue,
                        encoder,
                        &request,
                        source,
                        &mut compositing_diagnostics,
                        &mut color_stage_diagnostics,
                    )?));
                }
                GpuVisualFrameElement::Adjustment {
                    effect_plan,
                    opacity,
                    blend_mode,
                    frame_seed,
                } => {
                    if effect_plan.is_identity() || opacity.clamp(0.0, 1.0) == 0.0 {
                        continue;
                    }
                    prepared.push(PreparedElement::Adjustment {
                        effect_plan: Arc::clone(effect_plan),
                        opacity: *opacity,
                        blend_mode: *blend_mode,
                        frame_seed: *frame_seed,
                    });
                }
                GpuVisualFrameElement::CrossDissolve { left, right, progress } => {
                    let left = self.record_transition_input(
                        runtime,
                        device,
                        queue,
                        encoder,
                        &request,
                        left,
                        &mut compositing_diagnostics,
                        &mut color_stage_diagnostics,
                    )?;
                    let right = self.record_transition_input(
                        runtime,
                        device,
                        queue,
                        encoder,
                        &request,
                        right,
                        &mut compositing_diagnostics,
                        &mut color_stage_diagnostics,
                    )?;
                    let dissolved = runtime.record_wgpu_cross_dissolve(
                        &self.compositor,
                        device,
                        queue,
                        encoder,
                        &left,
                        &right,
                        *progress,
                        request.working_color_space,
                    )?;
                    compositing_diagnostics.accumulate(dissolved.diagnostics);
                    prepared.push(PreparedElement::Source(PreparedSourceLayer {
                        source: GpuVisualFrameSource::GpuWorking(dissolved.output),
                        opacity: 1.0,
                        blend_mode: BlendMode::Normal,
                        transform: IDENTITY_AFFINE,
                        effect_plan: None,
                        frame_seed: 0,
                    }));
                }
            }
        }

        let layers = prepared.iter().map(PreparedElement::as_composite_layer).collect::<Vec<_>>();
        let graph = runtime
            .record_wgpu_composite_graph(
                &self.compositor,
                device,
                queue,
                encoder,
                GpuCompositeRequest {
                    width: request.width,
                    height: request.height,
                    working_color_space: request.working_color_space,
                    layers: &layers,
                },
                request.color_engine,
                RenderColorTransformGpuOptions::default(),
            )
            .map_err(|error| GpuVisualFrameExecutionError::CompositeGraph(Box::new(error)))?;
        compositing_diagnostics.accumulate(graph.compositing_diagnostics);
        color_stage_diagnostics.accumulate(graph.color_stage_diagnostics);
        let expected = PRODUCT_GPU_WORKING_FLOAT_DECISION.format().texture_format();
        let actual = graph.output.texture_format();
        if actual != expected {
            return Err(GpuVisualFrameExecutionError::WorkingFloatPolicyMismatch {
                expected,
                actual,
            });
        }
        Ok(GpuVisualFrameRecord {
            output: graph.output,
            working_float_decision: PRODUCT_GPU_WORKING_FLOAT_DECISION,
            compositing_diagnostics,
            color_stage_diagnostics,
            external_adjustment_passes: graph.external_adjustment_passes,
            active_working_set,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_source(
        &self,
        runtime: &mut RenderGpuOutputBoundaryRuntime,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        request: &GpuVisualFrameRequest<'_>,
        source: &GpuVisualSourceLayer,
        compositing_diagnostics: &mut GpuCompositingDiagnostics,
        color_stage_diagnostics: &mut RenderColorStageDiagnostics,
    ) -> Result<PreparedSourceLayer, GpuVisualFrameExecutionError> {
        if source.effect_plan.is_identity()
            || source.effect_plan.processing_domain() == EffectColorDomain::SceneLinearRgb
        {
            return Ok(PreparedSourceLayer {
                source: source.source.clone(),
                opacity: source.opacity,
                blend_mode: source.blend_mode,
                transform: source.transform,
                effect_plan: (!source.effect_plan.is_identity())
                    .then(|| Arc::clone(&source.effect_plan)),
                frame_seed: source.frame_seed,
            });
        }

        let (source_width, source_height) = source.source.extent(request.width, request.height);
        let source_layer = PreparedSourceLayer {
            source: source.source.clone(),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: IDENTITY_AFFINE,
            effect_plan: None,
            frame_seed: source.frame_seed,
        };
        let source_composite_layer = source_layer.as_composite_layer();
        let materialized = runtime.record_wgpu_working_composite(
            &self.compositor,
            device,
            queue,
            encoder,
            GpuCompositeRequest {
                width: source_width,
                height: source_height,
                working_color_space: request.working_color_space,
                layers: std::slice::from_ref(&source_composite_layer),
            },
        )?;
        compositing_diagnostics.accumulate(materialized.diagnostics);
        let effect = runtime
            .record_wgpu_effect_domain_round_trip(
                &self.compositor,
                &source.effect_plan,
                &materialized.output,
                request.color_engine.clone(),
                source.frame_seed,
                RenderColorTransformGpuOptions::default(),
                RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                    device,
                    queue,
                    encoder,
                    load_op: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                },
            )
            .map_err(|error| GpuVisualFrameExecutionError::SourceEffectDomain(Box::new(error)))?;
        color_stage_diagnostics.accumulate(effect.to_processing.stage_diagnostics);
        color_stage_diagnostics.accumulate(effect.to_working.stage_diagnostics);
        Ok(PreparedSourceLayer {
            source: GpuVisualFrameSource::GpuWorking(effect.to_working.materialized.output),
            opacity: source.opacity,
            blend_mode: source.blend_mode,
            transform: source.transform,
            effect_plan: None,
            frame_seed: source.frame_seed,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn record_transition_input(
        &self,
        runtime: &mut RenderGpuOutputBoundaryRuntime,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        request: &GpuVisualFrameRequest<'_>,
        input: &GpuVisualTransitionInput,
        compositing_diagnostics: &mut GpuCompositingDiagnostics,
        color_stage_diagnostics: &mut RenderColorStageDiagnostics,
    ) -> Result<GpuColorFrameHandle, GpuVisualFrameExecutionError> {
        let prepared = match input {
            GpuVisualTransitionInput::Transparent => None,
            GpuVisualTransitionInput::Source(source) => Some(self.prepare_source(
                runtime,
                device,
                queue,
                encoder,
                request,
                source,
                compositing_diagnostics,
                color_stage_diagnostics,
            )?),
        };
        let layers = prepared
            .as_ref()
            .map(PreparedSourceLayer::as_composite_layer)
            .into_iter()
            .collect::<Vec<_>>();
        let frame = runtime.record_wgpu_working_composite(
            &self.compositor,
            device,
            queue,
            encoder,
            GpuCompositeRequest {
                width: request.width,
                height: request.height,
                working_color_space: request.working_color_space,
                layers: &layers,
            },
        )?;
        compositing_diagnostics.accumulate(frame.diagnostics);
        Ok(frame.output)
    }
}

/// Estimate one complete visual node plus GPU frames retained by earlier
/// child nodes in the same prepared closure.
pub fn estimate_gpu_visual_frame_active_working_set(
    request: &GpuVisualFrameRequest<'_>,
    retained_closure: GpuVisualFrameActiveTextureDemand,
) -> Result<GpuVisualFrameActiveWorkingSetEstimate, GpuVisualFrameActiveWorkingSetEstimateError> {
    if request.width == 0 || request.height == 0 {
        return Err(GpuVisualFrameActiveWorkingSetEstimateError::EmptyCanvas);
    }
    let mut estimate = GpuVisualFrameActiveWorkingSetEstimate {
        retained_closure,
        ..GpuVisualFrameActiveWorkingSetEstimate::default()
    };
    let mut external_adjustments = 0_u64;

    for element in request.elements {
        match element {
            GpuVisualFrameElement::Source(source) => {
                estimate_visual_source(request, source, &mut estimate)?;
            }
            GpuVisualFrameElement::Adjustment { effect_plan, opacity, .. } => {
                if effect_plan.is_identity() || opacity.clamp(0.0, 1.0) == 0.0 {
                    continue;
                }
                if effect_plan.processing_domain() != EffectColorDomain::SceneLinearRgb {
                    external_adjustments = external_adjustments.checked_add(1).ok_or(
                        GpuVisualFrameActiveWorkingSetEstimateError::ArithmeticOverflow {
                            stage: GpuVisualFrameActiveWorkingSetStage::Effects,
                        },
                    )?;
                    estimate.effects.checked_add(
                        repeated_texture_demand(
                            request.width,
                            request.height,
                            3,
                            GpuVisualFrameActiveWorkingSetStage::Effects,
                        )?,
                        GpuVisualFrameActiveWorkingSetStage::Effects,
                    )?;
                }
            }
            GpuVisualFrameElement::CrossDissolve { left, right, .. } => {
                for input in [left, right] {
                    if let GpuVisualTransitionInput::Source(source) = input {
                        estimate_visual_source(request, source, &mut estimate)?;
                    }
                }
                // Each endpoint owns a conservative two-texture accumulator,
                // including the explicit transparent case, then one additional
                // working texture carries the dissolve result.
                estimate.transitions.checked_add(
                    repeated_texture_demand(
                        request.width,
                        request.height,
                        5,
                        GpuVisualFrameActiveWorkingSetStage::Transitions,
                    )?,
                    GpuVisualFrameActiveWorkingSetStage::Transitions,
                )?;
            }
        }
    }

    let composite_textures = external_adjustments
        .checked_mul(3)
        .and_then(|extra| extra.checked_add(2))
        .ok_or(
            GpuVisualFrameActiveWorkingSetEstimateError::ArithmeticOverflow {
                stage: GpuVisualFrameActiveWorkingSetStage::WorkingComposite,
            },
        )?;
    estimate.working_composite = repeated_texture_demand(
        request.width,
        request.height,
        composite_textures,
        GpuVisualFrameActiveWorkingSetStage::WorkingComposite,
    )?;

    let mut total = GpuVisualFrameActiveTextureDemand::default();
    for (demand, stage) in [
        (
            estimate.retained_closure,
            GpuVisualFrameActiveWorkingSetStage::RetainedClosure,
        ),
        (
            estimate.source_uploads,
            GpuVisualFrameActiveWorkingSetStage::SourceUploads,
        ),
        (
            estimate.effects,
            GpuVisualFrameActiveWorkingSetStage::Effects,
        ),
        (
            estimate.transitions,
            GpuVisualFrameActiveWorkingSetStage::Transitions,
        ),
        (
            estimate.working_composite,
            GpuVisualFrameActiveWorkingSetStage::WorkingComposite,
        ),
    ] {
        total.checked_add(demand, stage)?;
    }
    estimate.total = total;
    Ok(estimate)
}

fn estimate_visual_source(
    request: &GpuVisualFrameRequest<'_>,
    source: &GpuVisualSourceLayer,
    estimate: &mut GpuVisualFrameActiveWorkingSetEstimate,
) -> Result<(), GpuVisualFrameActiveWorkingSetEstimateError> {
    let (width, height) = source.source.extent(request.width, request.height);
    if matches!(
        source.source,
        GpuVisualFrameSource::Working(_) | GpuVisualFrameSource::DataTexture(_)
    ) {
        estimate.source_uploads.checked_add(
            texture_demand(
                width,
                height,
                GpuVisualFrameActiveWorkingSetStage::SourceUploads,
            )?,
            GpuVisualFrameActiveWorkingSetStage::SourceUploads,
        )?;
    }
    if !source.effect_plan.is_identity()
        && source.effect_plan.processing_domain() != EffectColorDomain::SceneLinearRgb
    {
        // A conservative two-texture source-local accumulator plus
        // working→processing, processing Effect, and processing→working outputs.
        estimate.effects.checked_add(
            repeated_texture_demand(
                width,
                height,
                5,
                GpuVisualFrameActiveWorkingSetStage::Effects,
            )?,
            GpuVisualFrameActiveWorkingSetStage::Effects,
        )?;
    }
    Ok(())
}

fn texture_demand(
    width: u32,
    height: u32,
    stage: GpuVisualFrameActiveWorkingSetStage,
) -> Result<GpuVisualFrameActiveTextureDemand, GpuVisualFrameActiveWorkingSetEstimateError> {
    repeated_texture_demand(width, height, 1, stage)
}

fn repeated_texture_demand(
    width: u32,
    height: u32,
    textures: u64,
    stage: GpuVisualFrameActiveWorkingSetStage,
) -> Result<GpuVisualFrameActiveTextureDemand, GpuVisualFrameActiveWorkingSetEstimateError> {
    let one = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(u64::from(product_gpu_working_bytes_per_pixel())))
        .ok_or(GpuVisualFrameActiveWorkingSetEstimateError::ArithmeticOverflow { stage })?;
    let bytes = one
        .checked_mul(textures)
        .ok_or(GpuVisualFrameActiveWorkingSetEstimateError::ArithmeticOverflow { stage })?;
    Ok(GpuVisualFrameActiveTextureDemand { bytes, textures })
}

const IDENTITY_AFFINE: [f32; 6] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

enum PreparedElement {
    Source(PreparedSourceLayer),
    Adjustment {
        effect_plan: Arc<CompiledEffectGpuPlan>,
        opacity: f32,
        blend_mode: BlendMode,
        frame_seed: i64,
    },
}

impl PreparedElement {
    fn as_composite_layer(&self) -> GpuCompositeLayer<'_> {
        match self {
            Self::Source(source) => source.as_composite_layer(),
            Self::Adjustment { effect_plan, opacity, blend_mode, frame_seed } => {
                GpuCompositeLayer {
                    source: GpuCompositeLayerSource::Adjustment,
                    opacity: *opacity,
                    blend_mode: *blend_mode,
                    transform: IDENTITY_AFFINE,
                    effect_plan: Some(effect_plan),
                    frame_seed: *frame_seed,
                }
            }
        }
    }
}

struct PreparedSourceLayer {
    source: GpuVisualFrameSource,
    opacity: f32,
    blend_mode: BlendMode,
    transform: [f32; 6],
    effect_plan: Option<Arc<CompiledEffectGpuPlan>>,
    frame_seed: i64,
}

impl PreparedSourceLayer {
    fn as_composite_layer(&self) -> GpuCompositeLayer<'_> {
        GpuCompositeLayer {
            source: match &self.source {
                GpuVisualFrameSource::Working(frame) => GpuCompositeLayerSource::CpuFrame(frame),
                GpuVisualFrameSource::DataTexture(frame) => {
                    GpuCompositeLayerSource::CpuDataTexture(frame)
                }
                GpuVisualFrameSource::GpuWorking(frame) => GpuCompositeLayerSource::GpuFrame(frame),
                GpuVisualFrameSource::Solid(color) => GpuCompositeLayerSource::SolidColor(*color),
            },
            opacity: self.opacity,
            blend_mode: self.blend_mode,
            transform: self.transform,
            effect_plan: self.effect_plan.as_deref(),
            frame_seed: self.frame_seed,
        }
    }
}
