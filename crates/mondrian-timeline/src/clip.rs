//! Clip（时间线剪辑片段）

use glam::Vec2;
use mondrian_core::{
    automation::{
        timecode_to_ticks, AnimatedProperty, PropertyBag, PropertyDescriptor, PropertyHost,
        PropertyMutation, PropertyValue,
    },
    types::*,
    MondrianError, Result,
};
use mondrian_effects::{evaluate_effect_stack, AdjustmentLayerParams, EffectNode, EffectType};
use serde::{Deserialize, Serialize};

/// 裁剪边缘
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimEdge {
    /// 裁剪入点（左侧）
    In,
    /// 裁剪出点（右侧）
    Out,
}

/// 2D 变换（位置 / 缩放 / 旋转 / 锚点 / 不透明度），所有属性可关键帧动画
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transform2D {
    properties: PropertyBag,
}

impl Transform2D {
    pub const POSITION_PATH: &'static str = "transform.position";
    pub const SCALE_PATH: &'static str = "transform.scale";
    pub const ROTATION_PATH: &'static str = "transform.rotation";
    pub const ANCHOR_POINT_PATH: &'static str = "transform.anchor_point";
    pub const OPACITY_PATH: &'static str = "transform.opacity";

    pub fn identity() -> Self {
        let mut properties = PropertyBag::default();
        properties.define(PropertyDescriptor::new(
            Self::POSITION_PATH,
            "位置",
            PropertyValue::Vec2(Vec2::ZERO),
        ));
        properties.define(PropertyDescriptor::new(
            Self::SCALE_PATH,
            "缩放",
            PropertyValue::Vec2(Vec2::ONE),
        ));
        properties.define(PropertyDescriptor::new(
            Self::ROTATION_PATH,
            "旋转",
            PropertyValue::Float(0.0),
        ));
        properties.define(PropertyDescriptor::new(
            Self::ANCHOR_POINT_PATH,
            "锚点",
            PropertyValue::Vec2(Vec2::ZERO),
        ));
        properties.define(PropertyDescriptor::new(
            Self::OPACITY_PATH,
            "不透明度",
            PropertyValue::Float(1.0),
        ));
        Self { properties }
    }

    /// 求值为 3x3 仿射变换矩阵（用于 GPU 渲染）
    pub fn evaluate_matrix(&self, time: TimeCode) -> glam::Mat3 {
        let pos = self.evaluate_vec2(Self::POSITION_PATH, time);
        let scale = self.evaluate_vec2(Self::SCALE_PATH, time);
        let rot = self.evaluate_f32(Self::ROTATION_PATH, time).to_radians();

        let cos_r = rot.cos();
        let sin_r = rot.sin();

        glam::Mat3::from_cols(
            glam::Vec3::new(scale.x * cos_r, scale.x * sin_r, 0.0),
            glam::Vec3::new(-scale.y * sin_r, scale.y * cos_r, 0.0),
            glam::Vec3::new(pos.x, pos.y, 1.0),
        )
    }

    pub fn evaluate_opacity(&self, time: TimeCode) -> f32 {
        self.evaluate_f32(Self::OPACITY_PATH, time)
    }

    pub fn to_property_bag(&self) -> PropertyBag {
        self.properties.clone()
    }

    pub fn apply_property_mutation(&mut self, mutation: PropertyMutation) -> Result<()> {
        let path = property_mutation_path(&mutation);
        if !path.starts_with("transform.") {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "transform_apply_property_mutation".to_string(),
                reason: format!("Transform2D 不支持属性路径: {path}"),
            });
        }

        if matches!(mutation, PropertyMutation::RemoveProperty { .. }) {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "transform_apply_property_mutation".to_string(),
                reason: "内建 transform 属性不可移除".to_string(),
            });
        }

        self.properties.apply_mutation(mutation)
    }

    fn evaluate_vec2(&self, path: &str, time: TimeCode) -> Vec2 {
        self.properties
            .evaluate(path, timecode_to_ticks(time))
            .and_then(|value| value.as_vec2())
            .unwrap_or(Vec2::ZERO)
    }

    fn evaluate_f32(&self, path: &str, time: TimeCode) -> f32 {
        self.properties
            .evaluate(path, timecode_to_ticks(time))
            .and_then(|value| value.as_f32())
            .unwrap_or(0.0)
    }
}

fn blend_mode_to_text(mode: Option<BlendMode>) -> String {
    match mode {
        None => "inherit".to_string(),
        Some(BlendMode::Normal) => "Normal".to_string(),
        Some(BlendMode::Multiply) => "Multiply".to_string(),
        Some(BlendMode::Screen) => "Screen".to_string(),
        Some(BlendMode::Overlay) => "Overlay".to_string(),
        Some(BlendMode::Darken) => "Darken".to_string(),
        Some(BlendMode::Lighten) => "Lighten".to_string(),
        Some(BlendMode::ColorDodge) => "ColorDodge".to_string(),
        Some(BlendMode::ColorBurn) => "ColorBurn".to_string(),
        Some(BlendMode::HardLight) => "HardLight".to_string(),
        Some(BlendMode::SoftLight) => "SoftLight".to_string(),
        Some(BlendMode::Difference) => "Difference".to_string(),
        Some(BlendMode::Exclusion) => "Exclusion".to_string(),
        Some(BlendMode::Add) => "Add".to_string(),
        Some(BlendMode::Subtract) => "Subtract".to_string(),
    }
}

fn blend_mode_from_text(value: &str) -> Result<Option<BlendMode>> {
    Ok(match value {
        "inherit" => None,
        "Multiply" => Some(BlendMode::Multiply),
        "Screen" => Some(BlendMode::Screen),
        "Overlay" => Some(BlendMode::Overlay),
        "Darken" => Some(BlendMode::Darken),
        "Lighten" => Some(BlendMode::Lighten),
        "ColorDodge" => Some(BlendMode::ColorDodge),
        "ColorBurn" => Some(BlendMode::ColorBurn),
        "HardLight" => Some(BlendMode::HardLight),
        "SoftLight" => Some(BlendMode::SoftLight),
        "Difference" => Some(BlendMode::Difference),
        "Exclusion" => Some(BlendMode::Exclusion),
        "Add" => Some(BlendMode::Add),
        "Subtract" => Some(BlendMode::Subtract),
        "Normal" => Some(BlendMode::Normal),
        other => {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "blend_mode_parse".to_string(),
                reason: format!("无法解析混合模式: {other}"),
            });
        }
    })
}

/// 变速曲线（当前以速度倍数属性驱动，可扩展到更复杂时间重映射）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeedMap {
    multiplier: AnimatedProperty,
}

impl SpeedMap {
    pub const MULTIPLIER_PATH: &'static str = "speed.multiplier";

    pub fn new() -> Self {
        Self {
            multiplier: AnimatedProperty::from_descriptor(PropertyDescriptor::new(
                Self::MULTIPLIER_PATH,
                "速度倍数",
                PropertyValue::Double(1.0),
            )),
        }
    }

    pub fn property(&self) -> &AnimatedProperty {
        &self.multiplier
    }

    pub fn evaluate_multiplier(&self, local_time: TimeCode) -> f64 {
        self.multiplier.evaluate(timecode_to_ticks(local_time)).as_f64().unwrap_or(1.0)
    }

    /// 给定时间线本地时间 → 素材源时间（帧偏移）
    pub fn map_time(&self, local_time: TimeCode) -> TimeCode {
        let speed = self.evaluate_multiplier(local_time);
        TimeCode::new(
            (local_time.frame as f64 * speed) as i64,
            local_time.time_base,
        )
    }

    fn apply_property_mutation(&mut self, mutation: PropertyMutation) -> Result<()> {
        let path = property_mutation_path(&mutation);
        if path != Self::MULTIPLIER_PATH {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "speed_apply_property_mutation".to_string(),
                reason: format!("SpeedMap 不支持属性路径: {path}"),
            });
        }

        if matches!(mutation, PropertyMutation::RemoveProperty { .. }) {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "speed_apply_property_mutation".to_string(),
                reason: "内建 speed.multiplier 属性不可移除".to_string(),
            });
        }

        self.multiplier.apply_mutation(mutation)
    }
}

/// 时间线片段语义
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ClipKind {
    #[default]
    Media,
    AdjustmentLayer,
}

/// 时间线上的一个剪辑片段
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    /// 片段语义（媒体/调整图层）
    #[serde(default)]
    pub kind: ClipKind,
    /// 关联的素材资产
    pub asset_id: AssetId,
    /// 在时间线上的起始位置
    pub position: TimeCode,
    /// 在时间线上的持续时长
    pub duration: TimeCode,
    /// 素材内入点
    pub source_in: TimeCode,
    /// 素材内出点（= source_in + duration / speed）
    pub source_out: TimeCode,
    /// 2D 变换（关键帧）
    pub transform: Transform2D,
    /// 变速模式
    pub speed: SpeedMap,
    /// 效果链（实例级，属性路径已按 effect id 做命名空间隔离）
    #[serde(default)]
    pub effects: Vec<EffectNode>,
    /// 关联的音频/视频 Clip（保持同步）
    pub linked_clip: Option<ClipId>,
    /// 是否禁用
    pub is_disabled: bool,
    /// 混合模式（覆盖轨道设置）
    pub blend_mode: Option<BlendMode>,
    /// 显示标签（可选）
    pub label: Option<String>,
}

impl Clip {
    pub const BLEND_MODE_PATH: &'static str = "clip.blend_mode";

    pub fn new(asset_id: AssetId, position: TimeCode, duration: TimeCode) -> Self {
        let tb = position.time_base;
        Self {
            id: ClipId::new(),
            kind: ClipKind::Media,
            asset_id,
            position,
            duration,
            source_in: TimeCode::new(0, tb),
            source_out: duration,
            transform: Transform2D::identity(),
            speed: SpeedMap::new(),
            effects: vec![],
            linked_clip: None,
            is_disabled: false,
            blend_mode: None,
            label: None,
        }
    }

    pub fn new_adjustment_layer(asset_id: AssetId, position: TimeCode, duration: TimeCode) -> Self {
        let mut clip = Self::new(asset_id, position, duration);
        clip.kind = ClipKind::AdjustmentLayer;
        clip.source_in = TimeCode::new(0, position.time_base);
        clip.source_out = duration;
        clip.label = Some("调整图层".to_string());
        clip
    }

    pub fn is_adjustment_layer(&self) -> bool {
        self.kind == ClipKind::AdjustmentLayer
    }

    /// Clip 在时间线上的结束位置
    pub fn end_position(&self) -> TimeCode {
        self.position + self.duration
    }

    /// 判断给定时间码是否在此 Clip 范围内
    pub fn contains(&self, time: TimeCode) -> bool {
        time >= self.position && time < self.end_position()
    }

    /// 将时间线时间 → Clip 内本地时间 → 素材源时间
    pub fn timeline_to_source_time(&self, timeline_time: TimeCode) -> TimeCode {
        let local = timeline_time - self.position;
        let source_local = self.speed.map_time(local);
        self.source_in + source_local
    }

    pub fn evaluate_effect_params(&self, time: TimeCode) -> AdjustmentLayerParams {
        evaluate_effect_stack(&self.effects, time).adjustment
    }

    pub fn add_effect(&mut self, effect_type: EffectType) -> EffectId {
        let label = self.next_effect_group_label(&effect_type);
        let mut effect = EffectNode::new(effect_type);
        effect.instantiate_for_clip(label);
        let effect_id = effect.id;
        self.effects.push(effect);
        effect_id
    }

    pub fn effect_enabled(&self, effect_id: EffectId) -> Option<bool> {
        self.effects
            .iter()
            .find(|effect| effect.id == effect_id)
            .map(|effect| effect.is_enabled)
    }

    pub fn effect_property_path(&self, suffix: &str) -> Option<String> {
        self.effects
            .iter()
            .flat_map(|effect| effect.properties.iter())
            .find(|(path, _)| path.ends_with(suffix))
            .map(|(path, _)| path.to_string())
    }

    pub fn set_effect_enabled(&mut self, effect_id: EffectId, enabled: bool) -> Result<()> {
        let effect =
            self.effects.iter_mut().find(|effect| effect.id == effect_id).ok_or_else(|| {
                MondrianError::WorkflowStepFailed {
                    step_id: "clip_set_effect_enabled".to_string(),
                    reason: format!("效果不存在: {effect_id}"),
                }
            })?;
        effect.is_enabled = enabled;
        Ok(())
    }

    pub fn remove_effect(&mut self, effect_id: EffectId) -> Result<()> {
        let index =
            self.effects.iter().position(|effect| effect.id == effect_id).ok_or_else(|| {
                MondrianError::WorkflowStepFailed {
                    step_id: "clip_remove_effect".to_string(),
                    reason: format!("效果不存在: {effect_id}"),
                }
            })?;
        self.effects.remove(index);
        Ok(())
    }

    fn next_effect_group_label(&self, effect_type: &EffectType) -> String {
        let base = effect_type.display_name();
        let count = self.effects.iter().filter(|effect| effect.effect_type == *effect_type).count();
        if count == 0 {
            base.to_string()
        } else {
            format!("{base} {}", count + 1)
        }
    }
}

impl PropertyHost for Clip {
    fn property_bag(&self) -> Result<PropertyBag> {
        let mut properties = if self.is_adjustment_layer() {
            let mut bag = PropertyBag::default();
            if let Some(opacity) =
                self.transform.to_property_bag().property(Transform2D::OPACITY_PATH).cloned()
            {
                bag.upsert(opacity);
            }
            bag
        } else {
            self.transform.to_property_bag()
        };
        for effect in &self.effects {
            for (_, property) in effect.property_bag()?.iter() {
                properties.upsert(property.clone());
            }
        }
        let blend_mode_text = blend_mode_to_text(self.blend_mode);
        let mut blend_mode_descriptor = PropertyDescriptor::new(
            Self::BLEND_MODE_PATH,
            "混合模式",
            PropertyValue::Text(blend_mode_text.clone()),
        );
        blend_mode_descriptor.is_animatable = false;
        let mut blend_mode_property = AnimatedProperty::from_descriptor(blend_mode_descriptor);
        blend_mode_property.set_static_value(PropertyValue::Text(blend_mode_text));
        properties.upsert(blend_mode_property);
        if !self.is_adjustment_layer() {
            properties.upsert(self.speed.property().clone());
        }
        Ok(properties)
    }

    fn apply_property_mutation(&mut self, mutation: PropertyMutation) -> Result<()> {
        let path = property_mutation_path(&mutation);
        if path == Transform2D::OPACITY_PATH
            || (!self.is_adjustment_layer() && path.starts_with("transform."))
        {
            self.transform.apply_property_mutation(mutation)
        } else if path == Self::BLEND_MODE_PATH {
            if matches!(mutation, PropertyMutation::RemoveProperty { .. }) {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "clip_apply_property_mutation".to_string(),
                    reason: "内建 clip.blend_mode 属性不可移除".to_string(),
                });
            }

            let mut properties = self.property_bag()?;
            properties.apply_mutation(mutation)?;
            let property = properties.property(Self::BLEND_MODE_PATH).ok_or_else(|| {
                MondrianError::WorkflowStepFailed {
                    step_id: "clip_apply_property_mutation".to_string(),
                    reason: "缺少 clip.blend_mode 属性".to_string(),
                }
            })?;
            let PropertyValue::Text(value) = property.static_value() else {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "clip_apply_property_mutation".to_string(),
                    reason: "clip.blend_mode 需要 text 值".to_string(),
                });
            };
            self.blend_mode = blend_mode_from_text(value)?;
            Ok(())
        } else if !self.is_adjustment_layer() && path == SpeedMap::MULTIPLIER_PATH {
            self.speed.apply_property_mutation(mutation)
        } else if path.starts_with("effect.") {
            if let Some(effect) = self.effects.iter_mut().find(|effect| {
                effect.property_bag().ok().is_some_and(|bag| bag.property(path).is_some())
            }) {
                effect.apply_property_mutation(mutation)
            } else {
                Err(MondrianError::WorkflowStepFailed {
                    step_id: "clip_apply_property_mutation".to_string(),
                    reason: format!("当前 Clip 不支持属性路径: {path}"),
                })
            }
        } else {
            Err(MondrianError::WorkflowStepFailed {
                step_id: "clip_apply_property_mutation".to_string(),
                reason: format!("当前 Clip 不支持属性路径: {path}"),
            })
        }
    }
}

/// 某时刻激活的 Clip（用于渲染请求）
#[derive(Debug, Clone)]
pub struct ActiveClip {
    pub clip: Clip,
    pub track_index: usize,
    /// 此时刻对应的素材源时间（用于解码）
    pub source_time: TimeCode,
    /// Transform 矩阵（已在此时刻求值）
    pub transform_matrix: glam::Mat3,
    /// 不透明度（已在此时刻求值）
    pub opacity: f32,
    /// 生效后的混合模式（clip 覆盖轨道，否则继承轨道）
    pub blend_mode: BlendMode,
}

fn property_mutation_path(mutation: &PropertyMutation) -> &str {
    mutation.path()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{
        automation::{timecode_to_ticks, Keyframe, PropertyMutation, PropertyValue},
        types::Rational,
    };

    fn tc(frame: i64) -> TimeCode {
        TimeCode::new(frame, Rational::new(1, 25))
    }

    #[test]
    fn clip_property_mutation_updates_transform_and_speed() {
        let mut clip = Clip::new(AssetId::new(), tc(0), tc(40));
        clip.apply_property_mutation(PropertyMutation::SetKeyframe {
            path: Transform2D::POSITION_PATH.to_string(),
            keyframe: Keyframe::linear(timecode_to_ticks(tc(0)), PropertyValue::Vec2(Vec2::ZERO)),
        })
        .expect("set start position");
        clip.apply_property_mutation(PropertyMutation::SetKeyframe {
            path: Transform2D::POSITION_PATH.to_string(),
            keyframe: Keyframe::linear(
                timecode_to_ticks(tc(20)),
                PropertyValue::Vec2(Vec2::new(20.0, 10.0)),
            ),
        })
        .expect("set end position");
        clip.apply_property_mutation(PropertyMutation::SetStaticValue {
            path: SpeedMap::MULTIPLIER_PATH.to_string(),
            value: PropertyValue::Double(1.5),
        })
        .expect("set speed multiplier");

        let position = clip
            .transform
            .to_property_bag()
            .evaluate(Transform2D::POSITION_PATH, timecode_to_ticks(tc(10)))
            .and_then(|value| value.as_vec2())
            .expect("evaluate position");
        assert_eq!(position, Vec2::new(10.0, 5.0));
        assert!((clip.speed.evaluate_multiplier(tc(10)) - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn clip_property_bag_exposes_blend_mode_as_static_property() {
        let clip = Clip::new(AssetId::new(), tc(0), tc(40));
        let bag = clip.property_bag().expect("property bag should build");
        let property =
            bag.property(Clip::BLEND_MODE_PATH).expect("blend mode property should exist");

        assert!(!property.descriptor.is_animatable);
        assert_eq!(
            property.evaluate(timecode_to_ticks(tc(0))),
            PropertyValue::Text("inherit".to_string())
        );
    }

    #[test]
    fn clip_property_mutation_updates_blend_mode_without_keyframes() {
        let mut clip = Clip::new(AssetId::new(), tc(0), tc(40));

        clip.apply_property_mutation(PropertyMutation::SetStaticValue {
            path: Clip::BLEND_MODE_PATH.to_string(),
            value: PropertyValue::Text("Multiply".to_string()),
        })
        .expect("set blend mode");

        assert_eq!(clip.blend_mode, Some(BlendMode::Multiply));
    }

    #[test]
    fn adjustment_layer_constructor_marks_clip_kind() {
        let clip = Clip::new_adjustment_layer(AssetId::new(), tc(12), tc(30));

        assert!(clip.is_adjustment_layer());
        assert_eq!(clip.kind, ClipKind::AdjustmentLayer);
        assert_eq!(clip.position, tc(12));
        assert_eq!(clip.duration, tc(30));
        assert!(clip.effects.is_empty());
    }

    #[test]
    fn adjustment_layer_instances_from_same_asset_do_not_share_state() {
        let shared_asset_id = AssetId::new();
        let mut first = Clip::new_adjustment_layer(shared_asset_id, tc(0), tc(30));
        let mut second = Clip::new_adjustment_layer(shared_asset_id, tc(40), tc(30));
        first.add_effect(EffectType::BasicCorrection);
        second.add_effect(EffectType::BasicCorrection);
        let exposure_path =
            first.effect_property_path("basic.exposure").expect("adjustment exposure path");

        first
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: exposure_path,
                value: PropertyValue::Float(1.25),
            })
            .expect("set first exposure");

        let first_exposure = first.evaluate_effect_params(tc(10)).exposure;
        let second_exposure = second.evaluate_effect_params(tc(50)).exposure;

        assert!((first_exposure - 1.25).abs() < 1.0e-4);
        assert!(second_exposure.abs() < 1.0e-4);
    }
}
