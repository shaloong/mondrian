//! Screen projection and sparse exact author coordinates for shared temporal handles.
use mondrian_core::{
    BezierHandleSide, ExactBezierHandle, ExactBezierHandleEdit, TimeScale, TimelineTime,
    TimelineTimeRange,
};
use mondrian_ui_widgets::{CurvePoint, CurveTangent};

pub(super) fn project_key(
    time: TimelineTime,
    value: f64,
    range: TimelineTimeRange,
    values: [f64; 2],
) -> Option<CurvePoint> {
    let elapsed = time.checked_sub(range.start).ok()?.to_f64();
    let x = elapsed / range.duration.to_f64();
    let y = mondrian_core::automation::ParameterNumericRange::new(values[0], values[1])
        .ok()?
        .normalized_position(value)?;
    (x.is_finite() && y.is_finite()).then(|| CurvePoint::new(x as f32, y as f32))
}

pub(super) fn project(
    keyframe_id: mondrian_core::KeyframeId,
    side: BezierHandleSide,
    key: (TimelineTime, f64),
    neighbor: TimelineTime,
    handle: ExactBezierHandle,
    range: TimelineTimeRange,
    values: [f64; 2],
) -> Option<CurveTangent> {
    let (time, value) = key;
    let span = range.duration.to_f64();
    let width = values[1] - values[0];
    if span <= 0.0
        || values[0] >= values[1]
        || !span.is_finite()
        || values.iter().any(|value| !value.is_finite())
    {
        return None;
    }
    let ratio = |time: TimelineTime| time.distance_from_f64(range.start) / span;
    let owner = ratio(time) as f32;
    let neighbor = ratio(neighbor) as f32;
    let x = ratio(time.checked_add(handle.time_offset).ok()?) as f32;
    let control = value + handle.value_offset;
    if !control.is_finite() {
        return None;
    }
    let y = if width.is_finite() && (control - values[0]).is_finite() {
        (control - values[0]) / width
    } else {
        (control * 0.5 - values[0] * 0.5) / (values[1] * 0.5 - values[0] * 0.5)
    } as f32;
    let limits = [owner.min(neighbor).max(0.0), owner.max(neighbor).min(1.0)];
    if !x.is_finite() || !y.is_finite() || limits[0] > limits[1] {
        return None;
    }
    Some(CurveTangent {
        keyframe_id,
        side,
        point: CurvePoint { x, y },
        time_limits: limits,
    })
}

pub(super) fn lower(
    tangent: CurveTangent,
    point: CurvePoint,
    time: TimelineTime,
    handle: ExactBezierHandle,
    range: TimelineTimeRange,
    values: [f64; 2],
) -> Option<ExactBezierHandleEdit> {
    if !point.x.is_finite() || !point.y.is_finite() {
        return None;
    }
    let time_offset = if point.x == tangent.point.x {
        None
    } else {
        let ratio = TimelineTime::from_f64_quantized(f64::from(point.x), 1_000_000).ok()?;
        Some(
            range
                .start
                .checked_add(range.duration.checked_scale(TimeScale::try_from(ratio).ok()?).ok()?)
                .ok()?
                .checked_sub(time)
                .ok()?,
        )
    };
    let value_offset = (point.y != tangent.point.y).then(|| {
        let delta = f64::from(point.y - tangent.point.y);
        let width = values[1] - values[0];
        if width.is_finite() {
            handle.value_offset + delta * width
        } else {
            (handle.value_offset * 0.5 + delta * (values[1] * 0.5 - values[0] * 0.5)) * 2.0
        }
    });
    if value_offset.is_some_and(|value| !value.is_finite()) {
        return None;
    }
    // Exact origin is an intentional no-op that can restore a live gesture.
    Some(ExactBezierHandleEdit {
        time_offset: time_offset.or(Some(handle.time_offset)),
        value_offset: value_offset.or(Some(handle.value_offset)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn temporal_handle_projection_and_lowering_cover_full_finite_value_span() {
        let range = TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE).expect("range");
        let time = TimelineTime::new(1, 3).expect("exact key");
        let handle = ExactBezierHandle {
            time_offset: TimelineTime::new(1, 7).expect("exact handle"),
            value_offset: f64::MAX / 4.0,
        };
        let tangent = project(
            mondrian_core::KeyframeId::new(),
            BezierHandleSide::Outgoing,
            (time, 0.0),
            TimelineTime::ONE,
            handle,
            range,
            [-f64::MAX, f64::MAX],
        )
        .expect("finite wide-range projection");
        assert_eq!(tangent.point.y, 0.625);
        let same = lower(
            tangent,
            tangent.point,
            time,
            handle,
            range,
            [-f64::MAX, f64::MAX],
        )
        .expect("exact origin");
        assert_eq!(same.time_offset, Some(handle.time_offset));
        assert_eq!(same.value_offset, Some(handle.value_offset));
        let edit = lower(
            tangent,
            CurvePoint { y: 0.75, ..tangent.point },
            time,
            handle,
            range,
            [-f64::MAX, f64::MAX],
        )
        .expect("finite edit");
        assert_eq!(edit.time_offset, Some(handle.time_offset));
        assert_eq!(edit.value_offset, Some(f64::MAX / 2.0));
    }
    #[test]
    fn temporal_handle_projection_retains_unclamped_controls_and_rejects_nonfinite_edits() {
        let range = TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE).expect("range");
        let handle = ExactBezierHandle {
            time_offset: TimelineTime::new(1, 3).expect("time"),
            value_offset: 2.0,
        };
        let tangent = project(
            mondrian_core::KeyframeId::new(),
            BezierHandleSide::Outgoing,
            (TimelineTime::ZERO, 0.25),
            TimelineTime::ONE,
            handle,
            range,
            [0.0, 1.0],
        )
        .expect("projection");
        assert_eq!(tangent.point.y, 2.25);
        assert!(lower(
            tangent,
            CurvePoint { y: f32::NAN, ..tangent.point },
            TimelineTime::ZERO,
            handle,
            range,
            [0.0, 1.0]
        )
        .is_none());
    }
}
