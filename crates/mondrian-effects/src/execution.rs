use crate::adjustment::{
    apply_custom_render_op_f32, apply_render_op, blend_adjustment_result,
    blend_rgba_f32_pixel_seeded, blend_rgba_pixel_seeded, unit_to_u8, EffectRasterRegion,
};
#[cfg(test)]
use crate::{
    compile_reference_effect_graph,
    graph::{compile_effect_domain_plan, effect_graph_node_use_counts},
    EffectRenderPlan,
};
use crate::{
    coverage::has_positive_coverage,
    graph::{EffectExecutionSchedule, EffectGraphIdentity},
    CompiledEffectGraph, EffectDomainTransition, EffectExecutionAdmissionError,
    EffectExecutionSession, EffectExecutionSessionConfig, EffectGraphNodeId, EffectGraphNodeKind,
    EffectProcessingBackend, EffectRenderGraph, EffectRenderOp, EffectWorkingPrecision,
};
use mondrian_core::{types::BlendMode, ExecutionCancellationToken, Result as MondrianResult};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc};

/// An RGBA8 custom visual processor, with straight alpha and encoded channels.
pub type CustomEffectRenderProcessor = Arc<
    dyn Fn(&mut Vec<u8>, u32, u32, &serde_json::Value, i64) -> MondrianResult<()> + Send + Sync,
>;

/// A Float32 custom visual processor, with straight alpha in the graph's color domain.
pub type CustomEffectFloatRenderProcessor = Arc<
    dyn Fn(&mut crate::EffectFloatFrame, &serde_json::Value, i64) -> MondrianResult<()>
        + Send
        + Sync,
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
    /// A requested upstream image could not be delivered. This is distinct
    /// from an absent media value and never publishes a substitute black frame.
    #[error("effect input delivery failed: {reason}")]
    InputUnavailable { reason: String },
    /// Original typed failure from a time-expanded upstream graph. A waiting
    /// custom processor must not be quarantined for this dependency's failure.
    #[error("upstream temporal graph failed: {source}")]
    TemporalInput {
        #[source]
        source: Box<crate::EffectTemporalExecutionError>,
    },
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
    /// The actual image sampling does not meet the frozen provider capability.
    /// No processor work or runtime quarantine is performed for this refusal.
    #[error("effect image scale {actual_sampling:?} does not meet {requirement:?}")]
    ImageScaleNotAdmitted {
        /// Immutable requirement of the work that would execute.
        requirement: crate::EffectImageScaleRequirement,
        /// Sampling carried by the actual input or explicit output staging.
        actual_sampling: mondrian_core::ImageSampling,
    },
    /// A custom processor returned an error or panicked without committing its staged pixels.
    #[error("custom effect processor `{key}` failed: {reason}")]
    CustomProcessorFailed { key: String, reason: String },
    /// The invocation could not acquire its working grant; the plugin remains available.
    #[error(
        "custom effect processor `{key}` requires {required_bytes} bytes; grant is {budget_bytes}"
    )]
    CustomProcessorBudgetExceeded {
        /// Exact compiled processor definition.
        key: String,
        /// Bytes required at the rejected working admission stage.
        required_bytes: usize,
        /// Applicable caller-owned byte grant for that admission stage.
        budget_bytes: usize,
    },
    /// An admitted processor could not acquire host storage before execution.
    /// Its definition is available, and this exact invocation is not replayed.
    #[error("custom effect processor `{key}`: {receipt}")]
    ProcessorHostResourceRefused {
        /// Exact compiler-bound processor definition.
        key: String,
        /// Actual resource refusal and canonical residency decision.
        receipt: crate::EffectProcessorHostResourceRefusalReceipt,
    },
    /// The owning scheduler stopped a custom invocation without quarantining it.
    #[error("custom effect processor invocation stopped: {reason:?}")]
    CustomProcessorStopped {
        /// Authoritative scheduler cancellation/deadline reason.
        reason: crate::EffectProcessorStopReason,
    },
    /// Feedback policy refused another occurrence before its DSP callback.
    #[error("processor feedback occurrence capacity {limit} is exhausted")]
    ProcessorFeedbackCapacityExceeded {
        /// Declared maximum occurrences in this actual work.
        limit: usize,
    },
    /// A legal processor requires authored repair; no pixels were published.
    #[error("custom effect processor `{key}` is blocked: {receipt}")]
    ProcessorUserBlocked {
        /// Compiler-bound provider key, never supplied by a native message.
        key: String,
        /// Actual occurrence, owner, feedback and bank retention decision.
        receipt: std::sync::Arc<crate::EffectProcessorFeedbackReceipt>,
    },
    /// A finite temporal operation entered an executor that owns only one
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
    /// A complete Float value cannot enter the declared consumer materialization.
    #[error(transparent)]
    FrameContract(#[from] crate::EffectFloatFrameError),
    /// The definition-bound program cannot enter this single-frame executor.
    #[error(transparent)]
    ExecutionContract(#[from] EffectExecutionAdmissionError),
}

impl EffectExecutionError {
    /// A recoverable actual processor receipt, without pixel completion evidence.
    pub fn processor_blocked(
        &self,
    ) -> Option<&std::sync::Arc<crate::EffectProcessorFeedbackReceipt>> {
        match self {
            Self::ProcessorUserBlocked { receipt, .. } => Some(receipt),
            _ => None,
        }
    }
}

/// Admit only render scale; PAR and window geometry retain their own authorities.
pub(crate) fn admit_image_scale(
    requirement: crate::EffectImageScaleRequirement,
    actual_sampling: mondrian_core::ImageSampling,
) -> Result<(), EffectExecutionError> {
    if requirement == crate::EffectImageScaleRequirement::Unit && !actual_sampling.is_unit_scale() {
        return Err(EffectExecutionError::ImageScaleNotAdmitted { requirement, actual_sampling });
    }
    Ok(())
}

pub(crate) fn admit_render_op_image_scale(
    op: &EffectRenderOp,
    actual_sampling: mondrian_core::ImageSampling,
) -> Result<(), EffectExecutionError> {
    if let EffectRenderOp::Custom { processor: Some(binding), .. } = op {
        admit_image_scale(binding.image_scale_requirement(), actual_sampling)?;
    }
    Ok(())
}

/// Error returned when an effect graph cannot execute on the float/linear CPU path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectFloatExecutionError {
    /// A complete image value violates its exact association/materialization contract.
    FrameContract(crate::EffectFloatFrameError),
    /// The owning scheduler canceled the frame or expired its deadline.
    Stopped {
        /// Exact observed scheduler reason, never a plugin runtime fault.
        reason: crate::EffectProcessorStopReason,
    },
    /// The definition-bound program cannot enter this single-frame executor.
    ExecutionContract(EffectExecutionAdmissionError),
    /// A bound custom Float32 processor failed without publishing staged pixels.
    CustomProcessor(EffectExecutionError),
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

impl From<crate::EffectFloatFrameError> for EffectFloatExecutionError {
    fn from(error: crate::EffectFloatFrameError) -> Self {
        Self::FrameContract(error)
    }
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
    input_format: crate::EffectValueFormat,
    sampling: mondrian_core::ImageSampling,
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
            EffectGraphNodeKind::CustomSource { .. } => {
                return Err(EffectExecutionError::CustomProcessorUnavailable {
                    key: "float_source".to_owned(),
                });
            }
            EffectGraphNodeKind::UnaryEffect { input: input_id, op }
            | EffectGraphNodeKind::DomainEffect { input: input_id, op, .. } => {
                if let (Some(compiled), Some(input_fingerprint)) =
                    (compiled, source_input_fingerprint)
                    && let Some(cached) = session.as_deref_mut().and_then(|session| {
                        get_cached_node_output(
                            session,
                            compiled,
                            *node_id,
                            width,
                            height,
                            input_fingerprint,
                            frame_seed,
                        )
                    })
                {
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
                    && let Some(session) = session.as_deref_mut()
                {
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
                outputs.insert(node.id, source);
            }
            EffectGraphNodeKind::Blend { base, overlay, blend_mode, opacity } => {
                if let (Some(compiled), Some(input_fingerprint)) =
                    (compiled, source_input_fingerprint)
                    && let Some(cached) = session.as_deref_mut().and_then(|session| {
                        get_cached_node_output(
                            session,
                            compiled,
                            *node_id,
                            width,
                            height,
                            input_fingerprint,
                            frame_seed,
                        )
                    })
                {
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
                    && let Some(session) = session.as_deref_mut()
                {
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
                outputs.insert(node.id, base_frame);
            }
            EffectGraphNodeKind::MaskSource { shape, feather, expansion, opacity, invert } => {
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
                let mut rgba = raster
                    .rasterize_rgba_u8(
                        crate::EffectPixelRoi::new(0, 0, width, height),
                        &cancellation,
                    )
                    .map_err(|error| EffectExecutionError::MaskRasterFailed {
                        node_id: node.id,
                        source: error,
                    })?;
                if *invert {
                    invert_alpha_mask_u8_in_place(&mut rgba);
                }
                outputs.insert(node.id, rgba);
            }
            EffectGraphNodeKind::Mask { input: input_id, mask, invert, mask_op } => {
                if let (Some(compiled), Some(input_fingerprint)) =
                    (compiled, source_input_fingerprint)
                    && let Some(cached) = session.as_deref_mut().and_then(|session| {
                        get_cached_node_output(
                            session,
                            compiled,
                            *node_id,
                            width,
                            height,
                            input_fingerprint,
                            frame_seed,
                        )
                    })
                {
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
                    && let Some(session) = session.as_deref_mut()
                {
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
                outputs.insert(node.id, source);
            }
            EffectGraphNodeKind::MaskCombine { left, right, mask_op } => {
                let Some(mut left_frame) = take_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *left,
                    required_len,
                ) else {
                    return Err(EffectExecutionError::InvalidGraph);
                };
                let Some(right_frame) = take_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *right,
                    required_len,
                ) else {
                    return Err(EffectExecutionError::InvalidGraph);
                };
                combine_alpha_masks_u8_in_place(&mut left_frame, &right_frame, *mask_op);
                release_execution_buffer(&mut buffer_pool, right_frame);
                outputs.insert(node.id, left_frame);
            }
            EffectGraphNodeKind::MatteMix { base, graded, matte } => {
                let Some(mut base_frame) = take_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *base,
                    required_len,
                ) else {
                    return Err(EffectExecutionError::InvalidGraph);
                };
                let Some(graded_frame) = take_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *graded,
                    required_len,
                ) else {
                    return Err(EffectExecutionError::InvalidGraph);
                };
                let Some(matte_frame) = take_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *matte,
                    required_len,
                ) else {
                    return Err(EffectExecutionError::InvalidGraph);
                };
                matte_mix_u8_in_place(&mut base_frame, &graded_frame, &matte_frame);
                release_execution_buffer(&mut buffer_pool, graded_frame);
                release_execution_buffer(&mut buffer_pool, matte_frame);
                outputs.insert(node.id, base_frame);
            }
            EffectGraphNodeKind::MultiInput { inputs, blend_mode, opacity } => {
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
    input: crate::EffectFloatFrameView<'_>,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
) -> Result<crate::EffectFloatFrame, EffectFloatExecutionError> {
    let mut session =
        EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(usize::MAX));
    session.apply_compiled_rgba_f32(
        input,
        compiled,
        frame_seed,
        &mut crate::EffectProcessorControl::uncancelled(),
    )
}

/// Execute a compiled float graph with renderer-owned color-domain processors.
///
/// `domain_cache_key` must be the complete stable semantic fingerprint of the
/// exact color engine/config, working space, dynamic properties, and
/// implementation revision used by `processor`. Reusing a key asserts
/// bit-equivalent processing; a truncated hash or process generation is
/// invalid.
pub fn apply_compiled_effect_graph_rgba_f32_with_domain_processor<F>(
    input: crate::EffectFloatFrameView<'_>,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
    domain_cache_key: EffectDomainProcessorCacheKey,
    mut processor: F,
) -> Result<crate::EffectFloatFrame, EffectFloatExecutionError>
where
    F: FnMut(&mut [[f32; 4]], EffectDomainTransition) -> Result<(), String>,
{
    let mut session =
        EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(usize::MAX));
    session.apply_compiled_rgba_f32_with_domain_processor(
        input,
        compiled,
        frame_seed,
        domain_cache_key,
        &mut processor,
        &mut crate::EffectProcessorControl::uncancelled(),
    )
}

type EffectDomainProcessor<'a> =
    dyn FnMut(&mut [[f32; 4]], EffectDomainTransition) -> Result<(), String> + 'a;

/// Cold admission and per-node pixel reservations for the scalar buffer pool.
fn scalar_processor_reservations(
    compiled: &CompiledEffectGraph,
    width: u32,
    height: u32,
    budget: usize,
    has_resident_processors: bool,
    control: &mut crate::EffectProcessorControl<'_>,
) -> Result<Option<Vec<usize>>, EffectFloatExecutionError> {
    let has_factory = compiled.schedule().ordered_nodes.iter().any(|id| {
        compiled.graph().node(*id).is_some_and(|node| matches!(&node.kind,
            EffectGraphNodeKind::UnaryEffect { op: EffectRenderOp::Custom { processor: Some(binding), .. }, .. }
            | EffectGraphNodeKind::DomainEffect { op: EffectRenderOp::Custom { processor: Some(binding), .. }, .. }
            | EffectGraphNodeKind::CustomSource { op: EffectRenderOp::Custom { processor: Some(binding), .. }, .. }
            if binding.float32_factory().is_some()))
    });
    if !has_factory && !has_resident_processors {
        return Ok(None);
    }
    // The entire route is checked before its first stateful callback. Slot or
    // metadata refusal must never execute a prefix and then replay it elsewhere.
    let requirements = crate::EffectProcessorFeedbackRequirements::for_compiled_graph(compiled);
    let feedback_count = control.feedback_count().saturating_add(requirements.occurrences());
    if feedback_count > crate::EFFECT_PROCESSOR_FEEDBACK_SLOTS {
        return Err(EffectFloatExecutionError::CustomProcessor(
            EffectExecutionError::ProcessorFeedbackCapacityExceeded {
                limit: crate::EFFECT_PROCESSOR_FEEDBACK_SLOTS,
            },
        ));
    }
    let feedback_route_bytes =
        control.feedback_route_bytes(requirements.maximum_owned_bytes(), feedback_count != 0);
    let frame_bytes = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(std::mem::size_of::<[f32; 4]>());
    let mut remaining = compiled.node_use_counts().clone();
    let mut pooled = 0usize;
    let mut allocated = 0usize;
    let mut reservations = Vec::with_capacity(compiled.schedule().ordered_nodes.len());
    for id in &compiled.schedule().ordered_nodes {
        scalar_checkpoint(control)?;
        let node = compiled
            .graph()
            .node(*id)
            .ok_or(EffectFloatExecutionError::MissingOutput { node_id: *id })?;
        let inputs = node.input_ids();
        let sequential_overlays = matches!(node.kind, EffectGraphNodeKind::MultiInput { .. });
        for (index, input) in inputs.iter().enumerate() {
            let uses = remaining
                .get_mut(input)
                .ok_or(EffectFloatExecutionError::MissingOutput { node_id: *input })?;
            *uses = uses
                .checked_sub(1)
                .ok_or(EffectFloatExecutionError::MissingOutput { node_id: *input })?;
            if *uses > 0 {
                if pooled > 0 {
                    pooled -= 1;
                } else {
                    allocated = allocated.saturating_add(1);
                }
            }
            // The executor releases each multi-input overlay immediately.
            if sequential_overlays && index > 0 {
                pooled = pooled.saturating_add(1);
            }
        }
        let (scratch_frames, processor_bytes, key) = match &node.kind {
            EffectGraphNodeKind::Source => {
                if pooled > 0 {
                    pooled -= 1;
                } else {
                    allocated = allocated.saturating_add(1);
                }
                (0, 0, "source")
            }
            EffectGraphNodeKind::CustomSource { op, .. } => {
                if pooled > 0 {
                    pooled -= 1;
                } else {
                    allocated = allocated.saturating_add(1);
                }
                let bytes = crate::processor_residency::frame_processor_charge(op, width, height)
                    .map_err(EffectFloatExecutionError::CustomProcessor)?;
                let key = match op {
                    EffectRenderOp::Custom { key, .. } => key.as_str(),
                    _ => "invalid_source",
                };
                (0, bytes, key)
            }
            EffectGraphNodeKind::MaskSource { .. } => {
                allocated = allocated.saturating_add(1);
                (0, 0, "mask")
            }
            EffectGraphNodeKind::UnaryEffect { op, .. }
            | EffectGraphNodeKind::DomainEffect { op, .. } => {
                let bytes = crate::processor_residency::frame_processor_charge(op, width, height)
                    .map_err(EffectFloatExecutionError::CustomProcessor)?;
                let key = match op {
                    EffectRenderOp::Custom { key, .. } => key.as_str(),
                    _ => "builtin",
                };
                (
                    crate::adjustment::render_op_f32_scratch_frames(op),
                    bytes,
                    key,
                )
            }
            _ => (0, 0, "join"),
        };
        let mask_bytes = if matches!(node.kind, EffectGraphNodeKind::MaskSource { .. }) {
            // Only this raster lives during scalar mask evaluation. The set
            // estimate bounds its geometry and row workspace without allocation.
            let estimate = || -> Result<usize, crate::MaskRasterError> {
                Ok(
                    crate::mask_raster::PreparedMaskRasterSet::required_retained_bytes(
                        compiled.graph(),
                    )?
                    .saturating_add(
                        crate::mask_raster::PreparedMaskRasterSet::required_max_scratch_bytes(
                            compiled.graph(),
                        )?,
                    ),
                )
            };
            estimate().map_err(|source| EffectFloatExecutionError::MaskRasterFailed {
                node_id: *id,
                source,
            })?
        } else {
            0
        };
        let occupied = allocated
            .saturating_add(scratch_frames)
            .saturating_mul(frame_bytes)
            .saturating_add(mask_bytes);
        let required =
            occupied.saturating_add(processor_bytes).saturating_add(feedback_route_bytes);
        if has_factory && required > budget {
            return Err(EffectFloatExecutionError::CustomProcessor(
                EffectExecutionError::CustomProcessorBudgetExceeded {
                    key: key.to_owned(),
                    required_bytes: required,
                    budget_bytes: budget,
                },
            ));
        }
        reservations.push(occupied);
        if inputs.len() > 1 && !sequential_overlays {
            pooled = pooled.saturating_add(inputs.len() - 1);
        }
    }
    Ok(Some(reservations))
}

fn scalar_checkpoint(
    control: &mut crate::EffectProcessorControl<'_>,
) -> Result<(), EffectFloatExecutionError> {
    control
        .checkpoint()
        .map_err(|reason| EffectFloatExecutionError::Stopped { reason })
}

fn scalar_mask_error(
    node_id: EffectGraphNodeId,
    error: crate::mask_raster::ControlledMaskRasterError<EffectFloatExecutionError>,
) -> EffectFloatExecutionError {
    match error {
        crate::mask_raster::ControlledMaskRasterError::Raster(source) => {
            EffectFloatExecutionError::MaskRasterFailed { node_id, source }
        }
        crate::mask_raster::ControlledMaskRasterError::Checkpoint(error) => error,
    }
}

fn finish_scalar_attempt(
    session: &mut EffectExecutionSession,
    control: &mut crate::EffectProcessorControl<'_>,
    result: Result<crate::EffectFloatFrame, EffectFloatExecutionError>,
) -> Result<crate::EffectFloatFrame, EffectFloatExecutionError> {
    // Do not overwrite a completed plugin fault with a later owner stop.
    // Successful output still needs its last publication checkpoint.
    let stopped = if result.is_ok() {
        control.checkpoint().err()
    } else {
        control.stop_reason()
    };
    if let Some(reason) = stopped {
        session.processors.clear();
        return Err(EffectFloatExecutionError::Stopped { reason });
    }
    result
}

// A zero-input node has no incoming alpha association. In an image-processing
// graph its staging request comes from its own retained declaration, not the
// unrelated external Source. Prefer the native Float associations without
// converting any pixels; the provider must still report a valid actual output.
// The explicit source-free generate entry instead keeps the caller's exact
// requested representation and rejects it before provider work if unsupported.
fn source_staging_format(
    compiled: &CompiledEffectGraph,
    node_id: EffectGraphNodeId,
    target: crate::EffectValueFormat,
    has_external_input: bool,
) -> Result<crate::EffectValueFormat, EffectFloatExecutionError> {
    use mondrian_core::ColorFrameAlpha;
    let contract = compiled.node_alpha_contract(node_id);
    let requested = if has_external_input {
        [
            ColorFrameAlpha::StraightFloat,
            ColorFrameAlpha::PremultipliedFloat,
            ColorFrameAlpha::Opaque,
            ColorFrameAlpha::StraightCoverage,
            ColorFrameAlpha::PremultipliedCoverage,
        ]
        .into_iter()
        .find(|alpha| contract.is_some_and(|contract| contract.admits_source_output(*alpha)))
    } else {
        Some(target.alpha())
            .filter(|alpha| contract.is_some_and(|contract| contract.admits_source_output(*alpha)))
    };
    requested
        .map(|alpha| target.with_alpha(alpha))
        .ok_or_else(|| unsupported_float_graph_node(node_id, "source_output_format_not_admitted"))
}

#[allow(clippy::too_many_arguments)]
fn apply_compiled_effect_graph_rgba_f32_inner(
    session: &mut EffectExecutionSession,
    input: Option<crate::EffectFloatFrameView<'_>>,
    extent: crate::EffectFrameExtent,
    format: crate::EffectValueFormat,
    sampling: mondrian_core::ImageSampling,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
    domain_cache_key: Option<EffectDomainProcessorCacheKey>,
    mut processor: Option<&mut EffectDomainProcessor<'_>>,
    control: &mut crate::EffectProcessorControl<'_>,
) -> Result<crate::EffectFloatFrame, EffectFloatExecutionError> {
    let width = extent.width();
    let height = extent.height();
    if let Some(input) = input {
        input.validate_controlled(&mut || scalar_checkpoint(control))?;
    }
    scalar_checkpoint(control)?;
    if format.precision() != crate::EffectWorkingPrecision::Float32 {
        return Err(
            crate::EffectFloatFrameError::WrongPrecision { precision: format.precision() }.into(),
        );
    }
    let required_len = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| unsupported_float_graph_node(EffectGraphNodeId(0), "extent_overflow"))?;
    required_len
        .checked_mul(std::mem::size_of::<[f32; 4]>())
        .ok_or(crate::EffectFloatFrameError::ExtentOverflow)?;
    for id in &compiled.schedule().ordered_nodes {
        if compiled
            .graph()
            .node(*id)
            .is_some_and(|node| matches!(node.kind, EffectGraphNodeKind::CustomSource { .. }))
        {
            source_staging_format(compiled, *id, format, input.is_some())?;
        }
    }
    let external_source = compiled.schedule().ordered_nodes.iter().find_map(|id| {
        compiled
            .graph()
            .node(*id)
            .filter(|node| matches!(node.kind, EffectGraphNodeKind::Source))
            .map(|node| node.id)
    });
    if let Some(id) = external_source {
        let source = input
            .ok_or_else(|| unsupported_float_graph_node(id, "external_source_requires_image"))?;
        if source.format().domain() != crate::EffectColorDomain::SceneLinearRgb {
            return Err(EffectFloatExecutionError::FrameContract(
                crate::EffectFloatFrameError::FormatMismatch {
                    expected: source.format().with_domain(crate::EffectColorDomain::SceneLinearRgb),
                    actual: source.format(),
                },
            ));
        }
    }
    validate_float_effect_graph(compiled, processor.is_some())?;
    if required_len == 0 {
        // Empty unary behavior stays a copy; zero-input native extents reject.
        return input
            .ok_or_else(|| {
                unsupported_float_graph_node(EffectGraphNodeId(0), "empty_source_extent")
            })?
            .to_owned()
            .map_err(EffectFloatExecutionError::FrameContract);
    }
    admit_image_scale(compiled.image_scale_requirement(), sampling)
        .map_err(EffectFloatExecutionError::CustomProcessor)?;
    let reservations = scalar_processor_reservations(
        compiled,
        width,
        height,
        session.max_working_bytes(),
        session.processors.counts().0 != 0,
        control,
    )?;
    if let Some(reservations) = &reservations {
        // Shared owner resources may serve another clip later in this frame.
        // Keep only the siblings that fit beside this graph's cold pixel peak,
        // including identity/cache copies. Optional retention cannot deny a
        // built-in-only invocation that has no native admission requirement.
        let occupied = reservations.iter().copied().max().unwrap_or(0);
        session
            .processors
            .trim_working(session.max_working_bytes().saturating_sub(occupied));
    }
    if compiled.graph().is_identity() {
        let input = input.ok_or_else(|| {
            unsupported_float_graph_node(EffectGraphNodeId(0), "identity_requires_image")
        })?;
        let output = crate::EffectFloatFrame::from_validated_parts(
            input.extent(),
            input.format(),
            input.sampling(),
            input.pixels().to_vec(),
        );
        scalar_checkpoint(control)?;
        return Ok(output);
    }
    if let Some(cached) = input.filter(|_| !control.uses_input_service()).and_then(|input| {
        get_cached_effect_output_f32(session, input, compiled, frame_seed, domain_cache_key)
    }) {
        scalar_checkpoint(control)?;
        return Ok(cached);
    }

    let mut outputs = HashMap::<EffectGraphNodeId, crate::EffectFloatFrame>::with_capacity(
        compiled.graph().nodes.len(),
    );
    let mut remaining_uses = compiled.node_use_counts().clone();
    let mut buffer_pool = Vec::<Vec<[f32; 4]>>::new();
    for (step, node_id) in compiled.schedule().ordered_nodes.iter().enumerate() {
        scalar_checkpoint(control)?;
        if let Some(reservations) = &reservations {
            session
                .processors
                .trim_working(session.max_working_bytes().saturating_sub(reservations[step]));
        }
        let Some(node) = compiled.graph().node(*node_id) else {
            return Err(EffectFloatExecutionError::UnsupportedNode {
                node_id: *node_id,
                reason: EffectFloatUnsupportedReason::UnsupportedGraphNode { kind: "missing" },
            });
        };
        match &node.kind {
            EffectGraphNodeKind::Source => {
                let input = input.ok_or_else(|| {
                    unsupported_float_graph_node(node.id, "external_source_requires_image")
                })?;
                let mut frame = take_float_execution_buffer(&mut buffer_pool, required_len);
                frame.copy_from_slice(input.pixels());
                outputs.insert(
                    node.id,
                    crate::EffectFloatFrame::from_validated_parts(
                        input.extent(),
                        input.format(),
                        input.sampling(),
                        frame,
                    ),
                );
            }
            EffectGraphNodeKind::CustomSource { op, domain_contract } => {
                // Cold sizing precedes this allocation. Reuse the scalar pool,
                // then charge its actual capacity together with all siblings
                // before the bank can prepare or invoke the provider.
                let mut frame = buffer_pool.pop().unwrap_or_default();
                let required_bytes = required_len
                    .checked_mul(std::mem::size_of::<[f32; 4]>())
                    .ok_or(crate::EffectFloatFrameError::ExtentOverflow)?;
                frame.try_reserve_exact(required_len.saturating_sub(frame.len())).map_err(
                    |_| crate::EffectFloatFrameError::AllocationUnavailable { required_bytes },
                )?;
                frame.truncate(required_len);
                while frame.len() < required_len {
                    scalar_checkpoint(control)?;
                    frame.resize(required_len.min(frame.len().saturating_add(4096)), [0.; 4]);
                }
                let mut output = crate::EffectFloatFrame::from_validated_parts(
                    extent,
                    source_staging_format(compiled, node.id, format, input.is_some())?
                        .with_domain(domain_contract.output),
                    sampling,
                    frame,
                );
                let caller_owned_bytes = outputs
                    .values()
                    .chain(std::iter::once(&output))
                    .fold(0usize, |bytes, frame| {
                        bytes.saturating_add(frame.retained_bytes())
                    })
                    .saturating_add(buffer_pool.iter().fold(0usize, |bytes, frame| {
                        bytes.saturating_add(
                            frame.capacity().saturating_mul(std::mem::size_of::<[f32; 4]>()),
                        )
                    }));
                control
                    .with_input_stage(None, |control| {
                        crate::adjustment::apply_custom_source_op_f32(
                            &mut output,
                            op,
                            frame_seed,
                            session,
                            caller_owned_bytes,
                            control,
                        )
                    })
                    .map_err(EffectFloatExecutionError::CustomProcessor)?;
                outputs.insert(node.id, output);
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
                source = apply_effect_domain_transition(
                    compiled,
                    Some(node.id),
                    *input_id,
                    source,
                    &mut processor,
                )?;
                let supported = if matches!(op, EffectRenderOp::Custom { .. }) {
                    let caller_owned_bytes = outputs
                        .values()
                        .chain(std::iter::once(&source))
                        .fold(0usize, |bytes, frame| {
                            bytes.saturating_add(frame.retained_bytes())
                        })
                        .saturating_add(buffer_pool.iter().fold(0usize, |bytes, buffer| {
                            bytes.saturating_add(
                                buffer.capacity().saturating_mul(std::mem::size_of::<[f32; 4]>()),
                            )
                        }));
                    let input_stage = control.input_stage_for(compiled, node.id);
                    control
                        .with_input_stage(input_stage, |control| {
                            apply_custom_render_op_f32(
                                &mut source,
                                op,
                                frame_seed,
                                Some(session),
                                caller_owned_bytes,
                                control,
                            )
                        })
                        .map_err(EffectFloatExecutionError::CustomProcessor)?;
                    true
                } else {
                    source = crate::materialize_straight_coverage(
                        source,
                        crate::EffectAlphaConsumer::StraightCoverageOperation,
                    )
                    .map_err(EffectFloatExecutionError::FrameContract)?;
                    source.set_format_after_write(
                        source
                            .format()
                            .with_alpha(mondrian_core::ColorFrameAlpha::StraightCoverage),
                    );
                    crate::adjustment::apply_render_op_f32_controlled(
                        source.allocation_mut(),
                        width,
                        height,
                        op,
                        frame_seed,
                        &mut || scalar_checkpoint(control),
                    )?
                };
                if !supported {
                    return Err(EffectFloatExecutionError::UnsupportedNode {
                        node_id: node.id,
                        reason: EffectFloatUnsupportedReason::UnsupportedRenderOp {
                            op: effect_render_op_name(op),
                        },
                    });
                }
                source.validate().map_err(EffectFloatExecutionError::FrameContract)?;
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
                overlay_frame = apply_effect_domain_transition(
                    compiled,
                    Some(node.id),
                    *overlay,
                    overlay_frame,
                    &mut processor,
                )?;
                crate::blend_float_frames_in_place(
                    &mut base_frame,
                    overlay_frame.view(),
                    *opacity,
                    *blend_mode,
                    frame_seed,
                )
                .map_err(EffectFloatExecutionError::FrameContract)?;
                release_float_execution_buffer(&mut buffer_pool, overlay_frame);
                outputs.insert(node.id, base_frame);
            }
            EffectGraphNodeKind::MaskSource { shape, feather, expansion, opacity, invert } => {
                let raster = crate::PreparedMaskRaster::prepare_controlled(
                    shape,
                    crate::EffectFrameExtent::new(width, height),
                    *feather,
                    *expansion,
                    *opacity,
                    &mut || scalar_checkpoint(control),
                )
                .map_err(|error| scalar_mask_error(node.id, error))?;
                let mut rgba = raster
                    .rasterize_rgba_f32_controlled(
                        crate::EffectPixelRoi::new(0, 0, width, height),
                        &mut || scalar_checkpoint(control),
                    )
                    .map_err(|error| scalar_mask_error(node.id, error))?;
                if *invert {
                    invert_alpha_mask_f32_in_place(&mut rgba);
                }
                let format = crate::EffectValueFormat::new(
                    EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::AlphaMask,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                );
                outputs.insert(
                    node.id,
                    crate::EffectFloatFrame::from_pixels(
                        crate::EffectFrameExtent::new(width, height),
                        format,
                        sampling,
                        rgba,
                    )
                    .map_err(EffectFloatExecutionError::FrameContract)?,
                );
            }
            EffectGraphNodeKind::Mask { input: input_id, mask, invert, mask_op } => {
                let Some(source) = take_float_graph_input(
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
                let mut source = crate::materialize_straight_coverage(
                    source,
                    crate::EffectAlphaConsumer::AlphaMask,
                )
                .map_err(EffectFloatExecutionError::FrameContract)?;
                let mask_frame = crate::materialize_straight_coverage(
                    mask_frame,
                    crate::EffectAlphaConsumer::AlphaMask,
                )
                .map_err(EffectFloatExecutionError::FrameContract)?;
                source.set_format_after_write(
                    source.format().with_alpha(mondrian_core::ColorFrameAlpha::StraightCoverage),
                );
                source.ensure_same_sampling(mask_frame.sampling())?;
                apply_alpha_mask_f32_in_place(&mut source, &mask_frame, *invert, *mask_op);
                release_float_execution_buffer(&mut buffer_pool, mask_frame);
                outputs.insert(node.id, source);
            }
            EffectGraphNodeKind::MaskCombine { left, right, mask_op } => {
                let Some(left_frame) = take_float_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *left,
                    required_len,
                ) else {
                    return Err(EffectFloatExecutionError::MissingOutput { node_id: *left });
                };
                let Some(right_frame) = take_float_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *right,
                    required_len,
                ) else {
                    return Err(EffectFloatExecutionError::MissingOutput { node_id: *right });
                };
                let mut left_frame = crate::materialize_straight_coverage(
                    left_frame,
                    crate::EffectAlphaConsumer::AlphaMask,
                )
                .map_err(EffectFloatExecutionError::FrameContract)?;
                let right_frame = crate::materialize_straight_coverage(
                    right_frame,
                    crate::EffectAlphaConsumer::AlphaMask,
                )
                .map_err(EffectFloatExecutionError::FrameContract)?;
                left_frame.set_format_after_write(
                    left_frame
                        .format()
                        .with_alpha(mondrian_core::ColorFrameAlpha::StraightCoverage),
                );
                left_frame.ensure_same_sampling(right_frame.sampling())?;
                combine_alpha_masks_f32_in_place(&mut left_frame, &right_frame, *mask_op);
                release_float_execution_buffer(&mut buffer_pool, right_frame);
                outputs.insert(node.id, left_frame);
            }
            EffectGraphNodeKind::MatteMix { base, graded, matte } => {
                let Some(base_frame) = take_float_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *base,
                    required_len,
                ) else {
                    return Err(EffectFloatExecutionError::MissingOutput { node_id: *base });
                };
                let Some(graded_frame) = take_float_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *graded,
                    required_len,
                ) else {
                    return Err(EffectFloatExecutionError::MissingOutput { node_id: *graded });
                };
                let Some(matte_frame) = take_float_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *matte,
                    required_len,
                ) else {
                    return Err(EffectFloatExecutionError::MissingOutput { node_id: *matte });
                };
                let mut base_frame = crate::materialize_straight_coverage(
                    base_frame,
                    crate::EffectAlphaConsumer::AlphaMask,
                )
                .map_err(EffectFloatExecutionError::FrameContract)?;
                let graded_frame = crate::materialize_straight_coverage(
                    graded_frame,
                    crate::EffectAlphaConsumer::AlphaMask,
                )
                .map_err(EffectFloatExecutionError::FrameContract)?;
                let matte_frame = crate::materialize_straight_coverage(
                    matte_frame,
                    crate::EffectAlphaConsumer::AlphaMask,
                )
                .map_err(EffectFloatExecutionError::FrameContract)?;
                base_frame.set_format_after_write(
                    base_frame
                        .format()
                        .with_alpha(mondrian_core::ColorFrameAlpha::StraightCoverage),
                );
                base_frame.ensure_same_sampling(graded_frame.sampling())?;
                base_frame.ensure_same_sampling(matte_frame.sampling())?;
                matte_mix_f32_in_place(&mut base_frame, &graded_frame, &matte_frame);
                release_float_execution_buffer(&mut buffer_pool, graded_frame);
                release_float_execution_buffer(&mut buffer_pool, matte_frame);
                outputs.insert(node.id, base_frame);
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
                    overlay = apply_effect_domain_transition(
                        compiled,
                        Some(node.id),
                        *overlay_id,
                        overlay,
                        &mut processor,
                    )?;
                    crate::blend_float_frames_in_place(
                        &mut result,
                        overlay.view(),
                        *opacity,
                        *blend_mode,
                        frame_seed,
                    )
                    .map_err(EffectFloatExecutionError::FrameContract)?;
                    release_float_execution_buffer(&mut buffer_pool, overlay);
                }
                outputs.insert(node.id, result);
            }
        }
    }

    let Some(output_id) = compiled.graph().output else {
        let input = input
            .ok_or_else(|| unsupported_float_graph_node(EffectGraphNodeId(0), "missing_output"))?;
        let output = crate::EffectFloatFrame::from_validated_parts(
            input.extent(),
            input.format(),
            input.sampling(),
            input.pixels().to_vec(),
        );
        scalar_checkpoint(control)?;
        return Ok(output);
    };
    let mut output = outputs
        .remove(&output_id)
        .ok_or(EffectFloatExecutionError::MissingOutput { node_id: output_id })?;
    output = apply_effect_domain_transition(compiled, None, output_id, output, &mut processor)?;
    output.validate().map_err(EffectFloatExecutionError::FrameContract)?;
    scalar_checkpoint(control)?;
    if let Some(input) = input.filter(|_| !control.uses_input_service()) {
        put_cached_effect_output_f32(
            session,
            input,
            compiled,
            frame_seed,
            domain_cache_key,
            &output,
        );
    }
    Ok(output)
}

impl EffectExecutionSession {
    /// Execute a zero-input current-frame graph into explicit output staging.
    /// Reachable external Source values reject before invoking any provider.
    pub fn generate_compiled_rgba_f32(
        &mut self,
        extent: crate::EffectFrameExtent,
        format: crate::EffectValueFormat,
        sampling: mondrian_core::ImageSampling,
        compiled: &CompiledEffectGraph,
        frame_seed: i64,
        control: &mut crate::EffectProcessorControl<'_>,
    ) -> Result<crate::EffectFloatFrame, EffectFloatExecutionError> {
        let result = apply_compiled_effect_graph_rgba_f32_inner(
            self, None, extent, format, sampling, compiled, frame_seed, None, None, control,
        );
        finish_scalar_attempt(self, control, result)
    }

    /// Execute one current-frame compiled graph over scene-linear Float32
    /// pixels.
    pub fn apply_compiled_rgba_f32(
        &mut self,
        input: crate::EffectFloatFrameView<'_>,
        compiled: &CompiledEffectGraph,
        frame_seed: i64,
        control: &mut crate::EffectProcessorControl<'_>,
    ) -> Result<crate::EffectFloatFrame, EffectFloatExecutionError> {
        let result = apply_compiled_effect_graph_rgba_f32_inner(
            self,
            Some(input),
            input.extent(),
            input.format(),
            input.sampling(),
            compiled,
            frame_seed,
            None,
            None,
            control,
        );
        finish_scalar_attempt(self, control, result)
    }

    /// Execute one current-frame Float32 graph while resolving explicit RGB
    /// processing-domain transitions through the caller's color Adapter.
    pub fn apply_compiled_rgba_f32_with_domain_processor<F>(
        &mut self,
        input: crate::EffectFloatFrameView<'_>,
        compiled: &CompiledEffectGraph,
        frame_seed: i64,
        domain_cache_key: EffectDomainProcessorCacheKey,
        mut processor: F,
        control: &mut crate::EffectProcessorControl<'_>,
    ) -> Result<crate::EffectFloatFrame, EffectFloatExecutionError>
    where
        F: FnMut(&mut [[f32; 4]], EffectDomainTransition) -> Result<(), String>,
    {
        let result = apply_compiled_effect_graph_rgba_f32_inner(
            self,
            Some(input),
            input.extent(),
            input.format(),
            input.sampling(),
            compiled,
            frame_seed,
            Some(domain_cache_key),
            Some(&mut processor),
            control,
        );
        finish_scalar_attempt(self, control, result)
    }
}

fn apply_effect_domain_transition(
    compiled: &CompiledEffectGraph,
    consumer: Option<EffectGraphNodeId>,
    input: EffectGraphNodeId,
    mut frame: crate::EffectFloatFrame,
    processor: &mut Option<&mut EffectDomainProcessor<'_>>,
) -> Result<crate::EffectFloatFrame, EffectFloatExecutionError> {
    let Some(transition) = compiled
        .domain_plan()
        .transitions
        .iter()
        .find(|transition| transition.consumer == consumer && transition.input == input)
        .copied()
    else {
        return Ok(frame);
    };
    let expected = frame.format().with_domain(transition.from);
    if frame.format() != expected {
        return Err(EffectFloatExecutionError::FrameContract(
            crate::EffectFloatFrameError::FormatMismatch { expected, actual: frame.format() },
        ));
    }
    // A nonlinear stock-OCIO processor consumes independent RGB. Every nonzero
    // alpha is used directly at Float32 execution precision; zero-alpha emission
    // refuses this materialization instead of transforming associated channels.
    // Division and reassociation do not promise bit-reversible pixel values.
    let straight = if frame.alpha().is_coverage() {
        if frame.alpha() == mondrian_core::ColorFrameAlpha::Opaque {
            mondrian_core::ColorFrameAlpha::Opaque
        } else {
            mondrian_core::ColorFrameAlpha::StraightCoverage
        }
    } else {
        mondrian_core::ColorFrameAlpha::StraightFloat
    };
    frame = frame.into_alpha(straight).map_err(EffectFloatExecutionError::FrameContract)?;
    let Some(processor) = processor.as_deref_mut() else {
        return Err(EffectFloatExecutionError::UnsupportedNode {
            node_id: consumer.unwrap_or(input),
            reason: EffectFloatUnsupportedReason::ColorDomainConversionRequired {
                transitions: compiled.domain_plan().transitions.len(),
            },
        });
    };
    processor(frame.pixels_mut(), transition).map_err(|reason| {
        EffectFloatExecutionError::UnsupportedNode {
            node_id: consumer.unwrap_or(input),
            reason: EffectFloatUnsupportedReason::ColorDomainTransitionFailed {
                from: transition.from,
                to: transition.to,
                reason,
            },
        }
    })?;
    frame.set_format_after_write(frame.format().with_domain(transition.to));
    frame.validate().map_err(EffectFloatExecutionError::FrameContract)?;
    Ok(frame)
}

/// Execute a compiled adjustment graph and blend the result over a float base frame.
pub fn apply_compiled_effect_graph_pass_rgba_f32(
    base: crate::EffectFloatFrameView<'_>,
    compiled: &CompiledEffectGraph,
    opacity: f32,
    blend_policy: mondrian_core::EffectPassBlendPolicy,
    frame_seed: i64,
) -> Result<crate::EffectFloatFrame, EffectFloatExecutionError> {
    let mut session =
        EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(usize::MAX));
    session.apply_compiled_pass_rgba_f32(
        base,
        compiled,
        opacity,
        blend_policy,
        frame_seed,
        &mut crate::EffectProcessorControl::uncancelled(),
    )
}

impl EffectExecutionSession {
    /// Execute and blend one current-frame Float32 adjustment graph.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_compiled_pass_rgba_f32(
        &mut self,
        base: crate::EffectFloatFrameView<'_>,
        compiled: &CompiledEffectGraph,
        opacity: f32,
        blend_policy: mondrian_core::EffectPassBlendPolicy,
        frame_seed: i64,
        control: &mut crate::EffectProcessorControl<'_>,
    ) -> Result<crate::EffectFloatFrame, EffectFloatExecutionError> {
        if let Err(error) = base.validate_controlled(&mut || scalar_checkpoint(control)) {
            return finish_scalar_attempt(self, control, Err(error));
        }
        if let Err(error) = scalar_checkpoint(control) {
            return finish_scalar_attempt(self, control, Err(error));
        }
        self.apply_compiled_pass_rgba_f32_owned(
            base.to_owned().map_err(EffectFloatExecutionError::FrameContract)?,
            compiled,
            opacity,
            blend_policy,
            frame_seed,
            control,
        )
    }

    /// Execute and blend one adjustment graph into an owned Float32 base.
    ///
    /// The caller transfers the base allocation across the Effect execution
    /// Seam. Non-trivial passes reuse it for the blended output instead of
    /// cloning a second full-frame base after graph execution.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_compiled_pass_rgba_f32_owned(
        &mut self,
        mut base: crate::EffectFloatFrame,
        compiled: &CompiledEffectGraph,
        opacity: f32,
        blend_policy: mondrian_core::EffectPassBlendPolicy,
        frame_seed: i64,
        control: &mut crate::EffectProcessorControl<'_>,
    ) -> Result<crate::EffectFloatFrame, EffectFloatExecutionError> {
        if let Err(error) = base.view().validate_controlled(&mut || scalar_checkpoint(control)) {
            return finish_scalar_attempt(self, control, Err(error));
        }
        if let Err(error) = scalar_checkpoint(control) {
            return finish_scalar_attempt(self, control, Err(error));
        }
        admit_single_frame_execution(
            compiled,
            EffectProcessingBackend::Cpu,
            EffectWorkingPrecision::Float32,
        )
        .map_err(EffectFloatExecutionError::ExecutionContract)?;
        if base.is_empty() {
            return Ok(base);
        }
        let policy = blend_policy;
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err(EffectFloatExecutionError::FrameContract(
                crate::EffectFloatFrameError::InvalidOpacity,
            ));
        }
        if !has_positive_coverage(opacity)
            || (policy == crate::EffectPassBlendPolicy::Mix && compiled.graph().is_identity())
        {
            return Ok(base);
        }

        let processed = self.apply_compiled_rgba_f32(base.view(), compiled, frame_seed, control)?;
        let blend_result = match policy {
            crate::EffectPassBlendPolicy::Mix => crate::mix_float_frames_controlled(
                &mut base,
                processed.view(),
                opacity,
                &mut || scalar_checkpoint(control),
            ),
            crate::EffectPassBlendPolicy::Blend(mode) => crate::blend_float_frames_controlled(
                &mut base,
                processed.view(),
                opacity,
                mode,
                frame_seed,
                &mut || scalar_checkpoint(control),
            ),
        };

        let result = blend_result.and_then(|()| scalar_checkpoint(control)).map(|()| base);
        finish_scalar_attempt(self, control, result)
    }
}

/// Execute and blend a float adjustment graph with renderer-owned domain processors.
pub fn apply_compiled_effect_graph_pass_rgba_f32_with_domain_processor<F>(
    base: crate::EffectFloatFrameView<'_>,
    compiled: &CompiledEffectGraph,
    opacity: f32,
    blend_policy: mondrian_core::EffectPassBlendPolicy,
    frame_seed: i64,
    domain_cache_key: EffectDomainProcessorCacheKey,
    processor: F,
) -> Result<crate::EffectFloatFrame, EffectFloatExecutionError>
where
    F: FnMut(&mut [[f32; 4]], EffectDomainTransition) -> Result<(), String>,
{
    let mut session =
        EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(usize::MAX));
    session.apply_compiled_pass_rgba_f32_with_domain_processor(
        base,
        compiled,
        opacity,
        blend_policy,
        frame_seed,
        domain_cache_key,
        processor,
        &mut crate::EffectProcessorControl::uncancelled(),
    )
}

impl EffectExecutionSession {
    /// Execute and blend one current-frame Float32 adjustment graph with
    /// renderer-owned color-domain conversion.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_compiled_pass_rgba_f32_with_domain_processor<F>(
        &mut self,
        base: crate::EffectFloatFrameView<'_>,
        compiled: &CompiledEffectGraph,
        opacity: f32,
        blend_policy: mondrian_core::EffectPassBlendPolicy,
        frame_seed: i64,
        domain_cache_key: EffectDomainProcessorCacheKey,
        processor: F,
        control: &mut crate::EffectProcessorControl<'_>,
    ) -> Result<crate::EffectFloatFrame, EffectFloatExecutionError>
    where
        F: FnMut(&mut [[f32; 4]], EffectDomainTransition) -> Result<(), String>,
    {
        if let Err(error) = base.validate_controlled(&mut || scalar_checkpoint(control)) {
            return finish_scalar_attempt(self, control, Err(error));
        }
        if let Err(error) = scalar_checkpoint(control) {
            return finish_scalar_attempt(self, control, Err(error));
        }
        self.apply_compiled_pass_rgba_f32_owned_with_domain_processor(
            base.to_owned().map_err(EffectFloatExecutionError::FrameContract)?,
            compiled,
            opacity,
            blend_policy,
            frame_seed,
            domain_cache_key,
            processor,
            control,
        )
    }

    /// Execute and blend one adjustment graph into an owned Float32 base while
    /// resolving explicit RGB processing-domain transitions.
    ///
    /// This is the ownership-preserving counterpart to
    /// [`Self::apply_compiled_pass_rgba_f32_with_domain_processor`].
    #[allow(clippy::too_many_arguments)]
    pub fn apply_compiled_pass_rgba_f32_owned_with_domain_processor<F>(
        &mut self,
        mut base: crate::EffectFloatFrame,
        compiled: &CompiledEffectGraph,
        opacity: f32,
        blend_policy: mondrian_core::EffectPassBlendPolicy,
        frame_seed: i64,
        domain_cache_key: EffectDomainProcessorCacheKey,
        processor: F,
        control: &mut crate::EffectProcessorControl<'_>,
    ) -> Result<crate::EffectFloatFrame, EffectFloatExecutionError>
    where
        F: FnMut(&mut [[f32; 4]], EffectDomainTransition) -> Result<(), String>,
    {
        if let Err(error) = base.view().validate_controlled(&mut || scalar_checkpoint(control)) {
            return finish_scalar_attempt(self, control, Err(error));
        }
        if let Err(error) = scalar_checkpoint(control) {
            return finish_scalar_attempt(self, control, Err(error));
        }
        admit_single_frame_execution(
            compiled,
            EffectProcessingBackend::Cpu,
            EffectWorkingPrecision::Float32,
        )
        .map_err(EffectFloatExecutionError::ExecutionContract)?;
        if base.is_empty() {
            return Ok(base);
        }
        let policy = blend_policy;
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err(EffectFloatExecutionError::FrameContract(
                crate::EffectFloatFrameError::InvalidOpacity,
            ));
        }
        if !has_positive_coverage(opacity)
            || (policy == crate::EffectPassBlendPolicy::Mix && compiled.graph().is_identity())
        {
            return Ok(base);
        }

        let processed = self.apply_compiled_rgba_f32_with_domain_processor(
            base.view(),
            compiled,
            frame_seed,
            domain_cache_key,
            processor,
            control,
        )?;
        let blend_result = match policy {
            crate::EffectPassBlendPolicy::Mix => crate::mix_float_frames_controlled(
                &mut base,
                processed.view(),
                opacity,
                &mut || scalar_checkpoint(control),
            ),
            crate::EffectPassBlendPolicy::Blend(mode) => crate::blend_float_frames_controlled(
                &mut base,
                processed.view(),
                opacity,
                mode,
                frame_seed,
                &mut || scalar_checkpoint(control),
            ),
        };

        let result = blend_result.and_then(|()| scalar_checkpoint(control)).map(|()| base);
        finish_scalar_attempt(self, control, result)
    }
}

/// Execute and blend a compiled graph, failing on unresolved domains.
pub fn apply_compiled_effect_graph_pass(
    base: &[u8],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    opacity: f32,
    blend_policy: mondrian_core::EffectPassBlendPolicy,
    frame_seed: i64,
    out: &mut Vec<u8>,
) -> std::result::Result<(), EffectExecutionError> {
    let mut session =
        EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(usize::MAX));
    session.apply_compiled_pass_rgba8(
        base,
        width,
        height,
        compiled,
        opacity,
        blend_policy,
        frame_seed,
        out,
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
        blend_policy: mondrian_core::EffectPassBlendPolicy,
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
        if !has_positive_coverage(opacity.clamp(0.0, 1.0))
            || (blend_policy == crate::EffectPassBlendPolicy::Mix && compiled.graph().is_identity())
        {
            out.copy_from_slice(base);
            return Ok(());
        }

        let processed = self.apply_compiled_rgba8(base, width, height, compiled, frame_seed)?;
        blend_adjustment_result(base, &processed, width, height, opacity, blend_policy, out);
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
    input: crate::EffectFloatFrameView<'_>,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
    domain_cache_key: Option<EffectDomainProcessorCacheKey>,
) -> Option<crate::EffectFloatFrame> {
    let key = effect_output_cache_key_f32(input, compiled, frame_seed, domain_cache_key)?;
    session.get_float_output(&key)
}

fn put_cached_effect_output_f32(
    session: &mut EffectExecutionSession,
    input: crate::EffectFloatFrameView<'_>,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
    domain_cache_key: Option<EffectDomainProcessorCacheKey>,
    output: &crate::EffectFloatFrame,
) {
    let Some(key) = effect_output_cache_key_f32(input, compiled, frame_seed, domain_cache_key)
    else {
        return;
    };
    session.put_float_output(key, output.clone());
}

fn effect_output_cache_key_f32(
    input: crate::EffectFloatFrameView<'_>,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
    domain_cache_key: Option<EffectDomainProcessorCacheKey>,
) -> Option<EffectFloatOutputCacheKey> {
    if !effect_output_cache_enabled_f32(compiled, domain_cache_key.is_some()) || input.is_empty() {
        return None;
    }
    Some(EffectFloatOutputCacheKey {
        graph_identity: compiled.identity().clone(),
        input_fingerprint: frame_buffer_fingerprint_f32(input.pixels()),
        width: input.extent().width(),
        height: input.extent().height(),
        input_format: input.format(),
        sampling: input.sampling(),
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
    if !domain_processor_available
        && let Some(transition) = compiled.domain_plan().transitions.first()
    {
        return Err(EffectFloatExecutionError::UnsupportedNode {
            node_id: transition.consumer.unwrap_or(transition.input),
            reason: EffectFloatUnsupportedReason::ColorDomainConversionRequired {
                transitions: compiled.domain_plan().transitions.len(),
            },
        });
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
            EffectGraphNodeKind::CustomSource { op, .. }
            | EffectGraphNodeKind::UnaryEffect { op, .. }
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
            | EffectGraphNodeKind::MaskCombine { .. }
            | EffectGraphNodeKind::MatteMix { .. }
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
    match op {
        EffectRenderOp::Custom { processor, .. } => {
            processor.as_ref().is_some_and(|binding| binding.supports_float32())
        }
        EffectRenderOp::TemporalFrameBlend { .. } => false,
        _ => true,
    }
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
        EffectRenderOp::WhiteBalance { .. } => "white_balance",
        EffectRenderOp::Primaries { .. } => "primaries",
        EffectRenderOp::HdrGrading { .. } => "hdr_grading",
        EffectRenderOp::AscCdl { .. } => "asc_cdl",
        EffectRenderOp::GamutCompression { .. } => "gamut_compression",
        EffectRenderOp::HighlightRecovery { .. } => "highlight_recovery",
        EffectRenderOp::ColorCurves { .. } => "color_curves",
        EffectRenderOp::HueSaturationLightness { .. } => "hue_saturation_lightness",
        EffectRenderOp::ChromaKey { .. } => "chroma_key",
        EffectRenderOp::Qualifier { .. } => "qualifier",
        EffectRenderOp::MattePreview { .. } => "matte_preview",
        EffectRenderOp::GaussianBlur { .. } => "gaussian_blur",
        EffectRenderOp::Sharpen { .. } => "sharpen",
        EffectRenderOp::Vignette { .. } => "vignette",
        EffectRenderOp::ChromaticAberration { .. } => "chromatic_aberration",
        EffectRenderOp::Grain { .. } => "grain",
        EffectRenderOp::Invert { .. } => "invert",
        EffectRenderOp::BoxBlur { .. } => "box_blur",
        EffectRenderOp::HueSaturation { .. } => "hue_saturation",
        EffectRenderOp::BlackAndWhite { .. } => "black_and_white",
        EffectRenderOp::ShiftChannels { .. } => "shift_channels",
        EffectRenderOp::DirectionalBlur { .. } => "directional_blur",
        EffectRenderOp::ChannelMixer { .. } => "channel_mixer",
        EffectRenderOp::FindEdges { .. } => "find_edges",
        EffectRenderOp::Threshold { .. } => "threshold",
        EffectRenderOp::Mosaic { .. } => "mosaic",
        EffectRenderOp::Posterize { .. } => "posterize",
        EffectRenderOp::Tint { .. } => "tint",
        EffectRenderOp::Vibrance { .. } => "vibrance",
        EffectRenderOp::Tritone { .. } => "tritone",
        EffectRenderOp::RadialBlur { .. } => "radial_blur",
        EffectRenderOp::Convolution { .. } => "convolution",
        EffectRenderOp::MotionTile { .. } => "motion_tile",
        EffectRenderOp::Kaleidoscope { .. } => "kaleidoscope",
        EffectRenderOp::Bevel { .. } => "bevel",
        EffectRenderOp::Scatter { .. } => "scatter",
        EffectRenderOp::BrushStrokes { .. } => "brush_strokes",
        EffectRenderOp::ColorEmboss { .. } => "color_emboss",
        EffectRenderOp::Emboss { .. } => "emboss",
        EffectRenderOp::DropShadow { .. } => "drop_shadow",
        EffectRenderOp::UnsharpMask { .. } => "unsharp_mask",
        EffectRenderOp::Crop { .. } => "crop",
        EffectRenderOp::TemporalFrameBlend { .. } => "temporal_frame_blend",
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
    outputs: &mut HashMap<EffectGraphNodeId, crate::EffectFloatFrame>,
    remaining_uses: &mut HashMap<EffectGraphNodeId, usize>,
    buffer_pool: &mut Vec<Vec<[f32; 4]>>,
    node_id: EffectGraphNodeId,
    required_len: usize,
) -> Option<crate::EffectFloatFrame> {
    let remaining = remaining_uses.get_mut(&node_id)?;
    if *remaining == 0 {
        return None;
    }
    *remaining -= 1;
    if *remaining == 0 {
        return outputs.remove(&node_id);
    }
    let source = outputs.get(&node_id)?;
    let mut storage = take_float_execution_buffer(buffer_pool, required_len);
    storage.copy_from_slice(source.pixels());
    Some(crate::EffectFloatFrame::from_validated_parts(
        source.extent(),
        source.format(),
        source.sampling(),
        storage,
    ))
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

fn release_float_execution_buffer(
    buffer_pool: &mut Vec<Vec<[f32; 4]>>,
    buffer: crate::EffectFloatFrame,
) {
    let mut storage = buffer.into_parts().3;
    storage.clear();
    buffer_pool.push(storage);
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
    if !has_positive_coverage(opacity) {
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

pub(crate) fn effect_graph_dither_seed(pixel_index: u32, frame_seed: i64) -> u32 {
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

fn invert_alpha_mask_u8_in_place(mask: &mut [u8]) {
    for pixel in mask.chunks_exact_mut(4) {
        pixel[3] = u8::MAX.saturating_sub(pixel[3]);
    }
}

pub(crate) fn invert_alpha_mask_f32_in_place(mask: &mut [[f32; 4]]) {
    for pixel in mask {
        pixel[3] = 1.0 - pixel[3].clamp(0.0, 1.0);
    }
}

fn combine_alpha_masks_u8_in_place(left: &mut [u8], right: &[u8], mask_op: crate::mask::MaskOp) {
    for (left_pixel, right_pixel) in left.chunks_exact_mut(4).zip(right.chunks_exact(4)) {
        let a = left_pixel[3] as f32 / 255.0;
        let b = right_pixel[3] as f32 / 255.0;
        left_pixel[3] = unit_to_u8(combine_mask_coverage(a, b, mask_op));
    }
}

fn matte_mix_u8_in_place(base: &mut [u8], graded: &[u8], matte: &[u8]) {
    for ((base_pixel, graded_pixel), matte_pixel) in
        base.chunks_exact_mut(4).zip(graded.chunks_exact(4)).zip(matte.chunks_exact(4))
    {
        let weight = matte_pixel[3] as f32 / 255.0;
        for channel in 0..3 {
            base_pixel[channel] = unit_to_u8(
                (base_pixel[channel] as f32 / 255.0) * (1.0 - weight)
                    + (graded_pixel[channel] as f32 / 255.0) * weight,
            );
        }
    }
}

fn combine_mask_coverage(a: f32, b: f32, mask_op: crate::mask::MaskOp) -> f32 {
    use crate::mask::MaskOp;
    let a = a.clamp(0.0, 1.0);
    let b = b.clamp(0.0, 1.0);
    match mask_op {
        MaskOp::Add => a.max(b),
        MaskOp::Subtract => a * (1.0 - b),
        MaskOp::Intersect => a.min(b),
        MaskOp::Difference => (a - b).abs(),
    }
}

pub(crate) fn combine_alpha_masks_f32_in_place(
    left: &mut [[f32; 4]],
    right: &[[f32; 4]],
    mask_op: crate::mask::MaskOp,
) {
    for (left_pixel, right_pixel) in left.iter_mut().zip(right) {
        left_pixel[3] = combine_mask_coverage(left_pixel[3], right_pixel[3], mask_op);
    }
}

pub(crate) fn combine_alpha_masks_f32_controlled<E>(
    left: &mut [[f32; 4]],
    right: &[[f32; 4]],
    mask_op: crate::mask::MaskOp,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    for (left_chunk, right_chunk) in left.chunks_mut(4_096).zip(right.chunks(4_096)) {
        checkpoint()?;
        combine_alpha_masks_f32_in_place(left_chunk, right_chunk, mask_op);
    }
    checkpoint()?;
    Ok(())
}

pub(crate) fn matte_mix_f32_in_place(
    base: &mut [[f32; 4]],
    graded: &[[f32; 4]],
    matte: &[[f32; 4]],
) {
    for ((base_pixel, graded_pixel), matte_pixel) in base.iter_mut().zip(graded).zip(matte) {
        let weight = matte_pixel[3].clamp(0.0, 1.0);
        for channel in 0..3 {
            base_pixel[channel] =
                base_pixel[channel] * (1.0 - weight) + graded_pixel[channel] * weight;
        }
        // Grade mattes are picture-color selectors, never coverage authority.
        // Retain the base alpha verbatim, including HDR/float edge values.
    }
}

pub(crate) fn matte_mix_f32_controlled<E>(
    base: &mut [[f32; 4]],
    graded: &[[f32; 4]],
    matte: &[[f32; 4]],
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    for ((base_chunk, graded_chunk), matte_chunk) in
        base.chunks_mut(4_096).zip(graded.chunks(4_096)).zip(matte.chunks(4_096))
    {
        checkpoint()?;
        matte_mix_f32_in_place(base_chunk, graded_chunk, matte_chunk);
    }
    checkpoint()?;
    Ok(())
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
    #[test]
    fn scalar_owner_stop_blocks_empty_identity_and_cached_output() {
        use crate::{EffectProcessorControl, EffectProcessorStopReason};
        let graph = crate::compile_reference_effect_graph(&crate::EffectRenderPlan {
            ops: vec![crate::EffectRenderOp::ColorAdjust {
                exposure: 1.0,
                contrast: 1.0,
                saturation: 1.0,
                working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
            }],
        })
        .expect("graph");
        let mut session = crate::EffectExecutionSession::default();
        let input = [[0.25, 0.5, 1.5, 0.5]];
        session
            .apply_compiled_rgba_f32(
                crate::EffectFloatFrameView::new(
                    crate::EffectFrameExtent::new(1, 1),
                    crate::EffectValueFormat::new(
                        crate::EffectWorkingPrecision::Float32,
                        crate::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &input,
                ),
                &graph,
                0,
                &mut EffectProcessorControl::uncancelled(),
            )
            .expect("populate");
        let mut checkpoint = || Some(EffectProcessorStopReason::Canceled);
        assert_eq!(
            session.apply_compiled_rgba_f32(
                crate::EffectFloatFrameView::new(
                    crate::EffectFrameExtent::new(1, 1),
                    crate::EffectValueFormat::new(
                        crate::EffectWorkingPrecision::Float32,
                        crate::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &input
                ),
                &graph,
                0,
                &mut EffectProcessorControl::new(&mut checkpoint)
            ),
            Err(super::EffectFloatExecutionError::Stopped {
                reason: EffectProcessorStopReason::Canceled
            })
        );
        let identity = crate::compile_reference_effect_graph(&crate::EffectRenderPlan::default())
            .expect("identity");
        for (pixels, width, height) in [(&input[..], 1, 1), (&[][..], 0, 0)] {
            let mut checkpoint = || Some(EffectProcessorStopReason::DeadlineExpired);
            assert_eq!(
                session.apply_compiled_rgba_f32(
                    crate::EffectFloatFrameView::new(
                        crate::EffectFrameExtent::new(width, height),
                        crate::EffectValueFormat::new(
                            crate::EffectWorkingPrecision::Float32,
                            crate::EffectColorDomain::SceneLinearRgb,
                            mondrian_core::ColorFrameAlpha::StraightCoverage
                        ),
                        mondrian_core::ImageSampling::UNIT_SQUARE,
                        pixels
                    ),
                    &identity,
                    0,
                    &mut EffectProcessorControl::new(&mut checkpoint)
                ),
                Err(super::EffectFloatExecutionError::Stopped {
                    reason: EffectProcessorStopReason::DeadlineExpired
                })
            );
        }
        assert_eq!(input, [[0.25, 0.5, 1.5, 0.5]]);
    }

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

    fn assert_scalar_admission_stops_before_invalid_tail(entry: u8) {
        let extent = crate::EffectFrameExtent::new(65_537, 1);
        let format = crate::EffectValueFormat::new(
            crate::EffectWorkingPrecision::Float32,
            crate::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightCoverage,
        );
        let mut input = crate::EffectFloatFrame::from_pixels(
            extent,
            format,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            vec![[0.25, -2.0, 8.0, 0.5]; 65_537],
        )
        .expect("complete initial owner");
        input.pixels_mut()[65_536][0] = f32::NAN;
        let graph =
            compile_reference_effect_graph(&EffectRenderPlan::default()).expect("identity graph");
        let mut session = EffectExecutionSession::default();
        let mut calls = 0;
        let mut owner = || {
            calls += 1;
            (calls == 2).then_some(crate::EffectProcessorStopReason::DeadlineExpired)
        };
        let mut control = crate::EffectProcessorControl::new(&mut owner);
        let result = match entry {
            0 => session.apply_compiled_rgba_f32(input.view(), &graph, 0, &mut control),
            1 => session.apply_compiled_pass_rgba_f32(
                input.view(),
                &graph,
                1.0,
                mondrian_core::EffectPassBlendPolicy::Mix,
                0,
                &mut control,
            ),
            2 => session.apply_compiled_pass_rgba_f32_owned(
                input,
                &graph,
                1.0,
                mondrian_core::EffectPassBlendPolicy::Mix,
                0,
                &mut control,
            ),
            3 => session.apply_compiled_pass_rgba_f32_with_domain_processor(
                input.view(),
                &graph,
                1.0,
                mondrian_core::EffectPassBlendPolicy::Mix,
                0,
                domain_cache_key(0),
                |_, _| Ok(()),
                &mut control,
            ),
            4 => session.apply_compiled_pass_rgba_f32_owned_with_domain_processor(
                input,
                &graph,
                1.0,
                mondrian_core::EffectPassBlendPolicy::Mix,
                0,
                domain_cache_key(0),
                |_, _| Ok(()),
                &mut control,
            ),
            _ => unreachable!("owned test entry"),
        };
        let error =
            result.expect_err("owner stop prevents publication and examining the next block");
        assert_eq!(
            error,
            EffectFloatExecutionError::Stopped {
                reason: crate::EffectProcessorStopReason::DeadlineExpired,
            }
        );
        assert_eq!(
            calls, 2,
            "the second bounded block is the exact owner frontier"
        );
    }

    macro_rules! scalar_admission_control {
        ($name:ident, $entry:expr) => {
            #[test]
            fn $name() {
                assert_scalar_admission_stops_before_invalid_tail($entry);
            }
        };
    }
    scalar_admission_control!(scalar_admission_borrowed_graph_stops_before_invalid_tail, 0);
    scalar_admission_control!(scalar_admission_borrowed_pass_stops_before_invalid_tail, 1);
    scalar_admission_control!(scalar_admission_owned_pass_stops_before_invalid_tail, 2);
    scalar_admission_control!(
        scalar_admission_borrowed_domain_pass_stops_before_invalid_tail,
        3
    );
    scalar_admission_control!(
        scalar_admission_owned_domain_pass_stops_before_invalid_tail,
        4
    );

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

    fn prepare_custom_u8_extension(
        key: &str,
        determinism: crate::EffectDeterminism,
        processor: CustomEffectRenderProcessor,
    ) -> Arc<CompiledEffectGraph> {
        let effect_type = mondrian_core::effect_data::EffectType::External(key.to_owned());
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
        .expect("register legal test extension");
        crate::PreparedEffectProgram::prepare(
            &[crate::EffectNode::new(effect_type)],
            &[],
            mondrian_core::WorkingColorSpace::LinearRec709,
        )
        .expect("prepare legal test extension")
        .evaluate(mondrian_core::TimelineTime::ZERO)
        .expect("evaluate legal test extension")
    }

    #[test]
    fn definition_frame_seed_contract_constrains_deterministic_custom_cache_keys() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_processor = Arc::clone(&calls);
        let compiled = prepare_custom_u8_extension(
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
        let compiled = prepare_custom_u8_extension(
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
        let effect_type = mondrian_core::effect_data::EffectType::External(
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
                mondrian_core::EffectPassBlendPolicy::Mix,
                0,
                &mut encoded_out,
            ),
            Err(EffectExecutionError::ExecutionContract(expected))
        );
        assert_eq!(
            apply_compiled_effect_graph_rgba_f32(
                crate::EffectFloatFrameView::new(
                    crate::EffectFrameExtent::new(1, 1),
                    crate::EffectValueFormat::new(
                        crate::EffectWorkingPrecision::Float32,
                        crate::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &[[0.0, 0.0, 0.0, 1.0]]
                ),
                &compiled,
                0
            ),
            Err(EffectFloatExecutionError::ExecutionContract(expected))
        );
        assert_eq!(
            apply_compiled_effect_graph_pass_rgba_f32(
                crate::EffectFloatFrameView::new(
                    crate::EffectFrameExtent::new(1, 1),
                    crate::EffectValueFormat::new(
                        crate::EffectWorkingPrecision::Float32,
                        crate::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &[[0.0, 0.0, 0.0, 1.0]]
                ),
                &compiled,
                0.0,
                mondrian_core::EffectPassBlendPolicy::Mix,
                0
            ),
            Err(EffectFloatExecutionError::ExecutionContract(expected))
        );
        assert_eq!(
            apply_compiled_effect_graph_pass_rgba_f32_with_domain_processor(
                crate::EffectFloatFrameView::new(
                    crate::EffectFrameExtent::new(1, 1),
                    crate::EffectValueFormat::new(
                        crate::EffectWorkingPrecision::Float32,
                        crate::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &[[0.0, 0.0, 0.0, 1.0]]
                ),
                &compiled,
                0.0,
                mondrian_core::EffectPassBlendPolicy::Mix,
                0,
                domain_cache_key(1),
                |_, _| Ok(())
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
            apply_compiled_effect_graph_rgba_f32(
                crate::EffectFloatFrameView::new(
                    crate::EffectFrameExtent::new(1, 1),
                    crate::EffectValueFormat::new(
                        crate::EffectWorkingPrecision::Float32,
                        crate::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &[[0.18, 0.18, 0.18, 1.0]]
                ),
                &compiled,
                0
            ),
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
        let sampling = mondrian_core::ImageSampling::new(
            mondrian_core::SampleAspectRatio::new(3, 2).unwrap(),
            [
                mondrian_core::Rational::new(1, 2),
                mondrian_core::Rational::new(2, 3),
            ],
        )
        .unwrap();

        let output = apply_compiled_effect_graph_rgba_f32_with_domain_processor(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(1, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                sampling,
                &[[0.4, 0.2, 0.1, 0.75]],
            ),
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

        assert_eq!(output.sampling(), sampling);
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
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(1, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &[[0.4, 0.2, 0.1, 0.75]],
            ),
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
                    crate::EffectFloatFrameView::new(
                        crate::EffectFrameExtent::new(1, 1),
                        crate::EffectValueFormat::new(
                            crate::EffectWorkingPrecision::Float32,
                            crate::EffectColorDomain::SceneLinearRgb,
                            mondrian_core::ColorFrameAlpha::StraightCoverage,
                        ),
                        mondrian_core::ImageSampling::UNIT_SQUARE,
                        &input,
                    ),
                    &compiled,
                    0,
                    domain_cache_key(cache_key),
                    |_, _| {
                        processor_calls += 1;
                        Ok(())
                    },
                    &mut crate::EffectProcessorControl::uncancelled(),
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

        let output = apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(1, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &[[1.25, 0.25, 0.125, 1.0]],
            ),
            &compiled,
            0,
        )
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
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(1, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &[[1.25, 0.25, 0.125, 1.0]],
            ),
            &compiled,
            0.5,
            mondrian_core::EffectPassBlendPolicy::Blend(BlendMode::Normal),
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
    fn owned_float_effect_graph_pass_reuses_the_base_allocation() {
        let compiled = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![color_adjust(1.0, 1.0, 1.0)],
        })
        .expect("compile color adjust graph");
        let base = vec![[1.25, 0.25, 0.125, 1.0]; 64];
        let base_allocation = base.as_ptr();
        let mut session = EffectExecutionSession::default();

        let output = session
            .apply_compiled_pass_rgba_f32_owned(
                crate::EffectFloatFrame::from_pixels(
                    crate::EffectFrameExtent::new(8, 8),
                    crate::EffectValueFormat::new(
                        crate::EffectWorkingPrecision::Float32,
                        crate::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    base,
                )
                .expect("complete owned Float input"),
                &compiled,
                0.5,
                mondrian_core::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                0,
                &mut crate::EffectProcessorControl::uncancelled(),
            )
            .expect("owned float color adjust pass");

        assert_eq!(output.as_ptr(), base_allocation);
        assert!((output[0][0] - 1.875).abs() <= 1.0e-6);
    }

    #[test]
    fn float_effect_graph_pass_supports_non_normal_blend_modes() {
        let compiled = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![color_adjust(0.0, 1.0, 1.0)],
        })
        .expect("compile color adjust graph");

        let output = apply_compiled_effect_graph_pass_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(1, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &[[0.25, 0.5, 0.75, 1.0]],
            ),
            &compiled,
            1.0,
            mondrian_core::EffectPassBlendPolicy::Blend(BlendMode::Multiply),
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

        let input_view = crate::EffectFloatFrameView::new(
            crate::EffectFrameExtent::new(1, 1),
            crate::EffectValueFormat::new(
                crate::EffectWorkingPrecision::Float32,
                crate::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightCoverage,
            ),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            &input,
        );
        assert!(effect_output_cache_key_f32(input_view, &compiled, 7, None).is_some());
        assert!(
            get_cached_effect_output_f32(&mut session, input_view, &compiled, 7, None).is_none()
        );
        let output = session
            .apply_compiled_rgba_f32(
                crate::EffectFloatFrameView::new(
                    crate::EffectFrameExtent::new(1, 1),
                    crate::EffectValueFormat::new(
                        crate::EffectWorkingPrecision::Float32,
                        crate::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &input,
                ),
                &compiled,
                7,
                &mut crate::EffectProcessorControl::uncancelled(),
            )
            .expect("float chain");

        assert_eq!(
            get_cached_effect_output_f32(&mut session, input_view, &compiled, 7, None),
            Some(output)
        );
    }

    #[test]
    fn float_output_cache_isolates_equal_pixels_with_different_sampling() {
        let compiled = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![
                color_adjust(1.0, 1.0, 1.0),
                EffectRenderOp::Vignette { intensity: 0.1, feather: 0.8 },
                color_adjust(0.0, 1.1, 0.9),
                EffectRenderOp::Vignette { intensity: 0.05, feather: 0.6 },
            ],
        })
        .unwrap();
        let extent = crate::EffectFrameExtent::new(1, 1);
        let format = crate::EffectValueFormat::new(
            crate::EffectWorkingPrecision::Float32,
            crate::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightCoverage,
        );
        let sampling = mondrian_core::ImageSampling::new(
            mondrian_core::SampleAspectRatio::new(4, 3).unwrap(),
            [
                mondrian_core::Rational::new(1, 2),
                mondrian_core::Rational::new(2, 3),
            ],
        )
        .unwrap();
        let pixels = [[0.25, 0.5, 0.75, 1.0]];
        let first = crate::EffectFloatFrameView::new(extent, format, sampling, &pixels);
        let second = crate::EffectFloatFrameView::new(
            extent,
            format,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            &pixels,
        );
        assert_ne!(
            effect_output_cache_key_f32(first, &compiled, 7, None),
            effect_output_cache_key_f32(second, &compiled, 7, None)
        );
        let mut session = EffectExecutionSession::default();
        let original = session
            .apply_compiled_rgba_f32(
                first,
                &compiled,
                7,
                &mut crate::EffectProcessorControl::uncancelled(),
            )
            .unwrap();
        assert_eq!(original.sampling(), sampling);
        assert!(get_cached_effect_output_f32(&mut session, second, &compiled, 7, None).is_none());
        let other = session
            .apply_compiled_rgba_f32(
                second,
                &compiled,
                7,
                &mut crate::EffectProcessorControl::uncancelled(),
            )
            .unwrap();
        assert_eq!(other.sampling(), second.sampling());
        assert_eq!(
            get_cached_effect_output_f32(&mut session, first, &compiled, 7, None),
            Some(original)
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
        let input_view = crate::EffectFloatFrameView::new(
            crate::EffectFrameExtent::new(1, 1),
            crate::EffectValueFormat::new(
                crate::EffectWorkingPrecision::Float32,
                crate::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightCoverage,
            ),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            &input,
        );
        assert!(effect_output_cache_key_f32(input_view, &compiled, 7, None).is_none());
        session
            .apply_compiled_rgba_f32(
                crate::EffectFloatFrameView::new(
                    crate::EffectFrameExtent::new(1, 1),
                    crate::EffectValueFormat::new(
                        crate::EffectWorkingPrecision::Float32,
                        crate::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &input,
                ),
                &compiled,
                7,
                &mut crate::EffectProcessorControl::uncancelled(),
            )
            .expect("low-cost float chain");
        assert!(
            get_cached_effect_output_f32(&mut session, input_view, &compiled, 7, None).is_none()
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

        let err = apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(1, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &[[0.25, 0.5, 0.75, 1.0]],
            ),
            &compiled,
            0,
        )
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
    fn float_custom_processor_preserves_hdr_pixels_and_uses_float_admission() {
        let processor = crate::CustomEffectProcessorBinding::new_f32(Arc::new(
            |pixels, params, _frame_seed| {
                assert_eq!((pixels.width(), pixels.height()), (1, 1));
                pixels[0][0] += params["gain"].as_f64().unwrap_or_default() as f32;
                Ok(())
            },
        ));
        let compiled = compile_reference_effect_graph(&EffectRenderPlan {
            ops: vec![EffectRenderOp::Custom {
                key: "test.custom.float32".to_owned(),
                params: serde_json::json!({ "gain": 0.75 }),
                cache_key: None,
                cache_policy: crate::EffectCachePolicy::Deterministic,
                processor: Some(processor),
            }],
        })
        .expect("compile float custom graph");

        assert!(compiled_effect_graph_supports_rgba_f32(&compiled));
        assert_eq!(
            apply_compiled_effect_graph_rgba_f32(
                crate::EffectFloatFrameView::new(
                    crate::EffectFrameExtent::new(1, 1),
                    crate::EffectValueFormat::new(
                        crate::EffectWorkingPrecision::Float32,
                        crate::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &[[2.0, -0.25, 0.5, 1.0]]
                ),
                &compiled,
                0
            )
            .expect("execute float custom graph")
            .pixels(),
            vec![[2.75, -0.25, 0.5, 1.0]]
        );
    }

    #[test]
    fn float_custom_processor_failure_keeps_original_pixels() {
        let processor = crate::CustomEffectProcessorBinding::new_f32(Arc::new(|pixels, _, _| {
            pixels[0][0] = 0.0;
            Err(mondrian_core::MondrianError::EffectGraphEvaluationFailed {
                reason: "processor failed".to_owned(),
            })
        }));
        let op = EffectRenderOp::Custom {
            key: "test.custom.failure".to_owned(),
            params: serde_json::json!({}),
            cache_key: None,
            cache_policy: crate::EffectCachePolicy::Uncacheable,
            processor: Some(processor),
        };
        let mut pixels = crate::EffectFloatFrame::from_pixels(
            crate::EffectFrameExtent::new(1, 1),
            crate::EffectValueFormat::new(
                crate::EffectWorkingPrecision::Float32,
                crate::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightCoverage,
            ),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            vec![[2.0, -0.25, 0.5, 1.0]],
        )
        .expect("typed staging");
        assert!(matches!(
            apply_custom_render_op_f32(
                &mut pixels,
                &op,
                0,
                None,
                16,
                &mut crate::EffectProcessorControl::uncancelled()
            ),
            Err(EffectExecutionError::CustomProcessorFailed { .. })
        ));
        assert_eq!(pixels.pixels(), vec![[2.0, -0.25, 0.5, 1.0]]);
        assert_eq!(crate::adjustment::render_op_f32_scratch_frames(&op), 1);
    }

    #[test]
    fn float_custom_processor_cannot_change_pixel_count() {
        let processor = crate::CustomEffectProcessorBinding::new_f32(Arc::new(|pixels, _, _| {
            *pixels = crate::EffectFloatFrame::from_pixels(
                crate::EffectFrameExtent::new(0, 0),
                pixels.format(),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                Vec::new(),
            )
            .expect("malformed owned extent");
            Ok(())
        }));
        let op = EffectRenderOp::Custom {
            key: "test.custom.resize".to_owned(),
            params: serde_json::json!({}),
            cache_key: None,
            cache_policy: crate::EffectCachePolicy::Uncacheable,
            processor: Some(processor),
        };
        let mut pixels = crate::EffectFloatFrame::from_pixels(
            crate::EffectFrameExtent::new(1, 1),
            crate::EffectValueFormat::new(
                crate::EffectWorkingPrecision::Float32,
                crate::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightCoverage,
            ),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            vec![[2.0, -0.25, 0.5, 1.0]],
        )
        .expect("typed staging");
        assert!(matches!(
            apply_custom_render_op_f32(
                &mut pixels,
                &op,
                0,
                None,
                16,
                &mut crate::EffectProcessorControl::uncancelled()
            ),
            Err(EffectExecutionError::CustomProcessorFailed { .. })
        ));
        assert_eq!(pixels.pixels(), vec![[2.0, -0.25, 0.5, 1.0]]);
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

        let output = apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(1, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            ),
            &compiled,
            7,
        )
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
        let sampling = mondrian_core::ImageSampling::new(
            mondrian_core::SampleAspectRatio::new(3, 2).unwrap(),
            [
                mondrian_core::Rational::new(1, 2),
                mondrian_core::Rational::new(2, 3),
            ],
        )
        .unwrap();
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
                        invert: false,
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

        let output = apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(1, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                sampling,
                &input,
            ),
            &compiled,
            0,
        )
        .expect("execute float mask graph");

        assert_eq!(output.sampling(), sampling);
        assert!(compiled_effect_graph_supports_rgba_f32(&compiled));
        assert_eq!(&output[0][..3], &input[0][..3]);
        assert!((output[0][3] - 0.8 * 0.123_456).abs() <= 1.0e-6);
        assert!((output[0][3] * 255.0 - (output[0][3] * 255.0).round()).abs() > 1.0e-3);
    }

    fn grade_matte_graph(mask_op: crate::mask::MaskOp) -> Arc<CompiledEffectGraph> {
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
                        opacity: 0.75,
                        invert: false,
                    },
                },
                crate::EffectGraphNode {
                    id: EffectGraphNodeId(3),
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
                        opacity: 0.25,
                        invert: false,
                    },
                },
                crate::EffectGraphNode {
                    id: EffectGraphNodeId(4),
                    kind: EffectGraphNodeKind::MaskCombine {
                        left: EffectGraphNodeId(2),
                        right: EffectGraphNodeId(3),
                        mask_op,
                    },
                },
                crate::EffectGraphNode {
                    id: EffectGraphNodeId(5),
                    kind: EffectGraphNodeKind::MatteMix {
                        base: EffectGraphNodeId(0),
                        graded: EffectGraphNodeId(1),
                        matte: EffectGraphNodeId(4),
                    },
                },
            ],
            output: Some(EffectGraphNodeId(5)),
        };
        crate::compile_reference_render_graph(graph).expect("compile grade-matte graph")
    }

    #[test]
    fn mask_combine_and_matte_mix_cover_all_ops_on_rgba8_and_float() {
        for (mask_op, weight) in [
            (crate::mask::MaskOp::Add, 0.75),
            (crate::mask::MaskOp::Subtract, 0.5625),
            (crate::mask::MaskOp::Intersect, 0.25),
            (crate::mask::MaskOp::Difference, 0.5),
        ] {
            let compiled = grade_matte_graph(mask_op);
            let float_input = [[0.2, 0.3, 0.4, 0.42]];
            let float_output = apply_compiled_effect_graph_rgba_f32(
                crate::EffectFloatFrameView::new(
                    crate::EffectFrameExtent::new(1, 1),
                    crate::EffectValueFormat::new(
                        crate::EffectWorkingPrecision::Float32,
                        crate::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &float_input,
                ),
                &compiled,
                0,
            )
            .expect("execute float grade matte");
            for channel in 0..3 {
                let expected = float_input[0][channel] * (1.0 + weight);
                assert!((float_output[0][channel] - expected).abs() <= 1.0e-6);
            }
            assert_eq!(float_output[0][3], float_input[0][3]);

            let u8_input = [51, 77, 102, 107];
            let u8_output = apply_compiled_effect_graph(&u8_input, 1, 1, &compiled, 0)
                .expect("execute RGBA8 grade matte");
            for channel in 0..3 {
                let base = u8_input[channel] as f32;
                let graded = (base * 2.0).min(255.0);
                let expected = base * (1.0 - weight) + graded * weight;
                assert!((u8_output[channel] as f32 - expected).abs() <= 1.5);
            }
            assert_eq!(u8_output[3], u8_input[3]);
        }
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

        let first = apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(8, 8),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            ),
            &compiled,
            1,
        )
        .expect("execute first dissolve frame");
        let repeated = apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(8, 8),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            ),
            &compiled,
            1,
        )
        .expect("execute repeated dissolve frame");
        let second = apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(8, 8),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            ),
            &compiled,
            2,
        )
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

        let output = apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(3, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            ),
            &compiled,
            0,
        )
        .expect("float blur");

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

        let first = apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(3, 3),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            ),
            &compiled,
            17,
        )
        .expect("built-in float graph");
        let second = apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(3, 3),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            ),
            &compiled,
            17,
        )
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

        let first = apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(2, 2),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            ),
            &compiled,
            1,
        )
        .expect("grain frame one");
        let second = apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(2, 2),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &input,
            ),
            &compiled,
            2,
        )
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

        let err = apply_compiled_effect_graph_rgba_f32(
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(1, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &[],
            ),
            &compiled,
            0,
        )
        .expect_err("input extent mismatch");

        assert!(matches!(
            err,
            EffectFloatExecutionError::FrameContract(
                crate::EffectFloatFrameError::ExtentMismatch { expected: 1, actual: 0 }
            )
        ));
    }
}

#[cfg(test)]
mod effect_pass_mix_contracts {
    use super::*;

    fn graph(blur: bool) -> Arc<CompiledEffectGraph> {
        let mut builder = crate::EffectGraphBuilderState::new();
        if blur {
            builder.append_unary(EffectRenderOp::GaussianBlur { radius: 1.0 });
        }
        crate::compile_reference_render_graph(builder.finish()).expect("actual compiled graph")
    }
    fn view<'a>(pixel: &'a [[f32; 4]]) -> crate::EffectFloatFrameView<'a> {
        crate::EffectFloatFrameView::new(
            crate::EffectFrameExtent::new(1, 1),
            crate::EffectValueFormat::new(
                crate::EffectWorkingPrecision::Float32,
                crate::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightCoverage,
            ),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            pixel,
        )
    }
    fn pass(
        entry: u8,
        graph: &CompiledEffectGraph,
        amount: f32,
        policy: mondrian_core::EffectPassBlendPolicy,
    ) -> [f32; 4] {
        let pixels = [[-0.5, 3.0, 0.8, 0.5]];
        let mut session = EffectExecutionSession::default();
        let mut control = crate::EffectProcessorControl::uncancelled();
        let result = match entry {
            0 => session.apply_compiled_pass_rgba_f32(
                view(&pixels),
                graph,
                amount,
                policy,
                0,
                &mut control,
            ),
            1 => session.apply_compiled_pass_rgba_f32_owned(
                view(&pixels).to_owned().expect("owner"),
                graph,
                amount,
                policy,
                0,
                &mut control,
            ),
            2 => session.apply_compiled_pass_rgba_f32_with_domain_processor(
                view(&pixels),
                graph,
                amount,
                policy,
                0,
                EffectDomainProcessorCacheKey::from_complete_semantic_fingerprint([29; 32]),
                |_, _| panic!("scene linear requires no domain conversion"),
                &mut control,
            ),
            3 => session.apply_compiled_pass_rgba_f32_owned_with_domain_processor(
                view(&pixels).to_owned().expect("owner"),
                graph,
                amount,
                policy,
                0,
                EffectDomainProcessorCacheKey::from_complete_semantic_fingerprint([29; 32]),
                |_, _| panic!("scene linear requires no domain conversion"),
                &mut control,
            ),
            _ => panic!("exact four public entries"),
        }
        .expect("real adjustment execution");
        assert_eq!(
            result.alpha(),
            mondrian_core::ColorFrameAlpha::StraightCoverage
        );
        result.pixels()[0]
    }
    #[test]
    fn effect_pass_mix_constant_preserves_coverage_across_all_float_entries() {
        let blur = graph(true);
        let identity = graph(false);
        for entry in 0..4 {
            for amount in [0.0, 0.125, 0.4, 1.0] {
                let expected = [-0.5, 3.0, 0.8, 0.5];
                for compiled in [&identity, &blur] {
                    let actual = pass(
                        entry,
                        compiled,
                        amount,
                        mondrian_core::EffectPassBlendPolicy::Mix,
                    );
                    for (actual, expected) in actual.into_iter().zip(expected) {
                        assert!(
                            (actual - expected).abs() < 1e-6,
                            "entry={entry} amount={amount} actual={actual} expected={expected}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn effect_pass_mix_explicit_normal_keeps_over_for_identity_graph() {
        let identity = graph(false);
        for entry in 0..4 {
            for amount in [0.125_f32, 0.4, 1.0] {
                let source_alpha = 0.5_f64 * f64::from(amount);
                let expected_alpha = source_alpha + 0.5 * (1.0 - source_alpha);
                let actual = pass(
                    entry,
                    &identity,
                    amount,
                    mondrian_core::EffectPassBlendPolicy::Blend(BlendMode::Normal),
                );
                assert!(
                    (f64::from(actual[3]) - expected_alpha).abs() < 1e-6,
                    "entry={entry} explicit over law"
                );
                for (actual, expected) in actual[..3].iter().zip([-0.5_f32, 3.0, 0.8]) {
                    assert!((*actual - expected).abs() < 1e-6);
                }
            }
        }
    }
    #[test]
    fn effect_pass_mix_encoded_constant_preserves_coverage_without_override() {
        let pixel = [40_u8, 100, 200, 128];
        for amount in [0.125, 0.4, 1.0] {
            let mut output = Vec::new();
            crate::adjustment::blend_adjustment_result(
                &pixel,
                &pixel,
                1,
                1,
                amount,
                mondrian_core::EffectPassBlendPolicy::Mix,
                &mut output,
            );
            assert_eq!(
                output, pixel,
                "encoded implicit mix must not composite a second copy"
            );
        }
    }
}

#[cfg(test)]
mod image_scale_cache_tests {
    use super::*;

    #[test]
    fn cached_float_output_cannot_bypass_actual_unit_scale_admission() {
        let graph = crate::image_scale_admission_tests::cacheable_unit_graph();
        let sampling = mondrian_core::ImageSampling::new(
            mondrian_core::SampleAspectRatio::new(4, 3).unwrap(),
            [mondrian_core::Rational::new(1, 2); 2],
        )
        .unwrap();
        let input = crate::EffectFloatFrame::from_pixels(
            crate::EffectFrameExtent::new(1, 1),
            crate::EffectValueFormat::new(
                EffectWorkingPrecision::Float32,
                crate::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightFloat,
            ),
            sampling,
            vec![[0.25, 0.5, 0.75, 1.0]],
        )
        .unwrap();
        let key = effect_output_cache_key_f32(input.view(), &graph, 0, None)
            .expect("the injected entry is eligible for normal output lookup");
        let mut session = EffectExecutionSession::default();
        session.put_float_output(key.clone(), input.clone());
        assert!(session.get_float_output(&key).is_some());
        assert_eq!(
            session
                .apply_compiled_rgba_f32(
                    input.view(),
                    &graph,
                    0,
                    &mut crate::EffectProcessorControl::uncancelled(),
                )
                .unwrap_err(),
            EffectFloatExecutionError::CustomProcessor(
                EffectExecutionError::ImageScaleNotAdmitted {
                    requirement: crate::EffectImageScaleRequirement::Unit,
                    actual_sampling: sampling,
                },
            )
        );
        assert!(
            session.get_float_output(&key).is_some(),
            "refusal does not mutate retained cache"
        );
        assert_eq!(session.diagnostics().processor_instances, 0);
    }
}
