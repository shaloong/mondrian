use super::*;

fn sequence_time_from_frame(
    frame: i64,
    time_base: Rational,
) -> mondrian_core::Result<TimelineTime> {
    Ok(TimelineTime::from_frame_position(FramePosition::new(
        frame, time_base,
    ))?)
}

fn sequence_frame_from_time(
    time: TimelineTime,
    frame_rate: Rational,
) -> mondrian_core::Result<i64> {
    Ok(time.to_frame_position(frame_rate, FrameRounding::Nearest)?.frame)
}

fn sequence_is_nested_reference(sequences: &[Sequence], sequence_id: SequenceId) -> bool {
    sequences.iter().any(|sequence| {
        sequence
            .video_tracks
            .iter()
            .chain(&sequence.audio_tracks)
            .flat_map(|track| &track.clips)
            .any(|clip| clip.nested_sequence_id() == Some(sequence_id))
    })
}
impl AppState {
    pub fn sequence_by_id(&self, id: SequenceId) -> Option<&Sequence> {
        self.authoring.as_ref().and_then(|session| session.sequence(id))
    }

    pub fn export_sequences_snapshot(&self) -> Vec<Sequence> {
        self.sequences().to_vec()
    }

    pub fn switch_active_sequence(&mut self, sequence_id: SequenceId) -> mondrian_core::Result<()> {
        self.switch_active_sequence_internal(sequence_id, false)
    }

    fn switch_active_sequence_internal(
        &mut self,
        sequence_id: SequenceId,
        push_current: bool,
    ) -> mondrian_core::Result<()> {
        self.authoring
            .as_mut()
            .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "switch_active_sequence".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            })?
            .switch_active_sequence(sequence_id, push_current)?;
        self.stop();
        self.settle_preview_access_source();
        Ok(())
    }

    pub fn open_nested_sequence(&mut self, sequence_id: SequenceId) -> mondrian_core::Result<()> {
        self.switch_active_sequence_internal(sequence_id, true)
    }

    pub fn return_to_parent_sequence(&mut self) -> mondrian_core::Result<bool> {
        let returned = self
            .authoring
            .as_mut()
            .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "return_to_parent_sequence".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            })?
            .return_to_parent_sequence()?;
        if returned {
            self.stop();
            self.settle_preview_access_source();
        }
        Ok(returned)
    }

    pub fn set_default_sequence(&mut self, sequence_id: SequenceId) -> mondrian_core::Result<()> {
        let session = self.authoring.as_mut().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "set_default_sequence".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            }
        })?;
        let before = session.document().clone();
        let mut after = before.clone();
        if after.sequences.sequence(sequence_id).is_none() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "set_default_sequence".to_string(),
                reason: format!("序列不存在: {sequence_id}"),
            });
        }
        if after.sequences.default_sequence_id == sequence_id {
            return Ok(());
        }
        after.sequences.default_sequence_id = sequence_id;
        session.commit_project_snapshot("设置默认序列", before, after)?;
        Ok(())
    }

    pub fn rename_sequence(
        &mut self,
        sequence_id: SequenceId,
        name: impl Into<String>,
    ) -> mondrian_core::Result<()> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "rename_sequence".to_string(),
                reason: "序列名称不能为空".to_string(),
            });
        }
        let before = self.sequence_by_id(sequence_id).cloned().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "rename_sequence".to_string(),
                reason: format!("序列不存在: {sequence_id}"),
            }
        })?;
        self.update_sequence_identity_and_settings(sequence_id, name, before.settings)
    }

    pub fn duplicate_sequence(
        &mut self,
        sequence_id: SequenceId,
        name: impl Into<String>,
    ) -> mondrian_core::Result<SequenceId> {
        let session = self.authoring.as_mut().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "duplicate_sequence".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            }
        })?;
        let before = session.document().clone();
        let mut after = before.clone();
        let mut duplicated = after.sequences.sequence(sequence_id).cloned().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "duplicate_sequence".to_string(),
                reason: format!("序列不存在: {sequence_id}"),
            }
        })?;
        let fallback_name = format!("{} 副本", duplicated.name);
        duplicated.fork_author_identities_for_sequence_duplicate();
        duplicated.name = name.into();
        if duplicated.name.trim().is_empty() {
            duplicated.name = fallback_name;
        }
        let duplicated_id = duplicated.id;
        after.sequences.add_sequence(duplicated)?;
        after.sequences.set_active(duplicated_id)?;
        session.commit_project_snapshot("复制序列", before, after)?;
        self.stop();
        self.settle_preview_access_source();
        Ok(duplicated_id)
    }

    pub fn delete_sequence(&mut self, sequence_id: SequenceId) -> mondrian_core::Result<()> {
        let session = self.authoring.as_mut().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "delete_sequence".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            }
        })?;
        let before = session.document().clone();
        if before.sequences.sequences.len() <= 1 {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "delete_sequence".to_string(),
                reason: "至少保留一个序列".to_string(),
            });
        }
        if sequence_is_nested_reference(&before.sequences.sequences, sequence_id) {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "delete_sequence".to_string(),
                reason: "序列正被嵌套引用，不能删除".to_string(),
            });
        }
        let mut after = before.clone();
        let Some(index) =
            after.sequences.sequences.iter().position(|sequence| sequence.id == sequence_id)
        else {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "delete_sequence".to_string(),
                reason: format!("序列不存在: {sequence_id}"),
            });
        };
        after.sequences.sequences.remove(index);
        let fallback_id =
            after.sequences.sequences.first().map(|sequence| sequence.id).ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "delete_sequence".to_string(),
                    reason: "删除后没有可用序列".to_string(),
                }
            })?;
        if after.sequences.default_sequence_id == sequence_id {
            after.sequences.default_sequence_id = fallback_id;
        }
        let active_changed = after.sequences.active_sequence_id == sequence_id;
        if active_changed {
            after.sequences.active_sequence_id = fallback_id;
        }
        session.commit_project_snapshot("删除序列", before, after)?;
        if active_changed {
            self.stop();
            self.settle_preview_access_source();
        }
        Ok(())
    }

    pub fn update_active_sequence_settings(
        &mut self,
        settings: mondrian_timeline::sequence::SequenceSettings,
    ) -> mondrian_core::Result<()> {
        settings.validate_with_color_environment(self.project_color_environment())?;
        let before = self.active_sequence().cloned().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "update_active_sequence_settings".to_string(),
                reason: "当前无项目".to_string(),
            }
        })?;
        let sequence_id = before.id;
        let mut after = before.clone();
        after.apply_settings(settings)?;
        self.record_sequence_snapshot_command("修改序列设置", before, after)?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(())
    }

    /// Atomically update one sequence name and settings through the app-state boundary.
    pub fn update_sequence_identity_and_settings(
        &mut self,
        sequence_id: SequenceId,
        name: impl Into<String>,
        settings: mondrian_timeline::sequence::SequenceSettings,
    ) -> mondrian_core::Result<()> {
        let name = name.into();
        let name = name.trim();
        if name.is_empty() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "update_sequence_identity_and_settings".to_string(),
                reason: "序列名称不能为空".to_string(),
            });
        }
        settings.validate_with_color_environment(self.project_color_environment())?;
        let before = self.sequence_by_id(sequence_id).cloned().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "update_sequence_identity_and_settings".to_string(),
                reason: format!("序列不存在: {sequence_id}"),
            }
        })?;
        let mut after = before.clone();
        after.name = name.to_owned();
        after.apply_settings(settings)?;

        self.record_sequence_snapshot_command("修改序列设置", before, after)?;
        if self.active_sequence_id() == Some(sequence_id) {
            self.stop();
            self.settle_preview_access_source();
        }
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(())
    }

    pub(super) fn record_sequence_snapshot_command(
        &mut self,
        description: impl Into<String>,
        before: Sequence,
        after: Sequence,
    ) -> mondrian_core::Result<()> {
        let commit = self
            .authoring
            .as_mut()
            .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "record_sequence_snapshot_command".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            })?
            .commit_sequence_snapshot(description, before, after)?;
        if !commit.undo_retained {
            self.set_status_hint("编辑已提交，但超出撤销历史内存预算", true);
        }
        Ok(())
    }

    pub(super) fn commit_active_sequence_edit<T>(
        &mut self,
        description: impl Into<String>,
        edit: impl FnOnce(&mut Sequence) -> mondrian_core::Result<T>,
    ) -> mondrian_core::Result<T> {
        let (value, commit) = self
            .authoring
            .as_mut()
            .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "commit_active_sequence_edit".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            })?
            .edit_active_sequence(description, edit)?;
        if !commit.undo_retained {
            self.set_status_hint("编辑已提交，但超出撤销历史内存预算", true);
        }
        Ok(value)
    }

    pub fn undo_timeline(&mut self) -> mondrian_core::Result<bool> {
        let commit = self.authoring.as_mut().map(AuthoringSession::undo).transpose()?.flatten();
        let Some(commit) = commit else {
            return Ok(false);
        };
        for sequence_id in commit.changed_sequence_ids {
            self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        }
        self.stop();
        self.settle_preview_access_source();
        Ok(true)
    }

    pub fn redo_timeline(&mut self) -> mondrian_core::Result<bool> {
        let commit = self.authoring.as_mut().map(AuthoringSession::redo).transpose()?.flatten();
        let Some(commit) = commit else {
            return Ok(false);
        };
        for sequence_id in commit.changed_sequence_ids {
            self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        }
        self.stop();
        self.settle_preview_access_source();
        Ok(true)
    }

    pub fn close_project(&mut self) {
        self.proxy_generation.bind_project(None);
        self.authoring = None;
        self.autosave_in_flight_request = None;
        self.stop();
        self.settle_preview_access_source();
        self.dragging_asset = None;
        self.clear_status_hint();
    }

    pub fn add_video_track(&mut self) -> anyhow::Result<()> {
        if self.active_sequence().is_some() {
            self.commit_active_sequence_edit("新增视频轨道", |sequence| {
                sequence.add_video_track();
                Ok(())
            })?;
        }
        Ok(())
    }

    pub fn add_audio_track(&mut self) -> anyhow::Result<()> {
        if self.active_sequence().is_some() {
            self.commit_active_sequence_edit("新增音频轨道", |sequence| {
                sequence.add_audio_track();
                Ok(())
            })?;
        }
        Ok(())
    }

    pub fn default_adjustment_layer_duration_frames(&self) -> mondrian_core::Result<i64> {
        let in_frame = self.in_point_frame()?;
        let selection_span = self
            .out_point_frame()?
            .map(|out| out.saturating_sub(in_frame))
            .filter(|span| *span > 0);
        if let Some(span) = selection_span {
            return Ok(span.max(1));
        }

        let fps = self
            .active_sequence()
            .map(|seq| seq.settings.frame_rate.to_f64())
            .unwrap_or(25.0);
        Ok(((DEFAULT_ADJUSTMENT_LAYER_DURATION_SECS * fps).round() as i64).max(1))
    }

    pub fn default_adjustment_layer_drag_duration(&self) -> mondrian_core::Result<Duration> {
        let fps = self
            .active_sequence()
            .map(|seq| seq.settings.frame_rate.to_f64())
            .unwrap_or(25.0);
        let secs = self.default_adjustment_layer_duration_frames()? as f64 / fps.max(1.0);
        Ok(Duration::from_secs_f64(secs.max(1.0 / fps.max(1.0))))
    }

    pub fn create_folder_in_library(
        &mut self,
        name: &str,
        parent_id: Option<&str>,
    ) -> mondrian_core::Result<String> {
        let library = self.asset_library().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "create_folder".to_string(),
                reason: "素材库未连接".to_string(),
            }
        })?;
        let folder_id = library.create_folder(name, parent_id)?;
        self.set_status_hint(format!("已新建文件夹：{}", name), false);
        Ok(folder_id)
    }

    /// Create a library folder using the next available default folder name.
    pub fn create_default_folder_in_library(
        &mut self,
        parent_id: Option<&str>,
    ) -> mondrian_core::Result<String> {
        let library = self.asset_library().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "create_folder".to_string(),
                reason: "素材库未连接".to_string(),
            }
        })?;
        let folders = library.list_folders()?;
        let mut next = 1usize;
        for folder in &folders {
            if let Some(suffix) = folder.name.strip_prefix("文件夹 ") {
                if let Ok(number) = suffix.trim().parse::<usize>() {
                    next = next.max(number + 1);
                }
            }
        }
        self.create_folder_in_library(&format!("文件夹 {next}"), parent_id)
    }

    /// Delete one asset-library folder/bin and unlink assets assigned to it or its children.
    pub fn delete_folder_from_library(&mut self, folder_id: &str) -> mondrian_core::Result<()> {
        let library = self.asset_library().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "delete_folder".to_string(),
                reason: "素材库未连接".to_string(),
            }
        })?;
        library.delete_folder(folder_id)?;
        self.event_bus.publish(mondrian_core::events::AppEvent::AssetLibraryReloaded);
        Ok(())
    }

    /// Move one asset-library item into a folder/bin or back to the root level.
    pub fn move_asset_in_library(
        &mut self,
        asset_id: AssetId,
        folder_id: Option<&str>,
    ) -> mondrian_core::Result<()> {
        let library = self.asset_library().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "move_asset".to_string(),
                reason: "素材库未连接".to_string(),
            }
        })?;
        library.move_asset_to_folder(asset_id, folder_id)?;
        self.event_bus.publish(mondrian_core::events::AppEvent::AssetLibraryReloaded);
        Ok(())
    }

    /// Move one asset-library folder/bin under another folder or back to root.
    pub fn move_folder_in_library(
        &mut self,
        folder_id: &str,
        parent_folder_id: Option<&str>,
    ) -> mondrian_core::Result<()> {
        let library = self.asset_library().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "move_folder".to_string(),
                reason: "素材库未连接".to_string(),
            }
        })?;
        library.move_folder(folder_id, parent_folder_id)?;
        self.event_bus.publish(mondrian_core::events::AppEvent::AssetLibraryReloaded);
        Ok(())
    }

    pub fn delete_asset_from_library(&mut self, asset_id: AssetId) -> mondrian_core::Result<()> {
        // Remove clips referencing this asset from the timeline first,
        // then delete from the library. Order matters for borrow reasons.
        let _removed = self.delete_asset_and_cleanup_timeline(asset_id)?;
        let library = self.asset_library().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "delete_asset".to_string(),
                reason: "素材库未连接".to_string(),
            }
        })?;
        library.delete_asset(asset_id)?;
        self.event_bus
            .publish(mondrian_core::events::AppEvent::AssetDeleted { asset_id });
        Ok(())
    }

    fn create_adjustment_layer_asset_internal(
        &mut self,
        name: Option<&str>,
        folder_id: Option<&str>,
        announce: bool,
    ) -> mondrian_core::Result<(AssetId, String)> {
        let library = self.asset_library().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "create_adjustment_layer_asset".to_string(),
                reason: "素材库未连接".to_string(),
            }
        })?;

        if let Some(folder_id) = folder_id {
            if !library.folder_exists(folder_id)? {
                return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "create_adjustment_layer_asset".to_string(),
                    reason: format!("目标素材文件夹不存在：{folder_id}"),
                });
            }
        }

        let asset_id = library.create_adjustment_layer_asset(name)?;
        library.move_asset_to_folder(asset_id, folder_id)?;
        let asset_name = library
            .get_asset(asset_id)?
            .map(|asset| asset.name)
            .unwrap_or_else(|| "调整图层".to_string());

        self.event_bus
            .publish(mondrian_core::events::AppEvent::AssetImported { asset_id });
        if announce {
            self.set_status_hint(format!("已新建：{}", asset_name), false);
        }
        Ok((asset_id, asset_name))
    }

    pub fn create_adjustment_layer_asset(
        &mut self,
        name: Option<&str>,
    ) -> mondrian_core::Result<AssetId> {
        self.create_adjustment_layer_asset_in_folder(name, None)
    }

    /// Create a reusable adjustment-layer asset in an optional asset-library folder.
    pub fn create_adjustment_layer_asset_in_folder(
        &mut self,
        name: Option<&str>,
        folder_id: Option<&str>,
    ) -> mondrian_core::Result<AssetId> {
        self.create_adjustment_layer_asset_internal(name, folder_id, true)
            .map(|(asset_id, _)| asset_id)
    }

    pub fn create_solid_color_asset(
        &mut self,
        name: Option<&str>,
    ) -> mondrian_core::Result<AssetId> {
        self.create_solid_color_asset_in_folder(name, None)
    }

    /// Create a reusable solid-color asset in an optional asset-library folder.
    pub fn create_solid_color_asset_in_folder(
        &mut self,
        name: Option<&str>,
        folder_id: Option<&str>,
    ) -> mondrian_core::Result<AssetId> {
        let library = self.asset_library().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "create_solid_color_asset".to_string(),
                reason: "素材库未连接".to_string(),
            }
        })?;
        if let Some(folder_id) = folder_id {
            if !library.folder_exists(folder_id)? {
                return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "create_solid_color_asset".to_string(),
                    reason: format!("目标素材文件夹不存在：{folder_id}"),
                });
            }
        }
        let asset_id = library.create_solid_color_asset(name)?;
        library.move_asset_to_folder(asset_id, folder_id)?;
        let asset_name = library
            .get_asset(asset_id)?
            .map(|asset| asset.name)
            .unwrap_or_else(|| "纯色层".to_string());
        self.event_bus
            .publish(mondrian_core::events::AppEvent::AssetImported { asset_id });
        self.set_status_hint(format!("已新建：{}", asset_name), false);
        Ok(asset_id)
    }

    pub fn create_adjustment_layer_on_video_track(
        &mut self,
        track_id: TrackId,
        timeline_frame: Option<i64>,
        overlap_mode: ClipOverlapMode,
    ) -> mondrian_core::Result<ClipId> {
        let in_frame = self.in_point_frame()?;
        let selection_start =
            self.out_point_frame()?.filter(|out| *out > in_frame).map(|_| in_frame);
        let start_frame = timeline_frame
            .or(selection_start)
            .unwrap_or_else(|| self.current_frame().max(0));
        let (asset_id, asset_name) =
            self.create_adjustment_layer_asset_internal(None, None, false)?;
        self.begin_drag_asset(
            asset_id,
            asset_name.clone(),
            AssetKind::AdjustmentLayer,
            self.default_adjustment_layer_drag_duration()?,
            false,
        );
        let clip_id =
            self.drop_dragging_asset_to_video_track_with_mode(track_id, start_frame, overlap_mode)?;
        self.set_status_hint(format!("已创建调整图层：{}", asset_name), false);
        Ok(clip_id)
    }

    pub fn create_solid_color_on_video_track(
        &mut self,
        track_id: TrackId,
        timeline_frame: Option<i64>,
        overlap_mode: ClipOverlapMode,
    ) -> mondrian_core::Result<ClipId> {
        self.create_solid_color_on_video_track_with_color(
            track_id,
            timeline_frame,
            overlap_mode,
            Color::from_hex(0x808080),
        )
    }

    pub fn create_solid_color_on_video_track_with_color(
        &mut self,
        track_id: TrackId,
        timeline_frame: Option<i64>,
        overlap_mode: ClipOverlapMode,
        color: Color,
    ) -> mondrian_core::Result<ClipId> {
        let (asset_id, asset_name) = {
            let library = self.asset_library().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "create_solid_color".to_string(),
                    reason: "素材库未连接".to_string(),
                }
            })?;
            let asset_id = library.create_solid_color_asset(None)?;
            let asset_name = library
                .get_asset(asset_id)?
                .map(|asset| asset.name)
                .unwrap_or_else(|| "纯色层".to_string());
            (asset_id, asset_name)
        };

        let in_frame = self.in_point_frame()?;
        let selection_start =
            self.out_point_frame()?.filter(|out| *out > in_frame).map(|_| in_frame);
        let start_frame = timeline_frame
            .or(selection_start)
            .unwrap_or_else(|| self.current_frame().max(0));
        let default_duration_secs = self.default_adjustment_layer_drag_duration()?.as_secs_f64();

        let (sequence_id, clip_id) = self.commit_active_sequence_edit("创建纯色层", |seq| {
            let time_base = seq.time_base();
            let fps = seq.settings.frame_rate.to_f64();
            let duration_frames = (default_duration_secs * fps).ceil() as i64;
            let duration_frames = duration_frames.max(1);
            let mut clip = Clip::new_solid_color(
                asset_id,
                color,
                sequence_time_from_frame(start_frame, time_base)?,
                sequence_time_from_frame(duration_frames, time_base)?,
            )?;
            clip.label = Some(asset_name.clone());
            let clip_id = clip.id;
            let track = seq.video_track_mut(track_id).ok_or_else(|| {
                mondrian_core::MondrianError::TrackNotFound { track_id: track_id.to_string() }
            })?;
            track.add_clip(clip)?;
            resolve_track_conflicts(track, clip_id, overlap_mode)?;
            compact_sequence_references(seq);
            let sequence_id = seq.id;
            Ok((sequence_id, clip_id))
        })?;

        self.set_status_hint(format!("已创建纯色层: {}", asset_name), false);
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(clip_id)
    }

    pub fn remove_track(&mut self, track_id: TrackId, is_video: bool) -> mondrian_core::Result<()> {
        self.commit_active_sequence_edit("删除轨道", |seq| {
            if is_video {
                seq.remove_video_track(track_id)?;
            } else {
                seq.remove_audio_track(track_id)?;
            }
            compact_sequence_references(seq);
            Ok(())
        })?;

        self.prune_selection_to_active_sequence();
        Ok(())
    }

    /// Remove multiple timeline tracks as one undoable edit after validating all targets.
    pub fn remove_tracks_bulk(&mut self, tracks: &[(TrackId, bool)]) -> mondrian_core::Result<()> {
        let mut targets = Vec::<(TrackId, bool)>::new();
        for target in tracks {
            if !targets.contains(target) {
                targets.push(*target);
            }
        }
        if targets.is_empty() {
            return Ok(());
        }

        self.commit_active_sequence_edit("删除轨道", |seq| {
            for (track_id, is_video) in &targets {
                let exists = if *is_video {
                    seq.video_tracks.iter().any(|track| track.id == *track_id)
                } else {
                    seq.audio_tracks.iter().any(|track| track.id == *track_id)
                };
                if !exists {
                    return Err(mondrian_core::MondrianError::TrackNotFound {
                        track_id: track_id.to_string(),
                    });
                }
            }

            let selected_video_count = targets.iter().filter(|(_, is_video)| *is_video).count();
            let selected_audio_count = targets.len() - selected_video_count;
            if seq.video_tracks.len().saturating_sub(selected_video_count) < 1 {
                return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "remove_tracks".to_string(),
                    reason: "至少保留 1 条视频轨道".to_string(),
                });
            }
            if seq.audio_tracks.len().saturating_sub(selected_audio_count) < 1 {
                return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "remove_tracks".to_string(),
                    reason: "至少保留 1 条音频轨道".to_string(),
                });
            }

            for (track_id, is_video) in &targets {
                if *is_video {
                    seq.remove_video_track(*track_id)?;
                } else {
                    seq.remove_audio_track(*track_id)?;
                }
            }
            compact_sequence_references(seq);
            Ok(())
        })?;

        self.prune_selection_to_active_sequence();
        Ok(())
    }

    pub fn move_track(
        &mut self,
        track_id: TrackId,
        is_video: bool,
        new_index: usize,
    ) -> mondrian_core::Result<()> {
        self.commit_active_sequence_edit("移动轨道", |seq| {
            if is_video {
                seq.move_video_track(track_id, new_index)?;
            } else {
                seq.move_audio_track(track_id, new_index)?;
            }
            Ok(())
        })?;
        Ok(())
    }

    pub fn set_track_visible(
        &mut self,
        track_id: TrackId,
        is_video: bool,
        visible: bool,
    ) -> mondrian_core::Result<()> {
        if self
            .active_sequence()
            .and_then(|seq| {
                if is_video {
                    seq.video_tracks.iter().find(|track| track.id == track_id)
                } else {
                    seq.audio_tracks.iter().find(|track| track.id == track_id)
                }
            })
            .is_some_and(|track| track.is_visible == visible)
        {
            return Ok(());
        }
        self.commit_active_sequence_edit("切换轨道可见性", |seq| {
            let track = if is_video {
                seq.video_track_mut(track_id)
            } else {
                seq.audio_track_mut(track_id)
            }
            .ok_or_else(|| mondrian_core::MondrianError::TrackNotFound {
                track_id: track_id.to_string(),
            })?;
            track.is_visible = visible;
            Ok(())
        })?;
        Ok(())
    }

    pub fn set_track_muted(
        &mut self,
        track_id: TrackId,
        is_video: bool,
        muted: bool,
    ) -> mondrian_core::Result<()> {
        if self
            .active_sequence()
            .and_then(|seq| {
                if is_video {
                    seq.video_tracks.iter().find(|track| track.id == track_id)
                } else {
                    seq.audio_tracks.iter().find(|track| track.id == track_id)
                }
            })
            .is_some_and(|track| track.is_muted == muted)
        {
            return Ok(());
        }
        self.commit_active_sequence_edit("切换轨道静音", |seq| {
            let track = if is_video {
                seq.video_track_mut(track_id)
            } else {
                seq.audio_track_mut(track_id)
            }
            .ok_or_else(|| mondrian_core::MondrianError::TrackNotFound {
                track_id: track_id.to_string(),
            })?;
            track.is_muted = muted;
            Ok(())
        })?;
        Ok(())
    }

    pub fn set_track_locked(
        &mut self,
        track_id: TrackId,
        is_video: bool,
        locked: bool,
    ) -> mondrian_core::Result<()> {
        if self
            .active_sequence()
            .and_then(|seq| {
                if is_video {
                    seq.video_tracks.iter().find(|track| track.id == track_id)
                } else {
                    seq.audio_tracks.iter().find(|track| track.id == track_id)
                }
            })
            .is_some_and(|track| track.is_locked == locked)
        {
            return Ok(());
        }
        self.commit_active_sequence_edit("切换轨道锁定", |seq| {
            let track = if is_video {
                seq.video_track_mut(track_id)
            } else {
                seq.audio_track_mut(track_id)
            }
            .ok_or_else(|| mondrian_core::MondrianError::TrackNotFound {
                track_id: track_id.to_string(),
            })?;
            track.is_locked = locked;
            Ok(())
        })?;
        Ok(())
    }

    pub fn set_clips_disabled_bulk(
        &mut self,
        selections: &[(TrackId, bool, ClipId)],
        disabled: bool,
    ) -> mondrian_core::Result<usize> {
        if selections.is_empty() {
            return Ok(0);
        }

        let (sequence_id, before, after, changed_count) = {
            let before = self.active_sequence().cloned().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "set_clips_disabled_bulk".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;
            let mut after = before.clone();
            let seq = &mut after;
            let mut clip_ids: HashSet<ClipId> = selections.iter().map(|(_, _, id)| *id).collect();
            expand_clip_link_groups(seq, &mut clip_ids);

            if clip_ids.is_empty() {
                return Ok(0);
            }

            for clip_id in &clip_ids {
                if let Some((track_id, _is_video, is_locked)) = find_clip_track_lock(seq, *clip_id)
                {
                    if is_locked {
                        return Err(mondrian_core::MondrianError::TrackLocked {
                            track_id: track_id.to_string(),
                        });
                    }
                }
            }

            let mut changed_count = 0usize;
            for clip_id in clip_ids {
                if set_clip_disabled(seq, clip_id, disabled) {
                    changed_count += 1;
                }
            }

            if changed_count == 0 {
                return Ok(0);
            }

            compact_sequence_references(seq);

            (seq.id, before, after, changed_count)
        };

        let action = if disabled {
            "禁用片段"
        } else {
            "启用片段"
        };
        self.record_sequence_snapshot_command(action, before, after)?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });

        Ok(changed_count)
    }

    pub fn remove_clip(
        &mut self,
        track_id: TrackId,
        is_video_track: bool,
        clip_id: ClipId,
    ) -> mondrian_core::Result<()> {
        let (sequence_id, before, after) = {
            let before = self.active_sequence().cloned().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "remove_clip".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;
            let mut after = before.clone();
            let seq = &mut after;
            let group_members = clip_link_group_member_ids(seq, clip_id);
            if group_members.is_empty() {
                return Err(mondrian_core::MondrianError::ClipNotFound {
                    clip_id: clip_id.to_string(),
                });
            }
            let location = find_clip_track_lock(seq, clip_id).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() }
            })?;
            if location.0 != track_id || location.1 != is_video_track {
                return Err(mondrian_core::MondrianError::ClipNotFound {
                    clip_id: clip_id.to_string(),
                });
            }
            for member in &group_members {
                if let Some((member_track, _, true)) = find_clip_track_lock(seq, *member) {
                    return Err(mondrian_core::MondrianError::TrackLocked {
                        track_id: member_track.to_string(),
                    });
                }
            }
            for member in group_members {
                let _ = remove_clip_from_sequence(seq, member);
            }
            compact_sequence_references(seq);

            (seq.id, before, after)
        };

        self.record_sequence_snapshot_command("删除片段", before, after)?;
        self.event_bus.publish(AppEvent::ClipRemoved { sequence_id, clip_id });
        Ok(())
    }

    pub fn remove_clips_bulk(
        &mut self,
        selections: &[(TrackId, bool, ClipId)],
        ripple: bool,
    ) -> mondrian_core::Result<usize> {
        if selections.is_empty() {
            return Ok(0);
        }

        let (sequence_id, before, after, removed_count) = {
            let before = self.active_sequence().cloned().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "remove_clips_bulk".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;
            let mut after = before.clone();
            let seq = &mut after;
            let mut removed_count = 0usize;
            let mut selected_ids: HashSet<ClipId> =
                selections.iter().map(|(_, _, id)| *id).collect();
            expand_clip_link_groups(seq, &mut selected_ids);

            let mut by_track: HashMap<(TrackId, bool), HashSet<ClipId>> = HashMap::new();
            for clip_id in &selected_ids {
                let Some((track_id, is_video, _)) = find_clip_track_lock(seq, *clip_id) else {
                    continue;
                };
                by_track.entry((track_id, is_video)).or_default().insert(*clip_id);
            }

            for ((track_id, is_video), clip_ids) in by_track {
                let track = if is_video {
                    seq.video_track_mut(track_id)
                } else {
                    seq.audio_track_mut(track_id)
                }
                .ok_or_else(|| mondrian_core::MondrianError::TrackNotFound {
                    track_id: track_id.to_string(),
                })?;

                if track.is_locked {
                    return Err(mondrian_core::MondrianError::TrackLocked {
                        track_id: track_id.to_string(),
                    });
                }

                let mut removed_segments = Vec::new();
                let mut kept: Vec<Clip> = Vec::with_capacity(track.clips.len());
                for clip in track.clips.drain(..) {
                    if clip_ids.contains(&clip.id) {
                        removed_count += 1;
                        removed_segments.push((clip.position, clip.duration));
                    } else {
                        kept.push(clip);
                    }
                }
                track.clips = kept;

                if ripple {
                    removed_segments.sort_by_key(|(start, _)| *start);
                    for (start, dur) in removed_segments {
                        let end = start.checked_add(dur)?;
                        for clip in &mut track.clips {
                            if clip.position >= end {
                                clip.position =
                                    clip.position.checked_sub(dur)?.max(TimelineTime::ZERO);
                            }
                        }
                    }
                }

                resolve_track_overlaps(track)?;
            }

            compact_sequence_references(seq);

            (seq.id, before, after, removed_count)
        };

        if removed_count > 0 {
            self.record_sequence_snapshot_command("删除多个片段", before, after)?;
            self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        }

        Ok(removed_count)
    }

    pub fn delete_asset_and_cleanup_timeline(
        &mut self,
        asset_id: AssetId,
    ) -> mondrian_core::Result<usize> {
        let Some(before) = self.active_sequence().cloned() else {
            return Ok(0);
        };
        let mut after = before.clone();
        let mut removed_count = 0usize;
        removed_count += remove_asset_clips_from_tracks(&mut after.video_tracks, asset_id)?;
        removed_count += remove_asset_clips_from_tracks(&mut after.audio_tracks, asset_id)?;
        compact_sequence_references(&mut after);
        if removed_count > 0 {
            self.record_sequence_snapshot_command("删除素材并清理时间线", before, after)?;
        }

        Ok(removed_count)
    }

    pub fn relink_asset(
        &mut self,
        asset_id: AssetId,
        new_path: &Path,
    ) -> mondrian_core::Result<()> {
        let library = self.asset_library().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "relink_asset".to_string(),
                reason: "素材库未连接".to_string(),
            }
        })?;
        library.relink_asset(asset_id, new_path)?;
        Ok(())
    }

    pub fn relink_offline_assets_in_directory(
        &mut self,
        directory: &Path,
    ) -> mondrian_core::Result<usize> {
        let library = self.asset_library().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "relink_offline_assets_in_directory".to_string(),
                reason: "素材库未连接".to_string(),
            }
        })?;

        let assets = library.list_assets()?;
        let offline_assets: Vec<_> = assets
            .into_iter()
            .filter(|asset| asset.kind != AssetKind::AdjustmentLayer && !asset.path.exists())
            .collect();
        if offline_assets.is_empty() {
            return Ok(0);
        }

        let mut filename_index = HashMap::<String, Vec<PathBuf>>::new();
        collect_files_by_name(directory, &mut filename_index)?;

        let mut relinked = 0usize;
        for asset in offline_assets {
            let Some(name) = asset.path.file_name().and_then(|v| v.to_str()) else {
                continue;
            };
            let key = name.to_ascii_lowercase();
            let Some(candidates) = filename_index.get(&key) else {
                continue;
            };

            for candidate in candidates {
                if library.relink_asset(asset.id, candidate).is_ok() {
                    relinked += 1;
                    break;
                }
            }
        }

        Ok(relinked)
    }

    /// Create a Sequence as one project-level authoring transaction.
    pub fn new_sequence(&mut self, name: &str) {
        let sequence = match Sequence::with_settings(name, self.new_sequence_defaults().clone()) {
            Ok(sequence) => sequence,
            Err(error) => {
                tracing::error!(%error, "新建序列默认模板无效");
                self.set_status_hint(format!("新建序列失败：{error}"), true);
                return;
            }
        };
        let sequence_id = sequence.id;
        let result = self
            .authoring
            .as_mut()
            .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "new_sequence".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            })
            .and_then(|session| {
                let before = session.document().clone();
                let mut after = before.clone();
                after.sequences.add_sequence(sequence)?;
                after.sequences.set_active(sequence_id)?;
                session.commit_project_snapshot("新建序列", before, after)?;
                Ok(())
            });
        match result {
            Ok(()) => {
                self.stop();
                self.settle_preview_access_source();
                tracing::info!(%sequence_id, "新建序列: {name}");
            }
            Err(error) => {
                tracing::error!(%error, "新建序列失败");
                self.set_status_hint(format!("新建序列失败：{error}"), true);
            }
        }
    }

    pub fn precompose_clips_as_sequence(
        &mut self,
        selections: &[(TrackId, bool, ClipId)],
        name: &str,
    ) -> mondrian_core::Result<ClipId> {
        if selections.is_empty() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "precompose_clips_as_sequence".to_string(),
                reason: "没有选中的片段".to_string(),
            });
        }

        let project_before = self
            .authoring
            .as_ref()
            .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "precompose_clips_as_sequence".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            })?
            .document()
            .clone();

        let (sequence_id, after, nested_sequence, nested_clip_id) = {
            let source = self.active_sequence().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "precompose_clips_as_sequence".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;
            let mut parent = source.clone();
            let mut selected_ids: HashSet<ClipId> =
                selections.iter().map(|(_, _, clip_id)| *clip_id).collect();
            expand_clip_link_groups(&parent, &mut selected_ids);

            let mut min_time: Option<TimelineTime> = None;
            let mut max_time = TimelineTime::ZERO;
            let mut target_video_track_index = None;
            let mut target_audio_track_index = None;
            for (track_index, track) in parent.video_tracks.iter().enumerate() {
                if track.is_locked && track.clips.iter().any(|clip| selected_ids.contains(&clip.id))
                {
                    return Err(mondrian_core::MondrianError::TrackLocked {
                        track_id: track.id.to_string(),
                    });
                }
                for clip in &track.clips {
                    if selected_ids.contains(&clip.id) {
                        min_time =
                            Some(min_time.map_or(clip.position, |time| time.min(clip.position)));
                        max_time = max_time.max(clip.end_position()?);
                        target_video_track_index.get_or_insert(track_index);
                    }
                }
            }
            for (track_index, track) in parent.audio_tracks.iter().enumerate() {
                if track.is_locked && track.clips.iter().any(|clip| selected_ids.contains(&clip.id))
                {
                    return Err(mondrian_core::MondrianError::TrackLocked {
                        track_id: track.id.to_string(),
                    });
                }
                for clip in &track.clips {
                    if selected_ids.contains(&clip.id) {
                        min_time =
                            Some(min_time.map_or(clip.position, |time| time.min(clip.position)));
                        max_time = max_time.max(clip.end_position()?);
                        target_audio_track_index.get_or_insert(track_index);
                    }
                }
            }

            let Some(min_time) = min_time else {
                return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "precompose_clips_as_sequence".to_string(),
                    reason: "选区没有有效时长".to_string(),
                });
            };
            if max_time <= min_time {
                return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "precompose_clips_as_sequence".to_string(),
                    reason: "选区没有有效时长".to_string(),
                });
            }

            let mut nested_sequence = Sequence::with_settings(name, parent.settings.clone())
                .map_err(|err| mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "precompose_clips_as_sequence".to_string(),
                    reason: format!("创建嵌套序列失败: {err}"),
                })?;
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
                for clip in track.clips.iter().filter(|clip| selected_ids.contains(&clip.id)) {
                    let mut nested_clip = clip.clone();
                    nested_clip.position = clip.position.checked_sub(min_time)?;
                    nested_sequence.video_tracks[track_index].add_clip(nested_clip)?;
                }
            }
            for (track_index, track) in parent.audio_tracks.iter().enumerate() {
                for clip in track.clips.iter().filter(|clip| selected_ids.contains(&clip.id)) {
                    let mut nested_clip = clip.clone();
                    nested_clip.position = clip.position.checked_sub(min_time)?;
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
                transition.id = mondrian_core::AudioTransitionId::new();
                transition.left = left;
                transition.right = right;
                transition.sequence_range = mondrian_core::TimelineTimeRange::new(
                    transition.sequence_range.start.checked_sub(min_time)?,
                    transition.sequence_range.duration,
                )?;
                nested_sequence.audio_program.transitions.push(transition);
            }
            for mut transition in source_video_transitions {
                if !selected_ids.contains(&transition.left)
                    || !selected_ids.contains(&transition.right)
                {
                    continue;
                }
                transition.id = mondrian_core::VideoTransitionId::new();
                transition.properties.fork_author_identities();
                transition.sequence_range = mondrian_core::TimelineTimeRange::new(
                    transition.sequence_range.start.checked_sub(min_time)?,
                    transition.sequence_range.duration,
                )?;
                nested_sequence.video_transitions.push(transition);
            }
            nested_sequence.fork_clip_link_groups_for_sequence_duplicate();

            for track in &mut parent.video_tracks {
                track.clips.retain(|clip| !selected_ids.contains(&clip.id));
            }
            for track in &mut parent.audio_tracks {
                track.clips.retain(|clip| !selected_ids.contains(&clip.id));
            }
            parent.compact_structural_references();

            let target_video_track_index = target_video_track_index.unwrap_or(0);
            let duration = max_time.checked_sub(min_time)?;
            let mut nested_clip = Clip::new_nested_sequence(
                nested_sequence.id,
                min_time,
                duration,
                Some(nested_sequence.name.clone()),
            )?;
            let nested_clip_id = nested_clip.id;
            let has_nested_audio =
                nested_sequence.audio_tracks.iter().any(|track| !track.clips.is_empty());
            let nested_audio = if has_nested_audio {
                let mut audio_clip = Clip::new_nested_sequence(
                    nested_sequence.id,
                    min_time,
                    duration,
                    Some(nested_sequence.name.clone()),
                )?;
                let link_group = ClipLinkGroupId::new();
                nested_clip.link_group = Some(link_group);
                audio_clip.link_group = Some(link_group);
                Some(audio_clip)
            } else {
                None
            };
            parent.video_tracks[target_video_track_index].add_clip(nested_clip)?;
            if let Some(audio_clip) = nested_audio {
                let output_id = nested_sequence
                    .audio_program
                    .outputs
                    .first()
                    .map(|output| output.id)
                    .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                        step_id: "precompose_clips_as_sequence".to_owned(),
                        reason: "嵌套序列缺少音频 Program Output".to_owned(),
                    })?;
                let audio_track_id = parent.audio_tracks[target_audio_track_index.unwrap_or(0)].id;
                parent.add_nested_audio_clip(audio_track_id, audio_clip, output_id)?;
            }

            (parent.id, parent, nested_sequence, nested_clip_id)
        };

        let mut project_after = project_before.clone();
        let parent_sequence_id = after.id;
        *project_after.sequences.sequence_mut(parent_sequence_id).ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "precompose_clips_as_sequence".to_owned(),
                reason: "父序列在项目事务中丢失".to_owned(),
            }
        })? = after;
        project_after.sequences.add_sequence(nested_sequence)?;
        self.authoring
            .as_mut()
            .expect("authoring session checked above")
            .commit_project_snapshot("预合成为嵌套序列", project_before, project_after)?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(nested_clip_id)
    }

    // ─── 播放控制 ────────────────────────────

    pub fn default_export_input_path(&self) -> Option<PathBuf> {
        let seq = self.active_sequence()?;
        let library = self.asset_library()?;

        let mut candidates = Vec::new();
        for track in &seq.video_tracks {
            for clip in &track.clips {
                if clip.is_disabled {
                    continue;
                }
                if let Some(asset_id) = clip.media_asset_id() {
                    candidates.push((clip.position, asset_id));
                }
            }
        }

        candidates.sort_by_key(|(frame, _)| *frame);
        candidates.dedup_by_key(|(_, asset_id)| *asset_id);

        for (_, asset_id) in candidates {
            match library.get_asset(asset_id) {
                Ok(Some(asset)) if matches!(asset.kind, mondrian_assets::AssetKind::Video) => {
                    return Some(asset.path);
                }
                Ok(_) => {}
                Err(err) => {
                    tracing::debug!("读取导出输入素材失败 {}: {}", asset_id, err);
                }
            }
        }

        None
    }

    pub fn last_content_frame(&self) -> mondrian_core::Result<i64> {
        let Some(seq) = self.active_sequence() else {
            return Ok(0);
        };

        let end = seq.total_duration()?;
        Ok((sequence_frame_from_time(end, seq.settings.frame_rate)? - 1).max(0))
    }

    pub fn jump_to_start_frame(&mut self) {
        let current = self.current_frame();
        // 入/出点不影响播放逻辑，仅影响导出。
        if current == 0 {
            return;
        }
        self.seek(0);
    }

    pub fn jump_to_end_frame(&mut self) -> mondrian_core::Result<()> {
        let current = self.current_frame();
        // 入/出点不影响播放逻辑，仅影响导出。
        let target = self.last_content_frame()?.max(0);
        if current == target {
            return Ok(());
        }
        self.seek(target);
        Ok(())
    }

    pub fn step_prev_frame(&mut self) {
        let current = self.current_frame();
        if current > 0 {
            self.seek(current - 1);
        }
    }

    pub fn step_next_frame(&mut self) {
        self.seek(self.current_frame() + 1);
    }

    pub fn mark_in_at_current_frame(&mut self) -> mondrian_core::Result<()> {
        let current = self.current_frame().max(0);
        let before = self.active_sequence().cloned().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "mark_in_at_current_frame".to_owned(),
                reason: "当前无序列".to_owned(),
            }
        })?;
        let mut after = before.clone();
        after.mark_in(sequence_time_from_frame(current, after.time_base())?);
        let sequence_id = after.id;
        self.record_sequence_snapshot_command("标记入点", before, after)?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(())
    }

    pub fn mark_out_at_current_frame(&mut self) -> mondrian_core::Result<()> {
        let current = self.current_frame().max(0);
        let before = self.active_sequence().cloned().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "mark_out_at_current_frame".to_owned(),
                reason: "当前无序列".to_owned(),
            }
        })?;
        let mut after = before.clone();
        after.mark_out(sequence_time_from_frame(current, after.time_base())?);
        let sequence_id = after.id;
        self.record_sequence_snapshot_command("标记出点", before, after)?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(())
    }

    pub fn trim_clips_bulk_to_frame(
        &mut self,
        clip_ids: &[ClipId],
        edge: TrimEdge,
        target_frame: i64,
    ) -> mondrian_core::Result<usize> {
        if clip_ids.is_empty() {
            return Ok(0);
        }

        let (sequence_id, before, after, changed_count) = {
            let before = self.active_sequence().cloned().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "trim_clips_bulk_to_frame".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;
            let mut after = before.clone();
            let seq = &mut after;
            let mut targets = clip_ids.iter().copied().collect::<HashSet<_>>();
            expand_clip_link_groups(seq, &mut targets);
            let mut changed_count = 0usize;

            for clip_id in targets {
                match trim_clip_edge_internal(seq, clip_id, edge, target_frame) {
                    Ok(true) => {
                        changed_count += 1;
                    }
                    Ok(false) => {}
                    Err(mondrian_core::MondrianError::ClipNotFound { .. }) => continue,
                    Err(err) => return Err(err),
                }
            }

            if changed_count == 0 {
                return Ok(0);
            }

            compact_sequence_references(seq);

            (seq.id, before, after, changed_count)
        };

        let action = match edge {
            TrimEdge::In => "修剪入点",
            TrimEdge::Out => "修剪出点",
        };
        self.record_sequence_snapshot_command(action, before, after)?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(changed_count)
    }

    pub fn roll_cut_to_frame(
        &mut self,
        clip_id: ClipId,
        target_frame: i64,
    ) -> mondrian_core::Result<bool> {
        let (sequence_id, before, after, changed) = {
            let before = self.active_sequence().cloned().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "roll_cut_to_frame".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;
            let mut after = before.clone();
            let seq = &mut after;
            let changed = roll_cut_for_clip_internal(seq, clip_id, target_frame)?;
            if !changed {
                return Ok(false);
            }
            compact_sequence_references(seq);
            (seq.id, before, after, changed)
        };

        if changed {
            self.record_sequence_snapshot_command("滚动修剪", before, after)?;
            self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        }
        Ok(changed)
    }

    pub fn slip_clips_bulk_by_frames(
        &mut self,
        clip_ids: &[ClipId],
        delta_frames: i64,
    ) -> mondrian_core::Result<usize> {
        if clip_ids.is_empty() || delta_frames == 0 {
            return Ok(0);
        }

        let library = self.asset_library_handle().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "slip_clips_bulk_by_frames".to_string(),
                reason: "素材库未连接".to_string(),
            }
        })?;

        let (sequence_id, before, after, changed_count) = {
            let before = self.active_sequence().cloned().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "slip_clips_bulk_by_frames".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;
            let mut after = before.clone();
            let seq = &mut after;
            let mut targets = clip_ids.iter().copied().collect::<HashSet<_>>();
            expand_clip_link_groups(seq, &mut targets);
            let mut changed_count = 0usize;

            for clip_id in targets {
                match slip_clip_internal(seq, library.as_ref(), clip_id, delta_frames) {
                    Ok(true) => {
                        changed_count += 1;
                    }
                    Ok(false) => {}
                    Err(mondrian_core::MondrianError::ClipNotFound { .. }) => continue,
                    Err(err) => return Err(err),
                }
            }

            if changed_count == 0 {
                return Ok(0);
            }

            compact_sequence_references(seq);

            (seq.id, before, after, changed_count)
        };

        self.record_sequence_snapshot_command("滑移片段", before, after)?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(changed_count)
    }

    pub fn slide_clips_bulk_by_frames(
        &mut self,
        clip_ids: &[ClipId],
        delta_frames: i64,
    ) -> mondrian_core::Result<usize> {
        if clip_ids.is_empty() || delta_frames == 0 {
            return Ok(0);
        }

        let (sequence_id, before, after, changed_count) = {
            let before = self.active_sequence().cloned().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "slide_clips_bulk_by_frames".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;
            let mut after = before.clone();
            let seq = &mut after;
            let mut targets = clip_ids.iter().copied().collect::<HashSet<_>>();
            expand_clip_link_groups(seq, &mut targets);
            let mut changed_count = 0usize;

            for clip_id in targets {
                match slide_clip_internal(seq, clip_id, delta_frames) {
                    Ok(true) => {
                        changed_count += 1;
                    }
                    Ok(false) => {}
                    Err(mondrian_core::MondrianError::ClipNotFound { .. }) => continue,
                    Err(err) => return Err(err),
                }
            }

            if changed_count == 0 {
                return Ok(0);
            }

            compact_sequence_references(seq);

            (seq.id, before, after, changed_count)
        };

        self.record_sequence_snapshot_command("滑动片段", before, after)?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(changed_count)
    }

    pub fn split_clip_at_frame(
        &mut self,
        track_id: TrackId,
        is_video_track: bool,
        clip_id: ClipId,
        split_frame: i64,
    ) -> mondrian_core::Result<bool> {
        let (sequence_id, before, after) = {
            let before = self.active_sequence().cloned().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "split_clip".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;
            let mut after = before.clone();
            let seq = &mut after;
            let split_done = Self::split_clip_at_frame_internal(
                seq,
                track_id,
                is_video_track,
                clip_id,
                split_frame,
            )?;
            if !split_done {
                return Ok(false);
            }
            (seq.id, before, after)
        };

        self.record_sequence_snapshot_command("分割片段", before, after)?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(true)
    }

    pub fn split_at_playhead(&mut self) -> mondrian_core::Result<usize> {
        let frame = self.current_frame();
        let targets = {
            let seq = self.active_sequence().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "split_at_playhead".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;

            let current = sequence_time_from_frame(frame, seq.time_base())?;
            let mut targets: Vec<(TrackId, bool, ClipId)> = Vec::new();
            for track in &seq.video_tracks {
                if track.is_locked {
                    continue;
                }
                for clip in &track.clips {
                    if current > clip.position && current < clip.end_position()? {
                        targets.push((track.id, true, clip.id));
                    }
                }
            }
            for track in &seq.audio_tracks {
                if track.is_locked {
                    continue;
                }
                for clip in &track.clips {
                    if current > clip.position && current < clip.end_position()? {
                        targets.push((track.id, false, clip.id));
                    }
                }
            }

            targets
        };

        let (sequence_id, before, after, split_count) = {
            let before = self.active_sequence().cloned().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "split_at_playhead".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;
            let mut after = before.clone();
            let seq = &mut after;
            let mut processed = HashSet::new();
            let mut split_count = 0usize;

            for (track_id, is_video, clip_id) in targets {
                if !processed.insert(clip_id) {
                    continue;
                }
                if Self::split_clip_at_frame_internal(seq, track_id, is_video, clip_id, frame)? {
                    split_count += 1;
                }
            }

            (seq.id, before, after, split_count)
        };

        if split_count > 0 {
            self.record_sequence_snapshot_command("在播放头分割片段", before, after)?;
            self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        }

        Ok(split_count)
    }

    fn split_clip_at_frame_internal(
        seq: &mut Sequence,
        track_id: TrackId,
        is_video_track: bool,
        clip_id: ClipId,
        split_frame: i64,
    ) -> mondrian_core::Result<bool> {
        let Some((actual_track_id, actual_is_video, _)) = find_clip_track_lock(seq, clip_id) else {
            return Err(mondrian_core::MondrianError::ClipNotFound {
                clip_id: clip_id.to_string(),
            });
        };
        if actual_track_id != track_id || actual_is_video != is_video_track {
            return Err(mondrian_core::MondrianError::ClipNotFound {
                clip_id: clip_id.to_string(),
            });
        }
        let members = clip_link_group_member_ids(seq, clip_id);
        for member in &members {
            if let Some((member_track, _, true)) = find_clip_track_lock(seq, *member) {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: member_track.to_string(),
                });
            }
        }

        let time_base = seq.time_base();
        let mut right_ids = Vec::new();
        let mut primary_split = false;
        for member in members {
            if let Some(result) = split_clip_anywhere(seq, member, split_frame, time_base) {
                primary_split |= member == clip_id;
                right_ids.push(result.right_clip_id);
            }
        }
        if right_ids.len() >= 2 {
            let right_group = ClipLinkGroupId::new();
            for right_id in right_ids {
                if let Some(right) = find_clip_mut(seq, right_id) {
                    right.link_group = Some(right_group);
                }
            }
        }
        compact_sequence_references(seq);
        Ok(primary_split)
    }

    pub fn begin_drag_asset(
        &mut self,
        asset_id: AssetId,
        name: String,
        kind: AssetKind,
        duration: Duration,
        has_linked_audio: bool,
    ) {
        self.dragging_asset =
            Some(DraggingAsset { asset_id, name, kind, duration, has_linked_audio });
    }

    pub fn clear_dragging_asset(&mut self) {
        self.dragging_asset = None;
    }

    pub fn dragging_asset(&self) -> Option<&DraggingAsset> {
        self.dragging_asset.as_ref()
    }

    pub fn drop_dragging_asset_to_video_track(
        &mut self,
        track_id: mondrian_core::types::TrackId,
        timeline_frame: i64,
    ) -> mondrian_core::Result<ClipId> {
        self.drop_dragging_asset_to_video_track_with_mode(
            track_id,
            timeline_frame,
            ClipOverlapMode::Overwrite,
        )
    }

    pub fn drop_dragging_asset_to_video_track_with_mode(
        &mut self,
        track_id: mondrian_core::types::TrackId,
        timeline_frame: i64,
        overlap_mode: ClipOverlapMode,
    ) -> mondrian_core::Result<ClipId> {
        let dragging =
            self.dragging_asset.clone().ok_or(mondrian_core::MondrianError::Cancelled)?;

        if !matches!(
            dragging.kind,
            AssetKind::Video | AssetKind::AdjustmentLayer | AssetKind::SolidColor
        ) {
            return Err(mondrian_core::MondrianError::UnsupportedFormat {
                format: "仅支持将视频素材或调整图层拖到视频轨".to_string(),
            });
        }

        // Resolve media dimensions for auto-fit before borrowing seq.
        let media_dim = if dragging.kind == AssetKind::Video {
            self.asset_library()
                .and_then(|lib| lib.get_asset(dragging.asset_id).ok().flatten())
                .and_then(|asset| asset.media_info.primary_video().cloned())
                .map(|v| (v.width, v.height))
        } else {
            None
        };

        let (sequence_id, clip_id, start_frame, before, after) = {
            let before = self.active_sequence().cloned().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "timeline_drop".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;
            let mut after = before.clone();
            let seq = &mut after;
            let fps = seq.settings.frame_rate.to_f64();
            let duration_frames = ((dragging.duration.as_secs_f64() * fps).ceil() as i64).max(1);
            let time_base = seq.time_base();
            let start_frame = timeline_frame.max(0);
            let start_time = sequence_time_from_frame(start_frame, time_base)?;
            let duration = sequence_time_from_frame(duration_frames, time_base)?;

            let mut clip = if dragging.kind == AssetKind::AdjustmentLayer {
                Clip::new_adjustment_layer(dragging.asset_id, start_time, duration)?
            } else if dragging.kind == AssetKind::SolidColor {
                Clip::new_solid_color(
                    dragging.asset_id,
                    Color::from_hex(0x808080),
                    start_time,
                    duration,
                )?
            } else {
                Clip::new(dragging.asset_id, start_time, duration)?
            };
            clip.label = Some(dragging.name.clone());
            // Auto-fit: set anchor to media center, position to seq center, scale to fit.
            if let Some((mw, mh)) = media_dim {
                if mw > 0 && mh > 0 {
                    let seq_w = seq.settings.resolution.width.max(1) as f32;
                    let seq_h = seq.settings.resolution.height.max(1) as f32;
                    let fit_scale = (seq_w / mw as f32).min(seq_h / mh as f32);
                    clip.transform
                        .set_anchor_point(glam::Vec2::new(mw as f32 * 0.5, mh as f32 * 0.5));
                    clip.transform.set_scale(glam::Vec2::new(fit_scale, fit_scale));
                    clip.transform.set_position(glam::Vec2::new(seq_w * 0.5, seq_h * 0.5));
                }
            }
            let clip_id = clip.id;

            let should_create_linked_audio =
                dragging.kind == AssetKind::Video && dragging.has_linked_audio;

            let mut linked_audio_clip = if should_create_linked_audio {
                let mut audio_clip = Clip::new(dragging.asset_id, start_time, duration)?;
                audio_clip.label = Some(dragging.name.clone());
                let link_group = ClipLinkGroupId::new();
                clip.link_group = Some(link_group);
                audio_clip.link_group = Some(link_group);
                Some(audio_clip)
            } else {
                None
            };

            let track = seq.video_track_mut(track_id).ok_or_else(|| {
                mondrian_core::MondrianError::TrackNotFound { track_id: track_id.to_string() }
            })?;
            track.add_clip(clip)?;
            resolve_track_conflicts(track, clip_id, overlap_mode)?;

            if let Some(audio_clip) = linked_audio_clip.take() {
                let audio_clip_id = audio_clip.id;
                let target_video_index =
                    seq.video_tracks.iter().position(|t| t.id == track_id).ok_or_else(|| {
                        mondrian_core::MondrianError::TrackNotFound {
                            track_id: track_id.to_string(),
                        }
                    })?;
                ensure_audio_track_index(seq, target_video_index);
                if let Some(audio_track_id) =
                    seq.audio_tracks.get(target_video_index).map(|track| track.id)
                {
                    seq.add_media_audio_clip(
                        audio_track_id,
                        audio_clip,
                        AudioSourceComponentId::primary(),
                    )?;
                    let audio_track = seq.audio_track_mut(audio_track_id).ok_or_else(|| {
                        mondrian_core::MondrianError::TrackNotFound {
                            track_id: audio_track_id.to_string(),
                        }
                    })?;
                    resolve_track_conflicts(audio_track, audio_clip_id, overlap_mode)?;
                }
            }
            compact_sequence_references(seq);

            (seq.id, clip_id, start_frame, before, after)
        };

        self.record_sequence_snapshot_command("添加视频片段", before, after)?;
        self.event_bus.publish(AppEvent::ClipAdded { sequence_id, clip_id });
        self.seek(start_frame);
        self.clear_dragging_asset();
        Ok(clip_id)
    }

    pub fn drop_dragging_asset_to_audio_track(
        &mut self,
        track_id: mondrian_core::types::TrackId,
        timeline_frame: i64,
    ) -> mondrian_core::Result<ClipId> {
        self.drop_dragging_asset_to_audio_track_with_mode(
            track_id,
            timeline_frame,
            ClipOverlapMode::Overwrite,
        )
    }

    pub fn drop_dragging_asset_to_audio_track_with_mode(
        &mut self,
        track_id: mondrian_core::types::TrackId,
        timeline_frame: i64,
        overlap_mode: ClipOverlapMode,
    ) -> mondrian_core::Result<ClipId> {
        let dragging =
            self.dragging_asset.clone().ok_or(mondrian_core::MondrianError::Cancelled)?;

        if dragging.kind != AssetKind::Audio {
            return Err(mondrian_core::MondrianError::UnsupportedFormat {
                format: "仅支持将音频素材拖到音频轨".to_string(),
            });
        }

        let (sequence_id, clip_id, start_frame, before, after) = {
            let before = self.active_sequence().cloned().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "timeline_drop_audio".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;
            let mut after = before.clone();
            let seq = &mut after;
            let fps = seq.settings.frame_rate.to_f64();
            let duration_frames = ((dragging.duration.as_secs_f64() * fps).ceil() as i64).max(1);
            let time_base = seq.time_base();
            let start_frame = timeline_frame.max(0);

            let mut clip = Clip::new(
                dragging.asset_id,
                sequence_time_from_frame(start_frame, time_base)?,
                sequence_time_from_frame(duration_frames, time_base)?,
            )?;
            clip.label = Some(dragging.name.clone());
            let clip_id = clip.id;

            seq.add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())?;
            let track = seq.audio_track_mut(track_id).ok_or_else(|| {
                mondrian_core::MondrianError::TrackNotFound { track_id: track_id.to_string() }
            })?;
            resolve_track_conflicts(track, clip_id, overlap_mode)?;
            compact_sequence_references(seq);

            (seq.id, clip_id, start_frame, before, after)
        };

        self.record_sequence_snapshot_command("添加音频片段", before, after)?;
        self.event_bus.publish(AppEvent::ClipAdded { sequence_id, clip_id });
        self.seek(start_frame);
        self.clear_dragging_asset();
        Ok(clip_id)
    }

    pub fn move_clip_in_track(
        &mut self,
        track_id: TrackId,
        is_video_track: bool,
        clip_id: ClipId,
        timeline_frame: i64,
    ) -> mondrian_core::Result<()> {
        self.move_clip_in_track_with_mode(
            track_id,
            is_video_track,
            clip_id,
            timeline_frame,
            ClipOverlapMode::Overwrite,
        )
    }

    pub fn move_clip_in_track_with_mode(
        &mut self,
        track_id: TrackId,
        is_video_track: bool,
        clip_id: ClipId,
        timeline_frame: i64,
        overlap_mode: ClipOverlapMode,
    ) -> mondrian_core::Result<()> {
        self.move_clip_to_track_with_mode(
            track_id,
            is_video_track,
            clip_id,
            timeline_frame,
            overlap_mode,
        )
    }

    pub fn move_clip_to_track(
        &mut self,
        target_track_id: TrackId,
        is_video_track: bool,
        clip_id: ClipId,
        timeline_frame: i64,
    ) -> mondrian_core::Result<()> {
        self.move_clip_to_track_with_mode(
            target_track_id,
            is_video_track,
            clip_id,
            timeline_frame,
            ClipOverlapMode::Overwrite,
        )
    }

    pub fn move_clip_to_track_with_mode(
        &mut self,
        target_track_id: TrackId,
        is_video_track: bool,
        clip_id: ClipId,
        timeline_frame: i64,
        overlap_mode: ClipOverlapMode,
    ) -> mondrian_core::Result<()> {
        let before = self.active_sequence().cloned().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "move_clip".to_string(),
                reason: "当前无序列".to_string(),
            }
        })?;
        let mut after = before.clone();
        let seq = &mut after;

        let time_base = seq.time_base();
        let new_start = timeline_frame.max(0);
        let source_track_index =
            find_clip_track_index(seq, is_video_track, clip_id).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() }
            })?;

        let target_track_index = if is_video_track {
            seq.video_tracks.iter().position(|t| t.id == target_track_id).ok_or_else(|| {
                mondrian_core::MondrianError::TrackNotFound {
                    track_id: target_track_id.to_string(),
                }
            })?
        } else {
            seq.audio_tracks.iter().position(|t| t.id == target_track_id).ok_or_else(|| {
                mondrian_core::MondrianError::TrackNotFound {
                    track_id: target_track_id.to_string(),
                }
            })?
        };

        let primary_position = find_clip(seq, clip_id)
            .ok_or_else(|| mondrian_core::MondrianError::ClipNotFound {
                clip_id: clip_id.to_string(),
            })?
            .position;
        let primary_frame = primary_position
            .to_frame_position(seq.settings.frame_rate, FrameRounding::Nearest)?
            .frame;
        let frame_delta = new_start.saturating_sub(primary_frame);
        let track_delta = target_track_index as isize - source_track_index as isize;
        let members = clip_link_group_member_ids(seq, clip_id);
        let mut moves = Vec::<(ClipId, bool, usize, i64)>::with_capacity(members.len());
        for member in members {
            let (member_track_id, member_is_video, member_locked) =
                find_clip_track_lock(seq, member).ok_or_else(|| {
                    mondrian_core::MondrianError::ClipNotFound { clip_id: member.to_string() }
                })?;
            if member_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: member_track_id.to_string(),
                });
            }
            let member_source_index = find_clip_track_index(seq, member_is_video, member)
                .ok_or_else(|| mondrian_core::MondrianError::ClipNotFound {
                    clip_id: member.to_string(),
                })?;
            let track_count = if member_is_video {
                seq.video_tracks.len()
            } else {
                seq.audio_tracks.len()
            };
            let member_target_index = if member == clip_id {
                target_track_index
            } else {
                (member_source_index as isize + track_delta)
                    .clamp(0, track_count.saturating_sub(1) as isize) as usize
            };
            let target_track = if member_is_video {
                &seq.video_tracks[member_target_index]
            } else {
                &seq.audio_tracks[member_target_index]
            };
            if target_track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: target_track.id.to_string(),
                });
            }
            let member_frame = find_clip(seq, member)
                .ok_or_else(|| mondrian_core::MondrianError::ClipNotFound {
                    clip_id: member.to_string(),
                })?
                .position
                .to_frame_position(seq.settings.frame_rate, FrameRounding::Nearest)?
                .frame;
            let target_frame = member_frame.saturating_add(frame_delta);
            if target_frame < 0 {
                return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "move_clip".to_owned(),
                    reason: "移动会使链接组成员越过序列零点".to_owned(),
                });
            }
            moves.push((member, member_is_video, member_target_index, target_frame));
        }

        for (member, member_is_video, member_target_index, target_frame) in &moves {
            if !move_existing_clip_to_track_index(
                seq,
                *member_is_video,
                *member,
                *member_target_index,
                *target_frame,
                time_base,
            ) {
                return Err(mondrian_core::MondrianError::ClipNotFound {
                    clip_id: member.to_string(),
                });
            }
        }

        let focus_ids = moves.iter().map(|(id, _, _, _)| *id).collect::<HashSet<_>>();
        for track in &mut seq.video_tracks {
            apply_track_conflicts_for_focus_group(track, &focus_ids, overlap_mode)?;
        }
        for track in &mut seq.audio_tracks {
            apply_track_conflicts_for_focus_group(track, &focus_ids, overlap_mode)?;
        }

        for track in &mut seq.video_tracks {
            track.clips.sort_by_key(|c| c.position);
        }
        for track in &mut seq.audio_tracks {
            track.clips.sort_by_key(|c| c.position);
        }
        compact_sequence_references(seq);
        let sequence_id = seq.id;
        self.record_sequence_snapshot_command("移动片段", before, after)?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(())
    }

    pub fn move_clip_group_by_delta_with_mode(
        &mut self,
        anchors: &[(ClipId, i64)],
        delta_frames: i64,
        overlap_mode: ClipOverlapMode,
    ) -> mondrian_core::Result<usize> {
        if anchors.is_empty() {
            return Ok(0);
        }

        let before = self.active_sequence().cloned().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "move_clip_group_by_delta_with_mode".to_string(),
                reason: "当前无序列".to_string(),
            }
        })?;
        let mut after = before.clone();
        let seq = &mut after;

        let mut anchor_frames = HashMap::<ClipId, i64>::new();
        for (clip_id, start_frame) in anchors {
            anchor_frames.entry(*clip_id).or_insert(*start_frame);
        }
        let mut member_ids = anchor_frames.keys().copied().collect::<HashSet<_>>();
        expand_clip_link_groups(seq, &mut member_ids);

        let mut target_positions = Vec::<(ClipId, i64)>::with_capacity(member_ids.len());
        for clip_id in member_ids {
            let current_frame = find_clip(seq, clip_id)
                .ok_or_else(|| mondrian_core::MondrianError::ClipNotFound {
                    clip_id: clip_id.to_string(),
                })?
                .position
                .to_frame_position(seq.settings.frame_rate, FrameRounding::Nearest)?
                .frame;
            let start_frame = anchor_frames.get(&clip_id).copied().unwrap_or(current_frame);

            let (track_id, _is_video, is_locked) =
                find_clip_track_lock(seq, clip_id).ok_or_else(|| {
                    mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() }
                })?;
            if is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track_id.to_string(),
                });
            }

            let target = start_frame.saturating_add(delta_frames);
            if target < 0 {
                return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "move_clip_group_by_delta_with_mode".to_owned(),
                    reason: "移动会使链接组成员越过序列零点".to_owned(),
                });
            }
            target_positions.push((clip_id, target));
        }

        if target_positions.is_empty() {
            return Ok(0);
        }

        let mut changed_count = 0usize;
        for (clip_id, target_frame) in &target_positions {
            if set_clip_position(seq, *clip_id, *target_frame) {
                changed_count += 1;
            }
        }
        if changed_count == 0 {
            return Ok(0);
        }

        for track in &mut seq.video_tracks {
            track.clips.sort_by_key(|c| c.position);
        }
        for track in &mut seq.audio_tracks {
            track.clips.sort_by_key(|c| c.position);
        }

        let focus_ids: HashSet<ClipId> = target_positions.iter().map(|(id, _)| *id).collect();
        for track in &mut seq.video_tracks {
            apply_track_conflicts_for_focus_group(track, &focus_ids, overlap_mode)?;
        }
        for track in &mut seq.audio_tracks {
            apply_track_conflicts_for_focus_group(track, &focus_ids, overlap_mode)?;
        }

        compact_sequence_references(seq);
        let sequence_id = seq.id;
        self.record_sequence_snapshot_command("移动多个片段", before, after)?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(changed_count)
    }

    // ── Mask manipulation ──────────────────────────────────────────────

    pub fn add_mask_to_clip(
        &mut self,
        selection: SelectedClipRef,
        name: &str,
    ) -> mondrian_core::Result<MaskId> {
        let (sequence_id, id) = self.commit_active_sequence_edit("添加蒙版", |sequence| {
            let clip = find_clip_mut_by_selection(sequence, selection).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                }
            })?;
            let component = MaskComponent::new(name.to_string(), MaskKeyframe::default());
            let id = component.id;
            clip.masks.push(component);
            Ok((sequence.id, id))
        })?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(id)
    }

    pub fn remove_mask_from_clip(
        &mut self,
        selection: SelectedClipRef,
        mask_id: MaskId,
    ) -> mondrian_core::Result<()> {
        let before = self.active_sequence().cloned().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "remove_mask".to_string(),
                reason: "当前无序列".to_string(),
            }
        })?;
        let mut after = before.clone();
        let clip = find_clip_mut_by_selection(&mut after, selection).ok_or_else(|| {
            mondrian_core::MondrianError::ClipNotFound { clip_id: selection.clip_id.to_string() }
        })?;
        let removed = {
            let len_before = clip.masks.len();
            clip.masks.retain(|m| m.id != mask_id);
            clip.masks.len() < len_before
        };
        if removed {
            let sequence_id = after.id;
            self.record_sequence_snapshot_command("删除蒙版", before, after)?;
            self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        }
        Ok(())
    }

    pub fn set_mask_enabled(
        &mut self,
        selection: SelectedClipRef,
        mask_id: MaskId,
        enabled: bool,
    ) -> mondrian_core::Result<()> {
        let before = self.active_sequence().cloned().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "set_mask_enabled".to_string(),
                reason: "当前无序列".to_string(),
            }
        })?;
        let mut after = before.clone();
        let clip = find_clip_mut_by_selection(&mut after, selection).ok_or_else(|| {
            mondrian_core::MondrianError::ClipNotFound { clip_id: selection.clip_id.to_string() }
        })?;
        let mask = clip.masks.iter_mut().find(|mask| mask.id == mask_id).ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "set_mask_enabled".to_owned(),
                reason: format!("mask {mask_id} not found on clip"),
            }
        })?;
        if mask.enabled == enabled {
            return Ok(());
        }
        mask.enabled = enabled;
        let sequence_id = after.id;
        self.record_sequence_snapshot_command("切换蒙版启用", before, after)?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(())
    }

    /// Toggle shape animation for a mask. When enabled, a second shape keyframe is
    /// added at the current time (copying the current static shape). When disabled,
    /// all keyframes except the one at `time` (or the first) are removed.
    pub fn set_mask_shape_animation_enabled(
        &mut self,
        selection: SelectedClipRef,
        mask_id: MaskId,
        enabled: bool,
        time: TimelineTime,
    ) -> mondrian_core::Result<bool> {
        let before = self.active_sequence().cloned().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "set_mask_shape_animation".to_string(),
                reason: "当前无序列".to_string(),
            }
        })?;
        let mut after = before.clone();
        let clip = find_clip_mut_by_selection(&mut after, selection).ok_or_else(|| {
            mondrian_core::MondrianError::ClipNotFound { clip_id: selection.clip_id.to_string() }
        })?;
        let mask = clip.masks.iter_mut().find(|mask| mask.id == mask_id).ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "set_mask_shape_animation".to_owned(),
                reason: format!("mask {mask_id} not found on clip"),
            }
        })?;
        if enabled == mask.shape_animation_enabled {
            return Ok(false);
        }
        if enabled {
            // Snapshot current shape as a second keyframe at current time.
            let current_shape = mask.evaluate_at(time).shape;
            mask.shape_keyframes.push((time, current_shape));
            mask.shape_keyframes.sort_by_key(|(t, _)| *t);
            mask.shape_animation_enabled = true;
        } else {
            // Keep only the shape at `time` (or the first shape) as the static shape.
            let kept = mask
                .shape_keyframes
                .iter()
                .find(|(t, _)| *t == time)
                .or_else(|| mask.shape_keyframes.first())
                .map(|(_, s)| s.clone())
                .unwrap_or(MaskShape::default());
            mask.shape_keyframes.clear();
            mask.shape_keyframes.push((TimelineTime::ZERO, kept));
            mask.shape_animation_enabled = false;
        }
        let sequence_id = after.id;
        self.record_sequence_snapshot_command("切换蒙版形状动画", before, after)?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(true)
    }

    /// Update or insert a mask keyframe at the given time.
    /// Shape is stored in `shape_keyframes`; scalar properties go to `PropertyBag`.
    pub fn set_mask_keyframe(
        &mut self,
        selection: SelectedClipRef,
        mask_id: MaskId,
        keyframe: MaskKeyframe,
        time: TimelineTime,
    ) -> mondrian_core::Result<bool> {
        let before = self.active_sequence().cloned().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "set_mask_keyframe".to_string(),
                reason: "当前无序列".to_string(),
            }
        })?;
        let mut after = before.clone();
        let clip = find_clip_mut_by_selection(&mut after, selection).ok_or_else(|| {
            mondrian_core::MondrianError::ClipNotFound { clip_id: selection.clip_id.to_string() }
        })?;
        let mask = clip.masks.iter_mut().find(|mask| mask.id == mask_id).ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "set_mask_keyframe".to_owned(),
                reason: format!("mask {mask_id} not found on clip"),
            }
        })?;
        // Shape: write to shape_keyframes.
        if let Some(pos) = mask.shape_keyframes.iter().position(|(t, _)| *t == time) {
            mask.shape_keyframes[pos] = (time, keyframe.shape);
        } else {
            mask.shape_keyframes.push((time, keyframe.shape));
            mask.shape_keyframes.sort_by_key(|(t, _)| *t);
        }
        // Scalar properties: write to PropertyBag.
        use mondrian_core::automation::PropertyValue;
        use mondrian_effects::mask::{
            MASK_PROP_EXPANSION, MASK_PROP_FEATHER, MASK_PROP_INVERT, MASK_PROP_MASK_OP,
            MASK_PROP_OPACITY,
        };
        mask.properties.write_value(
            MASK_PROP_FEATHER,
            time,
            PropertyValue::Float(keyframe.feather),
            InterpolationType::Linear,
        )?;
        mask.properties.write_value(
            MASK_PROP_OPACITY,
            time,
            PropertyValue::Float(keyframe.opacity),
            InterpolationType::Linear,
        )?;
        mask.properties.write_value(
            MASK_PROP_EXPANSION,
            time,
            PropertyValue::Float(keyframe.expansion),
            InterpolationType::Linear,
        )?;
        mask.properties.write_value(
            MASK_PROP_INVERT,
            time,
            PropertyValue::Bool(keyframe.invert),
            InterpolationType::Hold,
        )?;
        mask.properties.write_value(
            MASK_PROP_MASK_OP,
            time,
            PropertyValue::Text(keyframe.mask_op.as_str().to_string()),
            InterpolationType::Hold,
        )?;
        let sequence_id = after.id;
        self.record_sequence_snapshot_command("修改蒙版", before, after)?;
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        Ok(true)
    }
}
