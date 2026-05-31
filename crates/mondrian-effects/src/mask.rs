//! 蒙版系统 — 对标 Premiere Pro 的不透明度蒙版
//!
//! 每个 Clip 支持多个蒙版，每个蒙版支持关键帧动画。
//!
//! All type definitions are re-exported from `mondrian_core::mask_data`.

// Re-export mask types from mondrian-core.
pub use mondrian_core::mask_data::{
    interpolate_shape, BezierPoint, MaskComponent, MaskKeyframe, MaskOp, MaskShape, shape_label,
    MASK_PROP_EXPANSION, MASK_PROP_FEATHER, MASK_PROP_INVERT, MASK_PROP_MASK_OP,
    MASK_PROP_OPACITY, MASK_PROP_SHAPE,
};
pub use mondrian_core::types::MaskId;
