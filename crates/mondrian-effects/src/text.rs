//! 文字动画系统

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextLayer {
    pub content:     String,
    pub font_family: String,
    pub font_size:   f32,
    pub color:       [f32; 4],
    pub animation:   Option<TextAnimation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TextAnimation {
    FadeIn         { duration_frames: u32 },
    TypeWriter     { chars_per_frame: f32 },
    SlideFromBottom { duration_frames: u32 },
    ScalePop       { peak_scale: f32, duration_frames: u32 },
}
