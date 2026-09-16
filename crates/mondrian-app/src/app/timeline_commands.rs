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

/// Estimate the Clip's total source extent on the Sequence evaluation grid
/// from its Asset probe evidence, when the library can prove one.
fn estimate_asset_total_source_frames(
    library: &AssetLibrary,
    clip: &Clip,
    time_base: Rational,
) -> Option<i64> {
    if clip.is_adjustment_layer() {
        return None;
    }

    let asset_id = clip.media_asset_id()?;
    let asset = match library.get_asset(asset_id) {
        Ok(Some(asset)) => asset,
        Ok(None) => return None,
        Err(err) => {
            tracing::debug!("读取素材时长失败 {}: {}", asset_id, err);
            return None;
        }
    };

    let media_probe = asset.media_probe()?;
    let frames_from_stream = media_probe.estimated_frames().map(|v| v as i64).filter(|v| *v > 0);
    if frames_from_stream.is_some() {
        return frames_from_stream;
    }

    let duration_secs = media_probe.duration.as_secs_f64();
    if duration_secs <= 0.0 {
        return None;
    }

    let frame_duration_secs = time_base.to_f64();
    if frame_duration_secs <= f64::EPSILON {
        return None;
    }

    Some((duration_secs / frame_duration_secs).ceil() as i64)
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
        self.switch_active_sequence_internal(sequence_id, SequenceNavigationIntent::ReplaceRoot)
    }

    fn switch_active_sequence_internal(
        &mut self,
        sequence_id: SequenceId,
        intent: SequenceNavigationIntent,
    ) -> mondrian_core::Result<()> {
        let session = self.authoring.as_ref().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "switch_active_sequence".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            }
        })?;
        if session.sequence(sequence_id).is_none() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "switch_active_sequence".to_owned(),
                reason: format!("序列不存在: {sequence_id}"),
            });
        }
        self.stop()?;
        self.authoring
            .as_mut()
            .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "switch_active_sequence".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            })?
            .switch_active_sequence(sequence_id, intent)?;
        self.settle_preview_access_source();
        Ok(())
    }

    /// Whether the active Sequence owns a placement that references `sequence_id`.
    pub fn can_open_nested_sequence(&self, sequence_id: SequenceId) -> bool {
        let Some(session) = self.authoring.as_ref() else {
            return false;
        };
        let active_id = session.document().sequences.active_sequence_id;
        if active_id == sequence_id || session.sequence(sequence_id).is_none() {
            return false;
        }
        session.sequence(active_id).is_some_and(|sequence| {
            sequence
                .video_tracks
                .iter()
                .chain(&sequence.audio_tracks)
                .flat_map(|track| &track.clips)
                .any(|clip| clip.nested_sequence_id() == Some(sequence_id))
        })
    }

    pub fn open_nested_sequence(&mut self, sequence_id: SequenceId) -> mondrian_core::Result<()> {
        if !self.can_open_nested_sequence(sequence_id) {
            return Err(mondrian_core::MondrianError::ActionNotExecuted {
                action: "open_nested_sequence".to_owned(),
                reason: format!(
                    "active Sequence does not contain a nested placement for {sequence_id}"
                ),
            });
        }
        self.switch_active_sequence_internal(sequence_id, SequenceNavigationIntent::EnterNested)
    }

    pub fn return_to_parent_sequence(&mut self) -> mondrian_core::Result<bool> {
        let can_return = self
            .authoring
            .as_ref()
            .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "return_to_parent_sequence".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            })?
            .navigation_stack()
            .last()
            .is_some();
        if !can_return {
            return Ok(false);
        }
        self.stop()?;
        let returned = self
            .authoring
            .as_mut()
            .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "return_to_parent_sequence".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            })?
            .return_to_parent_sequence()?;
        if returned {
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
        if let Some(commit) = session.commit_project_snapshot("设置默认序列", before, after)?
        {
            self.consume_authoring_commit(commit);
        }
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
        let before = self
            .authoring
            .as_ref()
            .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "duplicate_sequence".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            })?
            .document()
            .clone();
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
        self.stop()?;
        let session = self.authoring.as_mut().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "duplicate_sequence".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            }
        })?;
        let commit = session.commit_project_snapshot("复制序列", before, after)?;
        session.switch_active_sequence(duplicated_id, SequenceNavigationIntent::ReplaceRoot)?;
        if let Some(commit) = commit {
            self.consume_authoring_commit(commit);
        }
        self.settle_preview_access_source();
        Ok(duplicated_id)
    }

    pub fn delete_sequence(&mut self, sequence_id: SequenceId) -> mondrian_core::Result<()> {
        let before = self
            .authoring
            .as_ref()
            .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "delete_sequence".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            })?
            .document()
            .clone();
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
            self.stop()?;
        }
        let session = self.authoring.as_mut().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "delete_sequence".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            }
        })?;
        if let Some(commit) = session.commit_project_snapshot("删除序列", before, after)? {
            self.consume_authoring_commit(commit);
        }
        if active_changed {
            self.settle_preview_access_source();
        }
        Ok(())
    }

    pub fn update_active_sequence_settings(
        &mut self,
        settings: mondrian_timeline::sequence::SequenceSettings,
    ) -> mondrian_core::Result<()> {
        settings.validate_with_color_environment(self.project_color_environment())?;
        let sequence_id = self.active_sequence_id().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "update_active_sequence_settings".to_string(),
                reason: "当前无项目".to_string(),
            }
        })?;
        self.stop()?;
        self.commit_sequence_edit(sequence_id, "修改序列设置", move |sequence| {
            sequence.apply_settings(settings)
        })?;
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
        let name = name.to_owned();
        let active = self.active_sequence_id() == Some(sequence_id);
        if active {
            self.stop()?;
        }
        self.commit_sequence_edit(sequence_id, "修改序列设置", move |sequence| {
            sequence.name = name;
            sequence.apply_settings(settings)
        })?;
        if active {
            self.settle_preview_access_source();
        }
        Ok(())
    }

    pub(super) fn commit_sequence_edit<T>(
        &mut self,
        sequence_id: SequenceId,
        description: impl Into<String>,
        edit: impl FnOnce(&mut Sequence) -> mondrian_core::Result<T>,
    ) -> mondrian_core::Result<T> {
        let (value, commit) = self
            .authoring
            .as_mut()
            .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "commit_sequence_edit".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            })?
            .edit_sequence(sequence_id, description, edit)?;
        if let Some(commit) = commit {
            self.consume_authoring_commit(commit);
        }
        Ok(value)
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
        if let Some(commit) = commit {
            self.consume_authoring_commit(commit);
        }
        Ok(value)
    }

    pub(super) fn commit_project_snapshot_command(
        &mut self,
        description: impl Into<String>,
        before: mondrian_project::ProjectDocument,
        after: mondrian_project::ProjectDocument,
    ) -> mondrian_core::Result<()> {
        let commit = self
            .authoring
            .as_mut()
            .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "commit_project_snapshot_command".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            })?
            .commit_project_snapshot(description, before, after)?;
        if let Some(commit) = commit {
            self.consume_authoring_commit(commit);
        }
        Ok(())
    }

    pub(super) fn consume_authoring_commit(
        &mut self,
        commit: mondrian_editor_state::AuthoringCommit,
    ) {
        if !commit.undo_retained {
            self.set_status_hint(
                "编辑已提交但未保留撤销记录；此前撤销段已清除以避免跨越未记录状态",
                true,
            );
        }

        let mut invalidated_sequence_ids =
            commit.changed_sequence_ids.into_iter().collect::<BTreeSet<_>>();
        if commit.project_wide {
            invalidated_sequence_ids.extend(self.sequences().iter().map(|sequence| sequence.id));
        }
        for sequence_id in invalidated_sequence_ids {
            self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        }
        self.synchronize_audio_idle_warmup_binding();
        self.reconcile_timeline_targeting();
    }

    pub fn undo_timeline(&mut self) -> mondrian_core::Result<bool> {
        if !self.can_undo_action() {
            return Ok(false);
        }
        self.stop()?;
        let commit = self.authoring.as_mut().map(AuthoringSession::undo).transpose()?.flatten();
        let Some(commit) = commit else {
            return Ok(false);
        };
        self.consume_authoring_commit(commit);
        self.settle_preview_access_source();
        Ok(true)
    }

    pub fn redo_timeline(&mut self) -> mondrian_core::Result<bool> {
        if !self.can_redo_action() {
            return Ok(false);
        }
        self.stop()?;
        let commit = self.authoring.as_mut().map(AuthoringSession::redo).transpose()?.flatten();
        let Some(commit) = commit else {
            return Ok(false);
        };
        self.consume_authoring_commit(commit);
        self.settle_preview_access_source();
        Ok(true)
    }

    pub fn close_project(&mut self) -> anyhow::Result<()> {
        if self.project_close_blocks_actions() {
            anyhow::bail!("Project close is already active or fail-closed");
        }
        self.stop().map_err(|error| anyhow::anyhow!(error.to_string()))?;
        self.prepare_project_session_close()?;
        self.finalize_project_close_state();
        Ok(())
    }

    /// Remove the already-quiesced Authoring Session and invalidate every
    /// Project-scoped execution Adapter.
    ///
    /// Persistence ownership must be retired before this method is called.
    pub(super) fn finalize_project_close_state(&mut self) {
        let _ = self.reference_output.retire();
        self.audio_idle_warmup.set_dispatch_enabled(false);
        self.audio_idle_warmup.bind_authoring(None);
        self.visual_tracking.cancel_all();
        self.proxy_generation.bind_project(None);
        self.media_import.bind_project(None);
        self.media_import_batches.clear();
        self.media_asset_mutations.bind_project(None);
        #[cfg(test)]
        mondrian_media::clear_thread_local_preview_decode_session();
        self.authoring = None;
        self.gallery_comparison = None;
        self.audio_monitoring.reset();
        self.collect_released_project_libraries();
        self.project_runtime_lease = None;
        self.manual_project_file_destination = None;
        self.manual_project_file_applied_request = None;
        self.clear_timeline_targeting();
        self.autosave_in_flight_request = None;
        self.settle_preview_access_source();
        self.dragging_asset = None;
        self.clear_status_hint();
        self.project_close_fault = None;
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

    /// Resolve the default Timeline span for a visual source without intrinsic duration.
    pub fn default_visual_placement_duration_frames(&self) -> mondrian_core::Result<i64> {
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
        Ok(((DEFAULT_VISUAL_PLACEMENT_DURATION_SECS * fps).round() as i64).max(1))
    }

    /// Resolve the drag payload duration for a visual source without intrinsic duration.
    pub fn default_visual_placement_drag_duration(&self) -> mondrian_core::Result<Duration> {
        let fps = self
            .active_sequence()
            .map(|seq| seq.settings.frame_rate.to_f64())
            .unwrap_or(25.0);
        let secs = self.default_visual_placement_duration_frames()? as f64 / fps.max(1.0);
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
            if let Some(suffix) = folder.name.strip_prefix("文件夹 ")
                && let Ok(number) = suffix.trim().parse::<usize>()
            {
                next = next.max(number + 1);
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

    /// Retire one record from ordinary Asset Library membership.
    ///
    /// Timeline placements, inactive/nested Sequences, Project proxy intent,
    /// and Undo/Redo snapshots retain their strong `AssetId` reference. The
    /// complete Library record remains resolvable for execution and relink.
    pub fn retire_asset_from_library(&mut self, asset_id: AssetId) -> mondrian_core::Result<()> {
        let library = self.asset_library().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "delete_asset".to_string(),
                reason: "素材库未连接".to_string(),
            }
        })?;
        library.retire_assets(&[asset_id])?;
        self.event_bus
            .publish(mondrian_core::events::AppEvent::AssetRetired { asset_id });
        self.event_bus.publish(mondrian_core::events::AppEvent::AssetLibraryReloaded);
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

        if let Some(folder_id) = folder_id
            && !library.folder_exists(folder_id)?
        {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "create_adjustment_layer_asset".to_string(),
                reason: format!("目标素材文件夹不存在：{folder_id}"),
            });
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
        if let Some(folder_id) = folder_id
            && !library.folder_exists(folder_id)?
        {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "create_solid_color_asset".to_string(),
                reason: format!("目标素材文件夹不存在：{folder_id}"),
            });
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
            self.default_visual_placement_drag_duration()?,
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
            super::timeline_insert::DEFAULT_SOLID_COLOR_CLIP_COLOR,
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
        let default_duration_secs = self.default_visual_placement_drag_duration()?.as_secs_f64();

        let (_sequence_id, clip_id) =
            self.commit_active_sequence_edit("创建纯色层", |seq| {
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
                seq.compact_structural_references();
                let sequence_id = seq.id;
                Ok((sequence_id, clip_id))
            })?;

        self.set_status_hint(format!("已创建纯色层: {}", asset_name), false);
        Ok(clip_id)
    }

    pub fn remove_track(&mut self, track_id: TrackId, is_video: bool) -> mondrian_core::Result<()> {
        self.commit_active_sequence_edit("删除轨道", |seq| {
            if is_video {
                seq.remove_video_track(track_id)?;
            } else {
                seq.remove_audio_track(track_id)?;
            }
            seq.compact_structural_references();
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
            seq.compact_structural_references();
            Ok(())
        })?;

        self.prune_selection_to_active_sequence();
        Ok(())
    }

    pub fn move_track(
        &mut self,
        track_id: TrackId,
        placement: mondrian_timeline::TrackRelativePlacement,
    ) -> mondrian_core::Result<bool> {
        let sequence = self.active_sequence().ok_or_else(|| {
            mondrian_core::MondrianError::ActionNotExecuted {
                action: "track_move".to_owned(),
                reason: "there is no active Sequence".to_owned(),
            }
        })?;
        if !sequence.track_relative_placement_would_change(track_id, placement)? {
            return Ok(false);
        }
        let changed = self.commit_active_sequence_edit("移动轨道", |sequence| {
            sequence.reorder_track_relative(track_id, placement)
        })?;
        Ok(changed)
    }

    pub fn set_track_visible(
        &mut self,
        track_id: TrackId,
        visible: bool,
    ) -> mondrian_core::Result<bool> {
        let sequence = self.active_sequence().ok_or_else(|| {
            mondrian_core::MondrianError::ActionNotExecuted {
                action: "track_set_author_control".to_owned(),
                reason: "there is no active Sequence".to_owned(),
            }
        })?;
        let Some(track) = sequence.video_tracks.iter().find(|track| track.id == track_id) else {
            if sequence.audio_tracks.iter().any(|track| track.id == track_id) {
                return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "set_track_visible".to_owned(),
                    reason: "visibility is defined only for video Tracks".to_owned(),
                });
            }
            return Err(mondrian_core::MondrianError::TrackNotFound {
                track_id: track_id.to_string(),
            });
        };
        if track.is_visible == visible {
            return Ok(false);
        }
        self.commit_active_sequence_edit("切换轨道可见性", |seq| {
            let track = seq.video_track_mut(track_id).ok_or_else(|| {
                mondrian_core::MondrianError::TrackNotFound { track_id: track_id.to_string() }
            })?;
            track.is_visible = visible;
            Ok(())
        })?;
        Ok(true)
    }

    pub fn set_track_muted(
        &mut self,
        track_id: TrackId,
        muted: bool,
    ) -> mondrian_core::Result<bool> {
        let sequence = self.active_sequence().ok_or_else(|| {
            mondrian_core::MondrianError::ActionNotExecuted {
                action: "track_set_author_control".to_owned(),
                reason: "there is no active Sequence".to_owned(),
            }
        })?;
        let Some(track) = sequence.audio_tracks.iter().find(|track| track.id == track_id) else {
            if sequence.video_tracks.iter().any(|track| track.id == track_id) {
                return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "set_track_muted".to_owned(),
                    reason: "mute is defined only for audio Tracks".to_owned(),
                });
            }
            return Err(mondrian_core::MondrianError::TrackNotFound {
                track_id: track_id.to_string(),
            });
        };
        if track.is_muted == muted {
            return Ok(false);
        }
        self.commit_active_sequence_edit("切换轨道静音", |seq| {
            let track = seq.audio_track_mut(track_id).ok_or_else(|| {
                mondrian_core::MondrianError::TrackNotFound { track_id: track_id.to_string() }
            })?;
            track.is_muted = muted;
            Ok(())
        })?;
        Ok(true)
    }

    pub fn set_track_locked(
        &mut self,
        track_id: TrackId,
        locked: bool,
    ) -> mondrian_core::Result<bool> {
        let sequence = self.active_sequence().ok_or_else(|| {
            mondrian_core::MondrianError::ActionNotExecuted {
                action: "track_set_author_control".to_owned(),
                reason: "there is no active Sequence".to_owned(),
            }
        })?;
        let track = sequence
            .video_tracks
            .iter()
            .chain(&sequence.audio_tracks)
            .find(|track| track.id == track_id)
            .ok_or_else(|| mondrian_core::MondrianError::TrackNotFound {
                track_id: track_id.to_string(),
            })?;
        if track.is_locked == locked {
            return Ok(false);
        }
        self.commit_active_sequence_edit("切换轨道锁定", |seq| {
            let track = seq
                .video_tracks
                .iter_mut()
                .chain(seq.audio_tracks.iter_mut())
                .find(|track| track.id == track_id)
                .ok_or_else(|| mondrian_core::MondrianError::TrackNotFound {
                    track_id: track_id.to_string(),
                })?;
            track.is_locked = locked;
            Ok(())
        })?;
        Ok(true)
    }

    pub fn set_clips_disabled_bulk(
        &mut self,
        selections: &[(TrackId, bool, ClipId)],
        disabled: bool,
    ) -> mondrian_core::Result<usize> {
        if selections.is_empty() {
            return Ok(0);
        }

        let sequence_id = self.active_sequence_id().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "set_clips_disabled_bulk".to_string(),
                reason: "当前无项目".to_string(),
            }
        })?;
        let action = if disabled {
            "禁用片段"
        } else {
            "启用片段"
        };
        self.commit_sequence_edit(sequence_id, action, |seq| {
            let mut clip_ids: HashSet<ClipId> = selections.iter().map(|(_, _, id)| *id).collect();
            expand_clip_selection_units(seq, &mut clip_ids);

            if clip_ids.is_empty() {
                return Ok(0);
            }

            for clip_id in &clip_ids {
                if let Some(location) = seq.clip_track_location(*clip_id)
                    && location.is_locked
                {
                    return Err(mondrian_core::MondrianError::TrackLocked {
                        track_id: location.track_id.to_string(),
                    });
                }
            }

            let mut changed_count = 0usize;
            for clip_id in clip_ids {
                if seq.set_clip_disabled(clip_id, disabled) {
                    changed_count += 1;
                }
            }

            if changed_count == 0 {
                return Ok(0);
            }

            seq.compact_structural_references();
            Ok(changed_count)
        })
    }

    pub fn remove_clip(
        &mut self,
        track_id: TrackId,
        is_video_track: bool,
        clip_id: ClipId,
    ) -> mondrian_core::Result<()> {
        let sequence_id = self.active_sequence_id().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "remove_clip".to_string(),
                reason: "当前无项目".to_string(),
            }
        })?;
        self.commit_sequence_edit(sequence_id, "删除片段", |seq| {
            let group_members = clip_selection_unit(seq, clip_id).unwrap_or_default();
            if group_members.is_empty() {
                return Err(mondrian_core::MondrianError::ClipNotFound {
                    clip_id: clip_id.to_string(),
                });
            }
            let location = seq.clip_track_location(clip_id).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() }
            })?;
            if location.track_id != track_id || location.is_video_track != is_video_track {
                return Err(mondrian_core::MondrianError::ClipNotFound {
                    clip_id: clip_id.to_string(),
                });
            }
            for member in &group_members {
                if let Some(member_location) = seq.clip_track_location(*member)
                    && member_location.is_locked
                {
                    return Err(mondrian_core::MondrianError::TrackLocked {
                        track_id: member_location.track_id.to_string(),
                    });
                }
            }
            for member in group_members {
                let _ = seq.remove_clip_anywhere(member);
            }
            seq.compact_structural_references();
            Ok(())
        })?;
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

        let sequence_id = self.active_sequence_id().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "remove_clips_bulk".to_string(),
                reason: "当前无项目".to_string(),
            }
        })?;
        self.commit_sequence_edit(sequence_id, "删除多个片段", |seq| {
            let mut removed_count = 0usize;
            let mut selected_ids: HashSet<ClipId> =
                selections.iter().map(|(_, _, id)| *id).collect();
            expand_clip_selection_units(seq, &mut selected_ids);

            let mut by_track: HashMap<(TrackId, bool), HashSet<ClipId>> = HashMap::new();
            for clip_id in &selected_ids {
                let Some(location) = seq.clip_track_location(*clip_id) else {
                    continue;
                };
                by_track
                    .entry((location.track_id, location.is_video_track))
                    .or_default()
                    .insert(*clip_id);
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
                track.clips = kept.into();

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

            seq.compact_structural_references();
            Ok(removed_count)
        })
    }

    /// Create a Sequence as one project-level authoring transaction.
    pub fn new_sequence(&mut self, name: &str) -> mondrian_core::Result<SequenceId> {
        let defaults = self
            .authoring
            .as_ref()
            .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "new_sequence".to_owned(),
                reason: "当前没有打开的项目".to_owned(),
            })?
            .document()
            .new_sequence_defaults
            .clone();
        let sequence = Sequence::with_settings(name, defaults)?;
        let sequence_id = sequence.id;
        self.stop()?;
        let commit = {
            let session = self.authoring.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "new_sequence".to_owned(),
                    reason: "当前没有打开的项目".to_owned(),
                }
            })?;
            let before = session.document().clone();
            let mut after = before.clone();
            after.sequences.add_sequence(sequence)?;
            let commit =
                session.commit_project_snapshot("新建序列", before, after)?.ok_or_else(|| {
                    mondrian_core::MondrianError::WorkflowStepFailed {
                        step_id: "new_sequence".to_owned(),
                        reason: "新建序列事务未产生作者状态变更".to_owned(),
                    }
                })?;
            session.switch_active_sequence(sequence_id, SequenceNavigationIntent::ReplaceRoot)?;
            commit
        };
        self.consume_authoring_commit(commit);
        self.settle_preview_access_source();
        tracing::info!(%sequence_id, "新建序列: {name}");
        Ok(sequence_id)
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
                Ok(Some(asset))
                    if matches!(
                        asset.kind,
                        mondrian_assets::AssetKind::Video | mondrian_assets::AssetKind::StillImage
                    ) =>
                {
                    if let Some(path) = asset.file_path() {
                        return Some(path.to_path_buf());
                    }
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

    pub fn jump_to_start_frame(&mut self) -> mondrian_core::Result<()> {
        let current = self.current_frame();
        // 入/出点不影响播放逻辑，仅影响导出。
        if current == 0 {
            return Ok(());
        }
        self.seek(0)
    }

    pub fn jump_to_end_frame(&mut self) -> mondrian_core::Result<()> {
        let current = self.current_frame();
        // 入/出点不影响播放逻辑，仅影响导出。
        let target = self.last_content_frame()?.max(0);
        if current == target {
            return Ok(());
        }
        self.seek(target)
    }

    pub fn step_prev_frame(&mut self) -> mondrian_core::Result<()> {
        let current = self.current_frame();
        if current > 0 {
            self.seek(current - 1)
        } else {
            Ok(())
        }
    }

    pub fn step_next_frame(&mut self) -> mondrian_core::Result<()> {
        let next = self.current_frame().checked_add(1).ok_or_else(|| {
            mondrian_core::MondrianError::ActionNotExecuted {
                action: "step_next_frame".to_owned(),
                reason: "timeline frame arithmetic overflow".to_owned(),
            }
        })?;
        self.seek(next)
    }

    pub fn mark_in_at_current_frame(&mut self) -> mondrian_core::Result<()> {
        let current = self.current_frame().max(0);
        let time_base = self.active_sequence().map(Sequence::time_base).ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "mark_in_at_current_frame".to_owned(),
                reason: "当前无序列".to_owned(),
            }
        })?;
        self.set_timeline_in_out_point(crate::app::product_action::TimelineSetInOutPointPayload {
            point: crate::app::product_action::TimelineInOutPointKind::In,
            position: FramePosition::new(current, time_base),
        })
    }

    pub fn mark_out_at_current_frame(&mut self) -> mondrian_core::Result<()> {
        let current = self.current_frame().max(0);
        let time_base = self.active_sequence().map(Sequence::time_base).ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "mark_out_at_current_frame".to_owned(),
                reason: "当前无序列".to_owned(),
            }
        })?;
        self.set_timeline_in_out_point(crate::app::product_action::TimelineSetInOutPointPayload {
            point: crate::app::product_action::TimelineInOutPointKind::Out,
            position: FramePosition::new(current, time_base),
        })
    }

    pub fn roll_cut_to_frame(
        &mut self,
        clip_id: ClipId,
        target_frame: i64,
    ) -> mondrian_core::Result<bool> {
        let sequence_id = self.active_sequence_id().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "roll_cut_to_frame".to_string(),
                reason: "当前无序列".to_string(),
            }
        })?;
        self.commit_sequence_edit(sequence_id, "滚动修剪", |seq| {
            let time_base = seq.time_base();
            let target = sequence_time_from_frame(target_frame, time_base)?;
            let minimum_duration = sequence_time_from_frame(1, time_base)?;
            let outcome =
                apply_roll_edit(seq, &RollEditRequest { clip_id, target, minimum_duration })
                    .map_err(mondrian_core::MondrianError::from)?;
            if outcome.is_none() {
                return Ok(false);
            }
            seq.compact_structural_references();
            Ok(true)
        })
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

        let sequence_id = self.active_sequence_id().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "slip_clips_bulk_by_frames".to_string(),
                reason: "当前无序列".to_string(),
            }
        })?;
        self.commit_sequence_edit(sequence_id, "滑移片段", |seq| {
            let mut targets = clip_ids.iter().copied().collect::<HashSet<_>>();
            expand_clip_selection_units(seq, &mut targets);
            let time_base = seq.time_base();
            let delta = sequence_time_from_frame(delta_frames, time_base)?;
            let mut changed_count = 0usize;

            for clip_id in targets {
                let estimated_total_source_extent = {
                    let Some(clip) = seq.find_clip(clip_id) else {
                        continue;
                    };
                    estimate_asset_total_source_frames(library.as_ref(), clip, time_base)
                        .map(|frames| sequence_time_from_frame(frames, time_base))
                        .transpose()?
                };
                match apply_slip_edit(
                    seq,
                    &SlipEditRequest { clip_id, delta, estimated_total_source_extent },
                ) {
                    Ok(Some(_)) => {
                        changed_count += 1;
                    }
                    Ok(None) => {}
                    Err(CutEditError::UnknownClip { .. }) => continue,
                    Err(CutEditError::AdjustmentLayerSlip) => {
                        return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                            step_id: "slip_clip".to_owned(),
                            reason: "调整图层不支持 slip".to_owned(),
                        });
                    }
                    Err(error) => return Err(mondrian_core::MondrianError::from(error)),
                }
            }

            if changed_count == 0 {
                return Ok(0);
            }

            seq.compact_structural_references();
            Ok(changed_count)
        })
    }

    pub fn slide_clips_bulk_by_frames(
        &mut self,
        clip_ids: &[ClipId],
        delta_frames: i64,
    ) -> mondrian_core::Result<usize> {
        if clip_ids.is_empty() || delta_frames == 0 {
            return Ok(0);
        }

        let sequence_id = self.active_sequence_id().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "slide_clips_bulk_by_frames".to_string(),
                reason: "当前无序列".to_string(),
            }
        })?;
        self.commit_sequence_edit(sequence_id, "滑动片段", |seq| {
            let mut targets = clip_ids.iter().copied().collect::<HashSet<_>>();
            expand_clip_selection_units(seq, &mut targets);
            let time_base = seq.time_base();
            let delta = sequence_time_from_frame(delta_frames, time_base)?;
            let minimum_duration = sequence_time_from_frame(1, time_base)?;
            let mut changed_count = 0usize;

            for clip_id in targets {
                match apply_slide_edit(seq, &SlideEditRequest { clip_id, delta, minimum_duration })
                {
                    Ok(Some(_)) => {
                        changed_count += 1;
                    }
                    Ok(None) => {}
                    Err(CutEditError::UnknownClip { .. }) => continue,
                    Err(error) => return Err(mondrian_core::MondrianError::from(error)),
                }
            }

            if changed_count == 0 {
                return Ok(0);
            }

            seq.compact_structural_references();
            Ok(changed_count)
        })
    }

    pub fn split_clip_at_frame(
        &mut self,
        track_id: TrackId,
        is_video_track: bool,
        clip_id: ClipId,
        split_frame: i64,
    ) -> mondrian_core::Result<Option<SplitClipOutcome>> {
        let sequence_id = self.active_sequence_id().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "split_clip".to_string(),
                reason: "当前无序列".to_string(),
            }
        })?;
        self.commit_sequence_edit(sequence_id, "分割片段", |sequence| {
            Self::split_clip_at_frame_internal(
                sequence,
                track_id,
                is_video_track,
                clip_id,
                split_frame,
            )
        })
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

        let sequence_id = self.active_sequence_id().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "split_at_playhead".to_string(),
                reason: "当前无序列".to_string(),
            }
        })?;
        self.commit_sequence_edit(sequence_id, "在播放头分割片段", move |seq| {
            let mut processed = HashSet::new();
            let mut split_count = 0usize;

            for (track_id, is_video, clip_id) in targets {
                if !processed.insert(clip_id) {
                    continue;
                }
                if Self::split_clip_at_frame_internal(seq, track_id, is_video, clip_id, frame)?
                    .is_some()
                {
                    split_count += 1;
                }
            }
            Ok(split_count)
        })
    }

    fn split_clip_at_frame_internal(
        seq: &mut Sequence,
        track_id: TrackId,
        is_video_track: bool,
        clip_id: ClipId,
        split_frame: i64,
    ) -> mondrian_core::Result<Option<SplitClipOutcome>> {
        let Some(location) = seq.clip_track_location(clip_id) else {
            return Err(mondrian_core::MondrianError::ClipNotFound {
                clip_id: clip_id.to_string(),
            });
        };
        if location.track_id != track_id || location.is_video_track != is_video_track {
            return Err(mondrian_core::MondrianError::ClipNotFound {
                clip_id: clip_id.to_string(),
            });
        }
        let members = clip_selection_unit(seq, clip_id).unwrap_or_default();
        for member in &members {
            if let Some(member_location) = seq.clip_track_location(*member)
                && member_location.is_locked
            {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: member_location.track_id.to_string(),
                });
            }
        }

        let split_time = sequence_time_from_frame(split_frame, seq.time_base())?;
        let mut split_members = Vec::new();
        for member in members {
            match apply_split_edit(seq, &SplitEditRequest { clip_id: member, at: split_time }) {
                Ok(outcome) => split_members.push(SplitClipMemberOutcome {
                    left_clip_id: member,
                    right_clip_id: outcome.right_clip_id,
                }),
                Err(CutEditError::SplitOutOfRange) => {}
                Err(error) => return Err(mondrian_core::MondrianError::from(error)),
            }
        }
        if split_members.len() >= 2 {
            let right_group = ClipLinkGroupId::new();
            for member in &split_members {
                if let Some(right) = seq.find_clip_mut(member.right_clip_id) {
                    right.link_group = Some(right_group);
                }
            }
        }
        seq.compact_structural_references();
        let Some(primary_index) =
            split_members.iter().position(|member| member.left_clip_id == clip_id)
        else {
            return Ok(None);
        };
        let primary = split_members.remove(primary_index);
        Ok(Some(SplitClipOutcome {
            primary,
            linked_members: split_members,
        }))
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
            AssetKind::Video
                | AssetKind::StillImage
                | AssetKind::AdjustmentLayer
                | AssetKind::SolidColor
        ) {
            return Err(mondrian_core::MondrianError::UnsupportedFormat {
                format: "仅支持将视频素材或调整图层拖到视频轨".to_string(),
            });
        }

        // Resolve source display geometry for auto-fit before borrowing seq.
        let media_picture = if matches!(dragging.kind, AssetKind::Video | AssetKind::StillImage) {
            let video = self
                .asset_library()
                .and_then(|lib| lib.get_asset(dragging.asset_id).ok().flatten())
                .and_then(|asset| {
                    asset.media_probe().and_then(|probe| probe.primary_video()).cloned()
                })
                .ok_or_else(|| mondrian_core::MondrianError::UnsupportedFormat {
                    format: "素材没有与当前修订一致的视频图片合同".to_owned(),
                })?;
            Some(
                mondrian_core::ResolvedPictureGeometry::resolve(
                    mondrian_core::Resolution { width: video.width, height: video.height },
                    video.picture,
                    None,
                    None,
                )
                .map_err(|error| {
                    mondrian_core::MondrianError::UnsupportedFormat {
                        format: format!("素材图片解释不受支持：{error}"),
                    }
                })?,
            )
        } else {
            None
        };

        let sequence_id = self.active_sequence_id().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "timeline_drop".to_string(),
                reason: "当前无序列".to_string(),
            }
        })?;
        let (clip_id, start_frame) =
            self.commit_sequence_edit(sequence_id, "添加视频片段", |seq| {
                let fps = seq.settings.frame_rate.to_f64();
                let duration_frames =
                    ((dragging.duration.as_secs_f64() * fps).ceil() as i64).max(1);
                let time_base = seq.time_base();
                let start_frame = timeline_frame.max(0);
                let start_time = sequence_time_from_frame(start_frame, time_base)?;
                let duration = sequence_time_from_frame(duration_frames, time_base)?;

                let mut clip = if dragging.kind == AssetKind::AdjustmentLayer {
                    Clip::new_adjustment_layer(dragging.asset_id, start_time, duration)?
                } else if dragging.kind == AssetKind::SolidColor {
                    Clip::new_solid_color(
                        dragging.asset_id,
                        super::timeline_insert::DEFAULT_SOLID_COLOR_CLIP_COLOR,
                        start_time,
                        duration,
                    )?
                } else if dragging.kind == AssetKind::StillImage {
                    Clip::new_still_image(dragging.asset_id, start_time, duration)?
                } else {
                    Clip::new(dragging.asset_id, start_time, duration)?
                };
                clip.label = Some(dragging.name.clone());
                if let Some(picture) = media_picture {
                    super::timeline_insert::auto_fit_picture(seq, &mut clip, picture)?;
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
                        seq.video_tracks.iter().position(|t| t.id == track_id).ok_or_else(
                            || mondrian_core::MondrianError::TrackNotFound {
                                track_id: track_id.to_string(),
                            },
                        )?;
                    seq.ensure_audio_track_index(target_video_index);
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
                seq.compact_structural_references();

                Ok((clip_id, start_frame))
            })?;

        self.event_bus.publish(AppEvent::ClipAdded { sequence_id, clip_id });
        self.reconcile_playhead_after_committed_authoring_change(
            start_frame,
            "drop_asset_to_video_track",
        );
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

        let sequence_id = self.active_sequence_id().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "timeline_drop_audio".to_string(),
                reason: "当前无序列".to_string(),
            }
        })?;
        let (clip_id, start_frame) =
            self.commit_sequence_edit(sequence_id, "添加音频片段", |seq| {
                let fps = seq.settings.frame_rate.to_f64();
                let duration_frames =
                    ((dragging.duration.as_secs_f64() * fps).ceil() as i64).max(1);
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
                seq.compact_structural_references();

                Ok((clip_id, start_frame))
            })?;

        self.event_bus.publish(AppEvent::ClipAdded { sequence_id, clip_id });
        self.reconcile_playhead_after_committed_authoring_change(
            start_frame,
            "drop_asset_to_audio_track",
        );
        self.clear_dragging_asset();
        Ok(clip_id)
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

        let sequence_id = self.active_sequence_id().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "move_clip_group_by_delta_with_mode".to_string(),
                reason: "当前无序列".to_string(),
            }
        })?;
        self.commit_sequence_edit(sequence_id, "移动多个片段", |seq| {
            let mut anchor_frames = HashMap::<ClipId, i64>::new();
            for (clip_id, start_frame) in anchors {
                anchor_frames.entry(*clip_id).or_insert(*start_frame);
            }
            let mut member_ids = anchor_frames.keys().copied().collect::<HashSet<_>>();
            expand_clip_selection_units(seq, &mut member_ids);

            let mut target_positions = Vec::<(ClipId, i64)>::with_capacity(member_ids.len());
            for clip_id in member_ids {
                let current_frame = seq
                    .find_clip(clip_id)
                    .ok_or_else(|| mondrian_core::MondrianError::ClipNotFound {
                        clip_id: clip_id.to_string(),
                    })?
                    .position
                    .to_frame_position(seq.settings.frame_rate, FrameRounding::Nearest)?
                    .frame;
                let start_frame = anchor_frames.get(&clip_id).copied().unwrap_or(current_frame);

                let location = seq.clip_track_location(clip_id).ok_or_else(|| {
                    mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() }
                })?;
                if location.is_locked {
                    return Err(mondrian_core::MondrianError::TrackLocked {
                        track_id: location.track_id.to_string(),
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

            let time_base = seq.time_base();
            let mut changed_count = 0usize;
            for (clip_id, target_frame) in &target_positions {
                let position = sequence_time_from_frame((*target_frame).max(0), time_base)?;
                if seq.set_clip_position(*clip_id, position) {
                    changed_count += 1;
                }
            }
            if changed_count == 0 {
                return Ok(0);
            }

            let focus_ids: HashSet<ClipId> = target_positions.iter().map(|(id, _)| *id).collect();
            apply_sequence_track_conflicts_for_focus_group(seq, &focus_ids, overlap_mode)?;

            seq.compact_structural_references();
            Ok(changed_count)
        })
    }
}
