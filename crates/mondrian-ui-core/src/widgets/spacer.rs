//! SpacerWidget —— 固定尺寸占位符

use crate::types::{LayoutConstraint, Point, Rect, Size, WidgetId};
use crate::widget::{EventContext, PaintContext};
use crate::{EventResult, UiEvent, Widget};

/// 固定尺寸的占位 Widget
pub struct Spacer {
    id: WidgetId,
    width: f32,
    height: f32,
    bounds: Rect,
}

impl Spacer {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            id: WidgetId::new(),
            width,
            height,
            bounds: Rect::ZERO,
        }
    }
}

impl Widget for Spacer {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        Size::new(self.width, self.height)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        EventResult::Ignored
    }

    fn paint(&self, _ctx: &PaintContext) {}

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}
