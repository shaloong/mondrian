//! Tooltip 弹出框 Widget
//!
//! 在指定锚点附近绘制带背景的文字提示。

use mondrian_ui_core::tooltip::TooltipState;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// Tooltip 弹出框 Widget
///
/// 当 `TooltipState::visible` 时绘制圆角背景 + 文字。
/// 位置由 TooltipState 指定。
pub struct TooltipWidget {
    id: WidgetId,
    state: TooltipState,
    bounds: Rect,
}

impl TooltipWidget {
    pub fn new() -> Self {
        Self {
            id: WidgetId::new(),
            state: TooltipState {
                text: String::new(),
                position: Point::ZERO,
                visible: false,
            },
            bounds: Rect::ZERO,
        }
    }

    pub fn update_state(&mut self, state: TooltipState) {
        self.state = state;
    }

    pub fn clear(&mut self) {
        self.state.visible = false;
    }
}

impl Widget for TooltipWidget {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        if !self.state.visible {
            return Size::ZERO;
        }
        // Rough estimate based on character count
        let char_count = self.state.text.chars().count() as f32;
        Size::new(char_count * 8.0 + 16.0, 24.0)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        if !self.state.visible || self.state.text.is_empty() {
            return;
        }

        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;

        // Estimate tooltip size from text
        let char_count = self.state.text.chars().count() as f32;
        let tw = (char_count * 8.0 + 16.0).max(40.0);
        let th = 24.0;
        let offset = spacing.tooltip_offset;

        // Position below and to the right of anchor, clamped to screen
        let x = self.state.position.x + offset;
        let y = self.state.position.y + offset;

        let bg = Rect::new(x, y, tw, th);
        let border = tokens.ring;

        // Background
        ctx.encoder.draw_rect(bg, tokens.popover, spacing.radius_sm);
        // Simple border by drawing a slightly larger rect behind
        let border_rect = bg.inset(-1.0, -1.0);
        ctx.encoder.draw_rect(border_rect, border, spacing.radius_sm);

        // Text is not drawn in paint() — handled by app-level TextRenderer pass
    }

    fn hit_test(&self, point: Point) -> bool {
        if !self.state.visible {
            return false;
        }
        self.bounds.contains(point)
    }
}

impl Default for TooltipWidget {
    fn default() -> Self {
        Self::new()
    }
}
