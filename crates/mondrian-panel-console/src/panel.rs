//! 控制台 Panel 实现
//!
//! 将 ConsoleLogLayer 捕获的日志渲染为可滚动的日志行列表。

use std::borrow::Cow;
use mondrian_core::Color;
use mondrian_core::events::AppEvent;
use mondrian_editor_ui::panel::{Panel, PanelKind};
use mondrian_ui_core::Widget;
use mondrian_ui_widgets::label::Label;
use mondrian_ui_widgets::scroll::ScrollView;

use crate::tracing_layer::{LogBuffer, LogEntry};

/// 控制台面板
pub struct ConsolePanel {
    buffer: LogBuffer,
    #[allow(dead_code)]
    max_lines: usize,
    #[allow(dead_code)]
    auto_scroll: bool,
}

impl ConsolePanel {
    pub fn new(buffer: LogBuffer, max_lines: usize) -> Self {
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
        let entries: Vec<LogEntry> = {
            let buf = self.buffer.lock().unwrap();
            buf.iter().rev().take(100).rev().cloned().collect()
        };

        // Build Labels for each log entry, color-coded by level
        let labels: Vec<Box<dyn Widget>> = entries
            .iter()
            .map(|entry| {
                let color = match entry.level {
                    tracing::Level::ERROR => Color::from_hex(0xE36D6D),
                    tracing::Level::WARN => Color::from_hex(0xF58220),
                    tracing::Level::INFO => Color::from_hex(0xF2F2F2),
                    tracing::Level::DEBUG => Color::from_hex(0x767680),
                    tracing::Level::TRACE => Color::from_hex(0x555560),
                };
                let text = format!("{} {:>5} {}", entry.timestamp, entry.level, entry.message);
                let label = Label::new(text)
                    .with_color(color)
                    .with_font_size(12.0);
                Box::new(label) as Box<dyn Widget>
            })
            .collect();

        if labels.is_empty() {
            let placeholder = Label::new("控制台就绪 — 无日志").with_color(Color::from_hex(0x767680));
            return Box::new(ScrollView::new(Some(Box::new(placeholder))));
        }

        // Build a simple column layout manually by nesting Containers
        // (avoiding FlexLayout dependency for simple vertical stacking)
        let content = build_column(labels);
        Box::new(ScrollView::new(Some(content)))
    }

    fn on_event(&mut self, _event: &AppEvent) {}
}

/// Build a vertical stack of widgets by chaining single-child Containers.
/// Each child gets its natural height, stacked vertically.
fn build_column(children: Vec<Box<dyn Widget>>) -> Box<dyn Widget> {
    if children.is_empty() {
        return Box::new(Label::new(""));
    }
    ColumnWidget::create(children)
}

/// Simple vertical stacking widget for log lines
struct ColumnWidget {
    id: mondrian_ui_core::types::WidgetId,
    children: Vec<Box<dyn Widget>>,
    bounds: mondrian_ui_core::types::Rect,
    child_heights: Vec<f32>,
    total_height: f32,
    gap: f32,
}

impl ColumnWidget {
    fn create(children: Vec<Box<dyn Widget>>) -> Box<dyn Widget> {
        Box::new(Self {
            id: mondrian_ui_core::types::WidgetId::new(),
            children,
            bounds: mondrian_ui_core::types::Rect::ZERO,
            child_heights: Vec::new(),
            total_height: 0.0,
            gap: 2.0,
        })
    }
}

impl Widget for ColumnWidget {
    fn id(&self) -> mondrian_ui_core::types::WidgetId { self.id }

    fn measure(&self, constraint: mondrian_ui_core::types::LayoutConstraint) -> mondrian_ui_core::types::Size {
        let mut total_h = 0.0f32;
        let mut max_w = 0.0f32;
        for child in &self.children {
            let cs = child.measure(mondrian_ui_core::types::LayoutConstraint::loose(
                constraint.max.width,
                0.0,
            ));
            total_h += cs.height + self.gap;
            max_w = max_w.max(cs.width);
        }
        mondrian_ui_core::types::Size::new(max_w, total_h)
    }

    fn layout(&mut self, bounds: mondrian_ui_core::types::Rect) {
        self.bounds = bounds;
        self.child_heights.clear();
        let mut y = bounds.y;
        for child in &mut self.children {
            let cs = child.measure(mondrian_ui_core::types::LayoutConstraint::loose(bounds.width, 0.0));
            let child_h = cs.height;
            self.child_heights.push(child_h);
            child.layout(mondrian_ui_core::types::Rect::new(bounds.x, y, bounds.width, child_h));
            y += child_h + self.gap;
        }
        self.total_height = y - bounds.y;
    }

    fn event(&mut self, event: &mondrian_ui_core::UiEvent, ctx: &mut mondrian_ui_core::widget::EventContext) -> mondrian_ui_core::EventResult {
        for child in &mut self.children {
            if child.event(event, ctx) == mondrian_ui_core::EventResult::Handled {
                return mondrian_ui_core::EventResult::Handled;
            }
        }
        mondrian_ui_core::EventResult::Ignored
    }

    fn paint(&self, ctx: &mut mondrian_ui_core::widget::PaintContext) {
        for child in &self.children {
            child.paint(ctx);
        }
    }

    fn hit_test(&self, point: mondrian_ui_core::types::Point) -> bool {
        self.bounds.contains(point)
    }

    fn children(&self) -> &[Box<dyn Widget>] { &self.children }
    fn children_mut(&mut self) -> &mut [Box<dyn Widget>] { &mut self.children }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracing_layer::ConsoleLogLayer;

    #[test]
    fn console_panel_creates_with_buffer() {
        let (_layer, buffer) = ConsoleLogLayer::new(100);
        let panel = ConsolePanel::new(buffer, 100);
        assert_eq!(panel.kind(), PanelKind::Console);
        assert_eq!(panel.title(), "控制台");
    }

    #[test]
    fn console_panel_builds_widget_tree() {
        let (_layer, buffer) = ConsoleLogLayer::new(10);
        let mut panel = ConsolePanel::new(buffer, 10);
        let _widget = panel.build_widget_tree();
    }

    #[test]
    fn console_panel_is_closable_and_draggable() {
        let (_layer, buffer) = ConsoleLogLayer::new(10);
        let panel = ConsolePanel::new(buffer, 10);
        assert!(panel.is_closable());
        assert!(panel.is_draggable());
    }
}
