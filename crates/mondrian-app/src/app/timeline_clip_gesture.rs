//! Deep Module for Timeline Clip move and edge-trim product gestures.
//!
//! External intent carries stable identities plus an explicit evaluation grid.
//! This Module lowers that coordinate once, derives Track media kind, expands
//! complete Link Groups, prepares every mutation without touching live author
//! state, and commits one candidate through the Authoring Session.

use super::product_action::{
    TimelineMoveClipPayload, TimelineTrimClipsPayload, TimelineTrimPayloadEdge,
};
use super::timeline_position::lower_nearest_sequence_frame;
use super::{
    apply_sequence_track_conflicts_for_focus_group, clip_selection_unit,
    prepare_trimmed_clip_at_time, AppState, ClipOverlapMode,
};
use mondrian_core::{ClipId, FramePosition, MondrianError, TimelineTime, TrackId};
use mondrian_timeline::{clip::TrimEdge, Clip, Sequence};
use std::collections::{HashMap, HashSet};

const MOVE_STEP: &str = "move_clip";
const TRIM_STEP: &str = "timeline_trim_clips";

#[derive(Debug, Clone)]
struct PreparedClipPlacementMove {
    clip_id: ClipId,
    is_video_track: bool,
    target_track_id: TrackId,
    target_position: TimelineTime,
}

#[derive(Debug, Clone)]
struct PreparedClipMove {
    moves: Vec<PreparedClipPlacementMove>,
    focus_ids: HashSet<ClipId>,
}

#[derive(Debug, Clone)]
struct PreparedBulkTrim {
    updates: Vec<Clip>,
}

impl AppState {
    /// Whether one typed Clip move can execute against current author state.
    pub fn can_move_clip_from_product_action(&self, payload: TimelineMoveClipPayload) -> bool {
        self.prepare_product_clip_move(payload).is_ok()
    }

    /// Move one Clip through stable identities and one explicit input grid.
    pub fn move_clip_from_product_action(
        &mut self,
        payload: TimelineMoveClipPayload,
    ) -> mondrian_core::Result<()> {
        let plan = self.prepare_product_clip_move(payload)?;
        self.commit_prepared_clip_move(plan, ClipOverlapMode::Overwrite)
    }

    /// Whether one typed bulk trim can change current author state atomically.
    pub fn can_trim_clips_from_product_action(&self, payload: &TimelineTrimClipsPayload) -> bool {
        self.prepare_product_bulk_trim(payload)
            .is_ok_and(|plan| !plan.updates.is_empty())
    }

    /// Trim one or more Clip edges through one explicit input grid.
    pub fn trim_clips_from_product_action(
        &mut self,
        payload: TimelineTrimClipsPayload,
    ) -> mondrian_core::Result<usize> {
        let plan = self.prepare_product_bulk_trim(&payload)?;
        if plan.updates.is_empty() {
            return Err(action_not_executed(TRIM_STEP, "请求不会改变任何 Clip 边界"));
        }
        self.commit_prepared_bulk_trim(plan, trim_action_label(payload.edge))
    }

    /// Move a Clip on the active Sequence after a frame coordinate has already
    /// been lowered at an internal evaluation seam.
    pub fn move_clip_to_track_with_mode(
        &mut self,
        target_track_id: TrackId,
        clip_id: ClipId,
        target_frame: i64,
        overlap_mode: ClipOverlapMode,
    ) -> mondrian_core::Result<()> {
        let plan = {
            let sequence = self.active_sequence().ok_or_else(|| missing_sequence(MOVE_STEP))?;
            prepare_clip_move_to_frame(sequence, target_track_id, clip_id, target_frame)?
        };
        self.commit_prepared_clip_move(plan, overlap_mode)
    }

    /// Trim Clips after a frame coordinate has already been lowered at an
    /// internal evaluation seam.
    pub fn trim_clips_bulk_to_frame(
        &mut self,
        clip_ids: &[ClipId],
        edge: TrimEdge,
        target_frame: i64,
    ) -> mondrian_core::Result<usize> {
        if clip_ids.is_empty() {
            return Ok(0);
        }
        let plan = {
            let sequence = self.active_sequence().ok_or_else(|| missing_sequence(TRIM_STEP))?;
            prepare_bulk_trim_to_frame(sequence, clip_ids, edge, target_frame)?
        };
        if plan.updates.is_empty() {
            return Ok(0);
        }
        self.commit_prepared_bulk_trim(plan, trim_edge_action_label(edge))
    }

    /// Assess one internal frame-grid Trim through the same preparation used
    /// by execution without mutating author state.
    pub(super) fn assess_trim_clips_to_frame(
        &self,
        clip_ids: &[ClipId],
        edge: TrimEdge,
        target_frame: i64,
    ) -> mondrian_core::Result<usize> {
        if clip_ids.is_empty() {
            return Err(action_not_executed(TRIM_STEP, "Clip 集合不能为空"));
        }
        let sequence = self.active_sequence().ok_or_else(|| missing_sequence(TRIM_STEP))?;
        let plan = prepare_bulk_trim_to_frame(sequence, clip_ids, edge, target_frame)?;
        if plan.updates.is_empty() {
            return Err(action_not_executed(TRIM_STEP, "请求不会改变任何 Clip 边界"));
        }
        Ok(plan.updates.len())
    }

    fn prepare_product_clip_move(
        &self,
        payload: TimelineMoveClipPayload,
    ) -> mondrian_core::Result<PreparedClipMove> {
        let sequence = self.active_sequence().ok_or_else(|| missing_sequence(MOVE_STEP))?;
        let target_frame = lower_nearest_sequence_frame(sequence, payload.position, MOVE_STEP)?;
        prepare_clip_move_to_frame(
            sequence,
            payload.target_track_id,
            payload.clip_id,
            target_frame,
        )
    }

    fn prepare_product_bulk_trim(
        &self,
        payload: &TimelineTrimClipsPayload,
    ) -> mondrian_core::Result<PreparedBulkTrim> {
        if payload.clip_ids.is_empty() {
            return Err(action_not_executed(TRIM_STEP, "Clip 集合不能为空"));
        }
        let sequence = self.active_sequence().ok_or_else(|| missing_sequence(TRIM_STEP))?;
        let target_frame = lower_nearest_sequence_frame(sequence, payload.position, TRIM_STEP)?;
        prepare_bulk_trim_to_frame(
            sequence,
            &payload.clip_ids,
            trim_edge(payload.edge),
            target_frame,
        )
    }

    fn commit_prepared_clip_move(
        &mut self,
        plan: PreparedClipMove,
        overlap_mode: ClipOverlapMode,
    ) -> mondrian_core::Result<()> {
        let sequence_id = self.active_sequence_id().ok_or_else(|| missing_sequence(MOVE_STEP))?;
        let moved_ids = plan.moves.iter().map(|movement| movement.clip_id).collect::<Vec<_>>();
        self.commit_sequence_edit(sequence_id, "移动片段", move |sequence| {
            apply_prepared_clip_move(sequence, &plan, overlap_mode)
        })?;
        self.refresh_selected_clip_locations(&moved_ids);
        Ok(())
    }

    fn commit_prepared_bulk_trim(
        &mut self,
        plan: PreparedBulkTrim,
        label: &'static str,
    ) -> mondrian_core::Result<usize> {
        let changed = plan.updates.len();
        let sequence_id = self.active_sequence_id().ok_or_else(|| missing_sequence(TRIM_STEP))?;
        self.commit_sequence_edit(sequence_id, label, move |sequence| {
            apply_prepared_bulk_trim(sequence, plan)
        })?;
        Ok(changed)
    }
}

fn prepare_clip_move_to_frame(
    sequence: &Sequence,
    target_track_id: TrackId,
    clip_id: ClipId,
    target_frame: i64,
) -> mondrian_core::Result<PreparedClipMove> {
    if target_frame < 0 {
        return Err(workflow_error(
            MOVE_STEP,
            "Timeline position must be non-negative",
        ));
    }
    let source_location = sequence
        .clip_track_location(clip_id)
        .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?;
    if source_location.is_locked {
        return Err(MondrianError::TrackLocked { track_id: source_location.track_id.to_string() });
    }
    let source_is_video = source_location.is_video_track;
    let source_track_index = source_location.track_index;
    let (target_is_video, target_track_index) = resolve_track_index(sequence, target_track_id)?;
    if source_is_video != target_is_video {
        return Err(workflow_error(
            MOVE_STEP,
            format!("Clip {clip_id} and target Track {target_track_id} have different media kinds"),
        ));
    }
    let target_track = track_at(sequence, target_is_video, target_track_index)?;
    if target_track.is_locked {
        return Err(MondrianError::TrackLocked { track_id: target_track.id.to_string() });
    }

    let primary = sequence
        .find_clip(clip_id)
        .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?;
    let primary_target =
        TimelineTime::from_frame_position(FramePosition::new(target_frame, sequence.time_base()))?;
    let time_delta = primary_target.checked_sub(primary.position)?;
    let track_delta = (target_track_index as i128) - (source_track_index as i128);

    let members = clip_selection_unit(sequence, clip_id).unwrap_or_default();
    if members.is_empty() {
        return Err(MondrianError::ClipNotFound { clip_id: clip_id.to_string() });
    }
    let mut moves = Vec::with_capacity(members.len());
    let mut changed = false;
    for member in members {
        let member_location = sequence
            .clip_track_location(member)
            .ok_or_else(|| MondrianError::ClipNotFound { clip_id: member.to_string() })?;
        if member_location.is_locked {
            return Err(MondrianError::TrackLocked {
                track_id: member_location.track_id.to_string(),
            });
        }
        let member_is_video = member_location.is_video_track;
        let member_source_index = member_location.track_index;
        let track_count = if member_is_video {
            sequence.video_tracks.len()
        } else {
            sequence.audio_tracks.len()
        };
        let member_target_index = (member_source_index as i128)
            .checked_add(track_delta)
            .filter(|index| *index >= 0 && *index < track_count as i128)
            .ok_or_else(|| {
                workflow_error(
                    MOVE_STEP,
                    format!("moving Clip {clip_id} would place linked member {member} outside its Track set"),
                )
            })? as usize;
        let member_target_track = track_at(sequence, member_is_video, member_target_index)?;
        if member_target_track.is_locked {
            return Err(MondrianError::TrackLocked {
                track_id: member_target_track.id.to_string(),
            });
        }
        let member_position = sequence
            .find_clip(member)
            .ok_or_else(|| MondrianError::ClipNotFound { clip_id: member.to_string() })?
            .position;
        let member_target_position = member_position.checked_add(time_delta)?;
        if member_target_position.is_negative() {
            return Err(workflow_error(MOVE_STEP, "移动会使链接组成员越过序列零点"));
        }
        changed |= member_location.track_id != member_target_track.id
            || member_position != member_target_position;
        moves.push(PreparedClipPlacementMove {
            clip_id: member,
            is_video_track: member_is_video,
            target_track_id: member_target_track.id,
            target_position: member_target_position,
        });
    }
    if !changed {
        return Err(action_not_executed(
            MOVE_STEP,
            "Clip 已位于请求的 Track 和时间",
        ));
    }
    let focus_ids = moves.iter().map(|movement| movement.clip_id).collect();
    Ok(PreparedClipMove { moves, focus_ids })
}

fn apply_prepared_clip_move(
    sequence: &mut Sequence,
    plan: &PreparedClipMove,
    overlap_mode: ClipOverlapMode,
) -> mondrian_core::Result<()> {
    for movement in &plan.moves {
        let (is_video, target_index) = resolve_track_index(sequence, movement.target_track_id)?;
        if is_video != movement.is_video_track {
            return Err(workflow_error(
                MOVE_STEP,
                "prepared Clip move Track kind changed before commit",
            ));
        }
        if !sequence.move_clip_to_track_at_time(
            movement.is_video_track,
            movement.clip_id,
            target_index,
            movement.target_position,
        ) {
            return Err(MondrianError::ClipNotFound { clip_id: movement.clip_id.to_string() });
        }
    }
    apply_sequence_track_conflicts_for_focus_group(sequence, &plan.focus_ids, overlap_mode)?;
    sequence.compact_structural_references();
    Ok(())
}

fn prepare_bulk_trim_to_frame(
    sequence: &Sequence,
    clip_ids: &[ClipId],
    edge: TrimEdge,
    target_frame: i64,
) -> mondrian_core::Result<PreparedBulkTrim> {
    if target_frame < 0 {
        return Err(workflow_error(
            TRIM_STEP,
            "Timeline position must be non-negative",
        ));
    }
    let time_base = sequence.time_base();
    let target = TimelineTime::from_frame_position(FramePosition::new(target_frame, time_base))?;
    let minimum_duration = TimelineTime::from_frame_position(FramePosition::new(1, time_base))?;

    let mut roots = Vec::with_capacity(clip_ids.len());
    let mut seen_roots = HashSet::with_capacity(clip_ids.len());
    for clip_id in clip_ids {
        if sequence.find_clip(*clip_id).is_none() {
            return Err(MondrianError::ClipNotFound { clip_id: clip_id.to_string() });
        }
        if seen_roots.insert(*clip_id) {
            roots.push(*clip_id);
        }
    }

    let mut claimed_members = HashSet::new();
    let mut targets = HashMap::new();
    for root_id in roots {
        if claimed_members.contains(&root_id) {
            continue;
        }
        let root = sequence
            .find_clip(root_id)
            .ok_or_else(|| MondrianError::ClipNotFound { clip_id: root_id.to_string() })?;
        let requested_delta = target.checked_sub(clip_edge_time(root, edge)?)?;
        let member_ids = clip_selection_unit(sequence, root_id).unwrap_or_default();
        let mut minimum_delta: Option<TimelineTime> = None;
        let mut maximum_delta: Option<TimelineTime> = None;
        for member_id in &member_ids {
            let member = sequence
                .find_clip(*member_id)
                .ok_or_else(|| MondrianError::ClipNotFound { clip_id: member_id.to_string() })?;
            let (member_minimum, member_maximum) =
                trim_delta_bounds(member, edge, minimum_duration)?;
            minimum_delta =
                Some(minimum_delta.map_or(member_minimum, |current| current.max(member_minimum)));
            maximum_delta = intersect_optional_upper_bound(maximum_delta, member_maximum);
        }
        let minimum_delta = minimum_delta.unwrap_or(TimelineTime::ZERO);
        let mut delta = requested_delta.max(minimum_delta);
        if let Some(maximum_delta) = maximum_delta {
            delta = delta.min(maximum_delta);
        }
        for member_id in member_ids {
            let member = sequence
                .find_clip(member_id)
                .ok_or_else(|| MondrianError::ClipNotFound { clip_id: member_id.to_string() })?;
            let member_target = clip_edge_time(member, edge)?.checked_add(delta)?;
            claimed_members.insert(member_id);
            targets.insert(member_id, member_target);
        }
    }

    let mut updates = Vec::new();
    for track in sequence.video_tracks.iter().chain(&sequence.audio_tracks) {
        for clip in &track.clips {
            let Some(target) = targets.get(&clip.id).copied() else {
                continue;
            };
            if track.is_locked {
                return Err(MondrianError::TrackLocked { track_id: track.id.to_string() });
            }
            if let Some(updated) =
                prepare_trimmed_clip_at_time(clip, edge, target, minimum_duration)?
            {
                updates.push(updated);
            }
        }
    }
    Ok(PreparedBulkTrim { updates })
}

fn trim_delta_bounds(
    clip: &Clip,
    edge: TrimEdge,
    minimum_duration: TimelineTime,
) -> mondrian_core::Result<(TimelineTime, Option<TimelineTime>)> {
    let retained_duration = clip.duration.min(minimum_duration);
    match edge {
        TrimEdge::In => Ok((
            TimelineTime::ZERO,
            Some(clip.duration.checked_sub(retained_duration)?),
        )),
        TrimEdge::Out => Ok((
            retained_duration.checked_sub(clip.duration)?,
            (clip.source_time_scale().numerator() != 0).then_some(TimelineTime::ZERO),
        )),
    }
}

fn intersect_optional_upper_bound(
    current: Option<TimelineTime>,
    next: Option<TimelineTime>,
) -> Option<TimelineTime> {
    match (current, next) {
        (Some(current), Some(next)) => Some(current.min(next)),
        (Some(current), None) => Some(current),
        (None, Some(next)) => Some(next),
        (None, None) => None,
    }
}

fn clip_edge_time(clip: &Clip, edge: TrimEdge) -> mondrian_core::Result<TimelineTime> {
    match edge {
        TrimEdge::In => Ok(clip.position),
        TrimEdge::Out => clip.end_position(),
    }
}

fn apply_prepared_bulk_trim(
    sequence: &mut Sequence,
    plan: PreparedBulkTrim,
) -> mondrian_core::Result<()> {
    for updated in plan.updates {
        let clip_id = updated.id;
        let Some(slot) = sequence
            .video_tracks
            .iter_mut()
            .chain(&mut sequence.audio_tracks)
            .find_map(|track| track.clips.iter_mut().find(|clip| clip.id == clip_id))
        else {
            return Err(MondrianError::ClipNotFound { clip_id: clip_id.to_string() });
        };
        *slot = updated;
    }
    for track in sequence.video_tracks.iter_mut().chain(&mut sequence.audio_tracks) {
        track.clips.sort_by_key(|clip| clip.position);
    }
    sequence.compact_structural_references();
    Ok(())
}

fn resolve_track_index(
    sequence: &Sequence,
    track_id: TrackId,
) -> mondrian_core::Result<(bool, usize)> {
    if let Some(index) = sequence.video_tracks.iter().position(|track| track.id == track_id) {
        return Ok((true, index));
    }
    if let Some(index) = sequence.audio_tracks.iter().position(|track| track.id == track_id) {
        return Ok((false, index));
    }
    Err(MondrianError::TrackNotFound { track_id: track_id.to_string() })
}

fn track_at(
    sequence: &Sequence,
    is_video: bool,
    index: usize,
) -> mondrian_core::Result<&mondrian_timeline::Track> {
    let track = if is_video {
        sequence.video_tracks.get(index)
    } else {
        sequence.audio_tracks.get(index)
    };
    track.ok_or_else(|| workflow_error(MOVE_STEP, "resolved Track index is unavailable"))
}

fn trim_edge(edge: TimelineTrimPayloadEdge) -> TrimEdge {
    match edge {
        TimelineTrimPayloadEdge::In => TrimEdge::In,
        TimelineTrimPayloadEdge::Out => TrimEdge::Out,
    }
}

fn trim_action_label(edge: TimelineTrimPayloadEdge) -> &'static str {
    trim_edge_action_label(trim_edge(edge))
}

fn trim_edge_action_label(edge: TrimEdge) -> &'static str {
    match edge {
        TrimEdge::In => "修剪入点",
        TrimEdge::Out => "修剪出点",
    }
}

fn missing_sequence(step_id: &'static str) -> MondrianError {
    workflow_error(step_id, "当前无序列")
}

fn workflow_error(step_id: &'static str, reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed { step_id: step_id.to_owned(), reason: reason.into() }
}

fn action_not_executed(action: &'static str, reason: impl Into<String>) -> MondrianError {
    MondrianError::ActionNotExecuted { action: action.to_owned(), reason: reason.into() }
}
