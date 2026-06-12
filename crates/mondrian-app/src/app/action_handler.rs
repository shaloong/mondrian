//! Action 派发 —— 桥接 Action 枚举与现有 AppState 方法
//!
//! Stage A 阶段，将 `Action` 映射到 `AppState` 已有的操作方法。
//! 这是过渡方案：后续 Stage 中 `EditorState` 会取代 `AppState` 成为唯一的 dispatch 目标。
//!
//! 当前版本的 action_handler 以最简方式实现：只对已确定存在的方法做桥接，
//! 其余 Action 记录日志后忽略。每个 Stage 逐步增加映射。

use crate::app::ui_actions::{
    TimelineMoveClipPayload, TimelineSeekPayload, TimelineSelectClipPayload,
    TimelineTrimClipPayload, TimelineTrimPayloadEdge, TIMELINE_MOVE_CLIP, TIMELINE_NAMESPACE,
    TIMELINE_SEEK, TIMELINE_SELECT_CLIP, TIMELINE_TRIM_CLIP,
};
use crate::app::{AppState, ClipOverlapMode, SelectedClipRef};
use mondrian_core::Result;
use mondrian_timeline::clip::TrimEdge;

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
                let payload = parse_timeline_payload::<TimelineSelectClipPayload>(name, payload)?;
                self.selection.selected_clips = vec![SelectedClipRef {
                    track_id: payload.track_id,
                    is_video_track: payload.is_video_track,
                    clip_id: payload.clip_id,
                }];
                Ok(())
            }
            TIMELINE_MOVE_CLIP => {
                let payload = parse_timeline_payload::<TimelineMoveClipPayload>(name, payload)?;
                self.move_clip_to_track_with_mode(
                    payload.target_track_id,
                    payload.is_video_track,
                    payload.clip_id,
                    payload.frame,
                    ClipOverlapMode::Overwrite,
                )
            }
            TIMELINE_TRIM_CLIP => {
                let payload = parse_timeline_payload::<TimelineTrimClipPayload>(name, payload)?;
                let edge = match payload.edge {
                    TimelineTrimPayloadEdge::In => TrimEdge::In,
                    TimelineTrimPayloadEdge::Out => TrimEdge::Out,
                };
                self.trim_clips_bulk_to_frame(&[payload.clip_id], edge, payload.frame)
                    .map(|_| ())
            }
            TIMELINE_SEEK => {
                let payload = parse_timeline_payload::<TimelineSeekPayload>(name, payload)?;
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
}

fn parse_timeline_payload<T: serde::de::DeserializeOwned>(
    name: &str,
    payload: serde_json::Value,
) -> Result<T> {
    serde_json::from_value(payload).map_err(|err| {
        mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: format!("timeline_ui_action.{name}"),
            reason: format!("invalid action payload: {err}"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ui_actions::{
        timeline_move_clip_action, timeline_seek_action, timeline_select_clip_action,
        timeline_trim_clip_action,
    };
    use mondrian_core::types::{AssetId, TimeCode};
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
}
