//! Read-only labels for the temporal editor's owner-domain projection.

use super::*;
use crate::text_metrics::measure_single_line;
use mondrian_core::{TimeScale, TimelineTime};

fn compact_number(value: f64, step: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    let magnitude = value.abs();
    let precision = if step.is_finite() && step > 0.0 {
        (-step.log10().floor()).max(0.0) as usize
    } else {
        0
    };
    if !(0.0001..1_000_000.0).contains(&magnitude) || precision > 8 {
        // Enough digits to distinguish narrow views around a large center.
        let digits = if step > 0.0 && step.is_finite() {
            (magnitude.log10() - step.log10()).ceil().clamp(0.0, 16.0) as usize
        } else {
            16
        };
        for precision in digits..=16 {
            let text = format!("{value:.precision$e}");
            if text.parse::<f64>().is_ok_and(f64::is_finite) {
                return text;
            }
        }
        format!("{value:.16e}")
    } else {
        let text = format!("{value:.precision$}");
        if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.').into()
        } else {
            text
        }
    }
}

impl CurveEditor {
    fn ruler_time(&self, fraction: f64) -> Option<TimelineTime> {
        let range = self.time_axis?;
        let normalized = TimelineTime::from_f64_quantized(
            self.viewport.time_start + fraction * self.viewport.time_span,
            1_000_000_000,
        )
        .ok()?;
        let scale = TimeScale::new(normalized.numerator(), normalized.denominator()).ok()?;
        range.start.checked_add(range.duration.checked_scale(scale).ok()?).ok()
    }

    pub(super) fn paint_rulers(&self, ctx: &mut PaintContext) {
        let Some(range) = self.time_axis else {
            return;
        };
        let plot = self.plot_rect();
        if plot.width <= 0.0 || plot.height <= 0.0 {
            return;
        }
        let style = &ctx.theme.typography.metadata;
        let font = style.font_size;
        let gap = ctx.theme.spacing.xs;
        let color = ctx.theme.colors.muted_foreground;
        ctx.encoder.push_clip(self.bounds);
        let duration = range.duration.numerator() as f64 / range.duration.denominator() as f64;
        let time_step =
            (duration * self.viewport.time_span / self.grid_columns.max(1) as f64).min(0.001);
        let visible_start = self.ruler_time(0.0).map(TimelineTime::to_f64);
        let exact_time_labels = visible_start.is_some_and(|start| {
            duration > 0.0 && start + duration * self.viewport.time_span == start
        });
        let mut previous_right = f32::NEG_INFINITY;
        let mut previous_time = None;
        for column in 0..=self.grid_columns {
            let fraction = column as f64 / self.grid_columns.max(1) as f64;
            let Some(time) = self.ruler_time(fraction) else {
                continue;
            };
            if previous_time == Some(time) {
                continue;
            }
            let seconds = time.numerator() as f64 / time.denominator() as f64;
            let text = if exact_time_labels {
                time.to_string()
            } else {
                format!("{}s", compact_number(seconds, time_step))
            };
            let (width, _) = measure_single_line(&text, font);
            let x = (plot.x + plot.width * fraction as f32 - width * 0.5)
                .max(self.bounds.x + gap)
                .min((self.bounds.x + self.bounds.width - gap - width).max(self.bounds.x + gap));
            if x < previous_right + gap || width + gap * 2.0 > self.bounds.width {
                continue;
            }
            let y = plot.y + plot.height + gap;
            if y + style.line_height > self.bounds.y + self.bounds.height {
                continue;
            }
            ctx.encoder.draw_text(&text, font, Point::new(x, y), color);
            previous_right = x + width;
            previous_time = Some(time);
        }
        let rows = self.grid_rows.max(1);
        // Divide the radius first: a full finite f64 range has an overflowing span.
        let value_step = (self.viewport.value_radius / rows as f64) * 2.0;
        let mut previous_bottom = f32::NEG_INFINITY;
        let count = if self.fixed_value.is_some() { 0 } else { rows };
        for row in 0..=count {
            let fraction = if count == 0 {
                0.5
            } else {
                row as f64 / rows as f64
            };
            let value = self.fixed_value.unwrap_or_else(|| self.viewport.value_at(1.0 - fraction));
            let text = if self.fixed_value.is_some() {
                value.to_string()
            } else {
                compact_number(value, value_step)
            };
            let (width, _) = measure_single_line(&text, font);
            let x = plot.x - gap - width;
            let y = (plot.y + plot.height * fraction as f32 - style.line_height * 0.5)
                .max(self.bounds.y + gap);
            if x < self.bounds.x + gap
                || y < previous_bottom + gap
                || y + style.line_height > plot.y + plot.height + gap
            {
                continue;
            }
            ctx.encoder.draw_text(&text, font, Point::new(x, y), color);
            previous_bottom = y + style.line_height;
        }
        ctx.encoder.pop_clip();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_labels_keep_narrow_values_distinct_and_full_range_finite() {
        assert_eq!(compact_number(123.25, 0.01), "123.25");
        assert_eq!(compact_number(-0.0, 0.1), "0");
        for (a, b) in [(1e20, 1e20 + 1e6), (1e-100, 2e-100), (-f64::MAX, f64::MAX)] {
            let step = (b * 0.5 - a * 0.5).abs();
            assert_ne!(compact_number(a, step), compact_number(b, step));
            assert!(compact_number(a, step).parse::<f64>().expect("number").is_finite());
        }
    }

    #[test]
    fn time_ruler_uses_owner_origin_and_zoom_without_authoring() {
        let range = mondrian_core::TimelineTimeRange::new(
            TimelineTime::new(1001, 30).expect("origin"),
            TimelineTime::new(10, 1).expect("duration"),
        )
        .expect("range");
        let mut editor = CurveEditor::new().with_time_axis(range);
        let origin = editor.points.clone();
        assert_eq!(editor.ruler_time(0.0), Some(range.start));
        assert_eq!(editor.ruler_time(1.0), range.end().ok());
        editor.viewport.time_start = 0.25;
        editor.viewport.time_span = 0.5;
        assert_eq!(
            editor.ruler_time(0.0),
            range.start.checked_add(TimelineTime::new(5, 2).expect("offset")).ok()
        );
        assert_eq!(
            editor.ruler_time(1.0),
            range.start.checked_add(TimelineTime::new(15, 2).expect("offset")).ok()
        );
        assert_eq!(editor.points, origin);
    }
}
