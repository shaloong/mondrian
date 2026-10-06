//! Shared text measurement for widgets.

use mondrian_ui_text::TextRenderer;
use std::cell::RefCell;
use std::collections::HashMap;

const MAX_MEASUREMENTS: usize = 512;
const MAX_MEASUREMENT_TEXT_BYTES: usize = 64 * 1024;

type MeasurementKey = (u32, Option<u32>);

struct TextMeasurer {
    renderer: TextRenderer,
    measurements: HashMap<MeasurementKey, HashMap<Box<str>, (f32, f32)>>,
    entries: usize,
    text_bytes: usize,
}

impl TextMeasurer {
    fn measure(&mut self, text: &str, font_size: f32, max_width: Option<f32>) -> (f32, f32) {
        // This private owner retains one font catalog for its entire lifetime.
        // Only identical shaping inputs reuse a result; the renderer and its
        // mutable font manager remain inaccessible to widget callers.
        let cacheable = font_size.is_finite()
            && max_width.is_none_or(f32::is_finite)
            && text.len() <= MAX_MEASUREMENT_TEXT_BYTES;
        let key = (font_size.to_bits(), max_width.map(f32::to_bits));
        if cacheable
            && let Some(size) = self.measurements.get(&key).and_then(|sizes| sizes.get(text))
        {
            return *size;
        }

        let size = match max_width {
            Some(width) => self.renderer.measure_text_box(text, font_size, width),
            None => self.renderer.measure_text(text, font_size),
        };
        if cacheable && size.0.is_finite() && size.1.is_finite() {
            if self.entries == MAX_MEASUREMENTS
                || self.text_bytes + text.len() > MAX_MEASUREMENT_TEXT_BYTES
            {
                self.measurements.clear();
                self.entries = 0;
                self.text_bytes = 0;
            }
            self.measurements.entry(key).or_default().insert(text.into(), size);
            self.entries += 1;
            self.text_bytes += text.len();
        }
        size
    }
}

thread_local! {
    static TEXT_MEASURER: RefCell<TextMeasurer> = RefCell::new(new_text_measurer());
}

#[cfg(test)]
thread_local! {
    static MEASURER_INITIALIZATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn new_text_measurer() -> TextMeasurer {
    #[cfg(test)]
    MEASURER_INITIALIZATIONS.with(|count| count.set(count.get() + 1));
    TextMeasurer {
        renderer: TextRenderer::new(),
        measurements: HashMap::new(),
        entries: 0,
        text_bytes: 0,
    }
}

pub(crate) fn measure_single_line(text: &str, font_size: f32) -> (f32, f32) {
    measure_single_line_at_size(text, font_size.max(1.0))
}

/// Measure with the caller's exact size, without imposing a widget's size policy.
pub(crate) fn measure_single_line_at_size(text: &str, font_size: f32) -> (f32, f32) {
    if text.is_empty() {
        return (0.0, 0.0);
    }
    TEXT_MEASURER.with_borrow_mut(|measurer| measurer.measure(text, font_size, None))
}

pub(crate) fn measure_text_box(text: &str, font_size: f32, max_width: f32) -> (f32, f32) {
    if text.is_empty() {
        return (0.0, 0.0);
    }
    TEXT_MEASURER.with_borrow_mut(|measurer| {
        measurer.measure(text, font_size.max(1.0), Some(max_width.max(1.0)))
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
    fn repeated_measurement_preserves_bidi_shaping_and_wrap_width() {
        let mut reference = TextRenderer::new();
        for _ in 0..3 {
            for text in ["Opacity 0.50", "不透明度", "e\u{301} 👩‍💻", "עברית العربية"]
            {
                for font_size in [0.5_f32, 8.0, 14.0, 24.0] {
                    assert_eq!(
                        measure_single_line_at_size(text, font_size),
                        reference.measure_text(text, font_size)
                    );
                    for width in [32.0, 80.0, 128.0] {
                        assert_eq!(
                            measure_text_box(text, font_size, width),
                            reference.measure_text_box(text, font_size.max(1.0), width)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn measurement_eviction_retains_geometry_and_font_owner() {
        std::thread::spawn(|| {
            let expected = measure_single_line("Opacity 不透明度", 14.0);
            for index in 0..MAX_MEASUREMENTS {
                measure_single_line(&format!("Track {index}"), 14.0);
            }
            assert_eq!(measure_single_line("Opacity 不透明度", 14.0), expected);
            TEXT_MEASURER.with_borrow(|measurer| {
                assert!(measurer.entries <= MAX_MEASUREMENTS);
                assert!(measurer.text_bytes <= MAX_MEASUREMENT_TEXT_BYTES);
            });
            assert_eq!(MEASURER_INITIALIZATIONS.with(|count| count.get()), 1);
        })
        .join()
        .expect("measurement eviction regression thread failed");
    }

    #[test]
    fn text_byte_budget_does_not_truncate_or_change_measurement() {
        let mut measurer = new_text_measurer();
        let mut reference = TextRenderer::new();
        for count in [
            MAX_MEASUREMENT_TEXT_BYTES / 2 + 1,
            MAX_MEASUREMENT_TEXT_BYTES + 1,
        ] {
            let text = "a".repeat(count);
            assert_eq!(
                measurer.measure(&text, 14.0, None),
                reference.measure_text(&text, 14.0)
            );
            assert_eq!(
                measurer.measure(&("b".to_owned() + &text), 14.0, None),
                reference.measure_text(&("b".to_owned() + &text), 14.0)
            );
            assert!(measurer.entries <= MAX_MEASUREMENTS);
            assert!(measurer.text_bytes <= MAX_MEASUREMENT_TEXT_BYTES);
            assert_eq!(measurer.entries, 1);
            assert_eq!(measurer.text_bytes, MAX_MEASUREMENT_TEXT_BYTES / 2 + 2);
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
