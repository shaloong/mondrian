//! 排版 Token
//!
//! 统一的文字样式系统。所有字体大小、行高、字重从这里获取。

/// 文字样式定义
#[derive(Debug, Clone)]
pub struct TextStyle {
    pub font_size: f32,
    pub line_height: f32,
    pub font_weight: FontWeight,
    pub letter_spacing: f32,
}

/// 字重
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontWeight {
    Light,
    Regular,
    Medium,
    Semibold,
    Bold,
}

impl FontWeight {
    pub fn to_css_value(self) -> f32 {
        match self {
            Self::Light => 300.0,
            Self::Regular => 400.0,
            Self::Medium => 500.0,
            Self::Semibold => 600.0,
            Self::Bold => 700.0,
        }
    }
}

/// 排版 Token 集合
#[derive(Debug, Clone)]
pub struct TypographyTokens {
    pub small: TextStyle,
    pub body: TextStyle,
    pub large: TextStyle,
    pub mono_small: TextStyle,
    pub mono_large: TextStyle,
    pub button: TextStyle,
    pub metadata: TextStyle,
    pub heading_h1: TextStyle,
    pub heading_h2: TextStyle,
    pub heading_h3: TextStyle,
    pub tab_label: TextStyle,
}

impl Default for TypographyTokens {
    fn default() -> Self {
        Self {
            small: TextStyle {
                font_size: 12.0,
                line_height: 16.0,
                font_weight: FontWeight::Regular,
                letter_spacing: 0.0,
            },
            body: TextStyle {
                font_size: 14.0,
                line_height: 20.0,
                font_weight: FontWeight::Regular,
                letter_spacing: 0.0,
            },
            large: TextStyle {
                font_size: 16.0,
                line_height: 24.0,
                font_weight: FontWeight::Medium,
                letter_spacing: 0.0,
            },
            mono_small: TextStyle {
                font_size: 12.0,
                line_height: 16.0,
                font_weight: FontWeight::Regular,
                letter_spacing: 0.0,
            },
            mono_large: TextStyle {
                font_size: 24.0,
                line_height: 32.0,
                font_weight: FontWeight::Bold,
                letter_spacing: 0.0,
            },
            button: TextStyle {
                font_size: 12.5,
                line_height: 16.0,
                font_weight: FontWeight::Medium,
                letter_spacing: 0.0,
            },
            metadata: TextStyle {
                font_size: 11.0,
                line_height: 14.0,
                font_weight: FontWeight::Regular,
                letter_spacing: 0.0,
            },
            heading_h1: TextStyle {
                font_size: 28.0,
                line_height: 36.0,
                font_weight: FontWeight::Bold,
                letter_spacing: -0.5,
            },
            heading_h2: TextStyle {
                font_size: 22.0,
                line_height: 28.0,
                font_weight: FontWeight::Semibold,
                letter_spacing: -0.25,
            },
            heading_h3: TextStyle {
                font_size: 18.0,
                line_height: 24.0,
                font_weight: FontWeight::Semibold,
                letter_spacing: 0.0,
            },
            tab_label: TextStyle {
                font_size: 13.0,
                line_height: 18.0,
                font_weight: FontWeight::Medium,
                letter_spacing: 0.0,
            },
        }
    }
}
