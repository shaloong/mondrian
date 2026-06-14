//! Self-hosted About dialog.
//!
//! Product shell modals live in their own modules and report shell-local
//! actions instead of mutating application state directly.

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_widgets::{Button, DialogSurface, Label};

use crate::app::ui_actions::app_shell_close_modal_action;

const CARD_MIN_WIDTH: f32 = 300.0;
const CARD_WIDTH: f32 = 420.0;
const CARD_MIN_HEIGHT: f32 = 220.0;
const CARD_HEIGHT: f32 = 260.0;
const CONTENT_PADDING: f32 = 22.0;
const TITLE_FONT_SIZE: f32 = 20.0;
const BODY_FONT_SIZE: f32 = 13.0;
const META_FONT_SIZE: f32 = 12.0;
const TITLE_Y: f32 = 22.0;
const VERSION_Y: f32 = 56.0;
const BODY_Y: f32 = 90.0;
const META_Y: f32 = 162.0;
const BUTTON_WIDTH: f32 = 84.0;
const BUTTON_HEIGHT: f32 = 32.0;
const BUTTON_BOTTOM_INSET: f32 = 20.0;

/// Product information modal for the self-hosted shell.
pub struct AboutDialog {
    id: WidgetId,
    surface: DialogSurface,
    bounds: Rect,
    card: Rect,
    title_label: Label,
    version_label: Label,
    body_label: Label,
    meta_label: Label,
    close_button: Button,
}

impl AboutDialog {
    /// Build the default Mondrian About dialog.
    pub fn new() -> Self {
        Self {
            id: WidgetId::new(),
            surface: DialogSurface::new(
                Size::new(CARD_MIN_WIDTH, CARD_MIN_HEIGHT),
                Size::new(CARD_WIDTH, CARD_HEIGHT),
            )
            .with_content_padding(CONTENT_PADDING),
            bounds: Rect::ZERO,
            card: Rect::ZERO,
            title_label: Label::new("Mondrian")
                .popover_foreground()
                .with_font_size(TITLE_FONT_SIZE)
                .with_padding(0.0, 0.0),
            version_label: Label::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                .muted()
                .with_font_size(META_FONT_SIZE)
                .with_padding(0.0, 0.0),
            body_label: Label::new(
                "A self-hosted video editing interface built on Mondrian's native UI stack.",
            )
            .popover_foreground()
            .with_font_size(BODY_FONT_SIZE)
            .with_padding(0.0, 0.0)
            .wrapped(),
            meta_label: Label::new("Custom UI runtime, dock panels, text, and GPU renderer.")
                .muted()
                .with_font_size(META_FONT_SIZE)
                .with_padding(0.0, 0.0)
                .wrapped(),
            close_button: Button::new("Done").on_click(app_shell_close_modal_action()),
        }
    }
}

impl Default for AboutDialog {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for AboutDialog {
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
        self.version_label.layout(Rect::new(
            content.x,
            content.y + VERSION_Y,
            content.width,
            META_FONT_SIZE * 1.4,
        ));
        self.body_label.layout(Rect::new(
            content.x,
            content.y + BODY_Y,
            content.width,
            BODY_FONT_SIZE * 4.0,
        ));
        self.meta_label.layout(Rect::new(
            content.x,
            content.y + META_Y,
            content.width,
            META_FONT_SIZE * 3.0,
        ));
        self.close_button.layout(Rect::new(
            self.card.x + self.card.width - CONTENT_PADDING - BUTTON_WIDTH,
            self.card.y + self.card.height - BUTTON_BOTTOM_INSET - BUTTON_HEIGHT,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        ));
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::KeyDown { key: KeyCode::Escape | KeyCode::Enter, .. } => {
                (ctx.dispatch)(app_shell_close_modal_action());
                EventResult::Handled
            }
            UiEvent::MouseDown { position, .. }
                if self.surface.is_outside_card(self.card, *position) =>
            {
                EventResult::Handled
            }
            _ => self.close_button.event(event, ctx),
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.surface.paint(self.bounds, self.card, ctx);
        self.title_label.paint(ctx);
        self.version_label.paint(ctx);
        self.body_label.paint(ctx);
        self.meta_label.paint(ctx);
        self.close_button.paint(ctx);
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
            1 => Some(&self.version_label),
            2 => Some(&self.body_label),
            3 => Some(&self.meta_label),
            4 => Some(&self.close_button),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.title_label),
            1 => Some(&mut self.version_label),
            2 => Some(&mut self.body_label),
            3 => Some(&mut self.meta_label),
            4 => Some(&mut self.close_button),
            _ => None,
        }
    }
}
