//! Semantic Action adapter for the application composition root.
//!
//! UI intent is routed to the owning domain Interface. Project mutations go
//! through `AuthoringSession`; transport intent goes through `PlaybackEngine`.

use crate::app::animation_authoring::{ClipNumericCurveEdit, NormalizedCurvePoint};
use crate::app::exporting::TimelineExportRequest;
use crate::app::preview_quality::normalize_preview_resolution_scale;
use crate::app::proxy_generation::{
    resolve_app_state_proxy_color_contract, ProxyGenerationOrigin, ProxyGenerationRequestOutcome,
};
use crate::app::selection::resolve_track_selection;
use crate::app::timeline_editing::{
    clip_link_group_member_ids, find_clip, find_clip_mut, find_clip_track_lock, set_clip_disabled,
};
use crate::app::ui_actions::{
    AssetsCreateAssetPayload, AssetsCreateFolderPayload, AssetsDeleteAssetPayload,
    AssetsDeleteFolderPayload, AssetsDeleteSelectionPayload, AssetsImportFilesPayload,
    AssetsMoveAssetPayload, AssetsMoveFolderPayload, AssetsMoveSelectionPayload,
    AssetsPrepareDragPayload, AssetsRebindAudioComponentPayload,
    AssetsRefreshAudioComponentsPayload, AssetsRelinkAssetPayload, AssetsRenameAssetPayload,
    AssetsRenameFolderPayload, AssetsSetInterpretationPayload, AssetsSetProxyModePayload,
    EffectsAddToClipPayload, ExportDraftUpdatePayload, ExportEnqueuePayload,
    ExportJobTargetPayload, InspectorAudioComponentEditField, InspectorAudioComponentSourcePayload,
    InspectorClipTransformField, InspectorCurveEditPayload, InspectorEditClipCurvePayload,
    InspectorRemoveEffectPayload, InspectorSelectEffectPayload,
    InspectorSetAudioComponentEditFieldPayload, InspectorSetAudioComponentSourcePayload,
    InspectorSetClipEnabledPayload, InspectorSetClipOpacityPayload,
    InspectorSetClipPropertyPayload, InspectorSetClipTintPayload,
    InspectorSetClipTransformFieldPayload, InspectorSetEffectEnabledPayload,
    InspectorSetEffectPropertyPayload, ProjectCreateWithSettingsPayload,
    ProjectRecoverFromAutosavePayload, ProjectUpdateColorEnvironmentPayload,
    ProjectUpdateNewSequenceDefaultsPayload, SequenceTargetPayload, SequenceUpdateSettingsPayload,
    TimelineAddTrackKind, TimelineAddTrackPayload, TimelineCreateCrossDissolvePayload,
    TimelineDropAssetPayload, TimelineInOutPointPayloadKind, TimelineInsertAssetPayload,
    TimelineMoveClipPayload, TimelineMoveTrackPayload, TimelineOpenNestedSequencePayload,
    TimelinePrecomposeSelectionPayload, TimelineSeekPayload, TimelineSelectClipPayload,
    TimelineSelectVideoTransitionPayload, TimelineSetInOutPointPayload,
    TimelineSetSelectedClipsEnabledPayload, TimelineSetTrackControlPayload,
    TimelineSetVideoTransitionRangePayload, TimelineTrackControlPayloadKind,
    TimelineTrimClipsPayload, TimelineTrimPayloadEdge, TimelineTrimSelectedClipsToPlayheadPayload,
    ViewerSetClipTransformPayload, ViewerSetPreviewResolutionScalePayload,
    ASSETS_CREATE_ADJUSTMENT_LAYER, ASSETS_CREATE_FOLDER, ASSETS_CREATE_SOLID_COLOR,
    ASSETS_DELETE_ASSET, ASSETS_DELETE_FOLDER, ASSETS_DELETE_SELECTION, ASSETS_IMPORT_FILES,
    ASSETS_MOVE_ASSET, ASSETS_MOVE_FOLDER, ASSETS_MOVE_SELECTION, ASSETS_NAMESPACE,
    ASSETS_PREPARE_DRAG, ASSETS_REBIND_AUDIO_COMPONENT, ASSETS_REFRESH_AUDIO_COMPONENTS,
    ASSETS_RELINK_ASSET, ASSETS_RENAME_ASSET, ASSETS_RENAME_FOLDER, ASSETS_SET_INTERPRETATION,
    ASSETS_SET_PROXY_MODE, EFFECTS_ADD_TO_CLIP, EFFECTS_NAMESPACE, EXPORT_CANCEL_JOB,
    EXPORT_CLEAR_COMPLETED, EXPORT_ENQUEUE, EXPORT_NAMESPACE, EXPORT_SET_DRAFT,
    INSPECTOR_EDIT_CLIP_CURVE, INSPECTOR_NAMESPACE, INSPECTOR_REMOVE_EFFECT,
    INSPECTOR_SELECT_EFFECT, INSPECTOR_SET_AUDIO_COMPONENT_EDIT_FIELD,
    INSPECTOR_SET_AUDIO_COMPONENT_SOURCE, INSPECTOR_SET_CLIP_ENABLED, INSPECTOR_SET_CLIP_OPACITY,
    INSPECTOR_SET_CLIP_PROPERTY, INSPECTOR_SET_CLIP_TINT, INSPECTOR_SET_CLIP_TRANSFORM_FIELD,
    INSPECTOR_SET_EFFECT_ENABLED, INSPECTOR_SET_EFFECT_PROPERTY, PROJECT_CREATE_WITH_SETTINGS,
    PROJECT_NAMESPACE, PROJECT_RECOVER_FROM_AUTOSAVE, PROJECT_UPDATE_COLOR_ENVIRONMENT,
    PROJECT_UPDATE_NEW_SEQUENCE_DEFAULTS, SEQUENCE_DELETE, SEQUENCE_DUPLICATE, SEQUENCE_NAMESPACE,
    SEQUENCE_NEW, SEQUENCE_RETURN_TO_PARENT, SEQUENCE_SET_ACTIVE_DEFAULT, SEQUENCE_SWITCH_ACTIVE,
    SEQUENCE_UPDATE_SETTINGS, TIMELINE_ADD_TRACK, TIMELINE_CLEAR_IN_OUT_POINTS,
    TIMELINE_CREATE_BASIC_TITLE, TIMELINE_CREATE_CROSS_DISSOLVE, TIMELINE_DROP_ASSET,
    TIMELINE_INSERT_ASSET, TIMELINE_MOVE_CLIP, TIMELINE_MOVE_TRACK, TIMELINE_NAMESPACE,
    TIMELINE_OPEN_NESTED_SEQUENCE, TIMELINE_PRECOMPOSE_SELECTION,
    TIMELINE_ROLL_SELECTED_CUT_TO_PLAYHEAD, TIMELINE_SEEK, TIMELINE_SELECT_CLIP,
    TIMELINE_SELECT_VIDEO_TRANSITION, TIMELINE_SET_IN_OUT_POINT,
    TIMELINE_SET_SELECTED_CLIPS_ENABLED, TIMELINE_SET_TRACK_CONTROL,
    TIMELINE_SET_VIDEO_TRANSITION_RANGE, TIMELINE_TRIM_CLIPS,
    TIMELINE_TRIM_SELECTED_CLIPS_TO_PLAYHEAD, VIEWER_NAMESPACE, VIEWER_SET_CLIP_TRANSFORM,
    VIEWER_SET_PREVIEW_RESOLUTION_SCALE,
};
use crate::app::{AppClipboardKind, AppState, ClipOverlapMode, SelectedClipRef};
use glam::Vec2;
use mondrian_assets::library::FolderRecord;
use mondrian_assets::{AssetKind, AssetLibrary};
use mondrian_core::automation::{
    InterpolationType, Keyframe, PropertyHost, PropertyMutation, PropertyValue,
};
use mondrian_core::events::AppEvent;
use mondrian_core::types::{AudioComponentEditId, ClipId, EffectId, FramePosition, Rational};
use mondrian_core::{FrameRounding, MondrianError, Result, TimelineTime};
use mondrian_timeline::audio::AudioComponentSource;
use mondrian_timeline::clip::{Clip, Transform2D, TrimEdge};
use std::path::PathBuf;
use std::time::Duration;

impl AppState {
    /// Dispatch one semantic Action into its owning product Interface.
    ///
    /// An Action handled by a shell-only Interface, an unknown namespace, or
    /// an unimplemented product path is rejected. A caller must never infer
    /// successful execution from a silent no-op.
    pub fn dispatch_action(&mut self, action: mondrian_editor_state::Action) -> Result<()> {
        use mondrian_editor_state::Action;

        match action {
            Action::NoOp => Ok(()),

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
                let end = self.last_content_frame()?;
                if end >= 0 {
                    self.seek(end);
                }
                Ok(())
            }

            // ── 选择（当前 AppState 可表达 track / clip / mask / animation selection）──
            Action::Select(target) => self.select_from_action(target),
            Action::SelectAll => {
                self.select_all_clips();
                Ok(())
            }
            Action::DeselectAll => {
                self.clear_selection();
                Ok(())
            }

            // ── 撤销/重做（已有方法）─────────────────────────────────────
            Action::Undo => {
                let _ = self.undo_timeline()?;
                Ok(())
            }
            Action::Redo => {
                let _ = self.redo_timeline()?;
                Ok(())
            }

            // ── 剪贴板（动画关键帧优先，否则使用 timeline clip clipboard）──
            Action::Copy => self.copy_from_action(),
            Action::Cut => self.cut_from_action(),
            Action::Paste => self.paste_from_action(),
            Action::Duplicate => self.duplicate_from_action(),

            // ── 时间线编辑（复用已有 undoable 命令层）────────────────────
            Action::DeleteSelection => self.delete_selection_from_ui(false),
            Action::RippleDeleteSelection => self.delete_selection_from_ui(true),
            Action::SplitClipAtPlayhead => self.split_at_playhead().map(|_| ()),
            Action::MarkInAtPlayhead => self.mark_in_at_current_frame(),
            Action::MarkOutAtPlayhead => self.mark_out_at_current_frame(),
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
                self.request_project_save().map_err(mondrian_core::MondrianError::Other)?;
                self.set_status_hint("正在后台保存项目…", false);
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
            Action::Custom { namespace, name, payload } if namespace == EXPORT_NAMESPACE => {
                self.dispatch_export_ui_action(&name, payload)
            }
            Action::Custom { namespace, name, payload } if namespace == VIEWER_NAMESPACE => {
                self.dispatch_viewer_ui_action(&name, payload)
            }
            Action::Custom { namespace, name, payload } if namespace == PROJECT_NAMESPACE => {
                self.dispatch_project_ui_action(&name, payload)
            }
            Action::Custom { namespace, name, payload } if namespace == SEQUENCE_NAMESPACE => {
                self.dispatch_sequence_ui_action(&name, payload)
            }

            unsupported => Err(MondrianError::WorkflowStepFailed {
                step_id: "dispatch_action".to_owned(),
                reason: format!(
                    "Action has no AppState product Interface implementation: {unsupported:?}"
                ),
            }),
        }
    }

    fn copy_from_action(&mut self) -> Result<()> {
        let Some(selection) = self.primary_selected_clip() else {
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
        self.request_project_save_as(path).map_err(|err| {
            let reason = err.to_string();
            self.set_status_hint(format!("无法启动另存为：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "save_project_as".to_string(), reason }
        })?;
        self.set_status_hint(format!("正在后台另存为：{}", display_path.display()), false);
        Ok(())
    }

    fn create_project_from_ui(&mut self, payload: ProjectCreateWithSettingsPayload) -> Result<()> {
        if payload.project_file.as_os_str().is_empty() {
            return Ok(());
        }
        let project_file = super::ensure_project_extension(payload.project_file);
        let name = if payload.name.trim().is_empty() {
            project_file
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.trim().is_empty())
                .map(|stem| stem.trim().to_string())
                .unwrap_or_else(|| "Untitled".to_string())
        } else {
            payload.name.trim().to_string()
        };

        self.create_new_project_with_settings_at(
            project_file.clone(),
            &name,
            payload.sequence_settings,
            payload.color_environment,
            payload.project_settings,
        )
        .map_err(|err| {
            let reason = err.to_string();
            self.set_status_hint(format!("新建项目失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "create_project".to_string(), reason }
        })?;
        self.set_status_hint(format!("项目已创建：{}", project_file.display()), false);
        Ok(())
    }

    fn recover_project_from_autosave_ui(
        &mut self,
        payload: ProjectRecoverFromAutosavePayload,
    ) -> Result<()> {
        if payload.project_file.as_os_str().is_empty()
            || payload.autosave_file.as_os_str().is_empty()
        {
            return Ok(());
        }
        self.open_project_from_autosave_snapshot(
            payload.project_file.clone(),
            payload.autosave_file,
        )
        .map_err(|err| {
            let reason = err.to_string();
            self.set_status_hint(format!("恢复自动保存失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "recover_project".to_string(), reason }
        })?;
        self.set_status_hint(
            format!("已从自动保存恢复：{}", payload.project_file.display()),
            false,
        );
        Ok(())
    }

    fn import_media_from_action(&mut self, paths: Vec<PathBuf>) -> Result<()> {
        self.import_media_into_folder_from_action(paths, None)
    }

    fn import_media_into_folder_from_action(
        &mut self,
        paths: Vec<PathBuf>,
        folder_id: Option<&str>,
    ) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }

        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("导入失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "import_media".to_string(), reason }
        })?;

        if let Some(folder_id) = folder_id {
            if !library.folder_exists(folder_id)? {
                let reason = format!("目标素材文件夹不存在：{folder_id}");
                self.set_status_hint(format!("导入失败：{reason}"), true);
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "import_media".to_string(),
                    reason,
                });
            }
        }

        drop(library);
        self.start_media_import_batch(paths, folder_id.map(str::to_owned))
    }

    fn delete_asset_from_ui(&mut self, payload: AssetsDeleteAssetPayload) -> Result<()> {
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("删除素材失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "delete_asset".to_string(), reason }
        })?;
        let asset_name = library
            .get_asset(payload.asset_id)?
            .map(|asset| asset.name)
            .unwrap_or_else(|| payload.asset_id.to_string());

        self.delete_asset_from_library(payload.asset_id).map_err(|err| {
            let reason = err.to_string();
            self.set_status_hint(format!("删除素材失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "delete_asset".to_string(), reason }
        })?;
        self.set_status_hint(format!("已删除素材：{asset_name}"), false);
        Ok(())
    }

    fn relink_asset_from_ui(&mut self, payload: AssetsRelinkAssetPayload) -> Result<()> {
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("重新链接素材失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "relink_asset".to_string(), reason }
        })?;
        let asset_name = library
            .get_asset(payload.asset_id)?
            .map(|asset| asset.name)
            .unwrap_or_else(|| payload.asset_id.to_string());

        self.relink_asset(payload.asset_id, &payload.path).map_err(|err| {
            let reason = err.to_string();
            self.set_status_hint(format!("重新链接素材失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "relink_asset".to_string(), reason }
        })?;
        self.event_bus.publish(mondrian_core::events::AppEvent::AssetLibraryReloaded);
        self.set_status_hint(
            format!("已重新链接素材：{asset_name} → {}", payload.path.display()),
            false,
        );
        Ok(())
    }

    fn refresh_audio_components_from_ui(
        &mut self,
        payload: AssetsRefreshAudioComponentsPayload,
    ) -> Result<()> {
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("音频 Component 探测失败：{reason}"), true);
            MondrianError::WorkflowStepFailed {
                step_id: "assets_refresh_audio_components".to_string(),
                reason,
            }
        })?;
        let asset_name = library
            .get_asset(payload.asset_id)?
            .map(|asset| asset.name)
            .unwrap_or_else(|| payload.asset_id.to_string());
        library.refresh_audio_components(payload.asset_id).map_err(|error| {
            let reason = error.to_string();
            self.set_status_hint(format!("音频 Component 探测失败：{reason}"), true);
            MondrianError::WorkflowStepFailed {
                step_id: "assets_refresh_audio_components".to_string(),
                reason,
            }
        })?;
        self.refresh_audio_playback_after_authoring_change();
        self.event_bus.publish(AppEvent::AssetLibraryReloaded);
        self.set_status_hint(format!("已刷新 {asset_name} 的音频流候选"), false);
        Ok(())
    }

    fn rebind_audio_component_from_ui(
        &mut self,
        payload: AssetsRebindAudioComponentPayload,
    ) -> Result<()> {
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("音频 Component 重绑定失败：{reason}"), true);
            MondrianError::WorkflowStepFailed {
                step_id: "assets_rebind_audio_component".to_string(),
                reason,
            }
        })?;
        let asset_name = library
            .get_asset(payload.asset_id)?
            .map(|asset| asset.name)
            .unwrap_or_else(|| payload.asset_id.to_string());
        library
            .rebind_audio_component(payload.asset_id, payload.component_id, payload.stream_index)
            .map_err(|error| {
                let reason = error.to_string();
                self.set_status_hint(format!("音频 Component 重绑定失败：{reason}"), true);
                MondrianError::WorkflowStepFailed {
                    step_id: "assets_rebind_audio_component".to_string(),
                    reason,
                }
            })?;
        self.refresh_audio_playback_after_authoring_change();
        self.event_bus.publish(AppEvent::AssetLibraryReloaded);
        self.set_status_hint(
            format!(
                "已将 {asset_name} 的音频 Component 映射到流 #{}",
                payload.stream_index
            ),
            false,
        );
        Ok(())
    }

    fn rename_asset_from_ui(&mut self, payload: AssetsRenameAssetPayload) -> Result<()> {
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("重命名素材失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "rename_asset".to_string(), reason }
        })?;
        library.rename_asset(payload.asset_id, &payload.name).map_err(|err| {
            let reason = err.to_string();
            self.set_status_hint(format!("重命名素材失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "rename_asset".to_string(), reason }
        })?;
        self.event_bus.publish(mondrian_core::events::AppEvent::AssetLibraryReloaded);
        self.set_status_hint(format!("已重命名素材：{}", payload.name.trim()), false);
        Ok(())
    }

    fn set_asset_interpretation_from_ui(
        &mut self,
        payload: AssetsSetInterpretationPayload,
    ) -> Result<()> {
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("解释素材失败：{reason}"), true);
            MondrianError::WorkflowStepFailed {
                step_id: "set_asset_interpretation".to_string(),
                reason,
            }
        })?;
        let asset_name = library
            .get_asset(payload.asset_id)?
            .map(|asset| asset.name)
            .unwrap_or_else(|| payload.asset_id.to_string());
        library
            .set_asset_interpretation(payload.asset_id, payload.interpretation)
            .map_err(|err| {
                let reason = err.to_string();
                self.set_status_hint(format!("解释素材失败：{reason}"), true);
                MondrianError::WorkflowStepFailed {
                    step_id: "set_asset_interpretation".to_string(),
                    reason,
                }
            })?;
        self.event_bus.publish(AppEvent::AssetLibraryReloaded);
        self.set_status_hint(format!("已更新素材解释：{asset_name}"), false);
        Ok(())
    }

    fn rename_folder_from_ui(&mut self, payload: AssetsRenameFolderPayload) -> Result<()> {
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("重命名文件夹失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "rename_folder".to_string(), reason }
        })?;
        library.rename_folder(&payload.folder_id, &payload.name).map_err(|err| {
            let reason = err.to_string();
            self.set_status_hint(format!("重命名文件夹失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "rename_folder".to_string(), reason }
        })?;
        self.event_bus.publish(mondrian_core::events::AppEvent::AssetLibraryReloaded);
        self.set_status_hint(format!("已重命名文件夹：{}", payload.name.trim()), false);
        Ok(())
    }

    fn set_asset_proxy_mode_from_ui(&mut self, payload: AssetsSetProxyModePayload) -> Result<()> {
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("设置代理模式失败：{reason}"), true);
            MondrianError::WorkflowStepFailed {
                step_id: "set_asset_proxy_mode".to_string(),
                reason,
            }
        })?;
        let asset = library.get_asset(payload.asset_id)?.ok_or_else(|| {
            let reason = format!("素材不存在：{}", payload.asset_id);
            self.set_status_hint(format!("设置代理模式失败：{reason}"), true);
            MondrianError::WorkflowStepFailed {
                step_id: "set_asset_proxy_mode".to_string(),
                reason,
            }
        })?;
        if !matches!(asset.kind, AssetKind::Video) {
            let reason = "只有视频素材支持代理模式".to_string();
            self.set_status_hint(format!("设置代理模式失败：{reason}"), true);
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "set_asset_proxy_mode".to_string(),
                reason,
            });
        }
        if payload.enabled && !asset.path.exists() {
            let reason = "素材文件不存在，请先重新链接媒体".to_string();
            self.set_status_hint(format!("设置代理模式失败：{reason}"), true);
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "set_asset_proxy_mode".to_string(),
                reason,
            });
        }
        if payload.enabled && !self.project_settings().proxy_enabled {
            let reason = "项目已禁用代理工作流，请先在项目设置中启用代理".to_string();
            self.set_status_hint(format!("设置代理模式失败：{reason}"), true);
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "set_asset_proxy_mode".to_string(),
                reason,
            });
        }

        let mut status = if payload.enabled {
            format!("已开启代理模式：{}", asset.name)
        } else {
            format!("已关闭代理模式：{}", asset.name)
        };
        if payload.enabled {
            let proxy_config = self.proxy_config();
            let proxy_color =
                resolve_app_state_proxy_color_contract(self, &asset).map_err(|reason| {
                    self.set_status_hint(format!("无法启用代理：{reason}"), true);
                    MondrianError::WorkflowStepFailed {
                        step_id: "set_asset_proxy_mode".to_owned(),
                        reason,
                    }
                })?;
            match self.request_proxy_generation(
                payload.asset_id,
                asset.path,
                proxy_config,
                proxy_color,
                ProxyGenerationOrigin::User,
            ) {
                ProxyGenerationRequestOutcome::AlreadyFresh => {}
                ProxyGenerationRequestOutcome::Admitted {
                    prior_status: mondrian_media::ProxyStatus::Missing,
                } => {
                    status.push_str("（后台生成中）");
                }
                ProxyGenerationRequestOutcome::Admitted {
                    prior_status: mondrian_media::ProxyStatus::Stale,
                } => {
                    status.push_str("（代理过期，后台重新生成中）");
                }
                ProxyGenerationRequestOutcome::Admitted {
                    prior_status: mondrian_media::ProxyStatus::Fresh,
                } => {}
                ProxyGenerationRequestOutcome::Deduplicated { .. } => {
                    status.push_str("（后台生成中）");
                }
                ProxyGenerationRequestOutcome::RetainedFailure(failure)
                | ProxyGenerationRequestOutcome::Failed(failure) => {
                    let reason = failure.detail;
                    self.set_status_hint(format!("无法启用代理：{reason}"), true);
                    return Err(MondrianError::WorkflowStepFailed {
                        step_id: "set_asset_proxy_mode".to_owned(),
                        reason,
                    });
                }
            }
        }
        self.set_asset_proxy_mode(payload.asset_id, payload.enabled);
        self.set_status_hint(status, false);
        Ok(())
    }

    fn delete_folder_from_ui(&mut self, payload: AssetsDeleteFolderPayload) -> Result<()> {
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("删除文件夹失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "delete_folder".to_string(), reason }
        })?;
        let folder_name = library
            .list_folders()?
            .into_iter()
            .find(|folder| folder.id == payload.folder_id)
            .map(|folder| folder.name)
            .unwrap_or_else(|| payload.folder_id.clone());

        self.delete_folder_from_library(&payload.folder_id).map_err(|err| {
            let reason = err.to_string();
            self.set_status_hint(format!("删除文件夹失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "delete_folder".to_string(), reason }
        })?;
        self.set_status_hint(format!("已删除文件夹：{folder_name}"), false);
        Ok(())
    }

    fn delete_asset_selection_from_ui(
        &mut self,
        payload: AssetsDeleteSelectionPayload,
    ) -> Result<()> {
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("删除素材选择失败：{reason}"), true);
            MondrianError::WorkflowStepFailed {
                step_id: "delete_asset_selection".to_string(),
                reason,
            }
        })?;
        let mut deleted_assets = 0usize;
        let mut deleted_folders = 0usize;

        for asset_id in &payload.asset_ids {
            self.delete_asset_and_cleanup_timeline(*asset_id).map_err(|err| {
                let reason = err.to_string();
                self.set_status_hint(format!("删除素材选择失败：{reason}"), true);
                MondrianError::WorkflowStepFailed {
                    step_id: "delete_asset_selection".to_string(),
                    reason,
                }
            })?;
            library.delete_asset(*asset_id).map_err(|err| {
                let reason = err.to_string();
                self.set_status_hint(format!("删除素材选择失败：{reason}"), true);
                MondrianError::WorkflowStepFailed {
                    step_id: "delete_asset_selection".to_string(),
                    reason,
                }
            })?;
            self.event_bus
                .publish(mondrian_core::events::AppEvent::AssetDeleted { asset_id: *asset_id });
            deleted_assets += 1;
        }
        for folder_id in &payload.folder_ids {
            library.delete_folder(folder_id).map_err(|err| {
                let reason = err.to_string();
                self.set_status_hint(format!("删除素材选择失败：{reason}"), true);
                MondrianError::WorkflowStepFailed {
                    step_id: "delete_asset_selection".to_string(),
                    reason,
                }
            })?;
            deleted_folders += 1;
        }

        if deleted_assets + deleted_folders == 0 {
            return Ok(());
        }
        self.event_bus.publish(mondrian_core::events::AppEvent::AssetLibraryReloaded);
        self.set_status_hint(
            format!("已删除 {deleted_assets} 个素材、{deleted_folders} 个文件夹"),
            false,
        );
        Ok(())
    }

    fn move_asset_from_ui(&mut self, payload: AssetsMoveAssetPayload) -> Result<()> {
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("移动素材失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "move_asset".to_string(), reason }
        })?;
        let asset_name = library
            .get_asset(payload.asset_id)?
            .map(|asset| asset.name)
            .unwrap_or_else(|| payload.asset_id.to_string());
        let target_name = match payload.folder_id.as_deref() {
            Some(folder_id) => library
                .list_folders()?
                .into_iter()
                .find(|folder| folder.id == folder_id)
                .map(|folder| folder.name)
                .unwrap_or_else(|| folder_id.to_string()),
            None => "All assets".to_string(),
        };

        self.move_asset_in_library(payload.asset_id, payload.folder_id.as_deref())
            .map_err(|err| {
                let reason = err.to_string();
                self.set_status_hint(format!("移动素材失败：{reason}"), true);
                MondrianError::WorkflowStepFailed { step_id: "move_asset".to_string(), reason }
            })?;
        self.set_status_hint(format!("已移动素材：{asset_name} → {target_name}"), false);
        Ok(())
    }

    fn move_folder_from_ui(&mut self, payload: AssetsMoveFolderPayload) -> Result<()> {
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("移动文件夹失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "move_folder".to_string(), reason }
        })?;
        let folders = library.list_folders()?;
        let folder_name = folders
            .iter()
            .find(|folder| folder.id == payload.folder_id)
            .map(|folder| folder.name.clone())
            .unwrap_or_else(|| payload.folder_id.clone());
        let target_name = match payload.parent_folder_id.as_deref() {
            Some(parent_id) => folders
                .iter()
                .find(|folder| folder.id == parent_id)
                .map(|folder| folder.name.clone())
                .unwrap_or_else(|| parent_id.to_string()),
            None => "All assets".to_string(),
        };

        self.move_folder_in_library(&payload.folder_id, payload.parent_folder_id.as_deref())
            .map_err(|err| {
                let reason = err.to_string();
                self.set_status_hint(format!("移动文件夹失败：{reason}"), true);
                MondrianError::WorkflowStepFailed { step_id: "move_folder".to_string(), reason }
            })?;
        self.set_status_hint(
            format!("已移动文件夹：{folder_name} → {target_name}"),
            false,
        );
        Ok(())
    }

    fn move_selection_from_ui(&mut self, payload: AssetsMoveSelectionPayload) -> Result<()> {
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("移动素材选择失败：{reason}"), true);
            MondrianError::WorkflowStepFailed {
                step_id: "move_asset_selection".to_string(),
                reason,
            }
        })?;
        let folders = library.list_folders().map_err(|err| {
            let reason = err.to_string();
            self.set_status_hint(format!("移动素材选择失败：{reason}"), true);
            MondrianError::WorkflowStepFailed {
                step_id: "move_asset_selection".to_string(),
                reason,
            }
        })?;
        if let Err(err) = validate_asset_selection_move(&library, &payload, &folders) {
            let reason = err.to_string();
            self.set_status_hint(format!("移动素材选择失败：{reason}"), true);
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "move_asset_selection".to_string(),
                reason,
            });
        }
        let target_name = match payload.target_folder_id.as_deref() {
            Some(folder_id) => folders
                .iter()
                .find(|folder| folder.id == folder_id)
                .map(|folder| folder.name.clone())
                .unwrap_or_else(|| folder_id.to_string()),
            None => "All assets".to_string(),
        };
        let mut moved_assets = 0usize;
        let mut moved_folders = 0usize;

        for asset_id in &payload.asset_ids {
            library
                .move_asset_to_folder(*asset_id, payload.target_folder_id.as_deref())
                .map_err(|err| {
                    let reason = err.to_string();
                    self.set_status_hint(format!("移动素材选择失败：{reason}"), true);
                    MondrianError::WorkflowStepFailed {
                        step_id: "move_asset_selection".to_string(),
                        reason,
                    }
                })?;
            moved_assets += 1;
        }
        for folder_id in &payload.folder_ids {
            if Some(folder_id.as_str()) == payload.target_folder_id.as_deref() {
                continue;
            }
            library
                .move_folder(folder_id, payload.target_folder_id.as_deref())
                .map_err(|err| {
                    let reason = err.to_string();
                    self.set_status_hint(format!("移动素材选择失败：{reason}"), true);
                    MondrianError::WorkflowStepFailed {
                        step_id: "move_asset_selection".to_string(),
                        reason,
                    }
                })?;
            moved_folders += 1;
        }

        if moved_assets + moved_folders == 0 {
            return Ok(());
        }
        self.event_bus.publish(mondrian_core::events::AppEvent::AssetLibraryReloaded);
        self.set_status_hint(
            format!("已移动 {moved_assets} 个素材、{moved_folders} 个文件夹 → {target_name}"),
            false,
        );
        Ok(())
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
        let Some(selection) = self.primary_selected_clip() else {
            return Ok(());
        };
        let timeline_time = self.current_timeline_time()?.unwrap_or(TimelineTime::ZERO);
        let destination_time = self
            .active_sequence()
            .and_then(|sequence| find_clip(sequence, selection.clip_id))
            .map(|clip| clip_visual_author_time_at(clip, timeline_time))
            .transpose()?
            .ok_or_else(|| missing_clip_error("paste_animation_keyframes", selection.clip_id))?;
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
        source_time: FramePosition,
    ) -> Result<()> {
        let target_frame = {
            let Some(seq) = self.active_sequence() else {
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
        let Some(seq) = self.active_sequence() else {
            return Err(missing_sequence_error(step_id));
        };
        let (track_id, is_video_track, _) = find_clip_track_lock(seq, clip_id)
            .ok_or_else(|| missing_clip_error(step_id, clip_id))?;
        let frame = find_clip(seq, clip_id)
            .ok_or_else(|| missing_clip_error(step_id, clip_id))?
            .position
            .to_frame_position(seq.settings.frame_rate, FrameRounding::Nearest)?
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
        let Some(seq) = self.active_sequence() else {
            return Err(missing_sequence_error("move_clip"));
        };
        let (current_track_id, current_is_video_track, current_frame) =
            self.clip_action_location("move_clip", clip_id)?;
        if current_is_video_track != is_video_track {
            return Err(clip_media_type_mismatch_error("move_clip", clip_id));
        }
        if current_track_id == target_track_id
            && current_is_video_track == is_video_track
            && current_frame == frame
        {
            return Ok(());
        }

        let linked_clip_ids = clip_link_group_member_ids(seq, clip_id);
        self.move_clip_to_track_with_mode(
            target_track_id,
            is_video_track,
            clip_id,
            frame,
            ClipOverlapMode::Overwrite,
        )?;
        self.refresh_selected_clip_locations(&linked_clip_ids);
        Ok(())
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
                self.select_clip_for_action("select_clip", clip_id)
            }
            mondrian_editor_state::action::SelectionTarget::AllClips => {
                self.select_all_clips();
                Ok(())
            }
            mondrian_editor_state::action::SelectionTarget::Track(track_id) => self
                .select_track_by_id(track_id)
                .map(|_| ())
                .ok_or_else(|| MondrianError::TrackNotFound { track_id: track_id.to_string() }),
            mondrian_editor_state::action::SelectionTarget::AllTracks => {
                self.select_all_tracks();
                Ok(())
            }
        }
    }

    fn select_clip_for_action(&mut self, step_id: &'static str, clip_id: ClipId) -> Result<()> {
        if self.active_sequence().is_none() {
            return Err(missing_sequence_error(step_id));
        }
        self.select_clip_by_id(clip_id)
            .map(|_| ())
            .ok_or_else(|| missing_clip_error(step_id, clip_id))
    }

    fn select_effect_for_action(
        &mut self,
        step_id: &'static str,
        clip_id: ClipId,
        effect_id: EffectId,
    ) -> Result<()> {
        if self.active_sequence().is_none() {
            return Err(missing_sequence_error(step_id));
        }
        self.select_effect_by_id(clip_id, effect_id)
            .map(|_| ())
            .ok_or_else(|| missing_effect_error(step_id, clip_id, effect_id))
    }

    fn delete_selection_from_ui(&mut self, ripple: bool) -> Result<()> {
        if let Some(selection) = self.selected_video_transition() {
            if ripple {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "ripple_delete_video_transition".to_owned(),
                    reason: "Ripple Delete does not apply to a visual Transition".to_owned(),
                });
            }
            self.remove_video_transition(selection.transition_id)?;
            self.clear_selection();
            return Ok(());
        }

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
        if !selections.is_empty() {
            self.remove_clips_bulk(&selections, ripple)?;
            self.clear_selection();
            return Ok(());
        }

        self.delete_selected_tracks_from_ui()
    }

    fn delete_selected_tracks_from_ui(&mut self) -> Result<()> {
        let track_ids = self.selection.selected_track_ids.clone();
        if track_ids.is_empty() {
            return Ok(());
        }

        let Some(seq) = self.active_sequence() else {
            return Err(missing_sequence_error("delete_selected_tracks"));
        };
        let tracks = track_ids
            .into_iter()
            .map(|track_id| {
                resolve_track_selection(seq, track_id)
                    .map(|selection| (selection.track_id, selection.is_video_track))
                    .ok_or_else(|| MondrianError::TrackNotFound { track_id: track_id.to_string() })
            })
            .collect::<Result<Vec<_>>>()?;

        self.remove_tracks_bulk(&tracks)?;
        self.clear_selection();
        Ok(())
    }

    pub fn can_undo_action(&self) -> bool {
        self.authoring_history().is_some_and(|history| history.can_undo())
    }

    pub fn can_redo_action(&self) -> bool {
        self.authoring_history().is_some_and(|history| history.can_redo())
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
                self.select_clip_for_action("timeline_select_clip", payload.clip_id)
            }
            TIMELINE_SELECT_VIDEO_TRANSITION => {
                let payload = parse_ui_payload::<TimelineSelectVideoTransitionPayload>(
                    "timeline_ui_action",
                    name,
                    payload,
                )?;
                self.select_video_transition_by_id(payload.transition_id)
                    .map(|_| ())
                    .ok_or_else(|| MondrianError::WorkflowStepFailed {
                        step_id: "timeline_select_video_transition".to_owned(),
                        reason: format!(
                            "video Transition does not exist: {}",
                            payload.transition_id
                        ),
                    })
            }
            TIMELINE_CREATE_CROSS_DISSOLVE => {
                let payload = parse_ui_payload::<TimelineCreateCrossDissolvePayload>(
                    "timeline_ui_action",
                    name,
                    payload,
                )?;
                match self
                    .create_default_cross_dissolve(payload.left_clip_id, payload.right_clip_id)
                {
                    Ok(outcome) => {
                        self.select_video_transition_by_id(outcome.transition_id);
                        self.set_status_hint("已创建交叉溶解", false);
                        Ok(())
                    }
                    Err(error) => {
                        self.set_status_hint(format!("无法创建交叉溶解：{error}"), true);
                        Err(error)
                    }
                }
            }
            TIMELINE_CREATE_BASIC_TITLE => self.create_basic_title_at_playhead().map(|_| ()),
            TIMELINE_SET_VIDEO_TRANSITION_RANGE => {
                let payload = parse_ui_payload::<TimelineSetVideoTransitionRangePayload>(
                    "timeline_ui_action",
                    name,
                    payload,
                )?;
                let sequence = self
                    .active_sequence()
                    .ok_or_else(|| missing_sequence_error("timeline_set_video_transition_range"))?;
                let frame_rate = sequence.settings.frame_rate;
                let time_base = Rational::new(frame_rate.den, frame_rate.num);
                let start = TimelineTime::from_frame_position(FramePosition::new(
                    payload.start_frame,
                    time_base,
                ))?;
                let end = TimelineTime::from_frame_position(FramePosition::new(
                    payload.end_frame,
                    time_base,
                ))?;
                let duration = end.checked_sub(start)?;
                let range = mondrian_core::TimelineTimeRange::new(start, duration)?;
                match self.set_video_transition_range(
                    payload.transition_id,
                    range,
                    crate::app::video_transitions::VideoTransitionHandlePolicy::Reject,
                ) {
                    Ok(_) => {
                        self.select_video_transition_by_id(payload.transition_id);
                        Ok(())
                    }
                    Err(error) => {
                        self.set_status_hint(format!("无法调整视频转场：{error}"), true);
                        Err(error)
                    }
                }
            }
            TIMELINE_MOVE_CLIP => {
                let payload = parse_ui_payload::<TimelineMoveClipPayload>(
                    "timeline_ui_action",
                    name,
                    payload,
                )?;
                self.move_clip_with_snapshot(
                    payload.target_track_id,
                    payload.is_video_track,
                    payload.clip_id,
                    payload.frame,
                )
            }
            TIMELINE_TRIM_CLIPS => {
                let payload = parse_ui_payload::<TimelineTrimClipsPayload>(
                    "timeline_ui_action",
                    name,
                    payload,
                )?;
                let edge = match payload.edge {
                    TimelineTrimPayloadEdge::In => TrimEdge::In,
                    TimelineTrimPayloadEdge::Out => TrimEdge::Out,
                };
                self.trim_clips_bulk_to_frame(&payload.clip_ids, edge, payload.frame)
                    .map(|_| ())
            }
            TIMELINE_TRIM_SELECTED_CLIPS_TO_PLAYHEAD => {
                let payload = parse_ui_payload::<TimelineTrimSelectedClipsToPlayheadPayload>(
                    "timeline_ui_action",
                    name,
                    payload,
                )?;
                let edge = match payload.edge {
                    TimelineTrimPayloadEdge::In => TrimEdge::In,
                    TimelineTrimPayloadEdge::Out => TrimEdge::Out,
                };
                self.trim_selected_clips_to_playhead_from_ui(edge)
            }
            TIMELINE_ROLL_SELECTED_CUT_TO_PLAYHEAD => self.roll_selected_cut_to_playhead_from_ui(),
            TIMELINE_SET_IN_OUT_POINT => {
                let payload = parse_ui_payload::<TimelineSetInOutPointPayload>(
                    "timeline_ui_action",
                    name,
                    payload,
                )?;
                self.set_in_out_point_from_ui(payload)
            }
            TIMELINE_CLEAR_IN_OUT_POINTS => self.clear_in_out_points_from_ui(),
            TIMELINE_SET_SELECTED_CLIPS_ENABLED => {
                let payload = parse_ui_payload::<TimelineSetSelectedClipsEnabledPayload>(
                    "timeline_ui_action",
                    name,
                    payload,
                )?;
                self.set_selected_clips_enabled_from_ui(payload.enabled)
            }
            TIMELINE_SEEK => {
                let payload =
                    parse_ui_payload::<TimelineSeekPayload>("timeline_ui_action", name, payload)?;
                self.seek_with_source(payload.frame.max(0), payload.source);
                Ok(())
            }
            TIMELINE_SET_TRACK_CONTROL => {
                let payload = parse_ui_payload::<TimelineSetTrackControlPayload>(
                    "timeline_ui_action",
                    name,
                    payload,
                )?;
                match payload.control {
                    TimelineTrackControlPayloadKind::Visibility => self.set_track_visible(
                        payload.track_id,
                        payload.is_video_track,
                        payload.enabled,
                    ),
                    TimelineTrackControlPayloadKind::Mute => self.set_track_muted(
                        payload.track_id,
                        payload.is_video_track,
                        payload.enabled,
                    ),
                    TimelineTrackControlPayloadKind::Lock => self.set_track_locked(
                        payload.track_id,
                        payload.is_video_track,
                        payload.enabled,
                    ),
                }
            }
            TIMELINE_ADD_TRACK => {
                let payload = parse_ui_payload::<TimelineAddTrackPayload>(
                    "timeline_ui_action",
                    name,
                    payload,
                )?;
                match payload.kind {
                    TimelineAddTrackKind::Video => self.add_video_track(),
                    TimelineAddTrackKind::Audio => self.add_audio_track(),
                }
                .map_err(|err| MondrianError::WorkflowStepFailed {
                    step_id: "timeline_add_track".to_string(),
                    reason: err.to_string(),
                })
            }
            TIMELINE_MOVE_TRACK => {
                let payload = parse_ui_payload::<TimelineMoveTrackPayload>(
                    "timeline_ui_action",
                    name,
                    payload,
                )?;
                self.move_track(
                    payload.track_id,
                    payload.is_video_track,
                    payload.target_index,
                )
            }
            TIMELINE_DROP_ASSET => {
                let payload = parse_ui_payload::<TimelineDropAssetPayload>(
                    "timeline_ui_action",
                    name,
                    payload,
                )?;
                self.drop_asset_from_ui(payload)
            }
            TIMELINE_INSERT_ASSET => {
                let payload = parse_ui_payload::<TimelineInsertAssetPayload>(
                    "timeline_ui_action",
                    name,
                    payload,
                )?;
                self.insert_asset_from_ui(payload).map(|_| ())
            }
            TIMELINE_PRECOMPOSE_SELECTION => {
                let payload = parse_ui_payload::<TimelinePrecomposeSelectionPayload>(
                    "timeline_ui_action",
                    name,
                    payload,
                )?;
                self.precompose_selection_from_ui(payload)
            }
            TIMELINE_OPEN_NESTED_SEQUENCE => {
                let payload = parse_ui_payload::<TimelineOpenNestedSequencePayload>(
                    "timeline_ui_action",
                    name,
                    payload,
                )?;
                self.open_nested_sequence(payload.sequence_id)
            }
            _ => Err(unknown_ui_action_error("timeline_ui_action", name)),
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
            INSPECTOR_EDIT_CLIP_CURVE => {
                let payload = parse_ui_payload::<InspectorEditClipCurvePayload>(
                    "inspector_ui_action",
                    name,
                    payload,
                )?;
                self.edit_clip_curve_from_ui(payload)
            }
            INSPECTOR_SET_CLIP_PROPERTY => {
                let payload = parse_ui_payload::<InspectorSetClipPropertyPayload>(
                    "inspector_ui_action",
                    name,
                    payload,
                )?;
                self.set_clip_property_from_ui(
                    SelectedClipRef {
                        track_id: payload.clip.track_id,
                        is_video_track: payload.clip.is_video_track,
                        clip_id: payload.clip.clip_id,
                    },
                    &payload.path,
                    payload.value,
                )
            }
            INSPECTOR_SET_AUDIO_COMPONENT_SOURCE => {
                let payload = parse_ui_payload::<InspectorSetAudioComponentSourcePayload>(
                    "inspector_ui_action",
                    name,
                    payload,
                )?;
                self.set_audio_component_source_from_ui(payload)
            }
            INSPECTOR_SET_AUDIO_COMPONENT_EDIT_FIELD => {
                let payload = parse_ui_payload::<InspectorSetAudioComponentEditFieldPayload>(
                    "inspector_ui_action",
                    name,
                    payload,
                )?;
                self.set_audio_component_edit_field_from_ui(payload)
            }
            INSPECTOR_SELECT_EFFECT => {
                let payload = parse_ui_payload::<InspectorSelectEffectPayload>(
                    "inspector_ui_action",
                    name,
                    payload,
                )?;
                self.select_effect_for_action(
                    "inspector_select_effect",
                    payload.clip.clip_id,
                    payload.effect_id,
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
            INSPECTOR_SET_EFFECT_PROPERTY => {
                let payload = parse_ui_payload::<InspectorSetEffectPropertyPayload>(
                    "inspector_ui_action",
                    name,
                    payload,
                )?;
                self.set_effect_property_from_ui(
                    SelectedClipRef {
                        track_id: payload.clip.track_id,
                        is_video_track: payload.clip.is_video_track,
                        clip_id: payload.clip.clip_id,
                    },
                    payload.effect_id,
                    &payload.path,
                    payload.value,
                )
            }
            _ => Err(unknown_ui_action_error("inspector_ui_action", name)),
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
                let selection = SelectedClipRef {
                    track_id: payload.clip.track_id,
                    is_video_track: payload.clip.is_video_track,
                    clip_id: payload.clip.clip_id,
                };
                let effect_id = self.add_effect_to_clip(selection, payload.effect_type)?;
                self.select_effect_by_id(selection.clip_id, effect_id).map(|_| ()).ok_or_else(
                    || missing_effect_error("effects_add_to_clip", selection.clip_id, effect_id),
                )
            }
            _ => Err(unknown_ui_action_error("effects_ui_action", name)),
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
            ASSETS_CREATE_ADJUSTMENT_LAYER => {
                let payload = parse_ui_payload::<AssetsCreateAssetPayload>(
                    "assets_ui_action",
                    name,
                    payload,
                )?;
                self.create_adjustment_layer_asset_in_folder(None, payload.folder_id.as_deref())
                    .map(|_| ())
            }
            ASSETS_CREATE_SOLID_COLOR => {
                let payload = parse_ui_payload::<AssetsCreateAssetPayload>(
                    "assets_ui_action",
                    name,
                    payload,
                )?;
                self.create_solid_color_asset_in_folder(None, payload.folder_id.as_deref())
                    .map(|_| ())
            }
            ASSETS_CREATE_FOLDER => {
                let payload = parse_ui_payload::<AssetsCreateFolderPayload>(
                    "assets_ui_action",
                    name,
                    payload,
                )?;
                self.create_default_folder_in_library(payload.parent_folder_id.as_deref())
                    .map(|_| ())
            }
            ASSETS_IMPORT_FILES => {
                let payload = parse_ui_payload::<AssetsImportFilesPayload>(
                    "assets_ui_action",
                    name,
                    payload,
                )?;
                self.import_media_into_folder_from_action(
                    payload.paths,
                    payload.folder_id.as_deref(),
                )
            }
            ASSETS_RELINK_ASSET => {
                let payload = parse_ui_payload::<AssetsRelinkAssetPayload>(
                    "assets_ui_action",
                    name,
                    payload,
                )?;
                self.relink_asset_from_ui(payload)
            }
            ASSETS_REBIND_AUDIO_COMPONENT => {
                let payload = parse_ui_payload::<AssetsRebindAudioComponentPayload>(
                    "assets_ui_action",
                    name,
                    payload,
                )?;
                self.rebind_audio_component_from_ui(payload)
            }
            ASSETS_REFRESH_AUDIO_COMPONENTS => {
                let payload = parse_ui_payload::<AssetsRefreshAudioComponentsPayload>(
                    "assets_ui_action",
                    name,
                    payload,
                )?;
                self.refresh_audio_components_from_ui(payload)
            }
            ASSETS_RENAME_ASSET => {
                let payload = parse_ui_payload::<AssetsRenameAssetPayload>(
                    "assets_ui_action",
                    name,
                    payload,
                )?;
                self.rename_asset_from_ui(payload)
            }
            ASSETS_SET_INTERPRETATION => {
                let payload = parse_ui_payload::<AssetsSetInterpretationPayload>(
                    "assets_ui_action",
                    name,
                    payload,
                )?;
                self.set_asset_interpretation_from_ui(payload)
            }
            ASSETS_RENAME_FOLDER => {
                let payload = parse_ui_payload::<AssetsRenameFolderPayload>(
                    "assets_ui_action",
                    name,
                    payload,
                )?;
                self.rename_folder_from_ui(payload)
            }
            ASSETS_SET_PROXY_MODE => {
                let payload = parse_ui_payload::<AssetsSetProxyModePayload>(
                    "assets_ui_action",
                    name,
                    payload,
                )?;
                self.set_asset_proxy_mode_from_ui(payload)
            }
            ASSETS_DELETE_ASSET => {
                let payload = parse_ui_payload::<AssetsDeleteAssetPayload>(
                    "assets_ui_action",
                    name,
                    payload,
                )?;
                self.delete_asset_from_ui(payload)
            }
            ASSETS_DELETE_FOLDER => {
                let payload = parse_ui_payload::<AssetsDeleteFolderPayload>(
                    "assets_ui_action",
                    name,
                    payload,
                )?;
                self.delete_folder_from_ui(payload)
            }
            ASSETS_DELETE_SELECTION => {
                let payload = parse_ui_payload::<AssetsDeleteSelectionPayload>(
                    "assets_ui_action",
                    name,
                    payload,
                )?;
                self.delete_asset_selection_from_ui(payload)
            }
            ASSETS_MOVE_ASSET => {
                let payload =
                    parse_ui_payload::<AssetsMoveAssetPayload>("assets_ui_action", name, payload)?;
                self.move_asset_from_ui(payload)
            }
            ASSETS_MOVE_FOLDER => {
                let payload =
                    parse_ui_payload::<AssetsMoveFolderPayload>("assets_ui_action", name, payload)?;
                self.move_folder_from_ui(payload)
            }
            ASSETS_MOVE_SELECTION => {
                let payload = parse_ui_payload::<AssetsMoveSelectionPayload>(
                    "assets_ui_action",
                    name,
                    payload,
                )?;
                self.move_selection_from_ui(payload)
            }
            _ => Err(unknown_ui_action_error("assets_ui_action", name)),
        }
    }

    fn dispatch_export_ui_action(&mut self, name: &str, payload: serde_json::Value) -> Result<()> {
        match name {
            EXPORT_SET_DRAFT => {
                let payload = parse_ui_payload::<ExportDraftUpdatePayload>(
                    "export_ui_action",
                    name,
                    payload,
                )?;
                match payload {
                    ExportDraftUpdatePayload::BuiltinPreset(preset) => {
                        self.set_export_draft_builtin_preset(preset)
                    }
                    ExportDraftUpdatePayload::Preset(preset) => {
                        self.set_export_draft_preset(preset)
                    }
                    ExportDraftUpdatePayload::Sequence(sequence_id) => {
                        self.set_export_draft_sequence_id(sequence_id)
                    }
                    ExportDraftUpdatePayload::Range(range) => self.set_export_draft_range(range),
                    ExportDraftUpdatePayload::OutputPath(output_path) => {
                        self.set_export_draft_output_path(output_path)
                    }
                }
                Ok(())
            }
            EXPORT_ENQUEUE => {
                let payload =
                    parse_ui_payload::<ExportEnqueuePayload>("export_ui_action", name, payload)?;
                self.enqueue_timeline_export(TimelineExportRequest {
                    preset: payload.preset,
                    sequence_id: payload.sequence_id,
                    range: payload.range,
                    output_path: payload.output_path,
                })
                .map(|_| ())
            }
            EXPORT_CANCEL_JOB => {
                let payload =
                    parse_ui_payload::<ExportJobTargetPayload>("export_ui_action", name, payload)?;
                self.cancel_export_job(payload.job_id);
                Ok(())
            }
            EXPORT_CLEAR_COMPLETED => {
                self.clear_completed_exports();
                Ok(())
            }
            _ => Err(unknown_ui_action_error("export_ui_action", name)),
        }
    }

    fn dispatch_viewer_ui_action(&mut self, name: &str, payload: serde_json::Value) -> Result<()> {
        match name {
            VIEWER_SET_PREVIEW_RESOLUTION_SCALE => {
                let payload = parse_ui_payload::<ViewerSetPreviewResolutionScalePayload>(
                    "viewer_ui_action",
                    name,
                    payload,
                )?;
                self.set_preview_resolution_scale_from_ui(payload.scale)
            }
            VIEWER_SET_CLIP_TRANSFORM => {
                let payload = parse_ui_payload::<ViewerSetClipTransformPayload>(
                    "viewer_ui_action",
                    name,
                    payload,
                )?;
                self.set_clip_transform_from_viewer_ui(payload)
            }
            _ => Err(unknown_ui_action_error("viewer_ui_action", name)),
        }
    }

    fn dispatch_project_ui_action(&mut self, name: &str, payload: serde_json::Value) -> Result<()> {
        match name {
            PROJECT_CREATE_WITH_SETTINGS => {
                let payload = parse_ui_payload::<ProjectCreateWithSettingsPayload>(
                    "project_ui_action",
                    name,
                    payload,
                )?;
                self.create_project_from_ui(payload)
            }
            PROJECT_RECOVER_FROM_AUTOSAVE => {
                let payload = parse_ui_payload::<ProjectRecoverFromAutosavePayload>(
                    "project_ui_action",
                    name,
                    payload,
                )?;
                self.recover_project_from_autosave_ui(payload)
            }
            PROJECT_UPDATE_NEW_SEQUENCE_DEFAULTS => {
                let payload = parse_ui_payload::<ProjectUpdateNewSequenceDefaultsPayload>(
                    "project_ui_action",
                    name,
                    payload,
                )?;
                self.update_new_sequence_defaults(payload.settings)
            }
            PROJECT_UPDATE_COLOR_ENVIRONMENT => {
                let payload = parse_ui_payload::<ProjectUpdateColorEnvironmentPayload>(
                    "project_ui_action",
                    name,
                    payload,
                )?;
                self.update_project_color_environment(payload.color_environment)
            }
            _ => Err(unknown_ui_action_error("project_ui_action", name)),
        }
    }

    fn dispatch_sequence_ui_action(
        &mut self,
        name: &str,
        payload: serde_json::Value,
    ) -> Result<()> {
        match name {
            SEQUENCE_NEW => {
                let next = self.export_sequences_snapshot().len() + 1;
                self.new_sequence(&format!("Sequence {next}"));
                Ok(())
            }
            SEQUENCE_RETURN_TO_PARENT => {
                self.return_to_parent_sequence()?;
                Ok(())
            }
            SEQUENCE_SET_ACTIVE_DEFAULT => {
                let sequence_id =
                    self.active_sequence_id().ok_or_else(|| MondrianError::WorkflowStepFailed {
                        step_id: "sequence_ui_action".to_owned(),
                        reason: "当前没有活动序列".to_owned(),
                    })?;
                self.set_default_sequence(sequence_id)
            }
            SEQUENCE_SWITCH_ACTIVE => {
                let payload =
                    parse_ui_payload::<SequenceTargetPayload>("sequence_ui_action", name, payload)?;
                self.switch_active_sequence(payload.sequence_id)
            }
            SEQUENCE_DUPLICATE => {
                let payload =
                    parse_ui_payload::<SequenceTargetPayload>("sequence_ui_action", name, payload)?;
                let source = self.sequence_by_id(payload.sequence_id).ok_or_else(|| {
                    MondrianError::WorkflowStepFailed {
                        step_id: "sequence_ui_action".to_owned(),
                        reason: format!("序列不存在: {}", payload.sequence_id),
                    }
                })?;
                let name = format!("{} Copy", source.name);
                self.duplicate_sequence(payload.sequence_id, name).map(|_| ())
            }
            SEQUENCE_DELETE => {
                let payload =
                    parse_ui_payload::<SequenceTargetPayload>("sequence_ui_action", name, payload)?;
                self.delete_sequence(payload.sequence_id)
            }
            SEQUENCE_UPDATE_SETTINGS => {
                let payload = parse_ui_payload::<SequenceUpdateSettingsPayload>(
                    "sequence_ui_action",
                    name,
                    payload,
                )?;
                self.update_sequence_identity_and_settings(
                    payload.sequence_id,
                    payload.name,
                    payload.settings,
                )
            }
            _ => Err(unknown_ui_action_error("sequence_ui_action", name)),
        }
    }

    fn set_preview_resolution_scale_from_ui(&mut self, scale: f32) -> Result<()> {
        let scale = normalize_preview_resolution_scale(scale);

        let sequence_id = self
            .active_sequence_id()
            .or_else(|| self.active_sequence().map(|sequence| sequence.id))
            .ok_or_else(|| MondrianError::WorkflowStepFailed {
                step_id: "viewer_ui_action".to_string(),
                reason: "当前无序列".to_string(),
            })?;
        let before = self
            .sequences()
            .iter()
            .find(|sequence| sequence.id == sequence_id)
            .cloned()
            .or_else(|| {
                self.active_sequence().filter(|sequence| sequence.id == sequence_id).cloned()
            })
            .ok_or_else(|| MondrianError::WorkflowStepFailed {
                step_id: "viewer_ui_action".to_string(),
                reason: format!("序列不存在: {sequence_id}"),
            })?;

        let current = normalize_preview_resolution_scale(before.settings.preview.resolution_scale);
        if (current - scale).abs() <= f32::EPSILON {
            return Ok(());
        }

        let mut after = before.clone();
        let mut settings = after.settings.clone();
        settings.preview.resolution_scale = scale;
        after.apply_settings(settings)?;

        self.record_sequence_snapshot_command("修改预览分辨率", before, after)?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        self.set_status_hint(
            format!("预览分辨率：{}", preview_resolution_scale_label(scale)),
            false,
        );
        Ok(())
    }

    fn prepare_asset_drag_from_ui(&mut self, payload: AssetsPrepareDragPayload) -> Result<()> {
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("素材准备失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "assets_prepare_drag".into(), reason }
        })?;
        let asset = library.get_asset(payload.asset_id)?.ok_or_else(|| {
            let reason = format!("素材不存在：{}", payload.asset_id);
            self.set_status_hint(format!("素材准备失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "assets_prepare_drag".into(), reason }
        })?;
        let duration = if matches!(
            asset.kind,
            AssetKind::StillImage | AssetKind::AdjustmentLayer | AssetKind::SolidColor
        ) {
            self.default_visual_placement_drag_duration()?
        } else if asset.media_info.duration > Duration::ZERO {
            asset.media_info.duration
        } else {
            self.default_visual_placement_drag_duration()?
        };
        let has_linked_audio = matches!(asset.kind, AssetKind::Video) && asset.media_info.has_audio;
        let lane = match asset.kind {
            AssetKind::Audio => "音频轨",
            AssetKind::Video
            | AssetKind::StillImage
            | AssetKind::AdjustmentLayer
            | AssetKind::SolidColor => "视频轨",
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

    fn drop_asset_from_ui(&mut self, payload: TimelineDropAssetPayload) -> Result<()> {
        let needs_prepare = self
            .dragging_asset()
            .is_none_or(|dragging| dragging.asset_id != payload.asset_id);
        if needs_prepare {
            self.prepare_asset_drag_from_ui(AssetsPrepareDragPayload {
                asset_id: payload.asset_id,
            })?;
        }

        let result = if payload.is_video_track {
            self.drop_dragging_asset_to_video_track(payload.target_track_id, payload.frame)
        } else {
            self.drop_dragging_asset_to_audio_track(payload.target_track_id, payload.frame)
        };

        match result {
            Ok(_) => {
                self.set_status_hint("已添加素材到时间线".to_string(), false);
                Ok(())
            }
            Err(err) => {
                self.set_status_hint(format!("素材放置失败：{err}"), true);
                Err(err)
            }
        }
    }

    fn set_effect_property_from_ui(
        &mut self,
        selection: SelectedClipRef,
        effect_id: EffectId,
        path: &str,
        value: PropertyValue,
    ) -> Result<()> {
        self.ensure_clip_track_unlocked("set_effect_property", selection.clip_id)?;
        let Some(before) = self.active_sequence().cloned() else {
            return Err(missing_sequence_error("set_effect_property"));
        };
        let mut after = before.clone();
        let changed = {
            let clip = find_clip_mut(&mut after, selection.clip_id)
                .ok_or_else(|| missing_clip_error("set_effect_property", selection.clip_id))?;
            let effect = clip.effects.iter_mut().find(|e| e.id == effect_id).ok_or_else(|| {
                MondrianError::WorkflowStepFailed {
                    step_id: "set_effect_property".to_string(),
                    reason: format!("effect {effect_id} not found on clip"),
                }
            })?;
            let property = effect.properties.property(path).ok_or_else(|| {
                MondrianError::WorkflowStepFailed {
                    step_id: "set_effect_property".to_string(),
                    reason: format!("effect property not found: {path}"),
                }
            })?;
            if property.static_value() == &value {
                false
            } else {
                effect.apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: path.to_string(),
                    value,
                })?;
                true
            }
        };
        if changed {
            self.record_sequence_snapshot_command("调整特效属性", before, after)?;
        }
        Ok(())
    }

    fn set_clip_property_from_ui(
        &mut self,
        selection: SelectedClipRef,
        path: &str,
        value: PropertyValue,
    ) -> Result<()> {
        self.ensure_clip_track_unlocked("set_clip_property", selection.clip_id)?;
        let current_time = self.current_timeline_time()?.unwrap_or(TimelineTime::ZERO);
        let Some(before) = self.active_sequence().cloned() else {
            return Err(missing_sequence_error("set_clip_property"));
        };
        let mut after = before.clone();
        let changed = {
            let clip = find_clip_mut(&mut after, selection.clip_id)
                .ok_or_else(|| missing_clip_error("set_clip_property", selection.clip_id))?;
            let properties = clip.property_bag()?;
            let property =
                properties.property(path).ok_or_else(|| MondrianError::WorkflowStepFailed {
                    step_id: "set_clip_property".to_owned(),
                    reason: format!("Clip property not found: {path}"),
                })?;
            let mutation = if property.is_animated() {
                let end = clip.end_position()?;
                let sequence_time = current_time.clamp(clip.position, end);
                let author_time = clip.timeline_to_clip_time(sequence_time)?;
                if property.evaluate(author_time) == value {
                    None
                } else {
                    let interpolation = match value {
                        PropertyValue::Float(_)
                        | PropertyValue::Double(_)
                        | PropertyValue::Color(_)
                        | PropertyValue::Vec2(_)
                        | PropertyValue::Vec3(_)
                        | PropertyValue::Vec4(_) => InterpolationType::Linear,
                        PropertyValue::Bool(_)
                        | PropertyValue::Int(_)
                        | PropertyValue::Enum(_)
                        | PropertyValue::Resource(_)
                        | PropertyValue::Text(_) => InterpolationType::Hold,
                    };
                    Some(PropertyMutation::SetKeyframe {
                        path: path.to_owned(),
                        keyframe: Keyframe::from_preset(author_time, value, interpolation),
                    })
                }
            } else if property.static_value() == &value {
                None
            } else {
                Some(PropertyMutation::SetStaticValue { path: path.to_owned(), value })
            };
            if let Some(mutation) = mutation {
                clip.apply_property_mutation(mutation)?;
                true
            } else {
                false
            }
        };
        if changed {
            let sequence_id = after.id;
            self.record_sequence_snapshot_command("调整剪辑属性", before, after)?;
            self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        }
        Ok(())
    }

    fn set_selected_clips_enabled_from_ui(&mut self, enabled: bool) -> Result<()> {
        let clip_ids = self.selected_clip_ids_for_timeline_action();
        self.set_clips_enabled_from_ui("timeline_set_selected_clips_enabled", &clip_ids, enabled)
    }

    fn trim_selected_clips_to_playhead_from_ui(&mut self, edge: TrimEdge) -> Result<()> {
        let clip_ids = self.selected_clip_ids_for_timeline_action();
        if clip_ids.is_empty() {
            return Ok(());
        }
        let target_frame = match edge {
            TrimEdge::In => self.current_frame(),
            TrimEdge::Out => self.current_frame().saturating_add(1),
        };
        self.trim_clips_bulk_to_frame(&clip_ids, edge, target_frame).map(|_| ())
    }

    fn roll_selected_cut_to_playhead_from_ui(&mut self) -> Result<()> {
        let clip_ids = self.selected_clip_ids_for_timeline_action();
        let [clip_id] = clip_ids.as_slice() else {
            return Ok(());
        };
        match self.roll_cut_to_frame(*clip_id, self.current_frame())? {
            true => Ok(()),
            false => {
                self.set_status_hint("未找到可滚动切点，或播放头不在可滚动范围", true);
                Ok(())
            }
        }
    }

    fn set_in_out_point_from_ui(&mut self, payload: TimelineSetInOutPointPayload) -> Result<()> {
        let before = self
            .active_sequence()
            .cloned()
            .ok_or_else(|| missing_sequence_error("timeline_set_in_out_point"))?;
        let mut after = before.clone();
        let time = TimelineTime::from_frame_position(FramePosition::new(
            payload.frame,
            after.time_base(),
        ))?;
        match payload.point {
            TimelineInOutPointPayloadKind::In => after.mark_in(time),
            TimelineInOutPointPayloadKind::Out => after.mark_out(time),
        }
        let sequence_id = after.id;
        self.record_sequence_snapshot_command("设置时间线入出点", before, after)?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(())
    }

    fn clear_in_out_points_from_ui(&mut self) -> Result<()> {
        let before = self
            .active_sequence()
            .cloned()
            .ok_or_else(|| missing_sequence_error("timeline_clear_in_out_points"))?;
        if before.in_point.is_none() && before.out_point.is_none() {
            return Ok(());
        }
        let mut after = before.clone();
        after.clear_in_out();
        let sequence_id = after.id;
        self.record_sequence_snapshot_command("清除时间线入出点", before, after)?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(())
    }

    fn selected_clip_ids_for_timeline_action(&self) -> Vec<ClipId> {
        let mut clip_ids = Vec::new();
        for selection in &self.selection.selected_clips {
            if !clip_ids.contains(&selection.clip_id) {
                clip_ids.push(selection.clip_id);
            }
        }
        clip_ids
    }

    fn precompose_selection_from_ui(
        &mut self,
        payload: TimelinePrecomposeSelectionPayload,
    ) -> Result<()> {
        let name = payload.name.trim();
        if name.is_empty() {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "timeline_precompose_selection".to_owned(),
                reason: "嵌套序列名称不能为空".to_owned(),
            });
        }
        let selections = self
            .selected_clips()
            .iter()
            .map(|selection| {
                (
                    selection.track_id,
                    selection.is_video_track,
                    selection.clip_id,
                )
            })
            .collect::<Vec<_>>();
        let nested_clip_id = self.precompose_clips_as_sequence(&selections, name)?;
        self.select_clip_for_action("timeline_precompose_selection", nested_clip_id)?;
        self.set_status_hint(format!("已创建嵌套序列“{name}”"), false);
        Ok(())
    }

    fn set_clip_enabled_from_ui(&mut self, clip_id: ClipId, enabled: bool) -> Result<()> {
        self.set_clips_enabled_from_ui("inspector_set_clip_enabled", &[clip_id], enabled)
    }

    fn set_clips_enabled_from_ui(
        &mut self,
        step_id: &'static str,
        clip_ids: &[ClipId],
        enabled: bool,
    ) -> Result<()> {
        if clip_ids.is_empty() {
            return Ok(());
        }
        for clip_id in clip_ids {
            self.ensure_clip_track_unlocked(step_id, *clip_id)?;
        }
        let Some(before) = self.active_sequence().cloned() else {
            return Err(missing_sequence_error(step_id));
        };
        let mut after = before.clone();
        let mut changed = false;
        for clip_id in clip_ids {
            changed |= set_clip_disabled(&mut after, *clip_id, !enabled);
        }
        if changed {
            self.record_sequence_snapshot_command("切换片段启用状态", before, after)?;
            Ok(())
        } else if clip_ids.iter().all(|clip_id| clip_exists(&after, *clip_id)) {
            Ok(())
        } else {
            let missing = clip_ids
                .iter()
                .copied()
                .find(|clip_id| !clip_exists(&after, *clip_id))
                .unwrap_or(clip_ids[0]);
            Err(missing_clip_error(step_id, missing))
        }
    }

    fn set_clip_opacity_from_ui(&mut self, clip_id: ClipId, opacity_percent: f32) -> Result<()> {
        self.ensure_clip_track_unlocked("inspector_set_clip_opacity", clip_id)?;
        let Some(before) = self.active_sequence().cloned() else {
            return Err(missing_sequence_error("inspector_set_clip_opacity"));
        };
        let opacity = (opacity_percent / 100.0).clamp(0.0, 1.0);
        let mut after = before.clone();
        let playhead = after.playhead;
        let changed = {
            let clip = find_clip_mut(&mut after, clip_id)
                .ok_or_else(|| missing_clip_error("inspector_set_clip_opacity", clip_id))?;
            let author_time = clip_visual_author_time_at(clip, playhead)?;
            if (clip.transform.evaluate_opacity(author_time) - opacity).abs() < f32::EPSILON {
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
            self.record_sequence_snapshot_command("调整片段不透明度", before, after)?;
        }
        Ok(())
    }

    fn set_clip_tint_from_ui(
        &mut self,
        clip_id: ClipId,
        color: mondrian_core::Color,
    ) -> Result<()> {
        self.ensure_clip_track_unlocked("inspector_set_clip_tint", clip_id)?;
        let Some(before) = self.active_sequence().cloned() else {
            return Err(missing_sequence_error("inspector_set_clip_tint"));
        };
        let mut after = before.clone();
        let changed = {
            let clip = find_clip_mut(&mut after, clip_id)
                .ok_or_else(|| missing_clip_error("inspector_set_clip_tint", clip_id))?;
            if clip.content.solid_color() == Some(color) {
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
            self.record_sequence_snapshot_command("调整片段颜色", before, after)?;
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

        self.ensure_clip_track_unlocked("inspector_set_clip_transform_field", clip_id)?;
        let Some(before) = self.active_sequence().cloned() else {
            return Err(missing_sequence_error("inspector_set_clip_transform_field"));
        };
        let mut after = before.clone();
        let playhead = after.playhead;
        let changed = {
            let clip = find_clip_mut(&mut after, clip_id)
                .ok_or_else(|| missing_clip_error("inspector_set_clip_transform_field", clip_id))?;
            let author_time = clip_visual_author_time_at(clip, playhead)?;
            match field {
                InspectorClipTransformField::PositionX => {
                    let mut position = clip.transform.get_position(author_time);
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
                    let mut position = clip.transform.get_position(author_time);
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
                    if (clip.transform.get_scale(author_time) - scale).length_squared()
                        < f32::EPSILON
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
                        .evaluate(Transform2D::ROTATION_PATH, author_time)
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
            self.record_sequence_snapshot_command("调整片段变换", before, after)?;
        }
        Ok(())
    }

    fn set_clip_transform_from_viewer_ui(
        &mut self,
        payload: ViewerSetClipTransformPayload,
    ) -> Result<()> {
        const STEP_ID: &str = "viewer_set_clip_transform";
        if payload
            .position
            .is_some_and(|position| !position.x.is_finite() || !position.y.is_finite())
            || payload.scale_percent.is_some_and(|scale| !scale.is_finite())
            || payload.rotation_degrees.is_some_and(|rotation| !rotation.is_finite())
        {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: STEP_ID.to_string(),
                reason: "transform values must be finite".to_string(),
            });
        }

        self.ensure_clip_track_unlocked(STEP_ID, payload.clip.clip_id)?;
        let Some(before) = self.active_sequence().cloned() else {
            return Err(missing_sequence_error(STEP_ID));
        };
        let mut after = before.clone();
        let playhead = after.playhead;
        let changed = {
            let clip = find_clip_mut(&mut after, payload.clip.clip_id)
                .ok_or_else(|| missing_clip_error(STEP_ID, payload.clip.clip_id))?;
            let author_time = clip_visual_author_time_at(clip, playhead)?;
            let mut changed = false;

            if let Some(position) = payload.position {
                let position = Vec2::new(position.x, position.y);
                if (clip.transform.get_position(author_time) - position).length_squared()
                    >= f32::EPSILON
                {
                    clip.apply_property_mutation(PropertyMutation::SetStaticValue {
                        path: Transform2D::POSITION_PATH.to_string(),
                        value: PropertyValue::Vec2(position),
                    })?;
                    changed = true;
                }
            }

            if let Some(scale_percent) = payload.scale_percent {
                let scale = Vec2::splat(scale_percent.max(0.0) / 100.0);
                if (clip.transform.get_scale(author_time) - scale).length_squared() >= f32::EPSILON
                {
                    clip.apply_property_mutation(PropertyMutation::SetStaticValue {
                        path: Transform2D::SCALE_PATH.to_string(),
                        value: PropertyValue::Vec2(scale),
                    })?;
                    changed = true;
                }
            }

            if let Some(rotation) = payload.rotation_degrees {
                let current = clip
                    .transform
                    .to_property_bag()
                    .evaluate(Transform2D::ROTATION_PATH, author_time)
                    .and_then(|value| value.as_f32())
                    .unwrap_or(0.0);
                if (current - rotation).abs() >= f32::EPSILON {
                    clip.apply_property_mutation(PropertyMutation::SetStaticValue {
                        path: Transform2D::ROTATION_PATH.to_string(),
                        value: PropertyValue::Float(rotation),
                    })?;
                    changed = true;
                }
            }

            changed
        };
        if changed {
            self.record_sequence_snapshot_command("调整监视器片段变换", before, after)?;
        }
        Ok(())
    }

    fn edit_clip_curve_from_ui(&mut self, payload: InspectorEditClipCurvePayload) -> Result<()> {
        let selection = SelectedClipRef {
            track_id: payload.clip.track_id,
            is_video_track: payload.clip.is_video_track,
            clip_id: payload.clip.clip_id,
        };
        let edit = match payload.edit {
            InspectorCurveEditPayload::Upsert { keyframe_id, point } => {
                ClipNumericCurveEdit::Upsert {
                    keyframe_id,
                    point: NormalizedCurvePoint::new(f64::from(point.x), f64::from(point.y))?,
                }
            }
            InspectorCurveEditPayload::Remove { keyframe_id } => {
                ClipNumericCurveEdit::Remove { keyframe_id }
            }
        };
        let removing = matches!(edit, ClipNumericCurveEdit::Remove { .. });
        let property = payload.property;
        let outcome = self.edit_clip_numeric_curve(selection, property.clone(), edit)?;
        let key_selection = crate::app::AnimationKeyframeSelection {
            property: crate::app::AnimationPropertySelection {
                clip_id: selection.clip_id,
                property: property.clone(),
            },
            keyframe_id: outcome.keyframe_id,
        };
        if removing {
            self.animation_selection.selected_keyframes.remove(&key_selection);
            self.set_active_animation_property(selection.clip_id, property);
        } else {
            self.select_animation_keyframe_only(key_selection);
        }
        Ok(())
    }

    fn set_audio_component_source_from_ui(
        &mut self,
        payload: InspectorSetAudioComponentSourcePayload,
    ) -> Result<()> {
        const STEP_ID: &str = "inspector_set_audio_component_source";
        self.ensure_audio_component_edit_target(STEP_ID, payload.clip.clip_id, payload.edit_id)?;

        let target_source = {
            let sequence = self.active_sequence().ok_or_else(|| missing_sequence_error(STEP_ID))?;
            let clip = find_clip(sequence, payload.clip.clip_id)
                .ok_or_else(|| missing_clip_error(STEP_ID, payload.clip.clip_id))?;
            let current_source = clip
                .audio_components
                .iter()
                .find(|edit| edit.id == payload.edit_id)
                .map(|edit| edit.source.clone())
                .ok_or_else(|| MondrianError::WorkflowStepFailed {
                    step_id: STEP_ID.to_string(),
                    reason: format!(
                        "audio Component Edit {} is absent from Clip {}",
                        payload.edit_id, payload.clip.clip_id
                    ),
                })?;
            let requested_source = match payload.source {
                InspectorAudioComponentSourcePayload::Media { component_id } => {
                    AudioComponentSource::Media { component_id }
                }
                InspectorAudioComponentSourcePayload::NestedOutput { output_id } => {
                    AudioComponentSource::NestedOutput { output_id }
                }
            };
            if current_source == requested_source {
                return Ok(());
            }
            match payload.source {
                InspectorAudioComponentSourcePayload::Media { component_id } => {
                    if clip.is_nested_sequence() {
                        return Err(MondrianError::WorkflowStepFailed {
                            step_id: STEP_ID.to_string(),
                            reason: "nested Sequence Clip cannot select an Asset Component"
                                .to_string(),
                        });
                    }
                    let library =
                        self.asset_library().ok_or_else(|| MondrianError::WorkflowStepFailed {
                            step_id: STEP_ID.to_string(),
                            reason: "asset library is unavailable".to_string(),
                        })?;
                    let asset_id =
                        clip.media_asset_id().ok_or_else(|| MondrianError::WorkflowStepFailed {
                            step_id: STEP_ID.to_string(),
                            reason: "non-media Clip cannot select an Asset Component".to_string(),
                        })?;
                    let asset = library.get_asset(asset_id)?.ok_or_else(|| {
                        MondrianError::AssetNotFound { asset_id: asset_id.to_string() }
                    })?;
                    asset.audio_components.validate().map_err(|error| {
                        MondrianError::WorkflowStepFailed {
                            step_id: STEP_ID.to_string(),
                            reason: format!("invalid Asset audio Component catalog: {error}"),
                        }
                    })?;
                    if !asset
                        .audio_components
                        .components
                        .iter()
                        .any(|component| component.id == component_id)
                    {
                        return Err(MondrianError::WorkflowStepFailed {
                            step_id: STEP_ID.to_string(),
                            reason: format!(
                                "Asset {} does not expose audio Component {component_id}",
                                asset_id
                            ),
                        });
                    }
                    requested_source
                }
                InspectorAudioComponentSourcePayload::NestedOutput { output_id } => {
                    let child_id = clip.nested_sequence_id().ok_or_else(|| {
                        MondrianError::WorkflowStepFailed {
                            step_id: STEP_ID.to_string(),
                            reason: "media Clip cannot select a nested Sequence output".to_string(),
                        }
                    })?;
                    let child = self
                        .sequences()
                        .iter()
                        .find(|candidate| candidate.id == child_id)
                        .ok_or_else(|| MondrianError::WorkflowStepFailed {
                            step_id: STEP_ID.to_string(),
                            reason: format!("nested Sequence {child_id} is unavailable"),
                        })?;
                    if !child.audio_program.outputs.iter().any(|output| output.id == output_id) {
                        return Err(MondrianError::WorkflowStepFailed {
                            step_id: STEP_ID.to_string(),
                            reason: format!(
                                "nested Sequence {child_id} does not expose output {output_id}"
                            ),
                        });
                    }
                    requested_source
                }
            }
        };

        let Some(before) = self.active_sequence().cloned() else {
            return Err(missing_sequence_error(STEP_ID));
        };
        let mut after = before.clone();
        let changed = {
            let clip = find_clip_mut(&mut after, payload.clip.clip_id)
                .ok_or_else(|| missing_clip_error(STEP_ID, payload.clip.clip_id))?;
            let edit = clip
                .audio_components
                .iter_mut()
                .find(|edit| edit.id == payload.edit_id)
                .ok_or_else(|| MondrianError::WorkflowStepFailed {
                    step_id: STEP_ID.to_string(),
                    reason: format!("audio Component Edit {} disappeared", payload.edit_id),
                })?;
            if edit.source == target_source {
                false
            } else {
                edit.source = target_source;
                true
            }
        };
        if !changed {
            return Ok(());
        }
        if let Err(error) = after.audio_program.validate(
            &after.audio_tracks,
            &after.audio_roles,
            after.settings.audio_channel_layout,
        ) {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: STEP_ID.to_string(),
                reason: format!("audio authoring rejected source selection: {error}"),
            });
        }
        self.record_sequence_snapshot_command("切换片段音频 Component", before, after)?;
        self.refresh_audio_playback_after_authoring_change();
        Ok(())
    }

    fn set_audio_component_edit_field_from_ui(
        &mut self,
        payload: InspectorSetAudioComponentEditFieldPayload,
    ) -> Result<()> {
        const STEP_ID: &str = "inspector_set_audio_component_edit_field";
        self.ensure_audio_component_edit_target(STEP_ID, payload.clip.clip_id, payload.edit_id)?;

        let Some(before) = self.active_sequence().cloned() else {
            return Err(missing_sequence_error(STEP_ID));
        };
        let mut after = before.clone();
        let edit = find_clip_mut(&mut after, payload.clip.clip_id)
            .ok_or_else(|| missing_clip_error(STEP_ID, payload.clip.clip_id))?
            .audio_components
            .iter_mut()
            .find(|edit| edit.id == payload.edit_id)
            .ok_or_else(|| {
                missing_audio_component_edit_error(STEP_ID, payload.clip.clip_id, payload.edit_id)
            })?;
        let changed = match payload.field {
            InspectorAudioComponentEditField::Enabled(value) if edit.enabled != value => {
                edit.enabled = value;
                true
            }
            InspectorAudioComponentEditField::VolumeDb(value) if edit.volume_db != value => {
                edit.volume_db = value;
                true
            }
            InspectorAudioComponentEditField::Pan(value) if edit.pan != value => {
                edit.pan = value;
                true
            }
            InspectorAudioComponentEditField::FadeIn(value) if edit.fades.fade_in != value => {
                edit.fades.fade_in = value;
                true
            }
            InspectorAudioComponentEditField::FadeOut(value) if edit.fades.fade_out != value => {
                edit.fades.fade_out = value;
                true
            }
            _ => false,
        };
        if !changed {
            return Ok(());
        }
        if let Err(error) = after.audio_program.validate(
            &after.audio_tracks,
            &after.audio_roles,
            after.settings.audio_channel_layout,
        ) {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: STEP_ID.to_string(),
                reason: format!("audio authoring rejected Component edit: {error}"),
            });
        }
        self.record_sequence_snapshot_command("调整片段音频 Component", before, after)?;
        self.refresh_audio_playback_after_authoring_change();
        Ok(())
    }

    fn ensure_audio_component_edit_target(
        &self,
        step_id: &'static str,
        clip_id: ClipId,
        edit_id: AudioComponentEditId,
    ) -> Result<()> {
        self.ensure_clip_track_unlocked(step_id, clip_id)?;
        let sequence = self.active_sequence().ok_or_else(|| missing_sequence_error(step_id))?;
        let (_, is_video_track, _) = find_clip_track_lock(sequence, clip_id)
            .ok_or_else(|| missing_clip_error(step_id, clip_id))?;
        if is_video_track {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: step_id.to_string(),
                reason: "audio Component Edit must belong to an audio Track Clip".to_string(),
            });
        }
        let clip =
            find_clip(sequence, clip_id).ok_or_else(|| missing_clip_error(step_id, clip_id))?;
        if !clip.audio_components.iter().any(|edit| edit.id == edit_id) {
            return Err(missing_audio_component_edit_error(
                step_id, clip_id, edit_id,
            ));
        }
        Ok(())
    }

    fn ensure_clip_track_unlocked(&self, step_id: &'static str, clip_id: ClipId) -> Result<()> {
        let Some(seq) = self.active_sequence() else {
            return Err(missing_sequence_error(step_id));
        };
        let Some((track_id, _, is_locked)) = find_clip_track_lock(seq, clip_id) else {
            return Err(missing_clip_error(step_id, clip_id));
        };
        if is_locked {
            return Err(MondrianError::TrackLocked { track_id: track_id.to_string() });
        }
        Ok(())
    }
}

fn validate_asset_selection_move(
    library: &AssetLibrary,
    payload: &AssetsMoveSelectionPayload,
    folders: &[FolderRecord],
) -> Result<()> {
    if let Some(target_folder_id) = payload.target_folder_id.as_deref() {
        if !folders.iter().any(|folder| folder.id == target_folder_id) {
            return Err(MondrianError::AssetDbError {
                reason: format!("目标文件夹不存在：{target_folder_id}"),
            });
        }
    }

    for asset_id in &payload.asset_ids {
        if library.get_asset(*asset_id)?.is_none() {
            return Err(MondrianError::AssetNotFound { asset_id: asset_id.to_string() });
        }
    }

    for folder_id in &payload.folder_ids {
        if Some(folder_id.as_str()) == payload.target_folder_id.as_deref() {
            continue;
        }
        validate_folder_reparent(folders, folder_id, payload.target_folder_id.as_deref())?;
    }

    Ok(())
}

fn validate_folder_reparent(
    folders: &[FolderRecord],
    folder_id: &str,
    parent_folder_id: Option<&str>,
) -> Result<()> {
    if !folders.iter().any(|folder| folder.id == folder_id) {
        return Err(MondrianError::AssetDbError {
            reason: format!("文件夹不存在：{folder_id}")
        });
    }
    let Some(parent_id) = parent_folder_id else {
        return Ok(());
    };
    if parent_id == folder_id {
        return Err(MondrianError::AssetDbError {
            reason: "不能将文件夹移动到自身".to_string()
        });
    }
    if !folders.iter().any(|folder| folder.id == parent_id) {
        return Err(MondrianError::AssetDbError {
            reason: format!("目标文件夹不存在：{parent_id}"),
        });
    }
    let mut descendants = vec![folder_id.to_string()];
    let mut index = 0usize;
    while index < descendants.len() {
        let current = descendants[index].clone();
        for folder in folders {
            if folder.parent_id.as_deref() == Some(current.as_str())
                && !descendants.iter().any(|id| id == &folder.id)
            {
                descendants.push(folder.id.clone());
            }
        }
        index += 1;
    }
    if descendants.iter().any(|id| id == parent_id) {
        return Err(MondrianError::AssetDbError {
            reason: "不能将文件夹移动到其子文件夹中".to_string(),
        });
    }
    Ok(())
}

fn source_trim_target_frame(
    clip: &Clip,
    edge: TrimEdge,
    source_time: FramePosition,
) -> Result<i64> {
    let source_frame_rate = Rational::new(source_time.time_base.den, source_time.time_base.num);
    let source_time = TimelineTime::from_frame_position(source_time)?;
    match edge {
        TrimEdge::In if source_time >= clip.source_out => {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "trim_clip_source".to_string(),
                reason: "source in must be before current source out".to_string(),
            });
        }
        TrimEdge::Out if source_time <= clip.source_in => {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "trim_clip_source".to_string(),
                reason: "source out must be after current source in".to_string(),
            });
        }
        _ => {}
    }

    let speed = clip.speed.scale();
    if speed.numerator() <= 0 {
        return Err(MondrianError::WorkflowStepFailed {
            step_id: "trim_clip_source".to_string(),
            reason: "source trim requires a positive finite speed multiplier".to_string(),
        });
    }

    let source_delta = source_time.checked_sub(clip.source_in)?;
    let timeline_delta = source_delta.checked_scale(speed.reciprocal()?)?;
    let target = clip.position.checked_add(timeline_delta)?.max(TimelineTime::ZERO);
    target
        .to_frame_position(source_frame_rate, FrameRounding::Nearest)
        .map(|position| position.frame)
        .map_err(Into::into)
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

#[cfg(test)]
fn write_minimal_wav(path: &std::path::Path) {
    let sample_rate = 8_000u32;
    let channels = 1u16;
    let bits_per_sample = 16u16;
    let samples = [0i16; 16];
    let data_size = (samples.len() * std::mem::size_of::<i16>()) as u32;
    let byte_rate = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
    let block_align = channels * bits_per_sample / 8;
    let mut bytes = Vec::with_capacity(44 + data_size as usize);

    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_size.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }

    std::fs::write(path, bytes).expect("write wav fixture");
}

#[cfg(test)]
fn poll_media_imports_until_idle(state: &mut AppState) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while state.pending_media_import_batches() > 0 {
        state.poll_media_imports();
        if state.pending_media_import_batches() == 0 {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for background media import"
        );
        std::thread::sleep(Duration::from_millis(10));
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

fn unknown_ui_action_error(step_prefix: &'static str, name: &str) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: format!("{step_prefix}.{name}"),
        reason: format!("unknown app UI action: {name}"),
    }
}

fn preview_resolution_scale_label(scale: f32) -> String {
    let percent = normalize_preview_resolution_scale(scale) * 100.0;
    if (percent.fract()).abs() <= f32::EPSILON {
        format!("{}%", percent.round() as u32)
    } else {
        format!("{percent:.1}%")
    }
}

fn missing_sequence_error(step_id: &'static str) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: step_id.to_string(),
        reason: "当前没有活动序列".to_string(),
    }
}

fn clip_visual_author_time_at(clip: &Clip, timeline_time: TimelineTime) -> Result<TimelineTime> {
    let placement_end = clip.end_position()?;
    clip.timeline_to_clip_time(timeline_time.clamp(clip.position, placement_end))
}

fn missing_clip_error(step_id: &'static str, clip_id: ClipId) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: step_id.to_string(),
        reason: format!("片段不存在: {clip_id}"),
    }
}

fn missing_audio_component_edit_error(
    step_id: &'static str,
    clip_id: ClipId,
    edit_id: AudioComponentEditId,
) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: step_id.to_string(),
        reason: format!("audio Component Edit {edit_id} is absent from Clip {clip_id}"),
    }
}

fn clip_media_type_mismatch_error(step_id: &'static str, clip_id: ClipId) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: step_id.to_string(),
        reason: format!("片段媒体类型与目标轨道类型不匹配: {clip_id}"),
    }
}

fn missing_effect_error(
    step_id: &'static str,
    clip_id: ClipId,
    effect_id: EffectId,
) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: step_id.to_string(),
        reason: format!("效果不存在: clip={clip_id}, effect={effect_id}"),
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

    fn tt(frame: i64, time_base: mondrian_core::Rational) -> mondrian_core::TimelineTime {
        let numerator = frame.checked_mul(time_base.num).expect("test time fits i64");
        mondrian_core::TimelineTime::new(numerator, time_base.den).expect("valid test time")
    }
    use crate::app::ui_actions::{
        assets_create_adjustment_layer_action, assets_create_folder_action,
        assets_create_solid_color_action, assets_delete_asset_action, assets_delete_folder_action,
        assets_delete_selection_action, assets_import_files_action, assets_move_asset_action,
        assets_move_folder_action, assets_move_selection_action, assets_prepare_drag_action,
        assets_rebind_audio_component_action, assets_refresh_audio_components_action,
        assets_relink_asset_action, assets_rename_asset_action, assets_rename_folder_action,
        assets_set_interpretation_action, assets_set_proxy_mode_action, effects_add_to_clip_action,
        export_cancel_job_action, export_clear_completed_action, export_enqueue_action,
        export_set_draft_action, inspector_edit_clip_curve_action, inspector_remove_effect_action,
        inspector_select_effect_action, inspector_set_audio_component_edit_field_action,
        inspector_set_audio_component_source_action, inspector_set_clip_enabled_action,
        inspector_set_clip_opacity_action, inspector_set_clip_property_action,
        inspector_set_clip_tint_action, inspector_set_clip_transform_field_action,
        inspector_set_effect_enabled_action, inspector_set_effect_property_action,
        project_create_with_settings_action, project_recover_from_autosave_action,
        project_update_color_environment_action, project_update_new_sequence_defaults_action,
        sequence_delete_action, sequence_duplicate_action, sequence_new_action,
        sequence_return_to_parent_action, sequence_set_active_default_action,
        sequence_switch_active_action, sequence_update_settings_action, timeline_add_track_action,
        timeline_clear_in_out_points_action, timeline_create_basic_title_action,
        timeline_drop_asset_action, timeline_insert_asset_action, timeline_move_clip_action,
        timeline_move_track_action, timeline_open_nested_sequence_action,
        timeline_roll_selected_cut_to_playhead_action, timeline_seek_action,
        timeline_seek_with_source_action, timeline_select_clip_action,
        timeline_set_in_out_point_action, timeline_set_selected_clips_enabled_action,
        timeline_set_track_control_action, timeline_trim_clips_action,
        timeline_trim_selected_clips_to_playhead_action, viewer_set_clip_transform_action,
        viewer_set_preview_resolution_scale_action, AssetsCreateAssetPayload,
        AssetsCreateFolderPayload, AssetsDeleteAssetPayload, AssetsDeleteFolderPayload,
        AssetsDeleteSelectionPayload, AssetsImportFilesPayload, AssetsMoveAssetPayload,
        AssetsMoveFolderPayload, AssetsMoveSelectionPayload, AssetsPrepareDragPayload,
        AssetsRebindAudioComponentPayload, AssetsRefreshAudioComponentsPayload,
        AssetsRelinkAssetPayload, AssetsRenameAssetPayload, AssetsRenameFolderPayload,
        AssetsSetInterpretationPayload, AssetsSetProxyModePayload, EffectsAddToClipPayload,
        ExportDraftUpdatePayload, ExportEnqueuePayload, ExportJobTargetPayload,
        InspectorAudioComponentEditField, InspectorAudioComponentSourcePayload,
        InspectorClipRefPayload, InspectorClipTransformField, InspectorCurveEditPayload,
        InspectorCurvePointPayload, InspectorEditClipCurvePayload, InspectorRemoveEffectPayload,
        InspectorSelectEffectPayload, InspectorSetAudioComponentEditFieldPayload,
        InspectorSetAudioComponentSourcePayload, InspectorSetClipEnabledPayload,
        InspectorSetClipOpacityPayload, InspectorSetClipPropertyPayload,
        InspectorSetClipTintPayload, InspectorSetClipTransformFieldPayload,
        InspectorSetEffectEnabledPayload, InspectorSetEffectPropertyPayload,
        ProjectCreateWithSettingsPayload, ProjectRecoverFromAutosavePayload,
        ProjectUpdateColorEnvironmentPayload, ProjectUpdateNewSequenceDefaultsPayload,
        SequenceTargetPayload, SequenceUpdateSettingsPayload, TimelineAddTrackKind,
        TimelineAddTrackPayload, TimelineDropAssetPayload, TimelineInOutPointPayloadKind,
        TimelineInsertAssetPayload, TimelineMoveTrackPayload, TimelineOpenNestedSequencePayload,
        TimelineSeekSource, TimelineSetInOutPointPayload, TimelineSetSelectedClipsEnabledPayload,
        TimelineSetTrackControlPayload, TimelineTrackControlPayloadKind, TimelineTrimClipsPayload,
        TimelineTrimPayloadEdge, TimelineTrimSelectedClipsToPlayheadPayload,
        ViewerSetClipTransformPayload, ViewerSetPreviewResolutionScalePayload,
        ViewerTransformPositionPayload,
    };
    use mondrian_assets::AssetLibrary;
    use mondrian_core::automation::AnimationParameterAddress;
    use mondrian_core::timeline_data::{AssetMediaInterpretation, MediaColorInterpretation};
    use mondrian_core::types::{
        AssetId, AudioSourceComponentId, ClipLinkGroupId, EffectId, FramePosition, MaskId, TrackId,
    };
    use mondrian_core::{Color, ColorSpace};
    use mondrian_core::{ProjectSettings, Rational, Resolution, WorkingColorSpace};
    use mondrian_effects::EffectType;
    use mondrian_media::info::{AudioCodec, ChannelLayout};
    use mondrian_media::{AudioStreamInfo, MediaInfo};
    use mondrian_timeline::audio::{AudioFade, AudioFadeCurve};
    use mondrian_timeline::clip::Clip;
    use mondrian_timeline::sequence::{
        PreviewRenderFormat, Sequence, SequencePreviewSettings, SequenceSettings,
    };
    use mondrian_timeline::{
        InsertAutomationPolicy, InsertTimelineStatePolicy, InsertTransitionPolicy,
    };

    fn pinned_test_custom_engine(working_space: &str) -> mondrian_core::ColorEngine {
        mondrian_core::ColorEngine::CustomOcio {
            identity: Box::new(
                mondrian_core::CustomOcioProjectIdentity::from_pinned_parts(
                    mondrian_core::OcioConfigSource::Environment,
                    "0".repeat(64),
                    "test-config".to_owned(),
                    "0".repeat(64),
                    working_space.to_owned(),
                    vec![mondrian_core::CustomOcioOutputIdentity::from_pinned_parts(
                        ColorSpace::Rec709,
                        "Test Display".to_owned(),
                        "Test View".to_owned(),
                        "Test Display Color Space".to_owned(),
                        mondrian_core::CustomOcioLookIdentity::None,
                    )
                    .expect("valid Custom OCIO output binding")],
                    Vec::new(),
                    Vec::new(),
                )
                .expect("structurally valid Custom OCIO identity"),
            ),
        }
    }

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
        let clip = Clip::new(AssetId::new(), tt(10, tb), tt(20, tb)).expect("valid clip");
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("add clip");
        state.test_set_sequence(Some(sequence));
        (state, track_id, clip_id)
    }

    fn audio_test_media_info(path: &std::path::Path) -> MediaInfo {
        let stream = |index, stream_id, language: &str, is_default| AudioStreamInfo {
            index,
            stream_id: Some(stream_id),
            language: Some(language.to_owned()),
            title: None,
            is_default,
            codec: AudioCodec::Aac,
            duration: Some(Duration::from_secs(1)),
            sample_rate: 48_000,
            channels: 2,
            channel_layout: ChannelLayout::Stereo,
            bit_depth: 24,
            avg_bitrate: 256_000,
        };
        MediaInfo {
            path: path.to_path_buf(),
            duration: Duration::from_secs(1),
            file_size: 1,
            container: "mov".to_string(),
            video_streams: Vec::new(),
            audio_streams: vec![stream(1, 10, "eng", false), stream(3, 30, "jpn", true)],
            has_video: false,
            has_audio: true,
        }
    }

    fn state_with_audio_asset() -> (
        PathBuf,
        AppState,
        TrackId,
        mondrian_core::ClipId,
        mondrian_core::AudioComponentEditId,
        AudioSourceComponentId,
    ) {
        let root = unique_temp_path("audio-component-action");
        let library = AssetLibrary::open(root.join("library")).expect("asset library");
        let media_path = root.join("component-source.mov");
        std::fs::write(&media_path, [0u8]).expect("media fixture");
        let asset_id = library
            .upsert_media_file_with_info(&media_path, audio_test_media_info(&media_path))
            .expect("register audio asset");
        let asset = library.get_asset(asset_id).expect("asset query").expect("asset");
        let alternate_component = asset
            .audio_components
            .components
            .iter()
            .find(|component| component.id != AudioSourceComponentId::primary())
            .expect("secondary Component")
            .id;

        let mut state = AppState::new();
        let mut sequence = Sequence::new("audio source selection");
        let track_id = sequence.audio_tracks[0].id;
        let clip = Clip::new(asset_id, TimelineTime::ZERO, tt(25, sequence.time_base()))
            .expect("audio Clip");
        let clip_id = clip.id;
        sequence
            .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
            .expect("add audio Clip");
        let edit_id = sequence.audio_tracks[0].clips[0].audio_components[0].id;
        state.test_set_sequence(Some(sequence));
        state.test_set_asset_library(Some(library));
        (root, state, track_id, clip_id, edit_id, alternate_component)
    }

    fn inspector_clip_payload(
        track_id: mondrian_core::types::TrackId,
        clip_id: mondrian_core::types::ClipId,
    ) -> InspectorClipRefPayload {
        InspectorClipRefPayload { track_id, is_video_track: true, clip_id }
    }

    fn opacity_parameter_address(state: &AppState, clip_id: ClipId) -> AnimationParameterAddress {
        let clip = state
            .active_sequence()
            .and_then(|sequence| find_clip(sequence, clip_id))
            .expect("clip");
        clip.transform
            .to_property_bag()
            .address_for_path(Transform2D::OPACITY_PATH)
            .expect("opacity address")
    }

    fn opacity_property_selection(
        state: &AppState,
        clip_id: ClipId,
    ) -> crate::app::AnimationPropertySelection {
        crate::app::AnimationPropertySelection {
            clip_id,
            property: opacity_parameter_address(state, clip_id),
        }
    }

    fn opacity_keyframe_selection(
        state: &AppState,
        clip_id: ClipId,
        time: TimelineTime,
    ) -> crate::app::AnimationKeyframeSelection {
        let property = opacity_property_selection(state, clip_id);
        let keyframe_id = state
            .active_sequence()
            .and_then(|sequence| find_clip(sequence, clip_id))
            .and_then(|clip| {
                let bag = clip.transform.to_property_bag();
                bag.property_by_address(&property.property)
                    .and_then(|(_, property)| property.keyframe_at(time))
                    .map(|keyframe| keyframe.id)
            })
            .unwrap_or_default();
        crate::app::AnimationKeyframeSelection { property, keyframe_id }
    }

    fn add_default_effect_with_first_property(
        state: &mut AppState,
        effect_type: EffectType,
    ) -> (EffectId, String, PropertyValue) {
        let effect: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(effect_type);
        let effect_id = effect.id;
        let clip = &mut state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0]
            .clips[0];
        clip.add_effect_node(effect);
        let effect = clip
            .effects
            .iter()
            .find(|effect| effect.id == effect_id)
            .expect("inserted effect");
        let (path, property) = effect
            .properties
            .iter()
            .next()
            .expect("effect should expose at least one property");
        (effect_id, path.to_string(), property.static_value().clone())
    }

    fn different_property_value(value: &PropertyValue) -> PropertyValue {
        match value {
            PropertyValue::Bool(value) => PropertyValue::Bool(!value),
            PropertyValue::Int(value) => PropertyValue::Int(value.saturating_add(1)),
            PropertyValue::Float(value) => PropertyValue::Float(*value + 0.5),
            PropertyValue::Double(value) => PropertyValue::Double(*value + 0.5),
            PropertyValue::Vec2(value) => PropertyValue::Vec2(*value + glam::Vec2::splat(0.5)),
            PropertyValue::Vec3(value) => PropertyValue::Vec3(*value + glam::Vec3::splat(0.5)),
            PropertyValue::Color(_) => PropertyValue::Color(Color::from_hex(0x44AAFF)),
            PropertyValue::Vec4(value) => {
                let mut changed = *value;
                changed[0] += 0.5;
                PropertyValue::Vec4(changed)
            }
            PropertyValue::Enum(value) => PropertyValue::Enum(value.clone()),
            PropertyValue::Resource(reference) => match reference {
                mondrian_core::automation::ParameterResourceReference::Unbound => {
                    PropertyValue::Resource(
                        mondrian_core::automation::ParameterResourceReference::ExternalFile {
                            path: std::path::PathBuf::from("test-resource.cube"),
                        },
                    )
                }
                _ => PropertyValue::Resource(
                    mondrian_core::automation::ParameterResourceReference::Unbound,
                ),
            },
            PropertyValue::Text(value) => PropertyValue::Text(format!("{value} edited")),
        }
    }

    #[test]
    fn dispatch_noop_does_not_mutate_editor_state() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];

        state
            .dispatch_action(mondrian_editor_state::Action::NoOp)
            .expect("dispatch noop");

        assert_eq!(
            state.selection.selected_clips,
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }]
        );
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_registered_ui_namespaces_reject_unknown_action_names() {
        for (namespace, expected_step) in [
            (TIMELINE_NAMESPACE, "timeline_ui_action.unknown"),
            (INSPECTOR_NAMESPACE, "inspector_ui_action.unknown"),
            (EFFECTS_NAMESPACE, "effects_ui_action.unknown"),
            (ASSETS_NAMESPACE, "assets_ui_action.unknown"),
            (EXPORT_NAMESPACE, "export_ui_action.unknown"),
            (PROJECT_NAMESPACE, "project_ui_action.unknown"),
            (SEQUENCE_NAMESPACE, "sequence_ui_action.unknown"),
        ] {
            let mut state = AppState::new();
            let err = state
                .dispatch_action(mondrian_editor_state::Action::Custom {
                    namespace: namespace.into(),
                    name: "unknown".into(),
                    payload: serde_json::Value::Null,
                })
                .expect_err("registered UI namespace should reject unknown action names");

            match err {
                MondrianError::WorkflowStepFailed { step_id, reason } => {
                    assert_eq!(step_id, expected_step);
                    assert!(reason.contains("unknown app UI action"));
                }
                other => panic!("expected unknown UI action workflow error, got {other:?}"),
            }
        }
    }

    #[test]
    fn dispatch_export_ui_rejects_empty_output_path_without_queueing() {
        let mut state = AppState::new();
        state.test_set_sequence(Some(Sequence::new("export")));

        let err = state
            .dispatch_action(export_enqueue_action(ExportEnqueuePayload {
                preset: mondrian_export::preset::ExportPreset::h264_aac_sdr_1080p(),
                sequence_id: None,
                range: mondrian_export::preset::TimelineExportRange::EntireSequence,
                output_path: PathBuf::new(),
            }))
            .expect_err("empty output path should fail");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state.render_queue.list_jobs().is_empty());
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
    }

    #[test]
    fn dispatch_export_ui_updates_draft_fields() {
        let mut state = AppState::new();
        let sequence_id = mondrian_core::types::SequenceId::new();
        let builtin = mondrian_export::preset::BuiltinExportPreset::ProRes4444Alpha;
        let mut customized = builtin.preset();
        customized.video_signal.range = mondrian_export::preset::ExportParameter::FollowSequence;

        state
            .dispatch_action(export_set_draft_action(
                ExportDraftUpdatePayload::BuiltinPreset(builtin),
            ))
            .expect("set preset");
        state
            .dispatch_action(export_set_draft_action(ExportDraftUpdatePayload::Preset(
                customized.clone(),
            )))
            .expect("customize preset");
        state
            .dispatch_action(export_set_draft_action(ExportDraftUpdatePayload::Sequence(
                Some(sequence_id),
            )))
            .expect("set sequence");
        state
            .dispatch_action(export_set_draft_action(ExportDraftUpdatePayload::Range(
                mondrian_export::preset::TimelineExportRange::EntireSequence,
            )))
            .expect("set range");
        state
            .dispatch_action(export_set_draft_action(
                ExportDraftUpdatePayload::OutputPath("E:/renders/out.mp4".to_owned()),
            ))
            .expect("set output path");

        assert_eq!(state.export_draft.selected_builtin_preset, builtin);
        assert_eq!(state.export_draft.preset, customized);
        assert_eq!(state.export_draft.selected_sequence_id, Some(sequence_id));
        assert_eq!(
            state.export_draft.range,
            mondrian_export::preset::TimelineExportRange::EntireSequence
        );
        assert_eq!(state.export_draft.output_path, "E:/renders/out.mp4");
    }

    #[test]
    fn dispatch_export_ui_routes_queue_management_actions() {
        let mut state = AppState::new();
        let job_id = mondrian_core::types::JobId::new();

        state
            .dispatch_action(export_cancel_job_action(ExportJobTargetPayload { job_id }))
            .expect("cancel missing job should be a queue no-op");
        state
            .dispatch_action(export_clear_completed_action())
            .expect("clear completed should be a queue no-op when empty");

        assert!(state.render_queue.list_jobs().is_empty());
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
    fn dispatch_timeline_ui_selects_clip_by_authoritative_clip_id() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        let stale_track_id = state.active_sequence().expect("sequence").video_tracks[1].id;
        state.selection.selected_mask = Some((MaskId::new(), clip_id, track_id));
        let property = opacity_property_selection(&state, clip_id);
        let keyframe = opacity_keyframe_selection(
            &state,
            clip_id,
            tt(12, state.active_sequence().expect("sequence").time_base()),
        );
        state.animation_selection.active_property = Some(property);
        state.animation_selection.selected_keyframes.insert(keyframe);

        state
            .dispatch_action(timeline_select_clip_action(TimelineSelectClipPayload {
                track_id: stale_track_id,
                is_video_track: false,
                clip_id,
            }))
            .expect("dispatch select");

        assert_eq!(
            state.selection.selected_clips,
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }]
        );
        assert!(state.selection.selected_mask.is_none());
        assert!(state.animation_selection.active_property.is_none());
        assert!(state.animation_selection.selected_keyframes.is_empty());
    }

    #[test]
    fn dispatch_timeline_ui_seek_updates_playback_frame() {
        let (mut state, _, _) = state_with_two_video_tracks();

        state
            .dispatch_action(timeline_seek_with_source_action(
                33,
                TimelineSeekSource::PointerDrag,
            ))
            .expect("dispatch seek");

        assert_eq!(state.current_frame(), 33);
        assert_eq!(
            state.last_timeline_seek_source,
            TimelineSeekSource::PointerDrag
        );

        state.dispatch_action(timeline_seek_action(44)).expect("dispatch seek");

        assert_eq!(state.current_frame(), 44);
        assert_eq!(state.last_timeline_seek_source, TimelineSeekSource::Settled);
    }

    #[test]
    fn dispatch_timeline_ui_opens_nested_sequence() {
        let mut state = AppState::new();
        let child = Sequence::new("child");
        let child_id = child.id;
        let mut parent = Sequence::new("parent");
        let tb = parent.time_base();
        parent.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(
                    child_id,
                    tt(0, tb),
                    tt(24, tb),
                    Some("child".to_owned()),
                )
                .expect("valid clip"),
            )
            .expect("add nested clip");
        state.test_set_active_sequence(parent.id);
        state.test_set_sequence(Some(parent));
        state.test_add_sequence(child);

        state
            .dispatch_action(timeline_open_nested_sequence_action(
                TimelineOpenNestedSequencePayload { sequence_id: child_id },
            ))
            .expect("open nested sequence");

        assert_eq!(state.active_sequence_id(), Some(child_id));
        assert_eq!(
            state.active_sequence().map(|sequence| sequence.id),
            Some(child_id)
        );
        assert_eq!(state.test_navigation_stack().len(), 1);
    }

    #[test]
    fn dispatch_sequence_ui_returns_to_parent_sequence() {
        let mut state = AppState::new();
        let child = Sequence::new("child");
        let child_id = child.id;
        let parent = Sequence::new("parent");
        let parent_id = parent.id;
        state.test_set_active_sequence(child_id);
        state.test_set_sequence(Some(child));
        state.test_add_sequence(parent);
        state.test_set_navigation_stack(vec![parent_id]);

        state
            .dispatch_action(sequence_return_to_parent_action())
            .expect("return to parent sequence");

        assert_eq!(state.active_sequence_id(), Some(parent_id));
        assert_eq!(
            state.active_sequence().map(|sequence| sequence.id),
            Some(parent_id)
        );
        assert!(state.test_navigation_stack().is_empty());
    }

    #[test]
    fn dispatch_sequence_ui_sets_active_sequence_as_default() {
        let mut state = AppState::new();
        let sequence = Sequence::new("default candidate");
        let sequence_id = sequence.id;
        state.test_set_active_sequence(sequence_id);
        state.test_set_sequence(Some(sequence.clone()));
        state.test_add_sequence(sequence);

        state
            .dispatch_action(sequence_set_active_default_action())
            .expect("set active default sequence");

        assert_eq!(state.default_sequence_id(), Some(sequence_id));
    }

    #[test]
    fn dispatch_sequence_ui_creates_new_sequence() {
        let mut state = AppState::new();
        state.test_set_sequence(Some(Sequence::new("Existing")));

        state.dispatch_action(sequence_new_action()).expect("create sequence");

        assert_eq!(state.sequences().len(), 2);
        assert_eq!(
            state.active_sequence().map(|sequence| sequence.name.as_str()),
            Some("Sequence 2")
        );
        assert_eq!(
            state.active_sequence_id(),
            state.active_sequence().map(|sequence| sequence.id)
        );
    }

    #[test]
    fn dispatch_sequence_ui_switches_active_sequence() {
        let mut state = AppState::new();
        let first = Sequence::new("first");
        let second = Sequence::new("second");
        let second_id = second.id;
        state.test_set_active_sequence(first.id);
        state.test_set_sequence(Some(first.clone()));
        state.test_add_sequence(first);
        state.test_add_sequence(second);

        state
            .dispatch_action(sequence_switch_active_action(SequenceTargetPayload {
                sequence_id: second_id,
            }))
            .expect("switch sequence");

        assert_eq!(state.active_sequence_id(), Some(second_id));
        assert_eq!(
            state.active_sequence().map(|sequence| sequence.name.as_str()),
            Some("second")
        );
    }

    #[test]
    fn dispatch_sequence_ui_duplicates_sequence_and_activates_copy() {
        let mut state = AppState::new();
        let source = Sequence::new("source");
        let source_id = source.id;
        state.test_set_active_sequence(source_id);
        state.test_set_sequence(Some(source.clone()));
        state.test_add_sequence(source);

        state
            .dispatch_action(sequence_duplicate_action(SequenceTargetPayload {
                sequence_id: source_id,
            }))
            .expect("duplicate sequence");

        assert_eq!(state.sequences().len(), 2);
        assert_ne!(state.active_sequence_id(), Some(source_id));
        assert_eq!(
            state.active_sequence().map(|sequence| sequence.name.as_str()),
            Some("source Copy")
        );
    }

    #[test]
    fn dispatch_sequence_ui_deletes_sequence_and_keeps_fallback_active() {
        let mut state = AppState::new();
        let first = Sequence::new("first");
        let first_id = first.id;
        let second = Sequence::new("second");
        let second_id = second.id;
        state.test_set_active_sequence(second_id);
        state.test_set_default_sequence(second_id);
        state.test_set_sequence(Some(second.clone()));
        state.test_add_sequence(first);
        state.test_add_sequence(second);

        state
            .dispatch_action(sequence_delete_action(SequenceTargetPayload {
                sequence_id: second_id,
            }))
            .expect("delete sequence");

        assert_eq!(state.sequences().len(), 1);
        assert_eq!(state.active_sequence_id(), Some(first_id));
        assert_eq!(state.default_sequence_id(), Some(first_id));
        assert_eq!(
            state.active_sequence().map(|sequence| sequence.name.as_str()),
            Some("first")
        );
    }

    #[test]
    fn dispatch_sequence_ui_updates_settings_atomically() {
        let mut state = AppState::new();
        let sequence = Sequence::new("offline");
        let sequence_id = sequence.id;
        state.test_set_active_sequence(sequence_id);
        state.test_set_sequence(Some(sequence.clone()));
        state.test_add_sequence(sequence);
        let settings = SequenceSettings {
            resolution: Resolution::UHD4K,
            frame_rate: Rational::FPS_2997,
            audio_sample_rate: 96_000,
            preview: SequencePreviewSettings {
                cache_enabled: false,
                ..SequencePreviewSettings::default()
            },
            ..SequenceSettings::default()
        };
        state
            .dispatch_action(sequence_update_settings_action(
                SequenceUpdateSettingsPayload {
                    sequence_id,
                    name: "Final Cut".to_owned(),
                    settings: settings.clone(),
                },
            ))
            .expect("update sequence settings");

        let active = state.active_sequence().expect("active sequence");
        assert_eq!(active.name, "Final Cut");
        assert_eq!(active.settings, settings);
        assert_eq!(state.sequences()[0].name, "Final Cut");
        assert!(state.can_undo_action());

        state.undo_timeline().expect("undo");
        let active = state.active_sequence().expect("active sequence");
        assert_eq!(active.name, "offline");
        assert_eq!(active.settings, SequenceSettings::default());
    }

    #[test]
    fn dispatch_sequence_ui_rejects_invalid_settings_without_renaming() {
        let mut state = AppState::new();
        let sequence = Sequence::new("original");
        let sequence_id = sequence.id;
        state.test_set_active_sequence(sequence_id);
        state.test_set_sequence(Some(sequence.clone()));
        state.test_add_sequence(sequence);
        let invalid_settings = SequenceSettings {
            audio_sample_rate: 12_345,
            ..SequenceSettings::default()
        };

        let err = state
            .dispatch_action(sequence_update_settings_action(
                SequenceUpdateSettingsPayload {
                    sequence_id,
                    name: "Should Not Stick".to_owned(),
                    settings: invalid_settings,
                },
            ))
            .expect_err("invalid settings should fail");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        let active = state.active_sequence().expect("active sequence");
        assert_eq!(active.name, "original");
        assert_eq!(active.settings, SequenceSettings::default());
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_sequence_ui_rejects_custom_ocio_working_space_mismatch_atomically() {
        let mut state = AppState::new();
        let sequence = Sequence::new("original");
        let sequence_id = sequence.id;
        state.test_set_active_sequence(sequence_id);
        state.test_set_sequence(Some(sequence.clone()));
        state.test_add_sequence(sequence);
        let mut settings = SequenceSettings::default();
        settings.color.working_color_space = WorkingColorSpace::AcesCg;
        *state.test_project_color_environment_mut() = mondrian_core::ProjectColorEnvironment::new(
            pinned_test_custom_engine("Linear Rec.2020"),
        );

        let error = state
            .dispatch_action(sequence_update_settings_action(
                SequenceUpdateSettingsPayload {
                    sequence_id,
                    name: "Must Not Stick".to_owned(),
                    settings,
                },
            ))
            .expect_err("Custom OCIO working mismatch must fail before mutation");

        assert!(error.to_string().contains("pins working space 'Linear Rec.2020'"));
        let active = state.active_sequence().expect("active sequence");
        assert_eq!(active.name, "original");
        assert_eq!(active.settings, SequenceSettings::default());
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_sequence_ui_rejects_standard_working_space_mismatch_atomically() {
        let mut state = AppState::new();
        let sequence = Sequence::new("original");
        let sequence_id = sequence.id;
        state.test_set_active_sequence(sequence_id);
        state.test_set_sequence(Some(sequence.clone()));
        state.test_add_sequence(sequence);
        let mut settings = SequenceSettings::default();
        settings.color.working_color_space = WorkingColorSpace::LinearP3D65;

        let error = state
            .dispatch_action(sequence_update_settings_action(
                SequenceUpdateSettingsPayload { sequence_id, name: "changed".to_owned(), settings },
            ))
            .expect_err("Standard working mismatch must fail before mutation");

        assert!(error.to_string().contains("Mondrian Standard"));
        let active = state.active_sequence().expect("active sequence");
        assert_eq!(active.name, "original");
        assert_eq!(active.settings, SequenceSettings::default());
        assert!(!state.can_undo_action());
    }

    #[test]
    fn new_sequence_adopts_custom_ocio_pinned_working_space() {
        let mut state = AppState::new();
        let mut existing = Sequence::new("Existing");
        existing.settings.color.working_color_space = WorkingColorSpace::AcesCg;
        state.test_set_sequence(Some(existing));
        *state.test_project_color_environment_mut() =
            mondrian_core::ProjectColorEnvironment::new(pinned_test_custom_engine("ACEScg"));
        let defaults = state.test_new_sequence_defaults_mut();
        defaults.color.working_color_space = WorkingColorSpace::AcesCg;

        state.new_sequence("Custom Working");

        assert_eq!(
            state
                .active_sequence()
                .expect("new active sequence")
                .settings
                .color
                .working_color_space,
            WorkingColorSpace::AcesCg
        );
    }

    #[test]
    fn dispatch_viewer_ui_updates_preview_scale_without_stopping_playback() {
        let mut state = AppState::new();
        let sequence = Sequence::new("preview");
        let sequence_id = sequence.id;
        state.test_set_active_sequence(sequence_id);
        state.test_set_sequence(Some(sequence.clone()));
        state.test_add_sequence(sequence);
        state.play();

        state
            .dispatch_action(viewer_set_preview_resolution_scale_action(
                ViewerSetPreviewResolutionScalePayload { scale: 0.25 },
            ))
            .expect("set preview scale");

        assert_eq!(
            state
                .active_sequence()
                .expect("active sequence")
                .settings
                .preview
                .resolution_scale,
            0.25
        );
        assert_eq!(state.sequences()[0].settings.preview.resolution_scale, 0.25);
        assert!(state.is_playing());
        assert!(state.can_undo_action());

        state.undo_timeline().expect("undo");
        assert_eq!(
            state
                .active_sequence()
                .expect("active sequence")
                .settings
                .preview
                .resolution_scale,
            SequenceSettings::default().preview.resolution_scale
        );
    }

    #[test]
    fn dispatch_viewer_ui_clamps_out_of_range_preview_scale() {
        let mut state = AppState::new();
        let sequence = Sequence::new("preview");
        let sequence_id = sequence.id;
        state.test_set_active_sequence(sequence_id);
        state.test_set_sequence(Some(sequence.clone()));
        state.test_add_sequence(sequence);

        state
            .dispatch_action(viewer_set_preview_resolution_scale_action(
                ViewerSetPreviewResolutionScalePayload { scale: 0.0 },
            ))
            .expect("set preview scale");

        assert_eq!(
            state
                .active_sequence()
                .expect("active sequence")
                .settings
                .preview
                .resolution_scale,
            0.125
        );
    }

    #[test]
    fn dispatch_timeline_ui_track_controls_update_real_tracks() {
        let (mut state, video_track_id, _) = state_with_two_video_tracks();
        let audio_track_id = state.active_sequence().expect("sequence").audio_tracks[0].id;

        state
            .dispatch_action(timeline_set_track_control_action(
                TimelineSetTrackControlPayload {
                    track_id: video_track_id,
                    is_video_track: true,
                    control: TimelineTrackControlPayloadKind::Visibility,
                    enabled: false,
                },
            ))
            .expect("toggle visibility");
        assert!(!state.active_sequence().expect("sequence").video_tracks[0].is_visible);
        assert!(state.can_undo_action());

        state
            .dispatch_action(timeline_set_track_control_action(
                TimelineSetTrackControlPayload {
                    track_id: audio_track_id,
                    is_video_track: false,
                    control: TimelineTrackControlPayloadKind::Mute,
                    enabled: true,
                },
            ))
            .expect("toggle mute");
        assert!(state.active_sequence().expect("sequence").audio_tracks[0].is_muted);

        state
            .dispatch_action(timeline_set_track_control_action(
                TimelineSetTrackControlPayload {
                    track_id: video_track_id,
                    is_video_track: true,
                    control: TimelineTrackControlPayloadKind::Lock,
                    enabled: true,
                },
            ))
            .expect("toggle lock");
        assert!(state.active_sequence().expect("sequence").video_tracks[0].is_locked);

        state.undo_timeline().expect("undo lock");
        assert!(!state.active_sequence().expect("sequence").video_tracks[0].is_locked);
        assert!(!state.active_sequence().expect("sequence").video_tracks[0].is_visible);
        assert!(state.active_sequence().expect("sequence").audio_tracks[0].is_muted);
    }

    #[test]
    fn dispatch_timeline_ui_moves_track_to_target_index() {
        let (mut state, first_track_id, _) = state_with_two_video_tracks();
        let second_track_id = state.active_sequence().expect("sequence").video_tracks[1].id;

        state
            .dispatch_action(timeline_move_track_action(TimelineMoveTrackPayload {
                track_id: second_track_id,
                is_video_track: true,
                target_index: 0,
            }))
            .expect("move track");

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert_eq!(sequence.video_tracks[0].id, second_track_id);
        assert_eq!(sequence.video_tracks[1].id, first_track_id);
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_timeline_ui_drops_asset_to_video_track() {
        let (mut state, target_track_id, _) = state_with_two_video_tracks();
        let library_root = unique_temp_path("timeline-drop-asset-library");
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let asset_id = library
            .create_solid_color_asset(Some("Slate"))
            .expect("create solid color asset");
        state.test_set_asset_library(Some(library));

        state
            .dispatch_action(timeline_drop_asset_action(TimelineDropAssetPayload {
                asset_id,
                target_track_id,
                is_video_track: true,
                frame: 40,
            }))
            .expect("drop asset");

        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        let created = sequence.video_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.library_asset_id() == Some(asset_id))
            .expect("created clip");
        assert_eq!(created.position, tt(40, tb));
        assert_eq!(created.label.as_deref(), Some("Slate"));
        assert!(state.dragging_asset().is_none());
        assert!(state.can_undo_action());
        assert!(state
            .status_hint
            .as_ref()
            .is_some_and(|(message, is_error)| !*is_error && message.contains("已添加素材")));

        remove_temp_path(&library_root);
    }

    #[test]
    fn dispatch_timeline_ui_performs_atomic_professional_insert() {
        let (mut state, target_track_id, original_clip_id) = state_with_two_video_tracks();
        let secondary_track_id = state.active_sequence().expect("sequence").video_tracks[1].id;
        let library_root = unique_temp_path("timeline-insert-asset-library");
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let asset_id = library
            .create_solid_color_asset(Some("Insert"))
            .expect("create solid color asset");
        state.test_set_asset_library(Some(library));

        let generation_before = state.project_author_generation();
        state
            .dispatch_action(timeline_insert_asset_action(TimelineInsertAssetPayload {
                asset_id,
                insert_frame: 15,
                source_in_frame: 0,
                duration_frames: 5,
                video_target_track_id: Some(target_track_id),
                audio_target_track_id: None,
                ripple_track_ids: vec![target_track_id, secondary_track_id],
                automation_policy: InsertAutomationPolicy::FollowEditorialContent,
                transition_policy: InsertTransitionPolicy::RejectAffected,
                timeline_state_policy: InsertTimelineStatePolicy::FollowEdit,
            }))
            .expect("Insert Edit");

        let sequence = state.active_sequence().expect("sequence");
        let track = &sequence.video_tracks[0];
        let tb = sequence.time_base();
        assert_eq!(state.project_author_generation(), generation_before + 1);
        assert_eq!(
            track
                .clips
                .iter()
                .find(|clip| clip.id == original_clip_id)
                .expect("left fragment")
                .duration,
            tt(5, tb)
        );
        assert!(track.clips.iter().any(|clip| {
            clip.library_asset_id() == Some(asset_id)
                && clip.position == tt(15, tb)
                && clip.duration == tt(5, tb)
        }));
        assert!(track.clips.iter().any(|clip| {
            clip.id != original_clip_id
                && clip.library_asset_id() != Some(asset_id)
                && clip.position == tt(20, tb)
                && clip.duration == tt(15, tb)
        }));
        assert!(state.can_undo_action());
        state.undo_timeline().expect("undo Insert");
        let restored = state.active_sequence().expect("restored sequence");
        assert_eq!(restored.video_tracks[0].clips.len(), 1);
        assert_eq!(restored.video_tracks[0].clips[0].position, tt(10, tb));
        assert_eq!(restored.video_tracks[0].clips[0].duration, tt(20, tb));

        remove_temp_path(&library_root);
    }

    #[test]
    fn dispatch_timeline_ui_rejects_incompatible_asset_drop_without_clip_mutation() {
        let (mut state, _, _) = state_with_two_video_tracks();
        let target_track_id = state.active_sequence().expect("sequence").audio_tracks[0].id;
        let initial_audio_clip_count =
            state.active_sequence().expect("sequence").audio_tracks[0].clips.len();
        let library_root = unique_temp_path("timeline-drop-incompatible-library");
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let asset_id = library
            .create_solid_color_asset(Some("Video Only"))
            .expect("create solid color asset");
        state.test_set_asset_library(Some(library));

        let err = state
            .dispatch_action(timeline_drop_asset_action(TimelineDropAssetPayload {
                asset_id,
                target_track_id,
                is_video_track: false,
                frame: 12,
            }))
            .expect_err("solid color should not drop onto audio track");

        assert!(matches!(err, MondrianError::UnsupportedFormat { .. }));
        assert_eq!(
            state.active_sequence().expect("sequence").audio_tracks[0].clips.len(),
            initial_audio_clip_count
        );
        assert!(state.dragging_asset().is_some());
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));

        remove_temp_path(&library_root);
    }

    #[test]
    fn dispatch_timeline_ui_adds_video_and_audio_tracks() {
        let (mut state, _, _) = state_with_two_video_tracks();
        let initial_video_tracks = state.active_sequence().expect("sequence").video_tracks.len();
        let initial_audio_tracks = state.active_sequence().expect("sequence").audio_tracks.len();

        state
            .dispatch_action(timeline_add_track_action(TimelineAddTrackPayload {
                kind: TimelineAddTrackKind::Video,
            }))
            .expect("add video track");
        state
            .dispatch_action(timeline_add_track_action(TimelineAddTrackPayload {
                kind: TimelineAddTrackKind::Audio,
            }))
            .expect("add audio track");

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert_eq!(sequence.video_tracks.len(), initial_video_tracks + 1);
        assert_eq!(sequence.audio_tracks.len(), initial_audio_tracks + 1);
        assert!(state.can_undo_action());

        state.undo_timeline().expect("undo audio track add");
        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert_eq!(sequence.video_tracks.len(), initial_video_tracks + 1);
        assert_eq!(sequence.audio_tracks.len(), initial_audio_tracks);

        state.undo_timeline().expect("undo video track add");
        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert_eq!(sequence.video_tracks.len(), initial_video_tracks);
        assert_eq!(sequence.audio_tracks.len(), initial_audio_tracks);
    }

    #[test]
    fn dispatch_timeline_ui_moves_clip_to_target_track() {
        let (mut state, source_track_id, clip_id) = state_with_two_video_tracks();
        let target_track_id = state.active_sequence().unwrap().video_tracks[1].id;
        state.selection.selected_clips = vec![SelectedClipRef {
            track_id: source_track_id,
            is_video_track: true,
            clip_id,
        }];

        state
            .dispatch_action(timeline_move_clip_action(TimelineMoveClipPayload {
                target_track_id,
                is_video_track: true,
                clip_id,
                frame: 42,
            }))
            .expect("dispatch move");

        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        assert!(sequence.video_tracks[0].clips.is_empty());
        let moved = &sequence.video_tracks[1].clips[0];
        assert_eq!(moved.id, clip_id);
        assert_eq!(moved.position, tt(42, tb));
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
    fn dispatch_timeline_ui_rejects_cross_media_clip_move() {
        let (mut state, source_track_id, clip_id) = state_with_two_video_tracks();
        let target_track_id = state.active_sequence().unwrap().audio_tracks[0].id;
        state.selection.selected_clips = vec![SelectedClipRef {
            track_id: source_track_id,
            is_video_track: true,
            clip_id,
        }];

        let err = state
            .dispatch_action(timeline_move_clip_action(TimelineMoveClipPayload {
                target_track_id,
                is_video_track: false,
                clip_id,
                frame: 42,
            }))
            .expect_err("cross-media move should fail");

        assert!(matches!(
            err,
            MondrianError::WorkflowStepFailed { step_id, .. } if step_id == "move_clip"
        ));
        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert_eq!(sequence.video_tracks[0].clips.len(), 1);
        assert_eq!(sequence.video_tracks[0].clips[0].id, clip_id);
        assert!(sequence.audio_tracks[0].clips.is_empty());
        assert_eq!(
            state.selection.selected_clips,
            vec![SelectedClipRef {
                track_id: source_track_id,
                is_video_track: true,
                clip_id,
            }]
        );
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_timeline_ui_trims_clip_edge() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();

        state
            .dispatch_action(timeline_trim_clips_action(TimelineTrimClipsPayload {
                clip_ids: vec![clip_id],
                edge: TimelineTrimPayloadEdge::In,
                frame: 16,
            }))
            .expect("dispatch trim");

        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        let clip = &sequence.video_tracks[0].clips[0];
        assert_eq!(clip.position, tt(16, tb));
        assert_eq!(clip.duration, tt(14, tb));
    }

    #[test]
    fn dispatch_timeline_ui_trims_multiple_clip_edges() {
        let (mut state, _, first_clip_id) = state_with_two_video_tracks();
        let tb = state.active_sequence().expect("sequence").time_base();
        let second_clip = Clip::new(AssetId::new(), tt(12, tb), tt(30, tb)).expect("valid clip");
        let second_clip_id = second_clip.id;
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[1]
            .add_clip(second_clip)
            .expect("add second clip");

        state
            .dispatch_action(timeline_trim_clips_action(TimelineTrimClipsPayload {
                clip_ids: vec![first_clip_id, second_clip_id],
                edge: TimelineTrimPayloadEdge::In,
                frame: 16,
            }))
            .expect("dispatch batch trim");

        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        let first = &sequence.video_tracks[0].clips[0];
        let second = &sequence.video_tracks[1].clips[0];
        assert_eq!(first.position, tt(16, tb));
        assert_eq!(first.duration, tt(14, tb));
        assert_eq!(second.position, tt(16, tb));
        assert_eq!(second.duration, tt(26, tb));
    }

    #[test]
    fn dispatch_timeline_ui_trims_current_selection_to_playhead() {
        let (mut state, first_track_id, first_clip_id) = state_with_two_video_tracks();
        let tb = state.active_sequence().expect("sequence").time_base();
        let second_clip = Clip::new(AssetId::new(), tt(12, tb), tt(30, tb)).expect("valid clip");
        let second_clip_id = second_clip.id;
        let second_track_id = state.active_sequence().expect("sequence").video_tracks[1].id;
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[1]
            .add_clip(second_clip)
            .expect("add second clip");
        state.selection.selected_clips = vec![
            SelectedClipRef {
                track_id: first_track_id,
                is_video_track: true,
                clip_id: first_clip_id,
            },
            SelectedClipRef {
                track_id: second_track_id,
                is_video_track: true,
                clip_id: second_clip_id,
            },
        ];
        state.seek(18);

        state
            .dispatch_action(timeline_trim_selected_clips_to_playhead_action(
                TimelineTrimSelectedClipsToPlayheadPayload { edge: TimelineTrimPayloadEdge::In },
            ))
            .expect("dispatch selected trim");

        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        let first = &sequence.video_tracks[0].clips[0];
        let second = &sequence.video_tracks[1].clips[0];
        assert_eq!(first.position, tt(18, tb));
        assert_eq!(first.duration, tt(12, tb));
        assert_eq!(second.position, tt(18, tb));
        assert_eq!(second.duration, tt(24, tb));
    }

    #[test]
    fn dispatch_timeline_ui_rolls_single_selected_cut_to_playhead() {
        let (mut state, track_id, clip_a_id) = state_with_two_video_tracks();
        let tb = state.active_sequence().expect("sequence").time_base();
        let clip_b = Clip::new(AssetId::new(), tt(30, tb), tt(20, tb)).expect("valid clip");
        let clip_b_id = clip_b.id;
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0]
            .add_clip(clip_b)
            .expect("add adjacent clip");
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id: clip_a_id }];
        state.seek(35);

        state
            .dispatch_action(timeline_roll_selected_cut_to_playhead_action())
            .expect("roll selected cut");

        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        let first = sequence.video_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == clip_a_id)
            .expect("first clip");
        let second = sequence.video_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == clip_b_id)
            .expect("second clip");
        assert_eq!(first.duration, tt(25, tb));
        assert_eq!(second.position, tt(35, tb));
        assert_eq!(second.duration, tt(15, tb));
        assert_eq!(second.source_in, tt(5, tb));
    }

    #[test]
    fn dispatch_timeline_ui_sets_current_selection_enabled_state_atomically() {
        let (mut state, first_track_id, first_clip_id) = state_with_two_video_tracks();
        let tb = state.active_sequence().expect("sequence").time_base();
        let second_clip = Clip::new(AssetId::new(), tt(40, tb), tt(10, tb)).expect("valid clip");
        let second_clip_id = second_clip.id;
        let second_track_id = state.active_sequence().expect("sequence").video_tracks[1].id;
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[1]
            .add_clip(second_clip)
            .expect("add second clip");
        state.selection.selected_clips = vec![
            SelectedClipRef {
                track_id: first_track_id,
                is_video_track: true,
                clip_id: first_clip_id,
            },
            SelectedClipRef {
                track_id: second_track_id,
                is_video_track: true,
                clip_id: second_clip_id,
            },
        ];

        state
            .dispatch_action(timeline_set_selected_clips_enabled_action(
                TimelineSetSelectedClipsEnabledPayload { enabled: false },
            ))
            .expect("disable selection");

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert!(sequence.video_tracks[0].clips[0].is_disabled);
        assert!(sequence.video_tracks[1].clips[0].is_disabled);

        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[1].is_locked = true;
        let result = state.dispatch_action(timeline_set_selected_clips_enabled_action(
            TimelineSetSelectedClipsEnabledPayload { enabled: true },
        ));

        assert!(result.is_err());
        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert!(
            sequence.video_tracks[0].clips[0].is_disabled,
            "locked-track rejection must not partially enable earlier selected clips"
        );
        assert!(sequence.video_tracks[1].clips[0].is_disabled);
    }

    #[test]
    fn dispatch_trim_clip_start_uses_source_in_time() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let tb = state.active_sequence().expect("sequence").time_base();

        state
            .dispatch_action(mondrian_editor_state::Action::TrimClipStart {
                clip_id,
                new_source_in: FramePosition::new(5, tb),
            })
            .expect("trim source in");

        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        let clip = &sequence.video_tracks[0].clips[0];
        assert_eq!(clip.position, tt(15, tb));
        assert_eq!(clip.duration, tt(15, tb));
        assert_eq!(clip.source_in, tt(5, tb));
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_trim_clip_end_uses_source_out_time() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let tb = state.active_sequence().expect("sequence").time_base();

        state
            .dispatch_action(mondrian_editor_state::Action::TrimClipEnd {
                clip_id,
                new_source_out: FramePosition::new(12, tb),
            })
            .expect("trim source out");

        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        let clip = &sequence.video_tracks[0].clips[0];
        assert_eq!(clip.position, tt(10, tb));
        assert_eq!(clip.duration, tt(12, tb));
        assert_eq!(clip.source_out, tt(12, tb));
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_trim_clip_end_rejects_source_out_before_source_in() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let tb = state.active_sequence().expect("sequence").time_base();

        let err = state
            .dispatch_action(mondrian_editor_state::Action::TrimClipEnd {
                clip_id,
                new_source_out: FramePosition::new(0, tb),
            })
            .expect_err("invalid source out should be rejected");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        assert_eq!(sequence.video_tracks[0].clips[0].duration, tt(20, tb));
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
        state.test_set_asset_library(Some(
            AssetLibrary::open(library_root.clone()).expect("library"),
        ));
        let missing_path = library_root.join("missing.mov");

        state
            .dispatch_action(mondrian_editor_state::Action::ImportMedia(vec![
                missing_path,
            ]))
            .expect("invalid media path is reported by background import completion");

        assert_eq!(state.pending_media_import_batches(), 1);
        assert!(state
            .status_hint
            .as_ref()
            .is_some_and(|(message, is_error)| !*is_error && message.contains("正在导入")));
        poll_media_imports_until_idle(&mut state);
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
        let assets = state.asset_library().expect("library").list_assets().expect("list assets");
        assert!(assets.is_empty());

        remove_temp_path(&library_root);
    }

    #[test]
    fn proxy_import_policy_follows_project_settings() {
        let mut state = AppState::new();

        state.auto_proxy_enabled = true;
        state.test_project_settings_mut().proxy_enabled = false;
        assert!(!state.should_auto_generate_proxy_for_import());

        state.auto_proxy_enabled = false;
        state.test_project_settings_mut().proxy_enabled = true;
        assert!(state.should_auto_generate_proxy_for_import());
    }

    #[test]
    fn project_proxy_config_controls_media_proxy_generation_contract() {
        let mut state = AppState::new();
        let cache_root = unique_temp_path("project-proxy-cache");
        state.test_project_settings_mut().proxy_resolution = Resolution::FHD;
        state.test_project_settings_mut().cache_dir = Some(cache_root.clone());

        let proxy_config = state.proxy_config();

        assert_eq!(
            proxy_config.resolution,
            mondrian_media::ProxyResolution::P1080
        );
        assert_eq!(proxy_config.cache_dir, cache_root.join("proxy"));

        remove_temp_path(&cache_root);
    }

    #[test]
    fn dispatch_assets_import_files_places_media_in_target_folder() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-import-folder-library");
        let media_root = unique_temp_path("assets-import-folder-media");
        std::fs::create_dir_all(&media_root).expect("media root");
        let media_path = media_root.join("tone.wav");
        write_minimal_wav(&media_path);

        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let folder_id = library.create_folder("Rushes", None).expect("create folder");
        state.test_set_asset_library(Some(library));

        state
            .dispatch_action(assets_import_files_action(AssetsImportFilesPayload {
                paths: vec![media_path.clone()],
                folder_id: Some(folder_id.clone()),
            }))
            .expect("schedule media import into folder");

        assert_eq!(state.pending_media_import_batches(), 1);

        poll_media_imports_until_idle(&mut state);

        let library = state.asset_library().expect("library");
        let assets = library.list_assets().expect("list assets");
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].name, "tone.wav");
        assert_eq!(assets[0].folder_id.as_deref(), Some(folder_id.as_str()));
        assert!(state
            .status_hint
            .as_ref()
            .is_some_and(|(message, is_error)| !*is_error && message.contains("已导入 1")));

        remove_temp_path(&library_root);
        remove_temp_path(&media_root);
    }

    #[test]
    fn dispatch_assets_import_files_rejects_missing_target_folder_before_importing() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-import-missing-folder-library");
        state.test_set_asset_library(Some(
            AssetLibrary::open(library_root.clone()).expect("library"),
        ));

        let err = state
            .dispatch_action(assets_import_files_action(AssetsImportFilesPayload {
                paths: vec![PathBuf::from("E:/media/missing.wav")],
                folder_id: Some("deleted-folder".to_owned()),
            }))
            .expect_err("missing folder should fail");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
        assert!(state
            .asset_library()
            .expect("library")
            .list_assets()
            .expect("list assets")
            .is_empty());

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
        state.test_set_asset_library(Some(library));

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
    fn dispatch_assets_audio_component_rebind_reports_missing_library() {
        let mut state = AppState::new();

        let error = state
            .dispatch_action(assets_rebind_audio_component_action(
                AssetsRebindAudioComponentPayload {
                    asset_id: AssetId::new(),
                    component_id: AudioSourceComponentId::new(),
                    stream_index: 3,
                },
            ))
            .expect_err("missing asset library should fail");

        assert!(matches!(
            error,
            MondrianError::WorkflowStepFailed { step_id, .. }
                if step_id == "assets_rebind_audio_component"
        ));
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
    }

    #[test]
    fn dispatch_assets_audio_component_refresh_reports_missing_library() {
        let mut state = AppState::new();

        let error = state
            .dispatch_action(assets_refresh_audio_components_action(
                AssetsRefreshAudioComponentsPayload { asset_id: AssetId::new() },
            ))
            .expect_err("missing asset library should fail");

        assert!(matches!(
            error,
            MondrianError::WorkflowStepFailed { step_id, .. }
                if step_id == "assets_refresh_audio_components"
        ));
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
    }

    #[test]
    fn dispatch_assets_delete_asset_removes_library_record_and_timeline_refs() {
        let (mut state, _, _) = state_with_two_video_tracks();
        let library_root = unique_temp_path("assets-delete-action-library");
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let asset_id = library.create_solid_color_asset(Some("Temp Plate")).expect("create asset");
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].clips[0]
            .content =
            mondrian_core::timeline_data::ClipContent::SolidColor { asset_id, color: Color::BLACK };
        state.test_set_asset_library(Some(library));
        let events = state.event_bus.subscribe();

        state
            .dispatch_action(assets_delete_asset_action(AssetsDeleteAssetPayload {
                asset_id,
            }))
            .expect("delete asset");

        assert!(state
            .asset_library()
            .expect("library")
            .get_asset(asset_id)
            .expect("get asset")
            .is_none());
        assert!(
            state.active_sequence().expect("sequence").video_tracks.iter().all(|track| {
                track.clips.iter().all(|clip| clip.library_asset_id() != Some(asset_id))
            })
        );
        assert!(state.can_undo_action());
        assert!(state
            .status_hint
            .as_ref()
            .is_some_and(|(message, is_error)| !*is_error && message.contains("Temp Plate")));
        let mut saw_asset_deleted = false;
        while let Ok(event) = events.try_recv() {
            if matches!(event, AppEvent::AssetDeleted { asset_id: event_asset_id } if event_asset_id == asset_id)
            {
                saw_asset_deleted = true;
            }
        }
        assert!(saw_asset_deleted);

        remove_temp_path(&library_root);
    }

    #[test]
    fn dispatch_assets_delete_asset_reports_missing_library() {
        let mut state = AppState::new();

        let err = state
            .dispatch_action(assets_delete_asset_action(AssetsDeleteAssetPayload {
                asset_id: AssetId::new(),
            }))
            .expect_err("missing library should fail");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
    }

    #[test]
    fn dispatch_assets_relink_asset_reports_missing_library() {
        let mut state = AppState::new();

        let err = state
            .dispatch_action(assets_relink_asset_action(AssetsRelinkAssetPayload {
                asset_id: AssetId::new(),
                path: PathBuf::from("E:/media/relinked.mov"),
            }))
            .expect_err("missing library should fail");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(
            state.status_hint.as_ref().is_some_and(|(message, is_error)| {
                *is_error && message.contains("重新链接素材失败")
            })
        );
    }

    #[test]
    fn dispatch_assets_relink_asset_updates_library_path_and_publishes_reload() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-relink-action-library");
        let media_root = unique_temp_path("assets-relink-action-media");
        std::fs::create_dir_all(&media_root).expect("media root");
        let original_path = media_root.join("original.wav");
        let replacement_path = media_root.join("replacement.wav");
        write_minimal_wav(&original_path);
        write_minimal_wav(&replacement_path);

        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let asset_id = library.import_media_file(&original_path).expect("import original");
        state.test_set_asset_library(Some(library));
        let events = state.event_bus.subscribe();

        state
            .dispatch_action(assets_relink_asset_action(AssetsRelinkAssetPayload {
                asset_id,
                path: replacement_path.clone(),
            }))
            .expect("relink asset");

        let asset = state
            .asset_library()
            .expect("library")
            .get_asset(asset_id)
            .expect("get asset")
            .expect("asset");
        assert_eq!(
            asset.path,
            replacement_path.canonicalize().expect("canonical path")
        );
        assert!(
            state.status_hint.as_ref().is_some_and(|(message, is_error)| {
                !*is_error && message.contains("已重新链接素材") && message.contains("original.wav")
            })
        );
        assert!(events.try_iter().any(|event| matches!(event, AppEvent::AssetLibraryReloaded)));

        remove_temp_path(&library_root);
        remove_temp_path(&media_root);
    }

    #[test]
    fn dispatch_assets_rename_asset_updates_library_and_publishes_reload() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-rename-action-library");
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let asset_id = library.create_solid_color_asset(Some("Old Name")).expect("create asset");
        state.test_set_asset_library(Some(library));
        let events = state.event_bus.subscribe();

        state
            .dispatch_action(assets_rename_asset_action(AssetsRenameAssetPayload {
                asset_id,
                name: "New Name".to_owned(),
            }))
            .expect("rename asset");

        let asset = state
            .asset_library()
            .expect("library")
            .get_asset(asset_id)
            .expect("get asset")
            .expect("asset");
        assert_eq!(asset.name, "New Name");
        assert!(state
            .status_hint
            .as_ref()
            .is_some_and(|(message, is_error)| { !*is_error && message.contains("New Name") }));
        assert!(events.try_iter().any(|event| matches!(event, AppEvent::AssetLibraryReloaded)));

        remove_temp_path(&library_root);
    }

    #[test]
    fn dispatch_assets_set_interpretation_updates_library_and_publishes_reload() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-interpret-action-library");
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let asset_id = library.create_solid_color_asset(Some("Shot A")).expect("create asset");
        state.test_set_asset_library(Some(library));
        let events = state.event_bus.subscribe();
        let interpretation = AssetMediaInterpretation {
            color: MediaColorInterpretation::Override { color_space: ColorSpace::Rec2100Pq },
            ..AssetMediaInterpretation::default()
        };

        state
            .dispatch_action(assets_set_interpretation_action(
                AssetsSetInterpretationPayload { asset_id, interpretation },
            ))
            .expect("set interpretation");

        let asset = state
            .asset_library()
            .expect("library")
            .get_asset(asset_id)
            .expect("get asset")
            .expect("asset");
        assert_eq!(asset.interpretation, interpretation);
        assert!(
            state.status_hint.as_ref().is_some_and(|(message, is_error)| {
                !*is_error && message.contains("已更新素材解释") && message.contains("Shot A")
            })
        );
        assert!(events.try_iter().any(|event| matches!(event, AppEvent::AssetLibraryReloaded)));

        remove_temp_path(&library_root);
    }

    #[test]
    fn dispatch_assets_rename_folder_updates_library_and_publishes_reload() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-rename-folder-action-library");
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let folder_id = library.create_folder("Old Bin", None).expect("create folder");
        state.test_set_asset_library(Some(library));
        let events = state.event_bus.subscribe();

        state
            .dispatch_action(assets_rename_folder_action(AssetsRenameFolderPayload {
                folder_id: folder_id.clone(),
                name: "New Bin".to_owned(),
            }))
            .expect("rename folder");

        let folder = state
            .asset_library()
            .expect("library")
            .list_folders()
            .expect("folders")
            .into_iter()
            .find(|folder| folder.id == folder_id)
            .expect("folder");
        assert_eq!(folder.name, "New Bin");
        assert!(events.try_iter().any(|event| matches!(event, AppEvent::AssetLibraryReloaded)));

        remove_temp_path(&library_root);
    }

    #[test]
    fn dispatch_assets_set_proxy_mode_reports_missing_library() {
        let mut state = AppState::new();

        let err = state
            .dispatch_action(assets_set_proxy_mode_action(AssetsSetProxyModePayload {
                asset_id: AssetId::new(),
                enabled: true,
            }))
            .expect_err("missing library should fail");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(
            state.status_hint.as_ref().is_some_and(|(message, is_error)| {
                *is_error && message.contains("设置代理模式失败")
            })
        );
    }

    #[test]
    fn dispatch_assets_set_proxy_mode_rejects_non_video_assets_without_state_change() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-proxy-audio-library");
        let media_root = unique_temp_path("assets-proxy-audio-media");
        std::fs::create_dir_all(&media_root).expect("media root");
        let media_path = media_root.join("tone.wav");
        write_minimal_wav(&media_path);
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let asset_id = library.import_media_file(&media_path).expect("import audio");
        state.test_set_asset_library(Some(library));

        let err = state
            .dispatch_action(assets_set_proxy_mode_action(AssetsSetProxyModePayload {
                asset_id,
                enabled: true,
            }))
            .expect_err("audio assets should not support proxy mode");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(!state.is_asset_proxy_mode(asset_id));
        assert!(
            state.status_hint.as_ref().is_some_and(|(message, is_error)| {
                *is_error && message.contains("只有视频素材支持代理模式")
            }),
            "status hint: {:?}",
            state.status_hint
        );

        remove_temp_path(&library_root);
        remove_temp_path(&media_root);
    }

    #[test]
    fn dispatch_assets_delete_folder_unlinks_nested_assets_and_publishes_reload() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-delete-folder-action-library");
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let folder_id = library.create_folder("Rushes", None).expect("create folder");
        let child_id = library.create_folder("Selects", Some(&folder_id)).expect("create child");
        let asset_id =
            library.create_solid_color_asset(Some("Nested Plate")).expect("create asset");
        library.move_asset_to_folder(asset_id, Some(&child_id)).expect("move asset");
        state.test_set_asset_library(Some(library));
        let events = state.event_bus.subscribe();

        state
            .dispatch_action(assets_delete_folder_action(AssetsDeleteFolderPayload {
                folder_id: folder_id.clone(),
            }))
            .expect("delete folder");

        let library = state.asset_library().expect("library");
        assert!(!library.folder_exists(&folder_id).expect("parent gone"));
        assert!(!library.folder_exists(&child_id).expect("child gone"));
        let asset = library.get_asset(asset_id).expect("get asset").expect("asset kept");
        assert_eq!(asset.folder_id, None);
        assert!(state
            .status_hint
            .as_ref()
            .is_some_and(|(message, is_error)| !*is_error && message.contains("Rushes")));
        let mut saw_reload = false;
        while let Ok(event) = events.try_recv() {
            if matches!(event, AppEvent::AssetLibraryReloaded) {
                saw_reload = true;
            }
        }
        assert!(saw_reload);

        remove_temp_path(&library_root);
    }

    #[test]
    fn dispatch_assets_delete_folder_reports_missing_library() {
        let mut state = AppState::new();

        let err = state
            .dispatch_action(assets_delete_folder_action(AssetsDeleteFolderPayload {
                folder_id: "missing-folder".to_owned(),
            }))
            .expect_err("missing library should fail");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
    }

    #[test]
    fn dispatch_assets_delete_selection_removes_assets_folders_and_timeline_refs() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-delete-selection-action-library");
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let folder_id = library.create_folder("Rushes", None).expect("create folder");
        let asset_id = library.create_solid_color_asset(Some("Plate")).expect("create asset");
        let keep_asset_id =
            library.create_solid_color_asset(Some("Keep")).expect("create keep asset");
        state.test_set_asset_library(Some(library));
        let mut sequence = Sequence::new("edit");
        let time_base = sequence.time_base();
        sequence.video_tracks[0]
            .add_clip(Clip::new(asset_id, tt(0, time_base), tt(24, time_base)).expect("valid clip"))
            .expect("add deleted asset clip");
        sequence.video_tracks[0]
            .add_clip(
                Clip::new(keep_asset_id, tt(24, time_base), tt(24, time_base)).expect("valid clip"),
            )
            .expect("add keep clip");
        state.test_set_sequence(Some(sequence));
        let events = state.event_bus.subscribe();

        state
            .dispatch_action(assets_delete_selection_action(
                AssetsDeleteSelectionPayload {
                    asset_ids: vec![asset_id],
                    folder_ids: vec![folder_id.clone()],
                },
            ))
            .expect("delete selection");

        let library = state.asset_library().expect("library");
        assert!(library.get_asset(asset_id).expect("get deleted").is_none());
        assert!(library.get_asset(keep_asset_id).expect("get keep").is_some());
        assert!(!library.folder_exists(&folder_id).expect("folder removed"));
        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert!(
            !sequence.video_tracks[0]
                .clips
                .iter()
                .any(|clip| clip.library_asset_id() == Some(asset_id)),
            "clips referencing deleted assets should be removed"
        );
        assert!(sequence.video_tracks[0]
            .clips
            .iter()
            .any(|clip| clip.library_asset_id() == Some(keep_asset_id)));
        let events: Vec<AppEvent> = events.try_iter().collect();
        assert!(events.iter().any(
            |event| matches!(event, AppEvent::AssetDeleted { asset_id: event_id } if *event_id == asset_id)
        ));
        assert!(events.iter().any(|event| matches!(event, AppEvent::AssetLibraryReloaded)));
        assert!(
            state.status_hint.as_ref().is_some_and(|(message, is_error)| {
                !*is_error && message.contains("1 个素材") && message.contains("1 个文件夹")
            })
        );

        remove_temp_path(&library_root);
    }

    #[test]
    fn dispatch_assets_move_asset_updates_folder_and_publishes_reload() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-move-asset-action-library");
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let folder_id = library.create_folder("Rushes", None).expect("create folder");
        let asset_id = library.create_solid_color_asset(Some("Plate")).expect("create asset");
        state.test_set_asset_library(Some(library));
        let events = state.event_bus.subscribe();

        state
            .dispatch_action(assets_move_asset_action(AssetsMoveAssetPayload {
                asset_id,
                folder_id: Some(folder_id.clone()),
            }))
            .expect("move asset");

        let asset = state
            .asset_library()
            .expect("library")
            .get_asset(asset_id)
            .expect("get asset")
            .expect("asset");
        assert_eq!(asset.folder_id.as_deref(), Some(folder_id.as_str()));
        assert!(state
            .status_hint
            .as_ref()
            .is_some_and(|(message, is_error)| !*is_error && message.contains("Plate")));
        assert!(events.try_iter().any(|event| matches!(event, AppEvent::AssetLibraryReloaded)));

        remove_temp_path(&library_root);
    }

    #[test]
    fn dispatch_assets_move_folder_reparents_and_rejects_cycles() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-move-folder-action-library");
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let parent_id = library.create_folder("Parent", None).expect("create parent");
        let child_id = library.create_folder("Child", None).expect("create child");
        state.test_set_asset_library(Some(library));

        state
            .dispatch_action(assets_move_folder_action(AssetsMoveFolderPayload {
                folder_id: child_id.clone(),
                parent_folder_id: Some(parent_id.clone()),
            }))
            .expect("move folder");
        let folders = state.asset_library().expect("library").list_folders().expect("list");
        assert_eq!(
            folders
                .iter()
                .find(|folder| folder.id == child_id)
                .expect("child")
                .parent_id
                .as_deref(),
            Some(parent_id.as_str())
        );

        let err = state
            .dispatch_action(assets_move_folder_action(AssetsMoveFolderPayload {
                folder_id: parent_id.clone(),
                parent_folder_id: Some(child_id.clone()),
            }))
            .expect_err("moving parent into child should fail");
        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));

        remove_temp_path(&library_root);
    }

    #[test]
    fn dispatch_assets_move_selection_batches_assets_and_folders() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-move-selection-action-library");
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let target_id = library.create_folder("Target", None).expect("create target");
        let folder_id = library.create_folder("Bin", None).expect("create folder");
        let first_asset = library.create_solid_color_asset(Some("Plate A")).expect("asset a");
        let second_asset = library.create_solid_color_asset(Some("Plate B")).expect("asset b");
        state.test_set_asset_library(Some(library));
        let events = state.event_bus.subscribe();

        state
            .dispatch_action(assets_move_selection_action(AssetsMoveSelectionPayload {
                asset_ids: vec![first_asset, second_asset],
                folder_ids: vec![folder_id.clone()],
                target_folder_id: Some(target_id.clone()),
            }))
            .expect("move selection");

        let library = state.asset_library().expect("library");
        for asset_id in [first_asset, second_asset] {
            let asset = library.get_asset(asset_id).expect("get asset").expect("asset");
            assert_eq!(asset.folder_id.as_deref(), Some(target_id.as_str()));
        }
        let folders = library.list_folders().expect("list folders");
        assert_eq!(
            folders
                .iter()
                .find(|folder| folder.id == folder_id)
                .expect("moved folder")
                .parent_id
                .as_deref(),
            Some(target_id.as_str())
        );
        assert!(
            state.status_hint.as_ref().is_some_and(|(message, is_error)| {
                !*is_error && message.contains("2 个素材") && message.contains("1 个文件夹")
            })
        );
        assert_eq!(
            events
                .try_iter()
                .filter(|event| matches!(event, AppEvent::AssetLibraryReloaded))
                .count(),
            1
        );

        remove_temp_path(&library_root);
    }

    #[test]
    fn dispatch_assets_move_selection_rejects_without_partial_asset_moves() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-move-selection-rollback-library");
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let original_folder_id = library.create_folder("Original", None).expect("original folder");
        let parent_id = library.create_folder("Parent", None).expect("parent folder");
        let child_id = library.create_folder("Child", Some(&parent_id)).expect("child folder");
        let asset_id = library.create_solid_color_asset(Some("Plate")).expect("asset");
        library
            .move_asset_to_folder(asset_id, Some(&original_folder_id))
            .expect("place asset");
        state.test_set_asset_library(Some(library));

        let err = state
            .dispatch_action(assets_move_selection_action(AssetsMoveSelectionPayload {
                asset_ids: vec![asset_id],
                folder_ids: vec![parent_id],
                target_folder_id: Some(child_id),
            }))
            .expect_err("invalid folder move should fail before moving assets");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        let library = state.asset_library().expect("library");
        let asset = library.get_asset(asset_id).expect("get asset").expect("asset");
        assert_eq!(
            asset.folder_id.as_deref(),
            Some(original_folder_id.as_str())
        );

        remove_temp_path(&library_root);
    }

    #[test]
    fn dispatch_assets_create_actions_write_library_records() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-create-actions-library");
        state.test_set_asset_library(Some(
            AssetLibrary::open(library_root.clone()).expect("library"),
        ));

        state
            .dispatch_action(assets_create_adjustment_layer_action(
                AssetsCreateAssetPayload { folder_id: None },
            ))
            .expect("create adjustment layer");
        state
            .dispatch_action(assets_create_solid_color_action(AssetsCreateAssetPayload {
                folder_id: None,
            }))
            .expect("create solid color");
        state
            .dispatch_action(assets_create_folder_action(AssetsCreateFolderPayload {
                parent_folder_id: None,
            }))
            .expect("create first folder");
        let first_folder_id = state
            .asset_library()
            .expect("library")
            .list_folders()
            .expect("list folders")
            .into_iter()
            .find(|folder| folder.name == "文件夹 1")
            .expect("first folder")
            .id;
        state
            .dispatch_action(assets_create_folder_action(AssetsCreateFolderPayload {
                parent_folder_id: Some(first_folder_id.clone()),
            }))
            .expect("create second folder");

        let library = state.asset_library().expect("library");
        let assets = library.list_assets().expect("list assets");
        assert_eq!(assets.len(), 2);
        assert!(assets.iter().any(|asset| asset.kind == AssetKind::AdjustmentLayer));
        assert!(assets.iter().any(|asset| asset.kind == AssetKind::SolidColor));
        let folders = library.list_folders().expect("list folders");
        let folder_names = folders.iter().map(|folder| folder.name.clone()).collect::<Vec<_>>();
        assert_eq!(folder_names, vec!["文件夹 1", "文件夹 2"]);
        assert_eq!(folders[0].parent_id, None);
        assert_eq!(
            folders[1].parent_id.as_deref(),
            Some(first_folder_id.as_str())
        );
        assert!(state
            .status_hint
            .as_ref()
            .is_some_and(|(message, is_error)| { !*is_error && message.contains("文件夹 2") }));

        remove_temp_path(&library_root);
    }

    #[test]
    fn dispatch_assets_create_actions_place_assets_in_target_folder() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-create-in-folder-library");
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let folder_id = library.create_folder("Generated", None).expect("create folder");
        state.test_set_asset_library(Some(library));

        state
            .dispatch_action(assets_create_adjustment_layer_action(
                AssetsCreateAssetPayload { folder_id: Some(folder_id.clone()) },
            ))
            .expect("create adjustment in folder");
        state
            .dispatch_action(assets_create_solid_color_action(AssetsCreateAssetPayload {
                folder_id: Some(folder_id.clone()),
            }))
            .expect("create solid in folder");

        let assets = state.asset_library().expect("library").list_assets().expect("list assets");
        assert_eq!(assets.len(), 2);
        assert!(assets
            .iter()
            .all(|asset| asset.folder_id.as_deref() == Some(folder_id.as_str())));
        assert!(assets.iter().any(|asset| asset.kind == AssetKind::AdjustmentLayer));
        assert!(assets.iter().any(|asset| asset.kind == AssetKind::SolidColor));

        remove_temp_path(&library_root);
    }

    #[test]
    fn dispatch_assets_create_actions_reject_missing_target_folder_without_asset() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-create-missing-folder-library");
        state.test_set_asset_library(Some(
            AssetLibrary::open(library_root.clone()).expect("library"),
        ));

        let err = state
            .dispatch_action(assets_create_solid_color_action(AssetsCreateAssetPayload {
                folder_id: Some("deleted-folder".to_owned()),
            }))
            .expect_err("missing folder should fail");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state
            .asset_library()
            .expect("library")
            .list_assets()
            .expect("list assets")
            .is_empty());

        remove_temp_path(&library_root);
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
    fn dispatch_project_recovery_reports_missing_autosave() {
        let mut state = AppState::new();
        let root = unique_temp_path("recover-missing-autosave");
        let project_file = root.join("cut.mdp");
        let autosave_file = root.join("autosave").join("missing.mdp");

        let err = state
            .dispatch_action(project_recover_from_autosave_action(
                ProjectRecoverFromAutosavePayload { project_file, autosave_file },
            ))
            .expect_err("missing autosave should fail");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
        remove_temp_path(&root);
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

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while state.current_project_path() != Some(expected_target.as_path()) {
            state.poll_project_persistence();
            assert!(
                std::time::Instant::now() < deadline,
                "Save As completion timed out"
            );
            std::thread::yield_now();
        }
        assert!(expected_target.exists());
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| !*is_error));

        remove_temp_path(&root);
    }

    #[test]
    fn dispatch_project_create_with_settings_creates_project_and_library() {
        let mut state = AppState::new();
        let root = unique_temp_path("create-project-with-settings");
        let target_without_extension = root.join("full-create").join("cut");
        let expected_target = target_without_extension.with_extension("mdp");
        let sequence_settings = SequenceSettings {
            resolution: Resolution { width: 3840, height: 2160 },
            frame_rate: Rational::new(30000, 1001),
            preview: SequencePreviewSettings {
                format: PreviewRenderFormat::ProResProxy,
                ..SequencePreviewSettings::default()
            },
            ..SequenceSettings::default()
        };
        let project_settings =
            ProjectSettings { proxy_enabled: false, ..ProjectSettings::default() };

        state
            .dispatch_action(project_create_with_settings_action(
                ProjectCreateWithSettingsPayload {
                    project_file: target_without_extension,
                    name: "Full Create".into(),
                    sequence_settings: sequence_settings.clone(),
                    color_environment: mondrian_core::ProjectColorEnvironment::default(),
                    project_settings: project_settings.clone(),
                },
            ))
            .expect("create project");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while state.current_project_path() != Some(expected_target.as_path()) {
            state.poll_project_persistence();
            assert!(
                std::time::Instant::now() < deadline,
                "Save As completion timed out"
            );
            std::thread::yield_now();
        }
        assert!(expected_target.exists());
        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert_eq!(sequence.name, "Full Create");
        assert_eq!(sequence.settings.resolution, sequence_settings.resolution);
        assert_eq!(sequence.settings.frame_rate, sequence_settings.frame_rate);
        assert_eq!(
            sequence.settings.preview.format,
            PreviewRenderFormat::ProResProxy
        );
        assert_eq!(
            sequence.settings.color.program_output.workflow,
            mondrian_timeline::sequence::ColorWorkflow::SceneReferred
        );
        let context =
            sequence.settings.root_program_color_context(state.project_color_environment());
        assert_eq!(
            context.engine,
            mondrian_core::ColorEngine::mondrian_standard()
        );
        assert_eq!(
            context.output_transform,
            mondrian_core::OutputTransformIntent::mondrian_standard()
        );
        assert!(!state.project_settings().proxy_enabled);
        assert_eq!(state.new_sequence_defaults(), &sequence_settings);
        assert!(state.asset_library().is_some());
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| !*is_error));

        remove_temp_path(&root);
    }

    #[test]
    fn updating_new_sequence_defaults_does_not_change_existing_sequence() {
        let mut state = AppState::new();
        let before = Sequence::new("Program");
        let sequence_id = before.id;
        let settings = before.settings.clone();
        state.test_set_sequence(Some(before));
        state
            .update_sequence_identity_and_settings(sequence_id, "Edited Program", settings)
            .expect("rename Sequence");
        assert_eq!(
            state.authoring_history().expect("history").diagnostics().undo_entries,
            1
        );
        let before_revision = state.active_sequence().expect("sequence").revision;
        let before_context = state
            .active_sequence()
            .expect("sequence")
            .settings
            .root_program_color_context(state.project_color_environment());
        let mut defaults = state.new_sequence_defaults().clone();
        defaults.resolution = Resolution::UHD4K;

        state
            .dispatch_action(project_update_new_sequence_defaults_action(
                ProjectUpdateNewSequenceDefaultsPayload { settings: defaults.clone() },
            ))
            .expect("update new-Sequence defaults");

        assert_eq!(state.new_sequence_defaults(), &defaults);
        assert_eq!(
            state.active_sequence().expect("active sequence").revision,
            before_revision
        );
        assert_eq!(
            state
                .active_sequence()
                .expect("active sequence")
                .settings
                .root_program_color_context(state.project_color_environment()),
            before_context
        );
        assert_eq!(
            state.authoring_history().expect("history").diagnostics().undo_entries,
            2
        );
        assert_eq!(
            state.authoring_history().expect("history").diagnostics().redo_entries,
            0
        );
    }

    #[test]
    fn unavailable_custom_ocio_does_not_block_structurally_valid_author_edits() {
        let mut state = AppState::new();
        *state.test_project_color_environment_mut() = mondrian_core::ProjectColorEnvironment::new(
            pinned_test_custom_engine("Linear Rec.2020"),
        );
        let mut defaults = state.new_sequence_defaults().clone();
        defaults.resolution = Resolution::UHD4K;

        state
            .dispatch_action(project_update_new_sequence_defaults_action(
                ProjectUpdateNewSequenceDefaultsPayload { settings: defaults.clone() },
            ))
            .expect("author intent remains editable without preparing execution dependencies");

        assert_eq!(state.new_sequence_defaults(), &defaults);
    }

    #[test]
    fn updating_project_color_environment_changes_resolution_without_rewriting_sequence() {
        let mut state = AppState::new();
        let sequence = Sequence::new("Program");
        let sequence_id = sequence.id;
        let original_settings = sequence.settings.clone();
        let original_revision = sequence.revision;
        state.test_set_active_sequence(sequence_id);
        state.test_set_sequence(Some(sequence.clone()));
        state.test_add_sequence(sequence);
        let color_environment =
            mondrian_core::ProjectColorEnvironment::new(mondrian_core::ColorEngine::Aces {
                preset: mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
            });

        state
            .dispatch_action(project_update_color_environment_action(
                ProjectUpdateColorEnvironmentPayload {
                    color_environment: color_environment.clone(),
                },
            ))
            .expect("replace Project color environment");

        let active = state.active_sequence().expect("active sequence");
        assert_eq!(active.settings, original_settings);
        assert_eq!(active.revision, original_revision);
        assert_eq!(state.project_color_environment(), &color_environment);
        assert_eq!(
            &active
                .settings
                .root_program_color_context(state.project_color_environment())
                .engine,
            color_environment.engine()
        );
        assert_eq!(
            state.authoring_history().expect("history").diagnostics().undo_entries,
            1
        );
    }

    #[test]
    fn project_color_environment_update_rejects_any_invalid_sequence_atomically() {
        let mut state = AppState::new();
        let aces_environment =
            mondrian_core::ProjectColorEnvironment::new(mondrian_core::ColorEngine::Aces {
                preset: mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
            });
        *state.test_project_color_environment_mut() = aces_environment.clone();
        let mut incompatible = Sequence::new("ACEScg Program");
        incompatible.settings.color.working_color_space = WorkingColorSpace::AcesCg;
        state.test_set_active_sequence(incompatible.id);
        state.test_set_sequence(Some(incompatible.clone()));
        state.test_add_sequence(incompatible);

        let error = state
            .dispatch_action(project_update_color_environment_action(
                ProjectUpdateColorEnvironmentPayload {
                    color_environment: mondrian_core::ProjectColorEnvironment::default(),
                },
            ))
            .expect_err("one incompatible Sequence must reject the whole Project change");

        assert!(error.to_string().contains("Mondrian Standard"));
        assert_eq!(state.project_color_environment(), &aces_environment);
        assert_eq!(
            state
                .active_sequence()
                .expect("active sequence")
                .settings
                .color
                .working_color_space,
            WorkingColorSpace::AcesCg
        );
        assert_eq!(
            state.authoring_history().expect("history").diagnostics().undo_entries,
            0
        );
    }

    #[test]
    fn invalid_new_sequence_defaults_are_rejected_atomically() {
        let mut state = AppState::new();
        let sequence = Sequence::new("Program");
        state.test_set_active_sequence(sequence.id);
        state.test_set_sequence(Some(sequence.clone()));
        state.test_add_sequence(sequence);
        let previous = state.new_sequence_defaults().clone();
        let previous_revision = state.active_sequence().expect("sequence").revision;
        let mut invalid = previous.clone();
        invalid.color.working_color_space = WorkingColorSpace::AcesCg;

        let error = state
            .dispatch_action(project_update_new_sequence_defaults_action(
                ProjectUpdateNewSequenceDefaultsPayload { settings: invalid },
            ))
            .expect_err("settings incompatible with the Project engine must not replace defaults");

        let message = error.to_string();
        assert!(message.contains("Mondrian Standard"));
        assert!(message.contains("not 'ACEScg'"));
        assert_eq!(state.new_sequence_defaults(), &previous);
        assert_eq!(
            state.active_sequence().expect("sequence").revision,
            previous_revision
        );
    }

    #[test]
    fn new_sequence_uses_updated_defaults_without_rewriting_existing_sequence() {
        let mut state = AppState::new();
        let sequence = Sequence::new("Existing");
        let existing_settings = sequence.settings.clone();
        state.test_set_active_sequence(sequence.id);
        state.test_set_sequence(Some(sequence.clone()));
        state.test_add_sequence(sequence);
        *state.test_project_color_environment_mut() =
            mondrian_core::ProjectColorEnvironment::new(mondrian_core::ColorEngine::Aces {
                preset: mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
            });
        let mut defaults = state.new_sequence_defaults().clone();
        defaults.resolution = Resolution::UHD4K;
        defaults.color.working_color_space = WorkingColorSpace::AcesCg;
        state
            .dispatch_action(project_update_new_sequence_defaults_action(
                ProjectUpdateNewSequenceDefaultsPayload { settings: defaults.clone() },
            ))
            .expect("update defaults");
        assert_eq!(state.sequences()[0].settings, existing_settings);
        state.new_sequence("Created From Defaults");
        assert_eq!(
            state.active_sequence().expect("new active sequence").settings,
            defaults
        );
    }

    #[test]
    fn dispatch_select_clip_resolves_selection_from_clip_id() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        state.selection.selected_track_ids = vec![track_id];
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
        assert!(state.selection.selected_track_ids.is_empty());
        assert!(state.selection.selected_mask.is_none());
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_select_track_records_track_selection_and_clears_nested_selection() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];
        state.selection.selected_mask = Some((MaskId::new(), clip_id, track_id));
        let property = opacity_property_selection(&state, clip_id);
        let keyframe = opacity_keyframe_selection(
            &state,
            clip_id,
            tt(10, state.active_sequence().expect("sequence").time_base()),
        );
        state.animation_selection.active_property = Some(property);
        state.animation_selection.selected_keyframes.insert(keyframe);

        state
            .dispatch_action(mondrian_editor_state::Action::Select(
                mondrian_editor_state::action::SelectionTarget::Track(track_id),
            ))
            .expect("select track");

        assert_eq!(state.selection.selected_track_ids, vec![track_id]);
        assert!(state.selection.selected_clips.is_empty());
        assert!(state.selection.selected_mask.is_none());
        assert!(state.animation_selection.active_property.is_none());
        assert!(state.animation_selection.selected_keyframes.is_empty());
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_select_track_rejects_unknown_track_id() {
        let (mut state, _, _) = state_with_two_video_tracks();
        let missing_track_id = TrackId::new();

        let err = state
            .dispatch_action(mondrian_editor_state::Action::Select(
                mondrian_editor_state::action::SelectionTarget::Track(missing_track_id),
            ))
            .expect_err("missing track should be rejected");

        assert!(
            matches!(err, MondrianError::TrackNotFound { track_id } if track_id == missing_track_id.to_string())
        );
    }

    #[test]
    fn dispatch_select_all_tracks_selects_video_and_audio_tracks() {
        let (mut state, video_track_id, clip_id) = state_with_two_video_tracks();
        let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
        sequence.add_audio_track();
        let expected_track_ids = sequence
            .video_tracks
            .iter()
            .map(|track| track.id)
            .chain(sequence.audio_tracks.iter().map(|track| track.id))
            .collect::<Vec<_>>();
        state.selection.selected_clips = vec![SelectedClipRef {
            track_id: video_track_id,
            is_video_track: true,
            clip_id,
        }];
        state.selection.selected_mask = Some((MaskId::new(), clip_id, video_track_id));

        state
            .dispatch_action(mondrian_editor_state::Action::Select(
                mondrian_editor_state::action::SelectionTarget::AllTracks,
            ))
            .expect("select all tracks");

        assert_eq!(state.selection.selected_track_ids, expected_track_ids);
        assert!(state.selection.selected_clips.is_empty());
        assert!(state.selection.selected_mask.is_none());
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_select_all_selects_video_and_audio_clips() {
        let (mut state, video_track_id, video_clip_id) = state_with_two_video_tracks();
        let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
        let audio_track_id = sequence.add_audio_track();
        let tb = sequence.time_base();
        let audio_clip = Clip::new(AssetId::new(), tt(30, tb), tt(10, tb)).expect("valid clip");
        let audio_clip_id = audio_clip.id;
        sequence
            .audio_track_mut(audio_track_id)
            .expect("audio track")
            .add_clip(audio_clip)
            .expect("add audio");
        state.selection.selected_track_ids = vec![video_track_id, audio_track_id];
        state.selection.selected_mask = Some((MaskId::new(), video_clip_id, video_track_id));
        state.animation_selection.active_property =
            Some(opacity_property_selection(&state, video_clip_id));

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
        assert!(state.selection.selected_track_ids.is_empty());
        assert!(state.selection.selected_mask.is_none());
        assert!(state.animation_selection.active_property.is_none());
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_deselect_all_clears_app_selection_scopes() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        state.selection.selected_track_ids = vec![track_id];
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];
        state.selection.selected_mask = Some((MaskId::new(), clip_id, track_id));
        let property = opacity_property_selection(&state, clip_id);
        let keyframe = opacity_keyframe_selection(
            &state,
            clip_id,
            tt(10, state.active_sequence().expect("sequence").time_base()),
        );
        state.animation_selection.active_property = Some(property);
        state.animation_selection.selected_keyframes.insert(keyframe);

        state
            .dispatch_action(mondrian_editor_state::Action::DeselectAll)
            .expect("deselect all");

        assert!(state.selection.selected_track_ids.is_empty());
        assert!(state.selection.selected_clips.is_empty());
        assert!(state.selection.selected_mask.is_none());
        assert!(state.animation_selection.active_property.is_none());
        assert!(state.animation_selection.selected_keyframes.is_empty());
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_delete_selection_removes_selected_clip() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];
        state.selection.selected_mask = Some((MaskId::new(), clip_id, track_id));
        state.animation_selection.active_property =
            Some(opacity_property_selection(&state, clip_id));

        state
            .dispatch_action(mondrian_editor_state::Action::DeleteSelection)
            .expect("delete selection");

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert!(sequence.video_tracks[0].clips.is_empty());
        assert!(state.selection.selected_clips.is_empty());
        assert!(state.selection.selected_mask.is_none());
        assert!(state.animation_selection.active_property.is_none());
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_ripple_delete_selection_closes_gap_after_selected_clip() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        let tb = state.active_sequence().expect("sequence").time_base();
        let trailing = Clip::new(AssetId::new(), tt(40, tb), tt(12, tb)).expect("valid clip");
        let trailing_id = trailing.id;
        state
            .active_sequence_mut_uncommitted()
            .expect("sequence")
            .video_track_mut(track_id)
            .expect("track")
            .add_clip(trailing)
            .expect("add trailing clip");
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];

        state
            .dispatch_action(mondrian_editor_state::Action::RippleDeleteSelection)
            .expect("ripple delete selection");

        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        let track = sequence.video_tracks.iter().find(|track| track.id == track_id).expect("track");
        assert_eq!(track.clips.len(), 1);
        assert_eq!(track.clips[0].id, trailing_id);
        assert_eq!(track.clips[0].position, tt(20, tb));
        assert!(state.selection.selected_clips.is_empty());
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_mark_in_out_actions_update_active_sequence_bounds() {
        let (mut state, _, _) = state_with_two_video_tracks();
        state.seek(42);
        state
            .dispatch_action(mondrian_editor_state::Action::MarkInAtPlayhead)
            .expect("mark in");
        state.seek(16);
        state
            .dispatch_action(mondrian_editor_state::Action::MarkOutAtPlayhead)
            .expect("mark out");

        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        assert_eq!(sequence.in_point, Some(tt(42, tb)));
        assert_eq!(sequence.out_point, Some(tt(42, tb)));
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_timeline_ui_sets_explicit_in_out_points() {
        let (mut state, _, _) = state_with_two_video_tracks();
        state
            .dispatch_action(timeline_set_in_out_point_action(
                TimelineSetInOutPointPayload {
                    point: TimelineInOutPointPayloadKind::In,
                    frame: 32,
                },
            ))
            .expect("set in point");
        state
            .dispatch_action(timeline_set_in_out_point_action(
                TimelineSetInOutPointPayload {
                    point: TimelineInOutPointPayloadKind::Out,
                    frame: 16,
                },
            ))
            .expect("set out point");

        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        assert_eq!(sequence.in_point, Some(tt(32, tb)));
        assert_eq!(sequence.out_point, Some(tt(32, tb)));
    }

    #[test]
    fn dispatch_timeline_ui_clears_in_out_points() {
        let (mut state, _, _) = state_with_two_video_tracks();
        {
            let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
            let tb = sequence.time_base();
            sequence.mark_in(tt(12, tb));
            sequence.mark_out(tt(48, tb));
        }

        state
            .dispatch_action(timeline_clear_in_out_points_action())
            .expect("clear in/out");

        let sequence = state.active_sequence().expect("sequence");
        assert_eq!(sequence.in_point, None);
        assert_eq!(sequence.out_point, None);
    }

    #[test]
    fn dispatch_delete_selection_preserves_locked_track() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].is_locked = true;
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];

        let err = state
            .dispatch_action(mondrian_editor_state::Action::DeleteSelection)
            .expect_err("locked track should reject delete");

        assert!(matches!(err, MondrianError::TrackLocked { .. }));
        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
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

        let mut video_clip = Clip::new(AssetId::new(), tt(10, tb), tt(20, tb)).expect("valid clip");
        let mut audio_clip = Clip::new(AssetId::new(), tt(10, tb), tt(20, tb)).expect("valid clip");
        let video_clip_id = video_clip.id;
        let link_group = ClipLinkGroupId::new();
        video_clip.link_group = Some(link_group);
        audio_clip.link_group = Some(link_group);

        sequence.video_tracks[0].add_clip(video_clip).expect("add video");
        sequence
            .audio_track_mut(audio_track_id)
            .expect("audio track")
            .add_clip(audio_clip)
            .expect("add audio");
        state.test_set_sequence(Some(sequence));
        state.selection.selected_clips = vec![SelectedClipRef {
            track_id: video_track_id,
            is_video_track: true,
            clip_id: video_clip_id,
        }];

        state
            .dispatch_action(mondrian_editor_state::Action::DeleteSelection)
            .expect("delete linked selection");

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert!(sequence.video_tracks[0].clips.is_empty());
        assert!(sequence.audio_tracks[0].clips.is_empty());
        assert!(state.selection.selected_clips.is_empty());
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_delete_selection_removes_selected_track() {
        let (mut state, track_id, _clip_id) = state_with_two_video_tracks();
        state.selection.selected_track_ids = vec![track_id];

        state
            .dispatch_action(mondrian_editor_state::Action::DeleteSelection)
            .expect("delete selected track");

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert!(!sequence.video_tracks.iter().any(|track| track.id == track_id));
        assert!(state.selection.selected_track_ids.is_empty());
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_delete_selection_removes_multiple_tracks_with_one_undo_snapshot() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("multi track edit");
        sequence.add_video_track();
        sequence.add_audio_track();
        let video_count = sequence.video_tracks.len();
        let audio_count = sequence.audio_tracks.len();
        let video_track_id = sequence.video_tracks[1].id;
        let audio_track_id = sequence.audio_tracks[1].id;
        state.test_set_sequence(Some(sequence));
        state.selection.selected_track_ids = vec![video_track_id, audio_track_id];

        state
            .dispatch_action(mondrian_editor_state::Action::DeleteSelection)
            .expect("delete selected tracks");

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert_eq!(sequence.video_tracks.len(), video_count - 1);
        assert_eq!(sequence.audio_tracks.len(), audio_count - 1);
        assert!(!sequence.video_tracks.iter().any(|track| track.id == video_track_id));
        assert!(!sequence.audio_tracks.iter().any(|track| track.id == audio_track_id));
        assert!(state.selection.selected_track_ids.is_empty());

        assert!(state.undo_timeline().expect("undo track delete"));
        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert_eq!(sequence.video_tracks.len(), video_count);
        assert_eq!(sequence.audio_tracks.len(), audio_count);
        assert!(sequence.video_tracks.iter().any(|track| track.id == video_track_id));
        assert!(sequence.audio_tracks.iter().any(|track| track.id == audio_track_id));
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_delete_selection_rejects_removing_last_track_without_partial_mutation() {
        let mut state = AppState::new();
        let sequence = Sequence::new("single track edit");
        let track_ids = sequence.video_tracks.iter().map(|track| track.id).collect::<Vec<_>>();
        state.test_set_sequence(Some(sequence));
        state.selection.selected_track_ids = track_ids.clone();

        let err = state
            .dispatch_action(mondrian_editor_state::Action::DeleteSelection)
            .expect_err("deleting all video tracks should be rejected");

        assert!(matches!(
            err,
            MondrianError::WorkflowStepFailed { step_id, .. } if step_id == "remove_tracks"
        ));
        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert_eq!(sequence.video_tracks.len(), track_ids.len());
        assert_eq!(state.selection.selected_track_ids, track_ids);
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_delete_selection_rejects_stale_selected_track() {
        let (mut state, _, _) = state_with_two_video_tracks();
        let missing_track_id = TrackId::new();
        state.selection.selected_track_ids = vec![missing_track_id];

        let err = state
            .dispatch_action(mondrian_editor_state::Action::DeleteSelection)
            .expect_err("stale selected track should be rejected");

        assert!(
            matches!(err, MondrianError::TrackNotFound { track_id } if track_id == missing_track_id.to_string())
        );
        assert_eq!(state.selection.selected_track_ids, vec![missing_track_id]);
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_split_clip_at_playhead_splits_intersecting_clip() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        state.seek(20);

        state
            .dispatch_action(mondrian_editor_state::Action::SplitClipAtPlayhead)
            .expect("split at playhead");

        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        let clips = &sequence.video_tracks[0].clips;
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0].id, clip_id);
        assert_eq!(clips[0].position, tt(10, tb));
        assert_eq!(clips[0].duration, tt(10, tb));
        assert_eq!(clips[1].position, tt(20, tb));
        assert_eq!(clips[1].duration, tt(10, tb));
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_split_clip_at_playhead_ignores_clip_boundary() {
        let (mut state, _, _) = state_with_two_video_tracks();
        state.seek(10);

        state
            .dispatch_action(mondrian_editor_state::Action::SplitClipAtPlayhead)
            .expect("split at clip boundary");

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert_eq!(sequence.video_tracks[0].clips.len(), 1);
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_nudge_clip_moves_with_undo_snapshot() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();

        state
            .dispatch_action(mondrian_editor_state::Action::NudgeClip { clip_id, delta_frames: 5 })
            .expect("nudge clip");

        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        assert_eq!(sequence.video_tracks[0].clips[0].position, tt(15, tb));
        assert!(state.can_undo_action());

        assert!(state.undo_timeline().expect("undo nudge"));
        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        assert_eq!(sequence.video_tracks[0].clips[0].position, tt(10, tb));
    }

    #[test]
    fn dispatch_nudge_clip_zero_delta_does_not_enter_undo_history() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();

        state
            .dispatch_action(mondrian_editor_state::Action::NudgeClip { clip_id, delta_frames: 0 })
            .expect("nudge clip");

        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        assert_eq!(sequence.video_tracks[0].clips[0].position, tt(10, tb));
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_move_clip_to_track_updates_selection_location() {
        let (mut state, source_track_id, clip_id) = state_with_two_video_tracks();
        let target_track_id = state.active_sequence().expect("sequence").video_tracks[1].id;
        let tb = state.active_sequence().expect("sequence").time_base();
        state.selection.selected_clips = vec![SelectedClipRef {
            track_id: source_track_id,
            is_video_track: true,
            clip_id,
        }];

        state
            .dispatch_action(mondrian_editor_state::Action::MoveClipToTrack {
                clip_id,
                target_track: target_track_id,
                position: FramePosition::new(42, tb),
            })
            .expect("move clip to track");

        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        assert!(sequence.video_tracks[0].clips.is_empty());
        assert_eq!(sequence.video_tracks[1].clips[0].id, clip_id);
        assert_eq!(sequence.video_tracks[1].clips[0].position, tt(42, tb));
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
        let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
        let target_video_track_id = sequence.video_tracks[1].id;
        let source_audio_track_id = sequence.audio_tracks[0].id;
        sequence.add_audio_track();
        let tb = sequence.time_base();

        let audio_clip = Clip::new(AssetId::new(), tt(10, tb), tt(20, tb)).expect("valid clip");
        let audio_clip_id = audio_clip.id;
        sequence
            .add_media_audio_clip(
                source_audio_track_id,
                audio_clip,
                AudioSourceComponentId::primary(),
            )
            .expect("add audio");
        let link_group = ClipLinkGroupId::new();
        sequence.video_tracks[0].clips[0].link_group = Some(link_group);
        sequence.audio_tracks[0].clips[0].link_group = Some(link_group);
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
                position: FramePosition::new(24, tb),
            })
            .expect("move linked clip to track");

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
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

        let clip = &state.active_sequence().expect("sequence").video_tracks[0].clips[0];
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

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        let clip = &sequence.video_tracks[0].clips[0];
        assert!((clip.transform.evaluate_opacity(sequence.playhead) - 0.42).abs() < 1.0e-6);
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_inspector_audio_source_selection_is_validated_and_undoable() {
        let (root, mut state, track_id, clip_id, edit_id, alternate_component) =
            state_with_audio_asset();
        let clip = InspectorClipRefPayload { track_id, is_video_track: false, clip_id };

        state
            .dispatch_action(inspector_set_audio_component_source_action(
                InspectorSetAudioComponentSourcePayload {
                    clip,
                    edit_id,
                    source: InspectorAudioComponentSourcePayload::Media {
                        component_id: alternate_component,
                    },
                },
            ))
            .expect("select alternate Component");

        assert_eq!(
            state.active_sequence().expect("sequence").audio_tracks[0].clips[0].audio_components[0]
                .source,
            AudioComponentSource::Media { component_id: alternate_component }
        );
        assert!(state.can_undo_action());
        assert!(state.undo_timeline().expect("undo source selection"));
        assert_eq!(
            state.active_sequence().expect("sequence").audio_tracks[0].clips[0].audio_components[0]
                .source,
            AudioComponentSource::Media { component_id: AudioSourceComponentId::primary() }
        );
        drop(state);
        remove_temp_path(&root);
    }

    #[test]
    fn dispatch_inspector_audio_source_rejects_unknown_component_without_mutation() {
        let (root, mut state, track_id, clip_id, edit_id, _) = state_with_audio_asset();
        let before = serde_json::to_vec(state.active_sequence().expect("sequence"))
            .expect("serialize before Sequence");

        let error = state
            .dispatch_action(inspector_set_audio_component_source_action(
                InspectorSetAudioComponentSourcePayload {
                    clip: InspectorClipRefPayload { track_id, is_video_track: false, clip_id },
                    edit_id,
                    source: InspectorAudioComponentSourcePayload::Media {
                        component_id: AudioSourceComponentId::new(),
                    },
                },
            ))
            .expect_err("unknown Component must fail closed");

        assert!(matches!(
            error,
            MondrianError::WorkflowStepFailed { step_id, .. }
                if step_id == "inspector_set_audio_component_source"
        ));
        assert_eq!(
            serde_json::to_vec(state.active_sequence().expect("sequence"))
                .expect("serialize after Sequence"),
            before
        );
        assert!(!state.can_undo_action());
        drop(state);
        remove_temp_path(&root);
    }

    #[test]
    fn dispatch_inspector_audio_component_fields_are_typed_validated_and_undoable() {
        let (root, mut state, track_id, clip_id, edit_id, _) = state_with_audio_asset();
        let clip = InspectorClipRefPayload { track_id, is_video_track: false, clip_id };
        let fade_in = AudioFade {
            duration: TimelineTime::new(1, 4).expect("fade in duration"),
            curve: AudioFadeCurve::EqualPower,
        };
        let fade_out = AudioFade {
            duration: TimelineTime::new(1, 8).expect("fade out duration"),
            curve: AudioFadeCurve::ConstantGain,
        };
        for field in [
            InspectorAudioComponentEditField::Enabled(false),
            InspectorAudioComponentEditField::VolumeDb(-7.5),
            InspectorAudioComponentEditField::Pan(0.25),
            InspectorAudioComponentEditField::FadeIn(Some(fade_in)),
            InspectorAudioComponentEditField::FadeOut(Some(fade_out)),
        ] {
            state
                .dispatch_action(inspector_set_audio_component_edit_field_action(
                    InspectorSetAudioComponentEditFieldPayload { clip, edit_id, field },
                ))
                .expect("valid typed audio Component mutation");
        }

        let edit = &state.active_sequence().expect("sequence").audio_tracks[0].clips[0]
            .audio_components[0];
        assert!(!edit.enabled);
        assert_eq!(edit.volume_db, -7.5);
        assert_eq!(edit.pan, 0.25);
        assert_eq!(edit.fades.fade_in, Some(fade_in));
        assert_eq!(edit.fades.fade_out, Some(fade_out));
        assert!(state.can_undo_action());

        assert!(state.undo_timeline().expect("undo fade out"));
        let edit = &state.active_sequence().expect("sequence").audio_tracks[0].clips[0]
            .audio_components[0];
        assert_eq!(edit.fades.fade_in, Some(fade_in));
        assert_eq!(edit.fades.fade_out, None);
        drop(state);
        remove_temp_path(&root);
    }

    #[test]
    fn dispatch_inspector_audio_component_noop_does_not_create_history() {
        let (root, mut state, track_id, clip_id, edit_id, _) = state_with_audio_asset();
        state
            .dispatch_action(inspector_set_audio_component_edit_field_action(
                InspectorSetAudioComponentEditFieldPayload {
                    clip: InspectorClipRefPayload { track_id, is_video_track: false, clip_id },
                    edit_id,
                    field: InspectorAudioComponentEditField::VolumeDb(0.0),
                },
            ))
            .expect("no-op audio mutation");

        assert!(!state.can_undo_action());
        drop(state);
        remove_temp_path(&root);
    }

    #[test]
    fn dispatch_inspector_audio_component_rejects_invalid_fade_atomically() {
        let (root, mut state, track_id, clip_id, edit_id, _) = state_with_audio_asset();
        let before = serde_json::to_vec(state.active_sequence().expect("sequence"))
            .expect("serialize before Sequence");
        let before_revision = state.active_sequence().expect("sequence").revision;

        let error = state
            .dispatch_action(inspector_set_audio_component_edit_field_action(
                InspectorSetAudioComponentEditFieldPayload {
                    clip: InspectorClipRefPayload { track_id, is_video_track: false, clip_id },
                    edit_id,
                    field: InspectorAudioComponentEditField::FadeIn(Some(AudioFade {
                        duration: TimelineTime::new(100, 1).expect("oversized fade"),
                        curve: AudioFadeCurve::EqualPower,
                    })),
                },
            ))
            .expect_err("fade beyond Clip must fail closed");

        assert!(matches!(
            error,
            MondrianError::WorkflowStepFailed { step_id, .. }
                if step_id == "inspector_set_audio_component_edit_field"
        ));
        assert_eq!(
            serde_json::to_vec(state.active_sequence().expect("sequence"))
                .expect("serialize after Sequence"),
            before
        );
        assert_eq!(
            state.active_sequence().expect("sequence").revision,
            before_revision
        );
        assert!(!state.can_undo_action());
        drop(state);
        remove_temp_path(&root);
    }

    #[test]
    fn clip_audio_component_fields_survive_save_and_reopen() {
        let (root, mut state, track_id, clip_id, edit_id, _) = state_with_audio_asset();
        let project_file = state
            .authoring
            .as_ref()
            .expect("authoring Session")
            .project_file()
            .to_path_buf();
        let runtime_root = state
            .authoring
            .as_ref()
            .expect("authoring Session")
            .runtime_root()
            .to_path_buf();
        let fade = AudioFade {
            duration: TimelineTime::new(1, 4).expect("fade duration"),
            curve: AudioFadeCurve::EqualPower,
        };
        let clip = InspectorClipRefPayload { track_id, is_video_track: false, clip_id };
        for field in [
            InspectorAudioComponentEditField::VolumeDb(-3.0),
            InspectorAudioComponentEditField::Pan(-0.5),
            InspectorAudioComponentEditField::FadeIn(Some(fade)),
        ] {
            state
                .dispatch_action(inspector_set_audio_component_edit_field_action(
                    InspectorSetAudioComponentEditFieldPayload { clip, edit_id, field },
                ))
                .expect("author audio field");
        }
        state.save_project_file().expect("save project");
        drop(state);

        let mut reopened = AppState::new();
        reopened.open_project_file(project_file).expect("reopen project");
        let edit = reopened
            .active_sequence()
            .expect("reopened sequence")
            .audio_tracks
            .iter()
            .flat_map(|track| &track.clips)
            .find(|clip| clip.id == clip_id)
            .and_then(|clip| clip.audio_components.iter().find(|edit| edit.id == edit_id))
            .expect("reopened audio Component Edit");
        assert_eq!(edit.volume_db, -3.0);
        assert_eq!(edit.pan, -0.5);
        assert_eq!(edit.fades.fade_in, Some(fade));
        let reopened_runtime_root = reopened
            .authoring
            .as_ref()
            .expect("reopened authoring Session")
            .runtime_root()
            .to_path_buf();
        drop(reopened);
        remove_temp_path(&reopened_runtime_root);
        remove_temp_path(&runtime_root);
        remove_temp_path(&root);
    }

    #[test]
    fn dispatch_inspector_nested_audio_source_accepts_only_child_public_output() {
        let mut child = Sequence::new("child");
        let initial_output = child.audio_program.outputs[0].id;
        let alternate_output = mondrian_core::ProgramOutputId::new();
        child.audio_program.outputs.push(mondrian_timeline::audio::AudioProgramOutput {
            id: alternate_output,
            name: "Dialogue".to_owned(),
            main_source: mondrian_timeline::audio::ProgramOutputMainSource::RoutedInputs,
            strip: mondrian_timeline::audio::AudioChannelStrip::default(),
        });

        let mut parent = Sequence::new("parent");
        let track_id = parent.audio_tracks[0].id;
        let clip = Clip::new_nested_sequence(
            child.id,
            TimelineTime::ZERO,
            tt(25, parent.time_base()),
            None,
        )
        .expect("nested Clip");
        let clip_id = clip.id;
        parent
            .add_nested_audio_clip(track_id, clip, initial_output)
            .expect("nested audio Clip");
        let edit_id = parent.audio_tracks[0].clips[0].audio_components[0].id;
        let mut state = AppState::new();
        state.test_set_sequence(Some(parent));
        state.test_set_sequences(vec![child]);

        state
            .dispatch_action(inspector_set_audio_component_source_action(
                InspectorSetAudioComponentSourcePayload {
                    clip: InspectorClipRefPayload { track_id, is_video_track: false, clip_id },
                    edit_id,
                    source: InspectorAudioComponentSourcePayload::NestedOutput {
                        output_id: alternate_output,
                    },
                },
            ))
            .expect("select child public output");

        assert_eq!(
            state.active_sequence().expect("parent").audio_tracks[0].clips[0].audio_components[0]
                .source,
            AudioComponentSource::NestedOutput { output_id: alternate_output }
        );
        assert!(state.undo_timeline().expect("undo nested source selection"));
        assert_eq!(
            state.active_sequence().expect("parent").audio_tracks[0].clips[0].audio_components[0]
                .source,
            AudioComponentSource::NestedOutput { output_id: initial_output }
        );
    }

    #[test]
    fn dispatch_inspector_ui_sets_clip_tint_color() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("solid color inspector");
        let track_id = sequence.video_tracks[0].id;
        let tb = sequence.time_base();
        let clip = Clip::new_solid_color(AssetId::new(), Color::BLACK, tt(10, tb), tt(20, tb))
            .expect("solid color Clip");
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("add solid color Clip");
        state.test_set_sequence(Some(sequence));
        let color = Color::from_rgba8(8, 144, 220, 192);

        state
            .dispatch_action(inspector_set_clip_tint_action(
                InspectorSetClipTintPayload {
                    clip: inspector_clip_payload(track_id, clip_id),
                    color,
                },
            ))
            .expect("dispatch tint");

        let clip = &state.active_sequence().expect("sequence").video_tracks[0].clips[0];
        assert_eq!(clip.content.solid_color(), Some(color));
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

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
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
            .evaluate(Transform2D::ROTATION_PATH, sequence.playhead)
            .and_then(|value| value.as_f32())
            .expect("rotation value");
        assert!((rotation + 12.5).abs() < f32::EPSILON);
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_viewer_ui_sets_clip_transform_atomically() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        let clip_ref = inspector_clip_payload(track_id, clip_id);

        state
            .dispatch_action(viewer_set_clip_transform_action(
                ViewerSetClipTransformPayload {
                    clip: clip_ref,
                    position: Some(ViewerTransformPositionPayload { x: 320.0, y: 180.0 }),
                    scale_percent: Some(125.0),
                    rotation_degrees: Some(8.5),
                },
            ))
            .expect("dispatch viewer transform");

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        let clip = &sequence.video_tracks[0].clips[0];
        assert_eq!(
            clip.transform.get_position(sequence.playhead),
            glam::Vec2::new(320.0, 180.0)
        );
        assert_eq!(
            clip.transform.get_scale(sequence.playhead),
            glam::Vec2::splat(1.25)
        );
        let rotation = clip
            .transform
            .to_property_bag()
            .evaluate(Transform2D::ROTATION_PATH, sequence.playhead)
            .and_then(|value| value.as_f32())
            .expect("rotation value");
        assert!((rotation - 8.5).abs() < f32::EPSILON);
        assert!(state.can_undo_action());

        assert!(state.undo_timeline().expect("undo viewer transform"));
        let sequence = state.active_sequence().expect("sequence after undo");
        let clip = &sequence.video_tracks[0].clips[0];
        assert_eq!(
            clip.transform.get_position(sequence.playhead),
            glam::Vec2::ZERO
        );
        assert_eq!(clip.transform.get_scale(sequence.playhead), glam::Vec2::ONE);
    }

    #[test]
    fn dispatch_inspector_ui_maps_incremental_curve_edits_to_opacity_keyframes() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        let property = opacity_parameter_address(&state, clip_id);

        for point in [
            InspectorCurvePointPayload { x: 0.0, y: 0.0 },
            InspectorCurvePointPayload { x: 0.5, y: 0.72 },
            InspectorCurvePointPayload { x: 1.0, y: 1.0 },
        ] {
            state
                .dispatch_action(inspector_edit_clip_curve_action(
                    InspectorEditClipCurvePayload {
                        clip: inspector_clip_payload(track_id, clip_id),
                        property: property.clone(),
                        edit: InspectorCurveEditPayload::Upsert { keyframe_id: None, point },
                    },
                ))
                .expect("dispatch curve point");
        }

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        let clip = &sequence.video_tracks[0].clips[0];
        let tb = sequence.time_base();
        assert!((clip.transform.evaluate_opacity(tt(0, tb)) - 0.0).abs() < 1.0e-6);
        assert!((clip.transform.evaluate_opacity(tt(10, tb)) - 0.72).abs() < 1.0e-6);
        assert!((clip.transform.evaluate_opacity(tt(20, tb)) - 1.0).abs() < 1.0e-6);
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_inspector_clip_mutations_preserve_locked_track() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        let opacity_property = opacity_parameter_address(&state, clip_id);
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].is_locked = true;
        let clip_ref = inspector_clip_payload(track_id, clip_id);

        for action in [
            inspector_set_clip_enabled_action(InspectorSetClipEnabledPayload {
                clip: clip_ref,
                enabled: false,
            }),
            inspector_set_clip_opacity_action(InspectorSetClipOpacityPayload {
                clip: clip_ref,
                opacity_percent: 42.0,
            }),
            inspector_set_clip_tint_action(InspectorSetClipTintPayload {
                clip: clip_ref,
                color: Color::from_hex(0x2255AA),
            }),
            inspector_set_clip_transform_field_action(InspectorSetClipTransformFieldPayload {
                clip: clip_ref,
                field: InspectorClipTransformField::PositionX,
                value: 128.0,
            }),
            viewer_set_clip_transform_action(ViewerSetClipTransformPayload {
                clip: clip_ref,
                position: Some(ViewerTransformPositionPayload { x: 320.0, y: 180.0 }),
                scale_percent: Some(125.0),
                rotation_degrees: Some(8.5),
            }),
            inspector_edit_clip_curve_action(InspectorEditClipCurvePayload {
                clip: clip_ref,
                property: opacity_property,
                edit: InspectorCurveEditPayload::Upsert {
                    keyframe_id: None,
                    point: InspectorCurvePointPayload { x: 0.0, y: 0.5 },
                },
            }),
        ] {
            let err = state
                .dispatch_action(action)
                .expect_err("locked track should reject inspector clip mutation");
            assert!(matches!(err, MondrianError::TrackLocked { .. }));
        }

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        let clip = &sequence.video_tracks[0].clips[0];
        assert!(!clip.is_disabled);
        assert_eq!(clip.content.solid_color(), None);
        assert_eq!(
            clip.transform.get_position(sequence.playhead),
            glam::Vec2::ZERO
        );
        assert!((clip.transform.evaluate_opacity(sequence.playhead) - 1.0).abs() < 1.0e-6);
        assert!(!state.can_undo_action());
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

        let clip = &state.active_sequence().expect("sequence").video_tracks[0].clips[0];
        assert_eq!(clip.effects.len(), 1);
        assert_eq!(clip.effects[0].effect_type, EffectType::GaussianBlur);
        let selected = state.primary_selected_effect().expect("new effect should be selected");
        assert_eq!(selected.clip.clip_id, clip_id);
        assert_eq!(selected.effect_id, clip.effects[0].id);
        assert!(state.can_undo_action());
        assert_eq!(
            state.authoring_history().and_then(|history| history.undo_description()),
            Some("添加高斯模糊")
        );

        assert!(state.undo_timeline().expect("undo add effect"));
        let clip = &state.active_sequence().expect("sequence").video_tracks[0].clips[0];
        assert!(clip.effects.is_empty());
        assert!(!state.can_undo_action());
        assert!(state.primary_selected_effect().is_none());
    }

    #[test]
    fn dispatch_inspector_ui_selects_effect_without_undo_history() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        let effect: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::GaussianBlur);
        let effect_id = effect.id;
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].clips[0]
            .add_effect_node(effect);

        state
            .dispatch_action(inspector_select_effect_action(
                InspectorSelectEffectPayload {
                    clip: inspector_clip_payload(track_id, clip_id),
                    effect_id,
                },
            ))
            .expect("dispatch select effect");

        let selected = state.primary_selected_effect().expect("selected effect");
        assert_eq!(selected.clip.clip_id, clip_id);
        assert_eq!(selected.effect_id, effect_id);
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_inspector_ui_select_effect_rejects_missing_effect_without_undo() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();

        let err = state
            .dispatch_action(inspector_select_effect_action(
                InspectorSelectEffectPayload {
                    clip: inspector_clip_payload(track_id, clip_id),
                    effect_id: EffectId::new(),
                },
            ))
            .expect_err("missing effect should reject selection");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state.primary_selected_effect().is_none());
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_inspector_ui_sets_effect_enabled_state() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        let effect: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::GaussianBlur);
        let effect_id = effect.id;
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].clips[0]
            .add_effect_node(effect);

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
            &state.active_sequence().expect("sequence").video_tracks[0].clips[0].effects[0];
        assert_eq!(effect.id, effect_id);
        assert!(!effect.is_enabled);
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_inspector_ui_effect_enabled_noop_does_not_enter_undo_history() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        let effect: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::GaussianBlur);
        let effect_id = effect.id;
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].clips[0]
            .add_effect_node(effect);

        state
            .dispatch_action(inspector_set_effect_enabled_action(
                InspectorSetEffectEnabledPayload {
                    clip: inspector_clip_payload(track_id, clip_id),
                    effect_id,
                    enabled: true,
                },
            ))
            .expect("dispatch no-op effect enabled");

        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_inspector_ui_effect_enabled_rejects_missing_effect_without_undo() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();

        let err = state
            .dispatch_action(inspector_set_effect_enabled_action(
                InspectorSetEffectEnabledPayload {
                    clip: inspector_clip_payload(track_id, clip_id),
                    effect_id: EffectId::new(),
                    enabled: false,
                },
            ))
            .expect_err("missing effect should reject mutation");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_inspector_ui_sets_effect_property_with_undo_snapshot() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        let (effect_id, path, initial_value) =
            add_default_effect_with_first_property(&mut state, EffectType::GaussianBlur);
        let next_value = different_property_value(&initial_value);

        state
            .dispatch_action(inspector_set_effect_property_action(
                InspectorSetEffectPropertyPayload {
                    clip: inspector_clip_payload(track_id, clip_id),
                    effect_id,
                    path: path.clone(),
                    value: next_value.clone(),
                },
            ))
            .expect("dispatch effect property");

        let property = state.active_sequence().expect("sequence").video_tracks[0].clips[0]
            .effects
            .iter()
            .find(|effect| effect.id == effect_id)
            .and_then(|effect| effect.properties.property(&path))
            .expect("updated property");
        assert_eq!(property.static_value(), &next_value);
        assert!(state.can_undo_action());

        assert!(state.undo_timeline().expect("undo effect property"));
        let property = state.active_sequence().expect("sequence").video_tracks[0].clips[0]
            .effects
            .iter()
            .find(|effect| effect.id == effect_id)
            .and_then(|effect| effect.properties.property(&path))
            .expect("restored property");
        assert_eq!(property.static_value(), &initial_value);
    }

    #[test]
    fn dispatch_basic_title_creation_and_inspector_edit_form_one_author_path() {
        let mut state = AppState::new();
        state.test_set_sequence(Some(Sequence::new("Basic Title")));

        state
            .dispatch_action(timeline_create_basic_title_action())
            .expect("dispatch Basic Title creation");

        let selection = state.primary_selected_clip().expect("selected Basic Title");
        let initial_text = state
            .active_sequence()
            .and_then(|sequence| find_clip(sequence, selection.clip_id))
            .and_then(|clip| clip.content.basic_title())
            .expect("Basic Title author state")
            .evaluate(TimelineTime::ZERO)
            .expect("evaluate initial title")
            .text;
        assert_eq!(initial_text, "标题");

        state
            .dispatch_action(inspector_set_clip_property_action(
                InspectorSetClipPropertyPayload {
                    clip: inspector_clip_payload(selection.track_id, selection.clip_id),
                    path: mondrian_core::BasicTitle::TEXT_PATH.to_owned(),
                    value: PropertyValue::Text("Mondrian".to_owned()),
                },
            ))
            .expect("dispatch Basic Title property edit");

        let edited_text = state
            .active_sequence()
            .and_then(|sequence| find_clip(sequence, selection.clip_id))
            .and_then(|clip| clip.content.basic_title())
            .expect("edited Basic Title")
            .evaluate(TimelineTime::ZERO)
            .expect("evaluate edited title")
            .text;
        assert_eq!(edited_text, "Mondrian");

        assert!(state.undo_timeline().expect("undo property edit"));
        let restored_text = state
            .active_sequence()
            .and_then(|sequence| find_clip(sequence, selection.clip_id))
            .and_then(|clip| clip.content.basic_title())
            .expect("restored Basic Title")
            .evaluate(TimelineTime::ZERO)
            .expect("evaluate restored title")
            .text;
        assert_eq!(restored_text, "标题");

        assert!(state.undo_timeline().expect("undo title creation"));
        assert!(state
            .active_sequence()
            .expect("sequence")
            .video_tracks
            .iter()
            .flat_map(|track| &track.clips)
            .all(|clip| clip.id != selection.clip_id));
    }

    #[test]
    fn dispatch_inspector_ui_effect_property_noop_does_not_enter_undo_history() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        let (effect_id, path, initial_value) =
            add_default_effect_with_first_property(&mut state, EffectType::GaussianBlur);

        state
            .dispatch_action(inspector_set_effect_property_action(
                InspectorSetEffectPropertyPayload {
                    clip: inspector_clip_payload(track_id, clip_id),
                    effect_id,
                    path,
                    value: initial_value,
                },
            ))
            .expect("dispatch no-op effect property");

        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_inspector_ui_effect_property_rejects_missing_path_without_undo() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        let (effect_id, _, initial_value) =
            add_default_effect_with_first_property(&mut state, EffectType::GaussianBlur);

        let err = state
            .dispatch_action(inspector_set_effect_property_action(
                InspectorSetEffectPropertyPayload {
                    clip: inspector_clip_payload(track_id, clip_id),
                    effect_id,
                    path: format!("effect.{effect_id}.missing"),
                    value: initial_value,
                },
            ))
            .expect_err("missing effect property should reject mutation");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(!state.can_undo_action());
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
        let clip = &mut state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0]
            .clips[0];
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

        let effects = &state.active_sequence().expect("sequence").video_tracks[0].clips[0].effects;
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
        let clip = &mut state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0]
            .clips[0];
        clip.add_effect_node(remove_effect);
        clip.add_effect_node(keep_effect);

        state
            .dispatch_action(mondrian_editor_state::Action::RemoveEffect {
                clip_id,
                effect_id: remove_id,
            })
            .expect("dispatch remove effect action");

        let effects = &state.active_sequence().expect("sequence").video_tracks[0].clips[0].effects;
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
        let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
        sequence.video_tracks[0].clips[0].add_effect_node(effect);
        sequence.video_tracks[0].is_locked = true;

        let err = state
            .dispatch_action(mondrian_editor_state::Action::RemoveEffect { clip_id, effect_id })
            .expect_err("locked track should reject effect removal");

        assert!(matches!(err, MondrianError::TrackLocked { .. }));
        let effects = &state.active_sequence().expect("sequence").video_tracks[0].clips[0].effects;
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
        let clip = &mut state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0]
            .clips[0];
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

        let effects = &state.active_sequence().expect("sequence").video_tracks[0].clips[0].effects;
        assert_eq!(
            effects.iter().map(|effect| effect.id).collect::<Vec<_>>(),
            vec![second_id, third_id, first_id]
        );
        assert!(state.can_undo_action());

        assert!(state.undo_timeline().expect("undo reorder effects"));
        let effects = &state.active_sequence().expect("sequence").video_tracks[0].clips[0].effects;
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
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].clips[0]
            .add_effect_node(effect);

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
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].clips[0]
            .add_effect_node(effect);

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
        let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
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
        let effects = &state.active_sequence().expect("sequence").video_tracks[0].clips[0].effects;
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
        let tb = state.active_sequence().expect("sequence").time_base();
        let source_time = tt(4, tb);
        let destination_time = tt(8, tb);
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
        let keyframe_selection = opacity_keyframe_selection(&state, clip_id, source_time);
        state.set_animation_keyframe_selection(vec![keyframe_selection]);
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
        state.selection.selected_mask = Some((MaskId::new(), clip_id, track_id));
        state.animation_selection.active_property =
            Some(opacity_property_selection(&state, clip_id));
        state.seek(50);

        state.dispatch_action(mondrian_editor_state::Action::Copy).expect("copy clip");
        state.dispatch_action(mondrian_editor_state::Action::Paste).expect("paste clip");

        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        let clips = &sequence.video_tracks[0].clips;
        assert_eq!(clips.len(), 2);
        assert!(clips.iter().any(|clip| clip.id == clip_id && clip.position == tt(10, tb)));
        let pasted = clips.iter().find(|clip| clip.id != clip_id).expect("pasted clip");
        assert_eq!(pasted.position, tt(50, tb));
        assert_eq!(pasted.duration, tt(20, tb));
        assert_eq!(state.active_clipboard_kind, Some(AppClipboardKind::Clips));
        assert_eq!(
            state.selection.selected_clips,
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id: pasted.id }]
        );
        assert!(state.selection.selected_mask.is_none());
        assert!(state.animation_selection.active_property.is_none());
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_cut_action_copies_and_removes_selected_clips() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];
        state.selection.selected_mask = Some((MaskId::new(), clip_id, track_id));
        state.animation_selection.active_property =
            Some(opacity_property_selection(&state, clip_id));

        state.dispatch_action(mondrian_editor_state::Action::Cut).expect("cut clip");

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert!(sequence.video_tracks[0].clips.is_empty());
        assert!(state.selection.selected_clips.is_empty());
        assert!(state.selection.selected_mask.is_none());
        assert!(state.animation_selection.active_property.is_none());
        assert!(state.has_clip_clipboard());
        assert_eq!(state.active_clipboard_kind, Some(AppClipboardKind::Clips));
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_cut_action_preserves_locked_track_and_clipboard() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].is_locked = true;
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];

        let err = state
            .dispatch_action(mondrian_editor_state::Action::Cut)
            .expect_err("locked track should reject cut");

        assert!(matches!(err, MondrianError::TrackLocked { .. }));
        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
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
        state.selection.selected_mask = Some((MaskId::new(), clip_id, track_id));
        state.animation_selection.active_property =
            Some(opacity_property_selection(&state, clip_id));
        state.seek(0);

        state
            .dispatch_action(mondrian_editor_state::Action::Duplicate)
            .expect("duplicate clip");

        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
        let clips = &sequence.video_tracks[0].clips;
        assert_eq!(clips.len(), 2);
        let duplicated = clips.iter().find(|clip| clip.id != clip_id).expect("duplicate");
        assert_eq!(duplicated.position, tt(30, tb));
        assert_eq!(duplicated.duration, tt(20, tb));
        assert_eq!(state.current_frame(), 30);
        assert_eq!(
            state.selection.selected_clips,
            vec![SelectedClipRef {
                track_id,
                is_video_track: true,
                clip_id: duplicated.id,
            }]
        );
        assert!(state.selection.selected_mask.is_none());
        assert!(state.animation_selection.active_property.is_none());
        assert!(!state.has_clip_clipboard());
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_duplicate_action_noops_without_selection() {
        let (mut state, _, _) = state_with_two_video_tracks();

        state
            .dispatch_action(mondrian_editor_state::Action::Duplicate)
            .expect("duplicate without selection");

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
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

        let mut video_clip = Clip::new(AssetId::new(), tt(10, tb), tt(20, tb)).expect("valid clip");
        let mut audio_clip = Clip::new(
            video_clip.media_asset_id().expect("video asset"),
            tt(10, tb),
            tt(20, tb),
        )
        .expect("valid clip");
        let video_clip_id = video_clip.id;
        let audio_clip_id = audio_clip.id;
        let link_group = ClipLinkGroupId::new();
        video_clip.link_group = Some(link_group);
        audio_clip.link_group = Some(link_group);
        sequence.video_tracks[0].add_clip(video_clip).expect("add video");
        sequence.audio_tracks[0].add_clip(audio_clip).expect("add audio");
        state.test_set_sequence(Some(sequence));
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

        let sequence = state.active_sequence().expect("sequence");
        let tb = sequence.time_base();
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
        assert_eq!(pasted_video.position, tt(40, tb));
        assert_eq!(pasted_audio.position, tt(40, tb));
        assert_eq!(pasted_video.link_group, pasted_audio.link_group);
        assert!(pasted_video.link_group.is_some());
        assert_ne!(pasted_video.link_group, Some(link_group));
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

    #[test]
    fn dispatch_rejects_action_without_an_app_state_product_interface() {
        let mut state = AppState::new();

        let error = state
            .dispatch_action(mondrian_editor_state::Action::NewProject)
            .expect_err("shell-owned action must not report product success");

        assert!(matches!(
            error,
            MondrianError::WorkflowStepFailed { ref step_id, .. }
                if step_id == "dispatch_action"
        ));
        assert!(!state.has_open_project());
    }

    #[test]
    fn dispatch_rejects_unknown_custom_namespace() {
        let mut state = AppState::new();

        let error = state
            .dispatch_action(mondrian_editor_state::Action::Custom {
                namespace: "third-party.unbound".to_owned(),
                name: "pretend-success".to_owned(),
                payload: serde_json::Value::Null,
            })
            .expect_err("unbound namespace must not report product success");

        assert!(matches!(
            error,
            MondrianError::WorkflowStepFailed { ref step_id, .. }
                if step_id == "dispatch_action"
        ));
    }
}
