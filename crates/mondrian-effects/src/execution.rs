use crate::adjustment::{
    apply_render_op, apply_render_op_f32, blend_adjustment_result, blend_rgba_f32_pixel_seeded,
    blend_rgba_pixel_seeded, unit_to_u8,
};
use crate::{
    get_or_compile_scheduled_effect_graph, graph::effect_graph_node_use_counts,
    CompiledEffectGraph, EffectExecutionSchedule, EffectGraphNodeId, EffectGraphNodeKind,
    EffectRenderGraph, EffectRenderOp, EffectRenderPlan,
};
use mondrian_core::{types::BlendMode, Result};
use std::{
    collections::{HashMap, VecDeque},
    hash::{Hash, Hasher},
    sync::{Arc, Mutex, OnceLock, RwLock},
};

pub type CustomEffectRenderProcessor =
    Arc<dyn Fn(&mut Vec<u8>, u32, u32, &serde_json::Value, i64) -> Result<()> + Send + Sync>;

/// Error returned when an effect graph cannot execute on the float/linear CPU path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectFloatExecutionError {
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
}

/// Reason an effect graph cannot use the float/linear CPU path yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectFloatUnsupportedReason {
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
struct EffectOutputCacheKey {
    graph_signature: u64,
    input_signature: u64,
    width: u32,
    height: u32,
    frame_seed: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EffectFloatOutputCacheKey {
    graph_signature: u64,
    input_signature: u64,
    width: u32,
    height: u32,
    frame_seed: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EffectNodeOutputCacheKey {
    subtree_signature: u64,
    input_signature: u64,
    width: u32,
    height: u32,
    frame_seed: Option<i64>,
}

#[derive(Debug)]
struct EffectOutputFrameCache {
    max_entries: usize,
    max_bytes: usize,
    total_bytes: usize,
    entries: HashMap<EffectOutputCacheKey, Vec<u8>>,
    order: VecDeque<EffectOutputCacheKey>,
}

#[derive(Debug)]
struct EffectNodeOutputFrameCache {
    max_entries: usize,
    max_bytes: usize,
    total_bytes: usize,
    entries: HashMap<EffectNodeOutputCacheKey, Vec<u8>>,
    order: VecDeque<EffectNodeOutputCacheKey>,
}

#[derive(Debug)]
struct EffectFloatOutputFrameCache {
    max_entries: usize,
    max_bytes: usize,
    total_bytes: usize,
    entries: HashMap<EffectFloatOutputCacheKey, Vec<[f32; 4]>>,
    order: VecDeque<EffectFloatOutputCacheKey>,
}

impl Default for EffectOutputFrameCache {
    fn default() -> Self {
        Self {
            max_entries: 32,
            max_bytes: 64 * 1024 * 1024,
            total_bytes: 0,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }
}

impl EffectOutputFrameCache {
    fn get(&mut self, key: &EffectOutputCacheKey) -> Option<Vec<u8>> {
        let value = self.entries.get(key).cloned()?;
        self.touch(key);
        Some(value)
    }

    fn insert(&mut self, key: EffectOutputCacheKey, value: Vec<u8>) {
        let size = value.len();
        if size > self.max_bytes {
            return;
        }
        if let Some(previous) = self.entries.insert(key.clone(), value) {
            self.total_bytes = self.total_bytes.saturating_sub(previous.len());
            self.remove_from_order(&key);
        }
        self.total_bytes += size;
        self.order.push_back(key);
        self.trim();
    }

    fn touch(&mut self, key: &EffectOutputCacheKey) {
        self.remove_from_order(key);
        self.order.push_back(key.clone());
    }

    fn remove_from_order(&mut self, key: &EffectOutputCacheKey) {
        if let Some(index) = self.order.iter().position(|existing| existing == key) {
            self.order.remove(index);
        }
    }

    fn trim(&mut self) {
        while self.entries.len() > self.max_entries || self.total_bytes > self.max_bytes {
            let Some(evicted_key) = self.order.pop_front() else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&evicted_key) {
                self.total_bytes = self.total_bytes.saturating_sub(evicted.len());
            }
        }
    }
}

impl Default for EffectNodeOutputFrameCache {
    fn default() -> Self {
        Self {
            max_entries: 128,
            max_bytes: 128 * 1024 * 1024,
            total_bytes: 0,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }
}

impl EffectNodeOutputFrameCache {
    fn get(&mut self, key: &EffectNodeOutputCacheKey) -> Option<Vec<u8>> {
        let value = self.entries.get(key).cloned()?;
        self.touch(key);
        Some(value)
    }

    fn insert(&mut self, key: EffectNodeOutputCacheKey, value: Vec<u8>) {
        let size = value.len();
        if size > self.max_bytes {
            return;
        }
        if let Some(previous) = self.entries.insert(key.clone(), value) {
            self.total_bytes = self.total_bytes.saturating_sub(previous.len());
            self.remove_from_order(&key);
        }
        self.total_bytes += size;
        self.order.push_back(key);
        self.trim();
    }

    fn touch(&mut self, key: &EffectNodeOutputCacheKey) {
        self.remove_from_order(key);
        self.order.push_back(key.clone());
    }

    fn remove_from_order(&mut self, key: &EffectNodeOutputCacheKey) {
        if let Some(index) = self.order.iter().position(|existing| existing == key) {
            self.order.remove(index);
        }
    }

    fn trim(&mut self) {
        while self.entries.len() > self.max_entries || self.total_bytes > self.max_bytes {
            let Some(evicted_key) = self.order.pop_front() else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&evicted_key) {
                self.total_bytes = self.total_bytes.saturating_sub(evicted.len());
            }
        }
    }
}

impl Default for EffectFloatOutputFrameCache {
    fn default() -> Self {
        Self {
            max_entries: 8,
            max_bytes: 256 * 1024 * 1024,
            total_bytes: 0,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }
}

impl EffectFloatOutputFrameCache {
    fn get(&mut self, key: &EffectFloatOutputCacheKey) -> Option<Vec<[f32; 4]>> {
        let value = self.entries.get(key).cloned()?;
        self.touch(key);
        Some(value)
    }

    fn insert(&mut self, key: EffectFloatOutputCacheKey, value: Vec<[f32; 4]>) {
        let size = value.len().saturating_mul(std::mem::size_of::<[f32; 4]>());
        if size > self.max_bytes {
            return;
        }
        if let Some(previous) = self.entries.insert(key.clone(), value) {
            self.total_bytes = self
                .total_bytes
                .saturating_sub(previous.len().saturating_mul(std::mem::size_of::<[f32; 4]>()));
            self.remove_from_order(&key);
        }
        self.total_bytes = self.total_bytes.saturating_add(size);
        self.order.push_back(key);
        self.trim();
    }

    fn touch(&mut self, key: &EffectFloatOutputCacheKey) {
        self.remove_from_order(key);
        self.order.push_back(key.clone());
    }

    fn remove_from_order(&mut self, key: &EffectFloatOutputCacheKey) {
        if let Some(index) = self.order.iter().position(|existing| existing == key) {
            self.order.remove(index);
        }
    }

    fn trim(&mut self) {
        while self.entries.len() > self.max_entries || self.total_bytes > self.max_bytes {
            let Some(evicted_key) = self.order.pop_front() else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&evicted_key) {
                self.total_bytes = self
                    .total_bytes
                    .saturating_sub(evicted.len().saturating_mul(std::mem::size_of::<[f32; 4]>()));
            }
        }
    }
}

pub(crate) fn custom_render_processor_registry(
) -> &'static RwLock<HashMap<String, CustomEffectRenderProcessor>> {
    static REGISTRY: OnceLock<RwLock<HashMap<String, CustomEffectRenderProcessor>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

fn effect_output_frame_cache() -> &'static Mutex<EffectOutputFrameCache> {
    static CACHE: OnceLock<Mutex<EffectOutputFrameCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(EffectOutputFrameCache::default()))
}

fn effect_node_output_frame_cache() -> &'static Mutex<EffectNodeOutputFrameCache> {
    static CACHE: OnceLock<Mutex<EffectNodeOutputFrameCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(EffectNodeOutputFrameCache::default()))
}

fn effect_float_output_frame_cache() -> &'static Mutex<EffectFloatOutputFrameCache> {
    static CACHE: OnceLock<Mutex<EffectFloatOutputFrameCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(EffectFloatOutputFrameCache::default()))
}

pub fn register_custom_render_processor(
    key: impl Into<String>,
    processor: CustomEffectRenderProcessor,
) {
    custom_render_processor_registry()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key.into(), processor);
}

pub fn apply_effect_render_plan(
    input: &[u8],
    width: u32,
    height: u32,
    plan: &EffectRenderPlan,
    frame_seed: i64,
) -> Vec<u8> {
    if plan.is_identity() || input.is_empty() || width == 0 || height == 0 {
        return input.to_vec();
    }

    let Some(compiled) = get_or_compile_scheduled_effect_graph(plan) else {
        return input.to_vec();
    };
    apply_compiled_effect_graph(input, width, height, compiled.as_ref(), frame_seed)
}

pub fn apply_effect_render_plan_pass(
    base: &[u8],
    width: u32,
    height: u32,
    plan: &EffectRenderPlan,
    opacity: f32,
    blend_mode: Option<BlendMode>,
    frame_seed: i64,
    out: &mut Vec<u8>,
) {
    let required_len = width as usize * height as usize * 4;
    if out.len() != required_len {
        out.resize(required_len, 0);
    }

    if required_len == 0 || base.len() != required_len {
        out.clear();
        return;
    }

    if opacity <= 1.0e-4 || plan.is_identity() {
        out.copy_from_slice(base);
        return;
    }

    let processed = apply_effect_render_plan(base, width, height, plan, frame_seed);
    blend_adjustment_result(base, &processed, width, height, opacity, blend_mode, out);
}

pub fn apply_effect_render_graph(
    input: &[u8],
    width: u32,
    height: u32,
    graph: &EffectRenderGraph,
    schedule: &EffectExecutionSchedule,
    frame_seed: i64,
) -> Vec<u8> {
    if graph.is_identity() || input.is_empty() || width == 0 || height == 0 {
        return input.to_vec();
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
) -> Vec<u8> {
    let required_len = width as usize * height as usize * 4;
    let source_input_signature = compiled.map(|_| frame_buffer_signature(input));
    let mut outputs = HashMap::<EffectGraphNodeId, Vec<u8>>::with_capacity(graph.nodes.len());
    let mut remaining_uses = node_use_counts.clone();
    let mut buffer_pool = Vec::<Vec<u8>>::new();
    for node_id in &schedule.ordered_nodes {
        let Some(node) = graph.node(*node_id) else {
            return input.to_vec();
        };
        match &node.kind {
            EffectGraphNodeKind::Source => {
                let mut frame = take_execution_buffer(&mut buffer_pool, required_len);
                frame.copy_from_slice(input);
                outputs.insert(node.id, frame);
            }
            EffectGraphNodeKind::UnaryEffect { input: input_id, op } => {
                if let (Some(compiled), Some(input_signature)) = (compiled, source_input_signature)
                {
                    if let Some(cached) = get_cached_node_output(
                        compiled,
                        *node_id,
                        width,
                        height,
                        input_signature,
                        frame_seed,
                    ) {
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
                    return input.to_vec();
                };

                apply_render_op(&mut source, width, height, op, frame_seed);
                if let (Some(compiled), Some(input_signature)) = (compiled, source_input_signature)
                {
                    put_cached_node_output(
                        compiled,
                        *node_id,
                        width,
                        height,
                        input_signature,
                        frame_seed,
                        &source,
                    );
                }
                outputs.insert(node.id, source);
            }
            EffectGraphNodeKind::Blend { base, overlay, blend_mode, opacity } => {
                if let (Some(compiled), Some(input_signature)) = (compiled, source_input_signature)
                {
                    if let Some(cached) = get_cached_node_output(
                        compiled,
                        *node_id,
                        width,
                        height,
                        input_signature,
                        frame_seed,
                    ) {
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
                    return input.to_vec();
                };
                let Some(overlay_frame) = take_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *overlay,
                    required_len,
                ) else {
                    return input.to_vec();
                };
                blend_graph_inputs_in_place(
                    &mut base_frame,
                    &overlay_frame,
                    *opacity,
                    *blend_mode,
                    frame_seed,
                );
                release_execution_buffer(&mut buffer_pool, overlay_frame);
                if let (Some(compiled), Some(input_signature)) = (compiled, source_input_signature)
                {
                    put_cached_node_output(
                        compiled,
                        *node_id,
                        width,
                        height,
                        input_signature,
                        frame_seed,
                        &base_frame,
                    );
                }
                outputs.insert(node.id, base_frame);
            }
            EffectGraphNodeKind::MaskSource { ref shape, feather, expansion, opacity } => {
                let alpha = crate::mask_raster::rasterize_mask_shape(
                    shape, width, height, *feather, *expansion, *opacity,
                );
                // Convert alpha-only buffer to RGBA (white RGB, mask-derived alpha).
                let mut rgba = vec![0u8; required_len];
                for (i, &a) in alpha.iter().enumerate() {
                    rgba[i * 4] = 255;
                    rgba[i * 4 + 1] = 255;
                    rgba[i * 4 + 2] = 255;
                    rgba[i * 4 + 3] = a;
                }
                outputs.insert(node.id, rgba);
            }
            EffectGraphNodeKind::Mask { input: input_id, mask, invert, mask_op } => {
                if let (Some(compiled), Some(input_signature)) = (compiled, source_input_signature)
                {
                    if let Some(cached) = get_cached_node_output(
                        compiled,
                        *node_id,
                        width,
                        height,
                        input_signature,
                        frame_seed,
                    ) {
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
                    return input.to_vec();
                };
                let Some(mask_frame) = take_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *mask,
                    required_len,
                ) else {
                    return input.to_vec();
                };
                apply_alpha_mask_in_place(&mut source, &mask_frame, *invert, *mask_op);
                release_execution_buffer(&mut buffer_pool, mask_frame);
                if let (Some(compiled), Some(input_signature)) = (compiled, source_input_signature)
                {
                    put_cached_node_output(
                        compiled,
                        *node_id,
                        width,
                        height,
                        input_signature,
                        frame_seed,
                        &source,
                    );
                }
                outputs.insert(node.id, source);
            }
            EffectGraphNodeKind::MultiInput { ref inputs, blend_mode, opacity } => {
                let Some(first_id) = inputs.first().copied() else {
                    return input.to_vec();
                };
                let first = take_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    first_id,
                    required_len,
                )
                .unwrap_or_else(|| input.to_vec());
                let mut result = first;
                for overlay_id in &inputs[1..] {
                    if let Some(overlay) = take_graph_input(
                        &mut outputs,
                        &mut remaining_uses,
                        &mut buffer_pool,
                        *overlay_id,
                        required_len,
                    ) {
                        blend_graph_inputs_in_place(
                            &mut result,
                            &overlay,
                            *opacity,
                            *blend_mode,
                            frame_seed,
                        );
                        release_execution_buffer(&mut buffer_pool, overlay);
                    }
                }
                outputs.insert(node.id, result);
            }
        }
    }

    graph
        .output
        .and_then(|output| outputs.remove(&output))
        .unwrap_or_else(|| input.to_vec())
}

pub fn apply_compiled_effect_graph(
    input: &[u8],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
) -> Vec<u8> {
    if let Some(cached) = get_cached_effect_output(input, width, height, compiled, frame_seed) {
        return cached;
    }

    let output = execute_effect_graph(
        input,
        width,
        height,
        &compiled.graph,
        &compiled.schedule,
        &compiled.node_use_counts,
        Some(compiled),
        frame_seed,
    );

    put_cached_effect_output(input, width, height, compiled, frame_seed, &output);
    output
}

/// Return whether a compiled graph can execute entirely on the float/linear CPU path.
pub fn compiled_effect_graph_supports_rgba_f32(compiled: &CompiledEffectGraph) -> bool {
    validate_float_effect_graph(compiled).is_ok()
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
    let required_len = width as usize * height as usize;
    if input.len() != required_len {
        return Err(EffectFloatExecutionError::InputSizeMismatch {
            expected: required_len,
            actual: input.len(),
        });
    }
    if compiled.graph.is_identity() || input.is_empty() || width == 0 || height == 0 {
        return Ok(input.to_vec());
    }
    validate_float_effect_graph(compiled)?;
    if let Some(cached) = get_cached_effect_output_f32(input, width, height, compiled, frame_seed) {
        return Ok(cached);
    }

    let mut outputs =
        HashMap::<EffectGraphNodeId, Vec<[f32; 4]>>::with_capacity(compiled.graph.nodes.len());
    let mut remaining_uses = compiled.node_use_counts.clone();
    let mut buffer_pool = Vec::<Vec<[f32; 4]>>::new();
    for node_id in &compiled.schedule.ordered_nodes {
        let Some(node) = compiled.graph.node(*node_id) else {
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
            EffectGraphNodeKind::UnaryEffect { input: input_id, op } => {
                let Some(mut source) = take_float_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *input_id,
                    required_len,
                ) else {
                    return Err(EffectFloatExecutionError::MissingOutput { node_id: *input_id });
                };
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
                let Some(overlay_frame) = take_float_graph_input(
                    &mut outputs,
                    &mut remaining_uses,
                    &mut buffer_pool,
                    *overlay,
                    required_len,
                ) else {
                    return Err(EffectFloatExecutionError::MissingOutput { node_id: *overlay });
                };
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
                let alpha = crate::mask_raster::rasterize_mask_shape_f32(
                    shape, width, height, *feather, *expansion, *opacity,
                );
                let mut rgba = take_float_execution_buffer(&mut buffer_pool, required_len);
                for (pixel, alpha) in rgba.iter_mut().zip(alpha) {
                    *pixel = [1.0, 1.0, 1.0, alpha];
                }
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
                    let Some(overlay) = take_float_graph_input(
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

    let Some(output_id) = compiled.graph.output else {
        return Ok(input.to_vec());
    };
    let output = outputs
        .remove(&output_id)
        .ok_or(EffectFloatExecutionError::MissingOutput { node_id: output_id })?;
    put_cached_effect_output_f32(input, width, height, compiled, frame_seed, &output);
    Ok(output)
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
    let required_len = width as usize * height as usize;
    if base.len() != required_len {
        return Err(EffectFloatExecutionError::InputSizeMismatch {
            expected: required_len,
            actual: base.len(),
        });
    }
    if required_len == 0 {
        return Ok(Vec::new());
    }
    let mode = blend_mode.unwrap_or(BlendMode::Normal);
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 1.0e-4 || compiled.graph.is_identity() {
        return Ok(base.to_vec());
    }

    let processed =
        apply_compiled_effect_graph_rgba_f32(base, width, height, compiled, frame_seed)?;
    let mut out = base.to_vec();
    blend_rgba_f32_in_place(&mut out, &processed, opacity, mode, frame_seed);
    Ok(out)
}

pub fn apply_effect_render_graph_pass(
    base: &[u8],
    width: u32,
    height: u32,
    graph: &EffectRenderGraph,
    schedule: &EffectExecutionSchedule,
    opacity: f32,
    blend_mode: Option<BlendMode>,
    frame_seed: i64,
    out: &mut Vec<u8>,
) {
    let required_len = width as usize * height as usize * 4;
    if out.len() != required_len {
        out.resize(required_len, 0);
    }

    if required_len == 0 || base.len() != required_len {
        out.clear();
        return;
    }

    if opacity <= 1.0e-4 || graph.is_identity() {
        out.copy_from_slice(base);
        return;
    }

    let processed = apply_effect_render_graph(base, width, height, graph, schedule, frame_seed);
    blend_adjustment_result(base, &processed, width, height, opacity, blend_mode, out);
}

pub fn apply_compiled_effect_graph_pass(
    base: &[u8],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    opacity: f32,
    blend_mode: Option<BlendMode>,
    frame_seed: i64,
    out: &mut Vec<u8>,
) {
    apply_effect_render_graph_pass(
        base,
        width,
        height,
        &compiled.graph,
        &compiled.schedule,
        opacity,
        blend_mode,
        frame_seed,
        out,
    )
}

fn get_cached_effect_output(
    input: &[u8],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
) -> Option<Vec<u8>> {
    let key = effect_output_cache_key(input, width, height, compiled, frame_seed)?;
    let mut cache = effect_output_frame_cache().lock().ok()?;
    cache.get(&key)
}

fn put_cached_effect_output(
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
    let Ok(mut cache) = effect_output_frame_cache().lock() else {
        return;
    };
    cache.insert(key, output.to_vec());
}

fn effect_output_cache_key(
    input: &[u8],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
) -> Option<EffectOutputCacheKey> {
    if !compiled.output_cache_enabled || input.is_empty() || width == 0 || height == 0 {
        return None;
    }

    Some(EffectOutputCacheKey {
        graph_signature: compiled.signature_hash,
        input_signature: frame_buffer_signature(input),
        width,
        height,
        frame_seed: (compiled.output_cache_policy
            == crate::effect::EffectCachePolicy::FrameDependent)
            .then_some(frame_seed),
    })
}

fn get_cached_effect_output_f32(
    input: &[[f32; 4]],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
) -> Option<Vec<[f32; 4]>> {
    let key = effect_output_cache_key_f32(input, width, height, compiled, frame_seed)?;
    let mut cache = effect_float_output_frame_cache().lock().ok()?;
    cache.get(&key)
}

fn put_cached_effect_output_f32(
    input: &[[f32; 4]],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
    output: &[[f32; 4]],
) {
    let Some(key) = effect_output_cache_key_f32(input, width, height, compiled, frame_seed) else {
        return;
    };
    let Ok(mut cache) = effect_float_output_frame_cache().lock() else {
        return;
    };
    cache.insert(key, output.to_vec());
}

fn effect_output_cache_key_f32(
    input: &[[f32; 4]],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
) -> Option<EffectFloatOutputCacheKey> {
    if !effect_output_cache_enabled_f32(compiled) || input.is_empty() || width == 0 || height == 0 {
        return None;
    }

    Some(EffectFloatOutputCacheKey {
        graph_signature: compiled.signature_hash,
        input_signature: frame_buffer_signature_f32(input),
        width,
        height,
        frame_seed: (compiled.output_cache_policy
            == crate::effect::EffectCachePolicy::FrameDependent)
            .then_some(frame_seed),
    })
}

fn effect_output_cache_enabled_f32(compiled: &CompiledEffectGraph) -> bool {
    compiled.output_cache_enabled && validate_float_effect_graph(compiled).is_ok()
}

fn validate_float_effect_graph(
    compiled: &CompiledEffectGraph,
) -> Result<(), EffectFloatExecutionError> {
    for node_id in &compiled.schedule.ordered_nodes {
        let Some(node) = compiled.graph.node(*node_id) else {
            return Err(unsupported_float_graph_node(*node_id, "missing"));
        };
        match &node.kind {
            EffectGraphNodeKind::Source => {}
            EffectGraphNodeKind::UnaryEffect { op, .. } => {
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

fn effect_render_op_supports_rgba_f32(op: &EffectRenderOp) -> bool {
    !matches!(op, EffectRenderOp::Custom { .. })
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
        EffectRenderOp::GaussianBlur { .. } => "gaussian_blur",
        EffectRenderOp::Sharpen { .. } => "sharpen",
        EffectRenderOp::Vignette { .. } => "vignette",
        EffectRenderOp::ChromaticAberration { .. } => "chromatic_aberration",
        EffectRenderOp::Grain { .. } => "grain",
        EffectRenderOp::Lut3D { .. } => "lut3d",
        EffectRenderOp::Custom { .. } => "custom",
    }
}

fn frame_buffer_signature(buffer: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    buffer.hash(&mut hasher);
    hasher.finish()
}

fn frame_buffer_signature_f32(buffer: &[[f32; 4]]) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for pixel in buffer {
        for channel in pixel {
            channel.to_bits().hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn get_cached_node_output(
    compiled: &CompiledEffectGraph,
    node_id: EffectGraphNodeId,
    width: u32,
    height: u32,
    input_signature: u64,
    frame_seed: i64,
) -> Option<Vec<u8>> {
    let key = effect_node_output_cache_key(
        compiled,
        node_id,
        width,
        height,
        input_signature,
        frame_seed,
    )?;
    let mut cache = effect_node_output_frame_cache().lock().ok()?;
    cache.get(&key)
}

fn put_cached_node_output(
    compiled: &CompiledEffectGraph,
    node_id: EffectGraphNodeId,
    width: u32,
    height: u32,
    input_signature: u64,
    frame_seed: i64,
    output: &[u8],
) {
    let Some(key) = effect_node_output_cache_key(
        compiled,
        node_id,
        width,
        height,
        input_signature,
        frame_seed,
    ) else {
        return;
    };
    let Ok(mut cache) = effect_node_output_frame_cache().lock() else {
        return;
    };
    cache.insert(key, output.to_vec());
}

fn effect_node_output_cache_key(
    compiled: &CompiledEffectGraph,
    node_id: EffectGraphNodeId,
    width: u32,
    height: u32,
    input_signature: u64,
    frame_seed: i64,
) -> Option<EffectNodeOutputCacheKey> {
    let profile = compiled.node_profiles.get(&node_id)?;
    if !profile.output_cache_enabled || width == 0 || height == 0 {
        return None;
    }

    Some(EffectNodeOutputCacheKey {
        subtree_signature: profile.subtree_signature,
        input_signature,
        width,
        height,
        frame_seed: (profile.cache_policy == crate::effect::EffectCachePolicy::FrameDependent)
            .then_some(frame_seed),
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
    use crate::mask::MaskOp;
    for (output, matte) in input.iter_mut().zip(mask) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_effect_graph_runs_color_adjust_without_clamping_extended_values() {
        let compiled = get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
            ops: vec![EffectRenderOp::ColorAdjust {
                exposure: 1.0,
                contrast: 1.0,
                saturation: 1.0,
            }],
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
        let compiled = get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
            ops: vec![EffectRenderOp::ColorAdjust {
                exposure: 1.0,
                contrast: 1.0,
                saturation: 1.0,
            }],
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
        let compiled = get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
            ops: vec![EffectRenderOp::ColorAdjust {
                exposure: 0.0,
                contrast: 1.0,
                saturation: 1.0,
            }],
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
        let compiled = get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
            ops: vec![
                EffectRenderOp::ColorAdjust { exposure: 1.0, contrast: 1.0, saturation: 1.0 },
                EffectRenderOp::WhiteBalance { temperature: 0.1, tint: -0.1 },
                EffectRenderOp::ColorAdjust { exposure: 0.0, contrast: 1.1, saturation: 0.9 },
                EffectRenderOp::WhiteBalance { temperature: -0.05, tint: 0.05 },
            ],
        })
        .expect("compile float adjustment chain");
        let input = [[0.25, 0.5, 0.75, 1.0]];

        assert!(effect_output_cache_key_f32(&input, 1, 1, &compiled, 7).is_some());
        assert!(get_cached_effect_output_f32(&input, 1, 1, &compiled, 7).is_none());
        let output =
            apply_compiled_effect_graph_rgba_f32(&input, 1, 1, &compiled, 7).expect("float chain");

        assert_eq!(
            get_cached_effect_output_f32(&input, 1, 1, &compiled, 7),
            Some(output)
        );
    }

    #[test]
    fn float_effect_graph_avoids_output_cache_for_low_cost_adjustments() {
        let compiled = get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
            ops: vec![
                EffectRenderOp::ColorAdjust { exposure: 1.0, contrast: 1.0, saturation: 1.0 },
                EffectRenderOp::WhiteBalance { temperature: 0.1, tint: -0.1 },
            ],
        })
        .expect("compile low-cost float adjustment chain");
        let input = [[0.25, 0.5, 0.75, 1.0]];

        assert!(!compiled.output_cache_enabled);
        assert!(effect_output_cache_key_f32(&input, 1, 1, &compiled, 7).is_none());
        apply_compiled_effect_graph_rgba_f32(&input, 1, 1, &compiled, 7)
            .expect("low-cost float chain");
        assert!(get_cached_effect_output_f32(&input, 1, 1, &compiled, 7).is_none());
    }

    #[test]
    fn float_effect_graph_reports_custom_ops_without_float_abi() {
        let compiled = get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
            ops: vec![EffectRenderOp::Custom {
                key: "test.custom.rgba8-only".to_owned(),
                params: serde_json::json!({}),
                cache_key: None,
                cache_policy: crate::EffectCachePolicy::Deterministic,
            }],
        })
        .expect("compile custom graph");

        let err =
            apply_compiled_effect_graph_rgba_f32(&[[0.25, 0.5, 0.75, 1.0]], 1, 1, &compiled, 0)
                .expect_err("custom effect without float ABI must fail closed");

        assert!(!compiled_effect_graph_supports_rgba_f32(&compiled));
        assert!(matches!(
            err,
            EffectFloatExecutionError::UnsupportedNode {
                reason: EffectFloatUnsupportedReason::UnsupportedRenderOp { op: "custom" },
                ..
            }
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
                        op: EffectRenderOp::ColorAdjust {
                            exposure: 1.0,
                            contrast: 1.0,
                            saturation: 1.0,
                        },
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
        let compiled = crate::get_or_compile_scheduled_render_graph(graph)
            .expect("compile branching float graph");
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
            crate::get_or_compile_scheduled_render_graph(graph).expect("compile float mask graph");
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
                        op: EffectRenderOp::ColorAdjust {
                            exposure: 1.0,
                            contrast: 1.0,
                            saturation: 1.0,
                        },
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
        let compiled = crate::get_or_compile_scheduled_render_graph(graph)
            .expect("compile float multi-input graph");
        let input = vec![[0.75, 0.25, 0.125, 1.0]; 64];

        let first = apply_compiled_effect_graph_rgba_f32(&input, 8, 8, &compiled, 1)
            .expect("execute first dissolve frame");
        let repeated = apply_compiled_effect_graph_rgba_f32(&input, 8, 8, &compiled, 1)
            .expect("execute repeated dissolve frame");
        let second = apply_compiled_effect_graph_rgba_f32(&input, 8, 8, &compiled, 2)
            .expect("execute second dissolve frame");

        assert!(compiled_effect_graph_supports_rgba_f32(&compiled));
        assert_eq!(
            compiled.output_cache_policy,
            crate::EffectCachePolicy::FrameDependent
        );
        assert_eq!(first, repeated);
        assert_ne!(first, second);
        assert!(first.iter().any(|pixel| pixel[0] > 1.0));
        assert!(first.iter().any(|pixel| pixel[0] < 1.0));
    }

    #[test]
    fn float_gaussian_blur_uses_premultiplied_alpha_and_preserves_hdr_color() {
        let compiled = get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
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
        let compiled = get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
            ops: vec![
                EffectRenderOp::GaussianBlur { radius: 0.5 },
                EffectRenderOp::Sharpen { amount: 0.5 },
                EffectRenderOp::Vignette { intensity: 0.25, feather: 0.8 },
                EffectRenderOp::ChromaticAberration { amount: 0.25 },
                EffectRenderOp::Grain { amount: 0.2 },
                EffectRenderOp::Lut3D { lut, intensity: 0.5 },
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
        let compiled = get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
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
        let compiled = get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
            ops: vec![EffectRenderOp::ColorAdjust {
                exposure: 0.0,
                contrast: 1.0,
                saturation: 1.0,
            }],
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
