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
    pub timeline_scrollbar_min_thumb: f32,
    pub timeline_scrollbar_handle_size: f32,
    pub timeline_scrollbar_handle_visual_size: f32,
    pub timeline_scrollbar_track_visual_thickness: f32,
    pub timeline_scrollbar_body_visual_thickness: f32,
    pub timeline_tool_button_size: f32,
    pub timeline_tool_button_gap: f32,
    pub timeline_toolbar_height: f32,
    pub timeline_toolbar_group_gap: f32,
    pub timeline_content_trailing_padding: f32,
    pub timeline_min_track_height: f32,
    pub timeline_max_track_height: f32,
    pub timeline_track_header_min_width: f32,

    // 检查器
    pub inspector_panel_width: f32,
    pub inspector_group_header_height: f32,
    pub property_row_height: f32,

    // 查看器
    pub viewer_default_width: f32,
    pub viewer_default_height: f32,
    pub viewer_transport_button_size: f32,
    pub viewer_transport_button_gap: f32,
    pub viewer_dropdown_row_height: f32,
    pub viewer_dropdown_padding_x: f32,
    pub viewer_dropdown_padding_y: f32,

    // 列表
    pub list_row_height: f32,
    pub list_row_radius: f32,

    // 菜单 / 下拉 / 右键菜单
    pub menu_min_width: f32,
    pub menu_item_height: f32,
    pub menu_trigger_height: f32,
    pub menu_bar_trigger_height: f32,
    pub menu_trigger_padding_x: f32,
    pub menu_bar_trigger_padding_x: f32,
    pub menu_arrow_space: f32,
    pub menu_popup_padding: f32,
    pub menu_popup_gap: f32,
    pub menu_row_padding_x: f32,
    pub menu_row_icon_size: f32,
    pub menu_row_icon_gap: f32,
    pub menu_row_shortcut_gap: f32,
    pub menu_scrollbar_space: f32,
    pub menu_viewport_margin: f32,
    pub menu_popup_max_height: f32,
    pub menu_popup_viewport_pad: f32,

    // 文本输入
    pub text_input_padding_x: f32,
    pub text_input_padding_y: f32,
    pub text_input_caret_width: f32,

    // 素材网格
    pub asset_grid_content_padding: f32,
    pub asset_grid_card_gap: f32,
    pub asset_grid_card_width: f32,
    pub asset_grid_card_height: f32,
    pub asset_grid_preview_aspect_ratio: f32,
    pub asset_grid_card_radius: f32,
    pub asset_grid_icon_size: f32,
    pub asset_grid_preview_padding: f32,
    pub asset_grid_footer_padding_x: f32,
    pub asset_grid_footer_top_gap: f32,
    pub asset_grid_footer_height: f32,

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
                blur: 2.0,
                spread: 0.0,
                color: [0.0, 0.0, 0.0, 0.12],
            },
            shadow_md: ShadowToken {
                offset_x: 0.0,
                offset_y: 4.0,
                blur: 16.0,
                spread: 2.0,
                color: [0.0, 0.0, 0.0, 0.14],
            },
            shadow_lg: ShadowToken {
                offset_x: 0.0,
                offset_y: 8.0,
                blur: 36.0,
                spread: 4.0,
                color: [0.0, 0.0, 0.0, 0.12],
            },
            shadow_xl: ShadowToken {
                offset_x: 0.0,
                offset_y: 12.0,
                blur: 56.0,
                spread: 6.0,
                color: [0.0, 0.0, 0.0, 0.24],
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
            timeline_scrollbar_min_thumb: 28.0,
            timeline_scrollbar_handle_size: 12.0,
            timeline_scrollbar_handle_visual_size: 8.0,
            timeline_scrollbar_track_visual_thickness: 6.0,
            timeline_scrollbar_body_visual_thickness: 5.0,
            timeline_tool_button_size: 26.0,
            timeline_tool_button_gap: 4.0,
            timeline_toolbar_height: 34.0,
            timeline_toolbar_group_gap: 10.0,
            timeline_content_trailing_padding: 160.0,
            timeline_min_track_height: 30.0,
            timeline_max_track_height: 96.0,
            timeline_track_header_min_width: 132.0,

            inspector_panel_width: 344.0,
            inspector_group_header_height: 28.0,
            property_row_height: 28.0,

            viewer_default_width: 480.0,
            viewer_default_height: 270.0,
            viewer_transport_button_size: 28.0,
            viewer_transport_button_gap: 6.0,
            viewer_dropdown_row_height: 24.0,
            viewer_dropdown_padding_x: 5.0,
            viewer_dropdown_padding_y: 5.0,

            list_row_height: 36.0,
            list_row_radius: 6.0,

            menu_min_width: 160.0,
            menu_item_height: 28.0,
            menu_trigger_height: 28.0,
            menu_bar_trigger_height: 22.0,
            menu_trigger_padding_x: 8.0,
            menu_bar_trigger_padding_x: 7.0,
            menu_arrow_space: 24.0,
            menu_popup_padding: 6.0,
            menu_popup_gap: 2.0,
            menu_row_padding_x: 10.0,
            menu_row_icon_size: 15.0,
            menu_row_icon_gap: 8.0,
            menu_row_shortcut_gap: 24.0,
            menu_scrollbar_space: 8.0,
            menu_viewport_margin: 4.0,
            menu_popup_max_height: 720.0,
            menu_popup_viewport_pad: 24.0,

            text_input_padding_x: 8.0,
            text_input_padding_y: 4.0,
            text_input_caret_width: 2.0,

            asset_grid_content_padding: 8.0,
            asset_grid_card_gap: 8.0,
            asset_grid_card_width: 172.0,
            asset_grid_card_height: 126.0,
            asset_grid_preview_aspect_ratio: 16.0 / 9.0,
            asset_grid_card_radius: 7.0,
            asset_grid_icon_size: 22.0,
            asset_grid_preview_padding: 6.0,
            asset_grid_footer_padding_x: 8.0,
            asset_grid_footer_top_gap: 7.0,
            asset_grid_footer_height: 18.0,

            export_grid_spacing: (12.0, 4.0),

            tooltip_offset: 8.0,
            tooltip_delay_ms: 450,
            tooltip_max_width: 280.0,

            animation_duration_ms: 200,
            animation_ease: AnimationEasing::EaseInOut,
        }
    }
}
