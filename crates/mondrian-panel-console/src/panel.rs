//! 控制台 Panel 实现
//!
//! 实现 mondrian_editor_ui::Panel trait。
//! 构建一个 ScrollView + 多行 Label 的 Widget 树。

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use mondrian_core::events::AppEvent;
use mondrian_editor_ui::panel::{Panel, PanelKind};
use mondrian_ui_core::Widget;
use mondrian_ui_widgets::scroll::ScrollView;

use crate::tracing_layer::LogEntry;
use std::collections::VecDeque;

/// 控制台面板
///
/// 持有日志缓冲区的共享引用，构建 Widget 树。
#[allow(dead_code)]
pub struct ConsolePanel {
    buffer: Arc<Mutex<VecDeque<LogEntry>>>,
    max_lines: usize,
    auto_scroll: bool,
}

impl ConsolePanel {
    pub fn new(
        buffer: Arc<Mutex<VecDeque<LogEntry>>>,
        max_lines: usize,
    ) -> Self {
        Self {
            buffer,
            max_lines,
            auto_scroll: true,
        }
    }
}

impl Panel for ConsolePanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Console
    }

    fn title(&self) -> Cow<'static, str> {
        "控制台".into()
    }

    fn build_widget_tree(&mut self) -> Box<dyn Widget> {
        let placeholder = mondrian_ui_core::widgets::ColoredBox::new(
            mondrian_core::Color {
                r: 0.1,
                g: 0.1,
                b: 0.12,
                a: 1.0,
            },
            400.0,
            300.0,
        );

        Box::new(ScrollView::new(Some(Box::new(placeholder))))
    }

    fn on_event(&mut self, _event: &AppEvent) {
        // Console 不需要响应外部事件
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracing_layer::ConsoleLogLayer;

    #[test]
    fn console_panel_creates_with_buffer() {
        let (_, buffer) = ConsoleLogLayer::new(100);
        let panel = ConsolePanel::new(buffer, 100);
        assert_eq!(panel.kind(), PanelKind::Console);
        assert_eq!(panel.title(), "控制台");
    }

    #[test]
    fn console_panel_builds_widget_tree() {
        let (_, buffer) = ConsoleLogLayer::new(100);
        let mut panel = ConsolePanel::new(buffer, 100);
        let _widget = panel.build_widget_tree();
    }

    #[test]
    fn console_panel_is_closable_by_default() {
        let (_, buffer) = ConsoleLogLayer::new(100);
        let panel = ConsolePanel::new(buffer, 100);
        assert!(panel.is_closable());
    }

    #[test]
    fn console_panel_is_draggable_by_default() {
        let (_, buffer) = ConsoleLogLayer::new(100);
        let panel = ConsolePanel::new(buffer, 100);
        assert!(panel.is_draggable());
    }
}
