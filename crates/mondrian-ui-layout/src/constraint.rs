//! 扩展布局约束
//!
//! 在 `mondrian-ui-core` 的 `LayoutConstraint` 基础上增加
//! preferred_size 和 flex_grow 属性。

use mondrian_ui_core::types::{LayoutConstraint, Size};

/// 扩展的布局约束 —— 用于 flex 布局计算
#[derive(Debug, Clone, Copy)]
pub struct ExtendedConstraint {
    pub min: Size,
    pub max: Size,
    pub preferred: Size,
    pub flex_grow: f32,
}

impl Default for ExtendedConstraint {
    fn default() -> Self {
        Self {
            min: Size::ZERO,
            max: Size { width: f32::MAX, height: f32::MAX },
            preferred: Size::ZERO,
            flex_grow: 0.0,
        }
    }
}

impl From<LayoutConstraint> for ExtendedConstraint {
    fn from(c: LayoutConstraint) -> Self {
        Self {
            min: c.min,
            max: c.max,
            preferred: c.min,
            flex_grow: 0.0,
        }
    }
}

impl ExtendedConstraint {
    pub fn tight(width: f32, height: f32) -> Self {
        Self {
            min: Size { width, height },
            max: Size { width, height },
            preferred: Size { width, height },
            flex_grow: 0.0,
        }
    }

    pub fn with_preferred(mut self, w: f32, h: f32) -> Self {
        self.preferred = Size { width: w, height: h };
        self
    }

    pub fn with_flex_grow(mut self, grow: f32) -> Self {
        self.flex_grow = grow;
        self
    }

    pub fn constrain(&self, size: Size) -> Size {
        Size {
            width: size.width.clamp(self.min.width, self.max.width),
            height: size.height.clamp(self.min.height, self.max.height),
        }
    }
}

/// 矩形边距
#[derive(Debug, Clone, Copy, Default)]
pub struct RectInsets {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl RectInsets {
    pub fn all(value: f32) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }

    pub fn symmetric(horizontal: f32, vertical: f32) -> Self {
        Self {
            top: vertical,
            right: horizontal,
            bottom: vertical,
            left: horizontal,
        }
    }
}
