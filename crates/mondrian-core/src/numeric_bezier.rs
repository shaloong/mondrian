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
    cubic_numeric_value(
        values,
        bezier_parameter(progress, time_controls, uniform_time),
    )
}

pub(crate) fn bezier_parameter(progress: f64, time_controls: [f64; 2], uniform_time: bool) -> f64 {
    if uniform_time || progress == 0.0 || progress == 1.0 {
        return progress;
    }
    let mut low = 0.0;
    let mut high = 1.0;
    let midpoint = cubic_numeric_value([0.0, time_controls[0], time_controls[1], 1.0], 0.5);
    if progress == midpoint {
        return 0.5;
    }
    if progress < midpoint {
        // On u<=1/2 each nonnegative Bernstein term gives an upper bound:
        // x(u)>=3*x1*u/4, x(u)>=3*x2*u²/2, and x(u)>=u³.
        // Scaling the bracket to the actual root retains relative precision
        // for tiny SDK times without adding iterations to every render sample.
        high = 0.5_f64.min(progress.cbrt());
        if time_controls[0] > 0.0 {
            high = high.min(progress / (0.75 * time_controls[0]));
        }
        if time_controls[1] > 0.0 {
            high = high.min((progress / (1.5 * time_controls[1])).sqrt());
        }
        high = high.next_up();
    }
    for _ in 0..56 {
        let candidate = (low + high) * 0.5;
        let sample = cubic_numeric_value([0.0, time_controls[0], time_controls[1], 1.0], candidate);
        if sample == progress || candidate == low || candidate == high {
            return candidate;
        }
        if sample < progress {
            low = candidate;
        } else {
            high = candidate;
        }
    }
    (low + high) * 0.5
}

pub(crate) fn cubic_numeric_value([p0, p1, p2, p3]: [f64; 4], t: f64) -> f64 {
    let left = interpolate_numeric_value(p0, p1, t);
    let middle = interpolate_numeric_value(p1, p2, t);
    let right = interpolate_numeric_value(p2, p3, t);
    let left = interpolate_numeric_value(left, middle, t);
    let right = interpolate_numeric_value(middle, right, t);
    interpolate_numeric_value(left, right, t)
}

/// Owner-time derivative of the same parametric cubic used for value queries.
pub(crate) fn numeric_bezier_derivative(
    progress: f64,
    time_controls: [f64; 2],
    values: [f64; 4],
    uniform_time: bool,
    span: f64,
) -> Option<f64> {
    let u = bezier_parameter(progress, time_controls, uniform_time);
    let times = [0.0, time_controls[0], time_controls[1], 1.0];
    let mut y =
        std::array::from_fn::<_, 3, _>(|i| scaled_difference(values[i + 1], values[i], span));
    let mut x = std::array::from_fn::<_, 3, _>(|i| times[i + 1] - times[i]);
    // Common factors cancel. At a stationary time control, compare successive
    // polynomial orders instead of manufacturing a finite vertical slope.
    for length in (1..=3).rev() {
        let dy = polynomial_value(&y[..length], u);
        let dx = if uniform_time {
            1.0 / 3.0
        } else {
            polynomial_value(&x[..length], u)
        };
        if dx != 0.0 {
            let result = dy / dx;
            return result.is_finite().then_some(result);
        }
        if dy != 0.0 || !dy.is_finite() {
            return None;
        }
        for i in 0..length.saturating_sub(1) {
            y[i] = y[i + 1] - y[i];
            x[i] = x[i + 1] - x[i];
        }
    }
    None
}

fn scaled_difference(a: f64, b: f64, scale: f64) -> f64 {
    let difference = a - b;
    if difference.is_finite() {
        difference / scale
    } else {
        a / scale - b / scale
    }
}

fn polynomial_value(values: &[f64], u: f64) -> f64 {
    match values {
        [a] => *a,
        [a, b] => interpolate_numeric_value(*a, *b, u),
        [a, b, c] => interpolate_numeric_value(
            interpolate_numeric_value(*a, *b, u),
            interpolate_numeric_value(*b, *c, u),
            u,
        ),
        _ => 0.0,
    }
}

/// Average value over an admitted time interval. Three-point Gauss integration
/// exactly integrates y(u) * x'(u), a polynomial of degree at most five.
pub(crate) fn numeric_bezier_mean(
    start: f64,
    end: f64,
    time_controls: [f64; 2],
    values: [f64; 4],
    uniform_time: bool,
) -> f64 {
    let a = bezier_parameter(start, time_controls, uniform_time);
    let b = bezier_parameter(end, time_controls, uniform_time);
    let middle = (a + b) * 0.5;
    let half = (b - a) * 0.5;
    let offset = half * (3.0_f64 / 5.0).sqrt();
    let samples = [middle - offset, middle, middle + offset];
    let weights = [5.0 / 9.0, 8.0 / 9.0, 5.0 / 9.0];
    let x = [
        time_controls[0],
        time_controls[1] - time_controls[0],
        1.0 - time_controls[1],
    ];
    let weights = std::array::from_fn::<_, 3, _>(|i| {
        weights[i]
            * if uniform_time {
                1.0
            } else {
                polynomial_value(&x, samples[i]).max(0.0)
            }
    });
    let scale = weights.into_iter().fold(0.0_f64, f64::max);
    if scale == 0.0 {
        return cubic_numeric_value(values, middle);
    }
    let mut average = 0.0;
    let mut total = 0.0;
    for i in 0..3 {
        let weight = weights[i] / scale;
        if weight > 0.0 {
            total += weight;
            average = interpolate_numeric_value(
                average,
                cubic_numeric_value(values, samples[i]),
                weight / total,
            );
        }
    }
    average
}
