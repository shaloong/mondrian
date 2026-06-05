//! 焦点管理器 trait
//!
//! 维护当前聚焦的 Widget/Panel/Window，支持 Tab/Shift+Tab 焦点遍历。

use crate::types::WidgetId;
use mondrian_editor_state::state::PanelKind;

/// 焦点管理器
///
/// ## 职责
///
/// * 记录当前聚焦的 Widget 和 Panel
/// * 支持 Tab / Shift+Tab 焦点遍历
/// * 确保同一时间只有一个焦点
pub trait FocusManager {
    /// 当前聚焦的 Widget
    fn focused_widget(&self) -> Option<WidgetId>;

    /// 当前聚焦的 Panel
    fn focused_panel(&self) -> Option<PanelKind>;

    /// 请求焦点
    fn request_focus(&mut self, widget: WidgetId, panel: PanelKind);

    /// 释放焦点
    fn release_focus(&mut self, widget: WidgetId);

    /// 焦点移到下一个可聚焦的 Widget（Tab）
    fn focus_next(&mut self);

    /// 焦点移到上一个可聚焦的 Widget（Shift+Tab）
    fn focus_prev(&mut self);

    /// 清除所有焦点
    fn clear_focus(&mut self);
}
