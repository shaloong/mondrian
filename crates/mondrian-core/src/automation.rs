//! 自动化 / 关键帧基础设施
//!
//! 提供共享的关键帧轨道、动态属性值、属性包和 mutation API，
//! 让内建属性、效果参数和未来插件属性走同一条链路。

use crate::{
    error::{MondrianError, Result},
    types::{Color, TimeCode},
};
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum InterpolationType {
    Hold,
    #[default]
    Linear,
    Bezier,
    EaseIn,
    EaseOut,
    EaseInOut,
}

pub trait Interpolatable: Clone + std::fmt::Debug + Send + Sync {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self;
}

impl Interpolatable for f32 {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        a + (b - a) * t
    }
}

impl Interpolatable for f64 {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        a + (b - a) * t as f64
    }
}

impl Interpolatable for Vec2 {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        Vec2::lerp(*a, *b, t)
    }
}

impl Interpolatable for Vec3 {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        Vec3::lerp(*a, *b, t)
    }
}

impl Interpolatable for [f32; 4] {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        [
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
            a[3] + (b[3] - a[3]) * t,
        ]
    }
}

impl Interpolatable for Color {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        a.lerp(*b, t)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PropertyValue {
    Bool(bool),
    Int(i64),
    Float(f32),
    Double(f64),
    Vec2(Vec2),
    Vec3(Vec3),
    Color(Color),
    Vec4([f32; 4]),
    Text(String),
}

impl PropertyValue {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Bool(_) => "bool",
            Self::Int(_) => "int",
            Self::Float(_) => "float",
            Self::Double(_) => "double",
            Self::Vec2(_) => "vec2",
            Self::Vec3(_) => "vec3",
            Self::Color(_) => "color",
            Self::Vec4(_) => "vec4",
            Self::Text(_) => "text",
        }
    }

    pub fn same_variant(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Self::Float(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Double(value) => Some(*value),
            Self::Float(value) => Some(*value as f64),
            _ => None,
        }
    }

    pub fn as_vec2(&self) -> Option<Vec2> {
        match self {
            Self::Vec2(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_vec3(&self) -> Option<Vec3> {
        match self {
            Self::Vec3(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_color(&self) -> Option<Color> {
        match self {
            Self::Color(value) => Some(*value),
            _ => None,
        }
    }
}

impl Interpolatable for PropertyValue {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        match (a, b) {
            (Self::Float(a), Self::Float(b)) => Self::Float(f32::lerp(a, b, t)),
            (Self::Double(a), Self::Double(b)) => Self::Double(f64::lerp(a, b, t)),
            (Self::Int(a), Self::Int(b)) => {
                Self::Int((*a as f64 + (*b - *a) as f64 * t as f64).round() as i64)
            }
            (Self::Vec2(a), Self::Vec2(b)) => Self::Vec2(Vec2::lerp(*a, *b, t)),
            (Self::Vec3(a), Self::Vec3(b)) => Self::Vec3(Vec3::lerp(*a, *b, t)),
            (Self::Color(a), Self::Color(b)) => Self::Color(a.lerp(*b, t)),
            (Self::Vec4(a), Self::Vec4(b)) => Self::Vec4(<[f32; 4]>::lerp(a, b, t)),
            _ => a.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keyframe<T> {
    pub time: TimeCode,
    pub value: T,
    pub interpolation: InterpolationType,
    pub control_in: Option<Vec2>,
    pub control_out: Option<Vec2>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyframeTrack<T: Interpolatable> {
    keyframes: Vec<Keyframe<T>>,
    static_value: T,
}

impl<T: Interpolatable + Serialize + for<'de> Deserialize<'de>> KeyframeTrack<T> {
    pub fn constant(value: T) -> Self {
        Self { keyframes: vec![], static_value: value }
    }

    pub fn from_parts(static_value: T, mut keyframes: Vec<Keyframe<T>>) -> Self {
        keyframes.sort_by_key(|keyframe| keyframe.time);
        Self { keyframes, static_value }
    }

    pub fn static_value(&self) -> &T {
        &self.static_value
    }

    pub fn set_static_value(&mut self, value: T) {
        self.static_value = value;
    }

    pub fn keyframes(&self) -> &[Keyframe<T>] {
        &self.keyframes
    }

    pub fn map<U, F>(&self, mut map_value: F) -> KeyframeTrack<U>
    where
        U: Interpolatable + Serialize + for<'de> Deserialize<'de>,
        F: FnMut(&T) -> U,
    {
        KeyframeTrack::from_parts(
            map_value(&self.static_value),
            self.keyframes
                .iter()
                .map(|keyframe| Keyframe {
                    time: keyframe.time,
                    value: map_value(&keyframe.value),
                    interpolation: keyframe.interpolation,
                    control_in: keyframe.control_in,
                    control_out: keyframe.control_out,
                })
                .collect(),
        )
    }

    pub fn try_map<U, E, F>(&self, mut map_value: F) -> std::result::Result<KeyframeTrack<U>, E>
    where
        U: Interpolatable + Serialize + for<'de> Deserialize<'de>,
        F: FnMut(&T) -> std::result::Result<U, E>,
    {
        let static_value = map_value(&self.static_value)?;
        let mut keyframes = Vec::with_capacity(self.keyframes.len());
        for keyframe in &self.keyframes {
            keyframes.push(Keyframe {
                time: keyframe.time,
                value: map_value(&keyframe.value)?,
                interpolation: keyframe.interpolation,
                control_in: keyframe.control_in,
                control_out: keyframe.control_out,
            });
        }
        Ok(KeyframeTrack::from_parts(static_value, keyframes))
    }

    pub fn evaluate(&self, time: TimeCode) -> T {
        if self.keyframes.is_empty() {
            return self.static_value.clone();
        }

        if time <= self.keyframes.first().expect("non-empty keyframes").time {
            return self.keyframes.first().expect("non-empty keyframes").value.clone();
        }
        if time >= self.keyframes.last().expect("non-empty keyframes").time {
            return self.keyframes.last().expect("non-empty keyframes").value.clone();
        }

        let idx = self
            .keyframes
            .partition_point(|keyframe| keyframe.time <= time)
            .saturating_sub(1);
        let keyframe_a = &self.keyframes[idx];
        let keyframe_b = &self.keyframes[idx + 1];
        let t = normalize_time(time, keyframe_a.time, keyframe_b.time);

        match keyframe_a.interpolation {
            InterpolationType::Hold => keyframe_a.value.clone(),
            InterpolationType::Linear => T::lerp(&keyframe_a.value, &keyframe_b.value, t),
            InterpolationType::Bezier => {
                let t_bezier = solve_bezier_t(t, keyframe_a.control_out, keyframe_b.control_in);
                T::lerp(&keyframe_a.value, &keyframe_b.value, t_bezier)
            }
            InterpolationType::EaseIn => T::lerp(&keyframe_a.value, &keyframe_b.value, ease_in(t)),
            InterpolationType::EaseOut => {
                T::lerp(&keyframe_a.value, &keyframe_b.value, ease_out(t))
            }
            InterpolationType::EaseInOut => {
                T::lerp(&keyframe_a.value, &keyframe_b.value, ease_in_out(t))
            }
        }
    }

    pub fn set_keyframe(&mut self, keyframe: Keyframe<T>) {
        let pos = self.keyframes.partition_point(|candidate| candidate.time < keyframe.time);
        if pos < self.keyframes.len() && self.keyframes[pos].time == keyframe.time {
            self.keyframes[pos] = keyframe;
        } else {
            self.keyframes.insert(pos, keyframe);
        }
    }

    pub fn remove_keyframe(&mut self, time: TimeCode) -> Option<Keyframe<T>> {
        if let Some(pos) = self.keyframes.iter().position(|keyframe| keyframe.time == time) {
            Some(self.keyframes.remove(pos))
        } else {
            None
        }
    }

    pub fn keyframe_count(&self) -> usize {
        self.keyframes.len()
    }

    pub fn is_animated(&self) -> bool {
        !self.keyframes.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyDescriptor {
    pub path: String,
    pub display_name: String,
    pub default_value: PropertyValue,
    #[serde(default = "default_property_animatable")]
    pub is_animatable: bool,
}

const fn default_property_animatable() -> bool {
    true
}

impl PropertyDescriptor {
    pub fn new(
        path: impl Into<String>,
        display_name: impl Into<String>,
        default_value: PropertyValue,
    ) -> Self {
        Self {
            path: path.into(),
            display_name: display_name.into(),
            default_value,
            is_animatable: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimatedProperty {
    pub descriptor: PropertyDescriptor,
    pub track: KeyframeTrack<PropertyValue>,
}

impl AnimatedProperty {
    pub fn from_descriptor(descriptor: PropertyDescriptor) -> Self {
        Self {
            track: KeyframeTrack::constant(descriptor.default_value.clone()),
            descriptor,
        }
    }

    pub fn evaluate(&self, time: TimeCode) -> PropertyValue {
        self.track.evaluate(time)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PropertyBag {
    properties: BTreeMap<String, AnimatedProperty>,
}

impl PropertyBag {
    pub fn define(&mut self, descriptor: PropertyDescriptor) {
        let key = descriptor.path.clone();
        self.properties
            .entry(key)
            .or_insert_with(|| AnimatedProperty::from_descriptor(descriptor));
    }

    pub fn upsert(&mut self, property: AnimatedProperty) {
        self.properties.insert(property.descriptor.path.clone(), property);
    }

    pub fn property(&self, path: &str) -> Option<&AnimatedProperty> {
        self.properties.get(path)
    }

    pub fn evaluate(&self, path: &str, time: TimeCode) -> Option<PropertyValue> {
        self.properties.get(path).map(|property| property.evaluate(time))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &AnimatedProperty)> {
        self.properties.iter().map(|(path, property)| (path.as_str(), property))
    }

    pub fn set_static_value(&mut self, path: &str, value: PropertyValue) -> Result<()> {
        let property = self.require_property_mut(path)?;
        ensure_value_compatible(path, &property.descriptor.default_value, &value)?;
        property.track.set_static_value(value);
        Ok(())
    }

    pub fn set_keyframe(&mut self, path: &str, keyframe: Keyframe<PropertyValue>) -> Result<()> {
        let property = self.require_property_mut(path)?;
        ensure_value_compatible(path, &property.descriptor.default_value, &keyframe.value)?;
        if !property.descriptor.is_animatable {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "property_set_keyframe".to_string(),
                reason: format!("属性不支持关键帧: {path}"),
            });
        }
        property.track.set_keyframe(keyframe);
        Ok(())
    }

    pub fn remove_keyframe(&mut self, path: &str, time: TimeCode) -> Result<()> {
        let property = self.require_property_mut(path)?;
        let _ = property.track.remove_keyframe(time);
        Ok(())
    }

    pub fn remove_property(&mut self, path: &str) -> Option<AnimatedProperty> {
        self.properties.remove(path)
    }

    pub fn apply_mutation(&mut self, mutation: PropertyMutation) -> Result<()> {
        match mutation {
            PropertyMutation::DefineProperty(descriptor) => {
                self.define(descriptor);
                Ok(())
            }
            PropertyMutation::SetStaticValue { path, value } => self.set_static_value(&path, value),
            PropertyMutation::SetKeyframe { path, keyframe } => self.set_keyframe(&path, keyframe),
            PropertyMutation::RemoveKeyframe { path, time } => self.remove_keyframe(&path, time),
            PropertyMutation::RemoveProperty { path } => {
                self.remove_property(&path);
                Ok(())
            }
        }
    }

    fn require_property_mut(&mut self, path: &str) -> Result<&mut AnimatedProperty> {
        self.properties.get_mut(path).ok_or_else(|| MondrianError::WorkflowStepFailed {
            step_id: "property_lookup".to_string(),
            reason: format!("属性不存在: {path}"),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PropertyMutation {
    DefineProperty(PropertyDescriptor),
    SetStaticValue {
        path: String,
        value: PropertyValue,
    },
    SetKeyframe {
        path: String,
        keyframe: Keyframe<PropertyValue>,
    },
    RemoveKeyframe {
        path: String,
        time: TimeCode,
    },
    RemoveProperty {
        path: String,
    },
}

pub trait PropertyHost {
    fn property_bag(&self) -> Result<PropertyBag>;
    fn apply_property_mutation(&mut self, mutation: PropertyMutation) -> Result<()>;

    fn apply_property_mutations<I>(&mut self, mutations: I) -> Result<()>
    where
        I: IntoIterator<Item = PropertyMutation>,
    {
        for mutation in mutations {
            self.apply_property_mutation(mutation)?;
        }
        Ok(())
    }
}

fn ensure_value_compatible(
    path: &str,
    expected: &PropertyValue,
    actual: &PropertyValue,
) -> Result<()> {
    if expected.same_variant(actual) {
        Ok(())
    } else {
        Err(MondrianError::WorkflowStepFailed {
            step_id: "property_type_check".to_string(),
            reason: format!(
                "属性类型不匹配: {path} 期望 {}，收到 {}",
                expected.kind(),
                actual.kind()
            ),
        })
    }
}

fn normalize_time(t: TimeCode, start: TimeCode, end: TimeCode) -> f32 {
    let total = (end.frame - start.frame) as f32;
    if total <= 0.0 {
        return 0.0;
    }
    (t.frame - start.frame) as f32 / total
}

fn ease_in(t: f32) -> f32 {
    t * t * t
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

fn ease_in_out(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

fn solve_bezier_t(x: f32, _cp_out: Option<Vec2>, _cp_in: Option<Vec2>) -> f32 {
    x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Rational;

    fn tc(frame: i64) -> TimeCode {
        TimeCode::new(frame, Rational::new(1, 25))
    }

    #[test]
    fn linear_interpolation() {
        let mut track = KeyframeTrack::<f32>::constant(0.0);
        track.set_keyframe(Keyframe {
            time: tc(0),
            value: 0.0,
            interpolation: InterpolationType::Linear,
            control_in: None,
            control_out: None,
        });
        track.set_keyframe(Keyframe {
            time: tc(100),
            value: 100.0,
            interpolation: InterpolationType::Linear,
            control_in: None,
            control_out: None,
        });

        let mid = track.evaluate(tc(50));
        assert!((mid - 50.0).abs() < 0.01, "Expected 50.0, got {mid}");
    }

    #[test]
    fn property_bag_supports_plugin_style_keyframe_mutation() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "effect.blur.radius",
            "模糊半径",
            PropertyValue::Float(8.0),
        ));
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "effect.blur.radius".to_string(),
            keyframe: Keyframe {
                time: tc(0),
                value: PropertyValue::Float(8.0),
                interpolation: InterpolationType::Linear,
                control_in: None,
                control_out: None,
            },
        })
        .expect("set start keyframe");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "effect.blur.radius".to_string(),
            keyframe: Keyframe {
                time: tc(20),
                value: PropertyValue::Float(28.0),
                interpolation: InterpolationType::Linear,
                control_in: None,
                control_out: None,
            },
        })
        .expect("set end keyframe");

        let value = bag
            .evaluate("effect.blur.radius", tc(10))
            .and_then(|value| value.as_f32())
            .expect("evaluate interpolated value");
        assert!((value - 18.0).abs() < 0.01);
    }

    #[test]
    fn property_bag_rejects_mismatched_types() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "effect.key.color",
            "抠像颜色",
            PropertyValue::Color(Color::WHITE),
        ));

        let err = bag
            .set_static_value("effect.key.color", PropertyValue::Float(1.0))
            .expect_err("mismatched value should fail");
        assert!(err.to_string().contains("属性类型不匹配"));
    }
}
