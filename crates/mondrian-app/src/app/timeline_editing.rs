use super::*;

pub(super) fn find_clip_track_index(
    seq: &Sequence,
    is_video_track: bool,
    clip_id: ClipId,
) -> Option<usize> {
    if is_video_track {
        seq.video_tracks.iter().position(|t| t.clips.iter().any(|c| c.id == clip_id))
    } else {
        seq.audio_tracks.iter().position(|t| t.clips.iter().any(|c| c.id == clip_id))
    }
}

#[derive(Clone, Copy)]
pub(super) struct ClipSplitResult {
    pub(super) right_clip_id: ClipId,
    pub(super) original_linked: Option<ClipId>,
}

pub(super) fn split_clip_anywhere(
    seq: &mut Sequence,
    clip_id: ClipId,
    split_frame: i64,
    time_base: Rational,
) -> Option<ClipSplitResult> {
    for track in &mut seq.video_tracks {
        if let Some(result) = split_clip_in_track(track, clip_id, split_frame, time_base) {
            return Some(result);
        }
    }
    for track in &mut seq.audio_tracks {
        if let Some(result) = split_clip_in_track(track, clip_id, split_frame, time_base) {
            return Some(result);
        }
    }
    None
}

pub(super) fn split_clip_in_track(
    track: &mut mondrian_timeline::track::Track,
    clip_id: ClipId,
    split_frame: i64,
    time_base: Rational,
) -> Option<ClipSplitResult> {
    let index = track.clips.iter().position(|c| c.id == clip_id)?;
    let clip = track.clips.get(index)?.clone();
    let start = clip.position.frame;
    let end = clip.end_position().frame;
    if split_frame <= start || split_frame >= end {
        return None;
    }

    let left_duration = split_frame - start;
    let right_duration = end - split_frame;
    if left_duration <= 0 || right_duration <= 0 {
        return None;
    }

    let split_tc = TimeCode::new(split_frame, time_base);
    let new_source_in = clip.timeline_to_source_time(split_tc);

    let mut left = clip.clone();
    left.duration = TimeCode::new(left_duration, left.duration.time_base);
    left.source_out = new_source_in;

    let mut right = clip;
    right.id = ClipId::new();
    right.position = TimeCode::new(split_frame, right.position.time_base);
    right.duration = TimeCode::new(right_duration, right.duration.time_base);
    right.source_in = new_source_in;
    right.linked_clip = None;

    track.clips[index] = left;
    let right_id = right.id;
    track.clips.insert(index + 1, right);

    Some(ClipSplitResult {
        right_clip_id: right_id,
        original_linked: track.clips[index].linked_clip,
    })
}

#[derive(Clone, Copy)]
struct RollBoundary {
    left_index: usize,
    right_index: usize,
    current_cut_frame: i64,
    min_frame: i64,
    max_frame: i64,
}

pub(super) fn roll_cut_for_clip_internal(
    seq: &mut Sequence,
    clip_id: ClipId,
    target_frame: i64,
) -> mondrian_core::Result<bool> {
    for track in &mut seq.video_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            return roll_cut_in_track(track, index, target_frame);
        }
    }

    for track in &mut seq.audio_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            return roll_cut_in_track(track, index, target_frame);
        }
    }

    Err(mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() })
}

pub(super) fn roll_cut_in_track(
    track: &mut mondrian_timeline::track::Track,
    clip_index: usize,
    target_frame: i64,
) -> mondrian_core::Result<bool> {
    let Some(current_clip) = track.clips.get(clip_index).cloned() else {
        return Ok(false);
    };

    let mut boundaries = Vec::with_capacity(2);

    if clip_index > 0 {
        let left = &track.clips[clip_index - 1];
        let right = &current_clip;
        if left.end_position().frame == right.position.frame {
            let min_frame_from_source_in =
                right.position.frame.saturating_sub(right.source_in.frame);
            let min_frame = (left.position.frame + 1).max(min_frame_from_source_in);
            let max_frame = right.end_position().frame - 1;
            if min_frame <= max_frame {
                boundaries.push(RollBoundary {
                    left_index: clip_index - 1,
                    right_index: clip_index,
                    current_cut_frame: right.position.frame,
                    min_frame,
                    max_frame,
                });
            }
        }
    }

    if clip_index + 1 < track.clips.len() {
        let left = &current_clip;
        let right = &track.clips[clip_index + 1];
        if left.end_position().frame == right.position.frame {
            let min_frame_from_source_in =
                right.position.frame.saturating_sub(right.source_in.frame);
            let min_frame = (left.position.frame + 1).max(min_frame_from_source_in);
            let max_frame = right.end_position().frame - 1;
            if min_frame <= max_frame {
                boundaries.push(RollBoundary {
                    left_index: clip_index,
                    right_index: clip_index + 1,
                    current_cut_frame: left.end_position().frame,
                    min_frame,
                    max_frame,
                });
            }
        }
    }

    let Some(boundary) = boundaries
        .into_iter()
        .min_by_key(|candidate| (target_frame as i128 - candidate.current_cut_frame as i128).abs())
    else {
        return Ok(false);
    };

    let new_cut_frame = target_frame.clamp(boundary.min_frame, boundary.max_frame);
    if new_cut_frame == boundary.current_cut_frame {
        return Ok(false);
    }

    let left_original = track.clips[boundary.left_index].clone();
    let right_original = track.clips[boundary.right_index].clone();

    let new_left_duration = new_cut_frame - left_original.position.frame;
    let new_right_duration = right_original.end_position().frame - new_cut_frame;
    if new_left_duration <= 0 || new_right_duration <= 0 {
        return Err(mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: "roll_cut".to_string(),
            reason: "滚动修剪后片段时长无效".to_string(),
        });
    }

    let new_left_source_out = left_original.timeline_to_source_time(TimeCode::new(
        new_cut_frame,
        left_original.position.time_base,
    ));
    let new_right_source_in = right_original.timeline_to_source_time(TimeCode::new(
        new_cut_frame,
        right_original.position.time_base,
    ));

    let mut left_updated = left_original;
    left_updated.duration = TimeCode::new(new_left_duration, left_updated.duration.time_base);
    left_updated.source_out = new_left_source_out;

    let mut right_updated = right_original;
    right_updated.position = TimeCode::new(new_cut_frame, right_updated.position.time_base);
    right_updated.duration = TimeCode::new(new_right_duration, right_updated.duration.time_base);
    right_updated.source_in = new_right_source_in;

    track.clips[boundary.left_index] = left_updated;
    track.clips[boundary.right_index] = right_updated;
    track.clips.sort_by_key(|clip| clip.position.frame);
    Ok(true)
}

pub(super) fn slip_clip_internal(
    seq: &mut Sequence,
    library: &AssetLibrary,
    clip_id: ClipId,
    delta_frames: i64,
) -> mondrian_core::Result<bool> {
    for track in &mut seq.video_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            let estimated_total_source_frames =
                estimate_asset_total_source_frames(library, &track.clips[index]);
            return slip_clip_in_track(track, index, delta_frames, estimated_total_source_frames);
        }
    }

    for track in &mut seq.audio_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            let estimated_total_source_frames =
                estimate_asset_total_source_frames(library, &track.clips[index]);
            return slip_clip_in_track(track, index, delta_frames, estimated_total_source_frames);
        }
    }

    Err(mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() })
}

pub(super) fn slip_clip_in_track(
    track: &mut mondrian_timeline::track::Track,
    clip_index: usize,
    delta_frames: i64,
    estimated_total_source_frames: Option<i64>,
) -> mondrian_core::Result<bool> {
    let Some(original) = track.clips.get(clip_index).cloned() else {
        return Ok(false);
    };
    if original.is_adjustment_layer() {
        return Err(mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: "slip_clip".to_string(),
            reason: "调整图层不支持 slip".to_string(),
        });
    }

    let source_span = original.source_out.frame - original.source_in.frame;
    if source_span <= 0 {
        return Err(mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: "slip_clip".to_string(),
            reason: "片段源时间范围无效".to_string(),
        });
    }

    let max_source_in = if let Some(total_frames) = estimated_total_source_frames {
        total_frames.saturating_sub(source_span).max(0)
    } else {
        i64::MAX.saturating_sub(source_span)
    };

    let old_source_in = original.source_in.frame;
    let proposed = old_source_in.saturating_add(delta_frames);
    let new_source_in = proposed.clamp(0, max_source_in);
    if new_source_in == old_source_in {
        return Ok(false);
    }

    let new_source_out = new_source_in.saturating_add(source_span);
    let mut updated = original;
    updated.source_in = TimeCode::new(new_source_in, updated.source_in.time_base);
    updated.source_out = TimeCode::new(new_source_out, updated.source_out.time_base);
    track.clips[clip_index] = updated;
    Ok(true)
}

pub(super) fn estimate_asset_total_source_frames(
    library: &AssetLibrary,
    clip: &Clip,
) -> Option<i64> {
    if clip.is_adjustment_layer() {
        return None;
    }

    let asset = match library.get_asset(clip.asset_id) {
        Ok(Some(asset)) => asset,
        Ok(None) => return None,
        Err(err) => {
            tracing::debug!("读取素材时长失败 {}: {}", clip.asset_id, err);
            return None;
        }
    };

    let frames_from_stream =
        asset.media_info.estimated_frames().map(|v| v as i64).filter(|v| *v > 0);
    if frames_from_stream.is_some() {
        return frames_from_stream;
    }

    let duration_secs = asset.media_info.duration.as_secs_f64();
    if duration_secs <= 0.0 {
        return None;
    }

    let frame_duration_secs = clip.position.time_base.to_f64();
    if frame_duration_secs <= f64::EPSILON {
        return None;
    }

    Some((duration_secs / frame_duration_secs).ceil() as i64)
}

pub(super) fn slide_clip_internal(
    seq: &mut Sequence,
    clip_id: ClipId,
    delta_frames: i64,
) -> mondrian_core::Result<bool> {
    for track in &mut seq.video_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            return slide_clip_in_track(track, index, delta_frames);
        }
    }

    for track in &mut seq.audio_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            return slide_clip_in_track(track, index, delta_frames);
        }
    }

    Err(mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() })
}

pub(super) fn slide_clip_in_track(
    track: &mut mondrian_timeline::track::Track,
    clip_index: usize,
    delta_frames: i64,
) -> mondrian_core::Result<bool> {
    if clip_index == 0 || clip_index + 1 >= track.clips.len() {
        return Ok(false);
    }

    let left_original = track.clips[clip_index - 1].clone();
    let center_original = track.clips[clip_index].clone();
    let right_original = track.clips[clip_index + 1].clone();

    let center_start = center_original.position.frame;
    let center_end = center_original.end_position().frame;
    let left_end = left_original.end_position().frame;
    let right_start = right_original.position.frame;

    if left_end != center_start || center_end != right_start {
        return Ok(false);
    }

    let min_start_from_left = left_original.position.frame + 1;
    let min_start_from_right_source = right_original
        .position
        .frame
        .saturating_sub(center_original.duration.frame)
        .saturating_sub(right_original.source_in.frame);
    let min_start = min_start_from_left.max(min_start_from_right_source);
    let max_start = right_original.end_position().frame - center_original.duration.frame - 1;
    if min_start > max_start {
        return Ok(false);
    }

    let proposed_start = center_start.saturating_add(delta_frames);
    let new_center_start = proposed_start.clamp(min_start, max_start);
    if new_center_start == center_start {
        return Ok(false);
    }

    let new_center_end = new_center_start + center_original.duration.frame;
    let new_left_duration = new_center_start - left_original.position.frame;
    let new_right_duration = right_original.end_position().frame - new_center_end;
    if new_left_duration <= 0 || new_right_duration <= 0 {
        return Err(mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: "slide_clip".to_string(),
            reason: "滑动后片段时长无效".to_string(),
        });
    }

    let new_left_source_out = left_original.timeline_to_source_time(TimeCode::new(
        new_center_start,
        left_original.position.time_base,
    ));
    let new_right_source_in = right_original.timeline_to_source_time(TimeCode::new(
        new_center_end,
        right_original.position.time_base,
    ));

    let mut left_updated = left_original;
    left_updated.duration = TimeCode::new(new_left_duration, left_updated.duration.time_base);
    left_updated.source_out = new_left_source_out;

    let mut center_updated = center_original;
    center_updated.position = TimeCode::new(new_center_start, center_updated.position.time_base);

    let mut right_updated = right_original;
    right_updated.position = TimeCode::new(new_center_end, right_updated.position.time_base);
    right_updated.duration = TimeCode::new(new_right_duration, right_updated.duration.time_base);
    right_updated.source_in = new_right_source_in;

    track.clips[clip_index - 1] = left_updated;
    track.clips[clip_index] = center_updated;
    track.clips[clip_index + 1] = right_updated;
    track.clips.sort_by_key(|clip| clip.position.frame);
    Ok(true)
}

pub(super) fn trim_clip_edge_internal(
    seq: &mut Sequence,
    clip_id: ClipId,
    edge: TrimEdge,
    target_frame: i64,
) -> mondrian_core::Result<bool> {
    for track in &mut seq.video_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            return trim_clip_in_track(track, index, edge, target_frame);
        }
    }

    for track in &mut seq.audio_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            return trim_clip_in_track(track, index, edge, target_frame);
        }
    }

    Err(mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() })
}

pub(super) fn trim_clip_in_track(
    track: &mut mondrian_timeline::track::Track,
    index: usize,
    edge: TrimEdge,
    target_frame: i64,
) -> mondrian_core::Result<bool> {
    let Some(original) = track.clips.get(index).cloned() else {
        return Ok(false);
    };
    let start = original.position.frame;
    let end = original.end_position().frame;
    if end <= start {
        return Ok(false);
    }

    let mut updated = original.clone();
    match edge {
        TrimEdge::In => {
            let new_start = target_frame.max(start).min(end - 1);
            if new_start == start {
                return Ok(false);
            }
            let new_in = original
                .timeline_to_source_time(TimeCode::new(new_start, original.position.time_base));
            updated.position = TimeCode::new(new_start, original.position.time_base);
            updated.duration = TimeCode::new(end - new_start, original.duration.time_base);
            updated.source_in = new_in;
        }
        TrimEdge::Out => {
            let new_end = target_frame.max(start + 1).min(end);
            if new_end == end {
                return Ok(false);
            }
            let new_out = original
                .timeline_to_source_time(TimeCode::new(new_end, original.position.time_base));
            updated.duration = TimeCode::new(new_end - start, original.duration.time_base);
            updated.source_out = new_out;
        }
    }

    if updated.duration.frame <= 0 {
        return Err(mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: "trim_clip".to_string(),
            reason: "修剪后片段时长无效".to_string(),
        });
    }

    track.clips[index] = updated;
    track.clips.sort_by_key(|clip| clip.position.frame);
    Ok(true)
}

pub(super) fn ensure_audio_track_index(seq: &mut Sequence, index: usize) {
    while seq.audio_tracks.len() <= index {
        seq.add_audio_track();
    }
}

pub(super) fn move_existing_clip_to_track_index(
    seq: &mut Sequence,
    is_video_track: bool,
    clip_id: ClipId,
    target_track_index: usize,
    new_start: i64,
    time_base: Rational,
) -> bool {
    if is_video_track {
        if target_track_index >= seq.video_tracks.len() {
            return false;
        }

        let Some(source_track_index) = find_clip_track_index(seq, true, clip_id) else {
            return false;
        };

        if source_track_index == target_track_index {
            if let Some(clip) =
                seq.video_tracks[source_track_index].clips.iter_mut().find(|c| c.id == clip_id)
            {
                clip.position = TimeCode::new(new_start, time_base);
                return true;
            }
            return false;
        }

        let Some(clip_index) =
            seq.video_tracks[source_track_index].clips.iter().position(|c| c.id == clip_id)
        else {
            return false;
        };

        let mut clip = seq.video_tracks[source_track_index].clips.remove(clip_index);
        clip.position = TimeCode::new(new_start, time_base);
        seq.video_tracks[target_track_index].clips.push(clip);
        true
    } else {
        if target_track_index >= seq.audio_tracks.len() {
            return false;
        }

        let Some(source_track_index) = find_clip_track_index(seq, false, clip_id) else {
            return false;
        };

        if source_track_index == target_track_index {
            if let Some(clip) =
                seq.audio_tracks[source_track_index].clips.iter_mut().find(|c| c.id == clip_id)
            {
                clip.position = TimeCode::new(new_start, time_base);
                return true;
            }
            return false;
        }

        let Some(clip_index) =
            seq.audio_tracks[source_track_index].clips.iter().position(|c| c.id == clip_id)
        else {
            return false;
        };

        let mut clip = seq.audio_tracks[source_track_index].clips.remove(clip_index);
        clip.position = TimeCode::new(new_start, time_base);
        seq.audio_tracks[target_track_index].clips.push(clip);
        true
    }
}

pub(super) fn remove_clip_from_sequence(seq: &mut Sequence, clip_id: ClipId) -> bool {
    for track in &mut seq.video_tracks {
        if track.remove_clip(clip_id).is_some() {
            return true;
        }
    }
    for track in &mut seq.audio_tracks {
        if track.remove_clip(clip_id).is_some() {
            return true;
        }
    }
    false
}

pub(super) fn remove_clip_from_sequence_with_ripple(
    seq: &mut Sequence,
    clip_id: ClipId,
    ripple: bool,
) -> bool {
    for track in &mut seq.video_tracks {
        if let Some(index) = track.clips.iter().position(|c| c.id == clip_id) {
            let removed = track.clips.remove(index);
            if ripple {
                let start = removed.position.frame;
                let end = start + removed.duration.frame.max(0);
                for clip in &mut track.clips {
                    if clip.position.frame >= end {
                        clip.position = TimeCode::new(
                            (clip.position.frame - removed.duration.frame.max(0)).max(0),
                            clip.position.time_base,
                        );
                    }
                }
            }
            resolve_track_overlaps(track);
            return true;
        }
    }
    for track in &mut seq.audio_tracks {
        if let Some(index) = track.clips.iter().position(|c| c.id == clip_id) {
            let removed = track.clips.remove(index);
            if ripple {
                let start = removed.position.frame;
                let end = start + removed.duration.frame.max(0);
                for clip in &mut track.clips {
                    if clip.position.frame >= end {
                        clip.position = TimeCode::new(
                            (clip.position.frame - removed.duration.frame.max(0)).max(0),
                            clip.position.time_base,
                        );
                    }
                }
            }
            resolve_track_overlaps(track);
            return true;
        }
    }
    false
}

pub(super) fn clear_broken_links(seq: &mut Sequence) {
    let existing: HashSet<ClipId> = seq
        .video_tracks
        .iter()
        .flat_map(|t| t.clips.iter().map(|c| c.id))
        .chain(seq.audio_tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.id)))
        .collect();

    for track in &mut seq.video_tracks {
        for clip in &mut track.clips {
            if let Some(linked) = clip.linked_clip {
                if !existing.contains(&linked) {
                    clip.linked_clip = None;
                }
            }
        }
    }
    for track in &mut seq.audio_tracks {
        for clip in &mut track.clips {
            if let Some(linked) = clip.linked_clip {
                if !existing.contains(&linked) {
                    clip.linked_clip = None;
                }
            }
        }
    }
}

pub(super) fn remove_asset_clips_from_tracks(
    tracks: &mut [mondrian_timeline::track::Track],
    asset_id: AssetId,
) -> usize {
    let mut removed = 0usize;
    for track in tracks {
        let before = track.clips.len();
        track.clips.retain(|clip| clip.asset_id != asset_id);
        removed += before.saturating_sub(track.clips.len());
        resolve_track_overlaps(track);
    }
    removed
}

pub(super) fn resolve_track_overlaps(track: &mut mondrian_timeline::track::Track) {
    track.clips.sort_by_key(|c| c.position.frame);
    let mut cursor = 0i64;
    for clip in &mut track.clips {
        if clip.position.frame < cursor {
            clip.position = TimeCode::new(cursor, clip.position.time_base);
        }
        cursor = clip.end_position().frame;
    }
}

pub(super) fn merge_ranges(mut ranges: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    ranges.retain(|(start, end)| end > start);
    if ranges.is_empty() {
        return ranges;
    }
    ranges.sort_by_key(|(start, _)| *start);

    let mut merged = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        if let Some((_, last_end)) = merged.last_mut() {
            if start <= *last_end {
                *last_end = (*last_end).max(end);
                continue;
            }
        }
        merged.push((start, end));
    }
    merged
}

pub(super) fn subtract_overwrite_range_from_clip(
    clip: Clip,
    overlap_start: i64,
    overlap_end: i64,
) -> Vec<Clip> {
    if overlap_end <= overlap_start {
        return vec![clip];
    }

    let clip_start = clip.position.frame;
    let clip_end = clip.end_position().frame;
    let cut_start = overlap_start.max(clip_start);
    let cut_end = overlap_end.min(clip_end);
    if cut_end <= cut_start {
        return vec![clip];
    }
    if cut_start <= clip_start && cut_end >= clip_end {
        return Vec::new();
    }

    if cut_start <= clip_start {
        let mut right = clip;
        let new_start = cut_end.max(clip_start);
        let new_source_in =
            right.timeline_to_source_time(TimeCode::new(new_start, right.position.time_base));
        right.position = TimeCode::new(new_start, right.position.time_base);
        right.duration = TimeCode::new((clip_end - new_start).max(0), right.duration.time_base);
        right.source_in = new_source_in;
        return if right.duration.frame > 0 {
            vec![right]
        } else {
            Vec::new()
        };
    }

    if cut_end >= clip_end {
        let mut left = clip;
        let new_end = cut_start.min(clip_end);
        let new_source_out =
            left.timeline_to_source_time(TimeCode::new(new_end, left.position.time_base));
        left.duration = TimeCode::new((new_end - clip_start).max(0), left.duration.time_base);
        left.source_out = new_source_out;
        return if left.duration.frame > 0 {
            vec![left]
        } else {
            Vec::new()
        };
    }

    let mut left = clip.clone();
    let left_new_end = cut_start;
    let left_new_source_out =
        left.timeline_to_source_time(TimeCode::new(left_new_end, left.position.time_base));
    left.duration = TimeCode::new((left_new_end - clip_start).max(0), left.duration.time_base);
    left.source_out = left_new_source_out;

    let mut right = clip;
    let right_new_start = cut_end;
    let right_new_source_in =
        right.timeline_to_source_time(TimeCode::new(right_new_start, right.position.time_base));
    right.id = ClipId::new();
    right.position = TimeCode::new(right_new_start, right.position.time_base);
    right.duration = TimeCode::new(
        (clip_end - right_new_start).max(0),
        right.duration.time_base,
    );
    right.source_in = right_new_source_in;
    right.linked_clip = None;

    let mut result = Vec::with_capacity(2);
    if left.duration.frame > 0 {
        result.push(left);
    }
    if right.duration.frame > 0 {
        result.push(right);
    }
    result
}

pub(super) fn apply_overwrite_conflicts(
    track: &mut mondrian_timeline::track::Track,
    focus_ids: &HashSet<ClipId>,
    focus_ranges: Vec<(i64, i64)>,
) {
    let merged_ranges = merge_ranges(focus_ranges);
    if merged_ranges.is_empty() {
        track.clips.sort_by_key(|c| c.position.frame);
        return;
    }

    let mut resolved = Vec::<Clip>::with_capacity(track.clips.len());
    for clip in std::mem::take(&mut track.clips) {
        if focus_ids.contains(&clip.id) {
            resolved.push(clip);
            continue;
        }

        let mut segments = vec![clip];
        for (range_start, range_end) in &merged_ranges {
            if segments.is_empty() {
                break;
            }
            let mut next_segments = Vec::with_capacity(segments.len());
            for segment in segments {
                next_segments.extend(subtract_overwrite_range_from_clip(
                    segment,
                    *range_start,
                    *range_end,
                ));
            }
            segments = next_segments;
        }
        resolved.extend(segments);
    }

    track.clips = resolved;
    track.clips.sort_by_key(|c| c.position.frame);
}

pub(super) fn resolve_track_conflicts(
    track: &mut mondrian_timeline::track::Track,
    focus_clip_id: ClipId,
    mode: ClipOverlapMode,
) {
    match mode {
        ClipOverlapMode::Insert => resolve_track_overlaps(track),
        ClipOverlapMode::Overwrite => {
            let Some(focus) = track.clips.iter().find(|c| c.id == focus_clip_id).cloned() else {
                track.clips.sort_by_key(|c| c.position.frame);
                return;
            };
            let focus_ids = HashSet::from([focus_clip_id]);
            apply_overwrite_conflicts(
                track,
                &focus_ids,
                vec![(focus.position.frame, focus.end_position().frame)],
            );
        }
    }
}

pub(super) fn apply_track_conflicts_for_focus_group(
    track: &mut mondrian_timeline::track::Track,
    focus_ids: &HashSet<ClipId>,
    mode: ClipOverlapMode,
) {
    if focus_ids.is_empty() {
        return;
    }
    match mode {
        ClipOverlapMode::Insert => resolve_track_overlaps(track),
        ClipOverlapMode::Overwrite => {
            let focus_ranges: Vec<(i64, i64)> = track
                .clips
                .iter()
                .filter(|clip| focus_ids.contains(&clip.id))
                .map(|clip| (clip.position.frame, clip.end_position().frame))
                .collect();
            if focus_ranges.is_empty() {
                track.clips.sort_by_key(|c| c.position.frame);
                return;
            }
            apply_overwrite_conflicts(track, focus_ids, focus_ranges);
        }
    }
}

pub(super) fn apply_conflict_policy_for_existing_clip(
    seq: &mut Sequence,
    clip_id: ClipId,
    mode: ClipOverlapMode,
) {
    for track in &mut seq.video_tracks {
        if track.clips.iter().any(|c| c.id == clip_id) {
            resolve_track_conflicts(track, clip_id, mode);
            return;
        }
    }
    for track in &mut seq.audio_tracks {
        if track.clips.iter().any(|c| c.id == clip_id) {
            resolve_track_conflicts(track, clip_id, mode);
            return;
        }
    }
}

pub(super) fn find_clip(seq: &Sequence, clip_id: ClipId) -> Option<&Clip> {
    for track in &seq.video_tracks {
        if let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) {
            return Some(clip);
        }
    }
    for track in &seq.audio_tracks {
        if let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) {
            return Some(clip);
        }
    }
    None
}

pub(super) fn find_clip_by_selection(seq: &Sequence, selection: SelectedClipRef) -> Option<&Clip> {
    if selection.is_video_track {
        seq.video_tracks
            .iter()
            .find(|track| track.id == selection.track_id)
            .and_then(|track| track.clips.iter().find(|clip| clip.id == selection.clip_id))
    } else {
        seq.audio_tracks
            .iter()
            .find(|track| track.id == selection.track_id)
            .and_then(|track| track.clips.iter().find(|clip| clip.id == selection.clip_id))
    }
}

pub(super) fn find_clip_track_lock(
    seq: &Sequence,
    clip_id: ClipId,
) -> Option<(TrackId, bool, bool)> {
    for track in &seq.video_tracks {
        if track.clips.iter().any(|c| c.id == clip_id) {
            return Some((track.id, true, track.is_locked));
        }
    }
    for track in &seq.audio_tracks {
        if track.clips.iter().any(|c| c.id == clip_id) {
            return Some((track.id, false, track.is_locked));
        }
    }
    None
}

pub(super) fn set_clip_disabled(seq: &mut Sequence, clip_id: ClipId, disabled: bool) -> bool {
    if let Some(clip) = find_clip_mut(seq, clip_id) {
        if clip.is_disabled == disabled {
            return false;
        }
        clip.is_disabled = disabled;
        return true;
    }
    false
}

pub(super) fn set_clip_position(seq: &mut Sequence, clip_id: ClipId, frame: i64) -> bool {
    if let Some(clip) = find_clip_mut(seq, clip_id) {
        let frame = frame.max(0);
        if clip.position.frame == frame {
            return false;
        }
        clip.position = TimeCode::new(frame, clip.position.time_base);
        return true;
    }
    false
}

pub(super) fn find_clip_mut(seq: &mut Sequence, clip_id: ClipId) -> Option<&mut Clip> {
    for track in &mut seq.video_tracks {
        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
            return Some(clip);
        }
    }
    for track in &mut seq.audio_tracks {
        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
            return Some(clip);
        }
    }
    None
}

pub(super) fn find_clip_mut_by_selection(
    seq: &mut Sequence,
    selection: SelectedClipRef,
) -> Option<&mut Clip> {
    if selection.is_video_track {
        seq.video_tracks
            .iter_mut()
            .find(|track| track.id == selection.track_id)
            .and_then(|track| track.clips.iter_mut().find(|clip| clip.id == selection.clip_id))
    } else {
        seq.audio_tracks
            .iter_mut()
            .find(|track| track.id == selection.track_id)
            .and_then(|track| track.clips.iter_mut().find(|clip| clip.id == selection.clip_id))
    }
}

// ─────────────────────────────────────────────
//  MondrianApp — eframe::App 实现
// ─────────────────────────────────────────────
