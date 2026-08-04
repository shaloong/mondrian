//! Effect data types — pure data, no evaluation logic.
//!
//! These types define the data model for effects. The evaluation logic lives in
//! `mondrian-effects`. This separation allows `mondrian-timeline` to depend on
//! effect data types without depending on the full effect evaluation engine.

use crate::automation::{
    ParameterResourceReference, PropertyBag, PropertyDescriptor, PropertyValue,
};
use crate::types::EffectId;
use crate::{ParameterId, ParameterIdError, TimelineTime};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Supported effect types.
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

impl EffectType {
    pub fn key(&self) -> String {
        match self {
            Self::BasicCorrection => "builtin.basic_correction".to_string(),
            Self::WhiteBalance => "builtin.white_balance".to_string(),
            Self::Lut3D => "builtin.lut_3d".to_string(),
            Self::ColorWheel => "builtin.color_wheel".to_string(),
            Self::Curves => "builtin.curves".to_string(),
            Self::HueSaturationLightness => "builtin.hue_saturation_lightness".to_string(),
            Self::GaussianBlur => "builtin.gaussian_blur".to_string(),
            Self::Sharpen => "builtin.sharpen".to_string(),
            Self::Vignette => "builtin.vignette".to_string(),
            Self::ChromaticAberration => "builtin.chromatic_aberration".to_string(),
            Self::Grain => "builtin.grain".to_string(),
            Self::ChromaKey => "builtin.chroma_key".to_string(),
            Self::LumaKey => "builtin.luma_key".to_string(),
            Self::Plugin(key) => key.clone(),
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key {
            "builtin.basic_correction" => Self::BasicCorrection,
            "builtin.white_balance" => Self::WhiteBalance,
            "builtin.lut_3d" => Self::Lut3D,
            "builtin.color_wheel" => Self::ColorWheel,
            "builtin.curves" => Self::Curves,
            "builtin.hue_saturation_lightness" => Self::HueSaturationLightness,
            "builtin.gaussian_blur" => Self::GaussianBlur,
            "builtin.sharpen" => Self::Sharpen,
            "builtin.vignette" => Self::Vignette,
            "builtin.chromatic_aberration" => Self::ChromaticAberration,
            "builtin.grain" => Self::Grain,
            "builtin.chroma_key" => Self::ChromaKey,
            "builtin.luma_key" => Self::LumaKey,
            other => Self::Plugin(other.to_string()),
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            Self::BasicCorrection => "基础调色",
            Self::WhiteBalance => "白平衡",
            Self::Lut3D => "3D LUT",
            Self::ColorWheel => "色轮",
            Self::Curves => "曲线",
            Self::HueSaturationLightness => "色相/饱和度/亮度",
            Self::GaussianBlur => "高斯模糊",
            Self::Sharpen => "锐化",
            Self::Vignette => "暗角",
            Self::ChromaticAberration => "色差",
            Self::Grain => "颗粒",
            Self::ChromaKey => "色度抠像",
            Self::LumaKey => "亮度抠像",
            Self::Plugin(name) => name,
        }
    }

    pub fn category_path(&self) -> Vec<&str> {
        match self {
            Self::BasicCorrection
            | Self::WhiteBalance
            | Self::Lut3D
            | Self::ColorWheel
            | Self::Curves
            | Self::HueSaturationLightness => vec!["颜色", "调色"],
            Self::GaussianBlur | Self::Sharpen => vec!["颜色", "模糊与锐化"],
            Self::Vignette | Self::ChromaticAberration | Self::Grain => vec!["颜色", "风格化"],
            Self::ChromaKey | Self::LumaKey => vec!["抠像"],
            Self::Plugin(_) => vec!["插件"],
        }
    }

    pub fn property_namespace(&self) -> String {
        match self {
            Self::BasicCorrection => "basic_correction".to_string(),
            Self::WhiteBalance => "white_balance".to_string(),
            Self::Lut3D => "lut_3d".to_string(),
            Self::ColorWheel => "color_wheel".to_string(),
            Self::Curves => "curves".to_string(),
            Self::HueSaturationLightness => "hue_saturation_lightness".to_string(),
            Self::GaussianBlur => "gaussian_blur".to_string(),
            Self::Sharpen => "sharpen".to_string(),
            Self::Vignette => "vignette".to_string(),
            Self::ChromaticAberration => "chromatic_aberration".to_string(),
            Self::Grain => "grain".to_string(),
            Self::ChromaKey => "chroma_key".to_string(),
            Self::LumaKey => "luma_key".to_string(),
            Self::Plugin(key) => key.clone(),
        }
    }

    pub fn property_path(&self, parameter: &str) -> String {
        format!("effect.{}.{}", self.property_namespace(), parameter)
    }

    /// Build the stable parameter identity owned by this effect definition.
    pub fn parameter_id(&self, parameter: &str) -> Result<ParameterId, ParameterIdError> {
        ParameterId::new(format!("mondrian.effect.{}.{}", self.key(), parameter))
    }
}

/// A single effect instance on a clip.
///
/// The struct holds identity (id, type), animatable properties, and opaque
/// JSON params. Effect evaluation is performed by `mondrian-effects`, not by
/// methods on this struct.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Create a new effect node with an empty property bag.
    ///
    /// Product authoring must use the fallible `instantiate_effect_node()`
    /// boundary from `mondrian-effects`; this empty constructor is for
    /// definition/runtime assembly where registry availability is handled by
    /// the caller.
    pub fn new(effect_type: EffectType) -> Self {
        Self {
            id: EffectId::new(),
            properties: PropertyBag::default(),
            effect_type,
            params: serde_json::json!({}),
            is_enabled: true,
        }
    }

    /// Validate one effect instance's parameter schemas and stable identities.
    pub fn validate_author_state(&self) -> crate::Result<()> {
        self.properties.validate()?;
        let mut parameter_ids = HashSet::new();
        for (_, property) in self.properties.iter() {
            let parameter_id = property.descriptor.parameter_id();
            if !parameter_ids.insert(parameter_id.clone()) {
                return Err(crate::MondrianError::WorkflowStepFailed {
                    step_id: "effect_parameter_identity_validation".to_owned(),
                    reason: format!(
                        "Effect {} contains duplicate parameter identity {}",
                        self.id, parameter_id
                    ),
                });
            }
        }
        Ok(())
    }

    pub fn evaluate_property(&self, path: &str, time: TimelineTime) -> Option<PropertyValue> {
        self.properties.evaluate(path, time)
    }

    /// Evaluate one definition-stable parameter independent of instance address.
    pub fn evaluate_parameter(
        &self,
        parameter_id: &ParameterId,
        time: TimelineTime,
    ) -> Option<PropertyValue> {
        self.unique_parameter_property(parameter_id)
            .map(|(_, property)| property.evaluate(time))
    }

    pub fn define_property(&mut self, descriptor: PropertyDescriptor) {
        self.properties.define(descriptor);
    }

    /// Evaluate one floating parameter by stable schema identity.
    pub fn evaluate_f32_parameter(
        &self,
        parameter_id: &ParameterId,
        time: TimelineTime,
        fallback: f32,
    ) -> f32 {
        self.evaluate_parameter(parameter_id, time)
            .and_then(|value| value.as_f32())
            .unwrap_or(fallback)
    }

    /// Namespace all property paths with an effect instance prefix.
    /// Called when an effect is placed on a clip to avoid path collisions.
    ///
    /// Not idempotent: calling twice double-prefixes property paths.
    /// Callers must ensure this is called exactly once per clip placement.
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

    /// Evaluate one non-empty text parameter by stable schema identity.
    pub fn evaluate_text_parameter(
        &self,
        parameter_id: &ParameterId,
        time: TimelineTime,
    ) -> Option<String> {
        self.evaluate_parameter(parameter_id, time).and_then(|value| match value {
            PropertyValue::Text(text) if !text.trim().is_empty() => Some(text),
            _ => None,
        })
    }

    /// Evaluate one stable enum key by definition-stable parameter identity.
    pub fn evaluate_enum_parameter(
        &self,
        parameter_id: &ParameterId,
        time: TimelineTime,
    ) -> Option<String> {
        self.evaluate_parameter(parameter_id, time).and_then(|value| match value {
            PropertyValue::Enum(key) => Some(key),
            _ => None,
        })
    }

    /// Evaluate a typed resource reference by stable schema identity.
    pub fn evaluate_resource_parameter(
        &self,
        parameter_id: &ParameterId,
        time: TimelineTime,
    ) -> Option<ParameterResourceReference> {
        self.evaluate_parameter(parameter_id, time).and_then(|value| match value {
            PropertyValue::Resource(reference) => Some(reference),
            _ => None,
        })
    }

    /// Mutate one property selected by definition-stable parameter identity.
    pub fn set_static_value_by_parameter(
        &mut self,
        parameter_id: &ParameterId,
        value: PropertyValue,
    ) -> crate::Result<()> {
        let path = self
            .unique_parameter_property(parameter_id)
            .map(|(path, _)| path.to_string())
            .ok_or_else(|| crate::MondrianError::WorkflowStepFailed {
            step_id: "effect_set_static_value".to_string(),
            reason: format!("effect parameter does not exist: {parameter_id}"),
        })?;
        self.properties.set_static_value(&path, value)
    }

    fn unique_parameter_property(
        &self,
        parameter_id: &ParameterId,
    ) -> Option<(&str, &crate::automation::AnimatedProperty)> {
        let mut matches = self
            .properties
            .iter()
            .filter(|(_, property)| property.descriptor.parameter_id() == parameter_id);
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    }
}

impl crate::automation::PropertyHost for EffectNode {
    fn property_bag(&self) -> crate::Result<PropertyBag> {
        Ok(self.properties.clone())
    }

    fn apply_property_mutation(
        &mut self,
        mutation: crate::automation::PropertyMutation,
    ) -> crate::Result<()> {
        self.properties.apply_mutation(mutation)
    }
}

/// Build a namespaced property path scoped to a single effect instance.
pub fn namespaced_effect_path(effect_id: EffectId, path: &str) -> String {
    if let Some(rest) = path.strip_prefix("effect.") {
        format!("effect.{effect_id}.{rest}")
    } else {
        format!("effect.{effect_id}.{path}")
    }
}

/// Parse an [`EffectId`] from a namespaced property path.
///
/// Effect property paths have the form `effect.<UUID>.<rest>`. This function
/// extracts the UUID segment and parses it into an `EffectId`.
///
/// Returns `None` if the path does not start with `effect.` or the second
/// segment is not a valid UUID.
pub fn parse_effect_id_from_property_path(path: &str) -> Option<EffectId> {
    let mut segments = path.split('.');
    if segments.next()? != "effect" {
        return None;
    }
    let id_raw = segments.next()?;
    uuid::Uuid::parse_str(id_raw).ok().map(EffectId)
}

impl crate::AuthoringFootprint for EffectType {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        match self {
            Self::Plugin(key) => collector.collect(key),
            Self::BasicCorrection
            | Self::WhiteBalance
            | Self::Lut3D
            | Self::ColorWheel
            | Self::Curves
            | Self::HueSaturationLightness
            | Self::GaussianBlur
            | Self::Sharpen
            | Self::Vignette
            | Self::ChromaticAberration
            | Self::Grain
            | Self::ChromaKey
            | Self::LumaKey => Ok(()),
        }
    }
}

impl crate::AuthoringFootprint for EffectNode {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self {
            id: _,
            effect_type,
            properties,
            params,
            is_enabled: _,
        } = self;
        collector.collect(effect_type)?;
        collector.collect(properties)?;
        collector.collect(params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_effect_id_from_property_path_extracts_valid_uuid() {
        let id = EffectId::new();
        let path = format!("effect.{}.some.parameter", id);
        let parsed = parse_effect_id_from_property_path(&path);
        assert_eq!(parsed, Some(id));
    }

    #[test]
    fn parse_effect_id_from_property_path_rejects_non_effect_prefix() {
        assert_eq!(
            parse_effect_id_from_property_path("transform.position"),
            None
        );
    }

    #[test]
    fn parse_effect_id_from_property_path_rejects_invalid_uuid() {
        assert_eq!(
            parse_effect_id_from_property_path("effect.not-a-uuid.param"),
            None
        );
    }

    #[test]
    fn parse_effect_id_from_property_path_rejects_too_few_segments() {
        assert_eq!(parse_effect_id_from_property_path("effect"), None);
        assert_eq!(
            parse_effect_id_from_property_path("effect.a1b2c3d4e5f6"),
            None
        );
    }
}
