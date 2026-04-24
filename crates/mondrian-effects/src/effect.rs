//! 效果节点抽象

use crate::adjustment::AdjustmentLayerParams;
use crate::execution::{register_custom_render_processor, CustomEffectRenderProcessor};
use crate::graph::{EffectGraphBuilderState, EffectRenderGraph};
use crate::plugin_contract::{
    effect_plugin_is_library_visible, effect_plugin_is_runtime_available,
    record_plugin_runtime_failure, register_plugin_contract, EffectPluginContract,
};
use mondrian_core::{
    automation::{
        timecode_to_ticks, AnimatablePropertyUiMetadata, PropertyBag, PropertyDescriptor,
        PropertyHost, PropertyMutation, PropertyValue,
    },
    types::{Color, EffectId, TimeCode},
    Result,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{Arc, OnceLock, RwLock},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectType {
    BasicCorrection,
    WhiteBalance,
    Lut3D,
    ColorWheel,
    Curves,
    HueSaturationLightness,
    GaussianBlur,
    Sharpen,
    Vignette,
    ChromaticAberration,
    Grain,
    ChromaKey,
    LumaKey,
    Plugin(String),
}

#[derive(Debug, Clone, Copy)]
pub struct EffectEvalContext {
    pub time: TimeCode,
}

#[derive(Debug, Clone, Default)]
pub struct EffectStackEvaluation {
    pub adjustment: AdjustmentLayerParams,
}

pub type EffectEvaluator =
    Arc<dyn Fn(&EffectNode, EffectEvalContext, &mut EffectStackEvaluation) + Send + Sync>;
pub type EffectGraphBuilder =
    Arc<dyn Fn(&EffectNode, EffectEvalContext, &mut EffectGraphBuilderState) + Send + Sync>;
pub type EffectRenderBuilder =
    Arc<dyn Fn(&EffectNode, EffectEvalContext, &mut EffectRenderPlan) + Send + Sync>;
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
            EffectRenderOp::Custom { key, params, cache_key, cache_policy } => {
                7u8.hash(state);
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
    pub supports_render_plan_fallback: bool,
    pub supports_custom_render_processor: bool,
    pub supports_cache_key_contract: bool,
    pub supports_legacy_parameter_evaluation: bool,
}

#[derive(Clone)]
pub struct EffectDefinition {
    key: String,
    display_name: String,
    default_properties: PropertyBag,
    evaluator: Option<EffectEvaluator>,
    graph_builder: Option<EffectGraphBuilder>,
    render_builder: Option<EffectRenderBuilder>,
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
            default_properties,
            evaluator: None,
            graph_builder: None,
            render_builder: None,
            capabilities: EffectCapabilities::default(),
            plugin_contract: None,
        }
    }

    pub fn with_evaluator(mut self, evaluator: EffectEvaluator) -> Self {
        self.evaluator = Some(evaluator);
        self.capabilities.supports_legacy_parameter_evaluation = true;
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

    pub fn with_render_builder(mut self, render_builder: EffectRenderBuilder) -> Self {
        self.render_builder = Some(render_builder);
        self.capabilities.supports_render_plan_fallback = true;
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
        let cache_key_builder_for_plan = cache_key_builder.clone();
        self.render_builder = Some(Arc::new(move |effect, context, plan| {
            if let Some(params) = params_builder(effect, context) {
                let cache_key = cache_key_builder_for_plan
                    .as_ref()
                    .and_then(|builder| builder(effect, context));
                plan.ops.push(EffectRenderOp::Custom {
                    key: effect_key.clone(),
                    params,
                    cache_key,
                    cache_policy,
                });
            }
        }));
        self.capabilities.supports_render_graph = true;
        self.capabilities.supports_render_plan_fallback = true;
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

    pub fn capabilities(&self) -> EffectCapabilities {
        self.capabilities
    }

    pub fn supports_visual_evaluation(&self) -> bool {
        (self.capabilities.supports_render_graph || self.capabilities.supports_render_plan_fallback)
            && effect_plugin_is_library_visible(self.key(), self.plugin_contract())
    }

    pub fn supports_legacy_parameter_evaluation(&self) -> bool {
        self.capabilities.supports_legacy_parameter_evaluation
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
        .expect("effect registry poisoned")
        .insert(key, Arc::new(definition));
}

pub fn effect_definition(effect_type: &EffectType) -> Option<Arc<EffectDefinition>> {
    let key = effect_type.key();
    effect_registry()
        .read()
        .expect("effect registry poisoned")
        .get(key.as_str())
        .cloned()
}

pub fn effect_library_types() -> Vec<EffectType> {
    let registry = effect_registry().read().expect("effect registry poisoned");
    let mut effects = registry
        .values()
        .filter(|definition| definition.supports_visual_evaluation())
        .map(|definition| EffectType::from_key(definition.key()))
        .collect::<Vec<_>>();
    effects.sort_by_key(|effect_type| effect_type.display_name());
    effects
}

pub fn evaluate_effect_stack(effects: &[EffectNode], time: TimeCode) -> EffectStackEvaluation {
    let mut evaluation = EffectStackEvaluation::default();
    let context = EffectEvalContext { time };
    for effect in effects.iter().filter(|effect| effect.is_enabled) {
        effect.evaluate_into(context, &mut evaluation);
    }
    evaluation
}

pub fn build_effect_render_plan(effects: &[EffectNode], time: TimeCode) -> EffectRenderPlan {
    let mut plan = EffectRenderPlan::default();
    let context = EffectEvalContext { time };
    for effect in effects.iter().filter(|effect| effect.is_enabled) {
        effect.evaluate_render_into(context, &mut plan);
    }
    plan
}

pub fn build_effect_render_graph(effects: &[EffectNode], time: TimeCode) -> EffectRenderGraph {
    let mut builder = EffectGraphBuilderState::new();
    let context = EffectEvalContext { time };
    for effect in effects.iter().filter(|effect| effect.is_enabled) {
        effect.evaluate_graph_into(context, &mut builder);
    }
    builder.finish()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectNode {
    #[serde(default)]
    pub id: EffectId,
    pub effect_type: EffectType,
    #[serde(default)]
    pub properties: PropertyBag,
    #[serde(default)]
    pub params: serde_json::Value,
    pub is_enabled: bool,
}

impl EffectNode {
    pub fn new(effect_type: EffectType) -> Self {
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

    pub fn evaluate_property(&self, path: &str, time: TimeCode) -> Option<PropertyValue> {
        self.properties.evaluate(path, timecode_to_ticks(time))
    }

    pub fn define_property(&mut self, descriptor: PropertyDescriptor) {
        self.properties.define(descriptor);
    }

    pub fn instantiate_for_clip(&mut self, group_name: String) {
        let mut namespaced = PropertyBag::default();
        for (_, property) in self.properties.iter() {
            let mut property = property.clone();
            property.descriptor.path = namespaced_effect_path(self.id, &property.descriptor.path);
            property.descriptor.ui_metadata.group_name = Some(group_name.clone());
            namespaced.upsert(property);
        }
        self.properties = namespaced;
    }

    pub fn evaluate_f32_by_suffix(&self, suffix: &str, time: TimeCode, fallback: f32) -> f32 {
        self.properties
            .iter()
            .find(|(path, _)| path.ends_with(suffix))
            .and_then(|(path, _)| self.evaluate_property(path, time))
            .and_then(|value| value.as_f32())
            .unwrap_or(fallback)
    }

    pub fn set_static_value_by_suffix(&mut self, suffix: &str, value: PropertyValue) -> Result<()> {
        let path = self
            .properties
            .iter()
            .find(|(path, _)| path.ends_with(suffix))
            .map(|(path, _)| path.to_string())
            .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "effect_set_static_value".to_string(),
                reason: format!("效果属性不存在: {suffix}"),
            })?;
        self.properties.set_static_value(&path, value)
    }

    pub fn evaluate_into(&self, context: EffectEvalContext, output: &mut EffectStackEvaluation) {
        if let Some(definition) = effect_definition(&self.effect_type) {
            if !effect_plugin_is_runtime_available(definition.key(), definition.plugin_contract()) {
                return;
            }
            if let Some(evaluator) = definition.evaluator.as_ref() {
                let mut staged = output.clone();
                let result =
                    catch_unwind(AssertUnwindSafe(|| evaluator(self, context, &mut staged)));
                if result.is_ok() {
                    *output = staged;
                } else {
                    record_plugin_runtime_failure(
                        definition.key(),
                        definition.plugin_contract(),
                        "effect evaluator panicked",
                    );
                }
            }
        }
    }

    pub fn evaluate_render_into(&self, context: EffectEvalContext, plan: &mut EffectRenderPlan) {
        if let Some(definition) = effect_definition(&self.effect_type) {
            if !effect_plugin_is_runtime_available(definition.key(), definition.plugin_contract()) {
                return;
            }
            if let Some(render_builder) = definition.render_builder.as_ref() {
                let mut staged = EffectRenderPlan::default();
                let result = catch_unwind(AssertUnwindSafe(|| {
                    render_builder(self, context, &mut staged)
                }));
                if result.is_ok() {
                    plan.ops.extend(staged.ops);
                } else {
                    record_plugin_runtime_failure(
                        definition.key(),
                        definition.plugin_contract(),
                        "effect render builder panicked",
                    );
                }
            }
        }
    }

    pub fn evaluate_graph_into(
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
            if let Some(render_builder) = definition.render_builder.as_ref() {
                let mut plan = EffectRenderPlan::default();
                let result = catch_unwind(AssertUnwindSafe(|| {
                    render_builder(self, context, &mut plan)
                }));
                if result.is_ok() {
                    let mut staged = builder.clone();
                    for op in plan.ops {
                        staged.append_unary(op);
                    }
                    *builder = staged;
                } else {
                    record_plugin_runtime_failure(
                        definition.key(),
                        definition.plugin_contract(),
                        "effect render-plan fallback panicked",
                    );
                }
            }
        }
    }
}

impl PropertyHost for EffectNode {
    fn property_bag(&self) -> Result<PropertyBag> {
        Ok(self.properties.clone())
    }

    fn apply_property_mutation(&mut self, mutation: PropertyMutation) -> Result<()> {
        self.properties.apply_mutation(mutation)
    }
}

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

fn builtin_effect_definition(effect_type: EffectType) -> EffectDefinition {
    let definition = EffectDefinition::new(
        effect_type.key(),
        builtin_display_name(&effect_type),
        default_properties_for(effect_type.clone()),
    );
    let definition = if let Some(evaluator) = builtin_evaluator_for(&effect_type) {
        definition.with_evaluator(evaluator)
    } else {
        definition
    };
    let definition = if let Some(graph_builder) = builtin_graph_builder_for(&effect_type) {
        definition.with_graph_builder(graph_builder)
    } else {
        definition
    };
    if let Some(render_builder) = builtin_render_builder_for(&effect_type) {
        definition.with_render_builder(render_builder)
    } else {
        definition
    }
}

fn builtin_evaluator_for(effect_type: &EffectType) -> Option<EffectEvaluator> {
    match effect_type {
        EffectType::BasicCorrection => {
            let exposure_path = effect_type.property_suffix("exposure");
            let contrast_path = effect_type.property_suffix("contrast");
            let saturation_path = effect_type.property_suffix("saturation");
            Some(Arc::new(move |effect, context, output| {
                output.adjustment.exposure = effect.evaluate_f32_by_suffix(
                    &exposure_path,
                    context.time,
                    output.adjustment.exposure,
                );
                output.adjustment.contrast = effect.evaluate_f32_by_suffix(
                    &contrast_path,
                    context.time,
                    output.adjustment.contrast,
                );
                output.adjustment.saturation = effect.evaluate_f32_by_suffix(
                    &saturation_path,
                    context.time,
                    output.adjustment.saturation,
                );
            }))
        }
        EffectType::WhiteBalance => {
            let temperature_path = effect_type.property_suffix("temperature");
            let tint_path = effect_type.property_suffix("tint");
            Some(Arc::new(move |effect, context, output| {
                output.adjustment.temperature = effect.evaluate_f32_by_suffix(
                    &temperature_path,
                    context.time,
                    output.adjustment.temperature,
                );
                output.adjustment.tint =
                    effect.evaluate_f32_by_suffix(&tint_path, context.time, output.adjustment.tint);
            }))
        }
        EffectType::GaussianBlur => {
            let radius_path = effect_type.property_suffix("radius");
            Some(Arc::new(move |effect, context, output| {
                output.adjustment.blur_radius = effect.evaluate_f32_by_suffix(
                    &radius_path,
                    context.time,
                    output.adjustment.blur_radius,
                );
            }))
        }
        EffectType::Sharpen => {
            let amount_path = effect_type.property_suffix("amount");
            Some(Arc::new(move |effect, context, output| {
                output.adjustment.sharpen_amount = effect.evaluate_f32_by_suffix(
                    &amount_path,
                    context.time,
                    output.adjustment.sharpen_amount,
                );
            }))
        }
        EffectType::Vignette => {
            let intensity_path = effect_type.property_suffix("intensity");
            let feather_path = effect_type.property_suffix("feather");
            Some(Arc::new(move |effect, context, output| {
                output.adjustment.vignette_intensity = effect.evaluate_f32_by_suffix(
                    &intensity_path,
                    context.time,
                    output.adjustment.vignette_intensity,
                );
                output.adjustment.vignette_feather = effect.evaluate_f32_by_suffix(
                    &feather_path,
                    context.time,
                    output.adjustment.vignette_feather,
                );
            }))
        }
        EffectType::ChromaticAberration => {
            let amount_path = effect_type.property_suffix("amount");
            Some(Arc::new(move |effect, context, output| {
                output.adjustment.chromatic_aberration = effect.evaluate_f32_by_suffix(
                    &amount_path,
                    context.time,
                    output.adjustment.chromatic_aberration,
                );
            }))
        }
        EffectType::Grain => {
            let amount_path = effect_type.property_suffix("amount");
            Some(Arc::new(move |effect, context, output| {
                output.adjustment.grain_amount = effect.evaluate_f32_by_suffix(
                    &amount_path,
                    context.time,
                    output.adjustment.grain_amount,
                );
            }))
        }
        _ => None,
    }
}

fn builtin_render_builder_for(effect_type: &EffectType) -> Option<EffectRenderBuilder> {
    match effect_type {
        EffectType::BasicCorrection => {
            let exposure_path = effect_type.property_suffix("exposure");
            let contrast_path = effect_type.property_suffix("contrast");
            let saturation_path = effect_type.property_suffix("saturation");
            Some(Arc::new(move |effect, context, plan| {
                let exposure = effect.evaluate_f32_by_suffix(&exposure_path, context.time, 0.0);
                let contrast = effect.evaluate_f32_by_suffix(&contrast_path, context.time, 1.0);
                let saturation = effect.evaluate_f32_by_suffix(&saturation_path, context.time, 1.0);
                if exposure.abs() > 1.0e-4
                    || (contrast - 1.0).abs() > 1.0e-4
                    || (saturation - 1.0).abs() > 1.0e-4
                {
                    plan.ops.push(EffectRenderOp::ColorAdjust { exposure, contrast, saturation });
                }
            }))
        }
        EffectType::WhiteBalance => {
            let temperature_path = effect_type.property_suffix("temperature");
            let tint_path = effect_type.property_suffix("tint");
            Some(Arc::new(move |effect, context, plan| {
                let temperature =
                    effect.evaluate_f32_by_suffix(&temperature_path, context.time, 0.0);
                let tint = effect.evaluate_f32_by_suffix(&tint_path, context.time, 0.0);
                if temperature.abs() > 1.0e-4 || tint.abs() > 1.0e-4 {
                    plan.ops.push(EffectRenderOp::WhiteBalance { temperature, tint });
                }
            }))
        }
        EffectType::GaussianBlur => {
            let radius_path = effect_type.property_suffix("radius");
            Some(Arc::new(move |effect, context, plan| {
                let radius = effect.evaluate_f32_by_suffix(&radius_path, context.time, 0.0);
                if radius.abs() > 1.0e-4 {
                    plan.ops.push(EffectRenderOp::GaussianBlur { radius });
                }
            }))
        }
        EffectType::Sharpen => {
            let amount_path = effect_type.property_suffix("amount");
            Some(Arc::new(move |effect, context, plan| {
                let amount = effect.evaluate_f32_by_suffix(&amount_path, context.time, 0.0);
                if amount.abs() > 1.0e-4 {
                    plan.ops.push(EffectRenderOp::Sharpen { amount });
                }
            }))
        }
        EffectType::Vignette => {
            let intensity_path = effect_type.property_suffix("intensity");
            let feather_path = effect_type.property_suffix("feather");
            Some(Arc::new(move |effect, context, plan| {
                let intensity = effect.evaluate_f32_by_suffix(&intensity_path, context.time, 0.0);
                let feather = effect.evaluate_f32_by_suffix(&feather_path, context.time, 0.65);
                if intensity.abs() > 1.0e-4 {
                    plan.ops.push(EffectRenderOp::Vignette { intensity, feather });
                }
            }))
        }
        EffectType::ChromaticAberration => {
            let amount_path = effect_type.property_suffix("amount");
            Some(Arc::new(move |effect, context, plan| {
                let amount = effect.evaluate_f32_by_suffix(&amount_path, context.time, 0.0);
                if amount.abs() > 1.0e-4 {
                    plan.ops.push(EffectRenderOp::ChromaticAberration { amount });
                }
            }))
        }
        EffectType::Grain => {
            let amount_path = effect_type.property_suffix("amount");
            Some(Arc::new(move |effect, context, plan| {
                let amount = effect.evaluate_f32_by_suffix(&amount_path, context.time, 0.0);
                if amount.abs() > 1.0e-4 {
                    plan.ops.push(EffectRenderOp::Grain { amount });
                }
            }))
        }
        _ => None,
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

fn namespaced_effect_path(effect_id: EffectId, path: &str) -> String {
    if let Some(rest) = path.strip_prefix("effect.") {
        format!("effect.{}.{}", effect_id, rest)
    } else {
        format!("effect.{}.{}", effect_id, path)
    }
}

impl EffectType {
    pub fn property_namespace(&self) -> String {
        match self {
            EffectType::BasicCorrection => "basic_correction".to_string(),
            EffectType::WhiteBalance => "white_balance".to_string(),
            EffectType::Lut3D => "lut_3d".to_string(),
            EffectType::ColorWheel => "color_wheel".to_string(),
            EffectType::Curves => "curves".to_string(),
            EffectType::HueSaturationLightness => "hue_saturation_lightness".to_string(),
            EffectType::GaussianBlur => "gaussian_blur".to_string(),
            EffectType::Sharpen => "sharpen".to_string(),
            EffectType::Vignette => "vignette".to_string(),
            EffectType::ChromaticAberration => "chromatic_aberration".to_string(),
            EffectType::Grain => "grain".to_string(),
            EffectType::ChromaKey => "chroma_key".to_string(),
            EffectType::LumaKey => "luma_key".to_string(),
            EffectType::Plugin(key) => key.clone(),
        }
    }

    pub fn property_path(&self, parameter: &str) -> String {
        format!("effect.{}.{}", self.property_namespace(), parameter)
    }

    pub fn property_suffix(&self, parameter: &str) -> String {
        format!("{}.{}", self.property_namespace(), parameter)
    }

    pub fn key(&self) -> String {
        match self {
            EffectType::BasicCorrection => "builtin.basic_correction".to_string(),
            EffectType::WhiteBalance => "builtin.white_balance".to_string(),
            EffectType::Lut3D => "builtin.lut_3d".to_string(),
            EffectType::ColorWheel => "builtin.color_wheel".to_string(),
            EffectType::Curves => "builtin.curves".to_string(),
            EffectType::HueSaturationLightness => "builtin.hue_saturation_lightness".to_string(),
            EffectType::GaussianBlur => "builtin.gaussian_blur".to_string(),
            EffectType::Sharpen => "builtin.sharpen".to_string(),
            EffectType::Vignette => "builtin.vignette".to_string(),
            EffectType::ChromaticAberration => "builtin.chromatic_aberration".to_string(),
            EffectType::Grain => "builtin.grain".to_string(),
            EffectType::ChromaKey => "builtin.chroma_key".to_string(),
            EffectType::LumaKey => "builtin.luma_key".to_string(),
            EffectType::Plugin(key) => key.clone(),
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key {
            "builtin.basic_correction" => EffectType::BasicCorrection,
            "builtin.white_balance" => EffectType::WhiteBalance,
            "builtin.lut_3d" => EffectType::Lut3D,
            "builtin.color_wheel" => EffectType::ColorWheel,
            "builtin.curves" => EffectType::Curves,
            "builtin.hue_saturation_lightness" => EffectType::HueSaturationLightness,
            "builtin.gaussian_blur" => EffectType::GaussianBlur,
            "builtin.sharpen" => EffectType::Sharpen,
            "builtin.vignette" => EffectType::Vignette,
            "builtin.chromatic_aberration" => EffectType::ChromaticAberration,
            "builtin.grain" => EffectType::Grain,
            "builtin.chroma_key" => EffectType::ChromaKey,
            "builtin.luma_key" => EffectType::LumaKey,
            other => EffectType::Plugin(other.to_string()),
        }
    }

    pub fn display_name(&self) -> String {
        if let Some(definition) = effect_definition(self) {
            return definition.display_name().to_string();
        }
        match self {
            EffectType::Plugin(key) => key.clone(),
            _ => builtin_display_name(self).to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{automation::Keyframe, types::Rational};

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
            EffectDefinition::new(plugin_type.key(), "AI 自动曝光", properties).with_evaluator(
                Arc::new(move |effect, context, output| {
                    output.adjustment.exposure = effect.evaluate_f32_by_suffix(
                        &exposure_suffix,
                        context.time,
                        output.adjustment.exposure,
                    );
                }),
            ),
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

        let stack = evaluate_effect_stack(&[effect], tc(0));
        assert!((stack.adjustment.exposure - 0.85).abs() < 0.001);
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
        let plan = build_effect_render_plan(&[effect], tc(0));
        assert_eq!(plan.ops.len(), 1);
        match &plan.ops[0] {
            EffectRenderOp::Custom { key, params, cache_key, cache_policy } => {
                assert_eq!(key, "plugin.render.glow");
                assert_eq!(cache_key, &None);
                assert_eq!(*cache_policy, EffectCachePolicy::Deterministic);
                assert!(
                    (params["amount"].as_f64().expect("amount should be numeric") - 0.4).abs()
                        < 1.0e-6
                );
            }
            _ => panic!("expected custom render op"),
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
        assert!(!caps.supports_legacy_parameter_evaluation);
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
        let plan = build_effect_render_plan(&[effect], tc(0));
        match &plan.ops[0] {
            EffectRenderOp::Custom { cache_key, cache_policy, .. } => {
                assert_eq!(cache_key.as_deref(), Some("lut:looks/teal_orange.cube"));
                assert_eq!(*cache_policy, EffectCachePolicy::Deterministic);
            }
            _ => panic!("expected custom render op"),
        }

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
