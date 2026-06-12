//! Action 派发 —— 桥接 Action 枚举与现有 AppState 方法
//!
//! Stage A 阶段，将 `Action` 映射到 `AppState` 已有的操作方法。
//! 这是过渡方案：后续 Stage 中 `EditorState` 会取代 `AppState` 成为唯一的 dispatch 目标。
//!
//! 当前版本的 action_handler 以最简方式实现：只对已确定存在的方法做桥接，
//! 其余 Action 记录日志后忽略。每个 Stage 逐步增加映射。

use crate::app::timeline_editing::{find_clip_mut, set_clip_disabled};
use crate::app::ui_actions::{
    InspectorSetClipEnabledPayload, InspectorSetClipOpacityPayload, InspectorSetClipTintPayload,
    TimelineMoveClipPayload, TimelineSeekPayload, TimelineSelectClipPayload,
    TimelineTrimClipPayload, TimelineTrimPayloadEdge, INSPECTOR_NAMESPACE,
    INSPECTOR_SET_CLIP_ENABLED, INSPECTOR_SET_CLIP_OPACITY, INSPECTOR_SET_CLIP_TINT,
    TIMELINE_MOVE_CLIP, TIMELINE_NAMESPACE, TIMELINE_SEEK, TIMELINE_SELECT_CLIP,
    TIMELINE_TRIM_CLIP,
};
use crate::app::{AppState, ClipOverlapMode, SelectedClipRef};
use mondrian_core::automation::{PropertyHost, PropertyMutation, PropertyValue};
use mondrian_core::types::ClipId;
use mondrian_core::{MondrianError, Result};
use mondrian_timeline::clip::{Clip, Transform2D, TrimEdge};

impl AppState {
    /// 派发 Action，修改内部状态
    ///
    /// 每个 Action 映射到 AppState 已有的方法。
    /// 已实现的直接调用，未实现的记录 trace 日志后返回 Ok。
    pub fn dispatch_action(&mut self, action: mondrian_editor_state::Action) -> Result<()> {
        use mondrian_editor_state::Action;

        match action {
            // ── 播放控制（已有方法）───────────────────────────────────────
            Action::Play => {
                self.play();
                Ok(())
            }
            Action::Pause => {
                self.pause();
                Ok(())
            }
            Action::TogglePlay => {
                if self.is_playing() {
                    self.pause();
                } else {
                    self.play();
                }
                Ok(())
            }
            Action::Seek(timecode) => {
                self.seek(timecode.frame);
                Ok(())
            }
            Action::StepForward => {
                self.seek(self.current_frame() + 1);
                Ok(())
            }
            Action::StepBack => {
                self.seek((self.current_frame() - 1).max(0));
                Ok(())
            }
            Action::GoToStart => {
                self.seek(0);
                Ok(())
            }
            Action::GoToEnd => {
                let end = self.last_content_frame();
                if end >= 0 {
                    self.seek(end);
                }
                Ok(())
            }

            // ── 撤销/重做（已有方法）─────────────────────────────────────
            Action::Undo => {
                let _ = self.undo_timeline();
                Ok(())
            }
            Action::Redo => {
                let _ = self.redo_timeline();
                Ok(())
            }

            // ── 项目操作 ──────────────────────────────────────────────────
            Action::SaveProject => {
                self.save_project().map_err(mondrian_core::MondrianError::Other)?;
                Ok(())
            }
            Action::CloseProject => {
                self.close_project();
                Ok(())
            }

            Action::Custom { namespace, name, payload } if namespace == TIMELINE_NAMESPACE => {
                self.dispatch_timeline_ui_action(&name, payload)
            }
            Action::Custom { namespace, name, payload } if namespace == INSPECTOR_NAMESPACE => {
                self.dispatch_inspector_ui_action(&name, payload)
            }

            // ── 尚未实现的操作（Stage B-F 逐步添加）─────────────────────
            _ => {
                tracing::debug!(target: "mondrian::action", "Action not yet implemented: {:?}", action);
                Ok(())
            }
        }
    }

    pub fn can_undo_action(&self) -> bool {
        self.cmd_history.can_undo()
    }

    pub fn can_redo_action(&self) -> bool {
        self.cmd_history.can_redo()
    }

    fn dispatch_timeline_ui_action(
        &mut self,
        name: &str,
        payload: serde_json::Value,
    ) -> Result<()> {
        match name {
            TIMELINE_SELECT_CLIP => {
                let payload = parse_ui_payload::<TimelineSelectClipPayload>(
                    "timeline_ui_action",
                    name,
                    payload,
                )?;
                self.selection.selected_clips = vec![SelectedClipRef {
                    track_id: payload.track_id,
                    is_video_track: payload.is_video_track,
                    clip_id: payload.clip_id,
                }];
                Ok(())
            }
            TIMELINE_MOVE_CLIP => {
                let payload = parse_ui_payload::<TimelineMoveClipPayload>(
                    "timeline_ui_action",
                    name,
                    payload,
                )?;
                self.move_clip_to_track_with_mode(
                    payload.target_track_id,
                    payload.is_video_track,
                    payload.clip_id,
                    payload.frame,
                    ClipOverlapMode::Overwrite,
                )
            }
            TIMELINE_TRIM_CLIP => {
                let payload = parse_ui_payload::<TimelineTrimClipPayload>(
                    "timeline_ui_action",
                    name,
                    payload,
                )?;
                let edge = match payload.edge {
                    TimelineTrimPayloadEdge::In => TrimEdge::In,
                    TimelineTrimPayloadEdge::Out => TrimEdge::Out,
                };
                self.trim_clips_bulk_to_frame(&[payload.clip_id], edge, payload.frame)
                    .map(|_| ())
            }
            TIMELINE_SEEK => {
                let payload =
                    parse_ui_payload::<TimelineSeekPayload>("timeline_ui_action", name, payload)?;
                self.seek(payload.frame.max(0));
                Ok(())
            }
            _ => {
                tracing::debug!(
                    target: "mondrian::action",
                    "Unknown self-hosted timeline action: {name}"
                );
                Ok(())
            }
        }
    }

    fn dispatch_inspector_ui_action(
        &mut self,
        name: &str,
        payload: serde_json::Value,
    ) -> Result<()> {
        match name {
            INSPECTOR_SET_CLIP_ENABLED => {
                let payload = parse_ui_payload::<InspectorSetClipEnabledPayload>(
                    "inspector_ui_action",
                    name,
                    payload,
                )?;
                self.set_clip_enabled_from_ui(payload.clip.clip_id, payload.enabled)
            }
            INSPECTOR_SET_CLIP_OPACITY => {
                let payload = parse_ui_payload::<InspectorSetClipOpacityPayload>(
                    "inspector_ui_action",
                    name,
                    payload,
                )?;
                self.set_clip_opacity_from_ui(payload.clip.clip_id, payload.opacity_percent)
            }
            INSPECTOR_SET_CLIP_TINT => {
                let payload = parse_ui_payload::<InspectorSetClipTintPayload>(
                    "inspector_ui_action",
                    name,
                    payload,
                )?;
                self.set_clip_tint_from_ui(payload.clip.clip_id, payload.color)
            }
            _ => {
                tracing::debug!(
                    target: "mondrian::action",
                    "Unknown self-hosted inspector action: {name}"
                );
                Ok(())
            }
        }
    }

    fn set_clip_enabled_from_ui(&mut self, clip_id: ClipId, enabled: bool) -> Result<()> {
        let Some(seq) = self.sequence.as_mut() else {
            return Err(missing_sequence_error("inspector_set_clip_enabled"));
        };
        let before = seq.clone();
        let changed = set_clip_disabled(seq, clip_id, !enabled);
        if changed {
            self.record_timeline_edit_snapshot("切换片段启用状态", before);
            Ok(())
        } else if clip_exists(seq, clip_id) {
            Ok(())
        } else {
            Err(missing_clip_error("inspector_set_clip_enabled", clip_id))
        }
    }

    fn set_clip_opacity_from_ui(&mut self, clip_id: ClipId, opacity_percent: f32) -> Result<()> {
        let Some(seq) = self.sequence.as_mut() else {
            return Err(missing_sequence_error("inspector_set_clip_opacity"));
        };
        let opacity = (opacity_percent / 100.0).clamp(0.0, 1.0);
        let before = seq.clone();
        let playhead = seq.playhead;
        let changed = {
            let clip = find_clip_mut(seq, clip_id)
                .ok_or_else(|| missing_clip_error("inspector_set_clip_opacity", clip_id))?;
            if (clip.transform.evaluate_opacity(playhead) - opacity).abs() < f32::EPSILON {
                false
            } else {
                clip.apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: Transform2D::OPACITY_PATH.to_string(),
                    value: PropertyValue::Float(opacity),
                })?;
                true
            }
        };
        if changed {
            self.record_timeline_edit_snapshot("调整片段不透明度", before);
        }
        Ok(())
    }

    fn set_clip_tint_from_ui(
        &mut self,
        clip_id: ClipId,
        color: mondrian_core::Color,
    ) -> Result<()> {
        let Some(seq) = self.sequence.as_mut() else {
            return Err(missing_sequence_error("inspector_set_clip_tint"));
        };
        let before = seq.clone();
        let changed = {
            let clip = find_clip_mut(seq, clip_id)
                .ok_or_else(|| missing_clip_error("inspector_set_clip_tint", clip_id))?;
            if clip.solid_color == Some(color) {
                false
            } else {
                clip.apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: Clip::SOLID_COLOR_PATH.to_string(),
                    value: PropertyValue::Color(color),
                })?;
                true
            }
        };
        if changed {
            self.record_timeline_edit_snapshot("调整片段颜色", before);
        }
        Ok(())
    }
}

fn parse_ui_payload<T: serde::de::DeserializeOwned>(
    step_prefix: &str,
    name: &str,
    payload: serde_json::Value,
) -> Result<T> {
    serde_json::from_value(payload).map_err(|err| MondrianError::WorkflowStepFailed {
        step_id: format!("{step_prefix}.{name}"),
        reason: format!("invalid action payload: {err}"),
    })
}

fn missing_sequence_error(step_id: &'static str) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: step_id.to_string(),
        reason: "当前没有活动序列".to_string(),
    }
}

fn missing_clip_error(step_id: &'static str, clip_id: ClipId) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: step_id.to_string(),
        reason: format!("片段不存在: {clip_id}"),
    }
}

fn clip_exists(seq: &mondrian_timeline::sequence::Sequence, clip_id: ClipId) -> bool {
    seq.video_tracks
        .iter()
        .any(|track| track.clips.iter().any(|clip| clip.id == clip_id))
        || seq
            .audio_tracks
            .iter()
            .any(|track| track.clips.iter().any(|clip| clip.id == clip_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ui_actions::{
        inspector_set_clip_enabled_action, inspector_set_clip_opacity_action,
        inspector_set_clip_tint_action, timeline_move_clip_action, timeline_seek_action,
        timeline_select_clip_action, timeline_trim_clip_action, InspectorClipRefPayload,
        InspectorSetClipEnabledPayload, InspectorSetClipOpacityPayload,
        InspectorSetClipTintPayload,
    };
    use mondrian_core::types::{AssetId, TimeCode};
    use mondrian_core::Color;
    use mondrian_timeline::clip::Clip;
    use mondrian_timeline::sequence::Sequence;

    fn state_with_two_video_tracks() -> (
        AppState,
        mondrian_core::types::TrackId,
        mondrian_core::types::ClipId,
    ) {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("edit");
        sequence.add_video_track();
        let tb = sequence.time_base();
        let track_id = sequence.video_tracks[0].id;
        let clip = Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(20, tb));
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("add clip");
        state.sequence = Some(sequence);
        (state, track_id, clip_id)
    }

    fn inspector_clip_payload(
        track_id: mondrian_core::types::TrackId,
        clip_id: mondrian_core::types::ClipId,
    ) -> InspectorClipRefPayload {
        InspectorClipRefPayload { track_id, is_video_track: true, clip_id }
    }

    #[test]
    fn dispatch_timeline_ui_selects_clip() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();

        state
            .dispatch_action(timeline_select_clip_action(TimelineSelectClipPayload {
                track_id,
                is_video_track: true,
                clip_id,
            }))
            .expect("dispatch select");

        assert_eq!(
            state.selection.selected_clips,
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }]
        );
    }

    #[test]
    fn dispatch_timeline_ui_seek_updates_playback_frame() {
        let (mut state, _, _) = state_with_two_video_tracks();

        state.dispatch_action(timeline_seek_action(33)).expect("dispatch seek");

        assert_eq!(state.current_frame(), 33);
    }

    #[test]
    fn dispatch_timeline_ui_moves_clip_to_target_track() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let target_track_id = state.sequence.as_ref().unwrap().video_tracks[1].id;

        state
            .dispatch_action(timeline_move_clip_action(TimelineMoveClipPayload {
                target_track_id,
                is_video_track: true,
                clip_id,
                frame: 42,
            }))
            .expect("dispatch move");

        let sequence = state.sequence.as_ref().expect("sequence");
        assert!(sequence.video_tracks[0].clips.is_empty());
        let moved = &sequence.video_tracks[1].clips[0];
        assert_eq!(moved.id, clip_id);
        assert_eq!(moved.position.frame, 42);
    }

    #[test]
    fn dispatch_timeline_ui_trims_clip_edge() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();

        state
            .dispatch_action(timeline_trim_clip_action(TimelineTrimClipPayload {
                clip_id,
                edge: TimelineTrimPayloadEdge::In,
                frame: 16,
            }))
            .expect("dispatch trim");

        let sequence = state.sequence.as_ref().expect("sequence");
        let clip = &sequence.video_tracks[0].clips[0];
        assert_eq!(clip.position.frame, 16);
        assert_eq!(clip.duration.frame, 14);
    }

    #[test]
    fn dispatch_inspector_ui_sets_clip_enabled_state() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();

        state
            .dispatch_action(inspector_set_clip_enabled_action(
                InspectorSetClipEnabledPayload {
                    clip: inspector_clip_payload(track_id, clip_id),
                    enabled: false,
                },
            ))
            .expect("dispatch enabled");

        let clip = &state.sequence.as_ref().expect("sequence").video_tracks[0].clips[0];
        assert!(clip.is_disabled);
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_inspector_ui_sets_clip_opacity() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();

        state
            .dispatch_action(inspector_set_clip_opacity_action(
                InspectorSetClipOpacityPayload {
                    clip: inspector_clip_payload(track_id, clip_id),
                    opacity_percent: 42.0,
                },
            ))
            .expect("dispatch opacity");

        let sequence = state.sequence.as_ref().expect("sequence");
        let clip = &sequence.video_tracks[0].clips[0];
        assert!((clip.transform.evaluate_opacity(sequence.playhead) - 0.42).abs() < 1.0e-6);
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_inspector_ui_sets_clip_tint_color() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        let color = Color::from_rgba8(8, 144, 220, 192);

        state
            .dispatch_action(inspector_set_clip_tint_action(
                InspectorSetClipTintPayload {
                    clip: inspector_clip_payload(track_id, clip_id),
                    color,
                },
            ))
            .expect("dispatch tint");

        let clip = &state.sequence.as_ref().expect("sequence").video_tracks[0].clips[0];
        assert_eq!(clip.solid_color, Some(color));
        assert!(state.can_undo_action());
    }
}
