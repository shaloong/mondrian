use crate::{EffectCachePolicy, EffectRenderOp, EffectRenderPlan};
use mondrian_core::types::BlendMode;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EffectGraphNodeId(pub u32);

pub type EffectGraphValue = EffectGraphNodeId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EffectGraphNodeKind {
    Source,
    UnaryEffect {
        input: EffectGraphNodeId,
        op: EffectRenderOp,
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
    /// N-input blend or compositing node (future: Audio Mix, Color Mixer, etc.).
    /// Currently implementation-deferred — 2-input Blend covers 95% of use cases.
    MultiInput {
        inputs: Vec<EffectGraphNodeId>,
        blend_mode: BlendMode,
        opacity: f32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectGraphNode {
    pub id: EffectGraphNodeId,
    pub kind: EffectGraphNodeKind,
}

impl EffectGraphNode {
    pub fn input_ids(&self) -> Vec<EffectGraphNodeId> {
        match self.kind {
            EffectGraphNodeKind::Source => Vec::new(),
            EffectGraphNodeKind::UnaryEffect { input, .. } => vec![input],
            EffectGraphNodeKind::Blend { base, overlay, .. } => vec![base, overlay],
            EffectGraphNodeKind::Mask { input, mask, .. } => vec![input, mask],
            EffectGraphNodeKind::MaskSource { .. } => Vec::new(),
            EffectGraphNodeKind::MultiInput { ref inputs, .. } => inputs.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EffectRenderGraph {
    pub nodes: Vec<EffectGraphNode>,
    pub output: Option<EffectGraphNodeId>,
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

    pub fn signature_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.output.hash(&mut hasher);
        for node in &self.nodes {
            node.id.hash(&mut hasher);
            match &node.kind {
                EffectGraphNodeKind::Source => {
                    0u8.hash(&mut hasher);
                }
                EffectGraphNodeKind::UnaryEffect { input, op } => {
                    1u8.hash(&mut hasher);
                    input.hash(&mut hasher);
                    hash_render_op(op, &mut hasher);
                }
                EffectGraphNodeKind::Blend { base, overlay, blend_mode, opacity } => {
                    2u8.hash(&mut hasher);
                    base.hash(&mut hasher);
                    overlay.hash(&mut hasher);
                    blend_mode.hash(&mut hasher);
                    opacity.to_bits().hash(&mut hasher);
                }
                EffectGraphNodeKind::Mask { input, mask, invert, mask_op } => {
                    3u8.hash(&mut hasher);
                    input.hash(&mut hasher);
                    mask.hash(&mut hasher);
                    invert.hash(&mut hasher);
                    mask_op.hash(&mut hasher);
                }
                EffectGraphNodeKind::MaskSource { ref shape, feather, expansion, opacity } => {
                    6u8.hash(&mut hasher);
                    shape_variant_hash(shape, &mut hasher);
                    feather.to_bits().hash(&mut hasher);
                    expansion.to_bits().hash(&mut hasher);
                    opacity.to_bits().hash(&mut hasher);
                }
                EffectGraphNodeKind::MultiInput { ref inputs, blend_mode, opacity } => {
                    7u8.hash(&mut hasher);
                    inputs.hash(&mut hasher);
                    blend_mode.hash(&mut hasher);
                    opacity.to_bits().hash(&mut hasher);
                }
            }
        }
        hasher.finish()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EffectExecutionSchedule {
    pub ordered_nodes: Vec<EffectGraphNodeId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledEffectGraph {
    pub graph: EffectRenderGraph,
    pub schedule: EffectExecutionSchedule,
    pub node_use_counts: HashMap<EffectGraphNodeId, usize>,
    pub node_profiles: HashMap<EffectGraphNodeId, CompiledEffectNodeProfile>,
    pub output_cache_policy: EffectCachePolicy,
    pub estimated_cost: u32,
    pub output_cache_enabled: bool,
    pub signature_hash: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CompiledEffectNodeProfile {
    pub subtree_signature: u64,
    pub cache_policy: EffectCachePolicy,
    pub estimated_cost: u32,
    pub output_cache_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct EffectGraphBuilderState {
    graph: EffectRenderGraph,
    current_output: EffectGraphNodeId,
    next_id: u32,
}

impl Default for EffectGraphBuilderState {
    fn default() -> Self {
        let graph = EffectRenderGraph::identity();
        let current_output = graph.output.expect("identity graph should have output");
        Self {
            graph,
            current_output,
            next_id: current_output.0 + 1,
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

    pub fn append_unary(&mut self, op: EffectRenderOp) -> EffectGraphNodeId {
        let node_id = self.add_unary_from(self.current_output, op);
        self.current_output = node_id;
        node_id
    }

    pub fn add_unary_from(
        &mut self,
        input: EffectGraphNodeId,
        op: EffectRenderOp,
    ) -> EffectGraphNodeId {
        let id = self.alloc_id();
        self.graph.nodes.push(EffectGraphNode {
            id,
            kind: EffectGraphNodeKind::UnaryEffect { input, op },
        });
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

#[derive(Debug)]
struct CompiledEffectGraphCache {
    max_entries: usize,
    entries: HashMap<u64, Arc<CompiledEffectGraph>>,
    order: VecDeque<u64>,
}

impl Default for CompiledEffectGraphCache {
    fn default() -> Self {
        Self {
            max_entries: 256,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }
}

impl CompiledEffectGraphCache {
    fn get(&mut self, key: u64) -> Option<Arc<CompiledEffectGraph>> {
        let value = self.entries.get(&key).cloned()?;
        self.touch(key);
        Some(value)
    }

    fn insert(&mut self, key: u64, value: Arc<CompiledEffectGraph>) {
        if let std::collections::hash_map::Entry::Occupied(mut e) = self.entries.entry(key) {
            e.insert(value);
            self.touch(key);
            return;
        }

        self.entries.insert(key, value);
        self.order.push_back(key);
        while self.entries.len() > self.max_entries {
            if let Some(evicted) = self.order.pop_front() {
                self.entries.remove(&evicted);
            } else {
                break;
            }
        }
    }

    fn touch(&mut self, key: u64) {
        if let Some(index) = self.order.iter().position(|existing| *existing == key) {
            self.order.remove(index);
        }
        self.order.push_back(key);
    }
}

fn compiled_effect_graph_cache() -> &'static Mutex<CompiledEffectGraphCache> {
    static CACHE: OnceLock<Mutex<CompiledEffectGraphCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(CompiledEffectGraphCache::default()))
}

pub fn compile_effect_render_graph(plan: &EffectRenderPlan) -> EffectRenderGraph {
    let mut graph = EffectRenderGraph::identity();
    let mut current = graph.output.expect("identity graph should have source output");

    for (index, op) in plan.ops.iter().cloned().enumerate() {
        let node_id = EffectGraphNodeId((index + 1) as u32);
        graph.nodes.push(EffectGraphNode {
            id: node_id,
            kind: EffectGraphNodeKind::UnaryEffect { input: current, op },
        });
        current = node_id;
    }

    graph.output = Some(current);
    graph
}

pub fn compile_scheduled_effect_graph(plan: &EffectRenderPlan) -> Option<CompiledEffectGraph> {
    let graph = compile_effect_render_graph(plan);
    let schedule = schedule_effect_render_graph(&graph)?;
    let node_profiles = compile_effect_node_profiles(&graph, &schedule)?;
    let (output_cache_policy, estimated_cost, output_cache_enabled) =
        effect_graph_cache_profile(&graph);
    Some(CompiledEffectGraph {
        node_use_counts: effect_graph_node_use_counts(&graph),
        node_profiles,
        output_cache_policy,
        estimated_cost,
        output_cache_enabled,
        signature_hash: graph.signature_hash(),
        graph,
        schedule,
    })
}

pub fn get_or_compile_scheduled_effect_graph(
    plan: &EffectRenderPlan,
) -> Option<Arc<CompiledEffectGraph>> {
    let signature = plan.signature_hash();
    {
        let mut cache = compiled_effect_graph_cache().lock().ok()?;
        if let Some(compiled) = cache.get(signature) {
            return Some(compiled);
        }
    }

    let compiled = Arc::new(compile_scheduled_effect_graph(plan)?);
    let mut cache = compiled_effect_graph_cache().lock().ok()?;
    cache.insert(signature, Arc::clone(&compiled));
    Some(compiled)
}

/// Return the process-wide compiled identity effect graph.
///
/// Most timeline clips have no enabled effects or masks. Keeping the identity
/// graph as a static compiled graph avoids rebuilding the same source-only
/// graph and taking the compiled-graph cache mutex on every preview/export
/// frame.
pub fn identity_compiled_effect_graph() -> Option<Arc<CompiledEffectGraph>> {
    static IDENTITY: OnceLock<Option<Arc<CompiledEffectGraph>>> = OnceLock::new();
    IDENTITY
        .get_or_init(|| compile_scheduled_effect_graph(&EffectRenderPlan::default()).map(Arc::new))
        .clone()
}

pub fn get_or_compile_scheduled_render_graph(
    graph: EffectRenderGraph,
) -> Option<Arc<CompiledEffectGraph>> {
    let signature = graph.signature_hash();
    {
        let mut cache = compiled_effect_graph_cache().lock().ok()?;
        if let Some(compiled) = cache.get(signature) {
            return Some(compiled);
        }
    }

    let schedule = schedule_effect_render_graph(&graph)?;
    let node_use_counts = effect_graph_node_use_counts(&graph);
    let node_profiles = compile_effect_node_profiles(&graph, &schedule)?;
    let (output_cache_policy, estimated_cost, output_cache_enabled) =
        effect_graph_cache_profile(&graph);
    let compiled = Arc::new(CompiledEffectGraph {
        signature_hash: signature,
        graph,
        schedule,
        node_use_counts,
        node_profiles,
        output_cache_policy,
        estimated_cost,
        output_cache_enabled,
    });
    let mut cache = compiled_effect_graph_cache().lock().ok()?;
    cache.insert(signature, Arc::clone(&compiled));
    Some(compiled)
}

pub fn schedule_effect_render_graph(graph: &EffectRenderGraph) -> Option<EffectExecutionSchedule> {
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

pub fn effect_graph_node_use_counts(
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

pub fn effect_graph_cache_profile(graph: &EffectRenderGraph) -> (EffectCachePolicy, u32, bool) {
    let mut policy = EffectCachePolicy::Deterministic;
    let mut estimated_cost = 0u32;
    let mut op_count = 0u32;

    for node in &graph.nodes {
        if let EffectGraphNodeKind::UnaryEffect { op, .. } = &node.kind {
            op_count += 1;
            estimated_cost += op.estimated_cost();
            if op.cache_policy() == EffectCachePolicy::FrameDependent {
                policy = EffectCachePolicy::FrameDependent;
            }
        }
    }

    let output_cache_enabled = op_count > 1 && estimated_cost >= 4;
    (policy, estimated_cost, output_cache_enabled)
}

pub fn compile_effect_node_profiles(
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
                let cache_policy = if input_profile.cache_policy
                    == EffectCachePolicy::FrameDependent
                    || op.cache_policy() == EffectCachePolicy::FrameDependent
                {
                    EffectCachePolicy::FrameDependent
                } else {
                    EffectCachePolicy::Deterministic
                };
                let estimated_cost = input_profile.estimated_cost + op.estimated_cost();
                let output_cache_enabled = estimated_cost >= 5
                    || (estimated_cost >= 4
                        && (use_counts.get(&node.id).copied().unwrap_or(0) > 1
                            || graph.output == Some(node.id)));
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
                let cache_policy = if base_profile.cache_policy == EffectCachePolicy::FrameDependent
                    || overlay_profile.cache_policy == EffectCachePolicy::FrameDependent
                {
                    EffectCachePolicy::FrameDependent
                } else {
                    EffectCachePolicy::Deterministic
                };
                let estimated_cost =
                    base_profile.estimated_cost + overlay_profile.estimated_cost + 2;
                let output_cache_enabled = estimated_cost >= 7
                    || (estimated_cost >= 6
                        && (use_counts.get(&node.id).copied().unwrap_or(0) > 1
                            || graph.output == Some(node.id)));
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
                let cache_policy = if input_profile.cache_policy
                    == EffectCachePolicy::FrameDependent
                    || mask_profile.cache_policy == EffectCachePolicy::FrameDependent
                {
                    EffectCachePolicy::FrameDependent
                } else {
                    EffectCachePolicy::Deterministic
                };
                let estimated_cost = input_profile.estimated_cost + mask_profile.estimated_cost + 1;
                let output_cache_enabled = estimated_cost >= 6
                    || (estimated_cost >= 5
                        && (use_counts.get(&node.id).copied().unwrap_or(0) > 1
                            || graph.output == Some(node.id)));
                CompiledEffectNodeProfile {
                    subtree_signature: hasher.finish(),
                    cache_policy,
                    estimated_cost,
                    output_cache_enabled,
                }
            }
            EffectGraphNodeKind::MaskSource { ref shape, feather, expansion, opacity } => {
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
            EffectGraphNodeKind::MultiInput { ref inputs, .. } => {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                7u8.hash(&mut hasher);
                inputs.hash(&mut hasher);
                let input_cost: u32 =
                    inputs.iter().filter_map(|id| profiles.get(id)).map(|p| p.estimated_cost).sum();
                let estimated_cost = input_cost + inputs.len() as u32;
                CompiledEffectNodeProfile {
                    subtree_signature: hasher.finish(),
                    cache_policy: EffectCachePolicy::Deterministic,
                    estimated_cost,
                    output_cache_enabled: estimated_cost >= 8,
                }
            }
        };
        profiles.insert(node.id, profile);
    }

    Some(profiles)
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
            state.write_u8(if *closed { 1 } else { 0 });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                EffectRenderOp::ColorAdjust { exposure: 0.5, contrast: 1.0, saturation: 1.0 },
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

        let compiled =
            get_or_compile_scheduled_render_graph(graph).expect("compile scheduled graph");
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
        let compiled = get_or_compile_scheduled_render_graph(graph).expect("compile");
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

        let compiled = get_or_compile_scheduled_render_graph(graph).expect("compile");
        // Verify the graph was compiled (non-zero cost).
        assert!(compiled.graph.nodes.len() == 3);
    }
}
