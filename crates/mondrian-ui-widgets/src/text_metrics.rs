//! Shared text measurement for widgets.

use mondrian_ui_text::TextRenderer;
use std::cell::RefCell;

thread_local! {
    static TEXT_MEASURER: RefCell<TextRenderer> = RefCell::new(TextRenderer::new());
}

pub(crate) fn measure_single_line(text: &str, font_size: f32) -> (f32, f32) {
    if text.is_empty() {
        return (0.0, 0.0);
    }
    TEXT_MEASURER.with_borrow_mut(|renderer| renderer.measure_text(text, font_size.max(1.0)))
}

pub(crate) fn measure_text_box(text: &str, font_size: f32, max_width: f32) -> (f32, f32) {
    if text.is_empty() {
        return (0.0, 0.0);
    }
    TEXT_MEASURER.with_borrow_mut(|renderer| {
        renderer.measure_text_box(text, font_size.max(1.0), max_width.max(1.0))
    })
}

pub(crate) fn centered_text_x(rect_x: f32, rect_width: f32, text: &str, font_size: f32) -> f32 {
    if text.is_empty() {
        return rect_x;
    }
    let (text_width, _) = measure_single_line(text, font_size);
    rect_x + (rect_width - text_width).max(0.0) * 0.5
}
