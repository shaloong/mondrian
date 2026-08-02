use crate::adjustment::{
    apply_render_op, apply_render_op_f32, blend_adjustment_result, blend_rgba_f32_pixel_seeded,
    blend_rgba_pixel_seeded, unit_to_u8, EffectRasterRegion,
};
#[cfg(test)]
use crate::{
    compile_reference_effect_graph,
    graph::{compile_effect_domain_plan, effect_graph_node_use_counts},
    EffectRenderPlan,
};
use crate::{
    graph::{EffectExecutionSchedule, EffectGraphIdentity},
    CompiledEffectGraph, EffectDomainTransition, EffectExecutionAdmissionError,
    EffectExecutionSession, EffectExecutionSessionConfig, EffectGraphNodeId, EffectGraphNodeKind,
    EffectProcessingBackend, EffectRenderGraph, EffectRenderOp, EffectWorkingPrecision,
};
use mondrian_core::{types::BlendMode, ExecutionCancellationToken, Result as MondrianResult};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc};

pub type CustomEffectRenderProcessor = Arc<
    dyn Fn(&mut Vec<u8>, u32, u32, &serde_json::Value, i64) -> MondrianResult<()> + Send + Sync,
>;

type EffectInputContentFingerprint = [u8; 32];

/// Complete semantic identity of a renderer-owned color-domain processor.
///
/// Callers must derive this fingerprint from every value that can change the
/// processor's pixels: the exact color-engine/config identity, working domain,
/// dynamic properties, and implementation revision. Two processors sharing a
/// key assert bit-equivalent behavior. A truncated hash, process generation, or
/// display label is not a valid key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectDomainProcessorCacheKey([u8; 32]);

impl EffectDomainProcessorCacheKey {
    /// Construct a key from a complete canonical semantic fingerprint.
    pub const fn from_complete_semantic_fingerprint(fingerprint: [u8; 32]) -> Self {
        Self(fingerprint)
    }

    /// Return the complete canonical semantic fingerprint.
    pub const fn semantic_fingerprint(self) -> [u8; 32] {
        self.0
    }
}

/// Error returned when an encoded RGBA8 executor cannot honor a compiled
/// effect graph's color-domain contract.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EffectExecutionError {
    /// The graph requires renderer-owned stock-OCIO RGB transitions.
    #[error("effect graph requires {transitions} unresolved RGB color-domain transition(s)")]
    ColorDomainConversionRequired {
        /// Number of explicit RGB-domain edges in the compiled plan.
        transitions: usize,
    },
    /// The graph contains non-convertible data or alpha domain edges.
    #[error("effect graph contains {blockers} non-convertible color-domain blocker(s)")]
    ColorDomainBlocked {
        /// Number of fail-closed domain blockers in the compiled plan.
        blockers: usize,
    },
    /// The raw graph and schedule could not produce a complete domain plan.
    #[error("effect graph does not have a complete color-domain plan")]
    InvalidColorDomainPlan,
    /// The authored linear plan could not compile into a schedulable graph.
    #[error("effect render plan could not compile into a schedulable graph")]
    InvalidGraph,
    /// A render operation received a value outside its admitted author contract.
    #[error("effect operation `{op}` has an invalid `{parameter}` parameter")]
    InvalidRenderParameter {
        /// Stable render-operation name.
        op: &'static str,
        /// Stable parameter name.
        parameter: &'static str,
    },
    /// A custom processor required by the compiled graph is not registered.
    #[error("custom effect processor `{key}` is unavailable")]
    CustomProcessorUnavailable { key: String },
    /// A custom processor returned an error or panicked without committing its staged pixels.
    #[error("custom effect processor `{key}` failed: {reason}")]
    CustomProcessorFailed { key: String, reason: String },
    /// A finite-history operation entered an executor that owns only one
    /// current frame.
    #[error("effect operation requires an exact temporal frame provider")]
    TemporalFrameProviderRequired,
    /// Prepared Mask geometry or raster execution failed.
    #[error("effect Mask node {node_id:?} failed: {source}")]
    MaskRasterFailed {
        /// Mask source node.
        node_id: EffectGraphNodeId,
        /// Typed raster failure.
        #[source]
        source: crate::MaskRasterError,
    },
    /// The definition-bound program cannot enter this single-frame executor.
    #[error(transparent)]
    ExecutionContract(#[from] EffectExecutionAdmissionError),
}

/// Error returned when an effect graph cannot execute on the float/linear CPU path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectFloatExecutionError {
    /// The definition-bound program cannot enter this single-frame executor.
    ExecutionContract(EffectExecutionAdmissionError),
    /// Input pixel count does not match the requested extent.
    InputSizeMismatch {
        /// Expected number of RGBA pixels.
        expected: usize,
        /// Actual number of RGBA pixels.
        actual: usize,
    },
    /// The graph contains a node shape or render op that cannot execute in float.
    UnsupportedNode {
        /// Unsupported node id.
        node_id: EffectGraphNodeId,
        /// Specific unsupported reason.
        reason: EffectFloatUnsupportedReason,
    },
    /// The compiled graph did not produce its declared output node.
    MissingOutput {
        /// Missing output node id.
        node_id: EffectGraphNodeId,
    },
    /// Prepared Mask geometry or raster execution failed.
    MaskRasterFailed {
        /// Mask source node.
        node_id: EffectGraphNodeId,
        /// Typed raster failure.
        source: crate::MaskRasterError,
    },
}

/// Reason an effect graph cannot use the float/linear CPU path yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectFloatUnsupportedReason {
    /// The graph requires renderer-owned OCIO transitions before float ops run.
    ColorDomainConversionRequired {
        /// Number of explicit RGB-domain edges in the compiled plan.
        transitions: usize,
    },
    /// The graph contains non-convertible data/alpha domain edges.
    ColorDomainBlocked {
        /// Number of fail-closed domain blockers in the compiled plan.
        blockers: usize,
    },
    /// A renderer-provided color-domain processor failed.
    ColorDomainTransitionFailed {
        /// Source domain of the failed transition.
        from: crate::EffectColorDomain,
        /// Destination domain of the failed transition.
        to: crate::EffectColorDomain,
        /// Processor failure reason.
        reason: String,
    },
    /// The graph node shape cannot execute on the float path.
    UnsupportedGraphNode {
        /// Stable node kind label for diagnostics.
        kind: &'static str,
    },
    /// The render op needs a float implementation before it can run here.
    UnsupportedRenderOp {
        /// Stable render-op label for diagnostics.
        op: &'static str,
    },
    /// The requested blend mode needs a float implementation before it can run here.
    UnsupportedBlendMode {
        /// Blend mode that is still legacy-only on the float path.
        mode: BlendMode,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct EffectOutputCacheKey {
    graph_identity: EffectGraphIdentity,
    input_fingerprint: EffectInputContentFingerprint,
    width: u32,
    height: u32,
    frame_seed: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct EffectFloatOutputCacheKey {
    graph_identity: EffectGraphIdentity,
    input_fingerprint: EffectInputContentFingerprint,
    width: u32,
    height: u32,
    frame_seed: Option<i64>,
    domain_cache_key: Option<EffectDomainProcessorCacheKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct EffectNodeOutputCacheKey {
    graph_identity: EffectGraphIdentity,
    node_id: EffectGraphNodeId,
    input_fingerprint: EffectInputContentFingerprint,
    width: u32,
    height: u32,
    frame_seed: Option<i64>,
}

/// Execute a scene-linear render plan on an encoded RGBA8 boundary.
///
/// This is a crate-local legacy/reference helper. Cross-crate production
/// callers compile once and execute the resulting [`CompiledEffectGraph`].
#[cfg(test)]
pub(crate) fn apply_effect_render_plan(
    input: &[u8],
    width: u32,
    height: u32,
    plan: &EffectRenderPlan,
    frame_seed: i64,
) -> std::result::Result<Vec<u8>, EffectExecutionError> {
    if plan.is_identity() || input.is_empty() || width == 0 || height == 0 {
        return Ok(input.to_vec());
    }

    let compiled =
        compile_reference_effect_graph(plan).ok_or(EffectExecutionError::InvalidGraph)?;
    apply_compiled_effect_graph(input, width, height, compiled.as_ref(), frame_seed)
}

/// Execute a raw effect graph when its compiled domain plan needs no conversion.
///
/// Raw graph/schedule execution is intentionally crate-local so callers cannot
/// detach compiler evidence from the sole production [`CompiledEffectGraph`]
/// IR.
#[cfg(test)]
pub(crate) fn apply_effect_render_graph(
    input: &[u8],
    width: u32,
    height: u32,
    graph: &EffectRenderGraph,
    schedule: &EffectExecutionSchedule,
    frame_seed: i64,
) -> std::result::Result<Vec<u8>, EffectExecutionError> {
    let domain_plan = compile_effect_domain_plan(graph, schedule)
        .ok_or(EffectExecutionError::InvalidColorDomainPlan)?;
    validate_encoded_effect_domain_plan(&domain_plan)?;
    if graph.is_identity() || input.is_empty() || width == 0 || height == 0 {
        return Ok(input.to_vec());
    }

    let node_use_counts = effect_graph_node_use_counts(graph);
    execute_effect_graph(
        input,
        width,
        height,
        graph,
        schedule,
        &node_use_counts,
        None, // compiled: Option<&CompiledEffectGraph>
        frame_seed,
        None,
    )
}

fn execute_effect_graph(
    input: &[u8],
    width: u32,
    height: u32,
    graph: &EffectRenderGraph,
    schedule: &EffectExecutionSchedule,
    node_use_counts: &HashMap<EffectGraphNodeId, usize>,
    compiled: Option<&CompiledEffectGraph>,
    frame_seed: i64,
    mut session: Option<&mut EffectExecutionSession>,
) -> std::result::Result<Vec<u8>, EffectExecutionError> {
    let required_len = width as usize * height as usize * 4;
    let source_input_fingerprint = compiled.map(|_| frame_buffer_fingerprint(input));
    let mut outputs = HashMap::<EffectGraphNodeId, Vec<u8>>::with_capacity(graph.nodes.len());
    let mut remaining_uses = node_use_counts.clone();
    let mut buffer_pool = Vec::<Vec<u8>>::new();
    for node_id in &schedule.ordered_nodes {
        let Some(node) = graph.node(*node_id) else {
            return Err(EffectExecutionError::InvalidGraph);
        };
        match &node.kind {
            EffectGraphNodeKind::Source => {
                let mut frame = take_execution_buffer(&mut buffer_pool, required_len);
                frame.copy_from_slice(input);
                outputs.insert(node.id, frame);
            }
            EffectGraphNodeKind::UnaryEffect { input: input_id, op }
            | EffectGraphNodeKind::DomainEffect { input: input_id, op, .. } => {
                if let (Some(compiled), Some(input_fingerprint)) =
                    (compiled, source_input_fingerprint)
                {
                    if let Some(cached) = session.as_deref_mut().and_then(|session| {
                        get_cached_node_output(
                            session,
                            compiled,
                            *node_id,
                            width,
                            height,
                            input_fingerprint,
                            frame_seed,
                        )
                    }) {
                        release_consumed_node_inputs(
                            node,
                            &mut outputs,
                            &mut remaining_uses,
                            &mut buffer_pool,
                            required_len,
                        );
                        outputs.insert(node.id, cached);
                        continue;
                    }
                }
                let Some(mut source) = take_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *input_id,
                    required_len,
                ) else {
                    return Err(EffectExecutionError::InvalidGraph);
                };

                apply_render_op(&mut source, width, height, op, frame_seed)?;
                if let (Some(compiled), Some(input_fingerprint)) =
                    (compiled, source_input_fingerprint)
                {
                    if let Some(session) = session.as_deref_mut() {
                        put_cached_node_output(
                            session,
                            compiled,
                            *node_id,
                            width,
                            height,
                            input_fingerprint,
                            frame_seed,
                            &source,
                        );
                    }
                }
                outputs.insert(node.id, source);
            }
            EffectGraphNodeKind::Blend { base, overlay, blend_mode, opacity } => {
                if let (Some(compiled), Some(input_fingerprint)) =
                    (compiled, source_input_fingerprint)
                {
                    if let Some(cached) = session.as_deref_mut().and_then(|session| {
                        get_cached_node_output(
                            session,
                            compiled,
                            *node_id,
                            width,
                            height,
                            input_fingerprint,
                            frame_seed,
                        )
                    }) {
                        release_consumed_node_inputs(
                            node,
                            &mut outputs,
                            &mut remaining_uses,
                            &mut buffer_pool,
                            required_len,
                        );
                        outputs.insert(node.id, cached);
                        continue;
                    }
                }
                let Some(mut base_frame) = take_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *base,
                    required_len,
                ) else {
                    return Err(EffectExecutionError::InvalidGraph);
                };
                let Some(overlay_frame) = take_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *overlay,
                    required_len,
                ) else {
                    return Err(EffectExecutionError::InvalidGraph);
                };
                blend_graph_inputs_in_place(
                    &mut base_frame,
                    &overlay_frame,
                    *opacity,
                    *blend_mode,
                    frame_seed,
                );
                release_execution_buffer(&mut buffer_pool, overlay_frame);
                if let (Some(compiled), Some(input_fingerprint)) =
                    (compiled, source_input_fingerprint)
                {
                    if let Some(session) = session.as_deref_mut() {
                        put_cached_node_output(
                            session,
                            compiled,
                            *node_id,
                            width,
                            height,
                            input_fingerprint,
                            frame_seed,
                            &base_frame,
                        );
                    }
                }
                outputs.insert(node.id, base_frame);
            }
            EffectGraphNodeKind::MaskSource { ref shape, feather, expansion, opacity } => {
                let cancellation = ExecutionCancellationToken::new();
                let raster = crate::PreparedMaskRaster::prepare(
                    shape,
                    crate::EffectFrameExtent::new(width, height),
                    *feather,
                    *expansion,
                    *opacity,
                    &cancellation,
                )
                .map_err(|error| EffectExecutionError::MaskRasterFailed {
                    node_id: node.id,
                    source: error,
                })?;
                let rgba = raster
                    .rasterize_rgba_u8(
                        crate::EffectPixelRoi::new(0, 0, width, height),
                        &cancellation,
                    )
                    .map_err(|error| EffectExecutionError::MaskRasterFailed {
                        node_id: node.id,
                        source: error,
                    })?;
                outputs.insert(node.id, rgba);
            }
            EffectGraphNodeKind::Mask { input: input_id, mask, invert, mask_op } => {
                if let (Some(compiled), Some(input_fingerprint)) =
                    (compiled, source_input_fingerprint)
                {
                    if let Some(cached) = session.as_deref_mut().and_then(|session| {
                        get_cached_node_output(
                            session,
                            compiled,
                            *node_id,
                            width,
                            height,
                            input_fingerprint,
                            frame_seed,
                        )
                    }) {
                        release_consumed_node_inputs(
                            node,
                            &mut outputs,
                            &mut remaining_uses,
                            &mut buffer_pool,
                            required_len,
                        );
                        outputs.insert(node.id, cached);
                        continue;
                    }
                }
                let Some(mut source) = take_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *input_id,
                    required_len,
                ) else {
                    return Err(EffectExecutionError::InvalidGraph);
                };
                let Some(mask_frame) = take_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *mask,
                    required_len,
                ) else {
                    return Err(EffectExecutionError::InvalidGraph);
                };
                apply_alpha_mask_in_place(&mut source, &mask_frame, *invert, *mask_op);
                release_execution_buffer(&mut buffer_pool, mask_frame);
                if let (Some(compiled), Some(input_fingerprint)) =
                    (compiled, source_input_fingerprint)
                {
                    if let Some(session) = session.as_deref_mut() {
                        put_cached_node_output(
                            session,
                            compiled,
                            *node_id,
                            width,
                            height,
                            input_fingerprint,
                            frame_seed,
                            &source,
                        );
                    }
                }
                outputs.insert(node.id, source);
            }
            EffectGraphNodeKind::MultiInput { ref inputs, blend_mode, opacity } => {
                let Some(first_id) = inputs.first().copied() else {
                    return Err(EffectExecutionError::InvalidGraph);
                };
                let first = take_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    first_id,
                    required_len,
                )
                .ok_or(EffectExecutionError::InvalidGraph)?;
                let mut result = first;
                for overlay_id in &inputs[1..] {
                    let overlay = take_graph_input(
                        &mut outputs,
                        &mut remaining_uses,
                        &mut buffer_pool,
                        *overlay_id,
                        required_len,
                    )
                    .ok_or(EffectExecutionError::InvalidGraph)?;
                    blend_graph_inputs_in_place(
                        &mut result,
                        &overlay,
                        *opacity,
                        *blend_mode,
                        frame_seed,
                    );
                    release_execution_buffer(&mut buffer_pool, overlay);
                }
                outputs.insert(node.id, result);
            }
        }
    }

    graph
        .output
        .and_then(|output| outputs.remove(&output))
        .ok_or(EffectExecutionError::InvalidGraph)
}

/// Execute a compiled effect graph on encoded RGBA8, failing on unresolved domains.
pub fn apply_compiled_effect_graph(
    input: &[u8],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
) -> std::result::Result<Vec<u8>, EffectExecutionError> {
    let mut session =
        EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(usize::MAX));
    session.apply_compiled_rgba8(input, width, height, compiled, frame_seed)
}

impl EffectExecutionSession {
    /// Execute one current-frame compiled graph over encoded RGBA8.
    pub fn apply_compiled_rgba8(
        &mut self,
        input: &[u8],
        width: u32,
        height: u32,
        compiled: &CompiledEffectGraph,
        frame_seed: i64,
    ) -> std::result::Result<Vec<u8>, EffectExecutionError> {
        admit_single_frame_execution(
            compiled,
            EffectProcessingBackend::Cpu,
            EffectWorkingPrecision::NormalizedU8,
        )?;
        validate_encoded_effect_domain_plan(compiled.domain_plan())?;
        if let Some(cached) =
            get_cached_effect_output(self, input, width, height, compiled, frame_seed)
        {
            return Ok(cached);
        }

        let output = execute_effect_graph(
            input,
            width,
            height,
            compiled.graph(),
            compiled.schedule(),
            compiled.node_use_counts(),
            Some(compiled),
            frame_seed,
            Some(self),
        )?;

        put_cached_effect_output(self, input, width, height, compiled, frame_seed, &output);
        Ok(output)
    }
}

fn validate_encoded_effect_domain_plan(
    plan: &crate::CompiledEffectDomainPlan,
) -> std::result::Result<(), EffectExecutionError> {
    if !plan.blockers.is_empty() {
        return Err(EffectExecutionError::ColorDomainBlocked { blockers: plan.blockers.len() });
    }
    if !plan.transitions.is_empty() {
        return Err(EffectExecutionError::ColorDomainConversionRequired {
            transitions: plan.transitions.len(),
        });
    }
    Ok(())
}

/// Return whether a compiled graph can execute entirely on the float/linear CPU path.
pub fn compiled_effect_graph_supports_rgba_f32(compiled: &CompiledEffectGraph) -> bool {
    validate_float_effect_graph(compiled, false).is_ok()
}

/// Return whether a renderer-provided domain processor makes this graph float-capable.
pub fn compiled_effect_graph_supports_rgba_f32_with_domain_processor(
    compiled: &CompiledEffectGraph,
) -> bool {
    validate_float_effect_graph(compiled, true).is_ok()
}

/// Return whether this graph's topology and operations have an `RGBA f32`
/// implementation.
///
/// This query deliberately ignores execution-contract admission and color
/// domains. A caller selecting a concrete execution route must independently
/// admit the exact backend/precision and prove that the domain plan is
/// resolvable. Keeping those questions separate prevents a stateful, temporal,
/// or GPU-only graph from being misreported as a pixel-shape or color-domain
/// limitation.
pub fn compiled_effect_graph_has_rgba_f32_execution_shape(compiled: &CompiledEffectGraph) -> bool {
    validate_float_effect_graph_shape(compiled).is_ok()
}

/// Return whether this graph's color-domain plan is resolvable on an `RGBA
/// f32` route.
///
/// This query deliberately ignores graph-operation support and execution
/// contracts. `domain_processor_available` means the owning renderer can
/// execute every declared domain transition; explicit domain blockers always
/// fail closed.
pub fn compiled_effect_graph_has_resolvable_rgba_f32_domain(
    compiled: &CompiledEffectGraph,
    domain_processor_available: bool,
) -> bool {
    validate_float_effect_domain(compiled, domain_processor_available).is_ok()
}

/// Execute a compiled graph over linear `f32` RGBA pixels.
///
/// Built-in unary, blend, mask, mask-source, and multi-input nodes execute in
/// the linear float working domain. Custom processors require an explicit float
/// ABI; unsupported processors return structured errors so callers can make a
/// diagnosed fallback decision.
pub fn apply_compiled_effect_graph_rgba_f32(
    input: &[[f32; 4]],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
) -> Result<Vec<[f32; 4]>, EffectFloatExecutionError> {
    let mut session =
        EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(usize::MAX));
    session.apply_compiled_rgba_f32(input, width, height, compiled, frame_seed)
}

/// Execute a compiled float graph with renderer-owned color-domain processors.
///
/// `domain_cache_key` must be the complete stable semantic fingerprint of the
/// exact color engine/config, working space, dynamic properties, and
/// implementation revision used by `processor`. Reusing a key asserts
/// bit-equivalent processing; a truncated hash or process generation is
/// invalid.
pub fn apply_compiled_effect_graph_rgba_f32_with_domain_processor<F>(
    input: &[[f32; 4]],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
    domain_cache_key: EffectDomainProcessorCacheKey,
    mut processor: F,
) -> Result<Vec<[f32; 4]>, EffectFloatExecutionError>
where
    F: FnMut(&mut [[f32; 4]], EffectDomainTransition) -> Result<(), String>,
{
    let mut session =
        EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(usize::MAX));
    session.apply_compiled_rgba_f32_with_domain_processor(
        input,
        width,
        height,
        compiled,
        frame_seed,
        domain_cache_key,
        &mut processor,
    )
}

type EffectDomainProcessor<'a> =
    dyn FnMut(&mut [[f32; 4]], EffectDomainTransition) -> Result<(), String> + 'a;

fn apply_compiled_effect_graph_rgba_f32_inner(
    session: &mut EffectExecutionSession,
    input: &[[f32; 4]],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
    domain_cache_key: Option<EffectDomainProcessorCacheKey>,
    mut processor: Option<&mut EffectDomainProcessor<'_>>,
) -> Result<Vec<[f32; 4]>, EffectFloatExecutionError> {
    let required_len = width as usize * height as usize;
    if input.len() != required_len {
        return Err(EffectFloatExecutionError::InputSizeMismatch {
            expected: required_len,
            actual: input.len(),
        });
    }
    validate_float_effect_graph(compiled, processor.is_some())?;
    if compiled.graph().is_identity() || input.is_empty() || width == 0 || height == 0 {
        return Ok(input.to_vec());
    }
    if let Some(cached) = get_cached_effect_output_f32(
        session,
        input,
        width,
        height,
        compiled,
        frame_seed,
        domain_cache_key,
    ) {
        return Ok(cached);
    }

    let mut outputs =
        HashMap::<EffectGraphNodeId, Vec<[f32; 4]>>::with_capacity(compiled.graph().nodes.len());
    let mut remaining_uses = compiled.node_use_counts().clone();
    let mut buffer_pool = Vec::<Vec<[f32; 4]>>::new();
    for node_id in &compiled.schedule().ordered_nodes {
        let Some(node) = compiled.graph().node(*node_id) else {
            return Err(EffectFloatExecutionError::UnsupportedNode {
                node_id: *node_id,
                reason: EffectFloatUnsupportedReason::UnsupportedGraphNode { kind: "missing" },
            });
        };
        match &node.kind {
            EffectGraphNodeKind::Source => {
                let mut frame = take_float_execution_buffer(&mut buffer_pool, required_len);
                frame.copy_from_slice(input);
                outputs.insert(node.id, frame);
            }
            EffectGraphNodeKind::UnaryEffect { input: input_id, op }
            | EffectGraphNodeKind::DomainEffect { input: input_id, op, .. } => {
                let Some(mut source) = take_float_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *input_id,
                    required_len,
                ) else {
                    return Err(EffectFloatExecutionError::MissingOutput { node_id: *input_id });
                };
                apply_effect_domain_transition(
                    compiled,
                    Some(node.id),
                    *input_id,
                    &mut source,
                    &mut processor,
                )?;
                if !apply_render_op_f32(&mut source, width, height, op, frame_seed) {
                    return Err(EffectFloatExecutionError::UnsupportedNode {
                        node_id: node.id,
                        reason: EffectFloatUnsupportedReason::UnsupportedRenderOp {
                            op: effect_render_op_name(op),
                        },
                    });
                }
                outputs.insert(node.id, source);
            }
            EffectGraphNodeKind::Blend { base, overlay, blend_mode, opacity } => {
                let Some(mut base_frame) = take_float_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *base,
                    required_len,
                ) else {
                    return Err(EffectFloatExecutionError::MissingOutput { node_id: *base });
                };
                let Some(mut overlay_frame) = take_float_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *overlay,
                    required_len,
                ) else {
                    return Err(EffectFloatExecutionError::MissingOutput { node_id: *overlay });
                };
                apply_effect_domain_transition(
                    compiled,
                    Some(node.id),
                    *overlay,
                    &mut overlay_frame,
                    &mut processor,
                )?;
                blend_rgba_f32_in_place(
                    &mut base_frame,
                    &overlay_frame,
                    *opacity,
                    *blend_mode,
                    frame_seed,
                );
                release_float_execution_buffer(&mut buffer_pool, overlay_frame);
                outputs.insert(node.id, base_frame);
            }
            EffectGraphNodeKind::MaskSource { shape, feather, expansion, opacity } => {
                let cancellation = ExecutionCancellationToken::new();
                let raster = crate::PreparedMaskRaster::prepare(
                    shape,
                    crate::EffectFrameExtent::new(width, height),
                    *feather,
                    *expansion,
                    *opacity,
                    &cancellation,
                )
                .map_err(|error| EffectFloatExecutionError::MaskRasterFailed {
                    node_id: node.id,
                    source: error,
                })?;
                let rgba = raster
                    .rasterize_rgba_f32(
                        crate::EffectPixelRoi::new(0, 0, width, height),
                        &cancellation,
                    )
                    .map_err(|error| EffectFloatExecutionError::MaskRasterFailed {
                        node_id: node.id,
                        source: error,
                    })?;
                outputs.insert(node.id, rgba);
            }
            EffectGraphNodeKind::Mask { input: input_id, mask, invert, mask_op } => {
                let Some(mut source) = take_float_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *input_id,
                    required_len,
                ) else {
                    return Err(EffectFloatExecutionError::MissingOutput { node_id: *input_id });
                };
                let Some(mask_frame) = take_float_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *mask,
                    required_len,
                ) else {
                    return Err(EffectFloatExecutionError::MissingOutput { node_id: *mask });
                };
                apply_alpha_mask_f32_in_place(&mut source, &mask_frame, *invert, *mask_op);
                release_float_execution_buffer(&mut buffer_pool, mask_frame);
                outputs.insert(node.id, source);
            }
            EffectGraphNodeKind::MultiInput { inputs, blend_mode, opacity } => {
                let Some(first_id) = inputs.first().copied() else {
                    return Err(unsupported_float_graph_node(node.id, "multi_input_empty"));
                };
                let Some(mut result) = take_float_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    first_id,
                    required_len,
                ) else {
                    return Err(EffectFloatExecutionError::MissingOutput { node_id: first_id });
                };
                for overlay_id in &inputs[1..] {
                    let Some(mut overlay) = take_float_graph_input(
                        &mut outputs,
                        &mut remaining_uses,
                        &mut buffer_pool,
                        *overlay_id,
                        required_len,
                    ) else {
                        return Err(EffectFloatExecutionError::MissingOutput {
                            node_id: *overlay_id,
                        });
                    };
                    apply_effect_domain_transition(
                        compiled,
                        Some(node.id),
                        *overlay_id,
                        &mut overlay,
                        &mut processor,
                    )?;
                    blend_rgba_f32_in_place(
                        &mut result,
                        &overlay,
                        *opacity,
                        *blend_mode,
                        frame_seed,
                    );
                    release_float_execution_buffer(&mut buffer_pool, overlay);
                }
                outputs.insert(node.id, result);
            }
        }
    }

    let Some(output_id) = compiled.graph().output else {
        return Ok(input.to_vec());
    };
    let mut output = outputs
        .remove(&output_id)
        .ok_or(EffectFloatExecutionError::MissingOutput { node_id: output_id })?;
    apply_effect_domain_transition(compiled, None, output_id, &mut output, &mut processor)?;
    put_cached_effect_output_f32(
        session,
        input,
        width,
        height,
        compiled,
        frame_seed,
        domain_cache_key,
        &output,
    );
    Ok(output)
}

impl EffectExecutionSession {
    /// Execute one current-frame compiled graph over scene-linear Float32
    /// pixels.
    pub fn apply_compiled_rgba_f32(
        &mut self,
        input: &[[f32; 4]],
        width: u32,
        height: u32,
        compiled: &CompiledEffectGraph,
        frame_seed: i64,
    ) -> Result<Vec<[f32; 4]>, EffectFloatExecutionError> {
        apply_compiled_effect_graph_rgba_f32_inner(
            self, input, width, height, compiled, frame_seed, None, None,
        )
    }

    /// Execute one current-frame Float32 graph while resolving explicit RGB
    /// processing-domain transitions through the caller's color Adapter.
    pub fn apply_compiled_rgba_f32_with_domain_processor<F>(
        &mut self,
        input: &[[f32; 4]],
        width: u32,
        height: u32,
        compiled: &CompiledEffectGraph,
        frame_seed: i64,
        domain_cache_key: EffectDomainProcessorCacheKey,
        mut processor: F,
    ) -> Result<Vec<[f32; 4]>, EffectFloatExecutionError>
    where
        F: FnMut(&mut [[f32; 4]], EffectDomainTransition) -> Result<(), String>,
    {
        apply_compiled_effect_graph_rgba_f32_inner(
            self,
            input,
            width,
            height,
            compiled,
            frame_seed,
            Some(domain_cache_key),
            Some(&mut processor),
        )
    }
}

fn apply_effect_domain_transition(
    compiled: &CompiledEffectGraph,
    consumer: Option<EffectGraphNodeId>,
    input: EffectGraphNodeId,
    pixels: &mut [[f32; 4]],
    processor: &mut Option<&mut EffectDomainProcessor<'_>>,
) -> Result<(), EffectFloatExecutionError> {
    let Some(transition) = compiled
        .domain_plan()
        .transitions
        .iter()
        .find(|transition| transition.consumer == consumer && transition.input == input)
        .copied()
    else {
        return Ok(());
    };
    let Some(processor) = processor.as_deref_mut() else {
        return Err(EffectFloatExecutionError::UnsupportedNode {
            node_id: consumer.unwrap_or(input),
            reason: EffectFloatUnsupportedReason::ColorDomainConversionRequired {
                transitions: compiled.domain_plan().transitions.len(),
            },
        });
    };
    processor(pixels, transition).map_err(|reason| EffectFloatExecutionError::UnsupportedNode {
        node_id: consumer.unwrap_or(input),
        reason: EffectFloatUnsupportedReason::ColorDomainTransitionFailed {
            from: transition.from,
            to: transition.to,
            reason,
        },
    })
}

/// Execute a compiled adjustment graph and blend the result over a float base frame.
pub fn apply_compiled_effect_graph_pass_rgba_f32(
    base: &[[f32; 4]],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    opacity: f32,
    blend_mode: Option<BlendMode>,
    frame_seed: i64,
) -> Result<Vec<[f32; 4]>, EffectFloatExecutionError> {
    let mut session =
        EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(usize::MAX));
    session.apply_compiled_pass_rgba_f32(
        base, width, height, compiled, opacity, blend_mode, frame_seed,
    )
}

impl EffectExecutionSession {
    /// Execute and blend one current-frame Float32 adjustment graph.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_compiled_pass_rgba_f32(
        &mut self,
        base: &[[f32; 4]],
        width: u32,
        height: u32,
        compiled: &CompiledEffectGraph,
        opacity: f32,
        blend_mode: Option<BlendMode>,
        frame_seed: i64,
    ) -> Result<Vec<[f32; 4]>, EffectFloatExecutionError> {
        let required_len = width as usize * height as usize;
        if base.len() != required_len {
            return Err(EffectFloatExecutionError::InputSizeMismatch {
                expected: required_len,
                actual: base.len(),
            });
        }
        admit_single_frame_execution(
            compiled,
            EffectProcessingBackend::Cpu,
            EffectWorkingPrecision::Float32,
        )
        .map_err(EffectFloatExecutionError::ExecutionContract)?;
        if required_len == 0 {
            return Ok(Vec::new());
        }
        let mode = blend_mode.unwrap_or(BlendMode::Normal);
        let opacity = opacity.clamp(0.0, 1.0);
        if opacity <= 1.0e-4 || compiled.graph().is_identity() {
            return Ok(base.to_vec());
        }

        let processed = self.apply_compiled_rgba_f32(base, width, height, compiled, frame_seed)?;
        let mut out = base.to_vec();
        blend_rgba_f32_in_place(&mut out, &processed, opacity, mode, frame_seed);
        Ok(out)
    }
}

/// Execute and blend a float adjustment graph with renderer-owned domain processors.
pub fn apply_compiled_effect_graph_pass_rgba_f32_with_domain_processor<F>(
    base: &[[f32; 4]],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    opacity: f32,
    blend_mode: Option<BlendMode>,
    frame_seed: i64,
    domain_cache_key: EffectDomainProcessorCacheKey,
    processor: F,
) -> Result<Vec<[f32; 4]>, EffectFloatExecutionError>
where
    F: FnMut(&mut [[f32; 4]], EffectDomainTransition) -> Result<(), String>,
{
    let mut session =
        EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(usize::MAX));
    session.apply_compiled_pass_rgba_f32_with_domain_processor(
        base,
        width,
        height,
        compiled,
        opacity,
        blend_mode,
        frame_seed,
        domain_cache_key,
        processor,
    )
}

impl EffectExecutionSession {
    /// Execute and blend one current-frame Float32 adjustment graph with
    /// renderer-owned color-domain conversion.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_compiled_pass_rgba_f32_with_domain_processor<F>(
        &mut self,
        base: &[[f32; 4]],
        width: u32,
        height: u32,
        compiled: &CompiledEffectGraph,
        opacity: f32,
        blend_mode: Option<BlendMode>,
        frame_seed: i64,
        domain_cache_key: EffectDomainProcessorCacheKey,
        processor: F,
    ) -> Result<Vec<[f32; 4]>, EffectFloatExecutionError>
    where
        F: FnMut(&mut [[f32; 4]], EffectDomainTransition) -> Result<(), String>,
    {
        let required_len = width as usize * height as usize;
        if base.len() != required_len {
            return Err(EffectFloatExecutionError::InputSizeMismatch {
                expected: required_len,
                actual: base.len(),
            });
        }
        admit_single_frame_execution(
            compiled,
            EffectProcessingBackend::Cpu,
            EffectWorkingPrecision::Float32,
        )
        .map_err(EffectFloatExecutionError::ExecutionContract)?;
        if required_len == 0 {
            return Ok(Vec::new());
        }
        let mode = blend_mode.unwrap_or(BlendMode::Normal);
        let opacity = opacity.clamp(0.0, 1.0);
        if opacity <= 1.0e-4 || compiled.graph().is_identity() {
            return Ok(base.to_vec());
        }

        let processed = self.apply_compiled_rgba_f32_with_domain_processor(
            base,
            width,
            height,
            compiled,
            frame_seed,
            domain_cache_key,
            processor,
        )?;
        let mut out = base.to_vec();
        blend_rgba_f32_in_place(&mut out, &processed, opacity, mode, frame_seed);
        Ok(out)
    }
}

/// Execute and blend a compiled graph, failing on unresolved domains.
pub fn apply_compiled_effect_graph_pass(
    base: &[u8],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    opacity: f32,
    blend_mode: Option<BlendMode>,
    frame_seed: i64,
    out: &mut Vec<u8>,
) -> std::result::Result<(), EffectExecutionError> {
    let mut session =
        EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(usize::MAX));
    session.apply_compiled_pass_rgba8(
        base, width, height, compiled, opacity, blend_mode, frame_seed, out,
    )
}

impl EffectExecutionSession {
    /// Execute and blend one current-frame encoded RGBA8 adjustment graph.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_compiled_pass_rgba8(
        &mut self,
        base: &[u8],
        width: u32,
        height: u32,
        compiled: &CompiledEffectGraph,
        opacity: f32,
        blend_mode: Option<BlendMode>,
        frame_seed: i64,
        out: &mut Vec<u8>,
    ) -> std::result::Result<(), EffectExecutionError> {
        admit_single_frame_execution(
            compiled,
            EffectProcessingBackend::Cpu,
            EffectWorkingPrecision::NormalizedU8,
        )?;
        let required_len = width as usize * height as usize * 4;
        if out.len() != required_len {
            out.resize(required_len, 0);
        }
        if required_len == 0 || base.len() != required_len {
            out.clear();
            return Ok(());
        }
        if opacity <= 1.0e-4 || compiled.graph().is_identity() {
            out.copy_from_slice(base);
            return Ok(());
        }

        let processed = self.apply_compiled_rgba8(base, width, height, compiled, frame_seed)?;
        blend_adjustment_result(base, &processed, width, height, opacity, blend_mode, out);
        Ok(())
    }
}

fn get_cached_effect_output(
    session: &mut EffectExecutionSession,
    input: &[u8],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
) -> Option<Vec<u8>> {
    let key = effect_output_cache_key(input, width, height, compiled, frame_seed)?;
    session.get_encoded_output(&key)
}

fn put_cached_effect_output(
    session: &mut EffectExecutionSession,
    input: &[u8],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
    output: &[u8],
) {
    let Some(key) = effect_output_cache_key(input, width, height, compiled, frame_seed) else {
        return;
    };
    session.put_encoded_output(key, output.to_vec());
}

fn effect_output_cache_key(
    input: &[u8],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
) -> Option<EffectOutputCacheKey> {
    if !compiled.output_cache_enabled()
        || !compiled.output_cache_policy().permits_cross_call_reuse()
        || input.is_empty()
        || width == 0
        || height == 0
    {
        return None;
    }

    Some(EffectOutputCacheKey {
        graph_identity: compiled.identity().clone(),
        input_fingerprint: frame_buffer_fingerprint(input),
        width,
        height,
        frame_seed: compiled.output_cache_policy().requires_frame_seed().then_some(frame_seed),
    })
}

fn get_cached_effect_output_f32(
    session: &mut EffectExecutionSession,
    input: &[[f32; 4]],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
    domain_cache_key: Option<EffectDomainProcessorCacheKey>,
) -> Option<Vec<[f32; 4]>> {
    let key =
        effect_output_cache_key_f32(input, width, height, compiled, frame_seed, domain_cache_key)?;
    session.get_float_output(&key)
}

fn put_cached_effect_output_f32(
    session: &mut EffectExecutionSession,
    input: &[[f32; 4]],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
    domain_cache_key: Option<EffectDomainProcessorCacheKey>,
    output: &[[f32; 4]],
) {
    let Some(key) =
        effect_output_cache_key_f32(input, width, height, compiled, frame_seed, domain_cache_key)
    else {
        return;
    };
    session.put_float_output(key, output.to_vec());
}

fn effect_output_cache_key_f32(
    input: &[[f32; 4]],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
    domain_cache_key: Option<EffectDomainProcessorCacheKey>,
) -> Option<EffectFloatOutputCacheKey> {
    if !effect_output_cache_enabled_f32(compiled, domain_cache_key.is_some())
        || input.is_empty()
        || width == 0
        || height == 0
    {
        return None;
    }

    Some(EffectFloatOutputCacheKey {
        graph_identity: compiled.identity().clone(),
        input_fingerprint: frame_buffer_fingerprint_f32(input),
        width,
        height,
        frame_seed: compiled.output_cache_policy().requires_frame_seed().then_some(frame_seed),
        domain_cache_key,
    })
}

fn effect_output_cache_enabled_f32(
    compiled: &CompiledEffectGraph,
    domain_processor_available: bool,
) -> bool {
    compiled.output_cache_enabled()
        && compiled.output_cache_policy().permits_cross_call_reuse()
        && validate_float_effect_graph(compiled, domain_processor_available).is_ok()
}

fn validate_float_effect_graph(
    compiled: &CompiledEffectGraph,
    domain_processor_available: bool,
) -> Result<(), EffectFloatExecutionError> {
    admit_single_frame_execution(
        compiled,
        EffectProcessingBackend::Cpu,
        EffectWorkingPrecision::Float32,
    )
    .map_err(EffectFloatExecutionError::ExecutionContract)?;
    validate_float_effect_domain(compiled, domain_processor_available)?;
    validate_float_effect_graph_shape(compiled)
}

fn validate_float_effect_domain(
    compiled: &CompiledEffectGraph,
    domain_processor_available: bool,
) -> Result<(), EffectFloatExecutionError> {
    if let Some(blocker) = compiled.domain_plan().blockers.first() {
        return Err(EffectFloatExecutionError::UnsupportedNode {
            node_id: blocker.consumer.unwrap_or(blocker.input),
            reason: EffectFloatUnsupportedReason::ColorDomainBlocked {
                blockers: compiled.domain_plan().blockers.len(),
            },
        });
    }
    if !domain_processor_available {
        if let Some(transition) = compiled.domain_plan().transitions.first() {
            return Err(EffectFloatExecutionError::UnsupportedNode {
                node_id: transition.consumer.unwrap_or(transition.input),
                reason: EffectFloatUnsupportedReason::ColorDomainConversionRequired {
                    transitions: compiled.domain_plan().transitions.len(),
                },
            });
        }
    }
    Ok(())
}

fn validate_float_effect_graph_shape(
    compiled: &CompiledEffectGraph,
) -> Result<(), EffectFloatExecutionError> {
    for node_id in &compiled.schedule().ordered_nodes {
        let Some(node) = compiled.graph().node(*node_id) else {
            return Err(unsupported_float_graph_node(*node_id, "missing"));
        };
        match &node.kind {
            EffectGraphNodeKind::Source => {}
            EffectGraphNodeKind::UnaryEffect { op, .. }
            | EffectGraphNodeKind::DomainEffect { op, .. } => {
                if !effect_render_op_supports_rgba_f32(op) {
                    return Err(EffectFloatExecutionError::UnsupportedNode {
                        node_id: node.id,
                        reason: EffectFloatUnsupportedReason::UnsupportedRenderOp {
                            op: effect_render_op_name(op),
                        },
                    });
                }
            }
            EffectGraphNodeKind::Blend { .. }
            | EffectGraphNodeKind::Mask { .. }
            | EffectGraphNodeKind::MaskSource { .. }
            | EffectGraphNodeKind::MultiInput { .. } => {}
        }
    }
    Ok(())
}

fn admit_single_frame_execution(
    compiled: &CompiledEffectGraph,
    backend: EffectProcessingBackend,
    precision: EffectWorkingPrecision,
) -> Result<(), EffectExecutionAdmissionError> {
    compiled.execution_envelope().admit_single_frame_backend(backend, precision)
}

fn effect_render_op_supports_rgba_f32(op: &EffectRenderOp) -> bool {
    !matches!(
        op,
        EffectRenderOp::Custom { .. } | EffectRenderOp::TemporalFrameMix { .. }
    )
}

fn unsupported_float_graph_node(
    node_id: EffectGraphNodeId,
    kind: &'static str,
) -> EffectFloatExecutionError {
    EffectFloatExecutionError::UnsupportedNode {
        node_id,
        reason: EffectFloatUnsupportedReason::UnsupportedGraphNode { kind },
    }
}

fn effect_render_op_name(op: &EffectRenderOp) -> &'static str {
    match op {
        EffectRenderOp::ColorAdjust { .. } => "color_adjust",
        EffectRenderOp::GaussianBlur { .. } => "gaussian_blur",
        EffectRenderOp::Sharpen { .. } => "sharpen",
        EffectRenderOp::Vignette { .. } => "vignette",
        EffectRenderOp::ChromaticAberration { .. } => "chromatic_aberration",
        EffectRenderOp::Grain { .. } => "grain",
        EffectRenderOp::TemporalFrameMix { .. } => "temporal_frame_mix",
        EffectRenderOp::Lut3D { .. } => "lut3d",
        EffectRenderOp::Custom { .. } => "custom",
    }
}

fn frame_buffer_fingerprint(buffer: &[u8]) -> EffectInputContentFingerprint {
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.effect-input.rgba8.v1");
    hasher.update((buffer.len() as u64).to_le_bytes());
    hasher.update(buffer);
    hasher.finalize().into()
}

fn frame_buffer_fingerprint_f32(buffer: &[[f32; 4]]) -> EffectInputContentFingerprint {
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.effect-input.rgba-f32.v1");
    hasher.update((buffer.len() as u64).to_le_bytes());
    for pixel in buffer {
        for channel in pixel {
            hasher.update(channel.to_bits().to_le_bytes());
        }
    }
    hasher.finalize().into()
}

fn get_cached_node_output(
    session: &mut EffectExecutionSession,
    compiled: &CompiledEffectGraph,
    node_id: EffectGraphNodeId,
    width: u32,
    height: u32,
    input_fingerprint: EffectInputContentFingerprint,
    frame_seed: i64,
) -> Option<Vec<u8>> {
    let key = effect_node_output_cache_key(
        compiled,
        node_id,
        width,
        height,
        input_fingerprint,
        frame_seed,
    )?;
    session.get_encoded_node(&key)
}

fn put_cached_node_output(
    session: &mut EffectExecutionSession,
    compiled: &CompiledEffectGraph,
    node_id: EffectGraphNodeId,
    width: u32,
    height: u32,
    input_fingerprint: EffectInputContentFingerprint,
    frame_seed: i64,
    output: &[u8],
) {
    let Some(key) = effect_node_output_cache_key(
        compiled,
        node_id,
        width,
        height,
        input_fingerprint,
        frame_seed,
    ) else {
        return;
    };
    session.put_encoded_node(key, output.to_vec());
}

fn effect_node_output_cache_key(
    compiled: &CompiledEffectGraph,
    node_id: EffectGraphNodeId,
    width: u32,
    height: u32,
    input_fingerprint: EffectInputContentFingerprint,
    frame_seed: i64,
) -> Option<EffectNodeOutputCacheKey> {
    let profile = compiled.node_profiles().get(&node_id)?;
    if !profile.output_cache_enabled
        || !profile.cache_policy.permits_cross_call_reuse()
        || width == 0
        || height == 0
    {
        return None;
    }

    Some(EffectNodeOutputCacheKey {
        graph_identity: compiled.identity().clone(),
        node_id,
        input_fingerprint,
        width,
        height,
        frame_seed: profile.cache_policy.requires_frame_seed().then_some(frame_seed),
    })
}

fn release_consumed_node_inputs(
    node: &crate::EffectGraphNode,
    outputs: &mut HashMap<EffectGraphNodeId, Vec<u8>>,
    remaining_uses: &mut HashMap<EffectGraphNodeId, usize>,
    buffer_pool: &mut Vec<Vec<u8>>,
    required_len: usize,
) {
    for input in node.input_ids() {
        if let Some(buffer) =
            take_graph_input(outputs, remaining_uses, buffer_pool, input, required_len)
        {
            release_execution_buffer(buffer_pool, buffer);
        }
    }
}

fn take_graph_input(
    outputs: &mut HashMap<EffectGraphNodeId, Vec<u8>>,
    remaining_uses: &mut HashMap<EffectGraphNodeId, usize>,
    buffer_pool: &mut Vec<Vec<u8>>,
    node_id: EffectGraphNodeId,
    required_len: usize,
) -> Option<Vec<u8>> {
    let remaining = remaining_uses.get_mut(&node_id)?;
    if *remaining == 0 {
        return outputs.remove(&node_id);
    }
    if *remaining == 1 {
        *remaining = 0;
        return outputs.remove(&node_id);
    }

    *remaining -= 1;
    let source = outputs.get(&node_id)?;
    let mut cloned = take_execution_buffer(buffer_pool, required_len);
    cloned.copy_from_slice(source);
    Some(cloned)
}

fn take_execution_buffer(buffer_pool: &mut Vec<Vec<u8>>, required_len: usize) -> Vec<u8> {
    if let Some(mut buffer) = buffer_pool.pop() {
        if buffer.len() != required_len {
            buffer.resize(required_len, 0);
        }
        return buffer;
    }
    vec![0u8; required_len]
}

fn take_float_graph_input(
    outputs: &mut HashMap<EffectGraphNodeId, Vec<[f32; 4]>>,
    remaining_uses: &mut HashMap<EffectGraphNodeId, usize>,
    buffer_pool: &mut Vec<Vec<[f32; 4]>>,
    node_id: EffectGraphNodeId,
    required_len: usize,
) -> Option<Vec<[f32; 4]>> {
    let remaining = remaining_uses.get_mut(&node_id)?;
    if *remaining == 0 {
        return None;
    }
    *remaining -= 1;
    if *remaining == 0 {
        return outputs.remove(&node_id);
    }

    let source = outputs.get(&node_id)?;
    let mut cloned = take_float_execution_buffer(buffer_pool, required_len);
    cloned.copy_from_slice(source);
    Some(cloned)
}

fn take_float_execution_buffer(
    buffer_pool: &mut Vec<Vec<[f32; 4]>>,
    required_len: usize,
) -> Vec<[f32; 4]> {
    if let Some(mut buffer) = buffer_pool.pop() {
        if buffer.len() != required_len {
            buffer.resize(required_len, [0.0; 4]);
        }
        return buffer;
    }
    vec![[0.0; 4]; required_len]
}

fn release_float_execution_buffer(buffer_pool: &mut Vec<Vec<[f32; 4]>>, mut buffer: Vec<[f32; 4]>) {
    buffer.clear();
    buffer_pool.push(buffer);
}

fn release_execution_buffer(buffer_pool: &mut Vec<Vec<u8>>, mut buffer: Vec<u8>) {
    buffer.clear();
    buffer_pool.push(buffer);
}

fn blend_graph_inputs_in_place(
    base: &mut [u8],
    overlay: &[u8],
    opacity: f32,
    blend_mode: BlendMode,
    frame_seed: i64,
) {
    for (i, (base_px, overlay_px)) in
        base.chunks_exact_mut(4).zip(overlay.chunks_exact(4)).enumerate()
    {
        let blended = blend_rgba_pixel_seeded(
            [base_px[0], base_px[1], base_px[2], base_px[3]],
            [overlay_px[0], overlay_px[1], overlay_px[2], overlay_px[3]],
            opacity,
            blend_mode,
            effect_graph_dither_seed(i as u32, frame_seed),
        );
        base_px.copy_from_slice(&blended);
    }
}

fn blend_rgba_f32_in_place(
    base: &mut [[f32; 4]],
    overlay: &[[f32; 4]],
    opacity: f32,
    blend_mode: BlendMode,
    frame_seed: i64,
) {
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 1.0e-4 {
        return;
    }

    for (index, (base_px, overlay_px)) in base.iter_mut().zip(overlay.iter()).enumerate() {
        *base_px = blend_rgba_f32_pixel_seeded(
            *base_px,
            *overlay_px,
            opacity,
            blend_mode,
            effect_graph_dither_seed(index as u32, frame_seed),
        );
    }
}

pub(crate) fn blend_rgba_f32_region_controlled<E>(
    base: &mut [[f32; 4]],
    overlay: &[[f32; 4]],
    region: EffectRasterRegion,
    opacity: f32,
    blend_mode: BlendMode,
    frame_seed: i64,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if base.len() != overlay.len() || !region.is_valid_for(base.len()) {
        return Ok(false);
    }
    if base.is_empty() {
        checkpoint()?;
        return Ok(true);
    }
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 1.0e-4 {
        checkpoint()?;
        return Ok(true);
    }

    let row_width = region.row_width();
    for (row_index, (base_row, overlay_row)) in
        base.chunks_mut(row_width).zip(overlay.chunks(row_width)).enumerate()
    {
        let row_start = region.global_row_start(row_index);
        for (chunk_index, (base_chunk, overlay_chunk)) in
            base_row.chunks_mut(4_096).zip(overlay_row.chunks(4_096)).enumerate()
        {
            checkpoint()?;
            let chunk_start = row_start + (chunk_index * 4_096) as u64;
            for (index, (base_px, overlay_px)) in
                base_chunk.iter_mut().zip(overlay_chunk.iter()).enumerate()
            {
                *base_px = blend_rgba_f32_pixel_seeded(
                    *base_px,
                    *overlay_px,
                    opacity,
                    blend_mode,
                    effect_graph_dither_seed((chunk_start + index as u64) as u32, frame_seed),
                );
            }
        }
    }
    checkpoint()?;
    Ok(true)
}

fn effect_graph_dither_seed(pixel_index: u32, frame_seed: i64) -> u32 {
    pixel_index ^ (frame_seed as u32).rotate_left(13) ^ ((frame_seed >> 32) as u32).rotate_right(7)
}

fn apply_alpha_mask_in_place(
    input: &mut [u8],
    mask: &[u8],
    invert: bool,
    mask_op: crate::mask::MaskOp,
) {
    use crate::mask::MaskOp;
    for (out_px, mask_px) in input.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
        let mut matte = mask_px[3] as f32 / 255.0;
        if invert {
            matte = 1.0 - matte;
        }
        let src_alpha = out_px[3] as f32 / 255.0;
        let result = match mask_op {
            MaskOp::Add => src_alpha * matte,
            MaskOp::Subtract => src_alpha * (1.0 - matte),
            MaskOp::Intersect => src_alpha.min(matte),
            MaskOp::Difference => (src_alpha - matte).abs(),
        };
        out_px[3] = unit_to_u8(result);
    }
}

fn apply_alpha_mask_f32_in_place(
    input: &mut [[f32; 4]],
    mask: &[[f32; 4]],
    invert: bool,
    mask_op: crate::mask::MaskOp,
) {
    let result: Result<(), std::convert::Infallible> =
        apply_alpha_mask_f32_region_controlled(input, mask, invert, mask_op, &mut || Ok(()));
    debug_assert!(result.is_ok());
}

pub(crate) fn apply_alpha_mask_f32_region_controlled<E>(
    input: &mut [[f32; 4]],
    mask: &[[f32; 4]],
    invert: bool,
    mask_op: crate::mask::MaskOp,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    use crate::mask::MaskOp;
    for (input_chunk, mask_chunk) in input.chunks_mut(4_096).zip(mask.chunks(4_096)) {
        checkpoint()?;
        for (output, matte) in input_chunk.iter_mut().zip(mask_chunk) {
            let matte = if invert {
                1.0 - matte[3].clamp(0.0, 1.0)
            } else {
                matte[3].clamp(0.0, 1.0)
            };
            let source_alpha = output[3].clamp(0.0, 1.0);
            output[3] = match mask_op {
                MaskOp::Add => source_alpha * matte,
                MaskOp::Subtract => source_alpha * (1.0 - matte),
                MaskOp::Intersect => source_alpha.min(matte),
                MaskOp::Difference => (source_alpha - matte).abs(),
            };
        }
    }
    checkpoint()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn region_blend_checks_cancellation_at_bounded_pixel_chunks() {
        let mut base = vec![[0.0, 0.0, 0.0, 1.0]; 8_193];
        let overlay = vec![[1.0, 1.0, 1.0, 1.0]; 8_193];
        let mut checkpoints = 0_u32;
        let result = blend_rgba_f32_region_controlled(
            &mut base,
            &overlay,
            EffectRasterRegion::full_frame(8_193, 1),
            0.5,
            BlendMode::Dissolve,
            17,
            &mut || {
                checkpoints = checkpoints.saturating_add(1);
                if checkpoints == 2 {
                    Err(())
                } else {
                    Ok(())
                }
            },
        );

        assert_eq!(result, Err(()));
        assert_eq!(checkpoints, 2);
    }

    fn color_adjust(exposure: f32, contrast: f32, saturation: f32) -> EffectRenderOp {
        EffectRenderOp::ColorAdjust {
            exposure,
            contrast,
            saturation,
            working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
        }
    }

    fn domain_cache_key(revision: u64) -> EffectDomainProcessorCacheKey {
        let mut fingerprint = [0; 32];
        fingerprint[..8].copy_from_slice(&revision.to_le_bytes());
        EffectDomainProcessorCacheKey::from_complete_semantic_fingerprint(fingerprint)
    }

    #[test]
    fn effect_input_fingerprints_are_content_complete_and_type_separated() {
        assert_ne!(
            frame_buffer_fingerprint(&[0, 1, 2, 3]),
            frame_buffer_fingerprint(&[0, 1, 2, 4])
        );
        assert_ne!(
            frame_buffer_fingerprint_f32(&[[0.0, 0.0, 0.0, 1.0]]),
            frame_buffer_fingerprint_f32(&[[-0.0, 0.0, 0.0, 1.0]])
        );
        assert_ne!(
            frame_buffer_fingerprint(&[0; 16]),
            frame_buffer_fingerprint_f32(&[[0.0; 4]])
        );
    }

    fn custom_u8_contract(determinism: crate::EffectDeterminism) -> crate::EffectExecutionContract {
        crate::EffectExecutionContract {
            execution_modes: crate::EffectExecutionModes::CPU_U8,
            determinism,
            state_model: crate::EffectStateModel::Stateless,
            temporal_input: crate::EffectTemporalInputExtent::CURRENT_FRAME,
            roi_propagation: crate::EffectRoiPropagation::UnknownRequiresFullFrame,
            resource_lifetime: crate::EffectResourceLifetime::Frame,
            topology: crate::EffectGraphTopology::LinearChain,
        }
    }

    fn prepare_custom_u8_plugin(
        key: &str,
        determinism: crate::EffectDeterminism,
        processor: CustomEffectRenderProcessor,
    ) -> Arc<CompiledEffectGraph> {
        let effect_type = mondrian_core::effect_data::EffectType::Plugin(key.to_owned());
        let params_builder: crate::EffectRenderParamsBuilder =
            Arc::new(|_, _| Ok(Some(serde_json::json!({}))));
        crate::register_effect_definition(
            crate::EffectDefinition::new(
                effect_type.key(),
                key,
                Default::default(),
                crate::EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(custom_u8_contract(determinism))
            .with_custom_render_backend(
                params_builder,
                None,
                crate::EffectCachePolicy::Deterministic,
                processor,
            ),
        )
        .expect("register legal test plugin");
        crate::PreparedEffectProgram::prepare(
            &[crate::EffectNode::new(effect_type)],
            &[],
            mondrian_core::WorkingColorSpace::LinearRec709,
        )
        .expect("prepare legal test plugin")
        .evaluate(mondrian_core::TimelineTime::ZERO)
        .expect("evaluate legal test plugin")
    }

    #[test]
    fn definition_frame_seed_contract_constrains_deterministic_custom_cache_keys() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_processor = Arc::clone(&calls);
        let compiled = prepare_custom_u8_plugin(
            "test.cache.definition-frame-seeded",
            crate::EffectDeterminism::FrameSeeded,
            Arc::new(move |pixels, _, _, _, frame_seed| {
                calls_for_processor.fetch_add(1, Ordering::SeqCst);
                for pixel in pixels.chunks_exact_mut(4) {
                    pixel[0] = frame_seed as u8;
                }
                Ok(())
            }),
        );
        let input = [7, 11, 13, 255];

        let mut session = EffectExecutionSession::default();
        let first = session
            .apply_compiled_rgba8(&input, 1, 1, &compiled, 41)
            .expect("first seeded frame");
        let repeated = session
            .apply_compiled_rgba8(&input, 1, 1, &compiled, 41)
            .expect("cached seeded frame");
        let second = session
            .apply_compiled_rgba8(&input, 1, 1, &compiled, 42)
            .expect("second seeded frame");

        assert_eq!(
            compiled.execution_envelope().aggregate().determinism,
            crate::EffectDeterminism::FrameSeeded
        );
        assert_eq!(
            compiled.output_cache_policy(),
            crate::EffectCachePolicy::FrameDependent
        );
        assert!(compiled.output_cache_enabled());
        assert!(compiled
            .node_profiles()
            .values()
            .all(|profile| profile.cache_policy == crate::EffectCachePolicy::FrameDependent));
        assert_eq!(first, repeated);
        assert_ne!(first, second);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn definition_nondeterministic_contract_disables_all_compiled_caches() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_processor = Arc::clone(&calls);
        let compiled = prepare_custom_u8_plugin(
            "test.cache.definition-nondeterministic",
            crate::EffectDeterminism::Nondeterministic,
            Arc::new(move |pixels, _, _, _, _| {
                let invocation = calls_for_processor.fetch_add(1, Ordering::SeqCst) + 1;
                for pixel in pixels.chunks_exact_mut(4) {
                    pixel[0] = invocation as u8;
                }
                Ok(())
            }),
        );
        let input = [7, 11, 13, 255];

        let first =
            apply_compiled_effect_graph(&input, 1, 1, &compiled, 41).expect("first invocation");
        let second =
            apply_compiled_effect_graph(&input, 1, 1, &compiled, 41).expect("second invocation");

        assert_eq!(
            compiled.execution_envelope().aggregate().determinism,
            crate::EffectDeterminism::Nondeterministic
        );
        assert_eq!(
            compiled.output_cache_policy(),
            crate::EffectCachePolicy::Uncacheable
        );
        assert!(!compiled.output_cache_enabled());
        assert!(compiled.node_profiles().values().all(|profile| {
            profile.cache_policy == crate::EffectCachePolicy::Uncacheable
                && !profile.output_cache_enabled
        }));
        assert_ne!(first, second);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn compiled_cpu_executors_admit_contract_before_identity_fast_paths() {
        let effect_type = mondrian_core::effect_data::EffectType::Plugin(
            "test.execution.identity-stateful".to_owned(),
        );
        crate::register_effect_definition(
            crate::EffectDefinition::new(
                effect_type.key(),
                "Stateful identity",
                Default::default(),
                crate::EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(crate::EffectExecutionContract {
                execution_modes: crate::EffectExecutionModes::CPU_U8
                    .union(crate::EffectExecutionModes::CPU_F32),
                determinism: crate::EffectDeterminism::Deterministic,
                state_model: crate::EffectStateModel::StatefulSequential,
                temporal_input: crate::EffectTemporalInputExtent::CURRENT_FRAME,
                roi_propagation: crate::EffectRoiPropagation::PixelLocal,
                resource_lifetime: crate::EffectResourceLifetime::ContinuitySession,
                topology: crate::EffectGraphTopology::LinearChain,
            })
            .with_graph_builder(Arc::new(|_, _, _| Ok(()))),
        )
        .expect("register stateful identity");
        let compiled = crate::PreparedEffectProgram::prepare(
            &[crate::EffectNode::new(effect_type)],
            &[],
            mondrian_core::WorkingColorSpace::LinearRec709,
        )
        .expect("prepare stateful identity")
        .evaluate(mondrian_core::TimelineTime::ZERO)
        .expect("evaluate stateful identity");
        assert!(compiled.graph().is_identity());
        assert!(compiled_effect_graph_has_rgba_f32_execution_shape(
            &compiled
        ));
        assert!(compiled_effect_graph_has_resolvable_rgba_f32_domain(
            &compiled, true
        ));
        assert!(
            !compiled_effect_graph_supports_rgba_f32_with_domain_processor(&compiled),
            "shape/domain queries must not erase continuity admission"
        );

        let expected = EffectExecutionAdmissionError::ContinuitySessionRequired;
        assert_eq!(
            apply_compiled_effect_graph(&[0, 0, 0, 255], 1, 1, &compiled, 0),
            Err(EffectExecutionError::ExecutionContract(expected))
        );
        let mut encoded_out = Vec::new();
        assert_eq!(
            apply_compiled_effect_graph_pass(
                &[0, 0, 0, 255],
                1,
                1,
                &compiled,
                0.0,
                None,
                0,
                &mut encoded_out,
            ),
            Err(EffectExecutionError::ExecutionContract(expected))
        );
        assert_eq!(
            apply_compiled_effect_graph_rgba_f32(&[[0.0, 0.0, 0.0, 1.0]], 1, 1, &compiled, 0,),
            Err(EffectFloatExecutionError::ExecutionContract(expected))
        );
        assert_eq!(
            apply_compiled_effect_graph_pass_rgba_f32(
                &[[0.0, 0.0, 0.0, 1.0]],
                1,
                1,
                &compiled,
                0.0,
                None,
                0,
            ),
            Err(EffectFloatExecutionError::ExecutionContract(expected))
        );
        assert_eq!(
            apply_compiled_effect_graph_pass_rgba_f32_with_domain_processor(
                &[[0.0, 0.0, 0.0, 1.0]],
                1,
                1,
                &compiled,
                0.0,
                None,
                0,
                domain_cache_key(1),
                |_, _| Ok(()),
            ),
            Err(EffectFloatExecutionError::ExecutionContract(expected))
        );
    }

    #[test]
    fn display_encoded_effect_cannot_execute_as_scene_linear_without_ocio_transitions() {
        let display_domain = crate::EffectColorDomain::DisplayEncodedRgb {
            color_space: mondrian_core::ColorSpace::Rec709,
        };
        let compiled = crate::compile_reference_effect_graph_in_domain(
            &EffectRenderPlan { ops: vec![color_adjust(0.25, 1.0, 1.0)] },
            crate::EffectColorDomainContract::preserving(display_domain),
        )
        .expect("valid display-domain graph");

        assert!(compiled_effect_graph_has_rgba_f32_execution_shape(
            &compiled
        ));
        assert!(!compiled_effect_graph_has_resolvable_rgba_f32_domain(
            &compiled, false
        ));
        assert!(compiled_effect_graph_has_resolvable_rgba_f32_domain(
            &compiled, true
        ));
        assert!(!compiled_effect_graph_supports_rgba_f32(&compiled));
        assert_eq!(
            apply_compiled_effect_graph_rgba_f32(&[[0.18, 0.18, 0.18, 1.0]], 1, 1, &compiled, 0,),
            Err(EffectFloatExecutionError::UnsupportedNode {
                node_id: EffectGraphNodeId(1),
                reason: EffectFloatUnsupportedReason::ColorDomainConversionRequired {
                    transitions: 2,
                },
            })
        );
        assert_eq!(
            apply_compiled_effect_graph(&[46, 46, 46, 255], 1, 1, &compiled, 0),
            Err(EffectExecutionError::ColorDomainConversionRequired { transitions: 2 })
        );
    }

    #[test]
    fn domain_processor_executes_in_place_round_trip_around_effect() {
        let display_domain = crate::EffectColorDomain::DisplayEncodedRgb {
            color_space: mondrian_core::ColorSpace::Rec709,
        };
        let compiled = crate::compile_reference_effect_graph_in_domain(
            &EffectRenderPlan { ops: vec![color_adjust(0.0, 1.0, 1.0)] },
            crate::EffectColorDomainContract::preserving(display_domain),
        )
        .expect("valid display-domain graph");
        let mut transitions = Vec::new();

        let output = apply_compiled_effect_graph_rgba_f32_with_domain_processor(
            &[[0.4, 0.2, 0.1, 0.75]],
            1,
            1,
            &compiled,
            0,
            domain_cache_key(7),
            |pixels, transition| {
                transitions.push(transition);
                let scale = if transition.to == display_domain {
                    0.5
                } else {
                    2.0
                };
                for pixel in pixels {
                    for channel in &mut pixel[..3] {
                        *channel *= scale;
                    }
                }
                Ok(())
            },
        )
        .expect("execute graph with resolved domains");

        assert_eq!(transitions, compiled.domain_plan().transitions);
        for (actual, expected) in output[0].iter().zip([0.4, 0.2, 0.1, 0.75]) {
            assert!((actual - expected).abs() < 1.0e-6);
        }
    }

    #[test]
    fn domain_processor_failure_preserves_exact_transition_context() {
        let display_domain = crate::EffectColorDomain::DisplayEncodedRgb {
            color_space: mondrian_core::ColorSpace::Rec709,
        };
        let compiled = crate::compile_reference_effect_graph_in_domain(
            &EffectRenderPlan { ops: vec![color_adjust(0.0, 1.0, 1.0)] },
            crate::EffectColorDomainContract::preserving(display_domain),
        )
        .expect("valid display-domain graph");

        let error = apply_compiled_effect_graph_rgba_f32_with_domain_processor(
            &[[0.4, 0.2, 0.1, 0.75]],
            1,
            1,
            &compiled,
            0,
            domain_cache_key(9_001),
            |_, transition| Err(format!("missing processor for {:?}", transition.to)),
        )
        .expect_err("processor failure must remain structured");

        assert!(matches!(
            error,
            EffectFloatExecutionError::UnsupportedNode {
                reason: EffectFloatUnsupportedReason::ColorDomainTransitionFailed {
                    from: crate::EffectColorDomain::SceneLinearRgb,
                    to,
                    reason,
                },
                ..
            } if to == display_domain && reason.contains("missing processor")
        ));
    }

    #[test]
    fn domain_processed_output_cache_isolated_by_renderer_color_context() {
        let display_domain = crate::EffectColorDomain::DisplayEncodedRgb {
            color_space: mondrian_core::ColorSpace::Rec709,
        };
        let compiled = crate::compile_reference_effect_graph_in_domain(
            &EffectRenderPlan {
                ops: vec![
                    color_adjust(0.0, 1.0, 1.0),
                    EffectRenderOp::Vignette { intensity: 0.1, feather: 0.8 },
                    color_adjust(0.0, 1.1, 0.9),
                    EffectRenderOp::Vignette { intensity: 0.05, feather: 0.6 },
                ],
            },
            crate::EffectColorDomainContract::preserving(display_domain),
        )
        .expect("valid cacheable display-domain graph");
        assert!(compiled.output_cache_enabled());
        let input = [[0.314_159, 0.271_828, 0.161_803, 0.875]];
        let mut processor_calls = 0;
        let mut session = EffectExecutionSession::default();

        for cache_key in [91_001, 91_001, 91_002] {
            session
                .apply_compiled_rgba_f32_with_domain_processor(
                    &input,
                    1,
                    1,
                    &compiled,
                    0,
                    domain_cache_key(cache_key),
                    |_, _| {
                        processor_calls += 1;
                        Ok(())
                    },
                )
                .expect("execute cacheable domain graph");
        }

        assert_eq!(
            processor_calls, 4,
            "same context hits; different context misses"
        );
    }

    #[test]
    fn float_effect_graph_runs_color_adjust_without_clamping_extended_values() {
        let compiled = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![color_adjust(1.0, 1.0, 1.0)],
        })
        .expect("compile color adjust graph");

        let output =
            apply_compiled_effect_graph_rgba_f32(&[[1.25, 0.25, 0.125, 1.0]], 1, 1, &compiled, 0)
                .expect("float color adjust");

        assert!(compiled_effect_graph_supports_rgba_f32(&compiled));
        assert!((output[0][0] - 2.5).abs() <= 1.0e-6);
        assert!((output[0][1] - 0.5).abs() <= 1.0e-6);
        assert!((output[0][2] - 0.25).abs() <= 1.0e-6);
        assert_eq!(output[0][3], 1.0);
    }

    #[test]
    fn float_effect_graph_pass_blends_normal_adjustment_without_clamping_extended_values() {
        let compiled = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![color_adjust(1.0, 1.0, 1.0)],
        })
        .expect("compile color adjust graph");

        let output = apply_compiled_effect_graph_pass_rgba_f32(
            &[[1.25, 0.25, 0.125, 1.0]],
            1,
            1,
            &compiled,
            0.5,
            Some(BlendMode::Normal),
            0,
        )
        .expect("float color adjust pass");

        assert!(compiled_effect_graph_supports_rgba_f32(&compiled));
        assert!((output[0][0] - 1.875).abs() <= 1.0e-6);
        assert!((output[0][1] - 0.375).abs() <= 1.0e-6);
        assert!((output[0][2] - 0.1875).abs() <= 1.0e-6);
        assert_eq!(output[0][3], 1.0);
    }

    #[test]
    fn float_effect_graph_pass_supports_non_normal_blend_modes() {
        let compiled = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![color_adjust(0.0, 1.0, 1.0)],
        })
        .expect("compile color adjust graph");

        let output = apply_compiled_effect_graph_pass_rgba_f32(
            &[[0.25, 0.5, 0.75, 1.0]],
            1,
            1,
            &compiled,
            1.0,
            Some(BlendMode::Multiply),
            0,
        )
        .expect("multiply stays on float pass path");

        assert!((output[0][0] - 0.0625).abs() <= 1.0e-6);
        assert!((output[0][1] - 0.25).abs() <= 1.0e-6);
        assert!((output[0][2] - 0.5625).abs() <= 1.0e-6);
        assert_eq!(output[0][3], 1.0);
    }

    #[test]
    fn float_effect_graph_caches_deterministic_multi_op_output() {
        let compiled = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![
                color_adjust(1.0, 1.0, 1.0),
                EffectRenderOp::Vignette { intensity: 0.1, feather: 0.8 },
                color_adjust(0.0, 1.1, 0.9),
                EffectRenderOp::Vignette { intensity: 0.05, feather: 0.6 },
            ],
        })
        .expect("compile float adjustment chain");
        let input = [[0.25, 0.5, 0.75, 1.0]];
        let mut session = EffectExecutionSession::default();

        assert!(effect_output_cache_key_f32(&input, 1, 1, &compiled, 7, None).is_some());
        assert!(
            get_cached_effect_output_f32(&mut session, &input, 1, 1, &compiled, 7, None).is_none()
        );
        let output = session
            .apply_compiled_rgba_f32(&input, 1, 1, &compiled, 7)
            .expect("float chain");

        assert_eq!(
            get_cached_effect_output_f32(&mut session, &input, 1, 1, &compiled, 7, None),
            Some(output)
        );
    }

    #[test]
    fn float_effect_graph_avoids_output_cache_for_low_cost_adjustments() {
        let compiled = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![
                color_adjust(1.0, 1.0, 1.0),
                EffectRenderOp::Vignette { intensity: 0.1, feather: 0.8 },
            ],
        })
        .expect("compile low-cost float adjustment chain");
        let input = [[0.25, 0.5, 0.75, 1.0]];
        let mut session = EffectExecutionSession::default();

        assert!(!compiled.output_cache_enabled());
        assert!(effect_output_cache_key_f32(&input, 1, 1, &compiled, 7, None).is_none());
        session
            .apply_compiled_rgba_f32(&input, 1, 1, &compiled, 7)
            .expect("low-cost float chain");
        assert!(
            get_cached_effect_output_f32(&mut session, &input, 1, 1, &compiled, 7, None).is_none()
        );
    }

    #[test]
    fn float_effect_graph_rejects_rgba8_only_custom_precision_at_admission() {
        let processor = crate::CustomEffectProcessorBinding::new(Arc::new(
            |_buffer, _width, _height, _params, _frame_seed| Ok(()),
        ));
        let compiled = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![EffectRenderOp::Custom {
                key: "test.custom.rgba8-only".to_owned(),
                params: serde_json::json!({}),
                cache_key: None,
                cache_policy: crate::EffectCachePolicy::Deterministic,
                processor: Some(processor),
            }],
        })
        .expect("compile custom graph");

        let err =
            apply_compiled_effect_graph_rgba_f32(&[[0.25, 0.5, 0.75, 1.0]], 1, 1, &compiled, 0)
                .expect_err("custom effect without float ABI must fail closed");

        assert!(!compiled_effect_graph_supports_rgba_f32(&compiled));
        assert!(matches!(
            err,
            EffectFloatExecutionError::ExecutionContract(
                EffectExecutionAdmissionError::ExecutionModeNotAdmitted {
                    stage_index: 0,
                    backend: EffectProcessingBackend::Cpu,
                    precision: EffectWorkingPrecision::Float32,
                    admitted: crate::EffectExecutionModes::CPU_U8,
                }
            )
        ));
    }

    #[test]
    fn float_branching_blend_graph_preserves_hdr_and_straight_alpha() {
        let graph = EffectRenderGraph {
            nodes: vec![
                crate::EffectGraphNode {
                    id: EffectGraphNodeId(0),
                    kind: EffectGraphNodeKind::Source,
                },
                crate::EffectGraphNode {
                    id: EffectGraphNodeId(1),
                    kind: EffectGraphNodeKind::UnaryEffect {
                        input: EffectGraphNodeId(0),
                        op: color_adjust(1.0, 1.0, 1.0),
                    },
                },
                crate::EffectGraphNode {
                    id: EffectGraphNodeId(2),
                    kind: EffectGraphNodeKind::Blend {
                        base: EffectGraphNodeId(0),
                        overlay: EffectGraphNodeId(1),
                        blend_mode: BlendMode::Normal,
                        opacity: 0.5,
                    },
                },
            ],
            output: Some(EffectGraphNodeId(2)),
        };
        let compiled =
            crate::compile_reference_render_graph(graph).expect("compile branching float graph");
        let input = [[1.5, 0.25, -0.125, 0.5]];

        let output = apply_compiled_effect_graph_rgba_f32(&input, 1, 1, &compiled, 7)
            .expect("execute branching float graph");
        let expected = blend_rgba_f32_pixel_seeded(
            input[0],
            [3.0, 0.5, -0.25, 0.5],
            0.5,
            BlendMode::Normal,
            effect_graph_dither_seed(0, 7),
        );

        assert!(compiled_effect_graph_supports_rgba_f32(&compiled));
        for (actual, expected) in output[0].iter().zip(expected) {
            assert!((actual - expected).abs() <= 1.0e-6);
        }
        assert!(output[0][0] > 1.0);
        assert!(output[0][2] < 0.0);
        assert!((output[0][3] - 0.625).abs() <= f32::EPSILON);
    }

    #[test]
    fn float_mask_graph_uses_unquantized_matte_and_preserves_rgb() {
        let graph = EffectRenderGraph {
            nodes: vec![
                crate::EffectGraphNode {
                    id: EffectGraphNodeId(0),
                    kind: EffectGraphNodeKind::Source,
                },
                crate::EffectGraphNode {
                    id: EffectGraphNodeId(1),
                    kind: EffectGraphNodeKind::MaskSource {
                        shape: crate::mask::MaskShape::Rectangle {
                            x: 0.0,
                            y: 0.0,
                            width: 1.0,
                            height: 1.0,
                            corner_radius: 0.0,
                        },
                        feather: 0.0,
                        expansion: 0.0,
                        opacity: 0.123_456,
                    },
                },
                crate::EffectGraphNode {
                    id: EffectGraphNodeId(2),
                    kind: EffectGraphNodeKind::Mask {
                        input: EffectGraphNodeId(0),
                        mask: EffectGraphNodeId(1),
                        invert: false,
                        mask_op: crate::mask::MaskOp::Add,
                    },
                },
            ],
            output: Some(EffectGraphNodeId(2)),
        };
        let compiled =
            crate::compile_reference_render_graph(graph).expect("compile float mask graph");
        let input = [[2.0, -0.25, 0.5, 0.8]];

        let output = apply_compiled_effect_graph_rgba_f32(&input, 1, 1, &compiled, 0)
            .expect("execute float mask graph");

        assert!(compiled_effect_graph_supports_rgba_f32(&compiled));
        assert_eq!(&output[0][..3], &input[0][..3]);
        assert!((output[0][3] - 0.8 * 0.123_456).abs() <= 1.0e-6);
        assert!((output[0][3] * 255.0 - (output[0][3] * 255.0).round()).abs() > 1.0e-3);
    }

    #[test]
    fn float_multi_input_dissolve_is_frame_dependent_and_cache_safe() {
        let graph = EffectRenderGraph {
            nodes: vec![
                crate::EffectGraphNode {
                    id: EffectGraphNodeId(0),
                    kind: EffectGraphNodeKind::Source,
                },
                crate::EffectGraphNode {
                    id: EffectGraphNodeId(1),
                    kind: EffectGraphNodeKind::UnaryEffect {
                        input: EffectGraphNodeId(0),
                        op: color_adjust(1.0, 1.0, 1.0),
                    },
                },
                crate::EffectGraphNode {
                    id: EffectGraphNodeId(2),
                    kind: EffectGraphNodeKind::MultiInput {
                        inputs: vec![EffectGraphNodeId(0), EffectGraphNodeId(1)],
                        blend_mode: BlendMode::Dissolve,
                        opacity: 0.5,
                    },
                },
            ],
            output: Some(EffectGraphNodeId(2)),
        };
        let compiled =
            crate::compile_reference_render_graph(graph).expect("compile float multi-input graph");
        let input = vec![[0.75, 0.25, 0.125, 1.0]; 64];

        let first = apply_compiled_effect_graph_rgba_f32(&input, 8, 8, &compiled, 1)
            .expect("execute first dissolve frame");
        let repeated = apply_compiled_effect_graph_rgba_f32(&input, 8, 8, &compiled, 1)
            .expect("execute repeated dissolve frame");
        let second = apply_compiled_effect_graph_rgba_f32(&input, 8, 8, &compiled, 2)
            .expect("execute second dissolve frame");

        assert!(compiled_effect_graph_supports_rgba_f32(&compiled));
        assert_eq!(
            compiled.output_cache_policy(),
            crate::EffectCachePolicy::FrameDependent
        );
        assert_eq!(first, repeated);
        assert_ne!(first, second);
        assert!(first.iter().any(|pixel| pixel[0] > 1.0));
        assert!(first.iter().any(|pixel| pixel[0] < 1.0));
    }

    #[test]
    fn float_gaussian_blur_uses_premultiplied_alpha_and_preserves_hdr_color() {
        let compiled = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![EffectRenderOp::GaussianBlur { radius: 1.0 }],
        })
        .expect("compile blur graph");
        let input = [
            [0.0, 8.0, 0.0, 0.0],
            [2.0, 0.25, 0.125, 1.0],
            [0.0, 8.0, 0.0, 0.0],
        ];

        let output =
            apply_compiled_effect_graph_rgba_f32(&input, 3, 1, &compiled, 0).expect("float blur");

        assert!(compiled_effect_graph_supports_rgba_f32(&compiled));
        assert!(output[0][3] > 0.0 && output[0][3] < 1.0);
        assert!((output[0][0] - 2.0).abs() <= 1.0e-5);
        assert!((output[0][1] - 0.25).abs() <= 1.0e-5);
        assert!((output[1][0] - 2.0).abs() <= 1.0e-5);
        assert!(output[1][3] < 1.0);
    }

    #[test]
    fn all_builtin_unary_effects_execute_without_rgba8_quantization() {
        let lut = crate::Lut3D::identity(2).expect("identity LUT");
        let compiled = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![
                EffectRenderOp::GaussianBlur { radius: 0.5 },
                EffectRenderOp::Sharpen { amount: 0.5 },
                EffectRenderOp::Vignette { intensity: 0.25, feather: 0.8 },
                EffectRenderOp::ChromaticAberration { amount: 0.25 },
                EffectRenderOp::Grain { amount: 0.2 },
                EffectRenderOp::Lut3D {
                    lut: Arc::new(crate::PreparedLut3D::new(lut)),
                    intensity: 0.5,
                },
            ],
        })
        .expect("compile built-in graph");
        let input = vec![[1.5, 0.5, 0.25, 1.0]; 9];

        let first = apply_compiled_effect_graph_rgba_f32(&input, 3, 3, &compiled, 17)
            .expect("built-in float graph");
        let second = apply_compiled_effect_graph_rgba_f32(&input, 3, 3, &compiled, 17)
            .expect("deterministic built-in float graph");

        assert!(compiled_effect_graph_supports_rgba_f32(&compiled));
        assert_eq!(first, second);
        assert!(first.iter().all(|pixel| pixel.iter().all(|channel| channel.is_finite())));
        assert!(first.iter().all(|pixel| (pixel[3] - 1.0).abs() <= 1.0e-5));
        assert!(first.iter().any(|pixel| pixel[0] > 1.0));
    }

    #[test]
    fn float_grain_is_frame_dependent_without_clamping_extended_range() {
        let compiled = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![EffectRenderOp::Grain { amount: 1.0 }],
        })
        .expect("compile grain graph");
        let input = vec![[1.25, -0.1, 0.5, 0.75]; 4];

        let first = apply_compiled_effect_graph_rgba_f32(&input, 2, 2, &compiled, 1)
            .expect("grain frame one");
        let second = apply_compiled_effect_graph_rgba_f32(&input, 2, 2, &compiled, 2)
            .expect("grain frame two");

        assert_ne!(first, second);
        assert!(first.iter().all(|pixel| (pixel[3] - 0.75).abs() <= f32::EPSILON));
        assert!(first.iter().any(|pixel| pixel[0] > 1.0));
        assert!(first.iter().any(|pixel| pixel[1] < 0.0));
    }

    #[test]
    fn float_effect_graph_rejects_mismatched_input_extent() {
        let compiled = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![color_adjust(0.0, 1.0, 1.0)],
        })
        .expect("compile color adjust graph");

        let err = apply_compiled_effect_graph_rgba_f32(&[], 1, 1, &compiled, 0)
            .expect_err("input extent mismatch");

        assert!(matches!(
            err,
            EffectFloatExecutionError::InputSizeMismatch { expected: 1, actual: 0 }
        ));
    }
}
