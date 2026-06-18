//! ScrollView 控件
//!
//! 虚拟滚动容器。子内容按滚动后的屏幕坐标布局，绘制阶段只负责裁剪。

use glam::Vec2;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// Axes that a [`ScrollView`] may scroll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAxes {
    /// Vertical overflow only. This is the default for inspector/list panels.
    Vertical,
    /// Horizontal overflow only.
    Horizontal,
    /// Horizontal and vertical overflow.
    Both,
}

impl ScrollAxes {
    fn horizontal(self) -> bool {
        matches!(self, Self::Horizontal | Self::Both)
    }

    fn vertical(self) -> bool {
        matches!(self, Self::Vertical | Self::Both)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollbarAxis {
    Horizontal,
    Vertical,
}

/// 可滚动的单子节点容器
pub struct ScrollView {
    id: WidgetId,
    child: Option<Box<dyn Widget>>,
    bounds: Rect,
    content_size: Size,
    scroll_offset: Vec2,
    axes: ScrollAxes,
    scrollbar_width: f32,
    dragging_thumb: Option<ScrollbarAxis>,
    hovered_thumb: Option<ScrollbarAxis>,
    drag_start_position: Point,
    drag_start_scroll: Vec2,
}

impl ScrollView {
    pub fn new(child: Option<Box<dyn Widget>>) -> Self {
        Self {
            id: WidgetId::new(),
            child,
            bounds: Rect::ZERO,
            content_size: Size::ZERO,
            scroll_offset: Vec2::ZERO,
            axes: ScrollAxes::Vertical,
            scrollbar_width: 8.0,
            dragging_thumb: None,
            hovered_thumb: None,
            drag_start_position: Point::ZERO,
            drag_start_scroll: Vec2::ZERO,
        }
    }

    /// Enable horizontal, vertical, or dual-axis scrolling.
    pub fn with_axes(mut self, axes: ScrollAxes) -> Self {
        self.axes = axes;
        self.clamp_scroll_offset();
        self
    }

    /// Set the visual scrollbar lane width.
    pub fn with_scrollbar_width(mut self, width: f32) -> Self {
        self.scrollbar_width = width.max(4.0);
        self
    }

    pub fn scroll_offset(&self) -> Vec2 {
        self.scroll_offset
    }

    /// Set the scroll offset, clamped to the current content and viewport.
    pub fn set_scroll_offset(&mut self, offset: Vec2) {
        self.scroll_offset = offset;
        self.clamp_scroll_offset();
        self.layout_child();
    }

    /// Returns true when a scrollbar track or thumb should receive this point.
    pub fn scrollbar_hit_test(&self, point: Point) -> bool {
        self.vertical_scrollbar_thumb_rect().is_some_and(|thumb| thumb.contains(point))
            || self
                .horizontal_scrollbar_thumb_rect()
                .is_some_and(|thumb| thumb.contains(point))
            || (self.has_vertical_scrollbar()
                && self.vertical_scrollbar_track_rect().contains(point))
            || (self.has_horizontal_scrollbar()
                && self.horizontal_scrollbar_track_rect().contains(point))
    }

    /// Returns true while a scrollbar thumb has active pointer capture.
    pub fn is_scrollbar_dragging(&self) -> bool {
        self.dragging_thumb.is_some()
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset.y = self.max_scroll_y();
        self.layout_child();
    }

    pub fn is_at_bottom(&self) -> bool {
        self.scroll_offset.y >= self.max_scroll_y() - 1.0
    }

    fn max_scroll_y(&self) -> f32 {
        if self.axes.vertical() {
            (self.content_size.height - self.bounds.height).max(0.0)
        } else {
            0.0
        }
    }

    fn max_scroll_x(&self) -> f32 {
        if self.axes.horizontal() {
            (self.content_size.width - self.bounds.width).max(0.0)
        } else {
            0.0
        }
    }

    fn has_vertical_scrollbar(&self) -> bool {
        self.max_scroll_y() > 0.0 && self.bounds.height > 0.0
    }

    fn has_horizontal_scrollbar(&self) -> bool {
        self.max_scroll_x() > 0.0 && self.bounds.width > 0.0
    }

    fn clamp_scroll_offset(&mut self) {
        self.scroll_offset.x = self.scroll_offset.x.clamp(0.0, self.max_scroll_x());
        self.scroll_offset.y = self.scroll_offset.y.clamp(0.0, self.max_scroll_y());
    }

    fn set_scroll_x(&mut self, x: f32) -> bool {
        let old = self.scroll_offset.x;
        self.scroll_offset.x = x;
        self.clamp_scroll_offset();
        let changed = (self.scroll_offset.x - old).abs() > 0.01;
        if changed {
            self.layout_child();
        }
        changed
    }

    fn set_scroll_y(&mut self, y: f32) -> bool {
        let old = self.scroll_offset.y;
        self.scroll_offset.y = y;
        self.clamp_scroll_offset();
        let changed = (self.scroll_offset.y - old).abs() > 0.01;
        if changed {
            self.layout_child();
        }
        changed
    }

    fn set_thumb_hovered(&mut self, position: Point) -> bool {
        let hovered = if self
            .vertical_scrollbar_thumb_rect()
            .is_some_and(|thumb| thumb.contains(position))
        {
            Some(ScrollbarAxis::Vertical)
        } else if self
            .horizontal_scrollbar_thumb_rect()
            .is_some_and(|thumb| thumb.contains(position))
        {
            Some(ScrollbarAxis::Horizontal)
        } else {
            None
        };
        let changed = hovered != self.hovered_thumb;
        self.hovered_thumb = hovered;
        changed
    }

    fn child_bounds(&self) -> Rect {
        Rect::new(
            self.bounds.x - self.scroll_offset.x,
            self.bounds.y - self.scroll_offset.y,
            self.content_size.width,
            self.content_size.height,
        )
    }

    fn layout_child(&mut self) {
        let child_bounds = self.child_bounds();
        if let Some(child) = &mut self.child {
            child.layout(child_bounds);
        }
    }

    fn child_overlay_hit_test(&self, point: Point) -> bool {
        self.child.as_ref().is_some_and(|child| child.overlay_hit_test(point))
    }

    fn vertical_scrollbar_track_rect(&self) -> Rect {
        let bottom_reserved = if self.has_horizontal_scrollbar() {
            self.scrollbar_width
        } else {
            0.0
        };
        Rect::new(
            self.bounds.x + self.bounds.width - self.scrollbar_width + 2.0,
            self.bounds.y,
            (self.scrollbar_width - 4.0).max(1.0),
            (self.bounds.height - bottom_reserved).max(0.0),
        )
    }

    fn horizontal_scrollbar_track_rect(&self) -> Rect {
        let right_reserved = if self.has_vertical_scrollbar() {
            self.scrollbar_width
        } else {
            0.0
        };
        Rect::new(
            self.bounds.x,
            self.bounds.y + self.bounds.height - self.scrollbar_width + 2.0,
            (self.bounds.width - right_reserved).max(0.0),
            (self.scrollbar_width - 4.0).max(1.0),
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

    fn horizontal_scrollbar_thumb_rect(&self) -> Option<Rect> {
        if !self.has_horizontal_scrollbar() {
            return None;
        }
        let track = self.horizontal_scrollbar_track_rect();
        if track.width <= 0.0 {
            return None;
        }
        let thumb_w = (track.width * (self.bounds.width / self.content_size.width))
            .max(16.0)
            .min(track.width);
        let thumb_range = (track.width - thumb_w).max(0.0);
        let max_scroll = self.max_scroll_x().max(1.0);
        let thumb_x = track.x + (self.scroll_offset.x / max_scroll) * thumb_range;
        Some(Rect::new(thumb_x, track.y, thumb_w, track.height))
    }

    fn scroll_offset_for_thumb_delta(&self, delta_y: f32) -> f32 {
        let Some(thumb) = self.vertical_scrollbar_thumb_rect() else {
            return self.scroll_offset.y;
        };
        let track = self.vertical_scrollbar_track_rect();
        let thumb_range = (track.height - thumb.height).max(1.0);
        let scroll_range = self.max_scroll_y();
        self.drag_start_scroll.y + (delta_y / thumb_range) * scroll_range
    }

    fn scroll_x_for_thumb_delta(&self, delta_x: f32) -> f32 {
        let Some(thumb) = self.horizontal_scrollbar_thumb_rect() else {
            return self.scroll_offset.x;
        };
        let track = self.horizontal_scrollbar_track_rect();
        let thumb_range = (track.width - thumb.width).max(1.0);
        let scroll_range = self.max_scroll_x();
        self.drag_start_scroll.x + (delta_x / thumb_range) * scroll_range
    }

    fn page_axis_at(&mut self, axis: ScrollbarAxis, position: Point) -> bool {
        match axis {
            ScrollbarAxis::Vertical => {
                let Some(thumb) = self.vertical_scrollbar_thumb_rect() else {
                    return false;
                };
                let page = (self.bounds.height - thumb.height).max(1.0);
                if position.y < thumb.y {
                    self.set_scroll_y(self.scroll_offset.y - page)
                } else if position.y > thumb.y + thumb.height {
                    self.set_scroll_y(self.scroll_offset.y + page)
                } else {
                    false
                }
            }
            ScrollbarAxis::Horizontal => {
                let Some(thumb) = self.horizontal_scrollbar_thumb_rect() else {
                    return false;
                };
                let page = (self.bounds.width - thumb.width).max(1.0);
                if position.x < thumb.x {
                    self.set_scroll_x(self.scroll_offset.x - page)
                } else if position.x > thumb.x + thumb.width {
                    self.set_scroll_x(self.scroll_offset.x + page)
                } else {
                    false
                }
            }
        }
    }

    fn start_thumb_drag(&mut self, axis: ScrollbarAxis, position: Point, ctx: &mut EventContext) {
        self.dragging_thumb = Some(axis);
        self.hovered_thumb = Some(axis);
        self.drag_start_position = position;
        self.drag_start_scroll = self.scroll_offset;
        ctx.request_pointer_capture(self.id);
        ctx.request_repaint();
    }

    fn stop_thumb_drag(&mut self, ctx: &mut EventContext) {
        self.dragging_thumb = None;
        self.hovered_thumb = None;
        ctx.release_pointer_capture(self.id);
        ctx.request_repaint();
    }
}

impl Widget for ScrollView {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        if let Some(child) = &self.child {
            let child_size = child.measure(LayoutConstraint {
                min: Size::ZERO,
                max: Size::new(
                    if self.axes.horizontal() {
                        f32::MAX
                    } else {
                        constraint.max.width
                    },
                    if self.axes.vertical() {
                        f32::MAX
                    } else {
                        constraint.max.height
                    },
                ),
            });
            Size {
                width: child_size.width.min(constraint.max.width).max(constraint.min.width),
                height: child_size.height.min(constraint.max.height).max(constraint.min.height),
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
                max: Size::new(
                    if self.axes.horizontal() {
                        f32::MAX
                    } else {
                        bounds.width.max(0.0)
                    },
                    if self.axes.vertical() {
                        f32::MAX
                    } else {
                        bounds.height.max(0.0)
                    },
                ),
            });
            self.content_size = measured;
            self.clamp_scroll_offset();
            self.layout_child();
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                if let Some(thumb) = self.vertical_scrollbar_thumb_rect() {
                    if thumb.contains(*position) {
                        self.start_thumb_drag(ScrollbarAxis::Vertical, *position, ctx);
                        return EventResult::Handled;
                    }
                }
                if let Some(thumb) = self.horizontal_scrollbar_thumb_rect() {
                    if thumb.contains(*position) {
                        self.start_thumb_drag(ScrollbarAxis::Horizontal, *position, ctx);
                        return EventResult::Handled;
                    }
                }
                if self.has_vertical_scrollbar()
                    && self.vertical_scrollbar_track_rect().contains(*position)
                {
                    let changed = self.page_axis_at(ScrollbarAxis::Vertical, *position);
                    if changed {
                        ctx.request_repaint();
                    }
                    return EventResult::Handled;
                }
                if self.has_horizontal_scrollbar()
                    && self.horizontal_scrollbar_track_rect().contains(*position)
                {
                    let changed = self.page_axis_at(ScrollbarAxis::Horizontal, *position);
                    if changed {
                        ctx.request_repaint();
                    }
                    return EventResult::Handled;
                }

                if let Some(ref mut child) = self.child {
                    child.event(event, ctx)
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::MouseMove { position, .. } if self.dragging_thumb.is_some() => {
                let changed = match self.dragging_thumb {
                    Some(ScrollbarAxis::Vertical) => self.set_scroll_y(
                        self.scroll_offset_for_thumb_delta(position.y - self.drag_start_position.y),
                    ),
                    Some(ScrollbarAxis::Horizontal) => self.set_scroll_x(
                        self.scroll_x_for_thumb_delta(position.x - self.drag_start_position.x),
                    ),
                    None => false,
                };
                if changed {
                    ctx.request_repaint();
                }
                EventResult::Handled
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.dragging_thumb.is_some() => {
                self.stop_thumb_drag(ctx);
                EventResult::Handled
            }
            UiEvent::MouseMove { position, .. } => {
                if self.set_thumb_hovered(*position) {
                    ctx.request_repaint();
                    return EventResult::Handled;
                }

                if let Some(ref mut child) = self.child {
                    child.event(event, ctx)
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::MouseWheel { delta, position, modifiers } => {
                if !self.bounds.contains(*position) {
                    if self.child_overlay_hit_test(*position) {
                        if let Some(ref mut child) = self.child {
                            return child.event(event, ctx);
                        }
                    }
                    return EventResult::Ignored;
                }
                let changed = if modifiers.shift {
                    self.set_scroll_x(self.scroll_offset.x + *delta)
                } else {
                    self.set_scroll_y(self.scroll_offset.y + *delta)
                };
                if changed {
                    ctx.request_repaint();
                }
                EventResult::Handled
            }
            _ => {
                if let Some(ref mut child) = self.child {
                    child.event(event, ctx)
                } else {
                    EventResult::Ignored
                }
            }
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        ctx.encoder.push_clip(self.bounds);
        if let Some(ref child) = self.child {
            child.paint(ctx);
        }

        if let Some(mut sb_rect) = self.vertical_scrollbar_thumb_rect() {
            let dragging = self.dragging_thumb == Some(ScrollbarAxis::Vertical);
            let hovered = self.hovered_thumb == Some(ScrollbarAxis::Vertical);
            let active = dragging || hovered;
            if active {
                sb_rect = Rect::new(
                    sb_rect.x - 1.0,
                    sb_rect.y,
                    sb_rect.width + 2.0,
                    sb_rect.height,
                );
            }
            let track = self.vertical_scrollbar_track_rect();
            let mut track_color = ctx.theme.colors.scrollbar_thumb;
            track_color.a *= if active { 0.22 } else { 0.12 };
            ctx.encoder.draw_rect(track, track_color, ctx.theme.spacing.radius_full);

            let mut thumb_color = ctx.theme.colors.scrollbar_thumb;
            thumb_color.a *= if dragging {
                1.0
            } else if hovered {
                0.82
            } else {
                0.62
            };
            ctx.encoder.draw_rect(sb_rect, thumb_color, ctx.theme.spacing.radius_full);
        }

        if let Some(mut sb_rect) = self.horizontal_scrollbar_thumb_rect() {
            let dragging = self.dragging_thumb == Some(ScrollbarAxis::Horizontal);
            let hovered = self.hovered_thumb == Some(ScrollbarAxis::Horizontal);
            let active = dragging || hovered;
            if active {
                sb_rect = Rect::new(
                    sb_rect.x,
                    sb_rect.y - 1.0,
                    sb_rect.width,
                    sb_rect.height + 2.0,
                );
            }
            let track = self.horizontal_scrollbar_track_rect();
            let mut track_color = ctx.theme.colors.scrollbar_thumb;
            track_color.a *= if active { 0.22 } else { 0.12 };
            ctx.encoder.draw_rect(track, track_color, ctx.theme.spacing.radius_full);

            let mut thumb_color = ctx.theme.colors.scrollbar_thumb;
            thumb_color.a *= if dragging {
                1.0
            } else if hovered {
                0.82
            } else {
                0.62
            };
            ctx.encoder.draw_rect(sb_rect, thumb_color, ctx.theme.spacing.radius_full);
        }

        ctx.encoder.pop_clip();
    }

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        if let Some(ref child) = self.child {
            child.paint_overlay(ctx);
        }
    }

    fn overlay_hit_test(&self, point: Point) -> bool {
        self.child_overlay_hit_test(point)
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_hit_test_clip(&self) -> Option<Rect> {
        Some(self.bounds)
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
    use mondrian_core::Color;
    use mondrian_platform::NoopPlatformService;
    use mondrian_ui_core::widget::{DrawCommandEncoder, EventRequests, PointerCaptureRequest};
    use mondrian_ui_theme::ThemePreset;
    use std::cell::RefCell;
    use std::rc::Rc;

    use mondrian_ui_core::widgets::Spacer;

    struct RecordingChild {
        id: WidgetId,
        preferred: Size,
        last_mouse_down: Rc<RefCell<Option<Point>>>,
        last_layout: Rc<RefCell<Option<Rect>>>,
    }

    impl Widget for RecordingChild {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, _constraint: LayoutConstraint) -> Size {
            self.preferred
        }

        fn layout(&mut self, bounds: Rect) {
            *self.last_layout.borrow_mut() = Some(bounds);
        }

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

    struct OverlayChild {
        id: WidgetId,
        preferred: Size,
        last_hit: Rc<RefCell<Option<Point>>>,
        last_wheel: Rc<RefCell<Option<Point>>>,
        overlay_painted: Rc<RefCell<bool>>,
    }

    impl OverlayChild {
        fn new(
            preferred: Size,
            last_hit: Rc<RefCell<Option<Point>>>,
            last_wheel: Rc<RefCell<Option<Point>>>,
            overlay_painted: Rc<RefCell<bool>>,
        ) -> Self {
            Self {
                id: WidgetId::new(),
                preferred,
                last_hit,
                last_wheel,
                overlay_painted,
            }
        }
    }

    impl Widget for OverlayChild {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, _constraint: LayoutConstraint) -> Size {
            self.preferred
        }

        fn layout(&mut self, _bounds: Rect) {}

        fn event(&mut self, event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            if let UiEvent::MouseWheel { position, .. } = event {
                *self.last_wheel.borrow_mut() = Some(*position);
                EventResult::Handled
            } else {
                EventResult::Ignored
            }
        }

        fn paint(&self, _ctx: &mut PaintContext) {}

        fn paint_overlay(&self, _ctx: &mut PaintContext) {
            *self.overlay_painted.borrow_mut() = true;
        }

        fn overlay_hit_test(&self, point: Point) -> bool {
            *self.last_hit.borrow_mut() = Some(point);
            true
        }

        fn hit_test(&self, _point: Point) -> bool {
            false
        }
    }

    #[derive(Default)]
    struct RecordingEncoder {
        translations: Vec<Vec2>,
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

        fn push_translate(&mut self, offset: Vec2) {
            self.translations.push(offset);
        }

        fn pop_transform(&mut self) {}
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
    fn scroll_view_default_vertical_ignores_shift_horizontal_scroll() {
        let child = Spacer::new(800.0, 800.0);
        let mut sv = ScrollView::new(Some(Box::new(child)));
        sv.layout(Rect::new(0.0, 0.0, 300.0, 300.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let dispatch = |_| {};
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);

        sv.event(
            &UiEvent::MouseWheel {
                delta: 50.0,
                position: Point::new(10.0, 10.0),
                modifiers: Modifiers::shift(),
            },
            &mut ctx,
        );

        assert_eq!(sv.scroll_offset().x, 0.0);
        assert_eq!(sv.scroll_offset().y, 0.0);
    }

    #[test]
    fn scroll_view_horizontal_axis_shift_wheel_updates_x_and_relayouts_child() {
        let last_mouse_down = Rc::new(RefCell::new(None));
        let last_layout = Rc::new(RefCell::new(None));
        let child = RecordingChild {
            id: WidgetId::new(),
            preferred: Size::new(800.0, 200.0),
            last_mouse_down,
            last_layout: Rc::clone(&last_layout),
        };
        let mut sv = ScrollView::new(Some(Box::new(child))).with_axes(ScrollAxes::Horizontal);
        sv.layout(Rect::new(20.0, 30.0, 300.0, 200.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let dispatch = |_| {};
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);

        sv.event(
            &UiEvent::MouseWheel {
                delta: 40.0,
                position: Point::new(40.0, 50.0),
                modifiers: Modifiers::shift(),
            },
            &mut ctx,
        );

        assert_eq!(sv.scroll_offset().x, 40.0);
        assert_eq!(sv.scroll_offset().y, 0.0);
        assert_eq!(
            *last_layout.borrow(),
            Some(Rect::new(-20.0, 30.0, 800.0, 200.0))
        );
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn scroll_view_mouse_wheel_requests_repaint_when_offset_changes() {
        let child = Spacer::new(200.0, 800.0);
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

        sv.event(
            &UiEvent::MouseWheel {
                delta: 10.0,
                position: Point::new(10.0, 10.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(ctx.requests.repaint);
    }

    #[test]
    fn scroll_view_mouse_wheel_at_edge_does_not_request_repaint() {
        let child = Spacer::new(200.0, 800.0);
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

        sv.event(
            &UiEvent::MouseWheel {
                delta: -10.0,
                position: Point::new(10.0, 10.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(!ctx.requests.repaint);
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
    fn scroll_view_lays_out_child_in_scrolled_screen_space_and_forwards_events() {
        let last_mouse_down = Rc::new(RefCell::new(None));
        let last_layout = Rc::new(RefCell::new(None));
        let child = RecordingChild {
            id: WidgetId::new(),
            preferred: Size::new(200.0, 800.0),
            last_mouse_down: Rc::clone(&last_mouse_down),
            last_layout: Rc::clone(&last_layout),
        };
        let mut sv = ScrollView::new(Some(Box::new(child)));
        sv.layout(Rect::new(20.0, 30.0, 300.0, 300.0));
        assert_eq!(
            *last_layout.borrow(),
            Some(Rect::new(20.0, 30.0, 200.0, 800.0))
        );

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
        assert_eq!(
            *last_layout.borrow(),
            Some(Rect::new(20.0, -10.0, 200.0, 800.0))
        );
        sv.event(
            &UiEvent::MouseDown {
                position: Point::new(50.0, 70.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(*last_mouse_down.borrow(), Some(Point::new(50.0, 70.0)));
    }

    #[test]
    fn scroll_view_overlay_hit_test_uses_screen_space() {
        let last_hit = Rc::new(RefCell::new(None));
        let last_wheel = Rc::new(RefCell::new(None));
        let overlay_painted = Rc::new(RefCell::new(false));
        let child = OverlayChild::new(
            Size::new(100.0, 400.0),
            Rc::clone(&last_hit),
            last_wheel,
            overlay_painted,
        );
        let mut sv = ScrollView::new(Some(Box::new(child)));
        sv.layout(Rect::new(10.0, 20.0, 100.0, 100.0));
        sv.scroll_to_bottom();

        assert!(!sv.hit_test(Point::new(250.0, 300.0)));
        assert!(sv.overlay_hit_test(Point::new(250.0, 300.0)));
        assert_eq!(*last_hit.borrow(), Some(Point::new(250.0, 300.0)));
    }

    #[test]
    fn scroll_view_paint_overlay_uses_child_screen_layout_without_clip() {
        let last_hit = Rc::new(RefCell::new(None));
        let last_wheel = Rc::new(RefCell::new(None));
        let overlay_painted = Rc::new(RefCell::new(false));
        let child = OverlayChild::new(
            Size::new(100.0, 400.0),
            last_hit,
            last_wheel,
            Rc::clone(&overlay_painted),
        );
        let mut sv = ScrollView::new(Some(Box::new(child)));
        sv.layout(Rect::new(10.0, 20.0, 100.0, 100.0));
        sv.scroll_to_bottom();

        let mut encoder = RecordingEncoder::default();
        let theme = ThemePreset::Dark.build();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 500.0, 500.0),
        };

        sv.paint_overlay(&mut ctx);

        assert!(encoder.translations.is_empty());
        assert!(*overlay_painted.borrow());
    }

    #[test]
    fn scroll_view_routes_wheel_outside_viewport_to_open_child_overlay() {
        let last_hit = Rc::new(RefCell::new(None));
        let last_wheel = Rc::new(RefCell::new(None));
        let overlay_painted = Rc::new(RefCell::new(false));
        let child = OverlayChild::new(
            Size::new(100.0, 400.0),
            last_hit,
            Rc::clone(&last_wheel),
            overlay_painted,
        );
        let mut sv = ScrollView::new(Some(Box::new(child)));
        sv.layout(Rect::new(10.0, 20.0, 100.0, 100.0));
        sv.scroll_to_bottom();

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let dispatch = |_| {};
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);

        let result = sv.event(
            &UiEvent::MouseWheel {
                delta: 24.0,
                position: Point::new(250.0, 300.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(*last_wheel.borrow(), Some(Point::new(250.0, 300.0)));
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
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn scroll_view_horizontal_thumb_drag_updates_offset_and_releases_capture() {
        let child = Spacer::new(900.0, 200.0);
        let mut sv = ScrollView::new(Some(Box::new(child))).with_axes(ScrollAxes::Horizontal);
        sv.layout(Rect::new(0.0, 0.0, 300.0, 200.0));

        let thumb = sv.horizontal_scrollbar_thumb_rect().expect("overflow should show thumb");
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
                position: Point::new(start.x + 75.0, start.y),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(sv.scroll_offset().x > 0.0);

        sv.event(
            &UiEvent::MouseUp {
                position: Point::new(start.x + 75.0, start.y),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Release(sv.id))
        );
    }

    #[test]
    fn scroll_view_both_axes_scrollbar_tracks_reserve_corner() {
        let child = Spacer::new(900.0, 800.0);
        let mut sv = ScrollView::new(Some(Box::new(child))).with_axes(ScrollAxes::Both);
        sv.layout(Rect::new(10.0, 20.0, 300.0, 200.0));

        let vertical = sv.vertical_scrollbar_track_rect();
        let horizontal = sv.horizontal_scrollbar_track_rect();

        assert_eq!(vertical.height, 192.0);
        assert_eq!(horizontal.width, 292.0);
        assert!(vertical.x >= horizontal.x + horizontal.width);
        assert!(horizontal.y >= vertical.y + vertical.height);
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
    fn scroll_view_hovering_thumb_requests_repaint() {
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

        sv.event(
            &UiEvent::MouseMove {
                position: thumb.center(),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(ctx.requests.repaint);
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
