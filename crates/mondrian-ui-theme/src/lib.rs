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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typography::FontWeight;

    // ═══════════════════════════════════════════════════════════════════════
    // ThemePreset
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn all_presets_have_unique_display_names() {
        let names: Vec<&str> = ThemePreset::ALL.iter().map(|p| p.display_name()).collect();
        let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
        assert_eq!(names.len(), unique.len());
    }

    #[test]
    fn each_preset_builds_with_correct_name() {
        assert_eq!(ThemePreset::Dark.build().name, "Dark");
        assert_eq!(ThemePreset::Light.build().name, "Light");
        assert_eq!(ThemePreset::Resolve.build().name, "Resolve");
        assert_eq!(ThemePreset::Premiere.build().name, "Premiere");
        assert_eq!(ThemePreset::Fusion.build().name, "Fusion");
        assert_eq!(ThemePreset::Custom.build().name, "Custom");
    }

    #[test]
    fn all_presets_build_without_panicking() {
        for preset in ThemePreset::ALL {
            let theme = preset.build();
            assert!(!theme.name.is_empty());
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Theme global switching
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn global_theme_can_be_read() {
        let theme = current_theme();
        assert_eq!(theme.name, "Dark"); // default
    }

    #[test]
    fn global_theme_can_be_switched_to_light() {
        set_theme_preset(ThemePreset::Light);
        assert_eq!(current_theme().name, "Light");
        // Restore for other tests
        set_theme_preset(ThemePreset::Dark);
    }

    #[test]
    fn global_theme_can_be_set_directly() {
        let custom = Theme {
            name: "TestTheme".into(),
            colors: ColorTokens::dark(),
            typography: TypographyTokens::default(),
            spacing: SpacingTokens::default(),
        };
        set_theme(custom);
        assert_eq!(current_theme().name, "TestTheme");
        // Restore
        set_theme_preset(ThemePreset::Dark);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Theme structural correctness
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn dark_theme_colors_are_dark() {
        let theme = ThemePreset::Dark.build();
        let base = theme.colors.bg_base;
        // Dark theme background should be dark (low luminance)
        let luminance = 0.2126 * base.r + 0.7152 * base.g + 0.0722 * base.b;
        assert!(luminance < 0.3, "Dark theme bg_base should be dark, got luminance {luminance}");
    }

    #[test]
    fn light_theme_colors_are_light() {
        let theme = ThemePreset::Light.build();
        let base = theme.colors.bg_base;
        let luminance = 0.2126 * base.r + 0.7152 * base.g + 0.0722 * base.b;
        assert!(luminance > 0.7, "Light theme bg_base should be light, got luminance {luminance}");
    }

    #[test]
    fn light_and_dark_themes_are_distinct() {
        let dark = ThemePreset::Dark.build();
        let light = ThemePreset::Light.build();
        // Backgrounds should differ substantially
        let diff = (dark.colors.bg_base.r - light.colors.bg_base.r).abs()
            + (dark.colors.bg_base.g - light.colors.bg_base.g).abs()
            + (dark.colors.bg_base.b - light.colors.bg_base.b).abs();
        assert!(diff > 1.0, "Dark and Light themes should have visibly different backgrounds");
    }

    #[test]
    fn all_color_presets_have_valid_alpha() {
        for preset in ThemePreset::ALL {
            let theme = preset.build();
            // Check overlay colors have valid alpha
            assert!(theme.colors.overlay_fill.a >= 0.0 && theme.colors.overlay_fill.a <= 1.0);
            assert!(theme.colors.overlay_stroke.a >= 0.0 && theme.colors.overlay_stroke.a <= 1.0);
        }
    }

    #[test]
    fn each_preset_has_distinct_accent_colors() {
        // Resolve=blue, Premiere=magenta, Fusion=orange
        let resolve = ThemePreset::Resolve.build();
        let premiere = ThemePreset::Premiere.build();
        let fusion = ThemePreset::Fusion.build();

        // Accent colors should be different across presets
        let r1 = resolve.colors.interaction_highlight;
        let r2 = premiere.colors.interaction_highlight;
        let r3 = fusion.colors.interaction_highlight;

        assert_ne!((r1.r, r1.g, r1.b), (r2.r, r2.g, r2.b));
        assert_ne!((r2.r, r2.g, r2.b), (r3.r, r3.g, r3.b));
        assert_ne!((r1.r, r1.g, r1.b), (r3.r, r3.g, r3.b));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Spacing tokens
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn spacing_scale_is_monotonic() {
        let s = SpacingTokens::default();
        assert!(s.xs < s.sm);
        assert!(s.sm < s.md);
        // lg=28, xl=48: strict monotonic fits default values; if custom, just non-decreasing
        assert!(s.md <= s.lg);
        assert!(s.lg <= s.xl);
        assert!(s.xl <= s.xxl);
    }

    #[test]
    fn spacing_radius_scale_is_monotonic() {
        let s = SpacingTokens::default();
        assert!(s.radius_none < s.radius_sm);
        assert!(s.radius_sm < s.radius_md);
        assert!(s.radius_md <= s.radius_lg);
        assert!(s.radius_lg <= s.radius_xl);
        assert!(s.radius_xl < s.radius_full);
    }

    #[test]
    fn spacing_defaults_are_positive() {
        let s = SpacingTokens::default();
        assert!(s.xs > 0.0);
        assert!(s.interact_height > 0.0);
        assert!(s.timeline_track_height > 0.0);
        assert!(s.timeline_ruler_height > 0.0);
        assert!(s.panel_gap > 0.0);
    }

    #[test]
    fn shadow_tokens_have_reasonable_values() {
        let s = SpacingTokens::default();
        // shadow_none should be all zeros
        assert_eq!(s.shadow_none.offset_x, 0.0);
        assert_eq!(s.shadow_none.offset_y, 0.0);
        assert_eq!(s.shadow_none.blur, 0.0);
        // shadow_xl should be larger than shadow_md
        assert!(s.shadow_xl.blur > s.shadow_md.blur);
        assert!(s.shadow_xl.offset_y > s.shadow_md.offset_y);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Typography tokens
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn typography_defaults_are_positive() {
        let t = TypographyTokens::default();
        assert!(t.body.font_size > 0.0);
        assert!(t.body.line_height > 0.0);
        assert!(t.body.line_height >= t.body.font_size, "line_height should be >= font_size");
    }

    #[test]
    fn typography_headings_are_larger_than_body() {
        let t = TypographyTokens::default();
        assert!(t.heading_h1.font_size > t.heading_h2.font_size);
        assert!(t.heading_h2.font_size > t.heading_h3.font_size);
        assert!(t.heading_h3.font_size > t.body.font_size);
    }

    #[test]
    fn typography_mono_large_is_large() {
        let t = TypographyTokens::default();
        assert!(t.mono_large.font_size > t.mono_small.font_size);
        assert_eq!(t.mono_large.font_weight, FontWeight::Bold);
    }

    #[test]
    fn font_weight_css_values() {
        assert_eq!(FontWeight::Light.to_css_value(), 300.0);
        assert_eq!(FontWeight::Regular.to_css_value(), 400.0);
        assert_eq!(FontWeight::Medium.to_css_value(), 500.0);
        assert_eq!(FontWeight::Semibold.to_css_value(), 600.0);
        assert_eq!(FontWeight::Bold.to_css_value(), 700.0);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // ColorTokens convenience methods
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn bg_for_state_active_overrides_hover() {
        let colors = ColorTokens::dark();
        // active=true, hovered=true → should return active
        let active = colors.bg_for_state(true, true);
        assert_eq!(active, colors.bg_surface_active);
        // active=true, hovered=false → still active
        let active2 = colors.bg_for_state(false, true);
        assert_eq!(active2, colors.bg_surface_active);
    }

    #[test]
    fn bg_for_state_hovered() {
        let colors = ColorTokens::dark();
        let hovered = colors.bg_for_state(true, false);
        assert_eq!(hovered, colors.bg_surface_hover);
    }

    #[test]
    fn bg_for_state_normal() {
        let colors = ColorTokens::dark();
        let normal = colors.bg_for_state(false, false);
        assert_eq!(normal, colors.bg_surface);
    }

    #[test]
    fn border_for_state_focused() {
        let colors = ColorTokens::dark();
        let focused = colors.border_for_state(true);
        assert_eq!(focused, colors.interaction_highlight);
    }

    #[test]
    fn border_for_state_unfocused() {
        let colors = ColorTokens::dark();
        let unfocused = colors.border_for_state(false);
        assert_eq!(unfocused, colors.border_subtle);
    }

    #[test]
    fn text_for_muted_primary() {
        let colors = ColorTokens::dark();
        assert_eq!(colors.text_for_muted(false), colors.text_primary);
    }

    #[test]
    fn text_for_muted_muted() {
        let colors = ColorTokens::dark();
        assert_eq!(colors.text_for_muted(true), colors.text_muted);
    }

    #[test]
    fn to_wgpu_converts_to_f32_array() {
        let color = mondrian_core::Color {
            r: 0.1,
            g: 0.2,
            b: 0.3,
            a: 0.4,
        };
        let arr = ColorTokens::dark().to_wgpu(&color);
        assert_eq!(arr, [0.1, 0.2, 0.3, 0.4]);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Thread safety
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn theme_can_be_sent_between_threads() {
        let theme = ThemePreset::Dark.build();
        std::thread::spawn(move || {
            assert_eq!(theme.name, "Dark");
        })
        .join()
        .unwrap();
    }

    #[test]
    fn global_theme_is_accessible_from_spawned_thread() {
        // First read on main thread to ensure initialized
        let _guard = current_theme();
        std::thread::spawn(|| {
            let theme = current_theme();
            let _name = &theme.name;
        })
        .join()
        .unwrap();
    }
}
