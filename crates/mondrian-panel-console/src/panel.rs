//! 控制台 Panel 实现
//!
//! 实现 mondrian_editor_ui::Panel trait。
//! 构建一个 ScrollView + 多行 Label 的 Widget 树。

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use mondrian_core::events::AppEvent;
use mondrian_editor_ui::panel::{Panel, PanelContext, PanelKind};
use mondrian_ui_core::types::LayoutConstraint;
use mondrian_ui_core::Widget;
use mondrian_ui_widgets::label::Label;
use mondrian_ui_widgets::scroll::ScrollView;

use crate::tracing_layer::LogEntry;
use std::collections::VecDeque;

/// 控制台面板
///
/// 持有日志缓冲区的共享引用，构建 Widget 树。
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
        // 构建 Widget 树：
        // ScrollView
        //   └─ Container (Column: 每行一个 Label)
        //
        // 注意：Stage C 阶段用简单的 Container + Label 列表
        // 后续可用 FlexLayout Column 替代

        // 创建一堆 Label widget 作为子节点
        // 当前简化为返回一个 ScrollView + Color widget placeholder

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
