//! Mondrian UI 主题系统 — shadcn/ui + Apple 风格
//!
//! 提供语义化的颜色、排版、间距 Token。
//! 所有 UI 代码通过此 crate 获取主题值，禁止硬编码。

pub mod colors;
pub mod spacing;
pub mod typography;

use std::sync::RwLock;

use colors::ColorTokens;
use serde::{Deserialize, Serialize};
use spacing::SpacingTokens;
use typography::TypographyTokens;

/// 主题 — 所有视觉属性的集合
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub colors: ColorTokens,
    pub typography: TypographyTokens,
    pub spacing: SpacingTokens,
}

/// 主题预设
///
/// Dark 和 Light 是内置主题。用户可通过 plugin 或直接构造 `Theme` 来扩展。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemePreset {
    Dark,
    Light,
}

impl ThemePreset {
    pub const ALL: [Self; 2] = [Self::Dark, Self::Light];

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Dark => "深色",
            Self::Light => "浅色",
        }
    }

    pub fn build(self) -> Theme {
        let (name, colors) = match self {
            Self::Dark => ("Dark", ColorTokens::dark()),
            Self::Light => ("Light", ColorTokens::light()),
        };
        Theme {
            name: name.into(),
            colors,
            typography: TypographyTokens::default(),
            spacing: SpacingTokens::default(),
        }
    }
}

/// 活跃主题的全局存储
static ACTIVE_THEME: std::sync::OnceLock<RwLock<Theme>> = std::sync::OnceLock::new();

fn ensure_initialized() -> &'static RwLock<Theme> {
    ACTIVE_THEME.get_or_init(|| RwLock::new(ThemePreset::Dark.build()))
}

pub fn current_theme() -> std::sync::RwLockReadGuard<'static, Theme> {
    ensure_initialized().read().unwrap()
}

pub fn set_theme(theme: Theme) {
    if let Some(lock) = ACTIVE_THEME.get() {
        *lock.write().unwrap() = theme;
    } else {
        ACTIVE_THEME.get_or_init(|| RwLock::new(theme));
    }
}

pub fn set_theme_preset(preset: ThemePreset) {
    set_theme(preset.build());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_preset_builds_with_correct_name() {
        assert_eq!(ThemePreset::Dark.build().name, "Dark");
        assert_eq!(ThemePreset::Light.build().name, "Light");
    }

    #[test]
    fn all_presets_build_without_panicking() {
        for preset in ThemePreset::ALL {
            let theme = preset.build();
            assert!(!theme.name.is_empty());
        }
    }

    #[test]
    fn global_theme_can_be_read_and_switched() {
        set_theme_preset(ThemePreset::Light);
        assert_eq!(current_theme().name, "Light");
        set_theme_preset(ThemePreset::Dark);
    }

    #[test]
    fn dark_theme_is_dark() {
        let t = ThemePreset::Dark.build();
        let lum = 0.2126 * t.colors.background.r
            + 0.7152 * t.colors.background.g
            + 0.0722 * t.colors.background.b;
        assert!(lum < 0.2);
    }

    #[test]
    fn light_theme_is_light() {
        let t = ThemePreset::Light.build();
        let lum = 0.2126 * t.colors.background.r
            + 0.7152 * t.colors.background.g
            + 0.0722 * t.colors.background.b;
        assert!(lum > 0.7);
    }

    #[test]
    fn surface_for_state_works() {
        let c = ColorTokens::dark();
        let normal = c.surface_for_state(false, false);
        let hovered = c.surface_for_state(true, false);
        let active = c.surface_for_state(false, true);
        // active + hovered → active wins
        let active_hover = c.surface_for_state(true, true);

        assert_eq!(normal, c.card);
        assert_eq!(hovered, c.muted);
        assert_eq!(active, c.accent);
        assert_eq!(active_hover, c.accent);
    }

    #[test]
    fn border_for_state_works() {
        let c = ColorTokens::dark();
        assert_eq!(c.border_for_state(false), c.border);
        assert_eq!(c.border_for_state(true), c.ring);
    }

    #[test]
    fn text_for_muted_works() {
        let c = ColorTokens::dark();
        assert_eq!(c.text_for_muted(false), c.foreground);
        assert_eq!(c.text_for_muted(true), c.muted_foreground);
    }

    #[test]
    fn typography_uses_neutral_letter_spacing() {
        let typography = TypographyTokens::default();
        let styles = [
            &typography.small,
            &typography.body,
            &typography.large,
            &typography.mono_small,
            &typography.mono_large,
            &typography.button,
            &typography.metadata,
            &typography.heading_h1,
            &typography.heading_h2,
            &typography.heading_h3,
            &typography.tab_label,
        ];

        assert!(styles.iter().all(|style| style.letter_spacing == 0.0));
    }

    #[test]
    fn color_handle_tokens_keep_dual_contrast_available() {
        for preset in ThemePreset::ALL {
            let colors = preset.build().colors;

            assert!(colors.color_handle_shadow.a > 0.0);
            assert!(colors.color_handle_strong_shadow.a >= colors.color_handle_shadow.a);
            assert_eq!(colors.color_handle_outer, mondrian_core::Color::WHITE);
            assert_eq!(colors.color_handle_inner, mondrian_core::Color::BLACK);
            assert_ne!(colors.checkerboard_light, colors.checkerboard_dark);
            assert!(colors.eyedropper_overlay.a > 0.0);
        }
    }

    #[test]
    fn theme_can_be_sent_between_threads() {
        let theme = ThemePreset::Dark.build();
        std::thread::spawn(move || {
            assert_eq!(theme.name, "Dark");
        })
        .join()
        .unwrap();
    }
}
