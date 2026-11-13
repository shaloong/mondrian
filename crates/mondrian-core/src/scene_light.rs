//! Render values for lights evaluated in one Sequence's physical pixel space.

use serde::{Deserialize, Serialize};

/// Falloff in the Sequence's physical pixel coordinate system.
#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SceneLightFalloff {
    #[default]
    None,
    Smooth {
        radius: f32,
        distance: f32,
    },
    InverseSquareClamped {
        radius: f32,
    },
}

/// Light geometry in Sequence space. X points right, Y down, and Z toward the
/// viewer. Positions and points of interest use the same physical pixel units.
#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SceneLightSource {
    #[default]
    Ambient,
    Parallel {
        position: [f32; 3],
        target: [f32; 3],
        falloff: SceneLightFalloff,
    },
    Point {
        position: [f32; 3],
        falloff: SceneLightFalloff,
    },
    Spot {
        position: [f32; 3],
        target: [f32; 3],
        cone_angle_degrees: f32,
        cone_feather_percent: f32,
        falloff: SceneLightFalloff,
    },
}

/// Values consumed by rendering. The color is straight, scene-linear RGB.
/// Negative intensity subtracts light and 100 percent is unit intensity.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneLightSample {
    pub color: [f32; 3],
    pub intensity_percent: f32,
    pub source: SceneLightSource,
}

impl Default for SceneLightSample {
    fn default() -> Self {
        Self {
            color: [1.0; 3],
            intensity_percent: 100.0,
            source: SceneLightSource::Ambient,
        }
    }
}
