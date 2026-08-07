use crate::{
    EffectCachePolicy, EffectColorDomain, EffectColorDomainContract, EffectDeterminism,
    EffectExecutionContract, EffectExecutionContractViolation, EffectExecutionDemand,
    EffectExecutionDemandError, EffectExecutionEnvelope, EffectExecutionEnvironment,
    EffectExecutionModes, EffectFrameExtent, EffectGraphTopology, EffectLinearStagePlacement,
    EffectLinearStagePlacementError, EffectPixelRoi, EffectRenderOp, EffectRenderPlan,
    EffectResourceLifetime, EffectRoiPropagation, EffectStateModel, EffectTemporalInputExtent,
};
use mondrian_core::{types::BlendMode, TimelineTime};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, OnceLock},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EffectGraphNodeId(pub u32);

pub type EffectGraphValue = EffectGraphNodeId;

#[derive(Debug, Clone)]
pub enum EffectGraphNodeKind {
    Source,
    UnaryEffect {
        input: EffectGraphNodeId,
        op: EffectRenderOp,
    },
    /// Unary operation with an explicit non-default processing-domain contract.
    DomainEffect {
        input: EffectGraphNodeId,
        op: EffectRenderOp,
        domain_contract: EffectColorDomainContract,
    },
    Blend {
        base: EffectGraphNodeId,
        overlay: EffectGraphNodeId,
        blend_mode: BlendMode,
        opacity: f32,
    },
    Mask {
        input: EffectGraphNodeId,
        mask: EffectGraphNodeId,
        invert: bool,
        mask_op: crate::mask::MaskOp,
    },
    /// Synthetic source node that rasterizes a mask shape into an alpha buffer.
    MaskSource {
        shape: crate::mask::MaskShape,
        feather: f32,
        expansion: f32,
        opacity: f32,
    },
    /// Ordered N-input visual compositing node.
    ///
    /// Inputs are evaluated in stored order through the shared compiled graph
    /// and execution path.
    MultiInput {
        inputs: Vec<EffectGraphNodeId>,
        blend_mode: BlendMode,
        opacity: f32,
    },
}

#[derive(Debug, Clone)]
pub struct EffectGraphNode {
    pub id: EffectGraphNodeId,
    pub kind: EffectGraphNodeKind,
}

impl EffectGraphNode {
    pub fn input_ids(&self) -> Vec<EffectGraphNodeId> {
        match self.kind {
            EffectGraphNodeKind::Source => Vec::new(),
            EffectGraphNodeKind::UnaryEffect { input, .. }
            | EffectGraphNodeKind::DomainEffect { input, .. } => vec![input],
            EffectGraphNodeKind::Blend { base, overlay, .. } => vec![base, overlay],
            EffectGraphNodeKind::Mask { input, mask, .. } => vec![input, mask],
            EffectGraphNodeKind::MaskSource { .. } => Vec::new(),
            EffectGraphNodeKind::MultiInput { ref inputs, .. } => inputs.clone(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct EffectRenderGraph {
    pub nodes: Vec<EffectGraphNode>,
    pub output: Option<EffectGraphNodeId>,
}

/// Complete process-local semantic identity for one bound effect graph.
///
/// The canonical bytes are equality authority. A derived `u64` is exposed only
/// for compact diagnostics; it never proves graph equivalence.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct EffectGraphIdentity {
    canonical: Arc<[u8]>,
    semantic_fingerprint: [u8; 32],
}

impl EffectGraphIdentity {
    fn from_graph(graph: &EffectRenderGraph) -> Self {
        use std::hash::{Hash, Hasher};

        let mut writer = EffectGraphIdentityWriter::default();
        writer.write(b"mondrian.effect-graph-identity.v1");
        graph.output.hash(&mut writer);
        graph.nodes.len().hash(&mut writer);
        for node in &graph.nodes {
            node.id.hash(&mut writer);
            match &node.kind {
                EffectGraphNodeKind::Source => {
                    0u8.hash(&mut writer);
                }
                EffectGraphNodeKind::UnaryEffect { input, op } => {
                    1u8.hash(&mut writer);
                    input.hash(&mut writer);
                    hash_render_op(op, &mut writer);
                }
                EffectGraphNodeKind::DomainEffect { input, op, domain_contract } => {
                    8u8.hash(&mut writer);
                    input.hash(&mut writer);
                    domain_contract.hash(&mut writer);
                    hash_render_op(op, &mut writer);
                }
                EffectGraphNodeKind::Blend { base, overlay, blend_mode, opacity } => {
                    2u8.hash(&mut writer);
                    base.hash(&mut writer);
                    overlay.hash(&mut writer);
                    blend_mode.hash(&mut writer);
                    opacity.to_bits().hash(&mut writer);
                }
                EffectGraphNodeKind::Mask { input, mask, invert, mask_op } => {
                    3u8.hash(&mut writer);
                    input.hash(&mut writer);
                    mask.hash(&mut writer);
                    invert.hash(&mut writer);
                    mask_op.hash(&mut writer);
                }
                EffectGraphNodeKind::MaskSource { shape, feather, expansion, opacity } => {
                    6u8.hash(&mut writer);
                    shape_variant_hash(shape, &mut writer);
                    feather.to_bits().hash(&mut writer);
                    expansion.to_bits().hash(&mut writer);
                    opacity.to_bits().hash(&mut writer);
                }
                EffectGraphNodeKind::MultiInput { inputs, blend_mode, opacity } => {
                    7u8.hash(&mut writer);
                    inputs.hash(&mut writer);
                    blend_mode.hash(&mut writer);
                    opacity.to_bits().hash(&mut writer);
                }
            }
        }
        let canonical = writer.finish_bytes();
        let semantic_fingerprint = Sha256::digest(canonical.as_ref()).into();
        Self { canonical, semantic_fingerprint }
    }

    fn diagnostic_hash(&self) -> u64 {
        u64::from_le_bytes([
            self.semantic_fingerprint[0],
            self.semantic_fingerprint[1],
            self.semantic_fingerprint[2],
            self.semantic_fingerprint[3],
            self.semantic_fingerprint[4],
            self.semantic_fingerprint[5],
            self.semantic_fingerprint[6],
            self.semantic_fingerprint[7],
        ])
    }

    /// Conservative bytes retained when an owner keeps one identity as a
    /// cache key. The canonical allocation is counted in full even when an
    /// `Arc` is shared with the compiled graph, because the cache key can
    /// extend that allocation's lifetime independently.
    pub(crate) fn retained_bytes_estimate(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(self.canonical.len())
            .saturating_add(std::mem::size_of::<usize>().saturating_mul(2))
    }
}

/// Prefix-preserving adapter that turns the existing signature field writer
/// into a complete equality identity. Length-prefixing every `Hasher::write`
/// call prevents two different field boundaries from producing the same byte
/// stream.
#[derive(Default)]
struct EffectGraphIdentityWriter {
    bytes: Vec<u8>,
}

impl EffectGraphIdentityWriter {
    fn finish_bytes(self) -> Arc<[u8]> {
        self.bytes.into()
    }
}

impl std::hash::Hasher for EffectGraphIdentityWriter {
    fn finish(&self) -> u64 {
        let fingerprint: [u8; 32] = Sha256::digest(&self.bytes).into();
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
        self.bytes.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        self.bytes.extend_from_slice(bytes);
    }
}

impl EffectRenderGraph {
    pub fn identity() -> Self {
        let source = EffectGraphNodeId(0);
        Self {
            nodes: vec![EffectGraphNode { id: source, kind: EffectGraphNodeKind::Source }],
            output: Some(source),
        }
    }

    pub fn is_identity(&self) -> bool {
        self.nodes.len() == 1
            && matches!(
                self.nodes.first().map(|node| &node.kind),
                Some(EffectGraphNodeKind::Source)
            )
    }

    pub fn node(&self, id: EffectGraphNodeId) -> Option<&EffectGraphNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// Return a compact diagnostic hash.
    ///
    /// Matching values do not establish semantic equality.
    pub fn signature_hash(&self) -> u64 {
        self.semantic_identity().diagnostic_hash()
    }

    fn semantic_identity(&self) -> EffectGraphIdentity {
        EffectGraphIdentity::from_graph(self)
    }

    pub(crate) fn retained_bytes_estimate(&self) -> usize {
        let node_heap_bytes = self
            .nodes
            .iter()
            .map(|node| match &node.kind {
                EffectGraphNodeKind::UnaryEffect { op, .. }
                | EffectGraphNodeKind::DomainEffect { op, .. } => {
                    render_op_retained_bytes_estimate(op)
                }
                EffectGraphNodeKind::MaskSource {
                    shape: mondrian_core::mask_data::MaskShape::Path { points, .. },
                    ..
                } => points
                    .capacity()
                    .saturating_mul(std::mem::size_of::<mondrian_core::mask_data::BezierPoint>()),
                EffectGraphNodeKind::MultiInput { inputs, .. } => {
                    inputs.capacity().saturating_mul(std::mem::size_of::<EffectGraphNodeId>())
                }
                EffectGraphNodeKind::Source
                | EffectGraphNodeKind::Blend { .. }
                | EffectGraphNodeKind::Mask { .. }
                | EffectGraphNodeKind::MaskSource { .. } => 0,
            })
            .fold(0_usize, usize::saturating_add);
        std::mem::size_of::<Self>()
            .saturating_add(
                self.nodes.capacity().saturating_mul(std::mem::size_of::<EffectGraphNode>()),
            )
            .saturating_add(node_heap_bytes)
    }
}

fn render_op_retained_bytes_estimate(op: &EffectRenderOp) -> usize {
    match op {
        EffectRenderOp::Lut3D { lut, .. } => lut.retained_bytes_estimate(),
        EffectRenderOp::Custom { key, params, cache_key, processor, .. } => {
            let params_bytes = serde_json::to_vec(params).map_or(512, |bytes| bytes.len().max(512));
            std::mem::size_of::<EffectRenderOp>()
                .saturating_add(key.capacity())
                .saturating_add(params_bytes)
                .saturating_add(cache_key.as_ref().map_or(0, String::capacity))
                .saturating_add(processor.as_ref().map_or(0, |_| 512))
        }
        _ => std::mem::size_of::<EffectRenderOp>(),
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct EffectExecutionSchedule {
    pub(crate) ordered_nodes: Vec<EffectGraphNodeId>,
}

/// Exact Definition-stage ownership of values emitted into one compiled graph.
///
/// This is compiler evidence attached to the unique [`CompiledEffectGraph`];
/// it is not another graph representation. Empty `emitted_nodes` preserve an
/// enabled Definition stage that evaluated to an identity operation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CompiledEffectStageBinding {
    stage_index: usize,
    contract: EffectExecutionContract,
    input_value: EffectGraphValue,
    output_value: EffectGraphValue,
    emitted_nodes: Arc<[EffectGraphNodeId]>,
}

impl CompiledEffectStageBinding {
    pub(crate) fn new(
        stage_index: usize,
        contract: EffectExecutionContract,
        input_value: EffectGraphValue,
        output_value: EffectGraphValue,
        emitted_nodes: impl Into<Arc<[EffectGraphNodeId]>>,
    ) -> Self {
        Self {
            stage_index,
            contract,
            input_value,
            output_value,
            emitted_nodes: emitted_nodes.into(),
        }
    }

    /// Definition-stage index in [`EffectExecutionEnvelope::stages`].
    pub const fn stage_index(&self) -> usize {
        self.stage_index
    }

    /// Exact Definition-owned execution contract.
    pub const fn contract(&self) -> EffectExecutionContract {
        self.contract
    }

    /// Graph value entering the Definition evaluator.
    pub const fn input_value(&self) -> EffectGraphValue {
        self.input_value
    }

    /// Graph value selected when the Definition evaluator completed.
    pub const fn output_value(&self) -> EffectGraphValue {
        self.output_value
    }

    /// Every node emitted by this stage, including reachable internal branch
    /// values. Empty means the evaluated Definition was an identity.
    pub fn emitted_nodes(&self) -> &[EffectGraphNodeId] {
        &self.emitted_nodes
    }
}

#[derive(Debug, Clone)]
pub struct CompiledEffectGraph {
    graph: EffectRenderGraph,
    identity: EffectGraphIdentity,
    schedule: EffectExecutionSchedule,
    node_use_counts: HashMap<EffectGraphNodeId, usize>,
    node_profiles: HashMap<EffectGraphNodeId, CompiledEffectNodeProfile>,
    output_cache_policy: EffectCachePolicy,
    estimated_cost: u32,
    output_cache_enabled: bool,
    signature_hash: u64,
    /// Explicit domain transitions and blockers required by this graph.
    domain_plan: CompiledEffectDomainPlan,
    /// Definition-bound or conservatively implementation-derived execution
    /// requirements. Every executable graph carries this single planning
    /// authority.
    execution_envelope: EffectExecutionEnvelope,
    /// Exact Definition-stage ownership of emitted graph values.
    stage_bindings: Arc<[CompiledEffectStageBinding]>,
    /// Exact modes implemented and Definition-admitted for each reachable
    /// executable node.
    node_execution_modes: HashMap<EffectGraphNodeId, EffectExecutionModes>,
}

impl CompiledEffectGraph {
    /// Immutable render graph owned by this compiled IR.
    pub const fn graph(&self) -> &EffectRenderGraph {
        &self.graph
    }

    pub(crate) const fn identity(&self) -> &EffectGraphIdentity {
        &self.identity
    }

    /// Return the complete strong semantic fingerprint used by cross-Module
    /// execution and presentation identities.
    ///
    /// Effects-owned cache equality still compares the complete canonical
    /// identity; this fingerprint is for typed keys that must cross crate or
    /// registration seams.
    pub const fn semantic_fingerprint(&self) -> [u8; 32] {
        self.identity.semantic_fingerprint
    }

    /// Immutable topological execution schedule used by effects-owned
    /// executors and backend planners.
    pub(crate) const fn schedule(&self) -> &EffectExecutionSchedule {
        &self.schedule
    }

    /// Remaining-use template used for execution-buffer liveness.
    pub const fn node_use_counts(&self) -> &HashMap<EffectGraphNodeId, usize> {
        &self.node_use_counts
    }

    /// Immutable per-node subtree and cache profiles.
    pub const fn node_profiles(&self) -> &HashMap<EffectGraphNodeId, CompiledEffectNodeProfile> {
        &self.node_profiles
    }

    /// Conservative cross-call reuse contract for the complete output.
    pub const fn output_cache_policy(&self) -> EffectCachePolicy {
        self.output_cache_policy
    }

    /// Compiler-estimated relative output cost.
    pub const fn estimated_cost(&self) -> u32 {
        self.estimated_cost
    }

    /// Whether the complete output is eligible for the effects-owned cache.
    pub const fn output_cache_enabled(&self) -> bool {
        self.output_cache_enabled
    }

    /// Compact diagnostic hash for this immutable graph.
    ///
    /// Matching values do not establish semantic equality.
    pub const fn signature_hash(&self) -> u64 {
        self.signature_hash
    }

    /// Explicit processing-domain transitions and blockers.
    pub const fn domain_plan(&self) -> &CompiledEffectDomainPlan {
        &self.domain_plan
    }

    /// Complete definition-bound execution evidence for this immutable graph.
    pub const fn execution_envelope(&self) -> &EffectExecutionEnvelope {
        &self.execution_envelope
    }

    /// Exact Definition-stage ownership retained by this compiled graph.
    pub fn stage_bindings(&self) -> &[CompiledEffectStageBinding] {
        &self.stage_bindings
    }

    /// Exact modes that both the node implementation and its owning
    /// Definition stage admit.
    pub fn node_execution_modes(&self, node_id: EffectGraphNodeId) -> Option<EffectExecutionModes> {
        self.node_execution_modes.get(&node_id).copied()
    }

    pub(crate) fn retained_bytes_estimate(&self) -> usize {
        let binding_nodes = self
            .stage_bindings
            .iter()
            .map(|binding| {
                binding
                    .emitted_nodes
                    .len()
                    .saturating_mul(std::mem::size_of::<EffectGraphNodeId>())
            })
            .fold(0_usize, usize::saturating_add);
        std::mem::size_of::<Self>()
            .saturating_add(self.graph.retained_bytes_estimate())
            .saturating_add(self.identity.retained_bytes_estimate())
            .saturating_add(
                self.schedule
                    .ordered_nodes
                    .capacity()
                    .saturating_mul(std::mem::size_of::<EffectGraphNodeId>()),
            )
            .saturating_add(
                self.node_use_counts
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(EffectGraphNodeId, usize)>()),
            )
            .saturating_add(
                self.node_profiles.capacity().saturating_mul(std::mem::size_of::<(
                    EffectGraphNodeId,
                    CompiledEffectNodeProfile,
                )>()),
            )
            .saturating_add(
                self.domain_plan
                    .node_output_domains
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(EffectGraphNodeId, EffectColorDomain)>()),
            )
            .saturating_add(
                self.domain_plan
                    .transitions
                    .capacity()
                    .saturating_mul(std::mem::size_of::<EffectDomainTransition>()),
            )
            .saturating_add(
                self.domain_plan
                    .blockers
                    .capacity()
                    .saturating_mul(std::mem::size_of::<EffectDomainBlocker>()),
            )
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
            .saturating_add(binding_nodes)
            .saturating_add(
                self.node_execution_modes
                    .capacity()
                    .saturating_mul(
                        std::mem::size_of::<(EffectGraphNodeId, EffectExecutionModes)>(),
                    ),
            )
    }

    /// Derive checked temporal, spatial, state, resource, and precision demand
    /// for one exact output request.
    pub fn plan_execution_demand(
        &self,
        output_time: TimelineTime,
        frame_extent: EffectFrameExtent,
        output_roi: EffectPixelRoi,
    ) -> Result<EffectExecutionDemand, EffectExecutionDemandError> {
        self.execution_envelope
            .plan_execution_demand(output_time, frame_extent, output_roi)
    }

    /// Propose a deterministic lane placement for a definition-level linear
    /// chain. General DAGs require a graph-value-aware planner and fail closed.
    pub fn plan_linear_stage_placement(
        &self,
        environment: &EffectExecutionEnvironment,
    ) -> Result<EffectLinearStagePlacement, EffectLinearStagePlacementError> {
        self.execution_envelope.plan_linear_stage_placement(environment)
    }
}

/// Static graph shape prepared independently from frame-varying operation
/// parameters.
///
/// A matching frame graph can reuse its dependency schedule, use counts, and
/// color-domain plan. Dynamic values still receive fresh cache signatures and
/// node profiles, but cannot force the static graph compiler to repeat work.
#[derive(Debug, Clone)]
pub struct PreparedEffectGraphTopology {
    shape: EffectGraphShape,
    structural_signature: u64,
    schedule: EffectExecutionSchedule,
    node_use_counts: HashMap<EffectGraphNodeId, usize>,
    domain_plan: CompiledEffectDomainPlan,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EffectGraphShape {
    output: EffectGraphNodeId,
    nodes: Vec<EffectGraphNodeShape>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum EffectGraphNodeShape {
    Source {
        id: EffectGraphNodeId,
    },
    Unary {
        id: EffectGraphNodeId,
        input: EffectGraphNodeId,
    },
    Domain {
        id: EffectGraphNodeId,
        input: EffectGraphNodeId,
        domain_contract: EffectColorDomainContract,
    },
    Blend {
        id: EffectGraphNodeId,
        base: EffectGraphNodeId,
        overlay: EffectGraphNodeId,
    },
    Mask {
        id: EffectGraphNodeId,
        input: EffectGraphNodeId,
        mask: EffectGraphNodeId,
    },
    MaskSource {
        id: EffectGraphNodeId,
    },
    MultiInput {
        id: EffectGraphNodeId,
        inputs: Vec<EffectGraphNodeId>,
    },
}

impl PreparedEffectGraphTopology {
    /// Structural signature excluding every frame-varying operation value.
    pub const fn structural_signature(&self) -> u64 {
        self.structural_signature
    }

    pub(crate) fn retained_bytes_estimate(&self) -> usize {
        let shape_inputs = self
            .shape
            .nodes
            .iter()
            .map(|node| match node {
                EffectGraphNodeShape::MultiInput { inputs, .. } => {
                    inputs.capacity().saturating_mul(std::mem::size_of::<EffectGraphNodeId>())
                }
                _ => 0,
            })
            .fold(0_usize, usize::saturating_add);
        std::mem::size_of::<Self>()
            .saturating_add(
                self.shape
                    .nodes
                    .capacity()
                    .saturating_mul(std::mem::size_of::<EffectGraphNodeShape>()),
            )
            .saturating_add(shape_inputs)
            .saturating_add(
                self.schedule
                    .ordered_nodes
                    .capacity()
                    .saturating_mul(std::mem::size_of::<EffectGraphNodeId>()),
            )
            .saturating_add(
                self.node_use_counts
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(EffectGraphNodeId, usize)>()),
            )
            .saturating_add(
                self.domain_plan
                    .node_output_domains
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(EffectGraphNodeId, EffectColorDomain)>()),
            )
            .saturating_add(
                self.domain_plan
                    .transitions
                    .capacity()
                    .saturating_mul(std::mem::size_of::<EffectDomainTransition>()),
            )
            .saturating_add(
                self.domain_plan
                    .blockers
                    .capacity()
                    .saturating_mul(std::mem::size_of::<EffectDomainBlocker>()),
            )
    }

    /// Whether a frame graph has exactly this node, edge, output, and processing
    /// domain shape.
    pub fn matches(&self, graph: &EffectRenderGraph) -> bool {
        effect_graph_shape(graph).is_some_and(|shape| shape == self.shape)
    }

    /// Bind frame-varying operations to this prepared topology.
    pub fn bind(&self, graph: EffectRenderGraph) -> Option<Arc<CompiledEffectGraph>> {
        self.bind_inner(graph, None)
    }

    /// Bind frame-varying operations, Definition contracts, and exact emitted
    /// node/value ownership into one production IR.
    pub(crate) fn bind_with_execution_bindings(
        &self,
        graph: EffectRenderGraph,
        execution_envelope: EffectExecutionEnvelope,
        stage_bindings: impl Into<Arc<[CompiledEffectStageBinding]>>,
    ) -> Option<Arc<CompiledEffectGraph>> {
        self.bind_inner(graph, Some((execution_envelope, stage_bindings.into())))
    }

    fn bind_inner(
        &self,
        graph: EffectRenderGraph,
        execution_evidence: Option<(EffectExecutionEnvelope, Arc<[CompiledEffectStageBinding]>)>,
    ) -> Option<Arc<CompiledEffectGraph>> {
        if !self.matches(&graph) {
            return None;
        }
        if !reachable_custom_processor_snapshots_are_complete(&graph, &self.schedule) {
            return None;
        }
        let (execution_envelope, stage_bindings) = execution_evidence
            .or_else(|| implementation_execution_evidence(&graph, &self.schedule))?;
        let node_execution_modes = compile_node_execution_modes(
            &graph,
            &self.schedule,
            &execution_envelope,
            &stage_bindings,
        )?;
        let mut node_profiles = compile_effect_node_profiles(&graph, &self.schedule)?;
        constrain_cache_profiles_by_execution_envelope(&mut node_profiles, &execution_envelope);
        let (output_cache_policy, estimated_cost, output_cache_enabled) =
            compiled_effect_graph_cache_profile(&graph, &node_profiles)?;
        let identity = graph.semantic_identity();
        Some(Arc::new(CompiledEffectGraph {
            signature_hash: identity.diagnostic_hash(),
            identity,
            graph,
            schedule: self.schedule.clone(),
            node_use_counts: self.node_use_counts.clone(),
            node_profiles,
            output_cache_policy,
            estimated_cost,
            output_cache_enabled,
            domain_plan: self.domain_plan.clone(),
            execution_envelope,
            stage_bindings,
            node_execution_modes,
        }))
    }
}

/// One color-domain conversion edge required before a consumer or after output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EffectDomainTransition {
    /// Consumer node, or `None` when returning the graph output to scene-linear.
    pub consumer: Option<EffectGraphNodeId>,
    /// Producer node whose output is converted.
    pub input: EffectGraphNodeId,
    /// Producer domain.
    pub from: EffectColorDomain,
    /// Consumer/final-output domain.
    pub to: EffectColorDomain,
}

/// Why a graph edge cannot be converted by color management.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectDomainBlockerKind {
    /// Color transforms cannot reinterpret data or alpha/mask payloads.
    NonRgbDomainTransition,
    /// A mask input did not produce an alpha/mask payload.
    MaskInputIsNotAlpha,
}

/// One fail-closed effect-domain planning blocker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EffectDomainBlocker {
    /// Consumer node that rejected the edge, or `None` for final output.
    pub consumer: Option<EffectGraphNodeId>,
    /// Producer node on the blocked edge.
    pub input: EffectGraphNodeId,
    /// Actual producer domain.
    pub from: EffectColorDomain,
    /// Required consumer domain.
    pub to: EffectColorDomain,
    /// Stable blocker category.
    pub kind: EffectDomainBlockerKind,
}

/// Compiled color-domain contract for an effect graph.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompiledEffectDomainPlan {
    /// Domain produced by each reachable graph node before final normalization.
    pub node_output_domains: HashMap<EffectGraphNodeId, EffectColorDomain>,
    /// Legal RGB conversions that the renderer must resolve through OCIO.
    pub transitions: Vec<EffectDomainTransition>,
    /// Invalid data/alpha crossings that must fail closed.
    pub blockers: Vec<EffectDomainBlocker>,
}

impl CompiledEffectDomainPlan {
    /// Whether execution needs one or more OCIO domain conversions.
    pub fn requires_conversion(&self) -> bool {
        !self.transitions.is_empty()
    }

    /// Whether the graph can execute directly in the scene-linear float path.
    pub fn is_direct_scene_linear(&self) -> bool {
        self.transitions.is_empty() && self.blockers.is_empty()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CompiledEffectNodeProfile {
    /// Compact subtree diagnostic used for profiling and compiler comparison.
    ///
    /// This value is not cache equality authority.
    pub subtree_signature: u64,
    /// Conservative cross-call reuse contract for this node output.
    pub cache_policy: EffectCachePolicy,
    /// Compiler-estimated relative execution cost.
    pub estimated_cost: u32,
    /// Whether the node is worth considering for an admitted output cache.
    pub output_cache_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct EffectGraphBuilderState {
    graph: EffectRenderGraph,
    current_output: EffectGraphNodeId,
    next_id: u32,
    active_domain_contract: EffectColorDomainContract,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct EffectGraphBuilderCheckpoint {
    node_count: usize,
    current_output: EffectGraphNodeId,
}

#[derive(Debug, Clone, Copy)]
struct EffectImplementationRequirements {
    execution_modes: EffectExecutionModes,
    determinism: EffectDeterminism,
    temporal_input: EffectTemporalInputExtent,
    roi_from_effect_input: EffectRoiPropagation,
    resource_lifetime: EffectResourceLifetime,
}

impl EffectImplementationRequirements {
    const IDENTITY: Self = Self {
        execution_modes: EffectExecutionModes::ALL,
        determinism: EffectDeterminism::Deterministic,
        temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
        roi_from_effect_input: EffectRoiPropagation::PixelLocal,
        resource_lifetime: EffectResourceLifetime::Frame,
    };

    fn compose_node(self, node: Self) -> Self {
        Self {
            execution_modes: self.execution_modes.intersection(node.execution_modes),
            determinism: self.determinism.max(node.determinism),
            temporal_input: self
                .temporal_input
                .accumulate(node.temporal_input)
                .unwrap_or(EffectTemporalInputExtent::UNBOUNDED),
            roi_from_effect_input: compose_roi(
                self.roi_from_effect_input,
                node.roi_from_effect_input,
            ),
            resource_lifetime: self.resource_lifetime.max(node.resource_lifetime),
        }
    }

    fn merge_branches(self, other: Self) -> Self {
        Self {
            execution_modes: self.execution_modes.intersection(other.execution_modes),
            determinism: self.determinism.max(other.determinism),
            temporal_input: self.temporal_input.merge(other.temporal_input),
            roi_from_effect_input: merge_roi(
                self.roi_from_effect_input,
                other.roi_from_effect_input,
            ),
            resource_lifetime: self.resource_lifetime.max(other.resource_lifetime),
        }
    }
}

fn derive_implementation_requirements<'a>(
    nodes: impl IntoIterator<Item = &'a EffectGraphNode>,
    output: EffectGraphNodeId,
) -> EffectImplementationRequirements {
    let mut requirements = HashMap::<EffectGraphNodeId, EffectImplementationRequirements>::new();
    let requirement_for =
        |id: EffectGraphNodeId,
         requirements: &HashMap<EffectGraphNodeId, EffectImplementationRequirements>| {
            requirements
                .get(&id)
                .copied()
                .unwrap_or(EffectImplementationRequirements::IDENTITY)
        };

    for node in nodes {
        let derived = match &node.kind {
            EffectGraphNodeKind::Source => EffectImplementationRequirements::IDENTITY,
            EffectGraphNodeKind::UnaryEffect { input, op }
            | EffectGraphNodeKind::DomainEffect { input, op, .. } => {
                requirement_for(*input, &requirements).compose_node(render_op_requirements(op))
            }
            EffectGraphNodeKind::Blend { base, overlay, .. } => {
                requirement_for(*base, &requirements)
                    .merge_branches(requirement_for(*overlay, &requirements))
                    .compose_node(gpu_blend_node_requirements())
            }
            EffectGraphNodeKind::Mask { input, mask, .. } => requirement_for(*input, &requirements)
                .merge_branches(requirement_for(*mask, &requirements))
                .compose_node(gpu_mask_node_requirements()),
            EffectGraphNodeKind::MaskSource { .. } => compositing_node_requirements(),
            EffectGraphNodeKind::MultiInput { inputs, .. } => inputs
                .iter()
                .map(|input| requirement_for(*input, &requirements))
                .reduce(EffectImplementationRequirements::merge_branches)
                .unwrap_or(EffectImplementationRequirements::IDENTITY)
                .compose_node(compositing_node_requirements()),
        };
        requirements.insert(node.id, derived);
    }

    requirement_for(output, &requirements)
}

fn implementation_execution_evidence(
    graph: &EffectRenderGraph,
    schedule: &EffectExecutionSchedule,
) -> Option<(EffectExecutionEnvelope, Arc<[CompiledEffectStageBinding]>)> {
    if graph.is_identity() {
        return Some((EffectExecutionEnvelope::identity(), Arc::from([])));
    }
    let output = graph.output?;
    let ordered_nodes = schedule
        .ordered_nodes
        .iter()
        .map(|node_id| graph.node(*node_id))
        .collect::<Option<Vec<_>>>()?;
    let aggregate_requirements =
        derive_implementation_requirements(ordered_nodes.iter().copied(), output);
    let stage_requirements = ordered_nodes
        .iter()
        .copied()
        .filter_map(|node| {
            raw_node_implementation_requirements(node).map(|requirements| (node, requirements))
        })
        .collect::<Vec<_>>();
    let aggregate =
        implementation_contract(aggregate_requirements, EffectGraphTopology::GeneralDag);
    let stages = stage_requirements
        .iter()
        .map(|(_, requirements)| {
            implementation_contract(*requirements, EffectGraphTopology::GeneralDag)
        })
        .collect::<Vec<_>>();
    let envelope = EffectExecutionEnvelope::new(
        aggregate,
        Arc::<[EffectExecutionContract]>::from(stages.clone()),
    );
    let bindings = stage_requirements
        .into_iter()
        .enumerate()
        .map(|(stage_index, (node, _))| {
            let input_value = node.input_ids().first().copied().unwrap_or(node.id);
            CompiledEffectStageBinding::new(
                stage_index,
                stages[stage_index],
                input_value,
                node.id,
                Arc::from([node.id]),
            )
        })
        .collect::<Vec<_>>();
    Some((envelope, bindings.into()))
}

fn raw_node_implementation_requirements(
    node: &EffectGraphNode,
) -> Option<EffectImplementationRequirements> {
    match &node.kind {
        EffectGraphNodeKind::Source => None,
        EffectGraphNodeKind::UnaryEffect { op, .. }
        | EffectGraphNodeKind::DomainEffect { op, .. } => {
            Some(raw_render_op_execution_requirements(op))
        }
        EffectGraphNodeKind::Blend { .. } => Some(gpu_blend_node_requirements()),
        EffectGraphNodeKind::MultiInput { inputs, .. } if !inputs.is_empty() => {
            Some(gpu_blend_node_requirements())
        }
        EffectGraphNodeKind::Mask { .. } => Some(gpu_mask_node_requirements()),
        EffectGraphNodeKind::MaskSource { .. } | EffectGraphNodeKind::MultiInput { .. } => {
            Some(compositing_node_requirements())
        }
    }
}

fn compile_node_execution_modes(
    graph: &EffectRenderGraph,
    schedule: &EffectExecutionSchedule,
    envelope: &EffectExecutionEnvelope,
    bindings: &[CompiledEffectStageBinding],
) -> Option<HashMap<EffectGraphNodeId, EffectExecutionModes>> {
    if bindings.len() != envelope.stages().len() {
        return None;
    }
    let scheduled_nodes = schedule.ordered_nodes.iter().copied().collect::<HashSet<_>>();
    let mut owner_by_node = HashMap::<EffectGraphNodeId, usize>::new();
    for (expected_index, binding) in bindings.iter().enumerate() {
        if binding.stage_index != expected_index
            || binding.contract != envelope.stages()[expected_index]
            || graph.node(binding.input_value).is_none()
            || graph.node(binding.output_value).is_none()
            || (binding.emitted_nodes.is_empty() && binding.output_value != binding.input_value)
            || (!binding.emitted_nodes.is_empty()
                && binding.output_value != binding.input_value
                && !binding.emitted_nodes.contains(&binding.output_value))
        {
            return None;
        }
        for node_id in binding.emitted_nodes.iter().copied() {
            if matches!(
                graph.node(node_id).map(|node| &node.kind),
                None | Some(EffectGraphNodeKind::Source)
            ) || !scheduled_nodes.contains(&node_id)
                || owner_by_node.insert(node_id, expected_index).is_some()
            {
                return None;
            }
        }
    }

    let mut modes = HashMap::with_capacity(schedule.ordered_nodes.len());
    for node_id in schedule.ordered_nodes.iter().copied() {
        let node = graph.node(node_id)?;
        if matches!(&node.kind, EffectGraphNodeKind::Source) {
            modes.insert(node_id, EffectExecutionModes::ALL);
            continue;
        }
        let stage_index = owner_by_node.get(&node_id).copied()?;
        let implemented = raw_node_implementation_requirements(node)?.execution_modes;
        let admitted = implemented.intersection(bindings[stage_index].contract.execution_modes);
        if admitted.is_empty() {
            return None;
        }
        modes.insert(node_id, admitted);
    }
    Some(modes)
}

fn raw_render_op_execution_requirements(op: &EffectRenderOp) -> EffectImplementationRequirements {
    render_op_requirements(op)
}

fn implementation_contract(
    requirements: EffectImplementationRequirements,
    topology: EffectGraphTopology,
) -> EffectExecutionContract {
    EffectExecutionContract {
        execution_modes: requirements.execution_modes,
        determinism: requirements.determinism,
        state_model: EffectStateModel::Stateless,
        temporal_input: requirements.temporal_input,
        roi_propagation: requirements.roi_from_effect_input,
        resource_lifetime: requirements.resource_lifetime,
        topology,
    }
}

impl Default for EffectGraphBuilderState {
    fn default() -> Self {
        let graph = EffectRenderGraph::identity();
        let current_output = graph.output.expect("identity graph should have output");
        Self {
            graph,
            current_output,
            next_id: current_output.0 + 1,
            active_domain_contract: EffectColorDomainContract::SCENE_LINEAR,
        }
    }
}

impl EffectGraphBuilderState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn source(&self) -> EffectGraphValue {
        EffectGraphNodeId(0)
    }

    pub fn current_output(&self) -> EffectGraphValue {
        self.current_output
    }

    pub(crate) fn checkpoint(&self) -> EffectGraphBuilderCheckpoint {
        EffectGraphBuilderCheckpoint {
            node_count: self.graph.nodes.len(),
            current_output: self.current_output,
        }
    }

    pub(crate) fn node_ids_since(
        &self,
        checkpoint: EffectGraphBuilderCheckpoint,
    ) -> Arc<[EffectGraphNodeId]> {
        self.graph
            .nodes
            .iter()
            .skip(checkpoint.node_count)
            .map(|node| node.id)
            .collect::<Vec<_>>()
            .into()
    }

    pub(crate) fn bind_custom_runtime_owner_since(
        &mut self,
        checkpoint: EffectGraphBuilderCheckpoint,
        effect_key: &str,
        definition_registry_revision: u64,
        contract: Option<&crate::EffectPluginContract>,
    ) {
        for node in self.graph.nodes.iter_mut().skip(checkpoint.node_count) {
            let (EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::Custom { processor, .. },
                ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::Custom { processor, .. },
                ..
            }) = &mut node.kind
            else {
                continue;
            };
            if let Some(binding) = processor {
                *binding =
                    binding.with_runtime_owner(effect_key, definition_registry_revision, contract);
            }
        }
    }

    pub(crate) fn satisfies_topology_since(
        &self,
        checkpoint: EffectGraphBuilderCheckpoint,
        topology: EffectGraphTopology,
    ) -> bool {
        if topology == EffectGraphTopology::GeneralDag {
            return true;
        }
        let mut expected_input = checkpoint.current_output;
        for node in self.graph.nodes.iter().skip(checkpoint.node_count) {
            let input = match &node.kind {
                EffectGraphNodeKind::UnaryEffect { input, .. }
                | EffectGraphNodeKind::DomainEffect { input, .. } => *input,
                _ => return false,
            };
            if input != expected_input {
                return false;
            }
            expected_input = node.id;
        }
        self.current_output == expected_input
    }

    pub(crate) fn validate_execution_contract_since(
        &self,
        checkpoint: EffectGraphBuilderCheckpoint,
        declared: EffectExecutionContract,
    ) -> Result<(), EffectExecutionContractViolation> {
        let requirements = self.implementation_requirements_since(checkpoint);
        if !declared.execution_modes.is_subset_of(requirements.execution_modes) {
            return Err(
                EffectExecutionContractViolation::ExecutionModesTooOptimistic {
                    declared: declared.execution_modes,
                    implemented: requirements.execution_modes,
                },
            );
        }
        if declared.determinism < requirements.determinism {
            return Err(EffectExecutionContractViolation::DeterminismTooOptimistic {
                declared: declared.determinism,
                required: requirements.determinism,
            });
        }
        if !declared.temporal_input.covers(requirements.temporal_input) {
            return Err(
                EffectExecutionContractViolation::TemporalExtentTooOptimistic {
                    declared: declared.temporal_input,
                    required: requirements.temporal_input,
                },
            );
        }
        if !declared.roi_propagation.covers(requirements.roi_from_effect_input) {
            return Err(EffectExecutionContractViolation::RoiTooOptimistic {
                declared: declared.roi_propagation,
                required: requirements.roi_from_effect_input,
            });
        }
        if declared.resource_lifetime < requirements.resource_lifetime {
            return Err(EffectExecutionContractViolation::ResourceLifetimeTooShort {
                declared: declared.resource_lifetime,
                required: requirements.resource_lifetime,
            });
        }
        Ok(())
    }

    fn implementation_requirements_since(
        &self,
        checkpoint: EffectGraphBuilderCheckpoint,
    ) -> EffectImplementationRequirements {
        derive_implementation_requirements(
            self.graph.nodes.iter().skip(checkpoint.node_count),
            self.current_output,
        )
    }

    pub(crate) fn set_active_domain_contract(
        &mut self,
        domain_contract: EffectColorDomainContract,
    ) {
        self.active_domain_contract = domain_contract;
    }

    pub fn append_unary(&mut self, op: EffectRenderOp) -> EffectGraphNodeId {
        let node_id = self.add_unary_from(self.current_output, op);
        self.current_output = node_id;
        node_id
    }

    /// Append one operation with an instance-resolved processing-domain
    /// contract without mutating the definition's default contract.
    ///
    /// This is used by resource effects such as `.cube` LUTs whose files carry
    /// numeric domains but no color-space identity. The authored instance must
    /// resolve that identity explicitly before graph construction.
    pub fn append_unary_in_domain(
        &mut self,
        op: EffectRenderOp,
        domain_contract: EffectColorDomainContract,
    ) -> EffectGraphNodeId {
        let previous = self.active_domain_contract;
        self.active_domain_contract = domain_contract;
        let node_id = self.append_unary(op);
        self.active_domain_contract = previous;
        node_id
    }

    pub fn add_unary_from(
        &mut self,
        input: EffectGraphNodeId,
        op: EffectRenderOp,
    ) -> EffectGraphNodeId {
        let id = self.alloc_id();
        let kind = if self.active_domain_contract == EffectColorDomainContract::SCENE_LINEAR {
            EffectGraphNodeKind::UnaryEffect { input, op }
        } else {
            EffectGraphNodeKind::DomainEffect {
                input,
                op,
                domain_contract: self.active_domain_contract,
            }
        };
        self.graph.nodes.push(EffectGraphNode { id, kind });
        id
    }

    pub fn add_blend(
        &mut self,
        base: EffectGraphNodeId,
        overlay: EffectGraphNodeId,
        blend_mode: BlendMode,
        opacity: f32,
    ) -> EffectGraphNodeId {
        let id = self.alloc_id();
        self.graph.nodes.push(EffectGraphNode {
            id,
            kind: EffectGraphNodeKind::Blend { base, overlay, blend_mode, opacity },
        });
        id
    }

    /// Add one ordered N-input compositing node.
    ///
    /// Compilation rejects an empty input list. A single input remains an
    /// identity-shaped operation and is lowered to an exact materialization
    /// copy when its selected execution lane requires a distinct output.
    pub fn add_multi_input(
        &mut self,
        inputs: Vec<EffectGraphNodeId>,
        blend_mode: BlendMode,
        opacity: f32,
    ) -> EffectGraphNodeId {
        let id = self.alloc_id();
        self.graph.nodes.push(EffectGraphNode {
            id,
            kind: EffectGraphNodeKind::MultiInput { inputs, blend_mode, opacity },
        });
        id
    }

    pub fn add_mask(
        &mut self,
        input: EffectGraphNodeId,
        mask: EffectGraphNodeId,
        invert: bool,
        mask_op: crate::mask::MaskOp,
    ) -> EffectGraphNodeId {
        let id = self.alloc_id();
        self.graph.nodes.push(EffectGraphNode {
            id,
            kind: EffectGraphNodeKind::Mask { input, mask, invert, mask_op },
        });
        id
    }

    /// Add one synthetic Mask raster value with no graph-value inputs.
    ///
    /// The returned alpha-domain value must be consumed by an explicit Mask
    /// or other compatible compositing node. Preparation and execution remain
    /// subject to the owning Effect stage's declared modes and resource
    /// contract.
    pub fn add_mask_source(
        &mut self,
        shape: crate::mask::MaskShape,
        feather: f32,
        expansion: f32,
        opacity: f32,
    ) -> EffectGraphNodeId {
        let id = self.alloc_id();
        self.graph.nodes.push(EffectGraphNode {
            id,
            kind: EffectGraphNodeKind::MaskSource { shape, feather, expansion, opacity },
        });
        id
    }

    pub fn set_current_output(&mut self, node_id: EffectGraphNodeId) {
        self.current_output = node_id;
    }

    pub fn blend_current_with<F>(
        &mut self,
        blend_mode: BlendMode,
        opacity: f32,
        build_overlay: F,
    ) -> EffectGraphValue
    where
        F: FnOnce(&mut Self, EffectGraphValue) -> EffectGraphValue,
    {
        let base = self.current_output;
        let overlay = build_overlay(self, base);
        let output = self.add_blend(base, overlay, blend_mode, opacity);
        self.current_output = output;
        output
    }

    pub fn mask_current_with<F>(
        &mut self,
        invert: bool,
        mask_op: crate::mask::MaskOp,
        build_mask: F,
    ) -> EffectGraphValue
    where
        F: FnOnce(&mut Self, EffectGraphValue) -> EffectGraphValue,
    {
        let input = self.current_output;
        let mask = build_mask(self, input);
        let output = self.add_mask(input, mask, invert, mask_op);
        self.current_output = output;
        output
    }

    pub fn finish(mut self) -> EffectRenderGraph {
        self.graph.output = Some(self.current_output);
        self.graph
    }

    fn alloc_id(&mut self) -> EffectGraphNodeId {
        let id = EffectGraphNodeId(self.next_id);
        self.next_id += 1;
        id
    }
}

fn render_op_requirements(op: &EffectRenderOp) -> EffectImplementationRequirements {
    let cpu_float = EffectImplementationRequirements {
        execution_modes: EffectExecutionModes::CPU_U8.union(EffectExecutionModes::CPU_F32),
        determinism: EffectDeterminism::Deterministic,
        temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
        roi_from_effect_input: EffectRoiPropagation::PixelLocal,
        resource_lifetime: EffectResourceLifetime::Frame,
    };
    match op {
        EffectRenderOp::ColorAdjust { .. } | EffectRenderOp::Vignette { .. } => {
            EffectImplementationRequirements {
                execution_modes: cpu_float.execution_modes.union(EffectExecutionModes::GPU_F32),
                ..cpu_float
            }
        }
        EffectRenderOp::GaussianBlur { radius } => EffectImplementationRequirements {
            roi_from_effect_input: finite_radius_roi(*radius),
            ..cpu_float
        },
        EffectRenderOp::Sharpen { .. } => EffectImplementationRequirements {
            roi_from_effect_input: finite_radius_roi(crate::effect::SHARPEN_BLUR_RADIUS_PIXELS),
            ..cpu_float
        },
        EffectRenderOp::ChromaticAberration { .. } => EffectImplementationRequirements {
            roi_from_effect_input: EffectRoiPropagation::FullFrame,
            ..cpu_float
        },
        EffectRenderOp::Grain { .. } => EffectImplementationRequirements {
            execution_modes: cpu_float.execution_modes.union(EffectExecutionModes::GPU_F32),
            determinism: EffectDeterminism::FrameSeeded,
            ..cpu_float
        },
        EffectRenderOp::TemporalFrameBlend { sample_offset, .. } => {
            EffectImplementationRequirements {
                execution_modes: EffectExecutionModes::CPU_F32,
                temporal_input: temporal_input_extent_for_sample_offset(*sample_offset),
                ..cpu_float
            }
        }
        EffectRenderOp::Lut3D { .. } => EffectImplementationRequirements {
            resource_lifetime: EffectResourceLifetime::PreparedProgram,
            ..cpu_float
        },
        EffectRenderOp::Custom { cache_policy, .. } => EffectImplementationRequirements {
            execution_modes: EffectExecutionModes::CPU_U8,
            determinism: match cache_policy {
                EffectCachePolicy::Deterministic => EffectDeterminism::Deterministic,
                EffectCachePolicy::FrameDependent => EffectDeterminism::FrameSeeded,
                EffectCachePolicy::Uncacheable => EffectDeterminism::Nondeterministic,
            },
            temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
            roi_from_effect_input: EffectRoiPropagation::UnknownRequiresFullFrame,
            resource_lifetime: EffectResourceLifetime::Frame,
        },
    }
}

fn temporal_input_extent_for_sample_offset(
    sample_offset: TimelineTime,
) -> EffectTemporalInputExtent {
    if sample_offset.is_zero() {
        return EffectTemporalInputExtent::CURRENT_FRAME;
    }
    if sample_offset.is_negative() {
        return EffectTemporalInputExtent {
            past: TimelineTime::ZERO
                .checked_sub(sample_offset)
                .map(crate::EffectTemporalSpan::Finite)
                .unwrap_or(crate::EffectTemporalSpan::Unbounded),
            future: crate::EffectTemporalSpan::None,
        };
    }
    EffectTemporalInputExtent {
        past: crate::EffectTemporalSpan::None,
        future: crate::EffectTemporalSpan::Finite(sample_offset),
    }
}

fn compositing_node_requirements() -> EffectImplementationRequirements {
    EffectImplementationRequirements {
        execution_modes: EffectExecutionModes::CPU_U8.union(EffectExecutionModes::CPU_F32),
        determinism: EffectDeterminism::Deterministic,
        temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
        roi_from_effect_input: EffectRoiPropagation::PixelLocal,
        resource_lifetime: EffectResourceLifetime::Frame,
    }
}

fn gpu_blend_node_requirements() -> EffectImplementationRequirements {
    EffectImplementationRequirements {
        execution_modes: compositing_node_requirements()
            .execution_modes
            .union(EffectExecutionModes::GPU_F32),
        ..compositing_node_requirements()
    }
}

fn gpu_mask_node_requirements() -> EffectImplementationRequirements {
    EffectImplementationRequirements {
        execution_modes: compositing_node_requirements()
            .execution_modes
            .union(EffectExecutionModes::GPU_F32),
        ..compositing_node_requirements()
    }
}

fn finite_radius_roi(radius: f32) -> EffectRoiPropagation {
    crate::adjustment::gaussian_blur_input_halo(radius).map_or(
        EffectRoiPropagation::UnknownRequiresFullFrame,
        |pixels| EffectRoiPropagation::Expand {
            horizontal_pixels: pixels,
            vertical_pixels: pixels,
        },
    )
}

fn compose_roi(first: EffectRoiPropagation, second: EffectRoiPropagation) -> EffectRoiPropagation {
    match (first, second) {
        (EffectRoiPropagation::UnknownRequiresFullFrame, _)
        | (_, EffectRoiPropagation::UnknownRequiresFullFrame) => {
            EffectRoiPropagation::UnknownRequiresFullFrame
        }
        (EffectRoiPropagation::FullFrame, _) | (_, EffectRoiPropagation::FullFrame) => {
            EffectRoiPropagation::FullFrame
        }
        (EffectRoiPropagation::PixelLocal, roi) | (roi, EffectRoiPropagation::PixelLocal) => roi,
        (
            EffectRoiPropagation::Expand {
                horizontal_pixels: first_horizontal,
                vertical_pixels: first_vertical,
            },
            EffectRoiPropagation::Expand {
                horizontal_pixels: second_horizontal,
                vertical_pixels: second_vertical,
            },
        ) => EffectRoiPropagation::Expand {
            horizontal_pixels: first_horizontal.saturating_add(second_horizontal),
            vertical_pixels: first_vertical.saturating_add(second_vertical),
        },
    }
}

fn merge_roi(first: EffectRoiPropagation, second: EffectRoiPropagation) -> EffectRoiPropagation {
    match (first, second) {
        (EffectRoiPropagation::UnknownRequiresFullFrame, _)
        | (_, EffectRoiPropagation::UnknownRequiresFullFrame) => {
            EffectRoiPropagation::UnknownRequiresFullFrame
        }
        (EffectRoiPropagation::FullFrame, _) | (_, EffectRoiPropagation::FullFrame) => {
            EffectRoiPropagation::FullFrame
        }
        (EffectRoiPropagation::PixelLocal, roi) | (roi, EffectRoiPropagation::PixelLocal) => roi,
        (
            EffectRoiPropagation::Expand {
                horizontal_pixels: first_horizontal,
                vertical_pixels: first_vertical,
            },
            EffectRoiPropagation::Expand {
                horizontal_pixels: second_horizontal,
                vertical_pixels: second_vertical,
            },
        ) => EffectRoiPropagation::Expand {
            horizontal_pixels: first_horizontal.max(second_horizontal),
            vertical_pixels: first_vertical.max(second_vertical),
        },
    }
}

pub(crate) fn compile_effect_render_graph(plan: &EffectRenderPlan) -> EffectRenderGraph {
    compile_effect_render_graph_in_domain(plan, EffectColorDomainContract::SCENE_LINEAR)
}

/// Compile a linear effect plan with one explicit processing-domain contract.
pub(crate) fn compile_effect_render_graph_in_domain(
    plan: &EffectRenderPlan,
    domain_contract: EffectColorDomainContract,
) -> EffectRenderGraph {
    let mut graph = EffectRenderGraph::identity();
    let mut current = graph.output.expect("identity graph should have source output");

    for (index, op) in plan.ops.iter().cloned().enumerate() {
        let node_id = EffectGraphNodeId((index + 1) as u32);
        let kind = if domain_contract == EffectColorDomainContract::SCENE_LINEAR {
            EffectGraphNodeKind::UnaryEffect { input: current, op }
        } else {
            EffectGraphNodeKind::DomainEffect { input: current, op, domain_contract }
        };
        graph.nodes.push(EffectGraphNode { id: node_id, kind });
        current = node_id;
    }

    graph.output = Some(current);
    graph
}

/// Compile the reachable effect graph into explicit color-domain edges.
///
/// The graph source and final output are scene-linear by compositor contract.
/// RGB-to-RGB edges become renderer-owned OCIO transitions. Data and alpha
/// crossings are retained as blockers so callers fail closed instead of
/// interpreting them as picture RGB.
pub(crate) fn compile_effect_domain_plan(
    graph: &EffectRenderGraph,
    schedule: &EffectExecutionSchedule,
) -> Option<CompiledEffectDomainPlan> {
    let mut plan = CompiledEffectDomainPlan::default();

    for node_id in &schedule.ordered_nodes {
        let node = graph.node(*node_id)?;
        let output_domain = match &node.kind {
            EffectGraphNodeKind::Source => EffectColorDomain::SceneLinearRgb,
            EffectGraphNodeKind::UnaryEffect { input, .. } => {
                let input_domain = *plan.node_output_domains.get(input)?;
                plan_domain_edge(
                    &mut plan,
                    Some(node.id),
                    *input,
                    input_domain,
                    EffectColorDomain::SceneLinearRgb,
                    EffectDomainBlockerKind::NonRgbDomainTransition,
                );
                EffectColorDomain::SceneLinearRgb
            }
            EffectGraphNodeKind::DomainEffect { input, domain_contract, .. } => {
                let input_domain = *plan.node_output_domains.get(input)?;
                plan_domain_edge(
                    &mut plan,
                    Some(node.id),
                    *input,
                    input_domain,
                    domain_contract.input,
                    EffectDomainBlockerKind::NonRgbDomainTransition,
                );
                domain_contract.output
            }
            EffectGraphNodeKind::Blend { base, overlay, .. } => {
                let base_domain = *plan.node_output_domains.get(base)?;
                let overlay_domain = *plan.node_output_domains.get(overlay)?;
                plan_domain_edge(
                    &mut plan,
                    Some(node.id),
                    *overlay,
                    overlay_domain,
                    base_domain,
                    EffectDomainBlockerKind::NonRgbDomainTransition,
                );
                base_domain
            }
            EffectGraphNodeKind::Mask { input, mask, .. } => {
                let input_domain = *plan.node_output_domains.get(input)?;
                let mask_domain = *plan.node_output_domains.get(mask)?;
                if mask_domain != EffectColorDomain::AlphaMask {
                    plan.blockers.push(EffectDomainBlocker {
                        consumer: Some(node.id),
                        input: *mask,
                        from: mask_domain,
                        to: EffectColorDomain::AlphaMask,
                        kind: EffectDomainBlockerKind::MaskInputIsNotAlpha,
                    });
                }
                input_domain
            }
            EffectGraphNodeKind::MaskSource { .. } => EffectColorDomain::AlphaMask,
            EffectGraphNodeKind::MultiInput { inputs, .. } => {
                let first = *inputs.first()?;
                let target_domain = *plan.node_output_domains.get(&first)?;
                for input in inputs.iter().skip(1) {
                    let input_domain = *plan.node_output_domains.get(input)?;
                    plan_domain_edge(
                        &mut plan,
                        Some(node.id),
                        *input,
                        input_domain,
                        target_domain,
                        EffectDomainBlockerKind::NonRgbDomainTransition,
                    );
                }
                target_domain
            }
        };
        plan.node_output_domains.insert(node.id, output_domain);
    }

    let output = graph.output?;
    let output_domain = *plan.node_output_domains.get(&output)?;
    plan_domain_edge(
        &mut plan,
        None,
        output,
        output_domain,
        EffectColorDomain::SceneLinearRgb,
        EffectDomainBlockerKind::NonRgbDomainTransition,
    );
    Some(plan)
}

fn plan_domain_edge(
    plan: &mut CompiledEffectDomainPlan,
    consumer: Option<EffectGraphNodeId>,
    input: EffectGraphNodeId,
    from: EffectColorDomain,
    to: EffectColorDomain,
    blocker_kind: EffectDomainBlockerKind,
) {
    if from == to {
        return;
    }
    if from.is_rgb() && to.is_rgb() {
        plan.transitions.push(EffectDomainTransition { consumer, input, from, to });
    } else {
        plan.blockers
            .push(EffectDomainBlocker { consumer, input, from, to, kind: blocker_kind });
    }
}

fn compile_scheduled_effect_graph(plan: &EffectRenderPlan) -> Option<CompiledEffectGraph> {
    let graph = compile_effect_render_graph(plan);
    compile_scheduled_render_graph(graph)
}

#[doc(hidden)]
/// Compile one uncached reference plan with an explicit color-domain contract.
///
/// This constructor exists only for scalar/reference tests. Production
/// Preview and Export must obtain graphs from [`crate::PreparedEffectProgram`].
pub fn compile_reference_effect_graph_in_domain(
    plan: &EffectRenderPlan,
    domain_contract: EffectColorDomainContract,
) -> Option<CompiledEffectGraph> {
    let graph = compile_effect_render_graph_in_domain(plan, domain_contract);
    compile_scheduled_render_graph(graph)
}

fn compile_scheduled_render_graph(graph: EffectRenderGraph) -> Option<CompiledEffectGraph> {
    let schedule = schedule_effect_render_graph(&graph)?;
    if !reachable_custom_processor_snapshots_are_complete(&graph, &schedule) {
        return None;
    }
    let domain_plan = compile_effect_domain_plan(&graph, &schedule)?;
    let (execution_envelope, stage_bindings) =
        implementation_execution_evidence(&graph, &schedule)?;
    let node_execution_modes =
        compile_node_execution_modes(&graph, &schedule, &execution_envelope, &stage_bindings)?;
    let mut node_profiles = compile_effect_node_profiles(&graph, &schedule)?;
    constrain_cache_profiles_by_execution_envelope(&mut node_profiles, &execution_envelope);
    let (output_cache_policy, estimated_cost, output_cache_enabled) =
        compiled_effect_graph_cache_profile(&graph, &node_profiles)?;
    let identity = graph.semantic_identity();
    Some(CompiledEffectGraph {
        node_use_counts: effect_graph_node_use_counts(&graph),
        node_profiles,
        output_cache_policy,
        estimated_cost,
        output_cache_enabled,
        signature_hash: identity.diagnostic_hash(),
        identity,
        domain_plan,
        execution_envelope,
        stage_bindings,
        node_execution_modes,
        graph,
        schedule,
    })
}

/// Return the process-wide compiled identity effect graph.
///
/// Most timeline clips have no enabled effects or masks. Keeping the identity
/// graph as a static compiled graph avoids rebuilding the same source-only
/// graph for the common no-effect path. It is the only compiled effect graph
/// with process-wide residency; prepared programs and execution sessions own
/// every non-identity topology.
pub fn identity_compiled_effect_graph() -> Option<Arc<CompiledEffectGraph>> {
    static IDENTITY: OnceLock<Option<Arc<CompiledEffectGraph>>> = OnceLock::new();
    IDENTITY
        .get_or_init(|| compile_scheduled_effect_graph(&EffectRenderPlan::default()).map(Arc::new))
        .clone()
}

#[doc(hidden)]
/// Compile one linear reference plan without retaining it globally.
///
/// This constructor exists for cross-crate scalar/reference tests. Production
/// Preview and Export must obtain graphs from
/// [`crate::PreparedEffectProgram`].
pub fn compile_reference_effect_graph(plan: &EffectRenderPlan) -> Option<Arc<CompiledEffectGraph>> {
    compile_scheduled_effect_graph(plan).map(Arc::new)
}

#[doc(hidden)]
/// Compile one raw reference graph without retaining it globally.
///
/// This constructor exists for cross-crate graph/executor tests.
/// Production Preview and Export must obtain graphs from
/// [`crate::PreparedEffectProgram`].
pub fn compile_reference_render_graph(
    graph: EffectRenderGraph,
) -> Option<Arc<CompiledEffectGraph>> {
    compile_scheduled_render_graph(graph).map(Arc::new)
}

fn reachable_custom_processor_snapshots_are_complete(
    graph: &EffectRenderGraph,
    schedule: &EffectExecutionSchedule,
) -> bool {
    schedule.ordered_nodes.iter().all(|node_id| {
        graph.node(*node_id).is_some_and(|node| match &node.kind {
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::Custom { processor, .. },
                ..
            }
            | EffectGraphNodeKind::DomainEffect {
                op: EffectRenderOp::Custom { processor, .. },
                ..
            } => processor.is_some(),
            _ => true,
        })
    })
}

/// Prepare the static schedule, ownership counts, and color-domain plan for one
/// graph shape.
pub fn prepare_effect_graph_topology(
    graph: &EffectRenderGraph,
) -> Option<PreparedEffectGraphTopology> {
    use std::hash::{Hash, Hasher};

    let shape = effect_graph_shape(graph)?;
    let schedule = schedule_effect_render_graph(graph)?;
    let node_use_counts = effect_graph_node_use_counts(graph);
    let domain_plan = compile_effect_domain_plan(graph, &schedule)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    shape.hash(&mut hasher);
    Some(PreparedEffectGraphTopology {
        shape,
        structural_signature: hasher.finish(),
        schedule,
        node_use_counts,
        domain_plan,
    })
}

fn effect_graph_shape(graph: &EffectRenderGraph) -> Option<EffectGraphShape> {
    let output = graph.output?;
    let nodes = graph
        .nodes
        .iter()
        .map(|node| match &node.kind {
            EffectGraphNodeKind::Source => EffectGraphNodeShape::Source { id: node.id },
            EffectGraphNodeKind::UnaryEffect { input, .. } => {
                EffectGraphNodeShape::Unary { id: node.id, input: *input }
            }
            EffectGraphNodeKind::DomainEffect { input, domain_contract, .. } => {
                EffectGraphNodeShape::Domain {
                    id: node.id,
                    input: *input,
                    domain_contract: *domain_contract,
                }
            }
            EffectGraphNodeKind::Blend { base, overlay, .. } => {
                EffectGraphNodeShape::Blend { id: node.id, base: *base, overlay: *overlay }
            }
            EffectGraphNodeKind::Mask { input, mask, .. } => {
                EffectGraphNodeShape::Mask { id: node.id, input: *input, mask: *mask }
            }
            EffectGraphNodeKind::MaskSource { .. } => {
                EffectGraphNodeShape::MaskSource { id: node.id }
            }
            EffectGraphNodeKind::MultiInput { inputs, .. } => {
                EffectGraphNodeShape::MultiInput { id: node.id, inputs: inputs.clone() }
            }
        })
        .collect();
    Some(EffectGraphShape { output, nodes })
}

pub(crate) fn schedule_effect_render_graph(
    graph: &EffectRenderGraph,
) -> Option<EffectExecutionSchedule> {
    if !effect_render_graph_is_well_formed(graph) {
        return None;
    }
    let output = graph.output?;
    let mut reachable = HashSet::new();
    collect_reachable_nodes(graph, output, &mut reachable)?;

    let mut indegree = HashMap::<EffectGraphNodeId, usize>::new();
    let mut outgoing = HashMap::<EffectGraphNodeId, Vec<EffectGraphNodeId>>::new();

    for node in graph.nodes.iter().filter(|node| reachable.contains(&node.id)) {
        indegree.entry(node.id).or_insert(0);
        for input in node.input_ids() {
            if !reachable.contains(&input) {
                continue;
            }
            *indegree.entry(node.id).or_insert(0) += 1;
            outgoing.entry(input).or_default().push(node.id);
        }
    }

    let mut ready = indegree
        .iter()
        .filter_map(|(id, degree)| (*degree == 0).then_some(*id))
        .collect::<VecDeque<_>>();
    let mut ordered_nodes = Vec::with_capacity(reachable.len());

    while let Some(node_id) = ready.pop_front() {
        ordered_nodes.push(node_id);
        if let Some(children) = outgoing.get(&node_id) {
            for child in children {
                let degree = indegree.get_mut(child)?;
                *degree = degree.saturating_sub(1);
                if *degree == 0 {
                    ready.push_back(*child);
                }
            }
        }
    }

    (ordered_nodes.len() == reachable.len()).then_some(EffectExecutionSchedule { ordered_nodes })
}

pub(crate) fn effect_graph_node_use_counts(
    graph: &EffectRenderGraph,
) -> HashMap<EffectGraphNodeId, usize> {
    let Some(output) = graph.output else {
        return HashMap::new();
    };
    let mut reachable = HashSet::new();
    if collect_reachable_nodes(graph, output, &mut reachable).is_none() {
        return HashMap::new();
    }

    let mut counts = HashMap::<EffectGraphNodeId, usize>::new();
    for node in graph.nodes.iter().filter(|node| reachable.contains(&node.id)) {
        counts.entry(node.id).or_insert(0);
        for input in node.input_ids() {
            if reachable.contains(&input) {
                *counts.entry(input).or_insert(0) += 1;
            }
        }
    }
    counts
}

fn compiled_effect_graph_cache_profile(
    graph: &EffectRenderGraph,
    profiles: &HashMap<EffectGraphNodeId, CompiledEffectNodeProfile>,
) -> Option<(EffectCachePolicy, u32, bool)> {
    let profile = profiles.get(&graph.output?)?;
    Some((
        profile.cache_policy,
        profile.estimated_cost,
        profile.output_cache_enabled,
    ))
}

fn constrain_cache_profiles_by_execution_envelope(
    profiles: &mut HashMap<EffectGraphNodeId, CompiledEffectNodeProfile>,
    execution_envelope: &EffectExecutionEnvelope,
) {
    match execution_envelope.aggregate().determinism {
        EffectDeterminism::Deterministic => {}
        EffectDeterminism::FrameSeeded => {
            for profile in profiles.values_mut() {
                profile.cache_policy =
                    profile.cache_policy.combine(EffectCachePolicy::FrameDependent);
            }
        }
        EffectDeterminism::Nondeterministic => {
            for profile in profiles.values_mut() {
                profile.cache_policy = EffectCachePolicy::Uncacheable;
                profile.output_cache_enabled = false;
            }
        }
    }
}

pub(crate) fn compile_effect_node_profiles(
    graph: &EffectRenderGraph,
    schedule: &EffectExecutionSchedule,
) -> Option<HashMap<EffectGraphNodeId, CompiledEffectNodeProfile>> {
    use std::hash::{Hash, Hasher};

    let mut profiles = HashMap::<EffectGraphNodeId, CompiledEffectNodeProfile>::new();
    let use_counts = effect_graph_node_use_counts(graph);

    for node_id in &schedule.ordered_nodes {
        let node = graph.node(*node_id)?;
        let profile = match &node.kind {
            EffectGraphNodeKind::Source => {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                0u8.hash(&mut hasher);
                node.id.hash(&mut hasher);
                CompiledEffectNodeProfile {
                    subtree_signature: hasher.finish(),
                    cache_policy: EffectCachePolicy::Deterministic,
                    estimated_cost: 0,
                    output_cache_enabled: false,
                }
            }
            EffectGraphNodeKind::UnaryEffect { input, op } => {
                let input_profile = profiles.get(input)?;
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                1u8.hash(&mut hasher);
                input_profile.subtree_signature.hash(&mut hasher);
                op.hash_signature(&mut hasher);
                let cache_policy = input_profile.cache_policy.combine(op.cache_policy());
                let estimated_cost = input_profile.estimated_cost + op.estimated_cost();
                let output_cache_enabled = cache_policy.permits_cross_call_reuse()
                    && (estimated_cost >= 5
                        || (estimated_cost >= 4
                            && (use_counts.get(&node.id).copied().unwrap_or(0) > 1
                                || graph.output == Some(node.id))));
                CompiledEffectNodeProfile {
                    subtree_signature: hasher.finish(),
                    cache_policy,
                    estimated_cost,
                    output_cache_enabled,
                }
            }
            EffectGraphNodeKind::DomainEffect { input, op, domain_contract } => {
                let input_profile = profiles.get(input)?;
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                8u8.hash(&mut hasher);
                input_profile.subtree_signature.hash(&mut hasher);
                domain_contract.hash(&mut hasher);
                op.hash_signature(&mut hasher);
                let cache_policy = input_profile.cache_policy.combine(op.cache_policy());
                let estimated_cost = input_profile.estimated_cost + op.estimated_cost();
                let output_cache_enabled = cache_policy.permits_cross_call_reuse()
                    && (estimated_cost >= 5
                        || (estimated_cost >= 4
                            && (use_counts.get(&node.id).copied().unwrap_or(0) > 1
                                || graph.output == Some(node.id))));
                CompiledEffectNodeProfile {
                    subtree_signature: hasher.finish(),
                    cache_policy,
                    estimated_cost,
                    output_cache_enabled,
                }
            }
            EffectGraphNodeKind::Blend { base, overlay, blend_mode, opacity } => {
                let base_profile = profiles.get(base)?;
                let overlay_profile = profiles.get(overlay)?;
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                2u8.hash(&mut hasher);
                base_profile.subtree_signature.hash(&mut hasher);
                overlay_profile.subtree_signature.hash(&mut hasher);
                blend_mode.hash(&mut hasher);
                opacity.to_bits().hash(&mut hasher);
                let blend_policy = if *blend_mode == mondrian_core::types::BlendMode::Dissolve {
                    EffectCachePolicy::FrameDependent
                } else {
                    EffectCachePolicy::Deterministic
                };
                let cache_policy = base_profile
                    .cache_policy
                    .combine(overlay_profile.cache_policy)
                    .combine(blend_policy);
                let estimated_cost =
                    base_profile.estimated_cost + overlay_profile.estimated_cost + 2;
                let output_cache_enabled = cache_policy.permits_cross_call_reuse()
                    && (estimated_cost >= 7
                        || (estimated_cost >= 6
                            && (use_counts.get(&node.id).copied().unwrap_or(0) > 1
                                || graph.output == Some(node.id))));
                CompiledEffectNodeProfile {
                    subtree_signature: hasher.finish(),
                    cache_policy,
                    estimated_cost,
                    output_cache_enabled,
                }
            }
            EffectGraphNodeKind::Mask { input, mask, invert, mask_op } => {
                let input_profile = profiles.get(input)?;
                let mask_profile = profiles.get(mask)?;
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                3u8.hash(&mut hasher);
                input_profile.subtree_signature.hash(&mut hasher);
                mask_profile.subtree_signature.hash(&mut hasher);
                invert.hash(&mut hasher);
                mask_op.hash(&mut hasher);
                let cache_policy = input_profile.cache_policy.combine(mask_profile.cache_policy);
                let estimated_cost = input_profile.estimated_cost + mask_profile.estimated_cost + 1;
                let output_cache_enabled = cache_policy.permits_cross_call_reuse()
                    && (estimated_cost >= 6
                        || (estimated_cost >= 5
                            && (use_counts.get(&node.id).copied().unwrap_or(0) > 1
                                || graph.output == Some(node.id))));
                CompiledEffectNodeProfile {
                    subtree_signature: hasher.finish(),
                    cache_policy,
                    estimated_cost,
                    output_cache_enabled,
                }
            }
            EffectGraphNodeKind::MaskSource { shape, feather, expansion, opacity } => {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                6u8.hash(&mut hasher);
                shape_variant_hash(shape, &mut hasher);
                feather.to_bits().hash(&mut hasher);
                expansion.to_bits().hash(&mut hasher);
                opacity.to_bits().hash(&mut hasher);
                CompiledEffectNodeProfile {
                    subtree_signature: hasher.finish(),
                    cache_policy: EffectCachePolicy::Deterministic,
                    estimated_cost: 4,
                    output_cache_enabled: false,
                }
            }
            EffectGraphNodeKind::MultiInput { inputs, blend_mode, opacity } => {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                7u8.hash(&mut hasher);
                let mut cache_policy = if *blend_mode == mondrian_core::types::BlendMode::Dissolve {
                    EffectCachePolicy::FrameDependent
                } else {
                    EffectCachePolicy::Deterministic
                };
                let mut input_cost = 0u32;
                for input in inputs {
                    let profile = profiles.get(input)?;
                    profile.subtree_signature.hash(&mut hasher);
                    input_cost = input_cost.saturating_add(profile.estimated_cost);
                    cache_policy = cache_policy.combine(profile.cache_policy);
                }
                blend_mode.hash(&mut hasher);
                opacity.to_bits().hash(&mut hasher);
                let estimated_cost = input_cost + inputs.len() as u32;
                CompiledEffectNodeProfile {
                    subtree_signature: hasher.finish(),
                    cache_policy,
                    estimated_cost,
                    output_cache_enabled: cache_policy.permits_cross_call_reuse()
                        && estimated_cost >= 8,
                }
            }
        };
        profiles.insert(node.id, profile);
    }

    Some(profiles)
}

fn effect_render_graph_is_well_formed(graph: &EffectRenderGraph) -> bool {
    let mut node_ids = HashSet::with_capacity(graph.nodes.len());
    graph.nodes.iter().all(|node| {
        node_ids.insert(node.id)
            && !matches!(&node.kind, EffectGraphNodeKind::MultiInput { inputs, .. } if inputs.is_empty())
    })
}

fn collect_reachable_nodes(
    graph: &EffectRenderGraph,
    node_id: EffectGraphNodeId,
    visited: &mut HashSet<EffectGraphNodeId>,
) -> Option<()> {
    if !visited.insert(node_id) {
        return Some(());
    }
    let node = graph.node(node_id)?;
    for input in node.input_ids() {
        collect_reachable_nodes(graph, input, visited)?;
    }
    Some(())
}

fn hash_render_op(op: &EffectRenderOp, state: &mut impl std::hash::Hasher) {
    op.hash_signature(state);
}

fn shape_variant_hash(shape: &crate::mask::MaskShape, state: &mut impl std::hash::Hasher) {
    match shape {
        crate::mask::MaskShape::Rectangle { x, y, width, height, corner_radius } => {
            state.write_u8(0);
            state.write_u32(x.to_bits());
            state.write_u32(y.to_bits());
            state.write_u32(width.to_bits());
            state.write_u32(height.to_bits());
            state.write_u32(corner_radius.to_bits());
        }
        crate::mask::MaskShape::Ellipse { center, radii } => {
            state.write_u8(1);
            state.write_u32(center.x.to_bits());
            state.write_u32(center.y.to_bits());
            state.write_u32(radii.x.to_bits());
            state.write_u32(radii.y.to_bits());
        }
        crate::mask::MaskShape::Path { points, closed } => {
            state.write_u8(2);
            state.write_usize(points.len());
            for point in points {
                state.write_u32(point.position.x.to_bits());
                state.write_u32(point.position.y.to_bits());
                state.write_u32(point.control_in.x.to_bits());
                state.write_u32(point.control_in.y.to_bits());
                state.write_u32(point.control_out.x.to_bits());
                state.write_u32(point.control_out.y.to_bits());
            }
            state.write_u8(if *closed { 1 } else { 0 });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectProcessingBackend, EffectTemporalSpan, EffectWorkingPrecision};

    #[test]
    fn signed_temporal_sample_offsets_derive_exact_directional_extents() {
        let past = TimelineTime::new(-1, 2).expect("past offset");
        let past_duration = TimelineTime::new(1, 2).expect("past duration");
        let future = TimelineTime::new(3, 4).expect("future offset");

        assert_eq!(
            temporal_input_extent_for_sample_offset(past),
            EffectTemporalInputExtent {
                past: EffectTemporalSpan::Finite(past_duration),
                future: EffectTemporalSpan::None,
            }
        );
        assert_eq!(
            temporal_input_extent_for_sample_offset(TimelineTime::ZERO),
            EffectTemporalInputExtent::CURRENT_FRAME
        );
        assert_eq!(
            temporal_input_extent_for_sample_offset(future),
            EffectTemporalInputExtent {
                past: EffectTemporalSpan::None,
                future: EffectTemporalSpan::Finite(future),
            }
        );
    }

    #[test]
    fn compiled_graph_identity_distinguishes_parameters_and_topology() {
        let processor = crate::CustomEffectProcessorBinding::new(Arc::new(
            |_buffer, _width, _height, _params, _frame_seed| Ok(()),
        ));
        let custom_plan = |gain: f64| EffectRenderPlan {
            ops: vec![EffectRenderOp::Custom {
                key: "test.dynamic-params".to_owned(),
                params: serde_json::json!({ "gain": gain }),
                cache_key: Some("stable-implementation".to_owned()),
                cache_policy: EffectCachePolicy::Deterministic,
                processor: Some(processor.clone()),
            }],
        };
        let first = Arc::new(
            compile_scheduled_effect_graph(&custom_plan(0.25))
                .expect("compile first dynamic parameter graph"),
        );
        let second = Arc::new(
            compile_scheduled_effect_graph(&custom_plan(0.75))
                .expect("compile second dynamic parameter graph"),
        );
        let different_topology = Arc::new(
            compile_scheduled_effect_graph(&EffectRenderPlan {
                ops: vec![
                    EffectRenderOp::GaussianBlur { radius: 2.0 },
                    EffectRenderOp::Custom {
                        key: "test.dynamic-params".to_owned(),
                        params: serde_json::json!({ "gain": 0.25 }),
                        cache_key: Some("stable-implementation".to_owned()),
                        cache_policy: EffectCachePolicy::Deterministic,
                        processor: Some(processor),
                    },
                ],
            })
            .expect("compile different topology"),
        );

        assert_ne!(first.identity(), second.identity());
        assert_ne!(first.identity(), different_topology.identity());
        assert_ne!(second.identity(), different_topology.identity());
        assert_ne!(first.semantic_fingerprint(), second.semantic_fingerprint());
        assert_ne!(
            first.semantic_fingerprint(),
            different_topology.semantic_fingerprint()
        );
    }

    #[test]
    fn reachable_unbound_custom_processor_is_rejected() {
        let key = "test.custom.unbound";
        let unbound_op = EffectRenderOp::Custom {
            key: key.to_owned(),
            params: serde_json::json!({}),
            cache_key: None,
            cache_policy: EffectCachePolicy::Deterministic,
            processor: None,
        };
        let rejected =
            compile_reference_effect_graph(&EffectRenderPlan { ops: vec![unbound_op.clone()] });
        assert!(
            rejected.is_none(),
            "a reachable Custom node without an implementation must fail compilation"
        );

        let mut old_unbound_buffer = vec![255, 255, 255, 255];
        let execution_error =
            crate::adjustment::apply_render_op(&mut old_unbound_buffer, 1, 1, &unbound_op, 0)
                .expect_err(
                    "an already emitted unbound operation must not perform a late registry lookup",
                );
        assert!(matches!(
            execution_error,
            crate::EffectExecutionError::CustomProcessorUnavailable { ref key }
                if key == "test.custom.unbound"
        ));
        assert!(
            compile_reference_effect_graph(&EffectRenderPlan { ops: vec![unbound_op] }).is_none(),
            "raw Custom operations cannot acquire an implementation from ambient process state"
        );
    }

    #[test]
    fn only_identity_compilation_has_process_wide_residency() {
        let plan = EffectRenderPlan {
            ops: vec![EffectRenderOp::GaussianBlur { radius: 2.0 }],
        };
        let first_reference =
            compile_reference_effect_graph(&plan).expect("compile first reference graph");
        let second_reference =
            compile_reference_effect_graph(&plan).expect("compile second reference graph");
        assert_eq!(first_reference.identity(), second_reference.identity());
        assert!(
            !Arc::ptr_eq(&first_reference, &second_reference),
            "generic reference compilation must not retain process-wide graph residency"
        );

        let first_identity = identity_compiled_effect_graph().expect("compile identity graph");
        let second_identity = identity_compiled_effect_graph().expect("reuse identity graph");
        assert!(Arc::ptr_eq(&first_identity, &second_identity));
    }

    #[test]
    fn mask_path_coordinates_are_part_of_complete_graph_identity() {
        let graph = |x: f32| EffectRenderGraph {
            nodes: vec![EffectGraphNode {
                id: EffectGraphNodeId(0),
                kind: EffectGraphNodeKind::MaskSource {
                    shape: crate::mask::MaskShape::Path {
                        points: vec![mondrian_core::mask_data::BezierPoint {
                            position: glam::Vec2::new(x, 0.25),
                            control_in: glam::Vec2::new(-0.1, 0.0),
                            control_out: glam::Vec2::new(0.1, 0.0),
                        }],
                        closed: true,
                    },
                    feather: 0.0,
                    expansion: 0.0,
                    opacity: 1.0,
                },
            }],
            output: Some(EffectGraphNodeId(0)),
        };

        assert_ne!(
            graph(0.25).semantic_identity(),
            graph(0.75).semantic_identity()
        );
    }

    #[test]
    fn display_encoded_effect_plans_explicit_round_trip_to_scene_linear() {
        let display_domain =
            EffectColorDomain::DisplayEncodedRgb { color_space: mondrian_core::ColorSpace::Rec709 };
        let plan = EffectRenderPlan {
            ops: vec![EffectRenderOp::ColorAdjust {
                exposure: 0.25,
                contrast: 1.0,
                saturation: 1.0,
                working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
            }],
        };

        let compiled = compile_reference_effect_graph_in_domain(
            &plan,
            EffectColorDomainContract::preserving(display_domain),
        )
        .expect("valid effect graph");

        assert_eq!(compiled.domain_plan.transitions.len(), 2);
        assert_eq!(
            compiled.domain_plan.transitions[0],
            EffectDomainTransition {
                consumer: Some(EffectGraphNodeId(1)),
                input: EffectGraphNodeId(0),
                from: EffectColorDomain::SceneLinearRgb,
                to: display_domain,
            }
        );
        assert_eq!(
            compiled.domain_plan.transitions[1],
            EffectDomainTransition {
                consumer: None,
                input: EffectGraphNodeId(1),
                from: display_domain,
                to: EffectColorDomain::SceneLinearRgb,
            }
        );
        assert!(compiled.domain_plan.blockers.is_empty());
    }

    #[test]
    fn compile_effect_render_graph_builds_linear_chain_from_plan() {
        let plan = EffectRenderPlan {
            ops: vec![
                EffectRenderOp::GaussianBlur { radius: 3.0 },
                EffectRenderOp::Sharpen { amount: 0.5 },
            ],
        };

        let graph = compile_effect_render_graph(&plan);
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.output, Some(EffectGraphNodeId(2)));
        assert!(matches!(
            graph.node(EffectGraphNodeId(0)).map(|node| &node.kind),
            Some(EffectGraphNodeKind::Source)
        ));
        assert!(matches!(
            graph.node(EffectGraphNodeId(1)).map(|node| &node.kind),
            Some(EffectGraphNodeKind::UnaryEffect { input: EffectGraphNodeId(0), .. })
        ));
        assert!(matches!(
            graph.node(EffectGraphNodeId(2)).map(|node| &node.kind),
            Some(EffectGraphNodeKind::UnaryEffect { input: EffectGraphNodeId(1), .. })
        ));
    }

    #[test]
    fn scheduler_orders_dependencies_before_consumers() {
        let graph = compile_effect_render_graph(&EffectRenderPlan {
            ops: vec![
                EffectRenderOp::ColorAdjust {
                    exposure: 0.5,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
                },
                EffectRenderOp::Grain { amount: 0.2 },
            ],
        });

        let schedule = schedule_effect_render_graph(&graph).expect("schedule graph");
        assert_eq!(
            schedule.ordered_nodes,
            vec![
                EffectGraphNodeId(0),
                EffectGraphNodeId(1),
                EffectGraphNodeId(2)
            ]
        );
    }

    #[test]
    fn scheduler_orders_binary_dependencies_before_blend_node() {
        let graph = EffectRenderGraph {
            nodes: vec![
                EffectGraphNode {
                    id: EffectGraphNodeId(0),
                    kind: EffectGraphNodeKind::Source,
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(1),
                    kind: EffectGraphNodeKind::UnaryEffect {
                        input: EffectGraphNodeId(0),
                        op: EffectRenderOp::GaussianBlur { radius: 2.0 },
                    },
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(2),
                    kind: EffectGraphNodeKind::UnaryEffect {
                        input: EffectGraphNodeId(0),
                        op: EffectRenderOp::Sharpen { amount: 0.5 },
                    },
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(3),
                    kind: EffectGraphNodeKind::Blend {
                        base: EffectGraphNodeId(1),
                        overlay: EffectGraphNodeId(2),
                        blend_mode: BlendMode::Overlay,
                        opacity: 0.5,
                    },
                },
            ],
            output: Some(EffectGraphNodeId(3)),
        };

        let schedule = schedule_effect_render_graph(&graph).expect("schedule graph");
        let pos = |id| {
            schedule
                .ordered_nodes
                .iter()
                .position(|node_id| *node_id == id)
                .expect("node should exist in schedule")
        };

        assert!(pos(EffectGraphNodeId(0)) < pos(EffectGraphNodeId(1)));
        assert!(pos(EffectGraphNodeId(0)) < pos(EffectGraphNodeId(2)));
        assert!(pos(EffectGraphNodeId(1)) < pos(EffectGraphNodeId(3)));
        assert!(pos(EffectGraphNodeId(2)) < pos(EffectGraphNodeId(3)));
    }

    #[test]
    fn scheduler_rejects_cycles() {
        let graph = EffectRenderGraph {
            nodes: vec![
                EffectGraphNode {
                    id: EffectGraphNodeId(0),
                    kind: EffectGraphNodeKind::UnaryEffect {
                        input: EffectGraphNodeId(1),
                        op: EffectRenderOp::GaussianBlur { radius: 2.0 },
                    },
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(1),
                    kind: EffectGraphNodeKind::UnaryEffect {
                        input: EffectGraphNodeId(0),
                        op: EffectRenderOp::Sharpen { amount: 1.0 },
                    },
                },
            ],
            output: Some(EffectGraphNodeId(1)),
        };

        assert!(schedule_effect_render_graph(&graph).is_none());
    }

    #[test]
    fn scheduler_rejects_duplicate_node_ids_and_empty_multi_input() {
        let duplicate_ids = EffectRenderGraph {
            nodes: vec![
                EffectGraphNode {
                    id: EffectGraphNodeId(0),
                    kind: EffectGraphNodeKind::Source,
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(0),
                    kind: EffectGraphNodeKind::Source,
                },
            ],
            output: Some(EffectGraphNodeId(0)),
        };
        let empty_multi_input = EffectRenderGraph {
            nodes: vec![EffectGraphNode {
                id: EffectGraphNodeId(0),
                kind: EffectGraphNodeKind::MultiInput {
                    inputs: Vec::new(),
                    blend_mode: BlendMode::Normal,
                    opacity: 1.0,
                },
            }],
            output: Some(EffectGraphNodeId(0)),
        };

        assert!(schedule_effect_render_graph(&duplicate_ids).is_none());
        assert!(schedule_effect_render_graph(&empty_multi_input).is_none());
        assert!(compile_reference_render_graph(duplicate_ids).is_none());
        assert!(compile_reference_render_graph(empty_multi_input).is_none());
    }

    #[test]
    fn compiled_graph_tracks_branching_use_counts() {
        let graph = EffectRenderGraph {
            nodes: vec![
                EffectGraphNode {
                    id: EffectGraphNodeId(0),
                    kind: EffectGraphNodeKind::Source,
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(1),
                    kind: EffectGraphNodeKind::UnaryEffect {
                        input: EffectGraphNodeId(0),
                        op: EffectRenderOp::GaussianBlur { radius: 2.0 },
                    },
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(2),
                    kind: EffectGraphNodeKind::UnaryEffect {
                        input: EffectGraphNodeId(0),
                        op: EffectRenderOp::Sharpen { amount: 0.5 },
                    },
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(3),
                    kind: EffectGraphNodeKind::Blend {
                        base: EffectGraphNodeId(1),
                        overlay: EffectGraphNodeId(2),
                        blend_mode: BlendMode::Overlay,
                        opacity: 0.5,
                    },
                },
            ],
            output: Some(EffectGraphNodeId(3)),
        };

        let compiled = compile_reference_render_graph(graph).expect("compile scheduled graph");
        assert_eq!(
            compiled.node_use_counts.get(&EffectGraphNodeId(0)),
            Some(&2)
        );
        assert_eq!(
            compiled.node_use_counts.get(&EffectGraphNodeId(1)),
            Some(&1)
        );
        assert_eq!(
            compiled.node_use_counts.get(&EffectGraphNodeId(2)),
            Some(&1)
        );
        assert_eq!(
            compiled.node_use_counts.get(&EffectGraphNodeId(3)),
            Some(&0)
        );
    }

    #[test]
    fn builder_helpers_create_branching_blend_graph() {
        let mut builder = EffectGraphBuilderState::new();
        builder.blend_current_with(BlendMode::Screen, 0.35, |graph, source| {
            graph.add_unary_from(source, EffectRenderOp::GaussianBlur { radius: 6.0 })
        });
        let graph = builder.finish();

        assert_eq!(graph.output, Some(EffectGraphNodeId(2)));
        assert!(matches!(
            graph.node(EffectGraphNodeId(1)).map(|node| &node.kind),
            Some(EffectGraphNodeKind::UnaryEffect { input: EffectGraphNodeId(0), .. })
        ));
        assert!(matches!(
            graph.node(EffectGraphNodeId(2)).map(|node| &node.kind),
            Some(EffectGraphNodeKind::Blend {
                base: EffectGraphNodeId(0),
                overlay: EffectGraphNodeId(1),
                blend_mode: BlendMode::Screen,
                ..
            })
        ));
    }

    #[test]
    fn compiled_graph_marks_frame_dependent_cache_policy() {
        let compiled = compile_scheduled_effect_graph(&EffectRenderPlan {
            ops: vec![
                EffectRenderOp::GaussianBlur { radius: 2.0 },
                EffectRenderOp::Grain { amount: 0.5 },
            ],
        })
        .expect("compile scheduled graph");

        assert_eq!(
            compiled.output_cache_policy,
            EffectCachePolicy::FrameDependent
        );
        assert!(compiled.output_cache_enabled);
        assert!(compiled.estimated_cost >= 6);
    }

    #[test]
    fn multi_input_profile_hashes_compositing_contract_and_propagates_dissolve_policy() {
        let graph = |blend_mode, opacity| EffectRenderGraph {
            nodes: vec![
                EffectGraphNode {
                    id: EffectGraphNodeId(0),
                    kind: EffectGraphNodeKind::Source,
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(1),
                    kind: EffectGraphNodeKind::MultiInput {
                        inputs: vec![EffectGraphNodeId(0), EffectGraphNodeId(0)],
                        blend_mode,
                        opacity,
                    },
                },
            ],
            output: Some(EffectGraphNodeId(1)),
        };
        let normal = compile_reference_render_graph(graph(BlendMode::Normal, 0.5))
            .expect("compile normal multi-input");
        let dissolve = compile_reference_render_graph(graph(BlendMode::Dissolve, 0.5))
            .expect("compile dissolve multi-input");
        let opaque = compile_reference_render_graph(graph(BlendMode::Normal, 1.0))
            .expect("compile opaque multi-input");

        assert_eq!(normal.output_cache_policy, EffectCachePolicy::Deterministic);
        assert_eq!(
            dissolve.output_cache_policy,
            EffectCachePolicy::FrameDependent
        );
        assert_ne!(
            normal.node_profiles[&EffectGraphNodeId(1)].subtree_signature,
            dissolve.node_profiles[&EffectGraphNodeId(1)].subtree_signature
        );
        assert_ne!(
            normal.node_profiles[&EffectGraphNodeId(1)].subtree_signature,
            opaque.node_profiles[&EffectGraphNodeId(1)].subtree_signature
        );
    }

    #[test]
    fn mask_source_node_has_no_dependencies() {
        let graph = EffectRenderGraph {
            nodes: vec![EffectGraphNode {
                id: EffectGraphNodeId(0),
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
                    opacity: 1.0,
                },
            }],
            output: Some(EffectGraphNodeId(0)),
        };

        let node = graph.node(EffectGraphNodeId(0)).unwrap();
        assert!(node.input_ids().is_empty());

        // Compile and verify profile.
        let compiled = compile_reference_render_graph(graph).expect("compile");
        let profile = compiled.node_profiles.get(&EffectGraphNodeId(0)).unwrap();
        assert_eq!(profile.cache_policy, EffectCachePolicy::Deterministic);
        assert!(!profile.output_cache_enabled);
    }

    #[test]
    fn mask_node_connected_to_mask_source_compiles() {
        let graph = EffectRenderGraph {
            nodes: vec![
                EffectGraphNode {
                    id: EffectGraphNodeId(0),
                    kind: EffectGraphNodeKind::Source,
                },
                EffectGraphNode {
                    id: EffectGraphNodeId(1),
                    kind: EffectGraphNodeKind::MaskSource {
                        shape: crate::mask::MaskShape::Ellipse {
                            center: glam::Vec2::new(0.5, 0.5),
                            radii: glam::Vec2::new(0.25, 0.25),
                        },
                        feather: 2.0,
                        expansion: 0.0,
                        opacity: 1.0,
                    },
                },
                EffectGraphNode {
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

        let compiled = compile_reference_render_graph(graph).expect("compile");
        assert_eq!(compiled.graph.nodes.len(), 3);
        let source_modes =
            compiled.node_execution_modes(EffectGraphNodeId(1)).expect("MaskSource modes");
        assert!(source_modes.contains(
            EffectProcessingBackend::Cpu,
            EffectWorkingPrecision::Float32
        ));
        assert!(!source_modes.contains(
            EffectProcessingBackend::Gpu,
            EffectWorkingPrecision::Float32
        ));
        let mask_modes = compiled.node_execution_modes(EffectGraphNodeId(2)).expect("Mask modes");
        assert!(mask_modes.contains(
            EffectProcessingBackend::Gpu,
            EffectWorkingPrecision::Float32
        ));
    }
}
