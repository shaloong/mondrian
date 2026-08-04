//! Deep Module for Timeline Clip move and edge-trim product gestures.
//!
//! External intent carries stable identities plus an explicit evaluation grid.
//! This Module lowers that coordinate once, derives Track media kind, expands
//! complete Link Groups, prepares every mutation without touching live author
//! state, and commits one candidate through the Authoring Session.

use super::product_action::{
    TimelineMoveClipPayload, TimelineTrimClipsPayload, TimelineTrimPayloadEdge,
};
use super::timeline_editing::{
    apply_sequence_track_conflicts_for_focus_group, clip_link_group_member_ids,
    compact_sequence_references, expand_clip_link_groups, find_clip, find_clip_track_index,
    find_clip_track_lock, move_existing_clip_to_track_index_at_time, prepare_trimmed_clip,
};
use super::timeline_position::lower_nearest_sequence_frame;
use super::{AppState, ClipOverlapMode};
use mondrian_core::{ClipId, FramePosition, MondrianError, TimelineTime, TrackId};
use mondrian_timeline::{clip::TrimEdge, Clip, Sequence};
use std::collections::HashSet;

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
    let (source_track_id, source_is_video, source_locked) = find_clip_track_lock(sequence, clip_id)
        .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?;
    if source_locked {
        return Err(MondrianError::TrackLocked { track_id: source_track_id.to_string() });
    }
    let source_track_index = find_clip_track_index(sequence, source_is_video, clip_id)
        .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?;
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

    let primary = find_clip(sequence, clip_id)
        .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?;
    let primary_target =
        TimelineTime::from_frame_position(FramePosition::new(target_frame, sequence.time_base()))?;
    let time_delta = primary_target.checked_sub(primary.position)?;
    let track_delta = (target_track_index as i128) - (source_track_index as i128);

    let members = clip_link_group_member_ids(sequence, clip_id);
    if members.is_empty() {
        return Err(MondrianError::ClipNotFound { clip_id: clip_id.to_string() });
    }
    let mut moves = Vec::with_capacity(members.len());
    let mut changed = false;
    for member in members {
        let (member_track_id, member_is_video, member_locked) =
            find_clip_track_lock(sequence, member)
                .ok_or_else(|| MondrianError::ClipNotFound { clip_id: member.to_string() })?;
        if member_locked {
            return Err(MondrianError::TrackLocked { track_id: member_track_id.to_string() });
        }
        let member_source_index = find_clip_track_index(sequence, member_is_video, member)
            .ok_or_else(|| MondrianError::ClipNotFound { clip_id: member.to_string() })?;
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
        let member_position = find_clip(sequence, member)
            .ok_or_else(|| MondrianError::ClipNotFound { clip_id: member.to_string() })?
            .position;
        let member_target_position = member_position.checked_add(time_delta)?;
        if member_target_position.is_negative() {
            return Err(workflow_error(MOVE_STEP, "移动会使链接组成员越过序列零点"));
        }
        changed |=
            member_track_id != member_target_track.id || member_position != member_target_position;
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
        if !move_existing_clip_to_track_index_at_time(
            sequence,
            movement.is_video_track,
            movement.clip_id,
            target_index,
            movement.target_position,
        ) {
            return Err(MondrianError::ClipNotFound { clip_id: movement.clip_id.to_string() });
        }
    }
    apply_sequence_track_conflicts_for_focus_group(sequence, &plan.focus_ids, overlap_mode)?;
    compact_sequence_references(sequence);
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
    let mut targets = clip_ids.iter().copied().collect::<HashSet<_>>();
    for clip_id in &targets {
        if find_clip(sequence, *clip_id).is_none() {
            return Err(MondrianError::ClipNotFound { clip_id: clip_id.to_string() });
        }
    }
    for clip_id in &targets {
        let anchor = find_clip(sequence, *clip_id)
            .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?;
        let anchor_edge = clip_edge_time(anchor, edge)?;
        for member_id in clip_link_group_member_ids(sequence, *clip_id) {
            let member = find_clip(sequence, member_id)
                .ok_or_else(|| MondrianError::ClipNotFound { clip_id: member_id.to_string() })?;
            if clip_edge_time(member, edge)? != anchor_edge {
                return Err(workflow_error(
                    TRIM_STEP,
                    "offset Link Group edges require an explicit J/L trim policy",
                ));
            }
        }
    }
    expand_clip_link_groups(sequence, &mut targets);

    let mut updates = Vec::new();
    let time_base = sequence.time_base();
    for track in sequence.video_tracks.iter().chain(&sequence.audio_tracks) {
        for clip in track.clips.iter().filter(|clip| targets.contains(&clip.id)) {
            if track.is_locked {
                return Err(MondrianError::TrackLocked { track_id: track.id.to_string() });
            }
            if let Some(updated) = prepare_trimmed_clip(clip, edge, target_frame, time_base)? {
                updates.push(updated);
            }
        }
    }
    Ok(PreparedBulkTrim { updates })
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
    compact_sequence_references(sequence);
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
