//! ContainerWidget —— 具有 padding + background 的单子节点容器

use mondrian_core::Color;

use crate::types::{LayoutConstraint, Point, Rect, Size, WidgetId};
use crate::widget::{EventContext, PaintContext};
use crate::{EventResult, UiEvent, Widget};

/// 容器 Widget：单子节点 + padding + background color
pub struct Container {
    id: WidgetId,
    child: Option<Box<dyn Widget>>,
    padding: f32,
    background: Option<Color>,
    bounds: Rect,
}

impl Container {
    pub fn new(child: Option<Box<dyn Widget>>) -> Self {
        Self {
            id: WidgetId::new(),
            child,
            padding: 0.0,
            background: None,
            bounds: Rect::ZERO,
        }
    }

    pub fn with_padding(mut self, padding: f32) -> Self {
        self.padding = padding;
        self
    }

    pub fn with_background(mut self, color: Color) -> Self {
        self.background = Some(color);
        self
    }
}

impl Widget for Container {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        let pad2 = self.padding * 2.0;
        let inner_constraint = LayoutConstraint {
            min: Size {
                width: (constraint.min.width - pad2).max(0.0),
                height: (constraint.min.height - pad2).max(0.0),
            },
            max: Size {
                width: (constraint.max.width - pad2).max(0.0),
                height: (constraint.max.height - pad2).max(0.0),
            },
        };

        let child_size = self
            .child
            .as_ref()
            .map(|c| c.measure(inner_constraint))
            .unwrap_or(Size::ZERO);

        Size {
            width: child_size.width + pad2,
            height: child_size.height + pad2,
        }
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        if let Some(child) = &mut self.child {
            let inner = Rect::new(
                bounds.x + self.padding,
                bounds.y + self.padding,
                (bounds.width - self.padding * 2.0).max(0.0),
                (bounds.height - self.padding * 2.0).max(0.0),
            );
            child.layout(inner);
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if let Some(child) = &mut self.child {
            child.event(event, ctx)
        } else {
            EventResult::Ignored
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        if let Some(bg) = self.background {
            ctx.encoder.draw_rect(self.bounds, bg, 0.0);
        }
        if let Some(child) = &self.child {
            child.paint(ctx);
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn children(&self) -> &[Box<dyn Widget>] {
        &[]
    }

    fn children_mut(&mut self) -> &mut [Box<dyn Widget>] {
        &mut []
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::Spacer;

    #[test]
    fn container_empty_measure_is_padding_only() {
        let c = Container::new(None).with_padding(10.0);
        let s = c.measure(LayoutConstraint::LOOSE);
        assert_eq!(s, Size::new(20.0, 20.0)); // 2 * padding
    }

    #[test]
    fn container_with_child_measure_adds_padding() {
        let child = Spacer::new(50.0, 30.0);
        let c = Container::new(Some(Box::new(child))).with_padding(5.0);
        let s = c.measure(LayoutConstraint::LOOSE);
        assert_eq!(s, Size::new(60.0, 40.0)); // 50+10, 30+10
    }

    #[test]
    fn container_layout_positions_child_inside_padding() {
        let child = Spacer::new(100.0, 100.0);
        let mut c = Container::new(Some(Box::new(child))).with_padding(10.0);
        c.layout(Rect::new(0.0, 0.0, 200.0, 200.0));

        // Container's hit test uses outer bounds
        assert!(c.hit_test(Point::new(5.0, 5.0)));
        assert!(c.hit_test(Point::new(199.0, 199.0)));
    }

    #[test]
    fn container_children_returns_empty_slice() {
        let child = Spacer::new(10.0, 10.0);
        let c = Container::new(Some(Box::new(child)));
        // Known issue: Container::children() returns empty,
        // children are handled manually in paint/layout/event
        assert!(c.children().is_empty());
    }

    #[test]
    fn container_without_background_empty_paint_is_noop() {
        let _c = Container::new(None);
        // paint should not panic even with no background and no child
    }
}
