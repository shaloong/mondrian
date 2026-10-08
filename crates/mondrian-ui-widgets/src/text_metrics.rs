//! Shared text measurement for widgets.

use mondrian_ui_text::{
    single_line_request_admitted, SingleLineTextCaret, SingleLineTextNavigation, TextRenderer,
};
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

/// Complete associated caret/pointer geometry from the shared widget font owner.
pub(crate) fn single_line_geometry(
    text: &str,
    font_size: f32,
    caret: SingleLineTextCaret,
    pointer_x: Option<f32>,
) -> Option<mondrian_ui_text::SingleLineTextGeometry> {
    if !single_line_request_admitted(text, font_size, caret, None) {
        return None;
    }
    if text.is_empty() {
        return Some(mondrian_ui_text::SingleLineTextGeometry {
            width: 0.0,
            height: 0.0,
            caret_x: 0.0,
            hit_caret: pointer_x.map(|_| SingleLineTextCaret::before_next(0)),
        });
    }
    TEXT_MEASURER.with_borrow_mut(|renderer| {
        renderer.single_line_geometry(text, font_size, caret, pointer_x)
    })
}

/// Complete selected spans and associated caret from one shared advanced layout.
pub(crate) fn single_line_selection_geometry(
    text: &str,
    font_size: f32,
    caret: SingleLineTextCaret,
    selection_byte_range: Option<std::ops::Range<usize>>,
) -> Option<mondrian_ui_text::SingleLineTextSelectionGeometry> {
    if !single_line_request_admitted(text, font_size, caret, selection_byte_range.as_ref()) {
        return None;
    }
    if text.is_empty() {
        return Some(mondrian_ui_text::SingleLineTextSelectionGeometry {
            line: mondrian_ui_text::SingleLineTextGeometry {
                width: 0.0,
                height: 0.0,
                caret_x: 0.0,
                hit_caret: None,
            },
            spans: Vec::new(),
        });
    }
    TEXT_MEASURER.with_borrow_mut(|renderer| {
        renderer.single_line_selection_geometry(text, font_size, caret, None, selection_byte_range)
    })
}

pub(crate) fn navigate_single_line(
    text: &str,
    font_size: f32,
    caret: SingleLineTextCaret,
    navigation: SingleLineTextNavigation,
    collapse_selection: Option<std::ops::Range<usize>>,
) -> Option<SingleLineTextCaret> {
    if !single_line_request_admitted(text, font_size, caret, collapse_selection.as_ref()) {
        return None;
    }
    if text.is_empty() {
        return Some(SingleLineTextCaret::before_next(0));
    }
    TEXT_MEASURER.with_borrow_mut(|renderer| {
        renderer.navigate_single_line(text, font_size, caret, navigation, collapse_selection)
    })
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

    #[test]
    fn invalid_single_line_geometry_does_not_initialize_font_owner() {
        std::thread::spawn(|| {
            assert!(single_line_geometry(
                "你",
                14.0,
                SingleLineTextCaret::at_logical_boundary(1),
                Some(2.0)
            )
            .is_none());
            assert!(single_line_geometry(
                "x",
                0.0,
                SingleLineTextCaret::at_logical_boundary(0),
                None
            )
            .is_none());
            assert!(single_line_geometry(
                "",
                14.0,
                SingleLineTextCaret::at_logical_boundary(1),
                None
            )
            .is_none());
            assert_eq!(MEASURER_INITIALIZATIONS.with(|count| count.get()), 0);
            let geometry = single_line_geometry(
                "",
                14.0,
                SingleLineTextCaret::at_logical_boundary(0),
                Some(0.0),
            )
            .expect("valid empty geometry");
            assert_eq!(geometry.width, 0.0);
            assert_eq!(geometry.hit_caret.map(|caret| caret.byte_index), Some(0));
            assert_eq!(MEASURER_INITIALIZATIONS.with(|count| count.get()), 0);
        })
        .join()
        .expect("geometry admission regression thread failed");
    }

    #[test]
    fn complete_selection_geometry_invalid_or_empty_does_not_initialize_font_owner() {
        std::thread::spawn(|| {
            assert!(single_line_selection_geometry(
                "你",
                14.0,
                SingleLineTextCaret::at_logical_boundary(0),
                Some(1..3)
            )
            .is_none());
            assert!(single_line_selection_geometry(
                "x",
                14.0,
                SingleLineTextCaret::at_logical_boundary(0),
                Some(0..2)
            )
            .is_none());
            assert!(single_line_selection_geometry(
                "",
                14.0,
                SingleLineTextCaret::at_logical_boundary(0),
                Some(0..1)
            )
            .is_none());
            assert!(single_line_selection_geometry(
                "",
                0.0,
                SingleLineTextCaret::at_logical_boundary(0),
                None
            )
            .is_none());
            assert_eq!(MEASURER_INITIALIZATIONS.with(|count| count.get()), 0);
            let empty = single_line_selection_geometry(
                "",
                14.0,
                SingleLineTextCaret::at_logical_boundary(0),
                Some(0..0),
            )
            .expect("valid empty selection");
            assert!(empty.spans.is_empty());
            assert_eq!(empty.line.width, 0.0);
            assert_eq!(MEASURER_INITIALIZATIONS.with(|count| count.get()), 0);
        })
        .join()
        .expect("selection admission regression thread failed");
    }

    #[test]
    fn associated_single_line_navigation_invalid_metrics_and_empty_line_do_not_initialize_font_owner(
    ) {
        std::thread::spawn(|| {
            let start = SingleLineTextCaret::before_next(0);
            let right = SingleLineTextNavigation::Right { word: false };
            assert!(navigate_single_line("x", f32::MAX, start, right, None).is_none());
            assert!(single_line_geometry("x", f32::MAX, start, None).is_none());
            assert!(single_line_selection_geometry("x", f32::MAX, start, Some(0..1)).is_none());
            assert!(navigate_single_line(
                "e\u{301}",
                14.0,
                SingleLineTextCaret::after_previous(1),
                right,
                None
            )
            .is_none());
            assert!(navigate_single_line(
                "x",
                14.0,
                SingleLineTextCaret::after_previous(0),
                right,
                None
            )
            .is_none());
            assert_eq!(MEASURER_INITIALIZATIONS.with(|count| count.get()), 0);
            assert_eq!(
                navigate_single_line("", 14.0, start, right, None),
                Some(start)
            );
            assert_eq!(MEASURER_INITIALIZATIONS.with(|count| count.get()), 0);
            let actual =
                navigate_single_line("a", 14.0, start, right, None).expect("normal navigation");
            assert_eq!(actual, SingleLineTextCaret::after_previous(1));
            assert_eq!(MEASURER_INITIALIZATIONS.with(|count| count.get()), 1);
        })
        .join()
        .expect("navigation admission regression thread failed");
    }
}
