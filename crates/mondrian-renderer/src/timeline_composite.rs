use crate::{
    ColorFrameAlpha, ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding,
    ColorFrameResidency, ColorFrameSpace, CpuColorFrame,
};
use mondrian_core::{
    types::{BlendMode, Color},
    ColorEngine, OcioColorSpaceIdentity, OcioCpuProcessorCacheDiagnostics, WorkingColorSpace,
    WorkingRgbaF32Frame,
};
use mondrian_effects::{
    blend_rgba_f32_pixel_seeded, blend_rgba_pixel_seeded,
    compiled_effect_graph_has_resolvable_rgba_f32_domain,
    compiled_effect_graph_has_rgba_f32_execution_shape, CompiledEffectGpuPlan, CompiledEffectGraph,
    EffectColorDomain, EffectDomainProcessorCacheKey, EffectDomainTransition,
    EffectExecutionAdmissionError, EffectExecutionError, EffectExecutionSession,
    EffectExecutionSessionConfig, EffectExecutionSessionDiagnostics, EffectFloatExecutionError,
    EffectGpuPlanBlocker, EffectProcessingBackend, EffectWorkingPrecision,
    HeterogeneousCpuExecutionStopReason, PreparedHeterogeneousCpuCompletion,
    PreparedHeterogeneousEffectWork, PreparedHeterogeneousEffectWorkError,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    hash::{Hash, Hasher},
    sync::Arc,
};

mod transition;
pub(crate) use transition::cross_dissolve_straight_rgba_f32;
use transition::{
    composite_transition_input_f32, composite_transition_input_rgba8,
    cross_dissolve_straight_rgba8, diagnose_transition_input,
};
pub use transition::{TimelineCrossDissolveLayer, TimelineTransitionInput};

#[derive(Debug, Clone)]
pub struct TimelineMediaLayer<'a> {
    pub frame: &'a CpuColorFrame,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub transform: [f32; 6],
    pub effect_graph: Arc<CompiledEffectGraph>,
    pub frame_seed: i64,
}

#[derive(Debug, Clone)]
pub struct TimelineAdjustmentLayer {
    pub effect_graph: Arc<CompiledEffectGraph>,
    pub opacity: f32,
    pub blend_mode: Option<BlendMode>,
    pub frame_seed: i64,
}

#[derive(Debug, Clone)]
pub struct TimelineSolidColorLayer {
    pub color: Color,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub transform: [f32; 6],
    pub effect_graph: Arc<CompiledEffectGraph>,
    pub frame_seed: i64,
}

#[derive(Debug, Clone)]
pub enum TimelineCompositeElement<'a> {
    Media(TimelineMediaLayer<'a>),
    Adjustment(TimelineAdjustmentLayer),
    SolidColor(TimelineSolidColorLayer),
    CrossDissolve(TimelineCrossDissolveLayer<'a>),
}

/// Initial coverage behind a timeline composite.
///
/// Program and nested-sequence working frames use [`Self::Transparent`].
/// [`Self::OpaqueBlack`] is an explicit delivery adapter for formats that
/// cannot carry alpha; it must not be used as a viewer background.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimelineCompositeBackground {
    /// Preserve uncovered and partially covered program pixels.
    #[default]
    Transparent,
    /// Composite the program over scene-linear black and return opaque pixels.
    OpaqueBlack,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TimelineCompositeOptions {
    /// Coverage behind the bottom-most timeline layer.
    pub background: TimelineCompositeBackground,
}

impl TimelineCompositeOptions {
    /// Build options for an explicit opaque-black delivery composite.
    pub const fn opaque_black() -> Self {
        Self {
            background: TimelineCompositeBackground::OpaqueBlack,
        }
    }
}

/// Renderer-owned runtime for resolving effect RGB domains through stock OCIO.
#[derive(Debug, Clone, Copy)]
pub struct TimelineEffectColorRuntime<'a> {
    /// Exact project color engine/config provider.
    pub engine: &'a ColorEngine,
    /// Sequence working space represented by `SceneLinearRgb`.
    pub working_color_space: WorkingColorSpace,
}

impl<'a> TimelineEffectColorRuntime<'a> {
    /// Bind a project color engine to one sequence working space.
    pub const fn new(engine: &'a ColorEngine, working_color_space: WorkingColorSpace) -> Self {
        Self { engine, working_color_space }
    }

    fn cache_key(self) -> EffectDomainProcessorCacheKey {
        let mut writer = EffectDomainCacheIdentityWriter::new();
        writer.write(b"mondrian.timeline-effect-color-runtime.v1");
        self.engine.hash(&mut writer);
        self.working_color_space.hash(&mut writer);
        writer.finish_key()
    }

    fn process_transition(
        self,
        pixels: &mut [[f32; 4]],
        transition: EffectDomainTransition,
        session: &mut crate::RenderCpuColorExecutionSession,
    ) -> Result<(), String> {
        let src = self.identity(transition.from)?;
        let dst = self.identity(transition.to)?;
        session.convert_identity_float_for_renderer(
            self.engine,
            pixels.as_flattened_mut(),
            src,
            dst,
        )
    }

    fn identity(self, domain: EffectColorDomain) -> Result<OcioColorSpaceIdentity, String> {
        match domain {
            EffectColorDomain::SceneLinearRgb => Ok(self.working_color_space.into()),
            EffectColorDomain::LogPerceptualRgb { color_space }
            | EffectColorDomain::DisplayLinearRgb { color_space }
            | EffectColorDomain::DisplayEncodedRgb { color_space } => Ok(color_space.into()),
            EffectColorDomain::Data | EffectColorDomain::AlphaMask => {
                Err(format!("{domain:?} is not a color-managed RGB domain"))
            }
        }
    }
}

struct EffectDomainCacheIdentityWriter {
    hasher: Sha256,
}

impl EffectDomainCacheIdentityWriter {
    fn new() -> Self {
        Self { hasher: Sha256::new() }
    }

    fn finish_key(self) -> EffectDomainProcessorCacheKey {
        EffectDomainProcessorCacheKey::from_complete_semantic_fingerprint(
            self.hasher.finalize().into(),
        )
    }
}

impl Hasher for EffectDomainCacheIdentityWriter {
    fn finish(&self) -> u64 {
        let fingerprint: [u8; 32] = self.hasher.clone().finalize().into();
        u64::from_le_bytes([
            fingerprint[0],
            fingerprint[1],
            fingerprint[2],
            fingerprint[3],
            fingerprint[4],
            fingerprint[5],
            fingerprint[6],
            fingerprint[7],
        ])
    }

    fn write(&mut self, bytes: &[u8]) {
        self.hasher.update((bytes.len() as u64).to_le_bytes());
        self.hasher.update(bytes);
    }
}

#[derive(Default)]
pub struct TimelineCompositeScratch {
    media_source: Vec<u8>,
    media_effect: Vec<u8>,
    adjustment: Vec<u8>,
    solid_fill: Vec<u8>,
    solid_fill_f32: Vec<[f32; 4]>,
    solid_effect_f32: Vec<[f32; 4]>,
    effect_execution: EffectExecutionSession,
    color_execution: crate::RenderCpuColorExecutionSession,
    cpu_working_set_grant: TimelineCpuWorkingSetGrant,
}

impl TimelineCompositeScratch {
    /// Evaluate a prepared visual frame through this exact Preview/Export
    /// owner's dynamic-topology residency.
    pub fn evaluate_prepared_visual_program(
        &mut self,
        program: &crate::PreparedVisualProgram,
        request: crate::TimelineEvaluationRequest,
    ) -> mondrian_core::Result<crate::TimelineRenderPlan> {
        crate::evaluate_prepared_visual_program_with_session(
            program,
            request,
            &mut self.effect_execution,
        )
    }

    /// Evaluate and freeze one complete Timeline frame under this exact
    /// Preview/Export owner's Effect execution Session.
    ///
    /// This is the production Interface for ordinary Render Plan evaluation
    /// plus finite-temporal preparation. It binds the generation before either
    /// phase and retains every root or sampled dynamic topology in the same
    /// bounded Session that later executes the pixels.
    pub fn prepare_timeline_frame_execution(
        &mut self,
        program: &crate::PreparedVisualProgram,
        request: crate::TimelineFrameExecutionRequest,
    ) -> Result<crate::PreparedTimelineFrameExecution, crate::TimelineFramePreparationError> {
        let (request, generation, continuity, frame_extent, output_roi, cancellation) =
            request.into_parts();
        self.effect_execution.bind_generation(generation);
        let plan = crate::evaluate_prepared_visual_program_with_session(
            program,
            request,
            &mut self.effect_execution,
        )
        .map_err(crate::TimelineFramePreparationError::Evaluation)?;
        crate::timeline_temporal::prepare_timeline_temporal_execution_with_session(
            program,
            &plan,
            generation,
            continuity,
            frame_extent,
            output_roi,
            cancellation,
            &mut self.effect_execution,
        )
        .map_err(crate::TimelineFramePreparationError::Temporal)
    }

    /// Apply one product resource decision to this Preview/Export-owned Effect
    /// execution Session. Trimming is synchronous and cannot affect another
    /// consumer's residency.
    pub fn reconfigure_effect_execution(&mut self, config: EffectExecutionSessionConfig) {
        self.effect_execution.reconfigure(config);
    }

    /// Apply an owner-scoped hard grant to compositor-owned transient frames
    /// and reusable scratch buffers.
    ///
    /// The grant excludes decoded input frames, Effect processor residency,
    /// and OCIO processor residency because those resources have independent
    /// owner-scoped grants. Reducing the retained-scratch grant synchronously
    /// releases buffers that no longer fit.
    pub fn reconfigure_cpu_working_set(&mut self, grant: TimelineCpuWorkingSetGrant) {
        self.cpu_working_set_grant = grant;
        self.enforce_retained_scratch_grant();
    }

    /// Return the installed hard grant and current reusable scratch residency.
    pub fn cpu_working_set_diagnostics(&self) -> TimelineCpuWorkingSetDiagnostics {
        TimelineCpuWorkingSetDiagnostics {
            grant: self.cpu_working_set_grant,
            retained_scratch_bytes: self.retained_scratch_bytes(),
        }
    }

    /// Admit a conservative whole-operation active-byte estimate against this
    /// owner's exact compositor grant without allocating pixels.
    ///
    /// Recursive visual materializers use this Seam to include nested working
    /// outputs that remain live while a parent composite executes. The later
    /// per-composite estimate remains mandatory; this admission prevents
    /// caller-owned child outputs from bypassing the same hard grant.
    pub fn admit_cpu_active_working_set(
        &self,
        required_bytes: u64,
        precision: TimelineCpuCompositePrecision,
    ) -> Result<(), TimelineCpuWorkingSetError> {
        let granted_bytes = self.cpu_working_set_grant.max_active_bytes;
        if required_bytes > granted_bytes {
            return Err(TimelineCpuWorkingSetError::ActiveGrantExceeded {
                required_bytes,
                granted_bytes,
                precision,
            });
        }
        Ok(())
    }

    /// Bind a scheduler generation and retire cache entries from the prior
    /// generation.
    pub fn bind_effect_execution_generation(&mut self, generation: u64) {
        self.effect_execution.bind_generation(generation);
    }

    /// Bounded Effect cache diagnostics for this compositor owner.
    pub fn effect_execution_diagnostics(&self) -> EffectExecutionSessionDiagnostics {
        self.effect_execution.diagnostics()
    }

    /// Execute one completely resolved finite temporal Timeline batch through
    /// this Preview/Export owner's Effect Session.
    pub fn execute_prepared_temporal_batch(
        &mut self,
        batch: &crate::TimelineTemporalDemandBatch,
        prepared: &mut mondrian_effects::PreparedTemporalFrameSet,
    ) -> Result<
        mondrian_effects::EffectTemporalExecutionOutput,
        mondrian_effects::EffectTemporalExecutionError,
    > {
        crate::timeline_temporal::execute_prepared_timeline_temporal_batch(
            &mut self.effect_execution,
            batch,
            prepared,
        )
    }

    /// Execute only one prepared heterogeneous CPU prefix through this
    /// Preview/Export owner's Effect Session.
    ///
    /// The Session remains encapsulated; the returned completion can only
    /// continue through the renderer heterogeneous GPU Module. The owning
    /// scheduler supplies cooperative generation-cancellation/deadline
    /// checkpoints; stopped partial work never becomes a completion token.
    pub fn execute_prepared_heterogeneous_cpu_prefix_with_checkpoint(
        &mut self,
        prepared: &PreparedHeterogeneousEffectWork,
        generation: u64,
        input: &[[f32; 4]],
        frame_seed: i64,
        working_color_space: WorkingColorSpace,
        checkpoint: impl FnMut() -> Option<HeterogeneousCpuExecutionStopReason>,
    ) -> Result<PreparedHeterogeneousCpuCompletion, PreparedHeterogeneousEffectWorkError> {
        prepared.execute_cpu_prefix_with_checkpoint(
            &self.effect_execution,
            generation,
            input,
            frame_seed,
            working_color_space,
            checkpoint,
        )
    }

    /// Materialize one procedural Solid Color and execute a prepared
    /// heterogeneous CPU prefix through this owner's Effect Session.
    ///
    /// The full raster is allocated only after the caller has validated its
    /// immutable request/grant. It remains attempt-local and is discarded as
    /// soon as the CPU frontier completion has retained every required value.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_prepared_heterogeneous_solid_cpu_prefix_with_checkpoint(
        &mut self,
        prepared: &PreparedHeterogeneousEffectWork,
        generation: u64,
        extent: mondrian_effects::EffectFrameExtent,
        color: Color,
        frame_seed: i64,
        working_color_space: WorkingColorSpace,
        checkpoint: impl FnMut() -> Option<HeterogeneousCpuExecutionStopReason>,
    ) -> Result<PreparedHeterogeneousCpuCompletion, PreparedHeterogeneousEffectWorkError> {
        let pixel_count = usize::try_from(extent.width())
            .ok()
            .and_then(|width| {
                usize::try_from(extent.height())
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .ok_or(
                PreparedHeterogeneousEffectWorkError::UnsupportedRouteShape {
                    reason: "procedural_solid_extent_overflow",
                },
            )?;
        let pixel = [color.r, color.g, color.b, color.a];
        self.solid_fill_f32.resize(pixel_count, pixel);
        self.solid_fill_f32.fill(pixel);
        prepared.execute_cpu_prefix_with_checkpoint(
            &self.effect_execution,
            generation,
            &self.solid_fill_f32,
            frame_seed,
            working_color_space,
            checkpoint,
        )
    }

    /// Apply an owner-scoped OCIO CPU processor residency limit.
    pub fn reconfigure_color_execution(&mut self, processor_capacity: usize) {
        self.color_execution.reconfigure(processor_capacity);
    }

    /// Release every OCIO CPU processor retained by this compositor owner.
    pub fn clear_color_execution(&mut self) {
        self.color_execution.clear();
    }

    /// Return bounded OCIO processor reuse evidence for this compositor owner.
    pub fn color_execution_diagnostics(&self) -> OcioCpuProcessorCacheDiagnostics {
        self.color_execution.diagnostics()
    }

    /// Borrow the CPU color execution Session owned by this composite scratch.
    pub fn color_execution_mut(&mut self) -> &mut crate::RenderCpuColorExecutionSession {
        &mut self.color_execution
    }

    /// Resolve one GPU lowering plan through this compositor owner's bounded
    /// execution/planning Session.
    pub fn get_or_lower_effect_gpu_plan(
        &mut self,
        graph: &CompiledEffectGraph,
    ) -> Result<Arc<CompiledEffectGpuPlan>, EffectGpuPlanBlocker> {
        self.effect_execution.get_or_lower_gpu_plan(graph)
    }

    fn retained_scratch_bytes(&self) -> u64 {
        self.retained_scratch_capacities().into_iter().fold(0_u64, u64::saturating_add)
    }

    fn retained_scratch_capacities(&self) -> [u64; 6] {
        let float_pixel_bytes = std::mem::size_of::<[f32; 4]>();
        [
            u64::try_from(self.media_source.capacity()).unwrap_or(u64::MAX),
            u64::try_from(self.media_effect.capacity()).unwrap_or(u64::MAX),
            u64::try_from(self.adjustment.capacity()).unwrap_or(u64::MAX),
            u64::try_from(self.solid_fill.capacity()).unwrap_or(u64::MAX),
            u64::try_from(self.solid_fill_f32.capacity().saturating_mul(float_pixel_bytes))
                .unwrap_or(u64::MAX),
            u64::try_from(self.solid_effect_f32.capacity().saturating_mul(float_pixel_bytes))
                .unwrap_or(u64::MAX),
        ]
    }

    fn clear_compositor_scratch(&mut self) {
        self.media_source = Vec::new();
        self.media_effect = Vec::new();
        self.adjustment = Vec::new();
        self.solid_fill = Vec::new();
        self.solid_fill_f32 = Vec::new();
        self.solid_effect_f32 = Vec::new();
    }

    fn prepare_cpu_working_set(
        &mut self,
        estimate: TimelineCpuWorkingSetEstimate,
    ) -> Result<(), TimelineCpuWorkingSetError> {
        let grant = self.cpu_working_set_grant;
        if estimate.active_bytes > grant.max_active_bytes {
            return Err(TimelineCpuWorkingSetError::ActiveGrantExceeded {
                required_bytes: estimate.active_bytes,
                granted_bytes: grant.max_active_bytes,
                precision: estimate.precision,
            });
        }
        if estimate.retained_scratch_bytes > grant.max_retained_scratch_bytes {
            return Err(TimelineCpuWorkingSetError::RetainedGrantExceeded {
                required_bytes: estimate.retained_scratch_bytes,
                granted_bytes: grant.max_retained_scratch_bytes,
                precision: estimate.precision,
            });
        }

        let projected_retained = self
            .retained_scratch_capacities()
            .into_iter()
            .zip(estimate.retained_requirements.bytes)
            .map(|(current, required)| current.max(required))
            .fold(0_u64, u64::saturating_add);
        if projected_retained > grant.max_retained_scratch_bytes {
            self.clear_compositor_scratch();
        }
        Ok(())
    }

    fn enforce_retained_scratch_grant(&mut self) {
        if self.retained_scratch_bytes() > self.cpu_working_set_grant.max_retained_scratch_bytes {
            self.clear_compositor_scratch();
        }
    }
}

/// A CPU composite result paired with color-path diagnostics for the plan.
#[derive(Debug, Clone)]
pub struct TimelineCompositeFrame {
    /// The composited frame in the requested working color context.
    pub frame: CpuColorFrame,
    /// Per-plan diagnostics describing whether compositing stayed float/linear
    /// or fell back to the legacy RGBA8 path.
    pub diagnostics: TimelineCompositeDiagnostics,
    /// Evidence describing which production compositor operations actually ran.
    pub execution: TimelineCompositeExecutionDiagnostics,
}

/// Execution evidence for one CPU Timeline composite.
///
/// This remains separate from [`TimelineCompositeDiagnostics`]: the latter
/// describes color correctness and fallback, while this structure proves
/// whether an optimization was actually selected.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCompositeExecutionDiagnostics {
    /// Complete composites returned by sharing an exact source frame.
    pub zero_copy_identity_passthroughs: u64,
    /// Transparent Float32 canvases initialized directly from the first layer.
    pub direct_first_layer_initializations: u64,
    /// Transparent Float32 canvases initialized by fusing the first two exact
    /// full-frame identity/Normal media layers into one output write.
    pub fused_first_two_full_frame_normal_blends: u64,
}

/// Counters describing which timeline composite path was used and why.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCompositeDiagnostics {
    /// Number of timeline elements evaluated for the composite plan.
    pub elements: u64,
    /// Composite plans that stayed on the float/linear path.
    pub float_linear_composites: u64,
    /// Composite plans that fell back to the legacy RGBA8 path.
    pub legacy_rgba8_composites: u64,
    /// Media layers that required legacy RGBA8 because of blend mode support.
    pub legacy_media_blend_mode: u64,
    /// Media layers that required legacy RGBA8 because of transform support.
    pub legacy_media_transform: u64,
    /// Media layers that required legacy RGBA8 because of effect graph support.
    pub legacy_media_effect: u64,
    /// Solid layers that required legacy RGBA8 because of blend mode support.
    pub legacy_solid_blend_mode: u64,
    /// Solid layers that required legacy RGBA8 because of transform support.
    pub legacy_solid_transform: u64,
    /// Solid layers that required legacy RGBA8 because of effect graph support.
    pub legacy_solid_effect: u64,
    /// Adjustment layers that required legacy RGBA8 because of blend mode support.
    pub legacy_adjustment_blend_mode: u64,
    /// Adjustment layers that required legacy RGBA8 because of effect graph support.
    pub legacy_adjustment_effect: u64,
    /// Composite plans blocked because an effect-domain transition was not resolved.
    pub blocked_color_domain_composites: u64,
    /// Media effects with unresolved or invalid color-domain edges.
    pub blocked_media_effect_domain: u64,
    /// Solid effects with unresolved or invalid color-domain edges.
    pub blocked_solid_effect_domain: u64,
    /// Adjustment effects with unresolved or invalid color-domain edges.
    pub blocked_adjustment_effect_domain: u64,
    /// Effect nodes that could not execute on GPU (structured blockers).
    pub effect_gpu_blockers: u64,
    /// Effect nodes that executed on GPU successfully.
    pub effect_gpu_executed: u64,
}

/// High-level compositing color path selected by timeline compositing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineCompositeColorPath {
    /// Every composite plan stayed in the float/linear working path.
    #[default]
    FloatLinear,
    /// At least one composite plan required the legacy RGBA8 path.
    LegacyRgba8,
    /// Compositing failed closed because an effect-domain contract was unresolved.
    Blocked,
}

/// Structured effect-domain blockers for one or more composite plans.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCompositeDomainBlockerBreakdown {
    /// Media effects with unresolved domain transitions or invalid domain edges.
    pub media_effect: u64,
    /// Solid effects with unresolved domain transitions or invalid domain edges.
    pub solid_effect: u64,
    /// Adjustment effects with unresolved domain transitions or invalid domain edges.
    pub adjustment_effect: u64,
}

/// Failure to composite a timeline frame without changing authored semantics.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimelineCompositeError {
    /// A media layer does not carry the exact typed working-frame contract
    /// required by this Sequence composite.
    #[error("timeline media frame contract mismatch: expected {expected:?}, got {actual:?}")]
    MediaFrameContractMismatch {
        /// Contract required at the compositor boundary.
        expected: ColorFrameDescriptor,
        /// Contract supplied by the materializer.
        actual: ColorFrameDescriptor,
    },
    /// Frame storage does not contain exactly the pixels declared by its descriptor.
    #[error(
        "timeline media frame storage length mismatch: descriptor requires {expected_pixels} pixels, got {actual_pixels}"
    )]
    MediaFrameStorageLengthMismatch {
        /// Pixel count declared by width and height.
        expected_pixels: usize,
        /// Pixel count present in immutable storage.
        actual_pixels: usize,
    },
    /// The legacy encoded executor rejected the compiled effect graph.
    #[error(transparent)]
    EncodedEffect(#[from] EffectExecutionError),
    /// The float/linear executor rejected a node or failed to produce output.
    #[error("float effect execution failed: {reason:?}")]
    FloatEffect { reason: EffectFloatExecutionError },
    /// The graph contains unresolved or non-convertible effect-domain edges.
    #[error(
        "effect color domain is blocked (media={media_effect}, solid={solid_effect}, adjustment={adjustment_effect})"
    )]
    EffectDomainBlocked {
        media_effect: u64,
        solid_effect: u64,
        adjustment_effect: u64,
    },
    /// Product execution would quantize the working composite through legacy RGBA8.
    #[error(
        "Timeline execution requires a Float32 working composite, but {effect_graphs} active effect graph(s) require the forbidden legacy NormalizedU8 route"
    )]
    LegacyRgba8WorkingCompositeForbidden {
        /// Number of active Effect graphs that cannot enter the Float32 route.
        effect_graphs: usize,
    },
    /// The compositor-owned active or retained working set exceeds its
    /// owner-scoped hard grant.
    #[error(transparent)]
    CpuWorkingSet(#[from] TimelineCpuWorkingSetError),
}

impl From<EffectFloatExecutionError> for TimelineCompositeError {
    fn from(reason: EffectFloatExecutionError) -> Self {
        Self::FloatEffect { reason }
    }
}

/// Exact sample representation selected by the current CPU Timeline
/// compositor for one complete render plan.
///
/// Product Preview and Export select Float32 for the complete working
/// composite. `NormalizedU8` remains only for explicit encoded-boundary tools
/// and compatibility diagnostics; it is never an automatic product fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineCpuCompositePrecision {
    /// Scene-linear float32 execution.
    Float32,
    /// Explicit legacy normalized RGBA8 execution outside the product working
    /// composite.
    NormalizedU8,
}

/// Hard owner-scoped resource grant for the CPU timeline compositor.
///
/// `max_active_bytes` bounds transient output/transition/effect-result frames.
/// `max_retained_scratch_bytes` independently bounds reusable format
/// adaptation and solid/effect scratch. Decoded inputs, Effect-session
/// residency, and OCIO processors are governed by their own grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCpuWorkingSetGrant {
    /// Maximum logical bytes simultaneously used by transient compositor frames.
    pub max_active_bytes: u64,
    /// Maximum logical bytes retained in reusable compositor scratch buffers.
    pub max_retained_scratch_bytes: u64,
}

impl TimelineCpuWorkingSetGrant {
    /// Construct a grant with no product-level bound.
    ///
    /// This exists for isolated tests and embedders. Product Preview and
    /// Export owners must install a machine-class grant before execution.
    pub const fn unbounded() -> Self {
        Self {
            max_active_bytes: u64::MAX,
            max_retained_scratch_bytes: u64::MAX,
        }
    }
}

impl Default for TimelineCpuWorkingSetGrant {
    fn default() -> Self {
        Self::unbounded()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RetainedScratchRequirements {
    bytes: [u64; 6],
}

impl RetainedScratchRequirements {
    fn total(self) -> Result<u64, TimelineCpuWorkingSetError> {
        self.bytes.into_iter().try_fold(0_u64, |total, bytes| {
            total.checked_add(bytes).ok_or(TimelineCpuWorkingSetError::ArithmeticOverflow)
        })
    }
}

/// Conservative pre-allocation estimate for one CPU timeline composite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineCpuWorkingSetEstimate {
    /// Representation selected for the complete composite.
    pub precision: TimelineCpuCompositePrecision,
    /// Peak logical bytes for transient compositor-owned frames.
    pub active_bytes: u64,
    /// Logical bytes required by reusable compositor scratch.
    pub retained_scratch_bytes: u64,
    retained_requirements: RetainedScratchRequirements,
}

/// Point-in-time CPU compositor resource evidence for one execution owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineCpuWorkingSetDiagnostics {
    /// Hard grant currently installed on the owner.
    pub grant: TimelineCpuWorkingSetGrant,
    /// Actual logical capacity of reusable compositor scratch buffers.
    pub retained_scratch_bytes: u64,
}

/// A CPU compositor resource estimate or admission failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TimelineCpuWorkingSetError {
    /// Frame-size or aggregate byte arithmetic exceeded the representable range.
    #[error("CPU compositor working-set arithmetic overflowed")]
    ArithmeticOverflow,
    /// Transient frame demand exceeds the owner's active-byte grant.
    #[error(
        "CPU compositor active working set requires {required_bytes} bytes but only {granted_bytes} bytes were granted for {precision:?}"
    )]
    ActiveGrantExceeded {
        /// Conservative transient-byte demand.
        required_bytes: u64,
        /// Owner-scoped hard grant.
        granted_bytes: u64,
        /// Complete-composite representation.
        precision: TimelineCpuCompositePrecision,
    },
    /// Reusable scratch demand exceeds the owner's retained-byte grant.
    #[error(
        "CPU compositor scratch requires {required_bytes} bytes but only {granted_bytes} bytes were granted for {precision:?}"
    )]
    RetainedGrantExceeded {
        /// Conservative retained-byte demand.
        required_bytes: u64,
        /// Owner-scoped hard grant.
        granted_bytes: u64,
        /// Complete-composite representation.
        precision: TimelineCpuCompositePrecision,
    },
}

fn checked_frame_bytes(
    width: u32,
    height: u32,
    bytes_per_pixel: u64,
) -> Result<u64, TimelineCpuWorkingSetError> {
    u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
        .ok_or(TimelineCpuWorkingSetError::ArithmeticOverflow)
}

fn observe_float_input_working_set(
    input: &TimelineTransitionInput<'_>,
    output_float_bytes: u64,
    retained: &mut RetainedScratchRequirements,
) -> Result<u64, TimelineCpuWorkingSetError> {
    match input {
        TimelineTransitionInput::Transparent => Ok(0),
        TimelineTransitionInput::Media(layer) => {
            if layer.effect_graph.graph().is_identity() {
                Ok(0)
            } else {
                let descriptor = layer.frame.descriptor();
                checked_frame_bytes(descriptor.width, descriptor.height, 16)
            }
        }
        TimelineTransitionInput::SolidColor(layer) => {
            if layer.effect_graph.graph().is_identity() && is_identity_transform(layer.transform) {
                return Ok(0);
            }
            retained.bytes[4] = retained.bytes[4].max(output_float_bytes);
            if !layer.effect_graph.graph().is_identity() {
                retained.bytes[5] = retained.bytes[5].max(output_float_bytes);
            }
            Ok(0)
        }
    }
}

fn observe_legacy_input_scratch(
    input: &TimelineTransitionInput<'_>,
    output_rgba8_bytes: u64,
    retained: &mut RetainedScratchRequirements,
) -> Result<(), TimelineCpuWorkingSetError> {
    match input {
        TimelineTransitionInput::Transparent => {}
        TimelineTransitionInput::Media(layer) => {
            let descriptor = layer.frame.descriptor();
            let source_bytes = checked_frame_bytes(descriptor.width, descriptor.height, 4)?;
            retained.bytes[0] = retained.bytes[0].max(source_bytes);
            if !layer.effect_graph.graph().is_identity() {
                retained.bytes[1] = retained.bytes[1].max(source_bytes);
            }
        }
        TimelineTransitionInput::SolidColor(layer) => {
            retained.bytes[3] = retained.bytes[3].max(output_rgba8_bytes);
            if !layer.effect_graph.graph().is_identity() {
                retained.bytes[1] = retained.bytes[1].max(output_rgba8_bytes);
            }
        }
    }
    Ok(())
}

/// Estimate the compositor-owned CPU working set before allocating a frame.
///
/// The estimate is deliberately conservative at Cross Dissolve boundaries:
/// the output canvas and both endpoint canvases coexist, and one endpoint's
/// float Effect result may coexist with all three. This function performs no
/// pixel work and is suitable for Preview quality selection and Export
/// capability validation.
pub fn estimate_timeline_cpu_working_set(
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'_>],
    precision: TimelineCpuCompositePrecision,
) -> Result<TimelineCpuWorkingSetEstimate, TimelineCpuWorkingSetError> {
    let output_rgba8_bytes = checked_frame_bytes(width, height, 4)?;
    let output_float_bytes = checked_frame_bytes(width, height, 16)?;
    let mut retained = RetainedScratchRequirements { bytes: [0; 6] };

    let active_bytes = match precision {
        TimelineCpuCompositePrecision::Float32 => {
            let mut peak = output_float_bytes;
            let mut has_composited_layer = false;
            for element in elements {
                match element {
                    TimelineCompositeElement::Media(layer) => {
                        if !layer.effect_graph.graph().is_identity() {
                            let descriptor = layer.frame.descriptor();
                            let effect_bytes =
                                checked_frame_bytes(descriptor.width, descriptor.height, 16)?;
                            peak = peak.max(
                                output_float_bytes
                                    .checked_add(effect_bytes)
                                    .ok_or(TimelineCpuWorkingSetError::ArithmeticOverflow)?,
                            );
                        }
                        has_composited_layer = true;
                    }
                    TimelineCompositeElement::Adjustment(layer) => {
                        if has_composited_layer
                            && layer.opacity > 1.0e-4
                            && !layer.effect_graph.graph().is_identity()
                        {
                            peak = peak.max(
                                output_float_bytes
                                    .checked_mul(2)
                                    .ok_or(TimelineCpuWorkingSetError::ArithmeticOverflow)?,
                            );
                        }
                    }
                    TimelineCompositeElement::SolidColor(layer) => {
                        if !layer.effect_graph.graph().is_identity()
                            || !is_identity_transform(layer.transform)
                        {
                            retained.bytes[4] = retained.bytes[4].max(output_float_bytes);
                            if !layer.effect_graph.graph().is_identity() {
                                retained.bytes[5] = retained.bytes[5].max(output_float_bytes);
                            }
                        }
                        has_composited_layer = true;
                    }
                    TimelineCompositeElement::CrossDissolve(transition) => {
                        let endpoint_effect_bytes = observe_float_input_working_set(
                            &transition.left,
                            output_float_bytes,
                            &mut retained,
                        )?
                        .max(observe_float_input_working_set(
                            &transition.right,
                            output_float_bytes,
                            &mut retained,
                        )?);
                        peak = peak.max(
                            output_float_bytes
                                .checked_mul(3)
                                .and_then(|bytes| bytes.checked_add(endpoint_effect_bytes))
                                .ok_or(TimelineCpuWorkingSetError::ArithmeticOverflow)?,
                        );
                        has_composited_layer = true;
                    }
                }
            }
            peak
        }
        TimelineCpuCompositePrecision::NormalizedU8 => {
            let mut peak = output_rgba8_bytes
                .checked_add(output_float_bytes)
                .ok_or(TimelineCpuWorkingSetError::ArithmeticOverflow)?;
            let mut has_composited_layer = false;
            for element in elements {
                match element {
                    TimelineCompositeElement::Media(layer) => {
                        let descriptor = layer.frame.descriptor();
                        let source_bytes =
                            checked_frame_bytes(descriptor.width, descriptor.height, 4)?;
                        retained.bytes[0] = retained.bytes[0].max(source_bytes);
                        if !layer.effect_graph.graph().is_identity() {
                            retained.bytes[1] = retained.bytes[1].max(source_bytes);
                        }
                        has_composited_layer = true;
                    }
                    TimelineCompositeElement::Adjustment(layer) => {
                        if has_composited_layer
                            && layer.opacity > 1.0e-4
                            && !layer.effect_graph.graph().is_identity()
                        {
                            retained.bytes[2] = retained.bytes[2].max(output_rgba8_bytes);
                        }
                    }
                    TimelineCompositeElement::SolidColor(layer) => {
                        retained.bytes[3] = retained.bytes[3].max(output_rgba8_bytes);
                        if !layer.effect_graph.graph().is_identity() {
                            retained.bytes[1] = retained.bytes[1].max(output_rgba8_bytes);
                        }
                        has_composited_layer = true;
                    }
                    TimelineCompositeElement::CrossDissolve(transition) => {
                        observe_legacy_input_scratch(
                            &transition.left,
                            output_rgba8_bytes,
                            &mut retained,
                        )?;
                        observe_legacy_input_scratch(
                            &transition.right,
                            output_rgba8_bytes,
                            &mut retained,
                        )?;
                        peak = peak.max(
                            output_rgba8_bytes
                                .checked_mul(3)
                                .ok_or(TimelineCpuWorkingSetError::ArithmeticOverflow)?,
                        );
                        has_composited_layer = true;
                    }
                }
            }
            peak
        }
    };
    let retained_scratch_bytes = retained.total()?;
    Ok(TimelineCpuWorkingSetEstimate {
        precision,
        active_bytes,
        retained_scratch_bytes,
        retained_requirements: retained,
    })
}

/// Execution-admission evidence for one dynamic Timeline render plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineCpuCompositeAdmission {
    /// Exact representation selected for every active Effect graph.
    pub precision: TimelineCpuCompositePrecision,
    /// Number of active Effect graphs admitted.
    pub effect_graphs: usize,
}

/// Admit a dynamically evaluated Timeline plan to the current CPU
/// single-frame compositor.
///
/// This is the production preflight Seam shared by Preview and Export. It
/// performs no media decode and touches no pixels. Dynamic Effect graph
/// evaluation must already have succeeded while constructing `plan`; this
/// function then rejects unresolved domains, temporal input, ordered state,
/// unsupported backends/precisions, and heterogeneous routes that the current
/// transfer-free CPU compositor cannot execute. Preview and Export both remain
/// Float32 until an explicit output boundary and fail closed when a graph would
/// require the legacy NormalizedU8 route.
pub fn admit_timeline_render_plan_for_cpu_compositor(
    plan: &crate::TimelineRenderPlan,
) -> Result<TimelineCpuCompositeAdmission, TimelineCompositeError> {
    let scan = scan_render_plan_effect_graphs(plan);
    let float_domain_blockers =
        render_plan_domain_blockers(plan, TimelineCpuCompositePrecision::Float32);
    if !float_domain_blockers.is_empty() {
        return Err(domain_blocker_error(float_domain_blockers));
    }
    if scan.float_shape_blockers > 0 {
        return Err(
            TimelineCompositeError::LegacyRgba8WorkingCompositeForbidden {
                effect_graphs: scan.float_shape_blockers,
            },
        );
    }
    admit_render_plan_effect_graphs(plan, TimelineCpuCompositePrecision::Float32).map_err(
        |reason| TimelineCompositeError::FloatEffect {
            reason: EffectFloatExecutionError::ExecutionContract(reason),
        },
    )?;
    Ok(TimelineCpuCompositeAdmission {
        precision: TimelineCpuCompositePrecision::Float32,
        effect_graphs: scan.effect_graphs,
    })
}

fn domain_blocker_error(
    blockers: TimelineCompositeDomainBlockerBreakdown,
) -> TimelineCompositeError {
    TimelineCompositeError::EffectDomainBlocked {
        media_effect: blockers.media_effect,
        solid_effect: blockers.solid_effect,
        adjustment_effect: blockers.adjustment_effect,
    }
}

impl TimelineCompositeDomainBlockerBreakdown {
    /// Total number of blocked effect-domain inputs.
    pub fn total(self) -> u64 {
        self.media_effect
            .saturating_add(self.solid_effect)
            .saturating_add(self.adjustment_effect)
    }

    /// Whether no effect-domain blocker was recorded.
    pub fn is_empty(self) -> bool {
        self.total() == 0
    }
}

/// Structured reasons a composite plan required the legacy RGBA8 path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCompositeLegacyBreakdown {
    /// Media layers that required legacy RGBA8 because of blend mode support.
    pub media_blend_mode: u64,
    /// Media layers that required legacy RGBA8 because of transform support.
    pub media_transform: u64,
    /// Media layers that required legacy RGBA8 because of effect graph support.
    pub media_effect: u64,
    /// Solid layers that required legacy RGBA8 because of blend mode support.
    pub solid_blend_mode: u64,
    /// Solid layers that required legacy RGBA8 because of transform support.
    pub solid_transform: u64,
    /// Solid layers that required legacy RGBA8 because of effect graph support.
    pub solid_effect: u64,
    /// Adjustment layers that required legacy RGBA8 because of blend mode support.
    pub adjustment_blend_mode: u64,
    /// Adjustment layers that required legacy RGBA8 because of effect graph support.
    pub adjustment_effect: u64,
}

impl TimelineCompositeLegacyBreakdown {
    /// Total number of recorded legacy RGBA8 causes.
    pub fn total(self) -> u64 {
        self.media_blend_mode
            .saturating_add(self.media_transform)
            .saturating_add(self.media_effect)
            .saturating_add(self.solid_blend_mode)
            .saturating_add(self.solid_transform)
            .saturating_add(self.solid_effect)
            .saturating_add(self.adjustment_blend_mode)
            .saturating_add(self.adjustment_effect)
    }

    /// Returns true when no legacy RGBA8 causes were recorded.
    pub fn is_empty(self) -> bool {
        self.total() == 0
    }
}

/// Renderer-owned summary of composite color-path safety for a diagnostics snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCompositeColorPathSummary {
    /// Selected color path after folding all composite diagnostics.
    pub path: TimelineCompositeColorPath,
    /// Number of timeline elements evaluated by the composite plans.
    pub elements: u64,
    /// Composite plans that stayed on the float/linear path.
    pub float_linear_composites: u64,
    /// Composite plans that fell back to the legacy RGBA8 path.
    pub legacy_rgba8_composites: u64,
    /// Structured reasons for any legacy RGBA8 fallback.
    pub legacy_breakdown: TimelineCompositeLegacyBreakdown,
    /// Composite plans that failed closed on unresolved effect-domain semantics.
    pub blocked_composites: u64,
    /// Structured effect-domain blocker counts.
    pub domain_blockers: TimelineCompositeDomainBlockerBreakdown,
}

impl TimelineCompositeColorPathSummary {
    /// Number of composite plans represented by this summary.
    pub fn composite_plans(self) -> u64 {
        self.float_linear_composites
            .saturating_add(self.legacy_rgba8_composites)
            .saturating_add(self.blocked_composites)
    }

    /// Returns true when at least one composite plan used or required legacy RGBA8.
    pub fn uses_legacy_rgba8(self) -> bool {
        matches!(self.path, TimelineCompositeColorPath::LegacyRgba8)
    }

    /// Returns true when all represented plans stayed on the float/linear path.
    pub fn is_fully_float_linear(self) -> bool {
        self.composite_plans() > 0
            && !self.uses_legacy_rgba8()
            && self.blocked_composites == 0
            && self.float_linear_composites == self.composite_plans()
    }
}

impl TimelineCompositeDiagnostics {
    /// Merge another diagnostic snapshot into this one using saturating counters.
    pub fn accumulate(&mut self, other: Self) {
        self.elements = self.elements.saturating_add(other.elements);
        self.float_linear_composites =
            self.float_linear_composites.saturating_add(other.float_linear_composites);
        self.legacy_rgba8_composites =
            self.legacy_rgba8_composites.saturating_add(other.legacy_rgba8_composites);
        self.legacy_media_blend_mode =
            self.legacy_media_blend_mode.saturating_add(other.legacy_media_blend_mode);
        self.legacy_media_transform =
            self.legacy_media_transform.saturating_add(other.legacy_media_transform);
        self.legacy_media_effect =
            self.legacy_media_effect.saturating_add(other.legacy_media_effect);
        self.legacy_solid_blend_mode =
            self.legacy_solid_blend_mode.saturating_add(other.legacy_solid_blend_mode);
        self.legacy_solid_transform =
            self.legacy_solid_transform.saturating_add(other.legacy_solid_transform);
        self.legacy_solid_effect =
            self.legacy_solid_effect.saturating_add(other.legacy_solid_effect);
        self.legacy_adjustment_blend_mode = self
            .legacy_adjustment_blend_mode
            .saturating_add(other.legacy_adjustment_blend_mode);
        self.legacy_adjustment_effect =
            self.legacy_adjustment_effect.saturating_add(other.legacy_adjustment_effect);
        self.blocked_color_domain_composites = self
            .blocked_color_domain_composites
            .saturating_add(other.blocked_color_domain_composites);
        self.blocked_media_effect_domain = self
            .blocked_media_effect_domain
            .saturating_add(other.blocked_media_effect_domain);
        self.blocked_solid_effect_domain = self
            .blocked_solid_effect_domain
            .saturating_add(other.blocked_solid_effect_domain);
        self.blocked_adjustment_effect_domain = self
            .blocked_adjustment_effect_domain
            .saturating_add(other.blocked_adjustment_effect_domain);
        self.effect_gpu_blockers =
            self.effect_gpu_blockers.saturating_add(other.effect_gpu_blockers);
        self.effect_gpu_executed =
            self.effect_gpu_executed.saturating_add(other.effect_gpu_executed);
    }

    /// Returns true when the composite plan used any legacy RGBA8 fallback.
    pub fn uses_legacy_rgba8(self) -> bool {
        self.color_path_summary().uses_legacy_rgba8()
    }

    /// Return structured legacy RGBA8 fallback reasons.
    pub fn legacy_breakdown(self) -> TimelineCompositeLegacyBreakdown {
        TimelineCompositeLegacyBreakdown {
            media_blend_mode: self.legacy_media_blend_mode,
            media_transform: self.legacy_media_transform,
            media_effect: self.legacy_media_effect,
            solid_blend_mode: self.legacy_solid_blend_mode,
            solid_transform: self.legacy_solid_transform,
            solid_effect: self.legacy_solid_effect,
            adjustment_blend_mode: self.legacy_adjustment_blend_mode,
            adjustment_effect: self.legacy_adjustment_effect,
        }
    }

    /// Return structured unresolved effect-domain reasons.
    pub fn domain_blockers(self) -> TimelineCompositeDomainBlockerBreakdown {
        TimelineCompositeDomainBlockerBreakdown {
            media_effect: self.blocked_media_effect_domain,
            solid_effect: self.blocked_solid_effect_domain,
            adjustment_effect: self.blocked_adjustment_effect_domain,
        }
    }

    /// Whether this plan failed closed on effect-domain semantics.
    pub fn is_color_domain_blocked(self) -> bool {
        self.blocked_color_domain_composites > 0 || !self.domain_blockers().is_empty()
    }

    /// Return the high-level composite color path for this diagnostics snapshot.
    pub fn color_path(self) -> TimelineCompositeColorPath {
        if self.is_color_domain_blocked() {
            TimelineCompositeColorPath::Blocked
        } else if self.legacy_rgba8_composites > 0 || !self.legacy_breakdown().is_empty() {
            TimelineCompositeColorPath::LegacyRgba8
        } else {
            TimelineCompositeColorPath::FloatLinear
        }
    }

    /// Return a renderer-owned summary that callers can use for reports and budgets.
    pub fn color_path_summary(self) -> TimelineCompositeColorPathSummary {
        TimelineCompositeColorPathSummary {
            path: self.color_path(),
            elements: self.elements,
            float_linear_composites: self.float_linear_composites,
            legacy_rgba8_composites: self.legacy_rgba8_composites,
            legacy_breakdown: self.legacy_breakdown(),
            blocked_composites: self.blocked_color_domain_composites,
            domain_blockers: self.domain_blockers(),
        }
    }
}

/// Composite timeline elements through the diagnosed legacy RGBA8 boundary.
///
/// Unresolved effect-domain contracts return a structured error and are never
/// bypassed or evaluated as encoded pixels.
pub fn composite_timeline_elements(
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'_>],
    options: TimelineCompositeOptions,
    scratch: &mut TimelineCompositeScratch,
) -> Result<Vec<u8>, mondrian_effects::EffectExecutionError> {
    let mut out = Vec::new();
    composite_timeline_elements_into(&mut out, width, height, elements, options, scratch)?;
    Ok(out)
}

/// Composite timeline elements into a typed color-managed working frame.
pub fn composite_timeline_elements_color_frame(
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'_>],
    options: TimelineCompositeOptions,
    runtime: TimelineEffectColorRuntime<'_>,
    scratch: &mut TimelineCompositeScratch,
) -> Result<CpuColorFrame, TimelineCompositeError> {
    composite_timeline_elements_color_frame_with_diagnostics(
        width, height, elements, options, runtime, scratch,
    )
    .map(|composite| composite.frame)
}

/// Composite timeline elements and return both the working frame and
/// diagnostics for the selected color path.
pub fn composite_timeline_elements_color_frame_with_diagnostics(
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'_>],
    options: TimelineCompositeOptions,
    runtime: TimelineEffectColorRuntime<'_>,
    scratch: &mut TimelineCompositeScratch,
) -> Result<TimelineCompositeFrame, TimelineCompositeError> {
    validate_timeline_media_frame_contracts(elements, runtime.working_color_space)?;
    let diagnostics =
        composite_path_diagnostics_with_session(elements, &mut scratch.effect_execution);
    if diagnostics.is_color_domain_blocked() {
        let blockers = diagnostics.domain_blockers();
        return Err(TimelineCompositeError::EffectDomainBlocked {
            media_effect: blockers.media_effect,
            solid_effect: blockers.solid_effect,
            adjustment_effect: blockers.adjustment_effect,
        });
    }
    if diagnostics.uses_legacy_rgba8() {
        if let Err(reason) =
            admit_composite_element_effect_graphs(elements, TimelineCpuCompositePrecision::Float32)
        {
            return Err(TimelineCompositeError::FloatEffect {
                reason: EffectFloatExecutionError::ExecutionContract(reason),
            });
        }
        return Err(
            TimelineCompositeError::LegacyRgba8WorkingCompositeForbidden {
                effect_graphs: diagnostics.legacy_breakdown().total() as usize,
            },
        );
    }
    let mut execution = TimelineCompositeExecutionDiagnostics::default();
    if let Some(frame) =
        exact_zero_copy_identity_passthrough(width, height, elements, options, runtime)
    {
        // Graph shape alone is not an execution contract. Temporal,
        // ordered-state, backend, precision, and domain obligations must
        // fail closed before even a pixel-identity graph may bypass the
        // compositor.
        admit_composite_element_effect_graphs(elements, TimelineCpuCompositePrecision::Float32)
            .map_err(EffectFloatExecutionError::ExecutionContract)?;
        scratch.admit_cpu_active_working_set(0, TimelineCpuCompositePrecision::Float32)?;
        execution.zero_copy_identity_passthroughs = 1;
        scratch.enforce_retained_scratch_grant();
        return Ok(TimelineCompositeFrame { frame: frame.clone(), diagnostics, execution });
    }
    let precision = TimelineCpuCompositePrecision::Float32;
    let estimate = estimate_timeline_cpu_working_set(width, height, elements, precision)?;
    scratch.prepare_cpu_working_set(estimate)?;
    let frame_result: Result<CpuColorFrame, TimelineCompositeError> =
        composite_supported_elements_to_working_frame(
            width,
            height,
            elements,
            options,
            runtime.working_color_space,
            runtime,
            scratch,
            &mut execution,
        )
        .map(CpuColorFrame::working)
        .map_err(TimelineCompositeError::from);
    scratch.enforce_retained_scratch_grant();
    Ok(TimelineCompositeFrame { frame: frame_result?, diagnostics, execution })
}

fn validate_timeline_media_frame_contracts(
    elements: &[TimelineCompositeElement<'_>],
    working_color_space: WorkingColorSpace,
) -> Result<(), TimelineCompositeError> {
    fn validate_layer(
        layer: &TimelineMediaLayer<'_>,
        working_color_space: WorkingColorSpace,
    ) -> Result<(), TimelineCompositeError> {
        let actual = layer.frame.descriptor();
        let expected = ColorFrameDescriptor {
            width: actual.width,
            height: actual.height,
            color_space: ColorFrameSpace::Working(working_color_space),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Cpu,
            alpha: ColorFrameAlpha::StraightCoverage,
        };
        if actual != expected {
            return Err(TimelineCompositeError::MediaFrameContractMismatch { expected, actual });
        }
        let expected_pixels = (actual.width as usize)
            .checked_mul(actual.height as usize)
            .ok_or(TimelineCpuWorkingSetError::ArithmeticOverflow)?;
        let actual_pixels = layer.frame.rgba_f32().data.len();
        if actual_pixels != expected_pixels {
            return Err(TimelineCompositeError::MediaFrameStorageLengthMismatch {
                expected_pixels,
                actual_pixels,
            });
        }
        Ok(())
    }

    for element in elements {
        match element {
            TimelineCompositeElement::Media(layer) => {
                validate_layer(layer, working_color_space)?;
            }
            TimelineCompositeElement::CrossDissolve(transition) => {
                for input in [&transition.left, &transition.right] {
                    if let TimelineTransitionInput::Media(layer) = input {
                        validate_layer(layer, working_color_space)?;
                    }
                }
            }
            TimelineCompositeElement::Adjustment(_) | TimelineCompositeElement::SolidColor(_) => {}
        }
    }
    Ok(())
}

fn exact_zero_copy_identity_passthrough<'frame>(
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'frame>],
    options: TimelineCompositeOptions,
    runtime: TimelineEffectColorRuntime<'_>,
) -> Option<&'frame CpuColorFrame> {
    let [TimelineCompositeElement::Media(layer)] = elements else {
        return None;
    };
    let descriptor = layer.frame.descriptor();
    let pixel_count = (width as usize).checked_mul(height as usize)?;
    (options.background == TimelineCompositeBackground::Transparent
        && layer.opacity == 1.0
        && layer.blend_mode == BlendMode::Normal
        && is_identity_transform(layer.transform)
        && layer.effect_graph.graph().is_identity()
        && descriptor.width == width
        && descriptor.height == height
        && descriptor.color_space == ColorFrameSpace::Working(runtime.working_color_space)
        && descriptor.domain == ColorFrameDomain::Working
        && descriptor.encoding == ColorFrameEncoding::LinearFloat
        && descriptor.residency == ColorFrameResidency::Cpu
        && descriptor.alpha == ColorFrameAlpha::StraightCoverage
        && layer.frame.rgba_f32().data.len() == pixel_count)
        .then_some(layer.frame)
}

fn composite_supported_elements_to_working_frame(
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'_>],
    options: TimelineCompositeOptions,
    working_color_space: WorkingColorSpace,
    runtime: TimelineEffectColorRuntime<'_>,
    scratch: &mut TimelineCompositeScratch,
    execution: &mut TimelineCompositeExecutionDiagnostics,
) -> Result<WorkingRgbaF32Frame, EffectFloatExecutionError> {
    admit_composite_element_effect_graphs(elements, TimelineCpuCompositePrecision::Float32)
        .map_err(EffectFloatExecutionError::ExecutionContract)?;
    let pixel_count = width as usize * height as usize;
    if pixel_count == 0 {
        return Ok(WorkingRgbaF32Frame {
            width,
            height,
            data: Vec::new(),
            color_space: working_color_space,
        });
    }

    let initial_pixel = match options.background {
        TimelineCompositeBackground::Transparent => [0.0, 0.0, 0.0, 0.0],
        TimelineCompositeBackground::OpaqueBlack => [0.0, 0.0, 0.0, 1.0],
    };
    let (mut canvas, mut has_composited_layer, consumed_elements) = if let Some(canvas) =
        initialize_from_two_full_frame_normal_media_layers(width, height, elements, options)
    {
        execution.direct_first_layer_initializations =
            execution.direct_first_layer_initializations.saturating_add(1);
        execution.fused_first_two_full_frame_normal_blends =
            execution.fused_first_two_full_frame_normal_blends.saturating_add(1);
        (canvas, true, 2)
    } else {
        (Vec::new(), false, 0)
    };

    for element in &elements[consumed_elements..] {
        match element {
            TimelineCompositeElement::Media(layer) => {
                let frame = layer.frame.rgba_f32();
                let effect_output;
                let (src_data, src_width, src_height) = if layer.effect_graph.graph().is_identity()
                {
                    (&frame.data, frame.width, frame.height)
                } else {
                    effect_output = apply_effect_graph_f32(
                        &mut scratch.effect_execution,
                        &mut scratch.color_execution,
                        &frame.data,
                        frame.width,
                        frame.height,
                        &layer.effect_graph,
                        layer.frame_seed,
                        runtime,
                    )?;
                    (&effect_output, frame.width, frame.height)
                };
                if !has_composited_layer
                    && options.background == TimelineCompositeBackground::Transparent
                    && layer.opacity > 1.0e-4
                    && layer.blend_mode == BlendMode::Normal
                    && is_identity_transform(layer.transform)
                    && src_width == width
                    && src_height == height
                    && src_data.len() == pixel_count
                {
                    let opacity = layer.opacity.clamp(0.0, 1.0);
                    canvas.extend(
                        src_data
                            .iter()
                            .copied()
                            .map(|pixel| initialize_normal_rgba_f32_pixel(pixel, opacity)),
                    );
                    execution.direct_first_layer_initializations =
                        execution.direct_first_layer_initializations.saturating_add(1);
                } else {
                    ensure_float_canvas_initialized(&mut canvas, pixel_count, initial_pixel);
                    alpha_blend_f32_layer(
                        &mut canvas,
                        width as usize,
                        height as usize,
                        src_data,
                        src_width as usize,
                        src_height as usize,
                        layer.opacity,
                        layer.blend_mode,
                        layer.transform,
                        layer.frame_seed,
                    );
                }
                has_composited_layer = true;
            }
            TimelineCompositeElement::SolidColor(layer) => {
                ensure_float_canvas_initialized(&mut canvas, pixel_count, initial_pixel);
                let color = [layer.color.r, layer.color.g, layer.color.b, layer.color.a];
                if layer.effect_graph.graph().is_identity()
                    && is_identity_transform(layer.transform)
                {
                    alpha_blend_f32_solid(
                        &mut canvas,
                        color,
                        layer.opacity,
                        layer.blend_mode,
                        layer.frame_seed,
                    );
                } else {
                    scratch.solid_fill_f32.resize(pixel_count, color);
                    scratch.solid_fill_f32.fill(color);
                    let source = if layer.effect_graph.graph().is_identity() {
                        scratch.solid_fill_f32.as_slice()
                    } else {
                        scratch.solid_effect_f32 = apply_effect_graph_f32(
                            &mut scratch.effect_execution,
                            &mut scratch.color_execution,
                            &scratch.solid_fill_f32,
                            width,
                            height,
                            &layer.effect_graph,
                            layer.frame_seed,
                            runtime,
                        )?;
                        scratch.solid_effect_f32.as_slice()
                    };
                    alpha_blend_f32_layer(
                        &mut canvas,
                        width as usize,
                        height as usize,
                        source,
                        width as usize,
                        height as usize,
                        layer.opacity,
                        layer.blend_mode,
                        layer.transform,
                        layer.frame_seed,
                    );
                }
                has_composited_layer = true;
            }
            TimelineCompositeElement::Adjustment(layer) => {
                if !has_composited_layer
                    || layer.opacity <= 1.0e-4
                    || layer.effect_graph.graph().is_identity()
                {
                    continue;
                }
                ensure_float_canvas_initialized(&mut canvas, pixel_count, initial_pixel);
                canvas = apply_effect_graph_pass_f32(
                    &mut scratch.effect_execution,
                    &mut scratch.color_execution,
                    &canvas,
                    width,
                    height,
                    &layer.effect_graph,
                    layer.opacity,
                    layer.blend_mode,
                    layer.frame_seed,
                    runtime,
                )?;
            }
            TimelineCompositeElement::CrossDissolve(transition) => {
                ensure_float_canvas_initialized(&mut canvas, pixel_count, initial_pixel);
                let mut left = canvas.clone();
                composite_transition_input_f32(
                    &mut left,
                    width,
                    height,
                    &transition.left,
                    runtime,
                    scratch,
                )?;
                let mut right = canvas.clone();
                composite_transition_input_f32(
                    &mut right,
                    width,
                    height,
                    &transition.right,
                    runtime,
                    scratch,
                )?;
                cross_dissolve_straight_rgba_f32(&mut canvas, &left, &right, transition.progress);
                has_composited_layer = true;
            }
        }
    }
    ensure_float_canvas_initialized(&mut canvas, pixel_count, initial_pixel);

    Ok(WorkingRgbaF32Frame {
        width,
        height,
        data: canvas,
        color_space: working_color_space,
    })
}

fn initialize_from_two_full_frame_normal_media_layers(
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'_>],
    options: TimelineCompositeOptions,
) -> Option<Vec<[f32; 4]>> {
    if options.background != TimelineCompositeBackground::Transparent {
        return None;
    }
    let [TimelineCompositeElement::Media(first), TimelineCompositeElement::Media(second), ..] =
        elements
    else {
        return None;
    };
    let pixel_count = (width as usize).checked_mul(height as usize)?;
    if !is_exact_full_frame_identity_normal_media(first, width, height, pixel_count)
        || !is_exact_full_frame_identity_normal_media(second, width, height, pixel_count)
        || first.opacity <= 1.0e-4
        || second.opacity <= 1.0e-4
    {
        return None;
    }

    let first_pixels = &first.frame.rgba_f32().data;
    let second_pixels = &second.frame.rgba_f32().data;
    let first_opacity = first.opacity.clamp(0.0, 1.0);
    let second_opacity = second.opacity.clamp(0.0, 1.0);
    let mut canvas = Vec::with_capacity(pixel_count);
    canvas.extend(
        first_pixels.iter().zip(second_pixels).map(|(first_pixel, second_pixel)| {
            let base = initialize_normal_rgba_f32_pixel(*first_pixel, first_opacity);
            blend_normal_rgba_f32_pixel(base, *second_pixel, second_opacity)
        }),
    );
    Some(canvas)
}

fn is_exact_full_frame_identity_normal_media(
    layer: &TimelineMediaLayer<'_>,
    width: u32,
    height: u32,
    pixel_count: usize,
) -> bool {
    let frame = layer.frame.rgba_f32();
    layer.blend_mode == BlendMode::Normal
        && is_identity_transform(layer.transform)
        && layer.effect_graph.graph().is_identity()
        && frame.width == width
        && frame.height == height
        && frame.data.len() == pixel_count
}

fn ensure_float_canvas_initialized(
    canvas: &mut Vec<[f32; 4]>,
    pixel_count: usize,
    initial_pixel: [f32; 4],
) {
    if canvas.is_empty() {
        canvas.resize(pixel_count, initial_pixel);
    }
}

fn apply_effect_graph_f32(
    session: &mut EffectExecutionSession,
    color_session: &mut crate::RenderCpuColorExecutionSession,
    input: &[[f32; 4]],
    width: u32,
    height: u32,
    graph: &CompiledEffectGraph,
    frame_seed: i64,
    runtime: TimelineEffectColorRuntime<'_>,
) -> Result<Vec<[f32; 4]>, EffectFloatExecutionError> {
    if graph.domain_plan().requires_conversion() {
        session.apply_compiled_rgba_f32_with_domain_processor(
            input,
            width,
            height,
            graph,
            frame_seed,
            runtime.cache_key(),
            |pixels, transition| runtime.process_transition(pixels, transition, color_session),
        )
    } else {
        session.apply_compiled_rgba_f32(input, width, height, graph, frame_seed)
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_effect_graph_pass_f32(
    session: &mut EffectExecutionSession,
    color_session: &mut crate::RenderCpuColorExecutionSession,
    input: &[[f32; 4]],
    width: u32,
    height: u32,
    graph: &CompiledEffectGraph,
    opacity: f32,
    blend_mode: Option<BlendMode>,
    frame_seed: i64,
    runtime: TimelineEffectColorRuntime<'_>,
) -> Result<Vec<[f32; 4]>, EffectFloatExecutionError> {
    if graph.domain_plan().requires_conversion() {
        session.apply_compiled_pass_rgba_f32_with_domain_processor(
            input,
            width,
            height,
            graph,
            opacity,
            blend_mode,
            frame_seed,
            runtime.cache_key(),
            |pixels, transition| runtime.process_transition(pixels, transition, color_session),
        )
    } else {
        session.apply_compiled_pass_rgba_f32(
            input, width, height, graph, opacity, blend_mode, frame_seed,
        )
    }
}

/// Diagnose whether a set of timeline elements can stay on the float/linear
/// compositor path, or which capabilities force legacy RGBA8 fallback.
pub fn composite_path_diagnostics(
    elements: &[TimelineCompositeElement<'_>],
) -> TimelineCompositeDiagnostics {
    let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(0));
    composite_path_diagnostics_with_session(elements, &mut session)
}

fn composite_path_diagnostics_with_session(
    elements: &[TimelineCompositeElement<'_>],
    session: &mut EffectExecutionSession,
) -> TimelineCompositeDiagnostics {
    let mut diagnostics = TimelineCompositeDiagnostics {
        elements: elements.len() as u64,
        ..TimelineCompositeDiagnostics::default()
    };
    for element in elements {
        match element {
            TimelineCompositeElement::Media(layer) => {
                diagnostics.effect_gpu_blockers = diagnostics.effect_gpu_blockers.saturating_add(
                    u64::from(session.get_or_lower_gpu_plan(&layer.effect_graph).is_err()),
                );
            }
            TimelineCompositeElement::SolidColor(layer) => {
                diagnostics.effect_gpu_blockers = diagnostics.effect_gpu_blockers.saturating_add(
                    u64::from(session.get_or_lower_gpu_plan(&layer.effect_graph).is_err()),
                );
            }
            TimelineCompositeElement::Adjustment(layer) => {
                diagnostics.effect_gpu_blockers = diagnostics.effect_gpu_blockers.saturating_add(
                    u64::from(session.get_or_lower_gpu_plan(&layer.effect_graph).is_err()),
                );
            }
            TimelineCompositeElement::CrossDissolve(transition) => {
                diagnose_transition_input(&transition.left, &mut diagnostics, session);
                diagnose_transition_input(&transition.right, &mut diagnostics, session);
            }
        }
    }
    record_float_shape_fallbacks(elements, &mut diagnostics);
    record_float_mode_fallbacks(elements, &mut diagnostics);
    let legacy_reasons = diagnostics.legacy_media_blend_mode
        + diagnostics.legacy_media_transform
        + diagnostics.legacy_media_effect
        + diagnostics.legacy_solid_blend_mode
        + diagnostics.legacy_solid_transform
        + diagnostics.legacy_solid_effect
        + diagnostics.legacy_adjustment_blend_mode
        + diagnostics.legacy_adjustment_effect;
    let precision = if legacy_reasons == 0 {
        TimelineCpuCompositePrecision::Float32
    } else {
        TimelineCpuCompositePrecision::NormalizedU8
    };
    let domain_blockers = composite_element_domain_blockers(elements, precision);
    diagnostics.blocked_media_effect_domain = domain_blockers.media_effect;
    diagnostics.blocked_solid_effect_domain = domain_blockers.solid_effect;
    diagnostics.blocked_adjustment_effect_domain = domain_blockers.adjustment_effect;
    let blocked_reasons = diagnostics
        .blocked_media_effect_domain
        .saturating_add(diagnostics.blocked_solid_effect_domain)
        .saturating_add(diagnostics.blocked_adjustment_effect_domain);
    if blocked_reasons > 0 {
        diagnostics.blocked_color_domain_composites = 1;
    } else if legacy_reasons == 0 {
        diagnostics.float_linear_composites = 1;
    } else {
        diagnostics.legacy_rgba8_composites = 1;
    }
    diagnostics
}

#[derive(Debug, Clone, Copy)]
enum TimelineEffectGraphKind {
    Media,
    Solid,
    Adjustment,
}

#[derive(Debug, Clone, Copy)]
struct TimelineEffectGraphRef<'a> {
    graph: &'a CompiledEffectGraph,
    kind: TimelineEffectGraphKind,
}

fn visit_render_plan_effect_graphs<'plan>(
    plan: &'plan crate::TimelineRenderPlan,
    mut visit: impl FnMut(TimelineEffectGraphRef<'plan>),
) {
    let mut has_composited_layer = false;
    for element in &plan.elements {
        match element {
            crate::TimelineRenderPlanElement::Media(layer) => {
                visit(TimelineEffectGraphRef {
                    graph: &layer.effect_graph,
                    kind: TimelineEffectGraphKind::Media,
                });
                has_composited_layer = true;
            }
            crate::TimelineRenderPlanElement::Adjustment(layer) => {
                if has_composited_layer && layer.opacity > 1.0e-4 {
                    visit(TimelineEffectGraphRef {
                        graph: &layer.effect_graph,
                        kind: TimelineEffectGraphKind::Adjustment,
                    });
                }
            }
            crate::TimelineRenderPlanElement::SolidColor(layer) => {
                visit(TimelineEffectGraphRef {
                    graph: &layer.effect_graph,
                    kind: TimelineEffectGraphKind::Solid,
                });
                has_composited_layer = true;
            }
            crate::TimelineRenderPlanElement::BasicTitle(layer) => {
                visit(TimelineEffectGraphRef {
                    graph: &layer.effect_graph,
                    kind: TimelineEffectGraphKind::Media,
                });
                has_composited_layer = true;
            }
            crate::TimelineRenderPlanElement::NestedSequence(layer) => {
                visit(TimelineEffectGraphRef {
                    graph: &layer.effect_graph,
                    kind: TimelineEffectGraphKind::Media,
                });
                has_composited_layer = true;
            }
            crate::TimelineRenderPlanElement::CrossDissolve(transition) => {
                visit_render_plan_transition_graph(&transition.left, &mut visit);
                visit_render_plan_transition_graph(&transition.right, &mut visit);
                has_composited_layer = true;
            }
        }
    }
}

fn visit_render_plan_transition_graph<'plan>(
    input: &'plan crate::TimelineTransitionInputPlan,
    visit: &mut impl FnMut(TimelineEffectGraphRef<'plan>),
) {
    match input {
        crate::TimelineTransitionInputPlan::Transparent => {}
        crate::TimelineTransitionInputPlan::Media(layer) => {
            visit(TimelineEffectGraphRef {
                graph: &layer.effect_graph,
                kind: TimelineEffectGraphKind::Media,
            });
        }
        crate::TimelineTransitionInputPlan::SolidColor(layer) => {
            visit(TimelineEffectGraphRef {
                graph: &layer.effect_graph,
                kind: TimelineEffectGraphKind::Solid,
            });
        }
        crate::TimelineTransitionInputPlan::BasicTitle(layer) => {
            visit(TimelineEffectGraphRef {
                graph: &layer.effect_graph,
                kind: TimelineEffectGraphKind::Media,
            });
        }
        crate::TimelineTransitionInputPlan::NestedSequence(layer) => {
            visit(TimelineEffectGraphRef {
                graph: &layer.effect_graph,
                kind: TimelineEffectGraphKind::Media,
            });
        }
    }
}

fn visit_composite_element_effect_graphs<'elements, 'frame>(
    elements: &'elements [TimelineCompositeElement<'frame>],
    mut visit: impl FnMut(TimelineEffectGraphRef<'elements>),
) {
    let mut has_composited_layer = false;
    for element in elements {
        match element {
            TimelineCompositeElement::Media(layer) => {
                visit(TimelineEffectGraphRef {
                    graph: &layer.effect_graph,
                    kind: TimelineEffectGraphKind::Media,
                });
                has_composited_layer = true;
            }
            TimelineCompositeElement::Adjustment(layer) => {
                if has_composited_layer && layer.opacity > 1.0e-4 {
                    visit(TimelineEffectGraphRef {
                        graph: &layer.effect_graph,
                        kind: TimelineEffectGraphKind::Adjustment,
                    });
                }
            }
            TimelineCompositeElement::SolidColor(layer) => {
                visit(TimelineEffectGraphRef {
                    graph: &layer.effect_graph,
                    kind: TimelineEffectGraphKind::Solid,
                });
                has_composited_layer = true;
            }
            TimelineCompositeElement::CrossDissolve(transition) => {
                visit_composite_transition_graph(&transition.left, &mut visit);
                visit_composite_transition_graph(&transition.right, &mut visit);
                has_composited_layer = true;
            }
        }
    }
}

fn visit_composite_transition_graph<'elements, 'frame>(
    input: &'elements TimelineTransitionInput<'frame>,
    visit: &mut impl FnMut(TimelineEffectGraphRef<'elements>),
) {
    match input {
        TimelineTransitionInput::Transparent => {}
        TimelineTransitionInput::Media(layer) => {
            visit(TimelineEffectGraphRef {
                graph: &layer.effect_graph,
                kind: TimelineEffectGraphKind::Media,
            });
        }
        TimelineTransitionInput::SolidColor(layer) => {
            visit(TimelineEffectGraphRef {
                graph: &layer.effect_graph,
                kind: TimelineEffectGraphKind::Solid,
            });
        }
    }
}

#[derive(Default)]
struct TimelineEffectGraphScan {
    effect_graphs: usize,
    float_shape_blockers: usize,
}

impl TimelineEffectGraphScan {
    fn observe(&mut self, graph: TimelineEffectGraphRef<'_>) {
        self.effect_graphs = self.effect_graphs.saturating_add(1);
        if !compiled_effect_graph_has_rgba_f32_execution_shape(graph.graph) {
            self.float_shape_blockers = self.float_shape_blockers.saturating_add(1);
        }
    }
}

fn scan_render_plan_effect_graphs(plan: &crate::TimelineRenderPlan) -> TimelineEffectGraphScan {
    let mut scan = TimelineEffectGraphScan::default();
    visit_render_plan_effect_graphs(plan, |graph| scan.observe(graph));
    scan
}

fn render_plan_domain_blockers(
    plan: &crate::TimelineRenderPlan,
    precision: TimelineCpuCompositePrecision,
) -> TimelineCompositeDomainBlockerBreakdown {
    let mut blockers = TimelineCompositeDomainBlockerBreakdown::default();
    visit_render_plan_effect_graphs(plan, |graph| {
        record_domain_blocker(&mut blockers, graph, precision);
    });
    blockers
}

fn composite_element_domain_blockers(
    elements: &[TimelineCompositeElement<'_>],
    precision: TimelineCpuCompositePrecision,
) -> TimelineCompositeDomainBlockerBreakdown {
    let mut blockers = TimelineCompositeDomainBlockerBreakdown::default();
    visit_composite_element_effect_graphs(elements, |graph| {
        record_domain_blocker(&mut blockers, graph, precision);
    });
    blockers
}

fn record_float_mode_fallbacks(
    elements: &[TimelineCompositeElement<'_>],
    diagnostics: &mut TimelineCompositeDiagnostics,
) {
    visit_composite_element_effect_graphs(elements, |graph| {
        if compiled_effect_graph_has_rgba_f32_execution_shape(graph.graph)
            && matches!(
                graph.graph.execution_envelope().admit_single_frame_backend(
                    EffectProcessingBackend::Cpu,
                    EffectWorkingPrecision::Float32,
                ),
                Err(EffectExecutionAdmissionError::ExecutionModeNotAdmitted { .. })
            )
        {
            match graph.kind {
                TimelineEffectGraphKind::Media => {
                    diagnostics.legacy_media_effect =
                        diagnostics.legacy_media_effect.saturating_add(1);
                }
                TimelineEffectGraphKind::Solid => {
                    diagnostics.legacy_solid_effect =
                        diagnostics.legacy_solid_effect.saturating_add(1);
                }
                TimelineEffectGraphKind::Adjustment => {
                    diagnostics.legacy_adjustment_effect =
                        diagnostics.legacy_adjustment_effect.saturating_add(1);
                }
            }
        }
    });
}

fn record_float_shape_fallbacks(
    elements: &[TimelineCompositeElement<'_>],
    diagnostics: &mut TimelineCompositeDiagnostics,
) {
    visit_composite_element_effect_graphs(elements, |graph| {
        if !compiled_effect_graph_has_rgba_f32_execution_shape(graph.graph) {
            match graph.kind {
                TimelineEffectGraphKind::Media => {
                    diagnostics.legacy_media_effect =
                        diagnostics.legacy_media_effect.saturating_add(1);
                }
                TimelineEffectGraphKind::Solid => {
                    diagnostics.legacy_solid_effect =
                        diagnostics.legacy_solid_effect.saturating_add(1);
                }
                TimelineEffectGraphKind::Adjustment => {
                    diagnostics.legacy_adjustment_effect =
                        diagnostics.legacy_adjustment_effect.saturating_add(1);
                }
            }
        }
    });
}

fn record_domain_blocker(
    blockers: &mut TimelineCompositeDomainBlockerBreakdown,
    graph: TimelineEffectGraphRef<'_>,
    precision: TimelineCpuCompositePrecision,
) {
    if effect_domain_is_blocked_for_precision(graph.graph, precision) {
        match graph.kind {
            TimelineEffectGraphKind::Media => {
                blockers.media_effect = blockers.media_effect.saturating_add(1);
            }
            TimelineEffectGraphKind::Solid => {
                blockers.solid_effect = blockers.solid_effect.saturating_add(1);
            }
            TimelineEffectGraphKind::Adjustment => {
                blockers.adjustment_effect = blockers.adjustment_effect.saturating_add(1);
            }
        }
    }
}

fn effect_working_precision(precision: TimelineCpuCompositePrecision) -> EffectWorkingPrecision {
    match precision {
        TimelineCpuCompositePrecision::Float32 => EffectWorkingPrecision::Float32,
        TimelineCpuCompositePrecision::NormalizedU8 => EffectWorkingPrecision::NormalizedU8,
    }
}

fn admit_render_plan_effect_graphs(
    plan: &crate::TimelineRenderPlan,
    precision: TimelineCpuCompositePrecision,
) -> Result<(), EffectExecutionAdmissionError> {
    let mut blocker = None;
    let working_precision = effect_working_precision(precision);
    visit_render_plan_effect_graphs(plan, |graph| {
        if blocker.is_none() {
            blocker = graph
                .graph
                .execution_envelope()
                .admit_single_frame_backend(EffectProcessingBackend::Cpu, working_precision)
                .err();
        }
    });
    blocker.map_or(Ok(()), Err)
}

fn admit_composite_element_effect_graphs(
    elements: &[TimelineCompositeElement<'_>],
    precision: TimelineCpuCompositePrecision,
) -> Result<(), EffectExecutionAdmissionError> {
    let mut blocker = None;
    let working_precision = effect_working_precision(precision);
    visit_composite_element_effect_graphs(elements, |graph| {
        if blocker.is_none() {
            blocker = graph
                .graph
                .execution_envelope()
                .admit_single_frame_backend(EffectProcessingBackend::Cpu, working_precision)
                .err();
        }
    });
    blocker.map_or(Ok(()), Err)
}

fn effect_domain_is_blocked_for_precision(
    graph: &CompiledEffectGraph,
    precision: TimelineCpuCompositePrecision,
) -> bool {
    let domain_processor_available = matches!(precision, TimelineCpuCompositePrecision::Float32);
    !compiled_effect_graph_has_resolvable_rgba_f32_domain(graph, domain_processor_available)
}

fn alpha_blend_f32_solid(
    dst: &mut [[f32; 4]],
    color: [f32; 4],
    opacity: f32,
    blend_mode: BlendMode,
    frame_seed: i64,
) {
    let src_a = (color[3] * opacity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
    if src_a <= 1.0e-4 {
        return;
    }
    for (index, dst_px) in dst.iter_mut().enumerate() {
        *dst_px = blend_rgba_f32_pixel_seeded(
            *dst_px,
            color,
            opacity,
            blend_mode,
            dither_seed(index as u32, frame_seed),
        );
    }
}

fn alpha_blend_f32_layer(
    dst: &mut [[f32; 4]],
    dst_w: usize,
    dst_h: usize,
    src: &[[f32; 4]],
    src_w: usize,
    src_h: usize,
    opacity: f32,
    blend_mode: BlendMode,
    transform: [f32; 6],
    frame_seed: i64,
) {
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 1.0e-4 {
        return;
    }

    if is_identity_transform(transform) {
        let width = dst_w.min(src_w);
        let height = dst_h.min(src_h);
        if blend_mode == BlendMode::Normal && width == dst_w && width == src_w {
            for (dst_row, src_row) in
                dst.chunks_exact_mut(dst_w).zip(src.chunks_exact(src_w)).take(height)
            {
                for (dst_px, src_px) in dst_row.iter_mut().zip(src_row) {
                    *dst_px = blend_normal_rgba_f32_pixel(*dst_px, *src_px, opacity);
                }
            }
            return;
        }
        for y in 0..height {
            for x in 0..width {
                let dst_px = &mut dst[y * dst_w + x];
                let src_px = src[y * src_w + x];
                *dst_px = blend_rgba_f32_pixel_seeded(
                    *dst_px,
                    src_px,
                    opacity,
                    blend_mode,
                    dither_seed((y * dst_w + x) as u32, frame_seed),
                );
            }
        }
        return;
    }

    let Some(inv) = invert_affine(transform) else {
        return;
    };

    for dy in 0..dst_h {
        for dx in 0..dst_w {
            let fx = dx as f32 + 0.5;
            let fy = dy as f32 + 0.5;
            let sx = inv[0] * fx + inv[1] * fy + inv[2];
            let sy = inv[3] * fx + inv[4] * fy + inv[5];
            let Some(src_px) = sample_src_f32_bilinear(src, src_w, src_h, sx - 0.5, sy - 0.5)
            else {
                continue;
            };
            let dst_idx = dy * dst_w + dx;
            dst[dst_idx] = blend_rgba_f32_pixel_seeded(
                dst[dst_idx],
                src_px,
                opacity,
                blend_mode,
                dither_seed((dy * dst_w + dx) as u32, frame_seed),
            );
        }
    }
}

#[inline]
fn initialize_normal_rgba_f32_pixel(source: [f32; 4], opacity: f32) -> [f32; 4] {
    let alpha = (source[3] * opacity).clamp(0.0, 1.0);
    if alpha <= 1.0e-4 {
        [0.0, 0.0, 0.0, 0.0]
    } else {
        [source[0], source[1], source[2], alpha]
    }
}

#[inline]
fn blend_normal_rgba_f32_pixel(base_px: [f32; 4], blend_px: [f32; 4], opacity: f32) -> [f32; 4] {
    let base_alpha = base_px[3].clamp(0.0, 1.0);
    let blend_alpha = (blend_px[3] * opacity).clamp(0.0, 1.0);
    if blend_alpha <= 1.0e-4 {
        return base_px;
    }
    if base_alpha <= 1.0e-4 {
        return [blend_px[0], blend_px[1], blend_px[2], blend_alpha];
    }

    let inverse_blend_alpha = 1.0 - blend_alpha;
    let out_alpha = blend_alpha + base_alpha * inverse_blend_alpha;
    if out_alpha <= 1.0e-4 {
        return [0.0, 0.0, 0.0, 0.0];
    }
    let base_weight = base_alpha * inverse_blend_alpha;
    [
        (blend_px[0] * blend_alpha + base_px[0] * base_weight) / out_alpha,
        (blend_px[1] * blend_alpha + base_px[1] * base_weight) / out_alpha,
        (blend_px[2] * blend_alpha + base_px[2] * base_weight) / out_alpha,
        out_alpha,
    ]
}

fn sample_src_f32_bilinear(
    src: &[[f32; 4]],
    src_w: usize,
    src_h: usize,
    sx: f32,
    sy: f32,
) -> Option<[f32; 4]> {
    let x0 = sx.floor() as isize;
    let y0 = sy.floor() as isize;
    let x1 = x0 + 1;
    let y1 = y0 + 1;

    if x0 < 0 || y0 < 0 || x1 >= src_w as isize || y1 >= src_h as isize {
        if x0 >= 0 && y0 >= 0 && x0 < src_w as isize && y0 < src_h as isize {
            return Some(src[y0 as usize * src_w + x0 as usize]);
        }
        return None;
    }

    let fx = sx - x0 as f32;
    let fy = sy - y0 as f32;

    let tl = src[y0 as usize * src_w + x0 as usize];
    let tr = src[y0 as usize * src_w + x1 as usize];
    let bl = src[y1 as usize * src_w + x0 as usize];
    let br = src[y1 as usize * src_w + x1 as usize];

    let mut out = [0.0f32; 4];
    for c in 0..4 {
        let top = tl[c] + (tr[c] - tl[c]) * fx;
        let bot = bl[c] + (br[c] - bl[c]) * fx;
        out[c] = top + (bot - top) * fy;
    }
    Some(out)
}

fn dither_seed(pixel_index: u32, frame_seed: i64) -> u32 {
    pixel_index ^ (frame_seed as u32)
}

/// Composite into a caller-owned RGBA8 buffer with fail-closed effect domains.
pub fn composite_timeline_elements_into(
    out: &mut Vec<u8>,
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'_>],
    options: TimelineCompositeOptions,
    scratch: &mut TimelineCompositeScratch,
) -> Result<(), mondrian_effects::EffectExecutionError> {
    admit_composite_element_effect_graphs(elements, TimelineCpuCompositePrecision::NormalizedU8)
        .map_err(EffectExecutionError::ExecutionContract)?;
    let required_len = width as usize * height as usize * 4;
    if out.len() != required_len {
        out.resize(required_len, 0);
    }
    if required_len == 0 {
        out.clear();
        return Ok(());
    }

    match options.background {
        TimelineCompositeBackground::Transparent => out.fill(0),
        TimelineCompositeBackground::OpaqueBlack => clear_canvas_black_opaque(out),
    }
    let mut has_composited_media = false;

    for element in elements {
        match element {
            TimelineCompositeElement::Media(layer) => {
                let descriptor = layer.frame.descriptor();
                scratch.media_source = layer
                    .frame
                    .rgba_f32()
                    .data
                    .iter()
                    .flat_map(|pixel| {
                        pixel.iter().map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8)
                    })
                    .collect();
                let src_rgba = if layer.effect_graph.graph().is_identity() {
                    scratch.media_source.as_slice()
                } else {
                    scratch.media_effect = scratch.effect_execution.apply_compiled_rgba8(
                        &scratch.media_source,
                        descriptor.width,
                        descriptor.height,
                        &layer.effect_graph,
                        layer.frame_seed,
                    )?;
                    scratch.media_effect.as_slice()
                };
                alpha_blend_layer(
                    out,
                    width,
                    height,
                    src_rgba,
                    descriptor.width,
                    descriptor.height,
                    layer.opacity,
                    layer.blend_mode,
                    layer.transform,
                );
                has_composited_media = true;
            }
            TimelineCompositeElement::SolidColor(layer) => {
                fill_solid_rgba(
                    &mut scratch.solid_fill,
                    width as usize,
                    height as usize,
                    layer.color,
                );
                let src_rgba = if layer.effect_graph.graph().is_identity() {
                    scratch.solid_fill.as_slice()
                } else {
                    scratch.media_effect = scratch.effect_execution.apply_compiled_rgba8(
                        &scratch.solid_fill,
                        width,
                        height,
                        &layer.effect_graph,
                        layer.frame_seed,
                    )?;
                    scratch.media_effect.as_slice()
                };
                alpha_blend_layer(
                    out,
                    width,
                    height,
                    src_rgba,
                    width,
                    height,
                    layer.opacity,
                    layer.blend_mode,
                    layer.transform,
                );
                has_composited_media = true;
            }
            TimelineCompositeElement::Adjustment(layer) => {
                if !has_composited_media
                    || layer.opacity <= 1.0e-4
                    || layer.effect_graph.graph().is_identity()
                {
                    continue;
                }
                scratch.effect_execution.apply_compiled_pass_rgba8(
                    out,
                    width,
                    height,
                    &layer.effect_graph,
                    layer.opacity,
                    layer.blend_mode,
                    layer.frame_seed,
                    &mut scratch.adjustment,
                )?;
                std::mem::swap(out, &mut scratch.adjustment);
            }
            TimelineCompositeElement::CrossDissolve(transition) => {
                let mut left = out.clone();
                composite_transition_input_rgba8(
                    &mut left,
                    width,
                    height,
                    &transition.left,
                    scratch,
                )?;
                let mut right = out.clone();
                composite_transition_input_rgba8(
                    &mut right,
                    width,
                    height,
                    &transition.right,
                    scratch,
                )?;
                cross_dissolve_straight_rgba8(out, &left, &right, transition.progress);
                has_composited_media = true;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
fn working_frame_from_normalized_rgba8(
    width: u32,
    height: u32,
    rgba: &[u8],
    color_space: WorkingColorSpace,
) -> WorkingRgbaF32Frame {
    let data = rgba
        .chunks_exact(4)
        .map(|pixel| {
            [
                pixel[0] as f32 / 255.0,
                pixel[1] as f32 / 255.0,
                pixel[2] as f32 / 255.0,
                pixel[3] as f32 / 255.0,
            ]
        })
        .collect();
    WorkingRgbaF32Frame { width, height, data, color_space }
}

fn fill_solid_rgba(buf: &mut Vec<u8>, width: usize, height: usize, color: Color) {
    let r = (color.r.clamp(0.0, 1.0) * 255.0).round() as u8;
    let g = (color.g.clamp(0.0, 1.0) * 255.0).round() as u8;
    let b = (color.b.clamp(0.0, 1.0) * 255.0).round() as u8;
    let a = (color.a.clamp(0.0, 1.0) * 255.0).round() as u8;
    let pixel = [r, g, b, a];
    buf.resize(width * height * 4, 0);
    for chunk in buf.chunks_exact_mut(4) {
        chunk.copy_from_slice(&pixel);
    }
}

pub fn is_identity_transform(transform: [f32; 6]) -> bool {
    const EPS: f32 = 1.0e-4;
    (transform[0] - 1.0).abs() <= EPS
        && transform[1].abs() <= EPS
        && transform[2].abs() <= EPS
        && transform[3].abs() <= EPS
        && (transform[4] - 1.0).abs() <= EPS
        && transform[5].abs() <= EPS
}

pub fn quantize_transform_signature(transform: [f32; 6]) -> [i32; 6] {
    const SCALE: f32 = 1024.0;
    [
        (transform[0] * SCALE).round() as i32,
        (transform[1] * SCALE).round() as i32,
        (transform[2] * SCALE).round() as i32,
        (transform[3] * SCALE).round() as i32,
        (transform[4] * SCALE).round() as i32,
        (transform[5] * SCALE).round() as i32,
    ]
}

fn clear_canvas_black_opaque(canvas: &mut [u8]) {
    canvas.fill(0);
    for px in canvas.chunks_exact_mut(4) {
        px[3] = 255;
    }
}

fn alpha_blend_layer(
    dst_rgba: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    src_rgba: &[u8],
    src_w: u32,
    src_h: u32,
    opacity: f32,
    blend_mode: BlendMode,
    transform: [f32; 6],
) {
    let width = dst_w.min(src_w) as usize;
    let height = dst_h.min(src_h) as usize;
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 1.0e-4 {
        return;
    }

    let dst_stride = dst_w as usize * 4;
    let src_stride = src_w as usize * 4;

    if is_identity_transform(transform) {
        for y in 0..height {
            let dst_row = &mut dst_rgba[y * dst_stride..(y + 1) * dst_stride];
            let src_row = &src_rgba[y * src_stride..(y + 1) * src_stride];
            for (x, (dst_px, src_px)) in
                dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(4)).take(width).enumerate()
            {
                let blended = blend_rgba_pixel_seeded(
                    [dst_px[0], dst_px[1], dst_px[2], dst_px[3]],
                    [src_px[0], src_px[1], src_px[2], src_px[3]],
                    opacity,
                    blend_mode,
                    (y * dst_w as usize + x) as u32,
                );
                dst_px.copy_from_slice(&blended);
            }
        }
        return;
    }

    let Some(inv) = invert_affine(transform) else {
        return;
    };

    let dst_width = dst_w as usize;
    let dst_height = dst_h as usize;
    let src_width = src_w as usize;
    let src_height = src_h as usize;

    for dy in 0..dst_height {
        for dx in 0..dst_width {
            let fx = dx as f32 + 0.5;
            let fy = dy as f32 + 0.5;
            let sx = inv[0] * fx + inv[1] * fy + inv[2];
            let sy = inv[3] * fx + inv[4] * fy + inv[5];
            let Some(src_px) = sample_src_rgba(src_rgba, src_width, src_height, sx - 0.5, sy - 0.5)
            else {
                continue;
            };
            let dst_idx = (dy * dst_width + dx) * 4;
            if dst_idx + 3 >= dst_rgba.len() {
                continue;
            }
            let dst_px = &mut dst_rgba[dst_idx..dst_idx + 4];
            let blended = blend_rgba_pixel_seeded(
                [dst_px[0], dst_px[1], dst_px[2], dst_px[3]],
                src_px,
                opacity,
                blend_mode,
                (dy * dst_width + dx) as u32,
            );
            dst_px.copy_from_slice(&blended);
        }
    }
}

fn invert_affine(transform: [f32; 6]) -> Option<[f32; 6]> {
    let a = transform[0];
    let c = transform[1];
    let tx = transform[2];
    let b = transform[3];
    let d = transform[4];
    let ty = transform[5];

    let det = a * d - b * c;
    if det.abs() <= 1.0e-6 {
        return None;
    }

    let inv_det = 1.0 / det;
    let ia = d * inv_det;
    let ic = -c * inv_det;
    let ib = -b * inv_det;
    let id = a * inv_det;
    let itx = -(ia * tx + ic * ty);
    let ity = -(ib * tx + id * ty);
    Some([ia, ic, itx, ib, id, ity])
}

fn sample_src_rgba(
    src_rgba: &[u8],
    src_w: usize,
    src_h: usize,
    sx: f32,
    sy: f32,
) -> Option<[u8; 4]> {
    let x = sx.round() as isize;
    let y = sy.round() as isize;
    if x < 0 || y < 0 || x >= src_w as isize || y >= src_h as isize {
        return None;
    }

    let idx = (y as usize * src_w + x as usize) * 4;
    if idx + 3 >= src_rgba.len() {
        return None;
    }
    Some([
        src_rgba[idx],
        src_rgba[idx + 1],
        src_rgba[idx + 2],
        src_rgba[idx + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_effects::{
        compile_reference_effect_graph, register_effect_definition, CustomEffectRenderProcessor,
        EffectCachePolicy, EffectColorDomainContract, EffectDefinition, EffectDeterminism,
        EffectExecutionContract, EffectExecutionModes, EffectGraphTopology, EffectNode,
        EffectRenderPlan, EffectResourceLifetime, EffectRoiPropagation, EffectStateModel,
        EffectTemporalInputExtent, EffectType, PreparedEffectProgram,
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COLOR_ENGINE: ColorEngine = ColorEngine::mondrian_standard();

    fn pinned_custom_engine(source: mondrian_core::OcioConfigSource) -> ColorEngine {
        ColorEngine::CustomOcio {
            identity: Box::new(
                mondrian_core::CustomOcioProjectIdentity::from_pinned_parts(
                    source,
                    "0".repeat(64),
                    "test-resolved-config".to_owned(),
                    "0".repeat(64),
                    "Linear Rec.709 (sRGB)".to_owned(),
                    vec![mondrian_core::CustomOcioOutputIdentity::from_pinned_parts(
                        mondrian_core::ColorSpace::Rec709,
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

    fn test_color_runtime(
        working_color_space: WorkingColorSpace,
    ) -> TimelineEffectColorRuntime<'static> {
        TimelineEffectColorRuntime::new(&TEST_COLOR_ENGINE, working_color_space)
    }

    fn working_frame(rgba: &[u8], width: u32, height: u32) -> CpuColorFrame {
        CpuColorFrame::working(working_frame_from_normalized_rgba8(
            width,
            height,
            rgba,
            WorkingColorSpace::LinearRec709,
        ))
    }

    fn identity_media<'a>(frame: &'a CpuColorFrame) -> TimelineCompositeElement<'a> {
        TimelineCompositeElement::Media(TimelineMediaLayer {
            frame,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: compile_reference_effect_graph(&EffectRenderPlan::default())
                .expect("compile identity graph"),
            frame_seed: 0,
        })
    }

    fn identity_solid(color: Color) -> TimelineSolidColorLayer {
        TimelineSolidColorLayer {
            color,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: compile_reference_effect_graph(&EffectRenderPlan::default())
                .expect("compile identity graph"),
            frame_seed: 0,
        }
    }

    fn cpu_float_contract() -> EffectExecutionContract {
        EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: EffectDeterminism::Deterministic,
            state_model: EffectStateModel::Stateless,
            temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
            roi_propagation: EffectRoiPropagation::PixelLocal,
            resource_lifetime: EffectResourceLifetime::Frame,
            topology: EffectGraphTopology::LinearChain,
        }
    }

    fn custom_u8_graph(
        label: &str,
        domain_contract: EffectColorDomainContract,
        params: serde_json::Value,
        cache_policy: EffectCachePolicy,
        processor: CustomEffectRenderProcessor,
    ) -> Arc<CompiledEffectGraph> {
        static NEXT_DEFINITION: AtomicU64 = AtomicU64::new(1);
        let suffix = NEXT_DEFINITION.fetch_add(1, Ordering::Relaxed);
        let effect_type = EffectType::Plugin(format!("test.renderer.custom.{label}.{suffix}"));
        let determinism = match cache_policy {
            EffectCachePolicy::Deterministic => EffectDeterminism::Deterministic,
            EffectCachePolicy::FrameDependent => EffectDeterminism::FrameSeeded,
            EffectCachePolicy::Uncacheable => EffectDeterminism::Nondeterministic,
        };
        register_effect_definition(
            EffectDefinition::new(
                effect_type.key(),
                "Custom renderer test",
                Default::default(),
                domain_contract,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_U8,
                determinism,
                state_model: EffectStateModel::Stateless,
                temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
                roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
                resource_lifetime: EffectResourceLifetime::Frame,
                topology: EffectGraphTopology::LinearChain,
            })
            .with_custom_render_backend(
                Arc::new(move |_, _| Ok(Some(params.clone()))),
                None,
                cache_policy,
                processor,
            ),
        )
        .expect("register custom renderer test definition");
        PreparedEffectProgram::prepare(
            &[EffectNode::new(effect_type)],
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .expect("prepare custom renderer test program")
        .evaluate(mondrian_core::TimelineTime::ZERO)
        .expect("evaluate custom renderer test graph")
    }

    fn identity_graph_with_contract(
        label: &str,
        contract: EffectExecutionContract,
    ) -> Arc<CompiledEffectGraph> {
        static NEXT_DEFINITION: AtomicU64 = AtomicU64::new(1);
        let suffix = NEXT_DEFINITION.fetch_add(1, Ordering::Relaxed);
        let effect_type =
            EffectType::Plugin(format!("test.renderer.identity_admission.{label}.{suffix}"));
        register_effect_definition(
            EffectDefinition::new(
                effect_type.key(),
                "Identity admission test",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(contract)
            .with_graph_builder(Arc::new(|_, _, _| Ok(()))),
        )
        .expect("register identity test definition");
        PreparedEffectProgram::prepare(
            &[EffectNode::new(effect_type)],
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .expect("prepare identity test program")
        .evaluate(mondrian_core::TimelineTime::ZERO)
        .expect("evaluate identity test graph")
    }

    fn solid_plan_with_graph(
        intent: crate::TimelineRenderIntent,
        settings: crate::TimelineRenderSettings,
        effect_graph: Arc<CompiledEffectGraph>,
    ) -> crate::TimelineRenderPlan {
        crate::TimelineRenderPlan {
            position: mondrian_core::FramePosition::new(
                0,
                mondrian_core::types::Rational::new(1, 30),
            ),
            intent,
            settings,
            elements: vec![crate::TimelineRenderPlanElement::SolidColor(
                crate::TimelineSolidColorPlan {
                    placement: mondrian_core::timeline_data::TimelineClipExecutionRef {
                        sequence_id: mondrian_core::SequenceId::new(),
                        sequence_revision: mondrian_core::SequenceRevision::INITIAL,
                        clip_id: mondrian_core::ClipId::new(),
                        clip_time: mondrian_core::TimelineTime::ZERO,
                        endpoint:
                            mondrian_core::timeline_data::TimelineClipEndpointContext::Ordinary,
                    },
                    color: Color::WHITE,
                    opacity: 1.0,
                    blend_mode: BlendMode::Normal,
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_graph,
                    frame_seed: 0,
                },
            )],
            diagnostics: crate::TimelineEvaluationDiagnostics::default(),
        }
    }

    fn assert_current_frame_compositors_reject_identity_contract(
        elements: &[TimelineCompositeElement<'_>],
    ) {
        let mut encoded = Vec::new();
        let mut scratch = TimelineCompositeScratch::default();
        assert!(matches!(
            composite_timeline_elements_into(
                &mut encoded,
                1,
                1,
                elements,
                TimelineCompositeOptions::default(),
                &mut scratch,
            ),
            Err(EffectExecutionError::ExecutionContract(_))
        ));
        assert!(
            encoded.is_empty(),
            "admission must fail before the compositor writes output pixels"
        );

        let mut scratch = TimelineCompositeScratch::default();
        assert!(matches!(
            composite_timeline_elements_color_frame_with_diagnostics(
                1,
                1,
                elements,
                TimelineCompositeOptions::default(),
                test_color_runtime(WorkingColorSpace::LinearRec709),
                &mut scratch,
            ),
            Err(TimelineCompositeError::FloatEffect {
                reason: EffectFloatExecutionError::ExecutionContract(
                    mondrian_effects::EffectExecutionAdmissionError::ContinuitySessionRequired
                )
            })
        ));

        let mut scratch = TimelineCompositeScratch::default();
        let mut execution = TimelineCompositeExecutionDiagnostics::default();
        assert!(matches!(
            composite_supported_elements_to_working_frame(
                1,
                1,
                elements,
                TimelineCompositeOptions::default(),
                WorkingColorSpace::LinearRec709,
                test_color_runtime(WorkingColorSpace::LinearRec709),
                &mut scratch,
                &mut execution,
            ),
            Err(EffectFloatExecutionError::ExecutionContract(
                mondrian_effects::EffectExecutionAdmissionError::ContinuitySessionRequired
            ))
        ));
    }

    #[test]
    fn empty_program_frame_preserves_alpha_or_flattens_to_black_by_output_contract() {
        let mut scratch = TimelineCompositeScratch::default();
        let transparent = composite_timeline_elements(
            2,
            1,
            &[],
            TimelineCompositeOptions::default(),
            &mut scratch,
        )
        .expect("transparent empty program frame");
        assert_eq!(transparent, vec![0, 0, 0, 0, 0, 0, 0, 0]);

        let opaque = composite_timeline_elements(
            2,
            1,
            &[],
            TimelineCompositeOptions::opaque_black(),
            &mut scratch,
        )
        .expect("opaque empty program frame");
        assert_eq!(opaque, vec![0, 0, 0, 255, 0, 0, 0, 255]);
    }

    #[test]
    fn identity_pixel_fast_paths_never_bypass_execution_admission() {
        let forbidden = identity_graph_with_contract(
            "stateful",
            EffectExecutionContract {
                state_model: EffectStateModel::StatefulSequential,
                resource_lifetime: EffectResourceLifetime::ContinuitySession,
                ..cpu_float_contract()
            },
        );
        assert!(forbidden.graph().is_identity());
        let media = working_frame(&[20, 40, 80, 255], 1, 1);
        let valid_identity = compile_reference_effect_graph(&EffectRenderPlan::default())
            .expect("compile valid identity graph");

        assert_current_frame_compositors_reject_identity_contract(&[
            TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &media,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: Arc::clone(&forbidden),
                frame_seed: 0,
            }),
        ]);
        assert_current_frame_compositors_reject_identity_contract(&[
            TimelineCompositeElement::SolidColor(TimelineSolidColorLayer {
                color: Color::WHITE,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: Arc::clone(&forbidden),
                frame_seed: 0,
            }),
        ]);
        assert_current_frame_compositors_reject_identity_contract(&[
            TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &media,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: valid_identity,
                frame_seed: 0,
            }),
            TimelineCompositeElement::Adjustment(TimelineAdjustmentLayer {
                effect_graph: Arc::clone(&forbidden),
                opacity: 1.0,
                blend_mode: None,
                frame_seed: 0,
            }),
        ]);
        assert_current_frame_compositors_reject_identity_contract(&[
            TimelineCompositeElement::CrossDissolve(TimelineCrossDissolveLayer {
                left: TimelineTransitionInput::SolidColor(TimelineSolidColorLayer {
                    color: Color::BLACK,
                    opacity: 1.0,
                    blend_mode: BlendMode::Normal,
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_graph: forbidden,
                    frame_seed: 0,
                }),
                right: TimelineTransitionInput::Transparent,
                progress: 0.5,
            }),
        ]);
    }

    #[test]
    fn legal_identity_passthrough_remains_admitted_on_encoded_and_float_paths() {
        let media = working_frame(&[20, 40, 80, 255], 1, 1);
        let elements = [identity_media(&media)];
        let mut encoded = Vec::new();
        let mut scratch = TimelineCompositeScratch::default();
        composite_timeline_elements_into(
            &mut encoded,
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            &mut scratch,
        )
        .expect("encoded identity passthrough");
        assert_eq!(encoded, [20, 40, 80, 255]);

        let mut scratch = TimelineCompositeScratch::default();
        let mut execution = TimelineCompositeExecutionDiagnostics::default();
        let float = composite_supported_elements_to_working_frame(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            WorkingColorSpace::LinearRec709,
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
            &mut execution,
        )
        .expect("float identity passthrough");
        assert_eq!(float.data, media.rgba_f32().data);
    }

    #[test]
    fn typed_identity_passthrough_is_zero_copy_and_requires_no_compositor_working_set() {
        let media = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 2,
            height: 1,
            data: vec![[-0.25, 1.5, 0.4, 0.5], [3.0, -1.0, 0.75, 0.0]],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let elements = [identity_media(&media)];
        let mut scratch = TimelineCompositeScratch::default();
        scratch.reconfigure_cpu_working_set(TimelineCpuWorkingSetGrant {
            max_active_bytes: 0,
            max_retained_scratch_bytes: 0,
        });

        let output = composite_timeline_elements_color_frame_with_diagnostics(
            2,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("exact identity requires no compositor-owned allocation");

        assert!(output.frame.shares_storage_with(&media));
        assert_eq!(output.frame.rgba_f32().data, media.rgba_f32().data);
        assert_eq!(output.execution.zero_copy_identity_passthroughs, 1);
        assert_eq!(output.execution.direct_first_layer_initializations, 0);
        assert_eq!(output.execution.fused_first_two_full_frame_normal_blends, 0);
    }

    #[test]
    fn first_two_full_frame_normal_media_layers_fuse_without_color_drift() {
        let first = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 5,
            height: 1,
            data: vec![
                [-0.25, 1.5, 0.4, 0.0],
                [9.0, -8.0, 7.0, 1.0e-4],
                [3.0, -1.0, 0.75, 0.25],
                [0.1, 0.2, 0.3, 0.8],
                [4.0, 0.25, -0.5, 1.0],
            ],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let second = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 5,
            height: 1,
            data: vec![
                [2.0, 0.5, -0.5, 0.0],
                [-3.0, 4.0, 5.0, 1.0e-4],
                [-1.0, 1.25, 0.4, 0.5],
                [0.8, -0.2, 3.0, 0.1],
                [0.2, 0.4, 0.6, 1.0],
            ],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let identity = compile_reference_effect_graph(&EffectRenderPlan::default())
            .expect("compile identity graph");
        let first_opacity = 0.63;
        let second_opacity = 0.74;
        let elements = [
            TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &first,
                opacity: first_opacity,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: Arc::clone(&identity),
                frame_seed: 17,
            }),
            TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &second,
                opacity: second_opacity,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: identity,
                frame_seed: 29,
            }),
        ];

        let mut scratch = TimelineCompositeScratch::default();
        let output = composite_timeline_elements_color_frame_with_diagnostics(
            5,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("fused full-frame Normal composite");

        assert_eq!(output.execution.zero_copy_identity_passthroughs, 0);
        assert_eq!(output.execution.direct_first_layer_initializations, 1);
        assert_eq!(output.execution.fused_first_two_full_frame_normal_blends, 1);
        for (index, (first_pixel, second_pixel)) in
            first.rgba_f32().data.iter().zip(&second.rgba_f32().data).enumerate()
        {
            let base = blend_rgba_f32_pixel_seeded(
                [0.0, 0.0, 0.0, 0.0],
                *first_pixel,
                first_opacity,
                BlendMode::Normal,
                index as u32,
            );
            let expected = blend_rgba_f32_pixel_seeded(
                base,
                *second_pixel,
                second_opacity,
                BlendMode::Normal,
                index as u32,
            );
            let actual = output.frame.rgba_f32().data[index];
            for channel in 0..4 {
                assert!(
                    (actual[channel] - expected[channel]).abs() <= 1.0e-6,
                    "pixel={index}, channel={channel}, expected={expected:?}, actual={actual:?}"
                );
            }
        }
    }

    #[test]
    fn direct_first_layer_initialization_discards_hidden_rgb_like_canonical_source_over() {
        let media = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 2,
            height: 1,
            data: vec![[8.0, -4.0, 2.0, 0.0], [0.25, 0.5, 1.25, 0.5]],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let mut layer = match identity_media(&media) {
            TimelineCompositeElement::Media(layer) => layer,
            _ => unreachable!("identity fixture is media"),
        };
        layer.opacity = 0.999;

        let mut scratch = TimelineCompositeScratch::default();
        let output = composite_timeline_elements_color_frame_with_diagnostics(
            2,
            1,
            &[TimelineCompositeElement::Media(layer)],
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("direct first-layer composite");

        assert_eq!(output.execution.direct_first_layer_initializations, 1);
        for (index, source) in media.rgba_f32().data.iter().enumerate() {
            let expected = blend_rgba_f32_pixel_seeded(
                [0.0, 0.0, 0.0, 0.0],
                *source,
                0.999,
                BlendMode::Normal,
                index as u32,
            );
            assert_eq!(output.frame.rgba_f32().data[index], expected);
        }
    }

    #[test]
    fn first_two_layer_fusion_rejects_non_normal_contract() {
        let first = working_frame(&[32, 64, 128, 224], 1, 1);
        let second = working_frame(&[224, 96, 16, 192], 1, 1);
        let identity = compile_reference_effect_graph(&EffectRenderPlan::default())
            .expect("compile identity graph");
        let elements = [
            TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &first,
                opacity: 0.7,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: Arc::clone(&identity),
                frame_seed: 0,
            }),
            TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &second,
                opacity: 0.6,
                blend_mode: BlendMode::Multiply,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: identity,
                frame_seed: 0,
            }),
        ];

        let mut scratch = TimelineCompositeScratch::default();
        let output = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("non-Normal scalar composite");

        assert_eq!(output.execution.direct_first_layer_initializations, 1);
        assert_eq!(output.execution.fused_first_two_full_frame_normal_blends, 0);
        let mut base = first.rgba_f32().data[0];
        base[3] = (base[3] * 0.7).clamp(0.0, 1.0);
        let expected = blend_rgba_f32_pixel_seeded(
            base,
            second.rgba_f32().data[0],
            0.6,
            BlendMode::Multiply,
            0,
        );
        let actual = output.frame.rgba_f32().data[0];
        for channel in 0..4 {
            assert!((actual[channel] - expected[channel]).abs() <= 1.0e-6);
        }
    }

    #[test]
    fn identity_passthrough_rejects_any_operation_or_output_contract_change() {
        let media = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 1,
            height: 1,
            data: vec![[0.25, 0.5, 1.25, 0.5]],
            color_space: WorkingColorSpace::LinearRec709,
        });

        let mut opacity_layer = match identity_media(&media) {
            TimelineCompositeElement::Media(layer) => layer,
            _ => unreachable!("identity fixture is media"),
        };
        opacity_layer.opacity = 0.999;
        let mut scratch = TimelineCompositeScratch::default();
        let opacity_output = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &[TimelineCompositeElement::Media(opacity_layer)],
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("opacity composite");
        assert!(!opacity_output.frame.shares_storage_with(&media));
        assert_eq!(opacity_output.execution.zero_copy_identity_passthroughs, 0);
        assert_eq!(
            opacity_output.execution.direct_first_layer_initializations,
            1
        );
        let expected = blend_rgba_f32_pixel_seeded(
            [0.0, 0.0, 0.0, 0.0],
            media.rgba_f32().data[0],
            0.999,
            BlendMode::Normal,
            0,
        );
        for (actual, expected) in
            opacity_output.frame.rgba_f32().data[0].iter().zip(expected.iter())
        {
            assert!((*actual - *expected).abs() <= 1.0e-6);
        }

        let mut scratch = TimelineCompositeScratch::default();
        let opaque_output = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &[identity_media(&media)],
            TimelineCompositeOptions::opaque_black(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("opaque delivery composite");
        assert!(!opaque_output.frame.shares_storage_with(&media));
        assert_eq!(opaque_output.execution.zero_copy_identity_passthroughs, 0);

        let mut scratch = TimelineCompositeScratch::default();
        scratch.reconfigure_cpu_working_set(TimelineCpuWorkingSetGrant {
            max_active_bytes: 0,
            max_retained_scratch_bytes: 0,
        });
        assert!(matches!(
            composite_timeline_elements_color_frame_with_diagnostics(
                1,
                1,
                &[identity_media(&media)],
                TimelineCompositeOptions::opaque_black(),
                test_color_runtime(WorkingColorSpace::LinearRec709),
                &mut scratch,
            ),
            Err(TimelineCompositeError::CpuWorkingSet(
                TimelineCpuWorkingSetError::ActiveGrantExceeded { .. }
            ))
        ));

        let mut scratch = TimelineCompositeScratch::default();
        let resized_output = composite_timeline_elements_color_frame_with_diagnostics(
            2,
            1,
            &[identity_media(&media)],
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("extent-changing composite");
        assert!(!resized_output.frame.shares_storage_with(&media));
        assert_eq!(resized_output.execution.zero_copy_identity_passthroughs, 0);
    }

    #[test]
    fn compositor_rejects_mislabeled_working_frames_before_pixel_execution() {
        let media = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 1,
            height: 1,
            data: vec![[0.25, 0.5, 1.25, 1.0]],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let mut scratch = TimelineCompositeScratch::default();
        let error = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &[identity_media(&media)],
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec2020),
            &mut scratch,
        )
        .expect_err("working-space mismatch must not be relabeled");
        assert!(matches!(
            error,
            TimelineCompositeError::MediaFrameContractMismatch { .. }
        ));
    }

    #[test]
    fn specialized_normal_float_kernel_matches_scalar_reference() {
        let pixels = [
            ([0.0, 0.0, 0.0, 0.0], [-0.5, 1.5, 0.25, 0.0]),
            ([0.1, 0.2, 0.3, 0.25], [2.0, -1.0, 0.5, 0.5]),
            ([-0.3, 1.4, 0.0, 1.0], [0.7, 0.1, 2.5, 1.0]),
            ([0.8, 0.2, 0.4, 0.999], [0.1, 0.9, -0.2, 0.000_05]),
        ];
        for opacity in [0.0, 0.25, 0.75, 1.0] {
            for (base, blend) in pixels {
                let expected = blend_rgba_f32_pixel_seeded(
                    base,
                    blend,
                    opacity,
                    BlendMode::Normal,
                    0xdead_beef,
                );
                let actual = blend_normal_rgba_f32_pixel(base, blend, opacity);
                for channel in 0..4 {
                    assert!(
                        (actual[channel] - expected[channel]).abs() <= 1.0e-6,
                        "opacity={opacity}, channel={channel}, expected={expected:?}, actual={actual:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn u8_only_identity_is_rejected_before_working_pixels_are_quantized() {
        let media = working_frame(&[20, 40, 80, 255], 1, 1);
        let encoded_identity = identity_graph_with_contract(
            "encoded_only",
            EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_U8,
                ..cpu_float_contract()
            },
        );
        let elements = [TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &media,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: encoded_identity,
            frame_seed: 0,
        })];
        let mut scratch = TimelineCompositeScratch::default();
        let error = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect_err("encoded-only identity must not quantize the working composite");
        assert!(matches!(
            error,
            TimelineCompositeError::FloatEffect {
                reason: EffectFloatExecutionError::ExecutionContract(
                    EffectExecutionAdmissionError::ExecutionModeNotAdmitted {
                        backend: EffectProcessingBackend::Cpu,
                        precision: EffectWorkingPrecision::Float32,
                        ..
                    }
                )
            }
        ));
    }

    #[test]
    fn final_export_admission_rejects_implicit_u8_working_composite() {
        let encoded_identity = identity_graph_with_contract(
            "final_export_encoded_only",
            EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_U8,
                ..cpu_float_contract()
            },
        );
        let plan = solid_plan_with_graph(
            crate::TimelineRenderIntent::Export,
            crate::TimelineRenderSettings::export(),
            encoded_identity,
        );

        assert!(matches!(
            admit_timeline_render_plan_for_cpu_compositor(&plan),
            Err(TimelineCompositeError::FloatEffect {
                reason: EffectFloatExecutionError::ExecutionContract(
                    EffectExecutionAdmissionError::ExecutionModeNotAdmitted {
                        backend: EffectProcessingBackend::Cpu,
                        precision: EffectWorkingPrecision::Float32,
                        ..
                    }
                )
            })
        ));
    }

    #[test]
    fn interactive_preview_rejects_u8_working_composite_like_export() {
        let encoded_identity = identity_graph_with_contract(
            "preview_encoded_only",
            EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_U8,
                ..cpu_float_contract()
            },
        );
        let plan = solid_plan_with_graph(
            crate::TimelineRenderIntent::Preview,
            crate::TimelineRenderSettings::preview(0.5),
            encoded_identity,
        );

        assert!(matches!(
            admit_timeline_render_plan_for_cpu_compositor(&plan),
            Err(TimelineCompositeError::FloatEffect {
                reason: EffectFloatExecutionError::ExecutionContract(
                    EffectExecutionAdmissionError::ExecutionModeNotAdmitted {
                        backend: EffectProcessingBackend::Cpu,
                        precision: EffectWorkingPrecision::Float32,
                        ..
                    }
                )
            })
        ));
    }

    #[test]
    fn unreachable_adjustment_does_not_force_a_precision_route() {
        let encoded_identity = identity_graph_with_contract(
            "unreachable_encoded_adjustment",
            EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_U8,
                ..cpu_float_contract()
            },
        );
        let elements = [TimelineCompositeElement::Adjustment(
            TimelineAdjustmentLayer {
                effect_graph: encoded_identity,
                opacity: 1.0,
                blend_mode: None,
                frame_seed: 0,
            },
        )];
        let diagnostics = composite_path_diagnostics(&elements);
        assert_eq!(
            diagnostics.color_path(),
            TimelineCompositeColorPath::FloatLinear
        );
        assert_eq!(diagnostics.legacy_breakdown().total(), 0);

        let mut scratch = TimelineCompositeScratch::default();
        let frame = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("an Adjustment with no lower composited layer is unreachable");
        assert_eq!(frame.frame.rgba_f32().data, [[0.0, 0.0, 0.0, 0.0]]);
    }

    #[test]
    fn cross_dissolve_interpolates_opaque_inputs_in_working_linear_space() {
        let elements = [TimelineCompositeElement::CrossDissolve(
            TimelineCrossDissolveLayer {
                left: TimelineTransitionInput::SolidColor(identity_solid(Color {
                    r: 1.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                })),
                right: TimelineTransitionInput::SolidColor(identity_solid(Color {
                    r: 0.0,
                    g: 0.0,
                    b: 1.0,
                    a: 1.0,
                })),
                progress: 0.5,
            },
        )];
        let mut scratch = TimelineCompositeScratch::default();
        let frame = composite_timeline_elements_color_frame(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("Cross Dissolve");

        let pixel = frame.rgba_f32().data[0];
        assert!((pixel[0] - 0.5).abs() < 1.0e-6);
        assert_eq!(pixel[1], 0.0);
        assert!((pixel[2] - 0.5).abs() < 1.0e-6);
        assert_eq!(pixel[3], 1.0);
        assert_eq!(
            composite_path_diagnostics(&elements).effect_gpu_blockers,
            0,
            "a supported typed Transition must not remain a synthetic GPU blocker"
        );
    }

    #[test]
    fn cross_dissolve_uses_premultiplied_coverage_without_dark_fringe() {
        let elements = [TimelineCompositeElement::CrossDissolve(
            TimelineCrossDissolveLayer {
                left: TimelineTransitionInput::SolidColor(identity_solid(Color {
                    r: 1.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.5,
                })),
                right: TimelineTransitionInput::Transparent,
                progress: 0.5,
            },
        )];
        let mut scratch = TimelineCompositeScratch::default();
        let frame = composite_timeline_elements_color_frame(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("coverage-correct Cross Dissolve");

        let pixel = frame.rgba_f32().data[0];
        assert!(
            (pixel[0] - 1.0).abs() < 1.0e-6,
            "red must not darken: {pixel:?}"
        );
        assert_eq!(pixel[1], 0.0);
        assert_eq!(pixel[2], 0.0);
        assert!((pixel[3] - 0.25).abs() < 1.0e-6);
    }

    #[test]
    fn media_effects_are_applied_before_compositing() {
        let mut scratch = TimelineCompositeScratch::default();
        let media = working_frame(&[120, 80, 40, 255], 1, 1);
        let output = composite_timeline_elements(
            1,
            1,
            &[TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &media,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: compile_reference_effect_graph(&EffectRenderPlan {
                    ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                        exposure: 0.0,
                        contrast: 1.0,
                        saturation: 0.0,
                        working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                    }],
                })
                .expect("compile effect graph"),
                frame_seed: 0,
            })],
            TimelineCompositeOptions::default(),
            &mut scratch,
        )
        .expect("composite media effect");

        assert_eq!(output[0], output[1]);
        assert_eq!(output[1], output[2]);
        assert_eq!(output[3], 255);
    }

    #[test]
    fn custom_render_ops_flow_through_shared_compositor() {
        let effect_graph = custom_u8_graph(
            "invert",
            EffectColorDomainContract::SCENE_LINEAR,
            serde_json::json!({}),
            EffectCachePolicy::Deterministic,
            Arc::new(|buffer, _, _, _, _| {
                for px in buffer.chunks_exact_mut(4) {
                    px[0] = 255u8.saturating_sub(px[0]);
                    px[1] = 255u8.saturating_sub(px[1]);
                    px[2] = 255u8.saturating_sub(px[2]);
                }
                Ok(())
            }),
        );

        let mut scratch = TimelineCompositeScratch::default();
        let media = working_frame(&[10, 20, 30, 255], 1, 1);
        let output = composite_timeline_elements(
            1,
            1,
            &[TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &media,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph,
                frame_seed: 0,
            })],
            TimelineCompositeOptions::default(),
            &mut scratch,
        )
        .expect("composite custom effect");

        assert_eq!(&output[0..4], &[245, 235, 225, 255]);
    }

    #[test]
    fn adjustment_affects_only_layers_below_it() {
        let mut scratch = TimelineCompositeScratch::default();
        let lower = working_frame(&[255, 0, 0, 255, 255, 0, 0, 255], 2, 1);
        let upper = working_frame(&[0, 0, 0, 0, 0, 255, 0, 255], 2, 1);
        let output = composite_timeline_elements(
            2,
            1,
            &[
                identity_media(&lower),
                TimelineCompositeElement::Adjustment(TimelineAdjustmentLayer {
                    effect_graph: compile_reference_effect_graph(&EffectRenderPlan {
                        ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                            exposure: 0.0,
                            contrast: 1.0,
                            saturation: 0.0,
                            working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                        }],
                    })
                    .expect("compile adjustment graph"),
                    opacity: 1.0,
                    blend_mode: Some(BlendMode::Normal),
                    frame_seed: 0,
                }),
                identity_media(&upper),
            ],
            TimelineCompositeOptions::default(),
            &mut scratch,
        )
        .expect("composite adjustment stack");

        assert_eq!(&output[0..4], &[54, 54, 54, 255]);
        assert_eq!(&output[4..8], &[0, 255, 0, 255]);
    }

    #[test]
    fn media_blend_mode_is_applied_during_compositing() {
        let mut scratch = TimelineCompositeScratch::default();
        let base = working_frame(&[128, 64, 32, 255], 1, 1);
        let blend = working_frame(&[64, 192, 128, 255], 1, 1);
        let output = composite_timeline_elements(
            1,
            1,
            &[
                identity_media(&base),
                TimelineCompositeElement::Media(TimelineMediaLayer {
                    frame: &blend,
                    opacity: 1.0,
                    blend_mode: BlendMode::Multiply,
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_graph: compile_reference_effect_graph(&EffectRenderPlan::default())
                        .expect("compile identity graph"),
                    frame_seed: 0,
                }),
            ],
            TimelineCompositeOptions::default(),
            &mut scratch,
        )
        .expect("composite blend mode");

        assert_eq!(&output[0..4], &[32, 48, 16, 255]);
    }

    #[test]
    fn float_linear_compositor_matches_normal_single_layer_and_preserves_alpha() {
        let mut scratch = TimelineCompositeScratch::default();
        let media = working_frame(&[64, 128, 192, 255], 1, 1);
        let frame = composite_timeline_elements_color_frame(
            1,
            1,
            &[TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &media,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: compile_reference_effect_graph(&EffectRenderPlan::default())
                    .expect("compile identity graph"),
                frame_seed: 0,
            })],
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("composite float media");
        assert_eq!(frame.descriptor().domain, crate::ColorFrameDomain::Working);
        let output = frame
            .rgba_f32()
            .data
            .iter()
            .flat_map(|pixel| pixel.map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8))
            .collect::<Vec<_>>();

        assert_eq!(output[3], 255);
        assert!((output[0] as i16 - 64).abs() <= 1);
        assert!((output[1] as i16 - 128).abs() <= 1);
        assert!((output[2] as i16 - 192).abs() <= 1);
        assert!(scratch.media_source.is_empty());
        assert!(scratch.media_effect.is_empty());
    }

    #[test]
    fn transparent_program_canvas_preserves_partial_coverage_and_uncovered_pixels() {
        let mut scratch = TimelineCompositeScratch::default();
        let media = working_frame(&[255, 0, 0, 128], 1, 1);
        let frame = composite_timeline_elements_color_frame(
            2,
            1,
            &[identity_media(&media)],
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("composite transparent canvas");
        let pixels = &frame.rgba_f32().data;

        assert!((pixels[0][0] - 1.0).abs() <= 1.0 / 255.0);
        assert_eq!(pixels[0][1], 0.0);
        assert_eq!(pixels[0][2], 0.0);
        assert!((pixels[0][3] - 128.0 / 255.0).abs() <= 1.0 / 255.0);
        assert_eq!(pixels[1], [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn float_linear_compositor_preserves_extended_solid_color_without_rgba8_scratch() {
        let mut scratch = TimelineCompositeScratch::default();
        let frame = composite_timeline_elements_color_frame(
            1,
            1,
            &[TimelineCompositeElement::SolidColor(
                TimelineSolidColorLayer {
                    color: Color { r: 1.25, g: 0.5, b: 0.125, a: 1.0 },
                    opacity: 1.0,
                    blend_mode: BlendMode::Normal,
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_graph: compile_reference_effect_graph(&EffectRenderPlan::default())
                        .expect("compile identity graph"),
                    frame_seed: 0,
                },
            )],
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("composite extended solid");

        let px = frame.rgba_f32().data[0];
        assert_eq!(
            frame.descriptor().encoding,
            crate::ColorFrameEncoding::LinearFloat
        );
        assert!((px[0] - 1.25).abs() <= f32::EPSILON);
        assert!((px[1] - 0.5).abs() <= f32::EPSILON);
        assert!((px[2] - 0.125).abs() <= f32::EPSILON);
        assert!((px[3] - 1.0).abs() <= f32::EPSILON);
        assert!(scratch.solid_fill.is_empty());
        assert!(scratch.media_effect.is_empty());
    }

    #[test]
    fn float_linear_compositor_runs_color_adjust_effect_without_rgba8_scratch() {
        let mut scratch = TimelineCompositeScratch::default();
        let media = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 1,
            height: 1,
            data: vec![[1.25, 0.25, 0.125, 1.0]],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let frame = composite_timeline_elements_color_frame(
            1,
            1,
            &[TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &media,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: compile_reference_effect_graph(&EffectRenderPlan {
                    ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                        exposure: 1.0,
                        contrast: 1.0,
                        saturation: 1.0,
                        working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                    }],
                })
                .expect("compile float color adjust"),
                frame_seed: 0,
            })],
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("composite color adjustment");

        let px = frame.rgba_f32().data[0];
        assert!((px[0] - 2.5).abs() <= 1.0e-6);
        assert!((px[1] - 0.5).abs() <= 1.0e-6);
        assert!((px[2] - 0.25).abs() <= 1.0e-6);
        assert!((px[3] - 1.0).abs() <= f32::EPSILON);
        assert!(scratch.media_source.is_empty());
        assert!(scratch.media_effect.is_empty());
    }

    #[test]
    fn float_linear_compositor_runs_normal_adjustment_without_rgba8_scratch() {
        let mut scratch = TimelineCompositeScratch::default();
        let media = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 1,
            height: 1,
            data: vec![[1.25, 0.25, 0.125, 1.0]],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let frame = composite_timeline_elements_color_frame(
            1,
            1,
            &[
                identity_media(&media),
                TimelineCompositeElement::Adjustment(TimelineAdjustmentLayer {
                    effect_graph: compile_reference_effect_graph(&EffectRenderPlan {
                        ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                            exposure: 1.0,
                            contrast: 1.0,
                            saturation: 1.0,
                            working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                        }],
                    })
                    .expect("compile adjustment"),
                    opacity: 0.5,
                    blend_mode: Some(BlendMode::Normal),
                    frame_seed: 0,
                }),
            ],
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("composite adjustment");

        let px = frame.rgba_f32().data[0];
        assert!((px[0] - 1.875).abs() <= 1.0e-6);
        assert!((px[1] - 0.375).abs() <= 1.0e-6);
        assert!((px[2] - 0.1875).abs() <= 1.0e-6);
        assert!((px[3] - 1.0).abs() <= f32::EPSILON);
        assert!(scratch.media_source.is_empty());
        assert!(scratch.media_effect.is_empty());
        assert!(scratch.adjustment.is_empty());
    }

    #[test]
    fn float_linear_compositor_runs_media_and_solid_blend_modes_without_legacy_fallback() {
        let mut scratch = TimelineCompositeScratch::default();
        let media = working_frame(&[128, 96, 64, 255], 1, 1);
        let overlay = working_frame(&[64, 192, 128, 255], 1, 1);
        let elements = [
            identity_media(&media),
            TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &overlay,
                opacity: 0.75,
                blend_mode: BlendMode::Multiply,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: compile_reference_effect_graph(&EffectRenderPlan::default())
                    .expect("compile identity graph"),
                frame_seed: 0,
            }),
            TimelineCompositeElement::SolidColor(TimelineSolidColorLayer {
                color: Color { r: 0.25, g: 0.5, b: 1.0, a: 1.0 },
                opacity: 0.5,
                blend_mode: BlendMode::Screen,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: compile_reference_effect_graph(&EffectRenderPlan::default())
                    .expect("compile identity graph"),
                frame_seed: 0,
            }),
        ];

        let output = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("composite blend modes");

        assert_eq!(output.diagnostics.float_linear_composites, 1);
        assert_eq!(output.diagnostics.legacy_rgba8_composites, 0);
        assert_eq!(output.diagnostics.legacy_media_blend_mode, 0);
        assert_eq!(output.diagnostics.legacy_solid_blend_mode, 0);
        assert!(scratch.media_source.is_empty());
        assert!(scratch.solid_fill.is_empty());
    }

    #[test]
    fn float_linear_compositor_runs_adjustment_blend_modes_without_rgba8_scratch() {
        let mut float_scratch = TimelineCompositeScratch::default();
        let media = working_frame(&[255, 0, 0, 255], 1, 1);
        let elements = [
            identity_media(&media),
            TimelineCompositeElement::Adjustment(TimelineAdjustmentLayer {
                effect_graph: compile_reference_effect_graph(&EffectRenderPlan {
                    ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                        exposure: 0.0,
                        contrast: 1.0,
                        saturation: 0.0,
                        working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                    }],
                })
                .expect("compile adjustment"),
                opacity: 1.0,
                blend_mode: Some(BlendMode::Multiply),
                frame_seed: 0,
            }),
        ];
        let output = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut float_scratch,
        )
        .expect("composite adjustment blend mode");

        let diagnostics = output.diagnostics;
        assert_eq!(diagnostics.float_linear_composites, 1);
        assert_eq!(diagnostics.legacy_rgba8_composites, 0);
        assert_eq!(diagnostics.legacy_adjustment_blend_mode, 0);
        assert_eq!(diagnostics.legacy_adjustment_effect, 0);
        assert!(output.frame.rgba_f32().data[0][0] < media.rgba_f32().data[0][0]);
        assert!(float_scratch.media_source.is_empty());
        assert!(float_scratch.adjustment.is_empty());
    }

    #[test]
    fn float_linear_compositor_runs_builtin_spatial_effects_without_rgba8_fallback() {
        let mut float_scratch = TimelineCompositeScratch::default();
        let media = working_frame(&[120, 80, 40, 255], 1, 1);
        let builtin_graph = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![mondrian_effects::EffectRenderOp::GaussianBlur { radius: 1.0 }],
        })
        .expect("compile built-in effect");
        let elements = [
            TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &media,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: builtin_graph.clone(),
                frame_seed: 0,
            }),
            TimelineCompositeElement::SolidColor(TimelineSolidColorLayer {
                color: Color { r: 1.5, g: 0.25, b: 0.125, a: 1.0 },
                opacity: 0.5,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: builtin_graph,
                frame_seed: 0,
            }),
        ];

        let output = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut float_scratch,
        )
        .expect("composite spatial effects");

        assert_eq!(output.diagnostics.float_linear_composites, 1);
        assert_eq!(output.diagnostics.legacy_rgba8_composites, 0);
        assert_eq!(output.diagnostics.legacy_media_effect, 0);
        assert_eq!(output.diagnostics.legacy_solid_effect, 0);
        assert!(output.frame.rgba_f32().data[0][0] > 0.8);
    }

    #[test]
    fn float_linear_compositor_runs_clip_masks_without_rgba8_fallback() {
        let mut scratch = TimelineCompositeScratch::default();
        let graph = mondrian_effects::EffectRenderGraph {
            nodes: vec![
                mondrian_effects::EffectGraphNode {
                    id: mondrian_effects::EffectGraphNodeId(0),
                    kind: mondrian_effects::EffectGraphNodeKind::Source,
                },
                mondrian_effects::EffectGraphNode {
                    id: mondrian_effects::EffectGraphNodeId(1),
                    kind: mondrian_effects::EffectGraphNodeKind::MaskSource {
                        shape: mondrian_effects::MaskShape::Rectangle {
                            x: 0.0,
                            y: 0.0,
                            width: 1.0,
                            height: 1.0,
                            corner_radius: 0.0,
                        },
                        feather: 0.0,
                        expansion: 0.0,
                        opacity: 0.25,
                    },
                },
                mondrian_effects::EffectGraphNode {
                    id: mondrian_effects::EffectGraphNodeId(2),
                    kind: mondrian_effects::EffectGraphNodeKind::Mask {
                        input: mondrian_effects::EffectGraphNodeId(0),
                        mask: mondrian_effects::EffectGraphNodeId(1),
                        invert: false,
                        mask_op: mondrian_effects::MaskOp::Add,
                    },
                },
            ],
            output: Some(mondrian_effects::EffectGraphNodeId(2)),
        };
        let effect_graph =
            mondrian_effects::compile_reference_render_graph(graph).expect("compile mask graph");
        let elements = [TimelineCompositeElement::SolidColor(
            TimelineSolidColorLayer {
                color: Color { r: 2.0, g: -0.25, b: 0.5, a: 1.0 },
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph,
                frame_seed: 0,
            },
        )];

        let output = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("composite clip mask");
        let pixel = output.frame.rgba_f32().data[0];

        assert_eq!(output.diagnostics.float_linear_composites, 1);
        assert_eq!(output.diagnostics.legacy_rgba8_composites, 0);
        assert_eq!(output.diagnostics.legacy_solid_effect, 0);
        assert!((pixel[0] - 2.0).abs() <= 1.0e-6);
        assert!((pixel[1] + 0.25).abs() <= 1.0e-6);
        assert!((pixel[3] - 0.25).abs() <= f32::EPSILON);
    }

    #[test]
    fn float_linear_compositor_applies_solid_affine_transform() {
        let mut scratch = TimelineCompositeScratch::default();
        let effect_graph = compile_reference_effect_graph(&EffectRenderPlan::default())
            .expect("compile identity graph");
        let elements = [TimelineCompositeElement::SolidColor(
            TimelineSolidColorLayer {
                color: Color { r: 2.0, g: 0.25, b: 0.125, a: 1.0 },
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 1.0, 0.0, 1.0, 0.0],
                effect_graph,
                frame_seed: 0,
            },
        )];

        let output = composite_timeline_elements_color_frame(
            2,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("composite solid transform");
        let pixels = &output.rgba_f32().data;

        assert_eq!(pixels[0], [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(pixels[1], [2.0, 0.25, 0.125, 1.0]);
    }

    #[test]
    fn diagnostics_report_custom_effect_that_would_require_forbidden_rgba8() {
        let effect_graph = custom_u8_graph(
            "rgba8-only",
            EffectColorDomainContract::SCENE_LINEAR,
            serde_json::json!({}),
            EffectCachePolicy::Deterministic,
            Arc::new(|_buffer, _width, _height, _params, _frame_seed| Ok(())),
        );
        let media = working_frame(&[120, 80, 40, 255], 1, 1);
        let elements = [TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &media,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            frame_seed: 0,
        })];

        let diagnostics = composite_path_diagnostics(&elements);
        assert_eq!(diagnostics.legacy_rgba8_composites, 1);
        assert_eq!(diagnostics.legacy_media_effect, 1);
    }

    #[test]
    fn missing_custom_processor_is_rejected_before_composite_execution() {
        assert!(
            compile_reference_effect_graph(&EffectRenderPlan {
                ops: vec![mondrian_effects::EffectRenderOp::Custom {
                    key: "test.custom.missing-processor".to_owned(),
                    params: serde_json::json!({}),
                    cache_key: None,
                    cache_policy: mondrian_effects::EffectCachePolicy::Deterministic,
                    processor: None,
                }],
            })
            .is_none(),
            "a reachable Custom node without an immutable processor binding must not produce executable IR"
        );
    }

    #[test]
    fn unavailable_effect_domain_processor_blocks_instead_of_falling_back_to_rgba8() {
        let media = working_frame(&[120, 80, 40, 255], 1, 1);
        let display_domain = mondrian_effects::EffectColorDomain::DisplayEncodedRgb {
            color_space: mondrian_core::ColorSpace::Rec709,
        };
        let effect_graph = Arc::new(
            mondrian_effects::compile_reference_effect_graph_in_domain(
                &EffectRenderPlan {
                    ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                        exposure: 0.25,
                        contrast: 1.0,
                        saturation: 1.0,
                        working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                    }],
                },
                mondrian_effects::EffectColorDomainContract::preserving(display_domain),
            )
            .expect("compile display-domain graph"),
        );
        let elements = [TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &media,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            frame_seed: 0,
        })];
        let mut scratch = TimelineCompositeScratch::default();
        let unavailable_engine = pinned_custom_engine(mondrian_core::OcioConfigSource::Builtin {
            name: "test.invalid.effect-domain-config".to_owned(),
        });

        let error = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            TimelineEffectColorRuntime::new(&unavailable_engine, WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect_err("unavailable OCIO processor must fail composite");

        assert!(matches!(error, TimelineCompositeError::FloatEffect { .. }));
        assert_eq!(
            composite_timeline_elements(
                1,
                1,
                &elements,
                TimelineCompositeOptions::default(),
                &mut scratch,
            ),
            Err(
                mondrian_effects::EffectExecutionError::ColorDomainConversionRequired {
                    transitions: 2,
                }
            )
        );
    }

    #[test]
    fn stock_ocio_runtime_executes_display_domain_effect_and_returns_to_working_space() {
        let engine = ColorEngine::default();
        engine.ensure_loaded().expect("load Mondrian Standard OCIO package");
        let media = working_frame(&[120, 80, 40, 255], 1, 1);
        let input = media.rgba_f32().data[0];
        let display_domain =
            EffectColorDomain::DisplayEncodedRgb { color_space: mondrian_core::ColorSpace::Rec709 };
        let exposure = 0.25;
        let effect_graph = Arc::new(
            mondrian_effects::compile_reference_effect_graph_in_domain(
                &EffectRenderPlan {
                    ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                        exposure,
                        contrast: 1.0,
                        saturation: 1.0,
                        working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                    }],
                },
                mondrian_effects::EffectColorDomainContract::preserving(display_domain),
            )
            .expect("compile display-domain graph"),
        );
        let elements = [TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &media,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            frame_seed: 0,
        })];
        let mut scratch = TimelineCompositeScratch::default();

        let output = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            TimelineEffectColorRuntime::new(&engine, WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("execute display-domain effect");

        let mut expected = [input];
        engine
            .convert_identity_float(
                expected.as_flattened_mut(),
                WorkingColorSpace::LinearRec709.into(),
                mondrian_core::ColorSpace::Rec709.into(),
            )
            .expect("convert working to display encoded");
        for channel in &mut expected[0][..3] {
            *channel *= 2.0f32.powf(exposure);
        }
        engine
            .convert_identity_float(
                expected.as_flattened_mut(),
                mondrian_core::ColorSpace::Rec709.into(),
                WorkingColorSpace::LinearRec709.into(),
            )
            .expect("convert display encoded to working");

        assert_eq!(
            output.diagnostics.color_path(),
            TimelineCompositeColorPath::FloatLinear
        );
        assert_eq!(output.diagnostics.blocked_color_domain_composites, 0);
        assert_eq!(output.diagnostics.blocked_media_effect_domain, 0);
        assert_eq!(output.diagnostics.legacy_rgba8_composites, 0);
        let actual = output.frame.rgba_f32().data[0];
        for channel in 0..4 {
            assert!(
                (actual[channel] - expected[0][channel]).abs() <= 2.0e-5,
                "channel {channel}: actual={} expected={}",
                actual[channel],
                expected[0][channel]
            );
        }
    }

    #[test]
    fn display_domain_graph_without_float_abi_stays_blocked_instead_of_claiming_legacy() {
        let engine = ColorEngine::default();
        let media = working_frame(&[120, 80, 40, 255], 1, 1);
        let display_domain =
            EffectColorDomain::DisplayEncodedRgb { color_space: mondrian_core::ColorSpace::Rec709 };
        let effect_graph = custom_u8_graph(
            "display-rgba8-only",
            EffectColorDomainContract::preserving(display_domain),
            serde_json::json!({}),
            EffectCachePolicy::Deterministic,
            Arc::new(|_buffer, _width, _height, _params, _frame_seed| Ok(())),
        );
        let elements = [TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &media,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            frame_seed: 0,
        })];

        let error = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            TimelineEffectColorRuntime::new(&engine, WorkingColorSpace::LinearRec709),
            &mut TimelineCompositeScratch::default(),
        )
        .expect_err("custom display-domain graph must fail closed");

        assert_eq!(
            error,
            TimelineCompositeError::EffectDomainBlocked {
                media_effect: 1,
                solid_effect: 0,
                adjustment_effect: 0,
            }
        );
    }

    #[test]
    fn float_linear_compositor_reports_clean_float_path_diagnostics() {
        let mut scratch = TimelineCompositeScratch::default();
        let media = working_frame(&[64, 128, 192, 255], 1, 1);
        let elements = [identity_media(&media)];

        let output = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("composite clean float path");

        assert_eq!(output.diagnostics.elements, 1);
        assert_eq!(output.diagnostics.float_linear_composites, 1);
        assert_eq!(output.diagnostics.legacy_rgba8_composites, 0);
        assert!(!output.diagnostics.uses_legacy_rgba8());
        assert!(scratch.media_source.is_empty());
    }

    #[test]
    fn composite_color_path_summary_reports_clean_float_linear_path() {
        let diagnostics = TimelineCompositeDiagnostics {
            elements: 2,
            float_linear_composites: 2,
            ..TimelineCompositeDiagnostics::default()
        };

        let summary = diagnostics.color_path_summary();

        assert_eq!(summary.path, TimelineCompositeColorPath::FloatLinear);
        assert_eq!(summary.elements, 2);
        assert_eq!(summary.composite_plans(), 2);
        assert!(summary.legacy_breakdown.is_empty());
        assert!(summary.is_fully_float_linear());
        assert!(!diagnostics.uses_legacy_rgba8());
    }

    #[test]
    fn composite_color_path_summary_reports_structured_legacy_reasons() {
        let diagnostics = TimelineCompositeDiagnostics {
            elements: 4,
            float_linear_composites: 1,
            legacy_rgba8_composites: 1,
            legacy_media_transform: 1,
            legacy_solid_effect: 2,
            ..TimelineCompositeDiagnostics::default()
        };

        let summary = diagnostics.color_path_summary();

        assert_eq!(summary.path, TimelineCompositeColorPath::LegacyRgba8);
        assert_eq!(summary.composite_plans(), 2);
        assert_eq!(summary.legacy_breakdown.media_transform, 1);
        assert_eq!(summary.legacy_breakdown.solid_effect, 2);
        assert_eq!(summary.legacy_breakdown.total(), 3);
        assert!(!summary.is_fully_float_linear());
        assert!(diagnostics.uses_legacy_rgba8());
    }

    #[test]
    fn float_linear_compositor_handles_non_identity_transform() {
        let mut scratch = TimelineCompositeScratch::default();
        let media = working_frame(
            &[
                64, 128, 192, 255, 100, 150, 200, 255, 50, 100, 150, 255, 200, 50, 100, 255,
            ],
            2,
            2,
        );
        let elements = [TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &media,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [2.0, 0.0, 0.0, 0.0, 2.0, 0.0],
            effect_graph: compile_reference_effect_graph(&EffectRenderPlan::default())
                .expect("compile identity graph"),
            frame_seed: 0,
        })];

        let output = composite_timeline_elements_color_frame_with_diagnostics(
            2,
            2,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect("composite transformed media");

        assert_eq!(output.diagnostics.float_linear_composites, 1);
        assert_eq!(output.diagnostics.legacy_rgba8_composites, 0);
        assert_eq!(output.diagnostics.legacy_media_transform, 0);
        assert!(!output.diagnostics.uses_legacy_rgba8());
    }

    #[test]
    fn float_linear_compositor_deterministic_across_identity_and_scale() {
        let mut scratch_a = TimelineCompositeScratch::default();
        let mut scratch_b = TimelineCompositeScratch::default();
        let media = working_frame(
            &[
                100, 150, 200, 255, 50, 100, 150, 255, 200, 50, 100, 255, 150, 200, 50, 255,
            ],
            2,
            2,
        );
        let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let scale_1x = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

        let elements_a = [TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &media,
            opacity: 0.8,
            blend_mode: BlendMode::Multiply,
            transform: identity,
            effect_graph: compile_reference_effect_graph(&EffectRenderPlan::default())
                .expect("compile identity graph"),
            frame_seed: 42,
        })];
        let elements_b = [TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &media,
            opacity: 0.8,
            blend_mode: BlendMode::Multiply,
            transform: scale_1x,
            effect_graph: compile_reference_effect_graph(&EffectRenderPlan::default())
                .expect("compile identity graph"),
            frame_seed: 42,
        })];

        let out_a = composite_timeline_elements_color_frame_with_diagnostics(
            2,
            2,
            &elements_a,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch_a,
        )
        .expect("composite identity transform");
        let out_b = composite_timeline_elements_color_frame_with_diagnostics(
            2,
            2,
            &elements_b,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch_b,
        )
        .expect("composite equivalent scale");

        assert_eq!(out_a.frame.rgba_f32().data, out_b.frame.rgba_f32().data);
    }

    #[test]
    fn composite_color_path_summary_fails_closed_on_legacy_reason_mismatch() {
        let diagnostics = TimelineCompositeDiagnostics {
            float_linear_composites: 1,
            legacy_media_effect: 1,
            ..TimelineCompositeDiagnostics::default()
        };

        let summary = diagnostics.color_path_summary();

        assert_eq!(summary.path, TimelineCompositeColorPath::LegacyRgba8);
        assert_eq!(summary.legacy_rgba8_composites, 0);
        assert_eq!(summary.legacy_breakdown.total(), 1);
        assert!(diagnostics.uses_legacy_rgba8());
    }

    #[test]
    fn compositor_scratch_owns_gpu_plan_residency_and_generation_barrier() {
        let graph = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![mondrian_effects::EffectRenderOp::Grain { amount: 0.2 }],
        })
        .expect("compile GPU-capable graph");
        let mut scratch = TimelineCompositeScratch::default();
        scratch.reconfigure_effect_execution(EffectExecutionSessionConfig {
            max_gpu_plan_entries: 4,
            max_gpu_plan_bytes: 1024 * 1024,
            ..EffectExecutionSessionConfig::uncached(1024)
        });

        let first = scratch.get_or_lower_effect_gpu_plan(&graph).expect("first plan");
        let second = scratch.get_or_lower_effect_gpu_plan(&graph).expect("cached plan");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(scratch.effect_execution_diagnostics().gpu_plan_entries, 1);

        scratch.bind_effect_execution_generation(1);
        assert_eq!(
            scratch.effect_execution_diagnostics().gpu_plan_entries,
            1,
            "generation rotation retains frame-independent GPU-plan residency so scrubbing does not re-lower every graph"
        );
        let generation_retained =
            scratch.get_or_lower_effect_gpu_plan(&graph).expect("generation-retained plan");
        assert!(
            Arc::ptr_eq(&generation_retained, &second),
            "the retained plan is reused, not re-lowered"
        );
    }

    #[test]
    fn cross_dissolve_working_set_counts_three_coexisting_canvases() {
        let elements = [TimelineCompositeElement::CrossDissolve(
            TimelineCrossDissolveLayer {
                left: TimelineTransitionInput::SolidColor(identity_solid(Color {
                    r: 1.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                })),
                right: TimelineTransitionInput::Transparent,
                progress: 0.5,
            },
        )];

        let estimate = estimate_timeline_cpu_working_set(
            2,
            2,
            &elements,
            TimelineCpuCompositePrecision::Float32,
        )
        .expect("estimate");

        assert_eq!(estimate.active_bytes, 2 * 2 * 16 * 3);
        assert_eq!(estimate.retained_scratch_bytes, 0);

        let mut scratch = TimelineCompositeScratch::default();
        scratch.reconfigure_cpu_working_set(TimelineCpuWorkingSetGrant {
            max_active_bytes: estimate.active_bytes - 1,
            max_retained_scratch_bytes: u64::MAX,
        });
        let error = composite_timeline_elements_color_frame_with_diagnostics(
            2,
            2,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect_err("insufficient active grant must fail before compositing");

        assert!(matches!(
            error,
            TimelineCompositeError::CpuWorkingSet(
                TimelineCpuWorkingSetError::ActiveGrantExceeded {
                    required_bytes: 192,
                    granted_bytes: 191,
                    precision: TimelineCpuCompositePrecision::Float32,
                }
            )
        ));
    }

    #[test]
    fn transformed_solid_working_set_requires_bounded_retained_scratch() {
        let mut solid = identity_solid(Color { r: 0.25, g: 0.5, b: 0.75, a: 1.0 });
        solid.transform = [1.0, 0.0, 0.5, 0.0, 1.0, 0.0];
        let elements = [TimelineCompositeElement::SolidColor(solid)];
        let estimate = estimate_timeline_cpu_working_set(
            2,
            2,
            &elements,
            TimelineCpuCompositePrecision::Float32,
        )
        .expect("estimate");
        assert_eq!(estimate.retained_scratch_bytes, 2 * 2 * 16);

        let mut scratch = TimelineCompositeScratch::default();
        scratch.reconfigure_cpu_working_set(TimelineCpuWorkingSetGrant {
            max_active_bytes: u64::MAX,
            max_retained_scratch_bytes: estimate.retained_scratch_bytes - 1,
        });
        let error = composite_timeline_elements_color_frame_with_diagnostics(
            2,
            2,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        )
        .expect_err("insufficient retained grant must fail before compositing");

        assert!(matches!(
            error,
            TimelineCompositeError::CpuWorkingSet(
                TimelineCpuWorkingSetError::RetainedGrantExceeded {
                    required_bytes: 64,
                    granted_bytes: 63,
                    precision: TimelineCpuCompositePrecision::Float32,
                }
            )
        ));
    }
}
