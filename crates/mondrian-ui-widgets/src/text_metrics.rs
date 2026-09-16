//! Shared text measurement for widgets.

use mondrian_ui_text::TextRenderer;
use std::cell::RefCell;

thread_local! {
    static TEXT_MEASURER: RefCell<TextRenderer> = RefCell::new(new_text_measurer());
}

#[cfg(test)]
thread_local! {
    static MEASURER_INITIALIZATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn new_text_measurer() -> TextRenderer {
    #[cfg(test)]
    MEASURER_INITIALIZATIONS.with(|count| count.set(count.get() + 1));
    TextRenderer::new()
}

pub(crate) fn measure_single_line(text: &str, font_size: f32) -> (f32, f32) {
    measure_single_line_at_size(text, font_size.max(1.0))
}

/// Measure with the caller's exact size, without imposing a widget's size policy.
pub(crate) fn measure_single_line_at_size(text: &str, font_size: f32) -> (f32, f32) {
    if text.is_empty() {
        return (0.0, 0.0);
    }
    TEXT_MEASURER.with_borrow_mut(|renderer| renderer.measure_text(text, font_size))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Label, NumberInput};
    use mondrian_ui_core::{types::LayoutConstraint, Widget};

    #[test]
    fn empty_text_does_not_initialize_a_font_owner() {
        std::thread::spawn(|| {
            assert_eq!(measure_single_line("", 14.0), (0.0, 0.0));
            assert_eq!(measure_single_line_at_size("", 0.5), (0.0, 0.0));
            assert_eq!(measure_text_box("", 14.0, 80.0), (0.0, 0.0));
            assert_eq!(MEASURER_INITIALIZATIONS.with(|count| count.get()), 0);
        })
        .join()
        .expect("empty measurement regression thread failed");
    }

    #[test]
    fn shared_measurement_preserves_shaping_and_caller_size_policy() {
        let mut reference = TextRenderer::new();
        for text in ["Opacity 0.50", "不透明度", "e\u{301} 👩‍💻"] {
            for font_size in [0.5_f32, 8.0, 14.0, 24.0] {
                assert_eq!(
                    measure_single_line_at_size(text, font_size),
                    reference.measure_text(text, font_size)
                );
                assert_eq!(
                    measure_single_line(text, font_size),
                    reference.measure_text(text, font_size.max(1.0))
                );
            }
        }
    }

    #[test]
    fn numeric_input_and_label_share_one_cold_font_owner() {
        // A fresh thread is the production TLS lifetime, independent of test
        // ordering. Inspector construction and ordinary layout enter through
        // their real public widget methods in either order.
        for label_first in [false, true] {
            std::thread::spawn(move || {
                let measure_label = || {
                    let size = Label::new("Opacity 不透明度").measure(LayoutConstraint::LOOSE);
                    assert!(size.width > 0.0 && size.height > 0.0);
                };
                if label_first {
                    measure_label();
                }
                let input = NumberInput::new(0.5, 0.0, 1.0).with_decimals(2);
                assert_eq!(input.text(), "0.50");
                measure_label();
                assert_eq!(MEASURER_INITIALIZATIONS.with(|count| count.get()), 1);
            })
            .join()
            .expect("widget font-owner regression thread failed");
        }
    }
}
