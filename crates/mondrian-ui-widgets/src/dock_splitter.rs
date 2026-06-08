//! Dock 分割器控件
//!
//! 水平/垂直方向可拖拽调整比例的分割容器。

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// DockSplitter —— 可拖拽调整比例的双子节点分割容器
///
/// 视觉分割线为 1px，但拖拽热区为 `grab_zone`（默认 8px），
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
            grab_zone: 8.0,
        }
    }

    pub fn is_handle_hovered(&self) -> bool {
        self.handle_hovered
    }

    /// 设置交互热区宽度（默认 8.0）
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

    fn event(&mut self, event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                if self.grab_rect.contains(*position) {
                    self.dragging = true;
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
                    // Re-layout children with the updated ratio
                    let bounds = self.bounds;
                    self.layout(bounds);
                    return EventResult::Handled;
                }
                // 检查 hover (don't stop propagation — let children receive MouseMove too)
                self.handle_hovered = self.grab_rect.contains(*position);
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } => {
                if self.dragging {
                    self.dragging = false;
                    return EventResult::Handled;
                }
            }
            _ => {}
        }

        // Block movement events in the grab zone to prevent accidental child
        // interactions during a drag. MouseDown is NOT blocked here — it is
        // only caught by the before-match check when truly starting a drag.
        if self.grab_rect.contains(match event {
            UiEvent::MouseMove { position, .. }
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
            if child.event(event, _ctx) == EventResult::Handled {
                return EventResult::Handled;
            }
        }
        EventResult::Ignored
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

        // 绘制把手线条（视觉上保持细线，在热区中心）
        let (hx, hy) = (self.grab_rect.center().x, self.grab_rect.center().y);
        match self.direction {
            SplitDirection::Horizontal => {
                ctx.encoder.draw_line(
                    Point::new(hx, self.bounds.y + 4.0),
                    Point::new(hx, self.bounds.y + self.bounds.height - 4.0),
                    2.0,
                    handle_color,
                );
            }
            SplitDirection::Vertical => {
                ctx.encoder.draw_line(
                    Point::new(self.bounds.x + 4.0, hy),
                    Point::new(self.bounds.x + self.bounds.width - 4.0, hy),
                    2.0,
                    handle_color,
                );
            }
        }

        // Paint children
        for child in &self.children {
            child.paint(ctx);
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
