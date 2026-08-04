//! Clip（时间线剪辑片段）

use crate::audio::AudioComponentEdit;
use glam::Vec2;
use mondrian_core::{
    automation::{
        AnimatedProperty, ParameterEnumOption, ParameterInvalidValuePolicy,
        ParameterNumericContract, ParameterUnit, PropertyBag, PropertyDescriptor, PropertyHost,
        PropertyMutation, PropertyValue,
    },
    effect_data::{EffectNode, EffectType},
    mask_data::MaskComponent,
    types::*,
    AuthoringFootprint, AuthoringFootprintCollector, AuthoringFootprintError, AuthoringList,
    MondrianError, ParameterId, Result, TimeScale, TimelineTime,
};
use serde::{Deserialize, Serialize};

/// 裁剪边缘
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimEdge {
    /// 裁剪入点（左侧）
    In,
    /// 裁剪出点（右侧）
    Out,
}

/// Stable relative placement for one visual Effect inside a Clip chain.
///
/// Effect indexes are snapshot-local presentation data. Authoring therefore
/// addresses both the moving instance and its anchor by stable `EffectId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectRelativePlacement {
    /// Place the moving Effect immediately before the anchor.
    Before(EffectId),
    /// Place the moving Effect immediately after the anchor.
    After(EffectId),
}

/// 2D 变换（位置 / 缩放 / 旋转 / 锚点 / 不透明度），所有属性可关键帧动画
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transform2D {
    properties: PropertyBag,
}

impl AuthoringFootprint for Transform2D {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), AuthoringFootprintError> {
        let Self { properties } = self;
        collector.collect(properties)
    }
}

impl Transform2D {
    pub const POSITION_PATH: &'static str = "transform.position";
    pub const SCALE_PATH: &'static str = "transform.scale";
    pub const ROTATION_PATH: &'static str = "transform.rotation";
    pub const ANCHOR_POINT_PATH: &'static str = "transform.anchor_point";
    pub const OPACITY_PATH: &'static str = "transform.opacity";

    pub fn identity() -> Self {
        let mut properties = PropertyBag::default();
        properties.define(
            PropertyDescriptor::new(Self::POSITION_PATH, "位置", PropertyValue::Vec2(Vec2::ZERO))
                .with_parameter_id(ParameterId::new_static("mondrian.transform.position"))
                .with_unit(ParameterUnit::Pixels),
        );
        properties.define(
            PropertyDescriptor::new(Self::SCALE_PATH, "缩放", PropertyValue::Vec2(Vec2::ONE))
                .with_parameter_id(ParameterId::new_static("mondrian.transform.scale"))
                .with_unit(ParameterUnit::Normalized),
        );
        properties.define(
            PropertyDescriptor::new(Self::ROTATION_PATH, "旋转", PropertyValue::Float(0.0))
                .with_parameter_id(ParameterId::new_static("mondrian.transform.rotation"))
                .with_unit(ParameterUnit::Degrees),
        );
        properties.define(
            PropertyDescriptor::new(
                Self::ANCHOR_POINT_PATH,
                "锚点",
                PropertyValue::Vec2(Vec2::ZERO),
            )
            .with_parameter_id(ParameterId::new_static("mondrian.transform.anchor_point"))
            .with_unit(ParameterUnit::Pixels),
        );
        properties.define(
            PropertyDescriptor::new(Self::OPACITY_PATH, "不透明度", PropertyValue::Float(1.0))
                .with_parameter_id(ParameterId::new_static("mondrian.transform.opacity"))
                .with_numeric_contract(ParameterUnit::Normalized, normalized_opacity_contract()),
        );
        Self { properties }
    }

    /// 求值为 3x3 仿射变换矩阵
    ///
    /// T(position) · R(rotation) · S(scale) · T(-anchor)
    /// 锚点定义缩放旋转中心，位置定义锚点在父空间中的坐标。
    pub fn evaluate_matrix(&self, time: TimelineTime) -> glam::Mat3 {
        let pos = self.evaluate_vec2(Self::POSITION_PATH, time);
        let scale = self.evaluate_vec2(Self::SCALE_PATH, time);
        let rot = self.evaluate_f32(Self::ROTATION_PATH, time).to_radians();
        let anchor = self.evaluate_vec2(Self::ANCHOR_POINT_PATH, time);

        let cos_r = rot.cos();
        let sin_r = rot.sin();

        // pos + R * S * (v - anchor) for vertex v
        let tx = pos.x + scale.x * (cos_r * (-anchor.x) - sin_r * (-anchor.y));
        let ty = pos.y + scale.y * (sin_r * (-anchor.x) + cos_r * (-anchor.y));

        glam::Mat3::from_cols(
            glam::Vec3::new(scale.x * cos_r, scale.x * sin_r, 0.0),
            glam::Vec3::new(-scale.y * sin_r, scale.y * cos_r, 0.0),
            glam::Vec3::new(tx, ty, 1.0),
        )
    }

    pub fn evaluate_opacity(&self, time: TimelineTime) -> f32 {
        self.evaluate_f32(Self::OPACITY_PATH, time)
    }

    pub fn to_property_bag(&self) -> PropertyBag {
        self.properties.clone()
    }

    fn fork_author_identities(&mut self) {
        self.properties.fork_author_identities();
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

    /// Directly set position.
    pub fn set_position(&mut self, v: glam::Vec2) {
        let _ = self.properties.set_static_value(Self::POSITION_PATH, PropertyValue::Vec2(v));
    }

    /// Read current position value.
    pub fn get_position(&self, time: TimelineTime) -> glam::Vec2 {
        self.evaluate_vec2(Self::POSITION_PATH, time)
    }

    /// Directly set scale.
    pub fn set_scale(&mut self, v: glam::Vec2) {
        let _ = self.properties.set_static_value(Self::SCALE_PATH, PropertyValue::Vec2(v));
    }

    /// Read current scale value.
    pub fn get_scale(&self, time: TimelineTime) -> glam::Vec2 {
        self.evaluate_vec2(Self::SCALE_PATH, time)
    }

    /// Set anchor point.
    pub fn set_anchor_point(&mut self, v: glam::Vec2) {
        let _ = self
            .properties
            .set_static_value(Self::ANCHOR_POINT_PATH, PropertyValue::Vec2(v));
    }

    /// Read current anchor value.
    pub fn get_anchor_point(&self, time: TimelineTime) -> glam::Vec2 {
        self.evaluate_vec2(Self::ANCHOR_POINT_PATH, time)
    }

    fn evaluate_vec2(&self, path: &str, time: TimelineTime) -> Vec2 {
        self.properties
            .evaluate(path, time)
            .and_then(|value| value.as_vec2())
            .unwrap_or(Vec2::ZERO)
    }

    fn evaluate_f32(&self, path: &str, time: TimelineTime) -> f32 {
        self.properties
            .evaluate(path, time)
            .and_then(|value| value.as_f32())
            .unwrap_or(0.0)
    }
}

fn normalized_opacity_contract() -> ParameterNumericContract {
    ParameterNumericContract::closed(0.0, 1.0, Some(0.01), ParameterInvalidValuePolicy::Reject)
        .expect("opacity has a valid built-in numeric contract")
}

fn blend_mode_to_text(mode: Option<BlendMode>) -> String {
    match mode {
        None => "inherit".to_string(),
        Some(BlendMode::Normal) => "Normal".to_string(),
        Some(BlendMode::Dissolve) => "Dissolve".to_string(),
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
        Some(BlendMode::Subtract) => "Subtract".to_string(),
        Some(BlendMode::DarkerColor) => "DarkerColor".to_string(),
        Some(BlendMode::LighterColor) => "LighterColor".to_string(),
        Some(BlendMode::LinearBurn) => "LinearBurn".to_string(),
        Some(BlendMode::LinearDodge) => "LinearDodge".to_string(),
        Some(BlendMode::VividLight) => "VividLight".to_string(),
        Some(BlendMode::LinearLight) => "LinearLight".to_string(),
        Some(BlendMode::PinLight) => "PinLight".to_string(),
        Some(BlendMode::HardMix) => "HardMix".to_string(),
        Some(BlendMode::Divide) => "Divide".to_string(),
        Some(BlendMode::Hue) => "Hue".to_string(),
        Some(BlendMode::Saturation) => "Saturation".to_string(),
        Some(BlendMode::Color) => "Color".to_string(),
        Some(BlendMode::Luminosity) => "Luminosity".to_string(),
    }
}

fn blend_mode_options() -> Vec<ParameterEnumOption> {
    [
        "inherit",
        "Normal",
        "Dissolve",
        "Multiply",
        "Screen",
        "Overlay",
        "Darken",
        "Lighten",
        "ColorDodge",
        "ColorBurn",
        "HardLight",
        "SoftLight",
        "Difference",
        "Exclusion",
        "Subtract",
        "DarkerColor",
        "LighterColor",
        "LinearBurn",
        "LinearDodge",
        "VividLight",
        "LinearLight",
        "PinLight",
        "HardMix",
        "Divide",
        "Hue",
        "Saturation",
        "Color",
        "Luminosity",
    ]
    .into_iter()
    .map(|key| ParameterEnumOption::new(key, format!("mondrian.blend_mode.{key}.label")))
    .collect()
}

fn blend_mode_from_text(value: &str) -> Result<Option<BlendMode>> {
    Ok(match value {
        "inherit" => None,
        "Normal" => Some(BlendMode::Normal),
        "Dissolve" => Some(BlendMode::Dissolve),
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
        "Subtract" => Some(BlendMode::Subtract),
        "DarkerColor" => Some(BlendMode::DarkerColor),
        "LighterColor" => Some(BlendMode::LighterColor),
        "LinearBurn" => Some(BlendMode::LinearBurn),
        "LinearDodge" => Some(BlendMode::LinearDodge),
        "VividLight" => Some(BlendMode::VividLight),
        "LinearLight" => Some(BlendMode::LinearLight),
        "PinLight" => Some(BlendMode::PinLight),
        "HardMix" => Some(BlendMode::HardMix),
        "Divide" => Some(BlendMode::Divide),
        "Hue" => Some(BlendMode::Hue),
        "Saturation" => Some(BlendMode::Saturation),
        "Color" => Some(BlendMode::Color),
        "Luminosity" => Some(BlendMode::Luminosity),
        other => {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "blend_mode_parse".to_string(),
                reason: format!("无法解析混合模式: {other}"),
            });
        }
    })
}

/// Canonical mapping from Clip-local placement time to source-media time.
///
/// The tagged algebra is the persistence seam for future validated piecewise
/// retiming. Variable retiming must become another closed variant with an
/// explicit inverse/ambiguity contract; it must not be approximated by ordinary
/// parameter automation or parallel source-range fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClipSourceTimeMap {
    /// One exact affine mapping for forward speed, reverse speed, or a hold.
    Constant {
        /// Source coordinate sampled at Clip-local time zero.
        source_origin: TimelineTime,
        /// Exact source-time delta per Clip-local time delta.
        scale: TimeScale,
    },
}

impl ClipSourceTimeMap {
    /// Construct an identity mapping from the requested source origin.
    pub const fn identity(source_origin: TimelineTime) -> Self {
        Self::Constant { source_origin, scale: TimeScale::ONE }
    }

    /// Construct one exact constant source-time mapping.
    pub const fn constant(source_origin: TimelineTime, scale: TimeScale) -> Self {
        Self::Constant { source_origin, scale }
    }

    /// Source coordinate sampled at Clip-local time zero.
    pub const fn source_origin(&self) -> TimelineTime {
        match self {
            Self::Constant { source_origin, .. } => *source_origin,
        }
    }

    /// Exact source-time delta per Clip-local time delta.
    pub const fn scale(&self) -> TimeScale {
        match self {
            Self::Constant { scale, .. } => *scale,
        }
    }

    /// Return the same mapping with a different source origin.
    pub fn with_source_origin(&self, source_origin: TimelineTime) -> Self {
        match self {
            Self::Constant { scale, .. } => Self::Constant { source_origin, scale: *scale },
        }
    }

    /// Map one Clip-local time into exact source-media time.
    pub fn map(&self, clip_local_time: TimelineTime) -> Result<TimelineTime> {
        let source_delta = clip_local_time.checked_scale(self.scale())?;
        Ok(self.source_origin().checked_add(source_delta)?)
    }

    /// Invert one exact source coordinate into Clip-local time.
    ///
    /// A zero-rate hold has no unique inverse and is rejected.
    pub fn inverse(&self, source_time: TimelineTime) -> Result<TimelineTime> {
        let source_delta = source_time.checked_sub(self.source_origin())?;
        Ok(source_delta.checked_scale(self.scale().reciprocal()?)?)
    }
}

impl Default for ClipSourceTimeMap {
    fn default() -> Self {
        Self::identity(TimelineTime::ZERO)
    }
}

impl AuthoringFootprint for ClipSourceTimeMap {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), AuthoringFootprintError> {
        match self {
            Self::Constant { source_origin: _, scale: _ } => Ok(()),
        }
    }
}

/// 时间线片段语义 — re-exported from mondrian_core::timeline_data.
pub use mondrian_core::timeline_data::{
    AlphaInterpretation, ClipContent, ClipKind, MediaInterpretation, NestedColorProcessing,
};

/// 时间线上的一个剪辑片段
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    pub id: ClipId,
    /// Closed payload algebra; impossible cross-kind field combinations cannot
    /// enter the author snapshot.
    pub content: ClipContent,
    /// 在时间线上的起始位置
    pub position: TimelineTime,
    /// 在时间线上的持续时长
    pub duration: TimelineTime,
    /// Clip-local visual author time visible at `position`.
    ///
    /// Transform, Opacity, visual effects, Masks, and generated visual
    /// content all use this stable domain. Ordinary placement moves and source
    /// slips preserve it; an in-edge trim advances it so hidden keyframes are
    /// not silently rebased to the new visible edge.
    pub clip_time_in: TimelineTime,
    /// Closed source-sampling transform. Source terminal boundaries are derived
    /// from this mapping and `duration`; no parallel mutable source out-point is
    /// persisted.
    source_time_map: ClipSourceTimeMap,
    /// 2D 变换（关键帧）
    pub transform: Transform2D,
    /// 效果链（实例级，属性路径已按 effect id 做命名空间隔离）
    #[serde(default)]
    pub effects: AuthoringList<EffectNode>,
    /// 蒙版列表（按顺序叠加渲染）
    #[serde(default)]
    pub masks: AuthoringList<MaskComponent>,
    /// Optional Sequence-local edit-synchronization group. Every member with
    /// the same identity participates in linked selection/edit operations.
    #[serde(default)]
    pub link_group: Option<ClipLinkGroupId>,
    /// Placement-local audio authoring. Track membership and temporal placement
    /// remain owned exclusively by the containing Track and this Clip.
    #[serde(default)]
    pub audio_components: AuthoringList<AudioComponentEdit>,
    /// 是否禁用
    pub is_disabled: bool,
    /// 混合模式（覆盖轨道设置）
    pub blend_mode: Option<BlendMode>,
    /// 显示标签（可选）
    pub label: Option<String>,
}

impl AuthoringFootprint for Clip {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), AuthoringFootprintError> {
        let Self {
            id: _,
            content,
            position: _,
            duration: _,
            clip_time_in: _,
            source_time_map,
            transform,
            effects,
            masks,
            link_group: _,
            audio_components,
            is_disabled: _,
            blend_mode: _,
            label,
        } = self;
        collector.collect(content)?;
        collector.collect(source_time_map)?;
        collector.collect(transform)?;
        collector.collect(effects)?;
        collector.collect(masks)?;
        collector.collect(audio_components)?;
        collector.collect(label)
    }
}

impl Clip {
    pub const BLEND_MODE_PATH: &'static str = "clip.blend_mode";
    pub const SOLID_COLOR_PATH: &'static str = "clip.solid_color";

    pub fn new(asset_id: AssetId, position: TimelineTime, duration: TimelineTime) -> Result<Self> {
        Self::with_content(
            ClipContent::Media {
                asset_id,
                interpretation: MediaInterpretation::default(),
            },
            position,
            duration,
        )
    }

    /// Create a file-backed still placement as a zero-rate source-time hold.
    ///
    /// Timeline duration remains independently editable while every evaluation
    /// samples the same source instant. This avoids inventing a separate visual
    /// payload algebra for still files while preserving explicit hold semantics.
    pub fn new_still_image(
        asset_id: AssetId,
        position: TimelineTime,
        duration: TimelineTime,
    ) -> Result<Self> {
        let mut clip = Self::new(asset_id, position, duration)?;
        clip.set_constant_source_time_map(TimelineTime::ZERO, TimeScale::new(0, 1)?)?;
        Ok(clip)
    }
    fn with_content(
        content: ClipContent,
        position: TimelineTime,
        duration: TimelineTime,
    ) -> Result<Self> {
        if duration.is_negative() {
            return Err(mondrian_core::TimelineTimeError::NegativeDuration.into());
        }
        Ok(Self {
            id: ClipId::new(),
            content,
            position,
            duration,
            clip_time_in: TimelineTime::ZERO,
            source_time_map: ClipSourceTimeMap::default(),
            transform: Transform2D::identity(),
            effects: AuthoringList::new(),
            masks: AuthoringList::new(),
            link_group: None,
            audio_components: AuthoringList::new(),
            is_disabled: false,
            blend_mode: None,
            label: None,
        })
    }

    pub fn new_solid_color(
        asset_id: AssetId,
        color: Color,
        position: TimelineTime,
        duration: TimelineTime,
    ) -> Result<Self> {
        let mut clip = Self::with_content(
            ClipContent::SolidColor { asset_id, color },
            position,
            duration,
        )?;
        clip.label = Some("纯色层".to_string());
        Ok(clip)
    }

    /// Create one sequence-local Basic Title Clip.
    pub fn new_basic_title(
        text: impl Into<String>,
        font_family: impl Into<String>,
        position: TimelineTime,
        duration: TimelineTime,
    ) -> Result<Self> {
        let title = mondrian_core::BasicTitle::new(text, font_family)?;
        let mut clip = Self::with_content(ClipContent::BasicTitle { title }, position, duration)?;
        clip.label = Some("基础标题".to_owned());
        Ok(clip)
    }

    pub fn new_adjustment_layer(
        asset_id: AssetId,
        position: TimelineTime,
        duration: TimelineTime,
    ) -> Result<Self> {
        let mut clip = Self::with_content(
            ClipContent::AdjustmentLayer { asset_id },
            position,
            duration,
        )?;
        clip.label = Some("调整图层".to_string());
        Ok(clip)
    }

    pub fn new_nested_sequence(
        sequence_id: SequenceId,
        position: TimelineTime,
        duration: TimelineTime,
        label: Option<String>,
    ) -> Result<Self> {
        let mut clip = Self::with_content(
            ClipContent::NestedSequence {
                sequence_id,
                color_processing: NestedColorProcessing::default(),
            },
            position,
            duration,
        )?;
        clip.label = label.or_else(|| Some("嵌套序列".to_string()));
        Ok(clip)
    }

    pub fn is_adjustment_layer(&self) -> bool {
        matches!(self.content, ClipContent::AdjustmentLayer { .. })
    }

    pub fn is_solid_color(&self) -> bool {
        matches!(self.content, ClipContent::SolidColor { .. })
    }

    pub fn is_nested_sequence(&self) -> bool {
        matches!(self.content, ClipContent::NestedSequence { .. })
    }

    /// Whether this Clip contains a generated Basic Title.
    pub fn is_basic_title(&self) -> bool {
        matches!(self.content, ClipContent::BasicTitle { .. })
    }

    /// Stable content discriminator for presentation adapters.
    pub const fn kind(&self) -> ClipKind {
        self.content.kind()
    }

    /// Asset Library identity for asset-backed Clip content.
    ///
    /// Generated content may have a library identity without owning an
    /// external media file.
    pub const fn library_asset_id(&self) -> Option<AssetId> {
        self.content.library_asset_id()
    }

    /// File-backed media identity required by decode and export.
    pub const fn media_asset_id(&self) -> Option<AssetId> {
        self.content.media_asset_id()
    }

    /// Nested Sequence identity for nested content.
    pub const fn nested_sequence_id(&self) -> Option<SequenceId> {
        self.content.nested_sequence_id()
    }

    /// Media interpretation for file-backed content.
    pub const fn media_interpretation(&self) -> Option<&MediaInterpretation> {
        self.content.media_interpretation()
    }

    /// Mutable media interpretation for file-backed content.
    pub fn media_interpretation_mut(&mut self) -> Option<&mut MediaInterpretation> {
        self.content.media_interpretation_mut()
    }

    /// Clip 在时间线上的结束位置
    pub fn end_position(&self) -> Result<TimelineTime> {
        Ok(self.position.checked_add(self.duration)?)
    }

    /// Clip-local visual author time at the exclusive placement end.
    pub fn clip_time_out(&self) -> Result<TimelineTime> {
        Ok(self.clip_time_in.checked_add(self.duration)?)
    }

    /// Canonical source-time mapping owned by this placement.
    pub const fn source_time_map(&self) -> &ClipSourceTimeMap {
        &self.source_time_map
    }

    /// Source coordinate sampled at the visible Clip in-edge.
    ///
    /// For reverse playback this is the first sampled coordinate, not the
    /// minimum of a source interval.
    pub const fn source_origin(&self) -> TimelineTime {
        self.source_time_map.source_origin()
    }

    /// Exact source-time delta per Clip-local placement-time delta.
    pub const fn source_time_scale(&self) -> TimeScale {
        self.source_time_map.scale()
    }

    /// Exact source coordinate at the exclusive Clip placement end.
    ///
    /// This derived boundary may be before `source_origin` for reverse
    /// playback and equals it for a zero-rate hold.
    pub fn source_terminal_boundary(&self) -> Result<TimelineTime> {
        self.source_time_map.map(self.duration)
    }

    /// Replace the source coordinate sampled at the visible Clip in-edge.
    ///
    /// The candidate is checked against the current duration before commit, so
    /// arithmetic overflow cannot partially mutate author state.
    pub fn set_source_origin(&mut self, source_origin: TimelineTime) -> Result<()> {
        let candidate = self.source_time_map.with_source_origin(source_origin);
        candidate.map(self.duration)?;
        self.source_time_map = candidate;
        Ok(())
    }

    /// Replace this placement with one exact constant source-time mapping.
    ///
    /// Positive, negative, and zero scales represent forward playback,
    /// reverse playback, and a source hold respectively.
    pub fn set_constant_source_time_map(
        &mut self,
        source_origin: TimelineTime,
        scale: TimeScale,
    ) -> Result<()> {
        let candidate = ClipSourceTimeMap::constant(source_origin, scale);
        candidate.map(self.duration)?;
        self.source_time_map = candidate;
        Ok(())
    }

    /// Replace the complete canonical source-time mapping.
    ///
    /// The replacement is checked against the current Clip duration before it
    /// becomes author state.
    pub fn replace_source_time_map(&mut self, source_time_map: ClipSourceTimeMap) -> Result<()> {
        source_time_map.map(self.duration)?;
        self.source_time_map = source_time_map;
        Ok(())
    }

    /// Validate Clip time ranges and the complete source mapping.
    pub fn validate_time_state(&self) -> Result<()> {
        if self.duration.is_negative() {
            return Err(mondrian_core::TimelineTimeError::NegativeDuration.into());
        }
        self.end_position()?;
        self.clip_time_out()?;
        self.source_terminal_boundary()?;
        Ok(())
    }

    /// 判断给定时间码是否在此 Clip 范围内
    pub fn contains(&self, time: TimelineTime) -> Result<bool> {
        Ok(time >= self.position && time < self.end_position()?)
    }

    /// Map Sequence-local placement time into stable Clip-local visual time.
    ///
    /// This mapping deliberately does not include source sampling:
    /// visual processors are downstream of source sampling and remain attached
    /// to the Clip occurrence when the source is slipped or retimed.
    pub fn timeline_to_clip_time(&self, timeline_time: TimelineTime) -> Result<TimelineTime> {
        let placement_offset = timeline_time.checked_sub(self.position)?;
        Ok(self.clip_time_in.checked_add(placement_offset)?)
    }

    /// Map stable Clip-local visual time back into Sequence placement time.
    pub fn clip_to_timeline_time(&self, clip_time: TimelineTime) -> Result<TimelineTime> {
        let placement_offset = clip_time.checked_sub(self.clip_time_in)?;
        Ok(self.position.checked_add(placement_offset)?)
    }

    /// 将时间线时间 → Clip 内本地时间 → 素材源时间
    pub fn timeline_to_source_time(&self, timeline_time: TimelineTime) -> Result<TimelineTime> {
        let local = timeline_time.checked_sub(self.position)?;
        self.source_time_map.map(local)
    }

    /// Map one source-domain time back into this Clip's Sequence placement.
    ///
    /// This is used by source-handle admission to intersect an authored
    /// Transition range with real media or nested-Sequence extents. A zero-rate
    /// hold has no unique inverse and is rejected; callers handle it as a
    /// constant sample after checking whether `source_origin` exists.
    pub fn source_to_timeline_time(&self, source_time: TimelineTime) -> Result<TimelineTime> {
        let local = self.source_time_map.inverse(source_time)?;
        Ok(self.position.checked_add(local)?)
    }

    /// Fork placement-local audio edit identities for the right side of a razor.
    ///
    /// Processing definitions remain shared through their Scope IDs, while
    /// exact edit/scope-local coordinates advance by the split offset. This
    /// preserves authored automation without duplicating placement or retime.
    pub fn fork_audio_components_for_split(&mut self, split_offset: TimelineTime) -> Result<()> {
        if split_offset.is_negative() || split_offset > self.duration {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "split_audio_components".to_owned(),
                reason: "audio split offset is outside the Clip".to_owned(),
            });
        }
        for edit in &mut self.audio_components {
            edit.id = AudioComponentEditId::new();
            edit.local_time_in = edit.local_time_in.checked_add(split_offset)?;
            edit.processing.scope_in = edit.processing.scope_in.checked_add(split_offset)?;
        }
        Ok(())
    }

    /// Fork all placement-local identities for the right side of a razor edit.
    ///
    /// Processing scopes remain shared definitions, while Clip-owned effect,
    /// mask, and audio-edit identities become independently addressable.
    pub fn fork_placement_identities_for_split(
        &mut self,
        split_offset: TimelineTime,
    ) -> Result<()> {
        self.fork_visual_placement_identities();
        self.fork_audio_components_for_split(split_offset)
    }

    /// Fork the Clip and every visual placement-local identity for Copy or
    /// Sequence duplication. Audio aggregate identities are forked by the
    /// owning Sequence because their Processing Scopes live outside the Clip.
    pub fn fork_visual_placement_identities(&mut self) {
        self.id = ClipId::new();
        self.content.fork_author_identities();
        self.transform.fork_author_identities();
        for effect in &mut self.effects {
            effect.id = EffectId::new();
            effect.properties.fork_author_identities();
        }
        for mask in &mut self.masks {
            mask.id = MaskId::new();
            mask.properties.fork_author_identities();
        }
    }

    /// Shift edit/scope-local origins when the Clip's in edge moves.
    ///
    /// A positive delta trims authored time from the front; a negative delta
    /// restores previously trimmed time. Placement-only moves must not call
    /// this method.
    pub fn shift_audio_component_in(&mut self, delta: TimelineTime) -> Result<()> {
        for edit in &mut self.audio_components {
            let local_time_in = edit.local_time_in.checked_add(delta)?;
            let scope_in = edit.processing.scope_in.checked_add(delta)?;
            if local_time_in.is_negative() || scope_in.is_negative() {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "trim_audio_components".to_owned(),
                    reason: "audio trim would move before authored local time zero".to_owned(),
                });
            }
            edit.local_time_in = local_time_in;
            edit.processing.scope_in = scope_in;
        }
        Ok(())
    }

    /// Add a definition-bound effect node.
    ///
    /// Product authoring must construct the node from its registered definition
    /// (see `instantiate_effect_node()` in `mondrian-effects`) before
    /// crossing this Timeline-owned insertion boundary. Timeline deliberately
    /// cannot synthesize effect parameters because it does not own or depend on
    /// the executable effect registry.
    pub fn add_effect_node(&mut self, effect: EffectNode) -> EffectId {
        self.insert_effect_node_at(self.effects.len(), effect)
    }

    /// Insert a definition-bound effect node at the given index.
    pub fn insert_effect_node_at(&mut self, index: usize, mut effect: EffectNode) -> EffectId {
        let label = self.next_effect_group_label(&effect.effect_type);
        effect.instantiate_for_clip(label);
        let effect_id = effect.id;
        let idx = index.min(self.effects.len());
        self.effects.insert(idx, effect);
        effect_id
    }

    pub fn effect_enabled(&self, effect_id: EffectId) -> Option<bool> {
        self.effects
            .iter()
            .find(|effect| effect.id == effect_id)
            .map(|effect| effect.is_enabled)
    }

    /// Resolve the current UI/command address for one effect parameter.
    ///
    /// Execution identity is the `(EffectId, ParameterId)` pair; the returned
    /// string is only an Adapter alias used by the current mutation interface.
    pub fn effect_parameter_address(
        &self,
        effect_id: EffectId,
        parameter_id: &ParameterId,
    ) -> Option<String> {
        self.effects
            .iter()
            .find(|effect| effect.id == effect_id)?
            .properties
            .iter()
            .find(|(_, property)| property.descriptor.parameter_id() == parameter_id)
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

    /// Return whether one stable-identity relative move would change the chain.
    pub fn effect_relative_placement_would_change(
        &self,
        effect_id: EffectId,
        placement: EffectRelativePlacement,
    ) -> Result<bool> {
        let source = self.effect_index(effect_id)?;
        let anchor_id = match placement {
            EffectRelativePlacement::Before(anchor_id)
            | EffectRelativePlacement::After(anchor_id) => anchor_id,
        };
        if anchor_id == effect_id {
            return Err(effect_order_error(
                "an Effect cannot be ordered relative to itself",
            ));
        }
        let anchor = self.effect_index(anchor_id)?;
        Ok(match placement {
            EffectRelativePlacement::Before(_) => source.checked_add(1) != Some(anchor),
            EffectRelativePlacement::After(_) => anchor.checked_add(1) != Some(source),
        })
    }

    /// Move one Effect immediately before or after another stable instance.
    ///
    /// Returns `false` without mutation when the requested relation already
    /// holds. Missing or self-referential identities fail closed.
    pub fn reorder_effect_relative(
        &mut self,
        effect_id: EffectId,
        placement: EffectRelativePlacement,
    ) -> Result<bool> {
        if !self.effect_relative_placement_would_change(effect_id, placement)? {
            return Ok(false);
        }
        let source = self.effect_index(effect_id)?;
        let anchor_id = match placement {
            EffectRelativePlacement::Before(anchor_id)
            | EffectRelativePlacement::After(anchor_id) => anchor_id,
        };
        let effect = self.effects.remove(source);
        let anchor = self.effect_index(anchor_id)?;
        let target = match placement {
            EffectRelativePlacement::Before(_) => anchor,
            EffectRelativePlacement::After(_) => anchor.checked_add(1).ok_or_else(|| {
                effect_order_error("Effect insertion index overflowed the author chain")
            })?,
        };
        self.effects.insert(target, effect);
        Ok(true)
    }

    fn effect_index(&self, effect_id: EffectId) -> Result<usize> {
        self.effects
            .iter()
            .position(|effect| effect.id == effect_id)
            .ok_or_else(|| effect_order_error(format!("Effect does not exist: {effect_id}")))
    }

    fn next_effect_group_label(&self, effect_type: &EffectType) -> String {
        effect_type.display_name().to_string()
    }
}

fn effect_order_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "clip_effect_order".to_owned(),
        reason: reason.into(),
    }
}

impl PropertyHost for Clip {
    fn property_bag(&self) -> Result<PropertyBag> {
        let mut properties = if self.is_adjustment_layer() || self.is_nested_sequence() {
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
        // Aggregate mask properties with "mask.<uuid>." prefix.
        for mask in &self.masks {
            let mask_component = mask.clone();
            for (short_path, property) in mask_component.properties.iter() {
                let mut prop = property.clone();
                prop.descriptor.path = format!("mask.{}.{}", mask.id.0, short_path);
                properties.upsert(prop);
            }
        }
        let blend_mode_text = blend_mode_to_text(self.blend_mode);
        let blend_mode_descriptor = PropertyDescriptor::new(
            Self::BLEND_MODE_PATH,
            "混合模式",
            PropertyValue::Enum(blend_mode_text.clone()),
        )
        .with_parameter_id(ParameterId::new_static("mondrian.clip.blend_mode"))
        .with_enum_options(blend_mode_options())
        .with_animatable(false);
        let mut blend_mode_property = AnimatedProperty::from_descriptor(blend_mode_descriptor);
        blend_mode_property.set_static_value(PropertyValue::Enum(blend_mode_text))?;
        properties.upsert(blend_mode_property);
        if let Some(solid_color) = self.content.solid_color() {
            let solid_color_descriptor = PropertyDescriptor::new(
                Self::SOLID_COLOR_PATH,
                "纯色",
                PropertyValue::Color(solid_color),
            )
            .with_parameter_id(ParameterId::new_static("mondrian.clip.solid_color"))
            .with_animatable(false);
            let mut solid_color_property =
                AnimatedProperty::from_descriptor(solid_color_descriptor);
            solid_color_property.set_static_value(PropertyValue::Color(solid_color))?;
            properties.upsert(solid_color_property);
        }
        if let Some(title) = self.content.basic_title() {
            for (_, property) in title.property_bag().iter() {
                properties.upsert(property.clone());
            }
        }
        Ok(properties)
    }

    fn apply_property_mutation(&mut self, mutation: PropertyMutation) -> Result<()> {
        let path = property_mutation_path(&mutation);
        if path == Transform2D::OPACITY_PATH
            || (!self.is_adjustment_layer()
                && !self.is_nested_sequence()
                && path.starts_with("transform."))
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
            let PropertyValue::Enum(value) = property.static_value() else {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "clip_apply_property_mutation".to_string(),
                    reason: "clip.blend_mode 需要 enum 值".to_string(),
                });
            };
            self.blend_mode = blend_mode_from_text(value)?;
            Ok(())
        } else if path == Self::SOLID_COLOR_PATH {
            if matches!(mutation, PropertyMutation::RemoveProperty { .. }) {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "clip_apply_property_mutation".to_string(),
                    reason: "内建 clip.solid_color 属性不可移除".to_string(),
                });
            }

            let PropertyMutation::SetStaticValue { value, .. } = mutation else {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "clip_apply_property_mutation".to_string(),
                    reason: "clip.solid_color 只支持静态颜色值".to_string(),
                });
            };
            let PropertyValue::Color(color) = value else {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "clip_apply_property_mutation".to_string(),
                    reason: "clip.solid_color 需要 color 值".to_string(),
                });
            };
            let ClipContent::SolidColor { color: current, .. } = &mut self.content else {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "clip_apply_property_mutation".to_string(),
                    reason: "clip.solid_color 只适用于纯色内容".to_string(),
                });
            };
            *current = color;
            Ok(())
        } else if path.starts_with("title.") {
            let title = self.content.basic_title_mut().ok_or_else(|| {
                MondrianError::WorkflowStepFailed {
                    step_id: "clip_apply_property_mutation".to_owned(),
                    reason: "title.* properties only apply to Basic Title content".to_owned(),
                }
            })?;
            title.apply_property_mutation(mutation)
        } else if path.starts_with("mask.") {
            // Path format: "mask.<uuid>.<short_prop>"
            let parts: Vec<&str> = path.splitn(3, '.').collect();
            if parts.len() == 3 {
                let mask_uuid_prefix = parts[1];
                let short_prop = parts[2];
                let prefix = format!("mask.{}.", mask_uuid_prefix);
                if let Some(mask) =
                    self.masks.iter_mut().find(|m| m.id.0.to_string().starts_with(mask_uuid_prefix))
                {
                    // Shape is stored in shape_keyframes, not PropertyBag.
                    if short_prop == mondrian_core::mask_data::MASK_PROP_SHAPE {
                        match &mutation {
                            PropertyMutation::ClearAnimation { time: _, .. } => {
                                // Keep only the first shape keyframe.
                                if let Some(first) = mask.shape_keyframes.first().cloned() {
                                    mask.shape_keyframes.clear();
                                    mask.shape_keyframes.push(first);
                                }
                                return Ok(());
                            }
                            _ => {
                                // Other shape mutations (enable, disable) are handled
                                // directly via set_mask_keyframe in the UI.
                                return Ok(());
                            }
                        }
                    }
                    let short_mutation = mutation
                        .map_path(|full| full.strip_prefix(&prefix).unwrap_or(&full).to_string());
                    mask.properties.apply_mutation(short_mutation)
                } else {
                    Err(MondrianError::WorkflowStepFailed {
                        step_id: "clip_apply_property_mutation".to_string(),
                        reason: format!("蒙版未找到: {mask_uuid_prefix}"),
                    })
                }
            } else {
                Err(MondrianError::WorkflowStepFailed {
                    step_id: "clip_apply_property_mutation".to_string(),
                    reason: format!("无效的蒙版属性路径: {path}"),
                })
            }
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
    /// Stable Clip-local visual author time used by every Clip processor.
    pub clip_time: TimelineTime,
    /// 此时刻对应的素材源时间（用于解码）
    pub source_time: TimelineTime,
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
    use mondrian_core::automation::{Keyframe, PropertyMutation, PropertyValue};
    use mondrian_core::mask_data::MaskKeyframe;
    use mondrian_effects::EffectRenderOp;

    fn tt(frame: i64) -> TimelineTime {
        TimelineTime::new(frame, 25).expect("valid test time")
    }

    #[test]
    fn still_image_is_an_extensible_zero_rate_media_hold() {
        let asset_id = AssetId::new();
        let clip = Clip::new_still_image(asset_id, tt(10), tt(125)).expect("still Clip");

        assert_eq!(clip.media_asset_id(), Some(asset_id));
        assert_eq!(clip.source_time_scale().numerator(), 0);
        assert_eq!(clip.source_origin(), TimelineTime::ZERO);
        assert_eq!(
            clip.source_terminal_boundary().expect("source terminal"),
            TimelineTime::ZERO
        );
        assert_eq!(
            clip.timeline_to_source_time(tt(10)).expect("start sample"),
            TimelineTime::ZERO
        );
        assert_eq!(
            clip.timeline_to_source_time(tt(134)).expect("last visible sample"),
            TimelineTime::ZERO
        );
    }

    #[test]
    fn source_time_map_is_the_only_persisted_source_sampling_authority() {
        let mut clip = Clip::new(AssetId::new(), tt(0), tt(20)).expect("valid Clip");
        clip.set_constant_source_time_map(tt(100), TimeScale::new(-3, 2).expect("reverse scale"))
            .expect("set source map");

        let value = serde_json::to_value(&clip).expect("serialize Clip");
        let object = value.as_object().expect("Clip object");
        assert!(object.contains_key("source_time_map"));
        assert!(!object.contains_key("source_in"));
        assert!(!object.contains_key("source_out"));
        assert!(!object.contains_key("speed"));
        assert_eq!(
            clip.source_terminal_boundary().expect("terminal boundary"),
            tt(70)
        );

        let reopened: Clip = serde_json::from_value(value).expect("reopen Clip");
        assert_eq!(reopened.source_time_map(), clip.source_time_map());
    }

    #[test]
    fn source_map_overflow_is_rejected_without_partial_mutation() {
        let mut clip =
            Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE).expect("valid Clip");
        let before = clip.source_time_map().clone();

        assert!(clip
            .set_source_origin(TimelineTime::new(i64::MAX, 1).expect("maximum exact time"))
            .is_err());
        assert_eq!(clip.source_time_map(), &before);
    }

    fn exposure_from_graph(effects: &[EffectNode], time: TimelineTime) -> f32 {
        // Build graph and extract ColorAdjust exposure from UnaryEffect nodes.
        let graph = mondrian_effects::build_effect_render_graph(
            effects,
            time,
            mondrian_core::WorkingColorSpace::LinearRec709,
        )
        .expect("build effect graph");
        graph
            .nodes
            .iter()
            .find_map(|node| match &node.kind {
                mondrian_effects::EffectGraphNodeKind::UnaryEffect {
                    op: EffectRenderOp::ColorAdjust { exposure, .. },
                    ..
                } => Some(*exposure),
                _ => None,
            })
            .unwrap_or(0.0)
    }

    #[test]
    fn clip_transform_and_exact_source_scale_remain_independent() {
        let mut clip = Clip::new(AssetId::new(), tt(0), tt(40)).expect("valid clip");
        clip.apply_property_mutation(PropertyMutation::SetKeyframe {
            path: Transform2D::POSITION_PATH.to_string(),
            keyframe: Keyframe::linear(tt(0), PropertyValue::Vec2(Vec2::ZERO)),
        })
        .expect("set start position");
        clip.apply_property_mutation(PropertyMutation::SetKeyframe {
            path: Transform2D::POSITION_PATH.to_string(),
            keyframe: Keyframe::linear(tt(20), PropertyValue::Vec2(Vec2::new(20.0, 10.0))),
        })
        .expect("set end position");
        clip.set_constant_source_time_map(
            TimelineTime::ZERO,
            TimeScale::new(3, 2).expect("valid exact scale"),
        )
        .expect("set source map");

        let position = clip
            .transform
            .to_property_bag()
            .evaluate(Transform2D::POSITION_PATH, tt(10))
            .and_then(|value| value.as_vec2())
            .expect("evaluate position");
        assert_eq!(position, Vec2::new(10.0, 5.0));
        assert_eq!(
            clip.source_time_map().map(tt(10)).expect("map time"),
            tt(15)
        );
    }

    #[test]
    fn clip_property_bag_exposes_blend_mode_as_static_property() {
        let clip = Clip::new(AssetId::new(), tt(0), tt(40)).expect("valid clip");
        let bag = clip.property_bag().expect("property bag should build");
        let property =
            bag.property(Clip::BLEND_MODE_PATH).expect("blend mode property should exist");

        assert!(!property.descriptor.schema.is_animatable);
        assert_eq!(
            property.evaluate(tt(0)),
            PropertyValue::Enum("inherit".to_string())
        );
    }

    #[test]
    fn clip_property_mutation_updates_blend_mode_without_keyframes() {
        let mut clip = Clip::new(AssetId::new(), tt(0), tt(40)).expect("valid clip");

        clip.apply_property_mutation(PropertyMutation::SetStaticValue {
            path: Clip::BLEND_MODE_PATH.to_string(),
            value: PropertyValue::Enum("Multiply".to_string()),
        })
        .expect("set blend mode");

        assert_eq!(clip.blend_mode, Some(BlendMode::Multiply));
    }

    #[test]
    fn clip_property_bag_exposes_solid_color_as_static_property() {
        let color = Color::from_rgba8(12, 34, 56, 200);
        let clip = Clip::new_solid_color(AssetId::new(), color, tt(0), tt(40)).expect("valid clip");

        let bag = clip.property_bag().expect("property bag should build");
        let property =
            bag.property(Clip::SOLID_COLOR_PATH).expect("solid color property should exist");

        assert!(!property.descriptor.schema.is_animatable);
        assert_eq!(property.evaluate(tt(0)), PropertyValue::Color(color));
    }

    #[test]
    fn clip_property_mutation_updates_solid_color_without_keyframes() {
        let mut clip =
            Clip::new_solid_color(AssetId::new(), Color::from_hex(0x112233), tt(0), tt(40))
                .expect("valid clip");
        let color = Color::from_rgba8(200, 120, 40, 180);

        clip.apply_property_mutation(PropertyMutation::SetStaticValue {
            path: Clip::SOLID_COLOR_PATH.to_string(),
            value: PropertyValue::Color(color),
        })
        .expect("set solid color");

        assert_eq!(clip.content.solid_color(), Some(color));
    }

    #[test]
    fn adjustment_layer_constructor_marks_clip_kind() {
        let clip = Clip::new_adjustment_layer(AssetId::new(), tt(12), tt(30)).expect("valid clip");

        assert!(clip.is_adjustment_layer());
        assert_eq!(clip.kind(), ClipKind::AdjustmentLayer);
        assert_eq!(clip.position, tt(12));
        assert_eq!(clip.duration, tt(30));
        assert!(clip.effects.is_empty());
    }

    #[test]
    fn effect_relative_placement_uses_stable_identities_and_exact_adjacency() {
        let mut clip = Clip::new(AssetId::new(), tt(0), tt(30)).expect("valid Clip");
        let first = clip.add_effect_node(mondrian_effects::EffectNodeExt::with_defaults(
            EffectType::GaussianBlur,
        ));
        let second = clip.add_effect_node(mondrian_effects::EffectNodeExt::with_defaults(
            EffectType::Sharpen,
        ));
        let third = clip.add_effect_node(mondrian_effects::EffectNodeExt::with_defaults(
            EffectType::BasicCorrection,
        ));

        assert!(!clip
            .effect_relative_placement_would_change(first, EffectRelativePlacement::Before(second),)
            .expect("valid adjacency"));
        assert!(clip
            .reorder_effect_relative(first, EffectRelativePlacement::After(third))
            .expect("stable relative move"));
        assert_eq!(
            clip.effects.iter().map(|effect| effect.id).collect::<Vec<_>>(),
            vec![second, third, first]
        );
        assert!(!clip
            .reorder_effect_relative(first, EffectRelativePlacement::After(third))
            .expect("already adjacent"));
    }

    #[test]
    fn effect_relative_placement_rejects_stale_and_self_references_without_mutation() {
        let mut clip = Clip::new(AssetId::new(), tt(0), tt(30)).expect("valid Clip");
        let effect_id = clip.add_effect_node(mondrian_effects::EffectNodeExt::with_defaults(
            EffectType::GaussianBlur,
        ));
        let before = clip.effects.clone();

        assert!(clip
            .reorder_effect_relative(effect_id, EffectRelativePlacement::Before(EffectId::new()),)
            .is_err());
        assert!(clip
            .reorder_effect_relative(effect_id, EffectRelativePlacement::After(effect_id),)
            .is_err());
        assert_eq!(clip.effects, before);
    }

    #[test]
    fn nested_sequence_constructor_marks_clip_kind() {
        let nested_id = SequenceId::new();
        let clip =
            Clip::new_nested_sequence(nested_id, tt(12), tt(30), Some("Scene 02".to_string()))
                .expect("valid clip");

        assert!(clip.is_nested_sequence());
        assert_eq!(clip.kind(), ClipKind::NestedSequence);
        assert_eq!(clip.nested_sequence_id(), Some(nested_id));
        assert_eq!(clip.label.as_deref(), Some("Scene 02"));
        assert!(clip
            .property_bag()
            .expect("property bag")
            .property(Transform2D::OPACITY_PATH)
            .is_some());
    }

    #[test]
    fn media_interpretation_defaults_to_source_metadata() {
        let clip = Clip::new(AssetId::new(), tt(0), tt(10)).expect("valid clip");
        let interpretation = clip.media_interpretation().expect("media interpretation");
        assert_eq!(interpretation.color_space_override, None);
        assert_eq!(interpretation.alpha, AlphaInterpretation::Straight);
    }

    #[test]
    fn clip_visual_time_is_independent_of_placement_and_source_selection() {
        let mut clip = Clip::new(AssetId::new(), tt(10), tt(20)).expect("valid clip");
        clip.clip_time_in = tt(3);
        clip.set_constant_source_time_map(tt(100), TimeScale::new(2, 1).expect("2x speed"))
            .expect("set source map");

        assert_eq!(
            clip.timeline_to_clip_time(tt(12)).expect("Clip time"),
            tt(5)
        );
        assert_eq!(
            clip.timeline_to_source_time(tt(12)).expect("source time"),
            tt(104)
        );
        assert_eq!(
            clip.clip_to_timeline_time(tt(5)).expect("Sequence time"),
            tt(12)
        );

        clip.position = tt(50);
        clip.set_source_origin(tt(200)).expect("slip source");
        assert_eq!(
            clip.timeline_to_clip_time(tt(52)).expect("moved Clip time"),
            tt(5)
        );
        assert_eq!(
            clip.timeline_to_source_time(tt(52)).expect("slipped source time"),
            tt(204)
        );
    }

    #[test]
    fn media_interpretation_can_override_color_space() {
        let mut clip = Clip::new(AssetId::new(), tt(0), tt(10)).expect("valid clip");
        clip.media_interpretation_mut()
            .expect("media interpretation")
            .color_space_override = Some(ColorSpace::Rec2100Hlg);
        assert_eq!(
            clip.media_interpretation().expect("media interpretation").color_space_override,
            Some(ColorSpace::Rec2100Hlg)
        );
    }

    #[test]
    fn library_identity_does_not_imply_a_file_media_dependency() {
        let media_id = AssetId::new();
        let adjustment_id = AssetId::new();
        let solid_id = AssetId::new();
        let media = Clip::new(media_id, tt(0), tt(10)).expect("media Clip");
        let adjustment =
            Clip::new_adjustment_layer(adjustment_id, tt(0), tt(10)).expect("adjustment Clip");
        let solid = Clip::new_solid_color(solid_id, Color::from_hex(0x336699), tt(0), tt(10))
            .expect("solid Clip");

        assert_eq!(media.library_asset_id(), Some(media_id));
        assert_eq!(media.media_asset_id(), Some(media_id));
        assert_eq!(adjustment.library_asset_id(), Some(adjustment_id));
        assert_eq!(adjustment.media_asset_id(), None);
        assert_eq!(solid.library_asset_id(), Some(solid_id));
        assert_eq!(solid.media_asset_id(), None);
    }

    #[test]
    fn clip_content_json_rejects_cross_variant_and_legacy_fields() {
        let clip = Clip::new(AssetId::new(), tt(0), tt(10)).expect("valid clip");
        let mut content = serde_json::to_value(&clip.content).expect("serialize content");
        content.as_object_mut().expect("content object").insert(
            "sequence_id".to_owned(),
            serde_json::json!(SequenceId::new()),
        );
        assert!(serde_json::from_value::<ClipContent>(content).is_err());

        let mut serialized = serde_json::to_value(&clip).expect("serialize Clip");
        serialized
            .as_object_mut()
            .expect("Clip object")
            .insert("asset_id".to_owned(), serde_json::json!(AssetId::new()));
        assert!(serde_json::from_value::<Clip>(serialized).is_err());

        let mut missing_clip_time =
            serde_json::to_value(&clip).expect("serialize current-schema Clip");
        missing_clip_time.as_object_mut().expect("Clip object").remove("clip_time_in");
        assert!(serde_json::from_value::<Clip>(missing_clip_time).is_err());
    }

    #[test]
    fn copied_visual_placement_forks_all_addressable_identities() {
        let mut original = Clip::new(AssetId::new(), tt(0), tt(10)).expect("valid clip");
        original.add_effect_node(mondrian_effects::EffectNodeExt::with_defaults(
            EffectType::BasicCorrection,
        ));
        original.masks.push(MaskComponent::new(
            "Mask".to_owned(),
            MaskKeyframe::default(),
        ));
        let mut copied = original.clone();

        copied.fork_visual_placement_identities();

        assert_ne!(copied.id, original.id);
        assert_ne!(copied.effects[0].id, original.effects[0].id);
        assert_ne!(copied.masks[0].id, original.masks[0].id);
        assert_ne!(
            copied
                .transform
                .to_property_bag()
                .property(Transform2D::OPACITY_PATH)
                .expect("copied opacity")
                .track_id,
            original
                .transform
                .to_property_bag()
                .property(Transform2D::OPACITY_PATH)
                .expect("original opacity")
                .track_id,
        );
    }

    #[test]
    fn adjustment_layer_instances_from_same_asset_do_not_share_state() {
        let shared_asset_id = AssetId::new();
        let mut first =
            Clip::new_adjustment_layer(shared_asset_id, tt(0), tt(30)).expect("valid clip");
        let mut second =
            Clip::new_adjustment_layer(shared_asset_id, tt(40), tt(30)).expect("valid clip");
        let first_effect_id = first.add_effect_node(
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::BasicCorrection),
        );
        second.add_effect_node(mondrian_effects::EffectNodeExt::with_defaults(
            EffectType::BasicCorrection,
        ));
        let exposure_id = EffectType::BasicCorrection
            .parameter_id("exposure")
            .expect("exposure parameter ID");
        let exposure_path = first
            .effect_parameter_address(first_effect_id, &exposure_id)
            .expect("adjustment exposure path");

        first
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: exposure_path,
                value: PropertyValue::Float(1.25),
            })
            .expect("set first exposure");

        let first_exposure = exposure_from_graph(&first.effects, tt(10));
        let second_exposure = exposure_from_graph(&second.effects, tt(50));

        assert!((first_exposure - 1.25).abs() < 1.0e-4);
        assert!(second_exposure.abs() < 1.0e-4);
    }

    // ── Anchor matrix tests ───────────────────────────────────────

    #[test]
    fn anchor_zero_is_backward_compatible() {
        let mut t = Transform2D::identity();
        t.set_position(glam::Vec2::new(100.0, 200.0));
        let m = t.evaluate_matrix(tt(0));
        assert!((m.col(2).x - 100.0).abs() < 0.01, "tx={}", m.col(2).x);
        assert!((m.col(2).y - 200.0).abs() < 0.01, "ty={}", m.col(2).y);
    }

    #[test]
    fn anchor_center_pos_center_with_autofit_cancels() {
        let mut t = Transform2D::identity();
        t.set_position(glam::Vec2::new(960.0, 540.0));
        t.set_scale(glam::Vec2::new(0.5, 0.5));
        t.properties
            .set_static_value(
                Transform2D::ANCHOR_POINT_PATH,
                PropertyValue::Vec2(glam::Vec2::new(1920.0, 1080.0)),
            )
            .unwrap();
        let m = t.evaluate_matrix(tt(0));
        assert!((m.col(2).x - 0.0).abs() < 0.01, "tx={}", m.col(2).x);
        assert!((m.col(2).y - 0.0).abs() < 0.01, "ty={}", m.col(2).y);
        assert!((m.col(0).x - 0.5).abs() < 0.01);
    }

    #[test]
    fn position_change_with_nonzero_anchor() {
        let mut t = Transform2D::identity();
        t.set_scale(glam::Vec2::new(0.5, 0.5));
        t.properties
            .set_static_value(
                Transform2D::ANCHOR_POINT_PATH,
                PropertyValue::Vec2(glam::Vec2::new(1920.0, 1080.0)),
            )
            .unwrap();
        t.set_position(glam::Vec2::new(1060.0, 640.0));
        let m = t.evaluate_matrix(tt(0));
        assert!((m.col(2).x - 100.0).abs() < 0.01, "tx={}", m.col(2).x);
        assert!((m.col(2).y - 100.0).abs() < 0.01, "ty={}", m.col(2).y);
    }

    #[test]
    fn razor_forks_edit_identity_but_preserves_processing_scope_and_time() {
        let scope_id = AudioProcessingScopeId::new();
        let mut left = Clip::new(AssetId::new(), TimelineTime::ZERO, tt(100)).expect("audio clip");
        left.audio_components.push(AudioComponentEdit::media(
            AudioSourceComponentId::primary(),
            scope_id,
        ));
        let original_edit_id = left.audio_components[0].id;

        let split_offset = tt(40);
        let mut right = left.clone();
        right.fork_audio_components_for_split(split_offset).expect("fork authoring");

        assert_ne!(right.audio_components[0].id, original_edit_id);
        assert_eq!(right.audio_components[0].processing.scope_id, scope_id);
        assert_eq!(right.audio_components[0].local_time_in, split_offset);
        assert_eq!(right.audio_components[0].processing.scope_in, split_offset);
        assert_eq!(left.audio_components[0].local_time_in, TimelineTime::ZERO);
        assert_eq!(
            left.audio_components[0].processing.scope_in,
            TimelineTime::ZERO
        );
    }
}
