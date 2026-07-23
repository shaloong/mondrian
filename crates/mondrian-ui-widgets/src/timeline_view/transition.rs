//! Domain-light visual-Transition geometry and resize rules.

use mondrian_ui_core::types::{Point, Rect};

use super::model::{frame_to_x, track_y};
use super::{TimelineTransitionEdge, TimelineTransitionResizePosition};

const VERTICAL_INSET: f32 = 9.0;
const MIN_WIDTH: f32 = 10.0;
const HANDLE_MIN_WIDTH: f32 = 4.0;
const HANDLE_MAX_WIDTH: f32 = 7.0;

pub(super) fn rect(
    body: Rect,
    pixels_per_frame: f32,
    scroll_x: f32,
    track_height: f32,
    scroll_y: f32,
    track_index: usize,
    start_frame: i64,
    duration_frames: i64,
) -> Rect {
    let x = frame_to_x(body, pixels_per_frame, scroll_x, start_frame);
    let y = track_y(body, track_height, scroll_y, track_index) + VERTICAL_INSET;
    let width = if pixels_per_frame.is_finite() && pixels_per_frame > 0.0 {
        (duration_frames.max(1) as f32 * pixels_per_frame).max(MIN_WIDTH)
    } else {
        MIN_WIDTH
    };
    Rect::new(x, y, width, (track_height - VERTICAL_INSET * 2.0).max(0.0))
}

pub(super) fn edge_at(rect: Rect, point: Point) -> Option<TimelineTransitionEdge> {
    if !rect.contains(point) {
        return None;
    }
    let width = (rect.width * 0.18).clamp(HANDLE_MIN_WIDTH, HANDLE_MAX_WIDTH);
    if point.x <= rect.x + width {
        Some(TimelineTransitionEdge::In)
    } else if point.x >= rect.x + rect.width - width {
        Some(TimelineTransitionEdge::Out)
    } else {
        None
    }
}

pub(super) fn resize_position(
    edge: TimelineTransitionEdge,
    pointer_frame: i64,
    old_start_frame: i64,
    old_duration_frames: i64,
    cut_frame: i64,
    minimum_start_frame: i64,
    maximum_end_frame: i64,
) -> TimelineTransitionResizePosition {
    let old_end = old_start_frame.saturating_add(old_duration_frames.max(1));
    match edge {
        TimelineTransitionEdge::In => {
            let maximum_start = cut_frame.min(old_end.saturating_sub(1));
            let start = pointer_frame.clamp(minimum_start_frame, maximum_start);
            TimelineTransitionResizePosition {
                start_frame: start,
                duration_frames: old_end.saturating_sub(start).max(1),
            }
        }
        TimelineTransitionEdge::Out => {
            let minimum_end = cut_frame.max(old_start_frame.saturating_add(1));
            let end = pointer_frame.clamp(minimum_end, maximum_end_frame.max(minimum_end));
            TimelineTransitionResizePosition {
                start_frame: old_start_frame,
                duration_frames: end.saturating_sub(old_start_frame).max(1),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_cannot_move_a_transition_away_from_its_cut() {
        let left = resize_position(TimelineTransitionEdge::In, 18, 8, 4, 10, 0, 20);
        assert_eq!(left.start_frame, 10);
        assert_eq!(left.duration_frames, 2);

        let right = resize_position(TimelineTransitionEdge::Out, 2, 8, 4, 10, 0, 20);
        assert_eq!(right.start_frame, 8);
        assert_eq!(right.duration_frames, 2);
    }

    #[test]
    fn resize_is_bounded_by_endpoint_placement_geometry() {
        let left = resize_position(TimelineTransitionEdge::In, -100, 8, 4, 10, 3, 16);
        assert_eq!(left.start_frame, 3);
        assert_eq!(left.duration_frames, 9);

        let right = resize_position(TimelineTransitionEdge::Out, 100, 8, 4, 10, 3, 16);
        assert_eq!(right.start_frame, 8);
        assert_eq!(right.duration_frames, 8);
    }
}
