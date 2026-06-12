//! Widget adapter for the shared flex layout algorithm.
//!
//! `mondrian-ui-layout` owns the pure layout math. This module turns that math
//! into a reusable widget container for panels, forms, toolbars, and demos.

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_layout::constraint::RectInsets;
use mondrian_ui_layout::{AlignItems, FlexDirection, FlexLayout, JustifyContent};

/// A child widget with an optional flex grow factor.
pub struct FlexChild {
    widget: Box<dyn Widget>,
    flex_grow: f32,
}

impl FlexChild {
    /// Create a fixed-size flex child.
    pub fn fixed(widget: Box<dyn Widget>) -> Self {
        Self { widget, flex_grow: 0.0 }
    }

    /// Create a flexing child with a positive grow factor.
    pub fn flex(widget: Box<dyn Widget>, grow: f32) -> Self {
        Self { widget, flex_grow: grow.max(0.0) }
    }
}

/// A multi-child flex container widget.
pub struct FlexContainer {
    id: WidgetId,
    layout: FlexLayout,
    children: Vec<FlexChild>,
    bounds: Rect,
}

impl FlexContainer {
    /// Create a flex container with an explicit layout config.
    pub fn new(layout: FlexLayout, children: Vec<FlexChild>) -> Self {
        Self {
            id: WidgetId::new(),
            layout,
            children,
            bounds: Rect::ZERO,
        }
    }

    /// Create a vertical flex container.
    pub fn column(children: Vec<FlexChild>) -> Self {
        Self::new(FlexLayout::column(), children)
    }

    /// Create a horizontal flex container.
    pub fn row(children: Vec<FlexChild>) -> Self {
        Self::new(FlexLayout::row(), children)
    }

    /// Set the item gap.
    pub fn with_gap(mut self, gap: f32) -> Self {
        self.layout.gap = gap.max(0.0);
        self
    }

    /// Set equal padding on all sides.
    pub fn with_padding(mut self, padding: f32) -> Self {
        self.layout.padding = RectInsets::all(padding.max(0.0));
        self
    }

    /// Set cross-axis alignment.
    pub fn with_align_items(mut self, align_items: AlignItems) -> Self {
        self.layout.align_items = align_items;
        self
    }

    /// Set main-axis distribution.
    pub fn with_justify_content(mut self, justify_content: JustifyContent) -> Self {
        self.layout.justify_content = justify_content;
        self
    }

    fn child_refs(&self) -> Vec<&dyn Widget> {
        self.children.iter().map(|child| child.widget.as_ref()).collect()
    }

    fn flex_grows(&self) -> Vec<f32> {
        self.children.iter().map(|child| child.flex_grow).collect()
    }
}

impl Widget for FlexContainer {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        let is_row = matches!(self.layout.direction, FlexDirection::Row);
        let mut main: f32 = 0.0;
        let mut cross: f32 = 0.0;
        for child in &self.children {
            let measured = child.widget.measure(LayoutConstraint::LOOSE);
            if is_row {
                main += measured.width;
                cross = cross.max(measured.height);
            } else {
                main += measured.height;
                cross = cross.max(measured.width);
            }
        }
        let gaps = self.layout.gap * self.children.len().saturating_sub(1) as f32;
        main += gaps;

        let padding = self.layout.padding;
        let size = if is_row {
            Size::new(
                main + padding.left + padding.right,
                cross + padding.top + padding.bottom,
            )
        } else {
            Size::new(
                cross + padding.left + padding.right,
                main + padding.top + padding.bottom,
            )
        };
        constraint.constrain(size)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        let refs = self.child_refs();
        let grows = self.flex_grows();
        let rects = self.layout.compute_with_flex(bounds, &refs, &grows);
        for (child, rect) in self.children.iter_mut().zip(rects) {
            child.widget.layout(rect);
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        for child in self.children.iter_mut().rev() {
            if child.widget.event(event, ctx) == EventResult::Handled {
                return EventResult::Handled;
            }
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        for child in &self.children {
            child.widget.paint(ctx);
        }
    }

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        for child in &self.children {
            child.widget.paint_overlay(ctx);
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
            || self.children.iter().any(|child| child.widget.hit_test(point))
    }

    fn child_count(&self) -> usize {
        self.children.len()
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        self.children.get(index).map(|child| child.widget.as_ref())
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match self.children.get_mut(index) {
            Some(child) => Some(child.widget.as_mut()),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_core::Color;
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;
    use std::cell::Cell;
    use std::rc::Rc;

    struct ProbeWidget {
        id: WidgetId,
        preferred: Size,
        bounds: Rect,
        handled: Rc<Cell<bool>>,
        overlay_hit: bool,
    }

    impl ProbeWidget {
        fn new(preferred: Size, handled: Rc<Cell<bool>>) -> Self {
            Self {
                id: WidgetId::new(),
                preferred,
                bounds: Rect::ZERO,
                handled,
                overlay_hit: false,
            }
        }

        fn with_overlay_hit(mut self) -> Self {
            self.overlay_hit = true;
            self
        }
    }

    impl Widget for ProbeWidget {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, constraint: LayoutConstraint) -> Size {
            constraint.constrain(self.preferred)
        }

        fn layout(&mut self, bounds: Rect) {
            self.bounds = bounds;
        }

        fn event(&mut self, event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            if matches!(event, UiEvent::MouseDown { .. }) {
                self.handled.set(true);
                EventResult::Handled
            } else {
                EventResult::Ignored
            }
        }

        fn paint(&self, ctx: &mut PaintContext) {
            ctx.encoder.draw_rect(self.bounds, Color::WHITE, 0.0);
        }

        fn hit_test(&self, point: Point) -> bool {
            self.bounds.contains(point) || self.overlay_hit
        }
    }

    #[derive(Default)]
    struct RecordingEncoder {
        rects: Vec<Rect>,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, _bounds: Rect) {}

        fn pop_clip(&mut self) {}

        fn draw_rect(&mut self, bounds: Rect, _color: Color, _corner_radius: f32) {
            self.rects.push(bounds);
        }

        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {}

        fn draw_text(&mut self, _text: &str, _font_size: f32, _position: Point, _color: Color) {}

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    #[test]
    fn flex_container_column_measures_children_gap_and_padding() {
        let handled = Rc::new(Cell::new(false));
        let container = FlexContainer::column(vec![
            FlexChild::fixed(Box::new(ProbeWidget::new(
                Size::new(40.0, 10.0),
                Rc::clone(&handled),
            ))),
            FlexChild::fixed(Box::new(ProbeWidget::new(
                Size::new(20.0, 30.0),
                Rc::clone(&handled),
            ))),
        ])
        .with_gap(6.0)
        .with_padding(4.0);

        assert_eq!(
            container.measure(LayoutConstraint::LOOSE),
            Size::new(48.0, 54.0)
        );
    }

    #[test]
    fn flex_container_lays_out_flex_child_with_remaining_space() {
        let handled = Rc::new(Cell::new(false));
        let mut container = FlexContainer::row(vec![
            FlexChild::fixed(Box::new(ProbeWidget::new(
                Size::new(20.0, 10.0),
                Rc::clone(&handled),
            ))),
            FlexChild::flex(
                Box::new(ProbeWidget::new(Size::new(20.0, 10.0), Rc::clone(&handled))),
                1.0,
            ),
        ])
        .with_gap(10.0);

        container.layout(Rect::new(0.0, 0.0, 100.0, 20.0));

        let second = container.child(1).expect("second child");
        assert!(second.hit_test(Point::new(60.0, 5.0)));
        assert!(!second.hit_test(Point::new(25.0, 5.0)));
    }

    #[test]
    fn flex_container_routes_events_from_topmost_child_first() {
        let first = Rc::new(Cell::new(false));
        let second = Rc::new(Cell::new(false));
        let mut container = FlexContainer::row(vec![
            FlexChild::fixed(Box::new(ProbeWidget::new(
                Size::new(20.0, 10.0),
                Rc::clone(&first),
            ))),
            FlexChild::fixed(Box::new(ProbeWidget::new(
                Size::new(20.0, 10.0),
                Rc::clone(&second),
            ))),
        ]);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let dispatch = |_| {};
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);

        container.event(
            &UiEvent::MouseDown {
                position: Point::new(0.0, 0.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(!first.get());
        assert!(second.get());
    }

    #[test]
    fn flex_container_hit_test_includes_child_overlay() {
        let handled = Rc::new(Cell::new(false));
        let container = FlexContainer::column(vec![FlexChild::fixed(Box::new(
            ProbeWidget::new(Size::new(20.0, 10.0), handled).with_overlay_hit(),
        ))]);

        assert!(container.hit_test(Point::new(900.0, 900.0)));
    }

    #[test]
    fn flex_container_paints_children() {
        let handled = Rc::new(Cell::new(false));
        let mut container = FlexContainer::column(vec![FlexChild::fixed(Box::new(
            ProbeWidget::new(Size::new(20.0, 10.0), handled),
        ))]);
        container.layout(Rect::new(0.0, 0.0, 40.0, 20.0));
        let mut encoder = RecordingEncoder::default();
        let theme = ThemePreset::Dark.build();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 40.0, 20.0),
        };

        container.paint(&mut ctx);

        assert_eq!(encoder.rects.len(), 1);
    }
}
