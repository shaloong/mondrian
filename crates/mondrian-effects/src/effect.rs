//! 效果节点抽象

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectType {
    Lut3D,
    ColorWheel,
    Curves,
    HueSaturationLightness,
    GaussianBlur,
    Sharpen,
    Vignette,
    ChromaticAberration,
    Grain,
    ChromaKey,
    LumaKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectNode {
    pub effect_type: EffectType,
    pub params: serde_json::Value,
    pub is_enabled: bool,
}
