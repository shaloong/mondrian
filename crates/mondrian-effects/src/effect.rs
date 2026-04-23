//! 效果节点抽象

use crate::adjustment::AdjustmentLayerParams;
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

#[derive(Clone)]
pub struct EffectDefinition {
    key: String,
    display_name: String,
    default_properties: PropertyBag,
    evaluator: Option<EffectEvaluator>,
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
        }
    }

    pub fn with_evaluator(mut self, evaluator: EffectEvaluator) -> Self {
        self.evaluator = Some(evaluator);
        self
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn supports_visual_evaluation(&self) -> bool {
        self.evaluator.is_some()
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
            if let Some(evaluator) = definition.evaluator.as_ref() {
                evaluator(self, context, output);
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
    let mut define = |path: &str,
                      group: &str,
                      name: &str,
                      value: PropertyValue,
                      min: Option<f64>,
                      max: Option<f64>,
                      step: Option<f64>| {
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
    };

    match effect_type {
        EffectType::BasicCorrection => {
            define(
                "effect.basic.exposure",
                "基础校正",
                "曝光",
                PropertyValue::Float(0.0),
                Some(-4.0),
                Some(4.0),
                Some(0.01),
            );
            define(
                "effect.basic.contrast",
                "基础校正",
                "对比度",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(3.0),
                Some(0.01),
            );
            define(
                "effect.basic.saturation",
                "基础校正",
                "饱和度",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(3.0),
                Some(0.01),
            );
        }
        EffectType::WhiteBalance => {
            define(
                "effect.white_balance.temperature",
                "白平衡",
                "色温",
                PropertyValue::Float(0.0),
                Some(-1.0),
                Some(1.0),
                Some(0.01),
            );
            define(
                "effect.white_balance.tint",
                "白平衡",
                "色调",
                PropertyValue::Float(0.0),
                Some(-1.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::Lut3D => {
            define(
                "effect.lut.intensity",
                "LUT",
                "LUT 强度",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::ColorWheel => {
            define(
                "effect.color_wheel.lift",
                "色轮",
                "Lift",
                PropertyValue::Vec3(glam::Vec3::ONE),
                Some(0.0),
                Some(2.0),
                Some(0.01),
            );
            define(
                "effect.color_wheel.gamma",
                "色轮",
                "Gamma",
                PropertyValue::Vec3(glam::Vec3::ONE),
                Some(0.0),
                Some(2.0),
                Some(0.01),
            );
            define(
                "effect.color_wheel.gain",
                "色轮",
                "Gain",
                PropertyValue::Vec3(glam::Vec3::ONE),
                Some(0.0),
                Some(2.0),
                Some(0.01),
            );
        }
        EffectType::Curves => {
            define(
                "effect.curves.master",
                "曲线",
                "主曲线强度",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(2.0),
                Some(0.01),
            );
        }
        EffectType::HueSaturationLightness => {
            define(
                "effect.hsl.hue",
                "HSL",
                "色相",
                PropertyValue::Float(0.0),
                Some(-180.0),
                Some(180.0),
                Some(1.0),
            );
            define(
                "effect.hsl.saturation",
                "HSL",
                "饱和度",
                PropertyValue::Float(1.0),
                Some(0.0),
                Some(2.0),
                Some(0.01),
            );
            define(
                "effect.hsl.lightness",
                "HSL",
                "明度",
                PropertyValue::Float(0.0),
                Some(-1.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::GaussianBlur => {
            define(
                "effect.blur.radius",
                "模糊",
                "模糊半径",
                PropertyValue::Float(12.0),
                Some(0.0),
                Some(200.0),
                Some(0.1),
            );
        }
        EffectType::Sharpen => {
            define(
                "effect.sharpen.amount",
                "锐化",
                "锐化强度",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(4.0),
                Some(0.01),
            );
        }
        EffectType::Vignette => {
            define(
                "effect.vignette.intensity",
                "暗角",
                "暗角强度",
                PropertyValue::Float(0.35),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
            define(
                "effect.vignette.feather",
                "暗角",
                "暗角羽化",
                PropertyValue::Float(0.6),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::ChromaticAberration => {
            define(
                "effect.chromatic.amount",
                "色差",
                "色差强度",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::Grain => {
            define(
                "effect.grain.amount",
                "颗粒",
                "颗粒强度",
                PropertyValue::Float(0.0),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
            define(
                "effect.grain.size",
                "颗粒",
                "颗粒尺寸",
                PropertyValue::Float(1.0),
                Some(0.1),
                Some(4.0),
                Some(0.01),
            );
        }
        EffectType::ChromaKey => {
            define(
                "effect.chroma.key_color",
                "抠像",
                "抠像颜色",
                PropertyValue::Color(Color::from_hex(0x00FF00)),
                None,
                None,
                None,
            );
            define(
                "effect.chroma.similarity",
                "抠像",
                "相似度",
                PropertyValue::Float(0.2),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
            define(
                "effect.chroma.blend",
                "抠像",
                "边缘混合",
                PropertyValue::Float(0.1),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
        }
        EffectType::LumaKey => {
            define(
                "effect.luma.threshold",
                "亮度键",
                "阈值",
                PropertyValue::Float(0.5),
                Some(0.0),
                Some(1.0),
                Some(0.01),
            );
            define(
                "effect.luma.softness",
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

fn builtin_effect_definition(effect_type: EffectType) -> EffectDefinition {
    let definition = EffectDefinition::new(
        effect_type.key(),
        builtin_display_name(&effect_type),
        default_properties_for(effect_type.clone()),
    );
    if let Some(evaluator) = builtin_evaluator_for(&effect_type) {
        definition.with_evaluator(evaluator)
    } else {
        definition
    }
}

fn builtin_evaluator_for(effect_type: &EffectType) -> Option<EffectEvaluator> {
    match effect_type {
        EffectType::BasicCorrection => Some(Arc::new(|effect, context, output| {
            output.adjustment.exposure = effect.evaluate_f32_by_suffix(
                "basic.exposure",
                context.time,
                output.adjustment.exposure,
            );
            output.adjustment.contrast = effect.evaluate_f32_by_suffix(
                "basic.contrast",
                context.time,
                output.adjustment.contrast,
            );
            output.adjustment.saturation = effect.evaluate_f32_by_suffix(
                "basic.saturation",
                context.time,
                output.adjustment.saturation,
            );
        })),
        EffectType::WhiteBalance => Some(Arc::new(|effect, context, output| {
            output.adjustment.temperature = effect.evaluate_f32_by_suffix(
                "white_balance.temperature",
                context.time,
                output.adjustment.temperature,
            );
            output.adjustment.tint = effect.evaluate_f32_by_suffix(
                "white_balance.tint",
                context.time,
                output.adjustment.tint,
            );
        })),
        EffectType::GaussianBlur => Some(Arc::new(|effect, context, output| {
            output.adjustment.blur_radius = effect.evaluate_f32_by_suffix(
                "blur.radius",
                context.time,
                output.adjustment.blur_radius,
            );
        })),
        EffectType::Sharpen => Some(Arc::new(|effect, context, output| {
            output.adjustment.sharpen_amount = effect.evaluate_f32_by_suffix(
                "sharpen.amount",
                context.time,
                output.adjustment.sharpen_amount,
            );
        })),
        EffectType::Vignette => Some(Arc::new(|effect, context, output| {
            output.adjustment.vignette_intensity = effect.evaluate_f32_by_suffix(
                "vignette.intensity",
                context.time,
                output.adjustment.vignette_intensity,
            );
            output.adjustment.vignette_feather = effect.evaluate_f32_by_suffix(
                "vignette.feather",
                context.time,
                output.adjustment.vignette_feather,
            );
        })),
        EffectType::ChromaticAberration => Some(Arc::new(|effect, context, output| {
            output.adjustment.chromatic_aberration = effect.evaluate_f32_by_suffix(
                "chromatic.amount",
                context.time,
                output.adjustment.chromatic_aberration,
            );
        })),
        EffectType::Grain => Some(Arc::new(|effect, context, output| {
            output.adjustment.grain_amount = effect.evaluate_f32_by_suffix(
                "grain.amount",
                context.time,
                output.adjustment.grain_amount,
            );
        })),
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
    pub fn key(&self) -> String {
        match self {
            EffectType::BasicCorrection => "builtin.basic_correction".to_string(),
            EffectType::WhiteBalance => "builtin.white_balance".to_string(),
            EffectType::Lut3D => "builtin.lut_3d".to_string(),
            EffectType::ColorWheel => "builtin.color_wheel".to_string(),
            EffectType::Curves => "builtin.curves".to_string(),
            EffectType::HueSaturationLightness => "builtin.hsl".to_string(),
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
            "builtin.hsl" => EffectType::HueSaturationLightness,
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
        effect
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: "effect.blur.radius".to_string(),
                keyframe: Keyframe::linear(timecode_to_ticks(tc(0)), PropertyValue::Float(8.0)),
            })
            .expect("set start keyframe");
        effect
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: "effect.blur.radius".to_string(),
                keyframe: Keyframe::linear(timecode_to_ticks(tc(10)), PropertyValue::Float(28.0)),
            })
            .expect("set end keyframe");

        let value = effect
            .evaluate_property("effect.blur.radius", tc(5))
            .and_then(|value| value.as_f32())
            .expect("evaluate blur");
        assert!((value - 18.0).abs() < 0.01);
    }

    #[test]
    fn plugin_can_register_custom_effect_property() {
        let plugin_type = EffectType::Plugin("plugin.ai.auto_exposure".to_string());
        let mut properties = PropertyBag::default();
        properties.define(PropertyDescriptor::new(
            "plugin.ai.auto_exposure",
            "AI 自动曝光",
            PropertyValue::Float(0.0),
        ));
        register_effect_definition(
            EffectDefinition::new(plugin_type.key(), "AI 自动曝光", properties).with_evaluator(
                Arc::new(|effect, context, output| {
                    output.adjustment.exposure = effect.evaluate_f32_by_suffix(
                        "plugin.ai.auto_exposure",
                        context.time,
                        output.adjustment.exposure,
                    );
                }),
            ),
        );

        let mut effect = EffectNode::new(plugin_type.clone());
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: "plugin.ai.auto_exposure".to_string(),
                value: PropertyValue::Float(0.85),
            })
            .expect("set plugin property");

        let value = effect
            .evaluate_property("plugin.ai.auto_exposure", tc(0))
            .and_then(|value| value.as_f32())
            .expect("read plugin property");
        assert!((value - 0.85).abs() < 0.001);

        let stack = evaluate_effect_stack(&[effect], tc(0));
        assert!((stack.adjustment.exposure - 0.85).abs() < 0.001);
        assert_eq!(plugin_type.display_name(), "AI 自动曝光".to_string());
    }
}
