//! 自动化 / 关键帧基础设施
//!
//! 提供共享的关键帧轨道、动态属性值、属性包和 mutation API，
//! 让内建属性、效果参数和未来插件属性走同一条链路。

use crate::{
    error::{MondrianError, Result},
    types::{AnimationTrackId, AssetId, Color, KeyframeId},
    ParameterId, TimelineTime,
};
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
};

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

/// Persisted interpolation mathematics shared by visual and audio parameters.
///
/// Auto/continuous/ease choices are editor presets that author Bezier handles;
/// they are not distinct execution semantics and therefore do not enter a
/// Parameter Schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParameterInterpolation {
    Hold,
    Linear,
    Bezier,
}

impl InterpolationType {
    fn parameter_interpolation(self) -> ParameterInterpolation {
        match self {
            Self::Hold => ParameterInterpolation::Hold,
            Self::Linear => ParameterInterpolation::Linear,
            Self::Bezier
            | Self::AutoBezier
            | Self::ContinuousBezier
            | Self::EaseIn
            | Self::EaseOut => ParameterInterpolation::Bezier,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum KeyframeInterpolation {
    Hold,
    Linear,
    Bezier(BezierHandle),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BezierHandle {
    pub time_offset: TimelineTime,
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
    Enum(String),
    Resource(ParameterResourceReference),
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
    Enum,
    Resource,
    Text,
}

/// Stable choice exposed by an enum parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParameterEnumOption {
    pub key: String,
    pub message_id: String,
}

impl ParameterEnumOption {
    /// Construct one stable enum key and its default localization address.
    pub fn new(key: impl Into<String>, message_id: impl Into<String>) -> Self {
        Self { key: key.into(), message_id: message_id.into() }
    }
}

/// Recoverable author intent for parameters that select external resources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParameterResourceReference {
    Unbound,
    ProjectAsset { asset_id: AssetId },
    ExternalFile { path: PathBuf },
    Uri { uri: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AnimatablePropertyUiMetadata {
    #[serde(default)]
    pub group_name: Option<String>,
    #[serde(default)]
    pub supports_spatial: bool,
}

/// Versioned, stable identity and invalidation contract for one parameter.
///
/// The schema identity remains stable across UI layout, display-name, and
/// instance-address changes. `PropertyDescriptor::path` is only a current
/// authoring address alias and must not be used as execution identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParameterSchema {
    /// Definition-stable machine identity shared by every instance.
    pub parameter_id: ParameterId,
    /// Version of this parameter's serialized and execution semantics.
    pub schema_version: u32,
    /// Stable localization message identifier; never translated project data.
    pub message_id: String,
    /// Definition-stable value representation.
    pub value_type: PropertyValueType,
    /// Definition default used by reset, missing automation, and migration.
    pub default_value: PropertyValue,
    /// Whether authoring may attach an automation curve.
    pub is_animatable: bool,
    /// Physical/UI interpretation of numeric values.
    pub unit: ParameterUnit,
    /// Enforced numeric bounds and editor stepping, when numeric.
    pub numeric: Option<ParameterNumericContract>,
    /// Interpolation intents admitted by this schema.
    pub allowed_interpolations: Vec<ParameterInterpolation>,
    /// Stable choices for `PropertyValue::Enum`; empty for other value types.
    pub enum_options: Vec<ParameterEnumOption>,
    /// Whether a value change can invalidate rendered or analysed output.
    pub cache_impact: ParameterCacheImpact,
}

impl ParameterSchema {
    /// Construct the first schema revision for one stable parameter identity.
    pub fn v1(parameter_id: ParameterId, default_value: PropertyValue) -> Self {
        let message_id = format!("{}.label", parameter_id.as_str());
        let value_type = default_value.value_type();
        let is_animatable = value_type.supports_animation();
        let allowed_interpolations = if matches!(
            value_type,
            PropertyValueType::Bool
                | PropertyValueType::Int
                | PropertyValueType::Enum
                | PropertyValueType::Resource
                | PropertyValueType::Text
        ) {
            vec![ParameterInterpolation::Hold]
        } else {
            vec![
                ParameterInterpolation::Hold,
                ParameterInterpolation::Linear,
                ParameterInterpolation::Bezier,
            ]
        };
        Self {
            parameter_id,
            schema_version: 1,
            message_id,
            value_type,
            default_value,
            is_animatable,
            unit: ParameterUnit::Unitless,
            numeric: None,
            allowed_interpolations,
            enum_options: Vec::new(),
            cache_impact: ParameterCacheImpact::Value,
        }
    }

    /// Attach a physical interpretation when no bounded numeric contract is needed.
    pub fn with_unit(mut self, unit: ParameterUnit) -> Self {
        self.unit = unit;
        self
    }

    /// Attach one validated numeric contract and unit.
    pub fn with_numeric_contract(
        mut self,
        unit: ParameterUnit,
        numeric: ParameterNumericContract,
    ) -> Self {
        self.unit = unit;
        self.numeric = Some(numeric);
        self
    }

    /// Declare how this parameter participates in semantic cache invalidation.
    pub fn with_cache_impact(mut self, cache_impact: ParameterCacheImpact) -> Self {
        self.cache_impact = cache_impact;
        self
    }

    /// Attach the stable option set used by a discrete enum parameter.
    pub fn with_enum_options(mut self, options: Vec<ParameterEnumOption>) -> Self {
        self.enum_options = options;
        self.allowed_interpolations = vec![ParameterInterpolation::Hold];
        self
    }

    /// Declare whether instances may carry automation.
    pub fn with_animatable(mut self, is_animatable: bool) -> Self {
        self.is_animatable = is_animatable;
        self
    }

    /// Validate metadata that can also arrive from serialized plugin definitions.
    pub fn validate(&self) -> std::result::Result<(), ParameterSchemaError> {
        if self.schema_version == 0 {
            return Err(ParameterSchemaError::ZeroSchemaVersion);
        }
        if self.message_id.trim().is_empty() {
            return Err(ParameterSchemaError::EmptyMessageId);
        }
        if self.allowed_interpolations.is_empty() {
            return Err(ParameterSchemaError::EmptyInterpolationSet);
        }
        for (index, interpolation) in self.allowed_interpolations.iter().enumerate() {
            if self.allowed_interpolations[..index].contains(interpolation) {
                return Err(ParameterSchemaError::DuplicateInterpolation);
            }
        }
        if self.value_type != self.default_value.value_type() {
            return Err(ParameterSchemaError::ValueTypeMismatch);
        }
        if self.is_animatable && !self.value_type.supports_animation() {
            return Err(ParameterSchemaError::UnsupportedAnimationType);
        }
        if matches!(
            self.value_type,
            PropertyValueType::Bool
                | PropertyValueType::Int
                | PropertyValueType::Enum
                | PropertyValueType::Resource
                | PropertyValueType::Text
        ) && self.allowed_interpolations != [ParameterInterpolation::Hold]
        {
            return Err(ParameterSchemaError::InterpolationTypeMismatch);
        }
        if let Some(numeric) = self.numeric {
            numeric.validate()?;
        }
        for (index, option) in self.enum_options.iter().enumerate() {
            if option.key.trim().is_empty() || option.message_id.trim().is_empty() {
                return Err(ParameterSchemaError::InvalidEnumOption);
            }
            if self.enum_options[..index].iter().any(|previous| previous.key == option.key) {
                return Err(ParameterSchemaError::DuplicateEnumOption);
            }
        }
        match (&self.default_value, self.enum_options.is_empty()) {
            (PropertyValue::Enum(key), false) => {
                if !self.enum_options.iter().any(|option| option.key == *key) {
                    return Err(ParameterSchemaError::UnknownEnumDefault);
                }
            }
            (PropertyValue::Enum(_), true) | (_, false) => {
                return Err(ParameterSchemaError::EnumOptionsMismatch);
            }
            _ => {}
        }
        if self.numeric.is_some()
            && matches!(
                self.value_type,
                PropertyValueType::Bool
                    | PropertyValueType::Enum
                    | PropertyValueType::Resource
                    | PropertyValueType::Text
            )
        {
            return Err(ParameterSchemaError::NumericContractTypeMismatch);
        }
        if matches!(self.value_type, PropertyValueType::Resource)
            && !matches!(
                self.cache_impact,
                ParameterCacheImpact::Resource | ParameterCacheImpact::Topology
            )
        {
            return Err(ParameterSchemaError::ResourceCacheImpactRequired);
        }
        for value in self.default_value.to_channel_values() {
            if !value.is_finite() {
                return Err(ParameterSchemaError::NonFiniteDefaultValue);
            }
            if self.numeric.is_some_and(|numeric| !numeric.hard_range.contains(value)) {
                return Err(ParameterSchemaError::DefaultValueOutsideHardRange);
            }
        }
        Ok(())
    }
}

/// Unit and interpretation attached to a numeric parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ParameterUnit {
    #[default]
    Unitless,
    Pixels,
    Normalized,
    Percent,
    Degrees,
    TimelineTime,
    Stops,
    Nits,
    Decibels,
    /// Exact integer audio sample frames on the active Evaluation Grid.
    Samples,
}

/// Closed numeric interval used by hard and soft parameter bounds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ParameterNumericRange {
    pub min: f64,
    pub max: f64,
}

impl ParameterNumericRange {
    /// Construct a finite, ordered closed range.
    pub fn new(min: f64, max: f64) -> std::result::Result<Self, ParameterSchemaError> {
        if !min.is_finite() || !max.is_finite() {
            return Err(ParameterSchemaError::NonFiniteRange);
        }
        if min > max {
            return Err(ParameterSchemaError::ReversedRange);
        }
        Ok(Self { min, max })
    }

    fn contains(self, value: f64) -> bool {
        value >= self.min && value <= self.max
    }
}

/// Numeric editing and validation contract shared by UI and author mutations.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ParameterNumericContract {
    pub hard_range: ParameterNumericRange,
    pub soft_range: ParameterNumericRange,
    pub step: Option<f64>,
    pub invalid_value_policy: ParameterInvalidValuePolicy,
}

impl ParameterNumericContract {
    /// Construct a contract whose soft editor range equals its hard range.
    pub fn closed(
        min: f64,
        max: f64,
        step: Option<f64>,
        invalid_value_policy: ParameterInvalidValuePolicy,
    ) -> std::result::Result<Self, ParameterSchemaError> {
        let range = ParameterNumericRange::new(min, max)?;
        Self::new(range, range, step, invalid_value_policy)
    }

    /// Validate hard/soft ranges and a positive optional editor step.
    pub fn new(
        hard_range: ParameterNumericRange,
        soft_range: ParameterNumericRange,
        step: Option<f64>,
        invalid_value_policy: ParameterInvalidValuePolicy,
    ) -> std::result::Result<Self, ParameterSchemaError> {
        if soft_range.min < hard_range.min || soft_range.max > hard_range.max {
            return Err(ParameterSchemaError::SoftRangeOutsideHardRange);
        }
        if step.is_some_and(|step| !step.is_finite() || step <= 0.0) {
            return Err(ParameterSchemaError::InvalidStep);
        }
        Ok(Self { hard_range, soft_range, step, invalid_value_policy })
    }

    /// Revalidate a deserialized or directly constructed numeric contract.
    pub fn validate(self) -> std::result::Result<(), ParameterSchemaError> {
        let hard_range = ParameterNumericRange::new(self.hard_range.min, self.hard_range.max)?;
        let soft_range = ParameterNumericRange::new(self.soft_range.min, self.soft_range.max)?;
        Self::new(hard_range, soft_range, self.step, self.invalid_value_policy).map(|_| ())
    }
}

/// Policy applied when author input falls outside the hard numeric range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterInvalidValuePolicy {
    Reject,
    Clamp,
}

/// Invalid static or plugin-supplied parameter schema metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParameterSchemaError {
    #[error("parameter schema version must be greater than zero")]
    ZeroSchemaVersion,
    #[error("parameter message ID cannot be empty")]
    EmptyMessageId,
    #[error("parameter must admit at least one interpolation mode")]
    EmptyInterpolationSet,
    #[error("parameter interpolation modes must be unique")]
    DuplicateInterpolation,
    #[error("enum option keys and message IDs cannot be empty")]
    InvalidEnumOption,
    #[error("enum option keys must be unique")]
    DuplicateEnumOption,
    #[error("enum parameters require options and non-enum parameters cannot declare them")]
    EnumOptionsMismatch,
    #[error("enum default is not present in its option set")]
    UnknownEnumDefault,
    #[error("numeric constraints require a numeric property type")]
    NumericContractTypeMismatch,
    #[error("descriptor value type does not match its default value")]
    ValueTypeMismatch,
    #[error("parameter value type cannot be animated")]
    UnsupportedAnimationType,
    #[error("parameter interpolation modes are incompatible with its value type")]
    InterpolationTypeMismatch,
    #[error("parameter default numeric values must be finite")]
    NonFiniteDefaultValue,
    #[error("parameter default value must lie inside its hard range")]
    DefaultValueOutsideHardRange,
    #[error("resource parameters must declare resource cache impact")]
    ResourceCacheImpactRequired,
    #[error("parameter range endpoints must be finite")]
    NonFiniteRange,
    #[error("parameter range minimum exceeds maximum")]
    ReversedRange,
    #[error("parameter soft range must be contained by its hard range")]
    SoftRangeOutsideHardRange,
    #[error("parameter step must be finite and greater than zero")]
    InvalidStep,
}

/// How a parameter participates in semantic cache invalidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterCacheImpact {
    /// Presentation-only metadata that cannot affect generated output.
    None,
    /// The evaluated value changes output while topology remains stable.
    Value,
    /// The value selects an external resource whose revision joins the key.
    Resource,
    /// The value can alter graph topology or execution capabilities.
    Topology,
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
            Self::Enum(_) => "enum",
            Self::Resource(_) => "resource",
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
            Self::Enum(_) => PropertyValueType::Enum,
            Self::Resource(_) => PropertyValueType::Resource,
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
            Self::Enum(_) | Self::Resource(_) | Self::Text(_) => vec![],
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
            PropertyValueType::Enum | PropertyValueType::Resource | PropertyValueType::Text => {
                fallback.clone()
            }
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
            Self::Enum => 1,
            Self::Resource | Self::Text => 0,
        }
    }

    pub fn supports_animation(self) -> bool {
        !matches!(self, Self::Resource | Self::Text)
    }

    pub fn normalized_interpolation(self, interpolation: InterpolationType) -> InterpolationType {
        match self {
            Self::Bool | Self::Int | Self::Enum | Self::Resource | Self::Text => {
                InterpolationType::Hold
            }
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Keyframe<T> {
    pub id: KeyframeId,
    pub time: TimelineTime,
    pub value: T,
    pub interp_in: KeyframeInterpolation,
    pub interp_out: KeyframeInterpolation,
    pub temporal_flags: KeyframeTemporalFlags,
}

impl<T> Keyframe<T> {
    pub fn linear(time: TimelineTime, value: T) -> Self {
        Self::from_preset(time, value, InterpolationType::Linear)
    }

    pub fn from_preset(time: TimelineTime, value: T, interpolation: InterpolationType) -> Self {
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

    pub fn evaluate(&self, time: TimelineTime) -> T {
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
            normalize_time(time, keyframe_a.time, keyframe_b.time),
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

    pub fn remove_keyframe(&mut self, time: TimelineTime) -> Option<Keyframe<T>> {
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertyDescriptor {
    /// Stable parameter schema; independent from instance addressing.
    pub schema: ParameterSchema,
    /// Current authoring/UI address alias. Execution must use `schema.parameter_id`.
    pub path: String,
    pub display_name: String,
    #[serde(default)]
    pub ui_metadata: AnimatablePropertyUiMetadata,
}

impl PropertyDescriptor {
    /// Define a property from an address alias.
    ///
    /// This convenience constructor derives a namespaced identity for local
    /// prototypes and tests. Product definitions must immediately call
    /// [`Self::with_parameter_id`] so address changes cannot rename persisted
    /// parameter identity.
    pub fn new(
        path: impl Into<String>,
        display_name: impl Into<String>,
        default_value: PropertyValue,
    ) -> Self {
        let path = path.into();
        let derived_id = format!("mondrian.property.{path}");
        let parameter_id = ParameterId::new(derived_id.clone()).unwrap_or_else(|error| {
            panic!("property address `{path}` cannot derive `{derived_id}`: {error}")
        });
        let mut schema = ParameterSchema::v1(parameter_id, default_value);
        if matches!(schema.value_type, PropertyValueType::Resource) {
            schema.cache_impact = ParameterCacheImpact::Resource;
        }
        Self {
            schema,
            path,
            display_name: display_name.into(),
            ui_metadata: AnimatablePropertyUiMetadata::default(),
        }
    }

    /// Bind the product definition to an address-independent stable identity.
    pub fn with_parameter_id(mut self, parameter_id: ParameterId) -> Self {
        self.schema.parameter_id = parameter_id;
        self.schema.message_id = format!("{}.label", self.schema.parameter_id.as_str());
        self
    }

    /// Declare how this parameter participates in semantic cache invalidation.
    pub fn with_cache_impact(mut self, cache_impact: ParameterCacheImpact) -> Self {
        self.schema = self.schema.with_cache_impact(cache_impact);
        self
    }

    /// Attach a physical interpretation when no bounded numeric contract is needed.
    pub fn with_unit(mut self, unit: ParameterUnit) -> Self {
        self.schema = self.schema.with_unit(unit);
        self
    }

    /// Attach one validated numeric contract and unit.
    pub fn with_numeric_contract(
        mut self,
        unit: ParameterUnit,
        numeric: ParameterNumericContract,
    ) -> Self {
        self.schema = self.schema.with_numeric_contract(unit, numeric);
        self
    }

    /// Attach the stable option set used by a discrete enum parameter.
    pub fn with_enum_options(mut self, options: Vec<ParameterEnumOption>) -> Self {
        self.schema = self.schema.with_enum_options(options);
        self
    }

    /// Declare whether instances may carry automation.
    pub fn with_animatable(mut self, is_animatable: bool) -> Self {
        self.schema = self.schema.with_animatable(is_animatable);
        self
    }

    /// Stable definition identity used by execution and schema migration.
    pub fn parameter_id(&self) -> &ParameterId {
        &self.schema.parameter_id
    }

    /// Validate the relationship between value type, default value and schema.
    pub fn validate(&self) -> std::result::Result<(), ParameterSchemaError> {
        self.schema.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

    pub fn keyframe_at(&self, time: TimelineTime) -> Option<&Keyframe<f64>> {
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

    pub fn remove_keyframe(&mut self, time: TimelineTime) -> Option<Keyframe<f64>> {
        if let Some(pos) = self.keyframes.iter().position(|keyframe| keyframe.time == time) {
            let removed = self.keyframes.remove(pos);
            self.normalize_keyframes();
            Some(removed)
        } else {
            None
        }
    }

    pub fn evaluate(&self, time: TimelineTime, fallback: f64) -> f64 {
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
        let value_type = descriptor.schema.value_type;
        let static_value = descriptor.schema.default_value.clone();
        let channel_count = if descriptor.schema.is_animatable && value_type.supports_animation() {
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

    /// Fork every owner-local identity while preserving authored values.
    ///
    /// Keyframe identities shared by numeric channels remain shared after the
    /// fork, so a multi-channel key continues to represent one user gesture.
    pub fn fork_author_identities(&mut self) {
        self.track_id = AnimationTrackId::new();
        let mut keyframe_ids = HashMap::<KeyframeId, KeyframeId>::new();
        for channel in &mut self.channels {
            for keyframe in &mut channel.keyframes {
                keyframe.id = *keyframe_ids.entry(keyframe.id).or_default();
            }
        }
    }

    /// Validate persisted author state before it becomes an executable snapshot.
    pub fn validate(&self) -> Result<()> {
        self.descriptor.validate().map_err(|error| MondrianError::WorkflowStepFailed {
            step_id: "parameter_schema_validation".to_string(),
            reason: format!("{}: {error}", self.descriptor.path),
        })?;
        let _ = self.normalize_value(self.descriptor.schema.default_value.clone())?;
        let normalized_static = self.normalize_value(self.static_value.clone())?;
        if normalized_static != self.static_value {
            return Err(parameter_value_error(
                &self.descriptor.path,
                "persisted value lies outside the parameter hard range",
            ));
        }
        let expected_channels =
            if self.descriptor.schema.is_animatable && self.value_type().supports_animation() {
                self.value_type().channel_count()
            } else {
                0
            };
        if self.channels.len() != expected_channels {
            return Err(parameter_value_error(
                &self.descriptor.path,
                "persisted animation channel count does not match the parameter type",
            ));
        }
        let mut keyframe_times = HashMap::<KeyframeId, TimelineTime>::new();
        for (expected_index, channel) in self.channels.iter().enumerate() {
            if channel.index != expected_index {
                return Err(parameter_value_error(
                    &self.descriptor.path,
                    "animation channel indices must be dense and ordered",
                ));
            }
            let mut previous_time = None;
            for keyframe in &channel.keyframes {
                if let Some(existing_time) = keyframe_times.insert(keyframe.id, keyframe.time) {
                    if existing_time != keyframe.time {
                        return Err(parameter_value_error(
                            &self.descriptor.path,
                            "one keyframe identity cannot address different author times",
                        ));
                    }
                }
                if previous_time.is_some_and(|time| time >= keyframe.time) {
                    return Err(parameter_value_error(
                        &self.descriptor.path,
                        "keyframe times must be strictly increasing",
                    ));
                }
                previous_time = Some(keyframe.time);
                let value = self.normalize_numeric_channel(keyframe.value)?;
                if value != keyframe.value {
                    return Err(parameter_value_error(
                        &self.descriptor.path,
                        "persisted keyframe lies outside the parameter hard range",
                    ));
                }
                if matches!(self.value_type(), PropertyValueType::Enum)
                    && (value.fract() != 0.0
                        || value < 0.0
                        || value as usize >= self.descriptor.schema.enum_options.len())
                {
                    return Err(parameter_value_error(
                        &self.descriptor.path,
                        "enum keyframe does not address a declared option",
                    ));
                }
                self.ensure_interpolation_allowed(channel_keyframe_interpolation_type(keyframe))?;
            }
        }
        Ok(())
    }

    pub fn static_value(&self) -> &PropertyValue {
        &self.static_value
    }

    pub fn value_type(&self) -> PropertyValueType {
        self.descriptor.schema.value_type
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

    /// Stable authoring address for this property instance.
    ///
    /// `ParameterId` identifies the shared definition while
    /// `AnimationTrackId` distinguishes this concrete owner-local instance.
    /// The descriptor path remains only a current routing alias.
    pub fn address(&self) -> AnimationParameterAddress {
        AnimationParameterAddress {
            animation_track_id: self.track_id,
            parameter_id: self.descriptor.parameter_id().clone(),
        }
    }

    pub fn keyframe_times(&self) -> Vec<TimelineTime> {
        let mut times = self
            .channels
            .iter()
            .flat_map(|channel| channel.keyframes.iter().map(|keyframe| keyframe.time))
            .collect::<Vec<_>>();
        times.sort_unstable();
        times.dedup();
        times
    }

    /// Shift every key at or after an exact owner-time boundary.
    ///
    /// Key identities, values, interpolation, and handle offsets are retained.
    /// The mutation is atomic and is intended for owners whose coordinate
    /// domain is Sequence time; Clip-local properties must move with their
    /// Clip without calling this method.
    pub fn shift_keyframes_at_or_after(
        &mut self,
        boundary: TimelineTime,
        delta: TimelineTime,
    ) -> Result<()> {
        if delta.is_negative() {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "shift_property_keyframes".to_owned(),
                reason: "keyframe shift delta must not be negative".to_owned(),
            });
        }
        if delta.is_zero() {
            return Ok(());
        }

        let mut candidate = self.clone();
        for channel in &mut candidate.channels {
            for keyframe in &mut channel.keyframes {
                if keyframe.time >= boundary {
                    keyframe.time = keyframe.time.checked_add(delta)?;
                }
            }
            channel.normalize_keyframes();
        }
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Resolve the exact author time currently owned by a stable keyframe ID.
    pub fn keyframe_time_by_id(&self, keyframe_id: KeyframeId) -> Option<TimelineTime> {
        self.channels
            .iter()
            .flat_map(|channel| channel.keyframes())
            .find(|keyframe| keyframe.id == keyframe_id)
            .map(|keyframe| keyframe.time)
    }

    /// Resolve a complete property keyframe by stable identity.
    ///
    /// A complete property key must use the same identity for every populated
    /// numeric channel at its author time. Channel-specific keys are not
    /// representable as one `PropertyValue` key and therefore return `None`.
    pub fn keyframe_by_id(&self, keyframe_id: KeyframeId) -> Option<Keyframe<PropertyValue>> {
        let time = self.keyframe_time_by_id(keyframe_id)?;
        let populated = self
            .channels
            .iter()
            .filter_map(|channel| channel.keyframe_at(time))
            .collect::<Vec<_>>();
        if populated.len() != self.channels.len()
            || populated.iter().any(|keyframe| keyframe.id != keyframe_id)
        {
            return None;
        }
        self.keyframe_at(time)
    }

    pub fn keyframe_at(&self, time: TimelineTime) -> Option<Keyframe<PropertyValue>> {
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

    pub fn set_static_value(&mut self, value: PropertyValue) -> Result<()> {
        self.static_value = self.normalize_value(value)?;
        Ok(())
    }

    pub fn set_animation_enabled(&mut self, enabled: bool) {
        self.animation_enabled = enabled;
    }

    pub fn evaluate(&self, time: TimelineTime) -> PropertyValue {
        if !self.animation_enabled || !self.is_animated() || self.channels.is_empty() {
            return self.static_value.clone();
        }

        let fallback = self
            .value_to_channel_values(&self.static_value)
            .unwrap_or_else(|_| self.static_value.to_channel_values());
        let channels = self
            .channels
            .iter()
            .enumerate()
            .map(|(index, channel)| {
                channel.evaluate(time, fallback.get(index).copied().unwrap_or(0.0))
            })
            .collect::<Vec<_>>();
        self.value_from_channel_values(&channels, &self.static_value)
    }

    pub fn enable_animation(&mut self, time: TimelineTime) -> Result<()> {
        if !self.descriptor.schema.is_animatable || !self.value_type().supports_animation() {
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

    pub fn disable_animation(&mut self, time: TimelineTime) -> Result<()> {
        if !self.descriptor.schema.is_animatable || !self.value_type().supports_animation() {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "property_disable_animation".to_string(),
                reason: format!("属性不支持关键帧: {}", self.descriptor.path),
            });
        }
        self.static_value = self.evaluate(time);
        self.animation_enabled = false;
        Ok(())
    }

    pub fn clear_animation(&mut self, time: TimelineTime) -> Result<()> {
        self.disable_animation(time)?;
        for channel in &mut self.channels {
            channel.keyframes.clear();
        }
        Ok(())
    }

    pub fn write_value(
        &mut self,
        time: TimelineTime,
        value: PropertyValue,
        interpolation: InterpolationType,
        handles: Option<(Option<Vec2>, Option<Vec2>)>,
    ) -> Result<()> {
        let value = self.normalize_value(value)?;
        let channel_values = self.value_to_channel_values(&value)?;
        let updates = channel_values.into_iter().enumerate().collect::<Vec<(usize, f64)>>();
        if self.animation_enabled
            && self.descriptor.schema.is_animatable
            && !self.channels.is_empty()
        {
            self.write_channels(time, &updates, interpolation, handles)
        } else {
            self.static_value = value;
            Ok(())
        }
    }

    pub fn set_exact_keyframe(&mut self, mut keyframe: Keyframe<PropertyValue>) -> Result<()> {
        keyframe.value = self.normalize_value(keyframe.value)?;
        self.ensure_interpolation_allowed(keyframe_interpolation_type(&keyframe))?;
        let channel_values = self.value_to_channel_values(&keyframe.value)?;
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

    /// Atomically edit one complete property key by stable identity.
    ///
    /// Time and value change together against cloned channels, so no
    /// remove-then-insert intermediate state can normalize neighboring
    /// Bezier handles. The stored interpolation, temporal flags, and identity
    /// remain unchanged.
    pub fn edit_keyframe(
        &mut self,
        keyframe_id: KeyframeId,
        time: TimelineTime,
        value: PropertyValue,
    ) -> Result<()> {
        if self.keyframe_by_id(keyframe_id).is_none() {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "property_edit_keyframe".to_owned(),
                reason: format!(
                    "关键帧 {keyframe_id} 不存在或不是完整属性关键帧: {}",
                    self.descriptor.path
                ),
            });
        }

        let value = self.normalize_value(value)?;
        let channel_values = self.value_to_channel_values(&value)?;
        validate_channel_updates(
            self.channel_count(),
            &channel_values
                .iter()
                .enumerate()
                .map(|(index, value)| (index, *value))
                .collect::<Vec<_>>(),
            &self.descriptor.path,
        )?;

        let mut channels = self.channels.clone();
        for (channel, value) in channels.iter_mut().zip(channel_values) {
            if channel
                .keyframes
                .iter()
                .any(|keyframe| keyframe.time == time && keyframe.id != keyframe_id)
            {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "property_edit_keyframe".to_owned(),
                    reason: format!(
                        "关键帧 {keyframe_id} 不能移动到已占用时间 {time}: {}",
                        self.descriptor.path
                    ),
                });
            }
            let keyframe = channel
                .keyframes
                .iter_mut()
                .find(|keyframe| keyframe.id == keyframe_id)
                .ok_or_else(|| MondrianError::WorkflowStepFailed {
                    step_id: "property_edit_keyframe".to_owned(),
                    reason: format!(
                        "关键帧 {keyframe_id} 缺少完整属性通道: {}",
                        self.descriptor.path
                    ),
                })?;
            keyframe.time = time;
            keyframe.value = value;
            channel.keyframes.sort_by_key(|keyframe| keyframe.time);
        }
        self.channels = channels;
        self.normalize_channels();
        Ok(())
    }

    pub fn write_channels(
        &mut self,
        time: TimelineTime,
        channel_values: &[(usize, f64)],
        interpolation: InterpolationType,
        handles: Option<(Option<Vec2>, Option<Vec2>)>,
    ) -> Result<()> {
        validate_channel_updates(self.channel_count(), channel_values, &self.descriptor.path)?;
        let channel_values = channel_values
            .iter()
            .map(|(index, value)| Ok((*index, self.normalize_numeric_channel(*value)?)))
            .collect::<Result<Vec<_>>>()?;

        if self.animation_enabled
            && self.descriptor.schema.is_animatable
            && !self.channels.is_empty()
        {
            let normalized = self.value_type().normalized_interpolation(interpolation);
            self.ensure_interpolation_allowed(normalized.parameter_interpolation())?;
            for (index, value) in &channel_values {
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
                        time_offset: quantize_handle_time(handle.x.clamp(-1.0, 0.0))?,
                        value_offset: handle.y as f64,
                    })
                } else {
                    default_in
                };
                let interp_out = if let Some(handle) = control_out {
                    KeyframeInterpolation::Bezier(BezierHandle {
                        time_offset: quantize_handle_time(handle.x.clamp(0.0, 1.0))?,
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
            let mut static_channels = self.value_to_channel_values(&self.static_value)?;
            for (index, value) in &channel_values {
                if let Some(channel) = static_channels.get_mut(*index) {
                    *channel = *value;
                }
            }
            self.static_value =
                self.value_from_channel_values(&static_channels, &self.static_value);
            Ok(())
        }
    }

    pub fn remove_keyframe(&mut self, time: TimelineTime) {
        for channel in &mut self.channels {
            let _ = channel.remove_keyframe(time);
        }
        self.normalize_channels();
    }

    pub fn move_keyframe(&mut self, from_time: TimelineTime, to_time: TimelineTime) -> Result<()> {
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
        time: TimelineTime,
        interpolation: InterpolationType,
    ) -> Result<()> {
        if !self.descriptor.schema.is_animatable || !self.value_type().supports_animation() {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "property_update_keyframe_interpolation".to_string(),
                reason: format!("属性不支持关键帧: {}", self.descriptor.path),
            });
        }

        let normalized = self.value_type().normalized_interpolation(interpolation);
        self.ensure_interpolation_allowed(normalized.parameter_interpolation())?;
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
        time: TimelineTime,
        channel_index: usize,
        interp_in: KeyframeInterpolation,
        interp_out: KeyframeInterpolation,
    ) -> Result<()> {
        self.ensure_interpolation_allowed(ParameterInterpolation::Bezier)?;
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
        time: TimelineTime,
        channel_index: usize,
        value: f64,
    ) -> Result<()> {
        let value = self.normalize_numeric_channel(value)?;
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
        time: TimelineTime,
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

    fn normalize_value(&self, value: PropertyValue) -> Result<PropertyValue> {
        ensure_value_compatible(
            &self.descriptor.path,
            &self.descriptor.schema.default_value,
            &value,
        )?;
        if let PropertyValue::Enum(key) = &value {
            if !self.descriptor.schema.enum_options.iter().any(|option| option.key == *key) {
                return Err(parameter_value_error(
                    &self.descriptor.path,
                    &format!("unknown enum key `{key}`"),
                ));
            }
        }
        let channels = self.value_to_channel_values(&value)?;
        if channels.is_empty() {
            return Ok(value);
        }
        let normalized = channels
            .iter()
            .map(|value| self.normalize_numeric_channel(*value))
            .collect::<Result<Vec<_>>>()?;
        if self.descriptor.schema.numeric.is_none() {
            return Ok(value);
        }
        Ok(self.value_from_channel_values(&normalized, &value))
    }

    fn value_to_channel_values(&self, value: &PropertyValue) -> Result<Vec<f64>> {
        if let PropertyValue::Enum(key) = value {
            let index = self
                .descriptor
                .schema
                .enum_options
                .iter()
                .position(|option| option.key == *key)
                .ok_or_else(|| {
                    parameter_value_error(
                        &self.descriptor.path,
                        &format!("unknown enum key `{key}`"),
                    )
                })?;
            return Ok(vec![index as f64]);
        }
        Ok(value.to_channel_values())
    }

    fn value_from_channel_values(
        &self,
        channels: &[f64],
        fallback: &PropertyValue,
    ) -> PropertyValue {
        if matches!(self.value_type(), PropertyValueType::Enum) {
            let index = channels.first().copied().unwrap_or(0.0).round();
            if index.is_finite() && index >= 0.0 {
                if let Some(option) = self.descriptor.schema.enum_options.get(index as usize) {
                    return PropertyValue::Enum(option.key.clone());
                }
            }
            return fallback.clone();
        }
        PropertyValue::from_channel_values(self.value_type(), channels, fallback)
    }

    fn normalize_numeric_channel(&self, value: f64) -> Result<f64> {
        if !value.is_finite() {
            return Err(parameter_value_error(
                &self.descriptor.path,
                "numeric values must be finite",
            ));
        }
        let Some(contract) = self.descriptor.schema.numeric else {
            return Ok(value);
        };
        if contract.hard_range.contains(value) {
            return Ok(value);
        }
        match contract.invalid_value_policy {
            ParameterInvalidValuePolicy::Reject => Err(parameter_value_error(
                &self.descriptor.path,
                &format!(
                    "value {value} is outside [{}, {}]",
                    contract.hard_range.min, contract.hard_range.max
                ),
            )),
            ParameterInvalidValuePolicy::Clamp => {
                Ok(value.clamp(contract.hard_range.min, contract.hard_range.max))
            }
        }
    }

    fn ensure_interpolation_allowed(&self, interpolation: ParameterInterpolation) -> Result<()> {
        if self.descriptor.schema.allowed_interpolations.contains(&interpolation) {
            Ok(())
        } else {
            Err(MondrianError::WorkflowStepFailed {
                step_id: "parameter_interpolation_contract".to_string(),
                reason: format!(
                    "parameter {} does not admit {interpolation:?}",
                    self.descriptor.schema.parameter_id
                ),
            })
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
                ensure_value_compatible(&path, &self.descriptor.schema.default_value, &value)?;
                self.set_static_value(value)
            }
            PropertyMutation::SetKeyframe { path, keyframe } => {
                self.ensure_path(&path)?;
                ensure_value_compatible(
                    &path,
                    &self.descriptor.schema.default_value,
                    &keyframe.value,
                )?;
                if !self.descriptor.schema.is_animatable {
                    return Err(MondrianError::WorkflowStepFailed {
                        step_id: "property_set_keyframe".to_string(),
                        reason: format!("属性不支持关键帧: {path}"),
                    });
                }
                self.set_animation_enabled(true);
                self.set_exact_keyframe(keyframe)
            }
            PropertyMutation::EditKeyframe { path, keyframe_id, time, value } => {
                self.ensure_path(&path)?;
                ensure_value_compatible(&path, &self.descriptor.schema.default_value, &value)?;
                self.edit_keyframe(keyframe_id, time, value)
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
                ensure_value_compatible(&path, &self.descriptor.schema.default_value, &value)?;
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

/// Stable authoring address for one visual/property automation instance.
///
/// `ParameterId` is definition identity. `AnimationTrackId` is the stable
/// owner-local instance identity. Property paths are deliberately excluded
/// because they are mutable Adapter aliases rather than author identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AnimationParameterAddress {
    /// Stable owner-local automation instance.
    pub animation_track_id: AnimationTrackId,
    /// Definition-stable Parameter Schema identity.
    pub parameter_id: ParameterId,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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

    /// Resolve one property through its stable instance and schema identity.
    pub fn property_by_address(
        &self,
        address: &AnimationParameterAddress,
    ) -> Option<(&str, &AnimatedProperty)> {
        self.iter().find(|(_, property)| {
            property.track_id == address.animation_track_id
                && property.descriptor.parameter_id() == &address.parameter_id
        })
    }

    /// Resolve the stable instance address currently routed by one path alias.
    pub fn address_for_path(&self, path: &str) -> Option<AnimationParameterAddress> {
        self.property(path).map(AnimatedProperty::address)
    }

    /// Resolve a definition identity only when exactly one instance owns it.
    ///
    /// This supports paste into a different owner. Ambiguous repeated effect
    /// instances fail closed instead of selecting by suffix or collection
    /// order.
    pub fn unique_address_for_parameter_id(
        &self,
        parameter_id: &ParameterId,
    ) -> Option<AnimationParameterAddress> {
        let mut matches = self
            .iter()
            .filter(|(_, property)| property.descriptor.parameter_id() == parameter_id)
            .map(|(_, property)| property.address());
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    }

    pub fn evaluate(&self, path: &str, time: TimelineTime) -> Option<PropertyValue> {
        self.properties.get(path).map(|property| property.evaluate(time))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &AnimatedProperty)> {
        self.properties.iter().map(|(path, property)| (path.as_str(), property))
    }

    /// Shift Sequence-time keys in every property at or after one boundary.
    ///
    /// The complete bag is validated before publication, so one failing
    /// property cannot expose a partially shifted owner.
    pub fn shift_keyframes_at_or_after(
        &mut self,
        boundary: TimelineTime,
        delta: TimelineTime,
    ) -> Result<()> {
        let mut candidate = self.clone();
        for property in candidate.properties.values_mut() {
            property.shift_keyframes_at_or_after(boundary, delta)?;
        }
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Fork all property-owner and keyframe identities in this bag.
    pub fn fork_author_identities(&mut self) {
        for property in self.properties.values_mut() {
            property.fork_author_identities();
        }
    }

    /// Validate every property before accepting deserialized author state.
    pub fn validate(&self) -> Result<()> {
        let mut track_ids = HashSet::with_capacity(self.properties.len());
        for (address, property) in &self.properties {
            if address != &property.descriptor.path {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "property_address_validation".to_string(),
                    reason: format!(
                        "property map key `{address}` does not match descriptor address `{}`",
                        property.descriptor.path
                    ),
                });
            }
            if !track_ids.insert(property.track_id) {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "property_identity_validation".to_owned(),
                    reason: format!(
                        "duplicate animation-track identity {} in one property owner",
                        property.track_id
                    ),
                });
            }
            property.validate()?;
        }
        Ok(())
    }

    pub fn set_static_value(&mut self, path: &str, value: PropertyValue) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.set_static_value(value)
    }

    pub fn set_keyframe(&mut self, path: &str, keyframe: Keyframe<PropertyValue>) -> Result<()> {
        let property = self.require_property_mut(path)?;
        ensure_value_compatible(
            path,
            &property.descriptor.schema.default_value,
            &keyframe.value,
        )?;
        if !property.descriptor.schema.is_animatable {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "property_set_keyframe".to_string(),
                reason: format!("属性不支持关键帧: {path}"),
            });
        }
        property.set_animation_enabled(true);
        property.set_exact_keyframe(keyframe)?;
        Ok(())
    }

    pub fn enable_animation(&mut self, path: &str, time: TimelineTime) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.enable_animation(time)
    }

    pub fn disable_animation(&mut self, path: &str, time: TimelineTime) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.disable_animation(time)
    }

    pub fn clear_animation(&mut self, path: &str, time: TimelineTime) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.clear_animation(time)
    }

    pub fn write_value(
        &mut self,
        path: &str,
        time: TimelineTime,
        value: PropertyValue,
        interpolation: InterpolationType,
    ) -> Result<()> {
        let property = self.require_property_mut(path)?;
        ensure_value_compatible(path, &property.descriptor.schema.default_value, &value)?;
        property.write_value(time, value, interpolation, None)
    }

    pub fn write_channels(
        &mut self,
        path: &str,
        time: TimelineTime,
        channel_values: &[(usize, f64)],
        interpolation: InterpolationType,
    ) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.write_channels(time, channel_values, interpolation, None)
    }

    pub fn remove_keyframe(&mut self, path: &str, time: TimelineTime) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.remove_keyframe(time);
        Ok(())
    }

    pub fn move_keyframe(
        &mut self,
        path: &str,
        from_time: TimelineTime,
        to_time: TimelineTime,
    ) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.move_keyframe(from_time, to_time)
    }

    pub fn update_keyframe_interpolation(
        &mut self,
        path: &str,
        time: TimelineTime,
        interpolation: InterpolationType,
    ) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.update_keyframe_interpolation(time, interpolation)
    }

    /// Atomically edit one complete property key through its stable identity.
    pub fn edit_keyframe(
        &mut self,
        path: &str,
        keyframe_id: KeyframeId,
        time: TimelineTime,
        value: PropertyValue,
    ) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.edit_keyframe(keyframe_id, time, value)
    }

    pub fn update_channel_keyframe_handles(
        &mut self,
        path: &str,
        time: TimelineTime,
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
        time: TimelineTime,
        channel_index: usize,
        value: f64,
    ) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.update_channel_keyframe_value(time, channel_index, value)
    }

    pub fn update_keyframe_temporal_flags(
        &mut self,
        path: &str,
        time: TimelineTime,
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
            PropertyMutation::EditKeyframe { path, keyframe_id, time, value } => {
                self.edit_keyframe(&path, keyframe_id, time, value)
            }
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
    /// Atomically change one complete key's time and value by stable identity.
    EditKeyframe {
        path: String,
        keyframe_id: KeyframeId,
        time: TimelineTime,
        value: PropertyValue,
    },
    RemoveKeyframe {
        path: String,
        time: TimelineTime,
    },
    MoveKeyframe {
        path: String,
        from_time: TimelineTime,
        to_time: TimelineTime,
    },
    UpdateKeyframeInterpolation {
        path: String,
        time: TimelineTime,
        interpolation: InterpolationType,
    },
    UpdateChannelKeyframeHandles {
        path: String,
        time: TimelineTime,
        channel_index: usize,
        interp_in: KeyframeInterpolation,
        interp_out: KeyframeInterpolation,
    },
    UpdateChannelKeyframeValue {
        path: String,
        time: TimelineTime,
        channel_index: usize,
        value: f64,
    },
    UpdateKeyframeTemporalFlags {
        path: String,
        time: TimelineTime,
        temporal_flags: KeyframeTemporalFlags,
    },
    EnableAnimation {
        path: String,
        time: TimelineTime,
    },
    DisableAnimation {
        path: String,
        time: TimelineTime,
    },
    ClearAnimation {
        path: String,
        time: TimelineTime,
    },
    WriteValue {
        path: String,
        time: TimelineTime,
        value: PropertyValue,
        interpolation: InterpolationType,
    },
    WriteChannels {
        path: String,
        time: TimelineTime,
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
            | Self::EditKeyframe { path, .. }
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

    /// Transform the path of this mutation using the given function.
    /// Useful when delegating mutations between nested PropertyBags with different path prefixes.
    pub fn map_path(self, f: impl FnOnce(String) -> String) -> Self {
        use PropertyMutation::*;
        match self {
            DefineProperty(mut desc) => {
                desc.path = f(desc.path);
                DefineProperty(desc)
            }
            SetStaticValue { path, value } => SetStaticValue { path: f(path), value },
            SetKeyframe { path, keyframe } => SetKeyframe { path: f(path), keyframe },
            EditKeyframe { path, keyframe_id, time, value } => {
                EditKeyframe { path: f(path), keyframe_id, time, value }
            }
            RemoveKeyframe { path, time } => RemoveKeyframe { path: f(path), time },
            MoveKeyframe { path, from_time, to_time } => {
                MoveKeyframe { path: f(path), from_time, to_time }
            }
            UpdateKeyframeInterpolation { path, time, interpolation } => {
                UpdateKeyframeInterpolation { path: f(path), time, interpolation }
            }
            UpdateChannelKeyframeHandles { path, time, channel_index, interp_in, interp_out } => {
                UpdateChannelKeyframeHandles {
                    path: f(path),
                    time,
                    channel_index,
                    interp_in,
                    interp_out,
                }
            }
            UpdateChannelKeyframeValue { path, time, channel_index, value } => {
                UpdateChannelKeyframeValue { path: f(path), time, channel_index, value }
            }
            UpdateKeyframeTemporalFlags { path, time, temporal_flags } => {
                UpdateKeyframeTemporalFlags { path: f(path), time, temporal_flags }
            }
            EnableAnimation { path, time } => EnableAnimation { path: f(path), time },
            DisableAnimation { path, time } => DisableAnimation { path: f(path), time },
            ClearAnimation { path, time } => ClearAnimation { path: f(path), time },
            WriteValue { path, time, value, interpolation } => {
                WriteValue { path: f(path), time, value, interpolation }
            }
            WriteChannels { path, time, channel_values, interpolation } => {
                WriteChannels { path: f(path), time, channel_values, interpolation }
            }
            RemoveProperty { path } => RemoveProperty { path: f(path) },
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

fn parameter_value_error(path: &str, reason: &str) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "parameter_value_contract".to_string(),
        reason: format!("invalid value for {path}: {reason}"),
    }
}

fn keyframe_interpolation_type(keyframe: &Keyframe<PropertyValue>) -> ParameterInterpolation {
    interpolation_type_from_parts(
        keyframe.interp_in,
        keyframe.interp_out,
        keyframe.temporal_flags,
    )
}

fn channel_keyframe_interpolation_type(keyframe: &Keyframe<f64>) -> ParameterInterpolation {
    interpolation_type_from_parts(
        keyframe.interp_in,
        keyframe.interp_out,
        keyframe.temporal_flags,
    )
}

fn interpolation_type_from_parts(
    interp_in: KeyframeInterpolation,
    interp_out: KeyframeInterpolation,
    temporal_flags: KeyframeTemporalFlags,
) -> ParameterInterpolation {
    if temporal_flags.auto_bezier || temporal_flags.continuous {
        ParameterInterpolation::Bezier
    } else if matches!(interp_in, KeyframeInterpolation::Hold)
        || matches!(interp_out, KeyframeInterpolation::Hold)
    {
        ParameterInterpolation::Hold
    } else if matches!(interp_in, KeyframeInterpolation::Linear)
        && matches!(interp_out, KeyframeInterpolation::Linear)
    {
        ParameterInterpolation::Linear
    } else {
        ParameterInterpolation::Bezier
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

fn evaluate_numeric_channel(channel: &AnimationChannel, time: TimelineTime, fallback: f64) -> f64 {
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
        normalize_time(time, keyframe_a.time, keyframe_b.time),
        keyframe_a.interp_out,
        keyframe_b.interp_in,
    ) {
        SegmentProgress::Hold => keyframe_a.value,
        SegmentProgress::Progress(t) => f64::lerp(&keyframe_a.value, &keyframe_b.value, t),
    }
}

fn normalize_time(t: TimelineTime, start: TimelineTime, end: TimelineTime) -> f32 {
    let total = end.to_f64() - start.to_f64();
    if total <= 0.0 {
        return 0.0;
    }
    ((t.to_f64() - start.to_f64()) / total) as f32
}

fn quantize_handle_time(value: f32) -> Result<TimelineTime> {
    TimelineTime::from_f64_quantized(f64::from(value), 1_000_000).map_err(|error| {
        MondrianError::WorkflowStepFailed {
            step_id: "automation_handle_time".to_string(),
            reason: error.to_string(),
        }
    })
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

mod interp;
pub use interp::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn tt(frame: i64) -> TimelineTime {
        TimelineTime::new(frame, 25).expect("test timeline time")
    }

    fn ht(value: f64) -> TimelineTime {
        TimelineTime::from_f64_quantized(value, 1_000_000).expect("test handle time")
    }

    #[test]
    fn parameter_schema_survives_address_changes_and_is_required_on_deserialize() {
        let id = ParameterId::new_static("mondrian.test.blur_radius");
        let descriptor = PropertyDescriptor::new(
            "effect.temporary.radius",
            "Radius",
            PropertyValue::Float(4.0),
        )
        .with_parameter_id(id.clone());
        let mut property = AnimatedProperty::from_descriptor(descriptor);
        property.descriptor.path = "effect.instance-renamed.radius_alias".to_string();

        assert_eq!(property.descriptor.parameter_id(), &id);
        assert_eq!(property.descriptor.schema.schema_version, 1);
        assert_eq!(
            property.descriptor.schema.value_type,
            PropertyValueType::Float
        );
        assert_eq!(
            property.descriptor.schema.default_value,
            PropertyValue::Float(4.0)
        );
        assert!(property.descriptor.schema.is_animatable);
        assert_eq!(
            property.descriptor.schema.cache_impact,
            ParameterCacheImpact::Value
        );

        let encoded = serde_json::to_value(&property).expect("serialize property");
        let mut missing_schema = encoded.clone();
        if let Some(mut descriptor) = missing_schema
            .as_object_mut()
            .expect("property object")
            .remove("descriptor")
            .and_then(|descriptor| descriptor.as_object().cloned())
        {
            descriptor.remove("schema");
            missing_schema.as_object_mut().expect("property object").insert(
                "descriptor".to_string(),
                serde_json::Value::Object(descriptor),
            );
        }
        assert!(serde_json::from_value::<AnimatedProperty>(missing_schema).is_err());

        let decoded: AnimatedProperty =
            serde_json::from_value(encoded).expect("deserialize current parameter schema");
        assert_eq!(decoded.descriptor.parameter_id(), &id);
        assert_eq!(
            decoded.descriptor.path,
            "effect.instance-renamed.radius_alias"
        );
    }

    #[test]
    fn editor_interpolation_presets_lower_to_persisted_execution_semantics() {
        assert_eq!(
            InterpolationType::Hold.parameter_interpolation(),
            ParameterInterpolation::Hold
        );
        assert_eq!(
            InterpolationType::Linear.parameter_interpolation(),
            ParameterInterpolation::Linear
        );
        for preset in [
            InterpolationType::Bezier,
            InterpolationType::AutoBezier,
            InterpolationType::ContinuousBezier,
            InterpolationType::EaseIn,
            InterpolationType::EaseOut,
        ] {
            assert_eq!(
                preset.parameter_interpolation(),
                ParameterInterpolation::Bezier
            );
        }
    }

    #[test]
    fn linear_interpolation() {
        let mut track = KeyframeTrack::<f32>::constant(0.0);
        track.set_keyframe(Keyframe::linear(tt(0), 0.0));
        track.set_keyframe(Keyframe::linear(tt(100), 100.0));

        let mid = track.evaluate(tt(50));
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
            keyframe: Keyframe::linear(tt(0), PropertyValue::Float(8.0)),
        })
        .expect("set start keyframe");
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "effect.gaussian_blur.radius".to_string(),
            keyframe: Keyframe::linear(tt(20), PropertyValue::Float(28.0)),
        })
        .expect("set end keyframe");

        let value = bag
            .evaluate("effect.gaussian_blur.radius", tt(10))
            .and_then(|value| value.as_f32())
            .expect("evaluate interpolated value");
        assert!((value - 18.0).abs() < 0.01);
    }

    #[test]
    fn stable_parameter_address_survives_path_alias_changes() {
        let parameter_id = ParameterId::new_static("mondrian.test.stable_amount");
        let mut bag = PropertyBag::default();
        bag.define(
            PropertyDescriptor::new("effect.initial.amount", "Amount", PropertyValue::Float(0.0))
                .with_parameter_id(parameter_id),
        );
        let address = bag.address_for_path("effect.initial.amount").expect("stable address");
        let mut property = bag.properties.remove("effect.initial.amount").expect("property");
        property.descriptor.path = "effect.renamed.amount".to_owned();
        bag.upsert(property);

        let (path, resolved) = bag.property_by_address(&address).expect("resolve by identity");
        assert_eq!(path, "effect.renamed.amount");
        assert_eq!(resolved.address(), address);
        assert!(bag.property("effect.initial.amount").is_none());
    }

    #[test]
    fn edit_keyframe_is_atomic_and_preserves_manual_bezier_identity() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "effect.amount",
            "Amount",
            PropertyValue::Float(0.0),
        ));
        let first = Keyframe::linear(tt(0), PropertyValue::Float(0.0));
        let edited_id = KeyframeId::new();
        let middle = Keyframe {
            id: edited_id,
            time: tt(10),
            value: PropertyValue::Float(0.5),
            interp_in: KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: ht(-0.2),
                value_offset: -0.1,
            }),
            interp_out: KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: ht(0.3),
                value_offset: 0.2,
            }),
            temporal_flags: KeyframeTemporalFlags {
                auto_bezier: false,
                continuous: false,
                broken_handles: true,
            },
        };
        let last = Keyframe::linear(tt(20), PropertyValue::Float(1.0));
        for keyframe in [first, middle.clone(), last] {
            bag.apply_mutation(PropertyMutation::SetKeyframe {
                path: "effect.amount".to_owned(),
                keyframe,
            })
            .expect("seed key");
        }

        bag.apply_mutation(PropertyMutation::EditKeyframe {
            path: "effect.amount".to_owned(),
            keyframe_id: edited_id,
            time: tt(12),
            value: PropertyValue::Float(0.75),
        })
        .expect("edit key");

        let property = bag.property("effect.amount").expect("property");
        assert!(property.keyframe_at(tt(10)).is_none());
        let edited = property.keyframe_by_id(edited_id).expect("edited key");
        assert_eq!(edited.time, tt(12));
        assert_eq!(edited.value, PropertyValue::Float(0.75));
        assert_eq!(edited.id, middle.id);
        assert_eq!(edited.interp_in, middle.interp_in);
        assert_eq!(edited.interp_out, middle.interp_out);
        assert_eq!(edited.temporal_flags, middle.temporal_flags);
    }

    #[test]
    fn edit_keyframe_collision_rejects_without_partial_curve_changes() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "effect.amount",
            "Amount",
            PropertyValue::Float(0.0),
        ));
        let first = Keyframe::linear(tt(5), PropertyValue::Float(0.25));
        let first_id = first.id;
        let second = Keyframe::linear(tt(10), PropertyValue::Float(0.75));
        for keyframe in [first, second] {
            bag.apply_mutation(PropertyMutation::SetKeyframe {
                path: "effect.amount".to_owned(),
                keyframe,
            })
            .expect("seed key");
        }
        let before = bag.clone();

        let error = bag
            .apply_mutation(PropertyMutation::EditKeyframe {
                path: "effect.amount".to_owned(),
                keyframe_id: first_id,
                time: tt(10),
                value: PropertyValue::Float(0.5),
            })
            .expect_err("occupied target must reject");

        assert!(error.to_string().contains("已占用"));
        assert_eq!(bag, before);
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
    fn parameter_numeric_contract_is_enforced_on_every_write_shape() {
        let numeric = ParameterNumericContract::closed(
            0.0,
            1.0,
            Some(0.01),
            ParameterInvalidValuePolicy::Reject,
        )
        .expect("valid numeric contract");
        let descriptor =
            PropertyDescriptor::new("transform.opacity", "Opacity", PropertyValue::Float(1.0))
                .with_numeric_contract(ParameterUnit::Normalized, numeric);
        let mut property = AnimatedProperty::from_descriptor(descriptor);

        assert!(property.set_static_value(PropertyValue::Float(1.1)).is_err());
        assert!(property
            .write_value(
                tt(0),
                PropertyValue::Float(f32::NAN),
                InterpolationType::Linear,
                None,
            )
            .is_err());

        property.enable_animation(tt(0)).expect("enable animation");
        assert!(property
            .write_channels(tt(5), &[(0, -0.1)], InterpolationType::Linear, None)
            .is_err());
        assert_eq!(property.evaluate(tt(5)), PropertyValue::Float(1.0));
    }

    #[test]
    fn clamp_is_an_edit_policy_not_a_persisted_state_escape_hatch() {
        let numeric =
            ParameterNumericContract::closed(0.0, 1.0, None, ParameterInvalidValuePolicy::Clamp)
                .expect("valid numeric contract");
        let descriptor =
            PropertyDescriptor::new("transform.opacity", "Opacity", PropertyValue::Float(1.0))
                .with_numeric_contract(ParameterUnit::Normalized, numeric);
        let mut property = AnimatedProperty::from_descriptor(descriptor);
        property.set_static_value(PropertyValue::Float(2.0)).expect("edit input clamps");
        assert_eq!(property.static_value(), &PropertyValue::Float(1.0));

        property.static_value = PropertyValue::Float(2.0);
        assert!(property
            .validate()
            .expect_err("persisted out-of-range state must fail closed")
            .to_string()
            .contains("persisted value lies outside"));
    }

    #[test]
    fn parameter_interpolation_contract_rejects_unlisted_presets() {
        let mut descriptor =
            PropertyDescriptor::new("effect.step.mode", "Step", PropertyValue::Float(0.0));
        descriptor.schema.allowed_interpolations = vec![ParameterInterpolation::Hold];
        let mut property = AnimatedProperty::from_descriptor(descriptor);
        property.set_animation_enabled(true);

        let error = property
            .write_value(
                tt(0),
                PropertyValue::Float(1.0),
                InterpolationType::Linear,
                None,
            )
            .expect_err("unlisted interpolation must be rejected");
        assert!(error.to_string().contains("does not admit Linear"));
    }

    #[test]
    fn enum_parameter_uses_stable_options_for_hold_keyframes() {
        let descriptor = PropertyDescriptor::new(
            "mask.operation",
            "Operation",
            PropertyValue::Enum("Add".to_string()),
        )
        .with_enum_options(vec![
            ParameterEnumOption::new("Add", "mask.operation.add"),
            ParameterEnumOption::new("Subtract", "mask.operation.subtract"),
        ]);
        descriptor.validate().expect("valid enum descriptor");
        let mut property = AnimatedProperty::from_descriptor(descriptor);
        property.enable_animation(tt(0)).expect("enable enum automation");
        property
            .write_value(
                tt(10),
                PropertyValue::Enum("Subtract".to_string()),
                InterpolationType::Hold,
                None,
            )
            .expect("write discrete keyframe");

        assert_eq!(
            property.evaluate(tt(9)),
            PropertyValue::Enum("Add".to_string())
        );
        assert_eq!(
            property.evaluate(tt(10)),
            PropertyValue::Enum("Subtract".to_string())
        );
        assert!(property
            .write_value(
                tt(20),
                PropertyValue::Enum("Unknown".to_string()),
                InterpolationType::Hold,
                None,
            )
            .is_err());
    }

    #[test]
    fn resource_parameter_is_non_animatable_and_requires_resource_invalidation() {
        let descriptor = PropertyDescriptor::new(
            "lut.path",
            "LUT",
            PropertyValue::Resource(ParameterResourceReference::Unbound),
        );
        assert_eq!(
            descriptor.schema.cache_impact,
            ParameterCacheImpact::Resource
        );
        descriptor.validate().expect("valid resource descriptor");
        let mut property = AnimatedProperty::from_descriptor(descriptor);
        assert!(property.enable_animation(tt(0)).is_err());
    }

    #[test]
    fn bezier_interpolation_uses_control_points() {
        let mut track = KeyframeTrack::<f32>::constant(0.0);
        track.set_keyframe(Keyframe {
            id: KeyframeId::new(),
            time: tt(0),
            value: 0.0,
            interp_in: KeyframeInterpolation::Linear,
            interp_out: KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: TimelineTime::ZERO,
                value_offset: 0.0,
            }),
            temporal_flags: KeyframeTemporalFlags::default(),
        });
        track.set_keyframe(Keyframe {
            id: KeyframeId::new(),
            time: tt(100),
            value: 100.0,
            interp_in: KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: TimelineTime::ZERO,
                value_offset: 0.0,
            }),
            interp_out: KeyframeInterpolation::Linear,
            temporal_flags: KeyframeTemporalFlags::default(),
        });

        let mid = track.evaluate(tt(50));
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
            time: tt(12),
        })
        .expect("enable animation");

        let property = bag.property("transform.opacity").expect("opacity property should exist");
        assert!(property.is_enabled());
        assert_eq!(property.keyframe_times(), vec![tt(12)]);
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
            time: tt(10),
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
            time: tt(0),
        })
        .expect("enable animation");

        bag.apply_mutation(PropertyMutation::WriteValue {
            path: "transform.rotation".to_string(),
            time: tt(8),
            value: PropertyValue::Float(15.0),
            interpolation: InterpolationType::Linear,
        })
        .expect("create keyframe");
        bag.apply_mutation(PropertyMutation::WriteValue {
            path: "transform.rotation".to_string(),
            time: tt(8),
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
            .find(|keyframe| keyframe.time == tt(8))
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
            time: tt(0),
        })
        .expect("enable animation");
        bag.apply_mutation(PropertyMutation::WriteValue {
            path: "transform.position".to_string(),
            time: tt(20),
            value: PropertyValue::Float(20.0),
            interpolation: InterpolationType::Linear,
        })
        .expect("write animated value");

        bag.apply_mutation(PropertyMutation::DisableAnimation {
            path: "transform.position".to_string(),
            time: tt(10),
        })
        .expect("disable animation");

        let property = bag.property("transform.position").expect("position property should exist");
        assert!(!property.is_enabled());
        assert_eq!(property.keyframe_times().len(), 2);
        assert_eq!(property.evaluate(tt(10)), PropertyValue::Float(10.0));
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
            time: tt(3),
            channel_values: vec![(0, 9.0)],
            interpolation: InterpolationType::Linear,
        })
        .expect("write x channel");

        let property = bag.property("transform.position").expect("position property should exist");
        assert_eq!(
            property.evaluate(tt(3)),
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
            time: tt(0),
        })
        .expect("enable animation");

        bag.apply_mutation(PropertyMutation::WriteChannels {
            path: "transform.position".to_string(),
            time: tt(10),
            channel_values: vec![(0, 20.0)],
            interpolation: InterpolationType::Linear,
        })
        .expect("write x keyframe");

        let property = bag.property("transform.position").expect("position property should exist");
        assert_eq!(
            property.evaluate(tt(10)),
            PropertyValue::Vec2(Vec2::new(20.0, 0.0))
        );
        assert_eq!(
            property.evaluate(tt(5)),
            PropertyValue::Vec2(Vec2::new(10.0, 0.0))
        );
    }

    #[test]
    fn exact_subframe_time_interpolates_between_frames() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));

        let start = tt(0);
        let end = tt(1);
        let midpoint = TimelineTime::new(1, 50).expect("valid exact midpoint");

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
        let time = tt(8);

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
        let previous_time = tt(8);
        let time = tt(12);
        let next_time = tt(18);

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
                time_offset: ht(-0.25),
                value_offset: -0.1,
            }),
            interp_out: KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: ht(0.3),
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
        let start = tt(5);
        let target = tt(9);

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
        let first = tt(5);
        let second = tt(9);

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
        let start = tt(0);
        let time = tt(5);
        let end = tt(10);

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
        let start = tt(0);
        let time = tt(5);
        let end = tt(10);

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
                    time_offset: ht(-0.2),
                    value_offset: -0.15,
                }),
                interp_out: KeyframeInterpolation::Bezier(BezierHandle {
                    time_offset: ht(0.45),
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
        assert!((in_handle.time_offset.to_f64().abs() - 0.2).abs() < 1e-6);
        assert!((out_handle.time_offset.to_f64() - 0.45).abs() < 1e-6);
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

        let start = tt(0);
        let end = tt(10);
        let mid = tt(5);

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
        assert_eq!(property.keyframe_times(), Vec::<TimelineTime>::new());
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
        let start = tt(0);
        let end = tt(8);

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
                time_offset: TimelineTime::ONE_THIRD,
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
                time_offset: TimelineTime::NEGATIVE_ONE_THIRD,
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
        let time = tt(5);

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
        let time = tt(5);
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
                time_offset: ht(-0.3),
                value_offset: -0.1,
            }),
            interp_out: KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: ht(0.3),
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
        let start = tt(0);
        let mid = tt(5);
        let end = tt(10);

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
                    time_offset: ht(-0.25),
                    value_offset: -0.1,
                }),
                interp_out: KeyframeInterpolation::Bezier(BezierHandle {
                    time_offset: ht(0.25),
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
        let start = tt(0);
        let mid = tt(5);
        let end = tt(12);

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
                    time_offset: ht(-0.2),
                    value_offset: -0.3,
                }),
                interp_out: KeyframeInterpolation::Bezier(BezierHandle {
                    time_offset: ht(0.4),
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
        let time = tt(5);
        let before = tt(1);
        let after = tt(9);

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
        let start = tt(0);
        let end = tt(10);

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
                start.checked_sub(tt(1)).expect("test time remains in range"),
            )
            .and_then(|value| value.as_f32())
            .expect("evaluate before start");
        let after_end = bag
            .evaluate(
                "transform.opacity",
                end.checked_add(tt(1)).expect("test time remains in range"),
            )
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
        let start = tt(0);
        let mid = tt(8);
        let end = tt(16);

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
        assert!(in_handle.time_offset < TimelineTime::ZERO);
        assert!(out_handle.time_offset > TimelineTime::ZERO);
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
        let start = tt(0);
        let mid = tt(8);
        let end = tt(16);

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
