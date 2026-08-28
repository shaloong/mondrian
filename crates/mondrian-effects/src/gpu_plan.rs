//! Backend-neutral lowering for GPU working-space effect execution.
//!
//! The plan deliberately contains no graphics API objects. It is the contract
//! between effect graph compilation and renderer backends that execute fused
//! point operations over typed floating-point color-domain pixels.

use crate::{
    graph::EffectGraphIdentity, CompiledEffectGraph, CompiledEffectStageBinding, EffectColorDomain,
    EffectExecutionAdmissionError, EffectExecutionContract, EffectExecutionEnvelope,
    EffectExecutionModes, EffectGraphNodeId, EffectGraphNodeKind, EffectProcessingBackend,
    EffectRenderOp, EffectWorkingPrecision,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

/// Maximum number of point operations fused into one GPU pass.
pub const MAX_FUSED_GPU_EFFECT_OPS: usize = 16;
const GPU_PLAN_CACHE_ENTRY_OVERHEAD_BYTES: usize = 256;

/// One pointwise operation executable by a working-space GPU backend.
#[derive(Debug, Clone, PartialEq)]
pub enum EffectGpuPointOp {
    /// Exposure, contrast, and saturation adjustment.
    ColorAdjust {
        exposure: f32,
        contrast: f32,
        saturation: f32,
        /// CIE Y coefficients for the authored Sequence working space.
        luminance_coefficients: [f32; 3],
    },
    /// Working-space Bradford white-balance matrix.
    WhiteBalance { grade: crate::WhiteBalanceGrade },
    /// Lift/Gamma/Gain/Offset primary correction.
    Primaries { grade: crate::PrimariesGrade },
    /// ASC CDL v1.2 no-clamp correction.
    AscCdl { grade: crate::AscCdlGrade },
    /// ACES 1.3 Reference Gamut Compression.
    GamutCompression { grade: crate::GamutCompressionGrade },
    /// Scene-linear highlight chroma reconstruction.
    HighlightRecovery {
        grade: crate::HighlightRecoveryGrade,
    },
    /// Immutable sampled RGB/YRGB and secondary curves.
    ColorCurves {
        curves: Arc<crate::PreparedColorCurves>,
    },
    /// Source-relative radial vignette.
    Vignette { intensity: f32, feather: f32 },
    /// Deterministic monochromatic grain.
    Grain { amount: f32 },
    /// Hard source-frame crop using normalized edge insets.
    Crop {
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    },
    /// Prepared creative 3D LUT sampled in its explicitly authored processing domain.
    ///
    /// The backend-neutral plan retains the immutable semantic payload only;
    /// device upload, residency, and binding remain renderer-owned.
    Lut3D {
        /// Immutable parsed cube with a precomputed complete semantic fingerprint.
        lut: Arc<crate::PreparedLut3D>,
        /// Blend from the unbounded source RGB to the domain-clamped LUT sample.
        intensity: f32,
    },
}

/// Immutable renderer-facing GPU plan for a compiled effect graph.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledEffectGpuPlan {
    operations: Vec<EffectGpuPointOp>,
    node_ids: Arc<[EffectGraphNodeId]>,
    graph_signature: u64,
    graph_fingerprint: [u8; 32],
    source_value: EffectGraphNodeId,
    output_value: EffectGraphNodeId,
    processing_domain: EffectColorDomain,
}

impl CompiledEffectGpuPlan {
    /// Ordered point operations to execute for each source pixel.
    pub fn operations(&self) -> &[EffectGpuPointOp] {
        &self.operations
    }

    /// Exact graph nodes represented by this plan, in execution order.
    pub fn node_ids(&self) -> &[EffectGraphNodeId] {
        &self.node_ids
    }

    /// Signature of the compiled source graph represented by this plan.
    pub fn graph_signature(&self) -> u64 {
        self.graph_signature
    }

    /// Complete pixel-semantic graph fingerprint represented by this plan.
    pub const fn graph_fingerprint(&self) -> [u8; 32] {
        self.graph_fingerprint
    }

    /// Existing graph value consumed by the first GPU operation.
    pub const fn source_value(&self) -> EffectGraphNodeId {
        self.source_value
    }

    /// Graph value produced by the final GPU operation.
    pub const fn output_value(&self) -> EffectGraphNodeId {
        self.output_value
    }

    /// Exact color domain in which the point operations must execute.
    pub fn processing_domain(&self) -> EffectColorDomain {
        self.processing_domain
    }

    /// Whether this plan performs no pixel changes.
    pub fn is_identity(&self) -> bool {
        self.operations.is_empty()
    }
}

/// Stable reason a compiled graph cannot use the fused GPU point path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EffectGpuPlanBlocker {
    /// The definition-bound program cannot enter this single-frame GPU path.
    #[error(transparent)]
    ExecutionContract(#[from] EffectExecutionAdmissionError),
    /// One selected graph node does not admit the concrete GPU Float32 mode.
    #[error("effect node {node_id:?} does not admit GPU Float32 execution: {admitted:?}")]
    NodeExecutionModeNotAdmitted {
        /// Node rejected by its implementation/Definition intersection.
        node_id: EffectGraphNodeId,
        /// Exact modes retained on the compiled graph.
        admitted: EffectExecutionModes,
    },
    /// The compiled transition graph is not one supported preserving RGB domain.
    #[error("effect graph has unsupported GPU color-domain plan with {transitions} transitions")]
    UnsupportedColorDomainPlan {
        /// Number of explicit RGB-domain transitions in the compiled graph.
        transitions: usize,
    },
    /// A GPU point operation changes its processing domain.
    #[error("effect node {node_id:?} changes GPU processing domain from {input:?} to {output:?}")]
    NonPreservingColorDomain {
        /// Node containing the non-preserving operation.
        node_id: EffectGraphNodeId,
        /// Required input domain.
        input: EffectColorDomain,
        /// Produced output domain.
        output: EffectColorDomain,
    },
    /// One fused point pass cannot execute operations declared in different domains.
    #[error("effect graph mixes GPU processing domains {first:?} and {next:?}")]
    MixedProcessingDomains {
        /// First processing domain in the chain.
        first: EffectColorDomain,
        /// Conflicting later processing domain.
        next: EffectColorDomain,
    },
    /// Data or alpha domains crossed a color edge and cannot be converted.
    #[error("effect graph contains {blockers} invalid color-domain edges")]
    ColorDomainBlocked {
        /// Number of fail-closed domain blockers.
        blockers: usize,
    },
    /// A graph node is not part of a single-source unary chain.
    #[error("effect node {node_id:?} has unsupported GPU topology {kind}")]
    UnsupportedTopology {
        /// Node that introduced the unsupported topology.
        node_id: EffectGraphNodeId,
        /// Stable topology label for diagnostics.
        kind: &'static str,
    },
    /// A unary render operation has no fused working-space implementation.
    #[error("effect node {node_id:?} uses unsupported GPU operation {op}")]
    UnsupportedOperation {
        /// Node containing the operation.
        node_id: EffectGraphNodeId,
        /// Stable operation label for diagnostics.
        op: &'static str,
    },
    /// The graph chain is malformed or does not follow its compiled schedule.
    #[error("effect node {node_id:?} does not consume the preceding GPU chain output")]
    DisconnectedChain {
        /// First disconnected node.
        node_id: EffectGraphNodeId,
    },
    /// The fused pass capacity would be exceeded.
    #[error("effect graph requires {actual} fused operations; maximum is {maximum}")]
    TooManyOperations {
        /// Requested operation count.
        actual: usize,
        /// Backend contract limit.
        maximum: usize,
    },
    /// The graph has no declared output.
    #[error("effect graph has no output node")]
    MissingOutput,
}

/// Lower a compiled graph into a backend-neutral fused GPU point plan.
pub fn lower_effect_graph_to_gpu_plan(
    compiled: &CompiledEffectGraph,
) -> Result<CompiledEffectGpuPlan, EffectGpuPlanBlocker> {
    validate_gpu_execution_contract(compiled)?;
    let nodes = compiled
        .schedule()
        .ordered_nodes
        .iter()
        .copied()
        .filter(|node_id| {
            !matches!(
                compiled.graph().node(*node_id).map(|node| &node.kind),
                Some(EffectGraphNodeKind::Source)
            )
        })
        .collect::<Vec<_>>();
    lower_effect_graph_nodes_to_gpu_plan_inner(compiled, &nodes, true)
}

/// Lower an exact tail of graph values into one fused GPU point plan.
///
/// Unlike whole-graph lowering, this Interface admits each selected node from
/// the exact implementation/Definition mode intersection retained by
/// [`CompiledEffectGraph`]. It is therefore suitable for a CPU-prefix/GPU-tail
/// execution route without pretending that the complete Effect Execution
/// Envelope is homogeneous.
///
/// The selected nodes must form the complete unary tail ending at the compiled
/// graph output. Color-domain transitions are rejected: a future
/// heterogeneous color Adapter must represent those transitions as ordinary
/// graph-value dispatches rather than hiding them inside this point plan.
pub fn lower_effect_graph_nodes_to_gpu_plan(
    compiled: &CompiledEffectGraph,
    node_ids: &[EffectGraphNodeId],
) -> Result<CompiledEffectGpuPlan, EffectGpuPlanBlocker> {
    lower_effect_graph_nodes_to_gpu_plan_inner(compiled, node_ids, false)
}

/// Lower one exact compiled unary graph node into a backend-neutral GPU point
/// plan.
///
/// This Interface is used by graph-value GPU DAG execution, where a node can
/// sit on a branch and therefore is not necessarily part of the unique tail
/// ending at the graph output. The returned plan preserves the semantic input
/// and output value identities from the unique [`CompiledEffectGraph`]; it
/// does not create another graph or infer connectivity.
pub fn lower_effect_graph_node_to_gpu_point_plan(
    compiled: &CompiledEffectGraph,
    node_id: EffectGraphNodeId,
) -> Result<CompiledEffectGpuPlan, EffectGpuPlanBlocker> {
    if !compiled.domain_plan().blockers.is_empty() {
        return Err(EffectGpuPlanBlocker::ColorDomainBlocked {
            blockers: compiled.domain_plan().blockers.len(),
        });
    }
    if !compiled.domain_plan().transitions.is_empty() {
        return Err(EffectGpuPlanBlocker::UnsupportedColorDomainPlan {
            transitions: compiled.domain_plan().transitions.len(),
        });
    }
    let node = compiled
        .graph()
        .node(node_id)
        .ok_or(EffectGpuPlanBlocker::DisconnectedChain { node_id })?;
    let admitted = compiled.node_execution_modes(node_id).unwrap_or(EffectExecutionModes::NONE);
    if !admitted.contains(
        EffectProcessingBackend::Gpu,
        EffectWorkingPrecision::Float32,
    ) {
        return Err(EffectGpuPlanBlocker::NodeExecutionModeNotAdmitted { node_id, admitted });
    }
    let (source_value, op, processing_domain) = match &node.kind {
        EffectGraphNodeKind::UnaryEffect { input, op } => (
            *input,
            lower_point_op(node_id, op)?,
            EffectColorDomain::SceneLinearRgb,
        ),
        EffectGraphNodeKind::DomainEffect { input, op, domain_contract } => {
            if domain_contract.input != domain_contract.output {
                return Err(EffectGpuPlanBlocker::NonPreservingColorDomain {
                    node_id,
                    input: domain_contract.input,
                    output: domain_contract.output,
                });
            }
            (*input, lower_point_op(node_id, op)?, domain_contract.input)
        }
        kind => {
            return Err(EffectGpuPlanBlocker::UnsupportedTopology {
                node_id,
                kind: node_kind_name(kind),
            });
        }
    };
    Ok(CompiledEffectGpuPlan {
        operations: vec![op],
        node_ids: Arc::from([node_id]),
        graph_signature: compiled.signature_hash(),
        graph_fingerprint: compiled.semantic_fingerprint(),
        source_value,
        output_value: node_id,
        processing_domain,
    })
}

fn lower_effect_graph_nodes_to_gpu_plan_inner(
    compiled: &CompiledEffectGraph,
    node_ids: &[EffectGraphNodeId],
    allow_whole_graph_domain_boundaries: bool,
) -> Result<CompiledEffectGpuPlan, EffectGpuPlanBlocker> {
    let output = compiled.graph().output.ok_or(EffectGpuPlanBlocker::MissingOutput)?;
    if !compiled.domain_plan().blockers.is_empty() {
        return Err(EffectGpuPlanBlocker::ColorDomainBlocked {
            blockers: compiled.domain_plan().blockers.len(),
        });
    }
    if !allow_whole_graph_domain_boundaries && !compiled.domain_plan().transitions.is_empty() {
        return Err(EffectGpuPlanBlocker::UnsupportedColorDomainPlan {
            transitions: compiled.domain_plan().transitions.len(),
        });
    }

    let scheduled_values = compiled
        .schedule()
        .ordered_nodes
        .iter()
        .copied()
        .filter(|node_id| {
            !matches!(
                compiled.graph().node(*node_id).map(|node| &node.kind),
                Some(EffectGraphNodeKind::Source)
            )
        })
        .collect::<Vec<_>>();
    if node_ids.len() > scheduled_values.len()
        || &scheduled_values[scheduled_values.len().saturating_sub(node_ids.len())..] != node_ids
    {
        return Err(EffectGpuPlanBlocker::DisconnectedChain {
            node_id: node_ids.first().copied().unwrap_or(output),
        });
    }

    let mut source_value = output;
    let mut previous = None;
    let mut operations = Vec::new();
    let mut processing_domain = None;
    for (index, node_id) in node_ids.iter().enumerate() {
        let Some(node) = compiled.graph().node(*node_id) else {
            return Err(EffectGpuPlanBlocker::DisconnectedChain { node_id: *node_id });
        };
        let admitted =
            compiled.node_execution_modes(*node_id).unwrap_or(EffectExecutionModes::NONE);
        if !admitted.contains(
            EffectProcessingBackend::Gpu,
            EffectWorkingPrecision::Float32,
        ) {
            return Err(EffectGpuPlanBlocker::NodeExecutionModeNotAdmitted {
                node_id: *node_id,
                admitted,
            });
        }
        match &node.kind {
            EffectGraphNodeKind::UnaryEffect { input, op } => {
                if index == 0 {
                    source_value = *input;
                    previous = Some(*input);
                }
                if Some(*input) != previous {
                    return Err(EffectGpuPlanBlocker::DisconnectedChain { node_id: *node_id });
                }
                merge_processing_domain(&mut processing_domain, EffectColorDomain::SceneLinearRgb)?;
                operations.push(lower_point_op(*node_id, op)?);
                if operations.len() > MAX_FUSED_GPU_EFFECT_OPS {
                    return Err(EffectGpuPlanBlocker::TooManyOperations {
                        actual: operations.len(),
                        maximum: MAX_FUSED_GPU_EFFECT_OPS,
                    });
                }
            }
            EffectGraphNodeKind::DomainEffect { input, op, domain_contract } => {
                if index == 0 {
                    source_value = *input;
                    previous = Some(*input);
                }
                if Some(*input) != previous {
                    return Err(EffectGpuPlanBlocker::DisconnectedChain { node_id: *node_id });
                }
                if domain_contract.input != domain_contract.output {
                    return Err(EffectGpuPlanBlocker::NonPreservingColorDomain {
                        node_id: *node_id,
                        input: domain_contract.input,
                        output: domain_contract.output,
                    });
                }
                merge_processing_domain(&mut processing_domain, domain_contract.input)?;
                operations.push(lower_point_op(*node_id, op)?);
                if operations.len() > MAX_FUSED_GPU_EFFECT_OPS {
                    return Err(EffectGpuPlanBlocker::TooManyOperations {
                        actual: operations.len(),
                        maximum: MAX_FUSED_GPU_EFFECT_OPS,
                    });
                }
            }
            kind => {
                return Err(EffectGpuPlanBlocker::UnsupportedTopology {
                    node_id: *node_id,
                    kind: node_kind_name(kind),
                });
            }
        }
        previous = Some(*node_id);
    }
    if node_ids.is_empty() {
        let identity_source = compiled
            .schedule()
            .ordered_nodes
            .iter()
            .copied()
            .find(|node_id| {
                matches!(
                    compiled.graph().node(*node_id).map(|node| &node.kind),
                    Some(EffectGraphNodeKind::Source)
                )
            })
            .ok_or(EffectGpuPlanBlocker::DisconnectedChain { node_id: output })?;
        if identity_source != output {
            return Err(EffectGpuPlanBlocker::DisconnectedChain { node_id: output });
        }
        source_value = identity_source;
        previous = Some(identity_source);
    }
    if previous != Some(output) {
        return Err(EffectGpuPlanBlocker::DisconnectedChain { node_id: output });
    }
    let processing_domain = processing_domain.unwrap_or(EffectColorDomain::SceneLinearRgb);
    let transitions_match = if !allow_whole_graph_domain_boundaries
        || processing_domain == EffectColorDomain::SceneLinearRgb
    {
        compiled.domain_plan().transitions.is_empty()
    } else {
        compiled.domain_plan().transitions.len() == 2
            && compiled.domain_plan().transitions.iter().any(|transition| {
                transition.from == EffectColorDomain::SceneLinearRgb
                    && transition.to == processing_domain
            })
            && compiled.domain_plan().transitions.iter().any(|transition| {
                transition.from == processing_domain
                    && transition.to == EffectColorDomain::SceneLinearRgb
            })
    };
    if !transitions_match {
        return Err(EffectGpuPlanBlocker::UnsupportedColorDomainPlan {
            transitions: compiled.domain_plan().transitions.len(),
        });
    }
    Ok(CompiledEffectGpuPlan {
        operations,
        node_ids: Arc::from(node_ids),
        graph_signature: compiled.signature_hash(),
        graph_fingerprint: compiled.semantic_fingerprint(),
        source_value,
        output_value: output,
        processing_domain,
    })
}

fn merge_processing_domain(
    current: &mut Option<EffectColorDomain>,
    next: EffectColorDomain,
) -> Result<(), EffectGpuPlanBlocker> {
    match *current {
        Some(first) if first != next => {
            Err(EffectGpuPlanBlocker::MixedProcessingDomains { first, next })
        }
        Some(_) => Ok(()),
        None => {
            *current = Some(next);
            Ok(())
        }
    }
}

/// Return a Session-owned cached GPU plan for a compiled effect graph.
///
/// Both successful plans and deterministic lowering blockers are cached by the
/// compiled graph identity, preventing repeated plan allocation and repeated
/// blocker traversal during one Preview or Export lifetime. The compact graph
/// signature remains diagnostic metadata and is never equality authority.
///
/// The caller must supply its exclusive execution Session. There is no
/// process-global GPU-plan cache.
pub fn get_or_lower_effect_graph_to_gpu_plan(
    session: &mut crate::execution_session::EffectExecutionSession,
    compiled: &CompiledEffectGraph,
) -> Result<Arc<CompiledEffectGpuPlan>, EffectGpuPlanBlocker> {
    session.get_or_lower_gpu_plan(compiled)
}

fn validate_gpu_execution_contract(
    compiled: &CompiledEffectGraph,
) -> Result<(), EffectGpuPlanBlocker> {
    compiled.execution_envelope().admit_single_frame_backend(
        EffectProcessingBackend::Gpu,
        EffectWorkingPrecision::Float32,
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
struct CachedEffectGpuPlan {
    result: Result<Arc<CompiledEffectGpuPlan>, EffectGpuPlanBlocker>,
    retained_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EffectGpuPlanCacheKey {
    graph_identity: EffectGraphIdentity,
    execution_envelope: EffectExecutionEnvelope,
    stage_bindings: Arc<[CompiledEffectStageBinding]>,
}

impl EffectGpuPlanCacheKey {
    fn from_compiled(compiled: &CompiledEffectGraph) -> Self {
        Self {
            graph_identity: compiled.identity().clone(),
            execution_envelope: compiled.execution_envelope().clone(),
            stage_bindings: Arc::from(compiled.stage_bindings()),
        }
    }

    fn retained_bytes_estimate(&self) -> usize {
        self.graph_identity
            .retained_bytes_estimate()
            .saturating_add(std::mem::size_of::<EffectExecutionEnvelope>())
            .saturating_add(
                self.execution_envelope
                    .stages()
                    .len()
                    .saturating_mul(std::mem::size_of::<EffectExecutionContract>()),
            )
            .saturating_add(
                self.stage_bindings
                    .len()
                    .saturating_mul(std::mem::size_of::<CompiledEffectStageBinding>()),
            )
            .saturating_add(
                self.stage_bindings
                    .iter()
                    .map(|binding| {
                        binding
                            .emitted_nodes()
                            .len()
                            .saturating_mul(std::mem::size_of::<EffectGraphNodeId>())
                    })
                    .fold(0_usize, usize::saturating_add),
            )
            .saturating_add(std::mem::size_of::<usize>().saturating_mul(3))
    }
}

/// Bounded GPU-plan residency owned by one [`crate::EffectExecutionSession`].
#[derive(Debug)]
pub(crate) struct EffectGpuPlanCache {
    max_entries: usize,
    max_bytes: usize,
    total_bytes: usize,
    entries: HashMap<EffectGpuPlanCacheKey, CachedEffectGpuPlan>,
    lru: VecDeque<EffectGpuPlanCacheKey>,
}

impl EffectGpuPlanCache {
    pub(crate) fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            max_entries,
            max_bytes,
            total_bytes: 0,
            entries: HashMap::new(),
            lru: VecDeque::new(),
        }
    }

    pub(crate) fn get_or_lower(
        &mut self,
        compiled: &CompiledEffectGraph,
    ) -> Result<Arc<CompiledEffectGpuPlan>, EffectGpuPlanBlocker> {
        let key = EffectGpuPlanCacheKey::from_compiled(compiled);
        if let Some(result) = self.entries.get(&key).map(|entry| entry.result.clone()) {
            self.touch(&key);
            return result;
        }

        let lowered = lower_effect_graph_to_gpu_plan(compiled).map(Arc::new);
        self.insert(key, lowered.clone());
        lowered
    }

    fn insert(
        &mut self,
        key: EffectGpuPlanCacheKey,
        result: Result<Arc<CompiledEffectGpuPlan>, EffectGpuPlanBlocker>,
    ) {
        let retained_bytes = gpu_plan_cache_entry_bytes(&key, &result);
        if self.max_entries == 0 || retained_bytes > self.max_bytes {
            return;
        }
        if let Some(previous) = self.entries.remove(&key) {
            self.total_bytes = self.total_bytes.saturating_sub(previous.retained_bytes);
            self.remove_from_lru(&key);
        }
        self.total_bytes = self.total_bytes.saturating_add(retained_bytes);
        self.entries.insert(key.clone(), CachedEffectGpuPlan { result, retained_bytes });
        self.touch(&key);
        self.trim();
    }

    pub(crate) fn reconfigure(&mut self, max_entries: usize, max_bytes: usize) {
        self.max_entries = max_entries;
        self.max_bytes = max_bytes;
        self.trim();
    }

    pub(crate) fn entries(&self) -> usize {
        self.entries.len()
    }

    pub(crate) const fn bytes(&self) -> usize {
        self.total_bytes
    }

    fn trim(&mut self) {
        while self.entries.len() > self.max_entries || self.total_bytes > self.max_bytes {
            if let Some(evicted) = self.lru.pop_front() {
                if let Some(entry) = self.entries.remove(&evicted) {
                    self.total_bytes = self.total_bytes.saturating_sub(entry.retained_bytes);
                }
            } else {
                break;
            }
        }
    }

    fn touch(&mut self, key: &EffectGpuPlanCacheKey) {
        self.remove_from_lru(key);
        self.lru.push_back(key.clone());
    }

    fn remove_from_lru(&mut self, key: &EffectGpuPlanCacheKey) {
        if let Some(index) = self.lru.iter().position(|entry| entry == key) {
            self.lru.remove(index);
        }
    }
}

fn gpu_plan_cache_entry_bytes(
    key: &EffectGpuPlanCacheKey,
    result: &Result<Arc<CompiledEffectGpuPlan>, EffectGpuPlanBlocker>,
) -> usize {
    let result_bytes = match result {
        Ok(plan) => std::mem::size_of::<CompiledEffectGpuPlan>()
            .saturating_add(
                plan.operations
                    .capacity()
                    .saturating_mul(std::mem::size_of::<EffectGpuPointOp>()),
            )
            .saturating_add(
                plan.node_ids.len().saturating_mul(std::mem::size_of::<EffectGraphNodeId>()),
            )
            .saturating_add(plan.operations.iter().fold(0_usize, |total, operation| {
                let retained = match operation {
                    EffectGpuPointOp::Lut3D { lut, .. } => lut.retained_bytes_estimate(),
                    _ => 0,
                };
                total.saturating_add(retained)
            }))
            .saturating_add(std::mem::size_of::<usize>().saturating_mul(2)),
        Err(_) => std::mem::size_of::<EffectGpuPlanBlocker>(),
    };
    GPU_PLAN_CACHE_ENTRY_OVERHEAD_BYTES
        .saturating_add(key.retained_bytes_estimate())
        .saturating_add(result_bytes)
}

fn lower_point_op(
    node_id: EffectGraphNodeId,
    op: &EffectRenderOp,
) -> Result<EffectGpuPointOp, EffectGpuPlanBlocker> {
    match op {
        EffectRenderOp::ColorAdjust {
            exposure,
            contrast,
            saturation,
            working_color_space,
        } => Ok(EffectGpuPointOp::ColorAdjust {
            exposure: *exposure,
            contrast: *contrast,
            saturation: *saturation,
            luminance_coefficients: working_color_space.luminance_coefficients(),
        }),
        EffectRenderOp::WhiteBalance { grade } => {
            Ok(EffectGpuPointOp::WhiteBalance { grade: *grade })
        }
        EffectRenderOp::Primaries { grade } => Ok(EffectGpuPointOp::Primaries { grade: *grade }),
        EffectRenderOp::AscCdl { grade } => Ok(EffectGpuPointOp::AscCdl { grade: *grade }),
        EffectRenderOp::GamutCompression { grade } => {
            Ok(EffectGpuPointOp::GamutCompression { grade: *grade })
        }
        EffectRenderOp::HighlightRecovery { grade } => {
            Ok(EffectGpuPointOp::HighlightRecovery { grade: *grade })
        }
        EffectRenderOp::ColorCurves { curves } => {
            Ok(EffectGpuPointOp::ColorCurves { curves: Arc::clone(curves) })
        }
        EffectRenderOp::Vignette { intensity, feather } => {
            Ok(EffectGpuPointOp::Vignette { intensity: *intensity, feather: *feather })
        }
        EffectRenderOp::Grain { amount } => Ok(EffectGpuPointOp::Grain { amount: *amount }),
        EffectRenderOp::Crop { left, top, right, bottom } => Ok(EffectGpuPointOp::Crop {
            left: *left,
            top: *top,
            right: *right,
            bottom: *bottom,
        }),
        EffectRenderOp::Lut3D { lut, intensity } => {
            Ok(EffectGpuPointOp::Lut3D { lut: Arc::clone(lut), intensity: *intensity })
        }
        unsupported => Err(EffectGpuPlanBlocker::UnsupportedOperation {
            node_id,
            op: operation_name(unsupported),
        }),
    }
}

fn node_kind_name(kind: &EffectGraphNodeKind) -> &'static str {
    match kind {
        EffectGraphNodeKind::Source => "source",
        EffectGraphNodeKind::UnaryEffect { .. } => "unary",
        EffectGraphNodeKind::DomainEffect { .. } => "domain_effect",
        EffectGraphNodeKind::Blend { .. } => "blend",
        EffectGraphNodeKind::Mask { .. } => "mask",
        EffectGraphNodeKind::MaskSource { .. } => "mask_source",
        EffectGraphNodeKind::MaskCombine { .. } => "mask_combine",
        EffectGraphNodeKind::MatteMix { .. } => "matte_mix",
        EffectGraphNodeKind::MultiInput { .. } => "multi_input",
    }
}

fn operation_name(op: &EffectRenderOp) -> &'static str {
    match op {
        EffectRenderOp::ColorAdjust { .. } => "color_adjust",
        EffectRenderOp::WhiteBalance { .. } => "white_balance",
        EffectRenderOp::Primaries { .. } => "primaries",
        EffectRenderOp::AscCdl { .. } => "asc_cdl",
        EffectRenderOp::GamutCompression { .. } => "gamut_compression",
        EffectRenderOp::HighlightRecovery { .. } => "highlight_recovery",
        EffectRenderOp::ColorCurves { .. } => "color_curves",
        EffectRenderOp::Qualifier { .. } => "qualifier",
        EffectRenderOp::MattePreview { .. } => "matte_preview",
        EffectRenderOp::GaussianBlur { .. } => "gaussian_blur",
        EffectRenderOp::Sharpen { .. } => "sharpen",
        EffectRenderOp::Vignette { .. } => "vignette",
        EffectRenderOp::ChromaticAberration { .. } => "chromatic_aberration",
        EffectRenderOp::Grain { .. } => "grain",
        EffectRenderOp::Crop { .. } => "crop",
        EffectRenderOp::TemporalFrameBlend { .. } => "temporal_frame_blend",
        EffectRenderOp::Lut3D { .. } => "lut_3d",
        EffectRenderOp::Custom { .. } => "custom",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{compile_reference_render_graph, EffectGraphBuilderState};

    #[test]
    fn lowers_ordered_point_chain() {
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::ColorAdjust {
            exposure: 1.0,
            contrast: 1.2,
            saturation: 0.8,
            working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
        });
        builder.append_unary(EffectRenderOp::Grain { amount: 0.25 });
        builder.append_unary(EffectRenderOp::Crop {
            left: 0.25,
            top: 0.0,
            right: 0.0,
            bottom: 0.25,
        });
        let compiled = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&compiled).expect("supported point chain");
        assert_eq!(plan.operations().len(), 3);
        assert_eq!(
            plan.operations()[2],
            EffectGpuPointOp::Crop { left: 0.25, top: 0.0, right: 0.0, bottom: 0.25 }
        );
        assert_eq!(plan.graph_signature(), compiled.signature_hash());
    }

    #[test]
    fn lowers_creative_lut_inside_one_fused_grade_chain() {
        let lut = Arc::new(crate::PreparedLut3D::new(
            crate::Lut3D::identity(2).expect("identity LUT"),
        ));
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::ColorAdjust {
            exposure: 0.25,
            contrast: 1.1,
            saturation: 0.9,
            working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
        });
        builder.append_unary(EffectRenderOp::Lut3D { lut: Arc::clone(&lut), intensity: 0.75 });
        builder.append_unary(EffectRenderOp::Crop { left: 0.0, top: 0.0, right: 0.1, bottom: 0.0 });
        let compiled = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&compiled).expect("fused GPU grade chain");

        assert_eq!(plan.operations().len(), 3);
        assert!(matches!(
            &plan.operations()[1],
            EffectGpuPointOp::Lut3D { lut: lowered, intensity }
                if Arc::ptr_eq(lowered, &lut) && *intensity == 0.75
        ));
        assert_eq!(plan.node_ids().len(), 3);
        assert_eq!(plan.processing_domain(), EffectColorDomain::SceneLinearRgb);
    }

    #[test]
    fn rejects_cpu_only_spatial_operation_at_contract_admission() {
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::GaussianBlur { radius: 3.0 });
        let compiled = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let admitted =
            crate::EffectExecutionModes::CPU_U8.union(crate::EffectExecutionModes::CPU_F32);
        assert!(matches!(
            lower_effect_graph_to_gpu_plan(&compiled),
            Err(EffectGpuPlanBlocker::ExecutionContract(
                EffectExecutionAdmissionError::ExecutionModeNotAdmitted {
                    stage_index: 0,
                    backend: EffectProcessingBackend::Gpu,
                    precision: EffectWorkingPrecision::Float32,
                    admitted: actual,
                }
            )) if actual == admitted
        ));
    }

    #[test]
    fn lowers_preserving_display_domain_with_explicit_processing_identity() {
        let display_domain = crate::EffectColorDomain::DisplayEncodedRgb {
            color_space: mondrian_core::ColorSpace::Rec709,
        };
        let compiled = crate::compile_reference_effect_graph_in_domain(
            &crate::EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                }],
            },
            crate::EffectColorDomainContract::preserving(display_domain),
        )
        .expect("valid display-domain graph");

        let plan = lower_effect_graph_to_gpu_plan(&compiled)
            .expect("renderer can schedule preserving RGB domain transitions");

        assert_eq!(plan.processing_domain(), display_domain);
        assert_eq!(plan.operations().len(), 1);
    }

    #[test]
    fn rejects_non_preserving_gpu_processing_domain() {
        let input = crate::EffectColorDomain::DisplayEncodedRgb {
            color_space: mondrian_core::ColorSpace::Rec709,
        };
        let output = crate::EffectColorDomain::DisplayLinearRgb {
            color_space: mondrian_core::ColorSpace::LinearRec709,
        };
        let compiled = crate::compile_reference_effect_graph_in_domain(
            &crate::EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                }],
            },
            crate::EffectColorDomainContract { input, output },
        )
        .expect("valid domain graph");

        assert!(matches!(
            lower_effect_graph_to_gpu_plan(&compiled),
            Err(EffectGpuPlanBlocker::NonPreservingColorDomain {
                input: actual_input,
                output: actual_output,
                ..
            }) if actual_input == input && actual_output == output
        ));
    }

    #[test]
    fn identity_graph_lowers_to_empty_plan() {
        let compiled = compile_reference_render_graph(crate::EffectRenderGraph::identity())
            .expect("valid identity graph");
        let plan = lower_effect_graph_to_gpu_plan(&compiled).expect("identity is GPU safe");
        assert!(plan.is_identity());
    }

    #[test]
    fn rejects_chain_beyond_fused_capacity() {
        let mut builder = EffectGraphBuilderState::new();
        for _ in 0..=MAX_FUSED_GPU_EFFECT_OPS {
            builder.append_unary(EffectRenderOp::Grain { amount: 0.1 });
        }
        let compiled = compile_reference_render_graph(builder.finish()).expect("valid graph");
        assert_eq!(
            lower_effect_graph_to_gpu_plan(&compiled),
            Err(EffectGpuPlanBlocker::TooManyOperations {
                actual: MAX_FUSED_GPU_EFFECT_OPS + 1,
                maximum: MAX_FUSED_GPU_EFFECT_OPS,
            })
        );
    }

    #[test]
    fn cached_lowering_reuses_shared_plan() {
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Grain { amount: 0.2 });
        let compiled = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let mut session = crate::EffectExecutionSession::default();

        let first = get_or_lower_effect_graph_to_gpu_plan(&mut session, &compiled)
            .expect("supported graph");
        let second =
            get_or_lower_effect_graph_to_gpu_plan(&mut session, &compiled).expect("cached graph");

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(session.diagnostics().gpu_plan_entries, 1);
        assert!(session.diagnostics().gpu_plan_bytes > 0);
    }

    #[test]
    fn gpu_plan_budget_and_generation_are_owner_local_barriers() {
        let graph = |amount| {
            let mut builder = EffectGraphBuilderState::new();
            builder.append_unary(EffectRenderOp::Grain { amount });
            compile_reference_render_graph(builder.finish()).expect("valid graph")
        };
        let first_graph = graph(0.1);
        let second_graph = graph(0.2);
        let mut session = crate::EffectExecutionSession::new(crate::EffectExecutionSessionConfig {
            max_gpu_plan_entries: 1,
            max_gpu_plan_bytes: 1024 * 1024,
            ..crate::EffectExecutionSessionConfig::uncached(1024)
        });

        let first = session.get_or_lower_gpu_plan(&first_graph).expect("first plan");
        session.get_or_lower_gpu_plan(&second_graph).expect("second plan");
        assert_eq!(session.diagnostics().gpu_plan_entries, 1);
        let first_after_eviction =
            session.get_or_lower_gpu_plan(&first_graph).expect("re-lowered first plan");
        assert!(!Arc::ptr_eq(&first, &first_after_eviction));

        session.bind_generation(7);
        assert_eq!(
            session.diagnostics().gpu_plan_entries,
            1,
            "generation rotation must retain frame-independent GPU-plan residency"
        );
        let generation_retained =
            session.get_or_lower_gpu_plan(&first_graph).expect("generation-retained plan");
        assert!(
            Arc::ptr_eq(&generation_retained, &first_after_eviction),
            "the retained plan is reused, not re-lowered"
        );
        session.reconfigure(crate::EffectExecutionSessionConfig {
            max_gpu_plan_entries: 0,
            max_gpu_plan_bytes: 0,
            ..crate::EffectExecutionSessionConfig::uncached(1024)
        });
        assert_eq!(session.diagnostics().gpu_plan_entries, 0);
        assert_eq!(session.diagnostics().gpu_plan_bytes, 0);
    }

    #[test]
    fn oversized_gpu_plan_is_returned_but_not_retained() {
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Grain { amount: 0.2 });
        let compiled = compile_reference_render_graph(builder.finish()).expect("valid graph");
        let mut session = crate::EffectExecutionSession::new(crate::EffectExecutionSessionConfig {
            max_gpu_plan_entries: 8,
            max_gpu_plan_bytes: 1,
            ..crate::EffectExecutionSessionConfig::uncached(1024)
        });

        let first = session.get_or_lower_gpu_plan(&compiled).expect("uncached oversized plan");
        let second = session
            .get_or_lower_gpu_plan(&compiled)
            .expect("second uncached oversized plan");
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(session.diagnostics().gpu_plan_entries, 0);
    }
}
