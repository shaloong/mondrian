//! 转场效果

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transition {
    CrossDissolve,
    WipeLeft,
    WipeRight,
    WipeUp,
    WipeDown,
    ZoomIn,
    ZoomOut,
    FilmBurn,
    Glitch,
    LensFlare,
    Fade,
    DipToColor { r: u8, g: u8, b: u8 },
}

impl Transition {
    pub fn default_duration_frames(&self) -> u32 {
        match self {
            Self::CrossDissolve | Self::Fade => 25,
            Self::Glitch => 10,
            _ => 20,
        }
    }
}
