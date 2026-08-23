//! 焦点管理器 trait
//!
//! 维护当前聚焦的 Widget/Panel/Window。

use crate::types::WidgetId;
use mondrian_editor_state::state::PanelKind;

/// 焦点管理器
///
/// ## 职责
///
/// * 记录当前聚焦的 Widget 和 Panel
/// * 记录焦点所有权；Tab / Shift+Tab 遍历由拥有 WidgetTree 的事件路由器执行
/// * 确保同一时间只有一个焦点
pub trait FocusManager {
    /// 当前聚焦的 Widget
    fn focused_widget(&self) -> Option<WidgetId>;

    /// 当前聚焦的 Panel
    fn focused_panel(&self) -> Option<PanelKind>;

    /// Request focus for a widget.
    ///
    /// Panel ownership is derived by the event router from the widget tree, not
    /// supplied by leaf controls.
    fn request_focus(&mut self, widget: WidgetId);

    /// 释放焦点
    fn release_focus(&mut self, widget: WidgetId);

    /// 清除所有焦点
    fn clear_focus(&mut self);
}
