//! Screen projection and sparse exact author coordinates for shared temporal handles.
use mondrian_core::{
    BezierHandleSide, ExactBezierHandle, ExactBezierHandleEdit, TimeScale, TimelineTime,
    TimelineTimeRange,
};
use mondrian_ui_widgets::{CurvePoint, CurveTangent};

pub(super) fn nudge_key_time(
    origin: TimelineTime,
    delta: TimelineTime,
    range: TimelineTimeRange,
    times: impl Iterator<Item = TimelineTime>,
) -> Option<TimelineTime> {
    let time = origin.checked_add(delta).ok()?;
    if time == origin || time < range.start || time > range.end().ok()? {
        return None;
    }
    if times.into_iter().any(|neighbor| neighbor != origin && neighbor == time) {
        return None;
    }
    Some(time)
}

pub(super) fn nudge_handle_time(
    control: mondrian_core::TemporalBezierControl,
    delta: TimelineTime,
) -> Option<ExactBezierHandleEdit> {
    let limit = control.neighbor_time.checked_sub(control.key_time).ok()?;
    let next = control.handle.time_offset.checked_add(delta).ok()?;
    let next = match control.side {
        BezierHandleSide::Incoming => next.max(limit).min(TimelineTime::ZERO),
        BezierHandleSide::Outgoing => next.max(TimelineTime::ZERO).min(limit),
    };
    (next != control.handle.time_offset)
        .then_some(ExactBezierHandleEdit { time_offset: Some(next), value_offset: None })
}

pub(super) fn project_key(
    time: TimelineTime,
    value: f64,
    range: TimelineTimeRange,
) -> Option<CurvePoint> {
    let elapsed = time.checked_sub(range.start).ok()?.to_f64();
    let x = elapsed / range.duration.to_f64();
    (x.is_finite() && value.is_finite()).then_some(CurvePoint { x: x as f32, y: value })
}

pub(super) fn project(
    keyframe_id: mondrian_core::KeyframeId,
    side: BezierHandleSide,
    key: (TimelineTime, f64),
    neighbor: TimelineTime,
    handle: ExactBezierHandle,
    range: TimelineTimeRange,
) -> Option<CurveTangent> {
    let (time, value) = key;
    let span = range.duration.to_f64();
    if span <= 0.0 || !span.is_finite() {
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
    let y = control;
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
    key: (TimelineTime, f64),
    handle: ExactBezierHandle,
    range: TimelineTimeRange,
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
                .checked_sub(key.0)
                .ok()?,
        )
    };
    let value_offset = (point.y != tangent.point.y).then_some(point.y - key.1);
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
    fn exact_time_nudge_rejects_collisions_crossing_boundaries_and_zero_delta() {
        let origin = TimelineTime::new(1, 2).expect("origin");
        let frame = TimelineTime::new(1001, 30000).expect("fractional frame");
        let occupied = origin.checked_add(frame).expect("neighbor");
        let range = TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE).expect("range");
        assert_eq!(
            nudge_key_time(origin, frame, range, [occupied].into_iter()),
            None
        );
        assert_eq!(
            nudge_key_time(origin, TimelineTime::ONE, range, [].into_iter()),
            None
        );
        assert_eq!(
            nudge_key_time(origin, TimelineTime::ZERO, range, [].into_iter()),
            None
        );
        let dense_range = TimelineTimeRange::new(
            TimelineTime::ZERO,
            TimelineTime::new(3600, 1).expect("long viewport"),
        )
        .expect("range");
        assert_eq!(
            nudge_key_time(
                origin,
                frame,
                dense_range,
                [TimelineTime::ZERO, TimelineTime::ONE].into_iter()
            ),
            Some(occupied)
        );
        let incoming = mondrian_core::TemporalBezierControl {
            keyframe_id: mondrian_core::KeyframeId::new(),
            key_time: origin,
            key_value: 0.5,
            neighbor_time: TimelineTime::ZERO,
            side: BezierHandleSide::Incoming,
            handle: ExactBezierHandle {
                time_offset: TimelineTime::ZERO.checked_sub(frame).expect("negative"),
                value_offset: -0.123456789,
            },
        };
        assert_eq!(
            nudge_handle_time(incoming, frame),
            Some(ExactBezierHandleEdit {
                time_offset: Some(TimelineTime::ZERO),
                value_offset: None
            })
        );
        let zero = mondrian_core::TemporalBezierControl {
            handle: ExactBezierHandle { time_offset: TimelineTime::ZERO, ..incoming.handle },
            ..incoming
        };
        assert_eq!(nudge_handle_time(zero, frame), None);
    }

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
        )
        .expect("finite wide-range projection");
        assert_eq!(tangent.point.y, f64::MAX / 4.0);
        let same = lower(tangent, tangent.point, (time, 0.0), handle, range).expect("exact origin");
        assert_eq!(same.time_offset, Some(handle.time_offset));
        assert_eq!(same.value_offset, Some(handle.value_offset));
        let edit = lower(
            tangent,
            CurvePoint { y: f64::MAX / 2.0, ..tangent.point },
            (time, 0.0),
            handle,
            range,
        )
        .expect("finite edit");
        assert_eq!(edit.time_offset, Some(handle.time_offset));
        assert_eq!(edit.value_offset, Some(f64::MAX / 2.0));
        let flipped = lower(
            tangent,
            CurvePoint { y: -f64::MAX, ..tangent.point },
            (time, 0.0),
            handle,
            range,
        )
        .expect("finite opposite control without an overflowing screen delta");
        assert_eq!(flipped.time_offset, Some(handle.time_offset));
        assert_eq!(flipped.value_offset, Some(-f64::MAX));
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
        )
        .expect("projection");
        assert_eq!(tangent.point.y, 2.25);
        assert!(lower(
            tangent,
            CurvePoint { y: f64::NAN, ..tangent.point },
            (TimelineTime::ZERO, 0.25),
            handle,
            range,
        )
        .is_none());
    }
}
