//! Semantic Action adapter for the application composition root.
//!
//! UI intent is routed to the owning domain Interface. Project mutations go
//! through `AuthoringSession`; transport intent goes through `PlaybackEngine`.

use crate::app::media_asset_mutation::MediaAssetMutationKind;
use crate::app::preview_quality::normalize_preview_resolution_scale;
use crate::app::product_action::{
    AssetAudioComponentRebindPayload, AssetLibraryMovePayload, AssetLibrarySelectionPayload,
    AssetProductAction, AssetRelinkPayload, AssetRenameFolderPayload, AssetRenamePayload,
    AssetSetInterpretationPayload, AssetSetProxyModePayload, AssetTargetPayload,
    ClipCurveEditPayload, ClipProductAction, ExportDraftEdit, ExportProductAction, ProductAction,
    ProjectCreateWithSettingsPayload, ProjectProductAction, ProjectRecoverFromAutosavePayload,
    SequenceProductAction, SequenceUpdateSettingsPayload, TimelineClipSelectionModePayload,
    TimelineProductAction, TrackAddKind, TrackAuthorControl, TrackEditPolicyControl,
    TrackProductAction, VideoTransitionProductAction, VideoTransitionTargetPayload,
    ViewerProductAction, VisualEffectProductAction, VisualEffectSetParameterValuePayload,
};
#[cfg(test)]
use crate::app::product_action::{
    TimelineMoveClipPayload, TimelineSelectClipPayload, TIMELINE_NAMESPACE,
};
use crate::app::proxy_generation::{
    resolve_app_state_proxy_color_contract, ProxyGenerationOrigin, ProxyGenerationRequestOutcome,
};
use crate::app::selection::resolve_track_selection;
use crate::app::timeline_editing::{find_clip, find_clip_mut, find_clip_track_lock};
use crate::app::timeline_position::lower_nearest_sequence_frame;
#[cfg(test)]
use crate::app::SelectedClipRef;
use crate::app::{AppClipboardKind, AppState, ClipOverlapMode, ClipSelectionMode};
use mondrian_assets::AssetKind;
use mondrian_core::automation::PropertyHost;
#[cfg(test)]
use mondrian_core::automation::{PropertyMutation, PropertyValue};
use mondrian_core::events::AppEvent;
use mondrian_core::types::{ClipId, EffectId, FramePosition};
use mondrian_core::{FrameRounding, MondrianError, Result, TimelineTime};
use mondrian_export::queue::ExportCancelOutcome;
#[cfg(test)]
use mondrian_timeline::audio::AudioComponentSource;
#[cfg(test)]
use mondrian_timeline::clip::Transform2D;
use mondrian_timeline::clip::{Clip, TrimEdge};
use std::path::PathBuf;
use std::time::Duration;

enum AssetLibrarySubject {
    Asset(String),
    Folder(String),
}

impl AppState {
    /// Dispatch one semantic Action into its owning product Interface.
    ///
    /// An Action handled by a shell-only Interface, an unknown namespace, or
    /// an unimplemented product path is rejected. A caller must never infer
    /// successful execution from a silent no-op.
    pub fn dispatch_action(&mut self, action: mondrian_editor_state::Action) -> Result<()> {
        use mondrian_editor_state::Action;

        if self.project_close_blocks_actions() {
            return Err(MondrianError::ActionNotExecuted {
                action: "project_lifecycle_handoff".to_owned(),
                reason: "项目正在安全关闭，或持久化所有权未能证明；作者操作保持冻结".to_owned(),
            });
        }

        if let Some(product_action) = ProductAction::decode_external(&action).map_err(|error| {
            let step_id = error.dispatch_step_id();
            MondrianError::WorkflowStepFailed { step_id, reason: error.to_string() }
        })? {
            return self.dispatch_product_action(product_action);
        }

        match action {
            // ── 播放控制（已有方法）───────────────────────────────────────
            Action::Play => self.play(),
            Action::Pause => self.pause(),
            Action::TogglePlay => {
                if self.is_playing() {
                    self.pause()
                } else {
                    self.play()
                }
            }
            Action::Seek(position) => {
                let frame = self.sequence_frame_from_action_position("seek", position)?;
                self.seek(frame)
            }
            Action::StepForward => {
                self.seek(self.current_frame().checked_add(1).ok_or_else(|| {
                    MondrianError::ActionNotExecuted {
                        action: "step_forward".to_owned(),
                        reason: "timeline frame arithmetic overflow".to_owned(),
                    }
                })?)
            }
            Action::StepBack => {
                let previous = self.current_frame().checked_sub(1).ok_or_else(|| {
                    MondrianError::ActionNotExecuted {
                        action: "step_back".to_owned(),
                        reason: "timeline frame arithmetic overflow".to_owned(),
                    }
                })?;
                self.seek(previous.max(0))
            }
            Action::GoToStart => self.seek(0),
            Action::GoToEnd => {
                let end = self.last_content_frame()?;
                if end >= 0 {
                    self.seek(end)
                } else {
                    Err(MondrianError::ActionNotExecuted {
                        action: "go_to_end".to_owned(),
                        reason: "active Sequence has no valid terminal frame".to_owned(),
                    })
                }
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
                if self.undo_timeline()? {
                    Ok(())
                } else {
                    Err(MondrianError::ActionNotExecuted {
                        action: "undo".to_owned(),
                        reason: "撤销历史为空".to_owned(),
                    })
                }
            }
            Action::Redo => {
                if self.redo_timeline()? {
                    Ok(())
                } else {
                    Err(MondrianError::ActionNotExecuted {
                        action: "redo".to_owned(),
                        reason: "重做历史为空".to_owned(),
                    })
                }
            }

            // ── 剪贴板（动画关键帧优先，否则使用 timeline clip clipboard）──
            Action::Copy => self.copy_from_action(),
            Action::Cut => self.cut_from_action(),
            Action::Paste => self.paste_from_action(),
            Action::Duplicate => self.duplicate_from_action(),

            // ── 时间线编辑（复用已有 undoable 命令层）────────────────────
            Action::DeleteSelection => self.delete_selection_from_ui(false),
            Action::RippleDeleteSelection => self.delete_selection_from_ui(true),
            Action::SplitClipAtPlayhead => {
                let split_count = self.split_at_playhead()?;
                require_action_executed(
                    split_count > 0,
                    "split_clip_at_playhead",
                    "播放头未命中可拆分的未锁定片段",
                )
            }
            Action::MarkInAtPlayhead => self.mark_in_at_current_frame(),
            Action::MarkOutAtPlayhead => self.mark_out_at_current_frame(),
            Action::NudgeClip { clip_id, delta_frames } => {
                self.nudge_clip_from_action(clip_id, delta_frames)
            }
            Action::MoveClipToTrack { clip_id, target_track, position } => {
                let frame =
                    self.sequence_frame_from_action_position("move_clip_to_track", position)?;
                self.move_clip_to_track_from_action(clip_id, target_track, frame)
            }
            Action::TrimClipStart { clip_id, new_source_in } => {
                self.trim_clip_source_from_action(clip_id, TrimEdge::In, new_source_in)
            }
            Action::TrimClipEnd { clip_id, new_source_out } => {
                self.trim_clip_source_from_action(clip_id, TrimEdge::Out, new_source_out)
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
                self.close_project().map_err(mondrian_core::MondrianError::Other)?;
                Ok(())
            }
            Action::ImportMedia(paths) => self.import_media_from_action(paths),

            Action::Custom { namespace, name, .. } => {
                if let Some(error) = ProductAction::unknown_external_action_error(&namespace, &name)
                {
                    Err(error)
                } else {
                    Err(MondrianError::WorkflowStepFailed {
                        step_id: "dispatch_action".to_owned(),
                        reason: format!(
                            "Action has no AppState product Interface implementation: Custom {{ namespace: {namespace:?}, name: {name:?} }}"
                        ),
                    })
                }
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
        if !self.can_copy_to_app_clipboard() {
            return Err(action_not_executed(
                "copy",
                "当前没有可复制的片段或动画关键帧",
            ));
        }
        let Some(selection) = self.primary_selected_clip() else {
            let copied = self.copy_selected_clips_to_clipboard()?;
            return require_action_executed(copied, "copy", "当前没有可复制的片段");
        };
        if self.copy_selected_animation_keyframes(selection)? {
            Ok(())
        } else {
            let copied = self.copy_selected_clips_to_clipboard()?;
            require_action_executed(copied, "copy", "当前没有可复制的片段")
        }
    }

    fn cut_from_action(&mut self) -> Result<()> {
        let removed = self.cut_selected_clips_to_clipboard()?;
        require_action_executed(removed > 0, "cut", "当前没有可剪切的未锁定片段")
    }

    fn open_project_from_action(&mut self, path: PathBuf) -> Result<()> {
        if path.as_os_str().is_empty() {
            return Err(action_not_executed("open_project", "项目路径不能为空"));
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
            return Err(action_not_executed("save_project_as", "目标路径不能为空"));
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
            return Err(action_not_executed("create_project", "项目路径不能为空"));
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
        let project_file = payload.candidate.project_file.clone();
        self.open_project_from_autosave_snapshot(payload.candidate).map_err(|err| {
            let reason = err.to_string();
            self.set_status_hint(format!("恢复自动保存失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "recover_project".to_string(), reason }
        })?;
        self.set_status_hint(
            format!("已从自动保存恢复：{}", project_file.display()),
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
            return Err(action_not_executed(
                "import_media",
                "没有提供待导入的媒体文件",
            ));
        }
        if paths.iter().any(|path| path.as_os_str().is_empty()) {
            let reason = "媒体路径不能为空".to_string();
            self.set_status_hint(format!("导入失败：{reason}"), true);
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "import_media".to_string(),
                reason,
            });
        }

        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("导入失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "import_media".to_string(), reason }
        })?;

        if let Some(folder_id) = folder_id
            && !library.folder_exists(folder_id)?
        {
            let reason = format!("目标素材文件夹不存在：{folder_id}");
            self.set_status_hint(format!("导入失败：{reason}"), true);
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "import_media".to_string(),
                reason,
            });
        }

        drop(library);
        self.start_media_import_batch(paths, folder_id.map(str::to_owned))
    }

    fn relink_asset_from_ui(&mut self, payload: AssetRelinkPayload) -> Result<()> {
        self.request_media_asset_mutation(
            payload.asset_id,
            Some(payload.path),
            MediaAssetMutationKind::Relink,
        )
        .map_err(|err| {
            let reason = err.to_string();
            self.set_status_hint(format!("重新链接素材失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "relink_asset".to_string(), reason }
        })?;
        self.set_status_hint("正在分析重新链接的媒体…".to_owned(), false);
        Ok(())
    }

    fn refresh_audio_components_from_ui(&mut self, payload: AssetTargetPayload) -> Result<()> {
        self.request_media_asset_mutation(
            payload.asset_id,
            None,
            MediaAssetMutationKind::RefreshAudioComponents,
        )
        .map_err(|error| {
            let reason = error.to_string();
            self.set_status_hint(format!("音频 Component 探测失败：{reason}"), true);
            MondrianError::WorkflowStepFailed {
                step_id: "assets_refresh_audio_components".to_string(),
                reason,
            }
        })?;
        self.set_status_hint("正在刷新音频流候选…".to_owned(), false);
        Ok(())
    }

    fn rebind_audio_component_from_ui(
        &mut self,
        payload: AssetAudioComponentRebindPayload,
    ) -> Result<()> {
        self.request_media_asset_mutation(
            payload.asset_id,
            None,
            MediaAssetMutationKind::RebindAudioComponent {
                component_id: payload.component_id,
                stream_index: payload.stream_index,
            },
        )
        .map_err(|error| {
            let reason = error.to_string();
            self.set_status_hint(format!("音频 Component 重绑定失败：{reason}"), true);
            MondrianError::WorkflowStepFailed {
                step_id: "assets_rebind_audio_component".to_string(),
                reason,
            }
        })?;
        self.set_status_hint("正在验证音频 Component 重绑定…".to_owned(), false);
        Ok(())
    }

    fn rename_asset_from_ui(&mut self, payload: AssetRenamePayload) -> Result<()> {
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("重命名素材失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "rename_asset".to_string(), reason }
        })?;
        let requested_name = payload.name.trim();
        let asset =
            library
                .get_asset(payload.asset_id)?
                .ok_or_else(|| MondrianError::AssetNotFound {
                    asset_id: payload.asset_id.to_string(),
                })?;
        if asset.name == requested_name {
            return Err(action_not_executed(
                "rename_asset",
                "素材已经使用请求的名称",
            ));
        }
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
        payload: AssetSetInterpretationPayload,
    ) -> Result<()> {
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("解释素材失败：{reason}"), true);
            MondrianError::WorkflowStepFailed {
                step_id: "set_asset_interpretation".to_string(),
                reason,
            }
        })?;
        let asset_name =
            library
                .get_asset(payload.asset_id)?
                .ok_or_else(|| MondrianError::AssetNotFound {
                    asset_id: payload.asset_id.to_string(),
                })?;
        if asset_name.interpretation == payload.interpretation {
            return Err(action_not_executed(
                "set_asset_interpretation",
                "素材已经使用请求的解释设置",
            ));
        }
        let asset_name = asset_name.name;
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

    fn rename_folder_from_ui(&mut self, payload: AssetRenameFolderPayload) -> Result<()> {
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("重命名文件夹失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "rename_folder".to_string(), reason }
        })?;
        let requested_name = payload.name.trim();
        let folder = library
            .list_folders()?
            .into_iter()
            .find(|folder| folder.id == payload.folder_id)
            .ok_or_else(|| MondrianError::AssetDbError {
                reason: format!("文件夹不存在：{}", payload.folder_id),
            })?;
        if folder.name == requested_name {
            return Err(action_not_executed(
                "rename_folder",
                "文件夹已经使用请求的名称",
            ));
        }
        library.rename_folder(&payload.folder_id, &payload.name).map_err(|err| {
            let reason = err.to_string();
            self.set_status_hint(format!("重命名文件夹失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "rename_folder".to_string(), reason }
        })?;
        self.event_bus.publish(mondrian_core::events::AppEvent::AssetLibraryReloaded);
        self.set_status_hint(format!("已重命名文件夹：{}", payload.name.trim()), false);
        Ok(())
    }

    fn set_asset_proxy_mode_from_ui(&mut self, payload: AssetSetProxyModePayload) -> Result<()> {
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
        if self.proxy_mode_assets().contains(&payload.asset_id) == payload.enabled {
            return Err(action_not_executed(
                "set_asset_proxy_mode",
                "素材代理偏好已经处于请求状态",
            ));
        }
        let source_path = asset.file_path().map(PathBuf::from);
        if payload.enabled && source_path.as_ref().is_none_or(|source_path| !source_path.exists()) {
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
            let source_path = source_path.ok_or_else(|| MondrianError::WorkflowStepFailed {
                step_id: "set_asset_proxy_mode".to_owned(),
                reason: "视频素材没有文件源".to_owned(),
            })?;
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
                source_path,
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

    fn remove_asset_library_entries(
        &mut self,
        payload: AssetLibrarySelectionPayload,
    ) -> Result<()> {
        if payload.asset_ids.is_empty() && payload.folder_ids.is_empty() {
            return Err(action_not_executed(
                "remove_asset_library_entries",
                "当前没有可删除的素材或文件夹选择",
            ));
        }
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("删除素材选择失败：{reason}"), true);
            MondrianError::WorkflowStepFailed {
                step_id: "remove_asset_library_entries".to_string(),
                reason,
            }
        })?;
        let single_subject = match (payload.asset_ids.as_slice(), payload.folder_ids.as_slice()) {
            ([asset_id], []) => library
                .get_asset(*asset_id)?
                .map(|asset| AssetLibrarySubject::Asset(asset.name)),
            ([], [folder_id]) => library
                .list_folders()?
                .into_iter()
                .find(|folder| folder.id == *folder_id)
                .map(|folder| AssetLibrarySubject::Folder(folder.name)),
            _ => None,
        };
        let outcome = library
            .retire_assets_and_delete_folders(&payload.asset_ids, &payload.folder_ids)
            .map_err(|err| {
                let reason = err.to_string();
                self.set_status_hint(format!("删除素材选择失败：{reason}"), true);
                MondrianError::WorkflowStepFailed {
                    step_id: "remove_asset_library_entries".to_string(),
                    reason,
                }
            })?;

        if outcome.retired_assets + outcome.deleted_folders == 0 {
            return Err(action_not_executed(
                "remove_asset_library_entries",
                "素材库已经处于请求的状态",
            ));
        }
        let mut retired_ids = Vec::new();
        for asset_id in payload.asset_ids {
            if !retired_ids.contains(&asset_id) {
                retired_ids.push(asset_id);
                self.event_bus
                    .publish(mondrian_core::events::AppEvent::AssetRetired { asset_id });
            }
        }
        self.event_bus.publish(mondrian_core::events::AppEvent::AssetLibraryReloaded);
        if let Some(subject) = single_subject {
            let message = match subject {
                AssetLibrarySubject::Asset(name) => format!("已从素材面板移除：{name}"),
                AssetLibrarySubject::Folder(name) => format!("已删除文件夹：{name}"),
            };
            self.set_status_hint(message, false);
        } else {
            self.set_status_hint(
                format!(
                    "已从素材面板移除 {} 个素材、删除 {} 个文件夹",
                    outcome.retired_assets, outcome.deleted_folders
                ),
                false,
            );
        }
        Ok(())
    }

    fn move_asset_library_entries(&mut self, mut payload: AssetLibraryMovePayload) -> Result<()> {
        if payload.asset_ids.is_empty() && payload.folder_ids.is_empty() {
            return Err(action_not_executed(
                "move_asset_library_entries",
                "当前没有可移动的素材或文件夹选择",
            ));
        }
        if let Some(target_folder_id) = payload.target_folder_id.as_deref() {
            payload.folder_ids.retain(|folder_id| folder_id != target_folder_id);
        }
        if payload.asset_ids.is_empty() && payload.folder_ids.is_empty() {
            return Err(action_not_executed(
                "move_asset_library_entries",
                "目标文件夹不能同时作为唯一移动对象",
            ));
        }
        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("移动素材选择失败：{reason}"), true);
            MondrianError::WorkflowStepFailed {
                step_id: "move_asset_library_entries".to_string(),
                reason,
            }
        })?;
        let folders = library.list_folders().map_err(|err| {
            let reason = err.to_string();
            self.set_status_hint(format!("移动素材选择失败：{reason}"), true);
            MondrianError::WorkflowStepFailed {
                step_id: "move_asset_library_entries".to_string(),
                reason,
            }
        })?;
        let single_subject = match (payload.asset_ids.as_slice(), payload.folder_ids.as_slice()) {
            ([asset_id], []) => library
                .get_asset(*asset_id)?
                .map(|asset| AssetLibrarySubject::Asset(asset.name)),
            ([], [folder_id]) => folders
                .iter()
                .find(|folder| folder.id == *folder_id)
                .map(|folder| AssetLibrarySubject::Folder(folder.name.clone())),
            _ => None,
        };
        let target_name = match payload.target_folder_id.as_deref() {
            Some(folder_id) => folders
                .iter()
                .find(|folder| folder.id == folder_id)
                .map(|folder| folder.name.clone())
                .unwrap_or_else(|| folder_id.to_string()),
            None => "All assets".to_string(),
        };
        let outcome = library
            .move_assets_and_folders(
                &payload.asset_ids,
                &payload.folder_ids,
                payload.target_folder_id.as_deref(),
            )
            .map_err(|err| {
                let reason = err.to_string();
                self.set_status_hint(format!("移动素材选择失败：{reason}"), true);
                MondrianError::WorkflowStepFailed {
                    step_id: "move_asset_library_entries".to_string(),
                    reason,
                }
            })?;

        if outcome.moved_assets + outcome.moved_folders == 0 {
            return Err(action_not_executed(
                "move_asset_library_entries",
                "素材库已经处于请求的组织状态",
            ));
        }
        self.event_bus.publish(mondrian_core::events::AppEvent::AssetLibraryReloaded);
        if let Some(subject) = single_subject {
            let message = match subject {
                AssetLibrarySubject::Asset(name) => {
                    format!("已移动素材：{name} → {target_name}")
                }
                AssetLibrarySubject::Folder(name) => {
                    format!("已移动文件夹：{name} → {target_name}")
                }
            };
            self.set_status_hint(message, false);
        } else {
            self.set_status_hint(
                format!(
                    "已移动 {} 个素材、{} 个文件夹 → {target_name}",
                    outcome.moved_assets, outcome.moved_folders
                ),
                false,
            );
        }
        Ok(())
    }

    fn duplicate_from_action(&mut self) -> Result<()> {
        if !self.can_cut_to_app_clipboard() {
            return Err(action_not_executed(
                "duplicate",
                "当前没有可复制的未锁定片段",
            ));
        }
        let duplicated = self.duplicate_selected_clips_after_selection()?;
        require_action_executed(duplicated > 0, "duplicate", "当前没有可复制的未锁定片段")
    }

    fn paste_from_action(&mut self) -> Result<()> {
        if !self.can_paste_from_app_clipboard() {
            return Err(action_not_executed(
                "paste",
                "剪贴板为空或当前没有可用的粘贴目标",
            ));
        }
        match self.active_clipboard_kind {
            Some(AppClipboardKind::AnimationKeyframes) => self
                .paste_animation_keyframes_from_action()
                .or_else(|_| self.paste_clip_clipboard_from_action()),
            Some(AppClipboardKind::Clips) => self.paste_clip_clipboard_from_action(),
            None => {
                if self.has_animation_clipboard() {
                    self.paste_animation_keyframes_from_action()
                } else {
                    self.paste_clip_clipboard_from_action()
                }
            }
        }
    }

    fn paste_clip_clipboard_from_action(&mut self) -> Result<()> {
        let pasted = self.paste_clip_clipboard_at_playhead()?;
        require_action_executed(pasted > 0, "paste", "剪贴板为空或当前没有可用的粘贴目标")
    }

    fn paste_animation_keyframes_from_action(&mut self) -> Result<()> {
        let Some(selection) = self.primary_selected_clip() else {
            return Err(action_not_executed("paste", "当前没有动画关键帧粘贴目标"));
        };
        let timeline_time = self.current_timeline_time()?.unwrap_or(TimelineTime::ZERO);
        let destination_time = self
            .active_sequence()
            .and_then(|sequence| find_clip(sequence, selection.clip_id))
            .map(|clip| clip.clamped_visual_author_time(timeline_time))
            .transpose()?
            .ok_or_else(|| missing_clip_error("paste_animation_keyframes", selection.clip_id))?;
        let pasted = self.paste_animation_keyframes(selection, destination_time)?;
        require_action_executed(pasted, "paste", "动画关键帧剪贴板为空")
    }

    fn nudge_clip_from_action(&mut self, clip_id: ClipId, delta_frames: i64) -> Result<()> {
        if delta_frames == 0 {
            return Err(action_not_executed("nudge_clip", "移动帧数不能为零"));
        }
        let (track_id, _is_video_track, frame) =
            self.clip_action_location("nudge_clip", clip_id)?;
        self.move_clip_to_track_with_mode(
            track_id,
            clip_id,
            frame.saturating_add(delta_frames).max(0),
            ClipOverlapMode::Overwrite,
        )
    }

    fn move_clip_to_track_from_action(
        &mut self,
        clip_id: ClipId,
        target_track_id: mondrian_core::types::TrackId,
        frame: i64,
    ) -> Result<()> {
        self.move_clip_to_track_with_mode(
            target_track_id,
            clip_id,
            frame.max(0),
            ClipOverlapMode::Overwrite,
        )
    }

    /// Lower one exact Action position onto the active Sequence evaluation
    /// grid. `FramePosition::time_base` is part of the input value and is
    /// therefore converted to exact author time before the single, explicit
    /// nearest-frame quantization at this Adapter seam.
    fn sequence_frame_from_action_position(
        &self,
        step_id: &'static str,
        position: FramePosition,
    ) -> Result<i64> {
        let sequence = self.active_sequence().ok_or_else(|| missing_sequence_error(step_id))?;
        lower_nearest_sequence_frame(sequence, position, step_id)
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
            source_trim_target_time(clip, edge, source_time)?
                .to_frame_position(seq.settings.frame_rate, FrameRounding::Nearest)?
                .frame
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

    pub(super) fn select_clip_for_action(
        &mut self,
        step_id: &'static str,
        clip_id: ClipId,
    ) -> Result<()> {
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
            return self.dispatch_video_transition_product_action(
                VideoTransitionProductAction::Remove(VideoTransitionTargetPayload {
                    transition_id: selection.transition_id,
                }),
            );
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
            return Err(action_not_executed(
                "delete_selection",
                "当前没有可删除的时间线选择",
            ));
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

    fn dispatch_product_action(&mut self, action: ProductAction) -> Result<()> {
        match action {
            ProductAction::Timeline(action) => self.dispatch_timeline_product_action(action),
            ProductAction::Track(action) => self.dispatch_track_product_action(action),
            ProductAction::VideoTransition(action) => {
                self.dispatch_video_transition_product_action(action)
            }
            ProductAction::Audio(action) => self.dispatch_audio_product_action(action),
            ProductAction::Asset(action) => self.dispatch_asset_product_action(action),
            ProductAction::Viewer(action) => self.dispatch_viewer_product_action(action),
            ProductAction::Clip(action) => self.dispatch_clip_product_action(action),
            ProductAction::Project(action) => self.dispatch_project_product_action(action),
            ProductAction::Sequence(action) => self.dispatch_sequence_product_action(action),
            ProductAction::Export(action) => self.dispatch_export_product_action(action),
            ProductAction::VisualEffect(action) => {
                self.dispatch_visual_effect_product_action(action)
            }
        }
    }

    fn dispatch_asset_product_action(&mut self, action: AssetProductAction) -> Result<()> {
        match action {
            AssetProductAction::PrepareDrag(payload) => self.prepare_asset_drag(payload),
            AssetProductAction::RefreshAudioComponents(payload) => {
                self.refresh_audio_components_from_ui(payload)
            }
            AssetProductAction::RebindAudioComponent(payload) => {
                self.rebind_audio_component_from_ui(payload)
            }
            AssetProductAction::CreateGenerated(payload) => match payload.kind {
                mondrian_core::types::GeneratedAssetKind::AdjustmentLayer => self
                    .create_adjustment_layer_asset_in_folder(None, payload.folder_id.as_deref())
                    .map(|_| ()),
                mondrian_core::types::GeneratedAssetKind::SolidColor => self
                    .create_solid_color_asset_in_folder(None, payload.folder_id.as_deref())
                    .map(|_| ()),
            },
            AssetProductAction::CreateFolder(payload) => self
                .create_default_folder_in_library(payload.parent_folder_id.as_deref())
                .map(|_| ()),
            AssetProductAction::ImportFiles(payload) => self
                .import_media_into_folder_from_action(payload.paths, payload.folder_id.as_deref()),
            AssetProductAction::Relink(payload) => self.relink_asset_from_ui(payload),
            AssetProductAction::SetInterpretation(payload) => {
                self.set_asset_interpretation_from_ui(payload)
            }
            AssetProductAction::Rename(payload) => self.rename_asset_from_ui(payload),
            AssetProductAction::RenameFolder(payload) => self.rename_folder_from_ui(payload),
            AssetProductAction::SetProxyMode(payload) => self.set_asset_proxy_mode_from_ui(payload),
            AssetProductAction::RemoveEntries(payload) => {
                self.remove_asset_library_entries(*payload)
            }
            AssetProductAction::MoveEntries(payload) => self.move_asset_library_entries(*payload),
        }
    }

    fn dispatch_video_transition_product_action(
        &mut self,
        action: VideoTransitionProductAction,
    ) -> Result<()> {
        match action {
            VideoTransitionProductAction::Select(payload) => self
                .select_video_transition_by_id(payload.transition_id)
                .map(|_| ())
                .ok_or_else(|| MondrianError::WorkflowStepFailed {
                    step_id: "video_transition_select".to_owned(),
                    reason: format!(
                        "visual Transition does not exist: {}",
                        payload.transition_id
                    ),
                }),
            VideoTransitionProductAction::CreateCrossDissolve(payload) => {
                match self.create_default_cross_dissolve(
                    payload.left_clip_id,
                    payload.right_clip_id,
                    payload.handle_policy,
                ) {
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
            VideoTransitionProductAction::SetRange(payload) => {
                match self.set_video_transition_range(
                    payload.transition_id,
                    payload.requested_range,
                    payload.handle_policy,
                ) {
                    Ok(outcome) => {
                        require_action_executed(
                            outcome.changed,
                            "video_transition_set_range",
                            "visual Transition already has the requested range",
                        )?;
                        self.select_video_transition_by_id(payload.transition_id);
                        Ok(())
                    }
                    Err(error) => {
                        self.set_status_hint(format!("无法调整视频转场：{error}"), true);
                        Err(error)
                    }
                }
            }
            VideoTransitionProductAction::Remove(payload) => {
                self.remove_video_transition(payload.transition_id)?;
                self.clear_selection();
                Ok(())
            }
        }
    }

    fn dispatch_timeline_product_action(&mut self, action: TimelineProductAction) -> Result<()> {
        match action {
            TimelineProductAction::SelectClip(payload) => {
                let mode = match payload.mode {
                    TimelineClipSelectionModePayload::Replace => ClipSelectionMode::Replace,
                    TimelineClipSelectionModePayload::Toggle => ClipSelectionMode::Toggle,
                    TimelineClipSelectionModePayload::Preserve => ClipSelectionMode::Preserve,
                };
                self.select_clip_unit_by_id(payload.clip_id, mode)
                    .map(|_| ())
                    .ok_or_else(|| missing_clip_error("timeline_select_clip", payload.clip_id))
            }
            TimelineProductAction::MoveClip(payload) => self.move_clip_from_product_action(payload),
            TimelineProductAction::TrimClips(payload) => {
                self.trim_clips_from_product_action(payload).map(|_| ())
            }
            TimelineProductAction::Seek(payload) => self.seek_from_product_action(payload),
            TimelineProductAction::SetInOutPoint(payload) => {
                self.set_timeline_in_out_point(payload)
            }
            TimelineProductAction::ClearInOutPoints => self.clear_timeline_in_out_points(),
            TimelineProductAction::ApplyRangeEdit(kind) => {
                self.apply_timeline_range_edit(kind).map(|_| ())
            }
            TimelineProductAction::EditSelection(edit) => self.apply_timeline_selection_edit(edit),
            TimelineProductAction::CreateBasicTitle => {
                self.create_basic_title_at_playhead().map(|_| ())
            }
            TimelineProductAction::PlaceAsset(payload) => self.place_asset_on_timeline(payload),
            TimelineProductAction::InsertAsset(payload) => {
                self.insert_asset_from_ui(*payload).map(|_| ())
            }
            TimelineProductAction::PrecomposeSelection(payload) => {
                self.precompose_selection(payload)
            }
        }
    }

    fn dispatch_track_product_action(&mut self, action: TrackProductAction) -> Result<()> {
        match action {
            TrackProductAction::Add(payload) => {
                if self.active_sequence().is_none() {
                    return Err(action_not_executed(
                        "track_add",
                        "there is no active Sequence",
                    ));
                }
                match payload.kind {
                    TrackAddKind::Video => self.add_video_track(),
                    TrackAddKind::Audio => self.add_audio_track(),
                }
                .map_err(|error| MondrianError::WorkflowStepFailed {
                    step_id: "track_add".to_owned(),
                    reason: error.to_string(),
                })
            }
            TrackProductAction::Move(payload) => {
                self.move_track(payload.track_id, payload.placement).and_then(|changed| {
                    require_action_executed(
                        changed,
                        "track_move",
                        "Track already has the requested relative placement",
                    )
                })
            }
            TrackProductAction::SetAuthorControl(payload) => {
                let changed = match payload.control {
                    TrackAuthorControl::Visibility => {
                        self.set_track_visible(payload.track_id, payload.enabled)?
                    }
                    TrackAuthorControl::Mute => {
                        self.set_track_muted(payload.track_id, payload.enabled)?
                    }
                    TrackAuthorControl::Lock => {
                        self.set_track_locked(payload.track_id, payload.enabled)?
                    }
                };
                require_action_executed(
                    changed,
                    "track_set_author_control",
                    "Track author control already has the requested value",
                )
            }
            TrackProductAction::SetEditPolicy(payload) => {
                let changed = match payload.control {
                    TrackEditPolicyControl::Target => {
                        self.set_timeline_track_targeted(payload.track_id, payload.enabled)?
                    }
                    TrackEditPolicyControl::SyncLock => {
                        self.set_timeline_track_sync_locked(payload.track_id, payload.enabled)?
                    }
                };
                require_action_executed(
                    changed,
                    "track_set_edit_policy",
                    "Track edit policy already has the requested value",
                )
            }
        }
    }

    fn dispatch_visual_effect_product_action(
        &mut self,
        action: VisualEffectProductAction,
    ) -> Result<()> {
        match action {
            VisualEffectProductAction::AddToClip(payload) => {
                let effect_id = self.add_effect_to_clip(payload.clip_id, payload.effect_type)?;
                self.select_effect_for_action(
                    "visual_effect_add_to_clip",
                    payload.clip_id,
                    effect_id,
                )
            }
            VisualEffectProductAction::Select(payload) => {
                if self.primary_selected_effect().is_some_and(|selected| {
                    selected.clip.clip_id == payload.clip_id
                        && selected.effect_id == payload.effect_id
                }) {
                    return Err(action_not_executed(
                        "visual_effect_select",
                        "Effect is already the active Inspector selection",
                    ));
                }
                self.select_effect_for_action(
                    "visual_effect_select",
                    payload.clip_id,
                    payload.effect_id,
                )
            }
            VisualEffectProductAction::SetEnabled(payload) => require_action_executed(
                self.set_clip_effect_enabled(payload.clip_id, payload.effect_id, payload.enabled)?,
                "visual_effect_set_enabled",
                "Effect already has the requested enabled state",
            ),
            VisualEffectProductAction::Remove(payload) => require_action_executed(
                self.remove_effect_from_clip(payload.clip_id, payload.effect_id)?,
                "visual_effect_remove",
                "Effect was not removed",
            ),
            VisualEffectProductAction::Reorder(payload) => require_action_executed(
                self.reorder_effect_for_clip(
                    payload.clip_id,
                    payload.effect_id,
                    payload.placement,
                )?,
                "visual_effect_reorder",
                "Effect chain already has the requested relative order",
            ),
            VisualEffectProductAction::SetParameterValue(payload) => require_action_executed(
                self.set_effect_parameter_value(*payload)?,
                "visual_effect_set_parameter_value",
                "Effect parameter already has the requested value at the current author time",
            ),
        }
    }

    fn dispatch_export_product_action(&mut self, action: ExportProductAction) -> Result<()> {
        match action {
            ExportProductAction::EditDraft(edit) => {
                if let ExportDraftEdit::Sequence(Some(sequence_id)) = edit.as_ref()
                    && self.sequence_by_id(*sequence_id).is_none()
                {
                    return Err(action_not_executed(
                        "edit_export_draft",
                        format!("Export draft Sequence no longer exists: {sequence_id}"),
                    ));
                }
                let changed = match *edit {
                    ExportDraftEdit::BuiltinPreset(preset) => {
                        self.set_export_draft_builtin_preset(preset)
                    }
                    ExportDraftEdit::Preset(preset) => self.set_export_draft_preset(preset),
                    ExportDraftEdit::Sequence(sequence_id) => {
                        self.set_export_draft_sequence_id(sequence_id)
                    }
                    ExportDraftEdit::Range(range) => self.set_export_draft_range(range),
                    ExportDraftEdit::OutputPath(output_path) => {
                        self.set_export_draft_output_path(output_path)
                    }
                };
                require_action_executed(
                    changed,
                    "edit_export_draft",
                    "Export draft already has the requested value",
                )
            }
            ExportProductAction::Enqueue(request) => {
                self.enqueue_timeline_export(*request).map(|_| ())
            }
            ExportProductAction::Cancel(job_id) => match self.cancel_export_job(job_id) {
                ExportCancelOutcome::Requested => Ok(()),
                ExportCancelOutcome::AlreadyRequested => Err(action_not_executed(
                    "cancel_export",
                    "Export cancellation was already requested",
                )),
                ExportCancelOutcome::TooLateCommitting => Err(action_not_executed(
                    "cancel_export",
                    "Export already crossed irreversible publication",
                )),
                ExportCancelOutcome::AlreadyTerminal => Err(action_not_executed(
                    "cancel_export",
                    "Export attempt is already terminal",
                )),
                ExportCancelOutcome::NotFound => Err(action_not_executed(
                    "cancel_export",
                    format!("Export job is no longer retained: {job_id}"),
                )),
            },
            ExportProductAction::ClearTerminalHistory => require_action_executed(
                self.clear_terminal_export_history() > 0,
                "clear_terminal_export_history",
                "Export queue has no retained terminal evidence",
            ),
        }
    }

    fn dispatch_viewer_product_action(&mut self, action: ViewerProductAction) -> Result<()> {
        match action {
            ViewerProductAction::SetPreviewResolutionScale(payload) => {
                self.set_preview_resolution_scale_from_ui(payload.scale)
            }
        }
    }

    fn dispatch_clip_product_action(&mut self, action: ClipProductAction) -> Result<()> {
        match action {
            ClipProductAction::SetEnabled(payload) => require_action_executed(
                self.set_clip_enabled_by_id(payload.clip_id, payload.enabled)?,
                "clip_set_enabled",
                "Clip already has the requested enabled state",
            ),
            ClipProductAction::SetSolidColor(payload) => require_action_executed(
                self.set_clip_solid_color_by_id(payload.clip_id, payload.color)?,
                "clip_set_solid_color",
                "Solid Color Clip already has the requested source color",
            ),
            ClipProductAction::SetRate(payload) => require_action_executed(
                self.set_clip_rate_from_action(
                    payload.clip_id,
                    payload.rate,
                    payload.include_linked,
                )?,
                "clip_set_rate",
                "Clip already has the requested source-time rate",
            ),
            ClipProductAction::HoldFrame(payload) => require_action_executed(
                self.hold_video_clip_from_action(payload.clip_id, payload.sequence_time)?,
                "clip_hold_frame",
                "Clip already holds the requested source picture",
            ),
            ClipProductAction::WriteParameterValues(payload) => require_action_executed(
                self.write_clip_parameter_values(*payload)?,
                "clip_write_parameter_values",
                "Clip parameters already have the requested values at the current author time",
            ),
            ClipProductAction::EditNumericCurve(payload) => {
                let clip_id = payload.clip_id;
                let property = payload.parameter.clone();
                let removing = matches!(&payload.edit, ClipCurveEditPayload::Remove { .. });
                let outcome = self.edit_clip_numeric_curve_payload(*payload)?;
                require_action_executed(
                    outcome.changed,
                    "clip_edit_numeric_curve",
                    "Numeric curve already contains the requested key state",
                )?;
                let key_selection = crate::app::AnimationKeyframeSelection {
                    property: crate::app::AnimationPropertySelection {
                        clip_id,
                        property: property.clone(),
                    },
                    keyframe_id: outcome.keyframe_id,
                };
                if removing {
                    self.animation_selection.selected_keyframes.remove(&key_selection);
                    self.set_active_animation_property(clip_id, property);
                } else {
                    self.select_animation_keyframe_only(key_selection);
                }
                Ok(())
            }
        }
    }

    fn dispatch_project_product_action(&mut self, action: ProjectProductAction) -> Result<()> {
        match action {
            ProjectProductAction::CreateWithSettings(payload) => {
                self.create_project_from_ui(payload)
            }
            ProjectProductAction::RecoverFromAutosave(payload) => {
                self.recover_project_from_autosave_ui(payload)
            }
            ProjectProductAction::UpdateNewSequenceDefaults(payload) => {
                self.update_new_sequence_defaults(payload.settings)
            }
            ProjectProductAction::UpdateColorEnvironment(payload) => {
                self.update_project_color_environment(payload.color_environment)
            }
        }
    }

    fn dispatch_sequence_product_action(&mut self, action: SequenceProductAction) -> Result<()> {
        match action {
            SequenceProductAction::New => {
                let next = self.export_sequences_snapshot().len() + 1;
                self.new_sequence(&format!("Sequence {next}")).map(|_| ())
            }
            SequenceProductAction::ReturnToParent => require_action_executed(
                self.return_to_parent_sequence()?,
                "return_to_parent_sequence",
                "当前不在嵌套序列中",
            ),
            SequenceProductAction::SetActiveDefault => {
                let sequence_id =
                    self.active_sequence_id().ok_or_else(|| MondrianError::WorkflowStepFailed {
                        step_id: "set_default_sequence".to_owned(),
                        reason: "当前没有活动序列".to_owned(),
                    })?;
                self.set_default_sequence(sequence_id)
            }
            SequenceProductAction::SwitchActive(payload) => {
                self.switch_active_sequence(payload.sequence_id)
            }
            SequenceProductAction::OpenNested(payload) => {
                self.open_nested_sequence(payload.sequence_id)
            }
            SequenceProductAction::Duplicate(payload) => {
                let source = self.sequence_by_id(payload.sequence_id).ok_or_else(|| {
                    MondrianError::WorkflowStepFailed {
                        step_id: "duplicate_sequence".to_owned(),
                        reason: format!("序列不存在: {}", payload.sequence_id),
                    }
                })?;
                let name = format!("{} Copy", source.name);
                self.duplicate_sequence(payload.sequence_id, name).map(|_| ())
            }
            SequenceProductAction::Delete(payload) => self.delete_sequence(payload.sequence_id),
            SequenceProductAction::UpdateSettings(payload) => {
                let SequenceUpdateSettingsPayload { sequence_id, name, settings } = *payload;
                self.update_sequence_identity_and_settings(sequence_id, name, settings)
            }
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
        let sequence = self
            .sequences()
            .iter()
            .find(|sequence| sequence.id == sequence_id)
            .or_else(|| self.active_sequence().filter(|sequence| sequence.id == sequence_id))
            .ok_or_else(|| MondrianError::WorkflowStepFailed {
                step_id: "viewer_ui_action".to_string(),
                reason: format!("序列不存在: {sequence_id}"),
            })?;

        let current =
            normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale);
        if (current - scale).abs() <= f32::EPSILON {
            return Ok(());
        }

        self.commit_sequence_edit(sequence_id, "修改预览分辨率", |sequence| {
            let mut settings = sequence.settings.clone();
            settings.preview.resolution_scale = scale;
            sequence.apply_settings(settings)
        })?;
        self.set_status_hint(
            format!("预览分辨率：{}", preview_resolution_scale_label(scale)),
            false,
        );
        Ok(())
    }

    pub(super) fn prepare_asset_drag(&mut self, payload: AssetTargetPayload) -> Result<()> {
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
        let media_probe = asset.media_probe();
        if matches!(
            asset.kind,
            AssetKind::Audio | AssetKind::Video | AssetKind::StillImage
        ) && media_probe.is_none()
        {
            let reason = "文件素材的探测证据缺失或已失效，请先刷新或重新链接素材".to_owned();
            self.set_status_hint(format!("素材准备失败：{reason}"), true);
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "assets_prepare_drag".to_owned(),
                reason,
            });
        }
        let duration = match asset.kind {
            AssetKind::AdjustmentLayer | AssetKind::SolidColor => {
                self.default_visual_placement_drag_duration()?
            }
            AssetKind::StillImage => self.default_visual_placement_drag_duration()?,
            AssetKind::Audio | AssetKind::Video => {
                let Some(probe) = media_probe else {
                    return Err(MondrianError::WorkflowStepFailed {
                        step_id: "assets_prepare_drag".to_owned(),
                        reason: "文件素材探测证据在动作执行期间失效".to_owned(),
                    });
                };
                if probe.duration == Duration::ZERO {
                    let reason = "媒体探测没有证明正的源时长，不能创建虚构长度的 Clip".to_owned();
                    self.set_status_hint(format!("素材准备失败：{reason}"), true);
                    return Err(MondrianError::WorkflowStepFailed {
                        step_id: "assets_prepare_drag".to_owned(),
                        reason,
                    });
                }
                probe.duration
            }
        };
        let has_linked_audio = matches!(asset.kind, AssetKind::Video)
            && media_probe.is_some_and(|probe| probe.has_audio);
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

    fn set_effect_parameter_value(
        &mut self,
        payload: VisualEffectSetParameterValuePayload,
    ) -> Result<bool> {
        let VisualEffectSetParameterValuePayload { clip_id, effect_id, parameter, value } = payload;
        self.ensure_clip_track_unlocked("visual_effect_set_parameter_value", clip_id)?;
        let (_, is_video_track, _) =
            self.clip_action_location("visual_effect_set_parameter_value", clip_id)?;
        if !is_video_track {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "visual_effect_set_parameter_value".to_owned(),
                reason: "visual Effects require a video Clip".to_owned(),
            });
        }
        let current_time = self.current_timeline_time()?.unwrap_or(TimelineTime::ZERO);
        let Some(sequence_id) = self.active_sequence_id() else {
            return Err(missing_sequence_error("visual_effect_set_parameter_value"));
        };
        let mutation = {
            let sequence = self
                .active_sequence()
                .ok_or_else(|| missing_sequence_error("visual_effect_set_parameter_value"))?;
            let clip = find_clip(sequence, clip_id)
                .ok_or_else(|| missing_clip_error("visual_effect_set_parameter_value", clip_id))?;
            let end = clip.end_position()?;
            let author_time = clip.timeline_to_clip_time(current_time.clamp(clip.position, end))?;
            let effect =
                clip.effects.iter().find(|effect| effect.id == effect_id).ok_or_else(|| {
                    MondrianError::WorkflowStepFailed {
                        step_id: "visual_effect_set_parameter_value".to_owned(),
                        reason: format!("Effect {effect_id} does not belong to Clip {clip_id}"),
                    }
                })?;
            effect
                .properties
                .prepare_value_write_by_address(&parameter, author_time, value)?
        };
        let Some(mutation) = mutation else {
            return Ok(false);
        };
        let _sequence_id =
            self.commit_sequence_edit(sequence_id, "调整特效属性", |sequence| {
                let clip = find_clip_mut(sequence, clip_id).ok_or_else(|| {
                    missing_clip_error("visual_effect_set_parameter_value", clip_id)
                })?;
                let effect =
                    clip.effects.iter_mut().find(|effect| effect.id == effect_id).ok_or_else(
                        || MondrianError::WorkflowStepFailed {
                            step_id: "visual_effect_set_parameter_value".to_owned(),
                            reason: format!("Effect {effect_id} does not belong to Clip {clip_id}"),
                        },
                    )?;
                effect.apply_property_mutation(mutation)?;
                Ok(sequence.id)
            })?;
        Ok(true)
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

/// Map a source-local trim coordinate into exact Sequence-local author time.
///
/// The caller lowers the result once onto the active Sequence evaluation grid;
/// the source frame grid must never be reused to interpret a Sequence position.
fn source_trim_target_time(
    clip: &Clip,
    edge: TrimEdge,
    source_time: FramePosition,
) -> Result<TimelineTime> {
    let source_time = TimelineTime::from_frame_position(source_time)?;
    let speed = clip.source_time_scale();
    if speed.numerator() <= 0 {
        return Err(MondrianError::WorkflowStepFailed {
            step_id: "trim_clip_source".to_string(),
            reason: "source trim requires a positive finite speed multiplier".to_string(),
        });
    }
    let source_origin = clip.source_origin();
    let source_terminal = clip.source_terminal_boundary()?;
    match edge {
        TrimEdge::In if source_time >= source_terminal => {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "trim_clip_source".to_string(),
                reason: "source in must be before current source out".to_string(),
            });
        }
        TrimEdge::Out if source_time <= source_origin => {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "trim_clip_source".to_string(),
                reason: "source out must be after current source in".to_string(),
            });
        }
        _ => {}
    }

    let source_delta = source_time.checked_sub(source_origin)?;
    let timeline_delta = source_delta.checked_scale(speed.reciprocal()?)?;
    Ok(clip.position.checked_add(timeline_delta)?.max(TimelineTime::ZERO))
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

#[cfg(test)]
fn poll_media_asset_mutations_until_idle(state: &mut AppState) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while state.media_asset_mutation_diagnostics().outstanding > 0 {
        state.poll_media_asset_mutations();
        if state.media_asset_mutation_diagnostics().outstanding == 0 {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for background media Asset mutation"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn action_not_executed(action: &'static str, reason: impl Into<String>) -> MondrianError {
    MondrianError::ActionNotExecuted { action: action.to_owned(), reason: reason.into() }
}

fn require_action_executed(
    executed: bool,
    action: &'static str,
    reason: &'static str,
) -> Result<()> {
    if executed {
        Ok(())
    } else {
        Err(action_not_executed(action, reason))
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

fn missing_clip_error(step_id: &'static str, clip_id: ClipId) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: step_id.to_string(),
        reason: format!("片段不存在: {clip_id}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tt(frame: i64, time_base: mondrian_core::Rational) -> mondrian_core::TimelineTime {
        let numerator = frame.checked_mul(time_base.num).expect("test time fits i64");
        mondrian_core::TimelineTime::new(numerator, time_base.den).expect("valid test time")
    }

    fn assert_action_not_executed(error: MondrianError, expected_action: &'static str) -> String {
        match error {
            MondrianError::ActionNotExecuted { action, reason } => {
                assert_eq!(action, expected_action);
                assert!(!reason.trim().is_empty());
                reason
            }
            other => panic!("expected ActionNotExecuted for {expected_action}, got {other:?}"),
        }
    }
    use crate::app::ui_actions::{
        assets_create_adjustment_layer_action, assets_create_folder_action,
        assets_create_solid_color_action, assets_delete_asset_action, assets_delete_folder_action,
        assets_delete_selection_action, assets_import_files_action, assets_move_asset_action,
        assets_move_folder_action, assets_move_selection_action, assets_prepare_drag_action,
        assets_rebind_audio_component_action, assets_refresh_audio_components_action,
        assets_relink_asset_action, assets_rename_asset_action, assets_rename_folder_action,
        assets_set_interpretation_action, assets_set_proxy_mode_action,
        audio_component_edit_action, clip_edit_numeric_curve_action, clip_set_enabled_action,
        clip_set_solid_color_action, clip_write_parameter_values_action, export_cancel_action,
        export_clear_terminal_history_action, export_edit_draft_action, export_enqueue_action,
        project_create_with_settings_action, project_recover_from_autosave_action,
        project_update_color_environment_action, project_update_new_sequence_defaults_action,
        sequence_delete_action, sequence_duplicate_action, sequence_new_action,
        sequence_return_to_parent_action, sequence_set_active_default_action,
        sequence_switch_active_action, sequence_update_settings_action,
        timeline_clear_in_out_points_action, timeline_create_basic_title_action,
        timeline_drop_asset_action, timeline_insert_asset_action,
        timeline_link_selected_clips_action, timeline_move_clip_action,
        timeline_open_nested_sequence_action, timeline_roll_selected_cut_to_playhead_action,
        timeline_seek_action, timeline_seek_with_source_action, timeline_select_clip_action,
        timeline_set_in_out_point_action, timeline_set_selected_clips_enabled_action,
        timeline_trim_clips_action, timeline_trim_selected_clips_to_playhead_action,
        timeline_unlink_selected_clips_action, track_add_action, track_move_action,
        track_set_author_control_action, track_set_edit_policy_action,
        viewer_set_preview_resolution_scale_action, visual_effect_add_to_clip_action,
        visual_effect_remove_action, visual_effect_reorder_action, visual_effect_select_action,
        visual_effect_set_enabled_action, visual_effect_set_parameter_value_action,
        AssetsCreateAssetPayload, AssetsCreateFolderPayload, AssetsDeleteAssetPayload,
        AssetsDeleteFolderPayload, AssetsDeleteSelectionPayload, AssetsImportFilesPayload,
        AssetsMoveAssetPayload, AssetsMoveFolderPayload, AssetsMoveSelectionPayload,
        AssetsPrepareDragPayload, AssetsRebindAudioComponentPayload,
        AssetsRefreshAudioComponentsPayload, AssetsRelinkAssetPayload, AssetsRenameAssetPayload,
        AssetsRenameFolderPayload, AssetsSetInterpretationPayload, AssetsSetProxyModePayload,
        ClipCurveEditPayload, ClipEditNumericCurvePayload, ClipNormalizedCurvePointPayload,
        ClipParameterValueWrite, ClipSetEnabledPayload, ClipSetSolidColorPayload,
        ClipWriteParameterValuesPayload, ExportDraftEdit, ProjectCreateWithSettingsPayload,
        ProjectRecoverFromAutosavePayload, ProjectUpdateColorEnvironmentPayload,
        ProjectUpdateNewSequenceDefaultsPayload, SequenceTargetPayload,
        SequenceUpdateSettingsPayload, TimelineDropAssetPayload, TimelineExportRequest,
        TimelineInOutPointKind, TimelineInsertAssetPayload, TimelineSeekSource,
        TimelineSetInOutPointPayload, TimelineTrimClipsPayload, TimelineTrimPayloadEdge,
        TrackAddKind, TrackAddPayload, TrackAuthorControl, TrackEditPolicyControl,
        TrackMovePayload, TrackSetAuthorControlPayload, TrackSetEditPolicyPayload,
        ViewerSetPreviewResolutionScalePayload, VisualEffectAddToClipPayload,
        VisualEffectReorderPayload, VisualEffectSetEnabledPayload,
        VisualEffectSetParameterValuePayload, VisualEffectTargetPayload,
    };
    use mondrian_assets::AssetLibrary;
    use mondrian_core::automation::AnimationParameterAddress;
    use mondrian_core::timeline_data::{AssetMediaInterpretation, MediaColorInterpretation};
    use mondrian_core::types::{
        AssetId, AudioComponentEditId, AudioSourceComponentId, ClipLinkGroupId, EffectId,
        FramePosition, MaskId, TrackId,
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
        AudioComponentAddress, AudioComponentEditRequest, AudioComponentMutation,
        EffectRelativePlacement, InsertAutomationPolicy, InsertTimelineStatePolicy,
        InsertTransitionPolicy, TrackRelativePlacement,
    };

    fn audio_component_action(
        track_id: TrackId,
        clip_id: ClipId,
        edit_id: AudioComponentEditId,
        mutation: AudioComponentMutation,
    ) -> mondrian_editor_state::Action {
        audio_component_edit_action(AudioComponentEditRequest {
            address: AudioComponentAddress { track_id, clip_id, edit_id },
            mutation,
        })
    }

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
        state_with_two_video_tracks_at_rate(SequenceSettings::default().frame_rate)
    }

    fn state_with_two_video_tracks_at_rate(
        frame_rate: Rational,
    ) -> (
        AppState,
        mondrian_core::types::TrackId,
        mondrian_core::types::ClipId,
    ) {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("edit");
        sequence.settings.frame_rate = frame_rate;
        sequence.add_video_track();
        let tb = sequence.time_base();
        let track_id = sequence.video_tracks[0].id;
        let clip = Clip::new(AssetId::new(), tt(10, tb), tt(20, tb)).expect("valid clip");
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("add clip");
        state.test_set_sequence(Some(sequence));
        (state, track_id, clip_id)
    }

    fn audio_test_media_info() -> MediaInfo {
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
            duration: Duration::from_secs(1),
            file_size: 1,
            container: "mov".to_string(),
            video_streams: Vec::new(),
            audio_streams: vec![stream(1, 10, "eng", false), stream(3, 30, "jpn", true)],
            has_video: false,
            has_audio: true,
        }
    }

    fn commit_probed_test_media(library: &AssetLibrary, path: &std::path::Path) -> AssetId {
        let canonical_path = path.canonicalize().expect("canonical test media");
        let info = mondrian_media::probe_media_info(&canonical_path).expect("probe test media");
        let fingerprint = mondrian_core::MediaFileFingerprint::capture(&canonical_path);
        let candidate =
            mondrian_assets::AssetMediaProbeCandidate::new(canonical_path, fingerprint, info)
                .expect("test media candidate");
        library.commit_media_probe(candidate, None).expect("commit test media")
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
        let canonical_path = media_path.canonicalize().expect("canonical media fixture");
        let fingerprint = mondrian_core::MediaFileFingerprint::capture(&canonical_path);
        let candidate = mondrian_assets::AssetMediaProbeCandidate::new(
            canonical_path,
            fingerprint,
            audio_test_media_info(),
        )
        .expect("valid media candidate");
        let asset_id = library.commit_media_probe(candidate, None).expect("register audio asset");
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

    fn opacity_parameter_address(state: &AppState, clip_id: ClipId) -> AnimationParameterAddress {
        clip_parameter_address(state, clip_id, Transform2D::OPACITY_PATH)
    }

    fn clip_parameter_address(
        state: &AppState,
        clip_id: ClipId,
        path: &str,
    ) -> AnimationParameterAddress {
        let clip = state
            .active_sequence()
            .and_then(|sequence| find_clip(sequence, clip_id))
            .expect("clip");
        clip.intrinsic_parameter_bag()
            .address_for_path(path)
            .expect("Clip parameter address")
    }

    fn clip_parameter_action(
        clip_id: ClipId,
        parameter: AnimationParameterAddress,
        value: PropertyValue,
    ) -> mondrian_editor_state::Action {
        clip_write_parameter_values_action(ClipWriteParameterValuesPayload {
            clip_id,
            writes: vec![ClipParameterValueWrite { parameter, value }],
        })
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
    ) -> (EffectId, String, AnimationParameterAddress, PropertyValue) {
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
        let address = AnimationParameterAddress {
            animation_track_id: property.track_id,
            parameter_id: property.descriptor.parameter_id().clone(),
        };
        (
            effect_id,
            path.to_string(),
            address,
            property.static_value().clone(),
        )
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
    fn dispatch_undo_redo_reject_unexecuted_history_intents() {
        for (action, expected_action, expected_reason) in [
            (mondrian_editor_state::Action::Undo, "undo", "撤销历史为空"),
            (mondrian_editor_state::Action::Redo, "redo", "重做历史为空"),
        ] {
            let mut state = AppState::new();
            let error =
                state.dispatch_action(action).expect_err("empty history must reject the intent");
            match error {
                MondrianError::ActionNotExecuted { action, reason } => {
                    assert_eq!(action, expected_action);
                    assert_eq!(reason, expected_reason);
                }
                other => panic!("expected ActionNotExecuted, got {other:?}"),
            }
        }
    }

    #[test]
    fn dispatch_timeline_ui_namespace_rejects_unknown_action_names() {
        let mut state = AppState::new();
        let err = state
            .dispatch_action(mondrian_editor_state::Action::Custom {
                namespace: TIMELINE_NAMESPACE.into(),
                name: "unknown".into(),
                payload: serde_json::Value::Null,
            })
            .expect_err("registered UI namespace should reject unknown action names");

        match err {
            MondrianError::WorkflowStepFailed { step_id, reason } => {
                assert_eq!(step_id, "timeline_ui_action.unknown");
                assert!(reason.contains("unknown app UI action"));
            }
            other => panic!("expected unknown UI action workflow error, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_recognized_product_action_rejects_malformed_payload_before_legacy_routing() {
        let mut state = AppState::new();
        let error = state
            .dispatch_action(mondrian_editor_state::Action::Custom {
                namespace: TIMELINE_NAMESPACE.to_owned(),
                name: crate::app::product_action::TIMELINE_MOVE_CLIP.to_owned(),
                payload: serde_json::json!({"clip_id": ClipId::new()}),
            })
            .expect_err("recognized malformed product payload must fail closed");

        match error {
            MondrianError::WorkflowStepFailed { step_id, reason } => {
                assert_eq!(step_id, "timeline_ui_action.move_clip");
                assert!(reason.contains("recognized product action"));
            }
            other => panic!("expected product decode workflow error, got {other:?}"),
        }
    }

    #[test]
    fn migrated_product_actions_fail_closed_on_malformed_payloads() {
        for (namespace, name, expected_step) in [
            (
                crate::app::product_action::PROJECT_NAMESPACE,
                crate::app::product_action::PROJECT_CREATE_WITH_SETTINGS,
                "project_action.create_with_settings",
            ),
            (
                crate::app::product_action::SEQUENCE_NAMESPACE,
                crate::app::product_action::SEQUENCE_NEW,
                "sequence_action.new",
            ),
            (
                crate::app::product_action::EXPORT_NAMESPACE,
                crate::app::product_action::EXPORT_CLEAR_TERMINAL_HISTORY,
                "export_action.clear_terminal_history",
            ),
        ] {
            let mut state = AppState::new();
            let error = state
                .dispatch_action(mondrian_editor_state::Action::Custom {
                    namespace: namespace.to_owned(),
                    name: name.to_owned(),
                    payload: serde_json::json!({}),
                })
                .expect_err("recognized malformed product payload must fail closed");

            match error {
                MondrianError::WorkflowStepFailed { step_id, reason } => {
                    assert_eq!(step_id, expected_step);
                    assert!(reason.contains("invalid payload"));
                }
                other => panic!("expected malformed product payload failure, got {other:?}"),
            }
        }
    }

    #[test]
    fn dispatch_export_product_action_rejects_empty_output_path_without_queueing() {
        let mut state = AppState::new();
        state.test_set_sequence(Some(Sequence::new("export")));

        let err = state
            .dispatch_action(export_enqueue_action(TimelineExportRequest {
                preset: mondrian_export::preset::ExportPreset::h264_aac_sdr_1080p(),
                sequence_id: None,
                range: mondrian_export::preset::TimelineExportRange::EntireSequence,
                output_path: PathBuf::new(),
                output_policy: mondrian_export::preset::ExportOutputPolicy::CreateNew,
            }))
            .expect_err("empty output path should fail");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state.render_queue.list_jobs().is_empty());
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
    }

    #[test]
    fn dispatch_export_product_action_updates_draft_fields() {
        let mut state = AppState::new();
        let sequence = Sequence::new("Export");
        let sequence_id = sequence.id;
        state.test_set_sequence(Some(sequence));
        let builtin = mondrian_export::preset::BuiltinExportPreset::ProRes4444Alpha;
        let mut customized = builtin.preset();
        customized.video_signal.range = mondrian_export::preset::ExportParameter::FollowSequence;

        state
            .dispatch_action(export_edit_draft_action(ExportDraftEdit::BuiltinPreset(
                builtin,
            )))
            .expect("set preset");
        state
            .dispatch_action(export_edit_draft_action(ExportDraftEdit::Preset(
                customized.clone(),
            )))
            .expect("customize preset");
        state
            .dispatch_action(export_edit_draft_action(ExportDraftEdit::Sequence(Some(
                sequence_id,
            ))))
            .expect("set sequence");
        state
            .dispatch_action(export_edit_draft_action(ExportDraftEdit::Range(
                mondrian_export::preset::TimelineExportRange::EntireSequence,
            )))
            .expect("set range");
        state
            .dispatch_action(export_edit_draft_action(ExportDraftEdit::OutputPath(
                "E:/renders/out.mp4".to_owned(),
            )))
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
    fn export_draft_edit_rejects_noops_and_stale_sequence_targets() {
        let mut state = AppState::new();
        let sequence = Sequence::new("Export");
        state.test_set_sequence(Some(sequence));

        let error = state
            .dispatch_action(export_edit_draft_action(ExportDraftEdit::BuiltinPreset(
                state.export_draft.selected_builtin_preset,
            )))
            .expect_err("unchanged Export draft must not report a mutation");
        assert_action_not_executed(error, "edit_export_draft");

        let before = state.export_draft.clone();
        let error = state
            .dispatch_action(export_edit_draft_action(ExportDraftEdit::Sequence(Some(
                mondrian_core::types::SequenceId::new(),
            ))))
            .expect_err("stale Sequence target must fail closed");
        assert_action_not_executed(error, "edit_export_draft");
        assert_eq!(state.export_draft, before);
    }

    #[test]
    fn export_queue_actions_reject_stale_or_empty_targets() {
        let mut state = AppState::new();
        let job_id = mondrian_core::types::JobId::new();

        let error = state
            .dispatch_action(export_cancel_action(job_id))
            .expect_err("cancel missing job must not report success");
        assert_action_not_executed(error, "cancel_export");
        let error = state
            .dispatch_action(export_clear_terminal_history_action())
            .expect_err("empty terminal history must not report a clear");
        assert_action_not_executed(error, "clear_terminal_export_history");

        assert!(state.render_queue.list_jobs().is_empty());
    }

    #[test]
    fn dispatch_timeline_ui_selects_clip() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();

        state
            .dispatch_action(timeline_select_clip_action(TimelineSelectClipPayload {
                clip_id,
                mode: TimelineClipSelectionModePayload::Replace,
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
                clip_id,
                mode: TimelineClipSelectionModePayload::Replace,
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
    fn timeline_link_actions_are_group_aware_and_one_undo_step() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("links");
        let tb = sequence.time_base();
        let first = Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("first Clip");
        let first_id = first.id;
        let second = Clip::new(AssetId::new(), tt(10, tb), tt(10, tb)).expect("second Clip");
        let second_id = second.id;
        sequence.video_tracks[0].add_clip(first).expect("add first");
        sequence.video_tracks[1].add_clip(second).expect("add second");
        state.test_set_sequence(Some(sequence));

        state
            .dispatch_action(timeline_select_clip_action(TimelineSelectClipPayload {
                clip_id: first_id,
                mode: TimelineClipSelectionModePayload::Replace,
            }))
            .expect("select first");
        state
            .dispatch_action(timeline_select_clip_action(TimelineSelectClipPayload {
                clip_id: second_id,
                mode: TimelineClipSelectionModePayload::Toggle,
            }))
            .expect("add second");
        assert_eq!(state.selected_clips().len(), 2);

        state
            .dispatch_action(timeline_link_selected_clips_action())
            .expect("link selection");
        let linked_group = state
            .active_sequence()
            .expect("sequence")
            .video_tracks
            .iter()
            .flat_map(|track| &track.clips)
            .find(|clip| clip.id == first_id)
            .and_then(|clip| clip.link_group)
            .expect("link group");
        assert!(state
            .active_sequence()
            .expect("sequence")
            .video_tracks
            .iter()
            .flat_map(|track| &track.clips)
            .filter(|clip| clip.id == first_id || clip.id == second_id)
            .all(|clip| clip.link_group == Some(linked_group)));

        state.clear_selection();
        state
            .dispatch_action(timeline_select_clip_action(TimelineSelectClipPayload {
                clip_id: second_id,
                mode: TimelineClipSelectionModePayload::Replace,
            }))
            .expect("select linked member");
        assert_eq!(
            state
                .selected_clips()
                .iter()
                .map(|selection| selection.clip_id)
                .collect::<Vec<_>>(),
            vec![second_id, first_id]
        );

        assert!(state.undo_timeline().expect("undo link"));
        assert!(state
            .active_sequence()
            .expect("sequence")
            .video_tracks
            .iter()
            .flat_map(|track| &track.clips)
            .all(|clip| clip.link_group.is_none()));
        assert!(state.redo_timeline().expect("redo link"));
        state
            .dispatch_action(timeline_unlink_selected_clips_action())
            .expect("unlink selection");
        assert!(state
            .active_sequence()
            .expect("sequence")
            .video_tracks
            .iter()
            .flat_map(|track| &track.clips)
            .all(|clip| clip.link_group.is_none()));
    }

    #[test]
    fn dispatch_timeline_ui_seek_updates_playback_frame() {
        let (mut state, _, _) = state_with_two_video_tracks();
        let time_base = state.active_sequence().expect("sequence").time_base();

        state
            .dispatch_action(timeline_seek_with_source_action(
                FramePosition::new(33, time_base),
                TimelineSeekSource::PointerDrag,
            ))
            .expect("dispatch seek");

        assert_eq!(state.current_frame(), 33);
        assert_eq!(
            state.last_timeline_seek_source,
            TimelineSeekSource::PointerDrag
        );

        state
            .dispatch_action(timeline_seek_action(FramePosition::new(44, time_base)))
            .expect("dispatch seek");

        assert_eq!(state.current_frame(), 44);
        assert_eq!(state.last_timeline_seek_source, TimelineSeekSource::Settled);
    }

    #[test]
    fn rejected_timeline_product_seek_does_not_change_transport() {
        let (mut state, _, _) = state_with_two_video_tracks();
        let time_base = state.active_sequence().expect("sequence").time_base();
        state.seek(12).expect("initial seek");
        let before = state.playback_engine.snapshot();
        let source_before = state.last_timeline_seek_source;

        state
            .dispatch_action(timeline_seek_action(FramePosition::new(-1, time_base)))
            .expect_err("negative product seek must fail closed");

        assert_eq!(state.playback_engine.snapshot(), before);
        assert_eq!(state.last_timeline_seek_source, source_before);
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
                SequenceTargetPayload { sequence_id: child_id },
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
    fn nested_navigation_rejects_sequence_not_referenced_by_active_parent() {
        let mut state = AppState::new();
        let parent = Sequence::new("parent");
        let parent_id = parent.id;
        let unrelated = Sequence::new("unrelated");
        let unrelated_id = unrelated.id;
        state.test_set_active_sequence(parent_id);
        state.test_set_sequence(Some(parent));
        state.test_add_sequence(unrelated);

        let error = state
            .dispatch_action(timeline_open_nested_sequence_action(
                SequenceTargetPayload { sequence_id: unrelated_id },
            ))
            .expect_err("unrelated Sequence must not enter nested navigation");

        assert!(matches!(error, MondrianError::ActionNotExecuted { .. }));
        assert_eq!(state.active_sequence_id(), Some(parent_id));
        assert!(state.test_navigation_stack().is_empty());
    }

    #[test]
    fn dispatch_sequence_product_action_returns_to_parent_sequence() {
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
    fn sequence_return_to_parent_without_parent_is_not_reported_as_success() {
        let mut state = AppState::new();
        state.test_set_sequence(Some(Sequence::new("root")));

        let error = state
            .dispatch_action(sequence_return_to_parent_action())
            .expect_err("root Sequence has no parent transition to execute");

        assert!(matches!(error, MondrianError::ActionNotExecuted { .. }));
        assert!(state.test_navigation_stack().is_empty());
    }

    #[test]
    fn dispatch_sequence_product_action_sets_active_sequence_as_default() {
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
    fn dispatch_sequence_product_action_creates_new_sequence() {
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
    fn sequence_new_without_authoring_session_returns_the_real_failure() {
        let mut state = AppState::new();

        let error = state
            .dispatch_action(sequence_new_action())
            .expect_err("Sequence creation requires an Authoring Session");

        assert!(
            matches!(error, MondrianError::WorkflowStepFailed { .. }),
            "{error:?}"
        );
        assert!(state.sequences().is_empty());
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_sequence_product_action_switches_active_sequence() {
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
    fn dispatch_sequence_product_action_duplicates_sequence_and_activates_copy() {
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
    fn dispatch_sequence_product_action_deletes_sequence_and_keeps_fallback_active() {
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
    fn dispatch_sequence_product_action_updates_settings_atomically() {
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
    fn dispatch_sequence_product_action_rejects_invalid_settings_without_renaming() {
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
    fn dispatch_sequence_product_action_rejects_custom_ocio_working_space_mismatch_atomically() {
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
    fn dispatch_sequence_product_action_rejects_standard_working_space_mismatch_atomically() {
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

        state.new_sequence("Custom Working").expect("new sequence");

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
    fn dispatch_viewer_product_action_updates_preview_scale_without_stopping_playback() {
        let mut state = AppState::new();
        let sequence = Sequence::new("preview");
        let sequence_id = sequence.id;
        state.test_set_active_sequence(sequence_id);
        state.test_set_sequence(Some(sequence.clone()));
        state.test_add_sequence(sequence);
        state.play().expect("play");

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
    fn dispatch_viewer_product_action_clamps_out_of_range_preview_scale() {
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
            .dispatch_action(track_set_author_control_action(
                TrackSetAuthorControlPayload {
                    track_id: video_track_id,
                    control: TrackAuthorControl::Visibility,
                    enabled: false,
                },
            ))
            .expect("toggle visibility");
        assert!(!state.active_sequence().expect("sequence").video_tracks[0].is_visible);
        assert!(state.can_undo_action());

        state
            .dispatch_action(track_set_author_control_action(
                TrackSetAuthorControlPayload {
                    track_id: audio_track_id,
                    control: TrackAuthorControl::Mute,
                    enabled: true,
                },
            ))
            .expect("toggle mute");
        assert!(state.active_sequence().expect("sequence").audio_tracks[0].is_muted);

        state
            .dispatch_action(track_set_author_control_action(
                TrackSetAuthorControlPayload {
                    track_id: video_track_id,
                    control: TrackAuthorControl::Lock,
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
    fn track_product_actions_separate_author_history_from_session_policy_and_reject_noops() {
        let (mut state, video_track_id, _) = state_with_two_video_tracks();
        let sequence_id = state.active_sequence_id().expect("active Sequence");
        let initial_generation = state.project_author_generation();
        let initial_history = state
            .authoring_history()
            .and_then(|history| history.undo_description())
            .map(str::to_owned);

        let edit_policy = track_set_edit_policy_action(TrackSetEditPolicyPayload {
            track_id: video_track_id,
            control: TrackEditPolicyControl::Target,
            enabled: false,
        });
        state.dispatch_action(edit_policy.clone()).expect("disable Track Target");
        assert!(!state.timeline_track_targeted(sequence_id, video_track_id));
        assert_eq!(state.project_author_generation(), initial_generation);
        assert_eq!(
            state.authoring_history().and_then(|history| history.undo_description()),
            initial_history.as_deref()
        );
        assert_action_not_executed(
            state.dispatch_action(edit_policy).expect_err("repeated policy is a no-op"),
            "track_set_edit_policy",
        );
        assert_eq!(state.project_author_generation(), initial_generation);

        let author_control = track_set_author_control_action(TrackSetAuthorControlPayload {
            track_id: video_track_id,
            control: TrackAuthorControl::Visibility,
            enabled: false,
        });
        state.dispatch_action(author_control.clone()).expect("hide video Track");
        assert_eq!(state.project_author_generation(), initial_generation + 1);
        assert_eq!(
            state.authoring_history().and_then(|history| history.undo_description()),
            Some("切换轨道可见性")
        );
        assert_action_not_executed(
            state
                .dispatch_action(author_control)
                .expect_err("repeated author value is a no-op"),
            "track_set_author_control",
        );
        assert_eq!(state.project_author_generation(), initial_generation + 1);
    }

    #[test]
    fn dispatch_track_product_action_moves_track_relative_to_stable_anchor() {
        let (mut state, first_track_id, _) = state_with_two_video_tracks();
        let second_track_id = state.active_sequence().expect("sequence").video_tracks[1].id;

        let action = track_move_action(TrackMovePayload {
            track_id: second_track_id,
            placement: TrackRelativePlacement::Before(first_track_id),
        });
        state.dispatch_action(action.clone()).expect("move track");

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert_eq!(sequence.video_tracks[0].id, second_track_id);
        assert_eq!(sequence.video_tracks[1].id, first_track_id);
        assert!(state.can_undo_action());
        let generation = state.project_author_generation();
        assert_action_not_executed(
            state.dispatch_action(action).expect_err("satisfied Track relation is a no-op"),
            "track_move",
        );
        assert_eq!(state.project_author_generation(), generation);
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
        let time_base = state.active_sequence().expect("sequence").time_base();

        state
            .dispatch_action(timeline_drop_asset_action(TimelineDropAssetPayload {
                asset_id,
                target_track_id,
                position: FramePosition::new(40, time_base),
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
        let time_base = state.active_sequence().expect("sequence").time_base();

        let generation_before = state.project_author_generation();
        state
            .dispatch_action(timeline_insert_asset_action(TimelineInsertAssetPayload {
                asset_id,
                at: tt(15, time_base),
                source_in: TimelineTime::ZERO,
                duration: tt(5, time_base),
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
        let time_base = state.active_sequence().expect("sequence").time_base();

        let err = state
            .dispatch_action(timeline_drop_asset_action(TimelineDropAssetPayload {
                asset_id,
                target_track_id,
                position: FramePosition::new(12, time_base),
            }))
            .expect_err("solid color should not drop onto audio track");

        assert!(matches!(err, MondrianError::UnsupportedFormat { .. }));
        assert_eq!(
            state.active_sequence().expect("sequence").audio_tracks[0].clips.len(),
            initial_audio_clip_count
        );
        assert!(state.dragging_asset().is_none());
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));

        remove_temp_path(&library_root);
    }

    #[test]
    fn dispatch_timeline_ui_adds_video_and_audio_tracks() {
        let (mut state, _, _) = state_with_two_video_tracks();
        let initial_video_tracks = state.active_sequence().expect("sequence").video_tracks.len();
        let initial_audio_tracks = state.active_sequence().expect("sequence").audio_tracks.len();

        state
            .dispatch_action(track_add_action(TrackAddPayload {
                kind: TrackAddKind::Video,
            }))
            .expect("add video track");
        state
            .dispatch_action(track_add_action(TrackAddPayload {
                kind: TrackAddKind::Audio,
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
        let time_base = state.active_sequence().expect("sequence").time_base();
        let target_track_id = state.active_sequence().unwrap().video_tracks[1].id;
        state.selection.selected_clips = vec![SelectedClipRef {
            track_id: source_track_id,
            is_video_track: true,
            clip_id,
        }];

        state
            .dispatch_action(timeline_move_clip_action(TimelineMoveClipPayload {
                target_track_id,
                clip_id,
                position: FramePosition::new(42, time_base),
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
    fn semantic_move_converts_input_time_base_before_sequence_grid_quantization() {
        let (mut state, _, clip_id) = state_with_two_video_tracks_at_rate(Rational::FPS_2997);
        let target_track = state.active_sequence().expect("sequence").video_tracks[1].id;

        state
            .dispatch_action(mondrian_editor_state::Action::MoveClipToTrack {
                clip_id,
                target_track,
                position: FramePosition::new(24, Rational::new(1, 24)),
            })
            .expect("move exact one second onto Sequence grid");

        let sequence = state.active_sequence().expect("sequence");
        assert!(sequence.video_tracks[0].clips.is_empty());
        assert_eq!(
            sequence.video_tracks[1].clips[0].position,
            tt(30, sequence.time_base())
        );
    }

    #[test]
    fn semantic_seek_converts_input_time_base_before_sequence_grid_quantization() {
        let (mut state, _, _) = state_with_two_video_tracks_at_rate(Rational::FPS_2997);

        state
            .dispatch_action(mondrian_editor_state::Action::Seek(FramePosition::new(
                24,
                Rational::new(1, 24),
            )))
            .expect("seek exact one second onto Sequence grid");

        assert_eq!(state.current_frame(), 30);
    }

    #[test]
    fn product_timeline_gestures_lower_their_explicit_input_grid_once() {
        let input_grid = Rational::new(1, 24);

        let (mut move_state, _, move_clip_id) =
            state_with_two_video_tracks_at_rate(Rational::FPS_2997);
        let target_track = move_state.active_sequence().expect("sequence").video_tracks[1].id;
        move_state
            .dispatch_action(timeline_move_clip_action(TimelineMoveClipPayload {
                target_track_id: target_track,
                clip_id: move_clip_id,
                position: FramePosition::new(24, input_grid),
            }))
            .expect("move exact one second onto Sequence grid");
        let move_time_base = move_state.active_sequence().expect("sequence").time_base();
        assert_eq!(
            move_state.active_sequence().expect("sequence").video_tracks[1].clips[0].position,
            tt(30, move_time_base)
        );

        let (mut trim_state, _, trim_clip_id) =
            state_with_two_video_tracks_at_rate(Rational::FPS_2997);
        trim_state
            .dispatch_action(timeline_trim_clips_action(TimelineTrimClipsPayload {
                clip_ids: vec![trim_clip_id],
                edge: TimelineTrimPayloadEdge::In,
                position: FramePosition::new(16, input_grid),
            }))
            .expect("trim two-thirds of one second onto Sequence grid");
        let trim_time_base = trim_state.active_sequence().expect("sequence").time_base();
        assert_eq!(
            trim_state.active_sequence().expect("sequence").video_tracks[0].clips[0].position,
            tt(20, trim_time_base)
        );

        let (mut seek_state, _, _) = state_with_two_video_tracks_at_rate(Rational::FPS_2997);
        seek_state
            .dispatch_action(timeline_seek_action(FramePosition::new(24, input_grid)))
            .expect("seek exact one second onto Sequence grid");
        assert_eq!(seek_state.current_frame(), 30);
    }

    #[test]
    fn semantic_sequence_positions_fail_closed_for_invalid_time_bases() {
        let (mut state, source_track, clip_id) =
            state_with_two_video_tracks_at_rate(Rational::FPS_2997);
        let target_track = state.active_sequence().expect("sequence").video_tracks[1].id;
        let invalid = FramePosition::new(24, Rational::new(0, 24));

        state
            .dispatch_action(mondrian_editor_state::Action::Seek(invalid))
            .expect_err("invalid seek time base must fail");
        state
            .dispatch_action(mondrian_editor_state::Action::Seek(FramePosition::new(
                -1,
                Rational::new(1, 24),
            )))
            .expect_err("negative Sequence time must fail");
        state
            .dispatch_action(mondrian_editor_state::Action::MoveClipToTrack {
                clip_id,
                target_track,
                position: invalid,
            })
            .expect_err("invalid move time base must fail");

        assert_eq!(state.current_frame(), 0);
        let sequence = state.active_sequence().expect("sequence");
        assert_eq!(sequence.video_tracks[0].id, source_track);
        assert_eq!(sequence.video_tracks[0].clips[0].id, clip_id);
        assert!(sequence.video_tracks[1].clips.is_empty());
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_timeline_ui_rejects_cross_media_clip_move() {
        let (mut state, source_track_id, clip_id) = state_with_two_video_tracks();
        let time_base = state.active_sequence().expect("sequence").time_base();
        let target_track_id = state.active_sequence().unwrap().audio_tracks[0].id;
        state.selection.selected_clips = vec![SelectedClipRef {
            track_id: source_track_id,
            is_video_track: true,
            clip_id,
        }];

        let err = state
            .dispatch_action(timeline_move_clip_action(TimelineMoveClipPayload {
                target_track_id,
                clip_id,
                position: FramePosition::new(42, time_base),
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
        let time_base = state.active_sequence().expect("sequence").time_base();

        state
            .dispatch_action(timeline_trim_clips_action(TimelineTrimClipsPayload {
                clip_ids: vec![clip_id],
                edge: TimelineTrimPayloadEdge::In,
                position: FramePosition::new(16, time_base),
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
                position: FramePosition::new(16, tb),
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
        state.seek(18).expect("seek");

        state
            .dispatch_action(timeline_trim_selected_clips_to_playhead_action(
                TimelineTrimPayloadEdge::In,
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
        state.seek(35).expect("seek");

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
        assert_eq!(second.source_origin(), tt(5, tb));
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
            .dispatch_action(timeline_set_selected_clips_enabled_action(false))
            .expect("disable selection");

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert!(sequence.video_tracks[0].clips[0].is_disabled);
        assert!(sequence.video_tracks[1].clips[0].is_disabled);

        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[1].is_locked = true;
        let result = state.dispatch_action(timeline_set_selected_clips_enabled_action(true));

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
        assert_eq!(clip.source_origin(), tt(5, tb));
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_trim_clip_start_lowers_24fps_source_time_on_2997_sequence_grid() {
        let (mut state, _, clip_id) = state_with_two_video_tracks_at_rate(Rational::FPS_2997);

        state
            .dispatch_action(mondrian_editor_state::Action::TrimClipStart {
                clip_id,
                new_source_in: FramePosition::new(1, Rational::new(1, 24)),
            })
            .expect("trim source in across frame grids");

        let sequence = state.active_sequence().expect("sequence");
        let sequence_time_base = sequence.time_base();
        let clip = &sequence.video_tracks[0].clips[0];
        assert_eq!(clip.position, tt(11, sequence_time_base));
        assert_eq!(clip.duration, tt(19, sequence_time_base));
        assert_eq!(clip.source_origin(), tt(1, sequence_time_base));
    }

    #[test]
    fn dispatch_trim_clip_start_lowers_2997_source_time_on_24fps_sequence_grid() {
        let (mut state, _, clip_id) = state_with_two_video_tracks_at_rate(Rational::FPS_24);

        state
            .dispatch_action(mondrian_editor_state::Action::TrimClipStart {
                clip_id,
                new_source_in: FramePosition::new(15, Rational::new(1_001, 30_000)),
            })
            .expect("trim source in across frame grids");

        let sequence = state.active_sequence().expect("sequence");
        let sequence_time_base = sequence.time_base();
        let clip = &sequence.video_tracks[0].clips[0];
        assert_eq!(clip.position, tt(22, sequence_time_base));
        assert_eq!(clip.duration, tt(8, sequence_time_base));
        assert_eq!(clip.source_origin(), tt(12, sequence_time_base));
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
        assert_eq!(
            clip.source_terminal_boundary().expect("source terminal"),
            tt(12, tb)
        );
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
    fn dispatch_source_trim_fails_closed_for_invalid_source_time_base() {
        let (mut state, _, clip_id) = state_with_two_video_tracks_at_rate(Rational::FPS_2997);

        state
            .dispatch_action(mondrian_editor_state::Action::TrimClipStart {
                clip_id,
                new_source_in: FramePosition::new(1, Rational::new(0, 24)),
            })
            .expect_err("invalid source coordinate must fail");

        let sequence = state.active_sequence().expect("sequence");
        let time_base = sequence.time_base();
        let clip = &sequence.video_tracks[0].clips[0];
        assert_eq!(clip.position, tt(10, time_base));
        assert_eq!(clip.duration, tt(20, time_base));
        assert_eq!(clip.source_origin(), TimelineTime::ZERO);
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_import_media_rejects_empty_paths_before_library_lookup() {
        let mut state = AppState::new();

        let error = state
            .dispatch_action(mondrian_editor_state::Action::ImportMedia(Vec::new()))
            .expect_err("empty import is not an executable intent");

        assert_action_not_executed(error, "import_media");
        assert!(state.status_hint.is_none());
    }

    #[test]
    fn dispatch_asset_batch_actions_reject_empty_inputs_before_library_lookup() {
        let cases = [
            (
                assets_import_files_action(AssetsImportFilesPayload {
                    paths: Vec::new(),
                    folder_id: None,
                }),
                "import_media",
            ),
            (
                assets_delete_selection_action(AssetsDeleteSelectionPayload {
                    asset_ids: Vec::new(),
                    folder_ids: Vec::new(),
                }),
                "remove_asset_library_entries",
            ),
            (
                assets_move_selection_action(AssetsMoveSelectionPayload {
                    asset_ids: Vec::new(),
                    folder_ids: Vec::new(),
                    target_folder_id: None,
                }),
                "move_asset_library_entries",
            ),
        ];

        for (action, expected_action) in cases {
            let mut state = AppState::new();
            let error = state
                .dispatch_action(action)
                .expect_err("an empty batch is not an executable intent");
            assert_action_not_executed(error, expected_action);
        }
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
    fn dispatch_assets_delete_asset_retires_membership_without_mutating_author_state() {
        let (mut state, _, _) = state_with_two_video_tracks();
        let library_root = unique_temp_path("assets-delete-action-library");
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let asset_id = library.create_solid_color_asset(Some("Temp Plate")).expect("create asset");
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].clips[0]
            .content =
            mondrian_core::timeline_data::ClipContent::SolidColor { asset_id, color: Color::BLACK };
        state.test_set_asset_library(Some(library));
        state.set_asset_proxy_mode(asset_id, true);
        let sequence_before = state.active_sequence().expect("sequence").clone();
        let events = state.event_bus.subscribe();

        state
            .dispatch_action(assets_delete_asset_action(AssetsDeleteAssetPayload {
                asset_id,
            }))
            .expect("delete asset");

        let record = state
            .asset_library()
            .expect("library")
            .get_asset(asset_id)
            .expect("get asset")
            .expect("retained strong record");
        assert!(record.membership.is_retired());
        assert!(state
            .asset_library()
            .expect("library")
            .list_assets()
            .expect("visible")
            .is_empty());
        assert_eq!(state.active_sequence().expect("sequence"), &sequence_before);
        assert!(state.is_asset_proxy_mode(asset_id));
        assert!(state.can_undo_action());
        assert!(state
            .status_hint
            .as_ref()
            .is_some_and(|(message, is_error)| !*is_error && message.contains("Temp Plate")));
        let mut saw_asset_retired = false;
        while let Ok(event) = events.try_recv() {
            if matches!(event, AppEvent::AssetRetired { asset_id: event_asset_id } if event_asset_id == asset_id)
            {
                saw_asset_retired = true;
            }
        }
        assert!(saw_asset_retired);

        state
            .dispatch_action(mondrian_editor_state::Action::Undo)
            .expect("Undo proxy intent");
        assert!(!state.is_asset_proxy_mode(asset_id));
        assert!(state
            .asset_library()
            .expect("library")
            .get_asset(asset_id)
            .expect("lookup after Undo")
            .expect("record after Undo")
            .membership
            .is_retired());
        state
            .dispatch_action(mondrian_editor_state::Action::Redo)
            .expect("Redo proxy intent");
        assert!(state.is_asset_proxy_mode(asset_id));
        assert!(state
            .asset_library()
            .expect("library")
            .get_asset(asset_id)
            .expect("lookup after Redo")
            .expect("record after Redo")
            .membership
            .is_retired());

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
    fn dispatch_assets_relink_rejects_generated_asset_without_admitting_probe() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-relink-generated-library");
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let asset_id =
            library.create_solid_color_asset(Some("Generated")).expect("generated asset");
        state.test_set_asset_library(Some(library));

        let error = state
            .dispatch_action(assets_relink_asset_action(AssetsRelinkAssetPayload {
                asset_id,
                path: PathBuf::from("replacement.mov"),
            }))
            .expect_err("generated Asset cannot be relinked");

        assert!(matches!(
            error,
            MondrianError::WorkflowStepFailed { step_id, .. } if step_id == "relink_asset"
        ));
        assert_eq!(state.media_asset_mutation_diagnostics().outstanding, 0);

        remove_temp_path(&library_root);
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
        let asset_id = commit_probed_test_media(&library, &original_path);
        state.test_set_asset_library(Some(library));
        let events = state.event_bus.subscribe();

        state
            .dispatch_action(assets_relink_asset_action(AssetsRelinkAssetPayload {
                asset_id,
                path: replacement_path.clone(),
            }))
            .expect("relink asset");
        poll_media_asset_mutations_until_idle(&mut state);

        let asset = state
            .asset_library()
            .expect("library")
            .get_asset(asset_id)
            .expect("get asset")
            .expect("asset");
        let stored_path = asset.file_path().expect("file-backed Asset path");
        assert_eq!(
            stored_path.canonicalize().expect("canonical stored path"),
            replacement_path.canonicalize().expect("canonical replacement path")
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
    fn audio_component_refresh_and_rebind_commit_only_after_background_probe() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-audio-mutation-library");
        let media_root = unique_temp_path("assets-audio-mutation-media");
        std::fs::create_dir_all(&media_root).expect("media root");
        let media_path = media_root.join("audio.wav");
        write_minimal_wav(&media_path);
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let asset_id = commit_probed_test_media(&library, &media_path);
        let asset = library.get_asset(asset_id).expect("query").expect("asset");
        let component =
            asset.audio_components.components.first().expect("primary Component").clone();
        state.test_set_asset_library(Some(library));
        let events = state.event_bus.subscribe();

        state
            .dispatch_action(assets_refresh_audio_components_action(
                AssetsRefreshAudioComponentsPayload { asset_id },
            ))
            .expect("admit refresh");
        assert_eq!(state.media_asset_mutation_diagnostics().outstanding, 1);
        poll_media_asset_mutations_until_idle(&mut state);

        state
            .dispatch_action(assets_rebind_audio_component_action(
                AssetsRebindAudioComponentPayload {
                    asset_id,
                    component_id: component.id,
                    stream_index: component.binding.stream_index,
                },
            ))
            .expect("admit rebind");
        assert_eq!(state.media_asset_mutation_diagnostics().outstanding, 1);
        poll_media_asset_mutations_until_idle(&mut state);

        let diagnostics = state.media_asset_mutation_diagnostics();
        assert_eq!(diagnostics.outstanding, 0);
        assert_eq!(diagnostics.terminals.len(), 2);
        assert!(diagnostics.terminals.iter().all(|terminal| {
            terminal.evidence.disposition == mondrian_core::ExecutionTerminalDisposition::Completed
        }));
        assert!(state
            .status_hint
            .as_ref()
            .is_some_and(|(message, is_error)| { !*is_error && message.contains("映射到流") }));
        assert_eq!(
            events
                .try_iter()
                .filter(|event| matches!(event, AppEvent::AssetLibraryReloaded))
                .count(),
            2
        );

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
        let asset_id = commit_probed_test_media(&library, &media_path);
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
    fn dispatch_assets_delete_selection_is_one_library_transaction_and_keeps_timeline_refs() {
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
        assert!(library
            .get_asset(asset_id)
            .expect("get retired")
            .expect("strong record")
            .membership
            .is_retired());
        assert!(library
            .list_assets()
            .expect("visible Assets")
            .iter()
            .all(|asset| asset.id != asset_id));
        assert!(library.get_asset(keep_asset_id).expect("get keep").is_some());
        assert!(!library.folder_exists(&folder_id).expect("folder removed"));
        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        assert!(sequence.video_tracks[0]
            .clips
            .iter()
            .any(|clip| clip.library_asset_id() == Some(asset_id)));
        assert!(sequence.video_tracks[0]
            .clips
            .iter()
            .any(|clip| clip.library_asset_id() == Some(keep_asset_id)));
        let events: Vec<AppEvent> = events.try_iter().collect();
        assert!(events.iter().any(
            |event| matches!(event, AppEvent::AssetRetired { asset_id: event_id } if *event_id == asset_id)
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
    fn dispatch_assets_delete_selection_failure_publishes_no_partial_membership_or_events() {
        let mut state = AppState::new();
        let library_root = unique_temp_path("assets-delete-selection-atomic-library");
        let library = AssetLibrary::open(library_root.clone()).expect("library");
        let asset_id = library.create_solid_color_asset(Some("Keep")).expect("create asset");
        state.test_set_asset_library(Some(library));
        let events = state.event_bus.subscribe();

        let error = state
            .dispatch_action(assets_delete_selection_action(
                AssetsDeleteSelectionPayload {
                    asset_ids: vec![asset_id],
                    folder_ids: vec!["missing-folder".to_owned()],
                },
            ))
            .expect_err("invalid batch must fail");

        assert!(matches!(error, MondrianError::WorkflowStepFailed { .. }));
        let library = state.asset_library().expect("library");
        assert!(!library
            .get_asset(asset_id)
            .expect("asset")
            .expect("record")
            .membership
            .is_retired());
        assert_eq!(library.list_assets().expect("visible records").len(), 1);
        let events = events.try_iter().collect::<Vec<_>>();
        assert!(!events.iter().any(|event| {
            matches!(
                event,
                AppEvent::AssetRetired { asset_id: retired } if *retired == asset_id
            )
        }));
        assert!(!events.iter().any(|event| matches!(event, AppEvent::AssetLibraryReloaded)));

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
    fn dispatch_open_project_rejects_empty_path() {
        let mut state = AppState::new();

        let error = state
            .dispatch_action(mondrian_editor_state::Action::OpenProject(PathBuf::new()))
            .expect_err("empty open path is not an executable intent");

        assert_action_not_executed(error, "open_project");
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
    fn dispatch_save_project_as_rejects_empty_target_before_project_lookup() {
        let mut state = AppState::new();

        let error = state
            .dispatch_action(mondrian_editor_state::Action::SaveProjectAs(PathBuf::new()))
            .expect_err("empty save target is not an executable intent");

        assert_action_not_executed(error, "save_project_as");
        assert!(state.status_hint.is_none());
    }

    #[test]
    fn dispatch_project_recovery_reports_missing_autosave() {
        let mut state = AppState::new();
        let root = unique_temp_path("recover-missing-autosave");
        let project_file = root.join("cut.mdp");
        let autosave_file = root.join("autosave").join("missing.mdp");

        let err = state
            .dispatch_action(project_recover_from_autosave_action(
                ProjectRecoverFromAutosavePayload {
                    candidate: crate::app::CrashRecoveryCandidate {
                        project_id: mondrian_core::ProjectId::new(),
                        runtime_root: root.clone(),
                        project_file,
                        canonical_target: crate::app::RecoveryCanonicalTargetEvidence::Missing,
                        autosave_file,
                        author_generation: 1,
                        asset_library_revision: 0,
                        document_revision: 1,
                        archive_sha256: "0".repeat(64),
                        saved_at_unix_ms: 0,
                        total_snapshots: 1,
                    },
                },
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
    fn dispatch_project_create_with_settings_rejects_empty_target() {
        let mut state = AppState::new();
        let error = state
            .dispatch_action(project_create_with_settings_action(
                ProjectCreateWithSettingsPayload {
                    project_file: PathBuf::new(),
                    name: "Invalid".into(),
                    sequence_settings: SequenceSettings::default(),
                    color_environment: mondrian_core::ProjectColorEnvironment::default(),
                    project_settings: ProjectSettings::default(),
                },
            ))
            .expect_err("empty project target is not an executable intent");

        assert_action_not_executed(error, "create_project");
        assert!(state.active_sequence().is_none());
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
        state.new_sequence("Created From Defaults").expect("new sequence");
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
        state.seek(42).expect("seek");
        state
            .dispatch_action(mondrian_editor_state::Action::MarkInAtPlayhead)
            .expect("mark in");
        state.seek(16).expect("seek");
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
        let time_base = state.active_sequence().expect("sequence").time_base();
        state
            .dispatch_action(timeline_set_in_out_point_action(
                TimelineSetInOutPointPayload {
                    point: TimelineInOutPointKind::In,
                    position: FramePosition::new(32, time_base),
                },
            ))
            .expect("set in point");
        state
            .dispatch_action(timeline_set_in_out_point_action(
                TimelineSetInOutPointPayload {
                    point: TimelineInOutPointKind::Out,
                    position: FramePosition::new(16, time_base),
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
    fn dispatch_timeline_selection_actions_reject_missing_selection() {
        let actions = [
            (
                mondrian_editor_state::Action::DeleteSelection,
                "delete_selection",
            ),
            (
                mondrian_editor_state::Action::RippleDeleteSelection,
                "delete_selection",
            ),
            (
                timeline_trim_selected_clips_to_playhead_action(TimelineTrimPayloadEdge::In),
                "timeline_edit_selection",
            ),
            (
                timeline_roll_selected_cut_to_playhead_action(),
                "timeline_edit_selection",
            ),
            (
                timeline_set_selected_clips_enabled_action(false),
                "timeline_edit_selection",
            ),
        ];

        for (action, expected_action) in actions {
            let mut state = AppState::new();
            state.test_set_sequence(Some(Sequence::new("empty selection")));
            let error = state
                .dispatch_action(action)
                .expect_err("selection command requires a concrete target");
            assert_action_not_executed(error, expected_action);
            assert!(!state.can_undo_action());
        }
    }

    #[test]
    fn dispatch_split_clip_at_playhead_splits_intersecting_clip() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        state.seek(20).expect("seek");

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
    fn dispatch_split_clip_at_playhead_rejects_clip_boundary() {
        let (mut state, _, _) = state_with_two_video_tracks();
        state.seek(10).expect("seek");

        let error = state
            .dispatch_action(mondrian_editor_state::Action::SplitClipAtPlayhead)
            .expect_err("clip boundary has no splittable target");

        assert_action_not_executed(error, "split_clip_at_playhead");
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
    fn dispatch_nudge_clip_rejects_zero_delta() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();

        let error = state
            .dispatch_action(mondrian_editor_state::Action::NudgeClip { clip_id, delta_frames: 0 })
            .expect_err("zero delta is not an executable nudge");

        assert_action_not_executed(error, "nudge_clip");
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
        let (mut state, _, clip_id) = state_with_two_video_tracks();

        state
            .dispatch_action(clip_set_enabled_action(ClipSetEnabledPayload {
                clip_id,
                enabled: false,
            }))
            .expect("dispatch enabled");

        let clip = &state.active_sequence().expect("sequence").video_tracks[0].clips[0];
        assert!(clip.is_disabled);
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_inspector_ui_sets_clip_opacity() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let opacity = opacity_parameter_address(&state, clip_id);

        state
            .dispatch_action(clip_parameter_action(
                clip_id,
                opacity,
                PropertyValue::Float(0.42),
            ))
            .expect("dispatch opacity");

        let sequence = state.active_sequence().expect("sequence");
        let _tb = sequence.time_base();
        let clip = &sequence.video_tracks[0].clips[0];
        assert!((clip.transform.evaluate_opacity(sequence.playhead) - 0.42).abs() < 1.0e-6);
        assert!(state.can_undo_action());
    }

    #[test]
    fn clip_value_write_updates_authoritative_key_without_hidden_static_write() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let playhead = state.active_sequence().expect("sequence").playhead;
        let keyframe =
            mondrian_core::automation::Keyframe::linear(playhead, PropertyValue::Float(0.25));
        let keyframe_id = keyframe.id;
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].clips[0]
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: Transform2D::OPACITY_PATH.to_owned(),
                keyframe,
            })
            .expect("seed opacity key");
        let opacity = opacity_parameter_address(&state, clip_id);

        state
            .dispatch_action(clip_parameter_action(
                clip_id,
                opacity,
                PropertyValue::Float(0.42),
            ))
            .expect("write animated opacity");

        let property = state.active_sequence().expect("sequence").video_tracks[0].clips[0]
            .transform
            .to_property_bag()
            .property(Transform2D::OPACITY_PATH)
            .cloned()
            .expect("opacity property");
        let edited = property.keyframe_by_id(keyframe_id).expect("same key identity");
        assert_eq!(edited.value, PropertyValue::Float(0.42));
        assert_eq!(property.static_value(), &PropertyValue::Float(1.0));
    }

    #[test]
    fn clip_parameter_batch_rejects_stale_member_without_partial_commit() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let position = clip_parameter_address(&state, clip_id, Transform2D::POSITION_PATH);
        let stale = AnimationParameterAddress {
            animation_track_id: mondrian_core::AnimationTrackId::new(),
            parameter_id: mondrian_core::ParameterId::new_static("transform.opacity"),
        };
        let before = state.active_sequence().expect("sequence").clone();

        let error = state
            .dispatch_action(clip_write_parameter_values_action(
                ClipWriteParameterValuesPayload {
                    clip_id,
                    writes: vec![
                        ClipParameterValueWrite {
                            parameter: position,
                            value: PropertyValue::Vec2(glam::Vec2::new(20.0, 10.0)),
                        },
                        ClipParameterValueWrite {
                            parameter: stale,
                            value: PropertyValue::Float(0.5),
                        },
                    ],
                },
            ))
            .expect_err("stale batch member must fail closed");

        assert!(matches!(error, MondrianError::WorkflowStepFailed { .. }));
        assert_eq!(state.active_sequence().expect("sequence"), &before);
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_audio_component_source_selection_is_validated_and_undoable() {
        let (root, mut state, track_id, clip_id, edit_id, alternate_component) =
            state_with_audio_asset();
        let generation_before = state.project_author_generation();

        state
            .dispatch_action(audio_component_action(
                track_id,
                clip_id,
                edit_id,
                AudioComponentMutation::SetSource {
                    value: AudioComponentSource::Media { component_id: alternate_component },
                },
            ))
            .expect("select alternate Component");

        assert_eq!(
            state.active_sequence().expect("sequence").audio_tracks[0].clips[0].audio_components[0]
                .source,
            AudioComponentSource::Media { component_id: alternate_component }
        );
        assert_eq!(state.project_author_generation(), generation_before + 1);
        assert!(state.can_undo_action());
        assert!(state.undo_timeline().expect("undo source selection"));
        assert_eq!(
            state.active_sequence().expect("sequence").audio_tracks[0].clips[0].audio_components[0]
                .source,
            AudioComponentSource::Media { component_id: AudioSourceComponentId::primary() }
        );
        assert!(state.redo_timeline().expect("redo source selection"));
        assert_eq!(
            state.active_sequence().expect("sequence").audio_tracks[0].clips[0].audio_components[0]
                .source,
            AudioComponentSource::Media { component_id: alternate_component }
        );
        drop(state);
        remove_temp_path(&root);
    }

    #[test]
    fn dispatch_audio_component_source_rejects_locked_track_without_history() {
        let (root, mut state, track_id, clip_id, edit_id, alternate_component) =
            state_with_audio_asset();
        let mut locked = state.active_sequence().expect("sequence").clone();
        locked.audio_tracks[0].is_locked = true;
        state.test_set_sequence(Some(locked));
        let generation_before = state.project_author_generation();
        let history_before = state
            .authoring_history()
            .and_then(|history| history.undo_description())
            .map(str::to_owned);

        let error = state
            .dispatch_action(audio_component_action(
                track_id,
                clip_id,
                edit_id,
                AudioComponentMutation::SetSource {
                    value: AudioComponentSource::Media { component_id: alternate_component },
                },
            ))
            .expect_err("locked Track must reject source selection");

        assert!(matches!(error, MondrianError::TrackLocked { .. }));
        assert_eq!(state.project_author_generation(), generation_before);
        assert_eq!(
            state.authoring_history().and_then(|history| history.undo_description()),
            history_before.as_deref()
        );
        assert_eq!(
            state.active_sequence().expect("sequence").audio_tracks[0].clips[0].audio_components[0]
                .source,
            AudioComponentSource::Media { component_id: AudioSourceComponentId::primary() }
        );
        drop(state);
        remove_temp_path(&root);
    }

    #[test]
    fn dispatch_audio_component_same_source_is_a_noop_without_catalog_evidence() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("audio source no-op");
        let track_id = sequence.audio_tracks[0].id;
        let clip = Clip::new(
            AssetId::new(),
            TimelineTime::ZERO,
            tt(25, sequence.time_base()),
        )
        .expect("audio Clip");
        let clip_id = clip.id;
        sequence
            .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
            .expect("add audio Clip");
        let edit_id = sequence.audio_tracks[0].clips[0].audio_components[0].id;
        state.test_set_sequence(Some(sequence));
        let generation_before = state.project_author_generation();

        let error = state
            .dispatch_action(audio_component_action(
                track_id,
                clip_id,
                edit_id,
                AudioComponentMutation::SetSource {
                    value: AudioComponentSource::Media {
                        component_id: AudioSourceComponentId::primary(),
                    },
                },
            ))
            .expect_err("same source must not report execution");

        assert!(matches!(error, MondrianError::ActionNotExecuted { .. }));
        assert_eq!(state.project_author_generation(), generation_before);
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_audio_component_source_rejects_unknown_component_without_mutation() {
        let (root, mut state, track_id, clip_id, edit_id, _) = state_with_audio_asset();
        let before = serde_json::to_vec(state.active_sequence().expect("sequence"))
            .expect("serialize before Sequence");

        let error = state
            .dispatch_action(audio_component_action(
                track_id,
                clip_id,
                edit_id,
                AudioComponentMutation::SetSource {
                    value: AudioComponentSource::Media {
                        component_id: AudioSourceComponentId::new(),
                    },
                },
            ))
            .expect_err("unknown Component must fail closed");

        assert!(matches!(
            error,
            MondrianError::WorkflowStepFailed { step_id, .. }
                if step_id == "audio_edit_component"
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
        let fade_in = AudioFade {
            duration: TimelineTime::new(1, 4).expect("fade in duration"),
            curve: AudioFadeCurve::EqualPower,
        };
        let fade_out = AudioFade {
            duration: TimelineTime::new(1, 8).expect("fade out duration"),
            curve: AudioFadeCurve::ConstantGain,
        };
        let matrix = mondrian_core::AudioChannelMixMatrix::new(
            mondrian_core::AudioChannelLayout::Stereo,
            mondrian_core::AudioChannelLayout::Stereo,
            [
                mondrian_core::AudioChannelMixEntry::new(0, 1, 1.0).expect("right from left"),
                mondrian_core::AudioChannelMixEntry::new(1, 0, 1.0).expect("left from right"),
            ],
        )
        .expect("stereo swap");
        for mutation in [
            AudioComponentMutation::SetEnabled { value: false },
            AudioComponentMutation::SetVolumeDb { value: -7.5 },
            AudioComponentMutation::SetPan { value: 0.25 },
            AudioComponentMutation::SetFadeIn { value: Some(fade_in) },
            AudioComponentMutation::SetFadeOut { value: Some(fade_out) },
            AudioComponentMutation::SetChannelMapping {
                value: mondrian_timeline::audio::AudioComponentChannelMapping::Explicit(
                    matrix.clone(),
                ),
            },
        ] {
            state
                .dispatch_action(audio_component_action(track_id, clip_id, edit_id, mutation))
                .expect("valid typed audio Component mutation");
        }

        let edit = &state.active_sequence().expect("sequence").audio_tracks[0].clips[0]
            .audio_components[0];
        assert!(!edit.enabled);
        assert_eq!(edit.volume_db, -7.5);
        assert_eq!(edit.pan, 0.25);
        assert_eq!(edit.fades.fade_in, Some(fade_in));
        assert_eq!(edit.fades.fade_out, Some(fade_out));
        assert_eq!(
            edit.channel_mapping,
            mondrian_timeline::audio::AudioComponentChannelMapping::Explicit(matrix.clone())
        );
        assert!(state.can_undo_action());

        state
            .dispatch_action(audio_component_action(
                track_id,
                clip_id,
                edit_id,
                AudioComponentMutation::SetExplicitChannelMixGain {
                    expected_source_layout: mondrian_core::AudioChannelLayout::Stereo,
                    expected_destination_layout: mondrian_core::AudioChannelLayout::Stereo,
                    source_channel: 0,
                    destination_channel: 1,
                    gain: 0.5,
                },
            ))
            .expect("guarded coefficient edit");
        let edit = &state.active_sequence().expect("sequence").audio_tracks[0].clips[0]
            .audio_components[0];
        let mondrian_timeline::audio::AudioComponentChannelMapping::Explicit(updated) =
            &edit.channel_mapping
        else {
            panic!("expected explicit matrix");
        };
        assert_eq!(updated.entries()[1].gain().get(), 0.5);

        assert!(state.undo_timeline().expect("undo coefficient"));
        assert_eq!(
            state.active_sequence().expect("sequence").audio_tracks[0].clips[0].audio_components[0]
                .channel_mapping,
            mondrian_timeline::audio::AudioComponentChannelMapping::Explicit(matrix)
        );
        assert!(state.undo_timeline().expect("undo explicit matrix"));
        assert_eq!(
            state.active_sequence().expect("sequence").audio_tracks[0].clips[0].audio_components[0]
                .channel_mapping,
            mondrian_timeline::audio::AudioComponentChannelMapping::Standard
        );
        assert!(state.undo_timeline().expect("undo fade out"));
        let edit = &state.active_sequence().expect("sequence").audio_tracks[0].clips[0]
            .audio_components[0];
        assert_eq!(edit.fades.fade_in, Some(fade_in));
        assert_eq!(edit.fades.fade_out, None);
        drop(state);
        remove_temp_path(&root);
    }

    #[test]
    fn dispatch_audio_component_noop_is_not_reported_as_executed() {
        let (root, mut state, track_id, clip_id, edit_id, _) = state_with_audio_asset();
        let error = state
            .dispatch_action(audio_component_action(
                track_id,
                clip_id,
                edit_id,
                AudioComponentMutation::SetVolumeDb { value: 0.0 },
            ))
            .expect_err("no-op audio mutation must not report success");

        assert!(matches!(error, MondrianError::ActionNotExecuted { .. }));
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
            .dispatch_action(audio_component_action(
                track_id,
                clip_id,
                edit_id,
                AudioComponentMutation::SetFadeIn {
                    value: Some(AudioFade {
                        duration: TimelineTime::new(100, 1).expect("oversized fade"),
                        curve: AudioFadeCurve::EqualPower,
                    }),
                },
            ))
            .expect_err("fade beyond Clip must fail closed");

        assert!(matches!(
            error,
            MondrianError::WorkflowStepFailed { step_id, .. }
                if step_id == "audio_edit_component"
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
        for mutation in [
            AudioComponentMutation::SetVolumeDb { value: -3.0 },
            AudioComponentMutation::SetPan { value: -0.5 },
            AudioComponentMutation::SetFadeIn { value: Some(fade) },
        ] {
            state
                .dispatch_action(audio_component_action(track_id, clip_id, edit_id, mutation))
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
    fn dispatch_audio_component_nested_source_accepts_only_child_public_output() {
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
            .dispatch_action(audio_component_action(
                track_id,
                clip_id,
                edit_id,
                AudioComponentMutation::SetSource {
                    value: AudioComponentSource::NestedOutput { output_id: alternate_output },
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

        let before = state.active_sequence().expect("parent").clone();
        let error = state
            .dispatch_action(audio_component_action(
                track_id,
                clip_id,
                edit_id,
                AudioComponentMutation::SetSource {
                    value: AudioComponentSource::NestedOutput {
                        output_id: mondrian_core::ProgramOutputId::new(),
                    },
                },
            ))
            .expect_err("unknown child output must fail closed");
        assert!(matches!(
            error,
            MondrianError::WorkflowStepFailed { step_id, .. }
                if step_id == "audio_edit_component"
        ));
        assert_eq!(state.active_sequence().expect("parent"), &before);
    }

    #[test]
    fn dispatch_inspector_ui_sets_clip_tint_color() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("solid color inspector");
        let tb = sequence.time_base();
        let clip = Clip::new_solid_color(AssetId::new(), Color::BLACK, tt(10, tb), tt(20, tb))
            .expect("solid color Clip");
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("add solid color Clip");
        state.test_set_sequence(Some(sequence));
        let color = Color::from_rgba8(8, 144, 220, 192);

        state
            .dispatch_action(clip_set_solid_color_action(ClipSetSolidColorPayload {
                clip_id,
                color,
            }))
            .expect("dispatch tint");

        let clip = &state.active_sequence().expect("sequence").video_tracks[0].clips[0];
        assert_eq!(clip.content.solid_color(), Some(color));
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_clip_parameter_action_sets_transform_atomically() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let position = clip_parameter_address(&state, clip_id, Transform2D::POSITION_PATH);
        let scale = clip_parameter_address(&state, clip_id, Transform2D::SCALE_PATH);
        let rotation = clip_parameter_address(&state, clip_id, Transform2D::ROTATION_PATH);

        state
            .dispatch_action(clip_write_parameter_values_action(
                ClipWriteParameterValuesPayload {
                    clip_id,
                    writes: vec![
                        ClipParameterValueWrite {
                            parameter: position,
                            value: PropertyValue::Vec2(glam::Vec2::new(128.0, 72.0)),
                        },
                        ClipParameterValueWrite {
                            parameter: scale,
                            value: PropertyValue::Vec2(glam::Vec2::splat(1.5)),
                        },
                        ClipParameterValueWrite {
                            parameter: rotation,
                            value: PropertyValue::Float(-12.5),
                        },
                    ],
                },
            ))
            .expect("dispatch atomic transform");

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

        assert!(state.undo_timeline().expect("undo atomic transform"));
        let sequence = state.active_sequence().expect("sequence after undo");
        let clip = &sequence.video_tracks[0].clips[0];
        assert_eq!(
            clip.transform.get_position(sequence.playhead),
            glam::Vec2::ZERO
        );
        assert_eq!(clip.transform.get_scale(sequence.playhead), glam::Vec2::ONE);
    }

    #[test]
    fn empty_clip_parameter_write_fails_before_author_state_changes() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let before = state.active_sequence().expect("sequence").clone();

        let error = state
            .dispatch_product_action(ProductAction::Clip(
                ClipProductAction::WriteParameterValues(Box::new(
                    ClipWriteParameterValuesPayload { clip_id, writes: Vec::new() },
                )),
            ))
            .expect_err("empty Clip parameter gesture is not a product mutation");

        assert!(matches!(error, MondrianError::WorkflowStepFailed { .. }));
        assert_eq!(state.active_sequence().expect("sequence"), &before);
        assert!(!state.can_undo_action());
    }

    #[test]
    fn clip_parameter_write_with_non_finite_value_fails_before_author_state_changes() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let position = clip_parameter_address(&state, clip_id, Transform2D::POSITION_PATH);
        let before = state.active_sequence().expect("sequence").clone();

        let error = state
            .dispatch_product_action(ProductAction::Clip(
                ClipProductAction::WriteParameterValues(Box::new(
                    ClipWriteParameterValuesPayload {
                        clip_id,
                        writes: vec![ClipParameterValueWrite {
                            parameter: position,
                            value: PropertyValue::Vec2(glam::Vec2::new(f32::NAN, 0.0)),
                        }],
                    },
                )),
            ))
            .expect_err("non-finite Clip gesture is not an author mutation");

        assert!(matches!(error, MondrianError::WorkflowStepFailed { .. }));
        assert_eq!(state.active_sequence().expect("sequence"), &before);
        assert!(!state.can_undo_action());
    }

    #[test]
    fn clip_visual_parameter_write_rejects_audio_track_clip_before_author_state_changes() {
        let (mut state, _, _) = state_with_two_video_tracks();
        let time_base = state.active_sequence().expect("sequence").time_base();
        let audio_clip =
            Clip::new(AssetId::new(), TimelineTime::ZERO, tt(10, time_base)).expect("audio clip");
        let audio_clip_id = audio_clip.id;
        let position = audio_clip
            .intrinsic_parameter_bag()
            .address_for_path(Transform2D::POSITION_PATH)
            .expect("position parameter");
        state.active_sequence_mut_uncommitted().expect("sequence").audio_tracks[0]
            .add_clip(audio_clip)
            .expect("add audio clip");
        let before = state.active_sequence().expect("sequence").clone();

        let error = state
            .dispatch_product_action(ProductAction::Clip(
                ClipProductAction::WriteParameterValues(Box::new(
                    ClipWriteParameterValuesPayload {
                        clip_id: audio_clip_id,
                        writes: vec![ClipParameterValueWrite {
                            parameter: position,
                            value: PropertyValue::Vec2(glam::Vec2::new(10.0, 20.0)),
                        }],
                    },
                )),
            ))
            .expect_err("visual Clip parameters require a video Track Clip");

        assert!(matches!(error, MondrianError::WorkflowStepFailed { .. }));
        assert_eq!(state.active_sequence().expect("sequence"), &before);
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_inspector_ui_maps_incremental_curve_edits_to_opacity_keyframes() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let property = opacity_parameter_address(&state, clip_id);

        for point in [
            ClipNormalizedCurvePointPayload { time_ratio: 0.0, value_ratio: 0.0 },
            ClipNormalizedCurvePointPayload { time_ratio: 0.5, value_ratio: 0.72 },
            ClipNormalizedCurvePointPayload { time_ratio: 1.0, value_ratio: 1.0 },
        ] {
            state
                .dispatch_action(clip_edit_numeric_curve_action(
                    ClipEditNumericCurvePayload {
                        clip_id,
                        parameter: property.clone(),
                        edit: ClipCurveEditPayload::Upsert { keyframe_id: None, point },
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
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let opacity_property = opacity_parameter_address(&state, clip_id);
        let position_property = clip_parameter_address(&state, clip_id, Transform2D::POSITION_PATH);
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].is_locked = true;

        for action in [
            clip_set_enabled_action(ClipSetEnabledPayload { clip_id, enabled: false }),
            clip_parameter_action(
                clip_id,
                opacity_property.clone(),
                PropertyValue::Float(0.42),
            ),
            clip_set_solid_color_action(ClipSetSolidColorPayload {
                clip_id,
                color: Color::from_hex(0x2255AA),
            }),
            clip_parameter_action(
                clip_id,
                position_property,
                PropertyValue::Vec2(glam::Vec2::new(128.0, 72.0)),
            ),
            clip_edit_numeric_curve_action(ClipEditNumericCurvePayload {
                clip_id,
                parameter: opacity_property,
                edit: ClipCurveEditPayload::Upsert {
                    keyframe_id: None,
                    point: ClipNormalizedCurvePointPayload { time_ratio: 0.0, value_ratio: 0.5 },
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
    fn dispatch_visual_effect_adds_registered_effect_to_clip() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();

        state
            .dispatch_action(visual_effect_add_to_clip_action(
                VisualEffectAddToClipPayload { clip_id, effect_type: EffectType::GaussianBlur },
            ))
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
    fn dispatch_visual_effect_selects_by_stable_identity_without_undo_history() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let effect: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::GaussianBlur);
        let effect_id = effect.id;
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].clips[0]
            .add_effect_node(effect);

        state
            .dispatch_action(visual_effect_select_action(VisualEffectTargetPayload {
                clip_id,
                effect_id,
            }))
            .expect("dispatch select effect");

        let selected = state.primary_selected_effect().expect("selected effect");
        assert_eq!(selected.clip.clip_id, clip_id);
        assert_eq!(selected.effect_id, effect_id);
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_visual_effect_select_rejects_missing_effect_without_undo() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();

        let err = state
            .dispatch_action(visual_effect_select_action(VisualEffectTargetPayload {
                clip_id,
                effect_id: EffectId::new(),
            }))
            .expect_err("missing effect should reject selection");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state.primary_selected_effect().is_none());
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_visual_effect_sets_enabled_state() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let effect: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::GaussianBlur);
        let effect_id = effect.id;
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].clips[0]
            .add_effect_node(effect);

        state
            .dispatch_action(visual_effect_set_enabled_action(
                VisualEffectSetEnabledPayload { clip_id, effect_id, enabled: false },
            ))
            .expect("dispatch effect enabled");

        let effect =
            &state.active_sequence().expect("sequence").video_tracks[0].clips[0].effects[0];
        assert_eq!(effect.id, effect_id);
        assert!(!effect.is_enabled);
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_visual_effect_enabled_noop_is_rejected_without_undo_history() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let effect: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::GaussianBlur);
        let effect_id = effect.id;
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].clips[0]
            .add_effect_node(effect);

        let error = state
            .dispatch_action(visual_effect_set_enabled_action(
                VisualEffectSetEnabledPayload { clip_id, effect_id, enabled: true },
            ))
            .expect_err("no-op effect enabled must not report success");

        assert_action_not_executed(error, "visual_effect_set_enabled");
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_visual_effect_enabled_rejects_missing_effect_without_undo() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();

        let err = state
            .dispatch_action(visual_effect_set_enabled_action(
                VisualEffectSetEnabledPayload {
                    clip_id,
                    effect_id: EffectId::new(),
                    enabled: false,
                },
            ))
            .expect_err("missing effect should reject mutation");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_visual_effect_sets_parameter_by_stable_address_with_undo_snapshot() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let (effect_id, path, parameter, initial_value) =
            add_default_effect_with_first_property(&mut state, EffectType::GaussianBlur);
        let next_value = different_property_value(&initial_value);

        state
            .dispatch_action(visual_effect_set_parameter_value_action(
                VisualEffectSetParameterValuePayload {
                    clip_id,
                    effect_id,
                    parameter,
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
        let text_parameter = state
            .active_sequence()
            .and_then(|sequence| find_clip(sequence, selection.clip_id))
            .expect("Basic Title Clip")
            .intrinsic_parameter_bag()
            .address_for_path(mondrian_core::BasicTitle::TEXT_PATH)
            .expect("Basic Title text parameter");

        state
            .dispatch_action(clip_parameter_action(
                selection.clip_id,
                text_parameter,
                PropertyValue::Text("Mondrian".to_owned()),
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
    fn dispatch_visual_effect_parameter_noop_is_rejected_without_undo_history() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let (effect_id, _, parameter, initial_value) =
            add_default_effect_with_first_property(&mut state, EffectType::GaussianBlur);

        let error = state
            .dispatch_action(visual_effect_set_parameter_value_action(
                VisualEffectSetParameterValuePayload {
                    clip_id,
                    effect_id,
                    parameter,
                    value: initial_value,
                },
            ))
            .expect_err("no-op parameter edit must not report success");

        assert_action_not_executed(error, "visual_effect_set_parameter_value");
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_visual_effect_parameter_rejects_stale_address_without_undo() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let (effect_id, _, mut parameter, initial_value) =
            add_default_effect_with_first_property(&mut state, EffectType::GaussianBlur);
        parameter.parameter_id = mondrian_core::ParameterId::new_static("mondrian.test.missing");

        let err = state
            .dispatch_action(visual_effect_set_parameter_value_action(
                VisualEffectSetParameterValuePayload {
                    clip_id,
                    effect_id,
                    parameter,
                    value: initial_value,
                },
            ))
            .expect_err("missing effect property should reject mutation");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_visual_effect_removes_effect_instance() {
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
            .dispatch_action(visual_effect_remove_action(VisualEffectTargetPayload {
                clip_id,
                effect_id: remove_id,
            }))
            .expect("dispatch remove effect");

        let effects = &state.active_sequence().expect("sequence").video_tracks[0].clips[0].effects;
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].id, keep_id);
        assert!(state.can_undo_action());
    }

    #[test]
    fn dispatch_visual_effect_remove_preserves_locked_track() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let effect: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::GaussianBlur);
        let effect_id = effect.id;
        let sequence = state.active_sequence_mut_uncommitted().expect("sequence");
        sequence.video_tracks[0].clips[0].add_effect_node(effect);
        sequence.video_tracks[0].is_locked = true;

        let err = state
            .dispatch_action(visual_effect_remove_action(VisualEffectTargetPayload {
                clip_id,
                effect_id,
            }))
            .expect_err("locked track should reject effect removal");

        assert!(matches!(err, MondrianError::TrackLocked { .. }));
        let effects = &state.active_sequence().expect("sequence").video_tracks[0].clips[0].effects;
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].id, effect_id);
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_visual_effect_reorders_by_stable_anchor_with_undo_snapshot() {
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
            .dispatch_action(visual_effect_reorder_action(VisualEffectReorderPayload {
                clip_id,
                effect_id: first_id,
                placement: EffectRelativePlacement::After(third_id),
            }))
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
    fn dispatch_visual_effect_reorder_rejects_stale_anchor_without_undo() {
        let (mut state, _, clip_id) = state_with_two_video_tracks();
        let effect: mondrian_effects::EffectNode =
            mondrian_effects::EffectNodeExt::with_defaults(EffectType::GaussianBlur);
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].clips[0]
            .add_effect_node(effect);

        let effect_id =
            state.active_sequence().expect("sequence").video_tracks[0].clips[0].effects[0].id;
        let err = state
            .dispatch_action(visual_effect_reorder_action(VisualEffectReorderPayload {
                clip_id,
                effect_id,
                placement: EffectRelativePlacement::After(EffectId::new()),
            }))
            .expect_err("stale Effect anchor should reject reorder");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(!state.can_undo_action());
    }

    #[test]
    fn dispatch_visual_effect_reorder_preserves_locked_track() {
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
            .dispatch_action(visual_effect_reorder_action(VisualEffectReorderPayload {
                clip_id,
                effect_id: first_id,
                placement: EffectRelativePlacement::After(second_id),
            }))
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
        state.seek(18).expect("seek");

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
    fn dispatch_clipboard_actions_reject_missing_selection_or_clipboard() {
        let (mut state, track_id, clip_id) = state_with_two_video_tracks();

        let copy_error = state
            .dispatch_action(mondrian_editor_state::Action::Copy)
            .expect_err("copy without selection must fail");
        let cut_error = state
            .dispatch_action(mondrian_editor_state::Action::Cut)
            .expect_err("cut without selection must fail");
        let paste_error = state
            .dispatch_action(mondrian_editor_state::Action::Paste)
            .expect_err("paste without clipboard must fail");
        assert_action_not_executed(copy_error, "copy");
        assert_action_not_executed(cut_error, "cut");
        assert_action_not_executed(paste_error, "paste");
        assert!(!state.has_animation_clipboard());
        assert!(!state.can_undo_action());

        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];
        let paste_error = state
            .dispatch_action(mondrian_editor_state::Action::Paste)
            .expect_err("a target cannot make an empty clipboard executable");
        assert_action_not_executed(paste_error, "paste");
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
        state.seek(50).expect("seek");

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
        state.seek(0).expect("seek");

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
    fn dispatch_duplicate_action_rejects_missing_selection() {
        let (mut state, _, _) = state_with_two_video_tracks();

        let error = state
            .dispatch_action(mondrian_editor_state::Action::Duplicate)
            .expect_err("duplicate without selection must fail");

        assert_action_not_executed(error, "duplicate");
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
        state.seek(40).expect("seek");

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
