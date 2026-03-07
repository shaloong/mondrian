//! 蒙版系统

use glam::Vec2;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BezierPoint {
    pub position: Vec2,
    pub control_in: Vec2,
    pub control_out: Vec2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mask {
    pub shape: MaskShape,
    pub feather: f32,
    pub invert: bool,
}
