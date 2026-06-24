use mondrian_ui_core::types::{Point, Rect};

use super::{
    TimelineScrollbarDragKind, TimelineTrackControl, TimelineTrackRef, TimelineTrimEdge,
    SCROLLBAR_HANDLE_SIZE, SCROLLBAR_MIN_THUMB, SCROLLBAR_THICKNESS,
    TIMELINE_CONTENT_TRAILING_PADDING, TIMELINE_MAX_PIXELS_PER_FRAME, TIMELINE_MAX_TRACK_HEIGHT,
    TIMELINE_MIN_PIXELS_PER_FRAME, TIMELINE_MIN_TRACK_HEIGHT, TIMELINE_SCROLLBAR_GUTTER,
    TIMELINE_TOOLBAR_HEIGHT,
};

const CLIP_VERTICAL_INSET: f32 = 4.0;
const CLIP_MIN_WIDTH: f32 = 8.0;
const CLIP_EDGE_MIN_WIDTH: f32 = 3.0;
const CLIP_EDGE_MAX_WIDTH: f32 = 6.0;
const CLIP_EDGE_WIDTH_RATIO: f32 = 0.35;
const TRACK_CONTROL_SIZE: f32 = 18.0;
const TRACK_CONTROL_GAP: f32 = 8.0;
const TRACK_CONTROL_RIGHT_PADDING: f32 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct TimelineLayoutRects {
    pub toolbar: Rect,
    pub header: Rect,
    pub ruler: Rect,
    pub body: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct TimelineClipDragPosition {
    pub start_frame: i64,
    pub track_index: usize,
    pub changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TimelineTrimDragPosition {
    pub start_frame: i64,
    pub duration_frames: i64,
    pub changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct TimelineHorizontalZoomUpdate {
    pub pixels_per_frame: f32,
    pub scroll_x: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct TimelineTrackResizeUpdate {
    pub track_height: f32,
    pub scroll_y: f32,
}

pub(super) fn layout_rects(
    bounds: Rect,
    header_width: f32,
    ruler_height: f32,
) -> TimelineLayoutRects {
    let top_chrome = TIMELINE_TOOLBAR_HEIGHT + ruler_height.max(0.0);
    let header_width = header_width.max(0.0).min(bounds.width.max(0.0));
    let available_content_width = (bounds.width - header_width).max(0.0);
    let available_content_height = (bounds.height - top_chrome).max(0.0);
    let vertical_gutter = TIMELINE_SCROLLBAR_GUTTER.min(available_content_width);
    let horizontal_gutter = TIMELINE_SCROLLBAR_GUTTER.min(available_content_height);
    let viewport_width = (available_content_width - vertical_gutter).max(0.0);
    let viewport_height = (available_content_height - horizontal_gutter).max(0.0);

    TimelineLayoutRects {
        toolbar: Rect::new(
            bounds.x,
            bounds.y,
            bounds.width.max(0.0),
            TIMELINE_TOOLBAR_HEIGHT,
        ),
        header: Rect::new(
            bounds.x,
            bounds.y + top_chrome,
            header_width,
            viewport_height,
        ),
        ruler: Rect::new(
            bounds.x + header_width,
            bounds.y + TIMELINE_TOOLBAR_HEIGHT,
            viewport_width,
            ruler_height.max(0.0),
        ),
        body: Rect::new(
            bounds.x + header_width,
            bounds.y + top_chrome,
            viewport_width,
            viewport_height,
        ),
    }
}

pub(super) fn horizontal_scrollbar_track_rect(body: Rect) -> Option<Rect> {
    (body.width > SCROLLBAR_MIN_THUMB).then_some(Rect::new(
        body.x + SCROLLBAR_HANDLE_SIZE * 0.5,
        body.y + body.height,
        (body.width - SCROLLBAR_HANDLE_SIZE).max(0.0),
        SCROLLBAR_THICKNESS,
    ))
}

pub(super) fn vertical_scrollbar_track_rect(body: Rect) -> Option<Rect> {
    (body.height > SCROLLBAR_MIN_THUMB).then_some(Rect::new(
        body.x + body.width,
        body.y + SCROLLBAR_HANDLE_SIZE * 0.5,
        SCROLLBAR_THICKNESS,
        (body.height - SCROLLBAR_HANDLE_SIZE).max(0.0),
    ))
}

pub(super) fn scrollbar_clip_rect(body: Rect) -> Rect {
    Rect::new(
        body.x,
        body.y,
        body.width + TIMELINE_SCROLLBAR_GUTTER,
        body.height + TIMELINE_SCROLLBAR_GUTTER,
    )
}

pub(super) fn frame_to_x(body: Rect, pixels_per_frame: f32, scroll_x: f32, frame: i64) -> f32 {
    if pixels_per_frame <= 0.0 || !pixels_per_frame.is_finite() {
        return body.x - scroll_x.max(0.0);
    }
    body.x + frame.max(0) as f32 * pixels_per_frame - scroll_x.max(0.0)
}

pub(super) fn x_to_frame(body: Rect, pixels_per_frame: f32, scroll_x: f32, x: f32) -> i64 {
    if pixels_per_frame <= 0.0 || !pixels_per_frame.is_finite() {
        return 0;
    }
    ((x - body.x + scroll_x.max(0.0)) / pixels_per_frame).round().max(0.0) as i64
}

pub(super) fn track_y(body: Rect, track_height: f32, scroll_y: f32, track_index: usize) -> f32 {
    body.y + track_index as f32 * track_height.max(0.0) - scroll_y.max(0.0)
}

pub(super) fn track_index_at(
    body: Rect,
    track_height: f32,
    scroll_y: f32,
    track_count: usize,
    point: Point,
) -> Option<usize> {
    if !body.contains(point) {
        return None;
    }
    track_index_from_y(body, track_height, scroll_y, track_count, point.y)
}

pub(super) fn track_index_from_y(
    body: Rect,
    track_height: f32,
    scroll_y: f32,
    track_count: usize,
    y: f32,
) -> Option<usize> {
    if track_height <= 0.0 || !track_height.is_finite() {
        return None;
    }
    let index = ((y - body.y + scroll_y.max(0.0)) / track_height).floor();
    (index >= 0.0).then_some(index as usize).filter(|index| *index < track_count)
}

pub(super) fn track_header_at(
    header: Rect,
    body: Rect,
    track_height: f32,
    scroll_y: f32,
    track_count: usize,
    point: Point,
) -> Option<TimelineTrackRef> {
    if !header.contains(point) {
        return None;
    }
    track_index_from_y(body, track_height, scroll_y, track_count, point.y)
        .map(|track_index| TimelineTrackRef { track_index })
}

pub(super) fn track_header_rect(
    header: Rect,
    body: Rect,
    track_height: f32,
    scroll_y: f32,
    track_index: usize,
) -> Rect {
    Rect::new(
        header.x,
        track_y(body, track_height, scroll_y, track_index),
        header.width,
        track_height.max(0.0),
    )
}

pub(super) fn track_control_rect(header: Rect, control: TimelineTrackControl) -> Rect {
    let group_width = TRACK_CONTROL_SIZE * 3.0 + TRACK_CONTROL_GAP * 2.0;
    let start_x = header.x + header.width - TRACK_CONTROL_RIGHT_PADDING - group_width;
    let y = header.y + (header.height - TRACK_CONTROL_SIZE) * 0.5;
    let index = match control {
        TimelineTrackControl::Visibility => 0.0,
        TimelineTrackControl::Mute => 1.0,
        TimelineTrackControl::Lock => 2.0,
    };
    Rect::new(
        start_x + index * (TRACK_CONTROL_SIZE + TRACK_CONTROL_GAP),
        y,
        TRACK_CONTROL_SIZE,
        TRACK_CONTROL_SIZE,
    )
}

pub(super) fn track_control_at(
    header_rect: Rect,
    body_rect: Rect,
    track_height: f32,
    scroll_y: f32,
    track_count: usize,
    point: Point,
) -> Option<(TimelineTrackRef, TimelineTrackControl)> {
    let track_ref = track_header_at(
        header_rect,
        body_rect,
        track_height,
        scroll_y,
        track_count,
        point,
    )?;
    let header = track_header_rect(
        header_rect,
        body_rect,
        track_height,
        scroll_y,
        track_ref.track_index,
    );
    [
        TimelineTrackControl::Visibility,
        TimelineTrackControl::Mute,
        TimelineTrackControl::Lock,
    ]
    .into_iter()
    .find(|control| track_control_rect(header, *control).contains(point))
    .map(|control| (track_ref, control))
}

pub(super) fn clip_rect(
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
    let y = track_y(body, track_height, scroll_y, track_index) + CLIP_VERTICAL_INSET;
    let width = if pixels_per_frame <= 0.0 || !pixels_per_frame.is_finite() {
        CLIP_MIN_WIDTH
    } else {
        (duration_frames.max(1) as f32 * pixels_per_frame).max(CLIP_MIN_WIDTH)
    };
    Rect::new(
        x,
        y,
        width,
        (track_height.max(0.0) - CLIP_VERTICAL_INSET * 2.0).max(0.0),
    )
}

pub(super) fn hit_clip_edge(rect: Rect, point: Point) -> Option<TimelineTrimEdge> {
    if !rect.contains(point) {
        return None;
    }
    let edge_width =
        CLIP_EDGE_MAX_WIDTH.min((rect.width * CLIP_EDGE_WIDTH_RATIO).max(CLIP_EDGE_MIN_WIDTH));
    if point.x <= rect.x + edge_width {
        Some(TimelineTrimEdge::In)
    } else if point.x >= rect.x + rect.width - edge_width {
        Some(TimelineTrimEdge::Out)
    } else {
        None
    }
}

pub(super) fn clip_drag_proposed_start_frame(
    pointer_frame: i64,
    pointer_offset_frames: i64,
) -> i64 {
    pointer_frame.saturating_sub(pointer_offset_frames).max(0)
}

pub(super) fn clip_drag_position(
    pointer_frame: i64,
    pointer_offset_frames: i64,
    current_start_frame: i64,
    current_track_index: usize,
    target_track_index: usize,
    snap_frame: Option<i64>,
) -> TimelineClipDragPosition {
    let proposed_start_frame = clip_drag_proposed_start_frame(pointer_frame, pointer_offset_frames);
    let start_frame = snap_frame.map_or(proposed_start_frame, |frame| frame.max(0));
    TimelineClipDragPosition {
        start_frame,
        track_index: target_track_index,
        changed: current_start_frame != start_frame || current_track_index != target_track_index,
    }
}

pub(super) fn trim_drag_position(
    edge: TimelineTrimEdge,
    pointer_frame: i64,
    snap_frame: Option<i64>,
    old_start_frame: i64,
    old_duration_frames: i64,
    current_start_frame: i64,
    current_duration_frames: i64,
) -> TimelineTrimDragPosition {
    let pointer_frame = snap_frame.unwrap_or(pointer_frame);
    let old_duration_frames = old_duration_frames.max(1);
    let old_end = old_start_frame.saturating_add(old_duration_frames);
    let (start_frame, duration_frames) = match edge {
        TimelineTrimEdge::In => {
            let start_frame = pointer_frame.clamp(0, old_end.saturating_sub(1).max(0));
            (start_frame, old_end.saturating_sub(start_frame).max(1))
        }
        TimelineTrimEdge::Out => {
            let end_frame = pointer_frame.max(old_start_frame.saturating_add(1));
            (
                old_start_frame,
                end_frame.saturating_sub(old_start_frame).max(1),
            )
        }
    };
    TimelineTrimDragPosition {
        start_frame,
        duration_frames,
        changed: current_start_frame != start_frame || current_duration_frames != duration_frames,
    }
}

pub(super) fn scrollbar_scroll_for_thumb_delta(
    track_extent: f32,
    thumb_extent: f32,
    max_scroll: f32,
    start_scroll: f32,
    delta: f32,
) -> f32 {
    if !track_extent.is_finite()
        || !thumb_extent.is_finite()
        || !max_scroll.is_finite()
        || !start_scroll.is_finite()
        || !delta.is_finite()
    {
        return finite_or(start_scroll, 0.0).max(0.0);
    }
    let travel = (track_extent - thumb_extent).max(1.0);
    start_scroll + (delta / travel) * max_scroll.max(0.0)
}

pub(super) fn horizontal_zoom_for_handle_delta(
    body_width: f32,
    track_width: f32,
    content_frames: f32,
    delta_x: f32,
    kind: TimelineScrollbarDragKind,
    start_scroll: f32,
    start_pixels_per_frame: f32,
) -> Option<TimelineHorizontalZoomUpdate> {
    if body_width <= 1.0 || !body_width.is_finite() {
        return None;
    }
    let start_pixels = finite_or(start_pixels_per_frame, TIMELINE_MIN_PIXELS_PER_FRAME)
        .clamp(TIMELINE_MIN_PIXELS_PER_FRAME, TIMELINE_MAX_PIXELS_PER_FRAME);
    let content_frames = finite_or(content_frames, 0.0);
    let delta_x = finite_or(delta_x, 0.0);
    let start_scroll = finite_or(start_scroll, 0.0);
    let track_width = finite_or(track_width, 1.0);
    let total_frames =
        (content_frames.max(0.0) + TIMELINE_CONTENT_TRAILING_PADDING / start_pixels).max(1.0);
    let start_left = (start_scroll.max(0.0) / start_pixels).clamp(0.0, total_frames);
    let start_visible = (body_width / start_pixels).max(1.0);
    let start_right = (start_left + start_visible).clamp(start_left, total_frames);
    let delta_frames = delta_x / track_width.max(1.0) * total_frames;
    let min_visible_frames = (body_width / TIMELINE_MAX_PIXELS_PER_FRAME).max(1.0);
    let max_visible_frames = (body_width / TIMELINE_MIN_PIXELS_PER_FRAME).max(1.0);
    let (left_frame, proposed_visible_frames) = match kind {
        TimelineScrollbarDragKind::LeadingHandle => {
            let left = clamp_unordered(
                start_left + delta_frames,
                0.0,
                start_right - min_visible_frames,
            );
            (left, start_right - left)
        }
        TimelineScrollbarDragKind::TrailingHandle => {
            let right = clamp_unordered(
                start_right + delta_frames,
                start_left + min_visible_frames,
                total_frames,
            );
            (start_left, right - start_left)
        }
        TimelineScrollbarDragKind::Thumb => return None,
    };
    let visible_frames = proposed_visible_frames.clamp(min_visible_frames, max_visible_frames);
    let pixels_per_frame = (body_width / visible_frames)
        .clamp(TIMELINE_MIN_PIXELS_PER_FRAME, TIMELINE_MAX_PIXELS_PER_FRAME);
    Some(TimelineHorizontalZoomUpdate {
        pixels_per_frame,
        scroll_x: left_frame.max(0.0) * pixels_per_frame,
    })
}

pub(super) fn track_resize_for_handle_delta(
    body_height: f32,
    track_height_extent: f32,
    track_count: usize,
    delta_y: f32,
    kind: TimelineScrollbarDragKind,
    start_scroll: f32,
    start_track_height: f32,
) -> Option<TimelineTrackResizeUpdate> {
    if body_height <= 1.0 || !body_height.is_finite() {
        return None;
    }
    let start_height = finite_or(start_track_height, TIMELINE_MIN_TRACK_HEIGHT)
        .clamp(TIMELINE_MIN_TRACK_HEIGHT, TIMELINE_MAX_TRACK_HEIGHT);
    let start_scroll = finite_or(start_scroll, 0.0);
    let track_height_extent = finite_or(track_height_extent, 1.0);
    let delta_y = finite_or(delta_y, 0.0);
    let total_rows = (track_count as f32).max(1.0);
    let start_top = (start_scroll.max(0.0) / start_height).clamp(0.0, total_rows);
    let start_visible = (body_height / start_height).max(1.0);
    let start_bottom = (start_top + start_visible).clamp(start_top, total_rows);
    let delta_rows = delta_y / track_height_extent.max(1.0) * total_rows;
    let min_visible_rows = (body_height / TIMELINE_MAX_TRACK_HEIGHT).max(1.0);
    let max_visible_rows = (body_height / TIMELINE_MIN_TRACK_HEIGHT).max(1.0);
    let (top_row, proposed_visible_rows) = match kind {
        TimelineScrollbarDragKind::LeadingHandle => {
            let top = clamp_unordered(start_top + delta_rows, 0.0, start_bottom - min_visible_rows);
            (top, start_bottom - top)
        }
        TimelineScrollbarDragKind::TrailingHandle => {
            let bottom = clamp_unordered(
                start_bottom + delta_rows,
                start_top + min_visible_rows,
                total_rows,
            );
            (start_top, bottom - start_top)
        }
        TimelineScrollbarDragKind::Thumb => return None,
    };
    let visible_rows = proposed_visible_rows.clamp(min_visible_rows, max_visible_rows);
    let track_height =
        (body_height / visible_rows).clamp(TIMELINE_MIN_TRACK_HEIGHT, TIMELINE_MAX_TRACK_HEIGHT);
    Some(TimelineTrackResizeUpdate {
        track_height,
        scroll_y: top_row.max(0.0) * track_height,
    })
}

fn clamp_unordered(value: f32, min: f32, max: f32) -> f32 {
    value.clamp(min.min(max), min.max(max))
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_rect_eq(rect: Rect, expected: Rect) {
        assert!(
            (rect.x - expected.x).abs() < 0.001,
            "{rect:?} != {expected:?}"
        );
        assert!(
            (rect.y - expected.y).abs() < 0.001,
            "{rect:?} != {expected:?}"
        );
        assert!(
            (rect.width - expected.width).abs() < 0.001,
            "{rect:?} != {expected:?}"
        );
        assert!(
            (rect.height - expected.height).abs() < 0.001,
            "{rect:?} != {expected:?}"
        );
    }

    #[test]
    fn layout_rects_reserve_toolbar_ruler_and_scrollbar_gutters() {
        let rects = layout_rects(Rect::new(10.0, 20.0, 400.0, 240.0), 100.0, 24.0);

        assert_rect_eq(rects.toolbar, Rect::new(10.0, 20.0, 400.0, 34.0));
        assert_rect_eq(rects.ruler, Rect::new(110.0, 54.0, 288.0, 24.0));
        assert_rect_eq(rects.header, Rect::new(10.0, 78.0, 100.0, 170.0));
        assert_rect_eq(rects.body, Rect::new(110.0, 78.0, 288.0, 170.0));
    }

    #[test]
    fn layout_rects_collapse_without_negative_viewports() {
        let rects = layout_rects(Rect::new(0.0, 0.0, 40.0, 20.0), 100.0, 24.0);

        assert_eq!(rects.header.width, 40.0);
        assert_eq!(rects.ruler.width, 0.0);
        assert_eq!(rects.body.width, 0.0);
        assert_eq!(rects.body.height, 0.0);
    }

    #[test]
    fn frame_conversion_rounds_and_clamps_to_timeline_origin() {
        let body = Rect::new(100.0, 0.0, 300.0, 100.0);

        assert_eq!(frame_to_x(body, 2.0, 14.0, 20), 126.0);
        assert_eq!(x_to_frame(body, 2.0, 14.0, 126.9), 20);
        assert_eq!(frame_to_x(body, 2.0, 0.0, -5), 100.0);
        assert_eq!(x_to_frame(body, 2.0, 0.0, 90.0), 0);
        assert_eq!(x_to_frame(body, 0.0, 0.0, 126.0), 0);
    }

    #[test]
    fn track_hit_testing_respects_body_header_and_scroll() {
        let body = Rect::new(100.0, 50.0, 300.0, 90.0);
        let header = Rect::new(10.0, 50.0, 90.0, 90.0);

        assert_eq!(
            track_index_at(body, 30.0, 15.0, 4, Point::new(120.0, 66.0)),
            Some(1)
        );
        assert_eq!(
            track_header_at(header, body, 30.0, 15.0, 4, Point::new(20.0, 66.0)),
            Some(TimelineTrackRef { track_index: 1 })
        );
        assert_eq!(
            track_index_at(body, 30.0, 15.0, 4, Point::new(90.0, 66.0)),
            None
        );
        assert_eq!(
            track_header_at(header, body, 30.0, 15.0, 4, Point::new(120.0, 66.0)),
            None
        );
    }

    #[test]
    fn track_control_hit_testing_uses_stable_right_aligned_slots() {
        let body = Rect::new(100.0, 50.0, 300.0, 120.0);
        let header = Rect::new(0.0, 50.0, 120.0, 120.0);
        let row = track_header_rect(header, body, 40.0, 20.0, 1);
        let visibility = track_control_rect(row, TimelineTrackControl::Visibility);
        let mute = track_control_rect(row, TimelineTrackControl::Mute);
        let lock = track_control_rect(row, TimelineTrackControl::Lock);

        assert!(visibility.x < mute.x && mute.x < lock.x);
        assert_eq!(
            track_control_at(header, body, 40.0, 20.0, 4, lock.center()),
            Some((
                TimelineTrackRef { track_index: 1 },
                TimelineTrackControl::Lock
            ))
        );
    }

    #[test]
    fn clip_rect_clamps_duration_width_and_height() {
        let body = Rect::new(100.0, 50.0, 300.0, 100.0);

        assert_rect_eq(
            clip_rect(body, 2.0, 6.0, 30.0, 10.0, 2, 20, 5),
            Rect::new(134.0, 104.0, 10.0, 22.0),
        );
        assert_eq!(clip_rect(body, 0.25, 0.0, 6.0, 0.0, 0, 0, 1).height, 0.0);
        assert_eq!(clip_rect(body, 0.25, 0.0, 30.0, 0.0, 0, 0, 1).width, 8.0);
    }

    #[test]
    fn clip_edge_hit_testing_respects_inside_and_edge_width() {
        let rect = Rect::new(10.0, 10.0, 100.0, 24.0);

        assert_eq!(
            hit_clip_edge(rect, Point::new(12.0, 20.0)),
            Some(TimelineTrimEdge::In)
        );
        assert_eq!(
            hit_clip_edge(rect, Point::new(108.0, 20.0)),
            Some(TimelineTrimEdge::Out)
        );
        assert_eq!(hit_clip_edge(rect, Point::new(50.0, 20.0)), None);
        assert_eq!(hit_clip_edge(rect, Point::new(12.0, 40.0)), None);
    }

    #[test]
    fn clip_drag_position_clamps_start_and_reports_changes() {
        assert_eq!(clip_drag_proposed_start_frame(12, 20), 0);

        assert_eq!(
            clip_drag_position(50, 8, 40, 1, 2, None),
            TimelineClipDragPosition { start_frame: 42, track_index: 2, changed: true }
        );
        assert_eq!(
            clip_drag_position(50, 8, 12, 0, 0, Some(-10)),
            TimelineClipDragPosition { start_frame: 0, track_index: 0, changed: true }
        );
        assert_eq!(
            clip_drag_position(50, 8, 42, 2, 2, None),
            TimelineClipDragPosition { start_frame: 42, track_index: 2, changed: false }
        );
    }

    #[test]
    fn trim_drag_position_keeps_valid_one_frame_minimums() {
        assert_eq!(
            trim_drag_position(TimelineTrimEdge::In, 80, None, 30, 40, 30, 40),
            TimelineTrimDragPosition { start_frame: 69, duration_frames: 1, changed: true }
        );
        assert_eq!(
            trim_drag_position(TimelineTrimEdge::In, -10, None, 30, 40, 30, 40),
            TimelineTrimDragPosition { start_frame: 0, duration_frames: 70, changed: true }
        );
        assert_eq!(
            trim_drag_position(TimelineTrimEdge::Out, 20, None, 30, 40, 30, 40),
            TimelineTrimDragPosition { start_frame: 30, duration_frames: 1, changed: true }
        );
        assert_eq!(
            trim_drag_position(TimelineTrimEdge::Out, 44, Some(70), 30, 40, 30, 40),
            TimelineTrimDragPosition {
                start_frame: 30,
                duration_frames: 40,
                changed: false
            }
        );
        assert_eq!(
            trim_drag_position(TimelineTrimEdge::In, 0, None, -10, 1, -10, 1),
            TimelineTrimDragPosition { start_frame: 0, duration_frames: 1, changed: true }
        );
    }

    #[test]
    fn scrollbar_thumb_delta_maps_track_travel_to_scroll_range() {
        assert_eq!(
            scrollbar_scroll_for_thumb_delta(100.0, 40.0, 300.0, 20.0, 30.0),
            170.0
        );
        assert_eq!(
            scrollbar_scroll_for_thumb_delta(40.0, 40.0, 300.0, 20.0, 30.0),
            9020.0
        );
        assert_eq!(
            scrollbar_scroll_for_thumb_delta(100.0, 40.0, -300.0, 20.0, 30.0),
            20.0
        );
        assert_eq!(
            scrollbar_scroll_for_thumb_delta(f32::NAN, 40.0, 300.0, 20.0, 30.0),
            20.0
        );
        assert_eq!(
            scrollbar_scroll_for_thumb_delta(100.0, 40.0, 300.0, f32::NAN, 30.0),
            0.0
        );
    }

    #[test]
    fn horizontal_zoom_for_handle_delta_updates_zoom_and_scroll_without_panicking_on_tight_ranges()
    {
        let trailing = horizontal_zoom_for_handle_delta(
            300.0,
            288.0,
            240.0,
            50.0,
            TimelineScrollbarDragKind::TrailingHandle,
            0.0,
            2.0,
        )
        .unwrap();
        assert!(trailing.pixels_per_frame < 2.0);
        assert_eq!(trailing.scroll_x, 0.0);

        let leading = horizontal_zoom_for_handle_delta(
            300.0,
            288.0,
            240.0,
            50.0,
            TimelineScrollbarDragKind::LeadingHandle,
            100.0,
            2.0,
        )
        .unwrap();
        assert!(leading.pixels_per_frame > 2.0);
        assert!(leading.scroll_x > 100.0);

        assert_eq!(
            horizontal_zoom_for_handle_delta(
                300.0,
                288.0,
                240.0,
                50.0,
                TimelineScrollbarDragKind::Thumb,
                0.0,
                2.0,
            ),
            None
        );
        assert!(horizontal_zoom_for_handle_delta(
            0.5,
            0.0,
            0.0,
            10_000.0,
            TimelineScrollbarDragKind::LeadingHandle,
            0.0,
            2.0,
        )
        .is_none());

        let recovered = horizontal_zoom_for_handle_delta(
            300.0,
            f32::NAN,
            f32::NAN,
            f32::NAN,
            TimelineScrollbarDragKind::TrailingHandle,
            f32::NAN,
            f32::NAN,
        )
        .unwrap();
        assert!(recovered.pixels_per_frame.is_finite());
        assert!(recovered.scroll_x.is_finite());
    }

    #[test]
    fn track_resize_for_handle_delta_updates_height_and_scroll() {
        let trailing = track_resize_for_handle_delta(
            170.0,
            158.0,
            8,
            50.0,
            TimelineScrollbarDragKind::TrailingHandle,
            0.0,
            40.0,
        )
        .unwrap();
        assert_eq!(trailing.track_height, 30.0);
        assert_eq!(trailing.scroll_y, 0.0);

        let leading = track_resize_for_handle_delta(
            170.0,
            158.0,
            8,
            30.0,
            TimelineScrollbarDragKind::LeadingHandle,
            80.0,
            40.0,
        )
        .unwrap();
        assert!(leading.track_height > 40.0);
        assert!(leading.scroll_y > 80.0);

        assert_eq!(
            track_resize_for_handle_delta(
                170.0,
                158.0,
                8,
                50.0,
                TimelineScrollbarDragKind::Thumb,
                0.0,
                40.0,
            ),
            None
        );

        let recovered = track_resize_for_handle_delta(
            170.0,
            f32::NAN,
            8,
            f32::NAN,
            TimelineScrollbarDragKind::TrailingHandle,
            f32::NAN,
            f32::NAN,
        )
        .unwrap();
        assert!(recovered.track_height.is_finite());
        assert!(recovered.scroll_y.is_finite());
    }

    #[test]
    fn scrollbar_tracks_and_clip_rect_follow_body_geometry() {
        let body = Rect::new(10.0, 20.0, 100.0, 80.0);

        assert_rect_eq(
            horizontal_scrollbar_track_rect(body).unwrap(),
            Rect::new(16.0, 100.0, 88.0, 12.0),
        );
        assert_rect_eq(
            vertical_scrollbar_track_rect(body).unwrap(),
            Rect::new(110.0, 26.0, 12.0, 68.0),
        );
        assert_rect_eq(
            scrollbar_clip_rect(body),
            Rect::new(10.0, 20.0, 112.0, 92.0),
        );
        assert_eq!(
            horizontal_scrollbar_track_rect(Rect::new(0.0, 0.0, 28.0, 80.0)),
            None
        );
    }
}
