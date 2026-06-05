//! 语义化颜色 Token
//!
//! 每个 Token 描述 UI 用途，而非具体色值。
//! Dark/Light 变体提供完整的双色方案。
//! Resolve/Premiere/Fusion 变体模仿对应编辑器的默认风格。

use mondrian_core::Color;

/// 语义化颜色面板 —— 所有 UI 颜色从这里获取
#[derive(Debug, Clone)]
pub struct ColorTokens {
    // 背景层次
    pub bg_base: Color,
    pub bg_surface: Color,
    pub bg_surface_raised: Color,
    pub bg_surface_hover: Color,
    pub bg_surface_active: Color,

    // 边框
    pub border_subtle: Color,
    pub border_emphasis: Color,
    pub panel_divider_strong: Color,

    // 文本
    pub text_primary: Color,
    pub text_muted: Color,

    // 状态色
    pub status_warning: Color,
    pub status_success: Color,
    pub status_error: Color,

    // 交互
    pub interaction_highlight: Color,
    pub accent_secondary: Color,
    pub accent_audio: Color,

    // 时间线专属
    pub timeline_clip_video: Color,
    pub timeline_clip_audio: Color,
    pub timeline_playhead: Color,

    // 画布
    pub canvas_bg: Color,
    pub image_tint: Color,

    // 覆盖层
    pub overlay_fill: Color,
    pub overlay_stroke: Color,
}

impl ColorTokens {
    pub fn dark() -> Self {
        Self {
            bg_base: Color::from_hex(0x121212),
            bg_surface: Color::from_hex(0x1E1E1E),
            bg_surface_raised: Color::from_hex(0x252527),
            bg_surface_hover: Color::from_hex(0x2B2B30),
            bg_surface_active: Color::from_hex(0x333338),
            border_subtle: Color::from_hex(0x3A3A3C),
            border_emphasis: Color::from_hex(0x4C4C52),
            panel_divider_strong: Color::from_hex(0x2F2F33),
            text_primary: Color::from_hex(0xF2F2F2),
            text_muted: Color::from_hex(0x767680),
            status_warning: Color::from_hex(0xF58220),
            status_success: Color::from_hex(0x73D18F),
            status_error: Color::from_hex(0xE36D6D),
            interaction_highlight: Color::from_hex(0x006EFF),
            accent_secondary: Color::from_hex(0x0A3565),
            accent_audio: Color::from_hex(0x5AC8FA),
            timeline_clip_video: Color::from_hex(0x0A3565),
            timeline_clip_audio: Color::from_hex(0x1D587B),
            timeline_playhead: Color::from_hex(0x006EFF),
            canvas_bg: Color::BLACK,
            image_tint: Color::WHITE,
            overlay_fill: Color { r: 0.071, g: 0.071, b: 0.071, a: 0.91 },
            overlay_stroke: Color { r: 0.353, g: 0.784, b: 0.980, a: 0.627 },
        }
    }

    pub fn light() -> Self {
        Self {
            bg_base: Color::from_hex(0xFFFFFF),
            bg_surface: Color::from_hex(0xFFFFFF),
            bg_surface_raised: Color::from_hex(0xF7F9FC),
            bg_surface_hover: Color::from_hex(0xECF1F7),
            bg_surface_active: Color::from_hex(0xECF1F7),
            border_subtle: Color::from_hex(0xD3DAE4),
            border_emphasis: Color::from_hex(0xB4C0CE),
            panel_divider_strong: Color::from_hex(0xC8D1DC),
            text_primary: Color::from_hex(0x202733),
            text_muted: Color::from_hex(0x677486),
            status_warning: Color::from_hex(0xD4740A),
            status_success: Color::from_hex(0x1D8948),
            status_error: Color::from_hex(0xC13C3C),
            interaction_highlight: Color::from_hex(0x0A63D8),
            accent_secondary: Color::from_hex(0x1F5089),
            accent_audio: Color::from_hex(0x1F95CB),
            timeline_clip_video: Color::from_hex(0x3F6EB1),
            timeline_clip_audio: Color::from_hex(0x4A92BC),
            timeline_playhead: Color::from_hex(0x0A63D8),
            canvas_bg: Color::from_hex(0x141414),
            image_tint: Color::WHITE,
            overlay_fill: Color { r: 0.094, g: 0.133, b: 0.188, a: 0.847 },
            overlay_stroke: Color { r: 0.039, g: 0.388, b: 0.847, a: 0.533 },
        }
    }

    /// Resolve 风格 —— 深灰背景 + 蓝色强调 + 高对比度
    pub fn resolve() -> Self {
        Self {
            bg_base: Color::from_hex(0x1A1A1A),
            bg_surface: Color::from_hex(0x232323),
            bg_surface_raised: Color::from_hex(0x2A2A2A),
            bg_surface_hover: Color::from_hex(0x333333),
            bg_surface_active: Color::from_hex(0x3D3D3D),
            border_subtle: Color::from_hex(0x383838),
            border_emphasis: Color::from_hex(0x505050),
            panel_divider_strong: Color::from_hex(0x333333),
            text_primary: Color::from_hex(0xE6E6E6),
            text_muted: Color::from_hex(0x7A7A7A),
            status_warning: Color::from_hex(0xF5A623),
            status_success: Color::from_hex(0x6DD98A),
            status_error: Color::from_hex(0xE05555),
            interaction_highlight: Color::from_hex(0x2979FF),
            accent_secondary: Color::from_hex(0x0D47A1),
            accent_audio: Color::from_hex(0x40C4FF),
            timeline_clip_video: Color::from_hex(0x0D47A1),
            timeline_clip_audio: Color::from_hex(0x1565C0),
            timeline_playhead: Color::from_hex(0x2979FF),
            canvas_bg: Color::BLACK,
            image_tint: Color::WHITE,
            overlay_fill: Color { r: 0.1, g: 0.1, b: 0.1, a: 0.9 },
            overlay_stroke: Color { r: 0.16, g: 0.47, b: 1.0, a: 0.6 },
        }
    }

    /// Premiere 风格 —— 深灰紫背景 + 品红强调
    pub fn premiere() -> Self {
        Self {
            bg_base: Color::from_hex(0x1E1E28),
            bg_surface: Color::from_hex(0x262631),
            bg_surface_raised: Color::from_hex(0x2E2E3A),
            bg_surface_hover: Color::from_hex(0x363645),
            bg_surface_active: Color::from_hex(0x404052),
            border_subtle: Color::from_hex(0x3A3A48),
            border_emphasis: Color::from_hex(0x505060),
            panel_divider_strong: Color::from_hex(0x343440),
            text_primary: Color::from_hex(0xE8E8F0),
            text_muted: Color::from_hex(0x808090),
            status_warning: Color::from_hex(0xE8A840),
            status_success: Color::from_hex(0x50C878),
            status_error: Color::from_hex(0xD84860),
            interaction_highlight: Color::from_hex(0x8B4C9E),
            accent_secondary: Color::from_hex(0x6A3D7C),
            accent_audio: Color::from_hex(0x50B8E0),
            timeline_clip_video: Color::from_hex(0x6A3D7C),
            timeline_clip_audio: Color::from_hex(0x3D6B7C),
            timeline_playhead: Color::from_hex(0x8B4C9E),
            canvas_bg: Color::BLACK,
            image_tint: Color::WHITE,
            overlay_fill: Color { r: 0.118, g: 0.118, b: 0.157, a: 0.9 },
            overlay_stroke: Color { r: 0.545, g: 0.298, b: 0.620, a: 0.6 },
        }
    }

    /// Fusion 风格 —— 深灰背景 + 橙黄强调（类似 DaVinci Fusion）
    pub fn fusion() -> Self {
        Self {
            bg_base: Color::from_hex(0x181818),
            bg_surface: Color::from_hex(0x212121),
            bg_surface_raised: Color::from_hex(0x292929),
            bg_surface_hover: Color::from_hex(0x323232),
            bg_surface_active: Color::from_hex(0x3C3C3C),
            border_subtle: Color::from_hex(0x363636),
            border_emphasis: Color::from_hex(0x4E4E4E),
            panel_divider_strong: Color::from_hex(0x303030),
            text_primary: Color::from_hex(0xE5E5E5),
            text_muted: Color::from_hex(0x787878),
            status_warning: Color::from_hex(0xF5A623),
            status_success: Color::from_hex(0x6BBF6B),
            status_error: Color::from_hex(0xDE5A5A),
            interaction_highlight: Color::from_hex(0xF0A030),
            accent_secondary: Color::from_hex(0xB87820),
            accent_audio: Color::from_hex(0x40B8E0),
            timeline_clip_video: Color::from_hex(0xB87820),
            timeline_clip_audio: Color::from_hex(0x5A8A40),
            timeline_playhead: Color::from_hex(0xF0A030),
            canvas_bg: Color::BLACK,
            image_tint: Color::WHITE,
            overlay_fill: Color { r: 0.094, g: 0.094, b: 0.094, a: 0.9 },
            overlay_stroke: Color { r: 0.941, g: 0.627, b: 0.188, a: 0.6 },
        }
    } // end fusion()
} // end impl ColorTokens

// ═══════════════════════════════════════════════════════════════════════════════════
// 便捷方法
// ═══════════════════════════════════════════════════════════════════════════════════

impl ColorTokens {
    /// 将语义颜色转为 wgpu 兼容的 f32 数组
    pub fn to_wgpu(&self, color: &mondrian_core::Color) -> [f32; 4] {
        [color.r, color.g, color.b, color.a]
    }

    /// 根据交互状态选择背景色
    pub fn bg_for_state(&self, hovered: bool, active: bool) -> mondrian_core::Color {
        if active {
            self.bg_surface_active
        } else if hovered {
            self.bg_surface_hover
        } else {
            self.bg_surface
        }
    }

    /// 根据焦点状态选择边框色
    pub fn border_for_state(&self, focused: bool) -> mondrian_core::Color {
        if focused {
            self.interaction_highlight
        } else {
            self.border_subtle
        }
    }

    /// 根据重要程度选择文字色
    pub fn text_for_muted(&self, muted: bool) -> mondrian_core::Color {
        if muted {
            self.text_muted
        } else {
            self.text_primary
        }
    }
}
