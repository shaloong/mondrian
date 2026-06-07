//! 语义化颜色 Token — shadcn/ui 风格
//!
//! 每个 Token 描述 UI 用途，不描述具体色值。
//! 命名约定参考 shadcn/ui：每个有色表面都有对应的 foreground（文字色）。
//! 视觉风格参考 Apple Human Interface：低对比度边框、微妙层次、柔和阴影。

use mondrian_core::Color;

/// 语义化颜色面板
#[derive(Debug, Clone)]
pub struct ColorTokens {
    // ── 表面层级 (Surface Hierarchy) ──────────────────────────────────────
    /// 窗口/页面背景
    pub background: Color,
    /// 默认文字色
    pub foreground: Color,

    /// 卡片/面板背景（第一级抬升）
    pub card: Color,
    /// 卡片文字色
    pub card_foreground: Color,

    /// 弹出层背景（第二级抬升：dropdown, tooltip, popover）
    pub popover: Color,
    /// 弹出层文字色
    pub popover_foreground: Color,

    // ── 品牌 / 交互 (Brand & Interactive) ─────────────────────────────────
    /// 主色调（按钮、链接、选中态）
    pub primary: Color,
    /// 主色调上的文字
    pub primary_foreground: Color,

    /// 次要色调（次要按钮、标签）
    pub secondary: Color,
    /// 次要色调上的文字
    pub secondary_foreground: Color,

    // ── 辅助色 (Utility) ──────────────────────────────────────────────────
    /// 弱化背景（禁用的按钮、占位符区域）
    pub muted: Color,
    /// 弱化文字（辅助说明、placeholder）
    pub muted_foreground: Color,

    /// 强调背景（hover 高亮、选中项背景）
    pub accent: Color,
    /// 强调文字
    pub accent_foreground: Color,

    /// 危险/删除操作色
    pub destructive: Color,
    /// 危险色上的文字
    pub destructive_foreground: Color,

    // ── 边框 & 输入 (Borders & Inputs) ────────────────────────────────────
    /// 默认边框
    pub border: Color,
    /// 输入框边框
    pub input: Color,
    /// 聚焦环（focus ring）
    pub ring: Color,

    // ── 状态色 (Status) ───────────────────────────────────────────────────
    pub success: Color,
    pub warning: Color,
    pub error: Color,

    // ── 时间线专色 (Timeline) ─────────────────────────────────────────────
    pub timeline_clip_video: Color,
    pub timeline_clip_audio: Color,
    pub timeline_playhead: Color,

    // ── 画布 (Canvas) ─────────────────────────────────────────────────────
    pub canvas: Color,
    /// 画布上的叠加层（安全区域、参考线）
    pub canvas_overlay: Color,

    // ── 滚动条 (Scrollbar) ────────────────────────────────────────────────
    pub scrollbar_thumb: Color,
}

impl ColorTokens {
    /// Dark 主题 — 深色背景 + 高对比度文字
    pub fn dark() -> Self {
        Self {
            background: Color::from_hex(0x0B0B0E),
            foreground: Color::from_hex(0xEBEBF0),
            card: Color::from_hex(0x16161A),
            card_foreground: Color::from_hex(0xEBEBF0),
            popover: Color::from_hex(0x1C1C22),
            popover_foreground: Color::from_hex(0xEBEBF0),

            primary: Color::from_hex(0x3B82F6),
            primary_foreground: Color::WHITE,
            secondary: Color::from_hex(0x27272D),
            secondary_foreground: Color::from_hex(0xD4D4DB),

            muted: Color::from_hex(0x1C1C22),
            muted_foreground: Color::from_hex(0x717182),
            accent: Color::from_hex(0x27272D),
            accent_foreground: Color::from_hex(0xD4D4DB),

            destructive: Color::from_hex(0x7F1D1D),
            destructive_foreground: Color::from_hex(0xFCA5A5),

            border: Color::from_hex(0x27272D),
            input: Color::from_hex(0x27272D),
            ring: Color::from_hex(0x3B82F6),

            success: Color::from_hex(0x22C55E),
            warning: Color::from_hex(0xF59E0B),
            error: Color::from_hex(0xEF4444),

            timeline_clip_video: Color::from_hex(0x1E3A5F),
            timeline_clip_audio: Color::from_hex(0x1D587B),
            timeline_playhead: Color::from_hex(0x3B82F6),

            canvas: Color::BLACK,
            canvas_overlay: Color { r: 1.0, g: 1.0, b: 1.0, a: 0.08 },

            scrollbar_thumb: Color::from_hex(0x3F3F48),
        }
    }

    /// Light 主题 — 浅色背景 + 柔和对比度
    pub fn light() -> Self {
        Self {
            background: Color::WHITE,
            foreground: Color::from_hex(0x0B0B0E),
            card: Color::from_hex(0xF4F4F5),
            card_foreground: Color::from_hex(0x0B0B0E),
            popover: Color::WHITE,
            popover_foreground: Color::from_hex(0x0B0B0E),

            primary: Color::from_hex(0x2563EB),
            primary_foreground: Color::WHITE,
            secondary: Color::from_hex(0xF4F4F5),
            secondary_foreground: Color::from_hex(0x1A1A22),

            muted: Color::from_hex(0xF4F4F5),
            muted_foreground: Color::from_hex(0x717182),
            accent: Color::from_hex(0xF4F4F5),
            accent_foreground: Color::from_hex(0x1A1A22),

            destructive: Color::from_hex(0xFEE2E2),
            destructive_foreground: Color::from_hex(0x991B1B),

            border: Color::from_hex(0xE4E4E7),
            input: Color::from_hex(0xE4E4E7),
            ring: Color::from_hex(0x2563EB),

            success: Color::from_hex(0x16A34A),
            warning: Color::from_hex(0xD97706),
            error: Color::from_hex(0xDC2626),

            timeline_clip_video: Color::from_hex(0xDBEAFE),
            timeline_clip_audio: Color::from_hex(0xE0F2FE),
            timeline_playhead: Color::from_hex(0x2563EB),

            canvas: Color::from_hex(0x0B0B0E),
            canvas_overlay: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.06 },

            scrollbar_thumb: Color::from_hex(0xD4D4D8),
        }
    }

}

// ═══════════════════════════════════════════════════════════════════════════
// Convenience
// ═══════════════════════════════════════════════════════════════════════════

impl ColorTokens {
    pub fn to_wgpu(&self, color: &Color) -> [f32; 4] {
        [color.r, color.g, color.b, color.a]
    }

    /// Hover / Active / Normal 背景选择
    pub fn surface_for_state(&self, hovered: bool, active: bool) -> Color {
        if active { self.accent }
        else if hovered { self.muted }
        else { self.card }
    }

    /// 焦点边框选择
    pub fn border_for_state(&self, focused: bool) -> Color {
        if focused { self.ring } else { self.border }
    }

    /// 文字色：主要 / 弱化
    pub fn text_for_muted(&self, muted: bool) -> Color {
        if muted { self.muted_foreground } else { self.foreground }
    }
}
