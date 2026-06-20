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
    /// 模态弹窗背后的遮罩层
    pub modal_scrim: Color,

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

    /// 任意颜色背景上方控件把手的柔和阴影
    pub color_handle_shadow: Color,
    /// 任意颜色背景上方控件把手的强阴影
    pub color_handle_strong_shadow: Color,
    /// 任意颜色背景上方控件把手的浅色描边
    pub color_handle_outer: Color,
    /// 任意颜色背景上方控件把手的深色描边
    pub color_handle_inner: Color,
    /// 透明颜色预览棋盘格的浅色格
    pub checkerboard_light: Color,
    /// 透明颜色预览棋盘格的深色格
    pub checkerboard_dark: Color,
    /// Shell 取色器放大镜外圈背景
    pub eyedropper_overlay: Color,

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

    // ── 编辑器领域强调色 (Editor Domain Accents) ─────────────────────────
    /// 视频素材、嵌套序列等媒体对象的强调色
    pub media_video: Color,
    /// 音频素材的强调色
    pub media_audio: Color,
    /// 调整图层的强调色
    pub media_adjustment: Color,
    /// 纯色层/色彩素材的强调色
    pub media_solid: Color,
    /// 滤镜/模糊类效果的强调色
    pub effect_filter: Color,
    /// LUT / 调色查找表效果的强调色
    pub effect_lut: Color,
    /// 抠像效果的强调色
    pub effect_key: Color,
    /// 插件效果的强调色
    pub effect_plugin: Color,
    /// 默认效果节点强调色
    pub effect_default: Color,
    /// 节点图 source 节点强调色
    pub node_source: Color,
    /// 节点图 output 节点强调色
    pub node_output: Color,

    // ── 画布 (Canvas) ─────────────────────────────────────────────────────
    pub canvas: Color,
    /// 画布上的叠加层（安全区域、参考线）
    pub canvas_overlay: Color,

    // ── 滚动条 (Scrollbar) ────────────────────────────────────────────────
    pub scrollbar_thumb: Color,
}

impl ColorTokens {
    /// Dark 主题 — editor-grade dark surfaces with clear content hierarchy.
    pub fn dark() -> Self {
        Self {
            background: Color::from_hex(0x0B0D12),
            foreground: Color::from_hex(0xF4F4F5),
            card: Color::from_hex(0x151820),
            card_foreground: Color::from_hex(0xF4F4F5),
            popover: Color::from_hex(0x1D212B),
            popover_foreground: Color::from_hex(0xF4F4F5),
            modal_scrim: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.48 },

            primary: Color::from_hex(0x5DA7FF),
            primary_foreground: Color::WHITE,
            secondary: Color::from_hex(0x242936),
            secondary_foreground: Color::from_hex(0xE5E7EB),

            muted: Color::from_hex(0x191D26),
            muted_foreground: Color::from_hex(0xA0A7B5),
            accent: Color::from_hex(0x243246),
            accent_foreground: Color::from_hex(0xF4F4F5),
            color_handle_shadow: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.45 },
            color_handle_strong_shadow: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.8 },
            color_handle_outer: Color::WHITE,
            color_handle_inner: Color::BLACK,
            checkerboard_light: Color::from_hex(0xC5C8D1),
            checkerboard_dark: Color::from_hex(0x747B8A),
            eyedropper_overlay: Color::from_hex(0x151820),

            destructive: Color::from_hex(0x7F1D1D),
            destructive_foreground: Color::from_hex(0xFCA5A5),

            border: Color::from_hex(0x2A303C),
            input: Color::from_hex(0x303746),
            ring: Color::from_hex(0x73B4FF),

            success: Color::from_hex(0x22C55E),
            warning: Color::from_hex(0xF59E0B),
            error: Color::from_hex(0xEF4444),

            timeline_clip_video: Color::from_hex(0x235B91),
            timeline_clip_audio: Color::from_hex(0x20747D),
            timeline_playhead: Color::from_hex(0x73B4FF),
            media_video: Color::from_hex(0x4E8DF0),
            media_audio: Color::from_hex(0x2AA6A0),
            media_adjustment: Color::from_hex(0x8B78E6),
            media_solid: Color::from_hex(0xD75FE8),
            effect_filter: Color::from_hex(0x5DA7FF),
            effect_lut: Color::from_hex(0x22C55E),
            effect_key: Color::from_hex(0xF59E0B),
            effect_plugin: Color::from_hex(0xD946EF),
            effect_default: Color::from_hex(0x8B5CF6),
            node_source: Color::from_hex(0x4E8DF0),
            node_output: Color::from_hex(0x22C55E),

            canvas: Color::from_hex(0x05070B),
            canvas_overlay: Color { r: 1.0, g: 1.0, b: 1.0, a: 0.08 },

            scrollbar_thumb: Color::from_hex(0x586173),
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
            modal_scrim: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.22 },

            primary: Color::from_hex(0x2563EB),
            primary_foreground: Color::WHITE,
            secondary: Color::from_hex(0xF4F4F5),
            secondary_foreground: Color::from_hex(0x1A1A22),

            muted: Color::from_hex(0xF4F4F5),
            muted_foreground: Color::from_hex(0x717182),
            accent: Color::from_hex(0xF4F4F5),
            accent_foreground: Color::from_hex(0x1A1A22),
            color_handle_shadow: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.38 },
            color_handle_strong_shadow: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.72 },
            color_handle_outer: Color::WHITE,
            color_handle_inner: Color::BLACK,
            checkerboard_light: Color::from_hex(0xE5E7EB),
            checkerboard_dark: Color::from_hex(0xA1A1AA),
            eyedropper_overlay: Color::from_hex(0x1F1F24),

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
            media_video: Color::from_hex(0x2563EB),
            media_audio: Color::from_hex(0x0284C7),
            media_adjustment: Color::from_hex(0x7C3AED),
            media_solid: Color::from_hex(0xC026D3),
            effect_filter: Color::from_hex(0x2563EB),
            effect_lut: Color::from_hex(0x16A34A),
            effect_key: Color::from_hex(0xD97706),
            effect_plugin: Color::from_hex(0xC026D3),
            effect_default: Color::from_hex(0x7C3AED),
            node_source: Color::from_hex(0x2563EB),
            node_output: Color::from_hex(0x16A34A),

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
        if active {
            self.accent
        } else if hovered {
            self.muted
        } else {
            self.card
        }
    }

    /// 焦点边框选择
    pub fn border_for_state(&self, focused: bool) -> Color {
        if focused {
            self.ring
        } else {
            self.border
        }
    }

    /// 文字色：主要 / 弱化
    pub fn text_for_muted(&self, muted: bool) -> Color {
        if muted {
            self.muted_foreground
        } else {
            self.foreground
        }
    }
}
