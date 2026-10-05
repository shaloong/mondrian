//! Interpolation helpers: bezier, auto-slope, tangents.
use super::*;

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
            KeyframeInterpolation::DefaultBezier,
            KeyframeInterpolation::DefaultBezier,
            KeyframeTemporalFlags::default(),
        ),
        InterpolationType::AutoBezier => (
            KeyframeInterpolation::DefaultBezier,
            KeyframeInterpolation::DefaultBezier,
            KeyframeTemporalFlags {
                auto_bezier: true,
                continuous: true,
                broken_handles: false,
            },
        ),
        InterpolationType::ContinuousBezier => (
            KeyframeInterpolation::DefaultBezier,
            KeyframeInterpolation::DefaultBezier,
            KeyframeTemporalFlags {
                auto_bezier: false,
                continuous: true,
                broken_handles: false,
            },
        ),
        InterpolationType::EaseIn => (
            KeyframeInterpolation::Linear,
            KeyframeInterpolation::DefaultBezier,
            KeyframeTemporalFlags::default(),
        ),
        InterpolationType::EaseOut => (
            KeyframeInterpolation::DefaultBezier,
            KeyframeInterpolation::Linear,
            KeyframeTemporalFlags::default(),
        ),
    }
}

/// Resolve the stored side intents and constraints to a displayed interpolation family.
pub fn interpolation_mode_from_keyframe(
    interp_in: KeyframeInterpolation,
    interp_out: KeyframeInterpolation,
    temporal_flags: KeyframeTemporalFlags,
) -> InterpolationType {
    let valid_in = !matches!(interp_in, KeyframeInterpolation::Linear);
    let valid_out = !matches!(interp_out, KeyframeInterpolation::Linear);
    let has_bezier = matches!(
        interp_in,
        KeyframeInterpolation::Bezier(_) | KeyframeInterpolation::DefaultBezier
    ) || matches!(
        interp_out,
        KeyframeInterpolation::Bezier(_) | KeyframeInterpolation::DefaultBezier
    );

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

pub(crate) fn endpoint_auto_slope(
    primary_delta: f64,
    secondary_delta: Option<f64>,
    primary_h: f64,
    secondary_h: Option<f64>,
) -> f64 {
    let Some(secondary_delta) = secondary_delta else {
        return primary_delta;
    };
    if !primary_delta.is_finite() {
        return primary_delta;
    }
    if !secondary_delta.is_finite() {
        return if primary_delta.signum() == secondary_delta.signum() {
            0.0
        } else {
            3.0 * primary_delta
        };
    }
    let secondary_h = secondary_h.unwrap_or(primary_h);
    let weight = primary_h / (primary_h + secondary_h);
    let mut slope = interpolate_numeric_value(primary_delta, secondary_delta, -weight);
    if slope.signum() != primary_delta.signum() {
        slope = 0.0;
    } else if primary_delta.signum() != secondary_delta.signum()
        && slope.abs() > 3.0 * primary_delta.abs()
    {
        slope = 3.0 * primary_delta;
    }
    slope
}

#[cfg(test)]
pub(super) fn time_delta(later: TimelineTime, earlier: TimelineTime) -> f64 {
    later.distance_from_f64(earlier)
}

#[cfg(test)]
pub(crate) fn handle_for_out(interpolation: KeyframeInterpolation) -> ExactBezierHandle {
    match interpolation {
        KeyframeInterpolation::Bezier(handle) => handle,
        _ => ExactBezierHandle { time_offset: TimelineTime::ZERO, value_offset: 0.0 },
    }
}
#[cfg(test)]
pub(crate) fn handle_for_in(interpolation: KeyframeInterpolation) -> ExactBezierHandle {
    handle_for_out(interpolation)
}
#[cfg(test)]
pub(crate) fn tangent_slope_from_out<T>(
    _current: &Keyframe<T>,
    _next: &Keyframe<T>,
    handle: ExactBezierHandle,
) -> Option<f64> {
    (!handle.time_offset.is_zero()).then(|| handle.value_offset / handle.time_offset.to_f64())
}
#[cfg(test)]
pub(crate) fn tangent_slope_from_in<T>(
    previous: &Keyframe<T>,
    current: &Keyframe<T>,
    handle: ExactBezierHandle,
) -> Option<f64> {
    tangent_slope_from_out(previous, current, handle)
}
