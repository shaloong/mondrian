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

impl TextStyle {
    /// Return this style with font size and line height scaled together.
    pub fn scaled(&self, scale: f32) -> Self {
        Self {
            font_size: self.font_size * scale,
            line_height: self.line_height * scale,
            font_weight: self.font_weight,
            letter_spacing: self.letter_spacing,
        }
    }
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

impl TypographyTokens {
    /// Return typography tokens scaled by one accessibility text scale factor.
    pub fn scaled(&self, scale: f32) -> Self {
        Self {
            small: self.small.scaled(scale),
            body: self.body.scaled(scale),
            large: self.large.scaled(scale),
            mono_small: self.mono_small.scaled(scale),
            mono_large: self.mono_large.scaled(scale),
            button: self.button.scaled(scale),
            metadata: self.metadata.scaled(scale),
            heading_h1: self.heading_h1.scaled(scale),
            heading_h2: self.heading_h2.scaled(scale),
            heading_h3: self.heading_h3.scaled(scale),
            tab_label: self.tab_label.scaled(scale),
        }
    }
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
                letter_spacing: 0.0,
            },
            heading_h2: TextStyle {
                font_size: 22.0,
                line_height: 28.0,
                font_weight: FontWeight::Semibold,
                letter_spacing: 0.0,
            },
            heading_h3: TextStyle {
                font_size: 18.0,
                line_height: 24.0,
                font_weight: FontWeight::Semibold,
                letter_spacing: 0.0,
            },
            tab_label: TextStyle {
                font_size: 12.0,
                line_height: 16.0,
                font_weight: FontWeight::Regular,
                letter_spacing: 0.0,
            },
        }
    }
}
