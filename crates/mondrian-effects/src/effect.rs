//! 效果节点抽象

use mondrian_core::{
    automation::{
        timecode_to_ticks, AnimatablePropertyUiMetadata, PropertyBag, PropertyDescriptor,
        PropertyHost, PropertyMutation, PropertyValue,
    },
    types::{Color, EffectId, TimeCode},
    Result,
};
use serde::{Deserialize, Serialize};

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
        Self {
            id: EffectId::new(),
            properties: default_properties_for(effect_type.clone()),
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
    }

    properties
}

fn namespaced_effect_path(effect_id: EffectId, path: &str) -> String {
    if let Some(rest) = path.strip_prefix("effect.") {
        format!("effect.{}.{}", effect_id, rest)
    } else {
        format!("effect.{}.{}", effect_id, path)
    }
}

impl EffectType {
    pub fn display_name(&self) -> &'static str {
        match self {
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
        let mut effect = EffectNode::new(EffectType::Grain);
        effect.define_property(PropertyDescriptor::new(
            "plugin.ai.auto_exposure",
            "AI 自动曝光",
            PropertyValue::Float(0.0),
        ));
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
    }
}
