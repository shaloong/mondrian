//! Shared execution math for numeric automation in owner-time/value coordinates.

use crate::automation::interpolate_numeric_value;

/// Evaluate an admitted numeric cubic with derived normalized time controls.
///
/// Author coordinates remain exact owner times and parameter-value offsets.
/// Callers derive the two time fractions at this execution seam and admit
/// finite value controls before invoking the shared solver.
pub(crate) fn evaluate_numeric_bezier(
    progress: f64,
    time_controls: [f64; 2],
    values: [f64; 4],
    uniform_time: bool,
) -> f64 {
    if uniform_time {
        return cubic_numeric_value(values, progress);
    }
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..56 {
        let candidate = (low + high) * 0.5;
        let sample = cubic_numeric_value([0.0, time_controls[0], time_controls[1], 1.0], candidate);
        if sample == progress || candidate == low || candidate == high {
            return cubic_numeric_value(values, candidate);
        }
        if sample < progress {
            low = candidate;
        } else {
            high = candidate;
        }
    }
    cubic_numeric_value(values, (low + high) * 0.5)
}

fn cubic_numeric_value([p0, p1, p2, p3]: [f64; 4], t: f64) -> f64 {
    let left = interpolate_numeric_value(p0, p1, t);
    let middle = interpolate_numeric_value(p1, p2, t);
    let right = interpolate_numeric_value(p2, p3, t);
    let left = interpolate_numeric_value(left, middle, t);
    let right = interpolate_numeric_value(middle, right, t);
    interpolate_numeric_value(left, right, t)
}
