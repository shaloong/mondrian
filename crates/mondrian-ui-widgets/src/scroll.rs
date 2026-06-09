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
    dragging_vertical_thumb: bool,
    drag_start_y: f32,
    drag_start_scroll_y: f32,
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
            dragging_vertical_thumb: false,
            drag_start_y: 0.0,
            drag_start_scroll_y: 0.0,
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

    fn has_vertical_scrollbar(&self) -> bool {
        self.content_size.height > self.bounds.height && self.bounds.height > 0.0
    }

    fn clamp_scroll_offset(&mut self) {
        self.scroll_offset.x = self.scroll_offset.x.max(0.0);
        self.scroll_offset.y = self.scroll_offset.y.clamp(0.0, self.max_scroll_y());
    }

    fn translate_point_to_child(&self, point: &mut Point) {
        point.x = point.x - self.bounds.x + self.scroll_offset.x;
        point.y = point.y - self.bounds.y + self.scroll_offset.y;
    }

    fn vertical_scrollbar_track_rect(&self) -> Rect {
        Rect::new(
            self.bounds.x + self.bounds.width - self.scrollbar_width + 2.0,
            self.bounds.y,
            (self.scrollbar_width - 4.0).max(1.0),
            self.bounds.height,
        )
    }

    fn vertical_scrollbar_thumb_rect(&self) -> Option<Rect> {
        if !self.has_vertical_scrollbar() {
            return None;
        }
        let track = self.vertical_scrollbar_track_rect();
        let thumb_h = (track.height * (self.bounds.height / self.content_size.height))
            .max(16.0)
            .min(track.height);
        let thumb_range = (track.height - thumb_h).max(0.0);
        let max_scroll = self.max_scroll_y().max(1.0);
        let thumb_y = track.y + (self.scroll_offset.y / max_scroll) * thumb_range;
        Some(Rect::new(track.x, thumb_y, track.width, thumb_h))
    }

    fn scroll_offset_for_thumb_delta(&self, delta_y: f32) -> f32 {
        let Some(thumb) = self.vertical_scrollbar_thumb_rect() else {
            return self.scroll_offset.y;
        };
        let track = self.vertical_scrollbar_track_rect();
        let thumb_range = (track.height - thumb.height).max(1.0);
        let scroll_range = self.max_scroll_y();
        self.drag_start_scroll_y + (delta_y / thumb_range) * scroll_range
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
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                if let Some(thumb) = self.vertical_scrollbar_thumb_rect() {
                    if thumb.contains(*position) {
                        self.dragging_vertical_thumb = true;
                        self.drag_start_y = position.y;
                        self.drag_start_scroll_y = self.scroll_offset.y;
                        ctx.request_pointer_capture(self.id);
                        return EventResult::Handled;
                    }
                }
                if self.has_vertical_scrollbar()
                    && self.vertical_scrollbar_track_rect().contains(*position)
                {
                    let Some(thumb) = self.vertical_scrollbar_thumb_rect() else {
                        return EventResult::Ignored;
                    };
                    let page = (self.bounds.height - thumb.height).max(1.0);
                    if position.y < thumb.y {
                        self.scroll_offset.y -= page;
                    } else if position.y > thumb.y + thumb.height {
                        self.scroll_offset.y += page;
                    }
                    self.clamp_scroll_offset();
                    return EventResult::Handled;
                }

                let mut offset_event = event.clone();
                if let UiEvent::MouseDown { ref mut position, .. } = &mut offset_event {
                    self.translate_point_to_child(position);
                }
                if let Some(ref mut child) = self.child {
                    child.event(&offset_event, ctx)
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::MouseMove { position, .. } if self.dragging_vertical_thumb => {
                self.scroll_offset.y =
                    self.scroll_offset_for_thumb_delta(position.y - self.drag_start_y);
                self.clamp_scroll_offset();
                EventResult::Handled
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.dragging_vertical_thumb => {
                self.dragging_vertical_thumb = false;
                ctx.release_pointer_capture(self.id);
                EventResult::Handled
            }
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
                if let UiEvent::MouseUp { ref mut position, .. }
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

        if let Some(sb_rect) = self.vertical_scrollbar_thumb_rect() {
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
    use mondrian_platform::NoopPlatformService;
    use mondrian_ui_core::widget::{EventRequests, PointerCaptureRequest};
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
    fn scroll_view_vertical_thumb_drag_updates_offset() {
        let child = Spacer::new(200.0, 800.0);
        let mut sv = ScrollView::new(Some(Box::new(child)));
        sv.layout(Rect::new(0.0, 0.0, 300.0, 300.0));

        let thumb = sv.vertical_scrollbar_thumb_rect().expect("overflow should show thumb");
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let dispatch = |_| {};
        let mut requests = EventRequests::default();
        let platform = NoopPlatformService;
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &dispatch,
            platform: &platform,
            requests: &mut requests,
        };

        let start = thumb.center();
        sv.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(sv.id))
        );

        sv.event(
            &UiEvent::MouseMove {
                position: Point::new(start.x, start.y + 75.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(sv.scroll_offset().y > 0.0);
    }

    #[test]
    fn scroll_view_vertical_thumb_release_stops_dragging() {
        let child = Spacer::new(200.0, 800.0);
        let mut sv = ScrollView::new(Some(Box::new(child)));
        sv.layout(Rect::new(0.0, 0.0, 300.0, 300.0));

        let thumb = sv.vertical_scrollbar_thumb_rect().expect("overflow should show thumb");
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let dispatch = |_| {};
        let mut requests = EventRequests::default();
        let platform = NoopPlatformService;
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &dispatch,
            platform: &platform,
            requests: &mut requests,
        };

        let start = thumb.center();
        sv.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        sv.event(
            &UiEvent::MouseUp {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Release(sv.id))
        );

        let offset_after_release = sv.scroll_offset().y;
        sv.event(
            &UiEvent::MouseMove {
                position: Point::new(start.x, start.y + 100.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(sv.scroll_offset().y, offset_after_release);
    }

    #[test]
    fn scroll_view_track_click_pages_offset() {
        let child = Spacer::new(200.0, 800.0);
        let mut sv = ScrollView::new(Some(Box::new(child)));
        sv.layout(Rect::new(0.0, 0.0, 300.0, 300.0));

        let thumb = sv.vertical_scrollbar_thumb_rect().expect("overflow should show thumb");
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let dispatch = |_| {};
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);

        sv.event(
            &UiEvent::MouseDown {
                position: Point::new(thumb.center().x, thumb.y + thumb.height + 20.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(sv.scroll_offset().y > 0.0);
    }

    #[test]
    fn scroll_view_no_thumb_capture_when_content_fits() {
        let child = Spacer::new(200.0, 100.0);
        let mut sv = ScrollView::new(Some(Box::new(child)));
        sv.layout(Rect::new(0.0, 0.0, 300.0, 300.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let dispatch = |_| {};
        let mut requests = EventRequests::default();
        let platform = NoopPlatformService;
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &dispatch,
            platform: &platform,
            requests: &mut requests,
        };

        let result = sv.event(
            &UiEvent::MouseDown {
                position: Point::new(296.0, 20.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(ctx.requests.pointer_capture, None);
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
