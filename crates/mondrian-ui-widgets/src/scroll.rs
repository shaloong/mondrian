//! ScrollView 控件
//!
//! 虚拟滚动容器。用 PushTranslate/PopTransform + PushClip/PopClip 实现裁剪和偏移。

use glam::Vec2;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// 可滚动的单子节点容器
pub struct ScrollView {
    id: WidgetId,
    child: Option<Box<dyn Widget>>,
    bounds: Rect,
    content_size: Size,
    scroll_offset: Vec2,
    scrollbar_width: f32,
}

impl ScrollView {
    pub fn new(child: Option<Box<dyn Widget>>) -> Self {
        Self {
            id: WidgetId::new(),
            child,
            bounds: Rect::ZERO,
            content_size: Size::ZERO,
            scroll_offset: Vec2::ZERO,
            scrollbar_width: 8.0,
        }
    }

    pub fn scroll_offset(&self) -> Vec2 {
        self.scroll_offset
    }

    pub fn scroll_to_bottom(&mut self) {
        let max_y = (self.content_size.height - self.bounds.height).max(0.0);
        self.scroll_offset.y = max_y;
    }

    pub fn is_at_bottom(&self) -> bool {
        let max_y = (self.content_size.height - self.bounds.height).max(0.0);
        self.scroll_offset.y >= max_y - 1.0
    }

    fn max_scroll_y(&self) -> f32 {
        (self.content_size.height - self.bounds.height).max(0.0)
    }

    fn clamp_scroll_offset(&mut self) {
        self.scroll_offset.x = self.scroll_offset.x.max(0.0);
        self.scroll_offset.y = self.scroll_offset.y.clamp(0.0, self.max_scroll_y());
    }

    fn translate_point_to_child(&self, point: &mut Point) {
        point.x = point.x - self.bounds.x + self.scroll_offset.x;
        point.y = point.y - self.bounds.y + self.scroll_offset.y;
    }
}

impl Widget for ScrollView {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        if let Some(child) = &self.child {
            let child_size = child.measure(constraint);
            Size {
                width: child_size.width + self.scrollbar_width,
                height: child_size.height.min(constraint.max.height),
            }
        } else {
            Size::ZERO
        }
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        if let Some(child) = &mut self.child {
            let measured = child.measure(LayoutConstraint {
                min: Size::ZERO,
                max: Size::new((bounds.width - self.scrollbar_width).max(0.0), f32::MAX),
            });
            self.content_size = measured;
            let child_bounds = Rect::new(0.0, 0.0, measured.width, measured.height);
            child.layout(child_bounds);
            self.clamp_scroll_offset();
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseWheel { delta, position, .. } => {
                if !self.bounds.contains(*position) {
                    return EventResult::Ignored;
                }
                self.scroll_offset.y += *delta;
                self.clamp_scroll_offset();
                EventResult::Handled
            }
            _ => {
                let mut offset_event = event.clone();
                if let UiEvent::MouseDown { ref mut position, .. }
                | UiEvent::MouseUp { ref mut position, .. }
                | UiEvent::MouseMove { ref mut position, .. } = &mut offset_event
                {
                    self.translate_point_to_child(position);
                }
                if let Some(ref mut child) = self.child {
                    child.event(&offset_event, ctx)
                } else {
                    EventResult::Ignored
                }
            }
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.spacing;
        ctx.encoder.push_clip(self.bounds);
        // Translate child from (0,0) to scroll view's screen position, minus scroll offset.
        ctx.encoder.push_translate(Vec2::new(
            self.bounds.x - self.scroll_offset.x,
            self.bounds.y - self.scroll_offset.y,
        ));
        if let Some(ref child) = self.child {
            child.paint(ctx);
        }
        ctx.encoder.pop_transform();

        if self.content_size.height > self.bounds.height {
            let thumb_h = (self.bounds.height / self.content_size.height) * self.bounds.height;
            let scroll_range = (self.content_size.height - self.bounds.height).max(1.0);
            let track_height = (self.bounds.height - thumb_h).max(0.0);
            let thumb_y = (self.scroll_offset.y / scroll_range) * track_height;
            let sb_rect = Rect::new(
                self.bounds.x + self.bounds.width - self.scrollbar_width + 2.0,
                self.bounds.y + thumb_y,
                self.scrollbar_width - 4.0,
                thumb_h.max(16.0),
            );
            ctx.encoder
                .draw_rect(sb_rect, ctx.theme.colors.muted_foreground, tokens.radius_sm);
        }

        ctx.encoder.pop_clip();
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn children(&self) -> &[Box<dyn Widget>] {
        match &self.child {
            Some(c) => std::slice::from_ref(c),
            None => &[],
        }
    }

    fn children_mut(&mut self) -> &mut [Box<dyn Widget>] {
        match &mut self.child {
            Some(c) => std::slice::from_mut(c),
            None => &mut [],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use std::cell::RefCell;
    use std::rc::Rc;

    use mondrian_ui_core::widgets::Spacer;

    struct RecordingChild {
        id: WidgetId,
        preferred: Size,
        last_mouse_down: Rc<RefCell<Option<Point>>>,
    }

    impl Widget for RecordingChild {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, _constraint: LayoutConstraint) -> Size {
            self.preferred
        }

        fn layout(&mut self, _bounds: Rect) {}

        fn event(&mut self, event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            if let UiEvent::MouseDown { position, .. } = event {
                *self.last_mouse_down.borrow_mut() = Some(*position);
                EventResult::Handled
            } else {
                EventResult::Ignored
            }
        }

        fn paint(&self, _ctx: &mut PaintContext) {}

        fn hit_test(&self, _point: Point) -> bool {
            true
        }
    }

    #[test]
    fn scroll_view_new_has_zero_offset() {
        let sv = ScrollView::new(None);
        assert_eq!(sv.scroll_offset(), Vec2::ZERO);
    }

    #[test]
    fn scroll_view_measure_with_child() {
        let child = Spacer::new(200.0, 400.0);
        let sv = ScrollView::new(Some(Box::new(child)));
        let s = sv.measure(LayoutConstraint::loose(300.0, 300.0));
        assert!(s.width > 0.0);
        assert!(s.height > 0.0);
    }

    #[test]
    fn scroll_view_layout_sets_content_size() {
        let child = Spacer::new(200.0, 400.0);
        let mut sv = ScrollView::new(Some(Box::new(child)));
        sv.layout(Rect::new(0.0, 0.0, 300.0, 300.0));
        // content_size should reflect child's measured size
        assert!(sv.content_size.height > 0.0);
        assert!(sv.content_size.width > 0.0);
    }

    #[test]
    fn scroll_view_mouse_wheel_updates_offset() {
        let child = Spacer::new(200.0, 800.0);
        let mut sv = ScrollView::new(Some(Box::new(child)));
        sv.layout(Rect::new(0.0, 0.0, 300.0, 300.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let dispatch = |_| {};
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);

        sv.event(
            &UiEvent::MouseWheel {
                delta: 10.0,
                position: Point::new(10.0, 10.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(sv.scroll_offset().y > 0.0);
    }

    #[test]
    fn scroll_view_ignores_mouse_wheel_outside_bounds() {
        let child = Spacer::new(200.0, 800.0);
        let mut sv = ScrollView::new(Some(Box::new(child)));
        sv.layout(Rect::new(20.0, 20.0, 300.0, 300.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let dispatch = |_| {};
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);

        let result = sv.event(
            &UiEvent::MouseWheel {
                delta: 50.0,
                position: Point::new(0.0, 0.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(sv.scroll_offset().y, 0.0);
    }

    #[test]
    fn scroll_view_translates_pointer_events_to_child_content_space() {
        let last_mouse_down = Rc::new(RefCell::new(None));
        let child = RecordingChild {
            id: WidgetId::new(),
            preferred: Size::new(200.0, 800.0),
            last_mouse_down: Rc::clone(&last_mouse_down),
        };
        let mut sv = ScrollView::new(Some(Box::new(child)));
        sv.layout(Rect::new(20.0, 30.0, 300.0, 300.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let dispatch = |_| {};
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);
        sv.event(
            &UiEvent::MouseWheel {
                delta: 40.0,
                position: Point::new(40.0, 50.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        sv.event(
            &UiEvent::MouseDown {
                position: Point::new(50.0, 70.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(*last_mouse_down.borrow(), Some(Point::new(30.0, 80.0)));
    }

    #[test]
    fn scroll_view_scroll_to_bottom() {
        let child = Spacer::new(200.0, 800.0);
        let mut sv = ScrollView::new(Some(Box::new(child)));
        sv.layout(Rect::new(0.0, 0.0, 300.0, 300.0));

        assert!(!sv.is_at_bottom());
        sv.scroll_to_bottom();
        assert!(sv.is_at_bottom());
    }

    #[test]
    fn scroll_view_is_at_bottom_when_content_fits() {
        let child = Spacer::new(200.0, 100.0);
        let mut sv = ScrollView::new(Some(Box::new(child)));
        sv.layout(Rect::new(0.0, 0.0, 300.0, 300.0));
        // Content fits in viewport → always at bottom
        assert!(sv.is_at_bottom());
    }
}
