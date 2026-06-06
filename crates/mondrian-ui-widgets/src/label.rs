//! Label 控件
//!
//! 纯文本显示组件。不响应交互。

use mondrian_core::Color;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{DrawCommandEncoder, EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// Label Widget —— 纯文本显示
pub struct Label {
    id: WidgetId,
    text: String,
    bounds: Rect,
    color: Color,
    font_size: f32,
}

impl Label {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            id: WidgetId::new(),
            text: text.into(),
            bounds: Rect::ZERO,
            color: Color::WHITE,
            font_size: 14.0,
        }
    }

    pub fn with_color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    pub fn with_font_size(mut self, size: f32) -> Self {
        self.font_size = size;
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, text: String) {
        self.text = text;
    }
}

impl Widget for Label {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        let char_count = self.text.chars().count() as f32;
        Size::new(self.font_size * 0.6 * char_count, self.font_size * 1.4)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        EventResult::Ignored
    }

    fn paint(&self, _ctx: &mut PaintContext) {
        // Text rendering handled by TextRenderer at app level
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}
