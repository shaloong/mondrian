//! Clip（时间线剪辑片段）

use crate::keyframe::KeyframeTrack;
use glam::Vec2;
use mondrian_core::{
    automation::{
        AnimatedProperty, Interpolatable, KeyframeTrack as SharedKeyframeTrack, PropertyBag,
        PropertyDescriptor, PropertyHost, PropertyMutation, PropertyValue,
    },
    types::*,
    MondrianError, Result,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

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
    pub position: KeyframeTrack<Vec2>,
    pub scale: KeyframeTrack<Vec2>,
    pub rotation: KeyframeTrack<f32>,
    pub anchor_point: KeyframeTrack<Vec2>,
    pub opacity: KeyframeTrack<f32>,
}

impl Transform2D {
    pub const POSITION_PATH: &'static str = "transform.position";
    pub const SCALE_PATH: &'static str = "transform.scale";
    pub const ROTATION_PATH: &'static str = "transform.rotation";
    pub const ANCHOR_POINT_PATH: &'static str = "transform.anchor_point";
    pub const OPACITY_PATH: &'static str = "transform.opacity";

    pub fn identity() -> Self {
        Self {
            position: KeyframeTrack::constant(Vec2::ZERO),
            scale: KeyframeTrack::constant(Vec2::ONE),
            rotation: KeyframeTrack::constant(0.0),
            anchor_point: KeyframeTrack::constant(Vec2::ZERO),
            opacity: KeyframeTrack::constant(1.0),
        }
    }

    /// 求值为 3x3 仿射变换矩阵（用于 GPU 渲染）
    pub fn evaluate_matrix(&self, time: TimeCode) -> glam::Mat3 {
        let pos = self.position.evaluate(time);
        let scale = self.scale.evaluate(time);
        let rot = self.rotation.evaluate(time).to_radians();

        let cos_r = rot.cos();
        let sin_r = rot.sin();

        glam::Mat3::from_cols(
            glam::Vec3::new(scale.x * cos_r, scale.x * sin_r, 0.0),
            glam::Vec3::new(-scale.y * sin_r, scale.y * cos_r, 0.0),
            glam::Vec3::new(pos.x, pos.y, 1.0),
        )
    }

    pub fn to_property_bag(&self) -> PropertyBag {
        let mut properties = PropertyBag::default();
        properties.upsert(animated_property_from_track(
            Self::POSITION_PATH,
            "位置",
            self.position.map(|value| PropertyValue::Vec2(*value)),
        ));
        properties.upsert(animated_property_from_track(
            Self::SCALE_PATH,
            "缩放",
            self.scale.map(|value| PropertyValue::Vec2(*value)),
        ));
        properties.upsert(animated_property_from_track(
            Self::ROTATION_PATH,
            "旋转",
            self.rotation.map(|value| PropertyValue::Float(*value)),
        ));
        properties.upsert(animated_property_from_track(
            Self::ANCHOR_POINT_PATH,
            "锚点",
            self.anchor_point.map(|value| PropertyValue::Vec2(*value)),
        ));
        properties.upsert(animated_property_from_track(
            Self::OPACITY_PATH,
            "不透明度",
            self.opacity.map(|value| PropertyValue::Float(*value)),
        ));
        properties
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

        let mut properties = self.to_property_bag();
        properties.apply_mutation(mutation)?;
        self.position =
            track_from_property(&properties, Self::POSITION_PATH, PropertyValue::as_vec2)?;
        self.scale = track_from_property(&properties, Self::SCALE_PATH, PropertyValue::as_vec2)?;
        self.rotation =
            track_from_property(&properties, Self::ROTATION_PATH, PropertyValue::as_f32)?;
        self.anchor_point =
            track_from_property(&properties, Self::ANCHOR_POINT_PATH, PropertyValue::as_vec2)?;
        self.opacity = track_from_property(&properties, Self::OPACITY_PATH, PropertyValue::as_f32)?;
        Ok(())
    }
}

/// 变速模式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SpeedMode {
    /// 恒定速度（1.0 = 正常速度）
    Constant(f64),
    /// 变速曲线（时间重映射）
    Keyframed(KeyframeTrack<f64>),
    /// 倒放
    Reverse,
}

impl SpeedMode {
    pub const MULTIPLIER_PATH: &'static str = "speed.multiplier";

    /// 给定时间线本地时间 → 素材源时间（帧偏移）
    pub fn map_time(&self, local_time: TimeCode) -> TimeCode {
        match self {
            Self::Constant(speed) => TimeCode::new(
                (local_time.frame as f64 * speed) as i64,
                local_time.time_base,
            ),
            Self::Keyframed(track) => {
                let speed_at = track.evaluate(local_time);
                TimeCode::new(
                    (local_time.frame as f64 * speed_at) as i64,
                    local_time.time_base,
                )
            }
            Self::Reverse => local_time,
        }
    }

    fn to_property_track(&self) -> SharedKeyframeTrack<PropertyValue> {
        match self {
            Self::Constant(speed) => SharedKeyframeTrack::constant(PropertyValue::Double(*speed)),
            Self::Keyframed(track) => track.map(|value| PropertyValue::Double(*value)),
            Self::Reverse => SharedKeyframeTrack::constant(PropertyValue::Double(-1.0)),
        }
    }

    fn apply_property_mutation(&mut self, mutation: PropertyMutation) -> Result<()> {
        let path = property_mutation_path(&mutation);
        if path != Self::MULTIPLIER_PATH {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "speed_apply_property_mutation".to_string(),
                reason: format!("SpeedMode 不支持属性路径: {path}"),
            });
        }

        if matches!(mutation, PropertyMutation::RemoveProperty { .. }) {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "speed_apply_property_mutation".to_string(),
                reason: "内建 speed.multiplier 属性不可移除".to_string(),
            });
        }

        let mut properties = PropertyBag::default();
        properties.upsert(animated_property_from_track(
            Self::MULTIPLIER_PATH,
            "速度倍数",
            self.to_property_track(),
        ));
        properties.apply_mutation(mutation)?;
        let track = track_from_property(&properties, Self::MULTIPLIER_PATH, PropertyValue::as_f64)?;
        *self = if track.is_animated() {
            SpeedMode::Keyframed(track)
        } else {
            SpeedMode::Constant(*track.static_value())
        };
        Ok(())
    }
}

/// 效果引用（指向效果系统中的节点）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectRef {
    pub effect_id: EffectId,
    pub is_enabled: bool,
}

/// 时间线上的一个剪辑片段
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
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
    pub speed: SpeedMode,
    /// 效果链
    pub effects: Vec<EffectRef>,
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
    pub fn new(asset_id: AssetId, position: TimeCode, duration: TimeCode) -> Self {
        let tb = position.time_base;
        Self {
            id: ClipId::new(),
            asset_id,
            position,
            duration,
            source_in: TimeCode::new(0, tb),
            source_out: duration,
            transform: Transform2D::identity(),
            speed: SpeedMode::Constant(1.0),
            effects: vec![],
            linked_clip: None,
            is_disabled: false,
            blend_mode: None,
            label: None,
        }
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
}

impl PropertyHost for Clip {
    fn property_bag(&self) -> Result<PropertyBag> {
        let mut properties = self.transform.to_property_bag();
        properties.upsert(animated_property_from_track(
            SpeedMode::MULTIPLIER_PATH,
            "速度倍数",
            self.speed.to_property_track(),
        ));
        Ok(properties)
    }

    fn apply_property_mutation(&mut self, mutation: PropertyMutation) -> Result<()> {
        let path = property_mutation_path(&mutation);
        if path.starts_with("transform.") {
            self.transform.apply_property_mutation(mutation)
        } else if path == SpeedMode::MULTIPLIER_PATH {
            self.speed.apply_property_mutation(mutation)
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
}

fn animated_property_from_track(
    path: impl Into<String>,
    display_name: impl Into<String>,
    track: SharedKeyframeTrack<PropertyValue>,
) -> AnimatedProperty {
    let path = path.into();
    AnimatedProperty {
        descriptor: PropertyDescriptor {
            path,
            display_name: display_name.into(),
            default_value: track.static_value().clone(),
            is_animatable: true,
        },
        track,
    }
}

fn track_from_property<T, F>(
    properties: &PropertyBag,
    path: &str,
    mut decode: F,
) -> Result<KeyframeTrack<T>>
where
    T: Interpolatable + Serialize + DeserializeOwned,
    F: FnMut(&PropertyValue) -> Option<T>,
{
    let property = properties.property(path).ok_or_else(|| MondrianError::WorkflowStepFailed {
        step_id: "track_from_property".to_string(),
        reason: format!("属性不存在: {path}"),
    })?;
    property.track.try_map(|value| {
        decode(value).ok_or_else(|| MondrianError::WorkflowStepFailed {
            step_id: "track_from_property".to_string(),
            reason: format!("属性类型无法转换: {path}"),
        })
    })
}

fn property_mutation_path(mutation: &PropertyMutation) -> &str {
    match mutation {
        PropertyMutation::DefineProperty(descriptor) => &descriptor.path,
        PropertyMutation::SetStaticValue { path, .. } => path,
        PropertyMutation::SetKeyframe { path, .. } => path,
        PropertyMutation::RemoveKeyframe { path, .. } => path,
        PropertyMutation::RemoveProperty { path } => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{
        automation::{InterpolationType, Keyframe, PropertyMutation, PropertyValue},
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
            keyframe: Keyframe {
                time: tc(0),
                value: PropertyValue::Vec2(Vec2::ZERO),
                interpolation: InterpolationType::Linear,
                control_in: None,
                control_out: None,
            },
        })
        .expect("set start position");
        clip.apply_property_mutation(PropertyMutation::SetKeyframe {
            path: Transform2D::POSITION_PATH.to_string(),
            keyframe: Keyframe {
                time: tc(20),
                value: PropertyValue::Vec2(Vec2::new(20.0, 10.0)),
                interpolation: InterpolationType::Linear,
                control_in: None,
                control_out: None,
            },
        })
        .expect("set end position");
        clip.apply_property_mutation(PropertyMutation::SetStaticValue {
            path: SpeedMode::MULTIPLIER_PATH.to_string(),
            value: PropertyValue::Double(1.5),
        })
        .expect("set speed multiplier");

        assert_eq!(
            clip.transform.position.evaluate(tc(10)),
            Vec2::new(10.0, 5.0)
        );
        match clip.speed {
            SpeedMode::Constant(speed) => assert!((speed - 1.5).abs() < f64::EPSILON),
            _ => panic!("expected constant speed"),
        }
    }
}
