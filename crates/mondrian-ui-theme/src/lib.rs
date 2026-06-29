//! Mondrian UI 主题系统 — shadcn/ui + Apple 风格
//!
//! 提供语义化的颜色、排版、间距 Token。
//! 所有 UI 代码通过此 crate 获取主题值，禁止硬编码。

pub mod accessibility;
pub mod colors;
pub mod spacing;
pub mod typography;

use std::sync::RwLock;

pub use accessibility::AccessibilityPreferences;
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

/// User-facing theme preference.
///
/// `System` is a resolver mode, not a third theme. It resolves to one of the
/// concrete built-in presets supplied by the desktop shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemePreference {
    System,
    Dark,
    Light,
}

impl ThemePreference {
    pub const ALL: [Self; 3] = [Self::System, Self::Dark, Self::Light];

    pub fn display_name(self) -> &'static str {
        match self {
            Self::System => "跟随系统",
            Self::Dark => ThemePreset::Dark.display_name(),
            Self::Light => ThemePreset::Light.display_name(),
        }
    }

    pub fn resolve(self, system_preset: ThemePreset) -> ThemePreset {
        match self {
            Self::System => system_preset,
            Self::Dark => ThemePreset::Dark,
            Self::Light => ThemePreset::Light,
        }
    }
}

impl From<ThemePreset> for ThemePreference {
    fn from(value: ThemePreset) -> Self {
        match value {
            ThemePreset::Dark => Self::Dark,
            ThemePreset::Light => Self::Light,
        }
    }
}

/// 活跃主题的全局存储
static ACTIVE_THEME: std::sync::OnceLock<RwLock<Theme>> = std::sync::OnceLock::new();

fn ensure_initialized() -> &'static RwLock<Theme> {
    ACTIVE_THEME.get_or_init(|| RwLock::new(ThemePreset::Dark.build()))
}

pub fn current_theme() -> std::sync::RwLockReadGuard<'static, Theme> {
    ensure_initialized().read().unwrap_or_else(|e| e.into_inner())
}

pub fn set_theme(theme: Theme) {
    if let Some(lock) = ACTIVE_THEME.get() {
        *lock.write().unwrap_or_else(|e| e.into_inner()) = theme;
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
    fn theme_preference_system_resolves_without_creating_a_third_preset() {
        assert_eq!(
            ThemePreference::System.resolve(ThemePreset::Dark),
            ThemePreset::Dark
        );
        assert_eq!(
            ThemePreference::System.resolve(ThemePreset::Light),
            ThemePreset::Light
        );
        assert_eq!(ThemePreset::ALL.len(), 2);
        assert_eq!(ThemePreference::ALL.len(), 3);
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
    fn accessibility_preferences_scale_text_and_reduce_motion() {
        let theme = ThemePreset::Dark.build().with_accessibility(
            AccessibilityPreferences::default()
                .with_text_scale(1.25)
                .with_reduced_motion(true),
        );

        assert_eq!(theme.typography.body.font_size, 17.5);
        assert_eq!(theme.typography.body.line_height, 25.0);
        assert_eq!(theme.spacing.animation_duration_ms, 0);
        assert_eq!(
            theme.spacing.animation_ease,
            spacing::AnimationEasing::Linear
        );
    }

    #[test]
    fn accessibility_text_scale_is_clamped_and_nonfinite_safe() {
        let small = ThemePreset::Dark
            .build()
            .with_accessibility(AccessibilityPreferences::default().with_text_scale(0.1));
        let large = ThemePreset::Dark
            .build()
            .with_accessibility(AccessibilityPreferences::default().with_text_scale(4.0));
        let nonfinite = ThemePreset::Dark
            .build()
            .with_accessibility(AccessibilityPreferences::default().with_text_scale(f32::NAN));

        assert_eq!(small.typography.body.font_size, 14.0 * 0.85);
        assert_eq!(large.typography.body.font_size, 14.0 * 1.6);
        assert_eq!(nonfinite.typography.body.font_size, 14.0);
    }

    #[test]
    fn high_contrast_theme_strengthens_readable_tokens() {
        let dark = ThemePreset::Dark.build();
        let high_dark = dark
            .clone()
            .with_accessibility(AccessibilityPreferences::default().with_high_contrast(true));
        let light = ThemePreset::Light.build();
        let high_light = light
            .clone()
            .with_accessibility(AccessibilityPreferences::default().with_high_contrast(true));

        assert!(
            contrast_delta(high_dark.colors.background, high_dark.colors.border)
                > contrast_delta(dark.colors.background, dark.colors.border)
        );
        assert!(
            contrast_delta(high_dark.colors.background, high_dark.colors.input)
                > contrast_delta(dark.colors.background, dark.colors.input)
        );
        assert!(
            contrast_delta(high_light.colors.background, high_light.colors.border)
                > contrast_delta(light.colors.background, light.colors.border)
        );
        assert!(
            contrast_delta(high_light.colors.background, high_light.colors.input)
                > contrast_delta(light.colors.background, light.colors.input)
        );
        assert_eq!(high_dark.colors.foreground, mondrian_core::Color::WHITE);
        assert_eq!(high_light.colors.foreground, mondrian_core::Color::BLACK);
    }

    fn contrast_delta(background: mondrian_core::Color, foreground: mondrian_core::Color) -> f32 {
        (relative_luminance(composite_over(foreground, background))
            - relative_luminance(background))
        .abs()
    }

    fn composite_over(
        foreground: mondrian_core::Color,
        background: mondrian_core::Color,
    ) -> mondrian_core::Color {
        let inverse_alpha = 1.0 - foreground.a;
        mondrian_core::Color {
            r: foreground.r * foreground.a + background.r * inverse_alpha,
            g: foreground.g * foreground.a + background.g * inverse_alpha,
            b: foreground.b * foreground.a + background.b * inverse_alpha,
            a: 1.0,
        }
    }

    fn relative_luminance(color: mondrian_core::Color) -> f32 {
        0.2126 * color.r + 0.7152 * color.g + 0.0722 * color.b
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
    fn editor_domain_accent_tokens_are_available_per_theme() {
        for preset in ThemePreset::ALL {
            let colors = preset.build().colors;
            let accents = [
                colors.media_video,
                colors.media_audio,
                colors.media_adjustment,
                colors.media_solid,
                colors.effect_filter,
                colors.effect_lut,
                colors.effect_key,
                colors.effect_plugin,
                colors.effect_default,
                colors.node_source,
                colors.node_output,
            ];

            assert!(accents.iter().all(|color| color.a > 0.0));
            assert_ne!(colors.media_video, colors.media_audio);
            assert_ne!(colors.effect_filter, colors.effect_key);
            assert_ne!(colors.node_source, colors.node_output);
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
