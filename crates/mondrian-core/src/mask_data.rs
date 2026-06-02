//! Mask data types — pure geometric data for clip masks.
//!
//! These are pure data types with no rendering logic. Mask rasterization lives
//! in `mondrian-effects::mask_raster`.

use crate::automation::{PropertyBag, PropertyValue, TimeTicks};
use crate::types::MaskId;
use glam::Vec2;
use serde::{Deserialize, Serialize};

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

/// A mask keyframe — stores shape and legacy scalar fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaskKeyframe {
    pub shape: MaskShape,
    #[serde(default)]
    pub feather: f32,
    #[serde(default = "default_one")]
    pub opacity: f32,
    #[serde(default)]
    pub expansion: f32,
    #[serde(default)]
    pub invert: bool,
    #[serde(default)]
    pub mask_op: MaskOp,
}

impl Default for MaskKeyframe {
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
pub struct MaskComponent {
    pub id: MaskId,
    pub name: String,
    /// Shape keyframes. When `shape_animation_enabled` is false, only the first
    /// entry is used (static shape). When enabled, shapes are interpolated by time.
    pub shape_keyframes: Vec<(TimeTicks, MaskShape)>,
    /// Legacy combined keyframes — preserved for deserialization of old project files.
    #[serde(default)]
    pub keyframes: Vec<(TimeTicks, MaskKeyframe)>,
    /// Scalar animatable properties (feather, opacity, expansion, invert, mask_op).
    #[serde(skip)]
    pub properties: PropertyBag,
    pub enabled: bool,
    pub locked: bool,
    pub shape_animation_enabled: bool,
}

impl PartialEq for MaskComponent {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.name == other.name
            && self.shape_keyframes == other.shape_keyframes
            && self.keyframes == other.keyframes
            && self.enabled == other.enabled
            && self.locked == other.locked
            && self.shape_animation_enabled == other.shape_animation_enabled
    }
}

impl MaskComponent {
    pub fn new(name: String, initial: MaskKeyframe) -> Self {
        let id = MaskId::new();
        let mut properties = PropertyBag::default();
        use crate::automation::{AnimatablePropertyUiMetadata, PropertyDescriptor};

        let define_prop =
            |bag: &mut PropertyBag, path: &str, display: &str, value: PropertyValue| {
                let mut desc = PropertyDescriptor::new(path, display, value);
                desc.ui_metadata = AnimatablePropertyUiMetadata {
                    group_name: Some(name.clone()),
                    ..Default::default()
                };
                bag.define(desc);
            };

        define_prop(
            &mut properties,
            MASK_PROP_FEATHER,
            "羽化",
            PropertyValue::Float(initial.feather),
        );
        define_prop(
            &mut properties,
            MASK_PROP_OPACITY,
            "不透明度",
            PropertyValue::Float(initial.opacity),
        );
        define_prop(
            &mut properties,
            MASK_PROP_EXPANSION,
            "扩展",
            PropertyValue::Float(initial.expansion),
        );
        define_prop(
            &mut properties,
            MASK_PROP_INVERT,
            "反转",
            PropertyValue::Bool(initial.invert),
        );
        define_prop(
            &mut properties,
            MASK_PROP_MASK_OP,
            "模式",
            PropertyValue::Text(initial.mask_op.as_str().to_string()),
        );

        Self {
            id,
            name,
            shape_keyframes: vec![(0, initial.shape)],
            keyframes: Vec::new(),
            properties,
            enabled: true,
            locked: false,
            shape_animation_enabled: false,
        }
    }

    pub fn shape_keyframes(&self) -> &[(TimeTicks, MaskShape)] {
        &self.shape_keyframes
    }

    pub fn shape_keyframes_mut(&mut self) -> &mut Vec<(TimeTicks, MaskShape)> {
        &mut self.shape_keyframes
    }

    pub fn current_shape(&self) -> Option<&MaskShape> {
        self.shape_keyframes.first().map(|(_, shape)| shape)
    }

    pub fn property_path(mask_id: MaskId, property: &str) -> String {
        format!("mask.{mask_id}.{property}")
    }

    /// Evaluate the mask properties at a given time.
    pub fn evaluate_at(&self, time: TimeTicks) -> MaskKeyframe {
        let shape = if self.shape_keyframes.is_empty() {
            MaskShape::default()
        } else if !self.shape_animation_enabled
            || self.shape_keyframes.len() == 1
            || time <= self.shape_keyframes[0].0
        {
            self.shape_keyframes[0].1.clone()
        } else if time >= self.shape_keyframes.last().map(|(t, _)| *t).unwrap_or(0) {
            self.shape_keyframes.last().map(|(_, s)| s.clone()).unwrap_or_default()
        } else {
            let mut result = self.shape_keyframes[0].1.clone();
            for pair in self.shape_keyframes.windows(2) {
                let (t0, ref s0) = pair[0];
                let (t1, ref s1) = pair[1];
                if time >= t0 && time <= t1 {
                    let range = (t1 - t0).max(1);
                    let t = (time - t0) as f32 / range as f32;
                    result = interpolate_shape(s0, s1, t);
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
                if let PropertyValue::Text(s) = v {
                    Some(s.clone())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "Add".to_string());

        MaskKeyframe {
            shape,
            feather,
            opacity,
            expansion,
            invert,
            mask_op: MaskOp::parse(&mask_op_str),
        }
    }

    /// Migrate legacy keyframes to shape_keyframes + PropertyBag (for old project files).
    pub fn ensure_migrated(&mut self) {
        if !self.keyframes.is_empty() && self.shape_keyframes.is_empty() {
            use crate::automation::InterpolationType;
            for (t, kf) in &self.keyframes {
                self.shape_keyframes.push((*t, kf.shape.clone()));
                let _ = self.properties.write_value(
                    MASK_PROP_FEATHER,
                    *t,
                    PropertyValue::Float(kf.feather),
                    InterpolationType::Linear,
                );
                let _ = self.properties.write_value(
                    MASK_PROP_OPACITY,
                    *t,
                    PropertyValue::Float(kf.opacity),
                    InterpolationType::Linear,
                );
                let _ = self.properties.write_value(
                    MASK_PROP_EXPANSION,
                    *t,
                    PropertyValue::Float(kf.expansion),
                    InterpolationType::Linear,
                );
                let _ = self.properties.write_value(
                    MASK_PROP_INVERT,
                    *t,
                    PropertyValue::Bool(kf.invert),
                    InterpolationType::Hold,
                );
                let _ = self.properties.write_value(
                    MASK_PROP_MASK_OP,
                    *t,
                    PropertyValue::Text(kf.mask_op.as_str().to_string()),
                    InterpolationType::Hold,
                );
            }
            self.keyframes.clear();
        }
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

/// Linear interpolation between two MaskShapes.
///
/// Rectangle→Rectangle and Ellipse→Ellipse interpolate smoothly.
/// Cross-type morphs (e.g., Rectangle→Ellipse) snap: t < 0.5 returns
/// shape A, t >= 0.5 returns shape B. Path morphing is not supported
/// and also follows the snap behavior.
pub fn interpolate_shape(a: &MaskShape, b: &MaskShape, t: f32) -> MaskShape {
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
        ) => MaskShape::Rectangle {
            x: ax + (bx - ax) * t,
            y: ay + (by - ay) * t,
            width: aw + (bw - aw) * t,
            height: ah + (bh - ah) * t,
            corner_radius: ar + (br - ar) * t,
        },
        (
            MaskShape::Ellipse { center: ac, radii: ar },
            MaskShape::Ellipse { center: bc, radii: br },
        ) => MaskShape::Ellipse {
            center: *ac + (*bc - *ac) * t,
            radii: *ar + (*br - *ar) * t,
        },
        _ => {
            if t < 0.5 {
                a.clone()
            } else {
                b.clone()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::{InterpolationType, PropertyValue};

    #[test]
    fn mask_component_evaluates_single_keyframe() {
        let kf = MaskKeyframe { feather: 5.0, ..Default::default() };
        let mc = MaskComponent::new("M1".into(), kf);
        let result = mc.evaluate_at(0);
        assert_eq!(result.feather, 5.0);
        let result2 = mc.evaluate_at(100);
        assert_eq!(result2.feather, 5.0);
    }

    #[test]
    fn mask_component_interpolates_between_two_keyframes() {
        let kf0 = MaskKeyframe { feather: 0.0, opacity: 1.0, ..Default::default() };
        let mut mc = MaskComponent::new("M1".into(), kf0);
        mc.properties.enable_animation(MASK_PROP_FEATHER, 0).unwrap();
        mc.properties
            .write_value(
                MASK_PROP_FEATHER,
                0,
                PropertyValue::Float(0.0),
                InterpolationType::Linear,
            )
            .unwrap();
        mc.properties
            .write_value(
                MASK_PROP_FEATHER,
                100,
                PropertyValue::Float(10.0),
                InterpolationType::Linear,
            )
            .unwrap();
        mc.properties.enable_animation(MASK_PROP_OPACITY, 0).unwrap();
        mc.properties
            .write_value(
                MASK_PROP_OPACITY,
                0,
                PropertyValue::Float(1.0),
                InterpolationType::Linear,
            )
            .unwrap();
        mc.properties
            .write_value(
                MASK_PROP_OPACITY,
                100,
                PropertyValue::Float(0.0),
                InterpolationType::Linear,
            )
            .unwrap();

        let mid = mc.evaluate_at(50);
        assert!((mid.feather - 5.0).abs() < 1e-5);
        assert!((mid.opacity - 0.5).abs() < 1e-5);
    }

    #[test]
    fn mask_component_partial_eq_includes_animation_toggle() {
        let kf = MaskKeyframe::default();
        let mut a = MaskComponent::new("A".into(), kf.clone());
        let mut b = MaskComponent::new("A".into(), kf);
        a.id = b.id; // make ids match for comparison
        assert_eq!(a, b);
        b.shape_animation_enabled = true;
        assert_ne!(a, b, "shape_animation_enabled should affect equality");
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
        let a = MaskKeyframe { shape: shape_a, ..Default::default() };
        let mut mc = MaskComponent::new("M1".into(), a);
        mc.shape_keyframes.push((100, shape_b));
        mc.shape_animation_enabled = true;

        let mid = mc.evaluate_at(50);
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
}
