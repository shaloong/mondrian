//! AccessKit value projection; independent of the native window adapter.

use accesskit::Node;
use mondrian_ui_core::widget::{AccessibilityRole, AccessibilityValue};

/// Project one current Widget value onto the tree adapter's freshly built node.
/// Text input values retain the domain precision independently of display rounding.
pub(super) fn apply_value(node: &mut Node, role: AccessibilityRole, value: AccessibilityValue) {
    match value {
        AccessibilityValue::Text(value) => node.set_value(value),
        AccessibilityValue::Number { value, min, max } => {
            if role == AccessibilityRole::Slider {
                node.set_numeric_value(value);
                node.set_min_numeric_value(min);
                node.set_max_numeric_value(max);
            } else {
                node.set_value(value.to_string());
                node.set_description(format!("range {min} to {max}"));
            }
        }
        AccessibilityValue::Integer { value, min, max } => {
            // Integer NumberInput uses the existing textual edit action. Never
            // encode i64 author values in AccessKit's f64 numeric property.
            node.set_value(value.to_string());
            node.set_description(format!("range {min} to {max}"));
        }
        AccessibilityValue::Scroll { x, y, max_x, max_y } => {
            node.set_scroll_x(f64::from(x));
            node.set_scroll_y(f64::from(y));
            node.set_scroll_x_max(f64::from(max_x));
            node.set_scroll_y_max(f64::from(max_y));
        }
        AccessibilityValue::Color { r, g, b, a } => {
            node.set_description(format!("RGBA {r:.3}, {g:.3}, {b:.3}, {a:.3}"));
        }
        AccessibilityValue::Timeline { playhead_frame, track_count, clip_count, .. } => {
            node.set_description(format!(
                "Frame {playhead_frame}; {track_count} tracks; {clip_count} clips"
            ));
        }
        AccessibilityValue::Collection { total_count, selected_count, .. } => {
            node.set_description(format!("{total_count} items; {selected_count} selected"));
        }
        AccessibilityValue::Viewer { source_width, source_height, has_frame, .. } => {
            node.set_description(format!(
                "{source_width} by {source_height}; frame available: {has_frame}"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use accesskit::Role;

    #[test]
    fn integer_text_value_and_range_never_roundtrip_through_float() {
        let beyond_float_integer = (1_i64 << f64::MANTISSA_DIGITS) + 1;
        for value in [
            i64::MIN,
            -beyond_float_integer,
            beyond_float_integer,
            i64::MAX,
        ] {
            let mut node = Node::new(Role::TextInput);
            apply_value(
                &mut node,
                AccessibilityRole::TextInput,
                AccessibilityValue::Integer { value, min: i64::MIN, max: i64::MAX },
            );
            assert_eq!(node.value().unwrap().parse::<i64>().unwrap(), value);
            assert_eq!(
                node.description(),
                Some(format!("range {} to {}", i64::MIN, i64::MAX).as_str())
            );
            assert_eq!(node.numeric_value(), None);
        }
    }

    #[test]
    fn float_text_values_preserve_roundtrip_precision_and_signed_zero() {
        for value in [0.12345678901234568_f64, -0.0, f64::MIN_POSITIVE, f64::MAX] {
            let mut node = Node::new(Role::TextInput);
            apply_value(
                &mut node,
                AccessibilityRole::TextInput,
                AccessibilityValue::Number { value, min: -f64::MAX, max: f64::MAX },
            );
            assert_eq!(
                node.value().unwrap().parse::<f64>().unwrap().to_bits(),
                value.to_bits()
            );
            assert_eq!(node.numeric_value(), None);
        }
    }

    #[test]
    fn numeric_slider_values_keep_the_declared_float64_range() {
        let value = 0.12345678901234568_f64;
        let mut node = Node::new(Role::Slider);
        apply_value(
            &mut node,
            AccessibilityRole::Slider,
            AccessibilityValue::Number { value, min: -f64::MAX, max: f64::MAX },
        );
        assert_eq!(node.numeric_value().unwrap().to_bits(), value.to_bits());
        assert_eq!(node.min_numeric_value(), Some(-f64::MAX));
        assert_eq!(node.max_numeric_value(), Some(f64::MAX));
    }
}
