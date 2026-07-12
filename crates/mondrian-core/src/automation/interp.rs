//! Interpolation helpers: bezier, auto-slope, tangents.
use super::*;
use crate::TimeScale;

pub(crate) fn interpolation_defaults(
    interpolation: InterpolationType,
) -> (
    KeyframeInterpolation,
    KeyframeInterpolation,
    KeyframeTemporalFlags,
) {
    match interpolation {
        InterpolationType::Hold => (
            KeyframeInterpolation::Hold,
            KeyframeInterpolation::Hold,
            KeyframeTemporalFlags::default(),
        ),
        InterpolationType::Linear => (
            KeyframeInterpolation::Linear,
            KeyframeInterpolation::Linear,
            KeyframeTemporalFlags::default(),
        ),
        InterpolationType::Bezier => (
            KeyframeInterpolation::Bezier(default_out_bezier_handle()),
            KeyframeInterpolation::Bezier(default_in_bezier_handle()),
            KeyframeTemporalFlags::default(),
        ),
        InterpolationType::AutoBezier => (
            KeyframeInterpolation::Bezier(default_in_bezier_handle()),
            KeyframeInterpolation::Bezier(default_out_bezier_handle()),
            KeyframeTemporalFlags {
                auto_bezier: true,
                continuous: true,
                broken_handles: false,
            },
        ),
        InterpolationType::ContinuousBezier => (
            KeyframeInterpolation::Bezier(default_in_bezier_handle()),
            KeyframeInterpolation::Bezier(default_out_bezier_handle()),
            KeyframeTemporalFlags {
                auto_bezier: false,
                continuous: true,
                broken_handles: false,
            },
        ),
        InterpolationType::EaseIn => (
            KeyframeInterpolation::Linear,
            KeyframeInterpolation::Bezier(default_out_bezier_handle()),
            KeyframeTemporalFlags::default(),
        ),
        InterpolationType::EaseOut => (
            KeyframeInterpolation::Bezier(default_in_bezier_handle()),
            KeyframeInterpolation::Linear,
            KeyframeTemporalFlags::default(),
        ),
    }
}

const fn default_out_bezier_handle() -> BezierHandle {
    BezierHandle {
        time_offset: TimelineTime::ONE_THIRD,
        value_offset: 0.0,
    }
}

const fn default_in_bezier_handle() -> BezierHandle {
    BezierHandle {
        time_offset: TimelineTime::NEGATIVE_ONE_THIRD,
        value_offset: 0.0,
    }
}

pub(crate) fn handle_for_out(interpolation: KeyframeInterpolation) -> BezierHandle {
    match interpolation {
        KeyframeInterpolation::Bezier(handle) => BezierHandle {
            time_offset: handle.time_offset.clamp(TimelineTime::ZERO, TimelineTime::ONE),
            value_offset: handle.value_offset,
        },
        KeyframeInterpolation::Linear => default_out_bezier_handle(),
        KeyframeInterpolation::Hold => {
            BezierHandle { time_offset: TimelineTime::ZERO, value_offset: 0.0 }
        }
    }
}

pub(crate) fn handle_for_in(interpolation: KeyframeInterpolation) -> BezierHandle {
    match interpolation {
        KeyframeInterpolation::Bezier(handle) => BezierHandle {
            time_offset: handle.time_offset.clamp(TimelineTime::NEGATIVE_ONE, TimelineTime::ZERO),
            value_offset: handle.value_offset,
        },
        KeyframeInterpolation::Linear => default_in_bezier_handle(),
        KeyframeInterpolation::Hold => {
            BezierHandle { time_offset: TimelineTime::ZERO, value_offset: 0.0 }
        }
    }
}

pub(crate) fn extract_handle(interpolation: KeyframeInterpolation) -> Option<Vec2> {
    match interpolation {
        KeyframeInterpolation::Bezier(handle) => Some(Vec2::new(
            handle.time_offset.to_f64() as f32,
            handle.value_offset as f32,
        )),
        _ => None,
    }
}

pub(crate) fn solve_bezier_t(x: f32, cp_out: BezierHandle, cp_in: BezierHandle) -> f32 {
    let x = x.clamp(0.0, 1.0);
    let p1 = Vec2::new(
        cp_out.time_offset.to_f64() as f32,
        cp_out.value_offset as f32,
    );
    let p2 = Vec2::new(
        (1.0 + cp_in.time_offset.to_f64()) as f32,
        (1.0 + cp_in.value_offset) as f32,
    );

    let sample_curve_x = |t: f32| cubic_bezier(0.0, p1.x, p2.x, 1.0, t);
    let sample_curve_y = |t: f32| cubic_bezier(0.0, p1.y, p2.y, 1.0, t);
    let sample_curve_derivative_x = |t: f32| cubic_bezier_derivative(0.0, p1.x, p2.x, 1.0, t);

    let mut t = x;
    for _ in 0..8 {
        let error = sample_curve_x(t) - x;
        if error.abs() < 1.0e-5 {
            break;
        }

        let derivative = sample_curve_derivative_x(t);
        if derivative.abs() < 1.0e-5 {
            break;
        }

        t = (t - error / derivative).clamp(0.0, 1.0);
    }

    sample_curve_y(t).clamp(0.0, 1.0)
}

pub fn interpolation_mode_from_keyframe(
    interp_in: KeyframeInterpolation,
    interp_out: KeyframeInterpolation,
    temporal_flags: KeyframeTemporalFlags,
) -> InterpolationType {
    let valid_in = !matches!(interp_in, KeyframeInterpolation::Linear);
    let valid_out = !matches!(interp_out, KeyframeInterpolation::Linear);
    let has_bezier = matches!(interp_in, KeyframeInterpolation::Bezier(_))
        || matches!(interp_out, KeyframeInterpolation::Bezier(_));

    if matches!(interp_in, KeyframeInterpolation::Hold)
        || matches!(interp_out, KeyframeInterpolation::Hold)
    {
        InterpolationType::Hold
    } else if temporal_flags.auto_bezier && has_bezier {
        InterpolationType::AutoBezier
    } else if temporal_flags.continuous && !temporal_flags.broken_handles && has_bezier {
        InterpolationType::ContinuousBezier
    } else if valid_in || valid_out {
        InterpolationType::Bezier
    } else {
        InterpolationType::Linear
    }
}

pub(crate) fn compute_auto_bezier_handles(
    keyframes: &[Keyframe<f64>],
) -> Vec<(KeyframeInterpolation, KeyframeInterpolation)> {
    let count = keyframes.len();
    if count == 0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![(KeyframeInterpolation::Linear, KeyframeInterpolation::Linear)];
    }

    let mut slopes = vec![0.0; count];
    let h = keyframes
        .windows(2)
        .map(|pair| time_delta(pair[1].time, pair[0].time))
        .collect::<Vec<_>>();
    let delta = keyframes
        .windows(2)
        .zip(h.iter())
        .map(|(pair, dt)| (pair[1].value - pair[0].value) / *dt)
        .collect::<Vec<_>>();

    slopes[0] = endpoint_auto_slope(delta[0], delta.get(1).copied(), h[0], h.get(1).copied());
    slopes[count - 1] = endpoint_auto_slope(
        *delta.last().unwrap_or(&0.0),
        delta.get(delta.len().saturating_sub(2)).copied(),
        *h.last().unwrap_or(&1.0),
        h.get(h.len().saturating_sub(2)).copied(),
    );

    for index in 1..count - 1 {
        slopes[index] = interior_auto_slope(delta[index - 1], delta[index], h[index - 1], h[index]);
    }

    keyframes
        .iter()
        .enumerate()
        .map(|(index, keyframe)| {
            let interp_in = if index == 0 {
                KeyframeInterpolation::Linear
            } else {
                let prev = &keyframes[index - 1];
                let dt = time_delta(keyframe.time, prev.time);
                let dv = keyframe.value - prev.value;
                KeyframeInterpolation::Bezier(monotone_in_handle(slopes[index], dt, dv))
            };
            let interp_out = if index + 1 >= count {
                KeyframeInterpolation::Linear
            } else {
                let next = &keyframes[index + 1];
                let dt = time_delta(next.time, keyframe.time);
                let dv = next.value - keyframe.value;
                KeyframeInterpolation::Bezier(monotone_out_handle(slopes[index], dt, dv))
            };
            (interp_in, interp_out)
        })
        .collect()
}

pub(crate) fn endpoint_auto_slope(
    primary_delta: f64,
    secondary_delta: Option<f64>,
    primary_h: f64,
    secondary_h: Option<f64>,
) -> f64 {
    let Some(secondary_delta) = secondary_delta else {
        return primary_delta;
    };
    let secondary_h = secondary_h.unwrap_or(primary_h);
    let mut slope = ((2.0 * primary_h + secondary_h) * primary_delta - primary_h * secondary_delta)
        / (primary_h + secondary_h);
    if slope.signum() != primary_delta.signum() {
        slope = 0.0;
    } else if primary_delta.signum() != secondary_delta.signum()
        && slope.abs() > 3.0 * primary_delta.abs()
    {
        slope = 3.0 * primary_delta;
    }
    slope
}

pub(crate) fn interior_auto_slope(
    delta_prev: f64,
    delta_next: f64,
    h_prev: f64,
    h_next: f64,
) -> f64 {
    if delta_prev.abs() < f64::EPSILON
        || delta_next.abs() < f64::EPSILON
        || delta_prev.signum() != delta_next.signum()
    {
        return 0.0;
    }
    let w1 = 2.0 * h_next + h_prev;
    let w2 = h_next + 2.0 * h_prev;
    (w1 + w2) / (w1 / delta_prev + w2 / delta_next)
}

pub(crate) fn continuous_tangent_slope(
    previous: &Keyframe<f64>,
    current: &Keyframe<f64>,
    next: &Keyframe<f64>,
) -> f64 {
    let in_handle = handle_for_in(current.interp_in);
    let out_handle = handle_for_out(current.interp_out);
    let in_slope = tangent_slope_from_in(previous, current, in_handle);
    let out_slope = tangent_slope_from_out(current, next, out_handle);

    match (in_slope, out_slope) {
        (Some(in_slope), Some(out_slope)) => {
            if in_slope.signum() != out_slope.signum() {
                0.0
            } else {
                (in_slope + out_slope) * 0.5
            }
        }
        (Some(in_slope), None) => in_slope,
        (None, Some(out_slope)) => out_slope,
        (None, None) => 0.0,
    }
}

pub(crate) fn tangent_slope_from_out(
    current: &Keyframe<f64>,
    next: &Keyframe<f64>,
    handle: BezierHandle,
) -> Option<f64> {
    let dv = next.value - current.value;
    let dt = time_delta(next.time, current.time);
    let dx = normalized_handle_magnitude(handle.time_offset);
    if dv.abs() < f64::EPSILON {
        Some(0.0)
    } else {
        Some(handle.value_offset * dv / (dx * dt))
    }
}

pub(crate) fn tangent_slope_from_in(
    previous: &Keyframe<f64>,
    current: &Keyframe<f64>,
    handle: BezierHandle,
) -> Option<f64> {
    let dv = current.value - previous.value;
    let dt = time_delta(current.time, previous.time);
    let dx = normalized_handle_magnitude(handle.time_offset);
    if dv.abs() < f64::EPSILON {
        Some(0.0)
    } else {
        Some(-handle.value_offset * dv / (dx * dt))
    }
}

pub(crate) fn continuous_handle_for_out(
    current: &Keyframe<f64>,
    next: &Keyframe<f64>,
    slope: f64,
    time_offset: TimelineTime,
) -> BezierHandle {
    let dt = time_delta(next.time, current.time);
    let dv = next.value - current.value;
    let time_offset = normalized_positive_handle(time_offset);
    if dv.abs() < f64::EPSILON || slope.abs() < f64::EPSILON {
        BezierHandle { time_offset, value_offset: 0.0 }
    } else {
        BezierHandle {
            time_offset,
            value_offset: (slope * time_offset.to_f64() * dt / dv).clamp(-2.0, 2.0),
        }
    }
}

pub(crate) fn continuous_handle_for_in(
    previous: &Keyframe<f64>,
    current: &Keyframe<f64>,
    slope: f64,
    time_offset: TimelineTime,
) -> BezierHandle {
    let dt = time_delta(current.time, previous.time);
    let dv = current.value - previous.value;
    let positive = normalized_positive_handle(time_offset);
    let time_offset = positive
        .checked_scale(TimeScale::NEGATIVE_ONE)
        .unwrap_or(TimelineTime::NEGATIVE_ONE_THIRD);
    if dv.abs() < f64::EPSILON || slope.abs() < f64::EPSILON {
        BezierHandle { time_offset, value_offset: 0.0 }
    } else {
        BezierHandle {
            time_offset,
            value_offset: (slope * time_offset.to_f64() * dt / dv).clamp(-2.0, 2.0),
        }
    }
}

pub(crate) fn monotone_out_handle(slope: f64, dt: f64, dv: f64) -> BezierHandle {
    if dv.abs() < f64::EPSILON || slope.abs() < f64::EPSILON {
        return default_out_bezier_handle();
    }
    BezierHandle {
        time_offset: TimelineTime::ONE_THIRD,
        value_offset: ((slope * dt / dv) / 3.0).clamp(-1.0, 1.0),
    }
}

pub(crate) fn monotone_in_handle(slope: f64, dt: f64, dv: f64) -> BezierHandle {
    if dv.abs() < f64::EPSILON || slope.abs() < f64::EPSILON {
        return default_in_bezier_handle();
    }
    BezierHandle {
        time_offset: TimelineTime::NEGATIVE_ONE_THIRD,
        value_offset: (-(slope * dt / dv) / 3.0).clamp(-1.0, 1.0),
    }
}

fn time_delta(later: TimelineTime, earlier: TimelineTime) -> f64 {
    (later.to_f64() - earlier.to_f64()).max(f64::EPSILON)
}

fn normalized_positive_handle(value: TimelineTime) -> TimelineTime {
    let magnitude = if value.is_negative() {
        value.checked_scale(TimeScale::NEGATIVE_ONE).unwrap_or(TimelineTime::ONE_THIRD)
    } else {
        value
    };
    magnitude.clamp(
        TimelineTime::FIVE_PERCENT,
        TimelineTime::NINETY_FIVE_PERCENT,
    )
}

fn normalized_handle_magnitude(value: TimelineTime) -> f64 {
    normalized_positive_handle(value).to_f64()
}

pub(crate) fn cubic_bezier(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let u = 1.0 - t;
    u * u * u * p0 + 3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t * p3
}

pub(crate) fn cubic_bezier_derivative(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let u = 1.0 - t;
    3.0 * u * u * (p1 - p0) + 6.0 * u * t * (p2 - p1) + 3.0 * t * t * (p3 - p2)
}
