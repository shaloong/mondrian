//! Product Adapter for Timeline Lift and Extract commands.

use super::AppState;
use mondrian_core::{FrameRounding, MondrianError, TimelineTimeRange};
use mondrian_timeline::{
    apply_range_edit, assess_range_edit, RangeEditAutomationPolicy, RangeEditKind,
    RangeEditOutcome, RangeEditRequest, RangeEditTimelineStatePolicy, RangeEditTransitionPolicy,
};

impl AppState {
    /// Whether the current In/Out range and Track controls admit this edit.
    pub fn can_apply_timeline_range_edit(&self, kind: RangeEditKind) -> bool {
        self.timeline_range_edit_request(kind).is_ok_and(|request| {
            self.active_sequence()
                .is_some_and(|sequence| assess_range_edit(sequence, &request).is_ok())
        })
    }

    /// Apply Lift or Extract through one validated Author Transaction.
    pub fn apply_timeline_range_edit(
        &mut self,
        kind: RangeEditKind,
    ) -> mondrian_core::Result<RangeEditOutcome> {
        let request = self.timeline_range_edit_request(kind)?;
        let range_start = request.range.start;
        let (_sequence_id, frame_rate, outcome) =
            self.commit_active_sequence_edit(range_edit_description(kind), move |sequence| {
                let outcome = apply_range_edit(sequence, &request)
                    .map_err(|error| range_edit_error(error.to_string()))?;
                Ok((sequence.id, sequence.settings.frame_rate, outcome))
            })?;

        self.prune_selection_to_active_sequence();
        let frame = range_start.to_frame_position(frame_rate, FrameRounding::Nearest)?.frame.max(0);
        self.reconcile_playhead_after_committed_authoring_change(frame, "timeline_range_edit");
        self.set_status_hint(range_edit_status(kind, &outcome), false);
        Ok(outcome)
    }

    fn timeline_range_edit_request(
        &self,
        kind: RangeEditKind,
    ) -> mondrian_core::Result<RangeEditRequest> {
        let sequence = self.active_sequence().ok_or_else(|| range_edit_error("当前无序列"))?;
        let start = sequence.in_point();
        let end = sequence
            .out_point()
            .ok_or_else(|| range_edit_error("Lift/Extract 需要有效的序列入点和出点"))?;
        if end <= start {
            return Err(range_edit_error("序列出点必须晚于入点"));
        }
        let range = TimelineTimeRange::new(start, end.checked_sub(start)?)?;
        let targets = self.timeline_edit_targets(sequence);
        let ripple_tracks = if kind == RangeEditKind::Extract {
            targets.ripple_tracks
        } else {
            Default::default()
        };
        Ok(RangeEditRequest {
            kind,
            range,
            content_tracks: targets.content_tracks,
            ripple_tracks,
            automation_policy: if kind == RangeEditKind::Extract {
                RangeEditAutomationPolicy::FollowEditorialContent
            } else {
                RangeEditAutomationPolicy::PreserveSequenceTime
            },
            transition_policy: RangeEditTransitionPolicy::RemoveAffected,
            timeline_state_policy: RangeEditTimelineStatePolicy::CollapseToRangeStart,
        })
    }
}

fn range_edit_description(kind: RangeEditKind) -> &'static str {
    match kind {
        RangeEditKind::Lift => "提升入点/出点范围",
        RangeEditKind::Extract => "提取入点/出点范围",
    }
}

fn range_edit_status(kind: RangeEditKind, outcome: &RangeEditOutcome) -> String {
    let verb = match kind {
        RangeEditKind::Lift => "Lift",
        RangeEditKind::Extract => "Extract",
    };
    format!(
        "{verb} 完成：移除 {} 个片段，修剪 {} 个片段，分割 {} 个片段，移动 {} 个片段",
        outcome.removed_clip_ids.len(),
        outcome.trimmed_clip_ids.len(),
        outcome.split_clips.len(),
        outcome.shifted_clip_ids.len()
    )
}

fn range_edit_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "timeline_range_edit".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ui_actions::{
        timeline_extract_range_action, timeline_lift_range_action, track_set_edit_policy_action,
        TrackEditPolicyControl, TrackSetEditPolicyPayload,
    };
    use mondrian_core::{AssetId, FramePosition, Rational};
    use mondrian_timeline::{Clip, Sequence};

    fn tt(frame: i64, time_base: Rational) -> mondrian_core::TimelineTime {
        mondrian_core::TimelineTime::from_frame_position(FramePosition::new(frame, time_base))
            .expect("time")
    }

    fn state_with_range() -> (AppState, mondrian_core::TrackId, mondrian_core::TrackId) {
        let mut sequence = Sequence::new("range");
        let time_base = sequence.time_base();
        let first = sequence.video_tracks[0].id;
        let second = sequence.video_tracks[1].id;
        sequence.video_tracks[0]
            .add_clip(Clip::new(AssetId::new(), tt(0, time_base), tt(40, time_base)).expect("clip"))
            .expect("first");
        sequence.video_tracks[1]
            .add_clip(
                Clip::new(AssetId::new(), tt(50, time_base), tt(10, time_base)).expect("clip"),
            )
            .expect("second");
        sequence.in_point = Some(tt(10, time_base));
        sequence.out_point = Some(tt(30, time_base));
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        (state, first, second)
    }

    #[test]
    fn product_lift_is_one_undoable_transaction() {
        let (mut state, _, _) = state_with_range();
        let before_generation = state.project_author_generation();
        state.dispatch_action(timeline_lift_range_action()).expect("Lift action");
        let after_generation = state.project_author_generation();
        assert_eq!(after_generation, before_generation + 1);
        assert_eq!(
            state.active_sequence().expect("sequence").video_tracks[0].clips.len(),
            2
        );
        assert!(state.active_sequence().expect("sequence").in_point.is_none());

        assert!(state.undo_timeline().expect("undo"));
        assert_eq!(
            state.active_sequence().expect("sequence").video_tracks[0].clips.len(),
            1
        );
        assert!(state.active_sequence().expect("sequence").in_point.is_some());
        assert!(state.redo_timeline().expect("redo"));
        assert_eq!(
            state.active_sequence().expect("sequence").video_tracks[0].clips.len(),
            2
        );
    }

    #[test]
    fn product_extract_obeys_independent_target_and_sync_lock_controls() {
        let (mut state, first, second) = state_with_range();
        state
            .dispatch_action(track_set_edit_policy_action(TrackSetEditPolicyPayload {
                track_id: second,
                control: TrackEditPolicyControl::Target,
                enabled: false,
            }))
            .expect("untarget");
        state
            .dispatch_action(timeline_extract_range_action())
            .expect("Extract with Sync-Lock");
        assert_eq!(
            state.active_sequence().expect("sequence").video_tracks[1].clips[0].position,
            tt(30, state.active_sequence().expect("sequence").time_base())
        );

        assert!(state.undo_timeline().expect("undo"));
        let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
        sequence.in_point = Some(tt(10, sequence.time_base()));
        sequence.out_point = Some(tt(30, sequence.time_base()));
        state
            .dispatch_action(track_set_edit_policy_action(TrackSetEditPolicyPayload {
                track_id: second,
                control: TrackEditPolicyControl::SyncLock,
                enabled: false,
            }))
            .expect("disable Sync-Lock");
        state
            .dispatch_action(timeline_extract_range_action())
            .expect("Extract without Sync-Lock");
        let sequence = state.active_sequence().expect("sequence");
        assert_eq!(
            sequence.video_tracks[1].clips[0].position,
            tt(50, sequence.time_base())
        );
        assert!(state.timeline_track_targeted(sequence.id, first));
    }
}
