//! 效果节点抽象

use mondrian_core::{
    automation::{
        timecode_to_ticks, PropertyBag, PropertyDescriptor, PropertyHost, PropertyMutation,
        PropertyValue,
    },
    types::{Color, EffectId, TimeCode},
    Result,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectType {
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
    let mut define = |path: &str, name: &str, value: PropertyValue| {
        properties.define(PropertyDescriptor::new(path, name, value));
    };

    match effect_type {
        EffectType::Lut3D => {
            define(
                "effect.lut.intensity",
                "LUT 强度",
                PropertyValue::Float(1.0),
            );
        }
        EffectType::ColorWheel => {
            define(
                "effect.color_wheel.lift",
                "Lift",
                PropertyValue::Vec3(glam::Vec3::ONE),
            );
            define(
                "effect.color_wheel.gamma",
                "Gamma",
                PropertyValue::Vec3(glam::Vec3::ONE),
            );
            define(
                "effect.color_wheel.gain",
                "Gain",
                PropertyValue::Vec3(glam::Vec3::ONE),
            );
        }
        EffectType::Curves => {
            define(
                "effect.curves.master",
                "主曲线强度",
                PropertyValue::Float(1.0),
            );
        }
        EffectType::HueSaturationLightness => {
            define("effect.hsl.hue", "色相", PropertyValue::Float(0.0));
            define("effect.hsl.saturation", "饱和度", PropertyValue::Float(1.0));
            define("effect.hsl.lightness", "明度", PropertyValue::Float(0.0));
        }
        EffectType::GaussianBlur => {
            define("effect.blur.radius", "模糊半径", PropertyValue::Float(12.0));
        }
        EffectType::Sharpen => {
            define(
                "effect.sharpen.amount",
                "锐化强度",
                PropertyValue::Float(0.0),
            );
        }
        EffectType::Vignette => {
            define(
                "effect.vignette.intensity",
                "暗角强度",
                PropertyValue::Float(0.35),
            );
            define(
                "effect.vignette.feather",
                "暗角羽化",
                PropertyValue::Float(0.6),
            );
        }
        EffectType::ChromaticAberration => {
            define(
                "effect.chromatic.amount",
                "色差强度",
                PropertyValue::Float(0.0),
            );
        }
        EffectType::Grain => {
            define("effect.grain.amount", "颗粒强度", PropertyValue::Float(0.0));
            define("effect.grain.size", "颗粒尺寸", PropertyValue::Float(1.0));
        }
        EffectType::ChromaKey => {
            define(
                "effect.chroma.key_color",
                "抠像颜色",
                PropertyValue::Color(Color::from_hex(0x00FF00)),
            );
            define(
                "effect.chroma.similarity",
                "相似度",
                PropertyValue::Float(0.2),
            );
            define("effect.chroma.blend", "边缘混合", PropertyValue::Float(0.1));
        }
        EffectType::LumaKey => {
            define("effect.luma.threshold", "阈值", PropertyValue::Float(0.5));
            define("effect.luma.softness", "柔化", PropertyValue::Float(0.1));
        }
    }

    properties
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
