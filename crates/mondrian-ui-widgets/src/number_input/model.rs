use mondrian_ui_core::types::Modifiers;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct NumberInputModel {
    min: f64,
    max: f64,
    step: Option<f64>,
    decimals: usize,
    committed_value: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct NumberDisplayUpdate {
    pub(super) value: f64,
    pub(super) text: String,
}

impl NumberInputModel {
    pub(super) fn new(value: f64, min: f64, max: f64) -> Self {
        let (min, max) = ordered_range(min, max);
        let committed_value = clamp_finite(value, min, max);
        Self { min, max, step: None, decimals: 0, committed_value }
    }

    pub(super) fn set_decimals(&mut self, decimals: usize) -> String {
        self.decimals = decimals.min(6);
        self.display_text()
    }

    pub(super) fn set_step(&mut self, step: f64) {
        self.step = (step.is_finite() && step > 0.0).then_some(step);
    }

    pub(super) fn display_text(&self) -> String {
        format_number(self.committed_value, self.decimals)
    }

    pub(super) fn committed_value(&self) -> f64 {
        self.committed_value
    }

    pub(super) fn normalized_text_value(&self, text: &str) -> Option<f64> {
        parse_number(text).map(|value| self.normalize(value))
    }

    pub(super) fn keyboard_step(&self, modifiers: Modifiers) -> f64 {
        let base = self.step.unwrap_or_else(|| {
            if self.decimals == 0 {
                1.0
            } else {
                10_f64.powi(-(self.decimals as i32))
            }
        });
        if modifiers.shift {
            base * 10.0
        } else {
            base
        }
    }

    pub(super) fn page_step(&self) -> f64 {
        self.step.unwrap_or_else(|| self.keyboard_step(Modifiers::none())) * 10.0
    }

    pub(super) fn set_from_keyboard(
        &mut self,
        value: f64,
        current_text: &str,
    ) -> Option<NumberDisplayUpdate> {
        let next = self.normalize(value);
        let text = format_number(next, self.decimals);
        if (next - self.committed_value).abs() <= f64::EPSILON && current_text == text {
            return None;
        }
        self.committed_value = next;
        Some(NumberDisplayUpdate { value: next, text })
    }

    pub(super) fn commit_display_text(&mut self, text: &str) -> Option<NumberDisplayUpdate> {
        let value = self.normalized_text_value(text).unwrap_or(self.committed_value);
        self.committed_value = value;
        let display_text = format_number(value, self.decimals);
        (text != display_text).then_some(NumberDisplayUpdate { value, text: display_text })
    }

    pub(super) fn commit_valid_text(&mut self, text: &str) -> Option<f64> {
        let value = self.normalized_text_value(text)?;
        self.committed_value = value;
        Some(value)
    }

    fn normalize(&self, value: f64) -> f64 {
        if !value.is_finite() {
            return self.committed_value;
        }
        let clamped = value.clamp(self.min, self.max);
        if let Some(step) = self.step {
            let steps = ((clamped - self.min) / step).round();
            (self.min + steps * step).clamp(self.min, self.max)
        } else {
            clamped
        }
    }
}

fn ordered_range(min: f64, max: f64) -> (f64, f64) {
    if !min.is_finite() || !max.is_finite() {
        return (0.0, 1.0);
    }
    if min <= max {
        (min, max)
    } else {
        (max, min)
    }
}

fn clamp_finite(value: f64, min: f64, max: f64) -> f64 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        min
    }
}

fn parse_number(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value = trimmed.parse::<f64>().ok()?;
    value.is_finite().then_some(value)
}

fn format_number(value: f64, decimals: usize) -> String {
    if decimals == 0 {
        format!("{value:.0}")
    } else {
        format!("{value:.decimals$}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn model_quantizes_text_and_keyboard_updates_to_step() {
        let mut model = NumberInputModel::new(0.0, 0.0, 10.0);
        model.set_step(2.5);

        assert_eq!(model.normalized_text_value("3.6"), Some(2.5));
        assert_eq!(
            model.set_from_keyboard(4.1, "0"),
            Some(NumberDisplayUpdate { value: 5.0, text: "5".into() })
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

        assert_eq!(model.commit_valid_text("42"), Some(40.0));
        assert_eq!(
            model.commit_display_text("abc").map(|update| update.text),
            Some("40".into())
        );
    }
}
