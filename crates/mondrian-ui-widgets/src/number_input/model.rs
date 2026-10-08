use mondrian_ui_core::types::Modifiers;

mod sealed {
    pub trait Sealed {}
    impl Sealed for f32 {}
    impl Sealed for f64 {}
    impl Sealed for i64 {}
}

/// Numeric storage supported by text entry, keyboard stepping and slider projection.
///
/// Integers retain i64 storage throughout editing; floating values retain their
/// declared precision. Slider coordinates are presentation values only.
pub trait NumberInputValue: sealed::Sealed + Copy + PartialEq + PartialOrd + 'static {
    /// Exact typed representation used to admit local draft inspection.
    fn inspection_value(self) -> mondrian_ui_core::NumericInputInspectionValue;
    /// Default valid bounds used if supplied floating bounds are non-finite.
    fn fallback_range() -> (Self, Self);
    /// Whether this value can be displayed and edited.
    fn is_finite(self) -> bool;
    /// Parse a complete finite value without narrowing its declared type.
    fn parse(text: &str) -> Option<Self>;
    /// Format a value so parsing the text preserves it exactly.
    fn format(self, minimum_decimals: usize) -> String;
    /// Default positive keyboard increment.
    fn default_step(decimals: usize) -> Self;
    /// Whether an increment is finite and positive.
    fn is_positive(self) -> bool;
    /// Scale an increment, saturating instead of overflowing.
    fn scale(self, multiplier: u32) -> Self;
    /// Add or subtract an increment, saturating instead of overflowing.
    fn offset(self, step: Self, increase: bool) -> Self;
    /// Project an exact value into normalized slider coordinates.
    fn ratio(self, min: Self, max: Self) -> f32;
    /// Map a slider position into the numeric range, optionally snapping pointer input.
    fn from_ratio(ratio: f32, min: Self, max: Self, step: Option<Self>) -> Self;
}

macro_rules! floating_input_value {
    ($ty:ty, $inspection_variant:ident) => {
        impl NumberInputValue for $ty {
            fn inspection_value(self) -> mondrian_ui_core::NumericInputInspectionValue {
                mondrian_ui_core::NumericInputInspectionValue::$inspection_variant(self.to_bits())
            }
            fn fallback_range() -> (Self, Self) {
                (0.0, 1.0)
            }
            fn is_finite(self) -> bool {
                self.is_finite()
            }
            fn parse(text: &str) -> Option<Self> {
                text.trim().parse::<Self>().ok().filter(|value| value.is_finite())
            }
            fn format(self, minimum_decimals: usize) -> String {
                let fixed = format!("{self:.minimum_decimals$}");
                if fixed.parse::<Self>().is_ok_and(|value| value.to_bits() == self.to_bits()) {
                    fixed
                } else {
                    self.to_string()
                }
            }
            fn default_step(decimals: usize) -> Self {
                10.0_f64.powi(-(decimals as i32)) as Self
            }
            fn is_positive(self) -> bool {
                self.is_finite() && self > 0.0
            }
            fn scale(self, multiplier: u32) -> Self {
                (self * multiplier as Self).min(Self::MAX)
            }
            fn offset(self, step: Self, increase: bool) -> Self {
                if increase {
                    (self + step).min(Self::MAX)
                } else {
                    (self - step).max(-Self::MAX)
                }
            }
            fn ratio(self, min: Self, max: Self) -> f32 {
                if min == max {
                    return 0.5;
                }
                let scale = min.abs().max(max.abs()).max(1.0);
                ((self / scale - min / scale) / (max / scale - min / scale)).clamp(0.0, 1.0) as f32
            }
            fn from_ratio(ratio: f32, min: Self, max: Self, step: Option<Self>) -> Self {
                if !ratio.is_finite() {
                    return min;
                }
                if ratio <= 0.0 {
                    return min;
                }
                if ratio >= 1.0 {
                    return max;
                }
                let ratio = ratio as Self;
                let value = min * (1.0 - ratio) + max * ratio;
                if let Some(step) = step.filter(|step| step.is_positive()) {
                    let snapped = min + ((value - min) / step).round() * step;
                    if snapped.is_finite() {
                        return snapped.clamp(min, max);
                    }
                }
                value.clamp(min, max)
            }
        }
    };
}
floating_input_value!(f32, Float32);
floating_input_value!(f64, Float64);

impl NumberInputValue for i64 {
    fn inspection_value(self) -> mondrian_ui_core::NumericInputInspectionValue {
        mondrian_ui_core::NumericInputInspectionValue::Integer(self)
    }
    fn fallback_range() -> (Self, Self) {
        (Self::MIN, Self::MAX)
    }
    fn is_finite(self) -> bool {
        true
    }
    fn parse(text: &str) -> Option<Self> {
        text.trim().parse().ok()
    }
    fn format(self, _: usize) -> String {
        self.to_string()
    }
    fn default_step(_: usize) -> Self {
        1
    }
    fn is_positive(self) -> bool {
        self > 0
    }
    fn scale(self, multiplier: u32) -> Self {
        self.saturating_mul(i64::from(multiplier))
    }
    fn offset(self, step: Self, increase: bool) -> Self {
        if increase {
            self.saturating_add(step)
        } else {
            self.saturating_sub(step)
        }
    }
    fn ratio(self, min: Self, max: Self) -> f32 {
        if min == max {
            return 0.5;
        }
        ((i128::from(self) - i128::from(min)) as f64 / (i128::from(max) - i128::from(min)) as f64)
            .clamp(0.0, 1.0) as f32
    }
    fn from_ratio(ratio: f32, min: Self, max: Self, step: Option<Self>) -> Self {
        if !ratio.is_finite() {
            return min;
        }
        if ratio <= 0.0 {
            return min;
        }
        if ratio >= 1.0 {
            return max;
        }
        // f32 slider coordinates need at most 24 bits. The i128 numerator keeps
        // large integer endpoints exact, including the full signed i64 range.
        const DENOMINATOR: i128 = 1 << 24;
        let position = ((ratio as f64 * DENOMINATOR as f64).round() as i128).clamp(0, DENOMINATOR);
        let numerator =
            i128::from(min) * DENOMINATOR + (i128::from(max) - i128::from(min)) * position;
        let mut value = numerator / DENOMINATOR;
        if (numerator % DENOMINATOR).abs() * 2 >= DENOMINATOR {
            value += numerator.signum();
        }
        if let Some(step) = step.filter(|step| step.is_positive()) {
            let step = i128::from(step);
            let offset = value - i128::from(min);
            value = i128::from(min) + (offset + step / 2) / step * step;
        }
        value.clamp(i128::from(min), i128::from(max)) as i64
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct NumberInputModel<T: NumberInputValue = f64> {
    min: T,
    max: T,
    step: Option<T>,
    decimals: usize,
    committed_value: T,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct NumberDisplayUpdate<T: NumberInputValue = f64> {
    pub(super) value: T,
    pub(super) text: String,
}

impl<T: NumberInputValue> NumberInputModel<T> {
    pub(super) fn new(value: T, min: T, max: T) -> Self {
        let (min, max) = if !min.is_finite() || !max.is_finite() {
            T::fallback_range()
        } else if min <= max {
            (min, max)
        } else {
            (max, min)
        };
        let committed_value = if value.is_finite() {
            clamp(value, min, max)
        } else {
            min
        };
        Self { min, max, step: None, decimals: 0, committed_value }
    }
    pub(super) fn set_decimals(&mut self, decimals: usize) -> String {
        self.decimals = decimals.min(17);
        self.display_text()
    }
    pub(super) fn set_step(&mut self, step: T) {
        self.step = step.is_positive().then_some(step);
    }
    pub(super) fn inspection_contract(&self) -> mondrian_ui_core::NumericInputInspectionContract {
        mondrian_ui_core::NumericInputInspectionContract {
            baseline: self.committed_value.inspection_value(),
            min: self.min.inspection_value(),
            max: self.max.inspection_value(),
            step: self.step.map(NumberInputValue::inspection_value),
            minimum_decimals: self.decimals,
        }
    }
    pub(super) fn display_text(&self) -> String {
        self.committed_value.format(self.decimals)
    }
    pub(super) fn committed_value(&self) -> T {
        self.committed_value
    }
    pub(super) fn set_from_model(&mut self, value: T) -> Option<NumberDisplayUpdate<T>> {
        if !value.is_finite() {
            return None;
        }
        let next = clamp(value, self.min, self.max);
        if next.inspection_value() == self.committed_value.inspection_value() {
            return None;
        }
        self.committed_value = next;
        Some(NumberDisplayUpdate { value: next, text: self.display_text() })
    }
    pub(super) fn normalized_text_value(&self, text: &str) -> Option<T> {
        T::parse(text).map(|value| clamp(value, self.min, self.max))
    }
    pub(super) fn keyboard_step(&self, modifiers: Modifiers) -> T {
        let base = self.step.unwrap_or_else(|| T::default_step(self.decimals));
        if modifiers.shift {
            base.scale(10)
        } else {
            base
        }
    }
    pub(super) fn page_step(&self) -> T {
        self.keyboard_step(Modifiers::none()).scale(10)
    }
    pub(super) fn set_from_keyboard(
        &mut self,
        value: T,
        current_text: &str,
    ) -> Option<NumberDisplayUpdate<T>> {
        let next = if value.is_finite() {
            clamp(value, self.min, self.max)
        } else {
            self.committed_value
        };
        let text = next.format(self.decimals);
        if next == self.committed_value && current_text == text {
            return None;
        }
        self.committed_value = next;
        Some(NumberDisplayUpdate { value: next, text })
    }
    pub(super) fn commit_display_text(&mut self, text: &str) -> Option<NumberDisplayUpdate<T>> {
        let value = self.normalized_text_value(text).unwrap_or(self.committed_value);
        self.committed_value = value;
        let display_text = value.format(self.decimals);
        (text != display_text).then_some(NumberDisplayUpdate { value, text: display_text })
    }
    pub(super) fn commit_valid_text(&mut self, text: &str) -> Option<T> {
        let value = self.normalized_text_value(text)?;
        if value == self.committed_value {
            return None;
        }
        self.committed_value = value;
        Some(value)
    }
}
fn clamp<T: NumberInputValue>(value: T, min: T, max: T) -> T {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_storage_roundtrips_boundaries_and_rejects_fractional_or_overflow_text() {
        for value in [
            i64::MIN,
            -9_007_199_254_740_993,
            9_007_199_254_740_993,
            i64::MAX,
        ] {
            let mut model = NumberInputModel::new(value, i64::MIN, i64::MAX);
            model.set_decimals(17);
            assert_eq!(model.display_text(), value.to_string());
            assert_eq!(
                model.normalized_text_value(&model.display_text()),
                Some(value)
            );
            assert_eq!(model.normalized_text_value("9223372036854775808"), None);
            assert_eq!(model.normalized_text_value("-9223372036854775809"), None);
            assert_eq!(model.normalized_text_value("1.5"), None);
        }
        assert_eq!(i64::MAX.offset(10, true), i64::MAX);
        assert_eq!(i64::MIN.offset(10, false), i64::MIN);
    }

    #[test]
    fn integer_slider_mapping_preserves_endpoints_and_small_steps_near_large_values() {
        assert_eq!(i64::from_ratio(0.0, i64::MIN, i64::MAX, Some(1)), i64::MIN);
        assert_eq!(i64::from_ratio(1.0, i64::MIN, i64::MAX, Some(1)), i64::MAX);
        assert_eq!(
            i64::from_ratio(0.5, i64::MAX - 10, i64::MAX, Some(1)),
            i64::MAX - 5
        );
        assert_eq!(i64::from_ratio(0.5, -1, 0, None), -1);
    }

    #[test]
    fn display_and_text_edits_preserve_double_precision() {
        let value = 0.12345678901234568;
        let mut model = NumberInputModel::new(value, -1.0, 1.0);
        model.set_decimals(2);
        assert_eq!(
            model.normalized_text_value(&model.display_text()),
            Some(value)
        );
        model.set_step(0.1);
        assert_eq!(
            model.commit_valid_text("0.23456789012345678"),
            Some(0.23456789012345678)
        );
    }

    #[test]
    fn model_orders_range_and_recovers_nonfinite_values() {
        let mut model = NumberInputModel::new(f64::NAN, f64::NAN, f64::INFINITY);
        assert_eq!(model.set_decimals(2), "0.00");
        assert_eq!(model.normalized_text_value("2"), Some(1.0));

        let inverted = NumberInputModel::new(50.0, 100.0, 0.0);
        assert_eq!(inverted.display_text(), "50");
        assert_eq!(inverted.normalized_text_value("120"), Some(100.0));
    }

    #[test]
    fn model_step_controls_nudging_without_quantizing_text() {
        let mut model = NumberInputModel::new(0.0, 0.0, 10.0);
        model.set_step(2.5);

        assert_eq!(model.normalized_text_value("3.6"), Some(3.6));
        assert_eq!(
            model.set_from_keyboard(4.1, "0"),
            Some(NumberDisplayUpdate { value: 4.1, text: "4.1".into() })
        );
    }

    #[test]
    fn model_ignores_invalid_steps_and_uses_decimal_keyboard_precision() {
        let mut model = NumberInputModel::new(0.5, 0.0, 1.0);
        model.set_step(-1.0);
        model.set_decimals(2);

        assert_eq!(model.keyboard_step(Modifiers::none()), 0.01);
        assert_eq!(model.keyboard_step(Modifiers::shift()), 0.1);
        assert_eq!(model.page_step(), 0.1);
    }

    #[test]
    fn model_commit_display_text_reverts_invalid_text_to_committed_value() {
        let mut model = NumberInputModel::new(12.0, 0.0, 100.0);

        assert_eq!(
            model.commit_display_text("abc"),
            Some(NumberDisplayUpdate { value: 12.0, text: "12".into() })
        );
        assert_eq!(model.commit_display_text("12"), None);
    }

    #[test]
    fn model_commit_valid_text_updates_committed_value_without_forcing_display() {
        let mut model = NumberInputModel::new(12.0, 0.0, 100.0);
        model.set_step(5.0);

        assert_eq!(model.commit_valid_text("42"), Some(42.0));
        assert_eq!(
            model.commit_display_text("abc").map(|update| update.text),
            Some("42".into())
        );
    }
}
