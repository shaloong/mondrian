//! 效果节点抽象

use crate::execution::{register_custom_render_processor, CustomEffectRenderProcessor};
use crate::graph::{CompiledEffectGraph, EffectGraphBuilderState, EffectRenderGraph};
use crate::lut::Lut3D;
use crate::mask::MaskComponent;
use crate::plugin_contract::{
    effect_plugin_is_library_visible, effect_plugin_is_runtime_available,
    record_plugin_runtime_failure, register_plugin_contract, EffectPluginContract,
};
use mondrian_core::{
    automation::{
        AnimatablePropertyUiMetadata, PropertyBag, PropertyDescriptor, PropertyValue,
    },
    types::{Color, EffectId, TimeCode},
};
// Re-export effect data types from mondrian-core.
pub use mondrian_core::effect_data::{EffectNode, EffectType, namespaced_effect_path};

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    panic::{catch_unwind, AssertUnwindSafe},
    path::Path,
    sync::{Arc, OnceLock, RwLock},
};

#[derive(Debug, Clone, Copy)]
pub struct EffectEvalContext {
    pub time: TimeCode,
}

pub type EffectGraphBuilder =
    Arc<dyn Fn(&EffectNode, EffectEvalContext, &mut EffectGraphBuilderState) + Send + Sync>;
pub type EffectRenderParamsBuilder =
    Arc<dyn Fn(&EffectNode, EffectEvalContext) -> Option<serde_json::Value> + Send + Sync>;
pub type EffectCacheKeyBuilder =
    Arc<dyn Fn(&EffectNode, EffectEvalContext) -> Option<String> + Send + Sync>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EffectCachePolicy {
    #[default]
    Deterministic,
    FrameDependent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EffectRenderOp {
    ColorAdjust {
        exposure: f32,
        contrast: f32,
        saturation: f32,
    },
    WhiteBalance {
        temperature: f32,
        tint: f32,
    },
    GaussianBlur {
        radius: f32,
    },
    Sharpen {
        amount: f32,
    },
    Vignette {
        intensity: f32,
        feather: f32,
    },
    ChromaticAberration {
        amount: f32,
    },
    Grain {
        amount: f32,
    },
    Lut3D {
        lut: Lut3D,
        intensity: f32,
    },
    Custom {
        key: String,
        params: serde_json::Value,
        cache_key: Option<String>,
        cache_policy: EffectCachePolicy,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EffectRenderPlan {
    pub ops: Vec<EffectRenderOp>,
}

impl EffectRenderPlan {
    pub fn is_identity(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn signature_hash(&self) -> u64 {
        use std::hash::Hasher;

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for op in &self.ops {
            op.hash_signature(&mut hasher);
        }
        hasher.finish()
    }
}

impl EffectRenderOp {
    pub fn hash_signature<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash;

        match self {
            EffectRenderOp::ColorAdjust { exposure, contrast, saturation } => {
                0u8.hash(state);
                exposure.to_bits().hash(state);
                contrast.to_bits().hash(state);
                saturation.to_bits().hash(state);
            }
            EffectRenderOp::WhiteBalance { temperature, tint } => {
                1u8.hash(state);
                temperature.to_bits().hash(state);
                tint.to_bits().hash(state);
            }
            EffectRenderOp::GaussianBlur { radius } => {
                2u8.hash(state);
                radius.to_bits().hash(state);
            }
            EffectRenderOp::Sharpen { amount } => {
                3u8.hash(state);
                amount.to_bits().hash(state);
            }
            EffectRenderOp::Vignette { intensity, feather } => {
                4u8.hash(state);
                intensity.to_bits().hash(state);
                feather.to_bits().hash(state);
            }
            EffectRenderOp::ChromaticAberration { amount } => {
                5u8.hash(state);
                amount.to_bits().hash(state);
            }
            EffectRenderOp::Grain { amount } => {
                6u8.hash(state);
                amount.to_bits().hash(state);
            }
            EffectRenderOp::Lut3D { lut, intensity } => {
                7u8.hash(state);
                lut.name.hash(state);
                lut.size.hash(state);
                intensity.to_bits().hash(state);
                for rgb in &lut.data {
                    rgb[0].to_bits().hash(state);
                    rgb[1].to_bits().hash(state);
                    rgb[2].to_bits().hash(state);
                }
            }
            EffectRenderOp::Custom { key, params, cache_key, cache_policy } => {
                8u8.hash(state);
                key.hash(state);
                cache_policy.hash(state);
                if let Some(cache_key) = cache_key {
                    1u8.hash(state);
                    cache_key.hash(state);
                } else {
                    0u8.hash(state);
                    hash_json_value(params, state);
                }
            }
        }
    }

    pub fn cache_policy(&self) -> EffectCachePolicy {
        match self {
            EffectRenderOp::Grain { .. } => EffectCachePolicy::FrameDependent,
            EffectRenderOp::Custom { cache_policy, .. } => *cache_policy,
            _ => EffectCachePolicy::Deterministic,
        }
    }

    pub fn estimated_cost(&self) -> u32 {
        match self {
            EffectRenderOp::ColorAdjust { .. } => 1,
            EffectRenderOp::WhiteBalance { .. } => 1,
            EffectRenderOp::Vignette { .. } => 1,
            EffectRenderOp::Grain { .. } => 2,
            EffectRenderOp::Lut3D { .. } => 2,
            EffectRenderOp::GaussianBlur { .. } => 4,
            EffectRenderOp::Sharpen { .. } => 4,
            EffectRenderOp::ChromaticAberration { .. } => 4,
            EffectRenderOp::Custom { .. } => 5,
        }
    }
}

fn hash_json_value<H: std::hash::Hasher>(value: &serde_json::Value, state: &mut H) {
    use std::hash::Hash;

    match value {
        serde_json::Value::Null => {
            0u8.hash(state);
        }
        serde_json::Value::Bool(boolean) => {
            1u8.hash(state);
            boolean.hash(state);
        }
        serde_json::Value::Number(number) => {
            2u8.hash(state);
            number.to_string().hash(state);
        }
        serde_json::Value::String(text) => {
            3u8.hash(state);
            text.hash(state);
        }
        serde_json::Value::Array(items) => {
            4u8.hash(state);
            items.len().hash(state);
            for item in items {
                hash_json_value(item, state);
            }
        }
        serde_json::Value::Object(map) => {
            5u8.hash(state);
            let mut entries = map.iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            entries.len().hash(state);
            for (key, value) in entries {
                key.hash(state);
                hash_json_value(value, state);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EffectCapabilities {
    pub supports_render_graph: bool,
    pub supports_branching_render_graph: bool,
    pub supports_custom_render_processor: bool,
    pub supports_cache_key_contract: bool,
}

#[derive(Clone)]
pub struct EffectDefinition {
    key: String,
    display_name: String,
    category_path: Vec<String>,
    default_properties: PropertyBag,
    graph_builder: Option<EffectGraphBuilder>,
    capabilities: EffectCapabilities,
    plugin_contract: Option<EffectPluginContract>,
}

impl EffectDefinition {
    pub fn new(
        key: impl Into<String>,
        display_name: impl Into<String>,
        default_properties: PropertyBag,
    ) -> Self {
        Self {
            key: key.into(),
            display_name: display_name.into(),
            category_path: Vec::new(),
            default_properties,
            graph_builder: None,
            capabilities: EffectCapabilities::default(),
            plugin_contract: None,
        }
    }

    pub fn with_category(mut self, category_path: Vec<String>) -> Self {
        self.category_path = category_path;
        self
    }

    pub fn with_property(mut self, descriptor: PropertyDescriptor) -> Self {
        self.default_properties.define(descriptor);
        self
    }

    pub fn with_properties(mut self, properties: PropertyBag) -> Self {
        for (_, property) in properties.iter() {
            self.default_properties.upsert(property.clone());
        }
        self
    }

    pub fn with_graph_builder(mut self, graph_builder: EffectGraphBuilder) -> Self {
        self.graph_builder = Some(graph_builder);
        self.capabilities.supports_render_graph = true;
        self
    }

    pub fn with_branching_graph_builder(mut self, graph_builder: EffectGraphBuilder) -> Self {
        self.graph_builder = Some(graph_builder);
        self.capabilities.supports_render_graph = true;
        self.capabilities.supports_branching_render_graph = true;
        self
    }

    pub fn with_custom_render_processor(
        self,
        params_builder: EffectRenderParamsBuilder,
        processor: CustomEffectRenderProcessor,
    ) -> Self {
        self.with_custom_render_backend(
            params_builder,
            None,
            EffectCachePolicy::Deterministic,
            processor,
        )
    }

    pub fn with_custom_render_backend(
        mut self,
        params_builder: EffectRenderParamsBuilder,
        cache_key_builder: Option<EffectCacheKeyBuilder>,
        cache_policy: EffectCachePolicy,
        processor: CustomEffectRenderProcessor,
    ) -> Self {
        let effect_key = self.key.clone();
        register_custom_render_processor(effect_key.clone(), processor);
        let params_builder_for_graph = Arc::clone(&params_builder);
        let effect_key_for_graph = effect_key.clone();
        let cache_key_builder_for_graph = cache_key_builder.clone();
        self.graph_builder = Some(Arc::new(move |effect, context, graph| {
            if let Some(params) = params_builder_for_graph(effect, context) {
                let cache_key = cache_key_builder_for_graph
                    .as_ref()
                    .and_then(|builder| builder(effect, context));
                graph.append_unary(EffectRenderOp::Custom {
                    key: effect_key_for_graph.clone(),
                    params,
                    cache_key,
                    cache_policy,
                });
            }
        }));
        self.capabilities.supports_render_graph = true;
        self.capabilities.supports_custom_render_processor = true;
        self.capabilities.supports_cache_key_contract = cache_key_builder.is_some();
        self
    }

    pub fn with_plugin_contract(mut self, plugin_contract: EffectPluginContract) -> Self {
        self.plugin_contract = Some(plugin_contract);
        self
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn category_path(&self) -> &[String] {
        &self.category_path
    }

    pub fn capabilities(&self) -> EffectCapabilities {
        self.capabilities
    }

    pub fn supports_visual_evaluation(&self) -> bool {
        self.capabilities.supports_render_graph
            && effect_plugin_is_library_visible(self.key(), self.plugin_contract())
    }

    pub fn plugin_contract(&self) -> Option<&EffectPluginContract> {
        self.plugin_contract.as_ref()
    }
}

fn builtin_effect_types() -> [EffectType; 13] {
    [
        EffectType::BasicCorrection,
        EffectType::WhiteBalance,
        EffectType::Lut3D,
        EffectType::ColorWheel,
        EffectType::Curves,
        EffectType::HueSaturationLightness,
        EffectType::GaussianBlur,
        EffectType::Sharpen,
        EffectType::Vignette,
        EffectType::ChromaticAberration,
        EffectType::Grain,
        EffectType::ChromaKey,
        EffectType::LumaKey,
    ]
}

fn effect_registry() -> &'static RwLock<HashMap<String, Arc<EffectDefinition>>> {
    static REGISTRY: OnceLock<RwLock<HashMap<String, Arc<EffectDefinition>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut definitions = HashMap::new();
        for effect_type in builtin_effect_types() {
            let definition = builtin_effect_definition(effect_type);
            definitions.insert(definition.key.clone(), Arc::new(definition));
        }
        RwLock::new(definitions)
    })
}

pub fn register_effect_definition(definition: EffectDefinition) {
    if let Some(contract) = definition.plugin_contract().cloned() {
        register_plugin_contract(definition.key(), contract);
    }
    let key = definition.key.clone();
    effect_registry()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, Arc::new(definition));
}

pub fn effect_definition(effect_type: &EffectType) -> Option<Arc<EffectDefinition>> {
    let key = effect_type.key();
    effect_registry()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(key.as_str())
        .cloned()
}

pub fn effect_library_types() -> Vec<EffectType> {
    let registry = effect_registry().read().unwrap_or_else(|e| e.into_inner());
    let mut effects = registry
        .values()
        .filter(|definition| definition.supports_visual_evaluation())
        .map(|definition| EffectType::from_key(definition.key()))
        .collect::<Vec<_>>();
    effects.sort_by(|a, b| a.display_name().cmp(b.display_name()));
    effects
}

/// A node in the effect category tree.
#[derive(Debug, Clone)]
pub struct EffectCategoryNode {
    pub name: String,
    pub children: Vec<EffectCategoryNode>,
    pub effects: Vec<EffectType>,
}

/// Build a hierarchical category tree from all registered effects.
pub fn effect_category_tree() -> Vec<EffectCategoryNode> {
    let registry = effect_registry().read().unwrap_or_else(|e| e.into_inner());
    let mut roots: Vec<EffectCategoryNode> = Vec::new();

    for definition in registry.values() {
        if !definition.supports_visual_evaluation() {
            continue;
        }
        let effect_type = EffectType::from_key(definition.key());
        let path = definition.category_path();

        if path.is_empty() {
            // No category: add to a default "其他" root
            insert_effect_into_tree(&mut roots, &["其他".to_string()], &effect_type);
        } else {
            insert_effect_into_tree(&mut roots, path, &effect_type);
        }
    }

    // Sort each level alphabetically
    sort_category_tree(&mut roots);
    roots
}

fn insert_effect_into_tree(
    nodes: &mut Vec<EffectCategoryNode>,
    path: &[String],
    effect_type: &EffectType,
) {
    if path.is_empty() {
        return;
    }
    let head = &path[0];
    let tail = &path[1..];

    let node = nodes.iter_mut().find(|n| n.name == *head);
    if tail.is_empty() {
        // Leaf: add effect to this category
        if let Some(node) = node {
            node.effects.push(effect_type.clone());
        } else {
            nodes.push(EffectCategoryNode {
                name: head.clone(),
                children: Vec::new(),
                effects: vec![effect_type.clone()],
            });
        }
    } else if let Some(node) = node {
        insert_effect_into_tree(&mut node.children, tail, effect_type);
    } else {
        let mut new_node = EffectCategoryNode {
            name: head.clone(),
            children: Vec::new(),
            effects: Vec::new(),
        };
        insert_effect_into_tree(&mut new_node.children, tail, effect_type);
        nodes.push(new_node);
    }
}

fn sort_category_tree(nodes: &mut [EffectCategoryNode]) {
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    for node in nodes.iter_mut() {
        node.effects.sort_by(|a, b| a.display_name().cmp(b.display_name()));
        sort_category_tree(&mut node.children);
    }
}

pub fn build_effect_render_graph(effects: &[EffectNode], time: TimeCode) -> EffectRenderGraph {
    let mut builder = EffectGraphBuilderState::new();
    let context = EffectEvalContext { time };
    for effect in effects.iter().filter(|effect| effect.is_enabled) {
        effect.evaluate_graph_into(context, &mut builder);
    }
    builder.finish()
}

/// Build, mask-inject, and compile the effect graph for a clip.
///
/// This is the entry point used by the render plan builder. It replaces the
/// former `Clip::evaluate_compiled_effect_graph()` method, which lived in
/// `mondrian-timeline` and constituted an architecture violation (P-ARCH1).
pub fn compile_clip_effect_graph(
    effects: &[EffectNode],
    masks: &[MaskComponent],
    time: TimeCode,
) -> Option<Arc<CompiledEffectGraph>> {
    use crate::graph::{EffectGraphNode, EffectGraphNodeId, EffectGraphNodeKind};
    use crate::graph::get_or_compile_scheduled_render_graph;
    use mondrian_core::automation::timecode_to_ticks;
    let mut graph = build_effect_render_graph(effects, time);

    // Inject mask nodes after effects for each enabled mask.
    let mut current_output = graph.output;
    let mut next_id = graph.nodes.len() as u32;
    let ticks = timecode_to_ticks(time);

    for mask in masks {
        if !mask.enabled {
            continue;
        }
        let params = mask.evaluate_at(ticks);

        // MaskSource — rasterizes the shape into an alpha buffer.
        let src_id = EffectGraphNodeId(next_id);
        next_id += 1;
        graph.nodes.push(EffectGraphNode {
            id: src_id,
            kind: EffectGraphNodeKind::MaskSource {
                shape: params.shape,
                feather: params.feather,
                expansion: params.expansion,
                opacity: params.opacity,
            },
        });

        // Mask — applies the alpha buffer to the current output.
        let mask_id = EffectGraphNodeId(next_id);
        next_id += 1;
        let input_id = current_output.unwrap_or(EffectGraphNodeId(0));
        graph.nodes.push(EffectGraphNode {
            id: mask_id,
            kind: EffectGraphNodeKind::Mask {
                input: input_id,
                mask: src_id,
                invert: params.invert,
                mask_op: params.mask_op,
            },
        });
        current_output = Some(mask_id);
    }

    graph.output = current_output;
    get_or_compile_scheduled_render_graph(graph)
}

/// Extension trait for EffectNode methods that require the effect registry.
pub trait EffectNodeExt {
    fn with_defaults(effect_type: EffectType) -> Self;
    fn evaluate_graph_into(&self, context: EffectEvalContext, builder: &mut EffectGraphBuilderState);
}

impl EffectNodeExt for EffectNode {
    fn with_defaults(effect_type: EffectType) -> Self {
        let default_properties = effect_definition(&effect_type)
            .map(|definition| definition.default_properties.clone())
            .unwrap_or_default();
        Self {
            id: EffectId::new(),
            properties: default_properties,
            effect_type,
            params: serde_json::json!({}),
            is_enabled: true,
        }
    }

    fn evaluate_graph_into(
        &self,
        context: EffectEvalContext,
        builder: &mut EffectGraphBuilderState,
    ) {
        if let Some(definition) = effect_definition(&self.effect_type) {
            if !effect_plugin_is_runtime_available(definition.key(), definition.plugin_contract()) {
                return;
            }
            if let Some(graph_builder) = definition.graph_builder.as_ref() {
                let mut staged = builder.clone();
                let result = catch_unwind(AssertUnwindSafe(|| {
                    graph_builder(self, context, &mut staged)
                }));
                if result.is_ok() {
                    *builder = staged;
                } else {
                    record_plugin_runtime_failure(
                        definition.key(),
                        definition.plugin_contract(),
                        "effect graph builder panicked",
                    );
                }
                return;
            }
        }
    }
}

// PropertyHost impl for EffectNode moved to mondrian_core::effect_data

fn default_properties_for(effect_type: EffectType) -> PropertyBag {
    let mut properties = PropertyBag::default();

    match &effect_type {
        EffectType::BasicCorrection => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "exposure",
                "基础校正",
                "曝光",
                PropertyValue::Float(0.0),
                Some(-4.0),
                Some(4.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "contrast",
                "基础校正",
                "对比度",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(3.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "saturation",
                "基础校正",
                "饱和度",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(3.0),
                Some(0.01),
            );
        }
        EffectType::WhiteBalance => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "temperature",
                "白平衡",
                "色温",
                PropertyValue::Float(0.0),
                Some(-1.0),
                Some(1.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "tint",
                "白平衡",
                "色调",
                PropertyValue::Float(0.0),
                Some(-1.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::Lut3D => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "path",
                "LUT",
                "LUT 文件",
                PropertyValue::Text(String::new()),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "intensity",
                "LUT",
                "LUT 强度",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::ColorWheel => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "lift",
                "色轮",
                "Lift",
                PropertyValue::Vec3(glam::Vec3::ONE),
                Some(0.0),
                Some(2.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "gamma",
                "色轮",
                "Gamma",
                PropertyValue::Vec3(glam::Vec3::ONE),
                Some(0.0),
                Some(2.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "gain",
                "色轮",
                "Gain",
                PropertyValue::Vec3(glam::Vec3::ONE),
                Some(0.0),
                Some(2.0),
                Some(0.01),
            );
        }
        EffectType::Curves => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "master",
                "曲线",
                "主曲线强度",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(2.0),
                Some(0.01),
            );
        }
        EffectType::HueSaturationLightness => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "hue",
                "HSL",
                "色相",
                PropertyValue::Float(0.0),
                Some(-180.0),
                Some(180.0),
                Some(1.0),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "saturation",
                "HSL",
                "饱和度",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(2.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "lightness",
                "HSL",
                "明度",
                PropertyValue::Float(0.0),
                Some(-1.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::GaussianBlur => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "radius",
                "模糊",
                "模糊半径",
                PropertyValue::Float(12.0),
                Some(0.0),
                Some(200.0),
                Some(0.1),
            );
        }
        EffectType::Sharpen => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "amount",
                "锐化",
                "锐化强度",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(4.0),
                Some(0.01),
            );
        }
        EffectType::Vignette => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "intensity",
                "暗角",
                "暗角强度",
                PropertyValue::Float(0.35),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "feather",
                "暗角",
                "暗角羽化",
                PropertyValue::Float(0.6),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::ChromaticAberration => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "amount",
                "色差",
                "色差强度",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::Grain => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "amount",
                "颗粒",
                "颗粒强度",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "size",
                "颗粒",
                "颗粒尺寸",
                PropertyValue::Float(1.0),
                Some(0.1),
                Some(4.0),
                Some(0.01),
            );
        }
        EffectType::ChromaKey => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "key_color",
                "抠像",
                "抠像颜色",
                PropertyValue::Color(Color::from_hex(0x00FF00)),
                None,
                None,
                None,
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "similarity",
                "抠像",
                "相似度",
                PropertyValue::Float(0.2),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "blend",
                "抠像",
                "边缘混合",
                PropertyValue::Float(0.1),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::LumaKey => {
            define_builtin_property(
                &mut properties,
                &effect_type,
                "threshold",
                "亮度键",
                "阈值",
                PropertyValue::Float(0.5),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
            define_builtin_property(
                &mut properties,
                &effect_type,
                "softness",
                "亮度键",
                "柔化",
                PropertyValue::Float(0.1),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::Plugin(_) => {}
    }

    properties
}

fn define_builtin_property(
    properties: &mut PropertyBag,
    effect_type: &EffectType,
    parameter: &str,
    group: &str,
    name: &str,
    value: PropertyValue,
    min: Option<f64>,
    max: Option<f64>,
    step: Option<f64>,
) {
    let path = effect_type.property_path(parameter);
    let mut descriptor = PropertyDescriptor::new(path, name, value);
    descriptor.ui_metadata = AnimatablePropertyUiMetadata {
        group_name: Some(group.to_string()),
        min,
        max,
        soft_min: min,
        soft_max: max,
        step,
        supports_bezier: true,
        supports_spatial: false,
    };
    properties.define(descriptor);
}

fn builtin_effect_category(effect_type: &EffectType) -> Vec<String> {
    match effect_type {
        EffectType::BasicCorrection
        | EffectType::WhiteBalance
        | EffectType::ColorWheel
        | EffectType::Curves
        | EffectType::HueSaturationLightness => vec!["颜色".to_string()],
        EffectType::Lut3D => vec!["颜色".to_string(), "LUT".to_string()],
        EffectType::GaussianBlur | EffectType::Sharpen => vec!["模糊与锐化".to_string()],
        EffectType::Vignette | EffectType::ChromaticAberration | EffectType::Grain => {
            vec!["风格化".to_string()]
        }
        EffectType::ChromaKey | EffectType::LumaKey => vec!["抠像".to_string()],
        EffectType::Plugin(_) => vec!["插件".to_string()],
    }
}

fn builtin_effect_definition(effect_type: EffectType) -> EffectDefinition {
    let category = builtin_effect_category(&effect_type);
    let definition = EffectDefinition::new(
        effect_type.key(),
        builtin_display_name(&effect_type),
        default_properties_for(effect_type.clone()),
    )
    .with_category(category);
    if let Some(graph_builder) = builtin_graph_builder_for(&effect_type) {
        definition.with_graph_builder(graph_builder)
    } else {
        definition
    }
}

fn builtin_graph_builder_for(effect_type: &EffectType) -> Option<EffectGraphBuilder> {
    match effect_type {
        EffectType::BasicCorrection => {
            let exposure_path = effect_type.property_suffix("exposure");
            let contrast_path = effect_type.property_suffix("contrast");
            let saturation_path = effect_type.property_suffix("saturation");
            Some(Arc::new(move |effect, context, graph| {
                let exposure = effect.evaluate_f32_by_suffix(&exposure_path, context.time, 0.0);
                let contrast = effect.evaluate_f32_by_suffix(&contrast_path, context.time, 1.0);
                let saturation = effect.evaluate_f32_by_suffix(&saturation_path, context.time, 1.0);
                if exposure.abs() > 1.0e-4
                    || (contrast - 1.0).abs() > 1.0e-4
                    || (saturation - 1.0).abs() > 1.0e-4
                {
                    graph.append_unary(EffectRenderOp::ColorAdjust {
                        exposure,
                        contrast,
                        saturation,
                    });
                }
            }))
        }
        EffectType::WhiteBalance => {
            let temperature_path = effect_type.property_suffix("temperature");
            let tint_path = effect_type.property_suffix("tint");
            Some(Arc::new(move |effect, context, graph| {
                let temperature =
                    effect.evaluate_f32_by_suffix(&temperature_path, context.time, 0.0);
                let tint = effect.evaluate_f32_by_suffix(&tint_path, context.time, 0.0);
                if temperature.abs() > 1.0e-4 || tint.abs() > 1.0e-4 {
                    graph.append_unary(EffectRenderOp::WhiteBalance { temperature, tint });
                }
            }))
        }
        EffectType::Lut3D => {
            let path_suffix = effect_type.property_suffix("path");
            let intensity_path = effect_type.property_suffix("intensity");
            Some(Arc::new(move |effect, context, graph| {
                let intensity = effect.evaluate_f32_by_suffix(&intensity_path, context.time, 1.0);
                if intensity <= 1.0e-4 {
                    return;
                }
                let Some(path) = effect.evaluate_text_by_suffix(&path_suffix, context.time) else {
                    return;
                };
                match Lut3D::from_cube_file_cached(Path::new(path.trim())) {
                    Ok(lut) => {
                        graph.append_unary(EffectRenderOp::Lut3D { lut, intensity });
                    }
                    Err(err) => {
                        tracing::warn!(path = %path, "failed to load LUT graph file: {err}")
                    }
                }
            }))
        }
        EffectType::GaussianBlur => {
            let radius_path = effect_type.property_suffix("radius");
            Some(Arc::new(move |effect, context, graph| {
                let radius = effect.evaluate_f32_by_suffix(&radius_path, context.time, 0.0);
                if radius.abs() > 1.0e-4 {
                    graph.append_unary(EffectRenderOp::GaussianBlur { radius });
                }
            }))
        }
        EffectType::Sharpen => {
            let amount_path = effect_type.property_suffix("amount");
            Some(Arc::new(move |effect, context, graph| {
                let amount = effect.evaluate_f32_by_suffix(&amount_path, context.time, 0.0);
                if amount.abs() > 1.0e-4 {
                    graph.append_unary(EffectRenderOp::Sharpen { amount });
                }
            }))
        }
        EffectType::Vignette => {
            let intensity_path = effect_type.property_suffix("intensity");
            let feather_path = effect_type.property_suffix("feather");
            Some(Arc::new(move |effect, context, graph| {
                let intensity = effect.evaluate_f32_by_suffix(&intensity_path, context.time, 0.0);
                let feather = effect.evaluate_f32_by_suffix(&feather_path, context.time, 0.65);
                if intensity.abs() > 1.0e-4 {
                    graph.append_unary(EffectRenderOp::Vignette { intensity, feather });
                }
            }))
        }
        EffectType::ChromaticAberration => {
            let amount_path = effect_type.property_suffix("amount");
            Some(Arc::new(move |effect, context, graph| {
                let amount = effect.evaluate_f32_by_suffix(&amount_path, context.time, 0.0);
                if amount.abs() > 1.0e-4 {
                    graph.append_unary(EffectRenderOp::ChromaticAberration { amount });
                }
            }))
        }
        EffectType::Grain => {
            let amount_path = effect_type.property_suffix("amount");
            Some(Arc::new(move |effect, context, graph| {
                let amount = effect.evaluate_f32_by_suffix(&amount_path, context.time, 0.0);
                if amount.abs() > 1.0e-4 {
                    graph.append_unary(EffectRenderOp::Grain { amount });
                }
            }))
        }
        _ => None,
    }
}

fn builtin_display_name(effect_type: &EffectType) -> &'static str {
    match effect_type {
        EffectType::BasicCorrection => "基础校正",
        EffectType::WhiteBalance => "白平衡",
        EffectType::Lut3D => "LUT",
        EffectType::ColorWheel => "色轮",
        EffectType::Curves => "曲线",
        EffectType::HueSaturationLightness => "HSL",
        EffectType::GaussianBlur => "模糊",
        EffectType::Sharpen => "锐化",
        EffectType::Vignette => "暗角",
        EffectType::ChromaticAberration => "色差",
        EffectType::Grain => "颗粒",
        EffectType::ChromaKey => "色度抠像",
        EffectType::LumaKey => "亮度键",
        EffectType::Plugin(_) => "插件特效",
    }
}

// EffectType methods (key, from_key, display_name, etc.) are now in mondrian_core::effect_data

/// Registry-aware display name — prefers the registered definition's display_name over the static fallback.
pub fn effect_display_name(effect_type: &EffectType) -> String {
    if let Some(definition) = effect_definition(effect_type) {
        return definition.display_name().to_string();
    }
    if let EffectType::Plugin(key) = effect_type {
        return key.clone();
    }
    effect_type.display_name().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::EffectGraphNodeKind;
    use crate::LutCache;
    use mondrian_core::{
        automation::{timecode_to_ticks, Keyframe, PropertyHost, PropertyMutation, PropertyValue},
        types::Rational,
    };

    fn tc(frame: i64) -> TimeCode {
        TimeCode::new(frame, Rational::new(1, 25))
    }

    #[test]
    fn builtin_effect_properties_are_animatable() {
        let mut effect = EffectNode::new(EffectType::GaussianBlur);
        let radius_path = EffectType::GaussianBlur.property_path("radius");
        effect
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: radius_path.clone(),
                keyframe: Keyframe::linear(timecode_to_ticks(tc(0)), PropertyValue::Float(8.0)),
            })
            .expect("set start keyframe");
        effect
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: radius_path.clone(),
                keyframe: Keyframe::linear(timecode_to_ticks(tc(10)), PropertyValue::Float(28.0)),
            })
            .expect("set end keyframe");

        let value = effect
            .evaluate_property(&radius_path, tc(5))
            .and_then(|value| value.as_f32())
            .expect("evaluate blur");
        assert!((value - 18.0).abs() < 0.01);
    }

    #[test]
    fn builtin_effect_keys_and_property_namespaces_are_canonical() {
        let builtins = builtin_effect_types();
        for effect_type in builtins {
            let key = effect_type.key();
            let namespace = effect_type.property_namespace();
            assert_eq!(
                key.strip_prefix("builtin."),
                Some(namespace.as_str()),
                "builtin key and property namespace should stay in lockstep for {effect_type:?}"
            );

            let properties = default_properties_for(effect_type.clone());
            for (path, _) in properties.iter() {
                assert!(
                    path.starts_with(&format!("effect.{namespace}.")),
                    "property path {path} should use canonical namespace {namespace}"
                );
            }
        }
    }

    #[test]
    fn builtin_lut_effect_builds_render_op_from_cube_path() {
        LutCache::global().clear();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("mondrian-effect-lut-{unique}.cube"));
        std::fs::write(
            &path,
            "LUT_3D_SIZE 2
0 0 0
1 0 0
0 1 0
1 1 0
0 0 1
1 0 1
0 1 1
1 1 1
",
        )
        .expect("cube");

        let mut effect = EffectNode::new(EffectType::Lut3D);
        effect
            .set_static_value_by_suffix(
                &EffectType::Lut3D.property_suffix("path"),
                PropertyValue::Text(path.display().to_string()),
            )
            .expect("set lut path");
        effect
            .set_static_value_by_suffix(
                &EffectType::Lut3D.property_suffix("intensity"),
                PropertyValue::Float(0.75),
            )
            .expect("set intensity");

        let graph = build_effect_render_graph(&[effect], tc(0));
        assert!(graph.nodes.iter().any(|n| matches!(
            &n.kind,
            EffectGraphNodeKind::UnaryEffect { op: EffectRenderOp::Lut3D { .. }, .. }
        )));

        let _ = std::fs::remove_file(path);
        LutCache::global().clear();
    }

    #[test]
    fn plugin_can_register_custom_effect_property() {
        let plugin_type = EffectType::Plugin("plugin.ai.auto_exposure".to_string());
        let exposure_path = plugin_type.property_path("exposure");
        let exposure_suffix = plugin_type.property_suffix("exposure");
        let mut properties = PropertyBag::default();
        properties.define(PropertyDescriptor::new(
            exposure_path.clone(),
            "AI 自动曝光",
            PropertyValue::Float(0.0),
        ));
        register_effect_definition(
            EffectDefinition::new(plugin_type.key(), "AI 自动曝光", properties)
                .with_graph_builder(Arc::new(move |effect, context, graph| {
                    let exposure = effect.evaluate_f32_by_suffix(
                        &exposure_suffix, context.time, 0.0,
                    );
                    if exposure.abs() > 1e-4 {
                        graph.append_unary(EffectRenderOp::ColorAdjust {
                            exposure, contrast: 1.0, saturation: 1.0,
                        });
                    }
                })),
        );

        let mut effect = EffectNode::new(plugin_type.clone());
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: exposure_path.clone(),
                value: PropertyValue::Float(0.85),
            })
            .expect("set plugin property");

        let value = effect
            .evaluate_property(&exposure_path, tc(0))
            .and_then(|value| value.as_f32())
            .expect("read plugin property");
        assert!((value - 0.85).abs() < 0.001);

        // Verify the effect produces a graph node
        let graph = build_effect_render_graph(&[effect], tc(0));
        assert!(!graph.nodes.is_empty());
        assert_eq!(plugin_type.display_name(), "AI 自动曝光".to_string());
    }

    #[test]
    fn plugin_can_build_custom_render_op_plan() {
        let plugin_type = EffectType::Plugin("plugin.render.glow".to_string());
        let mut properties = PropertyBag::default();
        properties.define(PropertyDescriptor::new(
            "plugin.render.glow.amount",
            "Glow Amount",
            PropertyValue::Float(0.4),
        ));
        register_effect_definition(
            EffectDefinition::new(plugin_type.key(), "Glow", properties)
                .with_custom_render_processor(
                    Arc::new(|effect, context| {
                        let amount = effect.evaluate_f32_by_suffix(
                            "plugin.render.glow.amount",
                            context.time,
                            0.0,
                        );
                        if amount > 0.0 {
                            Some(serde_json::json!({ "amount": amount }))
                        } else {
                            None
                        }
                    }),
                    Arc::new(|_, _, _, _, _| Ok(())),
                ),
        );

        let effect = EffectNode::new(plugin_type);
        let graph = build_effect_render_graph(&[effect], tc(0));
        let custom_node = graph.nodes.iter().find(|n| matches!(
            &n.kind,
            EffectGraphNodeKind::UnaryEffect { op: EffectRenderOp::Custom { .. }, .. }
        ));
        assert!(custom_node.is_some(), "expected custom render op node");
        if let EffectGraphNodeKind::UnaryEffect { op: EffectRenderOp::Custom { key, params, cache_key, cache_policy }, .. } = &custom_node.unwrap().kind {
            assert_eq!(key, "plugin.render.glow");
            assert_eq!(*cache_key, None);
            assert_eq!(*cache_policy, EffectCachePolicy::Deterministic);
            assert!(
                (params["amount"].as_f64().expect("amount should be numeric") - 0.4).abs()
                    < 1.0e-6
            );
        }
    }

    #[test]
    fn plugin_can_build_branching_render_graph() {
        let plugin_type = EffectType::Plugin("plugin.graph.glow_mix".to_string());
        let mut properties = PropertyBag::default();
        properties.define(PropertyDescriptor::new(
            "plugin.graph.glow_mix.radius",
            "Glow Radius",
            PropertyValue::Float(4.0),
        ));
        properties.define(PropertyDescriptor::new(
            "plugin.graph.glow_mix.opacity",
            "Glow Opacity",
            PropertyValue::Float(0.35),
        ));
        register_effect_definition(
            EffectDefinition::new(plugin_type.key(), "Glow Mix", properties)
                .with_branching_graph_builder(Arc::new(|effect, context, graph| {
                    let radius = effect.evaluate_f32_by_suffix(
                        "plugin.graph.glow_mix.radius",
                        context.time,
                        0.0,
                    );
                    let opacity = effect
                        .evaluate_f32_by_suffix("plugin.graph.glow_mix.opacity", context.time, 0.0)
                        .clamp(0.0, 1.0);
                    if radius <= 1.0e-4 || opacity <= 1.0e-4 {
                        return;
                    }

                    graph.blend_current_with(
                        mondrian_core::types::BlendMode::Screen,
                        opacity,
                        |graph, source| {
                            graph.add_unary_from(source, EffectRenderOp::GaussianBlur { radius })
                        },
                    );
                })),
        );

        let effect = EffectNode::new(plugin_type.clone());
        let graph = build_effect_render_graph(&[effect], tc(0));
        assert_eq!(graph.nodes.len(), 3);
        assert!(matches!(
            graph.node(crate::graph::EffectGraphNodeId(1)).map(|node| &node.kind),
            Some(crate::graph::EffectGraphNodeKind::UnaryEffect { .. })
        ));
        assert!(matches!(
            graph.node(crate::graph::EffectGraphNodeId(2)).map(|node| &node.kind),
            Some(crate::graph::EffectGraphNodeKind::Blend { .. })
        ));

        let definition = effect_definition(&plugin_type).expect("effect definition");
        let caps = definition.capabilities();
        assert!(caps.supports_render_graph);
        assert!(caps.supports_branching_render_graph);
    }

    #[test]
    fn custom_render_backend_can_provide_stable_cache_key_contract() {
        let plugin_type = EffectType::Plugin("plugin.render.lut_loader".to_string());
        let mut properties = PropertyBag::default();
        properties.define(PropertyDescriptor::new(
            "plugin.render.lut_loader.asset_path",
            "LUT Path",
            PropertyValue::Text("looks/teal_orange.cube".to_string()),
        ));
        register_effect_definition(
            EffectDefinition::new(plugin_type.key(), "LUT Loader", properties)
                .with_custom_render_backend(
                    Arc::new(|effect, context| {
                        let path = effect
                            .evaluate_property("plugin.render.lut_loader.asset_path", context.time)
                            .and_then(|value| match value {
                                PropertyValue::Text(text) => Some(text),
                                _ => None,
                            })
                            .unwrap_or_default();
                        Some(serde_json::json!({ "asset_path": path }))
                    }),
                    Some(Arc::new(|effect, context| {
                        effect
                            .evaluate_property("plugin.render.lut_loader.asset_path", context.time)
                            .and_then(|value| match value {
                                PropertyValue::Text(text) => Some(text),
                                _ => None,
                            })
                            .map(|path| format!("lut:{path}"))
                    })),
                    EffectCachePolicy::Deterministic,
                    Arc::new(|_, _, _, _, _| Ok(())),
                ),
        );

        let effect = EffectNode::new(plugin_type.clone());
        let graph = build_effect_render_graph(&[effect], tc(0));
        let custom_node = graph.nodes.iter().find_map(|n| match &n.kind {
            EffectGraphNodeKind::UnaryEffect { op: EffectRenderOp::Custom { cache_key, cache_policy, .. }, .. } => {
                Some((cache_key.clone(), *cache_policy))
            }
            _ => None,
        });
        let (cache_key, cache_policy) = custom_node.expect("expected custom render op node");
        assert_eq!(cache_key.as_deref(), Some("lut:looks/teal_orange.cube"));
        assert_eq!(cache_policy, EffectCachePolicy::Deterministic);

        let caps = effect_definition(&plugin_type).expect("effect definition").capabilities();
        assert!(caps.supports_custom_render_processor);
        assert!(caps.supports_cache_key_contract);
    }

    #[test]
    fn failing_plugin_graph_builder_isolated_and_hidden_after_disable_policy() {
        let plugin_type = EffectType::Plugin("plugin.graph.unstable".to_string());
        register_effect_definition(
            EffectDefinition::new(plugin_type.key(), "Unstable Graph", PropertyBag::default())
                .with_plugin_contract(
                    crate::EffectPluginContract::new("1.0.0")
                        .with_failure_policy(
                            crate::EffectPluginFailurePolicy::DisablePluginDefinition,
                        )
                        .with_degradation_policy(
                            crate::EffectPluginDegradationPolicy::HideFromEffectLibrary,
                        ),
                )
                .with_graph_builder(Arc::new(|_, _, _| {
                    panic!("unstable graph builder");
                })),
        );

        let effect = EffectNode::new(plugin_type.clone());
        let graph = build_effect_render_graph(&[effect], tc(0));
        assert!(graph.is_identity());

        let status =
            crate::effect_plugin_runtime_status(&plugin_type.key()).expect("plugin runtime status");
        assert!(status.disabled);
        assert_eq!(
            status.last_error.as_deref(),
            Some("effect graph builder panicked")
        );
        assert!(!effect_library_types().contains(&plugin_type));
    }
}
