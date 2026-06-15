//! FocusManager 实现
//!
//! 跟踪当前聚焦的 Widget 和 Panel，支持 Tab / Shift+Tab 遍历。

use mondrian_editor_state::state::PanelKind;
use mondrian_ui_core::focus::FocusManager;
use mondrian_ui_core::types::WidgetId;

/// FocusManager 的具体实现
#[derive(Debug, Default)]
pub struct FocusManagerImpl {
    widget: Option<WidgetId>,
    panel: Option<PanelKind>,
}

impl FocusManagerImpl {
    pub fn new() -> Self {
        Self { widget: None, panel: None }
    }

    /// Set the focused widget directly (used by EventRouter during Tab traversal
    /// and click-to-focus orchestration).
    pub fn set_focused_widget(&mut self, widget: Option<WidgetId>, panel: Option<PanelKind>) {
        self.widget = widget;
        self.panel = panel;
    }
}

impl FocusManager for FocusManagerImpl {
    fn focused_widget(&self) -> Option<WidgetId> {
        self.widget
    }

    fn focused_panel(&self) -> Option<PanelKind> {
        self.panel
    }

    fn request_focus(&mut self, widget: WidgetId) {
        self.widget = Some(widget);
        self.panel = None;
    }

    fn release_focus(&mut self, widget: WidgetId) {
        if self.widget == Some(widget) {
            self.widget = None;
            self.panel = None;
        }
    }

    fn focus_next(&mut self) {
        // Tab traversal requires WidgetTree access which is owned by EventRouter.
        // The EventRouter should call WidgetTree-based traversal when this is invoked.
    }

    fn focus_prev(&mut self) {
        // See focus_next
    }

    fn clear_focus(&mut self) {
        self.widget = None;
        self.panel = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_has_no_focus() {
        let fm = FocusManagerImpl::new();
        assert_eq!(fm.focused_widget(), None);
        assert_eq!(fm.focused_panel(), None);
    }

    #[test]
    fn request_focus_sets_widget_without_panel_context() {
        let mut fm = FocusManagerImpl::new();
        let id = WidgetId::new();
        fm.request_focus(id);
        assert_eq!(fm.focused_widget(), Some(id));
        assert_eq!(fm.focused_panel(), None);
    }

    #[test]
    fn set_focused_widget_sets_widget_and_panel() {
        let mut fm = FocusManagerImpl::new();
        let id = WidgetId::new();
        fm.set_focused_widget(Some(id), Some(PanelKind::Viewer));
        assert_eq!(fm.focused_widget(), Some(id));
        assert_eq!(fm.focused_panel(), Some(PanelKind::Viewer));
    }

    #[test]
    fn release_focus_clears_if_matching() {
        let mut fm = FocusManagerImpl::new();
        let id = WidgetId::new();
        fm.set_focused_widget(Some(id), Some(PanelKind::Timeline));
        fm.release_focus(id);
        assert_eq!(fm.focused_widget(), None);
    }

    #[test]
    fn release_focus_ignores_non_matching() {
        let mut fm = FocusManagerImpl::new();
        let id1 = WidgetId::new();
        let id2 = WidgetId::new();
        fm.set_focused_widget(Some(id1), Some(PanelKind::Console));
        fm.release_focus(id2); // different widget
        assert_eq!(fm.focused_widget(), Some(id1));
    }

    #[test]
    fn clear_focus_removes_all() {
        let mut fm = FocusManagerImpl::new();
        fm.set_focused_widget(Some(WidgetId::new()), Some(PanelKind::Assets));
        fm.clear_focus();
        assert_eq!(fm.focused_widget(), None);
        assert_eq!(fm.focused_panel(), None);
    }
}
