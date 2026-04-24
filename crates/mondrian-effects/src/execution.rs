use crate::adjustment::{apply_render_op, blend_adjustment_result, blend_rgba_pixel, unit_to_u8};
use crate::{
    get_or_compile_scheduled_effect_graph, graph::effect_graph_node_use_counts,
    CompiledEffectGraph, EffectExecutionSchedule, EffectGraphNodeId, EffectGraphNodeKind,
    EffectRenderGraph, EffectRenderPlan,
};
use mondrian_core::{types::BlendMode, Result};
use std::{
    collections::{HashMap, VecDeque},
    hash::{Hash, Hasher},
    sync::{Arc, Mutex, OnceLock, RwLock},
};

pub type CustomEffectRenderProcessor =
    Arc<dyn Fn(&mut Vec<u8>, u32, u32, &serde_json::Value, i64) -> Result<()> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EffectOutputCacheKey {
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

pub fn register_custom_render_processor(
    key: impl Into<String>,
    processor: CustomEffectRenderProcessor,
) {
    custom_render_processor_registry()
        .write()
        .expect("custom render processor registry poisoned")
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
        None,
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
                if base == overlay {
                    let Some(mut base_frame) = take_graph_input(
                        &mut outputs,
                        &mut remaining_uses,
                        &mut buffer_pool,
                        *base,
                        required_len,
                    ) else {
                        return input.to_vec();
                    };
                    let mut overlay_frame = take_execution_buffer(&mut buffer_pool, required_len);
                    overlay_frame.copy_from_slice(&base_frame);
                    blend_graph_inputs_in_place(
                        &mut base_frame,
                        &overlay_frame,
                        *opacity,
                        *blend_mode,
                    );
                    release_execution_buffer(&mut buffer_pool, overlay_frame);
                    if let (Some(compiled), Some(input_signature)) =
                        (compiled, source_input_signature)
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
                    continue;
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
                blend_graph_inputs_in_place(&mut base_frame, &overlay_frame, *opacity, *blend_mode);
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
            EffectGraphNodeKind::Mask { input: input_id, mask, invert } => {
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
                if input_id == mask {
                    let Some(mut source) = take_graph_input(
                        &mut outputs,
                        &mut remaining_uses,
                        &mut buffer_pool,
                        *input_id,
                        required_len,
                    ) else {
                        return input.to_vec();
                    };
                    let mut mask_frame = take_execution_buffer(&mut buffer_pool, required_len);
                    mask_frame.copy_from_slice(&source);
                    apply_alpha_mask_in_place(&mut source, &mask_frame, *invert);
                    release_execution_buffer(&mut buffer_pool, mask_frame);
                    if let (Some(compiled), Some(input_signature)) =
                        (compiled, source_input_signature)
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
                    continue;
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
                apply_alpha_mask_in_place(&mut source, &mask_frame, *invert);
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

fn frame_buffer_signature(buffer: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    buffer.hash(&mut hasher);
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

fn release_execution_buffer(buffer_pool: &mut Vec<Vec<u8>>, mut buffer: Vec<u8>) {
    buffer.clear();
    buffer_pool.push(buffer);
}

fn blend_graph_inputs_in_place(
    base: &mut [u8],
    overlay: &[u8],
    opacity: f32,
    blend_mode: BlendMode,
) {
    for (base_px, overlay_px) in base.chunks_exact_mut(4).zip(overlay.chunks_exact(4)) {
        let blended = blend_rgba_pixel(
            [base_px[0], base_px[1], base_px[2], base_px[3]],
            [overlay_px[0], overlay_px[1], overlay_px[2], overlay_px[3]],
            opacity,
            blend_mode,
        );
        base_px.copy_from_slice(&blended);
    }
}

fn apply_alpha_mask_in_place(input: &mut [u8], mask: &[u8], invert: bool) {
    for (out_px, mask_px) in input.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
        let matte = if invert {
            1.0 - mask_px[3] as f32 / 255.0
        } else {
            mask_px[3] as f32 / 255.0
        };
        out_px[3] = unit_to_u8((out_px[3] as f32 / 255.0) * matte);
    }
}
