use mondrian_core::types::{Rational, TimeCode};
use mondrian_ui_core::types::{KeyCode, Modifiers, Point, Rect};

use super::{
    TimelineClipRef, TimelineEditCommand, TimelineInOutPoint, TimelineScrollbarDragKind,
    TimelineTrackControl, TimelineTrackKind, TimelineTrackRef, TimelineTrimEdge,
    SCROLLBAR_HANDLE_SIZE, SCROLLBAR_MIN_THUMB, SCROLLBAR_THICKNESS,
    TIMELINE_CONTENT_TRAILING_PADDING, TIMELINE_IN_OUT_MARKER_HIT_RADIUS,
    TIMELINE_MAX_PIXELS_PER_FRAME, TIMELINE_MAX_TRACK_HEIGHT, TIMELINE_MIN_PIXELS_PER_FRAME,
    TIMELINE_MIN_TRACK_HEIGHT, TIMELINE_SCROLLBAR_GUTTER, TIMELINE_TOOLBAR_HEIGHT,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TimelineTrackDragPosition {
    pub track_index: usize,
    pub changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TimelineInOutDragUpdate {
    pub in_point_frame: i64,
    pub out_point_frame: Option<i64>,
    pub current_frame: i64,
    pub changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TimelineAssetDropTarget {
    pub track_index: usize,
    pub frame: i64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TimelineEditCommandContext {
    pub selected_clip: Option<TimelineClipRef>,
    pub selected_track: Option<TimelineTrackRef>,
    pub in_point_frame: i64,
    pub has_out_point: bool,
    pub clip_intersects_playhead: bool,
    pub selected_clip_is_nested: bool,
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

pub(super) fn asset_drop_target_at(
    body: Rect,
    track_height: f32,
    scroll_y: f32,
    track_count: usize,
    pixels_per_frame: f32,
    scroll_x: f32,
    point: Point,
) -> Option<TimelineAssetDropTarget> {
    let track_index = track_index_at(body, track_height, scroll_y, track_count, point)?;
    Some(TimelineAssetDropTarget {
        track_index,
        frame: x_to_frame(body, pixels_per_frame, scroll_x, point.x),
    })
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

pub(super) fn in_out_marker_at(
    ruler: Rect,
    body: Rect,
    pixels_per_frame: f32,
    scroll_x: f32,
    in_point_frame: i64,
    out_point_frame: Option<i64>,
    point: Point,
) -> Option<TimelineInOutPoint> {
    if !ruler.contains(point) {
        return None;
    }
    if (point.x - frame_to_x(body, pixels_per_frame, scroll_x, in_point_frame)).abs()
        <= TIMELINE_IN_OUT_MARKER_HIT_RADIUS
    {
        return Some(TimelineInOutPoint::In);
    }
    let out = out_point_frame?;
    if (point.x - frame_to_x(body, pixels_per_frame, scroll_x, out.saturating_add(1))).abs()
        <= TIMELINE_IN_OUT_MARKER_HIT_RADIUS
    {
        return Some(TimelineInOutPoint::Out);
    }
    None
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

pub(super) fn in_out_visible_range(
    body: Rect,
    pixels_per_frame: f32,
    scroll_x: f32,
    in_point_frame: i64,
    out_point_frame: Option<i64>,
) -> Option<(f32, f32)> {
    let out = out_point_frame?;
    if out < in_point_frame {
        return None;
    }
    let start = frame_to_x(body, pixels_per_frame, scroll_x, in_point_frame);
    let end = frame_to_x(body, pixels_per_frame, scroll_x, out.saturating_add(1));
    let x0 = start.max(body.x);
    let x1 = end.min(body.x + body.width);
    (x1 > x0).then_some((x0, x1))
}

pub(super) fn in_out_marker_x(
    body: Rect,
    pixels_per_frame: f32,
    scroll_x: f32,
    point: TimelineInOutPoint,
    in_point_frame: i64,
    out_point_frame: Option<i64>,
) -> Option<f32> {
    let frame = match point {
        TimelineInOutPoint::In => in_point_frame,
        TimelineInOutPoint::Out => out_point_frame?.saturating_add(1),
    };
    Some(frame_to_x(body, pixels_per_frame, scroll_x, frame))
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

pub(super) fn track_drag_position(
    source_track_index: usize,
    current_track_index: usize,
    proposed_track_index: Option<usize>,
    source_kind: Option<TimelineTrackKind>,
    proposed_kind: Option<TimelineTrackKind>,
) -> TimelineTrackDragPosition {
    let track_index = match proposed_track_index {
        None => current_track_index,
        Some(target_track_index) if source_kind.is_some() && source_kind == proposed_kind => {
            target_track_index
        }
        Some(_) => source_track_index,
    };
    TimelineTrackDragPosition {
        track_index,
        changed: current_track_index != track_index,
    }
}

pub(super) fn in_out_drag_update(
    point: TimelineInOutPoint,
    pointer_frame: i64,
    current_drag_frame: i64,
    in_point_frame: i64,
    out_point_frame: Option<i64>,
) -> TimelineInOutDragUpdate {
    let frame = pointer_frame.max(0);
    let (in_point_frame, out_point_frame, current_frame) = match point {
        TimelineInOutPoint::In => {
            let out_point_frame = if out_point_frame.is_some_and(|out| out < frame) {
                Some(frame)
            } else {
                out_point_frame
            };
            (frame, out_point_frame, frame)
        }
        TimelineInOutPoint::Out => {
            let frame = frame.max(in_point_frame);
            (in_point_frame, Some(frame), frame)
        }
    };
    TimelineInOutDragUpdate {
        in_point_frame,
        out_point_frame,
        current_frame,
        changed: current_drag_frame != current_frame,
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

pub(super) fn ruler_fps(frame_rate: Rational) -> i64 {
    frame_rate.to_f64().round().max(1.0) as i64
}

pub(super) fn frame_time_base(frame_rate: Rational) -> Rational {
    Rational::new(frame_rate.den, frame_rate.num)
}

pub(super) fn pick_ruler_step_frames(
    pixels_per_frame: f32,
    frame_rate: Rational,
    target_px: f32,
    min_step: i64,
) -> i64 {
    let pixels_per_frame = finite_or(pixels_per_frame, TIMELINE_MIN_PIXELS_PER_FRAME)
        .max(TIMELINE_MIN_PIXELS_PER_FRAME);
    let target_px = finite_or(target_px, 0.0).max(0.0);
    let min_step = min_step.max(1);
    let raw = (target_px / pixels_per_frame).max(min_step as f32);
    let fps = ruler_fps(frame_rate).max(1);
    for step in [
        1,
        2,
        5,
        10,
        15,
        fps,
        fps * 2,
        fps * 5,
        fps * 10,
        fps * 15,
        fps * 30,
        fps * 60,
        fps * 120,
        fps * 240,
        fps * 300,
        fps * 600,
        fps * 900,
        fps * 1800,
        fps * 3600,
    ] {
        if step < min_step || step % min_step != 0 {
            continue;
        }
        if raw <= step as f32 {
            return step;
        }
    }
    let fallback = fps * 3600;
    if fallback >= min_step && fallback % min_step == 0 {
        fallback
    } else {
        min_step
    }
}

pub(super) fn tick_step_frames(pixels_per_frame: f32, frame_rate: Rational) -> i64 {
    pick_ruler_step_frames(pixels_per_frame, frame_rate, 20.0, 1)
}

pub(super) fn major_tick_step_frames(
    pixels_per_frame: f32,
    frame_rate: Rational,
    minor_step: i64,
) -> i64 {
    pick_ruler_step_frames(pixels_per_frame, frame_rate, 96.0, minor_step.max(1))
}

pub(super) fn ruler_label_for_frame(frame: i64, major_step: i64, frame_rate: Rational) -> String {
    let frame = frame.max(0);
    let fps = ruler_fps(frame_rate).max(1);
    let smpte = TimeCode::new(frame, frame_time_base(frame_rate)).to_smpte();
    let total_seconds = (frame as f64 / frame_rate.to_f64()).floor().max(0.0) as i64;
    let hours = total_seconds / 3600;
    let parts = smpte.split(':').collect::<Vec<_>>();

    if major_step < fps * 2 {
        smpte
    } else if hours > 0 || major_step >= fps * 60 * 10 {
        format!("{}:{}:{}", parts[0], parts[1], parts[2])
    } else {
        format!("{}:{}", parts[1], parts[2])
    }
}

pub(super) fn edit_command_has_local_target(
    command: TimelineEditCommand,
    context: TimelineEditCommandContext,
) -> bool {
    match command {
        TimelineEditCommand::PasteAtPlayhead
        | TimelineEditCommand::MarkInAtPlayhead
        | TimelineEditCommand::MarkOutAtPlayhead
        | TimelineEditCommand::TogglePlayback => true,
        TimelineEditCommand::ClearInOutPoints => {
            context.in_point_frame > 0 || context.has_out_point
        }
        TimelineEditCommand::SplitAtPlayhead => context.clip_intersects_playhead,
        TimelineEditCommand::DeleteSelection | TimelineEditCommand::RippleDeleteSelection => {
            context.selected_clip.is_some() || context.selected_track.is_some()
        }
        TimelineEditCommand::CutSelection
        | TimelineEditCommand::CopySelection
        | TimelineEditCommand::DuplicateSelection
        | TimelineEditCommand::TrimSelectionInToPlayhead
        | TimelineEditCommand::TrimSelectionOutToPlayhead
        | TimelineEditCommand::RollSelectedCutToPlayhead
        | TimelineEditCommand::EnableSelection
        | TimelineEditCommand::DisableSelection => context.selected_clip.is_some(),
        TimelineEditCommand::OpenNestedSequence(clip_ref) => {
            context.selected_clip == Some(clip_ref) && context.selected_clip_is_nested
        }
    }
}

pub(super) fn keyboard_edit_command(
    key: KeyCode,
    modifiers: Modifiers,
) -> Option<TimelineEditCommand> {
    if modifiers.alt {
        return None;
    }
    match key {
        KeyCode::X if !modifiers.shift && (modifiers.ctrl || modifiers.meta) => {
            Some(TimelineEditCommand::CutSelection)
        }
        KeyCode::C if !modifiers.shift && (modifiers.ctrl || modifiers.meta) => {
            Some(TimelineEditCommand::CopySelection)
        }
        KeyCode::V if !modifiers.shift && (modifiers.ctrl || modifiers.meta) => {
            Some(TimelineEditCommand::PasteAtPlayhead)
        }
        KeyCode::D if !modifiers.shift && (modifiers.ctrl || modifiers.meta) => {
            Some(TimelineEditCommand::DuplicateSelection)
        }
        KeyCode::K | KeyCode::B if !modifiers.shift && (modifiers.ctrl || modifiers.meta) => {
            Some(TimelineEditCommand::SplitAtPlayhead)
        }
        KeyCode::I if !modifiers.ctrl && !modifiers.meta && !modifiers.shift => {
            Some(TimelineEditCommand::MarkInAtPlayhead)
        }
        KeyCode::O if !modifiers.ctrl && !modifiers.meta && !modifiers.shift => {
            Some(TimelineEditCommand::MarkOutAtPlayhead)
        }
        KeyCode::Space if !modifiers.ctrl && !modifiers.meta && !modifiers.shift => {
            Some(TimelineEditCommand::TogglePlayback)
        }
        KeyCode::Delete | KeyCode::Backspace if !modifiers.ctrl && !modifiers.meta => {
            if modifiers.shift {
                Some(TimelineEditCommand::RippleDeleteSelection)
            } else {
                Some(TimelineEditCommand::DeleteSelection)
            }
        }
        _ => None,
    }
}

pub(super) fn keyboard_seek_frame(
    key: KeyCode,
    modifiers: Modifiers,
    playhead_frame: i64,
    max_content_frame: i64,
) -> Option<i64> {
    if modifiers.ctrl || modifiers.meta || modifiers.alt {
        return None;
    }
    let step = if modifiers.shift { 10 } else { 1 };
    match key {
        KeyCode::Left => Some(playhead_frame.saturating_sub(step).max(0)),
        KeyCode::Right => Some(playhead_frame.saturating_add(step)),
        KeyCode::Home => Some(0),
        KeyCode::End => Some(max_content_frame.max(0)),
        _ => None,
    }
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
    fn asset_drop_target_uses_body_track_hit_and_frame_mapping() {
        let body = Rect::new(100.0, 50.0, 300.0, 90.0);

        assert_eq!(
            asset_drop_target_at(body, 30.0, 15.0, 4, 2.0, 14.0, Point::new(126.0, 66.0)),
            Some(TimelineAssetDropTarget { track_index: 1, frame: 20 })
        );
        assert_eq!(
            asset_drop_target_at(body, 30.0, 0.0, 4, 2.0, 0.0, Point::new(90.0, 66.0)),
            None
        );
        assert_eq!(
            asset_drop_target_at(body, 30.0, 0.0, 4, 2.0, 0.0, Point::new(126.0, 200.0)),
            None
        );
        assert_eq!(
            asset_drop_target_at(body, 30.0, 0.0, 4, 0.0, 0.0, Point::new(126.0, 60.0)),
            Some(TimelineAssetDropTarget { track_index: 0, frame: 0 })
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
    fn in_out_marker_hit_testing_uses_ruler_bounds_and_out_end_frame() {
        let body = Rect::new(100.0, 50.0, 300.0, 100.0);
        let ruler = Rect::new(100.0, 20.0, 300.0, 28.0);

        assert_eq!(
            in_out_marker_at(ruler, body, 2.0, 0.0, 10, Some(20), Point::new(120.0, 24.0)),
            Some(TimelineInOutPoint::In)
        );
        assert_eq!(
            in_out_marker_at(ruler, body, 2.0, 0.0, 10, Some(20), Point::new(142.0, 24.0)),
            Some(TimelineInOutPoint::Out)
        );
        assert_eq!(
            in_out_marker_at(ruler, body, 2.0, 0.0, 10, Some(20), Point::new(142.0, 60.0)),
            None
        );
        assert_eq!(
            in_out_marker_at(ruler, body, 2.0, 0.0, 10, None, Point::new(142.0, 24.0)),
            None
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
    fn track_drag_position_only_moves_between_compatible_track_kinds() {
        assert_eq!(
            track_drag_position(
                1,
                1,
                Some(3),
                Some(TimelineTrackKind::Video),
                Some(TimelineTrackKind::Video),
            ),
            TimelineTrackDragPosition { track_index: 3, changed: true }
        );
        assert_eq!(
            track_drag_position(
                1,
                3,
                Some(2),
                Some(TimelineTrackKind::Video),
                Some(TimelineTrackKind::Audio),
            ),
            TimelineTrackDragPosition { track_index: 1, changed: true }
        );
        assert_eq!(
            track_drag_position(
                1,
                3,
                None,
                Some(TimelineTrackKind::Video),
                Some(TimelineTrackKind::Video),
            ),
            TimelineTrackDragPosition { track_index: 3, changed: false }
        );
        assert_eq!(
            track_drag_position(1, 1, Some(3), None, Some(TimelineTrackKind::Video)),
            TimelineTrackDragPosition { track_index: 1, changed: false }
        );
    }

    #[test]
    fn in_out_drag_update_clamps_frames_and_keeps_ordered_range() {
        assert_eq!(
            in_out_drag_update(TimelineInOutPoint::In, -5, 10, 10, Some(20)),
            TimelineInOutDragUpdate {
                in_point_frame: 0,
                out_point_frame: Some(20),
                current_frame: 0,
                changed: true
            }
        );
        assert_eq!(
            in_out_drag_update(TimelineInOutPoint::In, 30, 10, 10, Some(20)),
            TimelineInOutDragUpdate {
                in_point_frame: 30,
                out_point_frame: Some(30),
                current_frame: 30,
                changed: true
            }
        );
        assert_eq!(
            in_out_drag_update(TimelineInOutPoint::Out, 8, 20, 12, Some(20)),
            TimelineInOutDragUpdate {
                in_point_frame: 12,
                out_point_frame: Some(12),
                current_frame: 12,
                changed: true
            }
        );
        assert_eq!(
            in_out_drag_update(TimelineInOutPoint::Out, 20, 20, 12, Some(20)),
            TimelineInOutDragUpdate {
                in_point_frame: 12,
                out_point_frame: Some(20),
                current_frame: 20,
                changed: false
            }
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
    fn in_out_visible_range_clips_to_body_viewport() {
        let body = Rect::new(100.0, 50.0, 120.0, 100.0);

        assert_eq!(in_out_visible_range(body, 2.0, 0.0, 10, None), None);
        assert_eq!(in_out_visible_range(body, 2.0, 0.0, 30, Some(20)), None);
        assert_eq!(
            in_out_visible_range(body, 2.0, 0.0, 10, Some(20)),
            Some((120.0, 142.0))
        );
        assert_eq!(
            in_out_visible_range(body, 2.0, 30.0, 10, Some(90)),
            Some((100.0, 220.0))
        );
        assert_eq!(in_out_visible_range(body, 2.0, 400.0, 10, Some(20)), None);
    }

    #[test]
    fn in_out_marker_x_maps_out_to_exclusive_end_frame() {
        let body = Rect::new(100.0, 50.0, 120.0, 100.0);

        assert_eq!(
            in_out_marker_x(body, 2.0, 0.0, TimelineInOutPoint::In, 10, Some(20)),
            Some(120.0)
        );
        assert_eq!(
            in_out_marker_x(body, 2.0, 0.0, TimelineInOutPoint::Out, 10, Some(20)),
            Some(142.0)
        );
        assert_eq!(
            in_out_marker_x(body, 2.0, 0.0, TimelineInOutPoint::Out, 10, None),
            None
        );
    }

    #[test]
    fn ruler_step_selection_uses_frame_rate_and_zoom_density() {
        assert_eq!(tick_step_frames(0.5, Rational::FPS_25), 50);
        assert_eq!(major_tick_step_frames(0.5, Rational::FPS_25, 50), 250);
        assert_eq!(pick_ruler_step_frames(100.0, Rational::FPS_25, 20.0, 1), 1);
        assert_eq!(
            pick_ruler_step_frames(f32::NAN, Rational::FPS_25, 20.0, 1),
            125
        );
        assert_eq!(pick_ruler_step_frames(0.5, Rational::FPS_25, 96.0, 7), 7);
    }

    #[test]
    fn ruler_labels_use_configured_frame_rate_and_density() {
        assert_eq!(ruler_fps(Rational::FPS_25), 25);
        assert_eq!(frame_time_base(Rational::FPS_25), Rational::new(1, 25));
        assert_eq!(
            ruler_label_for_frame(250, 25, Rational::FPS_25),
            "00:00:10:00"
        );
        assert_eq!(ruler_label_for_frame(250, 125, Rational::FPS_25), "00:10");
        assert_eq!(
            ruler_label_for_frame(25 * 60 * 60 + 250, 25 * 60 * 10, Rational::FPS_25),
            "01:00:10"
        );
        assert_eq!(
            ruler_label_for_frame(-10, 25, Rational::FPS_25),
            "00:00:00:00"
        );
    }

    #[test]
    fn edit_command_targeting_uses_selection_playhead_and_mark_context() {
        let clip_ref = TimelineClipRef { track_index: 1, clip_index: 2 };
        let track_ref = TimelineTrackRef { track_index: 1 };
        let empty = TimelineEditCommandContext {
            selected_clip: None,
            selected_track: None,
            in_point_frame: 0,
            has_out_point: false,
            clip_intersects_playhead: false,
            selected_clip_is_nested: false,
        };

        assert!(edit_command_has_local_target(
            TimelineEditCommand::PasteAtPlayhead,
            empty
        ));
        assert!(!edit_command_has_local_target(
            TimelineEditCommand::CutSelection,
            empty
        ));
        assert!(!edit_command_has_local_target(
            TimelineEditCommand::DeleteSelection,
            empty
        ));
        assert!(!edit_command_has_local_target(
            TimelineEditCommand::ClearInOutPoints,
            empty
        ));
        assert!(!edit_command_has_local_target(
            TimelineEditCommand::SplitAtPlayhead,
            empty
        ));

        let selected_track =
            TimelineEditCommandContext { selected_track: Some(track_ref), ..empty };
        assert!(edit_command_has_local_target(
            TimelineEditCommand::DeleteSelection,
            selected_track
        ));
        assert!(edit_command_has_local_target(
            TimelineEditCommand::RippleDeleteSelection,
            selected_track
        ));
        assert!(!edit_command_has_local_target(
            TimelineEditCommand::CopySelection,
            selected_track
        ));

        let selected_clip = TimelineEditCommandContext {
            selected_clip: Some(clip_ref),
            clip_intersects_playhead: true,
            in_point_frame: 12,
            ..empty
        };
        assert!(edit_command_has_local_target(
            TimelineEditCommand::CopySelection,
            selected_clip
        ));
        assert!(edit_command_has_local_target(
            TimelineEditCommand::SplitAtPlayhead,
            selected_clip
        ));
        assert!(edit_command_has_local_target(
            TimelineEditCommand::ClearInOutPoints,
            selected_clip
        ));
    }

    #[test]
    fn edit_command_targeting_requires_selected_nested_clip_for_open_nested_sequence() {
        let clip_ref = TimelineClipRef { track_index: 0, clip_index: 0 };
        let other_ref = TimelineClipRef { track_index: 0, clip_index: 1 };
        let context = TimelineEditCommandContext {
            selected_clip: Some(clip_ref),
            selected_track: None,
            in_point_frame: 0,
            has_out_point: false,
            clip_intersects_playhead: false,
            selected_clip_is_nested: true,
        };

        assert!(edit_command_has_local_target(
            TimelineEditCommand::OpenNestedSequence(clip_ref),
            context
        ));
        assert!(!edit_command_has_local_target(
            TimelineEditCommand::OpenNestedSequence(other_ref),
            context
        ));
        assert!(!edit_command_has_local_target(
            TimelineEditCommand::OpenNestedSequence(clip_ref),
            TimelineEditCommandContext { selected_clip_is_nested: false, ..context }
        ));
    }

    #[test]
    fn keyboard_edit_command_maps_owned_chords_and_leaves_alt_or_extra_shift_unmatched() {
        let ctrl = Modifiers { ctrl: true, ..Default::default() };
        let meta = Modifiers { meta: true, ..Default::default() };
        let alt_ctrl = Modifiers { ctrl: true, alt: true, ..Default::default() };
        let shift_ctrl = Modifiers { ctrl: true, shift: true, ..Default::default() };

        assert_eq!(
            keyboard_edit_command(KeyCode::X, ctrl),
            Some(TimelineEditCommand::CutSelection)
        );
        assert_eq!(
            keyboard_edit_command(KeyCode::C, meta),
            Some(TimelineEditCommand::CopySelection)
        );
        assert_eq!(
            keyboard_edit_command(KeyCode::V, ctrl),
            Some(TimelineEditCommand::PasteAtPlayhead)
        );
        assert_eq!(
            keyboard_edit_command(KeyCode::D, ctrl),
            Some(TimelineEditCommand::DuplicateSelection)
        );
        assert_eq!(
            keyboard_edit_command(KeyCode::K, ctrl),
            Some(TimelineEditCommand::SplitAtPlayhead)
        );
        assert_eq!(
            keyboard_edit_command(KeyCode::B, ctrl),
            Some(TimelineEditCommand::SplitAtPlayhead)
        );
        assert_eq!(
            keyboard_edit_command(KeyCode::I, Modifiers::none()),
            Some(TimelineEditCommand::MarkInAtPlayhead)
        );
        assert_eq!(
            keyboard_edit_command(KeyCode::O, Modifiers::none()),
            Some(TimelineEditCommand::MarkOutAtPlayhead)
        );
        assert_eq!(
            keyboard_edit_command(KeyCode::Space, Modifiers::none()),
            Some(TimelineEditCommand::TogglePlayback)
        );
        assert_eq!(
            keyboard_edit_command(KeyCode::Delete, Modifiers::none()),
            Some(TimelineEditCommand::DeleteSelection)
        );
        assert_eq!(
            keyboard_edit_command(KeyCode::Backspace, Modifiers::shift()),
            Some(TimelineEditCommand::RippleDeleteSelection)
        );
        assert_eq!(keyboard_edit_command(KeyCode::X, alt_ctrl), None);
        assert_eq!(keyboard_edit_command(KeyCode::X, shift_ctrl), None);
        assert_eq!(keyboard_edit_command(KeyCode::I, ctrl), None);
    }

    #[test]
    fn keyboard_seek_frame_maps_plain_navigation_and_ignores_system_chords() {
        assert_eq!(
            keyboard_seek_frame(KeyCode::Left, Modifiers::none(), 20, 100),
            Some(19)
        );
        assert_eq!(
            keyboard_seek_frame(KeyCode::Left, Modifiers::shift(), 5, 100),
            Some(0)
        );
        assert_eq!(
            keyboard_seek_frame(KeyCode::Right, Modifiers::shift(), i64::MAX - 5, 100),
            Some(i64::MAX)
        );
        assert_eq!(
            keyboard_seek_frame(KeyCode::Home, Modifiers::none(), 20, 100),
            Some(0)
        );
        assert_eq!(
            keyboard_seek_frame(KeyCode::End, Modifiers::none(), 20, 100),
            Some(100)
        );
        assert_eq!(
            keyboard_seek_frame(KeyCode::End, Modifiers::none(), 20, -100),
            Some(0)
        );
        assert_eq!(
            keyboard_seek_frame(KeyCode::Right, Modifiers::ctrl(), 20, 100),
            None
        );
        assert_eq!(
            keyboard_seek_frame(
                KeyCode::Right,
                Modifiers { alt: true, ..Default::default() },
                20,
                100,
            ),
            None
        );
        assert_eq!(
            keyboard_seek_frame(KeyCode::A, Modifiers::none(), 20, 100),
            None
        );
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
