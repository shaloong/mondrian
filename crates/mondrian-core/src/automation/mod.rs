//! 自动化 / 关键帧基础设施
//!
//! 提供共享的关键帧轨道、动态属性值、属性包和 mutation API，
//! 让内建属性、效果参数和未来插件属性走同一条链路。

mod query;
pub use query::{AutomationQueryError, AutomationQueryTime, PreparedPropertyQuery};

use crate::{
    authoring::AuthoringMap,
    error::{MondrianError, Result},
    types::{AnimationTrackId, AssetId, Color, KeyframeId},
    AuthoringList, ParameterId, ParameterIdError, TimelineTime, TimelineTimeRange,
};
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
#[cfg(test)]
use std::path::PathBuf;

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

/// Temporal intent for one side of a numeric keyframe.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum KeyframeInterpolation {
    /// Keep the left endpoint value throughout the segment.
    Hold,
    /// Use the segment's straight line.
    Linear,
    /// An authored control in exact owner time and parameter-value units.
    Bezier(ExactBezierHandle),
    /// Default flat controls derived from the adjacent owner-time span.
    DefaultBezier,
}

impl KeyframeInterpolation {
    /// Whether this side carries authored or derived Bezier control intent.
    pub const fn is_bezier(self) -> bool {
        matches!(self, Self::Bezier(_) | Self::DefaultBezier)
    }
}

/// The shared owner-time and parameter-value control geometry.
pub use crate::parameter::ExactBezierHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct KeyframeTemporalFlags {
    pub auto_bezier: bool,
    pub continuous: bool,
    pub broken_handles: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PropertyValue {
    Bool(bool),
    Int(i64),
    Float(f32),
    Double(f64),
    /// A double-precision two-dimensional vector.
    DoubleVec2([f64; 2]),
    /// A double-precision three-dimensional vector.
    DoubleVec3([f64; 3]),
    /// Three double-precision RGB components in the declared parameter color domain.
    DoubleRgb([f64; 3]),
    /// Four double-precision RGBA components in the declared parameter color domain.
    DoubleColor([f64; 4]),
    /// Two exact signed 32-bit integer components.
    IntVec2([i32; 2]),
    /// Three exact signed 32-bit integer components.
    IntVec3([i32; 3]),
    Vec2(Vec2),
    Vec3(Vec3),
    Color(Color),
    Vec4([f32; 4]),
    /// A bounded normalized transfer curve edited as stable ordered control points.
    Curve(NormalizedCurve),
    /// A bounded set of include/exclude samples for a three-dimensional color qualifier.
    QualifierSamples(QualifierSampleSet),
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
    /// A double-precision two-dimensional vector.
    DoubleVec2,
    /// A double-precision three-dimensional vector.
    DoubleVec3,
    /// Three double-precision RGB components in the declared parameter color domain.
    DoubleRgb,
    /// Four double-precision RGBA components in the declared parameter color domain.
    DoubleColor,
    /// Two exact signed 32-bit integer components.
    IntVec2,
    /// Three exact signed 32-bit integer components.
    IntVec3,
    Vec2,
    Vec3,
    Color,
    Vec4,
    Curve,
    QualifierSamples,
    Enum,
    Resource,
    Text,
}

/// Maximum authored control points in one normalized transfer curve.
pub const MAX_NORMALIZED_CURVE_POINTS: usize = 32;

/// One finite normalized transfer-curve control point.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NormalizedCurvePoint {
    /// Normalized domain coordinate in `0..=1`.
    pub x: f32,
    /// Normalized output value in `0..=1`.
    pub y: f32,
}

impl NormalizedCurvePoint {
    /// Construct one control point; the owning curve performs validation.
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// A bounded normalized curve with fixed domain endpoints and ordered author points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedCurve {
    points: Vec<NormalizedCurvePoint>,
}

impl NormalizedCurve {
    /// Construct and validate one curve.
    pub fn new(
        points: Vec<NormalizedCurvePoint>,
    ) -> std::result::Result<Self, NormalizedCurveError> {
        let curve = Self { points };
        curve.validate()?;
        Ok(curve)
    }

    /// Linear identity curve.
    pub fn identity() -> Self {
        Self {
            points: vec![
                NormalizedCurvePoint::new(0.0, 0.0),
                NormalizedCurvePoint::new(1.0, 1.0),
            ],
        }
    }

    /// Flat curve used by neutral delta-style controls.
    pub fn flat(value: f32) -> std::result::Result<Self, NormalizedCurveError> {
        Self::new(vec![
            NormalizedCurvePoint::new(0.0, value),
            NormalizedCurvePoint::new(1.0, value),
        ])
    }

    /// Ordered control points including exact x=0 and x=1 endpoints.
    pub fn points(&self) -> &[NormalizedCurvePoint] {
        &self.points
    }

    /// Validate serialized or newly authored curve state.
    pub fn validate(&self) -> std::result::Result<(), NormalizedCurveError> {
        if !(2..=MAX_NORMALIZED_CURVE_POINTS).contains(&self.points.len()) {
            return Err(NormalizedCurveError::PointCount { actual: self.points.len() });
        }
        if self.points.first().is_none_or(|point| point.x != 0.0)
            || self.points.last().is_none_or(|point| point.x != 1.0)
        {
            return Err(NormalizedCurveError::MissingDomainEndpoints);
        }
        for (index, point) in self.points.iter().enumerate() {
            if !point.x.is_finite()
                || !point.y.is_finite()
                || !(0.0..=1.0).contains(&point.x)
                || !(0.0..=1.0).contains(&point.y)
            {
                return Err(NormalizedCurveError::InvalidPoint { index });
            }
            if index > 0 && self.points[index - 1].x >= point.x {
                return Err(NormalizedCurveError::NonIncreasingDomain { index });
            }
        }
        Ok(())
    }
}

/// Invalid normalized curve author state.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NormalizedCurveError {
    /// The curve has fewer than two or more than the supported maximum points.
    #[error("normalized curve requires 2..={MAX_NORMALIZED_CURVE_POINTS} points, got {actual}")]
    PointCount {
        /// Observed number of control points.
        actual: usize,
    },
    /// The first or last control point does not anchor the normalized domain.
    #[error("normalized curve must contain exact x=0 and x=1 endpoints")]
    MissingDomainEndpoints,
    /// One point is non-finite or outside the normalized square.
    #[error("normalized curve point {index} is non-finite or outside [0, 1]")]
    InvalidPoint {
        /// Index of the invalid point.
        index: usize,
    },
    /// Control-point domain coordinates are not strictly increasing.
    #[error("normalized curve point {index} does not have a strictly increasing x coordinate")]
    NonIncreasingDomain {
        /// Index of the first point that does not follow its predecessor.
        index: usize,
    },
}

/// Maximum authored samples retained by one three-dimensional qualifier.
pub const MAX_QUALIFIER_SAMPLES: usize = 16;

/// Boolean operation contributed by one three-dimensional qualifier sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualifierSampleOperation {
    /// Grow the selected color volume around this sample.
    Include,
    /// Remove the selected color volume around this sample.
    Exclude,
}

/// One finite normalized RGB sample in qualifier space.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualifierSample {
    /// Normalized RGB coordinate in `0..=1` for each channel.
    pub rgb: [f32; 3],
    /// Whether this sample includes or excludes its neighbourhood.
    pub operation: QualifierSampleOperation,
}

impl QualifierSample {
    /// Construct one sample; the owning set performs validation.
    pub const fn new(rgb: [f32; 3], operation: QualifierSampleOperation) -> Self {
        Self { rgb, operation }
    }
}

/// Bounded, ordered three-dimensional qualifier samples.
///
/// Order is persisted for stable authoring and UI projection but does not
/// change selection algebra: include samples form a union and exclude samples
/// subtract another union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct QualifierSampleSet(Vec<QualifierSample>);

impl QualifierSampleSet {
    /// Construct and validate one sample set.
    pub fn new(
        samples: Vec<QualifierSample>,
    ) -> std::result::Result<Self, QualifierSampleSetError> {
        let set = Self(samples);
        set.validate()?;
        Ok(set)
    }

    /// Default green-screen include sample.
    pub fn green_screen() -> Self {
        Self(vec![QualifierSample::new(
            [0.0, 1.0, 0.0],
            QualifierSampleOperation::Include,
        )])
    }

    /// Ordered authored samples.
    pub fn samples(&self) -> &[QualifierSample] {
        &self.0
    }

    /// Validate serialized or newly authored sample state.
    pub fn validate(&self) -> std::result::Result<(), QualifierSampleSetError> {
        if !(1..=MAX_QUALIFIER_SAMPLES).contains(&self.0.len()) {
            return Err(QualifierSampleSetError::SampleCount { actual: self.0.len() });
        }
        let mut has_include = false;
        for (index, sample) in self.0.iter().enumerate() {
            if sample
                .rgb
                .iter()
                .any(|channel| !channel.is_finite() || !(0.0..=1.0).contains(channel))
            {
                return Err(QualifierSampleSetError::InvalidSample { index });
            }
            has_include |= sample.operation == QualifierSampleOperation::Include;
        }
        if !has_include {
            return Err(QualifierSampleSetError::MissingIncludeSample);
        }
        Ok(())
    }
}

/// Invalid three-dimensional qualifier sample author state.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QualifierSampleSetError {
    /// The set is empty or exceeds the bounded authoring limit.
    #[error("qualifier requires 1..={MAX_QUALIFIER_SAMPLES} samples, got {actual}")]
    SampleCount {
        /// Observed number of samples.
        actual: usize,
    },
    /// One sample contains a non-finite or out-of-range channel.
    #[error("qualifier sample {index} is non-finite or outside [0, 1]")]
    InvalidSample {
        /// Index of the invalid sample.
        index: usize,
    },
    /// Exclude-only sample sets cannot create a selected volume.
    #[error("qualifier sample set requires at least one include sample")]
    MissingIncludeSample,
}

/// Stable choice exposed by an enum parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParameterEnumOption {
    /// Canonical author value, independent of presentation and locale.
    pub key: String,
    /// Declared option caption used when no translation is available.
    pub display_name: String,
    /// Localization address for this option's caption.
    pub message_id: String,
    /// Optional signed presentation rank; absent ranks preserve schema order.
    /// Ranks never change canonical keys or the ordinal encoding of automation.
    pub display_rank: Option<i32>,
}

impl ParameterEnumOption {
    /// Construct a stable choice with its declared caption and localization address.
    pub fn new(
        key: impl Into<String>,
        display_name: impl Into<String>,
        message_id: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            display_name: display_name.into(),
            message_id: message_id.into(),
            display_rank: None,
        }
    }

    /// Declare this choice's visual rank without changing its author ordinal.
    pub fn with_display_rank(mut self, rank: i32) -> Self {
        self.display_rank = Some(rank);
        self
    }
}

/// Binding families an executable resource provider accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterResourceKind {
    /// A recoverable path on the current machine.
    ExternalFile,
    /// A strong reference to the project asset library.
    ProjectAsset,
    /// A provider-resolved absolute resource URI.
    Uri,
}

/// Native browsing shape for an external resource path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourcePathKind {
    /// Select one file.
    File,
    /// Select one directory.
    Directory,
    /// The provider accepts either shape.
    FileOrDirectory,
}

/// Availability required when choosing a new external path in the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourcePathSelection {
    /// Retain recoverable author intent even while its target is unavailable.
    Reference,
    /// Choose an existing object of the declared shape.
    Existing,
    /// Choose an existing object or a path for a new object of the declared shape.
    ExistingOrNew,
}

/// Shape and selection policy of an external path control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourcePathContract {
    /// File or directory shape admitted by the provider.
    pub kind: ResourcePathKind,
    /// Availability checked for new selections, independently of stored recovery.
    pub selection: ResourcePathSelection,
    /// If present, require a UTF-8 path within this nonzero byte limit.
    /// Absence admits native platform path representations.
    pub max_utf8_bytes: Option<usize>,
}

/// Definition-owned resource admission shared by authoring and UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParameterResourceContract {
    /// Unique admitted nonempty binding families. Unbound is always allowed.
    pub allowed_kinds: Vec<ParameterResourceKind>,
    /// Required exactly when external paths are admitted.
    pub path: Option<ResourcePathContract>,
    /// Lowercase file suffixes without dots; empty admits any extension.
    /// Restrictions require File shape and never test filesystem availability.
    pub file_extensions: Vec<String>,
}
impl ParameterResourceContract {
    /// Admit all reference families for a general-purpose provider.
    pub fn all_bindings() -> Self {
        Self {
            allowed_kinds: vec![
                ParameterResourceKind::ExternalFile,
                ParameterResourceKind::ProjectAsset,
                ParameterResourceKind::Uri,
            ],
            path: Some(ResourcePathContract {
                kind: ResourcePathKind::FileOrDirectory,
                selection: ResourcePathSelection::Reference,
                max_utf8_bytes: None,
            }),
            file_extensions: Vec::new(),
        }
    }
    /// Admit only external files with the declared suffixes.
    pub fn external_file(file_extensions: Vec<String>) -> Self {
        Self {
            allowed_kinds: vec![ParameterResourceKind::ExternalFile],
            path: Some(ResourcePathContract {
                kind: ResourcePathKind::File,
                selection: ResourcePathSelection::Reference,
                max_utf8_bytes: None,
            }),
            file_extensions,
        }
    }
    /// Admit one external path shape with an explicit selection policy.
    pub fn external_path(kind: ResourcePathKind, selection: ResourcePathSelection) -> Self {
        Self {
            allowed_kinds: vec![ParameterResourceKind::ExternalFile],
            path: Some(ResourcePathContract { kind, selection, max_utf8_bytes: None }),
            file_extensions: Vec::new(),
        }
    }
    /// Admit external paths for a UTF-8 processor with an explicit byte limit.
    /// A zero limit is rejected by [`Self::validate`].
    pub fn external_utf8_path(
        kind: ResourcePathKind,
        selection: ResourcePathSelection,
        max_bytes: usize,
    ) -> Self {
        Self {
            allowed_kinds: vec![ParameterResourceKind::ExternalFile],
            path: Some(ResourcePathContract { kind, selection, max_utf8_bytes: Some(max_bytes) }),
            file_extensions: Vec::new(),
        }
    }
    /// Validate a complete provider declaration independently from host availability.
    pub fn validate(&self) -> std::result::Result<(), ParameterSchemaError> {
        if self.allowed_kinds.is_empty()
            || self
                .allowed_kinds
                .iter()
                .enumerate()
                .any(|(index, kind)| self.allowed_kinds[..index].contains(kind))
            || self.path.is_some_and(|path| path.max_utf8_bytes == Some(0))
            || self.path.is_some()
                != self.allowed_kinds.contains(&ParameterResourceKind::ExternalFile)
            || (!self.file_extensions.is_empty()
                && self.path.map(|path| path.kind) != Some(ResourcePathKind::File))
            || self.file_extensions.iter().enumerate().any(|(index, extension)| {
                extension.is_empty()
                    || !extension
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                    || self.file_extensions[..index].contains(extension)
            })
        {
            return Err(ParameterSchemaError::InvalidResourceContract);
        }
        Ok(())
    }
    /// Check author intent without resolving the file, asset or URI.
    pub fn admits(&self, reference: &ParameterResourceReference) -> bool {
        match reference {
            ParameterResourceReference::Unbound => true,
            ParameterResourceReference::ExternalFile { path } => {
                self.allowed_kinds.contains(&ParameterResourceKind::ExternalFile)
                    && !path.is_empty()
                    && !path.contains_nul()
                    && self.path.and_then(|contract| contract.max_utf8_bytes).is_none_or(|limit| {
                        path.utf8_text().is_some_and(|text| text.len() <= limit)
                    })
                    && (self.file_extensions.is_empty()
                        || path.extension_utf8().is_some_and(|extension| {
                            self.file_extensions
                                .iter()
                                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
                        }))
            }
            ParameterResourceReference::ProjectAsset { .. } => {
                self.allowed_kinds.contains(&ParameterResourceKind::ProjectAsset)
            }
            ParameterResourceReference::Uri { uri } => {
                self.allowed_kinds.contains(&ParameterResourceKind::Uri) && !uri.is_empty()
            }
        }
    }
}

/// Recoverable author intent for parameters that select external resources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParameterResourceReference {
    Unbound,
    ProjectAsset { asset_id: AssetId },
    ExternalFile { path: crate::NativePath },
    Uri { uri: String },
}

/// An external path cannot be bound without an unambiguous directory owner.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParameterResourcePathError {
    /// Empty paths do not identify a resource.
    #[error("external resource path is empty")]
    EmptyPath,
    /// Relative input requires an explicitly supplied absolute owner directory.
    #[error("external resource path owner must be absolute")]
    InvalidOwner,
    /// A drive-relative path cannot be resolved using the supplied directory.
    #[error("external resource path remains relative after owner binding")]
    AmbiguousPath,
    /// A foreign platform's path must be preserved or explicitly relinked.
    #[error(transparent)]
    ForeignPlatform(#[from] crate::ForeignNativePathError),
}

impl ParameterResourceReference {
    /// Bind relative external input to an explicit absolute owner directory.
    ///
    /// This is lexical and performs no filesystem access. Native path units
    /// and nonexistent targets are retained using platform PathBuf join semantics.
    /// Absolute references and non-file bindings retain their exact values.
    /// Windows drive-relative paths never consult per-drive process directories.
    pub fn resolve_external_path(
        &mut self,
        owner_directory: &std::path::Path,
    ) -> std::result::Result<(), ParameterResourcePathError> {
        let Self::ExternalFile { path } = self else {
            return Ok(());
        };
        let native = path.native_path()?;
        if native.as_os_str().is_empty() {
            return Err(ParameterResourcePathError::EmptyPath);
        }
        if native.is_absolute() {
            return Ok(());
        }
        if !owner_directory.is_absolute() {
            return Err(ParameterResourcePathError::InvalidOwner);
        }
        let resolved = owner_directory.join(native);
        if !resolved.is_absolute() {
            return Err(ParameterResourcePathError::AmbiguousPath);
        }
        *path = resolved.into();
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AnimatablePropertyUiMetadata {
    #[serde(default)]
    pub group_name: Option<String>,
    #[serde(default)]
    pub supports_spatial: bool,
    /// Definition-owned order within one Inspector group.
    ///
    /// `None` preserves deterministic address ordering for older author data.
    #[serde(default)]
    pub display_order: Option<u32>,
}

/// Definition-owned text editor shape, independent from the current value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterTextMode {
    /// A compact input for a single textual field.
    SingleLine,
    /// A text area that admits line breaks, including when the value is empty.
    MultiLine,
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
    /// Complete ordered per-channel bounds and editor stepping, or no bounds.
    /// A present list must contain exactly one contract per numeric value channel.
    pub numeric: Option<AuthoringList<ParameterNumericContract>>,
    /// Required for text parameters; absent for every other value type.
    pub text_mode: Option<ParameterTextMode>,
    /// Required exactly for Resource parameters; governs author and picker admission.
    pub resource: Option<ParameterResourceContract>,
    /// Ordered interpolation intents; the first is the default for new keys.
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
                | PropertyValueType::IntVec2
                | PropertyValueType::IntVec3
                | PropertyValueType::Curve
                | PropertyValueType::QualifierSamples
                | PropertyValueType::Enum
                | PropertyValueType::Resource
                | PropertyValueType::Text
        ) {
            vec![ParameterInterpolation::Hold]
        } else {
            vec![
                ParameterInterpolation::Linear,
                ParameterInterpolation::Hold,
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
            text_mode: (value_type == PropertyValueType::Text)
                .then_some(ParameterTextMode::SingleLine),
            allowed_interpolations,
            resource: (value_type == PropertyValueType::Resource)
                .then(ParameterResourceContract::all_bindings),
            enum_options: Vec::new(),
            cache_impact: ParameterCacheImpact::Value,
        }
    }

    /// Attach a physical interpretation when no bounded numeric contract is needed.
    pub fn with_unit(mut self, unit: ParameterUnit) -> Self {
        self.unit = unit;
        self
    }

    /// Declare the editor shape for a text parameter.
    pub fn with_text_mode(mut self, mode: ParameterTextMode) -> Self {
        self.text_mode = Some(mode);
        self
    }

    /// Declare the reference and native browsing shapes accepted by this provider.
    pub fn with_resource_contract(mut self, resource: ParameterResourceContract) -> Self {
        self.resource = Some(resource);
        self
    }

    /// Attach one validated numeric contract and unit.
    pub fn with_numeric_contract(
        mut self,
        unit: ParameterUnit,
        numeric: ParameterNumericContract,
    ) -> Self {
        self.unit = unit;
        self.numeric =
            Some(std::iter::repeat_n(numeric, self.value_type.channel_count().max(1)).collect());
        self
    }

    /// Declare complete per-channel numeric bounds without a scalar fallback.
    pub fn with_channel_numeric_contracts(
        mut self,
        unit: ParameterUnit,
        numeric: impl Into<AuthoringList<ParameterNumericContract>>,
    ) -> Self {
        self.unit = unit;
        self.numeric = Some(numeric.into());
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
        if (self.value_type == PropertyValueType::Text) != self.text_mode.is_some() {
            return Err(ParameterSchemaError::TextModeTypeMismatch);
        }
        if (self.value_type == PropertyValueType::Resource) != self.resource.is_some() {
            return Err(ParameterSchemaError::ResourceContractTypeMismatch);
        }
        if let Some(resource) = &self.resource {
            resource.validate()?;
            if !matches!(&self.default_value, PropertyValue::Resource(reference) if resource.admits(reference))
            {
                return Err(ParameterSchemaError::InvalidResourceDefault);
            }
        }
        if self.is_animatable && !self.value_type.supports_animation() {
            return Err(ParameterSchemaError::UnsupportedAnimationType);
        }
        if matches!(
            self.value_type,
            PropertyValueType::Bool
                | PropertyValueType::Curve
                | PropertyValueType::QualifierSamples
                | PropertyValueType::Enum
                | PropertyValueType::Resource
                | PropertyValueType::Text
        ) && self.allowed_interpolations != [ParameterInterpolation::Hold]
        {
            return Err(ParameterSchemaError::InterpolationTypeMismatch);
        }
        if let Some(contracts) = &self.numeric {
            if matches!(
                self.value_type,
                PropertyValueType::Bool
                    | PropertyValueType::Curve
                    | PropertyValueType::QualifierSamples
                    | PropertyValueType::Enum
                    | PropertyValueType::Resource
                    | PropertyValueType::Text
            ) {
                return Err(ParameterSchemaError::NumericContractTypeMismatch);
            }
            if contracts.len() != self.value_type.channel_count() {
                return Err(ParameterSchemaError::NumericChannelCountMismatch {
                    expected: self.value_type.channel_count(),
                    actual: contracts.len(),
                });
            }
            for numeric in contracts {
                numeric.validate()?;
                if matches!(
                    self.value_type,
                    PropertyValueType::IntVec2 | PropertyValueType::IntVec3
                ) && [
                    numeric.hard_range.min,
                    numeric.hard_range.max,
                    numeric.soft_range.min,
                    numeric.soft_range.max,
                ]
                .into_iter()
                .any(|value| {
                    value.fract() != 0.0
                        || !(f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&value)
                }) {
                    return Err(ParameterSchemaError::IntegerVectorRange);
                }
            }
        }
        if self.value_type == PropertyValueType::Int
            && self.allowed_interpolations != [ParameterInterpolation::Hold]
        {
            // Continuous execution projects through f64; integer interpolation must bound
            // every value to the exact integer domain rather than silently round
            // wide i64 author values during channel lowering.
            if matches!(self.default_value, PropertyValue::Int(value) if value.unsigned_abs() > (1_u64 << 53))
            {
                return Err(ParameterSchemaError::DefaultValueOutsideHardRange);
            }
            let exact_limit = (1_u64 << 53) as f64;
            if self.numeric.as_ref().is_none_or(|contracts| {
                contracts.iter().any(|numeric| {
                    numeric.hard_range.min < -exact_limit || numeric.hard_range.max > exact_limit
                })
            }) {
                return Err(ParameterSchemaError::InterpolationTypeMismatch);
            }
        }
        let mut choice_keys = std::collections::BTreeSet::new();
        let mut display_ranks = std::collections::BTreeSet::new();
        for option in &self.enum_options {
            if option.key.trim().is_empty()
                || option.display_name.trim().is_empty()
                || option.message_id.trim().is_empty()
            {
                return Err(ParameterSchemaError::InvalidEnumOption);
            }
            if !choice_keys.insert(option.key.as_str()) {
                return Err(ParameterSchemaError::DuplicateEnumOption);
            }
            if option.display_rank.is_some() != self.enum_options[0].display_rank.is_some()
                || option.display_rank.is_some_and(|rank| !display_ranks.insert(rank))
            {
                return Err(ParameterSchemaError::InvalidEnumDisplayOrder);
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
        if matches!(self.value_type, PropertyValueType::Resource)
            && !matches!(
                self.cache_impact,
                ParameterCacheImpact::Resource | ParameterCacheImpact::Topology
            )
        {
            return Err(ParameterSchemaError::ResourceCacheImpactRequired);
        }
        match &self.default_value {
            PropertyValue::Curve(curve) if curve.validate().is_err() => {
                return Err(ParameterSchemaError::InvalidCurveDefaultValue);
            }
            PropertyValue::QualifierSamples(samples) if samples.validate().is_err() => {
                return Err(ParameterSchemaError::InvalidQualifierSamplesDefaultValue);
            }
            _ => {}
        }
        for (index, value) in self.default_value.to_channel_values().into_iter().enumerate() {
            if !value.is_finite() {
                return Err(ParameterSchemaError::NonFiniteDefaultValue);
            }
            if self
                .numeric
                .as_ref()
                .and_then(|contracts| contracts.get(index))
                .is_some_and(|numeric| !numeric.hard_range.contains(value))
            {
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
    /// Milliseconds in the owning execution domain.
    Milliseconds,
    /// Exact integer audio sample frames on the active Evaluation Grid.
    Samples,
}

/// Closed numeric interval used by hard and soft parameter bounds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ParameterNumericRange {
    pub min: f64,
    pub max: f64,
}

/// Interpolate finite numeric endpoints without requiring a finite difference.
/// Unit-interval positions retain convex values and exact endpoints, in either
/// direction. Extrapolation keeps its arithmetic meaning for Bezier overshoot.
pub(crate) fn interpolate_numeric_value(left: f64, right: f64, position: f64) -> f64 {
    if position == 0.0 {
        return left;
    }
    if position == 1.0 {
        return right;
    }
    if left == right {
        return left;
    }
    if (left <= 0.0 && right >= 0.0) || (left >= 0.0 && right <= 0.0) {
        if position <= 0.5 {
            left.mul_add(1.0 - position, right * position)
        } else {
            right.mul_add(position, left * (1.0 - position))
        }
    } else {
        (right - left).mul_add(position, left)
    }
}

/// Resolve a value difference relative to a finite positive scale without
/// overflowing the difference, retaining close-value subtraction precision.
pub(crate) fn scaled_numeric_difference(right: f64, left: f64, scale: f64) -> f64 {
    let difference = right - left;
    if difference.is_finite() {
        difference / scale
    } else {
        right / scale - left / scale
    }
}

/// Resolve the monotone two-sided tangent through bounded slope ratios rather
/// than potentially overflowing reciprocals. Opposite or zero secants flatten.
pub(crate) fn monotone_harmonic_slope(
    left: f64,
    right: f64,
    left_span: f64,
    right_span: f64,
) -> f64 {
    if left == 0.0 || right == 0.0 || left.signum() != right.signum() {
        return 0.0;
    }
    let left_weight = 2.0 * right_span + left_span;
    let right_weight = right_span + 2.0 * left_span;
    let left_fraction = left_weight / (left_weight + right_weight);
    let magnitude = left.abs().min(right.abs());
    let denominator = left_fraction * (magnitude / left.abs())
        + (1.0 - left_fraction) * (magnitude / right.abs());
    (magnitude / denominator).copysign(left)
}

impl ParameterNumericRange {
    /// Project a finite value into the unit interval without overflowing the span.
    /// Values outside this editor interval project to its endpoints; a fixed
    /// interval projects to its midpoint. Invalid ranges or values are rejected.
    pub fn normalized_position(self, value: f64) -> Option<f64> {
        Self::new(self.min, self.max).ok()?;
        if !value.is_finite() {
            return None;
        }
        if self.min == self.max {
            return Some(0.5);
        }
        if value <= self.min {
            return Some(0.0);
        }
        if value >= self.max {
            return Some(1.0);
        }
        let span = self.max - self.min;
        let position = if span.is_finite() {
            (value - self.min) / span
        } else {
            (value * 0.5 - self.min * 0.5) / (self.max * 0.5 - self.min * 0.5)
        };
        Some(position.clamp(0.0, 1.0))
    }

    /// Resolve a finite unit-interval position while retaining exact endpoints.
    /// Opposite-sign bounds use weighted interpolation to avoid overflowing
    /// their difference. A fixed interval always resolves to its author value.
    pub fn value_at_normalized_position(self, position: f64) -> Option<f64> {
        Self::new(self.min, self.max).ok()?;
        if !position.is_finite() || !(0.0..=1.0).contains(&position) {
            return None;
        }
        let value = interpolate_numeric_value(self.min, self.max, position);
        Some(value.clamp(self.min, self.max))
    }

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

    fn integer_bounds(self) -> Option<(i64, i64)> {
        let range = self.hard_range;
        if range.min >= -(i64::MIN as f64) || range.max < i64::MIN as f64 {
            return None;
        }
        let min = range.min.ceil() as i64;
        let max = range.max.floor() as i64;
        (min <= max).then_some((min, max))
    }

    /// Offset a canonical scalar and constrain it to representable hard bounds.
    ///
    /// Integer increments round once and multiply/add in widened integer arithmetic;
    /// separate signed step counts preserve oversized coarse increments. The source
    /// integer never passes through floating-point geometry. Float
    /// bounds select inward representable values. Soft bounds and editor step
    /// do not change this explicit author-unit increment. Invalid contracts,
    /// non-finite increments and domains without a legal scalar return `None`.
    pub fn offset_scalar(
        self,
        value: PropertyValue,
        delta: f64,
        steps: i8,
    ) -> Option<PropertyValue> {
        self.validate().ok()?;
        if !delta.is_finite() {
            return None;
        }
        let range = self.hard_range;
        match value {
            PropertyValue::Int(value) => {
                let (min, max) = self.integer_bounds()?;
                if !(min..=max).contains(&value) {
                    return None;
                }
                let target = i128::from(value)
                    .saturating_add((delta.round() as i128).saturating_mul(i128::from(steps)));
                Some(PropertyValue::Int(
                    target.clamp(i128::from(min), i128::from(max)) as i64,
                ))
            }
            PropertyValue::Float(value) => {
                if !value.is_finite() || !range.contains(f64::from(value)) {
                    return None;
                }
                let mut min = range.min.max(-f64::from(f32::MAX)) as f32;
                let mut max = range.max.min(f64::from(f32::MAX)) as f32;
                if f64::from(min) < range.min {
                    min = min.next_up();
                }
                if f64::from(max) > range.max {
                    max = max.next_down();
                }
                if !min.is_finite() || !max.is_finite() || min > max {
                    return None;
                }
                if delta == 0.0 || steps == 0 {
                    return Some(PropertyValue::Float(value));
                }
                Some(PropertyValue::Float(
                    delta
                        .mul_add(f64::from(steps), f64::from(value))
                        .clamp(f64::from(min), f64::from(max)) as f32,
                ))
            }
            PropertyValue::Double(value) => {
                if !value.is_finite() || !range.contains(value) {
                    return None;
                }
                if delta == 0.0 || steps == 0 {
                    return Some(PropertyValue::Double(value));
                }
                Some(PropertyValue::Double(
                    delta.mul_add(f64::from(steps), value).clamp(range.min, range.max),
                ))
            }
            _ => None,
        }
    }

    /// Move a nonempty homogeneous scalar group with one common bounded increment.
    /// Integer source values remain exact; floating increments use fused arithmetic.
    /// A boundary reduces the whole group's movement rather than clamping keys separately.
    /// Invalid contracts, values, mixed types or non-finite increments reject the group.
    pub fn offset_scalars(
        self,
        values: &[PropertyValue],
        delta: f64,
        steps: i8,
    ) -> Option<Vec<PropertyValue>> {
        self.validate().ok()?;
        if values.is_empty() || !delta.is_finite() {
            return None;
        }
        if matches!(values.first(), Some(PropertyValue::Int(_))) {
            let (min, max) = self.integer_bounds()?;
            let mut lower = i128::MIN;
            let mut upper = i128::MAX;
            for value in values {
                let PropertyValue::Int(value) = value else {
                    return None;
                };
                if !(min..=max).contains(value) {
                    return None;
                }
                lower = lower.max(i128::from(min) - i128::from(*value));
                upper = upper.min(i128::from(max) - i128::from(*value));
            }
            let requested = (delta.round() as i128).saturating_mul(i128::from(steps));
            let offset = requested.clamp(lower, upper);
            return values
                .iter()
                .map(|value| {
                    let PropertyValue::Int(value) = value else {
                        return None;
                    };
                    i64::try_from(i128::from(*value) + offset).ok().map(PropertyValue::Int)
                })
                .collect();
        }
        let is_float = matches!(values.first(), Some(PropertyValue::Float(_)));
        let (min, max) = if is_float {
            let mut min = self.hard_range.min.max(-f64::from(f32::MAX)) as f32;
            let mut max = self.hard_range.max.min(f64::from(f32::MAX)) as f32;
            if f64::from(min) < self.hard_range.min {
                min = min.next_up();
            }
            if f64::from(max) > self.hard_range.max {
                max = max.next_down();
            }
            if !min.is_finite() || !max.is_finite() || min > max {
                return None;
            }
            (f64::from(min), f64::from(max))
        } else {
            (self.hard_range.min, self.hard_range.max)
        };
        let mut lower_steps = f64::NEG_INFINITY;
        let mut upper_steps = f64::INFINITY;
        let mut lower_offset = f64::NEG_INFINITY;
        let mut upper_offset = f64::INFINITY;
        for value in values {
            let value = match value {
                PropertyValue::Float(value) if is_float => f64::from(*value),
                PropertyValue::Double(value) if !is_float => *value,
                _ => return None,
            };
            if !value.is_finite() || !(min..=max).contains(&value) {
                return None;
            }
            lower_offset = lower_offset.max(min - value);
            upper_offset = upper_offset.min(max - value);
            if delta != 0.0 {
                let low = scalar_difference_ratio(min, value, delta);
                let high = scalar_difference_ratio(max, value, delta);
                lower_steps = lower_steps.max(low.min(high));
                upper_steps = upper_steps.min(low.max(high));
            }
        }
        if delta == 0.0 || steps == 0 {
            return Some(values.to_vec());
        }
        let requested_offset = delta * f64::from(steps);
        let bounded_offset = if requested_offset < lower_offset && lower_offset.is_finite() {
            Some(lower_offset)
        } else if requested_offset > upper_offset && upper_offset.is_finite() {
            Some(upper_offset)
        } else {
            None
        };
        let effective_steps = f64::from(steps).clamp(lower_steps, upper_steps);
        if bounded_offset == Some(0.0) || (bounded_offset.is_none() && effective_steps == 0.0) {
            return Some(values.to_vec());
        }
        let apply = |value| match bounded_offset {
            Some(offset) => value + offset,
            None => delta.mul_add(effective_steps, value),
        };
        values
            .iter()
            .map(|value| match value {
                PropertyValue::Float(value) if is_float => Some(PropertyValue::Float(
                    apply(f64::from(*value)).clamp(min, max) as f32,
                )),
                PropertyValue::Double(value) if !is_float => {
                    Some(PropertyValue::Double(apply(*value).clamp(min, max)))
                }
                _ => None,
            })
            .collect()
    }

    /// Revalidate a deserialized or directly constructed numeric contract.
    pub fn validate(self) -> std::result::Result<(), ParameterSchemaError> {
        let hard_range = ParameterNumericRange::new(self.hard_range.min, self.hard_range.max)?;
        let soft_range = ParameterNumericRange::new(self.soft_range.min, self.soft_range.max)?;
        Self::new(hard_range, soft_range, self.step, self.invalid_value_policy).map(|_| ())
    }
}

// Opposite-sign finite endpoints may overflow their difference. Scale first
// only in that case, avoiding both overflow and cancellation for nearby values.
fn scalar_difference_ratio(bound: f64, value: f64, delta: f64) -> f64 {
    let difference = bound - value;
    if difference.is_finite() {
        difference / delta
    } else {
        bound / delta - value / delta
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
    #[error("enum option keys, display names and message IDs cannot be empty")]
    InvalidEnumOption,
    #[error("enum option keys must be unique")]
    DuplicateEnumOption,
    /// Visual ranks must be absent for every choice or present and unique for all.
    #[error("enum display ranks must be either all absent or all present and unique")]
    InvalidEnumDisplayOrder,
    #[error("enum parameters require options and non-enum parameters cannot declare them")]
    EnumOptionsMismatch,
    #[error("enum default is not present in its option set")]
    UnknownEnumDefault,
    /// Text definitions require a text mode and other definitions must not carry one.
    #[error("text editor mode must be present exactly for text parameters")]
    TextModeTypeMismatch,
    /// Resource contracts belong exactly to Resource definitions.
    #[error("resource contract must be present exactly for resource parameters")]
    ResourceContractTypeMismatch,
    /// Binding families and native path shape must form a valid provider declaration.
    #[error("invalid resource binding or path contract")]
    InvalidResourceContract,
    /// The provider must accept its default reference.
    #[error("resource default is not admitted by the provider")]
    InvalidResourceDefault,
    #[error("numeric constraints require a numeric property type")]
    NumericContractTypeMismatch,
    /// Integer-vector bounds must be exactly representable signed 32-bit integers.
    #[error("integer vector bounds must be signed 32-bit integers")]
    IntegerVectorRange,
    /// Bound lists must cover exactly the declared numeric value shape.
    #[error(
        "numeric channel contract count {actual} does not match value channel count {expected}"
    )]
    NumericChannelCountMismatch { expected: usize, actual: usize },
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
    #[error("curve parameter default must be a valid normalized curve")]
    InvalidCurveDefaultValue,
    #[error("qualifier-samples parameter default must be a valid bounded sample set")]
    InvalidQualifierSamplesDefaultValue,
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
            Self::DoubleVec2(_) => "doublevec2",
            Self::DoubleVec3(_) => "doublevec3",
            Self::DoubleRgb(_) => "doublergb",
            Self::DoubleColor(_) => "doublecolor",
            Self::IntVec2(_) => "intvec2",
            Self::IntVec3(_) => "intvec3",
            Self::Vec2(_) => "vec2",
            Self::Vec3(_) => "vec3",
            Self::Color(_) => "color",
            Self::Vec4(_) => "vec4",
            Self::Curve(_) => "curve",
            Self::QualifierSamples(_) => "qualifier_samples",
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
            Self::DoubleVec2(_) => PropertyValueType::DoubleVec2,
            Self::DoubleVec3(_) => PropertyValueType::DoubleVec3,
            Self::DoubleRgb(_) => PropertyValueType::DoubleRgb,
            Self::DoubleColor(_) => PropertyValueType::DoubleColor,
            Self::IntVec2(_) => PropertyValueType::IntVec2,
            Self::IntVec3(_) => PropertyValueType::IntVec3,
            Self::Vec2(_) => PropertyValueType::Vec2,
            Self::Vec3(_) => PropertyValueType::Vec3,
            Self::Color(_) => PropertyValueType::Color,
            Self::Vec4(_) => PropertyValueType::Vec4,
            Self::Curve(_) => PropertyValueType::Curve,
            Self::QualifierSamples(_) => PropertyValueType::QualifierSamples,
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

    /// Project one numeric scalar for display or floating execution coordinates.
    /// Wide integers can round; author edits must retain the typed source value.
    pub fn as_numeric_f64(&self) -> Option<f64> {
        match self {
            Self::Int(value) => Some(*value as f64),
            _ => self.as_f64(),
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
            Self::DoubleVec2(value) => value.to_vec(),
            Self::DoubleVec3(value) => value.to_vec(),
            Self::DoubleRgb(value) => value.to_vec(),
            Self::DoubleColor(value) => value.to_vec(),
            Self::IntVec2(value) => value.iter().copied().map(f64::from).collect(),
            Self::IntVec3(value) => value.iter().copied().map(f64::from).collect(),
            Self::Vec2(value) => vec![value.x as f64, value.y as f64],
            Self::Vec3(value) => vec![value.x as f64, value.y as f64, value.z as f64],
            Self::Color(value) => vec![
                value.r as f64,
                value.g as f64,
                value.b as f64,
                value.a as f64,
            ],
            Self::Vec4(value) => value.iter().map(|component| *component as f64).collect(),
            Self::Curve(_)
            | Self::QualifierSamples(_)
            | Self::Enum(_)
            | Self::Resource(_)
            | Self::Text(_) => vec![],
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
            PropertyValueType::DoubleVec2 => Self::DoubleVec2(std::array::from_fn(|index| {
                channel_values.get(index).copied().unwrap_or(0.0)
            })),
            PropertyValueType::DoubleVec3 => Self::DoubleVec3(std::array::from_fn(|index| {
                channel_values.get(index).copied().unwrap_or(0.0)
            })),
            PropertyValueType::DoubleRgb => Self::DoubleRgb(std::array::from_fn(|index| {
                channel_values.get(index).copied().unwrap_or(0.0)
            })),
            PropertyValueType::DoubleColor => Self::DoubleColor(std::array::from_fn(|index| {
                channel_values.get(index).copied().unwrap_or(0.0)
            })),
            PropertyValueType::IntVec2 => Self::IntVec2(std::array::from_fn(|index| {
                channel_values.get(index).copied().unwrap_or(0.0).round() as i32
            })),
            PropertyValueType::IntVec3 => Self::IntVec3(std::array::from_fn(|index| {
                channel_values.get(index).copied().unwrap_or(0.0).round() as i32
            })),
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
            PropertyValueType::Curve
            | PropertyValueType::QualifierSamples
            | PropertyValueType::Enum
            | PropertyValueType::Resource
            | PropertyValueType::Text => fallback.clone(),
        }
    }
}

impl PropertyValueType {
    fn uses_f32_channels(self) -> bool {
        matches!(
            self,
            Self::Float | Self::Vec2 | Self::Vec3 | Self::Vec4 | Self::Color
        )
    }

    pub fn channel_count(self) -> usize {
        match self {
            Self::Bool | Self::Int | Self::Float | Self::Double => 1,
            Self::Vec2 | Self::DoubleVec2 | Self::IntVec2 => 2,
            Self::Vec3 | Self::DoubleVec3 | Self::DoubleRgb | Self::IntVec3 => 3,
            Self::Color | Self::Vec4 | Self::DoubleColor => 4,
            Self::Enum => 1,
            Self::Curve | Self::QualifierSamples | Self::Resource | Self::Text => 0,
        }
    }

    pub fn supports_animation(self) -> bool {
        !matches!(
            self,
            Self::Curve | Self::QualifierSamples | Self::Resource | Self::Text
        )
    }

    /// Keep categorical values at Hold; numeric modes are governed by their schema.
    pub fn normalized_interpolation(self, interpolation: InterpolationType) -> InterpolationType {
        match self {
            Self::Bool
            | Self::Curve
            | Self::QualifierSamples
            | Self::Enum
            | Self::Resource
            | Self::Text => InterpolationType::Hold,
            _ => interpolation,
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
    /// Define a property from an address alias, returning an error for
    /// untrusted (plugin/authoring) input instead of panicking the host.
    ///
    /// The convenience constructor derives a namespaced identity for local
    /// prototypes and tests. Product definitions must immediately call
    /// [`Self::with_parameter_id`] so address changes cannot rename persisted
    /// parameter identity. A plugin-supplied path must flow through this
    /// fallible entry point so malformed input surfaces as a typed error.
    pub fn try_new(
        path: impl Into<String>,
        display_name: impl Into<String>,
        default_value: PropertyValue,
    ) -> Result<Self, ParameterIdError> {
        let path = path.into();
        let derived_id = format!("mondrian.property.{path}");
        let parameter_id = ParameterId::new(derived_id)?;
        let mut schema = ParameterSchema::v1(parameter_id, default_value);
        if matches!(schema.value_type, PropertyValueType::Resource) {
            schema.cache_impact = ParameterCacheImpact::Resource;
        }
        Ok(Self {
            schema,
            path,
            display_name: display_name.into(),
            ui_metadata: AnimatablePropertyUiMetadata::default(),
        })
    }

    /// Define a property from an address alias.
    ///
    /// This convenience constructor derives a namespaced identity for local
    /// prototypes and tests. Product definitions must immediately call
    /// [`Self::with_parameter_id`] so address changes cannot rename persisted
    /// parameter identity. For untrusted input (plugin authoring, external
    /// files), use [`Self::try_new`] and propagate the typed error instead of
    /// panicking the host.
    pub fn new(
        path: impl Into<String>,
        display_name: impl Into<String>,
        default_value: PropertyValue,
    ) -> Self {
        Self::try_new(path, display_name, default_value).unwrap_or_else(|error| {
            panic!("property address cannot derive a parameter identity: {error}")
        })
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

    /// Declare the definition-owned editor shape for a text parameter.
    pub fn with_text_mode(mut self, mode: ParameterTextMode) -> Self {
        self.schema = self.schema.with_text_mode(mode);
        self
    }

    /// Declare resource admission from the owning executable definition.
    pub fn with_resource_contract(mut self, resource: ParameterResourceContract) -> Self {
        self.schema = self.schema.with_resource_contract(resource);
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

    /// Declare complete ordered numeric bounds for each component of this property.
    pub fn with_channel_numeric_contracts(
        mut self,
        unit: ParameterUnit,
        numeric: impl Into<AuthoringList<ParameterNumericContract>>,
    ) -> Self {
        self.schema = self.schema.with_channel_numeric_contracts(unit, numeric);
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

/// Canonical scalar channel key, retaining exact integer author bits.
/// Floating execution coordinates are derived explicitly at numeric seams.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AnimationChannelValue {
    /// Finite continuous author value (including exactly representable vectors).
    Number(f64),
    /// Exact signed integer author value.
    Integer(i64),
}

impl AnimationChannelValue {
    /// Project into numeric execution or UI geometry; wide integers can round.
    pub fn projection(self) -> f64 {
        match self {
            Self::Number(value) => value,
            Self::Integer(value) => value as f64,
        }
    }
}

impl crate::AuthoringFootprint for AnimationChannelValue {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        match self {
            Self::Number(value) => collector.collect(value),
            Self::Integer(value) => collector.collect(value),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimationChannel {
    pub index: usize,
    keyframes: AuthoringList<Keyframe<AnimationChannelValue>>,
}

impl AnimationChannel {
    pub fn new(index: usize) -> Self {
        Self { index, keyframes: AuthoringList::new() }
    }

    pub fn keyframes(&self) -> &[Keyframe<AnimationChannelValue>] {
        &self.keyframes
    }

    pub fn keyframe_at(&self, time: TimelineTime) -> Option<&Keyframe<AnimationChannelValue>> {
        self.keyframes.iter().find(|keyframe| keyframe.time == time)
    }

    fn set_keyframe(&mut self, keyframe: Keyframe<AnimationChannelValue>) {
        let pos = self.keyframes.partition_point(|candidate| candidate.time < keyframe.time);
        if pos < self.keyframes.len() && self.keyframes[pos].time == keyframe.time {
            self.keyframes[pos] = keyframe;
        } else {
            self.keyframes.insert(pos, keyframe);
        }
    }

    fn remove_keyframe(&mut self, time: TimelineTime) -> Option<Keyframe<AnimationChannelValue>> {
        self.keyframes
            .iter()
            .position(|keyframe| keyframe.time == time)
            .map(|index| self.keyframes.remove(index))
    }

    /// Evaluate the admitted numeric channel with shared owner-space execution math.
    pub fn evaluate(&self, time: TimelineTime, fallback: f64) -> f64 {
        evaluate_numeric_channel(self, time, fallback)
    }

    /// Whether this channel contains author keyframes.
    pub fn is_animated(&self) -> bool {
        !self.keyframes.is_empty()
    }

    fn normalize_keyframes(&mut self, parameter_id: &ParameterId) -> Result<()> {
        for index in 0..self.keyframes.len() {
            let previous = index.checked_sub(1).map(|i| self.keyframes[i].time);
            let next = self.keyframes.get(index + 1).map(|key| key.time);
            let key = &self.keyframes[index];
            let mut incoming_control = key.interp_in;
            let mut outgoing_control = key.interp_out;
            for (interpolation, neighbor, incoming) in [
                (&mut incoming_control, previous, true),
                (&mut outgoing_control, next, false),
            ] {
                if let (KeyframeInterpolation::Bezier(handle), Some(neighbor)) =
                    (*interpolation, neighbor)
                {
                    let duration = if incoming {
                        key.time.checked_sub(neighbor)?
                    } else {
                        neighbor.checked_sub(key.time)?
                    };
                    *interpolation = KeyframeInterpolation::Bezier(
                        crate::parameter::bound_continuous_handle(handle, duration, incoming)
                            .map_err(|error| {
                                parameter_value_error(parameter_id.as_str(), &error.to_string())
                            })?,
                    );
                }
            }
            if incoming_control != key.interp_in || outgoing_control != key.interp_out {
                let key = &mut self.keyframes[index];
                key.interp_in = incoming_control;
                key.interp_out = outgoing_control;
            }
        }
        if !self.keyframes.iter().any(|key| {
            key.temporal_flags.auto_bezier
                || (key.temporal_flags.continuous && !key.temporal_flags.broken_handles)
        }) {
            return Ok(());
        }
        let mut curve = crate::ExactAutomationCurve {
            parameter_id: parameter_id.clone(),
            default_value: 0.0,
            keyframes: self
                .keyframes
                .iter()
                .map(|key| crate::ExactAutomationKeyframe {
                    id: key.id,
                    time: key.time,
                    value: key.value.projection(),
                    interpolation_to_next: crate::AutomationSegmentInterpolation::Bezier,
                    in_handle: match key.interp_in {
                        KeyframeInterpolation::Bezier(handle) => Some(handle),
                        _ => None,
                    },
                    out_handle: match key.interp_out {
                        KeyframeInterpolation::Bezier(handle) => Some(handle),
                        _ => None,
                    },
                    tangent_mode: if key.temporal_flags.auto_bezier {
                        crate::ExactAutomationTangentMode::Auto
                    } else if key.temporal_flags.continuous && !key.temporal_flags.broken_handles {
                        crate::ExactAutomationTangentMode::Continuous
                    } else {
                        crate::ExactAutomationTangentMode::Manual
                    },
                })
                .collect(),
        };
        for index in 0..curve.keyframes.len() {
            if curve.keyframes[index].tangent_mode == crate::ExactAutomationTangentMode::Continuous
            {
                curve.initialize_continuous_constraint(index);
            }
        }
        let initialized = curve.keyframes.clone();
        curve
            .normalize_constrained_tangents()
            .map_err(|error| parameter_value_error(parameter_id.as_str(), &error.to_string()))?;
        for (index, native) in curve.keyframes.iter().enumerate() {
            let key = &self.keyframes[index];
            if !key.temporal_flags.auto_bezier
                && (!key.temporal_flags.continuous || key.temporal_flags.broken_handles)
            {
                continue;
            }
            let incoming = native
                .in_handle
                .or(initialized[index].in_handle)
                .map(KeyframeInterpolation::Bezier)
                .unwrap_or(key.interp_in);
            let outgoing = native
                .out_handle
                .or(initialized[index].out_handle)
                .map(KeyframeInterpolation::Bezier)
                .unwrap_or(key.interp_out);
            let continuous = index > 0 && index + 1 < curve.keyframes.len();
            if incoming != key.interp_in
                || outgoing != key.interp_out
                || (key.temporal_flags.auto_bezier && key.temporal_flags.continuous != continuous)
                || (key.temporal_flags.auto_bezier && key.temporal_flags.broken_handles)
            {
                let key = &mut self.keyframes[index];
                key.interp_in = incoming;
                key.interp_out = outgoing;
                if key.temporal_flags.auto_bezier {
                    key.temporal_flags.continuous = continuous;
                    key.temporal_flags.broken_handles = false;
                }
            }
        }
        Ok(())
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
    channels: AuthoringList<AnimationChannel>,
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
        if let Some(contracts) = self.descriptor.schema.numeric.as_ref()
            && let Some((contract, value)) = contracts
                .iter()
                .zip(self.static_value.to_channel_values())
                .find(|(contract, value)| !contract.hard_range.contains(*value))
        {
            return Err(parameter_value_error(
                &self.descriptor.path,
                &format!(
                    "persisted value lies outside [{}, {}]: {value}",
                    contract.hard_range.min, contract.hard_range.max
                ),
            ));
        }
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
            for (index, keyframe) in channel.keyframes.iter().enumerate() {
                if let Some(existing_time) = keyframe_times.insert(keyframe.id, keyframe.time)
                    && existing_time != keyframe.time
                {
                    return Err(parameter_value_error(
                        &self.descriptor.path,
                        "one keyframe identity cannot address different author times",
                    ));
                }
                if previous_time.is_some_and(|time| time >= keyframe.time) {
                    return Err(parameter_value_error(
                        &self.descriptor.path,
                        "keyframe times must be strictly increasing",
                    ));
                }
                previous_time = Some(keyframe.time);
                if matches!(keyframe.value, AnimationChannelValue::Integer(_))
                    != (self.value_type() == PropertyValueType::Int)
                {
                    return Err(parameter_value_error(
                        &self.descriptor.path,
                        "channel scalar kind does not match its property",
                    ));
                }
                if let AnimationChannelValue::Integer(value) = keyframe.value
                    && self.normalize_value(PropertyValue::Int(value))? != PropertyValue::Int(value)
                {
                    return Err(parameter_value_error(
                        &self.descriptor.path,
                        "persisted integer key lies outside hard bounds",
                    ));
                }
                let value =
                    self.normalize_numeric_channel(expected_index, keyframe.value.projection())?;
                if value != keyframe.value.projection() {
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
                validate_numeric_control_points(
                    &self.descriptor.path,
                    keyframe,
                    index.checked_sub(1).map(|i| channel.keyframes[i].time),
                    channel.keyframes.get(index + 1).map(|key| key.time),
                )?;
                self.ensure_interpolation_allowed(channel_keyframe_interpolation_type(keyframe))?;
            }
        }
        for channel in &self.channels {
            let mut normalized = channel.clone();
            normalized.normalize_keyframes(&self.descriptor.schema.parameter_id)?;
            if normalized != *channel {
                return Err(parameter_value_error(
                    &self.descriptor.path,
                    "persisted controls do not satisfy their temporal constraints",
                ));
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
            channel.normalize_keyframes(&self.descriptor.schema.parameter_id)?;
        }
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Remove keys inside one half-open owner-time range and close the gap.
    ///
    /// Keys at or after the exclusive range end move earlier by the exact
    /// range duration. Stable key identities and interpolation metadata are
    /// retained, and the complete property validates before publication.
    pub fn extract_time_range(&mut self, range: TimelineTimeRange) -> Result<()> {
        let end = range.end()?;
        if range.duration.is_zero() {
            return Ok(());
        }

        let mut candidate = self.clone();
        for channel in &mut candidate.channels {
            channel
                .keyframes
                .retain(|keyframe| keyframe.time < range.start || keyframe.time >= end);
            for keyframe in &mut channel.keyframes {
                if keyframe.time >= end {
                    keyframe.time = keyframe.time.checked_sub(range.duration)?;
                }
            }
            channel.normalize_keyframes(&self.descriptor.schema.parameter_id)?;
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

    /// Capture complete stable keys without evaluating or narrowing channel values.
    /// Missing, duplicate and channel-incomplete identities refuse the whole capture.
    pub fn capture_complete_keyframes(
        &self,
        ids: &[KeyframeId],
    ) -> Result<Vec<Vec<Keyframe<AnimationChannelValue>>>> {
        let requested: HashSet<_> = ids.iter().copied().collect();
        if requested.is_empty() || requested.len() != ids.len() {
            return Err(parameter_value_error(
                &self.descriptor.path,
                "invalid key selection",
            ));
        }
        let mut groups: HashMap<_, Vec<_>> = HashMap::with_capacity(ids.len());
        for channel in &self.channels {
            for key in channel.keyframes() {
                if requested.contains(&key.id) {
                    groups.entry(key.id).or_default().push(key.clone());
                }
            }
        }
        ids.iter()
            .map(|id| {
                let keys = groups.remove(id).ok_or_else(|| {
                    parameter_value_error(&self.descriptor.path, "missing selected key")
                })?;
                if keys.len() != self.channel_count()
                    || keys.iter().any(|key| key.time != keys[0].time)
                {
                    return Err(parameter_value_error(
                        &self.descriptor.path,
                        "incomplete selected key",
                    ));
                }
                Ok(keys)
            })
            .collect()
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
            value: self.evaluate_channels(time),
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

        self.evaluate_channels(time)
    }

    fn evaluate_channels(&self, time: TimelineTime) -> PropertyValue {
        if self.value_type() == PropertyValueType::Int
            && self.descriptor.schema.allowed_interpolations == [ParameterInterpolation::Hold]
            && let Some(channel) = self.channels.first()
            && !channel.keyframes.is_empty()
        {
            let index = channel.keyframes.partition_point(|key| key.time <= time).saturating_sub(1);
            if let AnimationChannelValue::Integer(value) = channel.keyframes[index].value {
                return PropertyValue::Int(value);
            }
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

    /// Return the definition's first admitted interpolation for new author keys.
    pub fn default_interpolation(&self) -> Result<InterpolationType> {
        self.descriptor
            .schema
            .allowed_interpolations
            .first()
            .copied()
            .map(|mode| match mode {
                ParameterInterpolation::Hold => InterpolationType::Hold,
                ParameterInterpolation::Linear => InterpolationType::Linear,
                ParameterInterpolation::Bezier => InterpolationType::Bezier,
            })
            .ok_or_else(|| {
                parameter_value_error(&self.descriptor.path, "parameter admits no interpolation")
            })
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
                self.default_interpolation()?,
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

    /// Write a property value without replacing authored temporal geometry.
    pub fn write_value(
        &mut self,
        time: TimelineTime,
        value: PropertyValue,
        interpolation: InterpolationType,
    ) -> Result<()> {
        let value = self.normalize_value(value)?;
        let channel_values = self.value_to_channel_values(&value)?;
        let updates = channel_values.into_iter().enumerate().collect::<Vec<(usize, f64)>>();
        if self.animation_enabled
            && self.descriptor.schema.is_animatable
            && !self.channels.is_empty()
        {
            if matches!(value, PropertyValue::Int(_)) {
                self.ensure_interpolation_allowed(interpolation.parameter_interpolation())?;
                let mut key = self
                    .keyframe_at(time)
                    .unwrap_or_else(|| Keyframe::from_preset(time, value.clone(), interpolation));
                key.value = value;
                self.set_exact_keyframe(key)
            } else {
                self.write_channels(time, &updates, interpolation)
            }
        } else {
            self.static_value = value;
            Ok(())
        }
    }

    /// Insert or replace a complete typed key only after validating its candidate.
    ///
    /// Invalid handle coordinates and normalization failures leave every channel
    /// and the animation-enabled state unchanged.
    pub fn set_exact_keyframe(&mut self, mut keyframe: Keyframe<PropertyValue>) -> Result<()> {
        validate_visual_handle_coordinates(
            &self.descriptor.path,
            keyframe.interp_in,
            keyframe.interp_out,
        )?;
        let mut candidate = self.clone();
        keyframe.value = candidate.normalize_value(keyframe.value)?;
        candidate.ensure_interpolation_allowed(keyframe_interpolation_type(&keyframe))?;
        let channel_values = candidate.value_to_author_channel_values(&keyframe.value)?;
        validate_channel_updates(
            candidate.channel_count(),
            &channel_values
                .iter()
                .enumerate()
                .map(|(index, value)| (index, value.projection()))
                .collect::<Vec<_>>(),
            &candidate.descriptor.path,
        )?;

        candidate.animation_enabled = true;
        for (index, value) in channel_values.into_iter().enumerate() {
            let channel = candidate.channels.get_mut(index).ok_or_else(|| {
                MondrianError::WorkflowStepFailed {
                    step_id: "property_set_exact_keyframe".to_string(),
                    reason: format!("属性通道不存在: {}[{index}]", candidate.descriptor.path),
                }
            })?;
            channel.set_keyframe(Keyframe {
                id: keyframe.id,
                time: keyframe.time,
                value,
                interp_in: keyframe.interp_in,
                interp_out: keyframe.interp_out,
                temporal_flags: keyframe.temporal_flags,
            });
            let position = channel
                .keyframes
                .binary_search_by_key(&keyframe.time, |key| key.time)
                .map_err(|_| {
                parameter_value_error(&candidate.descriptor.path, "inserted key is missing")
            })?;
            validate_numeric_control_points(
                &candidate.descriptor.path,
                &channel.keyframes[position],
                position.checked_sub(1).map(|i| channel.keyframes[i].time),
                channel.keyframes.get(position + 1).map(|key| key.time),
            )?;
        }
        candidate.normalize_channels()?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Atomically insert or move one canonical channel key by stable identity.
    ///
    /// The channel author value retains its numeric precision, independently of
    /// the evaluated PropertyValue scalar. Schema bounds, scalar kind, controls
    /// and interpolation are admitted before publication. An occupied destination
    /// owned by another key is rejected. Temporal controls are normalized to
    /// final neighbor spans before representability validation, including
    /// inherited controls during a move. Other channels remain unchanged.
    pub fn set_channel_keyframe(
        &mut self,
        channel_index: usize,
        mut keyframe: Keyframe<AnimationChannelValue>,
    ) -> Result<()> {
        self.admit_channels(|candidate| {
            if !candidate.descriptor.schema.is_animatable {
                return Err(parameter_value_error(
                    &candidate.descriptor.path,
                    "property is not animatable",
                ));
            }
            if channel_index >= candidate.channel_count() {
                return Err(parameter_value_error(
                    &candidate.descriptor.path,
                    "author channel does not exist",
                ));
            }
            candidate.admit_channel_keyframe(channel_index, &mut keyframe)?;
            let channel = &mut candidate.channels[channel_index];
            if channel.keyframe_at(keyframe.time).is_some_and(|key| key.id != keyframe.id) {
                return Err(parameter_value_error(
                    &candidate.descriptor.path,
                    "author time is occupied by another key",
                ));
            }
            if let Some(existing) = channel.keyframes.iter().find(|key| key.id == keyframe.id) {
                let old_time = existing.time;
                let _ = channel.remove_keyframe(old_time);
            }
            channel.set_keyframe(keyframe);
            candidate.animation_enabled = true;
            candidate.normalize_channels()
        })
    }

    /// Paste complete canonical keys, replacing occupied times as one atomic batch.
    /// Channel order follows the declared schema. Values and per-channel metadata
    /// remain canonical; constraints normalize once against the final topology.
    /// Reusing an existing identity at another time, duplicate destinations,
    /// incomplete groups and invalid values leave the entire property unchanged.
    pub fn paste_complete_keyframes(
        &mut self,
        mut groups: Vec<Vec<Keyframe<AnimationChannelValue>>>,
    ) -> Result<()> {
        self.admit_channels(|candidate| {
            let invalid =
                || parameter_value_error(&candidate.descriptor.path, "invalid complete key batch");
            if !candidate.descriptor.schema.is_animatable || groups.is_empty() {
                return Err(invalid());
            }
            let mut ids = HashMap::with_capacity(groups.len());
            let mut times = HashSet::with_capacity(groups.len());
            for keys in &mut groups {
                let first = keys.first().ok_or_else(invalid)?;
                let (id, time) = (first.id, first.time);
                if keys.len() != candidate.channel_count()
                    || ids.insert(id, time).is_some()
                    || !times.insert(time)
                    || keys.iter().any(|key| key.id != id || key.time != time)
                {
                    return Err(invalid());
                }
                for (index, key) in keys.iter_mut().enumerate() {
                    candidate.admit_channel_keyframe(index, key)?;
                }
            }
            for (index, channel) in candidate.channels.iter_mut().enumerate() {
                if channel
                    .keyframes
                    .iter()
                    .any(|key| ids.get(&key.id).is_some_and(|time| *time != key.time))
                {
                    return Err(parameter_value_error(
                        &candidate.descriptor.path,
                        "key identity already belongs to another time",
                    ));
                }
                channel.keyframes.retain(|key| !times.contains(&key.time));
                channel.keyframes.extend(groups.iter().map(|keys| keys[index].clone()));
                channel.keyframes.sort_by_key(|key| key.time);
            }
            candidate.animation_enabled = true;
            candidate.normalize_channels()
        })
    }

    fn admit_channel_keyframe(
        &self,
        channel_index: usize,
        keyframe: &mut Keyframe<AnimationChannelValue>,
    ) -> Result<()> {
        validate_visual_handle_coordinates(
            &self.descriptor.path,
            keyframe.interp_in,
            keyframe.interp_out,
        )?;
        self.ensure_interpolation_allowed(channel_keyframe_interpolation_type(keyframe))?;
        keyframe.value = match (self.value_type(), keyframe.value) {
            (PropertyValueType::Int, AnimationChannelValue::Integer(value)) => {
                let PropertyValue::Int(value) = self.normalize_value(PropertyValue::Int(value))?
                else {
                    return Err(parameter_value_error(
                        &self.descriptor.path,
                        "invalid integer channel value",
                    ));
                };
                AnimationChannelValue::Integer(value)
            }
            (PropertyValueType::Int, _) | (_, AnimationChannelValue::Integer(_)) => {
                return Err(parameter_value_error(
                    &self.descriptor.path,
                    "channel scalar kind does not match its property",
                ));
            }
            (_, AnimationChannelValue::Number(value)) => {
                AnimationChannelValue::Number(self.normalize_numeric_channel(channel_index, value)?)
            }
        };
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
        self.admit_channels(|candidate| candidate.edit_keyframe_candidate(keyframe_id, time, value))
    }

    fn edit_keyframe_candidate(
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
        let channel_values = self.value_to_author_channel_values(&value)?;
        validate_channel_updates(
            self.channel_count(),
            &channel_values
                .iter()
                .enumerate()
                .map(|(index, value)| (index, value.projection()))
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
        self.normalize_channels()?;
        Ok(())
    }

    /// Write selected numeric channels, retaining existing exact handles and identities.
    pub fn write_channels(
        &mut self,
        time: TimelineTime,
        channel_values: &[(usize, f64)],
        interpolation: InterpolationType,
    ) -> Result<()> {
        self.admit_channels(|candidate| {
            candidate.write_channels_candidate(time, channel_values, interpolation)
        })
    }

    fn write_channels_candidate(
        &mut self,
        time: TimelineTime,
        channel_values: &[(usize, f64)],
        interpolation: InterpolationType,
    ) -> Result<()> {
        validate_channel_updates(
            self.value_type().channel_count(),
            channel_values,
            &self.descriptor.path,
        )?;
        let channel_values = channel_values
            .iter()
            .map(|(index, value)| Ok((*index, self.normalize_numeric_channel(*index, *value)?)))
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
                let (interp_in, interp_out, default_flags) = interpolation_defaults(normalized);
                channel.set_keyframe(Keyframe {
                    id: existing.map(|keyframe| keyframe.id).unwrap_or_else(KeyframeId::new),
                    time,
                    value: if self.descriptor.schema.value_type == PropertyValueType::Int {
                        AnimationChannelValue::Integer(value.round() as i64)
                    } else {
                        AnimationChannelValue::Number(*value)
                    },
                    interp_in: existing.map(|keyframe| keyframe.interp_in).unwrap_or(interp_in),
                    interp_out: existing.map(|keyframe| keyframe.interp_out).unwrap_or(interp_out),
                    temporal_flags: existing
                        .map(|keyframe| keyframe.temporal_flags)
                        .unwrap_or(default_flags),
                });
            }
            self.normalize_channels()?;
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

    /// Atomically remove a complete key and admit all resulting channel constraints.
    pub fn remove_keyframe(&mut self, time: TimelineTime) -> Result<()> {
        self.admit_channels(|candidate| candidate.remove_keyframe_candidate(time))
    }

    fn remove_keyframe_candidate(&mut self, time: TimelineTime) -> Result<()> {
        for channel in &mut self.channels {
            let _ = channel.remove_keyframe(time);
        }
        self.normalize_channels()?;
        Ok(())
    }

    /// Remove a nonempty set of complete stable keys in one constraint admission.
    /// If every key is removed, retain the original evaluated value at `retain_time`.
    /// Duplicate or stale identities reject the entire edit.
    pub fn remove_keyframes(
        &mut self,
        ids: &[KeyframeId],
        retain_time: TimelineTime,
    ) -> Result<()> {
        self.admit_channels(|candidate| {
            let identities = ids.iter().copied().collect::<std::collections::HashSet<_>>();
            if ids.is_empty() || identities.len() != ids.len() {
                return Err(parameter_value_error(
                    &candidate.descriptor.path,
                    "key selection must be nonempty and unique",
                ));
            }
            let times = candidate
                .channels
                .first()
                .into_iter()
                .flat_map(|channel| &channel.keyframes)
                .filter(|key| identities.contains(&key.id))
                .filter(|key| {
                    candidate.channels.iter().skip(1).all(|channel| {
                        channel
                            .keyframes
                            .binary_search_by_key(&key.time, |other| other.time)
                            .ok()
                            .is_some_and(|index| channel.keyframes[index].id == key.id)
                    })
                })
                .map(|key| key.time)
                .collect::<std::collections::HashSet<_>>();
            if times.len() != ids.len() {
                return Err(parameter_value_error(
                    &candidate.descriptor.path,
                    "key selection contains a stale or incomplete key",
                ));
            }
            let retained = candidate.evaluate(retain_time);
            for channel in &mut candidate.channels {
                channel.keyframes.retain(|key| !times.contains(&key.time));
            }
            if candidate.channels.iter().all(|channel| channel.keyframes.is_empty()) {
                candidate.static_value = retained;
                candidate.animation_enabled = false;
                Ok(())
            } else {
                candidate.normalize_channels()
            }
        })
    }

    fn complete_keyframe_selection(
        &self,
        ids: &[KeyframeId],
    ) -> Result<std::collections::HashSet<KeyframeId>> {
        let identities = ids.iter().copied().collect::<std::collections::HashSet<_>>();
        if ids.is_empty() || identities.len() != ids.len() {
            return Err(parameter_value_error(
                &self.descriptor.path,
                "key selection must be nonempty and unique",
            ));
        }
        let complete = self
            .channels
            .first()
            .into_iter()
            .flat_map(|channel| &channel.keyframes)
            .filter(|key| identities.contains(&key.id))
            .filter(|key| {
                self.channels.iter().skip(1).all(|channel| {
                    channel
                        .keyframes
                        .binary_search_by_key(&key.time, |other| other.time)
                        .ok()
                        .is_some_and(|index| channel.keyframes[index].id == key.id)
                })
            })
            .count();
        if complete != ids.len() {
            return Err(parameter_value_error(
                &self.descriptor.path,
                "key selection contains a stale or incomplete key",
            ));
        }
        Ok(identities)
    }

    /// Offset selected complete keys in one numeric channel with a common bounded step.
    /// Other channel values, exact author times and stable identities remain unchanged.
    /// The complete selection and definition are admitted before one constraint normalization.
    pub fn offset_keyframe_values(
        &mut self,
        ids: &[KeyframeId],
        channel_index: usize,
        delta: f64,
        steps: i8,
    ) -> Result<()> {
        self.transform_selected_keyframes(
            ids,
            TimelineTime::ZERO,
            Some((channel_index, delta, steps)),
        )
    }

    /// Translate a nonempty unique set of complete stable keys by an exact delta.
    /// All channels move together and validate the final topology before publication.
    /// Typed scalar values are never evaluated or reconstructed through a float projection.
    pub fn shift_keyframes(&mut self, ids: &[KeyframeId], delta: TimelineTime) -> Result<()> {
        self.transform_selected_keyframes(ids, delta, None)
    }

    /// Translate complete keys in every channel and offset one numeric channel atomically.
    /// The value contract derives one bounded common displacement from canonical scalars.
    /// Exact times, typed values, IDs and tangent intent survive one final normalization.
    pub fn transform_keyframes(
        &mut self,
        ids: &[KeyframeId],
        channel_index: usize,
        time_delta: TimelineTime,
        value_delta: f64,
        steps: i8,
    ) -> Result<()> {
        self.transform_selected_keyframes(
            ids,
            time_delta,
            Some((channel_index, value_delta, steps)),
        )
    }

    fn transform_selected_keyframes(
        &mut self,
        ids: &[KeyframeId],
        time_delta: TimelineTime,
        value_edit: Option<(usize, f64, i8)>,
    ) -> Result<()> {
        self.validate()?;
        self.admit_channels(|candidate| {
            let identities = candidate.complete_keyframe_selection(ids)?;
            let mut value_changed = false;
            if let Some((channel_index, delta, steps)) = value_edit {
                let contract = candidate
                    .descriptor
                    .schema
                    .numeric
                    .as_ref()
                    .and_then(|contracts| contracts.get(channel_index))
                    .copied()
                    .ok_or_else(|| {
                        parameter_value_error(
                            &candidate.descriptor.path,
                            "numeric channel contract is absent",
                        )
                    })?;
                let channel = candidate.channels.get_mut(channel_index).ok_or_else(|| {
                    parameter_value_error(&candidate.descriptor.path, "numeric channel is absent")
                })?;
                let (indices, values): (Vec<_>, Vec<_>) = channel
                    .keyframes
                    .iter()
                    .enumerate()
                    .filter(|(_, key)| identities.contains(&key.id))
                    .map(|(index, key)| {
                        (
                            index,
                            match key.value {
                                AnimationChannelValue::Integer(value) => PropertyValue::Int(value),
                                AnimationChannelValue::Number(value) => {
                                    PropertyValue::Double(value)
                                }
                            },
                        )
                    })
                    .unzip();
                let shifted = contract.offset_scalars(&values, delta, steps).ok_or_else(|| {
                    parameter_value_error(
                        &candidate.descriptor.path,
                        "numeric group increment is invalid",
                    )
                })?;
                value_changed = values != shifted;
                if value_changed {
                    for (index, value) in indices.into_iter().zip(shifted) {
                        channel.keyframes[index].value = match value {
                            PropertyValue::Int(value) => AnimationChannelValue::Integer(value),
                            PropertyValue::Double(value) => AnimationChannelValue::Number(value),
                            _ => {
                                return Err(parameter_value_error(
                                    &candidate.descriptor.path,
                                    "numeric group changed its value type",
                                ))
                            }
                        };
                    }
                }
            }
            if time_delta.is_zero() && !value_changed {
                return Ok(());
            }
            for channel in &mut candidate.channels {
                for key in &mut channel.keyframes {
                    if identities.contains(&key.id) {
                        key.time = key.time.checked_add(time_delta)?;
                    }
                }
                channel.keyframes.sort_by_key(|key| key.time);
                if channel.keyframes.windows(2).any(|pair| pair[0].time == pair[1].time) {
                    return Err(parameter_value_error(
                        &candidate.descriptor.path,
                        "key selection collides with an unselected key",
                    ));
                }
            }
            candidate.normalize_channels()
        })
    }

    /// Atomically move a complete key while retaining its identity and typed value.
    pub fn move_keyframe(&mut self, from_time: TimelineTime, to_time: TimelineTime) -> Result<()> {
        self.admit_channels(|candidate| candidate.move_keyframe_candidate(from_time, to_time))
    }

    fn move_keyframe_candidate(
        &mut self,
        from_time: TimelineTime,
        to_time: TimelineTime,
    ) -> Result<()> {
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
        self.normalize_channels()?;

        Ok(())
    }

    /// Set one interpolation preset on a nonempty unique selection of complete keys.
    /// All channel intents change before constraints are normalized once; values,
    /// exact owner times and identities are preserved. Invalid selections publish nothing.
    pub fn update_keyframes_interpolation(
        &mut self,
        ids: &[KeyframeId],
        interpolation: InterpolationType,
    ) -> Result<()> {
        self.validate()?;
        self.admit_channels(|candidate| {
            let identities = candidate.complete_keyframe_selection(ids)?;
            if !candidate.descriptor.schema.is_animatable
                || !candidate.value_type().supports_animation()
            {
                return Err(parameter_value_error(
                    &candidate.descriptor.path,
                    "property does not support keyframe interpolation",
                ));
            }
            let normalized = candidate.value_type().normalized_interpolation(interpolation);
            candidate.ensure_interpolation_allowed(normalized.parameter_interpolation())?;
            for channel in &mut candidate.channels {
                for key in &mut channel.keyframes {
                    if identities.contains(&key.id) {
                        apply_keyframe_interpolation(key, normalized);
                    }
                }
            }
            candidate.normalize_channels()
        })
    }

    /// Select one key's interpolation preset, retaining existing Bezier controls.
    /// Explicit Bezier presets also retain both currently unconnected temporal sides,
    /// so adding a neighbor cannot discard the selected interpolation intent.
    pub fn update_keyframe_interpolation(
        &mut self,
        time: TimelineTime,
        interpolation: InterpolationType,
    ) -> Result<()> {
        self.admit_channels(|candidate| {
            candidate.update_keyframe_interpolation_candidate(time, interpolation)
        })
    }

    fn update_keyframe_interpolation_candidate(
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
                let existing = &mut channel.keyframes[index];
                apply_keyframe_interpolation(existing, normalized);
                updated_any = true;
            }
        }

        if !updated_any {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "property_update_keyframe_interpolation".to_string(),
                reason: format!("关键帧不存在: {} @ {}", self.descriptor.path, time),
            });
        }

        self.normalize_channels()?;
        Ok(())
    }

    /// Resolve a visible Bezier control from the canonical channel and neighbor span.
    /// Linear/Hold sides and sides without a corresponding segment have no handle.
    pub fn channel_keyframe_handle(
        &self,
        channel_index: usize,
        keyframe_id: KeyframeId,
        side: crate::BezierHandleSide,
    ) -> Option<ExactBezierHandle> {
        let keys = self.channels.get(channel_index)?.keyframes();
        let index = keys.iter().position(|key| key.id == keyframe_id)?;
        self.channel_keyframe_control(channel_index, index, side)
            .map(|control| control.handle)
    }

    /// Visit a channel's canonical controls in key-time order, incoming before outgoing.
    /// Missing channels yield no controls; each side resolves its immediate neighbor.
    pub fn channel_keyframe_controls(
        &self,
        channel_index: usize,
    ) -> impl Iterator<Item = crate::TemporalBezierControl> + '_ {
        let count = self.channels.get(channel_index).map_or(0, |channel| channel.keyframes().len());
        (0..count).flat_map(move |index| {
            [
                crate::BezierHandleSide::Incoming,
                crate::BezierHandleSide::Outgoing,
            ]
            .into_iter()
            .filter_map(move |side| self.channel_keyframe_control(channel_index, index, side))
        })
    }

    fn channel_keyframe_control(
        &self,
        channel_index: usize,
        index: usize,
        side: crate::BezierHandleSide,
    ) -> Option<crate::TemporalBezierControl> {
        let keys = self.channels.get(channel_index)?.keyframes();
        let key = keys.get(index)?;
        let (neighbor, interpolation, opposite) = match side {
            crate::BezierHandleSide::Incoming => {
                let neighbor = keys.get(index.checked_sub(1)?)?;
                (neighbor, key.interp_in, neighbor.interp_out)
            }
            crate::BezierHandleSide::Outgoing => {
                let neighbor = keys.get(index + 1)?;
                (neighbor, key.interp_out, neighbor.interp_in)
            }
        };
        if matches!(opposite, KeyframeInterpolation::Hold) {
            return None;
        }
        let handle = match interpolation {
            KeyframeInterpolation::Bezier(handle) => handle,
            KeyframeInterpolation::DefaultBezier => ExactBezierHandle {
                time_offset: neighbor
                    .time
                    .checked_sub(key.time)
                    .ok()?
                    .checked_scale(crate::TimeScale::new(1, 3).ok()?)
                    .ok()?,
                value_offset: 0.0,
            },
            _ => return None,
        };
        Some(crate::TemporalBezierControl {
            keyframe_id: key.id,
            key_time: key.time,
            key_value: key.value.projection(),
            neighbor_time: neighbor.time,
            side,
            handle,
        })
    }

    /// Atomically replace one channel's typed handles and admit the resulting constraints.
    pub fn update_channel_keyframe_handles(
        &mut self,
        time: TimelineTime,
        channel_index: usize,
        interp_in: KeyframeInterpolation,
        interp_out: KeyframeInterpolation,
    ) -> Result<()> {
        self.ensure_interpolation_allowed(ParameterInterpolation::Bezier)?;
        validate_visual_handle_coordinates(&self.descriptor.path, interp_in, interp_out)?;
        let mut candidate = self.clone();
        let channel = candidate.channels.get_mut(channel_index).ok_or_else(|| {
            MondrianError::WorkflowStepFailed {
                step_id: "property_update_channel_keyframe_handles".to_string(),
                reason: format!(
                    "属性通道不存在: {}[{channel_index}]",
                    candidate.descriptor.path
                ),
            }
        })?;

        let keyframe_index =
            channel.keyframes.iter().position(|keyframe| keyframe.time == time).ok_or_else(
                || MondrianError::WorkflowStepFailed {
                    step_id: "property_update_channel_keyframe_handles".to_string(),
                    reason: format!(
                        "关键帧不存在: {}[{channel_index}] @ {}",
                        candidate.descriptor.path, time
                    ),
                },
            )?;
        let mut provided = channel.keyframes[keyframe_index].clone();
        provided.interp_in = interp_in;
        provided.interp_out = interp_out;
        validate_numeric_control_points(
            &candidate.descriptor.path,
            &provided,
            keyframe_index.checked_sub(1).map(|index| channel.keyframes[index].time),
            channel.keyframes.get(keyframe_index + 1).map(|key| key.time),
        )?;
        let keyframe = &mut channel.keyframes[keyframe_index];
        let old_in = keyframe.interp_in;
        let old_out = keyframe.interp_out;
        let cleared_auto = keyframe.temporal_flags.auto_bezier;
        keyframe.interp_in = interp_in;
        keyframe.interp_out = interp_out;
        if keyframe.temporal_flags.continuous && !keyframe.temporal_flags.broken_handles {
            if old_in != interp_in
                && old_out == interp_out
                && let (
                    KeyframeInterpolation::Bezier(edited),
                    KeyframeInterpolation::Bezier(partner),
                ) = (interp_in, old_out)
            {
                keyframe.interp_out = KeyframeInterpolation::Bezier(
                    crate::parameter::align_continuous_partner(edited, partner),
                );
            } else if old_out != interp_out
                && old_in == interp_in
                && let (
                    KeyframeInterpolation::Bezier(edited),
                    KeyframeInterpolation::Bezier(partner),
                ) = (interp_out, old_in)
            {
                keyframe.interp_in = KeyframeInterpolation::Bezier(
                    crate::parameter::align_continuous_partner(edited, partner),
                );
            }
        }
        if cleared_auto {
            for channel in &mut candidate.channels {
                if let Some(other) =
                    channel.keyframes.iter_mut().find(|candidate| candidate.time == time)
                {
                    other.temporal_flags.auto_bezier = false;
                    other.temporal_flags.continuous = true;
                    other.temporal_flags.broken_handles = false;
                }
            }
        }
        candidate.normalize_channels()?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Atomically change one numeric channel while admitting its retained constraints.
    pub fn update_channel_keyframe_value(
        &mut self,
        time: TimelineTime,
        channel_index: usize,
        value: f64,
    ) -> Result<()> {
        self.admit_channels(|candidate| {
            candidate.update_channel_keyframe_value_candidate(time, channel_index, value)
        })
    }

    fn update_channel_keyframe_value_candidate(
        &mut self,
        time: TimelineTime,
        channel_index: usize,
        value: f64,
    ) -> Result<()> {
        let value = self.normalize_numeric_channel(channel_index, value)?;
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

        keyframe.value = if self.descriptor.schema.value_type == PropertyValueType::Int {
            AnimationChannelValue::Integer(value.round() as i64)
        } else {
            AnimationChannelValue::Number(value)
        };
        self.normalize_channels()?;
        Ok(())
    }

    /// Atomically select the complete property key's temporal constraints.
    pub fn update_keyframe_temporal_flags(
        &mut self,
        time: TimelineTime,
        temporal_flags: KeyframeTemporalFlags,
    ) -> Result<()> {
        self.admit_channels(|candidate| {
            candidate.update_keyframe_temporal_flags_candidate(time, temporal_flags)
        })
    }

    fn update_keyframe_temporal_flags_candidate(
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

        self.normalize_channels()?;
        Ok(())
    }

    fn admit_channels(&mut self, edit: impl FnOnce(&mut Self) -> Result<()>) -> Result<()> {
        let mut candidate = self.clone();
        edit(&mut candidate)?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    fn normalize_channels(&mut self) -> Result<()> {
        for channel in &mut self.channels {
            channel.normalize_keyframes(&self.descriptor.schema.parameter_id)?;
        }
        Ok(())
    }

    fn normalize_value(&self, value: PropertyValue) -> Result<PropertyValue> {
        ensure_value_compatible(
            &self.descriptor.path,
            &self.descriptor.schema.default_value,
            &value,
        )?;
        if let PropertyValue::Resource(reference) = &value
            && self
                .descriptor
                .schema
                .resource
                .as_ref()
                .is_none_or(|contract| !contract.admits(reference))
        {
            return Err(parameter_value_error(
                &self.descriptor.path,
                "resource reference is not admitted by the provider",
            ));
        }
        if let PropertyValue::Curve(curve) = &value {
            curve.validate().map_err(|error| {
                parameter_value_error(&self.descriptor.path, &error.to_string())
            })?;
        }
        if let PropertyValue::QualifierSamples(samples) = &value {
            samples.validate().map_err(|error| {
                parameter_value_error(&self.descriptor.path, &error.to_string())
            })?;
        }
        if let PropertyValue::Enum(key) = &value
            && !self.descriptor.schema.enum_options.iter().any(|option| option.key == *key)
        {
            return Err(parameter_value_error(
                &self.descriptor.path,
                &format!("unknown enum key `{key}`"),
            ));
        }
        if let PropertyValue::Int(integer) = value {
            self.value_to_channel_values(&PropertyValue::Int(integer))?;
            let Some(contract) =
                self.descriptor.schema.numeric.as_ref().and_then(|contracts| contracts.first())
            else {
                return Ok(PropertyValue::Int(integer));
            };
            let (min, max) = contract.integer_bounds().ok_or_else(|| {
                parameter_value_error(&self.descriptor.path, "hard bounds contain no integer")
            })?;
            if !(min..=max).contains(&integer)
                && contract.invalid_value_policy == ParameterInvalidValuePolicy::Reject
            {
                return Err(parameter_value_error(
                    &self.descriptor.path,
                    "integer value lies outside its hard bounds",
                ));
            }
            return Ok(PropertyValue::Int(integer.clamp(min, max)));
        }
        let channels = self.value_to_channel_values(&value)?;
        if channels.is_empty() {
            return Ok(value);
        }
        let normalized = channels
            .iter()
            .enumerate()
            .map(|(index, value)| self.normalize_numeric_channel(index, *value))
            .collect::<Result<Vec<_>>>()?;
        if self.descriptor.schema.numeric.is_none() {
            return Ok(value);
        }
        Ok(self.value_from_channel_values(&normalized, &value))
    }

    fn value_to_author_channel_values(
        &self,
        value: &PropertyValue,
    ) -> Result<Vec<AnimationChannelValue>> {
        let numeric = self.value_to_channel_values(value)?;
        Ok(match value {
            PropertyValue::Int(value) => vec![AnimationChannelValue::Integer(*value)],
            _ => numeric.into_iter().map(AnimationChannelValue::Number).collect(),
        })
    }

    fn value_to_channel_values(&self, value: &PropertyValue) -> Result<Vec<f64>> {
        if let PropertyValue::Int(integer) = value
            && self.descriptor.schema.allowed_interpolations != [ParameterInterpolation::Hold]
            && integer.unsigned_abs() > (1_u64 << 53)
        {
            return Err(parameter_value_error(
                &self.descriptor.path,
                "interpolated integers must remain exactly representable in author channels",
            ));
        }
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
            if index.is_finite()
                && index >= 0.0
                && let Some(option) = self.descriptor.schema.enum_options.get(index as usize)
            {
                return PropertyValue::Enum(option.key.clone());
            }
            return fallback.clone();
        }
        // Preserve f64 author channels. Only correct a Float projection when rounding
        // an in-range channel would cross its exact declared bound.
        if self.value_type().uses_f32_channels()
            && let Some(contracts) = &self.descriptor.schema.numeric
            && channels.len() == self.value_type().channel_count()
        {
            let mut projected = [0.0; 4];
            for (index, value) in channels.iter().copied().enumerate() {
                projected[index] = value;
                if let Some(contract) = contracts.get(index)
                    && contract.hard_range.contains(value)
                {
                    let rounded = value as f32;
                    if f64::from(rounded) < contract.hard_range.min {
                        projected[index] = f64::from(rounded.next_up());
                    } else if f64::from(rounded) > contract.hard_range.max {
                        projected[index] = f64::from(rounded.next_down());
                    }
                }
            }
            return PropertyValue::from_channel_values(
                self.value_type(),
                &projected[..channels.len()],
                fallback,
            );
        }
        PropertyValue::from_channel_values(self.value_type(), channels, fallback)
    }

    fn normalize_numeric_channel(&self, index: usize, value: f64) -> Result<f64> {
        if !value.is_finite() {
            return Err(parameter_value_error(
                &self.descriptor.path,
                "numeric values must be finite",
            ));
        }
        let normalized = if let Some(contracts) = self.descriptor.schema.numeric.as_ref() {
            if contracts.len() != self.value_type().channel_count() {
                return Err(parameter_value_error(
                    &self.descriptor.path,
                    "numeric channel contracts must cover every declared channel",
                ));
            }
            let contract = contracts.get(index).ok_or_else(|| {
                parameter_value_error(&self.descriptor.path, "numeric channel contract is missing")
            })?;
            if contract.hard_range.contains(value) {
                value
            } else {
                match contract.invalid_value_policy {
                    ParameterInvalidValuePolicy::Reject => {
                        return Err(parameter_value_error(
                            &self.descriptor.path,
                            &format!(
                                "value {value} is outside [{}, {}]",
                                contract.hard_range.min, contract.hard_range.max
                            ),
                        ))
                    }
                    ParameterInvalidValuePolicy::Clamp => {
                        value.clamp(contract.hard_range.min, contract.hard_range.max)
                    }
                }
            }
        } else {
            value
        };
        if matches!(
            self.value_type(),
            PropertyValueType::IntVec2 | PropertyValueType::IntVec3
        ) && !(f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&normalized)
        {
            return Err(parameter_value_error(
                &self.descriptor.path,
                "integer vector channels must fit a signed 32-bit component",
            ));
        }
        if self.value_type().uses_f32_channels() && !(normalized as f32).is_finite() {
            return Err(parameter_value_error(
                &self.descriptor.path,
                "numeric channel cannot be represented as a finite Float value",
            ));
        }
        Ok(normalized)
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
            PropertyMutation::SetChannelKeyframe { path, channel_index, keyframe } => {
                self.ensure_path(&path)?;
                self.set_channel_keyframe(channel_index, keyframe)
            }
            PropertyMutation::PasteCompleteKeyframes { path, keyframes } => {
                self.ensure_path(&path)?;
                self.paste_complete_keyframes(keyframes)
            }
            PropertyMutation::EditKeyframe { path, keyframe_id, time, value } => {
                self.ensure_path(&path)?;
                ensure_value_compatible(&path, &self.descriptor.schema.default_value, &value)?;
                self.edit_keyframe(keyframe_id, time, value)
            }
            PropertyMutation::RemoveKeyframe { path, time } => {
                self.ensure_path(&path)?;
                self.remove_keyframe(time)?;
                Ok(())
            }
            PropertyMutation::RemoveKeyframes { path, keyframe_ids, retain_time } => {
                self.ensure_path(&path)?;
                self.remove_keyframes(&keyframe_ids, retain_time)
            }
            PropertyMutation::ShiftKeyframes { path, keyframe_ids, delta } => {
                self.ensure_path(&path)?;
                self.shift_keyframes(&keyframe_ids, delta)
            }
            PropertyMutation::TransformKeyframes {
                path,
                channel_index,
                keyframe_ids,
                time_delta,
                value_delta,
                steps,
            } => {
                self.ensure_path(&path)?;
                self.transform_keyframes(
                    &keyframe_ids,
                    channel_index,
                    time_delta,
                    value_delta,
                    steps,
                )
            }
            PropertyMutation::OffsetKeyframeValues {
                path,
                channel_index,
                keyframe_ids,
                delta,
                steps,
            } => {
                self.ensure_path(&path)?;
                self.offset_keyframe_values(&keyframe_ids, channel_index, delta, steps)
            }
            PropertyMutation::MoveKeyframe { path, from_time, to_time } => {
                self.ensure_path(&path)?;
                self.move_keyframe(from_time, to_time)
            }
            PropertyMutation::UpdateKeyframesInterpolation {
                path,
                keyframe_ids,
                interpolation,
            } => {
                self.ensure_path(&path)?;
                self.update_keyframes_interpolation(&keyframe_ids, interpolation)
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
                self.write_value(time, value, interpolation)
            }
            PropertyMutation::WriteChannels { path, time, channel_values, interpolation } => {
                self.ensure_path(&path)?;
                self.write_channels(time, &channel_values, interpolation)
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

/// One complete or sparse value edit addressed to a current author parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ParameterValueWrite {
    /// Replace the complete parameter value, including explicit complete gestures.
    Value {
        /// Stable owner-local parameter instance and Definition identity.
        parameter: AnimationParameterAddress,
        /// Typed value at an explicitly supplied owner-local author time.
        value: PropertyValue,
    },
    /// Change only the specified numeric components of the current author value.
    Channels {
        /// Stable owner-local parameter instance and Definition identity.
        parameter: AnimationParameterAddress,
        /// Unique component indices and their values in canonical author units.
        channel_values: Vec<(usize, f64)>,
    },
}

impl ParameterValueWrite {
    /// Resolve the stable target shared by complete and component edits.
    pub fn parameter(&self) -> &AnimationParameterAddress {
        match self {
            Self::Value { parameter, .. } | Self::Channels { parameter, .. } => parameter,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PropertyBag {
    properties: AuthoringMap<String, AnimatedProperty>,
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

    /// Prepare one value write resolved through stable author identity.
    ///
    /// The returned mutation contains the current path alias only after the
    /// stable instance and Parameter Schema identities have resolved. Values
    /// are normalized against the current schema before no-op comparison.
    /// Static properties receive a static write; an enabled animated property
    /// receives either a new key or an edit that preserves the identity,
    /// interpolation handles, and temporal flags of an existing complete key.
    /// Existing independent channel keys receive a complete channel write,
    /// retaining each channel's identity and interpolation metadata.
    pub fn prepare_value_write_by_address(
        &self,
        address: &AnimationParameterAddress,
        time: TimelineTime,
        value: PropertyValue,
    ) -> Result<Option<PropertyMutation>> {
        let (path, property) =
            self.property_by_address(address)
                .ok_or_else(|| MondrianError::WorkflowStepFailed {
                    step_id: "property_address_resolution".to_owned(),
                    reason: format!(
                        "parameter instance {} / {} is not owned by this Property Bag",
                        address.animation_track_id, address.parameter_id
                    ),
                })?;
        let value = property.normalize_value(value)?;
        if property.evaluate(time) == value {
            return Ok(None);
        }

        if property.is_enabled() {
            if let Some(existing) = property.keyframe_at(time) {
                if property.keyframe_by_id(existing.id).is_none() {
                    return Ok(Some(PropertyMutation::WriteChannels {
                        path: path.to_owned(),
                        time,
                        channel_values: property
                            .value_to_channel_values(&value)?
                            .into_iter()
                            .enumerate()
                            .collect(),
                        interpolation: property.default_interpolation()?,
                    }));
                }
                return Ok(Some(PropertyMutation::EditKeyframe {
                    path: path.to_owned(),
                    keyframe_id: existing.id,
                    time,
                    value,
                }));
            }
            let interpolation = property.default_interpolation()?;
            return Ok(Some(PropertyMutation::SetKeyframe {
                path: path.to_owned(),
                keyframe: Keyframe::from_preset(time, value, interpolation),
            }));
        }

        Ok(Some(PropertyMutation::SetStaticValue {
            path: path.to_owned(),
            value,
        }))
    }

    /// Prepare a complete or sparse write against this current author owner.
    ///
    /// Both edit forms resolve stable identity and reuse the canonical value and
    /// channel preparation rules, including no-op suppression and key metadata.
    pub fn prepare_parameter_write(
        &self,
        write: &ParameterValueWrite,
        time: TimelineTime,
    ) -> Result<Option<PropertyMutation>> {
        match write {
            ParameterValueWrite::Value { parameter, value } => {
                self.prepare_value_write_by_address(parameter, time, value.clone())
            }
            ParameterValueWrite::Channels { parameter, channel_values } => {
                self.prepare_channel_write_by_address(parameter, time, channel_values)
            }
        }
    }

    /// Prepare a sparse numeric-component edit using current stable author identity.
    ///
    /// Normalize and compare in the property's canonical value representation.
    /// Unchanged components are omitted, so an evaluated no-op cannot create a
    /// key. Application uses the existing channel mutation, preserving untouched
    /// siblings and existing key identities, handles and temporal constraints.
    /// Preparation allocates only channel values, never copies animation curves.
    pub fn prepare_channel_write_by_address(
        &self,
        address: &AnimationParameterAddress,
        time: TimelineTime,
        channel_values: &[(usize, f64)],
    ) -> Result<Option<PropertyMutation>> {
        let (path, property) =
            self.property_by_address(address)
                .ok_or_else(|| MondrianError::WorkflowStepFailed {
                    step_id: "property_address_resolution".to_owned(),
                    reason: format!(
                        "parameter instance {} / {} is not owned by this Property Bag",
                        address.animation_track_id, address.parameter_id
                    ),
                })?;
        validate_channel_updates(property.value_type().channel_count(), channel_values, path)?;
        if channel_values.is_empty() {
            return Ok(None);
        }
        let current_value = property.evaluate(time);
        let current = property.value_to_channel_values(&current_value)?;
        if current.len() != property.value_type().channel_count() {
            return Err(parameter_value_error(
                path,
                "canonical value and channel counts differ",
            ));
        }
        let mut candidate = current.clone();
        for &(index, value) in channel_values {
            candidate[index] = property.normalize_numeric_channel(index, value)?;
        }
        let normalized = property
            .normalize_value(property.value_from_channel_values(&candidate, &current_value))?;
        if current_value == normalized {
            return Ok(None);
        }
        let normalized = property.value_to_channel_values(&normalized)?;
        let changed = channel_values
            .iter()
            .filter_map(|&(index, _)| {
                // A held i64 can exceed f64's consecutive-integer precision.
                // Its canonical value comparison above retains the edit intent
                // even when old/new numeric channel projections coincide.
                (current.len() == 1 || current[index] != normalized[index])
                    .then_some((index, normalized[index]))
            })
            .collect::<Vec<_>>();
        if changed.is_empty() {
            return Ok(None);
        }
        Ok(Some(PropertyMutation::WriteChannels {
            path: path.to_owned(),
            time,
            channel_values: changed,
            interpolation: property.default_interpolation()?,
        }))
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

    /// Remove one half-open Sequence-time range from every property.
    ///
    /// This is the gap-closing counterpart of
    /// [`Self::shift_keyframes_at_or_after`]. Clip-local property bags must not
    /// call this method because their coordinate space moves with the Clip.
    pub fn extract_time_range(&mut self, range: TimelineTimeRange) -> Result<()> {
        let mut candidate = self.clone();
        for property in candidate.properties.values_mut() {
            property.extract_time_range(range)?;
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
        property.write_value(time, value, interpolation)
    }

    pub fn write_channels(
        &mut self,
        path: &str,
        time: TimelineTime,
        channel_values: &[(usize, f64)],
        interpolation: InterpolationType,
    ) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.write_channels(time, channel_values, interpolation)
    }

    pub fn remove_keyframe(&mut self, path: &str, time: TimelineTime) -> Result<()> {
        let property = self.require_property_mut(path)?;
        property.remove_keyframe(time)?;
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
            PropertyMutation::SetChannelKeyframe { path, channel_index, keyframe } => {
                self.require_property_mut(&path)?.set_channel_keyframe(channel_index, keyframe)
            }
            PropertyMutation::PasteCompleteKeyframes { path, keyframes } => {
                self.require_property_mut(&path)?.paste_complete_keyframes(keyframes)
            }
            PropertyMutation::EditKeyframe { path, keyframe_id, time, value } => {
                self.edit_keyframe(&path, keyframe_id, time, value)
            }
            PropertyMutation::RemoveKeyframe { path, time } => self.remove_keyframe(&path, time),
            PropertyMutation::RemoveKeyframes { path, keyframe_ids, retain_time } => {
                self.require_property_mut(&path)?.remove_keyframes(&keyframe_ids, retain_time)
            }
            PropertyMutation::ShiftKeyframes { path, keyframe_ids, delta } => {
                self.require_property_mut(&path)?.shift_keyframes(&keyframe_ids, delta)
            }
            PropertyMutation::TransformKeyframes {
                path,
                channel_index,
                keyframe_ids,
                time_delta,
                value_delta,
                steps,
            } => self.require_property_mut(&path)?.transform_keyframes(
                &keyframe_ids,
                channel_index,
                time_delta,
                value_delta,
                steps,
            ),
            PropertyMutation::OffsetKeyframeValues {
                path,
                channel_index,
                keyframe_ids,
                delta,
                steps,
            } => self.require_property_mut(&path)?.offset_keyframe_values(
                &keyframe_ids,
                channel_index,
                delta,
                steps,
            ),
            PropertyMutation::MoveKeyframe { path, from_time, to_time } => {
                self.move_keyframe(&path, from_time, to_time)
            }
            PropertyMutation::UpdateKeyframesInterpolation {
                path,
                keyframe_ids,
                interpolation,
            } => self
                .require_property_mut(&path)?
                .update_keyframes_interpolation(&keyframe_ids, interpolation),
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
    /// Insert or move one canonical channel key without evaluating its scalar.
    SetChannelKeyframe {
        /// Current property path resolved by its owning author interface.
        path: String,
        /// Declared channel index in the owning parameter schema.
        channel_index: usize,
        /// Exact author value, identity, time and interpolation metadata.
        keyframe: Keyframe<AnimationChannelValue>,
    },
    /// Paste complete raw channel groups with one final-topology normalization.
    PasteCompleteKeyframes {
        /// Current owner-resolved property path.
        path: String,
        /// Each group contains every declared channel in schema order.
        keyframes: Vec<Vec<Keyframe<AnimationChannelValue>>>,
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
    /// Remove complete stable keys together, retaining the sampled value if no keys survive.
    RemoveKeyframes {
        /// Address of the owning property.
        path: String,
        /// Nonempty unique complete key identities.
        keyframe_ids: Vec<KeyframeId>,
        /// Exact owner time sampled before deleting the final keys.
        retain_time: TimelineTime,
    },
    /// Translate complete stable keys together without reconstructing typed values.
    ShiftKeyframes {
        /// Address of the owning property.
        path: String,
        /// Nonempty unique complete key identities.
        keyframe_ids: Vec<KeyframeId>,
        /// Signed exact owner-time translation.
        delta: TimelineTime,
    },
    /// Translate complete keys and offset one channel as one admitted mutation.
    TransformKeyframes {
        /// Address of the owning property.
        path: String,
        /// Declared numeric channel to offset; every channel shares the time translation.
        channel_index: usize,
        /// Nonempty unique complete key identities.
        keyframe_ids: Vec<KeyframeId>,
        /// Signed exact owner-time translation.
        time_delta: TimelineTime,
        /// Finite common increment in author value units.
        value_delta: f64,
        /// Signed value increment count.
        steps: i8,
    },
    /// Offset one channel of complete stable keys by a common bounded displacement.
    OffsetKeyframeValues {
        /// Address of the owning property.
        path: String,
        /// Declared numeric channel to edit.
        channel_index: usize,
        /// Nonempty unique complete key identities.
        keyframe_ids: Vec<KeyframeId>,
        /// Finite increment in author value units.
        delta: f64,
        /// Signed increment count.
        steps: i8,
    },
    MoveKeyframe {
        path: String,
        from_time: TimelineTime,
        to_time: TimelineTime,
    },
    /// Apply one interpolation intent to a complete stable key selection atomically.
    UpdateKeyframesInterpolation {
        /// Owning property path.
        path: String,
        /// Nonempty unique complete key identities.
        keyframe_ids: Vec<KeyframeId>,
        /// Preset governed by the property's current schema.
        interpolation: InterpolationType,
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
            | Self::SetChannelKeyframe { path, .. }
            | Self::PasteCompleteKeyframes { path, .. }
            | Self::EditKeyframe { path, .. }
            | Self::RemoveKeyframe { path, .. }
            | Self::RemoveKeyframes { path, .. }
            | Self::ShiftKeyframes { path, .. }
            | Self::TransformKeyframes { path, .. }
            | Self::OffsetKeyframeValues { path, .. }
            | Self::MoveKeyframe { path, .. }
            | Self::UpdateKeyframesInterpolation { path, .. }
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
            SetChannelKeyframe { path, channel_index, keyframe } => {
                SetChannelKeyframe { path: f(path), channel_index, keyframe }
            }
            PasteCompleteKeyframes { path, keyframes } => {
                PasteCompleteKeyframes { path: f(path), keyframes }
            }
            EditKeyframe { path, keyframe_id, time, value } => {
                EditKeyframe { path: f(path), keyframe_id, time, value }
            }
            RemoveKeyframe { path, time } => RemoveKeyframe { path: f(path), time },
            RemoveKeyframes { path, keyframe_ids, retain_time } => {
                RemoveKeyframes { path: f(path), keyframe_ids, retain_time }
            }
            ShiftKeyframes { path, keyframe_ids, delta } => {
                ShiftKeyframes { path: f(path), keyframe_ids, delta }
            }
            TransformKeyframes {
                path,
                channel_index,
                keyframe_ids,
                time_delta,
                value_delta,
                steps,
            } => TransformKeyframes {
                path: f(path),
                channel_index,
                keyframe_ids,
                time_delta,
                value_delta,
                steps,
            },
            OffsetKeyframeValues { path, channel_index, keyframe_ids, delta, steps } => {
                OffsetKeyframeValues {
                    path: f(path),
                    channel_index,
                    keyframe_ids,
                    delta,
                    steps,
                }
            }
            MoveKeyframe { path, from_time, to_time } => {
                MoveKeyframe { path: f(path), from_time, to_time }
            }
            UpdateKeyframesInterpolation { path, keyframe_ids, interpolation } => {
                UpdateKeyframesInterpolation { path: f(path), keyframe_ids, interpolation }
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

fn validate_visual_handle_coordinates(
    path: &str,
    interp_in: KeyframeInterpolation,
    interp_out: KeyframeInterpolation,
) -> Result<()> {
    for (interpolation, incoming) in [(interp_in, true), (interp_out, false)] {
        let KeyframeInterpolation::Bezier(handle) = interpolation else {
            continue;
        };
        if !handle.value_offset.is_finite() {
            return Err(parameter_value_error(
                path,
                "Bezier value coordinates must be finite",
            ));
        }
        if (incoming && handle.time_offset > TimelineTime::ZERO)
            || (!incoming && handle.time_offset < TimelineTime::ZERO)
        {
            return Err(parameter_value_error(
                path,
                "Bezier time coordinates must remain on their temporal side within the segment",
            ));
        }
    }
    Ok(())
}

fn validate_numeric_control_points(
    path: &str,
    key: &Keyframe<AnimationChannelValue>,
    previous: Option<TimelineTime>,
    next: Option<TimelineTime>,
) -> Result<()> {
    validate_visual_handle_coordinates(path, key.interp_in, key.interp_out)?;
    for (interpolation, neighbor, incoming) in [
        (key.interp_in, previous, true),
        (key.interp_out, next, false),
    ] {
        if let KeyframeInterpolation::Bezier(handle) = interpolation {
            if !(key.value.projection() + handle.value_offset).is_finite() {
                return Err(parameter_value_error(
                    path,
                    "Bezier value control is not representable",
                ));
            }
            if let Some(neighbor) = neighbor {
                let duration = if incoming {
                    key.time.checked_sub(neighbor)?
                } else {
                    neighbor.checked_sub(key.time)?
                };
                let magnitude = if incoming {
                    handle.time_offset.checked_scale(crate::TimeScale::NEGATIVE_ONE)?
                } else {
                    handle.time_offset
                };
                if magnitude > duration {
                    return Err(parameter_value_error(
                        path,
                        "Bezier time control exceeds the owner-time segment",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn keyframe_interpolation_type(keyframe: &Keyframe<PropertyValue>) -> ParameterInterpolation {
    interpolation_type_from_parts(
        keyframe.interp_in,
        keyframe.interp_out,
        keyframe.temporal_flags,
    )
}

fn channel_keyframe_interpolation_type(
    keyframe: &Keyframe<AnimationChannelValue>,
) -> ParameterInterpolation {
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
    if channel_values.len() > channel_count {
        return Err(parameter_value_error(path, "too many component updates"));
    }
    let mut seen = HashSet::with_capacity(channel_values.len());
    for (index, _) in channel_values {
        if *index >= channel_count {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "property_channel_bounds".to_string(),
                reason: format!("属性通道越界: {path}[{index}]"),
            });
        }
        if !seen.insert(*index) {
            return Err(parameter_value_error(
                path,
                &format!("duplicate channel index {index}"),
            ));
        }
    }
    Ok(())
}

fn evaluate_numeric_channel(channel: &AnimationChannel, time: TimelineTime, fallback: f64) -> f64 {
    if channel.keyframes.is_empty() {
        return fallback;
    }
    let Some(first) = channel.keyframes.first() else {
        return fallback;
    };
    if time <= first.time {
        return first.value.projection();
    }
    let Some(last) = channel.keyframes.last() else {
        return fallback;
    };
    if time >= last.time {
        return last.value.projection();
    }

    let idx = channel
        .keyframes
        .partition_point(|keyframe| keyframe.time <= time)
        .saturating_sub(1);
    let keyframe_a = &channel.keyframes[idx];
    let keyframe_b = &channel.keyframes[idx + 1];
    query::NumericSegment::between(keyframe_a, keyframe_b).evaluate(normalize_time(
        time,
        keyframe_a.time,
        keyframe_b.time,
    ))
}

fn normalize_time(t: TimelineTime, start: TimelineTime, end: TimelineTime) -> f64 {
    let total = end.distance_from_f64(start);
    if total <= 0.0 {
        return 0.0;
    }
    t.distance_from_f64(start) / total
}

fn numeric_segment_control(
    interpolation: KeyframeInterpolation,
    left: f64,
    right: f64,
    span: f64,
    exact_span: Option<TimelineTime>,
    incoming: bool,
) -> (f64, f64, bool) {
    let ratio = if incoming { 1.0 - 1.0 / 3.0 } else { 1.0 / 3.0 };
    let owner_value = if incoming { right } else { left };
    match interpolation {
        KeyframeInterpolation::Bezier(handle) => {
            let fraction = handle.time_offset.to_f64() / span;
            (
                if incoming { 1.0 + fraction } else { fraction },
                owner_value + handle.value_offset,
                exact_span.is_some_and(|duration| {
                    crate::TimeScale::new(if incoming { -3 } else { 3 }, 1).is_ok_and(|scale| {
                        handle
                            .time_offset
                            .checked_scale(scale)
                            .is_ok_and(|offset| offset == duration)
                    })
                }),
            )
        }
        KeyframeInterpolation::DefaultBezier => (ratio, owner_value, true),
        KeyframeInterpolation::Linear => {
            (ratio, interpolate_numeric_value(left, right, ratio), true)
        }
        KeyframeInterpolation::Hold => (ratio, owner_value, true),
    }
}

mod interp;
pub use interp::*;

impl crate::AuthoringFootprint for InterpolationType {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        match self {
            Self::Hold
            | Self::Linear
            | Self::Bezier
            | Self::AutoBezier
            | Self::ContinuousBezier
            | Self::EaseIn
            | Self::EaseOut => Ok(()),
        }
    }
}

impl crate::AuthoringFootprint for ParameterInterpolation {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        match self {
            Self::Hold | Self::Linear | Self::Bezier => Ok(()),
        }
    }
}

impl crate::AuthoringFootprint for KeyframeInterpolation {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        match self {
            Self::Hold | Self::Linear | Self::Bezier(_) | Self::DefaultBezier => Ok(()),
        }
    }
}

impl crate::AuthoringFootprint for ExactBezierHandle {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self { time_offset: _, value_offset: _ } = self;
        Ok(())
    }
}

impl crate::AuthoringFootprint for KeyframeTemporalFlags {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self { auto_bezier: _, continuous: _, broken_handles: _ } = self;
        Ok(())
    }
}

impl crate::AuthoringFootprint for PropertyValueType {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        match self {
            Self::Bool
            | Self::Int
            | Self::Float
            | Self::Double
            | Self::DoubleVec2
            | Self::DoubleVec3
            | Self::DoubleRgb
            | Self::DoubleColor
            | Self::IntVec2
            | Self::IntVec3
            | Self::Vec2
            | Self::Vec3
            | Self::Color
            | Self::Vec4
            | Self::Curve
            | Self::QualifierSamples
            | Self::Enum
            | Self::Resource
            | Self::Text => Ok(()),
        }
    }
}

impl crate::AuthoringFootprint for ParameterUnit {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        match self {
            Self::Unitless
            | Self::Pixels
            | Self::Normalized
            | Self::Percent
            | Self::Degrees
            | Self::TimelineTime
            | Self::Stops
            | Self::Nits
            | Self::Decibels
            | Self::Milliseconds
            | Self::Samples => Ok(()),
        }
    }
}

impl crate::AuthoringFootprint for ParameterNumericRange {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self { min: _, max: _ } = self;
        Ok(())
    }
}

impl crate::AuthoringFootprint for ParameterNumericContract {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self {
            hard_range: _,
            soft_range: _,
            step: _,
            invalid_value_policy: _,
        } = self;
        Ok(())
    }
}

impl crate::AuthoringFootprint for ParameterInvalidValuePolicy {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        match self {
            Self::Reject | Self::Clamp => Ok(()),
        }
    }
}

impl crate::AuthoringFootprint for ParameterCacheImpact {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        match self {
            Self::None | Self::Value | Self::Resource | Self::Topology => Ok(()),
        }
    }
}

impl crate::AuthoringFootprint for PropertyValue {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        match self {
            Self::Enum(value) | Self::Text(value) => collector.collect(value),
            Self::Resource(reference) => collector.collect(reference),
            Self::Curve(curve) => collector.collect(&curve.points),
            Self::QualifierSamples(samples) => collector.collect(&samples.0),
            Self::Bool(_)
            | Self::Int(_)
            | Self::Float(_)
            | Self::Double(_)
            | Self::DoubleVec2(_)
            | Self::DoubleVec3(_)
            | Self::DoubleRgb(_)
            | Self::DoubleColor(_)
            | Self::IntVec2(_)
            | Self::IntVec3(_)
            | Self::Vec2(_)
            | Self::Vec3(_)
            | Self::Color(_)
            | Self::Vec4(_) => Ok(()),
        }
    }
}

impl crate::AuthoringFootprint for NormalizedCurvePoint {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self { x: _, y: _ } = self;
        Ok(())
    }
}

impl crate::AuthoringFootprint for QualifierSampleOperation {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        match self {
            Self::Include | Self::Exclude => Ok(()),
        }
    }
}

impl crate::AuthoringFootprint for QualifierSample {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self { rgb: _, operation: _ } = self;
        Ok(())
    }
}

impl crate::AuthoringFootprint for ParameterEnumOption {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self { key, display_name, message_id, display_rank: _ } = self;
        collector.collect(key)?;
        collector.collect(display_name)?;
        collector.collect(message_id)
    }
}

impl crate::AuthoringFootprint for ParameterResourceReference {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        match self {
            Self::ExternalFile { path } => collector.collect(path),
            Self::Uri { uri } => collector.collect(uri),
            Self::Unbound | Self::ProjectAsset { asset_id: _ } => Ok(()),
        }
    }
}

impl crate::AuthoringFootprint for AnimatablePropertyUiMetadata {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self { group_name, supports_spatial: _, display_order: _ } = self;
        collector.collect(group_name)
    }
}

impl crate::AuthoringFootprint for ParameterResourceContract {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self { allowed_kinds, path: _, file_extensions } = self;
        collector.collect(allowed_kinds)?;
        collector.collect(file_extensions)
    }
}
impl crate::AuthoringFootprint for ParameterResourceKind {
    fn collect_authoring_footprint(
        &self,
        _: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        Ok(())
    }
}

impl crate::AuthoringFootprint for ParameterSchema {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self {
            parameter_id,
            schema_version: _,
            message_id,
            value_type: _,
            default_value,
            is_animatable: _,
            unit: _,
            numeric,
            text_mode: _,
            resource,
            allowed_interpolations,
            enum_options,
            cache_impact: _,
        } = self;
        collector.collect(parameter_id)?;
        collector.collect(message_id)?;
        collector.collect(default_value)?;
        collector.collect(numeric)?;
        collector.collect(allowed_interpolations)?;
        collector.collect(enum_options)?;
        collector.collect(resource)
    }
}

impl crate::AuthoringFootprint for PropertyDescriptor {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self { schema, path, display_name, ui_metadata } = self;
        collector.collect(schema)?;
        collector.collect(path)?;
        collector.collect(display_name)?;
        collector.collect(ui_metadata)
    }
}

impl<T: crate::AuthoringFootprint> crate::AuthoringFootprint for Keyframe<T> {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self {
            id: _,
            time: _,
            value,
            interp_in: _,
            interp_out: _,
            temporal_flags: _,
        } = self;
        collector.collect(value)
    }
}

impl crate::AuthoringFootprint for AnimationChannel {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self { index: _, keyframes } = self;
        collector.collect(keyframes)
    }
}

impl crate::AuthoringFootprint for AnimatedProperty {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self {
            track_id: _,
            descriptor,
            static_value,
            animation_enabled: _,
            channels,
        } = self;
        collector.collect(descriptor)?;
        collector.collect(static_value)?;
        collector.collect(channels)
    }
}

impl crate::AuthoringFootprint for AnimationParameterAddress {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self { animation_track_id: _, parameter_id } = self;
        collector.collect(parameter_id)
    }
}

impl crate::AuthoringFootprint for PropertyBag {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self { properties } = self;
        collector.collect(properties)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn combined_complete_key_transform_moves_all_channels_and_preserves_other_values() {
        let contract =
            ParameterNumericContract::closed(0.0, 1.0, None, ParameterInvalidValuePolicy::Reject)
                .expect("range");
        for mode in [
            InterpolationType::AutoBezier,
            InterpolationType::ContinuousBezier,
        ] {
            let descriptor = PropertyDescriptor::new(
                "test.transform",
                "Vector",
                PropertyValue::DoubleVec2([0.0, 0.0]),
            )
            .with_animatable(true)
            .with_numeric_contract(ParameterUnit::Unitless, contract);
            let mut property = AnimatedProperty::from_descriptor(descriptor);
            property.set_animation_enabled(true);
            for (n, values) in [(0, [0.25, 0.2]), (1, [0.75, 0.5]), (2, [0.5, 0.4])] {
                property
                    .set_exact_keyframe(Keyframe::linear(
                        TimelineTime::new(n, 3).expect("time"),
                        PropertyValue::DoubleVec2(values),
                    ))
                    .expect("seed");
            }
            let ids =
                property.channels()[0].keyframes().iter().map(|key| key.id).collect::<Vec<_>>();
            property.update_keyframes_interpolation(&ids, mode).expect("mode");
            let original = property.clone();
            for (selection, channel, time, value) in [
                (vec![], 0, TimelineTime::ZERO, 0.0),
                (vec![ids[0], ids[0]], 0, TimelineTime::ZERO, 0.0),
                (vec![ids[0], KeyframeId::new()], 0, TimelineTime::ZERO, 0.0),
                (ids.clone(), 9, TimelineTime::ONE_THIRD, 0.125),
                (ids[..2].to_vec(), 0, TimelineTime::ONE_THIRD, 0.125),
                (ids.clone(), 0, TimelineTime::ONE_THIRD, f64::NAN),
                (
                    ids.clone(),
                    0,
                    TimelineTime::new(i64::MAX, 1).expect("huge"),
                    0.125,
                ),
            ] {
                assert!(property.transform_keyframes(&selection, channel, time, value, 1).is_err());
                assert_eq!(
                    property, original,
                    "joint edit is all or nothing across every channel"
                );
            }
            let translation = TimelineTime::new(1, 7).expect("shift");
            property
                .transform_keyframes(&ids, 0, translation, 0.125, 1)
                .expect("joint edit");
            property.validate().expect("final topology and constraints");
            for n in -3..=30 {
                let time = TimelineTime::new(n, 31).expect("query");
                let PropertyValue::DoubleVec2(before) = original.evaluate(time) else {
                    panic!("vector")
                };
                let PropertyValue::DoubleVec2(after) =
                    property.evaluate(time.checked_add(translation).expect("shifted query"))
                else {
                    panic!("vector")
                };
                assert!((after[0] - before[0] - 0.125).abs() < 1e-12);
                assert!((after[1] - before[1]).abs() < 1e-12);
            }
            for (channel, source) in property.channels().iter().zip(original.channels()) {
                for (key, source) in channel.keyframes().iter().zip(source.keyframes()) {
                    assert_eq!(key.id, source.id);
                    assert_eq!(
                        key.time.checked_sub(source.time).expect("offset"),
                        translation
                    );
                    assert_eq!(key.temporal_flags, source.temporal_flags);
                    assert_eq!(key.interp_in, source.interp_in);
                    assert_eq!(key.interp_out, source.interp_out);
                }
            }
            let mut via_mutation = original.clone();
            let mutation = PropertyMutation::TransformKeyframes {
                path: "test.transform".into(),
                channel_index: 0,
                keyframe_ids: ids,
                time_delta: translation,
                value_delta: 0.125,
                steps: 1,
            };
            assert_eq!(mutation.path(), "test.transform");
            let mut bag = PropertyBag::default();
            bag.upsert(original.clone());
            bag.define(PropertyDescriptor::new(
                "unrelated",
                "Other",
                PropertyValue::Int(42),
            ));
            let unrelated = bag.property("unrelated").expect("other").clone();
            let nested = mutation.clone().map_path(|path| format!("nested.{path}"));
            assert_eq!(nested.path(), "nested.test.transform");
            bag.apply_mutation(
                nested.map_path(|path| path.trim_start_matches("nested.").to_owned()),
            )
            .expect("owner bag dispatch");
            assert_eq!(bag.property("test.transform"), Some(&property));
            assert_eq!(bag.property("unrelated"), Some(&unrelated));
            via_mutation.apply_mutation(mutation).expect("shared mutation dispatch");
            assert_eq!(via_mutation, property);
            let restored: AnimatedProperty =
                serde_json::from_slice(&serde_json::to_vec(&property).expect("encode"))
                    .expect("decode");
            assert_eq!(restored, property);
        }
    }

    #[test]
    fn combined_integer_key_transform_retains_values_above_float_integer_precision() {
        let start = (1_i64 << 53) + 1;
        let descriptor = PropertyDescriptor::new(
            "test.integer.transform",
            "Integer",
            PropertyValue::Int(start),
        )
        .with_animatable(true)
        .with_numeric_contract(
            ParameterUnit::Unitless,
            ParameterNumericContract::closed(
                0.0,
                i64::MAX as f64,
                Some(1.0),
                ParameterInvalidValuePolicy::Reject,
            )
            .expect("range"),
        );
        let mut property = AnimatedProperty::from_descriptor(descriptor);
        property.set_animation_enabled(true);
        for (n, value) in [(0, start), (1, start + 2)] {
            property
                .set_exact_keyframe(Keyframe::from_preset(
                    TimelineTime::new(n, 3).expect("time"),
                    PropertyValue::Int(value),
                    InterpolationType::Hold,
                ))
                .expect("seed exact integer");
        }
        let ids = property.channels()[0].keyframes().iter().map(|key| key.id).collect::<Vec<_>>();
        let delta = TimelineTime::new(1, 7).expect("shift");
        property.transform_keyframes(&ids, 0, delta, 1.0, 3).expect("exact joint edit");
        for (n, id) in ids.iter().enumerate() {
            let key = property.keyframe_by_id(*id).expect("same complete key");
            assert_eq!(key.value, PropertyValue::Int(start + n as i64 * 2 + 3));
            assert_eq!(
                key.time,
                TimelineTime::new(n as i64, 3)
                    .expect("origin")
                    .checked_add(delta)
                    .expect("shifted")
            );
        }
    }

    #[test]
    fn selected_key_value_offset_preserves_exact_author_keys_and_constraints() {
        for mode in [
            InterpolationType::AutoBezier,
            InterpolationType::ContinuousBezier,
        ] {
            let contract = ParameterNumericContract::closed(
                0.0,
                1.0,
                Some(0.1),
                ParameterInvalidValuePolicy::Reject,
            )
            .expect("range");
            let mut property = AnimatedProperty::from_descriptor(
                PropertyDescriptor::new("test.group.value", "Value", PropertyValue::Double(0.0))
                    .with_animatable(true)
                    .with_numeric_contract(ParameterUnit::Unitless, contract),
            );
            property.set_animation_enabled(true);
            let mut keys = Vec::new();
            for (n, value) in [(1, 0.2), (2, 0.8)] {
                let time = TimelineTime::new(n, 7).expect("exact time");
                property
                    .write_channels(time, &[(0, value)], InterpolationType::Linear)
                    .expect("seed");
                keys.push((time, property.keyframe_at(time).expect("key").id));
            }
            for (time, _) in &keys {
                property.update_keyframe_interpolation(*time, mode).expect("mode");
            }
            let original = property.clone();
            let ids = keys.iter().map(|(_, id)| *id).collect::<Vec<_>>();
            for selection in [
                vec![],
                vec![ids[0], ids[0]],
                vec![ids[0], KeyframeId::new()],
            ] {
                assert!(property.offset_keyframe_values(&selection, 0, 0.0, 0).is_err());
                assert_eq!(property, original);
            }
            assert!(property.offset_keyframe_values(&ids, 9, 1.0, 1).is_err());
            assert_eq!(property, original);
            property.offset_keyframe_values(&ids, 0, 0.1, 5).expect("one group shift");
            property.validate().expect("constraints");
            for (index, (time, id)) in keys.iter().enumerate() {
                let key = property.keyframe_at(*time).expect("same author time");
                assert_eq!(key.id, *id);
                let PropertyValue::Double(value) = key.value else {
                    panic!("double");
                };
                assert!((value - [0.4, 1.0][index]).abs() < 1e-15);
            }
            let changed = property.clone();
            let restored: AnimatedProperty =
                serde_json::from_slice(&serde_json::to_vec(&property).expect("encode"))
                    .expect("decode");
            assert_eq!(restored, changed);
            property.offset_keyframe_values(&ids, 0, 1.0, 5).expect("bound no-op");
            assert_eq!(property, changed);
        }
    }

    #[test]
    fn numeric_group_bound_noop_retains_signed_zero_bits() {
        let contract = ParameterNumericContract::closed(
            -f64::MAX,
            f64::MAX,
            None,
            ParameterInvalidValuePolicy::Reject,
        )
        .expect("range");
        let shifted = contract
            .offset_scalars(
                &[PropertyValue::Double(-0.0), PropertyValue::Double(f64::MAX)],
                f64::MAX,
                5,
            )
            .expect("bounded zero");
        let PropertyValue::Double(zero) = shifted[0] else {
            panic!("double");
        };
        assert_eq!(zero.to_bits(), (-0.0f64).to_bits());
    }

    #[test]
    fn numeric_group_offset_reaches_tiny_common_bound_with_huge_increment() {
        use super::{ParameterInvalidValuePolicy, ParameterNumericContract, PropertyValue};
        let tiny = f64::MIN_POSITIVE;
        let contract =
            ParameterNumericContract::closed(0.0, tiny, None, ParameterInvalidValuePolicy::Reject)
                .expect("tiny range");
        let values = [
            PropertyValue::Double(0.0),
            PropertyValue::Double(tiny / 2.0),
        ];
        assert_eq!(
            contract.offset_scalars(&values, f64::MAX, 5),
            Some(vec![
                PropertyValue::Double(tiny / 2.0),
                PropertyValue::Double(tiny)
            ])
        );
    }

    #[test]
    fn numeric_group_offset_preserves_relative_values_at_common_bounds() {
        let contract = super::ParameterNumericContract::closed(
            0.0,
            1.0,
            None,
            super::ParameterInvalidValuePolicy::Reject,
        )
        .expect("contract");
        let values = [
            super::PropertyValue::Double(0.2),
            super::PropertyValue::Double(0.8),
        ];
        let shifted = contract.offset_scalars(&values, 0.1, 5).expect("group");
        assert_eq!(shifted[1], super::PropertyValue::Double(1.0));
        let super::PropertyValue::Double(first) = shifted[0] else {
            panic!("double");
        };
        assert!((first - 0.4).abs() < 1e-15);
        let reversed = contract.offset_scalars(&values, -0.1, 5).expect("reverse");
        assert_eq!(reversed[0], super::PropertyValue::Double(0.0));
        let super::PropertyValue::Double(second) = reversed[1] else {
            panic!("double");
        };
        assert!((second - 0.6).abs() < 1e-15);
        assert_eq!(
            contract.offset_scalars(&values, 0.0, 5),
            Some(values.to_vec())
        );
        assert!(contract.offset_scalars(&[], 1.0, 1).is_none());
        assert!(contract
            .offset_scalars(
                &[
                    super::PropertyValue::Double(0.1),
                    super::PropertyValue::Int(0)
                ],
                0.1,
                1
            )
            .is_none());
        assert!(contract
            .offset_scalars(&[super::PropertyValue::Double(f64::NAN)], 0.1, 0)
            .is_none());
    }

    #[test]
    fn numeric_group_offset_preserves_wide_integer_bits_and_extreme_float_span() {
        use super::{ParameterInvalidValuePolicy, ParameterNumericContract, PropertyValue};
        let integer = ParameterNumericContract::closed(
            i64::MIN as f64,
            i64::MAX as f64,
            None,
            ParameterInvalidValuePolicy::Reject,
        )
        .expect("integer");
        let wide = 9_007_199_254_740_993i64;
        assert_eq!(
            integer.offset_scalars(
                &[PropertyValue::Int(wide), PropertyValue::Int(wide + 2)],
                1.0,
                1
            ),
            Some(vec![
                PropertyValue::Int(wide + 1),
                PropertyValue::Int(wide + 3)
            ])
        );
        let endpoints = [PropertyValue::Int(i64::MIN), PropertyValue::Int(i64::MAX)];
        assert_eq!(
            integer.offset_scalars(&endpoints, f64::MAX, 5),
            Some(endpoints.to_vec())
        );
        assert_eq!(
            integer.offset_scalars(&endpoints, f64::MAX, -5),
            Some(endpoints.to_vec())
        );
        let float = ParameterNumericContract::closed(
            -f64::MAX,
            f64::MAX,
            None,
            ParameterInvalidValuePolicy::Reject,
        )
        .expect("float");
        assert_eq!(
            float.offset_scalars(
                &[
                    PropertyValue::Double(-f64::MAX),
                    PropertyValue::Double(-f64::MAX / 2.0)
                ],
                f64::MAX,
                5
            ),
            Some(vec![
                PropertyValue::Double(f64::MAX / 2.0),
                PropertyValue::Double(f64::MAX)
            ])
        );
        let original = [PropertyValue::Double(-0.0), PropertyValue::Double(0.2)];
        let unchanged = float.offset_scalars(&original, 0.0, 5).expect("zero");
        let PropertyValue::Double(zero) = unchanged[0] else {
            panic!("double");
        };
        assert_eq!(zero.to_bits(), (-0.0f64).to_bits());
        assert_eq!(unchanged[1], original[1]);
    }

    use super::*;

    #[test]
    fn held_integer_author_keys_preserve_all_bits_through_edit_and_codec() {
        let numeric = ParameterNumericContract::closed(
            i64::MIN as f64,
            i64::MAX as f64,
            Some(1.0),
            ParameterInvalidValuePolicy::Reject,
        )
        .expect("full range");
        let mut property = AnimatedProperty::from_descriptor(
            PropertyDescriptor::new("test.exact.integer", "Count", PropertyValue::Int(0))
                .with_animatable(true)
                .with_numeric_contract(ParameterUnit::Unitless, numeric),
        );
        let time = TimelineTime::new(1, 3).expect("exact time");
        let key = Keyframe::from_preset(
            time,
            PropertyValue::Int(i64::MAX - 1),
            InterpolationType::Hold,
        );
        let id = key.id;
        property.set_exact_keyframe(key).expect("author key");
        assert_eq!(
            property.keyframe_by_id(id).expect("read exact key").value,
            PropertyValue::Int(i64::MAX - 1)
        );
        assert_eq!(
            property.evaluate(TimelineTime::ZERO),
            PropertyValue::Int(i64::MAX - 1)
        );
        property
            .edit_keyframe(id, time, PropertyValue::Int(i64::MIN + 1))
            .expect("exact edit");
        let decoded: AnimatedProperty =
            serde_json::from_slice(&serde_json::to_vec(&property).expect("serialize"))
                .expect("decode");
        decoded.validate().expect("validate reopened");
        let restored = decoded.keyframe_by_id(id).expect("same key");
        assert_eq!(restored.time, time);
        assert_eq!(restored.value, PropertyValue::Int(i64::MIN + 1));
        let mut disabled = decoded.clone();
        disabled.disable_animation(TimelineTime::ZERO).expect("disable");
        disabled
            .set_static_value(PropertyValue::Int(0))
            .expect("different static value");
        assert_eq!(disabled.evaluate(TimelineTime::ZERO), PropertyValue::Int(0));
        assert_eq!(
            disabled.keyframe_by_id(id).expect("disabled author key").value,
            PropertyValue::Int(i64::MIN + 1)
        );
        let mut legacy = serde_json::to_value(&decoded).expect("wire value");
        legacy["channels"][0]["keyframes"][0]["value"] = serde_json::json!(1.0);
        assert!(
            serde_json::from_value::<AnimatedProperty>(legacy).is_err(),
            "alpha admits no obsolete channel value parser"
        );
        let prepared =
            PreparedPropertyQuery::prepare(&decoded, crate::TimeScale::ONE).expect("query");
        assert_eq!(
            prepared
                .evaluate(AutomationQueryTime::new(0.5).expect("query time"))
                .expect("value"),
            PropertyValue::Int(i64::MIN + 1)
        );
    }

    #[test]
    fn canonical_channel_key_moves_retain_float_precision_and_reject_collisions_atomically() {
        let numeric =
            ParameterNumericContract::closed(0.1, 0.2, None, ParameterInvalidValuePolicy::Reject)
                .expect("bounds");
        let mut property = AnimatedProperty::from_descriptor(
            PropertyDescriptor::new("test.raw.float", "Float", PropertyValue::Float(0.15))
                .with_animatable(true)
                .with_numeric_contract(ParameterUnit::Unitless, numeric),
        );
        let mut key = Keyframe::linear(tt(1), AnimationChannelValue::Number(0.2));
        property.set_channel_keyframe(0, key.clone()).expect("exact channel insert");
        key.time = tt(2);
        property.set_channel_keyframe(0, key.clone()).expect("move exact key");
        assert_eq!(
            property.channel(0).expect("channel").keyframes(),
            std::slice::from_ref(&key)
        );
        assert_eq!(
            property.keyframe_by_id(key.id).expect("typed key").value,
            PropertyValue::Float(0.2_f32.next_down())
        );
        let encoded = serde_json::to_vec(&property).expect("encode");
        let reopened: AnimatedProperty = serde_json::from_slice(&encoded).expect("decode");
        reopened.validate().expect("validate");
        assert_eq!(reopened, property);
        let occupied = Keyframe::linear(tt(3), AnimationChannelValue::Number(0.15));
        property.set_channel_keyframe(0, occupied).expect("neighbor");
        let before = property.clone();
        key.time = tt(3);
        assert!(property.set_channel_keyframe(0, key.clone()).is_err());
        assert_eq!(property, before, "collision is atomic");
        key.time = tt(2);
        key.value = AnimationChannelValue::Integer(1);
        assert!(property.set_channel_keyframe(0, key.clone()).is_err());
        assert_eq!(property, before, "wrong scalar kind is atomic");
        key.value = AnimationChannelValue::Number(0.2);
        key.interp_out = KeyframeInterpolation::Bezier(ExactBezierHandle {
            time_offset: TimelineTime::ZERO,
            value_offset: f64::NAN,
        });
        assert!(property.set_channel_keyframe(0, key).is_err());
        assert_eq!(property, before, "invalid control is atomic");
        assert!(property
            .set_channel_keyframe(
                1,
                Keyframe::linear(tt(4), AnimationChannelValue::Number(0.15))
            )
            .is_err());
        assert_eq!(property, before, "invalid channel is atomic");
    }

    #[test]
    fn scalar_offsets_preserve_integer_precision_and_representable_bounds() {
        let all = ParameterNumericContract::closed(
            i64::MIN as f64,
            i64::MAX as f64,
            Some(1.0),
            ParameterInvalidValuePolicy::Reject,
        )
        .expect("range");
        for (value, delta, expected) in [
            (i64::MAX, -1.0, i64::MAX - 1),
            (i64::MIN, 1.0, i64::MIN + 1),
            (9_007_199_254_740_993, 1.0, 9_007_199_254_740_994),
            (i64::MAX - 1, 5.0, i64::MAX),
            (i64::MIN + 1, -5.0, i64::MIN),
            (i64::MAX, -f64::MAX, i64::MIN),
            (i64::MIN, f64::MAX, i64::MAX),
        ] {
            assert_eq!(
                all.offset_scalar(PropertyValue::Int(value), delta, 1),
                Some(PropertyValue::Int(expected))
            );
        }
        let fractional =
            ParameterNumericContract::closed(-1.5, 1.5, None, ParameterInvalidValuePolicy::Reject)
                .expect("fractional bounds");
        assert_eq!(
            fractional.offset_scalar(PropertyValue::Int(0), 5.0, 1),
            Some(PropertyValue::Int(1))
        );
        let empty =
            ParameterNumericContract::closed(0.1, 0.9, None, ParameterInvalidValuePolicy::Reject)
                .expect("empty integer domain");
        assert!(empty.offset_scalar(PropertyValue::Int(0), 1.0, 1).is_none());
        assert!(all.offset_scalar(PropertyValue::Int(0), f64::NAN, 1).is_none());
        let wide = ParameterNumericContract::closed(
            -f64::MAX,
            f64::MAX,
            None,
            ParameterInvalidValuePolicy::Reject,
        )
        .expect("wide");
        assert_eq!(
            wide.offset_scalar(PropertyValue::Float(f32::MAX), f64::MAX, 1),
            Some(PropertyValue::Float(f32::MAX))
        );
        assert_eq!(
            wide.offset_scalar(PropertyValue::Double(f64::MAX), f64::MAX, 1),
            Some(PropertyValue::Double(f64::MAX))
        );
        let inside =
            ParameterNumericContract::closed(0.1, 0.2, None, ParameterInvalidValuePolicy::Reject)
                .expect("float bounds");
        let PropertyValue::Float(low) =
            inside.offset_scalar(PropertyValue::Float(0.15), -5.0, 1).expect("low")
        else {
            panic!("Float");
        };
        let PropertyValue::Float(high) =
            inside.offset_scalar(PropertyValue::Float(0.15), 5.0, 1).expect("high")
        else {
            panic!("Float");
        };
        assert!(f64::from(low) >= 0.1 && f64::from(high) <= 0.2);
        assert_eq!(
            wide.offset_scalar(PropertyValue::Double(-f64::MAX), f64::MAX, 5),
            Some(PropertyValue::Double(f64::MAX))
        );
        let large = f64::from_bits(0x7fe0_0000_0000_0000);
        assert_eq!(
            wide.offset_scalar(PropertyValue::Double(-large), large, 2),
            Some(PropertyValue::Double(large)),
            "finite result after oversized intermediate product"
        );
        assert_eq!(
            all.offset_scalar(PropertyValue::Int(i64::MAX), 2.0, -5),
            Some(PropertyValue::Int(i64::MAX - 10))
        );
        let PropertyValue::Double(zero) =
            wide.offset_scalar(PropertyValue::Double(-0.0), 0.0, 1).expect("zero")
        else {
            panic!("Double");
        };
        assert_eq!(zero.to_bits(), (-0.0f64).to_bits());
    }

    #[test]
    fn ordered_visual_controls_preserve_exact_neighbors_defaults_and_hold_edges() {
        let path = "test.ordered.controls";
        let mut bag = overshoot_property_bag(path, 4.0);
        for (time, value) in [
            (TimelineTime::ZERO, 0.0),
            (TimelineTime::ONE, 0.5),
            (TimelineTime::new(3, 1).expect("last"), 1.0),
        ] {
            bag.set_keyframe(
                path,
                Keyframe::from_preset(
                    time,
                    PropertyValue::Double(value),
                    InterpolationType::Bezier,
                ),
            )
            .expect("key");
        }
        let mut property = bag.property(path).expect("property").clone();
        property.validate().expect("valid controls");
        let controls = property.channel_keyframe_controls(0).collect::<Vec<_>>();
        assert_eq!(controls.len(), 4);
        assert_eq!(
            controls.iter().map(|c| (c.key_time, c.side)).collect::<Vec<_>>(),
            vec![
                (TimelineTime::ZERO, crate::BezierHandleSide::Outgoing),
                (TimelineTime::ONE, crate::BezierHandleSide::Incoming),
                (TimelineTime::ONE, crate::BezierHandleSide::Outgoing),
                (
                    TimelineTime::new(3, 1).expect("last"),
                    crate::BezierHandleSide::Incoming
                ),
            ]
        );
        assert_eq!(
            controls.iter().map(|c| c.handle.time_offset).collect::<Vec<_>>(),
            vec![
                TimelineTime::ONE_THIRD,
                TimelineTime::NEGATIVE_ONE_THIRD,
                TimelineTime::new(2, 3).expect("out"),
                TimelineTime::new(-2, 3).expect("in"),
            ]
        );
        assert!(controls.iter().all(|c| c.handle.value_offset == 0.0));
        assert_eq!(
            controls[2].neighbor_time,
            TimelineTime::new(3, 1).expect("neighbor")
        );
        assert_eq!(controls[2].key_value, 0.5);
        assert_eq!(
            controls[2].keyframe_id,
            property.channels()[0].keyframes()[1].id
        );
        assert_eq!(property.channel_keyframe_controls(1).count(), 0);
        property.channels[0].keyframes[1].interp_in = KeyframeInterpolation::Hold;
        property.validate().expect("mixed hold edge");
        let controls = property.channel_keyframe_controls(0).collect::<Vec<_>>();
        assert_eq!(controls.len(), 2);
        assert_eq!(controls[0].key_time, TimelineTime::ONE);
        assert_eq!(controls[0].side, crate::BezierHandleSide::Outgoing);
    }

    fn overshoot_property_bag(path: &str, bound: f64) -> PropertyBag {
        let mut bag = PropertyBag::default();
        bag.define(
            PropertyDescriptor::new(path, "Overshoot", PropertyValue::Double(0.0))
                .with_numeric_contract(
                    ParameterUnit::Unitless,
                    ParameterNumericContract::closed(
                        -bound,
                        bound,
                        None,
                        ParameterInvalidValuePolicy::Reject,
                    )
                    .expect("wide legal range"),
                ),
        );
        bag
    }

    fn visual_handle_admission_fixture(path: &str) -> PropertyBag {
        let mut bag = overshoot_property_bag(path, 4.0);
        for (time, value) in [(TimelineTime::ZERO, 0.0), (TimelineTime::ONE, 1.0)] {
            bag.set_keyframe(path, Keyframe::linear(time, PropertyValue::Double(value)))
                .expect("valid author keys");
        }
        bag
    }

    fn invalid_visual_handles() -> Vec<(bool, ExactBezierHandle)> {
        let mut cases = Vec::new();
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for incoming in [false, true] {
                cases.push((
                    incoming,
                    ExactBezierHandle {
                        time_offset: if incoming {
                            TimelineTime::NEGATIVE_ONE_THIRD
                        } else {
                            TimelineTime::ONE_THIRD
                        },
                        value_offset: value,
                    },
                ));
            }
        }
        for (incoming, time_offset) in [
            (false, TimelineTime::NEGATIVE_ONE_THIRD),
            (false, TimelineTime::new(2, 1).expect("two")),
            (true, TimelineTime::ONE_THIRD),
            (true, TimelineTime::new(-2, 1).expect("negative two")),
        ] {
            cases.push((
                incoming,
                ExactBezierHandle { time_offset, value_offset: 0.25 },
            ));
        }
        cases
    }

    #[test]
    fn visual_handle_admission_rejects_invalid_keyframe_insert_atomically() {
        let path = "test.visual.handle.admission";
        for (incoming, handle) in invalid_visual_handles() {
            let mut bag = visual_handle_admission_fixture(path);
            let before = bag.clone();
            let mut key = Keyframe::linear(
                TimelineTime::new(1, 2).expect("half"),
                PropertyValue::Double(0.5),
            );
            if incoming {
                key.interp_in = KeyframeInterpolation::Bezier(handle);
            } else {
                key.interp_out = KeyframeInterpolation::Bezier(handle);
            }
            assert!(
                bag.apply_mutation(PropertyMutation::SetKeyframe {
                    path: path.into(),
                    keyframe: key
                })
                .is_err(),
                "invalid handle must reject before insertion: {handle:?}"
            );
            assert_eq!(bag, before);
        }
    }

    #[test]
    fn visual_handle_admission_rejects_invalid_pair_edit_atomically() {
        let path = "test.visual.handle.admission";
        for (incoming, handle) in invalid_visual_handles() {
            let mut bag = visual_handle_admission_fixture(path);
            let before = bag.clone();
            let invalid = KeyframeInterpolation::Bezier(handle);
            let (interp_in, interp_out) = if incoming {
                (invalid, KeyframeInterpolation::Linear)
            } else {
                (KeyframeInterpolation::Linear, invalid)
            };
            assert!(
                bag.update_channel_keyframe_handles(
                    path,
                    if incoming {
                        TimelineTime::ONE
                    } else {
                        TimelineTime::ZERO
                    },
                    0,
                    interp_in,
                    interp_out
                )
                .is_err(),
                "invalid edit must reject atomically: {handle:?}"
            );
            assert_eq!(bag, before);
        }
    }

    #[test]
    fn visual_handle_admission_retains_legal_vertical_and_segment_edge_controls() {
        let path = "test.visual.handle.admission";
        for (time_in, time_out) in [
            (TimelineTime::ZERO, TimelineTime::ZERO),
            (TimelineTime::NEGATIVE_ONE, TimelineTime::ONE),
            (TimelineTime::NEGATIVE_ONE_THIRD, TimelineTime::ONE_THIRD),
        ] {
            let mut bag = visual_handle_admission_fixture(path);
            let interp_in = KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: time_in,
                value_offset: -0.25,
            });
            let interp_out = KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: time_out,
                value_offset: 0.25,
            });
            bag.update_channel_keyframe_handles(path, TimelineTime::ZERO, 0, interp_in, interp_out)
                .expect("legal boundary controls");
            let key = bag
                .property(path)
                .expect("property")
                .keyframe_at(TimelineTime::ZERO)
                .expect("key");
            assert_eq!((key.interp_in, key.interp_out), (interp_in, interp_out));
            let restored: PropertyBag =
                serde_json::from_slice(&serde_json::to_vec(&bag).expect("serialize"))
                    .expect("decode");
            restored.validate().expect("legal persisted controls");
            assert_eq!(restored, bag);
            assert_eq!(
                restored.evaluate(path, TimelineTime::new(1, 2).expect("half")),
                bag.evaluate(path, TimelineTime::new(1, 2).expect("half"))
            );
        }
    }

    #[test]
    fn visual_handle_admission_candidate_failure_keeps_original_property() {
        let path = "test.visual.handle.admission";
        let mut bag = visual_handle_admission_fixture(path);
        let before = bag.clone();
        let mut key = bag
            .property(path)
            .expect("property")
            .keyframe_at(TimelineTime::ZERO)
            .expect("key");
        key.time = TimelineTime::new(1, 2).expect("half");
        key.value = PropertyValue::Double(0.5);
        assert!(
            bag.set_keyframe(path, key).is_err(),
            "a new time cannot reuse an existing identity"
        );
        assert_eq!(bag, before);
        bag.validate().expect("original state stays executable");
    }

    #[test]
    fn visual_handle_admission_rejects_invalid_persisted_coordinates() {
        let path = "test.visual.handle.admission";
        for (incoming, handle) in invalid_visual_handles() {
            let mut bag = visual_handle_admission_fixture(path);
            let key = &mut bag.properties.get_mut(path).expect("property").channels[0].keyframes
                [usize::from(incoming)];
            if incoming {
                key.interp_in = KeyframeInterpolation::Bezier(handle);
            } else {
                key.interp_out = KeyframeInterpolation::Bezier(handle);
            }
            assert!(
                bag.validate().is_err(),
                "invalid persisted control cannot become executable: {handle:?}"
            );
            if handle.value_offset.is_finite() {
                let restored: PropertyBag = serde_json::from_slice(
                    &serde_json::to_vec(&bag).expect("finite serialized handle"),
                )
                .expect("raw document decode");
                assert!(restored.validate().is_err());
            }
        }
    }

    #[test]
    fn visual_world_bezier_equal_endpoints_retain_absolute_value_excursion() {
        let path = "test.visual.world_equal_endpoints";
        let mut bag = overshoot_property_bag(path, 10.0);
        let mut left = Keyframe::linear(TimelineTime::ZERO, PropertyValue::Double(2.0));
        let mut right = Keyframe::linear(TimelineTime::ONE, PropertyValue::Double(2.0));
        left.interp_out = KeyframeInterpolation::Bezier(ExactBezierHandle {
            time_offset: TimelineTime::ONE_THIRD,
            value_offset: 4.0,
        });
        right.interp_in = KeyframeInterpolation::Bezier(ExactBezierHandle {
            time_offset: TimelineTime::NEGATIVE_ONE_THIRD,
            value_offset: 4.0,
        });
        for keyframe in [left, right] {
            bag.set_keyframe(path, keyframe).expect("legal world-space controls");
        }
        // Independent Bernstein reference: (2 + 3*6 + 3*6 + 2) / 8.
        assert_eq!(
            bag.evaluate(path, TimelineTime::new(1, 2).expect("midpoint")),
            Some(PropertyValue::Double(5.0)),
            "equal endpoint values must not erase authored handle value offsets"
        );
        let restored: PropertyBag =
            serde_json::from_slice(&serde_json::to_vec(&bag).expect("serialize")).expect("reopen");
        assert_eq!(restored, bag);
        assert_eq!(
            restored.evaluate(path, TimelineTime::new(1, 2).expect("midpoint")),
            Some(PropertyValue::Double(5.0))
        );
    }

    #[test]
    fn visual_world_bezier_handles_use_exact_owner_time_and_parameter_units() {
        let path = "test.visual.world_control_units";
        let mut bag = overshoot_property_bag(path, 10.0);
        let mut left = Keyframe::linear(
            TimelineTime::new(2, 1).expect("left"),
            PropertyValue::Double(1.0),
        );
        let mut right = Keyframe::linear(
            TimelineTime::new(8, 1).expect("right"),
            PropertyValue::Double(3.0),
        );
        left.interp_out = KeyframeInterpolation::Bezier(ExactBezierHandle {
            time_offset: TimelineTime::new(2, 1).expect("two owner seconds"),
            value_offset: 2.0,
        });
        right.interp_in = KeyframeInterpolation::Bezier(ExactBezierHandle {
            time_offset: TimelineTime::new(-2, 1).expect("negative two owner seconds"),
            value_offset: -0.5,
        });
        for keyframe in [left, right] {
            bag.set_keyframe(path, keyframe).expect("handles within the six-second segment");
        }
        // Control times are 2,4,6,8, so the time midpoint is the cubic midpoint.
        // Independent value reference: (1 + 3*3 + 3*2.5 + 3) / 8.
        assert_eq!(
            bag.evaluate(path, TimelineTime::new(5, 1).expect("owner midpoint")),
            Some(PropertyValue::Double(2.5625))
        );
        bag.validate().expect("exact author controls are valid");
    }

    #[test]
    fn visual_world_bezier_vertical_continuous_edit_preserves_partner_value_length() {
        let path = "test.visual.world.vertical";
        let mut bag = overshoot_property_bag(path, 10.0);
        let middle = Keyframe::from_preset(
            TimelineTime::ONE,
            PropertyValue::Double(2.0),
            InterpolationType::ContinuousBezier,
        );
        for key in [
            Keyframe::linear(TimelineTime::ZERO, PropertyValue::Double(2.0)),
            middle,
            Keyframe::linear(
                TimelineTime::new(2, 1).expect("end"),
                PropertyValue::Double(2.0),
            ),
        ] {
            bag.set_keyframe(path, key).expect("key");
        }
        bag.update_channel_keyframe_handles(
            path,
            TimelineTime::ONE,
            0,
            KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: TimelineTime::new(-1, 4).expect("in"),
                value_offset: -2.0,
            }),
            KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: TimelineTime::new(1, 4).expect("out"),
                value_offset: 2.0,
            }),
        )
        .expect("collinear controls");
        let incoming = bag
            .property(path)
            .expect("property")
            .channel(0)
            .expect("channel")
            .keyframe_at(TimelineTime::ONE)
            .expect("middle")
            .interp_in;
        bag.update_channel_keyframe_handles(
            path,
            TimelineTime::ONE,
            0,
            incoming,
            KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: TimelineTime::ZERO,
                value_offset: 3.0,
            }),
        )
        .expect("vertical edit");
        let key = bag
            .property(path)
            .expect("property")
            .channel(0)
            .expect("channel")
            .keyframe_at(TimelineTime::ONE)
            .expect("middle");
        assert_eq!(
            key.interp_in,
            KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: TimelineTime::ZERO,
                value_offset: -2.0,
            })
        );
        assert_eq!(
            key.interp_out,
            KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: TimelineTime::ZERO,
                value_offset: 3.0,
            })
        );
        let restored: PropertyBag =
            serde_json::from_slice(&serde_json::to_vec(&bag).expect("serialize")).expect("reopen");
        restored.validate().expect("constraints");
        assert_eq!(restored, bag);
        for numerator in 0..=40 {
            let time = TimelineTime::new(numerator, 20).expect("sample");
            let value = bag.evaluate(path, time).and_then(|value| value.as_f64()).expect("value");
            assert!(value.is_finite());
            assert_eq!(restored.evaluate(path, time), bag.evaluate(path, time));
        }
    }

    #[test]
    fn visual_world_bezier_explicit_over_span_continuous_edit_rejects_atomically() {
        let path = "test.visual.world.invalid_provided";
        let mut bag = overshoot_property_bag(path, 10.0);
        for (time, value, mode) in [
            (TimelineTime::ZERO, 0.0, InterpolationType::Linear),
            (TimelineTime::ONE, 1.0, InterpolationType::ContinuousBezier),
            (
                TimelineTime::new(2, 1).expect("end"),
                2.0,
                InterpolationType::Linear,
            ),
        ] {
            bag.set_keyframe(
                path,
                Keyframe::from_preset(time, PropertyValue::Double(value), mode),
            )
            .expect("key");
        }
        let original = bag.clone();
        let incoming = bag
            .property(path)
            .expect("property")
            .channel(0)
            .expect("channel")
            .keyframe_at(TimelineTime::ONE)
            .expect("middle")
            .interp_in;
        bag.update_channel_keyframe_handles(
            path,
            TimelineTime::ONE,
            0,
            incoming,
            KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: TimelineTime::new(2, 1).expect("beyond one-second span"),
                value_offset: 2.0,
            }),
        )
        .expect_err("an invalid supplied coordinate must not be healed by constraint projection");
        assert_eq!(bag, original);
    }

    #[test]
    fn visual_world_bezier_key_move_rebounds_retained_controls_without_changing_direction() {
        let path = "test.visual.world.retained_move";
        let mut bag = overshoot_property_bag(path, 10.0);
        let mut middle = Keyframe::linear(TimelineTime::ONE, PropertyValue::Double(1.0));
        let id = middle.id;
        middle.interp_in = KeyframeInterpolation::Bezier(ExactBezierHandle {
            time_offset: TimelineTime::new(-1, 4).expect("in"),
            value_offset: -0.5,
        });
        middle.interp_out = KeyframeInterpolation::Bezier(ExactBezierHandle {
            time_offset: TimelineTime::new(1, 4).expect("out"),
            value_offset: 0.5,
        });
        for key in [
            Keyframe::linear(TimelineTime::ZERO, PropertyValue::Double(0.0)),
            middle,
            Keyframe::linear(
                TimelineTime::new(2, 1).expect("end"),
                PropertyValue::Double(2.0),
            ),
        ] {
            bag.set_keyframe(path, key).expect("key");
        }
        bag.edit_keyframe(
            path,
            id,
            TimelineTime::new(19, 10).expect("near next key"),
            PropertyValue::Double(1.0),
        )
        .expect("moving a key must admit its retained controls in the new neighbor spans");
        let key = bag.property(path).expect("property").keyframe_by_id(id).expect("same identity");
        assert_eq!(key.time, TimelineTime::new(19, 10).expect("target"));
        assert_eq!(
            key.interp_in,
            KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: TimelineTime::new(-1, 4).expect("retained incoming"),
                value_offset: -0.5,
            })
        );
        assert_eq!(
            key.interp_out,
            KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: TimelineTime::new(1, 10).expect("bounded outgoing"),
                value_offset: 0.2,
            })
        );
        bag.validate().expect("admitted new spans");
    }

    #[test]
    fn visual_world_bezier_persisted_constraints_reject_forged_finite_handles() {
        for preset in [
            InterpolationType::AutoBezier,
            InterpolationType::ContinuousBezier,
        ] {
            let path = "test.visual.world.persisted_constraint";
            let mut bag = overshoot_property_bag(path, 10.0);
            for (time, value) in [(0, 0.0), (1, 1.0), (2, 2.0)] {
                bag.set_keyframe(
                    path,
                    Keyframe::from_preset(
                        TimelineTime::new(time, 1).expect("owner time"),
                        PropertyValue::Double(value),
                        preset,
                    ),
                )
                .expect("constrained key");
            }
            bag.validate().expect("canonical constraints");
            bag.properties.get_mut(path).expect("property").channels[0].keyframes[1].interp_out =
                KeyframeInterpolation::Bezier(ExactBezierHandle {
                    time_offset: TimelineTime::new(1, 3).expect("legal time"),
                    value_offset: 0.9,
                });
            let reopened: PropertyBag =
                serde_json::from_str(&serde_json::to_string(&bag).expect("finite wire"))
                    .expect("decode without healing");
            assert!(
                reopened.validate().is_err(),
                "{preset:?} must enforce its stored constraint"
            );
            assert_eq!(
                reopened, bag,
                "validation must not repair persisted author state"
            );
        }
    }

    #[test]
    fn visual_world_bezier_auto_flags_cannot_persist_broken_handles() {
        let path = "test.visual.world.auto_flags";
        let mut bag = overshoot_property_bag(path, 10.0);
        bag.set_keyframe(
            path,
            Keyframe::from_preset(
                TimelineTime::ZERO,
                PropertyValue::Double(1.0),
                InterpolationType::AutoBezier,
            ),
        )
        .expect("auto key");
        bag.update_keyframe_temporal_flags(
            path,
            TimelineTime::ZERO,
            KeyframeTemporalFlags {
                auto_bezier: true,
                continuous: false,
                broken_handles: true,
            },
        )
        .expect("auto derives unbroken constraints");
        assert!(
            !bag.property(path)
                .expect("property")
                .keyframe_at(TimelineTime::ZERO)
                .expect("key")
                .temporal_flags
                .broken_handles
        );
        bag.properties.get_mut(path).expect("property").channels[0].keyframes[0]
            .temporal_flags
            .broken_handles = true;
        assert!(
            bag.validate().is_err(),
            "persisted Auto cannot claim broken manual geometry"
        );
    }

    #[test]
    fn visual_manual_bezier_preserves_legal_overshoot_in_both_directions() {
        let path = "test.visual.overshoot";
        for time_offset in [
            TimelineTime::ONE_THIRD,
            TimelineTime::new(1, 4).expect("quarter"),
        ] {
            for (out_value, in_value, expected) in [(2.0, 1.0, 1.625), (-1.0, -2.0, -0.625)] {
                let mut bag = overshoot_property_bag(path, 4.0);
                let mut left = Keyframe::linear(TimelineTime::ZERO, PropertyValue::Double(0.0));
                let mut right = Keyframe::linear(TimelineTime::ONE, PropertyValue::Double(1.0));
                left.interp_out = KeyframeInterpolation::Bezier(ExactBezierHandle {
                    time_offset,
                    value_offset: out_value,
                });
                right.interp_in = KeyframeInterpolation::Bezier(ExactBezierHandle {
                    time_offset: time_offset
                        .checked_scale(crate::TimeScale::NEGATIVE_ONE)
                        .expect("incoming"),
                    value_offset: in_value,
                });
                for keyframe in [left, right] {
                    bag.apply_mutation(PropertyMutation::SetKeyframe {
                        path: path.into(),
                        keyframe,
                    })
                    .expect("author legal control points");
                }
                let middle = TimelineTime::new(1, 2).expect("middle");
                assert_eq!(
                    bag.evaluate(path, middle),
                    Some(PropertyValue::Double(expected)),
                    "Bezier controls must retain legal overshoot beyond the two endpoint values"
                );
                let restored: PropertyBag = serde_json::from_str(
                    &serde_json::to_string(&bag).expect("serialize authored curve"),
                )
                .expect("restore authored curve");
                assert_eq!(restored.evaluate(path, middle), bag.evaluate(path, middle));
            }
        }
    }

    #[test]
    fn visual_continuous_bezier_preserves_authored_overshoot() {
        let path = "test.visual.continuous_overshoot";
        let mut bag = overshoot_property_bag(path, 4.0);
        for (time, value) in [
            (TimelineTime::NEGATIVE_ONE, -1.0),
            (TimelineTime::ZERO, 0.0),
            (TimelineTime::ONE, 1.0),
        ] {
            bag.apply_mutation(PropertyMutation::SetKeyframe {
                path: path.into(),
                keyframe: Keyframe::linear(time, PropertyValue::Double(value)),
            })
            .expect("author keys");
        }
        bag.apply_mutation(PropertyMutation::UpdateKeyframeInterpolation {
            path: path.into(),
            time: TimelineTime::ZERO,
            interpolation: InterpolationType::ContinuousBezier,
        })
        .expect("continuous constraint");
        let incoming = bag
            .property(path)
            .expect("property")
            .channel(0)
            .expect("channel")
            .keyframe_at(TimelineTime::ZERO)
            .expect("middle key")
            .interp_in;
        bag.apply_mutation(PropertyMutation::UpdateChannelKeyframeHandles {
            path: path.into(),
            time: TimelineTime::ZERO,
            channel_index: 0,
            interp_in: incoming,
            interp_out: KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: TimelineTime::ONE_THIRD,
                value_offset: 2.0,
            }),
        })
        .expect("author continuous tangent");
        let right_out = bag
            .property(path)
            .expect("property")
            .channel(0)
            .expect("channel")
            .keyframe_at(TimelineTime::ONE)
            .expect("right key")
            .interp_out;
        bag.apply_mutation(PropertyMutation::UpdateChannelKeyframeHandles {
            path: path.into(),
            time: TimelineTime::ONE,
            channel_index: 0,
            interp_out: right_out,
            interp_in: KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: TimelineTime::NEGATIVE_ONE_THIRD,
                value_offset: 1.0,
            }),
        })
        .expect("author right control point");
        assert_eq!(
            bag.evaluate(path, TimelineTime::new(1, 2).expect("sample")),
            Some(PropertyValue::Double(1.625)),
            "Continuous constraint must not clip valid overshoot"
        );
    }

    #[test]
    fn full_finite_visual_bezier_overshoot_stays_finite() {
        let path = "test.visual.scaled_overshoot";
        for scale in [f64::MAX * 0.5, 1.0e-300] {
            let mut bag = overshoot_property_bag(path, f64::MAX);
            let mut left = Keyframe::linear(TimelineTime::ZERO, PropertyValue::Double(0.0));
            let mut right = Keyframe::linear(TimelineTime::ONE, PropertyValue::Double(scale));
            left.interp_out = KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: TimelineTime::ONE_THIRD,
                value_offset: 2.0 * scale,
            });
            right.interp_in = KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: TimelineTime::NEGATIVE_ONE_THIRD,
                value_offset: scale,
            });
            for keyframe in [left, right] {
                bag.apply_mutation(PropertyMutation::SetKeyframe { path: path.into(), keyframe })
                    .expect("finite control points within the declared range");
            }
            let evaluated = bag
                .evaluate(path, TimelineTime::new(1, 2).expect("sample"))
                .and_then(|value| value.as_f64())
                .expect("numeric value");
            assert!(evaluated.is_finite());
            assert_eq!(
                evaluated,
                1.625 * scale,
                "Legal overshoot must retain its physical value scale"
            );
        }
    }

    #[test]
    fn numeric_range_mapping_keeps_finite_monotonic_values_and_exact_endpoints() {
        let almost_max = f64::from_bits(f64::MAX.to_bits() - 1);
        for (min, max) in [
            (0.0, 1.0),
            (-f64::MAX, f64::MAX),
            (almost_max, f64::MAX),
            (-f64::MAX, -almost_max),
            (f64::from_bits(1), f64::from_bits(4)),
            (-f64::from_bits(4), f64::from_bits(4)),
            (0.0, f64::MIN_POSITIVE),
            (0.12345678901234568, 0.12345678901234568),
            (-0.0, 0.0),
        ] {
            let range = ParameterNumericRange::new(min, max).expect("canonical range");
            assert_eq!(
                range.value_at_normalized_position(0.0).expect("minimum").to_bits(),
                min.to_bits()
            );
            assert_eq!(
                range.value_at_normalized_position(1.0).expect("maximum").to_bits(),
                max.to_bits()
            );
            let mut previous_value = min;
            let mut previous_position = 0.0;
            for index in 0..=64 {
                let value =
                    range.value_at_normalized_position(f64::from(index) / 64.0).expect("value");
                assert!(value.is_finite() && value >= min && value <= max);
                assert!(value >= previous_value, "range mapping must be monotonic");
                let position = range.normalized_position(value).expect("position");
                assert!(position.is_finite() && (0.0..=1.0).contains(&position));
                assert!(position >= previous_position);
                previous_value = value;
                previous_position = position;
            }
        }
        let full = ParameterNumericRange::new(-f64::MAX, f64::MAX).expect("full range");
        assert_eq!(full.normalized_position(0.0), Some(0.5));
        assert_eq!(full.value_at_normalized_position(0.5), Some(0.0));
        assert_eq!(
            full.value_at_normalized_position(0.25),
            Some(-f64::MAX * 0.5)
        );
        assert_eq!(
            full.value_at_normalized_position(0.75),
            Some(f64::MAX * 0.5)
        );
    }

    #[test]
    fn numeric_range_mapping_rejects_invalid_input_and_clamps_only_projection() {
        let range = ParameterNumericRange::new(1.0, 2.0).expect("range");
        assert_eq!(range.normalized_position(-f64::MAX), Some(0.0));
        assert_eq!(range.normalized_position(f64::MAX), Some(1.0));
        for invalid in [f64::NAN, f64::NEG_INFINITY, f64::INFINITY] {
            assert_eq!(range.normalized_position(invalid), None);
            assert_eq!(range.value_at_normalized_position(invalid), None);
        }
        for invalid in [-0.1, 1.1] {
            assert_eq!(range.value_at_normalized_position(invalid), None);
        }
        for invalid in [
            ParameterNumericRange { min: 2.0, max: 1.0 },
            ParameterNumericRange { min: f64::NAN, max: 1.0 },
            ParameterNumericRange { min: 0.0, max: f64::INFINITY },
        ] {
            assert_eq!(invalid.normalized_position(0.5), None);
            assert_eq!(invalid.value_at_normalized_position(0.5), None);
        }
    }

    fn tt(frame: i64) -> TimelineTime {
        TimelineTime::new(frame, 25).expect("test timeline time")
    }

    #[test]
    fn property_descriptor_try_new_rejects_invalid_plugin_paths_without_panicking() {
        let invalid_characters =
            PropertyDescriptor::try_new("plugin radius!", "Radius", PropertyValue::Float(4.0));
        assert!(invalid_characters.is_err());
        let too_long =
            PropertyDescriptor::try_new("x".repeat(512), "Radius", PropertyValue::Float(4.0));
        assert!(too_long.is_err());
        let valid = PropertyDescriptor::try_new(
            "plugin.sdk.soft_glow.radius",
            "Radius",
            PropertyValue::Float(4.0),
        );
        assert!(valid.is_ok());
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
    fn qualifier_samples_and_schema_defaults_fail_closed_on_invalid_serialized_state() {
        assert!(matches!(
            QualifierSampleSet::new(Vec::new()),
            Err(QualifierSampleSetError::SampleCount { actual: 0 })
        ));
        assert!(matches!(
            QualifierSampleSet::new(vec![QualifierSample::new(
                [0.5, 0.5, 0.5],
                QualifierSampleOperation::Exclude,
            )]),
            Err(QualifierSampleSetError::MissingIncludeSample)
        ));
        assert!(matches!(
            QualifierSampleSet::new(vec![QualifierSample::new(
                [f32::NAN, 0.5, 0.5],
                QualifierSampleOperation::Include,
            )]),
            Err(QualifierSampleSetError::InvalidSample { index: 0 })
        ));

        let invalid_samples: QualifierSampleSet = serde_json::from_value(serde_json::json!([
            {
                "rgb": [0.5, 0.5, 0.5],
                "operation": "exclude"
            }
        ]))
        .expect("structurally deserialize invalid legacy/plugin sample set");
        let schema = ParameterSchema::v1(
            ParameterId::new_static("mondrian.test.qualifier.samples"),
            PropertyValue::QualifierSamples(invalid_samples),
        );
        assert_eq!(
            schema.validate(),
            Err(ParameterSchemaError::InvalidQualifierSamplesDefaultValue)
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
        let path = "test.linear.property";
        let mut bag = overshoot_property_bag(path, 200.0);
        for (time, value) in [(tt(0), 0.0), (tt(100), 100.0)] {
            bag.set_keyframe(path, Keyframe::linear(time, PropertyValue::Double(value)))
                .expect("key");
        }
        let mid = bag.evaluate(path, tt(50)).and_then(|value| value.as_f64()).expect("value");
        assert!((mid - 50.0).abs() < 0.01, "Expected 50.0, got {mid}");
    }

    #[test]
    fn full_finite_linear_visual_parameter_retains_convex_values() {
        for reverse in [false, true] {
            let (left, right) = if reverse {
                (f64::MAX, -f64::MAX)
            } else {
                (-f64::MAX, f64::MAX)
            };
            let path = "test.fullrange.visual";
            let mut bag = PropertyBag::default();
            bag.define(
                PropertyDescriptor::new(path, "Full finite parameter", PropertyValue::Double(0.0))
                    .with_numeric_contract(
                        ParameterUnit::Unitless,
                        ParameterNumericContract::closed(
                            -f64::MAX,
                            f64::MAX,
                            None,
                            ParameterInvalidValuePolicy::Reject,
                        )
                        .expect("legal range"),
                    ),
            );
            for (time, value) in [(TimelineTime::ZERO, left), (TimelineTime::ONE, right)] {
                bag.apply_mutation(PropertyMutation::SetKeyframe {
                    path: path.into(),
                    keyframe: Keyframe::linear(time, PropertyValue::Double(value)),
                })
                .expect("canonical key mutation");
            }
            for (numerator, expected) in [
                (0, left),
                (1, left * 0.5),
                (2, 0.0),
                (3, right * 0.5),
                (4, right),
            ] {
                let time = TimelineTime::new(numerator, 4).expect("exact sample time");
                assert_eq!(
                    bag.evaluate(path, time)
                        .and_then(|value| value.as_f64())
                        .expect("canonical visual evaluation"),
                    expected,
                    "finite convex visual values must not overflow during evaluation"
                );
            }
        }
    }

    #[test]
    fn visual_bezier_inverts_flat_endpoint_time_tangents() {
        let mut left = Keyframe::linear(TimelineTime::ZERO, PropertyValue::Double(0.0));
        let mut right = Keyframe::linear(TimelineTime::ONE, PropertyValue::Double(1.0));
        left.interp_out = KeyframeInterpolation::Bezier(ExactBezierHandle {
            time_offset: TimelineTime::ZERO,
            value_offset: 1.0 / 3.0,
        });
        right.interp_in = KeyframeInterpolation::Bezier(ExactBezierHandle {
            time_offset: TimelineTime::NEGATIVE_ONE,
            value_offset: -1.0 / 3.0,
        });
        let path = "test.flat.time.inversion";
        let mut bag = overshoot_property_bag(path, 10.0);
        for key in [left, right] {
            bag.set_keyframe(path, key).expect("legal key");
        }
        let evaluated = bag
            .evaluate(
                path,
                TimelineTime::new(1, 1_000_000_000).expect("exact sample"),
            )
            .and_then(|value| value.as_f64())
            .expect("value");
        assert!((evaluated - 0.001).abs() <= 64.0 * f64::EPSILON,
            "The legal x=t^3, y=t curve requires actual inversion even at a flat endpoint: {evaluated}");
    }

    #[test]
    fn visual_tangent_time_delta_retains_exact_sample_span() {
        let origin = TimelineTime::new(100_000, 1).expect("origin");
        let step = TimelineTime::new(1, 48_000).expect("sample span");
        let end = origin.checked_add(step).expect("translated end");
        let actual = interp::time_delta(end, origin);
        assert!((actual / step.to_f64() - 1.0).abs() <= 8.0 * f64::EPSILON,
            "Tangent spans must subtract exact author coordinates before floating projection: {actual}");
    }

    #[test]
    fn double_visual_curve_retains_exact_rational_progress() {
        let path = "test.precise.visual";
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            path,
            "Precise parameter",
            PropertyValue::Double(0.0),
        ));
        for (time, value) in [(TimelineTime::ZERO, 0.0), (TimelineTime::ONE, 1.0)] {
            bag.apply_mutation(PropertyMutation::SetKeyframe {
                path: path.into(),
                keyframe: Keyframe::linear(time, PropertyValue::Double(value)),
            })
            .expect("canonical key mutation");
        }
        let evaluated = bag
            .evaluate(path, TimelineTime::ONE_THIRD)
            .and_then(|value| value.as_f64())
            .expect("canonical evaluation");
        assert!(
            (evaluated - 1.0 / 3.0).abs() <= 64.0 * f64::EPSILON,
            "Double author values must not narrow their interpolation progress: {evaluated}"
        );
    }

    #[test]
    fn visual_curve_evaluation_is_invariant_under_author_time_translation() {
        for interpolation in [InterpolationType::Linear, InterpolationType::AutoBezier] {
            for origin in [
                TimelineTime::ZERO,
                TimelineTime::new(100_000, 1).expect("origin"),
            ] {
                let path = "test.translated.visual";
                let step = TimelineTime::new(1, 48_000).expect("sample duration");
                let middle = origin.checked_add(step).expect("middle key");
                let end = middle.checked_add(step).expect("last key");
                let mut bag = PropertyBag::default();
                bag.define(PropertyDescriptor::new(
                    path,
                    "Sample-accurate parameter",
                    PropertyValue::Double(0.0),
                ));
                for (time, value) in [(origin, 0.0), (middle, 0.5), (end, 1.0)] {
                    bag.apply_mutation(PropertyMutation::SetKeyframe {
                        path: path.into(),
                        keyframe: Keyframe::linear(time, PropertyValue::Double(value)),
                    })
                    .expect("canonical key mutation");
                }
                for time in [origin, middle, end] {
                    bag.apply_mutation(PropertyMutation::UpdateKeyframeInterpolation {
                        path: path.into(),
                        time,
                        interpolation,
                    })
                    .expect("interpolation");
                }
                for (offset, expected) in [
                    (TimelineTime::new(1, 96_000).expect("first sample"), 0.25),
                    (TimelineTime::new(1, 32_000).expect("second sample"), 0.75),
                ] {
                    let time = origin.checked_add(offset).expect("translated evaluation time");
                    let evaluated = bag
                        .evaluate(path, time)
                        .and_then(|value| value.as_f64())
                        .expect("canonical evaluation");
                    assert!((evaluated - expected).abs() <= 64.0 * f64::EPSILON,
                        "Exact local progress must survive time translation: {interpolation:?} at {origin:?}, expected {expected}, got {evaluated}");
                }
            }
        }
    }

    #[test]
    fn full_finite_auto_visual_parameter_retains_convex_values() {
        let path = "test.fullrange.auto_visual";
        let mut bag = PropertyBag::default();
        bag.define(
            PropertyDescriptor::new(path, "Full finite parameter", PropertyValue::Double(0.0))
                .with_numeric_contract(
                    ParameterUnit::Unitless,
                    ParameterNumericContract::closed(
                        -f64::MAX,
                        f64::MAX,
                        None,
                        ParameterInvalidValuePolicy::Reject,
                    )
                    .expect("legal range"),
                ),
        );
        for (time, value) in [
            (TimelineTime::ZERO, -f64::MAX),
            (TimelineTime::ONE, f64::MAX),
        ] {
            bag.apply_mutation(PropertyMutation::SetKeyframe {
                path: path.into(),
                keyframe: Keyframe::linear(time, PropertyValue::Double(value)),
            })
            .expect("canonical key mutation");
        }
        for time in [TimelineTime::ZERO, TimelineTime::ONE] {
            bag.apply_mutation(PropertyMutation::UpdateKeyframeInterpolation {
                path: path.into(),
                time,
                interpolation: InterpolationType::AutoBezier,
            })
            .expect("legal finite values must admit auto Bezier");
        }
        for numerator in 0..=16 {
            let time = TimelineTime::new(numerator, 16).expect("exact sample time");
            let evaluated = bag
                .evaluate(path, time)
                .and_then(|value| value.as_f64())
                .expect("canonical visual evaluation");
            assert!(evaluated.is_finite());
            let expected = -1.0 + numerator as f64 / 8.0;
            assert!((evaluated / f64::MAX - expected).abs() <= 8.0 * f64::from(f32::EPSILON), "Auto Bezier must retain the two-key straight line within normalized UI solver precision");
        }
    }

    #[test]
    fn tiny_auto_visual_parameter_preserves_scale_invariance() {
        let path = "test.tiny.auto_visual";
        let scale = 1.0e-20;
        let middle = TimelineTime::new(1, 2).expect("middle key time");
        let mut bag = PropertyBag::default();
        bag.define(
            PropertyDescriptor::new(path, "Small finite parameter", PropertyValue::Double(0.0))
                .with_numeric_contract(
                    ParameterUnit::Unitless,
                    ParameterNumericContract::closed(
                        0.0,
                        scale,
                        None,
                        ParameterInvalidValuePolicy::Reject,
                    )
                    .expect("legal range"),
                ),
        );
        for (time, value) in [
            (TimelineTime::ZERO, 0.0),
            (middle, scale * 0.5),
            (TimelineTime::ONE, scale),
        ] {
            bag.apply_mutation(PropertyMutation::SetKeyframe {
                path: path.into(),
                keyframe: Keyframe::linear(time, PropertyValue::Double(value)),
            })
            .expect("canonical key mutation");
        }
        for time in [TimelineTime::ZERO, middle, TimelineTime::ONE] {
            bag.apply_mutation(PropertyMutation::UpdateKeyframeInterpolation {
                path: path.into(),
                time,
                interpolation: InterpolationType::AutoBezier,
            })
            .expect("automatic interpolation");
        }
        for numerator in 0..=16 {
            let time = TimelineTime::new(numerator, 16).expect("exact sample time");
            let evaluated = bag
                .evaluate(path, time)
                .and_then(|value| value.as_f64())
                .expect("canonical visual evaluation");
            let expected = numerator as f64 / 16.0;
            assert!((evaluated / scale - expected).abs() <= 8.0 * f64::from(f32::EPSILON), "changing physical value units must preserve the auto Bezier shape: expected {expected}, got {}", evaluated / scale);
        }
    }

    #[test]
    fn large_nonuniform_auto_visual_parameter_retains_endpoint_slope() {
        let path = "test.large.nonuniform_visual";
        let mut bag = PropertyBag::default();
        bag.define(
            PropertyDescriptor::new(
                path,
                "Nonuniform finite parameter",
                PropertyValue::Double(0.0),
            )
            .with_numeric_contract(
                ParameterUnit::Unitless,
                ParameterNumericContract::closed(
                    -f64::MAX,
                    f64::MAX,
                    None,
                    ParameterInvalidValuePolicy::Reject,
                )
                .expect("legal range"),
            ),
        );
        for (time, value) in [
            (TimelineTime::ZERO, -f64::MAX),
            (
                TimelineTime::new(2, 1).expect("second key time"),
                -f64::MAX * 0.1,
            ),
            (
                TimelineTime::new(3, 1).expect("third key time"),
                f64::MAX * 0.8,
            ),
        ] {
            bag.apply_mutation(PropertyMutation::SetKeyframe {
                path: path.into(),
                keyframe: Keyframe::linear(time, PropertyValue::Double(value)),
            })
            .expect("canonical key mutation");
        }
        bag.apply_mutation(PropertyMutation::UpdateKeyframeInterpolation {
            path: path.into(),
            time: TimelineTime::ZERO,
            interpolation: InterpolationType::AutoBezier,
        })
        .expect("automatic interpolation");
        let first = bag
            .property(path)
            .and_then(|property| property.channel(0))
            .and_then(|channel| channel.keyframe_at(TimelineTime::ZERO))
            .expect("first canonical key");
        let KeyframeInterpolation::Bezier(handle) = first.interp_out else {
            panic!("Bezier control");
        };
        assert!((handle.value_offset / (0.9 * f64::MAX) - 1.0 / 9.0).abs() <= 8.0 * f64::EPSILON, "weighted intermediate overflow must not silently flatten a nonzero endpoint tangent: {}", handle.value_offset);
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
    fn property_bag_extracts_half_open_range_atomically_across_key_identities() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "track.opacity",
            "Opacity",
            PropertyValue::Float(1.0),
        ));
        let keys = [0_i64, 10, 20, 30]
            .into_iter()
            .map(|frame| Keyframe::linear(tt(frame), PropertyValue::Float(frame as f32 / 30.0)))
            .collect::<Vec<_>>();
        let first_id = keys[0].id;
        let removed_id = keys[1].id;
        let end_id = keys[2].id;
        let after_id = keys[3].id;
        for keyframe in keys {
            bag.apply_mutation(PropertyMutation::SetKeyframe {
                path: "track.opacity".to_owned(),
                keyframe,
            })
            .expect("seed key");
        }

        bag.extract_time_range(TimelineTimeRange::new(tt(10), tt(10)).expect("extract range"))
            .expect("extract");

        let property = bag.property("track.opacity").expect("property");
        assert_eq!(property.keyframe_time_by_id(first_id), Some(tt(0)));
        assert!(property.keyframe_time_by_id(removed_id).is_none());
        assert_eq!(property.keyframe_time_by_id(end_id), Some(tt(10)));
        assert_eq!(property.keyframe_time_by_id(after_id), Some(tt(20)));
        assert_eq!(property.keyframe_times(), [tt(0), tt(10), tt(20)]);
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
            interp_in: KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: ht(-0.2),
                value_offset: -0.1,
            }),
            interp_out: KeyframeInterpolation::Bezier(ExactBezierHandle {
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
    fn stable_address_value_write_preserves_existing_bezier_key_metadata() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "effect.amount",
            "Amount",
            PropertyValue::Float(0.0),
        ));
        let key = Keyframe {
            id: KeyframeId::new(),
            time: tt(10),
            value: PropertyValue::Float(0.5),
            interp_in: KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: ht(-0.2),
                value_offset: -0.1,
            }),
            interp_out: KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: ht(0.3),
                value_offset: 0.2,
            }),
            temporal_flags: KeyframeTemporalFlags {
                auto_bezier: false,
                continuous: false,
                broken_handles: true,
            },
        };
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "effect.amount".to_owned(),
            keyframe: key.clone(),
        })
        .expect("seed Bezier key");
        let address = bag.address_for_path("effect.amount").expect("stable address");

        let mutation = bag
            .prepare_value_write_by_address(&address, tt(10), PropertyValue::Float(0.75))
            .expect("prepare value write")
            .expect("changed value");
        assert!(matches!(
            &mutation,
            PropertyMutation::EditKeyframe { keyframe_id, time, .. }
                if *keyframe_id == key.id && *time == key.time
        ));
        bag.apply_mutation(mutation).expect("apply value write");

        let edited = bag
            .property("effect.amount")
            .and_then(|property| property.keyframe_by_id(key.id))
            .expect("same key remains");
        assert_eq!(edited.value, PropertyValue::Float(0.75));
        assert_eq!(edited.interp_in, key.interp_in);
        assert_eq!(edited.interp_out, key.interp_out);
        assert_eq!(edited.temporal_flags, key.temporal_flags);
    }

    #[test]
    fn enabled_empty_curve_value_write_creates_key_instead_of_hidden_static_value() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "effect.amount",
            "Amount",
            PropertyValue::Float(0.0),
        ));
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "effect.amount".to_owned(),
            keyframe: Keyframe::linear(tt(0), PropertyValue::Float(0.25)),
        })
        .expect("enable curve");
        bag.apply_mutation(PropertyMutation::RemoveKeyframe {
            path: "effect.amount".to_owned(),
            time: tt(0),
        })
        .expect("remove last key");
        let address = bag.address_for_path("effect.amount").expect("stable address");

        let mutation = bag
            .prepare_value_write_by_address(&address, tt(12), PropertyValue::Float(0.75))
            .expect("prepare value write")
            .expect("changed value");
        assert!(matches!(
            mutation,
            PropertyMutation::SetKeyframe { ref keyframe, .. }
                if keyframe.time == tt(12) && keyframe.value == PropertyValue::Float(0.75)
        ));
    }

    #[test]
    fn stable_address_value_write_rejects_stale_identity_and_elides_noop() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "effect.amount",
            "Amount",
            PropertyValue::Float(0.5),
        ));
        let address = bag.address_for_path("effect.amount").expect("stable address");
        assert!(bag
            .prepare_value_write_by_address(&address, tt(0), PropertyValue::Float(0.5))
            .expect("prepare no-op")
            .is_none());

        let stale = AnimationParameterAddress {
            animation_track_id: AnimationTrackId::new(),
            parameter_id: address.parameter_id,
        };
        assert!(bag
            .prepare_value_write_by_address(&stale, tt(0), PropertyValue::Float(0.75))
            .is_err());
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
    fn precise_compound_author_values_keep_all_components_and_atomic_sparse_keys() {
        let precise = 0.12500000000000003;
        let cases = [
            PropertyValue::DoubleVec2([precise; 2]),
            PropertyValue::DoubleVec3([precise; 3]),
            PropertyValue::DoubleRgb([precise; 3]),
            PropertyValue::DoubleColor([precise; 4]),
            PropertyValue::IntVec2([0, i32::MIN]),
            PropertyValue::IntVec3([0, i32::MIN, i32::MAX]),
        ];
        for default in cases {
            let integer = matches!(
                default,
                PropertyValue::IntVec2(_) | PropertyValue::IntVec3(_)
            );
            let initial = default.to_channel_values();
            for animated in [false, true] {
                let mut bag = PropertyBag::default();
                bag.define(
                    PropertyDescriptor::new("precise", "Precise", default.clone())
                        .with_animatable(animated),
                );
                bag.validate().expect("precise schema");
                let address = bag.property("precise").expect("property").address();
                let changed = if integer {
                    123456789.0
                } else {
                    0.25000000000000006
                };
                let mutation = bag
                    .prepare_channel_write_by_address(&address, tt(3), &[(0, changed)])
                    .expect("precise channel write")
                    .expect("changed");
                bag.apply_mutation(mutation).expect("atomic write");
                bag.validate().expect("valid author state");
                let actual = bag.evaluate("precise", tt(3)).expect("evaluate").to_channel_values();
                assert_eq!(actual[0].to_bits(), changed.to_bits());
                assert_eq!(&actual[1..], &initial[1..], "siblings remain exact");
                assert!(bag
                    .prepare_channel_write_by_address(&address, tt(3), &[(0, changed)])
                    .expect("no-op")
                    .is_none());
                let before = bag.clone();
                let invalid = if integer {
                    f64::from(i32::MAX) + 1.0
                } else {
                    f64::NAN
                };
                assert!(bag
                    .prepare_channel_write_by_address(&address, tt(4), &[(0, 0.5), (1, invalid)])
                    .is_err());
                assert_eq!(
                    bag, before,
                    "one invalid component rejects the complete edit"
                );
                let encoded = serde_json::to_vec(&bag).expect("serialize");
                let reopened: PropertyBag = serde_json::from_slice(&encoded).expect("reopen");
                reopened.validate().expect("reopened state");
                assert_eq!(reopened, bag, "types, IDs and exact key components survive");
            }
        }
    }

    #[test]
    fn integer_vector_schemas_reject_fractional_or_unrepresentable_bounds() {
        for range in [
            (0.0, 0.5),
            (f64::from(i32::MIN) - 1.0, 0.0),
            (0.0, f64::from(i32::MAX) + 1.0),
        ] {
            let schema = ParameterSchema::v1(
                ParameterId::new_static("integer.point"),
                PropertyValue::IntVec2([0; 2]),
            )
            .with_numeric_contract(
                ParameterUnit::Unitless,
                ParameterNumericContract::closed(
                    range.0,
                    range.1,
                    Some(1.0),
                    ParameterInvalidValuePolicy::Reject,
                )
                .expect("finite bounds"),
            );
            assert_eq!(
                schema.validate(),
                Err(ParameterSchemaError::IntegerVectorRange)
            );
        }
    }

    #[test]
    fn non_animatable_numeric_components_accept_atomic_sparse_edits() {
        for default in [
            PropertyValue::Float(0.125),
            PropertyValue::Vec2(Vec2::new(0.125, 0.125)),
            PropertyValue::Double(0.125),
        ] {
            let mut bag = PropertyBag::default();
            bag.define(
                PropertyDescriptor::new("static.point", "Point", default)
                    .with_animatable(false)
                    .with_numeric_contract(
                        ParameterUnit::Unitless,
                        ParameterNumericContract::closed(
                            0.0,
                            0.2,
                            None,
                            ParameterInvalidValuePolicy::Reject,
                        )
                        .expect("range"),
                    ),
            );
            bag.validate().expect("static definition is valid");
            let address = bag.property("static.point").expect("property").address();
            let mutation = bag
                .prepare_channel_write_by_address(&address, tt(0), &[(0, 0.2)])
                .expect("static value components do not require animation storage")
                .expect("changed component");
            bag.apply_mutation(mutation).expect("static sparse write");
            bag.validate().expect("accepted static value");
            let property = bag.property("static.point").expect("property");
            assert_eq!(
                property.channel_count(),
                0,
                "no animation channel is allocated"
            );
            assert!(property.static_value().to_channel_values()[0] <= 0.2);
            if property.value_type() == PropertyValueType::Vec2 {
                assert_eq!(
                    property.static_value().to_channel_values()[1],
                    0.125,
                    "sibling is preserved"
                );
            }
            assert!(bag
                .prepare_channel_write_by_address(&address, tt(0), &[(0, 0.2)])
                .expect("unchanged physical value")
                .is_none());
            let before = bag.clone();
            assert!(bag.prepare_channel_write_by_address(&address, tt(0), &[(0, 0.3)]).is_err());
            assert_eq!(bag, before);
        }
    }

    #[test]
    fn float_parameter_projection_obeys_exact_bounds_without_narrowing_double() {
        for kind in [
            PropertyValueType::Float,
            PropertyValueType::Vec2,
            PropertyValueType::Vec3,
            PropertyValueType::Vec4,
            PropertyValueType::Color,
        ] {
            let value = |v| {
                PropertyValue::from_channel_values(
                    kind,
                    &vec![v; kind.channel_count()],
                    &PropertyValue::Float(0.0),
                )
            };
            let descriptor = PropertyDescriptor::new("test.precision", "Precision", value(0.125))
                .with_numeric_contract(
                    ParameterUnit::Unitless,
                    ParameterNumericContract::closed(
                        -0.2,
                        0.2,
                        None,
                        ParameterInvalidValuePolicy::Clamp,
                    )
                    .expect("range"),
                );
            descriptor.validate().expect("representable default");
            for sign in [-1.0, 1.0] {
                let mut property = AnimatedProperty::from_descriptor(descriptor.clone());
                property.set_static_value(value(sign * 0.3)).expect("clamp");
                assert!(
                    property
                        .static_value()
                        .to_channel_values()
                        .iter()
                        .all(|v| (-0.2..=0.2).contains(v)),
                    "{kind:?}: rounded clamp must stay inside exact bounds"
                );
                property.validate().expect("accepted state is valid");
                let reopened: AnimatedProperty =
                    serde_json::from_slice(&serde_json::to_vec(&property).expect("wire"))
                        .expect("reopen");
                reopened.validate().expect("reopened bounds");
                assert_eq!(reopened, property);
                property.enable_animation(tt(0)).expect("enable");
                property
                    .write_channels(tt(1), &[(0, sign * 0.2)], InterpolationType::Linear)
                    .expect("exact channel endpoint");
                assert_eq!(
                    property.channel(0).expect("channel").keyframes[1].value,
                    AnimationChannelValue::Number(sign * 0.2),
                    "author channel precision is retained"
                );
                assert!(
                    property
                        .evaluate(tt(1))
                        .to_channel_values()
                        .iter()
                        .all(|v| (-0.2..=0.2).contains(v)),
                    "physical Float projection stays inside bounds"
                );
                let mut tampered = AnimatedProperty::from_descriptor(descriptor.clone());
                tampered.static_value = value(sign * 0.2);
                assert!(
                    tampered.validate().is_err(),
                    "persisted values must not be repaired by edit clamping"
                );
            }
        }
        let mut unbounded = AnimatedProperty::from_descriptor(PropertyDescriptor::new(
            "test.width",
            "Width",
            PropertyValue::Float(0.125),
        ));
        let before = unbounded.clone();
        assert!(
            unbounded
                .write_channels(tt(0), &[(0, f64::MAX)], InterpolationType::Linear)
                .is_err(),
            "finite channels must not publish infinite Float values"
        );
        assert_eq!(unbounded, before);
        let mut double = AnimatedProperty::from_descriptor(
            PropertyDescriptor::new("test.double", "Double", PropertyValue::Double(0.125))
                .with_numeric_contract(
                    ParameterUnit::Unitless,
                    ParameterNumericContract::closed(
                        0.0,
                        0.2,
                        None,
                        ParameterInvalidValuePolicy::Clamp,
                    )
                    .expect("double range"),
                ),
        );
        double.set_static_value(PropertyValue::Double(0.3)).expect("double clamp");
        assert_eq!(double.static_value(), &PropertyValue::Double(0.2));
        double.validate().expect("exact double bounds");
    }

    #[test]
    fn numeric_channel_contracts_admit_asymmetric_ranges_and_write_atomically() {
        let descriptor = PropertyDescriptor::new(
            "native.point",
            "Point",
            PropertyValue::Vec2(Vec2::new(0.25, 20.0)),
        );
        let contracts = vec![
            ParameterNumericContract::closed(
                0.0,
                1.0,
                Some(0.01),
                ParameterInvalidValuePolicy::Clamp,
            )
            .expect("X range"),
            ParameterNumericContract::closed(
                10.0,
                30.0,
                Some(0.1),
                ParameterInvalidValuePolicy::Reject,
            )
            .expect("Y range"),
        ];
        let mut wire = serde_json::to_value(&descriptor).expect("descriptor wire");
        wire["schema"]["numeric"] = serde_json::to_value(&contracts).expect("component contracts");
        let descriptor: PropertyDescriptor = serde_json::from_value(wire)
            .expect("one complete numeric contract per component must be admitted");
        descriptor.validate().expect("asymmetric default lies inside its own ranges");
        let mut property = AnimatedProperty::from_descriptor(descriptor);
        let original = property.clone();
        assert!(property.set_static_value(PropertyValue::Vec2(Vec2::new(2.0, 31.0))).is_err());
        assert_eq!(
            property, original,
            "a rejected Y cannot publish the clamped X"
        );
        property
            .set_static_value(PropertyValue::Vec2(Vec2::new(2.0, 25.0)))
            .expect("component-specific policies");
        assert_eq!(
            property.static_value(),
            &PropertyValue::Vec2(Vec2::new(1.0, 25.0))
        );
        let first = Keyframe::linear(tt(0), PropertyValue::Vec2(Vec2::new(0.25, 20.0)));
        let first_id = first.id;
        property.set_exact_keyframe(first).expect("complete first key");
        property
            .set_exact_keyframe(Keyframe::linear(
                tt(10),
                PropertyValue::Vec2(Vec2::new(0.75, 30.0)),
            ))
            .expect("complete last key");
        assert_eq!(property.channel(0).expect("X").keyframes[0].id, first_id);
        assert_eq!(property.channel(1).expect("Y").keyframes[0].id, first_id);
        assert_eq!(
            property.evaluate(tt(5)),
            PropertyValue::Vec2(Vec2::new(0.5, 25.0))
        );
        let before_rejection = property.clone();
        assert!(property
            .write_channels(tt(5), &[(0, -1.0), (1, 31.0)], InterpolationType::Linear)
            .is_err());
        assert_eq!(
            property, before_rejection,
            "all channel bounds validate before any key mutation"
        );
        property
            .write_channels(tt(5), &[(0, -1.0), (1, 25.0)], InterpolationType::Linear)
            .expect("X clamps independently, Y remains valid");
        assert_eq!(
            property.evaluate(tt(5)),
            PropertyValue::Vec2(Vec2::new(0.0, 25.0))
        );
        let serialized = serde_json::to_value(&property).expect("complete author wire");
        let restored: AnimatedProperty =
            serde_json::from_value(serialized.clone()).expect("restore");
        restored.validate().expect("persisted channel bounds");
        assert_eq!(restored, property);
        let mut missing = serialized;
        missing["descriptor"]["schema"]["numeric"]
            .as_array_mut()
            .expect("contracts")
            .pop();
        let mut malformed: AnimatedProperty = serde_json::from_value(missing).expect("wire shape");
        assert!(
            malformed.validate().is_err(),
            "a partial contract must not make a sibling unbounded"
        );
        let before = malformed.clone();
        assert!(
            malformed
                .write_channels(tt(5), &[(0, 0.75)], InterpolationType::Linear)
                .is_err(),
            "a sparse write must reject an incomplete contract"
        );
        assert_eq!(malformed, before);
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
            )
            .is_err());

        property.enable_animation(tt(0)).expect("enable animation");
        assert!(property.write_channels(tt(5), &[(0, -0.1)], InterpolationType::Linear).is_err());
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
    fn definition_default_controls_animation_enable_and_integer_value_writes() {
        for preferred in [
            ParameterInterpolation::Hold,
            ParameterInterpolation::Linear,
            ParameterInterpolation::Bezier,
        ] {
            let mut descriptor =
                PropertyDescriptor::new("effect.integer.default", "Integer", PropertyValue::Int(3))
                    .with_numeric_contract(
                        ParameterUnit::Unitless,
                        ParameterNumericContract::closed(
                            0.0,
                            9.0,
                            Some(1.0),
                            ParameterInvalidValuePolicy::Reject,
                        )
                        .expect("range"),
                    );
            descriptor.schema.allowed_interpolations = vec![preferred];
            for mode in [
                ParameterInterpolation::Hold,
                ParameterInterpolation::Linear,
                ParameterInterpolation::Bezier,
            ] {
                if mode != preferred {
                    descriptor.schema.allowed_interpolations.push(mode);
                }
            }
            descriptor.validate().expect("schema");
            let mut property = AnimatedProperty::from_descriptor(descriptor);
            property.enable_animation(tt(0)).expect("enable");
            assert_eq!(
                keyframe_interpolation_type(&property.keyframe_at(tt(0)).expect("initial key")),
                preferred
            );
            property
                .write_value(
                    tt(2),
                    PropertyValue::Int(5),
                    property.default_interpolation().expect("default"),
                )
                .expect("write integer");
            assert_eq!(
                keyframe_interpolation_type(&property.keyframe_at(tt(2)).expect("new key")),
                preferred
            );
            if preferred == ParameterInterpolation::Linear {
                assert_eq!(property.evaluate(tt(1)), PropertyValue::Int(4));
            }
            if preferred == ParameterInterpolation::Hold {
                assert_eq!(property.evaluate(tt(1)), PropertyValue::Int(3));
            }
        }
    }

    #[test]
    fn bounded_integer_curves_round_numeric_samples_and_reject_inexact_domains() {
        let mut descriptor =
            PropertyDescriptor::new("effect.integer", "Integer", PropertyValue::Int(0));
        assert_eq!(
            descriptor.schema.allowed_interpolations,
            [ParameterInterpolation::Hold]
        );
        descriptor.schema.allowed_interpolations = vec![
            ParameterInterpolation::Hold,
            ParameterInterpolation::Linear,
            ParameterInterpolation::Bezier,
        ];
        assert!(
            descriptor.validate().is_err(),
            "integer curves need an exact bounded numeric domain"
        );
        descriptor = descriptor.with_numeric_contract(
            ParameterUnit::Unitless,
            ParameterNumericContract::closed(
                -3.0,
                3.0,
                Some(1.0),
                ParameterInvalidValuePolicy::Reject,
            )
            .expect("range"),
        );
        descriptor.validate().expect("exact integer schema");
        for (start, end, expected) in [(0, 3, 2), (-3, 0, -2)] {
            let mut property = AnimatedProperty::from_descriptor(descriptor.clone());
            property
                .set_exact_keyframe(Keyframe::linear(tt(0), PropertyValue::Int(start)))
                .expect("first key");
            property
                .set_exact_keyframe(Keyframe::linear(tt(2), PropertyValue::Int(end)))
                .expect("second key");
            assert_eq!(property.evaluate(tt(1)), PropertyValue::Int(expected));
        }
        let exact_max = 1_i64 << 53;
        let mut exact = descriptor.clone();
        exact.schema = exact.schema.with_numeric_contract(
            ParameterUnit::Unitless,
            ParameterNumericContract::closed(
                0.0,
                exact_max as f64,
                Some(1.0),
                ParameterInvalidValuePolicy::Reject,
            )
            .expect("exact range"),
        );
        exact.schema.default_value = PropertyValue::Int(exact_max + 1);
        assert!(
            exact.validate().is_err(),
            "do not round an invalid default into the range"
        );
        exact.schema.default_value = PropertyValue::Int(0);
        let mut property = AnimatedProperty::from_descriptor(exact);
        assert!(property.set_static_value(PropertyValue::Int(exact_max + 1)).is_err());
        property
            .set_static_value(PropertyValue::Int(exact_max))
            .expect("exact endpoint");
        descriptor.schema = descriptor.schema.with_numeric_contract(
            ParameterUnit::Unitless,
            ParameterNumericContract::closed(
                0.0,
                ((1_u64 << 53) + 2) as f64,
                Some(1.0),
                ParameterInvalidValuePolicy::Reject,
            )
            .expect("wide range"),
        );
        assert!(
            descriptor.validate().is_err(),
            "inexact integer channels must fail closed"
        );
    }

    #[test]
    fn parameter_interpolation_contract_rejects_unlisted_presets() {
        let mut descriptor =
            PropertyDescriptor::new("effect.step.mode", "Step", PropertyValue::Float(0.0));
        descriptor.schema.allowed_interpolations = vec![ParameterInterpolation::Hold];
        let mut property = AnimatedProperty::from_descriptor(descriptor);
        property.set_animation_enabled(true);

        let error = property
            .write_value(tt(0), PropertyValue::Float(1.0), InterpolationType::Linear)
            .expect_err("unlisted interpolation must be rejected");
        assert!(error.to_string().contains("does not admit Linear"));
    }

    #[test]
    fn enum_choice_captions_are_required_persisted_and_accounted() {
        let choice = ParameterEnumOption::new("vendor:42", "Écho / 回声", "vendor.choice.echo");
        let json = serde_json::to_value(&choice).expect("serialize choice");
        assert_eq!(json["key"], "vendor:42");
        assert_eq!(json["display_name"], "Écho / 回声");
        assert_eq!(
            serde_json::from_value::<ParameterEnumOption>(json.clone()).expect("roundtrip"),
            choice
        );
        let mut missing_caption = json;
        missing_caption.as_object_mut().expect("object").remove("display_name");
        assert!(serde_json::from_value::<ParameterEnumOption>(missing_caption).is_err());
        let mut collector = crate::AuthoringFootprintCollector::new();
        collector.collect(&choice).expect("choice footprint");
        let actual = collector.finish().total_bytes();
        let mut expected = crate::AuthoringFootprintCollector::new();
        expected.collect(&choice.key).expect("key footprint");
        expected.collect(&choice.display_name).expect("caption footprint");
        expected.collect(&choice.message_id).expect("message footprint");
        assert_eq!(actual, expected.finish().total_bytes());

        let descriptor = PropertyDescriptor::new(
            "vendor.mode",
            "Mode",
            PropertyValue::Enum(choice.key.clone()),
        )
        .with_enum_options(vec![choice]);
        descriptor.validate().expect("caption schema");
        for blank in ["", " \n\t"] {
            let mut invalid = descriptor.clone();
            invalid.schema.enum_options[0].display_name = blank.to_owned();
            assert_eq!(
                invalid.schema.validate(),
                Err(ParameterSchemaError::InvalidEnumOption)
            );
        }
        let mut duplicate_caption = descriptor.clone();
        let mut other = duplicate_caption.schema.enum_options[0].clone();
        other.key = "vendor:43".to_owned();
        duplicate_caption.schema.enum_options.push(other);
        duplicate_caption.validate().expect("captions need not be unique");
        let mut property = AnimatedProperty::from_descriptor(duplicate_caption);
        property
            .write_value(
                tt(0),
                PropertyValue::Enum("vendor:43".to_owned()),
                InterpolationType::Hold,
            )
            .expect("canonical choice write");
        assert_eq!(
            property.evaluate(tt(0)),
            PropertyValue::Enum("vendor:43".to_owned())
        );
        assert!(property
            .write_value(
                tt(0),
                PropertyValue::Enum("Écho / 回声".to_owned()),
                InterpolationType::Hold
            )
            .is_err());
    }

    #[test]
    fn enum_visual_ranks_preserve_author_ordinals_and_hold_animation() {
        let descriptor = PropertyDescriptor::new(
            "vendor.mode",
            "Mode",
            PropertyValue::Enum("first".to_owned()),
        )
        .with_enum_options(vec![
            ParameterEnumOption::new("first", "First", "vendor.first").with_display_rank(i32::MAX),
            ParameterEnumOption::new("second", "Second", "vendor.second")
                .with_display_rank(i32::MIN),
        ]);
        descriptor.validate().expect("arbitrary unique signed ranks");
        let encoded = serde_json::to_vec(&descriptor).expect("serialize ranked choices");
        let decoded: PropertyDescriptor = serde_json::from_slice(&encoded).expect("rank roundtrip");
        assert_eq!(decoded, descriptor);
        let mut property = AnimatedProperty::from_descriptor(descriptor.clone());
        property.enable_animation(tt(0)).expect("enable");
        property
            .write_value(
                tt(10),
                PropertyValue::Enum("second".to_owned()),
                InterpolationType::Hold,
            )
            .expect("hold write");
        assert_eq!(
            property.evaluate(tt(9)),
            PropertyValue::Enum("first".to_owned())
        );
        assert_eq!(
            property.evaluate(tt(10)),
            PropertyValue::Enum("second".to_owned())
        );
        assert_eq!(property.descriptor.schema.enum_options[0].key, "first");
        let mut mixed = descriptor.clone();
        mixed.schema.enum_options[0].display_rank = None;
        assert_eq!(
            mixed.validate(),
            Err(ParameterSchemaError::InvalidEnumDisplayOrder)
        );
        let mut repeated = descriptor;
        repeated.schema.enum_options[1].display_rank = Some(i32::MAX);
        assert_eq!(
            repeated.validate(),
            Err(ParameterSchemaError::InvalidEnumDisplayOrder)
        );
    }

    #[test]
    fn enum_parameter_uses_stable_options_for_hold_keyframes() {
        let descriptor = PropertyDescriptor::new(
            "mask.operation",
            "Operation",
            PropertyValue::Enum("Add".to_string()),
        )
        .with_enum_options(vec![
            ParameterEnumOption::new("Add", "Add", "mask.operation.add"),
            ParameterEnumOption::new("Subtract", "Subtract", "mask.operation.subtract"),
        ]);
        descriptor.validate().expect("valid enum descriptor");
        let mut property = AnimatedProperty::from_descriptor(descriptor);
        property.enable_animation(tt(0)).expect("enable enum automation");
        property
            .write_value(
                tt(10),
                PropertyValue::Enum("Subtract".to_string()),
                InterpolationType::Hold,
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
            )
            .is_err());
    }

    #[test]
    fn text_editor_mode_is_explicit_and_survives_empty_values_and_serialization() {
        let id = ParameterId::new_static("mondrian.test.text_mode");
        for text in ["", "plain", "first\nsecond", "Unicode 文本"] {
            for mode in [ParameterTextMode::SingleLine, ParameterTextMode::MultiLine] {
                let schema = ParameterSchema::v1(id.clone(), PropertyValue::Text(text.to_owned()))
                    .with_text_mode(mode);
                schema.validate().expect("valid text metadata");
                let encoded = serde_json::to_string(&schema).expect("encode");
                let decoded: ParameterSchema = serde_json::from_str(&encoded).expect("decode");
                assert_eq!(decoded, schema);
                assert_eq!(decoded.text_mode, Some(mode));
            }
        }
        let mut text = ParameterSchema::v1(id.clone(), PropertyValue::Text(String::new()));
        assert_eq!(text.text_mode, Some(ParameterTextMode::SingleLine));
        text.text_mode = None;
        assert_eq!(
            text.validate(),
            Err(ParameterSchemaError::TextModeTypeMismatch)
        );
        let numeric = ParameterSchema::v1(id, PropertyValue::Double(1.0))
            .with_text_mode(ParameterTextMode::MultiLine);
        assert_eq!(
            numeric.validate(),
            Err(ParameterSchemaError::TextModeTypeMismatch)
        );
    }

    #[test]
    fn resource_path_contract_preserves_selection_policy_without_resolving_author_intent() {
        for kind in [
            ResourcePathKind::File,
            ResourcePathKind::Directory,
            ResourcePathKind::FileOrDirectory,
        ] {
            for selection in [
                ResourcePathSelection::Reference,
                ResourcePathSelection::Existing,
                ResourcePathSelection::ExistingOrNew,
            ] {
                let contract = ParameterResourceContract::external_path(kind, selection);
                contract.validate().expect("external path");
                let json = serde_json::to_vec(&contract).expect("serialize");
                let restored: ParameterResourceContract =
                    serde_json::from_slice(&json).expect("restore");
                assert_eq!(restored, contract);
                assert!(contract.admits(&ParameterResourceReference::ExternalFile {
                    path: PathBuf::from("unavailable/输入🙂.cube").into()
                }));
                let schema = ParameterSchema::v1(
                    ParameterId::new_static("test.external_path"),
                    PropertyValue::Resource(ParameterResourceReference::Unbound),
                )
                .with_resource_contract(contract)
                .with_cache_impact(ParameterCacheImpact::Resource);
                schema.validate().expect("pure author schema");
                assert!(!schema.is_animatable);
            }
        }
        assert!(serde_json::from_str::<ResourcePathContract>(r#"{"kind":"file"}"#).is_err());
        assert!(serde_json::from_str::<ResourcePathContract>(
            r#"{"kind":"file","selection":"existing","unknown":true}"#
        )
        .is_err());
    }

    #[test]
    fn external_utf8_paths_enforce_byte_limits_before_authoring() {
        let contract = ParameterResourceContract::external_utf8_path(
            ResourcePathKind::File,
            ResourcePathSelection::ExistingOrNew,
            6,
        );
        contract.validate().expect("UTF-8 path contract");
        assert!(contract.admits(&ParameterResourceReference::ExternalFile {
            path: PathBuf::from("汉字").into()
        }));
        assert!(!contract.admits(&ParameterResourceReference::ExternalFile {
            path: PathBuf::from("汉字🙂").into()
        }));
        assert!(contract.admits(&ParameterResourceReference::Unbound));
        assert!(ParameterResourceContract::external_utf8_path(
            ResourcePathKind::File,
            ResourcePathSelection::Existing,
            0
        )
        .validate()
        .is_err());
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            let reference = ParameterResourceReference::ExternalFile {
                path: PathBuf::from(std::ffi::OsString::from_wide(&[0xd800])).into(),
            };
            assert!(!contract.admits(&reference));
            assert!(ParameterResourceContract::external_file(Vec::new()).admits(&reference));
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let reference = ParameterResourceReference::ExternalFile {
                path: PathBuf::from(std::ffi::OsString::from_vec(vec![0xff])).into(),
            };
            assert!(!contract.admits(&reference));
            assert!(ParameterResourceContract::external_file(Vec::new()).admits(&reference));
        }
    }

    #[test]
    fn resource_contract_rejects_unsupported_bindings_before_author_mutation() {
        let contract = ParameterResourceContract::external_file(vec!["cube".to_owned()]);
        contract.validate().expect("file contract");
        let descriptor = PropertyDescriptor::new(
            "lut",
            "LUT",
            PropertyValue::Resource(ParameterResourceReference::Unbound),
        )
        .with_resource_contract(contract.clone());
        descriptor.validate().expect("resource schema");
        let mut property = AnimatedProperty::from_descriptor(descriptor);
        let accepted = PropertyValue::Resource(ParameterResourceReference::ExternalFile {
            path: PathBuf::from("missing Unicode 文件.CUBE").into(),
        });
        property.set_static_value(accepted.clone()).expect("offline author intent");
        let before = property.clone();
        for reference in [
            ParameterResourceReference::ProjectAsset { asset_id: AssetId::new() },
            ParameterResourceReference::Uri { uri: "https://example.test/lut.cube".to_owned() },
            ParameterResourceReference::ExternalFile { path: PathBuf::from("wrong.txt").into() },
            ParameterResourceReference::ExternalFile { path: PathBuf::new().into() },
            ParameterResourceReference::ExternalFile {
                path: PathBuf::from("invalid\0.cube").into(),
            },
        ] {
            assert!(property.set_static_value(PropertyValue::Resource(reference)).is_err());
            assert_eq!(
                property, before,
                "rejection must retain author value and identities"
            );
        }
        let encoded = serde_json::to_string(&property).expect("encode");
        let decoded: AnimatedProperty = serde_json::from_str(&encoded).expect("decode");
        decoded.validate().expect("persisted contract");
        assert_eq!(decoded, property);
        let mut corrupted = decoded.clone();
        corrupted.static_value = PropertyValue::Resource(ParameterResourceReference::Uri {
            uri: "file:///invalid.cube".to_owned(),
        });
        let persisted = serde_json::to_string(&corrupted).expect("corrupt author fixture");
        let decoded: AnimatedProperty =
            serde_json::from_str(&persisted).expect("decode corrupt fixture");
        assert!(
            decoded.validate().is_err(),
            "persisted unsupported bindings must fail validation"
        );
        let mut missing = property.descriptor.schema.clone();
        missing.resource = None;
        assert_eq!(
            missing.validate(),
            Err(ParameterSchemaError::ResourceContractTypeMismatch)
        );
        let invalid_default = ParameterSchema::v1(
            ParameterId::new_static("test.resource_default"),
            PropertyValue::Resource(ParameterResourceReference::Uri {
                uri: "https://example.test/file.cube".to_owned(),
            }),
        )
        .with_resource_contract(contract.clone());
        assert_eq!(
            invalid_default.validate(),
            Err(ParameterSchemaError::InvalidResourceDefault)
        );
        property
            .set_static_value(PropertyValue::Resource(ParameterResourceReference::Unbound))
            .expect("clear");
        let mut invalid = contract;
        invalid.allowed_kinds.push(ParameterResourceKind::ExternalFile);
        assert_eq!(
            invalid.validate(),
            Err(ParameterSchemaError::InvalidResourceContract)
        );
        invalid.allowed_kinds.pop();
        invalid.path.as_mut().expect("path contract").kind = ResourcePathKind::Directory;
        assert_eq!(
            invalid.validate(),
            Err(ParameterSchemaError::InvalidResourceContract)
        );
        let invalid_type = ParameterSchema::v1(
            ParameterId::new_static("test.resource_contract"),
            PropertyValue::Text(String::new()),
        )
        .with_resource_contract(ParameterResourceContract::all_bindings());
        assert_eq!(
            invalid_type.validate(),
            Err(ParameterSchemaError::ResourceContractTypeMismatch)
        );
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
        let path = "test.control.point.midpoint";
        let mut bag = overshoot_property_bag(path, 200.0);
        bag.set_keyframe(
            path,
            Keyframe {
                id: KeyframeId::new(),
                time: tt(0),
                value: PropertyValue::Double(0.0),
                interp_in: KeyframeInterpolation::Linear,
                interp_out: KeyframeInterpolation::Bezier(ExactBezierHandle {
                    time_offset: TimelineTime::ZERO,
                    value_offset: 0.0,
                }),
                temporal_flags: KeyframeTemporalFlags::default(),
            },
        )
        .expect("key");
        bag.set_keyframe(
            path,
            Keyframe {
                id: KeyframeId::new(),
                time: tt(100),
                value: PropertyValue::Double(100.0),
                interp_in: KeyframeInterpolation::Bezier(ExactBezierHandle {
                    time_offset: TimelineTime::ZERO,
                    value_offset: 0.0,
                }),
                interp_out: KeyframeInterpolation::Linear,
                temporal_flags: KeyframeTemporalFlags::default(),
            },
        )
        .expect("key");

        let mid = bag.evaluate(path, tt(50)).and_then(|value| value.as_f64()).expect("value");
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
            AnimationChannelValue::Number(0.75)
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
        assert_eq!(keyframe.value, AnimationChannelValue::Number(22.0));
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
    fn sparse_write_retains_untouched_constrained_curve_storage() {
        for interpolation in [
            InterpolationType::AutoBezier,
            InterpolationType::ContinuousBezier,
            InterpolationType::Bezier,
        ] {
            let mut bag = PropertyBag::default();
            bag.define(PropertyDescriptor::new(
                "vector",
                "Vector",
                PropertyValue::Vec3(Vec3::ZERO),
            ));
            bag.apply_mutation(PropertyMutation::EnableAnimation {
                path: "vector".to_owned(),
                time: tt(0),
            })
            .expect("enable animation");
            for (time, value) in [(tt(0), 0.1), (tt(1), 0.3), (tt(2), 0.7)] {
                bag.apply_mutation(PropertyMutation::WriteChannels {
                    path: "vector".to_owned(),
                    time,
                    channel_values: vec![(0, value), (1, value * 2.0), (2, value * 3.0)],
                    interpolation,
                })
                .expect("seed constrained curves");
            }
            let before = bag.clone();
            let original = before.property("vector").expect("property");
            let address = original.address();
            let mutation = bag
                .prepare_parameter_write(
                    &ParameterValueWrite::Channels {
                        parameter: address,
                        channel_values: vec![(0, 0.625)],
                    },
                    tt(1),
                )
                .expect("prepare component")
                .expect("changed");
            bag.apply_mutation(mutation).expect("write component");
            let edited = bag.property("vector").expect("property");
            assert_eq!(edited.evaluate(tt(1)).to_channel_values()[0], 0.625);
            for index in [1, 2] {
                let sibling = edited.channel(index).expect("sibling");
                let previous = original.channel(index).expect("original sibling");
                assert_eq!(sibling, previous, "constrained sibling must remain exact");
                assert_eq!(
                    sibling.keyframes().as_ptr(),
                    previous.keyframes().as_ptr(),
                    "sparse writes must not detach untouched sibling curve allocations",
                );
            }
            bag.validate().expect("valid constrained author state");
        }
    }

    #[test]
    fn stable_address_full_write_preserves_independent_channel_keys() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.position",
            "Position",
            PropertyValue::Vec2(Vec2::ZERO),
        ));
        bag.apply_mutation(PropertyMutation::EnableAnimation {
            path: "transform.position".to_owned(),
            time: tt(0),
        })
        .expect("enable vector animation");
        let address = bag.address_for_path("transform.position").expect("address");
        let mutation = bag
            .prepare_channel_write_by_address(&address, tt(3), &[(0, 9.0)])
            .expect("prepare axis")
            .expect("changed");
        bag.apply_mutation(mutation).expect("write x only");
        let x = bag
            .property("transform.position")
            .expect("property")
            .channel(0)
            .expect("x")
            .keyframes
            .last()
            .expect("key")
            .clone();
        for value in [Vec2::new(11.0, 13.0), Vec2::new(15.0, 17.0)] {
            let mutation = bag
                .prepare_value_write_by_address(&address, tt(3), PropertyValue::Vec2(value))
                .expect("prepare complete gesture")
                .expect("changed");
            bag.apply_mutation(mutation).expect("complete gesture over independent keys");
            let property = bag.property("transform.position").expect("property");
            let edited = property.channel(0).expect("x").keyframes.last().expect("key");
            assert_eq!(edited.id, x.id);
            assert_eq!(edited.interp_in, x.interp_in);
            assert_eq!(edited.interp_out, x.interp_out);
            assert_eq!(edited.temporal_flags, x.temporal_flags);
            assert_eq!(property.evaluate(tt(3)), PropertyValue::Vec2(value));
        }
    }

    #[test]
    fn stable_address_channel_write_preserves_exact_held_integer_intent() {
        let mut bag = PropertyBag::default();
        let mut descriptor =
            PropertyDescriptor::new("held.count", "Count", PropertyValue::Int((1_i64 << 53) + 1));
        descriptor.schema.allowed_interpolations = vec![ParameterInterpolation::Hold];
        bag.define(descriptor);
        bag.validate().expect("valid exact held integer");
        let address = bag.address_for_path("held.count").expect("address");
        let mutation = bag
            .prepare_channel_write_by_address(&address, tt(3), &[(0, (1_u64 << 53) as f64)])
            .expect("prepare representable target")
            .expect("different canonical integer");
        bag.apply_mutation(mutation).expect("apply integer edit");
        assert_eq!(
            bag.evaluate("held.count", tt(3)),
            Some(PropertyValue::Int(1_i64 << 53))
        );
        assert!(bag
            .prepare_channel_write_by_address(&address, tt(3), &[(0, (1_u64 << 53) as f64)],)
            .expect("same canonical target")
            .is_none());
    }

    #[test]
    fn stable_address_channel_write_preserves_current_static_sibling_bits() {
        let mut bag = PropertyBag::default();
        let sibling = f32::from_bits(0x3f800001);
        bag.define(PropertyDescriptor::new(
            "transform.scale",
            "Scale",
            PropertyValue::Vec2(Vec2::new(1.0, sibling)),
        ));
        let address = bag.address_for_path("transform.scale").expect("address");
        // Preparation reads the current owner, not a UI's captured vector.
        bag.apply_mutation(PropertyMutation::SetStaticValue {
            path: "transform.scale".to_owned(),
            value: PropertyValue::Vec2(Vec2::new(1.0, sibling * 2.0)),
        })
        .expect("concurrent sibling edit");
        let mutation = bag
            .prepare_channel_write_by_address(&address, tt(3), &[(0, 1.25)])
            .expect("prepare")
            .expect("changed");
        bag.apply_mutation(mutation).expect("apply");
        let PropertyValue::Vec2(value) = bag.evaluate("transform.scale", tt(3)).expect("value")
        else {
            panic!("vector");
        };
        assert_eq!(value.x.to_bits(), 1.25_f32.to_bits());
        assert_eq!(value.y.to_bits(), (sibling * 2.0).to_bits());
        assert!(bag
            .prepare_channel_write_by_address(&address, tt(3), &[(0, 1.25 + 1e-10)])
            .expect("canonical rounded no-op")
            .is_none());
    }

    #[test]
    fn stable_address_channel_write_keeps_independent_keys_and_elides_unchanged_axes() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.position",
            "Position",
            PropertyValue::Vec2(Vec2::new(2.0, 5.0)),
        ));
        bag.apply_mutation(PropertyMutation::EnableAnimation {
            path: "transform.position".to_owned(),
            time: tt(0),
        })
        .expect("enable");
        let address = bag.address_for_path("transform.position").expect("address");
        let before = bag.clone();
        assert!(bag
            .prepare_channel_write_by_address(&address, tt(3), &[(0, 2.0), (1, 5.0)])
            .expect("evaluated no-op")
            .is_none());
        assert_eq!(bag, before);
        let sibling = bag
            .property("transform.position")
            .expect("property")
            .channel(1)
            .expect("sibling")
            .clone();
        let mutation = bag
            .prepare_channel_write_by_address(&address, tt(3), &[(0, 9.0), (1, 5.0)])
            .expect("prepare")
            .expect("changed");
        assert!(
            matches!(&mutation, PropertyMutation::WriteChannels { channel_values, .. }
            if channel_values == &[(0, 9.0)])
        );
        bag.apply_mutation(mutation).expect("apply");
        let property = bag.property("transform.position").expect("property");
        assert_eq!(property.channel(1).expect("sibling"), &sibling);
        bag.apply_mutation(PropertyMutation::UpdateChannelKeyframeHandles {
            path: "transform.position".to_owned(),
            time: tt(3),
            channel_index: 0,
            interp_in: KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: ht(-0.01),
                value_offset: -0.5,
            }),
            interp_out: KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: ht(0.01),
                value_offset: 0.5,
            }),
        })
        .expect("authored Bezier handles");
        let key = bag
            .property("transform.position")
            .expect("property")
            .channel(0)
            .expect("x")
            .keyframes
            .last()
            .expect("key")
            .clone();
        let mutation = bag
            .prepare_channel_write_by_address(&address, tt(3), &[(0, 11.0)])
            .expect("prepare existing")
            .expect("changed");
        bag.apply_mutation(mutation).expect("edit existing");
        let edited = bag
            .property("transform.position")
            .expect("property")
            .channel(0)
            .expect("x")
            .keyframes
            .last()
            .expect("key");
        assert_eq!(edited.id, key.id);
        assert_eq!(edited.interp_in, key.interp_in);
        assert_eq!(edited.interp_out, key.interp_out);
        assert_eq!(edited.temporal_flags, key.temporal_flags);
    }

    #[test]
    fn stable_address_channel_write_validates_all_updates_before_noop_filtering() {
        let mut bag = PropertyBag::default();
        let numeric =
            ParameterNumericContract::closed(0.0, 1.0, None, ParameterInvalidValuePolicy::Clamp)
                .expect("contract");
        bag.define(
            PropertyDescriptor::new("amount", "Amount", PropertyValue::Float(1.0))
                .with_numeric_contract(ParameterUnit::Normalized, numeric),
        );
        let address = bag.address_for_path("amount").expect("address");
        let before = bag.clone();
        assert!(bag
            .prepare_channel_write_by_address(&address, tt(3), &[(0, 2.0)])
            .expect("clamped no-op")
            .is_none());
        for updates in [
            vec![(0, 1.0), (0, 0.5)],
            vec![(1, 0.5)],
            vec![(0, f64::NAN)],
            vec![(0, f64::INFINITY)],
        ] {
            assert!(bag.prepare_channel_write_by_address(&address, tt(3), &updates).is_err());
        }
        let stale = AnimationParameterAddress {
            animation_track_id: AnimationTrackId::new(),
            parameter_id: address.parameter_id,
        };
        assert!(bag.prepare_channel_write_by_address(&stale, tt(3), &[(0, 0.5)]).is_err());
        assert_eq!(bag, before);
    }

    #[test]
    fn write_channels_rejects_duplicate_components_without_mutating_owner() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.position",
            "Position",
            PropertyValue::Vec2(Vec2::new(2.0, 5.0)),
        ));
        let before = bag.clone();
        let result = bag.apply_mutation(PropertyMutation::WriteChannels {
            path: "transform.position".to_owned(),
            time: tt(3),
            channel_values: vec![(0, 9.0), (0, 11.0)],
            interpolation: InterpolationType::Linear,
        });
        assert!(
            result.is_err(),
            "ambiguous repeated component must be rejected"
        );
        assert_eq!(bag, before);
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
        assert_eq!(after_keyframe.value, AnimationChannelValue::Number(22.0));
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
            interp_in: KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: time
                    .checked_sub(previous_time)
                    .expect("incoming span")
                    .checked_scale(crate::TimeScale::new(-1, 4).expect("quarter"))
                    .expect("incoming offset"),
                value_offset: -0.1 * (0.75 - 0.25),
            }),
            interp_out: KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: next_time
                    .checked_sub(time)
                    .expect("outgoing span")
                    .checked_scale(crate::TimeScale::new(3, 10).expect("influence"))
                    .expect("outgoing offset"),
                value_offset: 0.15 * (0.95_f32 as f64 - 0.75),
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
    fn visual_bezier_endpoint_preset_retains_future_segment_intent() {
        for preset in [
            InterpolationType::Bezier,
            InterpolationType::ContinuousBezier,
        ] {
            let mut bag = PropertyBag::default();
            let path = "endpoint.curve";
            bag.define(PropertyDescriptor::new(
                path,
                "Endpoint",
                PropertyValue::Float(0.0),
            ));
            bag.apply_mutation(PropertyMutation::SetKeyframe {
                path: path.to_owned(),
                keyframe: Keyframe::linear(tt(5), PropertyValue::Float(0.5)),
            })
            .unwrap();
            bag.apply_mutation(PropertyMutation::UpdateKeyframeInterpolation {
                path: path.to_owned(),
                time: tt(5),
                interpolation: preset,
            })
            .unwrap();
            let selected = bag.property(path).unwrap().keyframe_at(tt(5)).unwrap();
            assert!(
                selected.interp_in.is_bezier(),
                "explicit {preset:?} must retain incoming intent before a neighbor exists"
            );
            assert!(
                selected.interp_out.is_bezier(),
                "explicit {preset:?} must retain outgoing intent before a neighbor exists"
            );
            for (time, value) in [(tt(0), 0.0), (tt(10), 1.0)] {
                bag.apply_mutation(PropertyMutation::SetKeyframe {
                    path: path.to_owned(),
                    keyframe: Keyframe::linear(time, PropertyValue::Float(value)),
                })
                .unwrap();
            }
            let stored = bag.property(path).unwrap().keyframe_at(tt(5)).unwrap();
            assert_eq!(stored.id, selected.id);
            assert!(stored.interp_in.is_bezier());
            assert!(stored.interp_out.is_bezier());
            bag.validate().unwrap();
            let reopened: PropertyBag =
                serde_json::from_str(&serde_json::to_string(&bag).unwrap()).unwrap();
            assert_eq!(reopened, bag);
        }
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
                interp_in: KeyframeInterpolation::Bezier(ExactBezierHandle {
                    time_offset: ht(-0.04),
                    value_offset: -0.075,
                }),
                interp_out: KeyframeInterpolation::Bezier(ExactBezierHandle {
                    time_offset: ht(0.09),
                    value_offset: 0.125,
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
        assert!((in_handle.time_offset.to_f64().abs() - 0.04).abs() < 1e-6);
        assert!((out_handle.time_offset.to_f64() - 0.09).abs() < 1e-6);
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
        assert_eq!(ease_in.interp_out, KeyframeInterpolation::DefaultBezier);

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
        assert_eq!(ease_out.interp_in, KeyframeInterpolation::DefaultBezier);
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
        assert_eq!(
            stored.temporal_flags,
            KeyframeTemporalFlags { continuous: false, ..flags }
        );
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
            interp_in: KeyframeInterpolation::Bezier(ExactBezierHandle {
                time_offset: ht(-0.3),
                value_offset: -0.1,
            }),
            interp_out: KeyframeInterpolation::Bezier(ExactBezierHandle {
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
    fn dragging_one_continuous_bezier_handle_aligns_the_opposite_tangent() {
        let mut bag = PropertyBag::default();
        bag.define(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));
        let start = tt(0);
        let middle = tt(5);
        let end = tt(10);
        for (time, value, mode) in [
            (start, 0.0, InterpolationType::Linear),
            (middle, 0.5, InterpolationType::ContinuousBezier),
            (end, 1.0, InterpolationType::Linear),
        ] {
            bag.apply_mutation(PropertyMutation::SetKeyframe {
                path: "transform.opacity".to_owned(),
                keyframe: Keyframe::from_preset(time, PropertyValue::Float(value), mode),
            })
            .expect("seed keyframe");
        }
        let before = bag
            .property("transform.opacity")
            .and_then(|property| property.channel(0))
            .and_then(|channel| channel.keyframe_at(middle))
            .cloned()
            .expect("middle keyframe");
        let new_out = KeyframeInterpolation::Bezier(ExactBezierHandle {
            time_offset: ht(0.06),
            value_offset: 0.2,
        });
        bag.apply_mutation(PropertyMutation::UpdateChannelKeyframeHandles {
            path: "transform.opacity".to_owned(),
            time: middle,
            channel_index: 0,
            interp_in: before.interp_in,
            interp_out: new_out,
        })
        .expect("drag outgoing handle");

        let channel = bag
            .property("transform.opacity")
            .and_then(|property| property.channel(0))
            .expect("opacity channel");
        let keys = channel.keyframes();
        let incoming = handle_for_in(keys[1].interp_in);
        let outgoing = handle_for_out(keys[1].interp_out);
        let in_slope = tangent_slope_from_in(&keys[0], &keys[1], incoming).expect("incoming slope");
        let out_slope =
            tangent_slope_from_out(&keys[1], &keys[2], outgoing).expect("outgoing slope");
        assert!((in_slope - out_slope).abs() < 1e-6);
        assert_eq!(keys[1].interp_out, new_out);
        assert!(keys[1].temporal_flags.continuous);
    }

    #[test]
    fn manual_bezier_preset_derives_controls_in_each_adjacent_owner_span() {
        let middle =
            Keyframe::from_preset(tt(5), PropertyValue::Double(0.5), InterpolationType::Bezier);
        assert_eq!(middle.interp_in, KeyframeInterpolation::DefaultBezier);
        assert_eq!(middle.interp_out, KeyframeInterpolation::DefaultBezier);
        let path = "test.default.owner.controls";
        let mut bag = overshoot_property_bag(path, 10.0);
        for key in [
            Keyframe::linear(tt(0), PropertyValue::Double(0.0)),
            middle,
            Keyframe::linear(tt(10), PropertyValue::Double(1.0)),
        ] {
            bag.set_keyframe(path, key).expect("key");
        }
        assert_eq!(
            bag.evaluate(path, TimelineTime::new(1, 10).expect("left midpoint")),
            Some(PropertyValue::Double(0.3125))
        );
        assert_eq!(
            bag.evaluate(path, TimelineTime::new(3, 10).expect("right midpoint")),
            Some(PropertyValue::Double(0.6875))
        );
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
                interp_in: KeyframeInterpolation::Bezier(ExactBezierHandle {
                    time_offset: ht(-0.05),
                    value_offset: -0.1,
                }),
                interp_out: KeyframeInterpolation::Bezier(ExactBezierHandle {
                    time_offset: ht(0.05),
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
        assert!(stored.interp_in.is_bezier());
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
                interp_in: KeyframeInterpolation::Bezier(ExactBezierHandle {
                    time_offset: ht(-0.04),
                    value_offset: -0.3,
                }),
                interp_out: KeyframeInterpolation::Bezier(ExactBezierHandle {
                    time_offset: ht(0.112),
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
        assert!(first.interp_out.is_bezier());
        assert!(last.interp_in.is_bezier());
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

#[cfg(test)]
mod resource_path_owner_tests {
    use super::*;
    fn owner() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\project")
        } else {
            PathBuf::from("/project")
        }
    }
    #[test]
    fn relative_resource_binding_preserves_native_path_and_nonexistent_intent() {
        let relative = PathBuf::from("missing").join("..").join("resource.cube");
        let mut reference =
            ParameterResourceReference::ExternalFile { path: relative.clone().into() };
        reference.resolve_external_path(&owner()).expect("explicit owner");
        assert_eq!(
            reference,
            ParameterResourceReference::ExternalFile { path: owner().join(relative).into() }
        );
        let bound = reference.clone();
        reference
            .resolve_external_path(std::path::Path::new("invalid-owner"))
            .expect("bound reference does not consult a new owner");
        assert_eq!(reference, bound);
    }
    #[test]
    fn invalid_resource_binding_leaves_author_intent_unchanged() {
        for path in [PathBuf::new(), PathBuf::from("resource.cube")] {
            let mut reference = ParameterResourceReference::ExternalFile { path: path.into() };
            let original = reference.clone();
            assert!(reference
                .resolve_external_path(std::path::Path::new("relative-owner"))
                .is_err());
            assert_eq!(reference, original);
        }
    }
    #[cfg(unix)]
    #[test]
    fn relative_resource_binding_preserves_non_unicode_path_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(std::ffi::OsString::from_vec(vec![b'l', 0xff, b't']));
        let mut reference = ParameterResourceReference::ExternalFile { path: path.clone().into() };
        reference.resolve_external_path(&owner()).expect("native path binding");
        assert_eq!(
            reference,
            ParameterResourceReference::ExternalFile { path: owner().join(path).into() }
        );
    }

    #[cfg(windows)]
    #[test]
    fn long_verbatim_resource_owner_reads_the_same_real_file_with_parent_components() {
        let root = tempfile::tempdir().expect("owned resource fixture");
        let mut directory = root.path().to_path_buf();
        for _ in 0..5 {
            directory.push("long-resource-directory".repeat(3));
        }
        std::fs::create_dir_all(directory.join("child")).expect("long directory");
        std::fs::write(directory.join("resource.cube"), b"owned resource").expect("resource");
        let owner = std::fs::canonicalize(&directory).expect("native verbatim owner");
        assert!(owner.as_os_str().len() > 260, "exercise a real long path");
        for path in [r"child\..\resource.cube", "child/../resource.cube"] {
            let mut reference =
                ParameterResourceReference::ExternalFile { path: PathBuf::from(path).into() };
            reference.resolve_external_path(&owner).expect("long resource binding");
            let ParameterResourceReference::ExternalFile { path } = reference else {
                panic!("external path");
            };
            assert_eq!(
                std::fs::read(path.native_path().expect("local path"))
                    .expect("resolved native file"),
                b"owned resource"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn relative_resource_binding_preserves_non_unicode_windows_path() {
        use std::os::windows::ffi::OsStringExt;
        let path = PathBuf::from(std::ffi::OsString::from_wide(&[
            b'l' as u16,
            0xd800,
            b't' as u16,
        ]));
        let mut reference = ParameterResourceReference::ExternalFile { path: path.clone().into() };
        reference.resolve_external_path(&owner()).expect("native path binding");
        let ParameterResourceReference::ExternalFile { path: resolved } = reference else {
            panic!("external path");
        };
        let resolved = resolved.native_path().expect("local native path");
        assert_eq!(
            resolved.strip_prefix(owner()).expect("explicit owner").as_os_str(),
            path.as_os_str()
        );
        assert!(resolved.to_str().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn native_resource_path_survives_json_persistence_without_unicode_loss() {
        use std::os::windows::ffi::OsStringExt;
        let reference = ParameterResourceReference::ExternalFile {
            path: PathBuf::from(std::ffi::OsString::from_wide(&[
                b'C' as u16,
                b':' as u16,
                b'\\' as u16,
                0xd800,
                b'.' as u16,
                b'c' as u16,
                b'u' as u16,
                b'b' as u16,
                b'e' as u16,
            ]))
            .into(),
        };
        let bytes = serde_json::to_vec(&reference).expect("persist admitted native resource");
        let decoded: ParameterResourceReference =
            serde_json::from_slice(&bytes).expect("restore native resource");
        assert_eq!(decoded, reference);
    }

    #[cfg(windows)]
    #[test]
    fn absolute_unc_resource_retains_its_identity() {
        let mut reference = ParameterResourceReference::ExternalFile {
            path: PathBuf::from(r"\\server\share\resource.cube").into(),
        };
        let original = reference.clone();
        reference.resolve_external_path(&owner()).expect("UNC resource");
        assert_eq!(reference, original);
    }

    #[cfg(windows)]
    #[test]
    fn drive_relative_resource_never_uses_process_drive_directories() {
        let mut reference = ParameterResourceReference::ExternalFile {
            path: PathBuf::from(r"D:resource.cube").into(),
        };
        let original = reference.clone();
        assert_eq!(
            reference.resolve_external_path(&owner()),
            Err(ParameterResourcePathError::AmbiguousPath)
        );
        assert_eq!(reference, original);
        let mut rooted = ParameterResourceReference::ExternalFile {
            path: PathBuf::from(r"\resource.cube").into(),
        };
        rooted.resolve_external_path(&owner()).expect("explicit owner drive");
        assert_eq!(
            rooted,
            ParameterResourceReference::ExternalFile {
                path: PathBuf::from(r"C:\resource.cube").into()
            }
        );
    }
}
