//! ScrollView 控件
//!
//! 虚拟滚动容器。用 PushTranslate/PopTransform + PushClip/PopClip 实现裁剪和偏移。

use glam::Vec2;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{DrawCommandEncoder, EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_theme::Theme;

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
            let child_bounds = Rect::new(0.0, 0.0, bounds.width - self.scrollbar_width, 0.0);
            child.layout(child_bounds);
            let measured = child.measure(LayoutConstraint::LOOSE);
            self.content_size = measured;
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseWheel { delta, .. } => {
                self.scroll_offset.y += *delta;
                self.scroll_offset.y = self
                    .scroll_offset
                    .y
                    .clamp(0.0, (self.content_size.height - self.bounds.height).max(0.0));
                EventResult::Handled
            }
            _ => {
                if let Some(ref mut child) = self.child {
                    let mut offset_event = event.clone();
                    if let UiEvent::MouseDown {
                        ref mut position, ..
                    }
                    | UiEvent::MouseUp {
                        ref mut position, ..
                    }
                    | UiEvent::MouseMove {
                        ref mut position, ..
                    } = &mut offset_event
                    {
                        position.x += self.scroll_offset.x;
                        position.y -= self.scroll_offset.y;
                    }
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
        ctx.encoder.push_translate(Vec2::new(
            -self.scroll_offset.x,
            -self.scroll_offset.y,
        ));
        if let Some(ref child) = self.child {
            child.paint(ctx);
        }
        ctx.encoder.pop_transform();

        // 绘制滚动条
        if self.content_size.height > self.bounds.height {
            let thumb_h = (self.bounds.height / self.content_size.height) * self.bounds.height;
            let thumb_y = (self.scroll_offset.y / self.content_size.height) * self.bounds.height;
            let sb_rect = Rect::new(
                self.bounds.x + self.bounds.width - self.scrollbar_width + 2.0,
                self.bounds.y + thumb_y,
                self.scrollbar_width - 4.0,
                thumb_h.max(16.0),
            );
            ctx.encoder.draw_rect(
                sb_rect,
                ctx.theme.colors.text_muted,
                tokens.radius_sm,
            );
        }

        ctx.encoder.pop_clip();
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn children(&self) -> &[Box<dyn Widget>] {
        // ScrollView children managed internally
        &[]
    }

    fn children_mut(&mut self) -> &mut [Box<dyn Widget>] {
        &mut []
    }
}
