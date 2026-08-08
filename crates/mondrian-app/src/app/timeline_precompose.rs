//! Deep Module for Project-scoped Precompose authoring.
//!
//! The public Product intent carries only the new Sequence name. This Module
//! resolves the latest Clip selection, expands Link Groups, prepares exact
//! geometry and lock obligations, builds parent/child candidates, and commits
//! both aggregates through one Project Author Transaction.

use super::product_action::TimelinePrecomposeSelectionPayload;
use super::{expand_clip_selection_units, AppState};
use mondrian_core::{
    AudioTransitionId, ClipId, ClipLinkGroupId, MondrianError, TimelineTime, TimelineTimeRange,
    VideoTransitionId,
};
use mondrian_timeline::{Clip, Sequence};
use std::collections::{HashMap, HashSet};

const PRECOMPOSE_STEP: &str = "timeline_precompose_selection";

struct PrecomposePlan {
    selected_ids: HashSet<ClipId>,
    min_time: TimelineTime,
    max_time: TimelineTime,
    selected_video_track_indices: Vec<usize>,
    selected_audio_track_indices: Vec<usize>,
    target_video_track_index: Option<usize>,
    target_audio_track_index: Option<usize>,
}

impl AppState {
    /// Whether the current selection can become one nested Composition.
    pub fn can_precompose_selection(&self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        let Some(sequence) = self.active_sequence() else {
            return false;
        };
        let selected_ids = self.selected_clips().iter().map(|item| item.clip_id).collect();
        prepare_precompose_plan(sequence, selected_ids).is_ok()
    }

    /// Precompose the latest current selection and select its replacement Clip.
    pub fn precompose_selection(
        &mut self,
        payload: TimelinePrecomposeSelectionPayload,
    ) -> mondrian_core::Result<()> {
        let name = payload.name.trim();
        if name.is_empty() {
            return Err(precompose_error("嵌套序列名称不能为空"));
        }
        let clip_ids = self.selected_clips().iter().map(|item| item.clip_id).collect::<Vec<_>>();
        let nested_clip_id = self.precompose_clips_as_sequence(&clip_ids, name)?;
        self.select_clip_for_action(PRECOMPOSE_STEP, nested_clip_id)?;
        self.set_status_hint(format!("已创建嵌套序列“{name}”"), false);
        Ok(())
    }

    /// Build one nested Composition from stable Clip identities.
    ///
    /// This lower-level Interface exists for deterministic authoring gates; UI
    /// callers use [`Self::precompose_selection`] so they cannot copy Track facts.
    pub(crate) fn precompose_clips_as_sequence(
        &mut self,
        clip_ids: &[ClipId],
        name: &str,
    ) -> mondrian_core::Result<ClipId> {
        let name = name.trim();
        if name.is_empty() {
            return Err(precompose_error("嵌套序列名称不能为空"));
        }
        let project_before = self
            .authoring
            .as_ref()
            .ok_or_else(|| precompose_error("当前没有打开的项目"))?
            .document()
            .clone();

        let (parent, nested_sequence, nested_clip_id) = {
            let source = self.active_sequence().ok_or_else(|| precompose_error("当前无序列"))?;
            let plan = prepare_precompose_plan(source, clip_ids.iter().copied().collect())?;
            let mut parent = source.clone();
            let mut nested_sequence = Sequence::with_settings(name, parent.settings.clone())
                .map_err(|error| precompose_error(format!("创建嵌套序列失败: {error}")))?;
            nested_sequence.role = mondrian_timeline::sequence::SequenceRole::NestedComposition;
            while nested_sequence.video_tracks.len() < parent.video_tracks.len() {
                nested_sequence.add_video_track();
            }
            while nested_sequence.audio_tracks.len() < parent.audio_tracks.len() {
                nested_sequence.add_audio_track();
            }

            let source_audio_scopes = parent.audio_program.processing_scopes.clone();
            let source_audio_transitions = parent.audio_program.transitions.clone();
            let source_video_transitions = parent.video_transitions.clone();
            let mut audio_edit_ids = HashMap::new();

            for (track_index, track) in parent.video_tracks.iter().enumerate() {
                for clip in track.clips.iter().filter(|clip| plan.selected_ids.contains(&clip.id)) {
                    let mut nested_clip = clip.clone();
                    nested_clip.position = clip.position.checked_sub(plan.min_time)?;
                    nested_sequence.video_tracks[track_index].add_clip(nested_clip)?;
                }
            }
            for (track_index, track) in parent.audio_tracks.iter().enumerate() {
                for clip in track.clips.iter().filter(|clip| plan.selected_ids.contains(&clip.id)) {
                    let mut nested_clip = clip.clone();
                    nested_clip.position = clip.position.checked_sub(plan.min_time)?;
                    audio_edit_ids.extend(
                        nested_sequence
                            .fork_audio_clip_authoring(&mut nested_clip, &source_audio_scopes)?,
                    );
                    nested_sequence.audio_tracks[track_index].add_clip(nested_clip)?;
                }
            }
            for mut transition in source_audio_transitions {
                let (Some(left), Some(right)) = (
                    audio_edit_ids.get(&transition.left).copied(),
                    audio_edit_ids.get(&transition.right).copied(),
                ) else {
                    continue;
                };
                transition.id = AudioTransitionId::new();
                transition.left = left;
                transition.right = right;
                transition.sequence_range = TimelineTimeRange::new(
                    transition.sequence_range.start.checked_sub(plan.min_time)?,
                    transition.sequence_range.duration,
                )?;
                nested_sequence.audio_program.transitions.push(transition);
            }
            for mut transition in source_video_transitions {
                if !plan.selected_ids.contains(&transition.left)
                    || !plan.selected_ids.contains(&transition.right)
                {
                    continue;
                }
                transition.id = VideoTransitionId::new();
                transition.properties.fork_author_identities();
                transition.sequence_range = TimelineTimeRange::new(
                    transition.sequence_range.start.checked_sub(plan.min_time)?,
                    transition.sequence_range.duration,
                )?;
                nested_sequence.video_transitions.push(transition);
            }
            nested_sequence.fork_clip_link_groups_for_sequence_duplicate();

            for track_index in &plan.selected_video_track_indices {
                parent.video_tracks[*track_index]
                    .clips
                    .retain(|clip| !plan.selected_ids.contains(&clip.id));
            }
            for track_index in &plan.selected_audio_track_indices {
                parent.audio_tracks[*track_index]
                    .clips
                    .retain(|clip| !plan.selected_ids.contains(&clip.id));
            }
            parent.compact_structural_references();

            let duration = plan.max_time.checked_sub(plan.min_time)?;
            let has_nested_video =
                nested_sequence.video_tracks.iter().any(|track| !track.clips.is_empty());
            let has_nested_audio =
                nested_sequence.audio_tracks.iter().any(|track| !track.clips.is_empty());
            let link_group = (has_nested_video && has_nested_audio).then(ClipLinkGroupId::new);
            let nested_video = if has_nested_video {
                let mut clip = Clip::new_nested_sequence(
                    nested_sequence.id,
                    plan.min_time,
                    duration,
                    Some(nested_sequence.name.clone()),
                )?;
                clip.link_group = link_group;
                Some(clip)
            } else {
                None
            };
            let nested_audio = if has_nested_audio {
                let mut clip = Clip::new_nested_sequence(
                    nested_sequence.id,
                    plan.min_time,
                    duration,
                    Some(nested_sequence.name.clone()),
                )?;
                clip.link_group = link_group;
                Some(clip)
            } else {
                None
            };
            let nested_clip_id = nested_video
                .as_ref()
                .or(nested_audio.as_ref())
                .map(|clip| clip.id)
                .ok_or_else(|| precompose_error("选区没有可放置到嵌套序列的内容"))?;
            if let Some(video_clip) = nested_video {
                let track_index = plan
                    .target_video_track_index
                    .ok_or_else(|| precompose_error("视频选区缺少父序列目标轨道"))?;
                parent.video_tracks[track_index].add_clip(video_clip)?;
            }
            if let Some(audio_clip) = nested_audio {
                let output_id = nested_sequence
                    .audio_program
                    .outputs
                    .first()
                    .map(|output| output.id)
                    .ok_or_else(|| precompose_error("嵌套序列缺少音频 Program Output"))?;
                let track_index = plan
                    .target_audio_track_index
                    .ok_or_else(|| precompose_error("音频选区缺少父序列目标轨道"))?;
                let track_id = parent.audio_tracks[track_index].id;
                parent.add_nested_audio_clip(track_id, audio_clip, output_id)?;
            }
            (parent, nested_sequence, nested_clip_id)
        };

        let mut project_after = project_before.clone();
        let parent_sequence_id = parent.id;
        *project_after
            .sequences
            .sequence_mut(parent_sequence_id)
            .ok_or_else(|| precompose_error("父序列在项目事务中丢失"))? = parent;
        project_after.sequences.add_sequence(nested_sequence)?;
        self.commit_project_snapshot_command("预合成为嵌套序列", project_before, project_after)?;
        Ok(nested_clip_id)
    }
}

fn prepare_precompose_plan(
    sequence: &Sequence,
    mut selected_ids: HashSet<ClipId>,
) -> mondrian_core::Result<PrecomposePlan> {
    if selected_ids.is_empty() {
        return Err(precompose_error("没有选中的片段"));
    }
    expand_clip_selection_units(sequence, &mut selected_ids);

    let mut min_time = None;
    let mut max_time = TimelineTime::ZERO;
    let mut found_ids = HashSet::new();
    let mut selected_video_track_indices = Vec::new();
    let mut selected_audio_track_indices = Vec::new();
    let mut target_video_track_index = None;
    let mut target_audio_track_index = None;
    for (track_index, track) in sequence.video_tracks.iter().enumerate() {
        let selected = track.clips.iter().filter(|clip| selected_ids.contains(&clip.id));
        let mut track_selected = false;
        for clip in selected {
            if track.is_locked {
                return Err(MondrianError::TrackLocked { track_id: track.id.to_string() });
            }
            found_ids.insert(clip.id);
            track_selected = true;
            min_time =
                Some(min_time.map_or(clip.position, |time: TimelineTime| time.min(clip.position)));
            max_time = max_time.max(clip.end_position()?);
            target_video_track_index.get_or_insert(track_index);
        }
        if track_selected {
            selected_video_track_indices.push(track_index);
        }
    }
    for (track_index, track) in sequence.audio_tracks.iter().enumerate() {
        let selected = track.clips.iter().filter(|clip| selected_ids.contains(&clip.id));
        let mut track_selected = false;
        for clip in selected {
            if track.is_locked {
                return Err(MondrianError::TrackLocked { track_id: track.id.to_string() });
            }
            found_ids.insert(clip.id);
            track_selected = true;
            min_time =
                Some(min_time.map_or(clip.position, |time: TimelineTime| time.min(clip.position)));
            max_time = max_time.max(clip.end_position()?);
            target_audio_track_index.get_or_insert(track_index);
        }
        if track_selected {
            selected_audio_track_indices.push(track_index);
        }
    }
    if found_ids != selected_ids {
        if let Some(missing) = selected_ids.difference(&found_ids).next() {
            return Err(MondrianError::ClipNotFound { clip_id: missing.to_string() });
        }
        return Err(precompose_error("选区解析结果与请求的 Clip 身份闭包不一致"));
    }
    let min_time = min_time.ok_or_else(|| precompose_error("选区没有有效时长"))?;
    if max_time <= min_time {
        return Err(precompose_error("选区没有有效时长"));
    }
    Ok(PrecomposePlan {
        selected_ids,
        min_time,
        max_time,
        selected_video_track_indices,
        selected_audio_track_indices,
        target_video_track_index,
        target_audio_track_index,
    })
}

fn precompose_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: PRECOMPOSE_STEP.to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::SelectedClipRef;
    use mondrian_core::{AssetId, Rational};

    fn tt(frame: i64, time_base: Rational) -> TimelineTime {
        TimelineTime::new(frame * time_base.num, time_base.den).expect("valid test time")
    }

    #[test]
    fn linked_precompose_admission_includes_unselected_locked_members() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("parent");
        let time_base = sequence.time_base();
        let first_track_id = sequence.video_tracks[0].id;
        let second_track_id = sequence.add_video_track();
        let second_track_index = sequence
            .video_tracks
            .iter()
            .position(|track| track.id == second_track_id)
            .expect("new Track must remain addressable by stable identity");
        let group_id = ClipLinkGroupId::new();
        let mut first =
            Clip::new(AssetId::new(), tt(0, time_base), tt(20, time_base)).expect("first clip");
        first.link_group = Some(group_id);
        let first_clip_id = first.id;
        let mut second =
            Clip::new(AssetId::new(), tt(0, time_base), tt(20, time_base)).expect("second clip");
        second.link_group = Some(group_id);
        sequence.video_tracks[0].add_clip(first).expect("add first clip");
        sequence.video_tracks[second_track_index]
            .add_clip(second)
            .expect("add second clip");
        sequence.video_tracks[second_track_index].is_locked = true;
        state.test_set_sequence(Some(sequence));
        state.selection.selected_clips = vec![SelectedClipRef {
            track_id: first_track_id,
            is_video_track: true,
            clip_id: first_clip_id,
        }];
        let generation = state.project_author_generation();

        assert!(!state.can_precompose_selection("Nested"));
        let error = state
            .precompose_selection(TimelinePrecomposeSelectionPayload { name: "Nested".to_owned() })
            .expect_err("locked linked member must reject complete Precompose");

        assert!(
            matches!(
                &error,
                MondrianError::TrackLocked { track_id }
                    if track_id == &second_track_id.to_string()
            ),
            "unexpected rejection: {error:?}"
        );
        assert_eq!(state.project_author_generation(), generation);
        assert!(!state.can_undo_action());
    }
}
