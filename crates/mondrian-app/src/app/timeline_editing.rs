use super::*;

fn author_time_from_frame(frame: i64, time_base: Rational) -> mondrian_core::Result<TimelineTime> {
    Ok(TimelineTime::from_frame_position(FramePosition::new(
        frame, time_base,
    ))?)
}

fn author_frame_from_time(time: TimelineTime, time_base: Rational) -> mondrian_core::Result<i64> {
    let frame_rate = Rational::new(time_base.den, time_base.num);
    Ok(time.to_frame_position(frame_rate, FrameRounding::Nearest)?.frame)
}

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

/// Resolve every member of the selected Clip's synchronization group.
/// Unlinked Clips return a one-element set containing themselves.
pub(super) fn clip_link_group_member_ids(seq: &Sequence, clip_id: ClipId) -> Vec<ClipId> {
    let Some(group) = find_clip(seq, clip_id).and_then(|clip| clip.link_group) else {
        return find_clip(seq, clip_id).map(|_| vec![clip_id]).unwrap_or_default();
    };
    seq.video_tracks
        .iter()
        .chain(&seq.audio_tracks)
        .flat_map(|track| &track.clips)
        .filter(|clip| clip.link_group == Some(group))
        .map(|clip| clip.id)
        .collect()
}

pub(super) fn expand_clip_link_groups(seq: &Sequence, clip_ids: &mut HashSet<ClipId>) {
    let selected = clip_ids.iter().copied().collect::<Vec<_>>();
    for clip_id in selected {
        clip_ids.extend(clip_link_group_member_ids(seq, clip_id));
    }
}

#[derive(Clone, Copy)]
pub(super) struct ClipSplitResult {
    pub(super) right_clip_id: ClipId,
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
    let start = author_frame_from_time(clip.position, time_base).ok()?;
    let end = author_frame_from_time(clip.end_position().ok()?, time_base).ok()?;
    if split_frame <= start || split_frame >= end {
        return None;
    }

    let left_duration = split_frame - start;
    let right_duration = end - split_frame;
    if left_duration <= 0 || right_duration <= 0 {
        return None;
    }

    let split_time = author_time_from_frame(split_frame, time_base).ok()?;
    let split_offset = split_time.checked_sub(clip.position).ok()?;
    let new_clip_time_in = clip.timeline_to_clip_time(split_time).ok()?;
    let new_source_origin = clip.timeline_to_source_time(split_time).ok()?;

    let mut left = clip.clone();
    left.duration = author_time_from_frame(left_duration, time_base).ok()?;

    let mut right = clip;
    right.fork_placement_identities_for_split(split_offset).ok()?;
    right.position = split_time;
    right.duration = author_time_from_frame(right_duration, time_base).ok()?;
    right.clip_time_in = new_clip_time_in;
    right.set_source_origin(new_source_origin).ok()?;
    right.link_group = None;

    track.clips[index] = left;
    let right_id = right.id;
    track.clips.insert(index + 1, right);

    Some(ClipSplitResult { right_clip_id: right_id })
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
    let time_base = seq.time_base();
    for track in &mut seq.video_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            return roll_cut_in_track(track, index, target_frame, time_base);
        }
    }

    for track in &mut seq.audio_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            return roll_cut_in_track(track, index, target_frame, time_base);
        }
    }

    Err(mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() })
}

pub(super) fn can_roll_cut_for_clip(
    seq: &Sequence,
    clip_id: ClipId,
    target_frame: i64,
) -> mondrian_core::Result<bool> {
    let time_base = seq.time_base();
    for track in seq.video_tracks.iter().chain(&seq.audio_tracks) {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Ok(false);
            }
            return resolve_roll_target(track, index, target_frame, time_base)
                .map(|target| target.is_some());
        }
    }
    Ok(false)
}

pub(super) fn roll_cut_in_track(
    track: &mut mondrian_timeline::track::Track,
    clip_index: usize,
    target_frame: i64,
    time_base: Rational,
) -> mondrian_core::Result<bool> {
    let Some((boundary, new_cut_frame)) =
        resolve_roll_target(track, clip_index, target_frame, time_base)?
    else {
        return Ok(false);
    };

    let left_original = track.clips[boundary.left_index].clone();
    let right_original = track.clips[boundary.right_index].clone();

    let new_left_duration =
        new_cut_frame - author_frame_from_time(left_original.position, time_base)?;
    let new_right_duration =
        author_frame_from_time(right_original.end_position()?, time_base)? - new_cut_frame;
    if new_left_duration <= 0 || new_right_duration <= 0 {
        return Err(mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: "roll_cut".to_string(),
            reason: "滚动修剪后片段时长无效".to_string(),
        });
    }

    let new_cut_time = author_time_from_frame(new_cut_frame, time_base)?;
    let new_right_clip_time_in = right_original.timeline_to_clip_time(new_cut_time)?;
    let new_right_source_origin = right_original.timeline_to_source_time(new_cut_time)?;

    let mut left_updated = left_original;
    left_updated.duration = author_time_from_frame(new_left_duration, time_base)?;

    let mut right_updated = right_original;
    right_updated.shift_audio_component_in(new_cut_time.checked_sub(right_updated.position)?)?;
    right_updated.position = new_cut_time;
    right_updated.duration = author_time_from_frame(new_right_duration, time_base)?;
    right_updated.clip_time_in = new_right_clip_time_in;
    right_updated.set_source_origin(new_right_source_origin)?;

    track.clips[boundary.left_index] = left_updated;
    track.clips[boundary.right_index] = right_updated;
    track.clips.sort_by_key(|clip| clip.position);
    Ok(true)
}

fn resolve_roll_target(
    track: &mondrian_timeline::track::Track,
    clip_index: usize,
    target_frame: i64,
    time_base: Rational,
) -> mondrian_core::Result<Option<(RollBoundary, i64)>> {
    let Some(current_clip) = track.clips.get(clip_index) else {
        return Ok(None);
    };

    let mut boundaries = Vec::with_capacity(2);

    if clip_index > 0 {
        let left = &track.clips[clip_index - 1];
        let right = current_clip;
        let left_end = author_frame_from_time(left.end_position()?, time_base)?;
        let right_start = author_frame_from_time(right.position, time_base)?;
        if left_end == right_start {
            let right_source_origin = author_frame_from_time(right.source_origin(), time_base)?;
            let min_frame_from_source_in = right_start.saturating_sub(right_source_origin);
            let min_frame = (author_frame_from_time(left.position, time_base)? + 1)
                .max(min_frame_from_source_in);
            let max_frame = author_frame_from_time(right.end_position()?, time_base)? - 1;
            if min_frame <= max_frame {
                boundaries.push(RollBoundary {
                    left_index: clip_index - 1,
                    right_index: clip_index,
                    current_cut_frame: right_start,
                    min_frame,
                    max_frame,
                });
            }
        }
    }

    if clip_index + 1 < track.clips.len() {
        let left = current_clip;
        let right = &track.clips[clip_index + 1];
        let left_end = author_frame_from_time(left.end_position()?, time_base)?;
        let right_start = author_frame_from_time(right.position, time_base)?;
        if left_end == right_start {
            let right_source_origin = author_frame_from_time(right.source_origin(), time_base)?;
            let min_frame_from_source_in = right_start.saturating_sub(right_source_origin);
            let min_frame = (author_frame_from_time(left.position, time_base)? + 1)
                .max(min_frame_from_source_in);
            let max_frame = author_frame_from_time(right.end_position()?, time_base)? - 1;
            if min_frame <= max_frame {
                boundaries.push(RollBoundary {
                    left_index: clip_index,
                    right_index: clip_index + 1,
                    current_cut_frame: left_end,
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
        return Ok(None);
    };

    let new_cut_frame = target_frame.clamp(boundary.min_frame, boundary.max_frame);
    if new_cut_frame == boundary.current_cut_frame {
        return Ok(None);
    }
    Ok(Some((boundary, new_cut_frame)))
}

pub(super) fn slip_clip_internal(
    seq: &mut Sequence,
    library: &AssetLibrary,
    clip_id: ClipId,
    delta_frames: i64,
) -> mondrian_core::Result<bool> {
    let time_base = seq.time_base();
    for track in &mut seq.video_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            let estimated_total_source_frames =
                estimate_asset_total_source_frames(library, &track.clips[index], time_base);
            return slip_clip_in_track(
                track,
                index,
                delta_frames,
                estimated_total_source_frames,
                time_base,
            );
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
                estimate_asset_total_source_frames(library, &track.clips[index], time_base);
            return slip_clip_in_track(
                track,
                index,
                delta_frames,
                estimated_total_source_frames,
                time_base,
            );
        }
    }

    Err(mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() })
}

pub(super) fn slip_clip_in_track(
    track: &mut mondrian_timeline::track::Track,
    clip_index: usize,
    delta_frames: i64,
    estimated_total_source_frames: Option<i64>,
    time_base: Rational,
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

    let source_span = author_frame_from_time(original.source_terminal_boundary()?, time_base)?
        - author_frame_from_time(original.source_origin(), time_base)?;
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

    let old_source_in = author_frame_from_time(original.source_origin(), time_base)?;
    let proposed = old_source_in.saturating_add(delta_frames);
    let new_source_in = proposed.clamp(0, max_source_in);
    if new_source_in == old_source_in {
        return Ok(false);
    }

    let mut updated = original;
    updated.set_source_origin(author_time_from_frame(new_source_in, time_base)?)?;
    track.clips[clip_index] = updated;
    Ok(true)
}

pub(super) fn estimate_asset_total_source_frames(
    library: &AssetLibrary,
    clip: &Clip,
    time_base: Rational,
) -> Option<i64> {
    if clip.is_adjustment_layer() {
        return None;
    }

    let asset_id = clip.media_asset_id()?;
    let asset = match library.get_asset(asset_id) {
        Ok(Some(asset)) => asset,
        Ok(None) => return None,
        Err(err) => {
            tracing::debug!("读取素材时长失败 {}: {}", asset_id, err);
            return None;
        }
    };

    let media_probe = asset.media_probe()?;
    let frames_from_stream = media_probe.estimated_frames().map(|v| v as i64).filter(|v| *v > 0);
    if frames_from_stream.is_some() {
        return frames_from_stream;
    }

    let duration_secs = media_probe.duration.as_secs_f64();
    if duration_secs <= 0.0 {
        return None;
    }

    let frame_duration_secs = time_base.to_f64();
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
    let time_base = seq.time_base();
    for track in &mut seq.video_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            return slide_clip_in_track(track, index, delta_frames, time_base);
        }
    }

    for track in &mut seq.audio_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            return slide_clip_in_track(track, index, delta_frames, time_base);
        }
    }

    Err(mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() })
}

pub(super) fn slide_clip_in_track(
    track: &mut mondrian_timeline::track::Track,
    clip_index: usize,
    delta_frames: i64,
    time_base: Rational,
) -> mondrian_core::Result<bool> {
    if clip_index == 0 || clip_index + 1 >= track.clips.len() {
        return Ok(false);
    }

    let left_original = track.clips[clip_index - 1].clone();
    let center_original = track.clips[clip_index].clone();
    let right_original = track.clips[clip_index + 1].clone();

    let center_start = author_frame_from_time(center_original.position, time_base)?;
    let center_end = author_frame_from_time(center_original.end_position()?, time_base)?;
    let left_end = author_frame_from_time(left_original.end_position()?, time_base)?;
    let right_start = author_frame_from_time(right_original.position, time_base)?;

    if left_end != center_start || center_end != right_start {
        return Ok(false);
    }

    let min_start_from_left = author_frame_from_time(left_original.position, time_base)? + 1;
    let center_duration = author_frame_from_time(center_original.duration, time_base)?;
    let min_start_from_right_source =
        right_start
            .saturating_sub(center_duration)
            .saturating_sub(author_frame_from_time(
                right_original.source_origin(),
                time_base,
            )?);
    let min_start = min_start_from_left.max(min_start_from_right_source);
    let max_start =
        author_frame_from_time(right_original.end_position()?, time_base)? - center_duration - 1;
    if min_start > max_start {
        return Ok(false);
    }

    let proposed_start = center_start.saturating_add(delta_frames);
    let new_center_start = proposed_start.clamp(min_start, max_start);
    if new_center_start == center_start {
        return Ok(false);
    }

    let new_center_end = new_center_start + center_duration;
    let new_left_duration =
        new_center_start - author_frame_from_time(left_original.position, time_base)?;
    let new_right_duration =
        author_frame_from_time(right_original.end_position()?, time_base)? - new_center_end;
    if new_left_duration <= 0 || new_right_duration <= 0 {
        return Err(mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: "slide_clip".to_string(),
            reason: "滑动后片段时长无效".to_string(),
        });
    }

    let new_right_clip_time_in =
        right_original.timeline_to_clip_time(author_time_from_frame(new_center_end, time_base)?)?;
    let new_right_source_origin = right_original
        .timeline_to_source_time(author_time_from_frame(new_center_end, time_base)?)?;

    let mut left_updated = left_original;
    left_updated.duration = author_time_from_frame(new_left_duration, time_base)?;

    let mut center_updated = center_original;
    center_updated.position = author_time_from_frame(new_center_start, time_base)?;

    let mut right_updated = right_original;
    let right_position = author_time_from_frame(new_center_end, time_base)?;
    right_updated.shift_audio_component_in(right_position.checked_sub(right_updated.position)?)?;
    right_updated.position = right_position;
    right_updated.duration = author_time_from_frame(new_right_duration, time_base)?;
    right_updated.clip_time_in = new_right_clip_time_in;
    right_updated.set_source_origin(new_right_source_origin)?;

    track.clips[clip_index - 1] = left_updated;
    track.clips[clip_index] = center_updated;
    track.clips[clip_index + 1] = right_updated;
    track.clips.sort_by_key(|clip| clip.position);
    Ok(true)
}

pub(super) fn prepare_trimmed_clip_at_time(
    original: &Clip,
    edge: TrimEdge,
    target: TimelineTime,
    minimum_duration: TimelineTime,
) -> mondrian_core::Result<Option<Clip>> {
    if minimum_duration <= TimelineTime::ZERO {
        return Err(mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: "trim_clip".to_string(),
            reason: "修剪最小时长必须为正".to_string(),
        });
    }
    let start = original.position;
    let end = original.end_position()?;
    if end <= start {
        return Ok(None);
    }
    // A frame-grid gesture must not make an ordinary Clip shorter than one
    // Sequence frame. Existing exact sub-frame placements remain valid and
    // are never lengthened merely to satisfy that gesture policy.
    let retained_duration = original.duration.min(minimum_duration);

    let mut updated = original.clone();
    match edge {
        TrimEdge::In => {
            let latest_start = end.checked_sub(retained_duration)?;
            if latest_start <= start {
                return Ok(None);
            }
            let new_start = target.max(start).min(latest_start);
            if new_start == start {
                return Ok(None);
            }
            let new_in = original.timeline_to_source_time(new_start)?;
            let new_clip_time_in = original.timeline_to_clip_time(new_start)?;
            updated.shift_audio_component_in(new_start.checked_sub(updated.position)?)?;
            updated.position = new_start;
            updated.duration = end.checked_sub(new_start)?;
            updated.clip_time_in = new_clip_time_in;
            updated.set_source_origin(new_in)?;
        }
        TrimEdge::Out => {
            let minimum_end = start.checked_add(retained_duration)?;
            let new_end = if original.source_time_scale().numerator() == 0 {
                target.max(minimum_end)
            } else if minimum_end > end {
                return Ok(None);
            } else {
                target.max(minimum_end).min(end)
            };
            if new_end == end {
                return Ok(None);
            }
            updated.duration = new_end.checked_sub(start)?;
        }
    }

    if updated.duration <= TimelineTime::ZERO {
        return Err(mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: "trim_clip".to_string(),
            reason: "修剪后片段时长无效".to_string(),
        });
    }

    Ok(Some(updated))
}

pub(super) fn ensure_audio_track_index(seq: &mut Sequence, index: usize) {
    while seq.audio_tracks.len() <= index {
        seq.add_audio_track();
    }
}

pub(super) fn move_existing_clip_to_track_index_at_time(
    seq: &mut Sequence,
    is_video_track: bool,
    clip_id: ClipId,
    target_track_index: usize,
    target_position: TimelineTime,
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
                clip.position = target_position;
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
        clip.position = target_position;
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
                clip.position = target_position;
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
        clip.position = target_position;
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

pub(super) fn compact_sequence_references(seq: &mut Sequence) {
    seq.compact_structural_references();
}

pub(super) fn resolve_track_overlaps(
    track: &mut mondrian_timeline::track::Track,
) -> mondrian_core::Result<()> {
    track.clips.sort_by_key(|c| c.position);
    let mut cursor = TimelineTime::ZERO;
    for clip in &mut track.clips {
        if clip.position < cursor {
            clip.position = cursor;
        }
        cursor = clip.end_position()?;
    }
    Ok(())
}

fn merge_time_ranges(
    mut ranges: Vec<(TimelineTime, TimelineTime)>,
) -> Vec<(TimelineTime, TimelineTime)> {
    ranges.retain(|(start, end)| end > start);
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
    overlap_start: TimelineTime,
    overlap_end: TimelineTime,
) -> mondrian_core::Result<Vec<Clip>> {
    if overlap_end <= overlap_start {
        return Ok(vec![clip]);
    }

    let clip_start = clip.position;
    let clip_end = clip.end_position()?;
    let cut_start = overlap_start.max(clip_start);
    let cut_end = overlap_end.min(clip_end);
    if cut_end <= cut_start {
        return Ok(vec![clip]);
    }
    if cut_start <= clip_start && cut_end >= clip_end {
        return Ok(Vec::new());
    }

    if cut_start <= clip_start {
        let mut right = clip;
        let new_start = cut_end.max(clip_start);
        let new_clip_time_in = right.timeline_to_clip_time(new_start)?;
        let new_source_origin = right.timeline_to_source_time(new_start)?;
        right.shift_audio_component_in(new_start.checked_sub(clip_start)?)?;
        right.position = new_start;
        right.duration = clip_end.checked_sub(new_start)?.max(TimelineTime::ZERO);
        right.clip_time_in = new_clip_time_in;
        right.set_source_origin(new_source_origin)?;
        right.link_group = None;
        return Ok(if right.duration > TimelineTime::ZERO {
            vec![right]
        } else {
            Vec::new()
        });
    }

    if cut_end >= clip_end {
        let mut left = clip;
        let new_end = cut_start.min(clip_end);
        left.duration = new_end.checked_sub(clip_start)?.max(TimelineTime::ZERO);
        left.link_group = None;
        return Ok(if left.duration > TimelineTime::ZERO {
            vec![left]
        } else {
            Vec::new()
        });
    }

    let mut left = clip.clone();
    let left_new_end = cut_start;
    left.duration = left_new_end.checked_sub(clip_start)?.max(TimelineTime::ZERO);
    left.link_group = None;

    let mut right = clip;
    let right_new_start = cut_end;
    let right_new_clip_time_in = right.timeline_to_clip_time(right_new_start)?;
    let right_new_source_origin = right.timeline_to_source_time(right_new_start)?;
    right.fork_placement_identities_for_split(right_new_start.checked_sub(clip_start)?)?;
    right.position = right_new_start;
    right.duration = clip_end.checked_sub(right_new_start)?.max(TimelineTime::ZERO);
    right.clip_time_in = right_new_clip_time_in;
    right.set_source_origin(right_new_source_origin)?;
    right.link_group = None;

    let mut result = Vec::with_capacity(2);
    if left.duration > TimelineTime::ZERO {
        result.push(left);
    }
    if right.duration > TimelineTime::ZERO {
        result.push(right);
    }
    Ok(result)
}

pub(super) fn apply_overwrite_conflicts(
    track: &mut mondrian_timeline::track::Track,
    focus_ids: &HashSet<ClipId>,
    focus_ranges: Vec<(TimelineTime, TimelineTime)>,
) -> mondrian_core::Result<()> {
    let merged_ranges = merge_time_ranges(focus_ranges);
    if merged_ranges.is_empty() {
        track.clips.sort_by_key(|c| c.position);
        return Ok(());
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
                )?);
            }
            segments = next_segments;
        }
        resolved.extend(segments);
    }

    track.clips = resolved.into();
    track.clips.sort_by_key(|c| c.position);
    Ok(())
}

pub(super) fn resolve_track_conflicts(
    track: &mut mondrian_timeline::track::Track,
    focus_clip_id: ClipId,
    mode: ClipOverlapMode,
) -> mondrian_core::Result<()> {
    match mode {
        ClipOverlapMode::PushForward => resolve_track_overlaps(track)?,
        ClipOverlapMode::Overwrite => {
            let Some(focus) = track.clips.iter().find(|c| c.id == focus_clip_id).cloned() else {
                track.clips.sort_by_key(|c| c.position);
                return Ok(());
            };
            let focus_ids = HashSet::from([focus_clip_id]);
            apply_overwrite_conflicts(
                track,
                &focus_ids,
                vec![(focus.position, focus.end_position()?)],
            )?;
        }
    }
    Ok(())
}

pub(super) fn apply_track_conflicts_for_focus_group(
    track: &mut mondrian_timeline::track::Track,
    focus_ids: &HashSet<ClipId>,
    mode: ClipOverlapMode,
) -> mondrian_core::Result<()> {
    if focus_ids.is_empty() {
        return Ok(());
    }
    let focus_ranges = track
        .clips
        .iter()
        .filter(|clip| focus_ids.contains(&clip.id))
        .map(|clip| Ok((clip.position, clip.end_position()?)))
        .collect::<mondrian_core::Result<Vec<_>>>()?;
    if focus_ranges.is_empty() {
        // A Track without a moved member is outside the mutation footprint.
        // In particular, do not detach or sort its COW Clip storage.
        return Ok(());
    }
    match mode {
        ClipOverlapMode::PushForward => resolve_track_overlaps(track)?,
        ClipOverlapMode::Overwrite => {
            apply_overwrite_conflicts(track, focus_ids, focus_ranges)?;
        }
    }
    Ok(())
}

pub(super) fn apply_sequence_track_conflicts_for_focus_group(
    sequence: &mut Sequence,
    focus_ids: &HashSet<ClipId>,
    mode: ClipOverlapMode,
) -> mondrian_core::Result<()> {
    let video_indices = sequence
        .video_tracks
        .iter()
        .enumerate()
        .filter_map(|(index, track)| {
            track.clips.iter().any(|clip| focus_ids.contains(&clip.id)).then_some(index)
        })
        .collect::<Vec<_>>();
    let audio_indices = sequence
        .audio_tracks
        .iter()
        .enumerate()
        .filter_map(|(index, track)| {
            track.clips.iter().any(|clip| focus_ids.contains(&clip.id)).then_some(index)
        })
        .collect::<Vec<_>>();

    for index in video_indices {
        apply_track_conflicts_for_focus_group(&mut sequence.video_tracks[index], focus_ids, mode)?;
    }
    for index in audio_indices {
        apply_track_conflicts_for_focus_group(&mut sequence.audio_tracks[index], focus_ids, mode)?;
    }
    Ok(())
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
    // Search all tracks of the matching type, not just the one specified by
    // track_id. After cross-track moves (or undo/redo), the clip may be on a
    // different track than the selection's stored track_id.
    if selection.is_video_track {
        seq.video_tracks
            .iter()
            .find_map(|track| track.clips.iter().find(|clip| clip.id == selection.clip_id))
    } else {
        seq.audio_tracks
            .iter()
            .find_map(|track| track.clips.iter().find(|clip| clip.id == selection.clip_id))
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
    let Ok(position) = author_time_from_frame(frame.max(0), seq.time_base()) else {
        return false;
    };
    if let Some(clip) = find_clip_mut(seq, clip_id) {
        if clip.position == position {
            return false;
        }
        clip.position = position;
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
    // Search all tracks of matching type, not just the one from selection.track_id.
    if selection.is_video_track {
        seq.video_tracks
            .iter_mut()
            .find_map(|track| track.clips.iter_mut().find(|clip| clip.id == selection.clip_id))
    } else {
        seq.audio_tracks
            .iter_mut()
            .find_map(|track| track.clips.iter_mut().find(|clip| clip.id == selection.clip_id))
    }
}

// ─────────────────────────────────────────────
//  Timeline editing helpers
// ─────────────────────────────────────────────
