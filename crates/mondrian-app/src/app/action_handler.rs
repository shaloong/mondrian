//! Action 派发 —— 桥接 Action 枚举与现有 AppState 方法
//!
//! Stage A 阶段，将 `Action` 映射到 `AppState` 已有的操作方法。
//! 这是过渡方案：后续 Stage 中 `EditorState` 会取代 `AppState` 成为唯一的 dispatch 目标。
//!
//! 当前版本的 action_handler 以最简方式实现：只对已确定存在的方法做桥接，
//! 其余 Action 记录日志后忽略。每个 Stage 逐步增加映射。

use crate::app::timeline_editing::{
    find_clip, find_clip_mut, find_clip_track_lock, set_clip_disabled,
};
use crate::app::ui_actions::{
    AssetsPrepareDragPayload, EffectsAddToClipPayload, InspectorClipTransformField,
    InspectorRemoveEffectPayload, InspectorSetClipEnabledPayload, InspectorSetClipOpacityPayload,
    InspectorSetClipTintPayload, InspectorSetClipTransformFieldPayload,
    InspectorSetEffectEnabledPayload, TimelineMoveClipPayload, TimelineSeekPayload,
    TimelineSelectClipPayload, TimelineTrimClipPayload, TimelineTrimPayloadEdge, ASSETS_NAMESPACE,
    ASSETS_PREPARE_DRAG, EFFECTS_ADD_TO_CLIP, EFFECTS_NAMESPACE, INSPECTOR_NAMESPACE,
    INSPECTOR_REMOVE_EFFECT, INSPECTOR_SET_CLIP_ENABLED, INSPECTOR_SET_CLIP_OPACITY,
    INSPECTOR_SET_CLIP_TINT, INSPECTOR_SET_CLIP_TRANSFORM_FIELD, INSPECTOR_SET_EFFECT_ENABLED,
    TIMELINE_MOVE_CLIP, TIMELINE_NAMESPACE, TIMELINE_SEEK, TIMELINE_SELECT_CLIP,
    TIMELINE_TRIM_CLIP,
};
use crate::app::{AppClipboardKind, AppState, ClipOverlapMode, SelectedClipRef};
use glam::Vec2;
use mondrian_assets::AssetKind;
use mondrian_core::automation::{PropertyHost, PropertyMutation, PropertyValue};
use mondrian_core::events::AppEvent;
use mondrian_core::types::{ClipId, TimeCode};
use mondrian_core::{MondrianError, Result};
use mondrian_timeline::clip::{Clip, Transform2D, TrimEdge};
use std::path::PathBuf;
use std::time::Duration;

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

            // ── 剪贴板（动画关键帧优先，否则使用 timeline clip clipboard）──
            Action::Copy => self.copy_from_action(),
            Action::Cut => self.cut_from_action(),
            Action::Paste => self.paste_from_action(),
            Action::Duplicate => self.duplicate_from_action(),

            // ── 时间线编辑（复用已有 undoable 命令层）────────────────────
            Action::DeleteSelection => self.delete_selected_clips_from_ui(),
            Action::SplitClipAtPlayhead => self.split_at_playhead().map(|_| ()),
            Action::NudgeClip { clip_id, delta_frames } => {
                self.nudge_clip_from_action(clip_id, delta_frames)
            }
            Action::MoveClipToTrack { clip_id, target_track, position } => {
                self.move_clip_to_track_from_action(clip_id, target_track, position.frame)
            }
            Action::TrimClipStart { clip_id, new_source_in } => {
                self.trim_clip_source_from_action(clip_id, TrimEdge::In, new_source_in)
            }
            Action::TrimClipEnd { clip_id, new_source_out } => {
                self.trim_clip_source_from_action(clip_id, TrimEdge::Out, new_source_out)
            }

            // ── 效果（复用 clip-level undoable 命令）──────────────────────
            Action::RemoveEffect { clip_id, effect_id } => {
                self.remove_effect_from_action(clip_id, effect_id)
            }
            Action::ReorderEffects { clip_id, from, to } => {
                self.reorder_effects_from_action(clip_id, from, to)
            }

            // ── 项目操作 ──────────────────────────────────────────────────
            Action::OpenProject(path) => self.open_project_from_action(path),
            Action::SaveProject => {
                self.save_project().map_err(mondrian_core::MondrianError::Other)?;
                Ok(())
            }
            Action::SaveProjectAs(path) => self.save_project_as_from_action(path),
            Action::CloseProject => {
                self.close_project();
                Ok(())
            }
            Action::ImportMedia(paths) => self.import_media_from_action(paths),

            Action::Custom { namespace, name, payload } if namespace == TIMELINE_NAMESPACE => {
                self.dispatch_timeline_ui_action(&name, payload)
            }
            Action::Custom { namespace, name, payload } if namespace == INSPECTOR_NAMESPACE => {
                self.dispatch_inspector_ui_action(&name, payload)
            }
            Action::Custom { namespace, name, payload } if namespace == EFFECTS_NAMESPACE => {
                self.dispatch_effects_ui_action(&name, payload)
            }
            Action::Custom { namespace, name, payload } if namespace == ASSETS_NAMESPACE => {
                self.dispatch_assets_ui_action(&name, payload)
            }

            // ── 尚未实现的操作（Stage B-F 逐步添加）─────────────────────
            _ => {
                tracing::debug!(target: "mondrian::action", "Action not yet implemented: {:?}", action);
                Ok(())
            }
        }
    }

    fn copy_from_action(&mut self) -> Result<()> {
        let Some(selection) = self.selection.selected_clips.first().copied() else {
            return self.copy_selected_clips_to_clipboard().map(|_| ());
        };
        if self.copy_selected_animation_keyframes(selection)? {
            Ok(())
        } else {
            self.copy_selected_clips_to_clipboard().map(|_| ())
        }
    }

    fn cut_from_action(&mut self) -> Result<()> {
        self.cut_selected_clips_to_clipboard().map(|_| ())
    }

    fn open_project_from_action(&mut self, path: PathBuf) -> Result<()> {
        if path.as_os_str().is_empty() {
            return Ok(());
        }
        self.open_project_file(path).map_err(|err| {
            let reason = err.to_string();
            self.set_status_hint(format!("打开项目失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "open_project".to_string(), reason }
        })?;
        self.set_status_hint("项目已打开", false);
        Ok(())
    }

    fn save_project_as_from_action(&mut self, path: PathBuf) -> Result<()> {
        if path.as_os_str().is_empty() {
            return Ok(());
        }
        if !self.has_open_project() {
            let reason = "当前无可另存项目".to_string();
            self.set_status_hint(reason.clone(), true);
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "save_project_as".to_string(),
                reason,
            });
        }
        let display_path = super::ensure_project_extension(path.clone());
        self.save_project_file_as(path).map_err(|err| {
            let reason = err.to_string();
            self.set_status_hint(format!("另存为失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "save_project_as".to_string(), reason }
        })?;
        self.set_status_hint(format!("项目已另存为：{}", display_path.display()), false);
        Ok(())
    }

    fn import_media_from_action(&mut self, paths: Vec<PathBuf>) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }

        let library = self.asset_library.clone().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("导入失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "import_media".to_string(), reason }
        })?;

        self.clear_status_hint();
        let mut imported_count = 0usize;
        let mut proxy_count = 0usize;
        let mut failures = Vec::new();

        for path in paths {
            match library.import_media_file(&path) {
                Ok(asset_id) => {
                    imported_count += 1;
                    let mut proxy_started = false;
                    if self.auto_proxy_enabled {
                        if let Ok(Some(asset)) = library.get_asset(asset_id) {
                            if matches!(asset.kind, AssetKind::Video) {
                                self.set_asset_proxy_mode(asset_id, true);
                                spawn_proxy_generation(asset_id, asset.path);
                                proxy_started = true;
                            } else {
                                self.set_asset_proxy_mode(asset_id, false);
                            }
                        }
                    } else {
                        self.set_asset_proxy_mode(asset_id, false);
                    }
                    if proxy_started {
                        proxy_count += 1;
                    }
                    self.event_bus.publish(AppEvent::AssetImported { asset_id });
                }
                Err(err) => failures.push(format!("{}: {err}", path.display())),
            }
        }

        if imported_count > 0 {
            let mut message = format!("已导入 {imported_count} 个媒体文件");
            if proxy_count > 0 {
                message.push_str(&format!("，{proxy_count} 个后台生成代理"));
            }
            if !failures.is_empty() {
                message.push_str(&format!("，{} 个失败", failures.len()));
                tracing::warn!(
                    target: "mondrian::action",
                    "media import completed with failures: {}",
                    failures.join("; ")
                );
            }
            self.set_status_hint(message, !failures.is_empty());
            let _ = self.save_project_file();
            return Ok(());
        }

        let reason = failures.first().cloned().unwrap_or_else(|| "未导入任何媒体文件".to_string());
        self.set_status_hint(format!("导入失败：{reason}"), true);
        Err(MondrianError::WorkflowStepFailed { step_id: "import_media".to_string(), reason })
    }

    fn duplicate_from_action(&mut self) -> Result<()> {
        self.duplicate_selected_clips_after_selection().map(|_| ())
    }

    fn paste_from_action(&mut self) -> Result<()> {
        match self.active_clipboard_kind {
            Some(AppClipboardKind::AnimationKeyframes) => self
                .paste_animation_keyframes_from_action()
                .or_else(|_| self.paste_clip_clipboard_at_playhead().map(|_| ())),
            Some(AppClipboardKind::Clips) => self.paste_clip_clipboard_at_playhead().map(|_| ()),
            None => {
                if self.has_animation_clipboard() {
                    self.paste_animation_keyframes_from_action()
                } else {
                    self.paste_clip_clipboard_at_playhead().map(|_| ())
                }
            }
        }
    }

    fn paste_animation_keyframes_from_action(&mut self) -> Result<()> {
        let Some(selection) = self.selection.selected_clips.first().copied() else {
            return Ok(());
        };
        let destination_time = self
            .current_time_code()
            .map(mondrian_core::automation::timecode_to_ticks)
            .unwrap_or(0);
        self.paste_animation_keyframes(selection, destination_time).map(|_| ())
    }

    fn nudge_clip_from_action(&mut self, clip_id: ClipId, delta_frames: i64) -> Result<()> {
        if delta_frames == 0 {
            return Ok(());
        }
        let (track_id, is_video_track, frame) = self.clip_action_location("nudge_clip", clip_id)?;
        self.move_clip_with_snapshot(
            track_id,
            is_video_track,
            clip_id,
            frame.saturating_add(delta_frames).max(0),
        )
    }

    fn move_clip_to_track_from_action(
        &mut self,
        clip_id: ClipId,
        target_track_id: mondrian_core::types::TrackId,
        frame: i64,
    ) -> Result<()> {
        let (_track_id, is_video_track, _frame) =
            self.clip_action_location("move_clip_to_track", clip_id)?;
        self.move_clip_with_snapshot(target_track_id, is_video_track, clip_id, frame.max(0))
    }

    fn trim_clip_source_from_action(
        &mut self,
        clip_id: ClipId,
        edge: TrimEdge,
        source_time: TimeCode,
    ) -> Result<()> {
        let target_frame = {
            let Some(seq) = self.sequence.as_ref() else {
                return Err(missing_sequence_error("trim_clip_source"));
            };
            let clip = find_clip(seq, clip_id)
                .ok_or_else(|| missing_clip_error("trim_clip_source", clip_id))?;
            source_trim_target_frame(clip, edge, source_time)?
        };
        self.trim_clips_bulk_to_frame(&[clip_id], edge, target_frame).map(|_| ())
    }

    fn clip_action_location(
        &self,
        step_id: &'static str,
        clip_id: ClipId,
    ) -> Result<(mondrian_core::types::TrackId, bool, i64)> {
        let Some(seq) = self.sequence.as_ref() else {
            return Err(missing_sequence_error(step_id));
        };
        let (track_id, is_video_track, _) = find_clip_track_lock(seq, clip_id)
            .ok_or_else(|| missing_clip_error(step_id, clip_id))?;
        let frame = find_clip(seq, clip_id)
            .ok_or_else(|| missing_clip_error(step_id, clip_id))?
            .position
            .frame;
        Ok((track_id, is_video_track, frame))
    }

    fn move_clip_with_snapshot(
        &mut self,
        target_track_id: mondrian_core::types::TrackId,
        is_video_track: bool,
        clip_id: ClipId,
        frame: i64,
    ) -> Result<()> {
        let Some(seq) = self.sequence.as_ref() else {
            return Err(missing_sequence_error("move_clip"));
        };
        let (current_track_id, current_is_video_track, current_frame) =
            self.clip_action_location("move_clip", clip_id)?;
        if current_track_id == target_track_id
            && current_is_video_track == is_video_track
            && current_frame == frame
        {
            return Ok(());
        }

        let linked_clip_id = find_clip(seq, clip_id).and_then(|clip| clip.linked_clip);
        let before = seq.clone();
        self.move_clip_to_track_with_mode(
            target_track_id,
            is_video_track,
            clip_id,
            frame,
            ClipOverlapMode::Overwrite,
        )?;
        if let Some(linked_clip_id) = linked_clip_id {
            self.refresh_selected_clip_locations(&[clip_id, linked_clip_id]);
        } else {
            self.refresh_selected_clip_locations(&[clip_id]);
        }
        self.record_timeline_edit_snapshot("移动片段", before);
        Ok(())
    }

    fn refresh_selected_clip_locations(&mut self, clip_ids: &[ClipId]) {
        let Some(seq) = self.sequence.as_ref() else {
            return;
        };
        let updates = clip_ids
            .iter()
            .filter_map(|clip_id| {
                find_clip_track_lock(seq, *clip_id)
                    .map(|(track_id, is_video_track, _)| (*clip_id, track_id, is_video_track))
            })
            .collect::<Vec<_>>();
        for selection in &mut self.selection.selected_clips {
            if let Some((_, track_id, is_video_track)) =
                updates.iter().find(|(clip_id, _, _)| *clip_id == selection.clip_id)
            {
                selection.track_id = *track_id;
                selection.is_video_track = *is_video_track;
            }
        }
    }

    fn remove_effect_from_action(
        &mut self,
        clip_id: ClipId,
        effect_id: mondrian_core::types::EffectId,
    ) -> Result<()> {
        let (track_id, is_video_track, _) = self.clip_action_location("remove_effect", clip_id)?;
        self.remove_effect_from_clip(
            SelectedClipRef { track_id, is_video_track, clip_id },
            effect_id,
        )
        .map(|_| ())
    }

    fn reorder_effects_from_action(
        &mut self,
        clip_id: ClipId,
        from: usize,
        to: usize,
    ) -> Result<()> {
        let (track_id, is_video_track, _) =
            self.clip_action_location("reorder_effects", clip_id)?;
        self.reorder_effects_for_clip(
            SelectedClipRef { track_id, is_video_track, clip_id },
            from,
            to,
        )
        .map(|_| ())
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

    fn dispatch_assets_ui_action(&mut self, name: &str, payload: serde_json::Value) -> Result<()> {
        match name {
            ASSETS_PREPARE_DRAG => {
                let payload = parse_ui_payload::<AssetsPrepareDragPayload>(
                    "assets_ui_action",
                    name,
                    payload,
                )?;
                self.prepare_asset_drag_from_ui(payload)
            }
            _ => {
                tracing::debug!(
                    target: "mondrian::action",
                    "Unknown self-hosted assets action: {name}"
                );
                Ok(())
            }
        }
    }

    fn prepare_asset_drag_from_ui(&mut self, payload: AssetsPrepareDragPayload) -> Result<()> {
        let library = self.asset_library.clone().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("素材准备失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "assets_prepare_drag".into(), reason }
        })?;
        let asset = library.get_asset(payload.asset_id)?.ok_or_else(|| {
            let reason = format!("素材不存在：{}", payload.asset_id);
            self.set_status_hint(format!("素材准备失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "assets_prepare_drag".into(), reason }
        })?;
        let duration = if asset.media_info.duration > Duration::ZERO
            && !matches!(asset.kind, AssetKind::AdjustmentLayer)
        {
            asset.media_info.duration
        } else {
            self.default_adjustment_layer_drag_duration()
        };
        let has_linked_audio = matches!(asset.kind, AssetKind::Video) && asset.media_info.has_audio;
        let lane = match asset.kind {
            AssetKind::Audio => "音频轨",
            AssetKind::Video | AssetKind::AdjustmentLayer | AssetKind::SolidColor => "视频轨",
        };
        self.begin_drag_asset(
            asset.id,
            asset.name.clone(),
            asset.kind,
            duration,
            has_linked_audio,
        );
        self.set_status_hint(format!("已准备拖放：{}（释放到{lane}）", asset.name), false);
        Ok(())
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

fn source_trim_target_frame(clip: &Clip, edge: TrimEdge, source_time: TimeCode) -> Result<i64> {
    match edge {
        TrimEdge::In if source_time.frame >= clip.source_out.frame => {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "trim_clip_source".to_string(),
                reason: "source in must be before current source out".to_string(),
            });
        }
        TrimEdge::Out if source_time.frame <= clip.source_in.frame => {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "trim_clip_source".to_string(),
                reason: "source out must be after current source in".to_string(),
            });
        }
        _ => {}
    }

    let speed = clip.speed.evaluate_multiplier(TimeCode::new(0, clip.position.time_base));
    if !speed.is_finite() || speed <= f64::EPSILON {
        return Err(MondrianError::WorkflowStepFailed {
            step_id: "trim_clip_source".to_string(),
            reason: "source trim requires a positive finite speed multiplier".to_string(),
        });
    }

    let source_delta = source_time.frame.saturating_sub(clip.source_in.frame);
    let timeline_delta = (source_delta as f64 / speed).round() as i64;
    Ok(clip.position.frame.saturating_add(timeline_delta).max(0))
}

fn spawn_proxy_generation(asset_id: mondrian_core::types::AssetId, source_path: PathBuf) {
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
        if let Ok(rt) = runtime {
            let generator =
                mondrian_media::ProxyGenerator::new(mondrian_media::ProxyConfig::default());
            let (progress_tx, _progress_rx) = tokio::sync::mpsc::channel(8);
            let _ = rt.block_on(generator.generate(asset_id, source_path, progress_tx));
        }
    });
}

#[cfg(test)]
fn unique_temp_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "mondrian-{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    ))
}

#[cfg(test)]
fn remove_temp_path(path: &std::path::Path) {
    if path.is_dir() {
        let _ = std::fs::remove_dir_all(path);
    } else {
        let _ = std::fs::remove_file(path);
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
        assets_prepare_drag_action, effects_add_to_clip_action, inspector_remove_effect_action,
        inspector_set_clip_enabled_action, inspector_set_clip_opacity_action,
        inspector_set_clip_tint_action, inspector_set_clip_transform_field_action,
        inspector_set_effect_enabled_action, timeline_move_clip_action, timeline_seek_action,
        timeline_select_clip_action, timeline_trim_clip_action, AssetsPrepareDragPayload,
        EffectsAddToClipPayload, InspectorClipRefPayload, InspectorClipTransformField,
        InspectorRemoveEffectPayload, InspectorSetClipEnabledPayload,
        InspectorSetClipOpacityPayload, InspectorSetClipTintPayload,
        InspectorSetClipTransformFieldPayload, InspectorSetEffectEnabledPayload,
    };
    use mondrian_assets::AssetLibrary;
    use mondrian_core::types::{AssetId, MaskId, TimeCode};
    use mondrian_core::Color;
    use mondrian_core::Rational;
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
    fn dispatch_trim_clip_start_uses_source_in_time() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let tb = state.sequence.as_ref().expect("sequence").time_base();

        state
            .dispatch_action(mondrian_editor_state::Action::TrimClipStart {
                clip_id,
                new_source_in: TimeCode::new(5, tb),
            })
            .expect("trim source in");

        let sequence = state.sequence.as_ref().expect("sequence");
        let clip = &sequence.video_tracks[0].clips[0];
        assert_eq!(clip.position.frame, 15);
        assert_eq!(clip.duration.frame, 15);
        assert_eq!(clip.source_in.frame, 5);
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_trim_clip_end_uses_source_out_time() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let tb = state.sequence.as_ref().expect("sequence").time_base();

        state
            .dispatch_action(mondrian_editor_state::Action::TrimClipEnd {
                clip_id,
                new_source_out: TimeCode::new(12, tb),
            })
            .expect("trim source out");

        let sequence = state.sequence.as_ref().expect("sequence");
        let clip = &sequence.video_tracks[0].clips[0];
        assert_eq!(clip.position.frame, 10);
        assert_eq!(clip.duration.frame, 12);
        assert_eq!(clip.source_out.frame, 12);
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_trim_clip_end_rejects_source_out_before_source_in() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let tb = state.sequence.as_ref().expect("sequence").time_base();

        let err = state
            .dispatch_action(mondrian_editor_state::Action::TrimClipEnd {
                clip_id,
                new_source_out: TimeCode::new(0, tb),
            })
            .expect_err("invalid source out should be rejected");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        let sequence = state.sequence.as_ref().expect("sequence");
        assert_eq!(sequence.video_tracks[0].clips[0].duration.frame, 20);
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_import_media_with_empty_paths_is_noop_without_library() {
        let mut state = AppState::new();

        state
            .dispatch_action(mondrian_editor_state::Action::ImportMedia(Vec::new()))
            .expect("empty import should be ignored");

        assert!(state.status_hint.is_none());
    }

    #[test]
    fn dispatch_import_media_reports_missing_asset_library() {
        let mut state = AppState::new();
        let err = state
            .dispatch_action(mondrian_editor_state::Action::ImportMedia(vec![
                PathBuf::from("missing.mov"),
            ]))
            .expect_err("missing asset library should fail");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
    }

    #[test]
    fn dispatch_import_media_rejects_invalid_path_without_adding_assets() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("import-media-library");
        state.asset_library = Some(AssetLibrary::open(library_root.clone()).expect("library"));
        let missing_path = library_root.join("missing.mov");

        let err = state
            .dispatch_action(mondrian_editor_state::Action::ImportMedia(vec![
                missing_path,
            ]))
            .expect_err("invalid media path should fail");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
        let assets = state
            .asset_library
            .as_ref()
            .expect("library")
            .list_assets()
            .expect("list assets");
        assert!(assets.is_empty());

        remove_temp_path(&library_root);
    }

    #[test]
    fn dispatch_assets_prepare_drag_reads_library_asset() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-prepare-drag-library");
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let asset_id = library
            .create_solid_color_asset(Some("Brand Solid"))
            .expect("create solid color asset");
        state.asset_library = Some(library);

        state
            .dispatch_action(assets_prepare_drag_action(AssetsPrepareDragPayload {
                asset_id,
            }))
            .expect("prepare drag");

        let dragging = state.dragging_asset().expect("dragging asset");
        assert_eq!(dragging.asset_id, asset_id);
        assert_eq!(dragging.name, "Brand Solid");
        assert_eq!(dragging.kind, AssetKind::SolidColor);
        assert!(dragging.duration > Duration::ZERO);
        assert!(!dragging.has_linked_audio);
        assert!(state
            .status_hint
            .as_ref()
            .is_some_and(|(message, is_error)| { !*is_error && message.contains("Brand Solid") }));

        remove_temp_path(&library_root);
    }

    #[test]
    fn dispatch_assets_prepare_drag_reports_missing_library() {
        let mut state = AppState::new();

        let err = state
            .dispatch_action(assets_prepare_drag_action(AssetsPrepareDragPayload {
                asset_id: AssetId::new(),
            }))
            .expect_err("missing asset library should fail");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state.dragging_asset().is_none());
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
    }

    #[test]
    fn dispatch_open_project_with_empty_path_is_noop() {
        let mut state = AppState::new();

        state
            .dispatch_action(mondrian_editor_state::Action::OpenProject(PathBuf::new()))
            .expect("empty open path should be ignored");

        assert!(state.status_hint.is_none());
    }

    #[test]
    fn dispatch_open_project_reports_missing_file() {
        let mut state = AppState::new();
        let missing = unique_temp_path("missing-project").join("missing.mdp");

        let err = state
            .dispatch_action(mondrian_editor_state::Action::OpenProject(missing))
            .expect_err("missing project should fail");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
    }

    #[test]
    fn dispatch_save_project_as_requires_open_project() {
        let mut state = AppState::new();
        let target = unique_temp_path("save-as-no-project").join("copy.mdp");

        let err = state
            .dispatch_action(mondrian_editor_state::Action::SaveProjectAs(target))
            .expect_err("save as without project should fail");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
    }

    #[test]
    fn dispatch_save_project_as_writes_copy_and_adds_project_extension() {
        let mut state = AppState::new();
        let root = unique_temp_path("save-as-project");
        let source = root.join("source.mdp");
        state
            .create_new_project_at(source.clone(), "source", 1920, 1080, Rational::new(24, 1))
            .expect("create source project");
        let target_without_extension = root.join("copies").join("copy");
        let expected_target = target_without_extension.with_extension("mdp");

        state
            .dispatch_action(mondrian_editor_state::Action::SaveProjectAs(
                target_without_extension,
            ))
            .expect("save project as");

        assert_eq!(state.current_project_path.as_ref(), Some(&expected_target));
        assert!(expected_target.exists());
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| !*is_error));

        remove_temp_path(&root);
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
    fn dispatch_split_clip_at_playhead_splits_intersecting_clip() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        state.seek(20);

        state
            .dispatch_action(mondrian_editor_state::Action::SplitClipAtPlayhead)
            .expect("split at playhead");

        let sequence = state.sequence.as_ref().expect("sequence");
        let clips = &sequence.video_tracks[0].clips;
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0].id, clip_id);
        assert_eq!(clips[0].position.frame, 10);
        assert_eq!(clips[0].duration.frame, 10);
        assert_eq!(clips[1].position.frame, 20);
        assert_eq!(clips[1].duration.frame, 10);
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_split_clip_at_playhead_ignores_clip_boundary() {
        let (mut state, _, _) = state_with_two_video_tracks();
        state.seek(10);

        state
            .dispatch_action(mondrian_editor_state::Action::SplitClipAtPlayhead)
            .expect("split at clip boundary");

        let sequence = state.sequence.as_ref().expect("sequence");
        assert_eq!(sequence.video_tracks[0].clips.len(), 1);
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_nudge_clip_moves_with_undo_snapshot() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();

        state
            .dispatch_action(mondrian_editor_state::Action::NudgeClip { clip_id, delta_frames: 5 })
            .expect("nudge clip");

        let sequence = state.sequence.as_ref().expect("sequence");
        assert_eq!(sequence.video_tracks[0].clips[0].position.frame, 15);
        assert!(state.can_undo_action());

        assert!(state.undo_timeline().expect("undo nudge"));
        let sequence = state.sequence.as_ref().expect("sequence");
        assert_eq!(sequence.video_tracks[0].clips[0].position.frame, 10);
    }

    #[test]
    fn dispatch_nudge_clip_zero_delta_does_not_enter_undo_history() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();

        state
            .dispatch_action(mondrian_editor_state::Action::NudgeClip { clip_id, delta_frames: 0 })
            .expect("nudge clip");

        let sequence = state.sequence.as_ref().expect("sequence");
        assert_eq!(sequence.video_tracks[0].clips[0].position.frame, 10);
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_move_clip_to_track_updates_selection_location() {
        let (mut state, source_track_id, clip_id) = state_with_two_video_tracks();
        let target_track_id = state.sequence.as_ref().expect("sequence").video_tracks[1].id;
        let tb = state.sequence.as_ref().expect("sequence").time_base();
        state.selection.selected_clips = vec![SelectedClipRef {
            track_id: source_track_id,
            is_video_track: true,
            clip_id,
        }];

        state
            .dispatch_action(mondrian_editor_state::Action::MoveClipToTrack {
                clip_id,
                target_track: target_track_id,
                position: TimeCode::new(42, tb),
            })
            .expect("move clip to track");

        let sequence = state.sequence.as_ref().expect("sequence");
        assert!(sequence.video_tracks[0].clips.is_empty());
        assert_eq!(sequence.video_tracks[1].clips[0].id, clip_id);
        assert_eq!(sequence.video_tracks[1].clips[0].position.frame, 42);
        assert_eq!(
            state.selection.selected_clips,
            vec![SelectedClipRef {
                track_id: target_track_id,
                is_video_track: true,
                clip_id,
            }]
        );
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_move_clip_to_track_refreshes_linked_selection_location() {
        let (mut state, source_video_track_id, video_clip_id) = state_with_two_video_tracks();
        let sequence = state.sequence.as_mut().expect("sequence");
        let target_video_track_id = sequence.video_tracks[1].id;
        let source_audio_track_id = sequence.audio_tracks[0].id;
        sequence.add_audio_track();
        let tb = sequence.time_base();

        let audio_clip = Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(20, tb));
        let audio_clip_id = audio_clip.id;
        sequence.audio_tracks[0].add_clip(audio_clip).expect("add audio");
        sequence.video_tracks[0].clips[0].linked_clip = Some(audio_clip_id);
        sequence.audio_tracks[0].clips[0].linked_clip = Some(video_clip_id);
        state.selection.selected_clips = vec![
            SelectedClipRef {
                track_id: source_video_track_id,
                is_video_track: true,
                clip_id: video_clip_id,
            },
            SelectedClipRef {
                track_id: source_audio_track_id,
                is_video_track: false,
                clip_id: audio_clip_id,
            },
        ];

        state
            .dispatch_action(mondrian_editor_state::Action::MoveClipToTrack {
                clip_id: video_clip_id,
                target_track: target_video_track_id,
                position: TimeCode::new(24, tb),
            })
            .expect("move linked clip to track");

        let sequence = state.sequence.as_ref().expect("sequence");
        assert!(!sequence.audio_tracks[0].clips.iter().any(|clip| clip.id == audio_clip_id));
        let target_audio_track_id = sequence
            .audio_tracks
            .iter()
            .find(|track| track.clips.iter().any(|clip| clip.id == audio_clip_id))
            .map(|track| track.id)
            .expect("linked audio should move to an audio track");
        assert_eq!(
            state.selection.selected_clips,
            vec![
                SelectedClipRef {
                    track_id: target_video_track_id,
                    is_video_track: true,
                    clip_id: video_clip_id,
                },
                SelectedClipRef {
                    track_id: target_audio_track_id,
                    is_video_track: false,
                    clip_id: audio_clip_id,
                },
            ]
        );
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

    #[test]
    fn dispatch_remove_effect_action_removes_effect_instance() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
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
            .dispatch_action(mondrian_editor_state::Action::RemoveEffect {
                clip_id,
                effect_id: remove_id,
            })
            .expect("dispatch remove effect action");

        let effects = &state.sequence.as_ref().expect("sequence").video_tracks[0].clips[0].effects;
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].id, keep_id);
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_remove_effect_action_preserves_locked_track() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let effect: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::GaussianBlur);
        let effect_id = effect.id;
        let sequence = state.sequence.as_mut().expect("sequence");
        sequence.video_tracks[0].clips[0].add_effect_node(effect);
        sequence.video_tracks[0].is_locked = true;

        let err = state
            .dispatch_action(mondrian_editor_state::Action::RemoveEffect { clip_id, effect_id })
            .expect_err("locked track should reject effect removal");

        assert!(matches!(err, MondrianError::TrackLocked { .. }));
        let effects = &state.sequence.as_ref().expect("sequence").video_tracks[0].clips[0].effects;
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].id, effect_id);
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_reorder_effects_action_reorders_with_undo_snapshot() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let first: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::GaussianBlur);
        let second: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::Sharpen);
        let third: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::BasicCorrection);
        let first_id = first.id;
        let second_id = second.id;
        let third_id = third.id;
        let clip = &mut state.sequence.as_mut().expect("sequence").video_tracks[0].clips[0];
        clip.add_effect_node(first);
        clip.add_effect_node(second);
        clip.add_effect_node(third);

        state
            .dispatch_action(mondrian_editor_state::Action::ReorderEffects {
                clip_id,
                from: 0,
                to: usize::MAX,
            })
            .expect("dispatch reorder effects");

        let effects = &state.sequence.as_ref().expect("sequence").video_tracks[0].clips[0].effects;
        assert_eq!(
            effects.iter().map(|effect| effect.id).collect::<Vec<_>>(),
            vec![second_id, third_id, first_id]
        );
        assert!(state.can_undo_action());

        assert!(state.undo_timeline().expect("undo reorder effects"));
        let effects = &state.sequence.as_ref().expect("sequence").video_tracks[0].clips[0].effects;
        assert_eq!(
            effects.iter().map(|effect| effect.id).collect::<Vec<_>>(),
            vec![first_id, second_id, third_id]
        );
    }

    #[test]
    fn dispatch_reorder_effects_action_noop_does_not_enter_undo_history() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let effect: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::GaussianBlur);
        state.sequence.as_mut().expect("sequence").video_tracks[0].clips[0].add_effect_node(effect);

        state
            .dispatch_action(mondrian_editor_state::Action::ReorderEffects {
                clip_id,
                from: 0,
                to: 0,
            })
            .expect("dispatch reorder noop");

        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_reorder_effects_action_rejects_out_of_range_source_index() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let effect: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::GaussianBlur);
        state.sequence.as_mut().expect("sequence").video_tracks[0].clips[0].add_effect_node(effect);

        let err = state
            .dispatch_action(mondrian_editor_state::Action::ReorderEffects {
                clip_id,
                from: 1,
                to: 0,
            })
            .expect_err("out-of-range source index should reject reorder");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_reorder_effects_action_preserves_locked_track() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let first: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::GaussianBlur);
        let second: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::Sharpen);
        let first_id = first.id;
        let second_id = second.id;
        let sequence = state.sequence.as_mut().expect("sequence");
        sequence.video_tracks[0].clips[0].add_effect_node(first);
        sequence.video_tracks[0].clips[0].add_effect_node(second);
        sequence.video_tracks[0].is_locked = true;

        let err = state
            .dispatch_action(mondrian_editor_state::Action::ReorderEffects {
                clip_id,
                from: 0,
                to: 1,
            })
            .expect_err("locked track should reject effect reorder");

        assert!(matches!(err, MondrianError::TrackLocked { .. }));
        let effects = &state.sequence.as_ref().expect("sequence").video_tracks[0].clips[0].effects;
        assert_eq!(
            effects.iter().map(|effect| effect.id).collect::<Vec<_>>(),
            vec![first_id, second_id]
        );
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_copy_paste_actions_use_animation_keyframe_clipboard() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        let selection = SelectedClipRef { track_id, is_video_track: true, clip_id };
        state.selection.selected_clips = vec![selection];
        let tb = state.sequence.as_ref().expect("sequence").time_base();
        let source_time = mondrian_core::automation::timecode_to_ticks(TimeCode::new(4, tb));
        let destination_time = mondrian_core::automation::timecode_to_ticks(TimeCode::new(18, tb));
        state
            .mutate_clip_property(
                selection,
                PropertyMutation::SetKeyframe {
                    path: Transform2D::OPACITY_PATH.to_string(),
                    keyframe: mondrian_core::automation::Keyframe::linear(
                        source_time,
                        PropertyValue::Float(0.25),
                    ),
                },
                "seed opacity keyframe",
            )
            .expect("seed keyframe");
        state.set_animation_keyframe_selection(vec![crate::app::AnimationKeyframeSelection {
            clip_id,
            path: Transform2D::OPACITY_PATH.to_string(),
            time: source_time,
        }]);
        state.seek(18);

        state
            .dispatch_action(mondrian_editor_state::Action::Copy)
            .expect("copy keyframe");
        state
            .dispatch_action(mondrian_editor_state::Action::Paste)
            .expect("paste keyframe");

        let property = state
            .clip_snapshot(selection)
            .and_then(|clip| clip.property_bag().ok())
            .and_then(|bag| bag.property(Transform2D::OPACITY_PATH).cloned())
            .expect("opacity property");
        let pasted = property.keyframe_at(destination_time).expect("pasted keyframe");
        assert_eq!(pasted.value, PropertyValue::Float(0.25));
        assert!(state.has_animation_clipboard());
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_clipboard_actions_noop_without_selection_or_clipboard() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();

        state
            .dispatch_action(mondrian_editor_state::Action::Copy)
            .expect("copy without selection");
        state
            .dispatch_action(mondrian_editor_state::Action::Paste)
            .expect("paste without selection");
        assert!(!state.has_animation_clipboard());
        assert!(!state.can_undo_action());

        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];
        state
            .dispatch_action(mondrian_editor_state::Action::Paste)
            .expect("paste without clipboard");
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_copy_paste_actions_use_clip_clipboard_when_no_keyframes_selected() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];
        state.seek(50);

        state.dispatch_action(mondrian_editor_state::Action::Copy).expect("copy clip");
        state.dispatch_action(mondrian_editor_state::Action::Paste).expect("paste clip");

        let sequence = state.sequence.as_ref().expect("sequence");
        let clips = &sequence.video_tracks[0].clips;
        assert_eq!(clips.len(), 2);
        assert!(clips.iter().any(|clip| clip.id == clip_id && clip.position.frame == 10));
        let pasted = clips.iter().find(|clip| clip.id != clip_id).expect("pasted clip");
        assert_eq!(pasted.position.frame, 50);
        assert_eq!(pasted.duration.frame, 20);
        assert_eq!(state.active_clipboard_kind, Some(AppClipboardKind::Clips));
        assert_eq!(
            state.selection.selected_clips,
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id: pasted.id }]
        );
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_cut_action_copies_and_removes_selected_clips() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];

        state.dispatch_action(mondrian_editor_state::Action::Cut).expect("cut clip");

        let sequence = state.sequence.as_ref().expect("sequence");
        assert!(sequence.video_tracks[0].clips.is_empty());
        assert!(state.selection.selected_clips.is_empty());
        assert!(state.has_clip_clipboard());
        assert_eq!(state.active_clipboard_kind, Some(AppClipboardKind::Clips));
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_cut_action_preserves_locked_track_and_clipboard() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        state.sequence.as_mut().expect("sequence").video_tracks[0].is_locked = true;
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];

        let err = state
            .dispatch_action(mondrian_editor_state::Action::Cut)
            .expect_err("locked track should reject cut");

        assert!(matches!(err, MondrianError::TrackLocked { .. }));
        let sequence = state.sequence.as_ref().expect("sequence");
        assert_eq!(sequence.video_tracks[0].clips.len(), 1);
        assert!(!state.has_clip_clipboard());
        assert_eq!(state.active_clipboard_kind, None);
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_duplicate_action_copies_selected_clips_after_selection_end() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];
        state.seek(0);

        state
            .dispatch_action(mondrian_editor_state::Action::Duplicate)
            .expect("duplicate clip");

        let sequence = state.sequence.as_ref().expect("sequence");
        let clips = &sequence.video_tracks[0].clips;
        assert_eq!(clips.len(), 2);
        let duplicated = clips.iter().find(|clip| clip.id != clip_id).expect("duplicate");
        assert_eq!(duplicated.position.frame, 30);
        assert_eq!(duplicated.duration.frame, 20);
        assert_eq!(state.current_frame(), 30);
        assert_eq!(
            state.selection.selected_clips,
            vec![SelectedClipRef {
                track_id,
                is_video_track: true,
                clip_id: duplicated.id,
            }]
        );
        assert!(!state.has_clip_clipboard());
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_duplicate_action_noops_without_selection() {
        let (mut state, _, _) = state_with_two_video_tracks();

        state
            .dispatch_action(mondrian_editor_state::Action::Duplicate)
            .expect("duplicate without selection");

        let sequence = state.sequence.as_ref().expect("sequence");
        assert_eq!(sequence.video_tracks[0].clips.len(), 1);
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_copy_paste_clip_actions_rebuild_linked_clip_pairs() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("linked clipboard");
        let tb = sequence.time_base();
        let video_track_id = sequence.video_tracks[0].id;
        let audio_track_id = sequence.audio_tracks[0].id;

        let mut video_clip =
            Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(20, tb));
        let mut audio_clip = Clip::new(
            video_clip.asset_id,
            TimeCode::new(10, tb),
            TimeCode::new(20, tb),
        );
        let video_clip_id = video_clip.id;
        let audio_clip_id = audio_clip.id;
        video_clip.linked_clip = Some(audio_clip_id);
        audio_clip.linked_clip = Some(video_clip_id);
        sequence.video_tracks[0].add_clip(video_clip).expect("add video");
        sequence.audio_tracks[0].add_clip(audio_clip).expect("add audio");
        state.sequence = Some(sequence);
        state.selection.selected_clips = vec![SelectedClipRef {
            track_id: video_track_id,
            is_video_track: true,
            clip_id: video_clip_id,
        }];
        state.seek(40);

        state
            .dispatch_action(mondrian_editor_state::Action::Copy)
            .expect("copy linked clip");
        state
            .dispatch_action(mondrian_editor_state::Action::Paste)
            .expect("paste linked clip");

        let sequence = state.sequence.as_ref().expect("sequence");
        let pasted_video = sequence.video_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id != video_clip_id)
            .expect("pasted video");
        let pasted_audio = sequence.audio_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id != audio_clip_id)
            .expect("pasted audio");
        assert_eq!(pasted_video.position.frame, 40);
        assert_eq!(pasted_audio.position.frame, 40);
        assert_eq!(pasted_video.linked_clip, Some(pasted_audio.id));
        assert_eq!(pasted_audio.linked_clip, Some(pasted_video.id));
        assert_eq!(
            state.selection.selected_clips,
            vec![
                SelectedClipRef {
                    track_id: video_track_id,
                    is_video_track: true,
                    clip_id: pasted_video.id,
                },
                SelectedClipRef {
                    track_id: audio_track_id,
                    is_video_track: false,
                    clip_id: pasted_audio.id,
                },
            ]
        );
        assert!(state.can_undo_action());
    }
}
