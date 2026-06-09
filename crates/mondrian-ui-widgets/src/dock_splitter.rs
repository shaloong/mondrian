//! Dock 分割器控件
//!
//! 水平/垂直方向可拖拽调整比例的分割容器。

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// DockSplitter —— 可拖拽调整比例的双子节点分割容器
///
/// 视觉分割线为 1px，但拖拽热区为 `grab_zone`（默认 6px），
/// 鼠标接近分割线即可拖拽，无需精确命中 1px 线条。
pub struct DockSplitter {
    id: WidgetId,
    direction: SplitDirection,
    /// 第一个子节点占的比例 [0.1, 0.9]
    ratio: f32,
    children: Vec<Box<dyn Widget>>,
    bounds: Rect,
    /// 拖拽热区（大于视觉线条，让用户容易抓住）
    grab_rect: Rect,
    /// 把手是否正在被拖拽
    dragging: bool,
    /// 热区是否被 hover
    handle_hovered: bool,
    /// 视觉分割线宽度（子布局间距，保持 1px）
    handle_size: f32,
    /// 交互热区宽度（鼠标检测范围，默认 8px）
    grab_zone: f32,
}

impl DockSplitter {
    pub fn new(
        direction: SplitDirection,
        ratio: f32,
        child_a: Box<dyn Widget>,
        child_b: Box<dyn Widget>,
    ) -> Self {
        Self {
            id: WidgetId::new(),
            direction,
            ratio: ratio.clamp(0.1, 0.9),
            children: vec![child_a, child_b],
            bounds: Rect::ZERO,
            grab_rect: Rect::ZERO,
            dragging: false,
            handle_hovered: false,
            handle_size: 1.0,
            grab_zone: 6.0,
        }
    }

    pub fn is_handle_hovered(&self) -> bool {
        self.handle_hovered
    }

    /// 设置交互热区宽度（默认 6.0）
    pub fn with_grab_zone(mut self, width: f32) -> Self {
        self.grab_zone = width.max(2.0);
        self
    }

    /// 收集自身及所有嵌套 DockSplitter 的热区位置和方向
    pub fn collect_grab_zones(&self) -> Vec<(Rect, SplitDirection)> {
        let mut zones = vec![(self.grab_rect, self.direction)];
        for child in &self.children {
            if let Some(splitter) =
                child.as_ref().as_any().and_then(|a| a.downcast_ref::<DockSplitter>())
            {
                zones.extend(splitter.collect_grab_zones());
            }
        }
        zones
    }

    fn compute_grab_rect(&self) -> Rect {
        let cx = self.bounds.x + self.bounds.width * self.ratio;
        let cy = self.bounds.y + self.bounds.height * self.ratio;
        let hw = self.grab_zone * 0.5;
        match self.direction {
            SplitDirection::Horizontal => {
                Rect::new(cx - hw, self.bounds.y, self.grab_zone, self.bounds.height)
            }
            SplitDirection::Vertical => {
                Rect::new(self.bounds.x, cy - hw, self.bounds.width, self.grab_zone)
            }
        }
    }

    fn handle_pointer_event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                if self.grab_rect.contains(*position) {
                    self.dragging = true;
                    self.handle_hovered = true;
                    ctx.request_pointer_capture(self.id);
                    return EventResult::Handled;
                }
            }
            UiEvent::MouseMove { position, .. } => {
                if self.dragging {
                    match self.direction {
                        SplitDirection::Horizontal => {
                            let rel = (position.x - self.bounds.x - self.handle_size * 0.5)
                                / (self.bounds.width - self.handle_size);
                            self.ratio = rel.clamp(0.1, 0.9);
                        }
                        SplitDirection::Vertical => {
                            let rel = (position.y - self.bounds.y - self.handle_size * 0.5)
                                / (self.bounds.height - self.handle_size);
                            self.ratio = rel.clamp(0.1, 0.9);
                        }
                    }
                    let bounds = self.bounds;
                    self.layout(bounds);
                    return EventResult::Handled;
                }
                self.handle_hovered = self.grab_rect.contains(*position);
                if self.handle_hovered {
                    return EventResult::Handled;
                }
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } => {
                if self.dragging {
                    self.dragging = false;
                    ctx.release_pointer_capture(self.id);
                    return EventResult::Handled;
                }
            }
            _ => {}
        }
        EventResult::Ignored
    }
}

impl Widget for DockSplitter {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        let child_a = self.children.first().map(|c| c.measure(constraint)).unwrap_or(Size::ZERO);
        let child_b = self.children.get(1).map(|c| c.measure(constraint)).unwrap_or(Size::ZERO);
        let preferred = match self.direction {
            SplitDirection::Horizontal => Size::new(
                child_a.width + child_b.width + self.handle_size,
                child_a.height.max(child_b.height),
            ),
            SplitDirection::Vertical => Size::new(
                child_a.width.max(child_b.width),
                child_a.height + child_b.height + self.handle_size,
            ),
        };
        constraint.constrain(preferred)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.grab_rect = self.compute_grab_rect();

        let (a_rect, b_rect) = match self.direction {
            SplitDirection::Horizontal => {
                let a_w = (bounds.width - self.handle_size) * self.ratio;
                let b_x = bounds.x + a_w + self.handle_size;
                (
                    Rect::new(bounds.x, bounds.y, a_w, bounds.height),
                    Rect::new(
                        b_x,
                        bounds.y,
                        bounds.width - a_w - self.handle_size,
                        bounds.height,
                    ),
                )
            }
            SplitDirection::Vertical => {
                let a_h = (bounds.height - self.handle_size) * self.ratio;
                let b_y = bounds.y + a_h + self.handle_size;
                (
                    Rect::new(bounds.x, bounds.y, bounds.width, a_h),
                    Rect::new(
                        bounds.x,
                        b_y,
                        bounds.width,
                        bounds.height - a_h - self.handle_size,
                    ),
                )
            }
        };

        if let Some(child) = self.children.get_mut(0) {
            child.layout(a_rect);
        }
        if let Some(child) = self.children.get_mut(1) {
            child.layout(b_rect);
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        let handle_result = self.handle_pointer_event(event, ctx);
        if handle_result == EventResult::Handled {
            return EventResult::Handled;
        }

        // Block movement events in the grab zone to prevent accidental child
        // interactions during a drag.
        if self.grab_rect.contains(match event {
            UiEvent::MouseDown { position, .. }
            | UiEvent::MouseUp { position, .. }
            | UiEvent::MouseMove { position, .. }
            | UiEvent::MouseWheel { position, .. }
            | UiEvent::DragEnter { position, .. }
            | UiEvent::DragOver { position, .. }
            | UiEvent::Drop { position, .. } => *position,
            _ => Point::new(-1.0, -1.0),
        }) {
            return EventResult::Handled;
        }

        // Forward to children
        for child in &mut self.children {
            if child.event(event, ctx) == EventResult::Handled {
                return EventResult::Handled;
            }
        }
        EventResult::Ignored
    }

    fn before_child_event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        self.handle_pointer_event(event, ctx)
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;

        // Handle color: highlight when dragging or hovered
        let handle_color = if self.dragging {
            tokens.primary
        } else if self.handle_hovered {
            tokens.ring
        } else {
            tokens.border
        };
        let handle_width = if self.dragging {
            4.0
        } else if self.handle_hovered {
            3.0
        } else {
            1.0
        };

        // Paint children first so the splitter handle always remains visible
        // above tab bars and panel backgrounds.
        for child in &self.children {
            child.paint(ctx);
        }

        // 绘制把手线条（视觉上保持细线，在热区中心）
        let (hx, hy) = (self.grab_rect.center().x, self.grab_rect.center().y);
        match self.direction {
            SplitDirection::Horizontal => {
                ctx.encoder.draw_line(
                    Point::new(hx, self.bounds.y + 4.0),
                    Point::new(hx, self.bounds.y + self.bounds.height - 4.0),
                    handle_width,
                    handle_color,
                );
            }
            SplitDirection::Vertical => {
                ctx.encoder.draw_line(
                    Point::new(self.bounds.x + 4.0, hy),
                    Point::new(self.bounds.x + self.bounds.width - 4.0, hy),
                    handle_width,
                    handle_color,
                );
            }
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn children(&self) -> &[Box<dyn Widget>] {
        &self.children
    }

    fn children_mut(&mut self) -> &mut [Box<dyn Widget>] {
        &mut self.children
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_ui_core::widget::DrawCommandEncoder;

    struct EmptyWidget {
        id: WidgetId,
        bounds: Rect,
    }

    impl EmptyWidget {
        fn new() -> Self {
            Self { id: WidgetId::new(), bounds: Rect::ZERO }
        }
    }

    impl Widget for EmptyWidget {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, constraint: LayoutConstraint) -> Size {
            constraint.constrain(Size::new(10.0, 10.0))
        }

        fn layout(&mut self, bounds: Rect) {
            self.bounds = bounds;
        }

        fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }

        fn paint(&self, _ctx: &mut PaintContext) {}

        fn hit_test(&self, point: Point) -> bool {
            self.bounds.contains(point)
        }
    }

    #[derive(Default)]
    struct RecordingEncoder {
        line_widths: Vec<f32>,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, _bounds: Rect) {}

        fn pop_clip(&mut self) {}

        fn draw_rect(&mut self, _bounds: Rect, _color: mondrian_core::Color, _corner_radius: f32) {}

        fn draw_line(
            &mut self,
            _start: Point,
            _end: Point,
            width: f32,
            _color: mondrian_core::Color,
        ) {
            self.line_widths.push(width);
        }

        fn draw_text(
            &mut self,
            _text: &str,
            _font_size: f32,
            _position: Point,
            _color: mondrian_core::Color,
        ) {
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    fn splitter(direction: SplitDirection) -> DockSplitter {
        let mut splitter = DockSplitter::new(
            direction,
            0.5,
            Box::new(EmptyWidget::new()),
            Box::new(EmptyWidget::new()),
        );
        splitter.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        splitter
    }

    fn event_ctx<'a>(
        focus: &'a mut DummyFocus,
        shortcut: &'a mut DummyShortcut,
        tooltip: &'a mut DummyTooltip,
    ) -> EventContext<'a> {
        make_event_ctx(focus, shortcut, tooltip, &|_| {})
    }

    #[test]
    fn default_grab_zone_is_six_pixels() {
        let splitter = splitter(SplitDirection::Horizontal);
        assert_eq!(splitter.grab_rect.width, 6.0);
    }

    #[test]
    fn before_child_event_captures_in_grab_zone() {
        let mut splitter = splitter(SplitDirection::Horizontal);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = event_ctx(&mut focus, &mut shortcut, &mut tooltip);

        let result = splitter.before_child_event(
            &UiEvent::MouseDown {
                position: splitter.grab_rect.center(),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert!(splitter.dragging);
        assert!(ctx.requests.pointer_capture.is_some());
    }

    #[test]
    fn handle_width_grows_on_hover_and_drag() {
        let mut splitter = splitter(SplitDirection::Vertical);
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();

        let mut encoder = RecordingEncoder::default();
        {
            let mut ctx = PaintContext {
                encoder: &mut encoder,
                theme: &theme,
                clip_rect: Rect::new(0.0, 0.0, 200.0, 100.0),
            };
            splitter.paint(&mut ctx);
        }
        assert_eq!(encoder.line_widths.last(), Some(&1.0));

        splitter.handle_hovered = true;
        let mut encoder = RecordingEncoder::default();
        {
            let mut ctx = PaintContext {
                encoder: &mut encoder,
                theme: &theme,
                clip_rect: Rect::new(0.0, 0.0, 200.0, 100.0),
            };
            splitter.paint(&mut ctx);
        }
        assert_eq!(encoder.line_widths.last(), Some(&3.0));

        splitter.dragging = true;
        let mut encoder = RecordingEncoder::default();
        {
            let mut ctx = PaintContext {
                encoder: &mut encoder,
                theme: &theme,
                clip_rect: Rect::new(0.0, 0.0, 200.0, 100.0),
            };
            splitter.paint(&mut ctx);
        }
        assert_eq!(encoder.line_widths.last(), Some(&4.0));
    }
}
