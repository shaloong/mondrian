//! Mask data types — pure geometric data for clip masks.
//!
//! These are pure data types with no rendering logic. Mask rasterization lives
//! in `mondrian-effects::mask_raster`.

use crate::automation::{PropertyBag, PropertyValue};
use crate::types::{KeyframeId, MaskId, TrackingId};
use crate::{AuthoringList, MediaFileFingerprint, TimelineTime};
use glam::Vec2;
use serde::{Deserialize, Serialize};

/// Maximum authored control points in one Mask path.
///
/// The limit bounds deterministic flattening, spatial-index construction, and
/// raster metadata for one immutable Effect program. More complex mattes must
/// be expressed as multiple typed Masks rather than one unbounded path.
pub const MAX_MASK_PATH_POINTS: usize = 4_096;

/// Bezier path control point.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BezierPoint {
    pub position: Vec2,
    pub control_in: Vec2,
    pub control_out: Vec2,
}

impl BezierPoint {
    pub fn new(pos: Vec2) -> Self {
        Self {
            position: pos,
            control_in: Vec2::ZERO,
            control_out: Vec2::ZERO,
        }
    }
}

/// Mask shape type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MaskShape {
    Rectangle {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        corner_radius: f32,
    },
    Ellipse {
        center: Vec2,
        radii: Vec2,
    },
    Path {
        points: Vec<BezierPoint>,
        closed: bool,
    },
}

impl Default for MaskShape {
    fn default() -> Self {
        Self::Rectangle {
            x: 0.1,
            y: 0.1,
            width: 0.8,
            height: 0.8,
            corner_radius: 0.0,
        }
    }
}

/// Interpolation applied from one Mask shape key to its successor.
///
/// Linear interpolation is valid only when both shapes have compatible
/// topology. A deliberate topology change must use Hold instead of relying on
/// an implicit midpoint snap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MaskShapeInterpolation {
    /// Retain the current shape until the next key.
    #[default]
    Hold,
    /// Interpolate every compatible geometric degree of freedom.
    Linear,
}

/// Motion model used to generate one Mask shape track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MaskTrackingModel {
    /// Robust 2-D target translation. Geometry type and size are preserved.
    #[default]
    ObjectTranslation,
    /// Eight-degree-of-freedom projective plane transform.
    PlanarHomography,
}

/// Which side of the anchor frame one tracking request analyzes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MaskTrackingDirection {
    /// Analyze from the anchor toward the Clip out-point.
    #[default]
    Forward,
    /// Analyze from the anchor toward the Clip in-point.
    Backward,
    /// Analyze both sides and publish one ordered shape track.
    Both,
}

/// Persisted, bounded settings required to deterministically recompute a track.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskTrackingSettings {
    /// Longest decoded analysis-raster edge.
    pub analysis_max_dimension: u32,
    /// Maximum retained spatial features per frame pair.
    pub max_features: u16,
    /// Integer search radius in analysis pixels.
    pub search_radius: u16,
    /// Patch radius in analysis pixels.
    pub patch_radius: u8,
    /// Minimum accepted inlier ratio.
    pub minimum_inlier_ratio: f32,
}

impl Default for MaskTrackingSettings {
    fn default() -> Self {
        Self {
            analysis_max_dimension: 960,
            max_features: 192,
            search_radius: 24,
            patch_radius: 4,
            minimum_inlier_ratio: 0.45,
        }
    }
}

impl MaskTrackingSettings {
    /// Validate persisted resource bounds and quality thresholds.
    pub fn validate(self) -> crate::Result<()> {
        if !(160..=2_048).contains(&self.analysis_max_dimension) {
            return Err(tracking_validation_error(
                "analysis_max_dimension must be within 160..=2048",
            ));
        }
        if !(16..=1_024).contains(&self.max_features) {
            return Err(tracking_validation_error(
                "max_features must be within 16..=1024",
            ));
        }
        if !(2..=128).contains(&self.search_radius) {
            return Err(tracking_validation_error(
                "search_radius must be within 2..=128",
            ));
        }
        if !(2..=12).contains(&self.patch_radius) {
            return Err(tracking_validation_error(
                "patch_radius must be within 2..=12",
            ));
        }
        if !self.minimum_inlier_ratio.is_finite()
            || !(0.1..=1.0).contains(&self.minimum_inlier_ratio)
        {
            return Err(tracking_validation_error(
                "minimum_inlier_ratio must be finite and within 0.1..=1.0",
            ));
        }
        // Bound the cross-product, not only each knob. An individually valid
        // feature/search/patch combination can otherwise create billions of
        // normalized-correlation sample visits for every adjacent frame pair.
        const MAX_PATCH_SAMPLE_VISITS_PER_PAIR: u64 = 128_000_000;
        let search_width = u64::from(self.search_radius) * 2 + 1;
        let search_candidates = search_width * search_width + 25;
        let patch_width = u64::from(self.patch_radius) + 1;
        let sample_visits =
            u64::from(self.max_features) * search_candidates * patch_width * patch_width * 2;
        if sample_visits > MAX_PATCH_SAMPLE_VISITS_PER_PAIR {
            return Err(tracking_validation_error(format!(
                "feature/search/patch combination exceeds the per-frame-pair analysis budget ({sample_visits}>{MAX_PATCH_SAMPLE_VISITS_PER_PAIR})"
            )));
        }
        Ok(())
    }
}

/// Recomputable author intent and source evidence for generated Mask keys.
///
/// Decoded frames, image pyramids, feature tracks, and result caches remain
/// runtime artifacts. Only this bounded recipe and the generated shape keys
/// enter the Project document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskTrackingRecipe {
    /// Stable identity of this analysis lineage.
    pub id: TrackingId,
    /// Requested motion model.
    pub model: MaskTrackingModel,
    /// Requested traversal direction.
    pub direction: MaskTrackingDirection,
    /// Exact Clip-local anchor time.
    pub anchor_time: TimelineTime,
    /// Inclusive Clip-local generated-key range start.
    pub range_start: TimelineTime,
    /// Inclusive Clip-local generated-key range end.
    pub range_end: TimelineTime,
    /// Bounded deterministic analysis settings.
    pub settings: MaskTrackingSettings,
    /// Source revision analyzed by the completed result.
    pub source_fingerprint: MediaFileFingerprint,
    /// Exact physical video stream analyzed by the completed result.
    pub video_stream_index: u32,
    /// Mean accepted inlier ratio across generated frame pairs.
    pub mean_inlier_ratio: f32,
    /// Worst accepted inlier ratio across generated frame pairs.
    pub minimum_observed_inlier_ratio: f32,
}

impl MaskTrackingRecipe {
    /// Validate ordering, resource bounds, source evidence, and quality evidence.
    pub fn validate(&self) -> crate::Result<()> {
        if self.range_start > self.anchor_time || self.anchor_time > self.range_end {
            return Err(tracking_validation_error(
                "tracking range must contain the anchor time",
            ));
        }
        self.settings.validate()?;
        if !self.source_fingerprint.authorizes_reuse() {
            return Err(tracking_validation_error(
                "tracking recipe requires a complete source fingerprint",
            ));
        }
        for (name, value) in [
            ("mean_inlier_ratio", self.mean_inlier_ratio),
            (
                "minimum_observed_inlier_ratio",
                self.minimum_observed_inlier_ratio,
            ),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(tracking_validation_error(format!(
                    "{name} must be finite and normalized"
                )));
            }
        }
        Ok(())
    }
}

/// One stable Clip-local Mask shape key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskShapeKeyframe {
    /// Stable author identity used by selection and future curve editing.
    pub id: KeyframeId,
    /// Exact Clip-local author time.
    pub time: TimelineTime,
    /// Complete shape value at this key.
    pub shape: MaskShape,
    /// Interpolation from this key to its successor.
    pub interpolation: MaskShapeInterpolation,
}

impl MaskShapeKeyframe {
    /// Create one shape key with a fresh stable identity.
    pub fn new(
        time: TimelineTime,
        shape: MaskShape,
        interpolation: MaskShapeInterpolation,
    ) -> Self {
        Self { id: KeyframeId::new(), time, shape, interpolation }
    }
}

/// Mask boolean operation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum MaskOp {
    #[default]
    Add,
    Subtract,
    Intersect,
    Difference,
}

impl MaskOp {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Add => "Add",
            Self::Subtract => "Subtract",
            Self::Intersect => "Intersect",
            Self::Difference => "Difference",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "Subtract" => Self::Subtract,
            "Intersect" => Self::Intersect,
            "Difference" => Self::Difference,
            _ => Self::Add,
        }
    }
}

fn default_one() -> f32 {
    1.0
}

/// Complete Mask value evaluated at one Clip-local time.
///
/// This is not an author key: shape and scalar parameters retain independent
/// key identities and interpolation contracts in [`MaskComponent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaskEvaluation {
    /// Evaluated geometry.
    pub shape: MaskShape,
    /// Evaluated edge feather in pixels.
    #[serde(default)]
    pub feather: f32,
    /// Evaluated normalized opacity.
    #[serde(default = "default_one")]
    pub opacity: f32,
    /// Evaluated edge expansion in pixels.
    #[serde(default)]
    pub expansion: f32,
    /// Whether coverage is inverted after geometry evaluation.
    #[serde(default)]
    pub invert: bool,
    /// Evaluated stack-combination operation.
    #[serde(default)]
    pub mask_op: MaskOp,
}

impl Default for MaskEvaluation {
    fn default() -> Self {
        Self {
            shape: MaskShape::default(),
            feather: 0.0,
            opacity: 1.0,
            expansion: 0.0,
            invert: false,
            mask_op: MaskOp::Add,
        }
    }
}

// Property paths for mask scalar properties (used in PropertyBag).
pub const MASK_PROP_FEATHER: &str = "feather";
pub const MASK_PROP_OPACITY: &str = "opacity";
pub const MASK_PROP_EXPANSION: &str = "expansion";
pub const MASK_PROP_INVERT: &str = "invert";
pub const MASK_PROP_MASK_OP: &str = "mask_op";
pub const MASK_PROP_SHAPE: &str = "shape";

/// A single mask component on a clip.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskComponent {
    pub id: MaskId,
    pub name: String,
    /// Shape keyframes. When `shape_animation_enabled` is false, only the first
    /// entry is used (static shape). When enabled, shapes are interpolated by time.
    pub shape_keyframes: AuthoringList<MaskShapeKeyframe>,
    /// Scalar animatable properties (feather, opacity, expansion, invert, mask_op).
    pub properties: PropertyBag,
    pub enabled: bool,
    pub locked: bool,
    pub shape_animation_enabled: bool,
    /// Last completed recomputable tracking lineage, when shape keys were
    /// generated by the tracking product path.
    #[serde(default)]
    pub tracking: Option<MaskTrackingRecipe>,
}

impl PartialEq for MaskComponent {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.name == other.name
            && self.shape_keyframes == other.shape_keyframes
            && self.properties == other.properties
            && self.enabled == other.enabled
            && self.locked == other.locked
            && self.shape_animation_enabled == other.shape_animation_enabled
            && self.tracking == other.tracking
    }
}

impl MaskComponent {
    pub fn new(name: String, initial: MaskEvaluation) -> Self {
        let id = MaskId::new();
        let mut properties = PropertyBag::default();
        use crate::automation::{
            AnimatablePropertyUiMetadata, ParameterEnumOption, ParameterInvalidValuePolicy,
            ParameterNumericContract, ParameterNumericRange, ParameterUnit, PropertyDescriptor,
        };

        let define_prop = |bag: &mut PropertyBag,
                           parameter_id: &'static str,
                           path: &str,
                           display: &str,
                           value: PropertyValue| {
            let mut desc = PropertyDescriptor::new(path, display, value)
                .with_parameter_id(crate::ParameterId::new_static(parameter_id));
            desc = match parameter_id {
                "mondrian.mask.feather" | "mondrian.mask.expansion" => {
                    desc.with_unit(ParameterUnit::Pixels)
                }
                "mondrian.mask.opacity" => {
                    let normalized = ParameterNumericRange { min: 0.0, max: 1.0 };
                    desc.with_numeric_contract(
                        ParameterUnit::Normalized,
                        ParameterNumericContract {
                            hard_range: normalized,
                            soft_range: normalized,
                            step: Some(0.01),
                            invalid_value_policy: ParameterInvalidValuePolicy::Reject,
                        },
                    )
                }
                "mondrian.mask.operation" => desc.with_enum_options(
                    ["Add", "Subtract", "Intersect", "Difference"]
                        .into_iter()
                        .map(|key| {
                            ParameterEnumOption::new(
                                key,
                                format!("mondrian.mask.operation.{key}.label"),
                            )
                        })
                        .collect(),
                ),
                _ => desc,
            };
            desc.ui_metadata = AnimatablePropertyUiMetadata {
                group_name: Some(name.clone()),
                ..Default::default()
            };
            bag.define(desc);
        };

        define_prop(
            &mut properties,
            "mondrian.mask.feather",
            MASK_PROP_FEATHER,
            "羽化",
            PropertyValue::Float(initial.feather),
        );
        define_prop(
            &mut properties,
            "mondrian.mask.opacity",
            MASK_PROP_OPACITY,
            "不透明度",
            PropertyValue::Float(initial.opacity),
        );
        define_prop(
            &mut properties,
            "mondrian.mask.expansion",
            MASK_PROP_EXPANSION,
            "扩展",
            PropertyValue::Float(initial.expansion),
        );
        define_prop(
            &mut properties,
            "mondrian.mask.invert",
            MASK_PROP_INVERT,
            "反转",
            PropertyValue::Bool(initial.invert),
        );
        define_prop(
            &mut properties,
            "mondrian.mask.operation",
            MASK_PROP_MASK_OP,
            "模式",
            PropertyValue::Enum(initial.mask_op.as_str().to_string()),
        );

        Self {
            id,
            name,
            shape_keyframes: vec![MaskShapeKeyframe::new(
                TimelineTime::ZERO,
                initial.shape,
                MaskShapeInterpolation::Hold,
            )]
            .into(),
            properties,
            enabled: true,
            locked: false,
            shape_animation_enabled: false,
            tracking: None,
        }
    }

    pub fn shape_keyframes(&self) -> &[MaskShapeKeyframe] {
        &self.shape_keyframes
    }

    pub fn shape_keyframes_mut(&mut self) -> &mut Vec<MaskShapeKeyframe> {
        &mut self.shape_keyframes
    }

    /// Validate the complete persisted mask author contract.
    pub fn validate_author_state(&self) -> crate::Result<()> {
        if self.shape_keyframes.is_empty() {
            return Err(mask_validation_error(
                self.id,
                "shape keyframes cannot be empty",
            ));
        }
        if !self.shape_animation_enabled && self.shape_keyframes.len() != 1 {
            return Err(mask_validation_error(
                self.id,
                "a static mask must contain exactly one shape keyframe",
            ));
        }
        let mut key_ids = std::collections::HashSet::with_capacity(self.shape_keyframes.len());
        for key in &self.shape_keyframes {
            if !key_ids.insert(key.id) {
                return Err(mask_validation_error(
                    self.id,
                    format!("duplicate shape key identity: {}", key.id),
                ));
            }
        }
        if !self.shape_animation_enabled && self.shape_keyframes[0].time != TimelineTime::ZERO {
            return Err(mask_validation_error(
                self.id,
                "a static mask shape key must use canonical Clip-local time zero",
            ));
        }
        for pair in self.shape_keyframes.windows(2) {
            if pair[0].time >= pair[1].time {
                return Err(mask_validation_error(
                    self.id,
                    "shape keyframe times must be strictly increasing",
                ));
            }
            if pair[0].interpolation == MaskShapeInterpolation::Linear
                && !shapes_have_compatible_topology(&pair[0].shape, &pair[1].shape)
            {
                return Err(mask_validation_error(
                    self.id,
                    "linear shape keys require compatible shape topology",
                ));
            }
        }
        for key in &self.shape_keyframes {
            validate_shape(self.id, &key.shape)?;
        }
        if let Some(recipe) = &self.tracking {
            recipe.validate()?;
            if !self.shape_animation_enabled {
                return Err(mask_validation_error(
                    self.id,
                    "a tracked Mask must keep shape animation enabled",
                ));
            }
        }

        self.properties.validate()?;
        for (path, parameter_id) in [
            (MASK_PROP_FEATHER, "mondrian.mask.feather"),
            (MASK_PROP_OPACITY, "mondrian.mask.opacity"),
            (MASK_PROP_EXPANSION, "mondrian.mask.expansion"),
            (MASK_PROP_INVERT, "mondrian.mask.invert"),
            (MASK_PROP_MASK_OP, "mondrian.mask.operation"),
        ] {
            let property = self.properties.property(path).ok_or_else(|| {
                mask_validation_error(self.id, format!("required property is missing: {path}"))
            })?;
            if property.descriptor.parameter_id() != &crate::ParameterId::new_static(parameter_id) {
                return Err(mask_validation_error(
                    self.id,
                    format!("property {path} has the wrong ParameterId"),
                ));
            }
        }
        Ok(())
    }

    pub fn current_shape(&self) -> Option<&MaskShape> {
        self.shape_keyframes.first().map(|key| &key.shape)
    }

    pub fn property_path(mask_id: MaskId, property: &str) -> String {
        format!("mask.{mask_id}.{property}")
    }

    /// Enable or disable shape animation without manufacturing a duplicate
    /// key at the current time.
    ///
    /// Disabling collapses the evaluated current shape to one canonical static
    /// key. An exact existing key retains its stable identity; otherwise the
    /// collapsed value receives a new identity.
    pub fn set_shape_animation_enabled(
        &mut self,
        enabled: bool,
        time: TimelineTime,
    ) -> Result<bool, crate::MondrianError> {
        if self.shape_animation_enabled == enabled {
            return Ok(false);
        }
        if enabled {
            self.shape_animation_enabled = true;
            return Ok(true);
        }

        let shape = self.evaluate_at(time).shape;
        let retained_id = self
            .shape_keyframes
            .iter()
            .find(|key| key.time == time)
            .map(|key| key.id)
            .unwrap_or_else(KeyframeId::new);
        self.shape_keyframes = vec![MaskShapeKeyframe {
            id: retained_id,
            time: TimelineTime::ZERO,
            shape,
            interpolation: MaskShapeInterpolation::Hold,
        }]
        .into();
        self.shape_animation_enabled = false;
        self.tracking = None;
        Ok(true)
    }

    /// Write a complete Mask shape at exact Clip-local author time.
    ///
    /// Static Masks always retain one canonical time-zero key. Animated Masks
    /// update an exact-time key without changing its identity or insert a new
    /// stable key in time order. The return value is the affected key identity,
    /// or `None` for a semantic no-op.
    pub fn write_shape(
        &mut self,
        time: TimelineTime,
        shape: MaskShape,
        interpolation: MaskShapeInterpolation,
    ) -> Result<Option<KeyframeId>, crate::MondrianError> {
        validate_shape(self.id, &shape)?;
        let time = if self.shape_animation_enabled {
            time
        } else {
            TimelineTime::ZERO
        };
        let interpolation = if self.shape_animation_enabled {
            interpolation
        } else {
            MaskShapeInterpolation::Hold
        };
        let mut candidate = self.clone();
        if let Some(existing) = candidate.shape_keyframes.iter_mut().find(|key| key.time == time) {
            if existing.shape == shape && existing.interpolation == interpolation {
                return Ok(None);
            }
            existing.shape = shape;
            existing.interpolation = interpolation;
            candidate.tracking = None;
            let id = existing.id;
            candidate.validate_author_state()?;
            *self = candidate;
            return Ok(Some(id));
        }
        if !self.shape_animation_enabled {
            return Err(mask_validation_error(
                self.id,
                "static Mask lost its canonical shape key",
            ));
        }
        let key = MaskShapeKeyframe::new(time, shape, interpolation);
        let id = key.id;
        candidate.shape_keyframes.push(key);
        candidate.shape_keyframes.sort_by_key(|key| key.time);
        candidate.tracking = None;
        candidate.validate_author_state()?;
        *self = candidate;
        Ok(Some(id))
    }

    /// Atomically replace one inclusive time range with generated tracking keys.
    ///
    /// Existing exact-time key identities are retained. Keys outside the
    /// generated range remain untouched; the complete candidate is validated
    /// before publication so cancellation or malformed analysis can never
    /// leave a partially generated track.
    pub fn apply_tracking_result(
        &mut self,
        recipe: MaskTrackingRecipe,
        generated: Vec<(TimelineTime, MaskShape)>,
    ) -> Result<bool, crate::MondrianError> {
        recipe.validate()?;
        if generated.is_empty() {
            return Err(tracking_validation_error(
                "tracking result must contain at least one generated shape",
            ));
        }
        let mut generated = generated;
        generated.sort_by_key(|(time, _)| *time);
        if generated.first().map(|entry| entry.0) != Some(recipe.range_start)
            || generated.last().map(|entry| entry.0) != Some(recipe.range_end)
        {
            return Err(tracking_validation_error(
                "tracking result does not cover its declared inclusive range",
            ));
        }
        if !generated.iter().any(|(time, _)| *time == recipe.anchor_time) {
            return Err(tracking_validation_error(
                "tracking result does not contain its anchor key",
            ));
        }
        for pair in generated.windows(2) {
            if pair[0].0 >= pair[1].0 {
                return Err(tracking_validation_error(
                    "tracking result times must be strictly increasing",
                ));
            }
        }
        let mut candidate = self.clone();
        let retained_ids = candidate
            .shape_keyframes
            .iter()
            .map(|key| (key.time, key.id))
            .collect::<std::collections::BTreeMap<_, _>>();
        candidate
            .shape_keyframes
            .retain(|key| key.time < recipe.range_start || key.time > recipe.range_end);
        for (time, shape) in generated {
            validate_shape(self.id, &shape)?;
            candidate.shape_keyframes.push(MaskShapeKeyframe {
                id: retained_ids.get(&time).copied().unwrap_or_else(KeyframeId::new),
                time,
                shape,
                interpolation: MaskShapeInterpolation::Linear,
            });
        }
        candidate.shape_keyframes.sort_by_key(|key| key.time);
        if let Some(last) = candidate.shape_keyframes.last_mut() {
            last.interpolation = MaskShapeInterpolation::Hold;
        }
        candidate.shape_animation_enabled = true;
        candidate.tracking = Some(recipe);
        candidate.validate_author_state()?;
        if *self == candidate {
            return Ok(false);
        }
        *self = candidate;
        Ok(true)
    }

    /// Evaluate the mask properties at a given time.
    pub fn evaluate_at(&self, time: TimelineTime) -> MaskEvaluation {
        let shape = if self.shape_keyframes.is_empty() {
            MaskShape::default()
        } else if !self.shape_animation_enabled
            || self.shape_keyframes.len() == 1
            || time <= self.shape_keyframes[0].time
        {
            self.shape_keyframes[0].shape.clone()
        } else if time
            >= self.shape_keyframes.last().map(|key| key.time).unwrap_or(TimelineTime::ZERO)
        {
            self.shape_keyframes.last().map(|key| key.shape.clone()).unwrap_or_default()
        } else {
            let mut result = self.shape_keyframes[0].shape.clone();
            for pair in self.shape_keyframes.windows(2) {
                let t0 = pair[0].time;
                let t1 = pair[1].time;
                if time >= t0 && time <= t1 {
                    result = match pair[0].interpolation {
                        MaskShapeInterpolation::Hold => pair[0].shape.clone(),
                        MaskShapeInterpolation::Linear => {
                            let duration =
                                t1.checked_sub(t0).map(TimelineTime::to_f64).unwrap_or(0.0);
                            let elapsed =
                                time.checked_sub(t0).map(TimelineTime::to_f64).unwrap_or(0.0);
                            let fraction = if duration > 0.0 {
                                (elapsed / duration) as f32
                            } else {
                                0.0
                            };
                            interpolate_shape(&pair[0].shape, &pair[1].shape, fraction)
                                .unwrap_or_else(|| pair[0].shape.clone())
                        }
                    };
                    break;
                }
            }
            result
        };

        let feather = self
            .properties
            .evaluate(MASK_PROP_FEATHER, time)
            .and_then(|v| v.as_f32())
            .unwrap_or(0.0);
        let opacity = self
            .properties
            .evaluate(MASK_PROP_OPACITY, time)
            .and_then(|v| v.as_f32())
            .unwrap_or(1.0);
        let expansion = self
            .properties
            .evaluate(MASK_PROP_EXPANSION, time)
            .and_then(|v| v.as_f32())
            .unwrap_or(0.0);
        let invert = self
            .properties
            .evaluate(MASK_PROP_INVERT, time)
            .and_then(|v| {
                if let PropertyValue::Bool(b) = v {
                    Some(b)
                } else {
                    None
                }
            })
            .unwrap_or(false);
        let mask_op_str = self
            .properties
            .evaluate(MASK_PROP_MASK_OP, time)
            .and_then(|v| {
                if let PropertyValue::Enum(s) = v {
                    Some(s.clone())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "Add".to_string());

        MaskEvaluation {
            shape,
            feather,
            opacity,
            expansion,
            invert,
            mask_op: MaskOp::parse(&mask_op_str),
        }
    }
}

fn validate_shape(mask_id: MaskId, shape: &MaskShape) -> crate::Result<()> {
    let valid = match shape {
        MaskShape::Rectangle { x, y, width, height, corner_radius } => {
            [*x, *y, *width, *height, *corner_radius].into_iter().all(f32::is_finite)
                && *width >= 0.0
                && *height >= 0.0
                && *corner_radius >= 0.0
        }
        MaskShape::Ellipse { center, radii } => {
            center.is_finite() && radii.is_finite() && radii.x >= 0.0 && radii.y >= 0.0
        }
        MaskShape::Path { points, .. } => {
            points.len() <= MAX_MASK_PATH_POINTS
                && points.iter().all(|point| {
                    point.position.is_finite()
                        && point.control_in.is_finite()
                        && point.control_out.is_finite()
                })
        }
    };
    if valid {
        Ok(())
    } else {
        Err(mask_validation_error(
            mask_id,
            "shape contains invalid geometry",
        ))
    }
}

fn mask_validation_error(mask_id: MaskId, reason: impl Into<String>) -> crate::MondrianError {
    crate::MondrianError::WorkflowStepFailed {
        step_id: "validate_mask_author_state".to_owned(),
        reason: format!("mask {mask_id}: {}", reason.into()),
    }
}

/// Human-readable label for the mask shape type.
pub fn shape_label(shape: &MaskShape) -> String {
    match shape {
        MaskShape::Rectangle { .. } => "矩形".to_string(),
        MaskShape::Ellipse { .. } => "椭圆".to_string(),
        MaskShape::Path { points, .. } if points.len() > 2 => format!("路径({}点)", points.len()),
        MaskShape::Path { .. } => "路径".to_string(),
    }
}

/// Linearly interpolate compatible Mask geometry.
///
/// Rectangle and Ellipse geometry interpolate within their own shape type.
/// Paths require identical point counts and closed state. Incompatible
/// topology returns `None`; callers must use explicit Hold interpolation for a
/// deliberate topology change.
pub fn interpolate_shape(a: &MaskShape, b: &MaskShape, t: f32) -> Option<MaskShape> {
    let t = t.clamp(0.0, 1.0);
    match (a, b) {
        (
            MaskShape::Rectangle {
                x: ax,
                y: ay,
                width: aw,
                height: ah,
                corner_radius: ar,
            },
            MaskShape::Rectangle {
                x: bx,
                y: by,
                width: bw,
                height: bh,
                corner_radius: br,
            },
        ) => Some(MaskShape::Rectangle {
            x: ax + (bx - ax) * t,
            y: ay + (by - ay) * t,
            width: aw + (bw - aw) * t,
            height: ah + (bh - ah) * t,
            corner_radius: ar + (br - ar) * t,
        }),
        (
            MaskShape::Ellipse { center: ac, radii: ar },
            MaskShape::Ellipse { center: bc, radii: br },
        ) => Some(MaskShape::Ellipse {
            center: *ac + (*bc - *ac) * t,
            radii: *ar + (*br - *ar) * t,
        }),
        (
            MaskShape::Path { points: a_points, closed: a_closed },
            MaskShape::Path { points: b_points, closed: b_closed },
        ) if a_closed == b_closed && a_points.len() == b_points.len() => Some(MaskShape::Path {
            points: a_points
                .iter()
                .zip(b_points)
                .map(|(a, b)| BezierPoint {
                    position: a.position + (b.position - a.position) * t,
                    control_in: a.control_in + (b.control_in - a.control_in) * t,
                    control_out: a.control_out + (b.control_out - a.control_out) * t,
                })
                .collect(),
            closed: *a_closed,
        }),
        _ => None,
    }
}

fn shapes_have_compatible_topology(a: &MaskShape, b: &MaskShape) -> bool {
    match (a, b) {
        (MaskShape::Rectangle { .. }, MaskShape::Rectangle { .. })
        | (MaskShape::Ellipse { .. }, MaskShape::Ellipse { .. }) => true,
        (
            MaskShape::Path { points: a_points, closed: a_closed },
            MaskShape::Path { points: b_points, closed: b_closed },
        ) => a_closed == b_closed && a_points.len() == b_points.len(),
        _ => false,
    }
}

impl crate::AuthoringFootprint for MaskShape {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        match self {
            Self::Rectangle { x: _, y: _, width: _, height: _, corner_radius: _ }
            | Self::Ellipse { center: _, radii: _ } => Ok(()),
            Self::Path { points, closed: _ } => collector.collect(points),
        }
    }
}

impl crate::AuthoringFootprint for BezierPoint {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self { position: _, control_in: _, control_out: _ } = self;
        Ok(())
    }
}

impl crate::AuthoringFootprint for MaskEvaluation {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self {
            shape,
            feather: _,
            opacity: _,
            expansion: _,
            invert: _,
            mask_op: _,
        } = self;
        collector.collect(shape)
    }
}

impl crate::AuthoringFootprint for MaskShapeKeyframe {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self { id: _, time: _, shape, interpolation: _ } = self;
        collector.collect(shape)
    }
}

impl crate::AuthoringFootprint for MaskComponent {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self {
            id: _,
            name,
            shape_keyframes,
            properties,
            enabled: _,
            locked: _,
            shape_animation_enabled: _,
            tracking,
        } = self;
        collector.collect(name)?;
        collector.collect(shape_keyframes)?;
        collector.collect(properties)?;
        collector.collect(tracking)
    }
}

impl crate::AuthoringFootprint for MaskTrackingRecipe {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self {
            id: _,
            model: _,
            direction: _,
            anchor_time: _,
            range_start: _,
            range_end: _,
            settings: _,
            source_fingerprint: _,
            video_stream_index: _,
            mean_inlier_ratio: _,
            minimum_observed_inlier_ratio: _,
        } = self;
        Ok(())
    }
}

fn tracking_validation_error(reason: impl Into<String>) -> crate::MondrianError {
    crate::MondrianError::WorkflowStepFailed {
        step_id: "mask_tracking".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::{InterpolationType, PropertyValue};

    fn complete_fingerprint() -> MediaFileFingerprint {
        MediaFileFingerprint {
            len: Some(1024),
            modified_secs: Some(7),
            modified_nanos: Some(11),
            object_identity: Some(crate::MediaFileObjectIdentity::Windows {
                volume_serial_number: 13,
                file_id: [17; 16],
            }),
            change_stamp: Some(crate::MediaFileChangeStamp::WindowsFileTime(19)),
        }
    }

    fn tt(hundredths: i64) -> TimelineTime {
        TimelineTime::new(hundredths, 100).expect("valid test time")
    }

    #[test]
    fn mask_component_evaluates_single_keyframe() {
        let kf = MaskEvaluation { feather: 5.0, ..Default::default() };
        let mc = MaskComponent::new("M1".into(), kf);
        let result = mc.evaluate_at(tt(0));
        assert_eq!(result.feather, 5.0);
        let result2 = mc.evaluate_at(tt(100));
        assert_eq!(result2.feather, 5.0);
    }

    #[test]
    fn tracking_result_is_atomic_preserves_exact_key_identity_and_round_trips() {
        let mut mask = MaskComponent::new("Tracked".into(), MaskEvaluation::default());
        let original_key_id = mask.shape_keyframes[0].id;
        let recipe = MaskTrackingRecipe {
            id: TrackingId::new(),
            model: MaskTrackingModel::ObjectTranslation,
            direction: MaskTrackingDirection::Forward,
            anchor_time: TimelineTime::ZERO,
            range_start: TimelineTime::ZERO,
            range_end: tt(2),
            settings: MaskTrackingSettings::default(),
            source_fingerprint: complete_fingerprint(),
            video_stream_index: 3,
            mean_inlier_ratio: 0.8,
            minimum_observed_inlier_ratio: 0.7,
        };
        let moved = MaskShape::Rectangle {
            x: 0.15,
            y: 0.1,
            width: 0.8,
            height: 0.8,
            corner_radius: 0.0,
        };
        assert!(mask
            .apply_tracking_result(
                recipe.clone(),
                vec![
                    (TimelineTime::ZERO, MaskShape::default()),
                    (tt(1), moved.clone()),
                    (tt(2), moved),
                ],
            )
            .expect("apply tracking"));
        assert_eq!(mask.shape_keyframes[0].id, original_key_id);
        assert_eq!(mask.tracking, Some(recipe));
        let reopened: MaskComponent =
            serde_json::from_str(&serde_json::to_string(&mask).expect("serialize tracked Mask"))
                .expect("reopen tracked Mask");
        assert_eq!(reopened, mask);

        let before = mask.clone();
        let mut invalid_recipe = mask.tracking.clone().expect("recipe");
        invalid_recipe.range_end = tt(3);
        assert!(mask
            .apply_tracking_result(
                invalid_recipe,
                vec![(TimelineTime::ZERO, MaskShape::default())],
            )
            .is_err());
        assert_eq!(mask, before, "failed publication must be all-or-nothing");
    }

    #[test]
    fn tracking_settings_reject_pathological_cross_product_workloads() {
        MaskTrackingSettings::default().validate().expect("default tracking budget");
        MaskTrackingSettings {
            max_features: 1_024,
            search_radius: 128,
            patch_radius: 12,
            ..MaskTrackingSettings::default()
        }
        .validate()
        .expect_err("individually bounded knobs must not combine into an unbounded workload");
        MaskTrackingSettings {
            max_features: 16,
            search_radius: 128,
            patch_radius: 2,
            ..MaskTrackingSettings::default()
        }
        .validate()
        .expect("large search remains available when the total workload is bounded");
    }

    #[test]
    fn mask_component_interpolates_between_two_keyframes() {
        let kf0 = MaskEvaluation { feather: 0.0, opacity: 1.0, ..Default::default() };
        let mut mc = MaskComponent::new("M1".into(), kf0);
        mc.properties.enable_animation(MASK_PROP_FEATHER, tt(0)).unwrap();
        mc.properties
            .write_value(
                MASK_PROP_FEATHER,
                tt(0),
                PropertyValue::Float(0.0),
                InterpolationType::Linear,
            )
            .unwrap();
        mc.properties
            .write_value(
                MASK_PROP_FEATHER,
                tt(100),
                PropertyValue::Float(10.0),
                InterpolationType::Linear,
            )
            .unwrap();
        mc.properties.enable_animation(MASK_PROP_OPACITY, tt(0)).unwrap();
        mc.properties
            .write_value(
                MASK_PROP_OPACITY,
                tt(0),
                PropertyValue::Float(1.0),
                InterpolationType::Linear,
            )
            .unwrap();
        mc.properties
            .write_value(
                MASK_PROP_OPACITY,
                tt(100),
                PropertyValue::Float(0.0),
                InterpolationType::Linear,
            )
            .unwrap();

        let mid = mc.evaluate_at(tt(50));
        assert!((mid.feather - 5.0).abs() < 1e-5);
        assert!((mid.opacity - 0.5).abs() < 1e-5);
    }

    #[test]
    fn mask_component_partial_eq_includes_animation_toggle() {
        let kf = MaskEvaluation::default();
        let a = MaskComponent::new("A".into(), kf);
        let mut b = a.clone();
        assert_eq!(a, b);
        b.shape_animation_enabled = true;
        assert_ne!(a, b, "shape_animation_enabled should affect equality");
    }

    #[test]
    fn mask_component_partial_eq_includes_property_author_state() {
        let mut original = MaskComponent::new("A".into(), MaskEvaluation::default());
        let mut edited = original.clone();
        assert_eq!(original, edited);

        edited
            .properties
            .set_static_value(MASK_PROP_FEATHER, PropertyValue::Float(12.0))
            .expect("edit mask feather");

        assert_ne!(original, edited, "mask PropertyBag must affect equality");
        original
            .properties
            .set_static_value(MASK_PROP_FEATHER, PropertyValue::Float(12.0))
            .expect("match mask feather");
        assert_eq!(original, edited);
    }

    #[test]
    fn mask_component_interpolates_rectangle_shape() {
        let shape_a = MaskShape::Rectangle {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
            corner_radius: 0.0,
        };
        let shape_b = MaskShape::Rectangle {
            x: 50.0,
            y: 50.0,
            width: 200.0,
            height: 200.0,
            corner_radius: 10.0,
        };
        let a = MaskEvaluation { shape: shape_a, ..Default::default() };
        let mut mc = MaskComponent::new("M1".into(), a);
        mc.shape_keyframes.push(MaskShapeKeyframe::new(
            tt(100),
            shape_b,
            MaskShapeInterpolation::Linear,
        ));
        mc.shape_keyframes[0].interpolation = MaskShapeInterpolation::Linear;
        mc.shape_animation_enabled = true;

        let mid = mc.evaluate_at(tt(50));
        if let MaskShape::Rectangle { x, y, width, height, corner_radius } = mid.shape {
            assert!((x - 25.0).abs() < 1e-4);
            assert!((y - 25.0).abs() < 1e-4);
            assert!((width - 150.0).abs() < 1e-4);
            assert!((height - 150.0).abs() < 1e-4);
            assert!((corner_radius - 5.0).abs() < 1e-4);
        } else {
            panic!("expected Rectangle");
        }
    }

    #[test]
    fn mask_shape_keys_keep_identity_and_reject_implicit_topology_morphs() {
        let mut mask = MaskComponent::new("M1".into(), MaskEvaluation::default());
        let first_id = mask.shape_keyframes[0].id;
        assert!(mask.set_shape_animation_enabled(true, TimelineTime::ZERO).unwrap());
        assert_eq!(mask.shape_keyframes.len(), 1);
        assert_eq!(mask.shape_keyframes[0].id, first_id);

        let ellipse = MaskShape::Ellipse {
            center: Vec2::new(0.5, 0.5),
            radii: Vec2::new(0.25, 0.25),
        };
        let second_id = mask
            .write_shape(tt(100), ellipse, MaskShapeInterpolation::Hold)
            .unwrap()
            .expect("insert shape key");
        assert_ne!(first_id, second_id);
        assert!(mask.validate_author_state().is_ok());

        mask.shape_keyframes[0].interpolation = MaskShapeInterpolation::Linear;
        assert!(mask.validate_author_state().is_err());
    }

    #[test]
    fn mask_author_state_rejects_unbounded_path_complexity() {
        let points = vec![BezierPoint::new(Vec2::ZERO); MAX_MASK_PATH_POINTS + 1];
        let mask = MaskComponent::new(
            "oversized".into(),
            MaskEvaluation {
                shape: MaskShape::Path { points, closed: true },
                ..MaskEvaluation::default()
            },
        );

        assert!(mask.validate_author_state().is_err());
    }
}
