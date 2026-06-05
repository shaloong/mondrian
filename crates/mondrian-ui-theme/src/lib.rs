//! Mondrian UI 主题系统
//!
//! 提供语义化的颜色、排版、间距、圆角、阴影 Token。
//! 所有 UI 代码通过此 crate 获取主题值，禁止硬编码颜色/间距。
//!
//! ## 设计原则
//!
//! * **语义命名** — Token 描述用途（`text_primary`），不描述颜色（不是 `gray_200`）
//! * **运行时切换** — 主题可以在运行时切换，无需重启
//! * **UI 框架无关** — 不依赖 egui/wgpu，纯数据结构
//! * **可扩展** — 预设主题 + 自定义主题

pub mod colors;
pub mod spacing;
pub mod typography;

use std::sync::RwLock;

use colors::ColorTokens;
use spacing::SpacingTokens;
use typography::TypographyTokens;

/// 主题 —— 所有视觉属性的集合
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub colors: ColorTokens,
    pub typography: TypographyTokens,
    pub spacing: SpacingTokens,
}

/// 主题预设
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemePreset {
    Dark,
    Light,
    Resolve,
    Premiere,
    Fusion,
    Custom,
}

impl ThemePreset {
    pub const ALL: [Self; 6] = [
        Self::Dark,
        Self::Light,
        Self::Resolve,
        Self::Premiere,
        Self::Fusion,
        Self::Custom,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Dark => "深色",
            Self::Light => "浅色",
            Self::Resolve => "Resolve 风格",
            Self::Premiere => "Premiere 风格",
            Self::Fusion => "Fusion 风格",
            Self::Custom => "自定义",
        }
    }

    /// 从预设创建对应的主题数据
    pub fn build(self) -> Theme {
        match self {
            Self::Dark => Theme {
                name: "Dark".into(),
                colors: ColorTokens::dark(),
                typography: TypographyTokens::default(),
                spacing: SpacingTokens::default(),
            },
            Self::Light => Theme {
                name: "Light".into(),
                colors: ColorTokens::light(),
                typography: TypographyTokens::default(),
                spacing: SpacingTokens::default(),
            },
            Self::Resolve => Theme {
                name: "Resolve".into(),
                colors: ColorTokens::resolve(),
                typography: TypographyTokens::default(),
                spacing: SpacingTokens::default(),
            },
            Self::Premiere => Theme {
                name: "Premiere".into(),
                colors: ColorTokens::premiere(),
                typography: TypographyTokens::default(),
                spacing: SpacingTokens::default(),
            },
            Self::Fusion => Theme {
                name: "Fusion".into(),
                colors: ColorTokens::fusion(),
                typography: TypographyTokens::default(),
                spacing: SpacingTokens::default(),
            },
            Self::Custom => Theme {
                name: "Custom".into(),
                colors: ColorTokens::dark(),
                typography: TypographyTokens::default(),
                spacing: SpacingTokens::default(),
            },
        }
    }
}

/// 活跃主题的全局存储（可运行时切换）
static ACTIVE_THEME: std::sync::OnceLock<RwLock<Theme>> = std::sync::OnceLock::new();

fn ensure_initialized() -> &'static RwLock<Theme> {
    ACTIVE_THEME.get_or_init(|| RwLock::new(ThemePreset::Dark.build()))
}

/// 获取当前活跃主题的只读引用
pub fn current_theme() -> std::sync::RwLockReadGuard<'static, Theme> {
    ensure_initialized().read().unwrap()
}

/// 设置当前活跃主题
pub fn set_theme(theme: Theme) {
    if let Some(lock) = ACTIVE_THEME.get() {
        *lock.write().unwrap() = theme;
    } else {
        ACTIVE_THEME.get_or_init(|| RwLock::new(theme));
    }
}

/// 通过预设切换主题
pub fn set_theme_preset(preset: ThemePreset) {
    set_theme(preset.build());
}
