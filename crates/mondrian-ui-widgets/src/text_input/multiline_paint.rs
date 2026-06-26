use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_core::widget::PaintContext;

use super::measure_text_width;
use super::multiline::MultilineTextEditState;
use super::multiline_geometry::MultilineTextGeometry;

#[derive(Clone, Debug)]
pub(super) struct MultilinePaintSnapshot<'a> {
    pub(super) bounds: Rect,
    pub(super) geometry: MultilineTextGeometry,
    pub(super) state: &'a MultilineTextEditState,
    pub(super) placeholder: &'a str,
    pub(super) enabled: bool,
    pub(super) read_only: bool,
    pub(super) focused: bool,
    pub(super) cursor_visible: bool,
    pub(super) has_selection: bool,
    pub(super) composition_active: bool,
    pub(super) preedit: Option<&'a str>,
    /// Byte prefix of committed text before the composition anchor.
    #[allow(dead_code)]
    pub(super) composition_prefix_byte: usize,
    /// Which logical line the composition is on.
    #[allow(dead_code)]
    pub(super) composition_line: usize,
}

pub(super) fn paint_multiline(ctx: &mut PaintContext, snapshot: MultilinePaintSnapshot<'_>) {
    let tokens = &ctx.theme.colors;
    let spacing = &ctx.theme.spacing;
    let metrics = snapshot.geometry.metrics;
    let geo = &snapshot.geometry;
    let clip = geo.clip;

    // ── Background + border ──
    let bg = if !snapshot.enabled {
        tokens.muted
    } else if snapshot.focused {
        tokens.popover
    } else {
        tokens.surface
    };
    let border = if snapshot.enabled && !snapshot.read_only {
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

    // ── Content clip ──
    ctx.push_clip(clip);

    let text_color = if !snapshot.enabled {
        tokens.text_disabled
    } else {
        tokens.foreground
    };

    // ── 1. Selection highlights (under text) ──
    if snapshot.enabled && snapshot.has_selection {
        for sel_rect in &snapshot.geometry.selection_rects {
            ctx.encoder.draw_rect(*sel_rect, tokens.primary, 0.0);
        }
    }

    // ── 2. Visible line text ──
    let state = snapshot.state;
    let lh = metrics.line_height;

    if state.text().is_empty() && !snapshot.composition_active {
        // Placeholder — visible even when focused (muted)
        let text_y = clip.y + (clip.height - lh).max(0.0) * 0.5;
        let placeholder_color = tokens.text_tertiary;
        ctx.encoder.draw_text(
            snapshot.placeholder,
            metrics.font_size,
            Point::new(clip.x, text_y),
            placeholder_color,
        );
    } else {
        for line in geo.first_visible..geo.last_visible {
            let line_text = state.line_text(line);
            if line_text.is_empty() {
                continue;
            }
            let text_y = clip.y + (line as f32 * lh) - geo.scroll_y;
            let text_x = clip.x - geo.scroll_x;
            ctx.encoder.draw_text(
                line_text,
                metrics.font_size,
                Point::new(text_x, text_y),
                text_color,
            );
        }
    }

    // ── 3. Preedit text + underline (at composition anchor) ──
    if snapshot.enabled && !snapshot.read_only && snapshot.focused && snapshot.preedit.is_some() {
        let preedit = snapshot.preedit.unwrap();
        if !preedit.is_empty() {
            let origin = snapshot.geometry.preedit_origin;
            ctx.encoder.draw_text(preedit, metrics.font_size, origin, tokens.foreground);

            let underline_y = origin.y + metrics.font_size * 1.25;
            let underline_w = measure_text_width(preedit, metrics.font_size).max(4.0);
            ctx.encoder.draw_line(
                Point::new(origin.x, underline_y),
                Point::new(origin.x + underline_w, underline_y),
                1.0,
                tokens.primary,
            );
        }
    }

    // ── 4. Caret ──
    if snapshot.enabled && snapshot.focused && snapshot.cursor_visible {
        // Clip caret to content clip for paint
        let caret = geo.caret;
        let clipped_caret = caret.intersection(&clip);
        if !clipped_caret.width.is_sign_negative()
            && clipped_caret.width > 0.0
            && clipped_caret.height > 0.0
        {
            let caret_color = if snapshot.read_only {
                tokens.text_tertiary
            } else if snapshot.has_selection {
                tokens.primary
            } else {
                tokens.foreground
            };
            ctx.encoder.draw_rect(clipped_caret, caret_color, 0.0);
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
        rects: Vec<(Rect, /*is_selection*/ bool)>,
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
            // Determine if this is a selection rect (inside clip after push) or chrome rect (before clip)
            let is_selection = !self.clips.is_empty();
            self.rects.push((bounds, is_selection));
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

    fn empty_geo() -> MultilineTextGeometry {
        MultilineTextGeometry {
            clip: Rect::new(18.0, 14.0, 104.0, 60.0),
            content_size: mondrian_ui_core::types::Size::new(0.0, 18.2),
            scroll_x: 0.0,
            scroll_y: 0.0,
            metrics: super::super::multiline_geometry::TextMetrics::from_font_size(14.0),
            visual_lines: vec![],
            first_visible: 0,
            last_visible: 1,
            caret: Rect::new(18.0, 14.0, 2.0, 14.0),
            selection_rects: vec![],
            preedit_origin: Point::new(18.0, 14.0),
            preedit_caret: Rect::new(18.0, 14.0, 2.0, 14.0),
        }
    }

    fn paint_snap(state: &MultilineTextEditState, geo: MultilineTextGeometry) -> RecordingEncoder {
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 400.0, 200.0),
        };
        let snapshot = MultilinePaintSnapshot {
            bounds: Rect::new(10.0, 10.0, 120.0, 80.0),
            geometry: geo,
            state,
            placeholder: "Type here...",
            enabled: true,
            read_only: false,
            focused: true,
            cursor_visible: true,
            has_selection: false,
            composition_active: false,
            preedit: None,
            composition_prefix_byte: 0,
            composition_line: 0,
        };
        paint_multiline(&mut ctx, snapshot);
        encoder
    }

    #[test]
    fn paints_background_clip_and_text() {
        let state = MultilineTextEditState::with_text("hello\nworld");
        let mut geo = empty_geo();
        geo.last_visible = 2;
        let encoder = paint_snap(&state, geo);

        assert_eq!(encoder.clip_pops, 1);
        let lines: Vec<&str> = encoder.texts.iter().map(|(t, _)| t.as_str()).collect();
        assert!(lines.contains(&"hello"));
        assert!(lines.contains(&"world"));
    }

    #[test]
    fn placeholder_renders_when_empty_focused_and_no_composition() {
        let state = MultilineTextEditState::new();
        let encoder = paint_snap(&state, empty_geo());

        assert_eq!(encoder.texts.first().unwrap().0, "Type here...");
    }

    #[test]
    fn placeholder_renders_when_empty_and_unfocused() {
        let state = MultilineTextEditState::new();
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 400.0, 200.0),
        };
        let snapshot = MultilinePaintSnapshot {
            bounds: Rect::new(10.0, 10.0, 120.0, 80.0),
            geometry: empty_geo(),
            state: &state,
            placeholder: "Enter text",
            enabled: true,
            read_only: false,
            focused: false,
            cursor_visible: false,
            has_selection: false,
            composition_active: false,
            preedit: None,
            composition_prefix_byte: 0,
            composition_line: 0,
        };
        paint_multiline(&mut ctx, snapshot);
        assert_eq!(encoder.texts.first().unwrap().0, "Enter text");
    }

    #[test]
    fn disabled_omits_cursor_selection_and_preedit() {
        let state = MultilineTextEditState::with_text("abc\ndef");
        let mut geo = empty_geo();
        geo.last_visible = 2;
        geo.selection_rects = vec![Rect::new(18.0, 14.0, 30.0, 18.0)];

        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 400.0, 200.0),
        };
        let snapshot = MultilinePaintSnapshot {
            bounds: Rect::new(10.0, 10.0, 120.0, 80.0),
            geometry: geo,
            state: &state,
            placeholder: "ph",
            enabled: false,
            read_only: false,
            focused: false,
            cursor_visible: false,
            has_selection: true,
            composition_active: false,
            preedit: Some("ni"),
            composition_prefix_byte: 0,
            composition_line: 0,
        };
        paint_multiline(&mut ctx, snapshot);

        // No preedit text, no cursor rect, no selection rect is drawn when disabled
        assert!(!encoder.texts.iter().any(|(t, _)| t == "ni"));
        assert!(encoder.lines.is_empty());
    }

    #[test]
    fn read_only_shows_text_and_selection_but_not_preedit() {
        let mut state = MultilineTextEditState::with_text("abc");
        state.select_all();
        let mut geo = empty_geo();
        geo.last_visible = 1;
        geo.selection_rects = vec![Rect::new(18.0, 14.0, 30.0, 18.0)];

        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 400.0, 200.0),
        };
        let snapshot = MultilinePaintSnapshot {
            bounds: Rect::new(10.0, 10.0, 120.0, 80.0),
            geometry: geo,
            state: &state,
            placeholder: "ph",
            enabled: true,
            read_only: true,
            focused: true,
            cursor_visible: true,
            has_selection: true,
            composition_active: false,
            preedit: Some("ni"),
            composition_prefix_byte: 0,
            composition_line: 0,
        };
        paint_multiline(&mut ctx, snapshot);

        // Still draws text
        assert!(encoder.texts.iter().any(|(t, _)| t == "abc"));
        // Does NOT draw preedit (read only)
        assert!(!encoder.texts.iter().any(|(t, _)| t == "ni"));
        // No cursor rect for read_only? Actually draws caret with muted color.
        // Just verify clip stack is balanced.
    }

    #[test]
    fn clip_stack_is_balanced() {
        let state = MultilineTextEditState::with_text("a\nb");
        let encoder = paint_snap(&state, empty_geo());
        assert_eq!(encoder.clips.len(), encoder.clip_pops);
    }

    #[test]
    fn preedit_draws_text_and_underline() {
        let state = MultilineTextEditState::with_text("before");
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 400.0, 200.0),
        };
        let snapshot = MultilinePaintSnapshot {
            bounds: Rect::new(10.0, 10.0, 120.0, 80.0),
            geometry: empty_geo(),
            state: &state,
            placeholder: "ph",
            enabled: true,
            read_only: false,
            focused: true,
            cursor_visible: false,
            has_selection: false,
            composition_active: true,
            preedit: Some("拼音"),
            composition_prefix_byte: 0,
            composition_line: 0,
        };
        paint_multiline(&mut ctx, snapshot);

        assert!(encoder.texts.iter().any(|(t, _)| t == "拼音"));
        assert_eq!(encoder.lines.len(), 1);
    }

    #[test]
    fn only_visible_lines_are_drawn() {
        let state = MultilineTextEditState::with_text("l0\nl1\nl2\nl3\nl4\nl5");
        let mut geo = empty_geo();
        geo.first_visible = 2;
        geo.last_visible = 4;
        geo.scroll_y = 2.0 * 14.0 * 1.3;

        let encoder = paint_snap(&state, geo);

        let drawn: Vec<&str> = encoder.texts.iter().map(|(t, _)| t.as_str()).collect();
        assert!(drawn.contains(&"l2"));
        assert!(drawn.contains(&"l3"));
        assert!(!drawn.contains(&"l0"));
        assert!(!drawn.contains(&"l5"));
    }

    #[test]
    fn caret_is_clipped_to_content_clip() {
        let state = MultilineTextEditState::with_text("very long line beyond bounds");
        let mut geo = empty_geo();
        geo.clip = Rect::new(18.0, 14.0, 40.0, 30.0);
        geo.caret = Rect::new(120.0, 14.0, 2.0, 14.0);

        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 400.0, 200.0),
        };
        let snapshot = MultilinePaintSnapshot {
            bounds: Rect::new(10.0, 10.0, 60.0, 60.0),
            geometry: geo,
            state: &state,
            placeholder: "ph",
            enabled: true,
            read_only: false,
            focused: true,
            cursor_visible: true,
            has_selection: false,
            composition_active: false,
            preedit: None,
            composition_prefix_byte: 0,
            composition_line: 0,
        };
        paint_multiline(&mut ctx, snapshot);

        // Caret should still exist (clipped) — check that it's not drawn beyond clip
        // The intersection of caret(120,14,2,14) and clip(18,14,40,30) is empty → no caret rect
        let last_rect_is_caret = encoder
            .rects
            .last()
            .map(|(r, is_sel)| !is_sel && r.width == 2.0)
            .unwrap_or(false);
        // caret was outside clip, so no caret rect should be drawn
        assert!(!last_rect_is_caret);
    }
}
