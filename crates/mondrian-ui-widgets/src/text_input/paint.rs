use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_core::widget::PaintContext;

use super::geometry::TextInputGeometry;
use super::measure_text_width;

#[derive(Clone, Copy, Debug)]
pub(super) struct TextInputPaintSnapshot<'a> {
    pub(super) bounds: Rect,
    pub(super) geometry: TextInputGeometry,
    pub(super) enabled: bool,
    pub(super) focused: bool,
    pub(super) cursor_visible: bool,
    pub(super) has_selection: bool,
    pub(super) text: &'a str,
    pub(super) placeholder: &'a str,
    pub(super) selection_byte_range: Option<(usize, usize)>,
    pub(super) cursor_prefix_byte: usize,
    pub(super) preedit: Option<&'a str>,
}

pub(super) fn paint_text_input(ctx: &mut PaintContext, snapshot: TextInputPaintSnapshot<'_>) {
    let tokens = &ctx.theme.colors;
    let spacing = &ctx.theme.spacing;
    let font_size = ctx.theme.typography.body.font_size;

    let bg = if !snapshot.enabled {
        tokens.muted
    } else if snapshot.focused {
        tokens.popover
    } else {
        tokens.surface
    };
    let border = if snapshot.enabled {
        tokens.border_for_state(snapshot.focused)
    } else {
        tokens.border
    };
    let border_inset = 1.0;
    ctx.encoder.draw_rect(
        snapshot.bounds.inset(-border_inset, -border_inset),
        border,
        spacing.radius_sm + border_inset,
    );
    ctx.encoder.draw_rect(snapshot.bounds, bg, spacing.radius_sm);

    ctx.push_clip(snapshot.geometry.clip);

    let text_x = snapshot.geometry.text_origin.x;
    let text_y = snapshot.geometry.text_origin.y;

    if snapshot.enabled
        && let Some((byte_start, byte_end)) = snapshot.selection_byte_range
    {
        let sel_x = text_x + measure_text_width(&snapshot.text[..byte_start], font_size);
        let sel_w = measure_text_width(&snapshot.text[byte_start..byte_end], font_size);
        let sel_h = font_size * 1.3;
        let sel_y = snapshot.bounds.y + (snapshot.bounds.height - sel_h).max(0.0) * 0.5;
        ctx.encoder
            .draw_rect(Rect::new(sel_x, sel_y, sel_w, sel_h), tokens.primary, 0.0);
    }

    if !snapshot.text.is_empty() {
        ctx.encoder.draw_text(
            snapshot.text,
            font_size,
            Point::new(text_x, text_y),
            if snapshot.enabled {
                tokens.foreground
            } else {
                tokens.text_disabled
            },
        );
    } else if !snapshot.focused {
        ctx.encoder.draw_text(
            snapshot.placeholder,
            font_size,
            Point::new(text_x, text_y),
            tokens.text_tertiary,
        );
    }

    if snapshot.enabled && snapshot.focused {
        if let Some(preedit) = snapshot.preedit {
            let preedit_x = text_x
                + measure_text_width(&snapshot.text[..snapshot.cursor_prefix_byte], font_size);
            ctx.encoder.draw_text(
                preedit,
                font_size,
                Point::new(preedit_x, text_y),
                tokens.foreground,
            );
            let underline_y = text_y + font_size * 1.25;
            let underline_w = measure_text_width(preedit, font_size).max(4.0);
            ctx.encoder.draw_line(
                Point::new(preedit_x, underline_y),
                Point::new(preedit_x + underline_w, underline_y),
                1.0,
                tokens.primary,
            );
        }

        if snapshot.cursor_visible {
            let cursor_color = if snapshot.has_selection {
                tokens.primary
            } else {
                tokens.foreground
            };
            ctx.encoder.draw_rect(snapshot.geometry.caret, cursor_color, 0.0);
        }
    }

    ctx.pop_clip();
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;

    #[derive(Default)]
    struct RecordingEncoder {
        clips: Vec<Rect>,
        clip_pops: usize,
        rects: Vec<Rect>,
        lines: Vec<(Point, Point, f32)>,
        texts: Vec<(String, Point)>,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, bounds: Rect) {
            self.clips.push(bounds);
        }

        fn pop_clip(&mut self) {
            self.clip_pops += 1;
        }

        fn draw_rect(&mut self, bounds: Rect, _color: mondrian_core::Color, _corner_radius: f32) {
            self.rects.push(bounds);
        }

        fn draw_line(
            &mut self,
            start: Point,
            end: Point,
            width: f32,
            _color: mondrian_core::Color,
        ) {
            self.lines.push((start, end, width));
        }

        fn draw_text(
            &mut self,
            text: &str,
            _font_size: f32,
            position: Point,
            _color: mondrian_core::Color,
        ) {
            self.texts.push((text.into(), position));
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    fn paint_snapshot<'a>(
        enabled: bool,
        focused: bool,
        cursor_visible: bool,
        text: &'a str,
        placeholder: &'a str,
    ) -> TextInputPaintSnapshot<'a> {
        TextInputPaintSnapshot {
            bounds: Rect::new(10.0, 20.0, 120.0, 28.0),
            geometry: TextInputGeometry {
                clip: Rect::new(18.0, 20.0, 104.0, 28.0),
                text_origin: Point::new(18.0, 24.8),
                content_left: 18.0,
                content_right: 122.0,
                visible_width: 104.0,
                caret: Rect::new(32.0, 24.0, 2.0, 20.0),
            },
            enabled,
            focused,
            cursor_visible,
            has_selection: false,
            text,
            placeholder,
            selection_byte_range: None,
            cursor_prefix_byte: text.len(),
            preedit: None,
        }
    }

    fn with_paint(snapshot: TextInputPaintSnapshot<'_>) -> RecordingEncoder {
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 400.0, 200.0),
        };
        paint_text_input(&mut ctx, snapshot);
        encoder
    }

    #[test]
    fn paint_draws_background_clip_text_cursor_then_pop_clip() {
        let encoder = with_paint(paint_snapshot(true, true, true, "abc", "ph"));

        assert_eq!(encoder.clips, vec![Rect::new(18.0, 20.0, 104.0, 28.0)]);
        assert_eq!(encoder.clip_pops, 1);
        assert_eq!(
            encoder.texts.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>(),
            ["abc"]
        );
        assert_eq!(encoder.rects.last().map(|rect| rect.width), Some(2.0));
    }

    #[test]
    fn paint_placeholder_only_when_unfocused_and_empty() {
        let focused = with_paint(paint_snapshot(true, true, true, "", "Search"));
        assert!(focused.texts.is_empty());

        let unfocused = with_paint(paint_snapshot(true, false, false, "", "Search"));
        assert_eq!(unfocused.texts[0].0, "Search");
    }

    #[test]
    fn paint_preedit_underlines_before_cursor() {
        let mut snapshot = paint_snapshot(true, true, false, "ab", "ph");
        snapshot.cursor_prefix_byte = 1;
        snapshot.preedit = Some("你");

        let encoder = with_paint(snapshot);

        assert_eq!(
            encoder.texts.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>(),
            ["ab", "你"]
        );
        assert_eq!(encoder.lines.len(), 1);
    }

    #[test]
    fn disabled_input_omits_selection_preedit_and_cursor_chrome() {
        let mut snapshot = paint_snapshot(false, true, true, "abc", "ph");
        snapshot.selection_byte_range = Some((0, 1));
        snapshot.has_selection = true;
        snapshot.preedit = Some("x");

        let encoder = with_paint(snapshot);

        assert_eq!(
            encoder.texts.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>(),
            ["abc"]
        );
        assert!(encoder.lines.is_empty());
        assert_eq!(encoder.rects.len(), 2);
    }
}
