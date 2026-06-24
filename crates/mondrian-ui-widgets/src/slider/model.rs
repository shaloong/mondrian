use mondrian_ui_core::types::{Modifiers, Point, Rect, Size};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct SliderModel {
    value: f32,
    min: f32,
    max: f32,
    step: Option<f32>,
}

impl SliderModel {
    pub(super) fn new(value: f32, min: f32, max: f32) -> Self {
        let (min, max) = ordered_range(min, max);
        Self {
            value: clamp_finite(value, min, max),
            min,
            max,
            step: None,
        }
    }

    pub(super) fn value(&self) -> f32 {
        self.value
    }

    pub(super) fn min(&self) -> f32 {
        self.min
    }

    pub(super) fn max(&self) -> f32 {
        self.max
    }

    pub(super) fn range(&self) -> f32 {
        self.max - self.min
    }

    pub(super) fn ratio(&self) -> f32 {
        let range = self.range();
        if range > 0.0 {
            ((self.value - self.min) / range).clamp(0.0, 1.0)
        } else {
            0.5
        }
    }

    pub(super) fn set_step(&mut self, step: f32) {
        self.step = (step.is_finite() && step > 0.0).then_some(step);
    }

    pub(super) fn keyboard_step(&self, modifiers: Modifiers) -> f32 {
        let base = self.step.unwrap_or_else(|| self.range().abs() / 100.0);
        if modifiers.shift {
            base * 10.0
        } else {
            base
        }
    }

    pub(super) fn page_step(&self) -> f32 {
        if let Some(step) = self.step {
            step * 10.0
        } else {
            self.range().abs() / 10.0
        }
    }

    pub(super) fn value_at_ratio(&self, ratio: f32) -> f32 {
        self.min + self.range() * ratio.clamp(0.0, 1.0)
    }

    pub(super) fn set_value(&mut self, value: f32) -> bool {
        let next = self.quantize_value(value);
        if (next - self.value).abs() <= f32::EPSILON {
            return false;
        }
        self.value = next;
        true
    }

    fn quantize_value(&self, value: f32) -> f32 {
        if !value.is_finite() {
            return self.value;
        }
        let clamped = value.clamp(self.min, self.max);
        let Some(step) = self.step else {
            return clamped;
        };
        if clamped <= self.min {
            return self.min;
        }
        if clamped >= self.max {
            return self.max;
        }
        let steps = ((clamped - self.min) / step).round();
        (self.min + steps * step).clamp(self.min, self.max)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct SliderGeometry {
    track_height: f32,
    thumb_size: f32,
    min_width: f32,
    measure_padding_y: f32,
    thumb_fit_inset: f32,
}

impl SliderGeometry {
    pub(super) fn new(
        track_height: f32,
        thumb_size: f32,
        min_width: f32,
        measure_padding_y: f32,
        thumb_fit_inset: f32,
    ) -> Self {
        Self {
            track_height: track_height.max(1.0),
            thumb_size: thumb_size.max(1.0),
            min_width: min_width.max(1.0),
            measure_padding_y: measure_padding_y.max(0.0),
            thumb_fit_inset: thumb_fit_inset.max(0.0),
        }
    }

    pub(super) fn measure_size(&self) -> Size {
        Size::new(self.min_width, self.thumb_size + self.measure_padding_y)
    }

    pub(super) fn track_rect(&self, bounds: Rect) -> Rect {
        let thumb_size = self.effective_thumb_size(bounds);
        let track_height = self.track_height.min(bounds.height.max(1.0));
        let track_y = bounds.y + bounds.height * 0.5 - track_height * 0.5;
        Rect::new(
            bounds.x + thumb_size * 0.5,
            track_y,
            (bounds.width - thumb_size).max(0.0),
            track_height,
        )
    }

    pub(super) fn thumb_rect(&self, bounds: Rect, ratio: f32) -> Rect {
        let thumb_size = self.effective_thumb_size(bounds);
        let track = self.track_rect(bounds);
        let center_x = track.x + track.width * ratio.clamp(0.0, 1.0);
        let x = center_x - thumb_size * 0.5;
        let y = bounds.y + (bounds.height - thumb_size) * 0.5;
        Rect::new(x, y, thumb_size, thumb_size)
    }

    pub(super) fn ratio_at_position(&self, bounds: Rect, position: Point) -> f32 {
        let track = self.track_rect(bounds);
        if track.width > 0.0 {
            ((position.x - track.x) / track.width).clamp(0.0, 1.0)
        } else if position.x >= bounds.x + bounds.width * 0.5 {
            1.0
        } else {
            0.0
        }
    }

    fn effective_thumb_size(&self, bounds: Rect) -> f32 {
        self.thumb_size.min((bounds.height - self.thumb_fit_inset).max(1.0))
    }
}

fn ordered_range(min: f32, max: f32) -> (f32, f32) {
    if !min.is_finite() || !max.is_finite() {
        return (0.0, 1.0);
    }
    if min <= max {
        (min, max)
    } else {
        (max, min)
    }
}

fn clamp_finite(value: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        min
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= 0.0001,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn model_orders_range_and_recovers_non_finite_input() {
        let model = SliderModel::new(f32::NAN, f32::NAN, f32::INFINITY);
        assert_eq!(model.value(), 0.0);
        assert_eq!(model.min(), 0.0);
        assert_eq!(model.max(), 1.0);

        let inverted = SliderModel::new(50.0, 100.0, 0.0);
        assert_eq!(inverted.value(), 50.0);
        assert_eq!(inverted.min(), 0.0);
        assert_eq!(inverted.max(), 100.0);
    }

    #[test]
    fn model_quantizes_only_user_edits_to_valid_step() {
        let mut model = SliderModel::new(0.23, 0.0, 1.0);
        model.set_step(0.25);

        assert_close(model.value(), 0.23);
        assert!(model.set_value(0.62));
        assert_close(model.value(), 0.5);
        assert!(model.set_value(0.99));
        assert_close(model.value(), 1.0);
    }

    #[test]
    fn model_ignores_invalid_step_and_nonfinite_updates() {
        let mut model = SliderModel::new(5.0, 0.0, 10.0);
        model.set_step(-1.0);

        assert_eq!(model.keyboard_step(Modifiers::none()), 0.1);
        assert!(!model.set_value(f32::NAN));
        assert_eq!(model.value(), 5.0);
    }

    #[test]
    fn geometry_maps_thumb_center_to_track_ratio() {
        let geometry = SliderGeometry::new(4.0, 12.0, 100.0, 4.0, 2.0);
        let bounds = Rect::new(10.0, 20.0, 200.0, 20.0);
        let track = geometry.track_rect(bounds);

        assert_close(
            geometry.ratio_at_position(bounds, Point::new(track.x, track.center().y)),
            0.0,
        );
        assert_close(
            geometry.ratio_at_position(bounds, Point::new(track.x + track.width, track.center().y)),
            1.0,
        );
    }

    #[test]
    fn geometry_shrinks_thumb_to_fit_short_bounds() {
        let geometry = SliderGeometry::new(4.0, 12.0, 100.0, 4.0, 2.0);
        let bounds = Rect::new(0.0, 0.0, 100.0, 10.0);
        let thumb = geometry.thumb_rect(bounds, 0.5);

        assert!(thumb.y >= bounds.y);
        assert!(thumb.y + thumb.height <= bounds.y + bounds.height);
    }
}
