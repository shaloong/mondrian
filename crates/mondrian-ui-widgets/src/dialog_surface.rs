//! Shared modal dialog surface primitives.
//!
//! This module keeps dialog chrome, centering, and outside-hit handling
//! consistent without taking ownership of each dialog's child layout.

use mondrian_core::Color;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::PaintContext;

/// Theme-aware modal surface geometry and chrome.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DialogSurface {
    min_size: Size,
    preferred_size: Size,
    content_padding: f32,
    border_width: f32,
}

impl DialogSurface {
    /// Create a modal surface with minimum and preferred card sizes.
    pub fn new(min_size: Size, preferred_size: Size) -> Self {
        Self {
            min_size,
            preferred_size,
            content_padding: 20.0,
            border_width: 1.0,
        }
    }

    /// Set the inset used by dialogs to place their content.
    pub fn with_content_padding(mut self, padding: f32) -> Self {
        self.content_padding = padding.max(0.0);
        self
    }

    /// Set the outline width used for modal cards.
    pub fn with_border_width(mut self, width: f32) -> Self {
        self.border_width = width.max(0.0);
        self
    }

    /// Preferred outer size exposed to layout parents.
    pub fn preferred_size(&self) -> Size {
        self.preferred_size
    }

    /// Compute the centered card rectangle inside the available bounds.
    pub fn card_rect(&self, bounds: Rect) -> Rect {
        let width = bounds.width.clamp(self.min_size.width, self.preferred_size.width);
        let height = bounds.height.clamp(self.min_size.height, self.preferred_size.height);
        Rect::new(
            bounds.x + (bounds.width - width) * 0.5,
            bounds.y + (bounds.height - height) * 0.5,
            width,
            height,
        )
    }

    /// Compute the content rectangle inside a card.
    pub fn content_rect(&self, card: Rect) -> Rect {
        card.inset(self.content_padding, self.content_padding)
    }

    /// Return true when a point is outside the dialog card.
    pub fn is_outside_card(&self, card: Rect, point: Point) -> bool {
        !card.contains(point)
    }

    /// Paint the modal scrim, card, and subtle outline.
    pub fn paint(&self, bounds: Rect, card: Rect, ctx: &mut PaintContext) {
        ctx.encoder.draw_rect(
            bounds,
            ctx.theme.colors.modal_scrim,
            ctx.theme.spacing.radius_none,
        );
        ctx.encoder
            .draw_rect(card, ctx.theme.colors.popover, ctx.theme.spacing.radius_md);
        if self.border_width <= 0.0 {
            return;
        }
        paint_rect_outline(ctx, card, self.border_width, ctx.theme.colors.border);
    }
}

fn paint_rect_outline(ctx: &mut PaintContext, rect: Rect, width: f32, color: Color) {
    let top_left = Point::new(rect.x, rect.y);
    let top_right = Point::new(rect.x + rect.width, rect.y);
    let bottom_left = Point::new(rect.x, rect.y + rect.height);
    let bottom_right = Point::new(rect.x + rect.width, rect.y + rect.height);
    ctx.encoder.draw_line(top_left, top_right, width, color);
    ctx.encoder.draw_line(bottom_left, bottom_right, width, color);
    ctx.encoder.draw_line(top_left, bottom_left, width, color);
    ctx.encoder.draw_line(top_right, bottom_right, width, color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;

    #[derive(Debug, PartialEq)]
    enum PaintCommand {
        Rect(Rect, Color, f32),
        Line(Point, Point, f32, Color),
    }

    #[derive(Default)]
    struct Recorder {
        commands: Vec<PaintCommand>,
    }

    impl DrawCommandEncoder for Recorder {
        fn push_clip(&mut self, _bounds: Rect) {}

        fn pop_clip(&mut self) {}

        fn draw_rect(&mut self, bounds: Rect, color: Color, corner_radius: f32) {
            self.commands.push(PaintCommand::Rect(bounds, color, corner_radius));
        }

        fn draw_line(&mut self, start: Point, end: Point, width: f32, color: Color) {
            self.commands.push(PaintCommand::Line(start, end, width, color));
        }

        fn draw_text(&mut self, _text: &str, _font_size: f32, _position: Point, _color: Color) {}

        fn draw_text_box(
            &mut self,
            _text: &str,
            _font_size: f32,
            _position: Point,
            _max_width: f32,
            _color: Color,
        ) {
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    #[test]
    fn card_rect_centers_and_clamps_to_preferred_size() {
        let surface = DialogSurface::new(Size::new(320.0, 240.0), Size::new(520.0, 390.0));

        let card = surface.card_rect(Rect::new(0.0, 0.0, 1000.0, 800.0));

        assert_eq!(card, Rect::new(240.0, 205.0, 520.0, 390.0));
    }

    #[test]
    fn card_rect_expands_to_minimum_when_bounds_are_smaller() {
        let surface = DialogSurface::new(Size::new(320.0, 240.0), Size::new(520.0, 390.0));

        let card = surface.card_rect(Rect::new(10.0, 20.0, 200.0, 100.0));

        assert_eq!(card, Rect::new(-50.0, -50.0, 320.0, 240.0));
    }

    #[test]
    fn content_rect_uses_configured_padding() {
        let surface = DialogSurface::new(Size::new(1.0, 1.0), Size::new(10.0, 10.0))
            .with_content_padding(3.0);

        assert_eq!(
            surface.content_rect(Rect::new(1.0, 2.0, 30.0, 40.0)),
            Rect::new(4.0, 5.0, 24.0, 34.0)
        );
    }

    #[test]
    fn paint_uses_modal_theme_tokens() {
        let theme = ThemePreset::Dark.build();
        let mut recorder = Recorder::default();
        let mut ctx = PaintContext {
            encoder: &mut recorder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 200.0, 100.0),
        };
        let surface = DialogSurface::new(Size::new(1.0, 1.0), Size::new(10.0, 10.0));

        surface.paint(
            Rect::new(0.0, 0.0, 200.0, 100.0),
            Rect::new(10.0, 20.0, 50.0, 40.0),
            &mut ctx,
        );

        assert_eq!(
            recorder.commands[0],
            PaintCommand::Rect(
                Rect::new(0.0, 0.0, 200.0, 100.0),
                theme.colors.modal_scrim,
                theme.spacing.radius_none,
            )
        );
        assert_eq!(
            recorder.commands[1],
            PaintCommand::Rect(
                Rect::new(10.0, 20.0, 50.0, 40.0),
                theme.colors.popover,
                theme.spacing.radius_md,
            )
        );
        assert_eq!(recorder.commands.len(), 6);
    }
}
