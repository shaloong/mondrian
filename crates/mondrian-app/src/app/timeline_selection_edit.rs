//! Deep Product Adapter for edits whose operands are the current Clip selection.
//!
//! Widgets emit only a closed editorial intent. This Module resolves current
//! selection/playhead state, performs read-only admission with the same deep
//! Timeline helpers used by execution, and owns transaction plus selection
//! reconciliation. It never persists selection as author state.

use super::product_action::{TimelineSelectionEdit, TimelineTrimPayloadEdge};
use super::selection::resolve_clip_selection;
use super::timeline_editing::{
    can_roll_cut_for_clip, can_trim_clip_edge, expand_clip_link_groups, find_clip,
    find_clip_track_lock, set_clip_disabled,
};
use super::AppState;
use mondrian_core::{ClipId, MondrianError};
use mondrian_timeline::{
    apply_clip_link_edit, assess_clip_link_edit, clip::TrimEdge, ClipLinkEditKind,
    ClipLinkEditRequest,
};
use std::collections::HashSet;

impl AppState {
    /// Whether one selection-scoped editorial intent can change current state.
    pub fn can_apply_timeline_selection_edit(&self, edit: TimelineSelectionEdit) -> bool {
        match edit {
            TimelineSelectionEdit::LinkClips => {
                self.can_edit_selected_clip_links(ClipLinkEditKind::Link)
            }
            TimelineSelectionEdit::UnlinkClips => {
                self.can_edit_selected_clip_links(ClipLinkEditKind::Unlink)
            }
            TimelineSelectionEdit::TrimClipsToPlayhead { edge } => {
                self.prepare_selected_clip_trim(edge).is_ok()
            }
            TimelineSelectionEdit::RollCutToPlayhead => self.prepare_selected_roll().is_ok(),
            TimelineSelectionEdit::SetClipsEnabled { enabled } => {
                self.prepare_selected_clips_enabled(enabled).is_ok()
            }
        }
    }

    /// Apply one selection-scoped editorial intent through its owning Interface.
    pub fn apply_timeline_selection_edit(
        &mut self,
        edit: TimelineSelectionEdit,
    ) -> mondrian_core::Result<()> {
        match edit {
            TimelineSelectionEdit::LinkClips => {
                self.edit_selected_clip_links(ClipLinkEditKind::Link)
            }
            TimelineSelectionEdit::UnlinkClips => {
                self.edit_selected_clip_links(ClipLinkEditKind::Unlink)
            }
            TimelineSelectionEdit::TrimClipsToPlayhead { edge } => {
                self.trim_selected_clips_to_playhead(edge)
            }
            TimelineSelectionEdit::RollCutToPlayhead => self.roll_selected_cut_to_playhead(),
            TimelineSelectionEdit::SetClipsEnabled { enabled } => {
                self.set_selected_clips_enabled(enabled)
            }
        }
    }

    fn selected_clip_ids_for_edit(&self) -> Vec<ClipId> {
        let mut clip_ids = Vec::new();
        for selection in self.selected_clips() {
            if !clip_ids.contains(&selection.clip_id) {
                clip_ids.push(selection.clip_id);
            }
        }
        clip_ids
    }

    fn expanded_selected_clip_ids(&self) -> mondrian_core::Result<Vec<ClipId>> {
        let sequence = self.active_sequence().ok_or_else(|| selection_edit_error("当前无序列"))?;
        let mut clip_ids = self.selected_clip_ids_for_edit().into_iter().collect::<HashSet<_>>();
        if clip_ids.is_empty() {
            return Err(selection_not_executed("当前没有片段选择"));
        }
        expand_clip_link_groups(sequence, &mut clip_ids);
        Ok(clip_ids.into_iter().collect())
    }

    fn can_edit_selected_clip_links(&self, kind: ClipLinkEditKind) -> bool {
        let Some(sequence) = self.active_sequence() else {
            return false;
        };
        let request = ClipLinkEditRequest::new(kind, self.selected_clip_ids_for_edit());
        assess_clip_link_edit(sequence, &request).is_ok_and(|assessment| assessment.would_change)
    }

    fn edit_selected_clip_links(&mut self, kind: ClipLinkEditKind) -> mondrian_core::Result<()> {
        let primary_clip_id = self.selected_clips().first().map(|selection| selection.clip_id);
        let request = ClipLinkEditRequest::new(kind, self.selected_clip_ids_for_edit());
        let assessment = self
            .active_sequence()
            .ok_or_else(|| selection_edit_error("当前无序列"))
            .and_then(|sequence| {
            assess_clip_link_edit(sequence, &request)
                .map_err(|error| selection_edit_error(error.to_string()))
        })?;
        if !assessment.would_change {
            return Err(selection_not_executed("当前选择不会改变 Clip Link Group"));
        }

        let description = match kind {
            ClipLinkEditKind::Link => "链接剪辑",
            ClipLinkEditKind::Unlink => "取消链接剪辑",
        };
        let outcome = self.commit_active_sequence_edit(description, |sequence| {
            apply_clip_link_edit(sequence, &request)
                .map_err(|error| selection_edit_error(error.to_string()))
        })?;
        let mut selections = self
            .active_sequence()
            .map(|sequence| {
                outcome
                    .affected_clip_ids
                    .iter()
                    .filter_map(|clip_id| resolve_clip_selection(sequence, *clip_id))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let Some(primary_clip_id) = primary_clip_id {
            if let Some(index) =
                selections.iter().position(|selection| selection.clip_id == primary_clip_id)
            {
                selections.swap(0, index);
            }
        }
        self.replace_clip_selection(selections);
        self.set_status_hint(
            match kind {
                ClipLinkEditKind::Link => "已链接所选剪辑",
                ClipLinkEditKind::Unlink => "已取消所选剪辑链接",
            },
            false,
        );
        Ok(())
    }

    fn prepare_selected_clip_trim(
        &self,
        edge: TimelineTrimPayloadEdge,
    ) -> mondrian_core::Result<(Vec<ClipId>, TrimEdge, i64)> {
        let clip_ids = self.expanded_selected_clip_ids()?;
        let target_frame = match edge {
            TimelineTrimPayloadEdge::In => self.current_frame(),
            TimelineTrimPayloadEdge::Out => self.current_frame().saturating_add(1),
        };
        let trim_edge = match edge {
            TimelineTrimPayloadEdge::In => TrimEdge::In,
            TimelineTrimPayloadEdge::Out => TrimEdge::Out,
        };
        let sequence = self.active_sequence().ok_or_else(|| selection_edit_error("当前无序列"))?;
        for clip_id in &clip_ids {
            if !can_trim_clip_edge(sequence, *clip_id, trim_edge, target_frame)? {
                return Err(selection_not_executed(
                    "所选 Clip 或其链接成员无法全部修剪到当前播放头",
                ));
            }
        }
        Ok((clip_ids, trim_edge, target_frame))
    }

    fn trim_selected_clips_to_playhead(
        &mut self,
        edge: TimelineTrimPayloadEdge,
    ) -> mondrian_core::Result<()> {
        let (clip_ids, trim_edge, target_frame) = self.prepare_selected_clip_trim(edge)?;
        let changed = self.trim_clips_bulk_to_frame(&clip_ids, trim_edge, target_frame)?;
        if changed == 0 {
            return Err(selection_not_executed("修剪没有改变任何 Clip"));
        }
        Ok(())
    }

    fn prepare_selected_roll(&self) -> mondrian_core::Result<(ClipId, i64)> {
        let clip_ids = self.selected_clip_ids_for_edit();
        let [clip_id] = clip_ids.as_slice() else {
            return Err(selection_not_executed("滚动编辑要求且仅允许选择一个 Clip"));
        };
        let target_frame = self.current_frame();
        let sequence = self.active_sequence().ok_or_else(|| selection_edit_error("当前无序列"))?;
        if !can_roll_cut_for_clip(sequence, *clip_id, target_frame)? {
            return Err(selection_not_executed(
                "未找到可滚动切点，或播放头不在可滚动范围",
            ));
        }
        Ok((*clip_id, target_frame))
    }

    fn roll_selected_cut_to_playhead(&mut self) -> mondrian_core::Result<()> {
        let (clip_id, target_frame) = self.prepare_selected_roll()?;
        if !self.roll_cut_to_frame(clip_id, target_frame)? {
            return Err(selection_not_executed("滚动编辑没有改变切点"));
        }
        Ok(())
    }

    fn prepare_selected_clips_enabled(&self, enabled: bool) -> mondrian_core::Result<Vec<ClipId>> {
        let clip_ids = self.selected_clip_ids_for_edit();
        if clip_ids.is_empty() {
            return Err(selection_not_executed("当前没有可修改的 Clip 选择"));
        }
        let sequence = self.active_sequence().ok_or_else(|| selection_edit_error("当前无序列"))?;
        let mut would_change = false;
        for clip_id in &clip_ids {
            let (_, _, locked) = find_clip_track_lock(sequence, *clip_id)
                .ok_or_else(|| selection_edit_error(format!("Clip 不存在: {clip_id}")))?;
            if locked {
                return Err(selection_not_executed("选择包含锁定 Track 上的 Clip"));
            }
            let clip = find_clip(sequence, *clip_id)
                .ok_or_else(|| selection_edit_error(format!("Clip 不存在: {clip_id}")))?;
            would_change |= clip.is_disabled == enabled;
        }
        if !would_change {
            return Err(selection_not_executed("所选 Clip 已具有请求的启用状态"));
        }
        Ok(clip_ids)
    }

    fn set_selected_clips_enabled(&mut self, enabled: bool) -> mondrian_core::Result<()> {
        let clip_ids = self.prepare_selected_clips_enabled(enabled)?;
        self.commit_active_sequence_edit("切换片段启用状态", |sequence| {
            for clip_id in &clip_ids {
                let (_, _, locked) = find_clip_track_lock(sequence, *clip_id)
                    .ok_or_else(|| selection_edit_error(format!("Clip 不存在: {clip_id}")))?;
                if locked {
                    return Err(selection_edit_error("选择包含锁定 Track 上的 Clip"));
                }
            }
            let changed = clip_ids
                .iter()
                .filter(|clip_id| set_clip_disabled(sequence, **clip_id, !enabled))
                .count();
            if changed == 0 {
                return Err(selection_not_executed("所选 Clip 已具有请求的启用状态"));
            }
            Ok(())
        })
    }
}

fn selection_not_executed(reason: impl Into<String>) -> MondrianError {
    MondrianError::ActionNotExecuted {
        action: "timeline_edit_selection".to_owned(),
        reason: reason.into(),
    }
}

fn selection_edit_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "timeline_edit_selection".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::product_action::TimelineTrimPayloadEdge;
    use crate::app::ui_actions::{
        timeline_roll_selected_cut_to_playhead_action, timeline_set_selected_clips_enabled_action,
        timeline_trim_selected_clips_to_playhead_action,
    };
    use crate::app::SelectedClipRef;
    use mondrian_core::{AssetId, ClipLinkGroupId, FramePosition, Rational, TimelineTime};
    use mondrian_timeline::{Clip, Sequence};

    fn tt(frame: i64, time_base: Rational) -> TimelineTime {
        TimelineTime::from_frame_position(FramePosition::new(frame, time_base)).expect("test time")
    }

    #[test]
    fn selected_enabled_edit_changes_mixed_state_once_and_rejects_repeat() {
        let mut sequence = Sequence::new("selection enabled");
        let time_base = sequence.time_base();
        let track_id = sequence.video_tracks[0].id;
        let first = Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base)).expect("clip");
        let first_id = first.id;
        let mut second =
            Clip::new(AssetId::new(), tt(20, time_base), tt(10, time_base)).expect("clip");
        let second_id = second.id;
        second.is_disabled = true;
        sequence.video_tracks[0].add_clip(first).expect("first");
        sequence.video_tracks[0].add_clip(second).expect("second");

        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        state.selection.selected_clips = vec![
            SelectedClipRef { track_id, is_video_track: true, clip_id: first_id },
            SelectedClipRef { track_id, is_video_track: true, clip_id: second_id },
        ];

        let generation = state.project_author_generation();
        state
            .dispatch_action(timeline_set_selected_clips_enabled_action(true))
            .expect("enable mixed selection");
        assert_eq!(state.project_author_generation(), generation + 1);
        let sequence = state.active_sequence().expect("sequence");
        assert!(sequence.video_tracks[0].clips.iter().all(|clip| !clip.is_disabled));

        let generation = state.project_author_generation();
        let error = state
            .dispatch_action(timeline_set_selected_clips_enabled_action(true))
            .expect_err("repeated enabled state is a no-op");
        assert!(matches!(error, MondrianError::ActionNotExecuted { .. }));
        assert_eq!(state.project_author_generation(), generation);
    }

    #[test]
    fn linked_trim_admission_includes_unselected_locked_members() {
        let mut sequence = Sequence::new("linked trim");
        let time_base = sequence.time_base();
        let video_track_id = sequence.video_tracks[0].id;
        let group = ClipLinkGroupId::new();
        let mut video =
            Clip::new(AssetId::new(), tt(0, time_base), tt(20, time_base)).expect("video");
        let video_id = video.id;
        video.link_group = Some(group);
        let mut audio =
            Clip::new(AssetId::new(), tt(0, time_base), tt(20, time_base)).expect("audio");
        audio.link_group = Some(group);
        sequence.video_tracks[0].add_clip(video).expect("video");
        sequence.audio_tracks[0].add_clip(audio).expect("audio");
        sequence.audio_tracks[0].is_locked = true;

        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        state.selection.selected_clips = vec![SelectedClipRef {
            track_id: video_track_id,
            is_video_track: true,
            clip_id: video_id,
        }];
        state.seek(5).expect("seek");
        let edit = TimelineSelectionEdit::TrimClipsToPlayhead { edge: TimelineTrimPayloadEdge::In };
        assert!(!state.can_apply_timeline_selection_edit(edit));

        let generation = state.project_author_generation();
        let error = state
            .dispatch_action(timeline_trim_selected_clips_to_playhead_action(
                TimelineTrimPayloadEdge::In,
            ))
            .expect_err("locked linked member blocks complete edit");
        assert!(matches!(error, MondrianError::ActionNotExecuted { .. }));
        assert_eq!(state.project_author_generation(), generation);
        assert_eq!(
            state.active_sequence().expect("sequence").video_tracks[0].clips[0].position,
            TimelineTime::ZERO
        );
    }

    #[test]
    fn roll_admission_uses_the_same_resolved_cut_as_execution() {
        let mut sequence = Sequence::new("roll");
        let time_base = sequence.time_base();
        let track_id = sequence.video_tracks[0].id;
        let first = Clip::new(AssetId::new(), tt(0, time_base), tt(20, time_base)).expect("first");
        let first_id = first.id;
        let second =
            Clip::new(AssetId::new(), tt(20, time_base), tt(20, time_base)).expect("second");
        sequence.video_tracks[0].add_clip(first).expect("first");
        sequence.video_tracks[0].add_clip(second).expect("second");
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id: first_id }];

        state.seek(20).expect("seek current cut");
        assert!(!state.can_apply_timeline_selection_edit(TimelineSelectionEdit::RollCutToPlayhead));
        state.seek(25).expect("seek changed cut");
        assert!(state.can_apply_timeline_selection_edit(TimelineSelectionEdit::RollCutToPlayhead));
        state
            .dispatch_action(timeline_roll_selected_cut_to_playhead_action())
            .expect("roll admitted cut");
        assert_eq!(
            state.active_sequence().expect("sequence").video_tracks[0].clips[0].duration,
            tt(25, time_base)
        );
    }
}
