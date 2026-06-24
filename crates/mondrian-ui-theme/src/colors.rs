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
    /// Secondary text used for panel titles, metadata, and quiet controls.
    pub text_secondary: Color,
    /// Tertiary text used for placeholder and empty-state body copy.
    pub text_tertiary: Color,
    /// Disabled text and icon color.
    pub text_disabled: Color,

    /// 卡片/面板背景（第一级抬升）
    pub card: Color,
    /// 卡片文字色
    pub card_foreground: Color,
    /// Alternate panel/header surface.
    pub panel_alt: Color,
    /// Top product chrome/titlebar surface.
    pub titlebar: Color,
    /// Control surface for inputs, buttons, chips, and compact chrome.
    pub surface: Color,
    /// Hover/active control surface.
    pub surface_2: Color,

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
    /// Native-window close hover surface.
    pub window_close_hover: Color,
    /// Native-window close pressed surface.
    pub window_close_pressed: Color,
    /// Native-window close icon color on hover/press.
    pub window_close_foreground: Color,

    // ── 边框 & 输入 (Borders & Inputs) ────────────────────────────────────
    /// 默认边框
    pub border: Color,
    /// Stronger border for elevated surfaces and active containment.
    pub border_strong: Color,
    /// 输入框边框
    pub input: Color,
    /// 聚焦环（focus ring）
    pub ring: Color,

    // ── 状态色 (Status) ───────────────────────────────────────────────────
    pub success: Color,
    pub warning: Color,
    pub error: Color,

    // ── 时间线专色 (Timeline) ─────────────────────────────────────────────
    /// 时间线轨道偶数行底色
    pub timeline_track_even: Color,
    /// 时间线轨道奇数行底色
    pub timeline_track_odd: Color,
    /// Timeline toolbar/ruler background.
    pub timeline_ruler: Color,
    /// Major timeline tick color.
    pub timeline_tick_major: Color,
    /// Minor timeline tick color.
    pub timeline_tick_minor: Color,
    /// Timeline in/out range fill.
    pub timeline_range_fill: Color,
    /// Timeline in/out range edge.
    pub timeline_range_edge: Color,
    /// Selected timeline clip outline.
    pub timeline_clip_selected_border: Color,
    /// Selected audio timeline clip outline.
    pub timeline_clip_audio_selected_border: Color,
    pub timeline_clip_video: Color,
    pub timeline_clip_video_hover: Color,
    pub timeline_clip_audio: Color,
    pub timeline_clip_audio_hover: Color,
    pub timeline_playhead: Color,
    /// Timeline range navigator track.
    pub timeline_navigator_track: Color,
    /// Timeline range navigator body in the normal state.
    pub timeline_navigator_body: Color,
    /// Timeline range navigator body while hovered.
    pub timeline_navigator_body_hover: Color,
    /// Timeline range navigator body while dragged.
    pub timeline_navigator_body_active: Color,
    /// Timeline range navigator handle in the normal state.
    pub timeline_navigator_handle: Color,
    /// Timeline range navigator handle while hovered.
    pub timeline_navigator_handle_hover: Color,
    /// Timeline range navigator handle while dragged.
    pub timeline_navigator_handle_active: Color,
    /// Timeline range navigator handle outline.
    pub timeline_navigator_handle_border: Color,

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
    /// Viewer stage background behind the fitted frame canvas.
    pub viewer_stage: Color,
    /// Viewer panel background around the stage and controls.
    pub viewer_panel: Color,
    /// 画布上的叠加层（安全区域、参考线）
    pub canvas_overlay: Color,
    /// Viewer outer safe/action guide.
    pub safe_guide: Color,
    /// Viewer inner/title safe guide.
    pub safe_guide_inner: Color,

    // ── 滚动条 (Scrollbar) ────────────────────────────────────────────────
    pub scrollbar_thumb: Color,
}

impl ColorTokens {
    /// Dark 主题 — editor-grade dark surfaces with clear content hierarchy.
    pub fn dark() -> Self {
        Self {
            background: Color::from_hex(0x101014),
            foreground: Color::from_hex(0xFAFAFA),
            text_secondary: Color::from_hex(0xA1A1AA),
            text_tertiary: Color::from_hex(0x71717A),
            text_disabled: Color::from_hex(0x52525B),
            card: Color::from_hex(0x101014),
            card_foreground: Color::from_hex(0xFAFAFA),
            panel_alt: Color::from_hex(0x18191E),
            titlebar: Color::from_hex(0x0B0B0E),
            surface: Color::from_hex(0x202126),
            surface_2: Color::from_hex(0x27272A),
            popover: Color::from_hex(0x18191E),
            popover_foreground: Color::from_hex(0xFAFAFA),
            modal_scrim: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.48 },

            primary: Color::from_hex(0x3B82F6),
            primary_foreground: Color::WHITE,
            secondary: Color::from_hex(0x202126),
            secondary_foreground: Color::from_hex(0xFAFAFA),

            muted: Color::from_hex(0x202126),
            muted_foreground: Color::from_hex(0xA1A1AA),
            accent: Color::from_hex(0x202126),
            accent_foreground: Color::from_hex(0xFAFAFA),
            color_handle_shadow: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.45 },
            color_handle_strong_shadow: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.8 },
            color_handle_outer: Color::WHITE,
            color_handle_inner: Color::BLACK,
            checkerboard_light: Color::from_hex(0x34353A),
            checkerboard_dark: Color::from_hex(0x2D2E32),
            eyedropper_overlay: Color::from_hex(0x101014),

            destructive: Color::from_hex(0x3A1718),
            destructive_foreground: Color::from_hex(0xFF453A),
            window_close_hover: Color::from_hex(0xE81123),
            window_close_pressed: Color::from_hex(0xC50F1F),
            window_close_foreground: Color::WHITE,

            border: Color { r: 1.0, g: 1.0, b: 1.0, a: 0.075 },
            border_strong: Color { r: 1.0, g: 1.0, b: 1.0, a: 0.12 },
            input: Color { r: 1.0, g: 1.0, b: 1.0, a: 0.075 },
            ring: Color::from_hex(0x3B82F6),

            success: Color::from_hex(0x48C774),
            warning: Color::from_hex(0xF5B84B),
            error: Color::from_hex(0xFF5D5D),

            timeline_track_even: Color::from_hex(0x15171D),
            timeline_track_odd: Color::from_hex(0x121318),
            timeline_ruler: Color::from_hex(0x101217),
            timeline_tick_major: Color { r: 1.0, g: 1.0, b: 1.0, a: 0.24 },
            timeline_tick_minor: Color { r: 1.0, g: 1.0, b: 1.0, a: 0.10 },
            timeline_range_fill: Color { r: 1.0, g: 1.0, b: 1.0, a: 0.032 },
            timeline_range_edge: Color { r: 0.231, g: 0.510, b: 0.965, a: 0.55 },
            timeline_clip_selected_border: Color::from_hex(0x80CFFF),
            timeline_clip_audio_selected_border: Color::from_hex(0xA2D7AF),
            timeline_clip_video: Color::from_hex(0x2F74A0),
            timeline_clip_video_hover: Color::from_hex(0x3783B3),
            timeline_clip_audio: Color::from_hex(0x547A5F),
            timeline_clip_audio_hover: Color::from_hex(0x618A6C),
            timeline_playhead: Color::from_hex(0x3B82F6),
            timeline_navigator_track: Color::from_hex(0x15171C),
            timeline_navigator_body: Color::from_hex(0x3A3C43),
            timeline_navigator_body_hover: Color::from_hex(0x474A52),
            timeline_navigator_body_active: Color::from_hex(0x535660),
            timeline_navigator_handle: Color::from_hex(0x666A73),
            timeline_navigator_handle_hover: Color::from_hex(0x858A95),
            timeline_navigator_handle_active: Color::from_hex(0x9DA3AF),
            timeline_navigator_handle_border: Color { r: 1.0, g: 1.0, b: 1.0, a: 0.10 },
            media_video: Color::from_hex(0x3B82F6),
            media_audio: Color::from_hex(0x5BC9BE),
            media_adjustment: Color::from_hex(0x765AA6),
            media_solid: Color::from_hex(0x7A5B42),
            effect_filter: Color::from_hex(0x5DA7FF),
            effect_lut: Color::from_hex(0x22C55E),
            effect_key: Color::from_hex(0xF59E0B),
            effect_plugin: Color::from_hex(0xD946EF),
            effect_default: Color::from_hex(0x8B5CF6),
            node_source: Color::from_hex(0x4E8DF0),
            node_output: Color::from_hex(0x22C55E),

            canvas: Color::from_hex(0x3A3B3F),
            viewer_stage: Color::from_hex(0x0D0E11),
            viewer_panel: Color::from_hex(0x101014),
            canvas_overlay: Color { r: 1.0, g: 1.0, b: 1.0, a: 0.08 },
            safe_guide: Color { r: 1.0, g: 1.0, b: 1.0, a: 0.13 },
            safe_guide_inner: Color { r: 1.0, g: 1.0, b: 1.0, a: 0.08 },

            scrollbar_thumb: Color::from_hex(0xFFFFFF),
        }
    }

    /// Light 主题 — 浅色背景 + 柔和对比度
    pub fn light() -> Self {
        Self {
            background: Color::WHITE,
            foreground: Color::from_hex(0x0B0B0E),
            text_secondary: Color::from_hex(0x4B5563),
            text_tertiary: Color::from_hex(0x717182),
            text_disabled: Color::from_hex(0xA1A1AA),
            card: Color::from_hex(0xF4F4F5),
            card_foreground: Color::from_hex(0x0B0B0E),
            panel_alt: Color::from_hex(0xECEEF2),
            titlebar: Color::from_hex(0xFFFFFF),
            surface: Color::from_hex(0xFFFFFF),
            surface_2: Color::from_hex(0xE8ECF3),
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
            window_close_hover: Color::from_hex(0xE81123),
            window_close_pressed: Color::from_hex(0xC50F1F),
            window_close_foreground: Color::WHITE,

            border: Color::from_hex(0xE4E4E7),
            border_strong: Color::from_hex(0xCBD5E1),
            input: Color::from_hex(0xE4E4E7),
            ring: Color::from_hex(0x2563EB),

            success: Color::from_hex(0x16A34A),
            warning: Color::from_hex(0xD97706),
            error: Color::from_hex(0xDC2626),

            timeline_track_even: Color::from_hex(0xF8FAFC),
            timeline_track_odd: Color::from_hex(0xF1F5F9),
            timeline_ruler: Color::from_hex(0xEEF2F7),
            timeline_tick_major: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.28 },
            timeline_tick_minor: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.12 },
            timeline_range_fill: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.045 },
            timeline_range_edge: Color { r: 0.145, g: 0.388, b: 0.922, a: 0.72 },
            timeline_clip_selected_border: Color::from_hex(0x2563EB),
            timeline_clip_audio_selected_border: Color::from_hex(0x15803D),
            timeline_clip_video: Color::from_hex(0xDBEAFE),
            timeline_clip_video_hover: Color::from_hex(0xBFDBFE),
            timeline_clip_audio: Color::from_hex(0xE0F2FE),
            timeline_clip_audio_hover: Color::from_hex(0xBAE6FD),
            timeline_playhead: Color::from_hex(0x2563EB),
            timeline_navigator_track: Color::from_hex(0xE4E4E7),
            timeline_navigator_body: Color::from_hex(0xA1A1AA),
            timeline_navigator_body_hover: Color::from_hex(0x8B8B96),
            timeline_navigator_body_active: Color::from_hex(0x71717A),
            timeline_navigator_handle: Color::from_hex(0x71717A),
            timeline_navigator_handle_hover: Color::from_hex(0x52525B),
            timeline_navigator_handle_active: Color::from_hex(0x3F3F46),
            timeline_navigator_handle_border: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.12 },
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
            viewer_stage: Color::from_hex(0xE7EAF0),
            viewer_panel: Color::from_hex(0xF1F5F9),
            canvas_overlay: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.06 },
            safe_guide: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.18 },
            safe_guide_inner: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.12 },

            scrollbar_thumb: Color::from_hex(0xD4D4D8),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Convenience
// ═══════════════════════════════════════════════════════════════════════════

impl ColorTokens {
    /// Return a high-contrast variant of these semantic color tokens.
    pub fn with_high_contrast(mut self) -> Self {
        if relative_luminance(self.background) < 0.5 {
            self.foreground = Color::WHITE;
            self.card_foreground = Color::WHITE;
            self.popover_foreground = Color::WHITE;
            self.primary_foreground = Color::WHITE;
            self.secondary_foreground = Color::WHITE;
            self.accent_foreground = Color::WHITE;
            self.text_secondary = Color::from_hex(0xE4E4E7);
            self.text_tertiary = Color::from_hex(0xD4D4D8);
            self.text_disabled = Color::from_hex(0xA1A1AA);
            self.muted_foreground = Color::from_hex(0xE4E4E7);
            self.border = Color { r: 1.0, g: 1.0, b: 1.0, a: 0.28 };
            self.border_strong = Color { r: 1.0, g: 1.0, b: 1.0, a: 0.42 };
            self.input = Color { r: 1.0, g: 1.0, b: 1.0, a: 0.32 };
            self.ring = Color::from_hex(0x60A5FA);
            self.timeline_tick_major = Color { r: 1.0, g: 1.0, b: 1.0, a: 0.42 };
            self.timeline_tick_minor = Color { r: 1.0, g: 1.0, b: 1.0, a: 0.20 };
            self.safe_guide = Color { r: 1.0, g: 1.0, b: 1.0, a: 0.26 };
            self.safe_guide_inner = Color { r: 1.0, g: 1.0, b: 1.0, a: 0.18 };
            self.scrollbar_thumb = Color::WHITE;
        } else {
            self.foreground = Color::BLACK;
            self.card_foreground = Color::BLACK;
            self.popover_foreground = Color::BLACK;
            self.secondary_foreground = Color::from_hex(0x111827);
            self.accent_foreground = Color::from_hex(0x111827);
            self.text_secondary = Color::from_hex(0x1F2937);
            self.text_tertiary = Color::from_hex(0x374151);
            self.text_disabled = Color::from_hex(0x4B5563);
            self.muted_foreground = Color::from_hex(0x1F2937);
            self.border = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.22 };
            self.border_strong = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.34 };
            self.input = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.28 };
            self.ring = Color::from_hex(0x1D4ED8);
            self.timeline_tick_major = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.44 };
            self.timeline_tick_minor = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.22 };
            self.safe_guide = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.30 };
            self.safe_guide_inner = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.22 };
            self.scrollbar_thumb = Color::from_hex(0x52525B);
        }
        self
    }

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

fn relative_luminance(color: Color) -> f32 {
    0.2126 * color.r + 0.7152 * color.g + 0.0722 * color.b
}
