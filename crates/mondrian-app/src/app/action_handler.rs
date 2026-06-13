//! Action 派发 —— 桥接 Action 枚举与现有 AppState 方法
//!
//! Stage A 阶段，将 `Action` 映射到 `AppState` 已有的操作方法。
//! 这是过渡方案：后续 Stage 中 `EditorState` 会取代 `AppState` 成为唯一的 dispatch 目标。
//!
//! 当前版本的 action_handler 以最简方式实现：只对已确定存在的方法做桥接，
//! 其余 Action 记录日志后忽略。每个 Stage 逐步增加映射。

use crate::app::timeline_editing::{find_clip_mut, find_clip_track_lock, set_clip_disabled};
use crate::app::ui_actions::{
    EffectsAddToClipPayload, InspectorClipTransformField, InspectorRemoveEffectPayload,
    InspectorSetClipEnabledPayload, InspectorSetClipOpacityPayload, InspectorSetClipTintPayload,
    InspectorSetClipTransformFieldPayload, InspectorSetEffectEnabledPayload,
    TimelineMoveClipPayload, TimelineSeekPayload, TimelineSelectClipPayload,
    TimelineTrimClipPayload, TimelineTrimPayloadEdge, EFFECTS_ADD_TO_CLIP, EFFECTS_NAMESPACE,
    INSPECTOR_NAMESPACE, INSPECTOR_REMOVE_EFFECT, INSPECTOR_SET_CLIP_ENABLED,
    INSPECTOR_SET_CLIP_OPACITY, INSPECTOR_SET_CLIP_TINT, INSPECTOR_SET_CLIP_TRANSFORM_FIELD,
    INSPECTOR_SET_EFFECT_ENABLED, TIMELINE_MOVE_CLIP, TIMELINE_NAMESPACE, TIMELINE_SEEK,
    TIMELINE_SELECT_CLIP, TIMELINE_TRIM_CLIP,
};
use crate::app::{AppState, ClipOverlapMode, SelectedClipRef};
use glam::Vec2;
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

            // ── 选择（当前 AppState 可表达 clip / mask / animation selection）──
            Action::Select(target) => self.select_from_action(target),
            Action::SelectAll => {
                self.select_all_clips_from_ui();
                Ok(())
            }
            Action::DeselectAll => {
                self.clear_selection_from_ui();
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

            // ── 时间线编辑（复用已有 undoable 命令层）────────────────────
            Action::DeleteSelection => self.delete_selected_clips_from_ui(),

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
            Action::Custom { namespace, name, payload } if namespace == EFFECTS_NAMESPACE => {
                self.dispatch_effects_ui_action(&name, payload)
            }

            // ── 尚未实现的操作（Stage B-F 逐步添加）─────────────────────
            _ => {
                tracing::debug!(target: "mondrian::action", "Action not yet implemented: {:?}", action);
                Ok(())
            }
        }
    }

    fn select_from_action(
        &mut self,
        target: mondrian_editor_state::action::SelectionTarget,
    ) -> Result<()> {
        match target {
            mondrian_editor_state::action::SelectionTarget::Clip(clip_id) => {
                let Some(seq) = self.sequence.as_ref() else {
                    return Err(missing_sequence_error("select_clip"));
                };
                let (track_id, is_video_track, _) = find_clip_track_lock(seq, clip_id)
                    .ok_or_else(|| missing_clip_error("select_clip", clip_id))?;
                self.selection.selected_clips =
                    vec![SelectedClipRef { track_id, is_video_track, clip_id }];
                self.selection.selected_mask = None;
                self.clear_animation_selection();
                Ok(())
            }
            mondrian_editor_state::action::SelectionTarget::AllClips => {
                self.select_all_clips_from_ui();
                Ok(())
            }
            mondrian_editor_state::action::SelectionTarget::Track(track_id) => {
                tracing::debug!(
                    target: "mondrian::action",
                    "Track selection is not represented in AppState yet: {track_id}"
                );
                Ok(())
            }
            mondrian_editor_state::action::SelectionTarget::AllTracks => {
                tracing::debug!(
                    target: "mondrian::action",
                    "Track selection is not represented in AppState yet"
                );
                Ok(())
            }
        }
    }

    fn select_all_clips_from_ui(&mut self) {
        let Some(seq) = self.sequence.as_ref() else {
            self.clear_selection_from_ui();
            return;
        };

        let selections = seq
            .video_tracks
            .iter()
            .flat_map(|track| {
                track.clips.iter().map(move |clip| SelectedClipRef {
                    track_id: track.id,
                    is_video_track: true,
                    clip_id: clip.id,
                })
            })
            .chain(seq.audio_tracks.iter().flat_map(|track| {
                track.clips.iter().map(move |clip| SelectedClipRef {
                    track_id: track.id,
                    is_video_track: false,
                    clip_id: clip.id,
                })
            }))
            .collect::<Vec<_>>();

        self.selection.selected_clips = selections;
        self.selection.selected_mask = None;
        self.clear_animation_selection();
    }

    fn clear_selection_from_ui(&mut self) {
        self.selection.selected_clips.clear();
        self.selection.selected_mask = None;
        self.clear_animation_selection();
    }

    fn delete_selected_clips_from_ui(&mut self) -> Result<()> {
        let selections = self
            .selection
            .selected_clips
            .iter()
            .map(|selection| {
                (
                    selection.track_id,
                    selection.is_video_track,
                    selection.clip_id,
                )
            })
            .collect::<Vec<_>>();
        if selections.is_empty() {
            return Ok(());
        }

        self.remove_clips_bulk(&selections, false)?;
        self.selection.selected_clips.clear();
        Ok(())
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
            INSPECTOR_SET_CLIP_TRANSFORM_FIELD => {
                let payload = parse_ui_payload::<InspectorSetClipTransformFieldPayload>(
                    "inspector_ui_action",
                    name,
                    payload,
                )?;
                self.set_clip_transform_field_from_ui(
                    payload.clip.clip_id,
                    payload.field,
                    payload.value,
                )
            }
            INSPECTOR_SET_EFFECT_ENABLED => {
                let payload = parse_ui_payload::<InspectorSetEffectEnabledPayload>(
                    "inspector_ui_action",
                    name,
                    payload,
                )?;
                self.set_clip_effect_enabled(
                    SelectedClipRef {
                        track_id: payload.clip.track_id,
                        is_video_track: payload.clip.is_video_track,
                        clip_id: payload.clip.clip_id,
                    },
                    payload.effect_id,
                    payload.enabled,
                )
                .map(|_| ())
            }
            INSPECTOR_REMOVE_EFFECT => {
                let payload = parse_ui_payload::<InspectorRemoveEffectPayload>(
                    "inspector_ui_action",
                    name,
                    payload,
                )?;
                self.remove_effect_from_clip(
                    SelectedClipRef {
                        track_id: payload.clip.track_id,
                        is_video_track: payload.clip.is_video_track,
                        clip_id: payload.clip.clip_id,
                    },
                    payload.effect_id,
                )
                .map(|_| ())
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

    fn dispatch_effects_ui_action(&mut self, name: &str, payload: serde_json::Value) -> Result<()> {
        match name {
            EFFECTS_ADD_TO_CLIP => {
                let payload = parse_ui_payload::<EffectsAddToClipPayload>(
                    "effects_ui_action",
                    name,
                    payload,
                )?;
                self.add_effect_to_clip(
                    SelectedClipRef {
                        track_id: payload.clip.track_id,
                        is_video_track: payload.clip.is_video_track,
                        clip_id: payload.clip.clip_id,
                    },
                    payload.effect_type,
                )
                .map(|_| ())
            }
            _ => {
                tracing::debug!(
                    target: "mondrian::action",
                    "Unknown self-hosted effects action: {name}"
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

    fn set_clip_transform_field_from_ui(
        &mut self,
        clip_id: ClipId,
        field: InspectorClipTransformField,
        value: f32,
    ) -> Result<()> {
        if !value.is_finite() {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "inspector_set_clip_transform_field".to_string(),
                reason: "transform value must be finite".to_string(),
            });
        }

        let Some(seq) = self.sequence.as_mut() else {
            return Err(missing_sequence_error("inspector_set_clip_transform_field"));
        };
        let before = seq.clone();
        let playhead = seq.playhead;
        let changed = {
            let clip = find_clip_mut(seq, clip_id)
                .ok_or_else(|| missing_clip_error("inspector_set_clip_transform_field", clip_id))?;
            match field {
                InspectorClipTransformField::PositionX => {
                    let mut position = clip.transform.get_position(playhead);
                    if (position.x - value).abs() < f32::EPSILON {
                        false
                    } else {
                        position.x = value;
                        clip.apply_property_mutation(PropertyMutation::SetStaticValue {
                            path: Transform2D::POSITION_PATH.to_string(),
                            value: PropertyValue::Vec2(position),
                        })?;
                        true
                    }
                }
                InspectorClipTransformField::PositionY => {
                    let mut position = clip.transform.get_position(playhead);
                    if (position.y - value).abs() < f32::EPSILON {
                        false
                    } else {
                        position.y = value;
                        clip.apply_property_mutation(PropertyMutation::SetStaticValue {
                            path: Transform2D::POSITION_PATH.to_string(),
                            value: PropertyValue::Vec2(position),
                        })?;
                        true
                    }
                }
                InspectorClipTransformField::ScalePercent => {
                    let scale = (value.max(0.0)) / 100.0;
                    let scale = Vec2::splat(scale);
                    if (clip.transform.get_scale(playhead) - scale).length_squared() < f32::EPSILON
                    {
                        false
                    } else {
                        clip.apply_property_mutation(PropertyMutation::SetStaticValue {
                            path: Transform2D::SCALE_PATH.to_string(),
                            value: PropertyValue::Vec2(scale),
                        })?;
                        true
                    }
                }
                InspectorClipTransformField::RotationDegrees => {
                    let current = clip
                        .transform
                        .to_property_bag()
                        .evaluate(
                            Transform2D::ROTATION_PATH,
                            mondrian_core::automation::timecode_to_ticks(playhead),
                        )
                        .and_then(|value| value.as_f32())
                        .unwrap_or(0.0);
                    if (current - value).abs() < f32::EPSILON {
                        false
                    } else {
                        clip.apply_property_mutation(PropertyMutation::SetStaticValue {
                            path: Transform2D::ROTATION_PATH.to_string(),
                            value: PropertyValue::Float(value),
                        })?;
                        true
                    }
                }
            }
        };
        if changed {
            self.record_timeline_edit_snapshot("调整片段变换", before);
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
        effects_add_to_clip_action, inspector_remove_effect_action,
        inspector_set_clip_enabled_action, inspector_set_clip_opacity_action,
        inspector_set_clip_tint_action, inspector_set_clip_transform_field_action,
        inspector_set_effect_enabled_action, timeline_move_clip_action, timeline_seek_action,
        timeline_select_clip_action, timeline_trim_clip_action, EffectsAddToClipPayload,
        InspectorClipRefPayload, InspectorClipTransformField, InspectorRemoveEffectPayload,
        InspectorSetClipEnabledPayload, InspectorSetClipOpacityPayload,
        InspectorSetClipTintPayload, InspectorSetClipTransformFieldPayload,
        InspectorSetEffectEnabledPayload,
    };
    use mondrian_core::types::{AssetId, MaskId, TimeCode};
    use mondrian_core::Color;
    use mondrian_effects::EffectType;
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
    fn dispatch_select_clip_resolves_selection_from_clip_id() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        state.selection.selected_mask = Some((MaskId::new(), clip_id, track_id));

        state
            .dispatch_action(mondrian_editor_state::Action::Select(
                mondrian_editor_state::action::SelectionTarget::Clip(clip_id),
            ))
            .expect("select clip");

        assert_eq!(
            state.selection.selected_clips,
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }]
        );
        assert!(state.selection.selected_mask.is_none());
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_select_all_selects_video_and_audio_clips() {
        let (mut state, video_track_id, video_clip_id) = state_with_two_video_tracks();
        let sequence = state.sequence.as_mut().expect("sequence");
        let audio_track_id = sequence.add_audio_track();
        let tb = sequence.time_base();
        let audio_clip = Clip::new(AssetId::new(), TimeCode::new(30, tb), TimeCode::new(10, tb));
        let audio_clip_id = audio_clip.id;
        sequence
            .audio_track_mut(audio_track_id)
            .expect("audio track")
            .add_clip(audio_clip)
            .expect("add audio");

        state
            .dispatch_action(mondrian_editor_state::Action::SelectAll)
            .expect("select all");

        assert_eq!(
            state.selection.selected_clips,
            vec![
                SelectedClipRef {
                    track_id: video_track_id,
                    is_video_track: true,
                    clip_id: video_clip_id,
                },
                SelectedClipRef {
                    track_id: audio_track_id,
                    is_video_track: false,
                    clip_id: audio_clip_id,
                },
            ]
        );
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_deselect_all_clears_clip_and_mask_selection() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];
        state.selection.selected_mask = Some((MaskId::new(), clip_id, track_id));

        state
            .dispatch_action(mondrian_editor_state::Action::DeselectAll)
            .expect("deselect all");

        assert!(state.selection.selected_clips.is_empty());
        assert!(state.selection.selected_mask.is_none());
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_delete_selection_removes_selected_clip() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];

        state
            .dispatch_action(mondrian_editor_state::Action::DeleteSelection)
            .expect("delete selection");

        let sequence = state.sequence.as_ref().expect("sequence");
        assert!(sequence.video_tracks[0].clips.is_empty());
        assert!(state.selection.selected_clips.is_empty());
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_delete_selection_preserves_locked_track() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        state.sequence.as_mut().expect("sequence").video_tracks[0].is_locked = true;
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];

        let err = state
            .dispatch_action(mondrian_editor_state::Action::DeleteSelection)
            .expect_err("locked track should reject delete");

        assert!(matches!(err, MondrianError::TrackLocked { .. }));
        let sequence = state.sequence.as_ref().expect("sequence");
        assert_eq!(sequence.video_tracks[0].clips.len(), 1);
        assert_eq!(
            state.selection.selected_clips,
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }]
        );
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_delete_selection_removes_linked_audio_clip() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("linked edit");
        let audio_track_id = sequence.add_audio_track();
        let tb = sequence.time_base();
        let video_track_id = sequence.video_tracks[0].id;

        let mut video_clip =
            Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(20, tb));
        let mut audio_clip =
            Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(20, tb));
        let video_clip_id = video_clip.id;
        let audio_clip_id = audio_clip.id;
        video_clip.linked_clip = Some(audio_clip_id);
        audio_clip.linked_clip = Some(video_clip_id);

        sequence.video_tracks[0].add_clip(video_clip).expect("add video");
        sequence
            .audio_track_mut(audio_track_id)
            .expect("audio track")
            .add_clip(audio_clip)
            .expect("add audio");
        state.sequence = Some(sequence);
        state.selection.selected_clips = vec![SelectedClipRef {
            track_id: video_track_id,
            is_video_track: true,
            clip_id: video_clip_id,
        }];

        state
            .dispatch_action(mondrian_editor_state::Action::DeleteSelection)
            .expect("delete linked selection");

        let sequence = state.sequence.as_ref().expect("sequence");
        assert!(sequence.video_tracks[0].clips.is_empty());
        assert!(sequence.audio_tracks[0].clips.is_empty());
        assert!(state.selection.selected_clips.is_empty());
        assert!(state.can_undo_action());
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

    #[test]
    fn dispatch_inspector_ui_sets_clip_transform_fields() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        let clip_ref = inspector_clip_payload(track_id, clip_id);

        for (field, value) in [
            (InspectorClipTransformField::PositionX, 128.0),
            (InspectorClipTransformField::PositionY, 72.0),
            (InspectorClipTransformField::ScalePercent, 150.0),
            (InspectorClipTransformField::RotationDegrees, -12.5),
        ] {
            state
                .dispatch_action(inspector_set_clip_transform_field_action(
                    InspectorSetClipTransformFieldPayload { clip: clip_ref, field, value },
                ))
                .expect("dispatch transform");
        }

        let sequence = state.sequence.as_ref().expect("sequence");
        let clip = &sequence.video_tracks[0].clips[0];
        assert_eq!(
            clip.transform.get_position(sequence.playhead),
            glam::Vec2::new(128.0, 72.0)
        );
        assert_eq!(
            clip.transform.get_scale(sequence.playhead),
            glam::Vec2::splat(1.5)
        );
        let rotation = clip
            .transform
            .to_property_bag()
            .evaluate(
                Transform2D::ROTATION_PATH,
                mondrian_core::automation::timecode_to_ticks(sequence.playhead),
            )
            .and_then(|value| value.as_f32())
            .expect("rotation value");
        assert!((rotation + 12.5).abs() < f32::EPSILON);
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_effects_ui_adds_effect_to_selected_clip() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();

        state
            .dispatch_action(effects_add_to_clip_action(EffectsAddToClipPayload {
                clip: inspector_clip_payload(track_id, clip_id),
                effect_type: EffectType::GaussianBlur,
            }))
            .expect("dispatch add effect");

        let clip = &state.sequence.as_ref().expect("sequence").video_tracks[0].clips[0];
        assert_eq!(clip.effects.len(), 1);
        assert_eq!(clip.effects[0].effect_type, EffectType::GaussianBlur);
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_inspector_ui_sets_effect_enabled_state() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        let effect: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::GaussianBlur);
        let effect_id = effect.id;
        state.sequence.as_mut().expect("sequence").video_tracks[0].clips[0].add_effect_node(effect);

        state
            .dispatch_action(inspector_set_effect_enabled_action(
                InspectorSetEffectEnabledPayload {
                    clip: inspector_clip_payload(track_id, clip_id),
                    effect_id,
                    enabled: false,
                },
            ))
            .expect("dispatch effect enabled");

        let effect =
            &state.sequence.as_ref().expect("sequence").video_tracks[0].clips[0].effects[0];
        assert_eq!(effect.id, effect_id);
        assert!(!effect.is_enabled);
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_inspector_ui_removes_effect_instance() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        let remove_effect: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::GaussianBlur);
        let keep_effect: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::Sharpen);
        let remove_id = remove_effect.id;
        let keep_id = keep_effect.id;
        let clip = &mut state.sequence.as_mut().expect("sequence").video_tracks[0].clips[0];
        clip.add_effect_node(remove_effect);
        clip.add_effect_node(keep_effect);

        state
            .dispatch_action(inspector_remove_effect_action(
                InspectorRemoveEffectPayload {
                    clip: inspector_clip_payload(track_id, clip_id),
                    effect_id: remove_id,
                },
            ))
            .expect("dispatch remove effect");

        let effects = &state.sequence.as_ref().expect("sequence").video_tracks[0].clips[0].effects;
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].id, keep_id);
        assert!(state.can_undo_action());
    }
}
