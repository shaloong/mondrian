//! 间距、圆角、阴影 Token
//!
//! 所有 UI 尺寸从这里获取，禁止硬编码数值。

/// 间距 Token 集合 —— 所有间隙、内边距、外边距
#[derive(Debug, Clone)]
pub struct SpacingTokens {
    // 基础间距阶梯
    pub xs: f32,
    pub sm: f32,
    pub md: f32,
    pub lg: f32,
    pub xl: f32,
    pub xxl: f32,

    // 圆角阶梯
    pub radius_none: f32,
    pub radius_sm: f32,
    pub radius_md: f32,
    pub radius_lg: f32,
    pub radius_xl: f32,
    pub radius_full: f32, // 胶囊形

    // 阴影定义
    pub shadow_none: ShadowToken,
    pub shadow_sm: ShadowToken,
    pub shadow_md: ShadowToken,
    pub shadow_lg: ShadowToken,
    pub shadow_xl: ShadowToken,

    // 面板间距
    pub panel_gap: f32,
    pub panel_inner_margin: (f32, f32),

    // 部件高度
    pub interact_height: f32, // 按钮/输入框标准高度
    pub icon_size: f32,

    // 边框
    pub border_standard: f32,
    pub border_emphasis: f32,

    // 时间线专属
    pub timeline_track_height: f32,
    pub timeline_ruler_height: f32,
    pub timeline_clip_radius: f32,
    pub timeline_track_label_width: f32,
    pub timeline_scrollbar_size: f32,
    pub timeline_default_pixels_per_frame: f32,

    // 检查器
    pub inspector_panel_width: f32,
    pub inspector_group_header_height: f32,
    pub property_row_height: f32,

    // 列表
    pub list_row_height: f32,
    pub list_row_radius: f32,

    // 导出
    pub export_grid_spacing: (f32, f32),

    // 工具提示
    pub tooltip_offset: f32,
    pub tooltip_delay_ms: u64,
    pub tooltip_max_width: f32,

    // 动画
    pub animation_duration_ms: u64,
    pub animation_ease: AnimationEasing,
}

/// 阴影定义
#[derive(Debug, Clone)]
pub struct ShadowToken {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: [f32; 4], // rgba
}

/// 动画缓动
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationEasing {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    Spring,
}

impl SpacingTokens {
    /// Return spacing tokens with nonessential theme animation disabled.
    pub fn with_reduced_motion(mut self) -> Self {
        self.animation_duration_ms = 0;
        self.animation_ease = AnimationEasing::Linear;
        self
    }
}

impl Default for SpacingTokens {
    fn default() -> Self {
        Self {
            xs: 4.0,
            sm: 6.0,
            md: 10.0,
            lg: 28.0,
            xl: 48.0,
            xxl: 80.0,

            radius_none: 0.0,
            radius_sm: 6.0,
            radius_md: 8.0,
            radius_lg: 10.0,
            radius_xl: 14.0,
            radius_full: 999.0,

            shadow_none: ShadowToken {
                offset_x: 0.0,
                offset_y: 0.0,
                blur: 0.0,
                spread: 0.0,
                color: [0.0, 0.0, 0.0, 0.0],
            },
            shadow_sm: ShadowToken {
                offset_x: 0.0,
                offset_y: 1.0,
                blur: 3.0,
                spread: 0.0,
                color: [0.0, 0.0, 0.0, 0.12],
            },
            shadow_md: ShadowToken {
                offset_x: 0.0,
                offset_y: 8.0,
                blur: 24.0,
                spread: 0.0,
                color: [0.0, 0.0, 0.0, 0.35],
            },
            shadow_lg: ShadowToken {
                offset_x: 0.0,
                offset_y: 8.0,
                blur: 24.0,
                spread: 0.0,
                color: [0.0, 0.0, 0.0, 0.24],
            },
            shadow_xl: ShadowToken {
                offset_x: 0.0,
                offset_y: 16.0,
                blur: 48.0,
                spread: 0.0,
                color: [0.0, 0.0, 0.0, 0.32],
            },

            panel_gap: 10.0,
            panel_inner_margin: (12.0, 12.0),

            interact_height: 28.0,
            icon_size: 14.0,

            border_standard: 1.0,
            border_emphasis: 2.0,

            timeline_track_height: 42.0,
            timeline_ruler_height: 28.0,
            timeline_clip_radius: 4.0,
            timeline_track_label_width: 96.0,
            timeline_scrollbar_size: 8.0,
            timeline_default_pixels_per_frame: 4.0,

            inspector_panel_width: 344.0,
            inspector_group_header_height: 28.0,
            property_row_height: 28.0,

            list_row_height: 36.0,
            list_row_radius: 6.0,

            export_grid_spacing: (12.0, 4.0),

            tooltip_offset: 8.0,
            tooltip_delay_ms: 450,
            tooltip_max_width: 280.0,

            animation_duration_ms: 200,
            animation_ease: AnimationEasing::EaseInOut,
        }
    }
}
