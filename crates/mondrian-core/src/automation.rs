//! 自动化 / 关键帧基础设施
//!
//! 提供共享的关键帧轨道、动态属性值、属性包和 mutation API，
//! 让内建属性、效果参数和未来插件属性走同一条链路。

use crate::{
    error::{MondrianError, Result},
    types::{AnimationTrackId, Color, KeyframeId, TimeCode},
};
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 面向 UI/命令层的插值意图。
///
/// 实际持久化和求值仍然基于 `KeyframeInterpolation` 与 `KeyframeTemporalFlags`，
/// 这个枚举只负责把菜单动作映射到一组默认手柄与约束。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum InterpolationType {
    Hold,
    #[default]
    Linear,
    Bezier,
    AutoBezier,
    ContinuousBezier,
    EaseIn,
    EaseOut,
}

pub type TimeTicks = i64;
pub const SUBFRAME_TICKS_PER_FRAME: TimeTicks = 1000;

pub fn timecode_to_ticks(time: TimeCode) -> TimeTicks {
    time.frame.saturating_mul(SUBFRAME_TICKS_PER_FRAME)
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum KeyframeInterpolation {
    Hold,
    Linear,
    Bezier(BezierHandle),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BezierHandle {
    pub time_offset: f64,
    pub value_offset: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct KeyframeTemporalFlags {
    pub auto_bezier: bool,
    pub continuous: bool,
    pub broken_handles: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PropertyValueType {
    Bool,
    Int,
    Float,
    Double,
    Vec2,
    Vec3,
    Color,
    Vec4,
    Text,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AnimatablePropertyUiMetadata {
    #[serde(default)]
    pub group_name: Option<String>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub soft_min: Option<f64>,
    #[serde(default)]
    pub soft_max: Option<f64>,
    #[serde(default)]
    pub step: Option<f64>,
    #[serde(default)]
    pub supports_bezier: bool,
    #[serde(default)]
    pub supports_spatial: bool,
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

    pub fn value_type(&self) -> PropertyValueType {
        match self {
            Self::Bool(_) => PropertyValueType::Bool,
            Self::Int(_) => PropertyValueType::Int,
            Self::Float(_) => PropertyValueType::Float,
            Self::Double(_) => PropertyValueType::Double,
            Self::Vec2(_) => PropertyValueType::Vec2,
            Self::Vec3(_) => PropertyValueType::Vec3,
            Self::Color(_) => PropertyValueType::Color,
            Self::Vec4(_) => PropertyValueType::Vec4,
            Self::Text(_) => PropertyValueType::Text,
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

    pub fn channel_count(&self) -> usize {
        self.value_type().channel_count()
    }

    pub fn to_channel_values(&self) -> Vec<f64> {
        match self {
            Self::Bool(value) => vec![if *value { 1.0 } else { 0.0 }],
            Self::Int(value) => vec![*value as f64],
            Self::Float(value) => vec![*value as f64],
            Self::Double(value) => vec![*value],
            Self::Vec2(value) => vec![value.x as f64, value.y as f64],
            Self::Vec3(value) => vec![value.x as f64, value.y as f64, value.z as f64],
            Self::Color(value) => vec![
                value.r as f64,
                value.g as f64,
                value.b as f64,
                value.a as f64,
            ],
            Self::Vec4(value) => value.iter().map(|component| *component as f64).collect(),
            Self::Text(_) => vec![],
        }
    }

    pub fn from_channel_values(
        value_type: PropertyValueType,
        channel_values: &[f64],
        fallback: &PropertyValue,
    ) -> Self {
        match value_type {
            PropertyValueType::Bool => {
                Self::Bool(channel_values.first().copied().unwrap_or(0.0) >= 0.5)
            }
            PropertyValueType::Int => {
                Self::Int(channel_values.first().copied().unwrap_or(0.0).round() as i64)
            }
            PropertyValueType::Float => {
                Self::Float(channel_values.first().copied().unwrap_or(0.0) as f32)
            }
            PropertyValueType::Double => {
                Self::Double(channel_values.first().copied().unwrap_or(0.0))
            }
            PropertyValueType::Vec2 => Self::Vec2(Vec2::new(
                channel_values.first().copied().unwrap_or(0.0) as f32,
                channel_values.get(1).copied().unwrap_or(0.0) as f32,
            )),
            PropertyValueType::Vec3 => Self::Vec3(Vec3::new(
                channel_values.first().copied().unwrap_or(0.0) as f32,
                channel_values.get(1).copied().unwrap_or(0.0) as f32,
                channel_values.get(2).copied().unwrap_or(0.0) as f32,
            )),
            PropertyValueType::Color => Self::Color(Color {
                r: channel_values.first().copied().unwrap_or(0.0) as f32,
                g: channel_values.get(1).copied().unwrap_or(0.0) as f32,
                b: channel_values.get(2).copied().unwrap_or(0.0) as f32,
                a: channel_values.get(3).copied().unwrap_or(1.0) as f32,
            }),
            PropertyValueType::Vec4 => Self::Vec4([
                channel_values.first().copied().unwrap_or(0.0) as f32,
                channel_values.get(1).copied().unwrap_or(0.0) as f32,
                channel_values.get(2).copied().unwrap_or(0.0) as f32,
                channel_values.get(3).copied().unwrap_or(0.0) as f32,
            ]),
            PropertyValueType::Text => fallback.clone(),
        }
    }
}

impl PropertyValueType {
    pub fn channel_count(self) -> usize {
        match self {
            Self::Bool | Self::Int | Self::Float | Self::Double => 1,
            Self::Vec2 => 2,
            Self::Vec3 => 3,
            Self::Color | Self::Vec4 => 4,
            Self::Text => 0,
        }
    }

    pub fn supports_animation(self) -> bool {
        !matches!(self, Self::Text)
    }

    pub fn normalized_interpolation(self, interpolation: InterpolationType) -> InterpolationType {
        match self {
            Self::Bool | Self::Int | Self::Text => InterpolationType::Hold,
            _ => interpolation,
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
    pub id: KeyframeId,
    pub time: TimeTicks,
    pub value: T,
    pub interp_in: KeyframeInterpolation,
    pub interp_out: KeyframeInterpolation,
    pub temporal_flags: KeyframeTemporalFlags,
}

impl<T> Keyframe<T> {
    pub fn linear(time: TimeTicks, value: T) -> Self {
        Self::from_preset(time, value, InterpolationType::Linear)
    }

    pub fn from_preset(time: TimeTicks, value: T, interpolation: InterpolationType) -> Self {
        let (interp_in, interp_out, temporal_flags) = interpolation_defaults(interpolation);
        Self {
            id: KeyframeId::new(),
            time,
            value,
            interp_in,
            interp_out,
            temporal_flags,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyframeTrack<T: Interpolatable> {
    keyframes: Vec<Keyframe<T>>,
    static_value: T,
    #[serde(default = "default_track_enabled")]
    enabled: bool,
}

impl<T: Interpolatable + Serialize + for<'de> Deserialize<'de>> KeyframeTrack<T> {
    pub fn constant(value: T) -> Self {
        Self {
            keyframes: vec![],
            static_value: value,
            enabled: false,
        }
    }

    pub fn from_parts(static_value: T, mut keyframes: Vec<Keyframe<T>>) -> Self {
        keyframes.sort_by_key(|keyframe| keyframe.time);
        Self { keyframes, static_value, enabled: true }
    }

    pub fn from_parts_with_enabled(
        static_value: T,
        mut keyframes: Vec<Keyframe<T>>,
        enabled: bool,
    ) -> Self {
        keyframes.sort_by_key(|keyframe| keyframe.time);
        Self { keyframes, static_value, enabled }
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

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn map<U, F>(&self, mut map_value: F) -> KeyframeTrack<U>
    where
        U: Interpolatable + Serialize + for<'de> Deserialize<'de>,
        F: FnMut(&T) -> U,
    {
        KeyframeTrack::from_parts_with_enabled(
            map_value(&self.static_value),
            self.keyframes
                .iter()
                .map(|keyframe| Keyframe {
                    id: keyframe.id,
                    time: keyframe.time,
                    value: map_value(&keyframe.value),
                    interp_in: keyframe.interp_in,
                    interp_out: keyframe.interp_out,
                    temporal_flags: keyframe.temporal_flags,
                })
                .collect(),
            self.enabled,
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
                id: keyframe.id,
                time: keyframe.time,
                value: map_value(&keyframe.value)?,
                interp_in: keyframe.interp_in,
                interp_out: keyframe.interp_out,
                temporal_flags: keyframe.temporal_flags,
            });
        }
        Ok(KeyframeTrack::from_parts_with_enabled(
            static_value,
            keyframes,
            self.enabled,
        ))
    }

    pub fn evaluate(&self, time: TimeCode) -> T {
        let time = timecode_to_ticks(time);
        self.evaluate_ticks(time)
    }

    pub fn evaluate_ticks(&self, time: TimeTicks) -> T {
        if self.keyframes.is_empty() || !self.enabled {
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
        match segment_progress(
            normalize_ticks(time, keyframe_a.time, keyframe_b.time),
            keyframe_a.interp_out,
            keyframe_b.interp_in,
        ) {
            SegmentProgress::Hold => keyframe_a.value.clone(),
            SegmentProgress::Progress(t) => T::lerp(&keyframe_a.value, &keyframe_b.value, t),
        }
    }

    pub fn set_keyframe(&mut self, keyframe: Keyframe<T>) {
        self.enabled = true;
        let pos = self.keyframes.partition_point(|candidate| candidate.time < keyframe.time);
        if pos < self.keyframes.len() && self.keyframes[pos].time == keyframe.time {
            self.keyframes[pos] = keyframe;
        } else {
            self.keyframes.insert(pos, keyframe);
        }
    }

    pub fn remove_keyframe(&mut self, time: TimeTicks) -> Option<Keyframe<T>> {
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

const fn default_track_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyDescriptor {
    pub path: String,
    pub display_name: String,
    pub default_value: PropertyValue,
    #[serde(default)]
    pub value_type: Option<PropertyValueType>,
    #[serde(default = "default_property_animatable")]
    pub is_animatable: bool,
    #[serde(default)]
    pub ui_metadata: AnimatablePropertyUiMetadata,
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
            value_type: Some(default_value.value_type()),
            default_value,
            is_animatable: true,
            ui_metadata: AnimatablePropertyUiMetadata::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimationChannel {
    pub index: usize,
    keyframes: Vec<Keyframe<f64>>,
}

impl AnimationChannel {
    pub fn new(index: usize) -> Self {
        Self { index, keyframes: Vec::new() }
    }

    pub fn keyframes(&self) -> &[Keyframe<f64>] {
        &self.keyframes
    }

    pub fn keyframe_at(&self, time: TimeTicks) -> Option<&Keyframe<f64>> {
        self.keyframes.iter().find(|keyframe| keyframe.time == time)
    }

    pub fn set_keyframe(&mut self, keyframe: Keyframe<f64>) {
        let pos = self.keyframes.partition_point(|candidate| candidate.time < keyframe.time);
        if pos < self.keyframes.len() && self.keyframes[pos].time == keyframe.time {
            self.keyframes[pos] = keyframe;
        } else {
            self.keyframes.insert(pos, keyframe);
        }
        self.normalize_keyframes();
    }

    pub fn remove_keyframe(&mut self, time: TimeTicks) -> Option<Keyframe<f64>> {
        if let Some(pos) = self.keyframes.iter().position(|keyframe| keyframe.time == time) {
            let removed = self.keyframes.remove(pos);
            self.normalize_keyframes();
            Some(removed)
        } else {
            None
        }
    }

    pub fn evaluate(&self, time: TimeTicks, fallback: f64) -> f64 {
        evaluate_numeric_channel(self, time, fallback)
    }

    pub fn is_animated(&self) -> bool {
        !self.keyframes.is_empty()
    }

    fn normalize_keyframes(&mut self) {
        if self.keyframes.is_empty() {
            return;
        }

        if self.keyframes.len() == 1 {
            return;
        }

        let auto_handles = compute_auto_bezier_handles(&self.keyframes);

        for index in 0..self.keyframes.len() {
            let has_prev = index > 0;
            let has_next = index + 1 < self.keyframes.len();
            let keyframe = &mut self.keyframes[index];

            if keyframe.temporal_flags.auto_bezier {
                if let Some((interp_in, interp_out)) = auto_handles.get(index).copied() {
                    keyframe.interp_in = if has_prev {
                        interp_in
                    } else {
                        keyframe.interp_in
                    };
                    keyframe.interp_out = if has_next {
                        interp_out
                    } else {
                        keyframe.interp_out
                    };
                    keyframe.temporal_flags.continuous = has_prev && has_next;
                    keyframe.temporal_flags.broken_handles = false;
                }
            }
        }

        for index in 0..self.keyframes.len() {
            if index == 0 || index + 1 >= self.keyframes.len() {
                continue;
            }

            let previous = self.keyframes[index - 1].clone();
            let current = self.keyframes[index].clone();
            let next = self.keyframes[index + 1].clone();

            if current.temporal_flags.auto_bezier
                || !current.temporal_flags.continuous
                || current.temporal_flags.broken_handles
            {
                continue;
            }

            let slope = continuous_tangent_slope(&previous, &current, &next);
            let keyframe = &mut self.keyframes[index];
            keyframe.interp_in = KeyframeInterpolation::Bezier(continuous_handle_for_in(
                &previous,
                &current,
                slope,
                handle_for_in(current.interp_in).time_offset,
            ));
            keyframe.interp_out = KeyframeInterpolation::Bezier(continuous_handle_for_out(
                &current,
                &next,
                slope,
                handle_for_out(current.interp_out).time_offset,
            ));
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimatedProperty {
    pub track_id: AnimationTrackId,
    pub descriptor: PropertyDescriptor,
    static_value: PropertyValue,
    #[serde(default)]
    animation_enabled: bool,
    #[serde(default)]
    channels: Vec<AnimationChannel>,
}

impl AnimatedProperty {
    pub fn from_descriptor(descriptor: PropertyDescriptor) -> Self {
        let value_type =
            descriptor.value_type.unwrap_or_else(|| descriptor.default_value.value_type());
        let static_value = descriptor.default_value.clone();
        let channel_count = if descriptor.is_animatable && value_type.supports_animation() {
            value_type.channel_count()
        } else {
            0
        };
        Self {
            track_id: AnimationTrackId::new(),
            descriptor,
            static_value,
            animation_enabled: false,
            channels: (0..channel_count).map(AnimationChannel::new).collect(),
        }
    }

    pub fn static_value(&self) -> &PropertyValue {
        &self.static_value
    }

    pub fn value_type(&self) -> PropertyValueType {
        self.descriptor.value_type.unwrap_or_else(|| self.static_value.value_type())
    }

    pub fn is_enabled(&self) -> bool {
        self.animation_enabled
    }

    pub fn is_animated(&self) -> bool {
        self.channels.iter().any(AnimationChannel::is_animated)
    }

    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    pub fn channels(&self) -> &[AnimationChannel] {
        &self.channels
    }

    pub fn channel(&self, index: usize) -> Option<&AnimationChannel> {
        self.channels.get(index)
    }

    pub fn keyframe_times(&self) -> Vec<TimeTicks> {
        let mut times = self
            .channels
            .iter()
            .flat_map(|channel| channel.keyframes.iter().map(|keyframe| keyframe.time))
            .collect::<Vec<_>>();
        times.sort_unstable();
        times.dedup();
        times
    }

    pub fn keyframe_at(&self, time: TimeTicks) -> Option<Keyframe<PropertyValue>> {
        let mut found = false;
        let mut id = None;
        let mut interp_in = KeyframeInterpolation::Linear;
        let mut interp_out = KeyframeInterpolation::Linear;
        let mut temporal_flags = KeyframeTemporalFlags::default();

        for channel in &self.channels {
            if let Some(keyframe) = channel.keyframe_at(time) {
                found = true;
                id.get_or_insert(keyframe.id);
                interp_in = keyframe.interp_in;
                interp_out = keyframe.interp_out;
                temporal_flags = keyframe.temporal_flags;
                break;
            }
        }

        if !found {
            return None;
        }

        Some(Keyframe {
            id: id.unwrap_or_else(KeyframeId::new),
            time,
            value: self.evaluate(time),
            interp_in,
            interp_out,
            temporal_flags,
        })
    }

    pub fn set_static_value(&mut self, value: PropertyValue) {
        self.static_value = value;
    }

    pub fn set_animation_enabled(&mut self, enabled: bool) {
        self.animation_enabled = enabled;
    }

    pub fn evaluate(&self, time: TimeTicks) -> PropertyValue {
        if !self.animation_enabled || !self.is_animated() || self.channels.is_empty() {
            return self.static_value.clone();
        }

        let fallback = self.static_value.to_channel_values();
        let channels = self
            .channels
            .iter()
            .enumerate()
            .map(|(index, channel)| {
                channel.evaluate(time, fallback.get(index).copied().unwrap_or(0.0))
            })
            .collect::<Vec<_>>();
        PropertyValue::from_channel_values(self.value_type(), &channels, &self.static_value)
    }

    pub fn enable_animation(&mut self, time: TimeTicks) -> Result<()> {
        if !self.descriptor.is_animatable || !self.value_type().supports_animation() {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "property_enable_animation".to_string(),
                reason: format!("属性不支持关键帧: {}", self.descriptor.path),
            });
        }
        if self.animation_enabled {
            return Ok(());
        }

        self.animation_enabled = true;
        if !self.is_animated() {
            self.write_value(
                time,
                self.static_value.clone(),
                InterpolationType::Linear,
                None,
            )?;
        }
        Ok(())
    }

    pub fn disable_animation(&mut self, time: TimeTicks) -> Result<()> {
        if !self.descriptor.is_animatable || !self.value_type().supports_animation() {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "property_disable_animation".to_string(),
                reason: format!("属性不支持关键帧: {}", self.descriptor.path),
            });
        }
        self.static_value = self.evaluate(time);
        self.animation_enabled = false;
        Ok(())
    }

    pub fn clear_animation(&mut self, time: TimeTicks) -> Result<()> {
        self.disable_animation(time)?;
        for channel in &mut self.channels {
            channel.keyframes.clear();
        }
        Ok(())
    }

    pub fn write_value(
        &mut self,
        time: TimeTicks,
        value: PropertyValue,
        interpolation: InterpolationType,
        handles: Option<(Option<Vec2>, Option<Vec2>)>,
    ) -> Result<()> {
        let channel_values = value.to_channel_values();
        let updates = channel_values.into_iter().enumerate().collect::<Vec<(usize, f64)>>();
        if self.animation_enabled && self.descriptor.is_animatable && !self.channels.is_empty() {
            self.write_channels(time, &updates, interpolation, handles)
        } else {
            self.static_value = value;
            Ok(())
        }
    }

    pub fn set_exact_keyframe(&mut self, keyframe: Keyframe<PropertyValue>) -> Result<()> {
        let channel_values = keyframe.value.to_channel_values();
        validate_channel_updates(
            self.channel_count(),
            &channel_values
                .iter()
                .enumerate()
                .map(|(index, value)| (index, *value))
                .collect::<Vec<_>>(),
            &self.descriptor.path,
        )?;

        self.animation_enabled = true;
        for (index, value) in channel_values.into_iter().enumerate() {
            let channel =
                self.channels.get_mut(index).ok_or_else(|| MondrianError::WorkflowStepFailed {
                    step_id: "property_set_exact_keyframe".to_string(),
                    reason: format!("属性通道不存在: {}[{index}]", self.descriptor.path),
                })?;
            channel.set_keyframe(Keyframe {
                id: keyframe.id,
                time: keyframe.time,
                value,
                interp_in: keyframe.interp_in,
                interp_out: keyframe.interp_out,
                temporal_flags: keyframe.temporal_flags,
            });
        }
        self.normalize_channels();
        Ok(())
    }

    pub fn write_channels(
        &mut self,
        time: TimeTicks,
        channel_values: &[(usize, f64)],
        interpolation: InterpolationType,
        handles: Option<(Option<Vec2>, Option<Vec2>)>,
    ) -> Result<()> {
        validate_channel_updates(self.channel_count(), channel_values, &self.descriptor.path)?;

        if self.animation_enabled && self.descriptor.is_animatable && !self.channels.is_empty() {
            let normalized = self.value_type().normalized_interpolation(interpolation);
            for (index, value) in channel_values {
                let channel = self.channels.get_mut(*index).ok_or_else(|| {
                    MondrianError::WorkflowStepFailed {
                        step_id: "property_write_channels".to_string(),
                        reason: format!("属性通道不存在: {}[{index}]", self.descriptor.path),
                    }
                })?;
                let existing = channel.keyframes.iter().find(|keyframe| keyframe.time == time);
                let (control_in, control_out) = handles.unwrap_or_else(|| {
                    (
                        existing.and_then(|keyframe| extract_handle(keyframe.interp_in)),
                        existing.and_then(|keyframe| extract_handle(keyframe.interp_out)),
                    )
                });
                let (default_in, default_out, default_flags) = interpolation_defaults(normalized);
                let interp_in = if let Some(handle) = control_in {
                    KeyframeInterpolation::Bezier(BezierHandle {
                        time_offset: handle.x.clamp(-1.0, 0.0) as f64,
                        value_offset: handle.y as f64,
                    })
                } else {
                    default_in
                };
                let interp_out = if let Some(handle) = control_out {
                    KeyframeInterpolation::Bezier(BezierHandle {
                        time_offset: handle.x.clamp(0.0, 1.0) as f64,
                        value_offset: handle.y as f64,
                    })
                } else {
                    default_out
                };
                channel.set_keyframe(Keyframe {
                    id: existing.map(|keyframe| keyframe.id).unwrap_or_else(KeyframeId::new),
                    time,
                    value: *value,
                    interp_in: existing.map(|keyframe| keyframe.interp_in).unwrap_or(interp_in),
                    interp_out: existing.map(|keyframe| keyframe.interp_out).unwrap_or(interp_out),
                    temporal_flags: existing
                        .map(|keyframe| keyframe.temporal_flags)
                        .unwrap_or(default_flags),
                });
            }
            self.normalize_channels();
            Ok(())
        } else {
            let mut static_channels = self.static_value.to_channel_values();
            for (index, value) in channel_values {
                if let Some(channel) = static_channels.get_mut(*index) {
                    *channel = *value;
                }
            }
            self.static_value = PropertyValue::from_channel_values(
                self.value_type(),
                &static_channels,
                &self.static_value,
            );
            Ok(())
        }
    }

    pub fn remove_keyframe(&mut self, time: TimeTicks) {
        for channel in &mut self.channels {
            let _ = channel.remove_keyframe(time);
        }
        self.normalize_channels();
    }

    pub fn move_keyframe(&mut self, from_time: TimeTicks, to_time: TimeTicks) -> Result<()> {
        if from_time == to_time {
            return Ok(());
        }

        let mut moved_any = false;
        let mut pending = Vec::new();

        for channel in &mut self.channels {
            let Some(keyframe) =
                channel.keyframes.iter().find(|keyframe| keyframe.time == from_time).cloned()
            else {
                continue;
            };

            if channel.keyframes.iter().any(|keyframe| keyframe.time == to_time) {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "property_move_keyframe".to_string(),
                    reason: format!(
                        "关键帧时间冲突: {} {} -> {}",
                        self.descriptor.path, from_time, to_time
                    ),
                });
            }

            moved_any = true;
            pending.push((channel.index, keyframe));
        }

        if !moved_any {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "property_move_keyframe".to_string(),
                reason: format!("关键帧不存在: {} @ {}", self.descriptor.path, from_time),
            });
        }

        for (channel_index, mut keyframe) in pending {
            let channel = self.channels.get_mut(channel_index).ok_or_else(|| {
                MondrianError::WorkflowStepFailed {
                    step_id: "property_move_keyframe".to_string(),
                    reason: format!("属性通道不存在: {}[{channel_index}]", self.descriptor.path),
                }
            })?;
            let _ = channel.remove_keyframe(from_time);
            keyframe.time = to_time;
            channel.set_keyframe(keyframe);
        }
        self.normalize_channels();

        Ok(())
    }

    pub fn update_keyframe_interpolation(
        &mut self,
        time: TimeTicks,
        interpolation: InterpolationType,
    ) -> Result<()> {
        if !self.descriptor.is_animatable || !self.value_type().supports_animation() {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "property_update_keyframe_interpolation".to_string(),
                reason: format!("属性不支持关键帧: {}", self.descriptor.path),
            });
        }

        let normalized = self.value_type().normalized_interpolation(interpolation);
        let mut updated_any = false;

        for channel in &mut self.channels {
            if let Some(index) = channel.keyframes.iter().position(|keyframe| keyframe.time == time)
            {
                let has_prev = index > 0;
                let has_next = index + 1 < channel.keyframes.len();
                let existing = &mut channel.keyframes[index];
                let (default_in, default_out, temporal_flags) = interpolation_defaults(normalized);
                match normalized {
                    InterpolationType::Bezier | InterpolationType::ContinuousBezier => {
                        existing.interp_in = if has_prev {
                            match existing.interp_in {
                                KeyframeInterpolation::Bezier(handle) => {
                                    KeyframeInterpolation::Bezier(handle)
                                }
                                _ => default_in,
                            }
                        } else {
                            existing.interp_in
                        };
                        existing.interp_out = if has_next {
                            match existing.interp_out {
                                KeyframeInterpolation::Bezier(handle) => {
                                    KeyframeInterpolation::Bezier(handle)
                                }
                                _ => default_out,
                            }
                        } else {
                            existing.interp_out
                        };
                        existing.temporal_flags = temporal_flags;
                    }
                    _ => {
                        existing.interp_in = default_in;
                        existing.interp_out = default_out;
                        existing.temporal_flags = temporal_flags;
                    }
                }
                updated_any = true;
            }
        }

        if !updated_any {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "property_update_keyframe_interpolation".to_string(),
                reason: format!("关键帧不存在: {} @ {}", self.descriptor.path, time),
            });
        }

        self.normalize_channels();
        Ok(())
    }

    pub fn update_channel_keyframe_handles(
        &mut self,
        time: TimeTicks,
        channel_index: usize,
        interp_in: KeyframeInterpolation,
        interp_out: KeyframeInterpolation,
    ) -> Result<()> {
        let channel = self.channels.get_mut(channel_index).ok_or_else(|| {
            MondrianError::WorkflowStepFailed {
                step_id: "property_update_channel_keyframe_handles".to_string(),
                reason: format!("属性通道不存在: {}[{channel_index}]", self.descriptor.path),
            }
        })?;

        let keyframe =
            channel.keyframes.iter_mut().find(|keyframe| keyframe.time == time).ok_or_else(
                || MondrianError::WorkflowStepFailed {
                    step_id: "property_update_channel_keyframe_handles".to_string(),
                    reason: format!(
                        "关键帧不存在: {}[{channel_index}] @ {}",
                        self.descriptor.path, time
                    ),
                },
            )?;

        let cleared_auto = keyframe.temporal_flags.auto_bezier;
        keyframe.interp_in = interp_in;
        keyframe.interp_out = interp_out;
        if cleared_auto {
            for channel in &mut self.channels {
                if let Some(other) =
                    channel.keyframes.iter_mut().find(|candidate| candidate.time == time)
                {
                    other.temporal_flags.auto_bezier = false;
                }
            }
        }
        self.normalize_channels();
        Ok(())
    }

    pub fn update_channel_keyframe_value(
        &mut self,
        time: TimeTicks,
        channel_index: usize,
        value: f64,
    ) -> Result<()> {
        let channel = self.channels.get_mut(channel_index).ok_or_else(|| {
            MondrianError::WorkflowStepFailed {
                step_id: "property_update_channel_keyframe_value".to_string(),
                reason: format!("属性通道不存在: {}[{channel_index}]", self.descriptor.path),
            }
        })?;

        let keyframe =
            channel.keyframes.iter_mut().find(|keyframe| keyframe.time == time).ok_or_else(
                || MondrianError::WorkflowStepFailed {
                    step_id: "property_update_channel_keyframe_value".to_string(),
                    reason: format!(
                        "关键帧不存在: {}[{channel_index}] @ {}",
                        self.descriptor.path, time
                    ),
                },
            )?;

        keyframe.value = value;
        self.normalize_channels();
        Ok(())
    }

    pub fn update_keyframe_temporal_flags(
        &mut self,
        time: TimeTicks,
        temporal_flags: KeyframeTemporalFlags,
    ) -> Result<()> {
        let mut updated_any = false;

        for channel in &mut self.channels {
            if let Some(keyframe) =
                channel.keyframes.iter_mut().find(|keyframe| keyframe.time == time)
            {
                keyframe.temporal_flags = temporal_flags;
                updated_any = true;
            }
        }

        if !updated_any {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "property_update_keyframe_temporal_flags".to_string(),
                reason: format!("关键帧不存在: {} @ {}", self.descriptor.path, time),
            });
        }

        self.normalize_channels();
        Ok(())
    }

    fn normalize_channels(&mut self) {
        for channel in &mut self.channels {
            channel.normalize_keyframes();
        }
    }

    pub fn apply_mutation(&mut self, mutation: PropertyMutation) -> Result<()> {
        match mutation {
            PropertyMutation::DefineProperty(descriptor) => {
                if descriptor.path != self.descriptor.path {
                    return Err(MondrianError::WorkflowStepFailed {
                        step_id: "property_apply_mutation".to_string(),
                        reason: format!(
                            "属性路径不匹配: expected {}, got {}",
                            self.descriptor.path, descriptor.path
                        ),
                    });
                }
                self.descriptor = descriptor;
                Ok(())
            }
            PropertyMutation::SetStaticValue { path, value } => {
                self.ensure_path(&path)?;
                ensure_value_compatible(&path, &self.descriptor.default_value, &value)?;
                self.set_static_value(value);
                Ok(())
            }
            PropertyMutation::SetKeyframe { path, keyframe } => {
                self.ensure_path(&path)?;
                ensure_value_compatible(&path, &self.descriptor.default_value, &keyframe.value)?;
                if !self.descriptor.is_animatable {
                    return Err(MondrianError::WorkflowStepFailed {
                        step_id: "property_set_keyframe".to_string(),
                        reason: format!("属性不支持关键帧: {path}"),
                    });
                }
                self.set_animation_enabled(true);
                self.set_exact_keyframe(keyframe)
            }
            PropertyMutation::RemoveKeyframe { path, time } => {
                self.ensure_path(&path)?;
                self.remove_keyframe(time);
                Ok(())
            }
            PropertyMutation::MoveKeyframe { path, from_time, to_time } => {
                self.ensure_path(&path)?;
                self.move_keyframe(from_time, to_time)
            }
            PropertyMutation::UpdateKeyframeInterpolation { path, time, interpolation } => {
                self.ensure_path(&path)?;
                self.update_keyframe_interpolation(time, interpolation)
            }
            PropertyMutation::UpdateChannelKeyframeHandles {
                path,
                time,
                channel_index,
                interp_in,
                interp_out,
            } => {
                self.ensure_path(&path)?;
                self.update_channel_keyframe_handles(time, channel_index, interp_in, interp_out)
            }
            PropertyMutation::UpdateChannelKeyframeValue { path, time, channel_index, value } => {
                self.ensure_path(&path)?;
                self.update_channel_keyframe_value(time, channel_index, value)
            }
            PropertyMutation::UpdateKeyframeTemporalFlags { path, time, temporal_flags } => {
                self.ensure_path(&path)?;
                self.update_keyframe_temporal_flags(time, temporal_flags)
            }
            PropertyMutation::EnableAnimation { path, time } => {
                self.ensure_path(&path)?;
                self.enable_animation(time)
            }
            PropertyMutation::DisableAnimation { path, time } => {
                self.ensure_path(&path)?;
                self.disable_animation(time)
            }
            PropertyMutation::ClearAnimation { path, time } => {
                self.ensure_path(&path)?;
                self.clear_animation(time)
            }
            PropertyMutation::WriteValue { path, time, value, interpolation } => {
                self.ensure_path(&path)?;
                ensure_value_compatible(&path, &self.descriptor.default_value, &value)?;
                self.write_value(time, value, interpolation, None)
            }
            PropertyMutation::WriteChannels { path, time, channel_values, interpolation } => {
                self.ensure_path(&path)?;
                self.write_channels(time, &channel_values, interpolation, None)
            }
            PropertyMutation::RemoveProperty { path } => {
                self.ensure_path(&path)?;
                Err(MondrianError::WorkflowStepFailed {
                    step_id: "property_apply_mutation".to_string(),
                    reason: format!("单属性上下文不支持移除属性: {path}"),
                })
            }
        }
    }

    fn ensure_path(&self, path: &str) -> Result<()> {
        if path == self.descriptor.path {
            Ok(())
        } else {
            Err(MondrianError::WorkflowStepFailed {
                step_id: "property_lookup".to_string(),
                reason: format!(
                    "属性路径不匹配: expected {}, got {path}",
                    self.descriptor.path
                ),
            })
        }
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

    pub fn evaluate(&self, path: &str, time: TimeTicks) -> Option<PropertyValue> {
        self.properties.get(path).map(|property| property.evaluate(time))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &AnimatedProperty)> {
        self.properties.iter().map(|(path, property)| (path.as_str(), property))
    }

    pub fn set_static_value(&mut self, path: &str, value: PropertyValue) -> Result<()> {
        let property = self.require_property_mut(path)?;
        ensure_value_compatible(path, &property.descriptor.default_value, &value)?;
        property.set_static_value(value);
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
        property.set_animation_enabled(true);
        property.set_exact_keyframe(keyframe)?;
        Ok(())
    }

    pub fn enable_animation(&mut self, path: &str, time: TimeTicks) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.enable_animation(time)
    }

    pub fn disable_animation(&mut self, path: &str, time: TimeTicks) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.disable_animation(time)
    }

    pub fn clear_animation(&mut self, path: &str, time: TimeTicks) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.clear_animation(time)
    }

    pub fn write_value(
        &mut self,
        path: &str,
        time: TimeTicks,
        value: PropertyValue,
        interpolation: InterpolationType,
    ) -> Result<()> {
        let property = self.require_property_mut(path)?;
        ensure_value_compatible(path, &property.descriptor.default_value, &value)?;
        property.write_value(time, value, interpolation, None)
    }

    pub fn write_channels(
        &mut self,
        path: &str,
        time: TimeTicks,
        channel_values: &[(usize, f64)],
        interpolation: InterpolationType,
    ) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.write_channels(time, channel_values, interpolation, None)
    }

    pub fn remove_keyframe(&mut self, path: &str, time: TimeTicks) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.remove_keyframe(time);
        Ok(())
    }

    pub fn move_keyframe(
        &mut self,
        path: &str,
        from_time: TimeTicks,
        to_time: TimeTicks,
    ) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.move_keyframe(from_time, to_time)
    }

    pub fn update_keyframe_interpolation(
        &mut self,
        path: &str,
        time: TimeTicks,
        interpolation: InterpolationType,
    ) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.update_keyframe_interpolation(time, interpolation)
    }

    pub fn update_channel_keyframe_handles(
        &mut self,
        path: &str,
        time: TimeTicks,
        channel_index: usize,
        interp_in: KeyframeInterpolation,
        interp_out: KeyframeInterpolation,
    ) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.update_channel_keyframe_handles(time, channel_index, interp_in, interp_out)
    }

    pub fn update_channel_keyframe_value(
        &mut self,
        path: &str,
        time: TimeTicks,
        channel_index: usize,
        value: f64,
    ) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.update_channel_keyframe_value(time, channel_index, value)
    }

    pub fn update_keyframe_temporal_flags(
        &mut self,
        path: &str,
        time: TimeTicks,
        temporal_flags: KeyframeTemporalFlags,
    ) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.update_keyframe_temporal_flags(time, temporal_flags)
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
            PropertyMutation::MoveKeyframe { path, from_time, to_time } => {
                self.move_keyframe(&path, from_time, to_time)
            }
            PropertyMutation::UpdateKeyframeInterpolation { path, time, interpolation } => {
                self.update_keyframe_interpolation(&path, time, interpolation)
            }
            PropertyMutation::UpdateChannelKeyframeHandles {
                path,
                time,
                channel_index,
                interp_in,
                interp_out,
            } => self.update_channel_keyframe_handles(
                &path,
                time,
                channel_index,
                interp_in,
                interp_out,
            ),
            PropertyMutation::UpdateChannelKeyframeValue { path, time, channel_index, value } => {
                self.update_channel_keyframe_value(&path, time, channel_index, value)
            }
            PropertyMutation::UpdateKeyframeTemporalFlags { path, time, temporal_flags } => {
                self.update_keyframe_temporal_flags(&path, time, temporal_flags)
            }
            PropertyMutation::EnableAnimation { path, time } => self.enable_animation(&path, time),
            PropertyMutation::DisableAnimation { path, time } => {
                self.disable_animation(&path, time)
            }
            PropertyMutation::ClearAnimation { path, time } => self.clear_animation(&path, time),
            PropertyMutation::WriteValue { path, time, value, interpolation } => {
                self.write_value(&path, time, value, interpolation)
            }
            PropertyMutation::WriteChannels { path, time, channel_values, interpolation } => {
                self.write_channels(&path, time, &channel_values, interpolation)
            }
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
        time: TimeTicks,
    },
    MoveKeyframe {
        path: String,
        from_time: TimeTicks,
        to_time: TimeTicks,
    },
    UpdateKeyframeInterpolation {
        path: String,
        time: TimeTicks,
        interpolation: InterpolationType,
    },
    UpdateChannelKeyframeHandles {
        path: String,
        time: TimeTicks,
        channel_index: usize,
        interp_in: KeyframeInterpolation,
        interp_out: KeyframeInterpolation,
    },
    UpdateChannelKeyframeValue {
        path: String,
        time: TimeTicks,
        channel_index: usize,
        value: f64,
    },
    UpdateKeyframeTemporalFlags {
        path: String,
        time: TimeTicks,
        temporal_flags: KeyframeTemporalFlags,
    },
    EnableAnimation {
        path: String,
        time: TimeTicks,
    },
    DisableAnimation {
        path: String,
        time: TimeTicks,
    },
    ClearAnimation {
        path: String,
        time: TimeTicks,
    },
    WriteValue {
        path: String,
        time: TimeTicks,
        value: PropertyValue,
        interpolation: InterpolationType,
    },
    WriteChannels {
        path: String,
        time: TimeTicks,
        channel_values: Vec<(usize, f64)>,
        interpolation: InterpolationType,
    },
    RemoveProperty {
        path: String,
    },
}

impl PropertyMutation {
    pub fn path(&self) -> &str {
        match self {
            Self::DefineProperty(descriptor) => &descriptor.path,
            Self::SetStaticValue { path, .. }
            | Self::SetKeyframe { path, .. }
            | Self::RemoveKeyframe { path, .. }
            | Self::MoveKeyframe { path, .. }
            | Self::UpdateKeyframeInterpolation { path, .. }
            | Self::UpdateChannelKeyframeHandles { path, .. }
            | Self::UpdateChannelKeyframeValue { path, .. }
            | Self::UpdateKeyframeTemporalFlags { path, .. }
            | Self::EnableAnimation { path, .. }
            | Self::DisableAnimation { path, .. }
            | Self::ClearAnimation { path, .. }
            | Self::WriteValue { path, .. }
            | Self::WriteChannels { path, .. }
            | Self::RemoveProperty { path } => path,
        }
    }
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

fn validate_channel_updates(
    channel_count: usize,
    channel_values: &[(usize, f64)],
    path: &str,
) -> Result<()> {
    for (index, _) in channel_values {
        if *index >= channel_count {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "property_channel_bounds".to_string(),
                reason: format!("属性通道越界: {path}[{index}]"),
            });
        }
    }
    Ok(())
}

fn evaluate_numeric_channel(channel: &AnimationChannel, time: TimeTicks, fallback: f64) -> f64 {
    if channel.keyframes.is_empty() {
        return fallback;
    }
    if time <= channel.keyframes.first().expect("non-empty keyframes").time {
        return channel.keyframes.first().expect("non-empty keyframes").value;
    }
    if time >= channel.keyframes.last().expect("non-empty keyframes").time {
        return channel.keyframes.last().expect("non-empty keyframes").value;
    }

    let idx = channel
        .keyframes
        .partition_point(|keyframe| keyframe.time <= time)
        .saturating_sub(1);
    let keyframe_a = &channel.keyframes[idx];
    let keyframe_b = &channel.keyframes[idx + 1];
    match segment_progress(
        normalize_ticks(time, keyframe_a.time, keyframe_b.time),
        keyframe_a.interp_out,
        keyframe_b.interp_in,
    ) {
        SegmentProgress::Hold => keyframe_a.value,
        SegmentProgress::Progress(t) => f64::lerp(&keyframe_a.value, &keyframe_b.value, t),
    }
}

fn normalize_ticks(t: TimeTicks, start: TimeTicks, end: TimeTicks) -> f32 {
    let total = (end - start) as f32;
    if total <= 0.0 {
        return 0.0;
    }
    (t - start) as f32 / total
}

enum SegmentProgress {
    Hold,
    Progress(f32),
}

fn segment_progress(
    t: f32,
    interp_out: KeyframeInterpolation,
    interp_in: KeyframeInterpolation,
) -> SegmentProgress {
    match (interp_out, interp_in) {
        (KeyframeInterpolation::Hold, _) | (_, KeyframeInterpolation::Hold) => {
            SegmentProgress::Hold
        }
        (KeyframeInterpolation::Linear, KeyframeInterpolation::Linear) => {
            SegmentProgress::Progress(t)
        }
        (out_interp, in_interp) => {
            let out_handle = handle_for_out(out_interp);
            let in_handle = handle_for_in(in_interp);
            SegmentProgress::Progress(solve_bezier_t(t, out_handle, in_handle))
        }
    }
}

fn interpolation_defaults(
    interpolation: InterpolationType,
) -> (
    KeyframeInterpolation,
    KeyframeInterpolation,
    KeyframeTemporalFlags,
) {
    match interpolation {
        InterpolationType::Hold => (
            KeyframeInterpolation::Hold,
            KeyframeInterpolation::Hold,
            KeyframeTemporalFlags::default(),
        ),
        InterpolationType::Linear => (
            KeyframeInterpolation::Linear,
            KeyframeInterpolation::Linear,
            KeyframeTemporalFlags::default(),
        ),
        InterpolationType::Bezier => (
            KeyframeInterpolation::Bezier(default_out_bezier_handle()),
            KeyframeInterpolation::Bezier(default_in_bezier_handle()),
            KeyframeTemporalFlags::default(),
        ),
        InterpolationType::AutoBezier => (
            KeyframeInterpolation::Bezier(default_in_bezier_handle()),
            KeyframeInterpolation::Bezier(default_out_bezier_handle()),
            KeyframeTemporalFlags {
                auto_bezier: true,
                continuous: true,
                broken_handles: false,
            },
        ),
        InterpolationType::ContinuousBezier => (
            KeyframeInterpolation::Bezier(default_in_bezier_handle()),
            KeyframeInterpolation::Bezier(default_out_bezier_handle()),
            KeyframeTemporalFlags {
                auto_bezier: false,
                continuous: true,
                broken_handles: false,
            },
        ),
        InterpolationType::EaseIn => (
            KeyframeInterpolation::Linear,
            KeyframeInterpolation::Bezier(default_out_bezier_handle()),
            KeyframeTemporalFlags::default(),
        ),
        InterpolationType::EaseOut => (
            KeyframeInterpolation::Bezier(default_in_bezier_handle()),
            KeyframeInterpolation::Linear,
            KeyframeTemporalFlags::default(),
        ),
    }
}

const fn default_out_bezier_handle() -> BezierHandle {
    BezierHandle { time_offset: 1.0 / 3.0, value_offset: 0.0 }
}

const fn default_in_bezier_handle() -> BezierHandle {
    BezierHandle { time_offset: -1.0 / 3.0, value_offset: 0.0 }
}

fn handle_for_out(interpolation: KeyframeInterpolation) -> BezierHandle {
    match interpolation {
        KeyframeInterpolation::Bezier(handle) => BezierHandle {
            time_offset: handle.time_offset.clamp(0.0, 1.0),
            value_offset: handle.value_offset,
        },
        KeyframeInterpolation::Linear => default_out_bezier_handle(),
        KeyframeInterpolation::Hold => BezierHandle { time_offset: 0.0, value_offset: 0.0 },
    }
}

fn handle_for_in(interpolation: KeyframeInterpolation) -> BezierHandle {
    match interpolation {
        KeyframeInterpolation::Bezier(handle) => BezierHandle {
            time_offset: handle.time_offset.clamp(-1.0, 0.0),
            value_offset: handle.value_offset,
        },
        KeyframeInterpolation::Linear => default_in_bezier_handle(),
        KeyframeInterpolation::Hold => BezierHandle { time_offset: 0.0, value_offset: 0.0 },
    }
}

fn extract_handle(interpolation: KeyframeInterpolation) -> Option<Vec2> {
    match interpolation {
        KeyframeInterpolation::Bezier(handle) => Some(Vec2::new(
            handle.time_offset as f32,
            handle.value_offset as f32,
        )),
        _ => None,
    }
}

fn solve_bezier_t(x: f32, cp_out: BezierHandle, cp_in: BezierHandle) -> f32 {
    let x = x.clamp(0.0, 1.0);
    let p1 = Vec2::new(cp_out.time_offset as f32, cp_out.value_offset as f32);
    let p2 = Vec2::new(
        (1.0 + cp_in.time_offset) as f32,
        (1.0 + cp_in.value_offset) as f32,
    );

    let sample_curve_x = |t: f32| cubic_bezier(0.0, p1.x, p2.x, 1.0, t);
    let sample_curve_y = |t: f32| cubic_bezier(0.0, p1.y, p2.y, 1.0, t);
    let sample_curve_derivative_x = |t: f32| cubic_bezier_derivative(0.0, p1.x, p2.x, 1.0, t);

    let mut t = x;
    for _ in 0..8 {
        let error = sample_curve_x(t) - x;
        if error.abs() < 1.0e-5 {
            break;
        }

        let derivative = sample_curve_derivative_x(t);
        if derivative.abs() < 1.0e-5 {
            break;
        }

        t = (t - error / derivative).clamp(0.0, 1.0);
    }

    sample_curve_y(t).clamp(0.0, 1.0)
}

pub fn interpolation_mode_from_keyframe(
    interp_in: KeyframeInterpolation,
    interp_out: KeyframeInterpolation,
    temporal_flags: KeyframeTemporalFlags,
) -> InterpolationType {
    let valid_in = !matches!(interp_in, KeyframeInterpolation::Linear);
    let valid_out = !matches!(interp_out, KeyframeInterpolation::Linear);
    let has_bezier = matches!(interp_in, KeyframeInterpolation::Bezier(_))
        || matches!(interp_out, KeyframeInterpolation::Bezier(_));

    if matches!(interp_in, KeyframeInterpolation::Hold)
        || matches!(interp_out, KeyframeInterpolation::Hold)
    {
        InterpolationType::Hold
    } else if temporal_flags.auto_bezier && has_bezier {
        InterpolationType::AutoBezier
    } else if temporal_flags.continuous && !temporal_flags.broken_handles && has_bezier {
        InterpolationType::ContinuousBezier
    } else if valid_in || valid_out {
        InterpolationType::Bezier
    } else {
        InterpolationType::Linear
    }
}

fn compute_auto_bezier_handles(
    keyframes: &[Keyframe<f64>],
) -> Vec<(KeyframeInterpolation, KeyframeInterpolation)> {
    let count = keyframes.len();
    if count == 0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![(KeyframeInterpolation::Linear, KeyframeInterpolation::Linear)];
    }

    let mut slopes = vec![0.0; count];
    let h = keyframes
        .windows(2)
        .map(|pair| (pair[1].time - pair[0].time).max(1) as f64)
        .collect::<Vec<_>>();
    let delta = keyframes
        .windows(2)
        .zip(h.iter())
        .map(|(pair, dt)| (pair[1].value - pair[0].value) / *dt)
        .collect::<Vec<_>>();

    slopes[0] = endpoint_auto_slope(delta[0], delta.get(1).copied(), h[0], h.get(1).copied());
    slopes[count - 1] = endpoint_auto_slope(
        *delta.last().unwrap_or(&0.0),
        delta.get(delta.len().saturating_sub(2)).copied(),
        *h.last().unwrap_or(&1.0),
        h.get(h.len().saturating_sub(2)).copied(),
    );

    for index in 1..count - 1 {
        slopes[index] = interior_auto_slope(delta[index - 1], delta[index], h[index - 1], h[index]);
    }

    keyframes
        .iter()
        .enumerate()
        .map(|(index, keyframe)| {
            let interp_in = if index == 0 {
                KeyframeInterpolation::Linear
            } else {
                let prev = &keyframes[index - 1];
                let dt = (keyframe.time - prev.time).max(1) as f64;
                let dv = keyframe.value - prev.value;
                KeyframeInterpolation::Bezier(monotone_in_handle(slopes[index], dt, dv))
            };
            let interp_out = if index + 1 >= count {
                KeyframeInterpolation::Linear
            } else {
                let next = &keyframes[index + 1];
                let dt = (next.time - keyframe.time).max(1) as f64;
                let dv = next.value - keyframe.value;
                KeyframeInterpolation::Bezier(monotone_out_handle(slopes[index], dt, dv))
            };
            (interp_in, interp_out)
        })
        .collect()
}

fn endpoint_auto_slope(
    primary_delta: f64,
    secondary_delta: Option<f64>,
    primary_h: f64,
    secondary_h: Option<f64>,
) -> f64 {
    let Some(secondary_delta) = secondary_delta else {
        return primary_delta;
    };
    let secondary_h = secondary_h.unwrap_or(primary_h);
    let mut slope = ((2.0 * primary_h + secondary_h) * primary_delta - primary_h * secondary_delta)
        / (primary_h + secondary_h);
    if slope.signum() != primary_delta.signum() {
        slope = 0.0;
    } else if primary_delta.signum() != secondary_delta.signum()
        && slope.abs() > 3.0 * primary_delta.abs()
    {
        slope = 3.0 * primary_delta;
    }
    slope
}

fn interior_auto_slope(delta_prev: f64, delta_next: f64, h_prev: f64, h_next: f64) -> f64 {
    if delta_prev.abs() < f64::EPSILON
        || delta_next.abs() < f64::EPSILON
        || delta_prev.signum() != delta_next.signum()
    {
        return 0.0;
    }
    let w1 = 2.0 * h_next + h_prev;
    let w2 = h_next + 2.0 * h_prev;
    (w1 + w2) / (w1 / delta_prev + w2 / delta_next)
}

fn continuous_tangent_slope(
    previous: &Keyframe<f64>,
    current: &Keyframe<f64>,
    next: &Keyframe<f64>,
) -> f64 {
    let in_handle = handle_for_in(current.interp_in);
    let out_handle = handle_for_out(current.interp_out);
    let in_slope = tangent_slope_from_in(previous, current, in_handle);
    let out_slope = tangent_slope_from_out(current, next, out_handle);

    match (in_slope, out_slope) {
        (Some(in_slope), Some(out_slope)) => {
            if in_slope.signum() != out_slope.signum() {
                0.0
            } else {
                (in_slope + out_slope) * 0.5
            }
        }
        (Some(in_slope), None) => in_slope,
        (None, Some(out_slope)) => out_slope,
        (None, None) => 0.0,
    }
}

fn tangent_slope_from_out(
    current: &Keyframe<f64>,
    next: &Keyframe<f64>,
    handle: BezierHandle,
) -> Option<f64> {
    let dv = next.value - current.value;
    let dt = (next.time - current.time).max(1) as f64;
    let dx = handle.time_offset.abs().clamp(0.05, 0.95);
    if dv.abs() < f64::EPSILON {
        Some(0.0)
    } else {
        Some(handle.value_offset * dv / (dx * dt))
    }
}

fn tangent_slope_from_in(
    previous: &Keyframe<f64>,
    current: &Keyframe<f64>,
    handle: BezierHandle,
) -> Option<f64> {
    let dv = current.value - previous.value;
    let dt = (current.time - previous.time).max(1) as f64;
    let dx = handle.time_offset.abs().clamp(0.05, 0.95);
    if dv.abs() < f64::EPSILON {
        Some(0.0)
    } else {
        Some(-handle.value_offset * dv / (dx * dt))
    }
}

fn continuous_handle_for_out(
    current: &Keyframe<f64>,
    next: &Keyframe<f64>,
    slope: f64,
    time_offset: f64,
) -> BezierHandle {
    let dt = (next.time - current.time).max(1) as f64;
    let dv = next.value - current.value;
    let time_offset = time_offset.abs().clamp(0.05, 0.95);
    if dv.abs() < f64::EPSILON || slope.abs() < f64::EPSILON {
        BezierHandle { time_offset, value_offset: 0.0 }
    } else {
        BezierHandle {
            time_offset,
            value_offset: (slope * time_offset * dt / dv).clamp(-2.0, 2.0),
        }
    }
}

fn continuous_handle_for_in(
    previous: &Keyframe<f64>,
    current: &Keyframe<f64>,
    slope: f64,
    time_offset: f64,
) -> BezierHandle {
    let dt = (current.time - previous.time).max(1) as f64;
    let dv = current.value - previous.value;
    let time_offset = -time_offset.abs().clamp(0.05, 0.95);
    if dv.abs() < f64::EPSILON || slope.abs() < f64::EPSILON {
        BezierHandle { time_offset, value_offset: 0.0 }
    } else {
        BezierHandle {
            time_offset,
            value_offset: (slope * time_offset * dt / dv).clamp(-2.0, 2.0),
        }
    }
}

fn monotone_out_handle(slope: f64, dt: f64, dv: f64) -> BezierHandle {
    if dv.abs() < f64::EPSILON || slope.abs() < f64::EPSILON {
        return default_out_bezier_handle();
    }
    BezierHandle {
        time_offset: 1.0 / 3.0,
        value_offset: ((slope * dt / dv) / 3.0).clamp(-1.0, 1.0),
    }
}

fn monotone_in_handle(slope: f64, dt: f64, dv: f64) -> BezierHandle {
    if dv.abs() < f64::EPSILON || slope.abs() < f64::EPSILON {
        return default_in_bezier_handle();
    }
    BezierHandle {
        time_offset: -1.0 / 3.0,
        value_offset: (-(slope * dt / dv) / 3.0).clamp(-1.0, 1.0),
    }
}

fn cubic_bezier(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let u = 1.0 - t;
    u * u * u * p0 + 3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t * p3
}

fn cubic_bezier_derivative(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let u = 1.0 - t;
    3.0 * u * u * (p1 - p0) + 6.0 * u * t * (p2 - p1) + 3.0 * t * t * (p3 - p2)
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
        track.set_keyframe(Keyframe::linear(timecode_to_ticks(tc(0)), 0.0));
        track.set_keyframe(Keyframe::linear(timecode_to_ticks(tc(100)), 100.0));

        let mid = track.evaluate(tc(50));
        assert!((mid - 50.0).abs() < 0.01, "Expected 50.0, got {mid}");
    }

    #[test]
    fn property_bag_supports_plugin_style_keyframe_mutation() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "effect.gaussian_blur.radius",
            "模糊半径",
            PropertyValue::Float(8.0),
        ));
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "effect.gaussian_blur.radius".to_string(),
            keyframe: Keyframe::linear(timecode_to_ticks(tc(0)), PropertyValue::Float(8.0)),
        })
        .expect("set start keyframe");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "effect.gaussian_blur.radius".to_string(),
            keyframe: Keyframe::linear(timecode_to_ticks(tc(20)), PropertyValue::Float(28.0)),
        })
        .expect("set end keyframe");

        let value = bag
            .evaluate("effect.gaussian_blur.radius", timecode_to_ticks(tc(10)))
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

    #[test]
    fn bezier_interpolation_uses_control_points() {
        let mut track = KeyframeTrack::<f32>::constant(0.0);
        track.set_keyframe(Keyframe {
            id: KeyframeId::new(),
            time: timecode_to_ticks(tc(0)),
            value: 0.0,
            interp_in: KeyframeInterpolation::Linear,
            interp_out: KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: 0.0,
                value_offset: 0.0,
            }),
            temporal_flags: KeyframeTemporalFlags::default(),
        });
        track.set_keyframe(Keyframe {
            id: KeyframeId::new(),
            time: timecode_to_ticks(tc(100)),
            value: 100.0,
            interp_in: KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: 0.0,
                value_offset: 0.0,
            }),
            interp_out: KeyframeInterpolation::Linear,
            temporal_flags: KeyframeTemporalFlags::default(),
        });

        let mid = track.evaluate(tc(50));
        assert!(
            (mid - 50.0).abs() < 0.05,
            "Expected midpoint to remain stable, got {mid}"
        );
    }

    #[test]
    fn enabling_animation_seeds_initial_keyframe_from_static_value() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.75),
        ));

        bag.apply_mutation(PropertyMutation::EnableAnimation {
            path: "transform.opacity".to_string(),
            time: timecode_to_ticks(tc(12)),
        })
        .expect("enable animation");

        let property = bag.property("transform.opacity").expect("opacity property should exist");
        assert!(property.is_enabled());
        assert_eq!(property.keyframe_times(), vec![timecode_to_ticks(tc(12))]);
        assert_eq!(
            property.channel(0).expect("opacity channel").keyframes()[0].value,
            0.75
        );
    }

    #[test]
    fn write_value_updates_static_value_when_animation_is_disabled() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "effect.gaussian_blur.radius",
            "模糊半径",
            PropertyValue::Float(8.0),
        ));

        bag.apply_mutation(PropertyMutation::WriteValue {
            path: "effect.gaussian_blur.radius".to_string(),
            time: timecode_to_ticks(tc(10)),
            value: PropertyValue::Float(18.0),
            interpolation: InterpolationType::Linear,
        })
        .expect("write static value");

        let property =
            bag.property("effect.gaussian_blur.radius").expect("blur property should exist");
        assert!(!property.is_enabled());
        assert!(property.keyframe_times().is_empty());
        assert_eq!(*property.static_value(), PropertyValue::Float(18.0));
    }

    #[test]
    fn write_value_creates_and_updates_keyframe_when_animation_is_enabled() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.rotation",
            "旋转",
            PropertyValue::Float(0.0),
        ));
        bag.apply_mutation(PropertyMutation::EnableAnimation {
            path: "transform.rotation".to_string(),
            time: timecode_to_ticks(tc(0)),
        })
        .expect("enable animation");

        bag.apply_mutation(PropertyMutation::WriteValue {
            path: "transform.rotation".to_string(),
            time: timecode_to_ticks(tc(8)),
            value: PropertyValue::Float(15.0),
            interpolation: InterpolationType::Linear,
        })
        .expect("create keyframe");
        bag.apply_mutation(PropertyMutation::WriteValue {
            path: "transform.rotation".to_string(),
            time: timecode_to_ticks(tc(8)),
            value: PropertyValue::Float(22.0),
            interpolation: InterpolationType::Hold,
        })
        .expect("update keyframe");

        let property = bag.property("transform.rotation").expect("rotation property should exist");
        assert!(property.is_enabled());
        assert_eq!(property.keyframe_times().len(), 2);
        let keyframe = property
            .channel(0)
            .expect("rotation channel")
            .keyframes()
            .iter()
            .find(|keyframe| keyframe.time == timecode_to_ticks(tc(8)))
            .expect("keyframe at t=8");
        assert_eq!(keyframe.value, 22.0);
        assert_eq!(keyframe.interp_out, KeyframeInterpolation::Linear);
    }

    #[test]
    fn disabling_animation_preserves_keyframes_but_uses_static_value() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.position",
            "位置",
            PropertyValue::Float(0.0),
        ));
        bag.apply_mutation(PropertyMutation::EnableAnimation {
            path: "transform.position".to_string(),
            time: timecode_to_ticks(tc(0)),
        })
        .expect("enable animation");
        bag.apply_mutation(PropertyMutation::WriteValue {
            path: "transform.position".to_string(),
            time: timecode_to_ticks(tc(20)),
            value: PropertyValue::Float(20.0),
            interpolation: InterpolationType::Linear,
        })
        .expect("write animated value");

        bag.apply_mutation(PropertyMutation::DisableAnimation {
            path: "transform.position".to_string(),
            time: timecode_to_ticks(tc(10)),
        })
        .expect("disable animation");

        let property = bag.property("transform.position").expect("position property should exist");
        assert!(!property.is_enabled());
        assert_eq!(property.keyframe_times().len(), 2);
        assert_eq!(
            property.evaluate(timecode_to_ticks(tc(10))),
            PropertyValue::Float(10.0)
        );
        assert_eq!(*property.static_value(), PropertyValue::Float(10.0));
    }

    #[test]
    fn write_channels_updates_only_requested_static_component() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.position",
            "位置",
            PropertyValue::Vec2(Vec2::new(2.0, 5.0)),
        ));

        bag.apply_mutation(PropertyMutation::WriteChannels {
            path: "transform.position".to_string(),
            time: timecode_to_ticks(tc(3)),
            channel_values: vec![(0, 9.0)],
            interpolation: InterpolationType::Linear,
        })
        .expect("write x channel");

        let property = bag.property("transform.position").expect("position property should exist");
        assert_eq!(
            property.evaluate(timecode_to_ticks(tc(3))),
            PropertyValue::Vec2(Vec2::new(9.0, 5.0))
        );
    }

    #[test]
    fn write_channels_creates_independent_channel_keyframes() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.position",
            "位置",
            PropertyValue::Vec2(Vec2::ZERO),
        ));
        bag.apply_mutation(PropertyMutation::EnableAnimation {
            path: "transform.position".to_string(),
            time: timecode_to_ticks(tc(0)),
        })
        .expect("enable animation");

        bag.apply_mutation(PropertyMutation::WriteChannels {
            path: "transform.position".to_string(),
            time: timecode_to_ticks(tc(10)),
            channel_values: vec![(0, 20.0)],
            interpolation: InterpolationType::Linear,
        })
        .expect("write x keyframe");

        let property = bag.property("transform.position").expect("position property should exist");
        assert_eq!(
            property.evaluate(timecode_to_ticks(tc(10))),
            PropertyValue::Vec2(Vec2::new(20.0, 0.0))
        );
        assert_eq!(
            property.evaluate(timecode_to_ticks(tc(5))),
            PropertyValue::Vec2(Vec2::new(10.0, 0.0))
        );
    }

    #[test]
    fn subframe_ticks_interpolate_between_frames() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));

        let start = timecode_to_ticks(tc(0));
        let end = timecode_to_ticks(tc(1));
        let midpoint = start + (end - start) / 2;

        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(start, PropertyValue::Float(0.0)),
        })
        .expect("set start keyframe");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(end, PropertyValue::Float(1.0)),
        })
        .expect("set end keyframe");

        let value = bag
            .evaluate("transform.opacity", midpoint)
            .and_then(|value| value.as_f32())
            .expect("evaluate midpoint");
        assert!((value - 0.5).abs() < 0.001);
    }

    #[test]
    fn writing_existing_keyframe_preserves_keyframe_id() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.rotation",
            "旋转",
            PropertyValue::Float(0.0),
        ));
        let time = timecode_to_ticks(tc(8));

        bag.apply_mutation(PropertyMutation::EnableAnimation {
            path: "transform.rotation".to_string(),
            time,
        })
        .expect("enable animation");
        bag.apply_mutation(PropertyMutation::WriteValue {
            path: "transform.rotation".to_string(),
            time,
            value: PropertyValue::Float(15.0),
            interpolation: InterpolationType::Linear,
        })
        .expect("create keyframe");

        let before_id = bag
            .property("transform.rotation")
            .and_then(|property| property.channel(0))
            .and_then(|channel| channel.keyframes().iter().find(|keyframe| keyframe.time == time))
            .map(|keyframe| keyframe.id)
            .expect("initial keyframe id");

        bag.apply_mutation(PropertyMutation::WriteValue {
            path: "transform.rotation".to_string(),
            time,
            value: PropertyValue::Float(22.0),
            interpolation: InterpolationType::Linear,
        })
        .expect("update keyframe");

        let after_keyframe = bag
            .property("transform.rotation")
            .and_then(|property| property.channel(0))
            .and_then(|channel| channel.keyframes().iter().find(|keyframe| keyframe.time == time))
            .expect("updated keyframe");
        assert_eq!(after_keyframe.id, before_id);
        assert_eq!(after_keyframe.value, 22.0);
    }

    #[test]
    fn set_keyframe_preserves_asymmetric_interpolation_and_flags() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(1.0),
        ));
        let previous_time = timecode_to_ticks(tc(8));
        let time = timecode_to_ticks(tc(12));
        let next_time = timecode_to_ticks(tc(18));

        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(previous_time, PropertyValue::Float(0.25)),
        })
        .expect("set previous keyframe");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(next_time, PropertyValue::Float(0.95)),
        })
        .expect("set next keyframe");

        let keyframe = Keyframe {
            id: KeyframeId::new(),
            time,
            value: PropertyValue::Float(0.75),
            interp_in: KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: -0.25,
                value_offset: -0.1,
            }),
            interp_out: KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: 0.3,
                value_offset: 0.15,
            }),
            temporal_flags: KeyframeTemporalFlags {
                auto_bezier: false,
                continuous: true,
                broken_handles: true,
            },
        };

        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: keyframe.clone(),
        })
        .expect("set keyframe");

        let stored = bag
            .property("transform.opacity")
            .and_then(|property| property.channel(0))
            .and_then(|channel| channel.keyframes().iter().find(|stored| stored.id == keyframe.id))
            .expect("stored keyframe");
        assert_eq!(stored.interp_in, keyframe.interp_in);
        assert_eq!(stored.interp_out, keyframe.interp_out);
        assert_eq!(stored.temporal_flags, keyframe.temporal_flags);
    }

    #[test]
    fn move_keyframe_preserves_identity_and_value() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));
        let start = timecode_to_ticks(tc(5));
        let target = timecode_to_ticks(tc(9));

        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(start, PropertyValue::Float(0.5)),
        })
        .expect("set keyframe");

        let before = bag
            .property("transform.opacity")
            .and_then(|property| property.channel(0))
            .and_then(|channel| channel.keyframes().iter().find(|keyframe| keyframe.time == start))
            .cloned()
            .expect("stored keyframe");

        bag.apply_mutation(PropertyMutation::MoveKeyframe {
            path: "transform.opacity".to_string(),
            from_time: start,
            to_time: target,
        })
        .expect("move keyframe");

        let after = bag
            .property("transform.opacity")
            .and_then(|property| property.channel(0))
            .and_then(|channel| channel.keyframes().iter().find(|keyframe| keyframe.time == target))
            .expect("moved keyframe");
        assert_eq!(after.id, before.id);
        assert_eq!(after.value, before.value);
        assert_eq!(after.interp_in, before.interp_in);
        assert_eq!(after.interp_out, before.interp_out);
    }

    #[test]
    fn move_keyframe_rejects_time_collision() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));
        let first = timecode_to_ticks(tc(5));
        let second = timecode_to_ticks(tc(9));

        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(first, PropertyValue::Float(0.2)),
        })
        .expect("set first keyframe");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(second, PropertyValue::Float(0.8)),
        })
        .expect("set second keyframe");

        let err = bag
            .apply_mutation(PropertyMutation::MoveKeyframe {
                path: "transform.opacity".to_string(),
                from_time: first,
                to_time: second,
            })
            .expect_err("expected collision");
        assert!(err.to_string().contains("关键帧时间冲突"));
    }

    #[test]
    fn update_keyframe_interpolation_changes_existing_keyframe() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));
        let start = timecode_to_ticks(tc(0));
        let time = timecode_to_ticks(tc(5));
        let end = timecode_to_ticks(tc(10));

        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(start, PropertyValue::Float(0.0)),
        })
        .expect("set start keyframe");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(time, PropertyValue::Float(0.5)),
        })
        .expect("set middle keyframe");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(end, PropertyValue::Float(1.0)),
        })
        .expect("set end keyframe");
        bag.apply_mutation(PropertyMutation::UpdateKeyframeInterpolation {
            path: "transform.opacity".to_string(),
            time,
            interpolation: InterpolationType::Hold,
        })
        .expect("update interpolation");

        let stored = bag
            .property("transform.opacity")
            .and_then(|property| property.channel(0))
            .and_then(|channel| channel.keyframes().iter().find(|keyframe| keyframe.time == time))
            .expect("stored keyframe");
        assert_eq!(stored.interp_in, KeyframeInterpolation::Hold);
        assert_eq!(stored.interp_out, KeyframeInterpolation::Hold);
    }

    #[test]
    fn switching_to_continuous_bezier_preserves_existing_handle_lengths() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));
        let start = timecode_to_ticks(tc(0));
        let time = timecode_to_ticks(tc(5));
        let end = timecode_to_ticks(tc(10));

        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(start, PropertyValue::Float(0.0)),
        })
        .expect("set start keyframe");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe {
                id: KeyframeId::new(),
                time,
                value: PropertyValue::Float(0.5),
                interp_in: KeyframeInterpolation::Bezier(BezierHandle {
                    time_offset: -0.2,
                    value_offset: -0.15,
                }),
                interp_out: KeyframeInterpolation::Bezier(BezierHandle {
                    time_offset: 0.45,
                    value_offset: 0.25,
                }),
                temporal_flags: KeyframeTemporalFlags::default(),
            },
        })
        .expect("set middle keyframe");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(end, PropertyValue::Float(1.0)),
        })
        .expect("set end keyframe");

        bag.apply_mutation(PropertyMutation::UpdateKeyframeInterpolation {
            path: "transform.opacity".to_string(),
            time,
            interpolation: InterpolationType::ContinuousBezier,
        })
        .expect("switch to continuous");

        let stored = bag
            .property("transform.opacity")
            .and_then(|property| property.channel(0))
            .and_then(|channel| channel.keyframes().iter().find(|keyframe| keyframe.time == time))
            .expect("stored keyframe");
        let KeyframeInterpolation::Bezier(in_handle) = stored.interp_in else {
            panic!("expected bezier in");
        };
        let KeyframeInterpolation::Bezier(out_handle) = stored.interp_out else {
            panic!("expected bezier out");
        };
        assert!((in_handle.time_offset.abs() - 0.2).abs() < 1e-6);
        assert!((out_handle.time_offset - 0.45).abs() < 1e-6);
        assert!(stored.temporal_flags.continuous);
        assert!(!stored.temporal_flags.broken_handles);
    }

    #[test]
    fn clear_animation_removes_all_keyframes_and_keeps_current_value() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));

        let start = timecode_to_ticks(tc(0));
        let end = timecode_to_ticks(tc(10));
        let mid = timecode_to_ticks(tc(5));

        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(start, PropertyValue::Float(0.0)),
        })
        .expect("set first keyframe");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(end, PropertyValue::Float(1.0)),
        })
        .expect("set second keyframe");

        let before_clear = bag.evaluate("transform.opacity", mid).expect("evaluate before clear");

        bag.apply_mutation(PropertyMutation::ClearAnimation {
            path: "transform.opacity".to_string(),
            time: mid,
        })
        .expect("clear animation");

        let property = bag.property("transform.opacity").expect("property remains");
        assert!(!property.is_enabled());
        assert!(!property.is_animated());
        assert_eq!(property.keyframe_times(), Vec::<TimeTicks>::new());
        assert_eq!(property.static_value(), &before_clear);
    }

    #[test]
    fn ease_presets_use_horizontal_single_side_handles() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));
        let start = timecode_to_ticks(tc(0));
        let end = timecode_to_ticks(tc(8));

        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(start, PropertyValue::Float(0.0)),
        })
        .expect("set start keyframe");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(end, PropertyValue::Float(1.0)),
        })
        .expect("set end keyframe");

        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(start, PropertyValue::Float(0.0)),
        })
        .expect("refresh start keyframe");

        bag.apply_mutation(PropertyMutation::UpdateKeyframeInterpolation {
            path: "transform.opacity".to_string(),
            time: start,
            interpolation: InterpolationType::EaseIn,
        })
        .expect("ease in");
        let ease_in = bag
            .property("transform.opacity")
            .and_then(|property| property.channel(0))
            .and_then(|channel| channel.keyframes().iter().find(|keyframe| keyframe.time == start))
            .cloned()
            .expect("stored ease-in keyframe");
        assert_eq!(ease_in.interp_in, KeyframeInterpolation::Linear);
        assert_eq!(
            ease_in.interp_out,
            KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: 1.0 / 3.0,
                value_offset: 0.0,
            })
        );

        bag.apply_mutation(PropertyMutation::UpdateKeyframeInterpolation {
            path: "transform.opacity".to_string(),
            time: end,
            interpolation: InterpolationType::EaseOut,
        })
        .expect("ease out");
        let ease_out = bag
            .property("transform.opacity")
            .and_then(|property| property.channel(0))
            .and_then(|channel| channel.keyframes().iter().find(|keyframe| keyframe.time == end))
            .cloned()
            .expect("stored ease-out keyframe");
        assert_eq!(
            ease_out.interp_in,
            KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: -1.0 / 3.0,
                value_offset: 0.0,
            })
        );
        assert_eq!(ease_out.interp_out, KeyframeInterpolation::Linear);
    }

    #[test]
    fn update_keyframe_temporal_flags_changes_existing_keyframe() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));
        let time = timecode_to_ticks(tc(5));

        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(time, PropertyValue::Float(0.5)),
        })
        .expect("set keyframe");

        let flags = KeyframeTemporalFlags {
            auto_bezier: true,
            continuous: true,
            broken_handles: false,
        };
        bag.apply_mutation(PropertyMutation::UpdateKeyframeTemporalFlags {
            path: "transform.opacity".to_string(),
            time,
            temporal_flags: flags,
        })
        .expect("update temporal flags");

        let stored = bag
            .property("transform.opacity")
            .and_then(|property| property.channel(0))
            .and_then(|channel| channel.keyframes().iter().find(|keyframe| keyframe.time == time))
            .expect("stored keyframe");
        assert_eq!(stored.temporal_flags, flags);
        assert_eq!(stored.interp_in, KeyframeInterpolation::Linear);
        assert_eq!(stored.interp_out, KeyframeInterpolation::Linear);
    }

    #[test]
    fn updating_channel_handles_clears_auto_flag_for_all_channels_at_time() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.position",
            "位置",
            PropertyValue::Vec2(Vec2::ZERO),
        ));
        let time = timecode_to_ticks(tc(5));
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.position".to_string(),
            keyframe: Keyframe::from_preset(
                time,
                PropertyValue::Vec2(Vec2::new(1.0, 2.0)),
                InterpolationType::AutoBezier,
            ),
        })
        .expect("set auto keyframe");

        bag.apply_mutation(PropertyMutation::UpdateChannelKeyframeHandles {
            path: "transform.position".to_string(),
            time,
            channel_index: 0,
            interp_in: KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: -0.3,
                value_offset: -0.1,
            }),
            interp_out: KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: 0.3,
                value_offset: 0.1,
            }),
        })
        .expect("update x handles");

        let property = bag.property("transform.position").expect("property");
        for channel_index in 0..2 {
            let keyframe = property
                .channel(channel_index)
                .and_then(|channel| channel.keyframe_at(time))
                .expect("channel keyframe");
            assert!(!keyframe.temporal_flags.auto_bezier);
        }
    }

    #[test]
    fn update_channel_keyframe_value_preserves_bezier_mode() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));
        let start = timecode_to_ticks(tc(0));
        let mid = timecode_to_ticks(tc(5));
        let end = timecode_to_ticks(tc(10));

        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(start, PropertyValue::Float(0.0)),
        })
        .expect("set start keyframe");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe {
                id: KeyframeId::new(),
                time: mid,
                value: PropertyValue::Float(0.5),
                interp_in: KeyframeInterpolation::Bezier(BezierHandle {
                    time_offset: -0.25,
                    value_offset: -0.1,
                }),
                interp_out: KeyframeInterpolation::Bezier(BezierHandle {
                    time_offset: 0.25,
                    value_offset: 0.1,
                }),
                temporal_flags: KeyframeTemporalFlags {
                    auto_bezier: false,
                    continuous: true,
                    broken_handles: false,
                },
            },
        })
        .expect("set middle keyframe");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(end, PropertyValue::Float(1.0)),
        })
        .expect("set end keyframe");

        bag.apply_mutation(PropertyMutation::UpdateChannelKeyframeValue {
            path: "transform.opacity".to_string(),
            time: mid,
            channel_index: 0,
            value: 0.8,
        })
        .expect("update keyframe value");

        let stored = bag
            .property("transform.opacity")
            .and_then(|property| property.channel(0))
            .and_then(|channel| channel.keyframes().iter().find(|keyframe| keyframe.time == mid))
            .expect("stored keyframe");
        assert!(matches!(stored.interp_in, KeyframeInterpolation::Bezier(_)));
        assert!(matches!(
            stored.interp_out,
            KeyframeInterpolation::Bezier(_)
        ));
        assert!(stored.temporal_flags.continuous);
        assert!(!stored.temporal_flags.broken_handles);
    }

    #[test]
    fn continuous_bezier_realigns_handles_after_value_change() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));
        let start = timecode_to_ticks(tc(0));
        let mid = timecode_to_ticks(tc(5));
        let end = timecode_to_ticks(tc(12));

        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(start, PropertyValue::Float(0.0)),
        })
        .expect("set start keyframe");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe {
                id: KeyframeId::new(),
                time: mid,
                value: PropertyValue::Float(1.0),
                interp_in: KeyframeInterpolation::Bezier(BezierHandle {
                    time_offset: -0.2,
                    value_offset: -0.3,
                }),
                interp_out: KeyframeInterpolation::Bezier(BezierHandle {
                    time_offset: 0.4,
                    value_offset: 0.4,
                }),
                temporal_flags: KeyframeTemporalFlags {
                    auto_bezier: false,
                    continuous: true,
                    broken_handles: false,
                },
            },
        })
        .expect("set middle keyframe");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(end, PropertyValue::Float(2.0)),
        })
        .expect("set end keyframe");

        bag.apply_mutation(PropertyMutation::UpdateChannelKeyframeValue {
            path: "transform.opacity".to_string(),
            time: mid,
            channel_index: 0,
            value: 0.4,
        })
        .expect("update keyframe value");

        let channel = bag
            .property("transform.opacity")
            .and_then(|property| property.channel(0))
            .expect("channel");
        let previous = channel.keyframe_at(start).expect("previous");
        let current = channel.keyframe_at(mid).expect("current");
        let next = channel.keyframe_at(end).expect("next");
        let in_handle = match current.interp_in {
            KeyframeInterpolation::Bezier(handle) => handle,
            _ => panic!("expected bezier in-handle"),
        };
        let out_handle = match current.interp_out {
            KeyframeInterpolation::Bezier(handle) => handle,
            _ => panic!("expected bezier out-handle"),
        };
        let in_slope = tangent_slope_from_in(previous, current, in_handle).expect("in slope");
        let out_slope = tangent_slope_from_out(current, next, out_handle).expect("out slope");
        assert!((in_slope - out_slope).abs() < 1e-6);
    }

    #[test]
    fn single_keyframe_never_exposes_bezier_handles() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));
        let time = timecode_to_ticks(tc(5));
        let before = timecode_to_ticks(tc(1));
        let after = timecode_to_ticks(tc(9));

        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::from_preset(
                time,
                PropertyValue::Float(0.5),
                InterpolationType::Bezier,
            ),
        })
        .expect("set single keyframe");

        let before_value = bag
            .evaluate("transform.opacity", before)
            .and_then(|value| value.as_f32())
            .expect("evaluate before keyframe");
        let after_value = bag
            .evaluate("transform.opacity", after)
            .and_then(|value| value.as_f32())
            .expect("evaluate after keyframe");
        assert!((before_value - 0.5).abs() < 0.001);
        assert!((after_value - 0.5).abs() < 0.001);
    }

    #[test]
    fn boundary_keyframes_only_keep_handles_on_existing_segments() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));
        let start = timecode_to_ticks(tc(0));
        let end = timecode_to_ticks(tc(10));

        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::from_preset(
                start,
                PropertyValue::Float(0.0),
                InterpolationType::Bezier,
            ),
        })
        .expect("set start keyframe");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::from_preset(
                end,
                PropertyValue::Float(1.0),
                InterpolationType::Bezier,
            ),
        })
        .expect("set end keyframe");

        let channel = bag
            .property("transform.opacity")
            .and_then(|property| property.channel(0))
            .expect("channel");
        let first = channel.keyframes().iter().find(|keyframe| keyframe.time == start).unwrap();
        let last = channel.keyframes().iter().find(|keyframe| keyframe.time == end).unwrap();
        assert!(matches!(first.interp_out, KeyframeInterpolation::Bezier(_)));
        assert!(matches!(last.interp_in, KeyframeInterpolation::Bezier(_)));
        let before_start = bag
            .evaluate(
                "transform.opacity",
                start.saturating_sub(SUBFRAME_TICKS_PER_FRAME),
            )
            .and_then(|value| value.as_f32())
            .expect("evaluate before start");
        let after_end = bag
            .evaluate("transform.opacity", end + SUBFRAME_TICKS_PER_FRAME)
            .and_then(|value| value.as_f32())
            .expect("evaluate after end");
        assert!((before_start - 0.0).abs() < 0.001);
        assert!((after_end - 1.0).abs() < 0.001);
    }

    #[test]
    fn auto_bezier_handles_remain_monotonic_for_monotone_values() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));
        let start = timecode_to_ticks(tc(0));
        let mid = timecode_to_ticks(tc(8));
        let end = timecode_to_ticks(tc(16));

        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(start, PropertyValue::Float(0.0)),
        })
        .expect("set start");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(mid, PropertyValue::Float(0.4)),
        })
        .expect("set mid");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(end, PropertyValue::Float(1.0)),
        })
        .expect("set end");
        bag.apply_mutation(PropertyMutation::UpdateKeyframeInterpolation {
            path: "transform.opacity".to_string(),
            time: mid,
            interpolation: InterpolationType::AutoBezier,
        })
        .expect("auto bezier");

        let stored = bag
            .property("transform.opacity")
            .and_then(|property| property.channel(0))
            .and_then(|channel| channel.keyframes().iter().find(|keyframe| keyframe.time == mid))
            .expect("stored auto keyframe");
        assert!(stored.temporal_flags.auto_bezier);
        let KeyframeInterpolation::Bezier(in_handle) = stored.interp_in else {
            panic!("expected auto in handle");
        };
        let KeyframeInterpolation::Bezier(out_handle) = stored.interp_out else {
            panic!("expected auto out handle");
        };
        assert!(in_handle.time_offset < 0.0);
        assert!(out_handle.time_offset > 0.0);
        assert!((1.0 + in_handle.value_offset).clamp(0.0, 1.0) >= 0.0);
        assert!(out_handle.value_offset.clamp(0.0, 1.0) >= 0.0);
    }

    #[test]
    fn auto_bezier_endpoints_are_not_marked_continuous() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));
        let start = timecode_to_ticks(tc(0));
        let mid = timecode_to_ticks(tc(8));
        let end = timecode_to_ticks(tc(16));

        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(start, PropertyValue::Float(0.0)),
        })
        .expect("set start");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(mid, PropertyValue::Float(0.4)),
        })
        .expect("set mid");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "transform.opacity".to_string(),
            keyframe: Keyframe::linear(end, PropertyValue::Float(1.0)),
        })
        .expect("set end");
        for time in [start, mid, end] {
            bag.apply_mutation(PropertyMutation::UpdateKeyframeInterpolation {
                path: "transform.opacity".to_string(),
                time,
                interpolation: InterpolationType::AutoBezier,
            })
            .expect("auto bezier");
        }

        let channel = bag
            .property("transform.opacity")
            .and_then(|property| property.channel(0))
            .expect("channel");
        let first = channel.keyframe_at(start).expect("first");
        let middle = channel.keyframe_at(mid).expect("middle");
        let last = channel.keyframe_at(end).expect("last");
        assert!(!first.temporal_flags.continuous);
        assert!(middle.temporal_flags.continuous);
        assert!(!last.temporal_flags.continuous);
    }
}
