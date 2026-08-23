use mondrian_ui_core::types::{Point, Rect};

use super::{measure_text_width, TextInputMetrics};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TextInputGeometry {
    pub(super) clip: Rect,
    pub(super) text_origin: Point,
    pub(super) content_left: f32,
    pub(super) content_right: f32,
    pub(super) visible_width: f32,
    pub(super) caret: Rect,
}

pub(super) fn line_height(font_size: f32) -> f32 {
    font_size * 1.3
}

pub(super) fn compute_text_geometry(
    bounds: Rect,
    scroll_x: f32,
    cursor_text_x: f32,
    preedit: Option<&str>,
    metrics: TextInputMetrics,
) -> TextInputGeometry {
    let font_size = metrics.font_size;
    let content_left = bounds.x + metrics.padding_x;
    let content_right = (bounds.x + bounds.width - metrics.padding_x).max(content_left);
    let visible_width = (bounds.width - metrics.padding_x * 2.0).max(1.0);
    let text_x = content_left - scroll_x;
    let text_y = bounds.y + (bounds.height - line_height(font_size)).max(0.0) * 0.5;
    let preedit_w = preedit.map_or(0.0, |text| measure_text_width(text, font_size));
    let caret_width = metrics.caret_width;
    let max_caret_x = (content_right - caret_width).max(content_left);
    let caret_x = (text_x + cursor_text_x + preedit_w).clamp(content_left, max_caret_x);

    TextInputGeometry {
        clip: Rect::new(content_left, bounds.y, visible_width, bounds.height),
        text_origin: Point::new(text_x, text_y),
        content_left,
        content_right,
        visible_width,
        caret: Rect::new(
            caret_x,
            bounds.y + metrics.padding_y,
            caret_width,
            (bounds.height - metrics.padding_y * 2.0).max(1.0),
        ),
    }
}

pub(super) fn text_x_from_pointer(
    position: Point,
    content_left: f32,
    content_right: f32,
    scroll_x: f32,
    text_width: f32,
) -> f32 {
    if position.x < content_left {
        0.0
    } else if position.x > content_right {
        text_width
    } else {
        (position.x - content_left + scroll_x).max(0.0)
    }
}

pub(super) fn scroll_offset_after_cursor(
    current_scroll: f32,
    text_width: f32,
    visible_width: f32,
    cursor_x: f32,
) -> f32 {
    let mut scroll = current_scroll;
    if cursor_x - scroll > visible_width - 4.0 {
        scroll = (cursor_x - visible_width + 4.0).min(text_width - visible_width).max(0.0);
    } else if cursor_x - scroll < 0.0 {
        scroll = cursor_x.max(0.0);
    }
    scroll.min((text_width - visible_width).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_clamps_caret_inside_content_even_with_preedit() {
        let geometry = compute_text_geometry(
            Rect::new(10.0, 20.0, 80.0, 28.0),
            0.0,
            1_000.0,
            Some("preedit"),
            TextInputMetrics::default(),
        );

        assert_eq!(geometry.content_left, 18.0);
        assert_eq!(geometry.content_right, 82.0);
        assert!(geometry.caret.x >= geometry.content_left);
        assert!(geometry.caret.x + geometry.caret.width <= geometry.content_right + 0.1);
    }

    #[test]
    fn pointer_text_x_maps_outside_points_to_text_edges() {
        assert_eq!(
            text_x_from_pointer(Point::new(4.0, 0.0), 10.0, 40.0, 8.0, 120.0),
            0.0
        );
        assert_eq!(
            text_x_from_pointer(Point::new(44.0, 0.0), 10.0, 40.0, 8.0, 120.0),
            120.0
        );
        assert_eq!(
            text_x_from_pointer(Point::new(20.0, 0.0), 10.0, 40.0, 8.0, 120.0),
            18.0
        );
    }

    #[test]
    fn scroll_offset_keeps_cursor_visible_and_clamped_to_text_width() {
        assert_eq!(scroll_offset_after_cursor(0.0, 200.0, 50.0, 70.0), 24.0);
        assert_eq!(scroll_offset_after_cursor(80.0, 200.0, 50.0, 60.0), 60.0);
        assert_eq!(scroll_offset_after_cursor(180.0, 100.0, 50.0, 160.0), 50.0);
        assert_eq!(scroll_offset_after_cursor(20.0, 30.0, 50.0, 28.0), 0.0);
    }
}
