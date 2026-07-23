//! Product authoring boundary for visual Transitions.
//!
//! This module owns source-handle admission and atomic AppState transactions.
//! Timeline persistence owns edit geometry; media/nested adapters own real
//! source extents; the renderer consumes only an already-admitted author plan.

use std::time::Duration;

use mondrian_core::{
    events::AppEvent, ClipId, FramePosition, MondrianError, Rational, TimelineTime,
    TimelineTimeRange, VideoTransitionId,
};
use mondrian_timeline::{clip::Clip, VideoTransition};

use super::AppState;

/// Explicit policy for a requested Transition whose source handles are short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoTransitionHandlePolicy {
    /// Preserve the requested author range or reject the edit.
    Reject,
    /// Intersect with real source extents while retaining the editorial cut.
    ShortenToAvailable,
}

/// Result of one committed visual-Transition edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoTransitionEditOutcome {
    /// Stable Transition identity.
    pub transition_id: VideoTransitionId,
    /// Exact range committed to the author document.
    pub applied_range: TimelineTimeRange,
    /// Whether explicit handle policy shortened the requested range.
    pub was_shortened: bool,
}

impl AppState {
    /// Create a centered, approximately one-second Cross Dissolve on the
    /// Sequence frame grid.
    ///
    /// The default is clamped only by endpoint placement geometry. Real source
    /// handles remain authoritative and are admitted with the fail-closed
    /// `Reject` policy; the UI must request shortening explicitly.
    pub fn create_default_cross_dissolve(
        &mut self,
        left_id: ClipId,
        right_id: ClipId,
    ) -> mondrian_core::Result<VideoTransitionEditOutcome> {
        let sequence = self.active_sequence().cloned().ok_or_else(no_active_sequence)?;
        let (_, _, left, right) = transition_endpoints(&sequence, left_id, right_id)?;
        let requested_range =
            default_cross_dissolve_range(left, right, sequence.settings.frame_rate)?;
        self.create_cross_dissolve(
            left_id,
            right_id,
            requested_range,
            VideoTransitionHandlePolicy::Reject,
        )
    }

    /// Preflight every enabled visual Transition in one immutable Sequence.
    ///
    /// This is used again at export snapshot capture because relinked media or
    /// an edited child Sequence may have changed external source extents since
    /// the author transaction was committed.
    pub(crate) fn validate_video_transition_source_handles(
        &self,
        sequence: &mondrian_timeline::sequence::Sequence,
    ) -> mondrian_core::Result<()> {
        for transition in
            sequence.video_transitions.iter().filter(|transition| transition.is_enabled)
        {
            let (_, _, left, right) =
                transition_endpoints(sequence, transition.left, transition.right)?;
            let left_extent = self.transition_source_extent(left)?;
            let right_extent = self.transition_source_extent(right)?;
            if !handles_satisfy(transition, left, right, left_extent, right_extent)? {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "video_transition_handle_preflight".to_owned(),
                    reason: format!(
                        "video Transition {} exceeds the current source handles",
                        transition.id
                    ),
                });
            }
        }
        Ok(())
    }

    /// Create one Cross Dissolve between an ordered adjacent Clip pair.
    ///
    /// The entire operation, including an explicitly requested shortening, is
    /// one validated author transaction and therefore one Undo step.
    pub fn create_cross_dissolve(
        &mut self,
        left_id: ClipId,
        right_id: ClipId,
        requested_range: TimelineTimeRange,
        handle_policy: VideoTransitionHandlePolicy,
    ) -> mondrian_core::Result<VideoTransitionEditOutcome> {
        let sequence = self.active_sequence().cloned().ok_or_else(no_active_sequence)?;
        let (track_id, track_locked, left, right) =
            transition_endpoints(&sequence, left_id, right_id)?;
        if track_locked {
            return Err(MondrianError::TrackLocked { track_id: track_id.to_string() });
        }
        let left_extent = self.transition_source_extent(left)?;
        let right_extent = self.transition_source_extent(right)?;
        let (applied_range, was_shortened) = admit_transition_range(
            left,
            right,
            requested_range,
            left_extent,
            right_extent,
            handle_policy,
        )?;
        let transition = VideoTransition::cross_dissolve(left_id, right_id, applied_range);
        let transition_id = transition.id;
        let sequence_id = self.commit_active_sequence_edit("创建交叉溶解", |sequence| {
            sequence.video_transitions.push(transition);
            sequence.validate_author_identities()?;
            Ok(sequence.id)
        })?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        if was_shortened {
            self.set_status_hint("交叉溶解已按可用源素材手柄缩短", false);
        }
        Ok(VideoTransitionEditOutcome { transition_id, applied_range, was_shortened })
    }

    /// Change one Transition range after re-running exact source-handle admission.
    pub fn set_video_transition_range(
        &mut self,
        transition_id: VideoTransitionId,
        requested_range: TimelineTimeRange,
        handle_policy: VideoTransitionHandlePolicy,
    ) -> mondrian_core::Result<VideoTransitionEditOutcome> {
        let sequence = self.active_sequence().cloned().ok_or_else(no_active_sequence)?;
        let transition = sequence
            .video_transitions
            .iter()
            .find(|transition| transition.id == transition_id)
            .ok_or_else(|| transition_not_found(transition_id))?;
        let (_, track_locked, left, right) =
            transition_endpoints(&sequence, transition.left, transition.right)?;
        if track_locked {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "set_video_transition_range".to_owned(),
                reason: "Transition endpoint Track is locked".to_owned(),
            });
        }
        let left_extent = self.transition_source_extent(left)?;
        let right_extent = self.transition_source_extent(right)?;
        let (applied_range, was_shortened) = admit_transition_range(
            left,
            right,
            requested_range,
            left_extent,
            right_extent,
            handle_policy,
        )?;
        let sequence_id = self.commit_active_sequence_edit("调整视频转场", |sequence| {
            let transition = sequence
                .video_transitions
                .iter_mut()
                .find(|transition| transition.id == transition_id)
                .ok_or_else(|| transition_not_found(transition_id))?;
            transition.sequence_range = applied_range;
            sequence.validate_author_identities()?;
            Ok(sequence.id)
        })?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        if was_shortened {
            self.set_status_hint("视频转场已按可用源素材手柄缩短", false);
        }
        Ok(VideoTransitionEditOutcome { transition_id, applied_range, was_shortened })
    }

    /// Delete one visual Transition as a single author transaction.
    pub fn remove_video_transition(
        &mut self,
        transition_id: VideoTransitionId,
    ) -> mondrian_core::Result<()> {
        let sequence = self.active_sequence().cloned().ok_or_else(no_active_sequence)?;
        let transition = sequence
            .video_transitions
            .iter()
            .find(|transition| transition.id == transition_id)
            .ok_or_else(|| transition_not_found(transition_id))?;
        let (_, track_locked, _, _) =
            transition_endpoints(&sequence, transition.left, transition.right)?;
        if track_locked {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "remove_video_transition".to_owned(),
                reason: "Transition endpoint Track is locked".to_owned(),
            });
        }
        let sequence_id = self.commit_active_sequence_edit("删除视频转场", |sequence| {
            let index = sequence
                .video_transitions
                .iter()
                .position(|transition| transition.id == transition_id)
                .ok_or_else(|| transition_not_found(transition_id))?;
            sequence.video_transitions.remove(index);
            Ok(sequence.id)
        })?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(())
    }

    fn transition_source_extent(
        &self,
        clip: &Clip,
    ) -> mondrian_core::Result<Option<TimelineTimeRange>> {
        match &clip.content {
            mondrian_core::timeline_data::ClipContent::SolidColor { .. } => Ok(None),
            mondrian_core::timeline_data::ClipContent::AdjustmentLayer { .. } => {
                Err(MondrianError::WorkflowStepFailed {
                    step_id: "video_transition_source_extent".to_owned(),
                    reason: "Adjustment Layer cannot be a Transition endpoint".to_owned(),
                })
            }
            mondrian_core::timeline_data::ClipContent::NestedSequence { sequence_id } => {
                let child = self.sequence_by_id(*sequence_id).ok_or_else(|| {
                    MondrianError::WorkflowStepFailed {
                        step_id: "video_transition_source_extent".to_owned(),
                        reason: format!("nested Sequence does not exist: {sequence_id}"),
                    }
                })?;
                Ok(Some(TimelineTimeRange::new(
                    TimelineTime::ZERO,
                    child.total_duration()?,
                )?))
            }
            mondrian_core::timeline_data::ClipContent::Media { asset_id, .. } => {
                let library =
                    self.asset_library().ok_or_else(|| MondrianError::WorkflowStepFailed {
                        step_id: "video_transition_source_extent".to_owned(),
                        reason: "素材库未连接".to_owned(),
                    })?;
                let asset = library.get_asset(*asset_id)?.ok_or_else(|| {
                    MondrianError::WorkflowStepFailed {
                        step_id: "video_transition_source_extent".to_owned(),
                        reason: format!("Transition source asset does not exist: {asset_id}"),
                    }
                })?;
                let Some(video) = asset.media_info.primary_video() else {
                    return Err(MondrianError::WorkflowStepFailed {
                        step_id: "video_transition_source_extent".to_owned(),
                        reason: format!("Transition source has no video stream: {asset_id}"),
                    });
                };
                if video.total_frames == Some(1) {
                    return Ok(None);
                }
                let Some(duration) = video.duration.filter(|duration| !duration.is_zero()) else {
                    return Err(MondrianError::WorkflowStepFailed {
                        step_id: "video_transition_source_extent".to_owned(),
                        reason: format!("Transition video-stream duration is unknown: {asset_id}"),
                    });
                };
                Ok(Some(TimelineTimeRange::new(
                    TimelineTime::ZERO,
                    timeline_time_from_duration(duration)?,
                )?))
            }
        }
    }
}

fn default_cross_dissolve_range(
    left: &Clip,
    right: &Clip,
    frame_rate: Rational,
) -> mondrian_core::Result<TimelineTimeRange> {
    if frame_rate.num <= 0 || frame_rate.den <= 0 {
        return Err(MondrianError::WorkflowStepFailed {
            step_id: "default_cross_dissolve_range".to_owned(),
            reason: "Sequence frame rate must be positive".to_owned(),
        });
    }
    let rounded_frames_per_second =
        frame_rate.num.checked_add(frame_rate.den / 2).ok_or_else(|| {
            MondrianError::WorkflowStepFailed {
                step_id: "default_cross_dissolve_range".to_owned(),
                reason: "Sequence frame rate exceeds the default-duration range".to_owned(),
            }
        })? / frame_rate.den;
    let total_frames = rounded_frames_per_second.max(1);
    let outgoing_frames = total_frames / 2;
    let incoming_frames = total_frames - outgoing_frames;
    let time_base = Rational::new(frame_rate.den, frame_rate.num);
    let outgoing =
        TimelineTime::from_frame_position(FramePosition::new(outgoing_frames, time_base))?;
    let incoming =
        TimelineTime::from_frame_position(FramePosition::new(incoming_frames, time_base))?;
    let cut = left.end_position()?;
    if right.position != cut {
        return Err(MondrianError::WorkflowStepFailed {
            step_id: "default_cross_dissolve_range".to_owned(),
            reason: "Cross Dissolve endpoints do not share one exact edit".to_owned(),
        });
    }
    let start = cut.checked_sub(outgoing)?.max(left.position);
    let end = cut.checked_add(incoming)?.min(right.end_position()?);
    if end <= start {
        return Err(MondrianError::WorkflowStepFailed {
            step_id: "default_cross_dissolve_range".to_owned(),
            reason: "adjacent Clips cannot contain a non-empty Cross Dissolve".to_owned(),
        });
    }
    TimelineTimeRange::new(start, end.checked_sub(start)?).map_err(Into::into)
}

fn transition_endpoints(
    sequence: &mondrian_timeline::sequence::Sequence,
    left_id: ClipId,
    right_id: ClipId,
) -> mondrian_core::Result<(mondrian_core::TrackId, bool, &Clip, &Clip)> {
    for track in &sequence.video_tracks {
        let Some(left_index) = track.clips.iter().position(|clip| clip.id == left_id) else {
            continue;
        };
        let Some(right_index) = track.clips.iter().position(|clip| clip.id == right_id) else {
            break;
        };
        if left_index.checked_add(1) != Some(right_index) {
            break;
        }
        return Ok((
            track.id,
            track.is_locked,
            &track.clips[left_index],
            &track.clips[right_index],
        ));
    }
    Err(MondrianError::WorkflowStepFailed {
        step_id: "video_transition_endpoints".to_owned(),
        reason: "Transition endpoints must be an ordered adjacent pair on one video Track"
            .to_owned(),
    })
}

fn admit_transition_range(
    left: &Clip,
    right: &Clip,
    requested: TimelineTimeRange,
    left_extent: Option<TimelineTimeRange>,
    right_extent: Option<TimelineTimeRange>,
    policy: VideoTransitionHandlePolicy,
) -> mondrian_core::Result<(TimelineTimeRange, bool)> {
    let requested_transition = VideoTransition::cross_dissolve(left.id, right.id, requested);
    requested_transition.validate_definition_state()?;
    if handles_satisfy(
        &requested_transition,
        left,
        right,
        left_extent,
        right_extent,
    )? {
        return Ok((requested, false));
    }
    if policy == VideoTransitionHandlePolicy::Reject {
        return Err(MondrianError::WorkflowStepFailed {
            step_id: "video_transition_handle_admission".to_owned(),
            reason: "requested Transition range exceeds available source handles".to_owned(),
        });
    }

    let mut admitted = requested;
    for (clip, extent) in [(left, left_extent), (right, right_extent)] {
        let Some(extent) = extent else {
            continue;
        };
        let Some(allowed) = source_extent_in_sequence_time(clip, extent)? else {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "video_transition_handle_admission".to_owned(),
                reason: "Transition endpoint has no usable source sample".to_owned(),
            });
        };
        admitted = intersect_ranges(admitted, allowed)?.ok_or_else(|| {
            MondrianError::WorkflowStepFailed {
                step_id: "video_transition_handle_admission".to_owned(),
                reason: "available source handles cannot produce a non-empty Transition".to_owned(),
            }
        })?;
    }
    let cut = left.end_position()?;
    if admitted.start > cut || admitted.end()? < cut {
        return Err(MondrianError::WorkflowStepFailed {
            step_id: "video_transition_handle_admission".to_owned(),
            reason: "shortening source handles would move the Transition away from its edit"
                .to_owned(),
        });
    }
    let admitted_transition = VideoTransition::cross_dissolve(left.id, right.id, admitted);
    if !handles_satisfy(&admitted_transition, left, right, left_extent, right_extent)? {
        return Err(MondrianError::WorkflowStepFailed {
            step_id: "video_transition_handle_admission".to_owned(),
            reason: "shortened Transition still exceeds source handles".to_owned(),
        });
    }
    Ok((admitted, admitted != requested))
}

fn handles_satisfy(
    transition: &VideoTransition,
    left: &Clip,
    right: &Clip,
    left_extent: Option<TimelineTimeRange>,
    right_extent: Option<TimelineTimeRange>,
) -> mondrian_core::Result<bool> {
    let demand = transition.source_demand(left, right)?;
    let left_extent = left_extent.unwrap_or(expand_empty_demand(demand.left)?);
    let right_extent = right_extent.unwrap_or(expand_empty_demand(demand.right)?);
    Ok(transition
        .validate_source_extents(left, right, left_extent, right_extent)
        .is_ok())
}

fn expand_empty_demand(demand: TimelineTimeRange) -> mondrian_core::Result<TimelineTimeRange> {
    if demand.is_empty() {
        Ok(TimelineTimeRange::new(demand.start, TimelineTime::ONE)?)
    } else {
        Ok(demand)
    }
}

fn source_extent_in_sequence_time(
    clip: &Clip,
    extent: TimelineTimeRange,
) -> mondrian_core::Result<Option<TimelineTimeRange>> {
    if clip.speed.scale().numerator() == 0 {
        let end = extent.end()?;
        return Ok((clip.source_in >= extent.start && clip.source_in < end)
            .then_some(TimelineTimeRange::new(clip.position, clip.duration)?));
    }
    let first = clip.source_to_timeline_time(extent.start)?;
    let second = clip.source_to_timeline_time(extent.end()?)?;
    let start = first.min(second);
    let end = first.max(second);
    if end <= start {
        return Ok(None);
    }
    Ok(Some(TimelineTimeRange::new(
        start,
        end.checked_sub(start)?,
    )?))
}

fn intersect_ranges(
    left: TimelineTimeRange,
    right: TimelineTimeRange,
) -> mondrian_core::Result<Option<TimelineTimeRange>> {
    let start = left.start.max(right.start);
    let end = left.end()?.min(right.end()?);
    if end <= start {
        return Ok(None);
    }
    Ok(Some(TimelineTimeRange::new(
        start,
        end.checked_sub(start)?,
    )?))
}

fn timeline_time_from_duration(duration: Duration) -> mondrian_core::Result<TimelineTime> {
    let nanos = duration.as_nanos();
    let numerator = i64::try_from(nanos).map_err(|_| MondrianError::WorkflowStepFailed {
        step_id: "video_transition_source_extent".to_owned(),
        reason: "source duration exceeds exact Timeline Time range".to_owned(),
    })?;
    Ok(TimelineTime::new(numerator, 1_000_000_000)?)
}

fn no_active_sequence() -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "video_transition_authoring".to_owned(),
        reason: "当前无项目".to_owned(),
    }
}

fn transition_not_found(id: VideoTransitionId) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "video_transition_authoring".to_owned(),
        reason: format!("video Transition does not exist: {id}"),
    }
}

#[cfg(test)]
mod tests {
    use mondrian_core::AssetId;

    use super::*;

    fn tt(frame: i64) -> TimelineTime {
        TimelineTime::new(frame, 25).expect("test time")
    }

    #[test]
    fn default_cross_dissolve_is_centered_on_the_sequence_frame_grid() {
        let left = Clip::new(AssetId::new(), tt(0), tt(50)).expect("left");
        let right = Clip::new(AssetId::new(), tt(50), tt(50)).expect("right");

        let range =
            default_cross_dissolve_range(&left, &right, Rational::FPS_25).expect("default range");

        assert_eq!(
            range,
            TimelineTimeRange::new(tt(38), tt(25)).expect("expected")
        );
    }

    #[test]
    fn default_cross_dissolve_clamps_only_to_clip_placement_geometry() {
        let left = Clip::new(AssetId::new(), tt(0), tt(5)).expect("left");
        let right = Clip::new(AssetId::new(), tt(5), tt(3)).expect("right");

        let range =
            default_cross_dissolve_range(&left, &right, Rational::FPS_25).expect("default range");

        assert_eq!(
            range,
            TimelineTimeRange::new(tt(0), tt(8)).expect("expected")
        );
    }

    #[test]
    fn handle_admission_rejects_by_default_and_shortens_only_explicitly() {
        let left = Clip::new(AssetId::new(), tt(0), tt(10)).expect("left");
        let mut right = Clip::new(AssetId::new(), tt(10), tt(10)).expect("right");
        right.source_in = tt(5);
        right.source_out = tt(15);
        let requested = TimelineTimeRange::new(tt(8), tt(4)).expect("requested");
        let left_extent = TimelineTimeRange::new(tt(0), tt(11)).expect("left extent");
        let right_extent = TimelineTimeRange::new(tt(4), tt(20)).expect("right extent");

        assert!(admit_transition_range(
            &left,
            &right,
            requested,
            Some(left_extent),
            Some(right_extent),
            VideoTransitionHandlePolicy::Reject,
        )
        .is_err());
        let (admitted, shortened) = admit_transition_range(
            &left,
            &right,
            requested,
            Some(left_extent),
            Some(right_extent),
            VideoTransitionHandlePolicy::ShortenToAvailable,
        )
        .expect("explicit shortening");
        assert!(shortened);
        assert_eq!(
            admitted,
            TimelineTimeRange::new(tt(9), tt(2)).expect("shortened range")
        );
    }

    #[test]
    fn app_transition_edits_are_atomic_and_undoable() {
        let mut state = AppState::default();
        state.test_ensure_authoring();
        let (sequence_id, left_id, right_id, requested) = {
            let sequence = state.active_sequence_mut_uncommitted().expect("test Sequence");
            let left = Clip::new_solid_color(
                AssetId::new(),
                mondrian_core::Color::from_rgba8(255, 0, 0, 255),
                tt(0),
                tt(10),
            )
            .expect("left");
            let right = Clip::new_solid_color(
                AssetId::new(),
                mondrian_core::Color::from_rgba8(0, 0, 255, 255),
                tt(10),
                tt(10),
            )
            .expect("right");
            let (left_id, right_id) = (left.id, right.id);
            sequence.video_tracks[0].add_clip(left).expect("left placement");
            sequence.video_tracks[0].add_clip(right).expect("right placement");
            (
                sequence.id,
                left_id,
                right_id,
                TimelineTimeRange::new(tt(8), tt(4)).expect("requested"),
            )
        };

        let created = state
            .create_cross_dissolve(
                left_id,
                right_id,
                requested,
                VideoTransitionHandlePolicy::Reject,
            )
            .expect("create Transition");
        let selected = state
            .select_video_transition_by_id(created.transition_id)
            .expect("select Transition");
        assert_eq!(selected.transition_id, created.transition_id);
        assert!(state.selection.selected_clips.is_empty());
        assert!(state.selection.selected_track_ids.is_empty());
        assert_eq!(
            state.sequence_by_id(sequence_id).expect("sequence").video_transitions.len(),
            1
        );
        assert!(state.undo_timeline().expect("undo create"));
        assert!(state
            .sequence_by_id(sequence_id)
            .expect("sequence")
            .video_transitions
            .is_empty());
        assert!(state.selected_video_transition().is_none());
        assert!(state.redo_timeline().expect("redo create"));
        state
            .select_video_transition_by_id(created.transition_id)
            .expect("select restored Transition");
        state
            .dispatch_action(mondrian_editor_state::Action::DeleteSelection)
            .expect("delete selected Transition");
        assert!(state
            .sequence_by_id(sequence_id)
            .expect("sequence")
            .video_transitions
            .is_empty());
        assert!(state.selection.selected_video_transition.is_none());
        assert!(state.undo_timeline().expect("undo delete"));
        assert_eq!(
            state.sequence_by_id(sequence_id).expect("sequence").video_transitions.len(),
            1
        );
    }
}
