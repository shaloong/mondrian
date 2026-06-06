//! ColoredBox —— 纯色矩形 Widget（用于测试）

use mondrian_core::Color;

use crate::types::{LayoutConstraint, Point, Rect, Size, WidgetId};
use crate::widget::{EventContext, PaintContext};
use crate::{EventResult, UiEvent, Widget};

/// 纯色矩形 Widget
pub struct ColoredBox {
    id: WidgetId,
    color: Color,
    hovered: bool,
    bounds: Rect,
    preferred: Size,
}

impl ColoredBox {
    pub fn new(color: Color, width: f32, height: f32) -> Self {
        Self {
            id: WidgetId::new(),
            color,
            hovered: false,
            bounds: Rect::ZERO,
            preferred: Size::new(width, height),
        }
    }

    pub fn color(&self) -> Color {
        self.color
    }

    pub fn is_hovered(&self) -> bool {
        self.hovered
    }
}

impl Widget for ColoredBox {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        self.preferred
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::FocusGained => {
                self.hovered = true;
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                self.hovered = false;
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, _ctx: &mut PaintContext) {}

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}
