//! Shared panel header chrome — title, subtitle, and filter input layout.
//!
//! Used by [`PanelList`](crate::PanelList), [`AssetGrid`](crate::AssetGrid), and
//! [`PropertyPanel`](crate::PropertyPanel) to keep header rendering consistent.

use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_core::PaintContext;

use crate::paint::mix_color;

/// Height constants shared across all panel-style widgets.
pub const HEADER_TITLE_ONLY_HEIGHT: f32 = 42.0;
pub const HEADER_TITLE_WITH_SUBTITLE_HEIGHT: f32 = 60.0;
pub const FILTER_INPUT_HEIGHT: f32 = 30.0;
pub const FILTER_INPUT_GAP: f32 = 10.0;
pub const HEADER_CONTENT_PADDING: f32 = 8.0;
const EMBEDDED_FILTER_TOP_PADDING: f32 = 8.0;

/// Panel header chrome with optional title, subtitle, and filter input zone.
///
/// When `show_text` is `false` (via [`with_embedded`] or
/// [`set_embedded`]), title/subtitle text is skipped — useful when the panel
/// is hosted inside a dock whose tab bar already shows the title.
#[derive(Debug, Clone)]
pub struct PanelHeader {
    pub title: String,
    pub subtitle: String,
    pub show_text: bool,
    pub has_filter: bool,
}

impl PanelHeader {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            subtitle: String::new(),
            show_text: true,
            has_filter: false,
        }
    }

    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = subtitle.into();
        self
    }

    pub fn with_filter(mut self, has_filter: bool) -> Self {
        self.has_filter = has_filter;
        self
    }

    pub fn with_embedded(mut self) -> Self {
        self.show_text = false;
        self
    }

    pub fn set_embedded(&mut self) {
        self.show_text = false;
    }

    pub fn title_text(&self) -> &str {
        &self.title
    }

    /// Height of the title block alone (0 when hidden).
    pub fn title_block_height(&self) -> f32 {
        if !self.show_text {
            0.0
        } else if self.subtitle.is_empty() {
            HEADER_TITLE_ONLY_HEIGHT
        } else {
            HEADER_TITLE_WITH_SUBTITLE_HEIGHT
        }
    }

    /// Total header height including filter input gap when present.
    pub fn height(&self) -> f32 {
        let base = self.title_block_height();
        if self.has_filter {
            let top_pad = if self.show_text {
                0.0
            } else {
                EMBEDDED_FILTER_TOP_PADDING
            };
            base + top_pad + FILTER_INPUT_HEIGHT + FILTER_INPUT_GAP
        } else {
            base
        }
    }

    /// Position for the optional filter input within `bounds`.
    pub fn filter_input_rect(&self, bounds: Rect) -> Option<Rect> {
        if !self.has_filter {
            return None;
        }
        let y = if self.show_text {
            bounds.y + self.title_block_height() - 4.0
        } else {
            bounds.y + EMBEDDED_FILTER_TOP_PADDING
        };
        Some(Rect::new(
            bounds.x + HEADER_CONTENT_PADDING,
            y,
            (bounds.width - HEADER_CONTENT_PADDING * 2.0).max(0.0),
            FILTER_INPUT_HEIGHT,
        ))
    }

    /// Paint title and subtitle text inside `bounds`.
    ///
    /// Caller is responsible for painting the panel background and the
    /// filter input widget. This only covers the text labels.
    pub fn paint(&self, ctx: &mut PaintContext, bounds: Rect) {
        if !self.show_text {
            return;
        }
        let colors = &ctx.theme.colors;
        let title_pos =
            mondrian_ui_core::types::snap_point(Point::new(bounds.x + 12.0, bounds.y + 12.0));
        ctx.encoder.draw_text(
            &self.title,
            ctx.theme.typography.body.font_size,
            title_pos,
            colors.foreground,
        );
        if !self.subtitle.is_empty() {
            ctx.encoder.draw_text_box(
                &self.subtitle,
                ctx.theme.typography.small.font_size,
                mondrian_ui_core::types::snap_point(Point::new(bounds.x + 12.0, bounds.y + 34.0)),
                (bounds.width - 24.0).max(0.0),
                colors.muted_foreground,
            );
        }
    }

    /// Paint the panel background fill that covers the entire bounds.
    pub fn paint_background(&self, ctx: &mut PaintContext, bounds: Rect) {
        let colors = &ctx.theme.colors;
        ctx.encoder
            .draw_rect(bounds, mix_color(colors.background, colors.card, 0.24), 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_block_height_returns_zero_when_text_hidden() {
        let header = PanelHeader::new("Test").with_embedded();
        assert_eq!(header.title_block_height(), 0.0);
        assert_eq!(header.height(), 0.0);
    }

    #[test]
    fn title_only_height_is_42() {
        let header = PanelHeader::new("Test");
        assert_eq!(header.title_block_height(), HEADER_TITLE_ONLY_HEIGHT);
    }

    #[test]
    fn title_with_subtitle_height_is_60() {
        let header = PanelHeader::new("Test").with_subtitle("Sub");
        assert_eq!(
            header.title_block_height(),
            HEADER_TITLE_WITH_SUBTITLE_HEIGHT
        );
    }

    #[test]
    fn filter_rect_is_none_without_filter() {
        let header = PanelHeader::new("Test");
        let bounds = Rect::new(0.0, 0.0, 300.0, 200.0);
        assert!(header.filter_input_rect(bounds).is_none());
    }

    #[test]
    fn filter_rect_is_positioned_below_title_block() {
        let header = PanelHeader::new("Test").with_subtitle("Sub").with_filter(true);
        let bounds = Rect::new(0.0, 0.0, 300.0, 200.0);
        let rect = header.filter_input_rect(bounds).unwrap();
        assert_eq!(rect.y, HEADER_TITLE_WITH_SUBTITLE_HEIGHT - 4.0);
        assert_eq!(rect.height, FILTER_INPUT_HEIGHT);
    }

    #[test]
    fn height_accounts_for_filter_and_gap() {
        let header = PanelHeader::new("Test").with_subtitle("Sub").with_filter(true);
        assert_eq!(
            header.height(),
            HEADER_TITLE_WITH_SUBTITLE_HEIGHT + FILTER_INPUT_HEIGHT + FILTER_INPUT_GAP
        );
    }

    #[test]
    fn embedded_filter_uses_top_padding() {
        let header = PanelHeader::new("Test").with_embedded().with_filter(true);
        let bounds = Rect::new(0.0, 0.0, 300.0, 200.0);
        let rect = header.filter_input_rect(bounds).unwrap();
        assert_eq!(rect.y, 8.0); // EMBEDDED_FILTER_TOP_PADDING
    }
}
