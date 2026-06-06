//! Button 控件
//!
//! 支持 Normal / Hovered / Pressed 三态 + 点击派发 Action。

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{DrawCommandEncoder, EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_theme::Theme;

/// 按钮状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonState {
    Normal,
    Hovered,
    Pressed,
}

/// Button Widget —— 可点击的标签按钮
pub struct Button {
    id: WidgetId,
    label: String,
    bounds: Rect,
    state: ButtonState,
    pub on_click: Option<Action>,
}

impl Button {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            id: WidgetId::new(),
            label: label.into(),
            bounds: Rect::ZERO,
            state: ButtonState::Normal,
            on_click: None,
        }
    }

    pub fn on_click(mut self, action: Action) -> Self {
        self.on_click = Some(action);
        self
    }

    pub fn state(&self) -> ButtonState {
        self.state
    }
}

impl Widget for Button {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        // 简化的尺寸估算（后续用 TextRenderer 精确测量）
        let char_count = self.label.chars().count() as f32;
        Size::new(12.0 * char_count + 24.0, 28.0)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } if self.bounds.contains(*position) => {
                self.state = ButtonState::Pressed;
                EventResult::Handled
            }
            UiEvent::MouseUp { position, button: MouseButton::Left, .. } => {
                if self.state == ButtonState::Pressed && self.bounds.contains(*position) {
                    if let Some(action) = &self.on_click {
                        (ctx.dispatch)(action.clone());
                    }
                }
                self.state = ButtonState::Normal;
                EventResult::Handled
            }
            UiEvent::FocusGained => {
                self.state = ButtonState::Hovered;
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                self.state = ButtonState::Normal;
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;

        let bg = match self.state {
            ButtonState::Normal => tokens.bg_surface,
            ButtonState::Hovered => tokens.bg_surface_hover,
            ButtonState::Pressed => tokens.bg_surface_active,
        };

        ctx.encoder.draw_rect(self.bounds, bg, spacing.radius_md);

        // 文字绘制由 TextRenderer 在应用层生成
        // 此处的标签暂用矩形占位
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}
