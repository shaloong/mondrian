//! Shared modal dialog surface primitives.
//!
//! This module keeps dialog chrome, centering, and outside-hit handling
//! consistent without taking ownership of each dialog's child layout.

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::PaintContext;
use mondrian_ui_theme::{Theme, ThemePreset};

use crate::paint::paint_popover_shadow;

#[derive(Debug, Clone, Copy, PartialEq)]
struct DialogSurfaceVisualTokens {
    content_padding: f32,
    border_width: f32,
    scrim_radius: f32,
    card_radius: f32,
    outline_radius: f32,
}

impl DialogSurfaceVisualTokens {
    fn from_theme(theme: &Theme) -> Self {
        let spacing = &theme.spacing;
        Self {
            content_padding: spacing.md * 2.0,
            border_width: spacing.border_standard,
            scrim_radius: spacing.radius_none,
            card_radius: spacing.radius_lg,
            outline_radius: spacing.radius_none,
        }
    }
}

impl Default for DialogSurfaceVisualTokens {
    fn default() -> Self {
        Self::from_theme(&ThemePreset::Dark.build())
    }
}

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
        let visual = DialogSurfaceVisualTokens::default();
        Self {
            min_size,
            preferred_size,
            content_padding: visual.content_padding,
            border_width: visual.border_width,
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
        self.paint_chrome(bounds, card, ctx);
        self.paint_card(card, ctx);
    }

    /// Paint scrim + shadow only (no card fill). Call [`paint_card`] after
    /// any content that should sit between shadow and card surface.
    pub fn paint_chrome(&self, bounds: Rect, card: Rect, ctx: &mut PaintContext) {
        let visual = DialogSurfaceVisualTokens::from_theme(ctx.theme);
        ctx.encoder.draw_rect(bounds, ctx.theme.colors.modal_scrim, visual.scrim_radius);
        paint_popover_shadow(ctx, card, visual.card_radius);
    }

    /// Paint the card fill with rounded corners on top of previously
    /// painted content (sidebar, etc).
    pub fn paint_card(&self, card: Rect, ctx: &mut PaintContext) {
        let visual = DialogSurfaceVisualTokens::from_theme(ctx.theme);
        ctx.encoder.draw_rect(card, ctx.theme.colors.popover, visual.card_radius);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::Color;
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
    fn visual_tokens_follow_theme_spacing() {
        let mut theme = ThemePreset::Dark.build();
        theme.spacing.md = 13.0;
        theme.spacing.border_standard = 2.0;
        theme.spacing.radius_none = 0.5;
        theme.spacing.radius_lg = 9.0;

        let visual = DialogSurfaceVisualTokens::from_theme(&theme);
        let surface = DialogSurface::new(Size::new(1.0, 1.0), Size::new(10.0, 10.0));

        assert_eq!(visual.content_padding, 26.0);
        assert_eq!(visual.border_width, 2.0);
        assert_eq!(visual.scrim_radius, 0.5);
        assert_eq!(visual.card_radius, 9.0);
        assert_eq!(
            surface.content_rect(Rect::new(0.0, 0.0, 100.0, 100.0)),
            Rect::new(20.0, 20.0, 60.0, 60.0),
            "default surface geometry preserves the dark preset token-derived padding"
        );
    }

    #[test]
    fn paint_uses_modal_theme_tokens() {
        let theme = ThemePreset::Dark.build();
        let visual = DialogSurfaceVisualTokens::from_theme(&theme);
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
                visual.scrim_radius,
            )
        );
        assert_eq!(
            recorder.commands[1],
            PaintCommand::Rect(
                Rect::new(10.0, 20.0, 50.0, 40.0),
                theme.colors.popover,
                visual.card_radius,
            )
        );
        assert_eq!(recorder.commands.len(), 5);
    }
}
