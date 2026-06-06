//! Dock 分割器控件
//!
//! 水平/垂直方向可拖拽调整比例的分割容器。

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// DockSplitter —— 可拖拽调整比例的双子节点分割容器
pub struct DockSplitter {
    id: WidgetId,
    direction: SplitDirection,
    /// 第一个子节点占的比例 [0.1, 0.9]
    ratio: f32,
    children: Vec<Box<dyn Widget>>,
    bounds: Rect,
    /// 拖拽把手的屏幕坐标区域
    handle_rect: Rect,
    /// 把手是否正在被拖拽
    dragging: bool,
    /// 把手是否被 hover
    handle_hovered: bool,
    /// 把手宽度（像素）
    handle_size: f32,
}

impl DockSplitter {
    pub fn new(direction: SplitDirection, ratio: f32, child_a: Box<dyn Widget>, child_b: Box<dyn Widget>) -> Self {
        Self {
            id: WidgetId::new(),
            direction,
            ratio: ratio.clamp(0.1, 0.9),
            children: vec![child_a, child_b],
            bounds: Rect::ZERO,
            handle_rect: Rect::ZERO,
            dragging: false,
            handle_hovered: false,
            handle_size: 1.0,
        }
    }

    fn compute_handle_rect(&self) -> Rect {
        let cx = self.bounds.x + self.bounds.width * self.ratio;
        let cy = self.bounds.y + self.bounds.height * self.ratio;
        let hw = self.handle_size * 0.5;
        match self.direction {
            SplitDirection::Horizontal => Rect::new(cx - hw, self.bounds.y, self.handle_size, self.bounds.height),
            SplitDirection::Vertical => Rect::new(self.bounds.x, cy - hw, self.bounds.width, self.handle_size),
        }
    }
}

impl Widget for DockSplitter {
    fn id(&self) -> WidgetId { self.id }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(200.0, 100.0))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.handle_rect = self.compute_handle_rect();

        let (a_rect, b_rect) = match self.direction {
            SplitDirection::Horizontal => {
                let a_w = (bounds.width - self.handle_size) * self.ratio;
                let b_x = bounds.x + a_w + self.handle_size;
                (
                    Rect::new(bounds.x, bounds.y, a_w, bounds.height),
                    Rect::new(b_x, bounds.y, bounds.width - a_w - self.handle_size, bounds.height),
                )
            }
            SplitDirection::Vertical => {
                let a_h = (bounds.height - self.handle_size) * self.ratio;
                let b_y = bounds.y + a_h + self.handle_size;
                (
                    Rect::new(bounds.x, bounds.y, bounds.width, a_h),
                    Rect::new(bounds.x, b_y, bounds.width, bounds.height - a_h - self.handle_size),
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
                if self.handle_rect.contains(*position) {
                    self.dragging = true;
                    return EventResult::Handled;
                }
            }
            UiEvent::MouseMove { position, .. } => {
                if self.dragging {
                    match self.direction {
                        SplitDirection::Horizontal => {
                            let rel = (position.x - self.bounds.x - self.handle_size * 0.5) / (self.bounds.width - self.handle_size);
                            self.ratio = rel.clamp(0.1, 0.9);
                        }
                        SplitDirection::Vertical => {
                            let rel = (position.y - self.bounds.y - self.handle_size * 0.5) / (self.bounds.height - self.handle_size);
                            self.ratio = rel.clamp(0.1, 0.9);
                        }
                    }
                    // Re-layout children with the updated ratio
                    let bounds = self.bounds;
                    self.layout(bounds);
                    return EventResult::Handled;
                }
                // 检查 hover
                let was_hovered = self.handle_hovered;
                self.handle_hovered = self.handle_rect.contains(*position);
                if self.handle_hovered != was_hovered {
                    return EventResult::Handled;
                }
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } => {
                if self.dragging {
                    self.dragging = false;
                    return EventResult::Handled;
                }
            }
            _ => {}
        }

        // 将事件转发给子节点（跳过 handle 区域的事件）
        if self.handle_rect.contains(match event {
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
            tokens.interaction_highlight
        } else if self.handle_hovered {
            tokens.border_emphasis
        } else {
            tokens.border_subtle
        };

        // 绘制把手线条
        let (hx, hy) = (self.handle_rect.center().x, self.handle_rect.center().y);
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

        // 子节点绘制由 TreeWalker 通过 children() 递归完成；
        // DockSplitter::paint 只负责绘制分割线把手。
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
}
