//! 蒙版系统 — 对标 Premiere Pro 的不透明度蒙版
//!
//! 每个 Clip 支持多个蒙版，每个蒙版支持关键帧动画。

use glam::Vec2;
use mondrian_core::automation::TimeTicks;
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// 蒙版 ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MaskId(pub Uuid);

impl MaskId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for MaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for MaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 贝塞尔路径控制点
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BezierPoint {
    pub position: Vec2,
    pub control_in: Vec2,
    pub control_out: Vec2,
}

impl BezierPoint {
    pub fn new(pos: Vec2) -> Self {
        Self { position: pos, control_in: Vec2::ZERO, control_out: Vec2::ZERO }
    }
}

/// 蒙版形状类型
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
            x: 0.1, y: 0.1, width: 0.8, height: 0.8, corner_radius: 0.0,
        }
    }
}

/// 蒙版布尔运算模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
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

/// 蒙版关键帧：单个时间点上的蒙版状态
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaskKeyframe {
    pub shape: MaskShape,
    pub feather: f32,
    pub opacity: f32,
    pub expansion: f32,
    pub invert: bool,
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

/// 单个蒙版组件：包含关键帧动画
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaskComponent {
    pub id: MaskId,
    pub name: String,
    pub keyframes: Vec<(TimeTicks, MaskKeyframe)>,
    pub enabled: bool,
    /// 编辑锁 — 禁止画布交互，但仍参与渲染
    pub locked: bool,
}

impl MaskComponent {
    pub fn new(name: String, initial: MaskKeyframe) -> Self {
        Self {
            id: MaskId::new(),
            name,
            keyframes: vec![(0, initial)],
            enabled: true,
            locked: false,
        }
    }

    /// Evaluate the mask at a given time by interpolating between keyframes.
    pub fn evaluate_at(&self, time: TimeTicks) -> MaskKeyframe {
        if self.keyframes.is_empty() {
            return MaskKeyframe::default();
        }
        if self.keyframes.len() == 1 || time <= self.keyframes[0].0 {
            return self.keyframes[0].1.clone();
        }
        if time >= self.keyframes.last().unwrap().0 {
            return self.keyframes.last().unwrap().1.clone();
        }

        // Find the surrounding keyframe pair and interpolate.
        for pair in self.keyframes.windows(2) {
            let (t0, ref kf0) = pair[0];
            let (t1, ref kf1) = pair[1];
            if time >= t0 && time <= t1 {
                let range = (t1 - t0).max(1);
                let t = (time - t0) as f32 / range as f32;
                return MaskKeyframe {
                    shape: interpolate_shape(&kf0.shape, &kf1.shape, t),
                    feather: kf0.feather + (kf1.feather - kf0.feather) * t,
                    opacity: kf0.opacity + (kf1.opacity - kf0.opacity) * t,
                    expansion: kf0.expansion + (kf1.expansion - kf0.expansion) * t,
                    invert: if t < 0.5 { kf0.invert } else { kf1.invert },
                    mask_op: if t < 0.5 { kf0.mask_op } else { kf1.mask_op },
                };
            }
        }
        self.keyframes.last().unwrap().1.clone()
    }
}

/// Linear interpolation between two MaskShapes.
/// Only Rectangle and Ellipse support smooth interpolation.
/// Path morphing is not supported — snaps to the second shape at t >= 0.5.
fn interpolate_shape(a: &MaskShape, b: &MaskShape, t: f32) -> MaskShape {
    match (a, b) {
        (MaskShape::Rectangle { x: ax, y: ay, width: aw, height: ah, corner_radius: ar },
         MaskShape::Rectangle { x: bx, y: by, width: bw, height: bh, corner_radius: br }) =>
        {
            MaskShape::Rectangle {
                x: ax + (bx - ax) * t,
                y: ay + (by - ay) * t,
                width: aw + (bw - aw) * t,
                height: ah + (bh - ah) * t,
                corner_radius: ar + (br - ar) * t,
            }
        }
        (MaskShape::Ellipse { center: ac, radii: ar },
         MaskShape::Ellipse { center: bc, radii: br }) =>
        {
            MaskShape::Ellipse {
                center: *ac + (*bc - *ac) * t,
                radii: *ar + (*br - *ar) * t,
            }
        }
        // Cross-type or Path: snap to destination shape at midpoint.
        _ => if t < 0.5 { a.clone() } else { b.clone() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let kf1 = MaskKeyframe { feather: 10.0, opacity: 0.0, ..Default::default() };
        let mut mc = MaskComponent::new("M1".into(), kf0);
        mc.keyframes.push((100, kf1));

        let mid = mc.evaluate_at(50);
        assert!((mid.feather - 5.0).abs() < 1e-5);
        assert!((mid.opacity - 0.5).abs() < 1e-5);
    }

    #[test]
    fn mask_component_interpolates_rectangle_shape() {
        let a = MaskKeyframe {
            shape: MaskShape::Rectangle { x: 0.0, y: 0.0, width: 100.0, height: 100.0, corner_radius: 0.0 },
            ..Default::default()
        };
        let b = MaskKeyframe {
            shape: MaskShape::Rectangle { x: 50.0, y: 50.0, width: 200.0, height: 200.0, corner_radius: 10.0 },
            ..Default::default()
        };
        let mut mc = MaskComponent::new("M1".into(), a);
        mc.keyframes.push((100, b));

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
