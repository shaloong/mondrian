//! Ruler, snap, scrollbar, and clip lookup helpers.
use super::*;

pub(crate) fn find_clip_track_ref(
    seq: &Sequence,
    clip_id: ClipId,
) -> Option<(TrackId, bool, usize)> {
    for (index, track) in seq.video_tracks.iter().enumerate() {
        if track.clips.iter().any(|clip| clip.id == clip_id) {
            return Some((track.id, true, index));
        }
    }
    for (index, track) in seq.audio_tracks.iter().enumerate() {
        if track.clips.iter().any(|clip| clip.id == clip_id) {
            return Some((track.id, false, index));
        }
    }
    None
}

pub(crate) fn selected_nested_sequence_id(
    state: &AppState,
    selection: Option<SelectedClipRef>,
) -> Option<mondrian_core::types::SequenceId> {
    let selection = selection?;
    let seq = state.sequence.as_ref()?;
    let clip = if selection.is_video_track {
        seq.video_tracks
            .iter()
            .find_map(|track| track.clips.iter().find(|clip| clip.id == selection.clip_id))
    } else {
        seq.audio_tracks
            .iter()
            .find_map(|track| track.clips.iter().find(|clip| clip.id == selection.clip_id))
    }?;
    if clip.is_nested_sequence() {
        clip.nested_sequence_id
    } else {
        None
    }
}

pub(crate) fn draw_single_line_ellipsis(
    painter: &egui::Painter,
    rect: Rect,
    text: &str,
    font_id: egui::FontId,
    color: Color32,
) {
    if rect.width() <= 4.0 || text.is_empty() {
        return;
    }

    let fits = |candidate: &str| {
        painter.layout_no_wrap(candidate.to_owned(), font_id.clone(), color).size().x
            <= rect.width()
    };

    let final_text = if fits(text) {
        text.to_owned()
    } else {
        let ellipsis = "...";
        let chars: Vec<char> = text.chars().collect();
        let mut truncated = ellipsis.to_owned();

        for keep in (0..chars.len()).rev() {
            let candidate = format!("{}{}", chars[..keep].iter().collect::<String>(), ellipsis);
            if fits(&candidate) {
                truncated = candidate;
                break;
            }
        }

        truncated
    };

    painter.text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        final_text,
        font_id,
        color,
    );
}

pub(crate) fn find_clip_in_sequence(
    seq: &Sequence,
    clip_id: ClipId,
) -> Option<&mondrian_timeline::clip::Clip> {
    for track in seq.video_tracks.iter().chain(seq.audio_tracks.iter()) {
        if let Some(clip) = track.clips.iter().find(|clip| clip.id == clip_id) {
            return Some(clip);
        }
    }
    None
}

#[derive(Copy, Clone)]
pub(crate) enum RulerGranularity {
    Frame,
    Second,
    Minute,
}

#[derive(Copy, Clone)]
pub(crate) struct RulerScale {
    pub(crate) major_step_frames: i64,
    pub(crate) minor_step_frames: i64,
    pub(crate) granularity: RulerGranularity,
}

pub(crate) fn choose_ruler_scale(pixels_per_frame: f32, fps: Rational) -> RulerScale {
    let fps_nominal = fps.to_f64().round().max(1.0) as i64;
    let pixels_per_second = pixels_per_frame * fps_nominal as f32;

    let (granularity, mut candidates): (RulerGranularity, Vec<i64>) = if pixels_per_second >= 120.0
    {
        (
            RulerGranularity::Frame,
            vec![
                1,
                2,
                5,
                10,
                15,
                (fps_nominal / 2).max(1),
                fps_nominal,
                fps_nominal * 2,
                fps_nominal * 5,
            ],
        )
    } else if pixels_per_second >= 12.0 {
        (
            RulerGranularity::Second,
            vec![1, 2, 5, 10, 15, 30].into_iter().map(|s| s * fps_nominal).collect(),
        )
    } else {
        (
            RulerGranularity::Minute,
            vec![1, 2, 5, 10, 15, 30, 60]
                .into_iter()
                .map(|m| m * 60 * fps_nominal)
                .collect(),
        )
    };

    candidates.sort_unstable();
    candidates.dedup();

    let min_major_pixels = 72.0;
    let major_step = candidates
        .iter()
        .copied()
        .find(|&step| pixels_per_frame * step as f32 >= min_major_pixels)
        .unwrap_or_else(|| candidates.last().copied().unwrap_or(1));

    let minor_step = choose_minor_step(major_step, pixels_per_frame);

    RulerScale {
        major_step_frames: major_step,
        minor_step_frames: minor_step,
        granularity,
    }
}

pub(crate) fn choose_minor_step(major_step_frames: i64, pixels_per_frame: f32) -> i64 {
    let min_minor_pixels = 8.0;
    for div in [10, 5, 4, 3, 2] {
        if major_step_frames % div == 0 {
            let minor = major_step_frames / div;
            if pixels_per_frame * minor as f32 >= min_minor_pixels {
                return minor.max(1);
            }
        }
    }
    major_step_frames.max(1)
}

pub(crate) fn format_ruler_label(
    frame: i64,
    fps: Rational,
    display_format: VideoDisplayFormat,
    granularity: RulerGranularity,
    start_timecode_frame: i64,
) -> String {
    let display_frame = frame.saturating_add(start_timecode_frame.max(0));
    if display_format == VideoDisplayFormat::Frames {
        return match granularity {
            RulerGranularity::Frame => display_frame.max(0).to_string(),
            RulerGranularity::Second | RulerGranularity::Minute => {
                let seconds = frame_to_seconds(display_frame, fps).floor() as i64;
                format_seconds_label(seconds, granularity)
            }
        };
    }

    if display_format == VideoDisplayFormat::FeetAndFrames16mm {
        return format_feet_and_frames(display_frame, 40);
    }

    if display_format == VideoDisplayFormat::FeetAndFrames35mm {
        return format_feet_and_frames(display_frame, 16);
    }

    if display_format == VideoDisplayFormat::Timecode2997DropFrame {
        return format_drop_frame_timecode(display_frame, 30);
    }

    let fps_nominal = fps.to_f64().round().max(1.0) as i64;
    let total_seconds = display_frame.div_euclid(fps_nominal);
    let frame_in_second = display_frame.rem_euclid(fps_nominal);

    match granularity {
        RulerGranularity::Frame => {
            let hours = total_seconds / 3600;
            let minutes = (total_seconds % 3600) / 60;
            let seconds = total_seconds % 60;
            format!("{hours:02}:{minutes:02}:{seconds:02}:{frame_in_second:02}")
        }
        RulerGranularity::Second | RulerGranularity::Minute => {
            format_seconds_label(total_seconds, granularity)
        }
    }
}

pub(crate) fn frame_to_seconds(frame: i64, fps: Rational) -> f64 {
    frame.max(0) as f64 / fps.to_f64().max(1.0)
}

pub(crate) fn format_seconds_label(total_seconds: i64, granularity: RulerGranularity) -> String {
    let total_seconds = total_seconds.max(0);
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    match granularity {
        RulerGranularity::Frame | RulerGranularity::Second => {
            format!("{hours:02}:{minutes:02}:{seconds:02}")
        }
        RulerGranularity::Minute => format!("{hours:02}:{minutes:02}"),
    }
}

pub(crate) fn format_feet_and_frames(frame: i64, frames_per_foot: i64) -> String {
    let frame = frame.max(0);
    let feet = frame / frames_per_foot;
    let frames = frame % frames_per_foot;
    format!("{feet}+{frames:02}")
}

pub(crate) fn format_drop_frame_timecode(frame: i64, nominal_fps: i64) -> String {
    let frame = frame.max(0);
    let drop_frames = ((nominal_fps as f64) * 0.066_666_666_7).round() as i64;
    let frames_per_hour = nominal_fps * 60 * 60;
    let frames_per_24_hours = frames_per_hour * 24;
    let frames_per_10_minutes = nominal_fps * 60 * 10 - drop_frames * 9;
    let frames_per_minute = nominal_fps * 60 - drop_frames;

    let mut d = frame % frames_per_24_hours;
    let hours = d / frames_per_hour;
    d %= frames_per_hour;
    let tens_of_minutes = d / frames_per_10_minutes;
    d %= frames_per_10_minutes;
    let minutes = tens_of_minutes * 10 + d / frames_per_minute;

    let dropped = drop_frames * (minutes - minutes / 10);
    let tc_frame = frame + dropped;
    let seconds = (tc_frame / nominal_fps) % 60;
    let frames = tc_frame % nominal_fps;
    format!("{hours:02}:{minutes:02}:{seconds:02};{frames:02}")
}

pub(crate) fn collect_snap_candidates(
    seq: &Sequence,
    state: &AppState,
    exclude_clip_id: Option<ClipId>,
) -> Vec<SnapCandidate> {
    let mut by_frame: HashMap<i64, SnapPriority> = HashMap::new();
    let mut register = |frame: i64, priority: SnapPriority| {
        let frame = frame.max(0);
        match by_frame.get_mut(&frame) {
            Some(existing) if priority < *existing => *existing = priority,
            Some(_) => {}
            None => {
                by_frame.insert(frame, priority);
            }
        }
    };

    register(seq.playhead.frame, SnapPriority::Playhead);

    for clip in seq
        .video_tracks
        .iter()
        .chain(seq.audio_tracks.iter())
        .flat_map(|track| track.clips.iter())
    {
        if exclude_clip_id.is_some_and(|exclude| clip.id == exclude) {
            continue;
        }
        register(clip.position.frame, SnapPriority::AdjacentClipEdge);
        register(clip.end_position().frame, SnapPriority::AdjacentClipEdge);
    }

    for marker_frame in collect_marker_snap_frames(state) {
        register(marker_frame, SnapPriority::Marker);
    }

    let in_point = state.in_point_frame().max(0);
    let out_point = state.out_point_frame();
    if in_point > 0 || out_point.is_some() {
        register(in_point, SnapPriority::InOutPoint);
        if let Some(out) = out_point {
            register(out, SnapPriority::InOutPoint);
        }
    }

    by_frame
        .into_iter()
        .map(|(frame, priority)| SnapCandidate { frame, priority })
        .collect()
}

pub(crate) fn collect_marker_snap_frames(_state: &AppState) -> Vec<i64> {
    // 当前项目模型尚未持久化 marker；这里预留吸附入口，后续接入 marker 数据即可生效。
    Vec::new()
}

pub(crate) fn decide_snap_target(
    target_frame: i64,
    candidates: &[SnapCandidate],
    pixels_per_frame: f32,
    snap_pixels: f32,
) -> SnapDecision {
    let target_frame = target_frame.max(0);
    if pixels_per_frame <= 0.0 || candidates.is_empty() {
        return SnapDecision { frame: target_frame, snapped: false };
    }

    let threshold_frames = (snap_pixels / pixels_per_frame).ceil().max(1.0) as i64;
    let mut best: Option<(SnapCandidate, i64)> = None;

    for candidate in candidates {
        let distance = (candidate.frame - target_frame).abs();
        if distance > threshold_frames {
            continue;
        }

        let should_replace = match best {
            None => true,
            Some((current, current_distance)) => {
                candidate.priority < current.priority
                    || (candidate.priority == current.priority && distance < current_distance)
                    || (candidate.priority == current.priority
                        && distance == current_distance
                        && candidate.frame < current.frame)
            }
        };
        if should_replace {
            best = Some((*candidate, distance));
        }
    }

    match best {
        Some((candidate, _)) => SnapDecision { frame: candidate.frame.max(0), snapped: true },
        None => SnapDecision { frame: target_frame, snapped: false },
    }
}

pub(crate) fn timeline_visible_span_bounds(total_frames: f64, viewport_width: f32) -> (f64, f64) {
    if total_frames <= 0.0 || viewport_width <= 0.0 {
        return (1.0, 1.0);
    }

    let min_visible = (viewport_width as f64 / tokens::timeline_max_pixels_per_frame() as f64)
        .max(1.0)
        .min(total_frames);
    let max_visible = (viewport_width as f64 / tokens::timeline_min_pixels_per_frame() as f64)
        .max(min_visible)
        .min(total_frames);
    (min_visible, max_visible)
}

pub(crate) fn timeline_scrollbar_metrics(
    track_width: f32,
    total_frames: f64,
    viewport_width: f32,
    pixels_per_frame: f32,
    offset_frames: f64,
) -> TimelineScrollbarMetrics {
    let total_frames = total_frames.max(1.0);
    let visible_span_frames = (viewport_width.max(1.0) as f64 / pixels_per_frame.max(0.01) as f64)
        .max(1.0)
        .min(total_frames);
    let max_offset = (total_frames - visible_span_frames).max(0.0);
    let offset_frames = offset_frames.clamp(0.0, max_offset);
    let min_thumb_width = (tokens::timeline_scrollbar_handle_width() * 2.0 + 10.0).min(track_width);
    let thumb_width = ((visible_span_frames / total_frames) as f32 * track_width)
        .clamp(min_thumb_width, track_width);
    let usable_track_width = (track_width - thumb_width).max(0.0);
    let thumb_left = if max_offset <= f64::EPSILON || usable_track_width <= 0.0 {
        0.0
    } else {
        (offset_frames / max_offset) as f32 * usable_track_width
    };

    TimelineScrollbarMetrics {
        total_frames,
        visible_span_frames,
        offset_frames,
        thumb_left,
        thumb_width,
        track_width,
    }
}

pub(crate) fn apply_timeline_scrollbar_drag(
    metrics: TimelineScrollbarMetrics,
    drag: TimelineScrollbarDragState,
    delta_px: f32,
    min_visible_span: f64,
    max_visible_span: f64,
) -> (f64, f64) {
    if metrics.track_width <= 0.0 || metrics.total_frames <= 0.0 {
        return (metrics.offset_frames, metrics.visible_span_frames);
    }

    let delta_frames = delta_px as f64 * (metrics.total_frames / metrics.track_width as f64);
    match drag.kind {
        TimelineScrollbarDragKind::Thumb => {
            let max_offset = (metrics.total_frames - drag.start_visible_span_frames).max(0.0);
            (
                (drag.start_offset_frames + delta_frames).clamp(0.0, max_offset),
                drag.start_visible_span_frames,
            )
        }
        TimelineScrollbarDragKind::LeadingHandle => {
            let right_edge = drag.start_offset_frames + drag.start_visible_span_frames;
            let min_left = (right_edge - max_visible_span).max(0.0);
            let max_left = (right_edge - min_visible_span).max(0.0);
            let new_left = (drag.start_offset_frames + delta_frames).clamp(min_left, max_left);
            let new_visible = (right_edge - new_left)
                .clamp(min_visible_span, max_visible_span)
                .min(metrics.total_frames.max(min_visible_span));
            let max_offset = (metrics.total_frames - new_visible).max(0.0);
            (new_left.min(max_offset), new_visible)
        }
        TimelineScrollbarDragKind::TrailingHandle => {
            let left_edge = drag.start_offset_frames;
            let min_right = left_edge + min_visible_span;
            let max_right = (left_edge + max_visible_span).min(metrics.total_frames);
            let start_right = left_edge + drag.start_visible_span_frames;
            let new_right = (start_right + delta_frames).clamp(min_right, max_right);
            let new_visible = (new_right - left_edge)
                .clamp(min_visible_span, max_visible_span)
                .min((metrics.total_frames - left_edge).max(min_visible_span));
            let max_offset = (metrics.total_frames - new_visible).max(0.0);
            (left_edge.min(max_offset), new_visible)
        }
    }
}

pub(crate) fn timeline_vertical_scrollbar_metrics(
    track_height: f32,
    total_rows: f32,
    visible_height: f32,
    current_track_height: f32,
    offset: f32,
) -> TimelineVerticalScrollbarMetrics {
    let total_rows = total_rows.max(1.0);
    let visible_rows = (visible_height.max(1.0) / current_track_height.max(1.0))
        .max(1.0)
        .min(total_rows);
    let offset_rows =
        (offset / current_track_height.max(1.0)).clamp(0.0, (total_rows - visible_rows).max(0.0));
    let min_thumb_height = 28.0f32.min(track_height);
    let thumb_height =
        ((visible_rows / total_rows) * track_height).clamp(min_thumb_height, track_height);
    let usable_track_height = (track_height - thumb_height).max(0.0);
    let max_offset_rows = (total_rows - visible_rows).max(0.0);
    let thumb_top = if max_offset_rows <= f32::EPSILON || usable_track_height <= 0.0 {
        0.0
    } else {
        (offset_rows / max_offset_rows) * usable_track_height
    };

    TimelineVerticalScrollbarMetrics {
        total_rows,
        visible_rows,
        thumb_top,
        thumb_height,
        track_height,
    }
}

pub(crate) fn apply_timeline_vertical_scrollbar_drag(
    metrics: TimelineVerticalScrollbarMetrics,
    drag: TimelineVerticalScrollbarDragState,
    delta_px: f32,
) -> f32 {
    if metrics.track_height <= 0.0 || metrics.total_rows <= metrics.visible_rows {
        return 0.0;
    }

    let delta_rows = delta_px * (metrics.total_rows / metrics.track_height);
    let max_offset_rows = (metrics.total_rows - metrics.visible_rows).max(0.0);
    let offset_rows = (drag.start_offset / drag.start_track_height.max(1.0) + delta_rows)
        .clamp(0.0, max_offset_rows);
    offset_rows * drag.start_track_height.max(1.0)
}

pub(crate) fn apply_timeline_vertical_zoom_drag(
    metrics: TimelineVerticalScrollbarMetrics,
    drag: TimelineVerticalScrollbarDragState,
    delta_px: f32,
    visible_height: f32,
) -> (f32, f32) {
    const ZOOM_SENSITIVITY: f32 = 0.35;

    let start_track_height = drag.start_track_height.max(tokens::timeline_min_track_height());
    let start_visible_rows =
        (visible_height / start_track_height.max(1.0)).max(1.0).min(metrics.total_rows);
    let start_top_row = drag.start_offset / start_track_height.max(1.0);
    let start_bottom_row = (start_top_row + start_visible_rows).min(metrics.total_rows);
    let row_delta =
        delta_px * (metrics.total_rows / metrics.track_height.max(1.0)) * ZOOM_SENSITIVITY;
    let target_visible_rows = match drag.kind {
        TimelineVerticalScrollbarDragKind::LeadingHandle => {
            (start_visible_rows - row_delta).max(1.0)
        }
        TimelineVerticalScrollbarDragKind::TrailingHandle => {
            (start_visible_rows + row_delta).max(1.0)
        }
        TimelineVerticalScrollbarDragKind::Thumb => start_visible_rows,
    }
    .min(metrics.total_rows);

    let new_track_height = visible_height / target_visible_rows.max(1.0);
    let new_track_height = new_track_height.clamp(
        tokens::timeline_min_track_height(),
        tokens::timeline_max_track_height(),
    );
    let new_visible_rows =
        (visible_height / new_track_height.max(1.0)).max(1.0).min(metrics.total_rows);
    let max_top_row = (metrics.total_rows - new_visible_rows).max(0.0);
    let new_top_row = match drag.kind {
        TimelineVerticalScrollbarDragKind::LeadingHandle => {
            (start_bottom_row - new_visible_rows).clamp(0.0, max_top_row)
        }
        TimelineVerticalScrollbarDragKind::TrailingHandle => start_top_row.clamp(0.0, max_top_row),
        TimelineVerticalScrollbarDragKind::Thumb => start_top_row.clamp(0.0, max_top_row),
    };

    (new_track_height, new_top_row * new_track_height)
}
