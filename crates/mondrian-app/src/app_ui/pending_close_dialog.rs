//! Pending close confirmation dialog for the app UI shell.

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_widgets::{Button, DialogSurface, Label};

use crate::app::ui_actions::{
    app_shell_pending_close_cancel_action, app_shell_pending_close_discard_action,
    app_shell_pending_close_save_continue_action,
};

const CARD_MIN_WIDTH: f32 = 360.0;
const CARD_WIDTH: f32 = 500.0;
const CARD_MIN_HEIGHT: f32 = 190.0;
const CARD_HEIGHT: f32 = 230.0;
const CONTENT_PADDING: f32 = 22.0;
const TITLE_FONT_SIZE: f32 = 18.0;
const BODY_FONT_SIZE: f32 = 13.0;
const TITLE_Y: f32 = 22.0;
const BODY_Y: f32 = 62.0;
const BUTTON_WIDTH: f32 = 118.0;
const BUTTON_HEIGHT: f32 = 32.0;
const BUTTON_GAP: f32 = 10.0;
const BUTTON_BOTTOM_INSET: f32 = 20.0;

/// Pending close/quit flow represented by the confirmation dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingCloseDialogAction {
    CloseProject,
    QuitApp,
}

impl PendingCloseDialogAction {
    fn action_text(self) -> &'static str {
        match self {
            Self::CloseProject => "关闭项目",
            Self::QuitApp => "退出应用",
        }
    }
}

/// Modal asking whether unsaved project changes should be saved before closing.
pub struct PendingCloseDialog {
    id: WidgetId,
    action: PendingCloseDialogAction,
    surface: DialogSurface,
    bounds: Rect,
    card: Rect,
    title_label: Label,
    body_label: Label,
    save_button: Button,
    discard_button: Button,
    cancel_button: Button,
}

impl PendingCloseDialog {
    /// Build a pending-close confirmation dialog.
    pub fn new(action: PendingCloseDialogAction) -> Self {
        Self {
            id: WidgetId::new(),
            action,
            surface: DialogSurface::new(
                Size::new(CARD_MIN_WIDTH, CARD_MIN_HEIGHT),
                Size::new(CARD_WIDTH, CARD_HEIGHT),
            )
            .with_content_padding(CONTENT_PADDING),
            bounds: Rect::ZERO,
            card: Rect::ZERO,
            title_label: Label::new("关闭前保存项目")
                .popover_foreground()
                .with_font_size(TITLE_FONT_SIZE)
                .with_padding(0.0, 0.0),
            body_label: Label::new(body_text(action))
                .muted()
                .with_font_size(BODY_FONT_SIZE)
                .with_padding(0.0, 0.0)
                .wrapped(),
            save_button: Button::new("保存并继续")
                .on_click(app_shell_pending_close_save_continue_action()),
            discard_button: Button::new("不保存")
                .on_click(app_shell_pending_close_discard_action()),
            cancel_button: Button::new("取消").on_click(app_shell_pending_close_cancel_action()),
        }
    }

    /// Pending close/quit action represented by this dialog.
    pub fn action(&self) -> PendingCloseDialogAction {
        self.action
    }
}

fn body_text(action: PendingCloseDialogAction) -> String {
    format!("正在{}。是否先保存当前项目？", action.action_text())
}

impl Widget for PendingCloseDialog {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        self.surface.preferred_size()
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.card = self.surface.card_rect(bounds);
        let content = self.surface.content_rect(self.card);
        self.title_label.layout(Rect::new(
            content.x,
            content.y + TITLE_Y,
            content.width,
            TITLE_FONT_SIZE * 1.4,
        ));
        self.body_label.layout(Rect::new(
            content.x,
            content.y + BODY_Y,
            content.width,
            BODY_FONT_SIZE * 4.0,
        ));

        let total_button_width = BUTTON_WIDTH * 3.0 + BUTTON_GAP * 2.0;
        let button_y = self.card.y + self.card.height - BUTTON_BOTTOM_INSET - BUTTON_HEIGHT;
        let mut button_x = self.card.x + self.card.width - CONTENT_PADDING - total_button_width;
        self.save_button
            .layout(Rect::new(button_x, button_y, BUTTON_WIDTH, BUTTON_HEIGHT));
        button_x += BUTTON_WIDTH + BUTTON_GAP;
        self.discard_button
            .layout(Rect::new(button_x, button_y, BUTTON_WIDTH, BUTTON_HEIGHT));
        button_x += BUTTON_WIDTH + BUTTON_GAP;
        self.cancel_button
            .layout(Rect::new(button_x, button_y, BUTTON_WIDTH, BUTTON_HEIGHT));
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                (ctx.dispatch)(app_shell_pending_close_cancel_action());
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Enter, .. } => {
                (ctx.dispatch)(app_shell_pending_close_save_continue_action());
                EventResult::Handled
            }
            UiEvent::MouseDown { position, .. }
                if self.surface.is_outside_card(self.card, *position) =>
            {
                EventResult::Handled
            }
            _ => {
                for button in [
                    &mut self.save_button,
                    &mut self.discard_button,
                    &mut self.cancel_button,
                ] {
                    if button.event(event, ctx) == EventResult::Handled {
                        return EventResult::Handled;
                    }
                }
                EventResult::Ignored
            }
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.surface.paint(self.bounds, self.card, ctx);
        self.title_label.paint(ctx);
        self.body_label.paint(ctx);
        self.save_button.paint(ctx);
        self.discard_button.paint(ctx);
        self.cancel_button.paint(ctx);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        5
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.title_label),
            1 => Some(&self.body_label),
            2 => Some(&self.save_button),
            3 => Some(&self.discard_button),
            4 => Some(&self.cancel_button),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.title_label),
            1 => Some(&mut self.body_label),
            2 => Some(&mut self.save_button),
            3 => Some(&mut self.discard_button),
            4 => Some(&mut self.cancel_button),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_close_dialog_body_names_action() {
        assert!(body_text(PendingCloseDialogAction::CloseProject).contains("关闭项目"));
        assert!(body_text(PendingCloseDialogAction::QuitApp).contains("退出应用"));
    }
}
