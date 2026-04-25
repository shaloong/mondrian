use super::*;

impl AppState {
    pub(super) fn record_sequence_snapshot_command(
        &mut self,
        description: impl Into<String>,
        before: Sequence,
        after: Sequence,
    ) {
        self.cmd_history.record_executed(Box::new(SequenceSnapshotCommand::new(
            description,
            before,
            after,
        )));
    }

    pub fn undo_timeline(&mut self) -> mondrian_core::Result<bool> {
        let (undone, sequence_id) = {
            let Some(seq) = self.sequence.as_mut() else {
                return Ok(false);
            };
            let undone = self.cmd_history.undo(seq)?;
            (undone, seq.id)
        };

        if undone {
            self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
            let _ = self.save_project_file();
        }

        Ok(undone)
    }

    pub fn redo_timeline(&mut self) -> mondrian_core::Result<bool> {
        let (redone, sequence_id) = {
            let Some(seq) = self.sequence.as_mut() else {
                return Ok(false);
            };
            let redone = self.cmd_history.redo(seq)?;
            (redone, seq.id)
        };

        if redone {
            self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
            let _ = self.save_project_file();
        }

        Ok(redone)
    }

    pub fn record_timeline_edit_snapshot(
        &mut self,
        description: impl Into<String>,
        before: Sequence,
    ) {
        let (sequence_id, after) = match self.sequence.as_ref() {
            Some(seq) => (seq.id, seq.clone()),
            None => return,
        };

        self.record_sequence_snapshot_command(description, before, after);
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        let _ = self.save_project_file();
    }

    pub fn close_project(&mut self) {
        if let Some(runtime) = self.project_runtime_dir.as_ref() {
            let _ = fs::remove_dir_all(runtime);
        }

        self.sequence = None;
        self.current_project_path = None;
        self.project_runtime_dir = None;
        self.project_in_point = None;
        self.project_out_point = None;
        self.asset_library = None;
        self.playback = PlaybackState::Stopped;
        self.playback_buffering = false;
        self.dragging_asset = None;
        self.cmd_history = mondrian_timeline::command::CommandHistory::new(200);
        self.proxy_mode_assets.clear();
        self.clear_status_hint();
    }

    pub fn add_video_track(&mut self) -> anyhow::Result<()> {
        let before = if let Some(seq) = self.sequence.as_mut() {
            let before = seq.clone();
            seq.add_video_track();
            Some(before)
        } else {
            None
        };
        if let Some(before) = before {
            self.record_timeline_edit_snapshot("新增视频轨道", before);
        }
        Ok(())
    }

    pub fn add_audio_track(&mut self) -> anyhow::Result<()> {
        let before = if let Some(seq) = self.sequence.as_mut() {
            let before = seq.clone();
            seq.add_audio_track();
            Some(before)
        } else {
            None
        };
        if let Some(before) = before {
            self.record_timeline_edit_snapshot("新增音频轨道", before);
        }
        Ok(())
    }

    pub fn default_adjustment_layer_duration_frames(&self) -> i64 {
        let selection_span = self
            .out_point_frame()
            .map(|out| out.saturating_sub(self.in_point_frame()))
            .filter(|span| *span > 0);
        if let Some(span) = selection_span {
            return span.max(1);
        }

        let fps = self
            .sequence
            .as_ref()
            .map(|seq| seq.settings.frame_rate.to_f64())
            .unwrap_or(25.0);
        ((DEFAULT_ADJUSTMENT_LAYER_DURATION_SECS * fps).round() as i64).max(1)
    }

    pub fn default_adjustment_layer_drag_duration(&self) -> Duration {
        let fps = self
            .sequence
            .as_ref()
            .map(|seq| seq.settings.frame_rate.to_f64())
            .unwrap_or(25.0);
        let secs = self.default_adjustment_layer_duration_frames() as f64 / fps.max(1.0);
        Duration::from_secs_f64(secs.max(1.0 / fps.max(1.0)))
    }

    fn create_adjustment_layer_asset_internal(
        &mut self,
        name: Option<&str>,
        announce: bool,
    ) -> mondrian_core::Result<(AssetId, String)> {
        let library = self.asset_library.as_ref().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "create_adjustment_layer_asset".to_string(),
                reason: "素材库未连接".to_string(),
            }
        })?;

        let asset_id = library.create_adjustment_layer_asset(name)?;
        let asset_name = library
            .get_asset(asset_id)?
            .map(|asset| asset.name)
            .unwrap_or_else(|| "调整图层".to_string());

        self.event_bus
            .publish(mondrian_core::events::AppEvent::AssetImported { asset_id });
        let _ = self.save_project_file();
        if announce {
            self.set_status_hint(format!("已新建：{}", asset_name), false);
        }
        Ok((asset_id, asset_name))
    }

    pub fn create_adjustment_layer_asset(
        &mut self,
        name: Option<&str>,
    ) -> mondrian_core::Result<AssetId> {
        self.create_adjustment_layer_asset_internal(name, true)
            .map(|(asset_id, _)| asset_id)
    }

    pub fn create_adjustment_layer_on_video_track(
        &mut self,
        track_id: TrackId,
        timeline_frame: Option<i64>,
        overlap_mode: ClipOverlapMode,
    ) -> mondrian_core::Result<ClipId> {
        let selection_start = self
            .out_point_frame()
            .filter(|out| *out > self.in_point_frame())
            .map(|_| self.in_point_frame());
        let start_frame = timeline_frame
            .or(selection_start)
            .unwrap_or_else(|| self.current_frame().max(0));
        let (asset_id, asset_name) = self.create_adjustment_layer_asset_internal(None, false)?;
        self.begin_drag_asset(
            asset_id,
            asset_name.clone(),
            AssetKind::AdjustmentLayer,
            self.default_adjustment_layer_drag_duration(),
            false,
        );
        let clip_id =
            self.drop_dragging_asset_to_video_track_with_mode(track_id, start_frame, overlap_mode)?;
        self.set_status_hint(format!("已创建调整图层：{}", asset_name), false);
        Ok(clip_id)
    }

    pub fn remove_track(&mut self, track_id: TrackId, is_video: bool) -> mondrian_core::Result<()> {
        let before = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "remove_track".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;

            let before = seq.clone();
            if is_video {
                seq.remove_video_track(track_id)?;
            } else {
                seq.remove_audio_track(track_id)?;
            }
            clear_broken_links(seq);
            before
        };

        self.record_timeline_edit_snapshot("删除轨道", before);
        Ok(())
    }

    pub fn move_track(
        &mut self,
        track_id: TrackId,
        is_video: bool,
        new_index: usize,
    ) -> mondrian_core::Result<()> {
        let before = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "move_track".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;
            let before = seq.clone();
            if is_video {
                seq.move_video_track(track_id, new_index)?;
            } else {
                seq.move_audio_track(track_id, new_index)?;
            }
            before
        };

        self.record_timeline_edit_snapshot("移动轨道", before);
        Ok(())
    }

    pub fn set_track_visible(
        &mut self,
        track_id: TrackId,
        is_video: bool,
        visible: bool,
    ) -> mondrian_core::Result<()> {
        let before = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "set_track_visible".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;
            let before = seq.clone();

            let track = if is_video {
                seq.video_track_mut(track_id)
            } else {
                seq.audio_track_mut(track_id)
            }
            .ok_or_else(|| mondrian_core::MondrianError::TrackNotFound {
                track_id: track_id.to_string(),
            })?;
            if track.is_visible == visible {
                return Ok(());
            }
            track.is_visible = visible;
            before
        };

        self.record_timeline_edit_snapshot("切换轨道可见性", before);
        Ok(())
    }

    pub fn set_track_muted(
        &mut self,
        track_id: TrackId,
        is_video: bool,
        muted: bool,
    ) -> mondrian_core::Result<()> {
        let before = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "set_track_muted".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;
            let before = seq.clone();

            let track = if is_video {
                seq.video_track_mut(track_id)
            } else {
                seq.audio_track_mut(track_id)
            }
            .ok_or_else(|| mondrian_core::MondrianError::TrackNotFound {
                track_id: track_id.to_string(),
            })?;
            if track.is_muted == muted {
                return Ok(());
            }
            track.is_muted = muted;
            before
        };

        self.record_timeline_edit_snapshot("切换轨道静音", before);
        Ok(())
    }

    pub fn set_track_locked(
        &mut self,
        track_id: TrackId,
        is_video: bool,
        locked: bool,
    ) -> mondrian_core::Result<()> {
        let before = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "set_track_locked".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;
            let before = seq.clone();

            let track = if is_video {
                seq.video_track_mut(track_id)
            } else {
                seq.audio_track_mut(track_id)
            }
            .ok_or_else(|| mondrian_core::MondrianError::TrackNotFound {
                track_id: track_id.to_string(),
            })?;
            if track.is_locked == locked {
                return Ok(());
            }
            track.is_locked = locked;
            before
        };

        self.record_timeline_edit_snapshot("切换轨道锁定", before);
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
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "set_clips_disabled_bulk".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;

            let before = seq.clone();
            let mut clip_ids: HashSet<ClipId> = selections.iter().map(|(_, _, id)| *id).collect();
            let selected_clip_ids: Vec<ClipId> = clip_ids.iter().copied().collect();

            for clip_id in selected_clip_ids {
                if let Some(linked_id) = find_clip(seq, clip_id).and_then(|clip| clip.linked_clip) {
                    clip_ids.insert(linked_id);
                }
            }

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

            (seq.id, before, seq.clone(), changed_count)
        };

        let action = if disabled {
            "禁用片段"
        } else {
            "启用片段"
        };
        self.record_sequence_snapshot_command(action, before, after);
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        let _ = self.save_project_file();

        Ok(changed_count)
    }

    pub fn remove_clip(
        &mut self,
        track_id: TrackId,
        is_video_track: bool,
        clip_id: ClipId,
    ) -> mondrian_core::Result<()> {
        let (sequence_id, before, after) = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "remove_clip".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;

            let before = seq.clone();
            let removed = if is_video_track {
                let track = seq.video_track_mut(track_id).ok_or_else(|| {
                    mondrian_core::MondrianError::TrackNotFound { track_id: track_id.to_string() }
                })?;
                track.remove_clip(clip_id)
            } else {
                let track = seq.audio_track_mut(track_id).ok_or_else(|| {
                    mondrian_core::MondrianError::TrackNotFound { track_id: track_id.to_string() }
                })?;
                track.remove_clip(clip_id)
            };

            let Some(removed_clip) = removed else {
                return Err(mondrian_core::MondrianError::ClipNotFound {
                    clip_id: clip_id.to_string(),
                });
            };

            if let Some(linked_id) = removed_clip.linked_clip {
                let _ = remove_clip_from_sequence(seq, linked_id);
            }

            (seq.id, before, seq.clone())
        };

        self.record_sequence_snapshot_command("删除片段", before, after);
        self.event_bus.publish(AppEvent::ClipRemoved { sequence_id, clip_id });
        let _ = self.save_project_file();
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

        let mut history_snapshot: Option<(SequenceId, Sequence, Sequence)> = None;

        let removed_count = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "remove_clips_bulk".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;

            let before = seq.clone();
            let mut removed_count = 0usize;
            let mut linked_to_remove: Vec<ClipId> = Vec::new();

            let mut by_track: HashMap<(TrackId, bool), HashSet<ClipId>> = HashMap::new();
            for (track_id, is_video, clip_id) in selections {
                by_track.entry((*track_id, *is_video)).or_default().insert(*clip_id);
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

                let mut removed_segments: Vec<(i64, i64)> = Vec::new();
                let mut kept: Vec<Clip> = Vec::with_capacity(track.clips.len());
                for clip in track.clips.drain(..) {
                    if clip_ids.contains(&clip.id) {
                        removed_count += 1;
                        removed_segments.push((clip.position.frame, clip.duration.frame.max(0)));
                        if let Some(linked) = clip.linked_clip {
                            linked_to_remove.push(linked);
                        }
                    } else {
                        kept.push(clip);
                    }
                }
                track.clips = kept;

                if ripple {
                    removed_segments.sort_by_key(|(start, _)| *start);
                    for (start, dur) in removed_segments {
                        let end = start + dur;
                        for clip in &mut track.clips {
                            if clip.position.frame >= end {
                                clip.position = TimeCode::new(
                                    (clip.position.frame - dur).max(0),
                                    clip.position.time_base,
                                );
                            }
                        }
                    }
                }

                resolve_track_overlaps(track);
            }

            let selected_ids: HashSet<ClipId> = selections.iter().map(|(_, _, id)| *id).collect();
            let mut dedup_linked = HashSet::new();
            for linked_id in linked_to_remove {
                if selected_ids.contains(&linked_id) || !dedup_linked.insert(linked_id) {
                    continue;
                }
                if remove_clip_from_sequence_with_ripple(seq, linked_id, ripple) {
                    removed_count += 1;
                }
            }

            clear_broken_links(seq);

            if removed_count > 0 {
                history_snapshot = Some((seq.id, before, seq.clone()));
            }

            removed_count
        };

        if let Some((sequence_id, before, after)) = history_snapshot {
            self.record_sequence_snapshot_command("删除多个片段", before, after);
            self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
            let _ = self.save_project_file();
        }

        Ok(removed_count)
    }

    pub fn delete_asset_and_cleanup_timeline(
        &mut self,
        asset_id: AssetId,
    ) -> mondrian_core::Result<usize> {
        let mut removed_count = 0usize;
        let mut before_snapshot: Option<Sequence> = None;

        if let Some(seq) = self.sequence.as_mut() {
            before_snapshot = Some(seq.clone());
            removed_count += remove_asset_clips_from_tracks(&mut seq.video_tracks, asset_id);
            removed_count += remove_asset_clips_from_tracks(&mut seq.audio_tracks, asset_id);

            let existing_clip_ids: HashSet<ClipId> = seq
                .video_tracks
                .iter()
                .flat_map(|track| track.clips.iter().map(|clip| clip.id))
                .chain(
                    seq.audio_tracks
                        .iter()
                        .flat_map(|track| track.clips.iter().map(|clip| clip.id)),
                )
                .collect();

            for track in &mut seq.video_tracks {
                for clip in &mut track.clips {
                    if let Some(linked_id) = clip.linked_clip {
                        if !existing_clip_ids.contains(&linked_id) {
                            clip.linked_clip = None;
                        }
                    }
                }
            }
            for track in &mut seq.audio_tracks {
                for clip in &mut track.clips {
                    if let Some(linked_id) = clip.linked_clip {
                        if !existing_clip_ids.contains(&linked_id) {
                            clip.linked_clip = None;
                        }
                    }
                }
            }
        }

        if removed_count > 0 {
            if let Some(before) = before_snapshot {
                self.record_timeline_edit_snapshot("删除素材并清理时间线", before);
            }
        }

        Ok(removed_count)
    }

    pub fn relink_asset(
        &mut self,
        asset_id: AssetId,
        new_path: &Path,
    ) -> mondrian_core::Result<()> {
        let library = self.asset_library.as_ref().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "relink_asset".to_string(),
                reason: "素材库未连接".to_string(),
            }
        })?;
        library.relink_asset(asset_id, new_path)?;
        let _ = self.save_project_file();
        Ok(())
    }

    pub fn relink_offline_assets_in_directory(
        &mut self,
        directory: &Path,
    ) -> mondrian_core::Result<usize> {
        let library = self.asset_library.as_ref().ok_or_else(|| {
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

        if relinked > 0 {
            let _ = self.save_project_file();
        }

        Ok(relinked)
    }

    /// 创建新序列并替换当前序列
    pub fn new_sequence(&mut self, name: &str) {
        self.sequence = Some(Sequence::new(name));
        self.project_in_point = None;
        self.project_out_point = None;
        self.ensure_minimum_tracks();
        self.cmd_history = mondrian_timeline::command::CommandHistory::new(200);
        let _ = self.save_project_file();
        tracing::info!("新建序列: {name}");
    }

    // ─── 播放控制 ────────────────────────────

    pub fn default_export_input_path(&self) -> Option<PathBuf> {
        let seq = self.sequence.as_ref()?;
        let library = self.asset_library.as_ref()?;

        let mut candidates: Vec<(i64, AssetId)> = Vec::new();
        for track in &seq.video_tracks {
            for clip in &track.clips {
                if clip.is_disabled {
                    continue;
                }
                candidates.push((clip.position.frame, clip.asset_id));
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

    pub fn last_content_frame(&self) -> i64 {
        let Some(seq) = self.sequence.as_ref() else {
            return 0;
        };

        let mut max_frame = 0i64;
        for track in seq.video_tracks.iter().chain(seq.audio_tracks.iter()) {
            for clip in &track.clips {
                max_frame = max_frame.max((clip.end_position().frame - 1).max(0));
            }
        }
        max_frame
    }

    pub fn jump_to_start_frame(&mut self) {
        let current = self.current_frame();
        // 入/出点不影响播放逻辑，仅影响导出。
        if current == 0 {
            return;
        }
        self.seek(0);
    }

    pub fn jump_to_end_frame(&mut self) {
        let current = self.current_frame();
        // 入/出点不影响播放逻辑，仅影响导出。
        let target = self.last_content_frame().max(0);
        if current == target {
            return;
        }
        self.seek(target);
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

    pub fn mark_in_at_current_frame(&mut self) {
        let current = self.current_frame().max(0);
        self.project_in_point = Some(current);
        if let Some(out) = self.project_out_point {
            if out < current {
                self.project_out_point = Some(current);
            }
        }
        let _ = self.save_project_file();
    }

    pub fn mark_out_at_current_frame(&mut self) {
        let current = self.current_frame().max(0);
        let in_point = self.project_in_point.unwrap_or(0).max(0);
        self.project_out_point = Some(current.max(in_point));
        let _ = self.save_project_file();
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
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "trim_clips_bulk_to_frame".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;

            let before = seq.clone();
            let mut processed = HashSet::<ClipId>::new();
            let mut changed_count = 0usize;

            for clip_id in clip_ids {
                if !processed.insert(*clip_id) {
                    continue;
                }

                let linked = find_clip(seq, *clip_id).and_then(|clip| clip.linked_clip);
                match trim_clip_edge_internal(seq, *clip_id, edge, target_frame) {
                    Ok(true) => {
                        changed_count += 1;
                    }
                    Ok(false) => {}
                    Err(mondrian_core::MondrianError::ClipNotFound { .. }) => continue,
                    Err(err) => return Err(err),
                }

                if let Some(linked_id) = linked {
                    if !processed.insert(linked_id) {
                        continue;
                    }
                    match trim_clip_edge_internal(seq, linked_id, edge, target_frame) {
                        Ok(true) => {
                            changed_count += 1;
                            if let Some(primary) = find_clip_mut(seq, *clip_id) {
                                primary.linked_clip = Some(linked_id);
                            }
                            if let Some(linked_clip) = find_clip_mut(seq, linked_id) {
                                linked_clip.linked_clip = Some(*clip_id);
                            }
                        }
                        Ok(false) => {}
                        Err(mondrian_core::MondrianError::ClipNotFound { .. }) => {}
                        Err(err) => return Err(err),
                    }
                }
            }

            if changed_count == 0 {
                return Ok(0);
            }

            (seq.id, before, seq.clone(), changed_count)
        };

        let action = match edge {
            TrimEdge::In => "修剪入点",
            TrimEdge::Out => "修剪出点",
        };
        self.record_sequence_snapshot_command(action, before, after);
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        let _ = self.save_project_file();
        Ok(changed_count)
    }

    pub fn roll_cut_to_frame(
        &mut self,
        clip_id: ClipId,
        target_frame: i64,
    ) -> mondrian_core::Result<bool> {
        let (sequence_id, before, after, changed) = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "roll_cut_to_frame".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;

            let before = seq.clone();
            let changed = roll_cut_for_clip_internal(seq, clip_id, target_frame)?;
            if !changed {
                return Ok(false);
            }
            (seq.id, before, seq.clone(), changed)
        };

        if changed {
            self.record_sequence_snapshot_command("滚动修剪", before, after);
            self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
            let _ = self.save_project_file();
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

        let library = self.asset_library.clone().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "slip_clips_bulk_by_frames".to_string(),
                reason: "素材库未连接".to_string(),
            }
        })?;

        let (sequence_id, before, after, changed_count) = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "slip_clips_bulk_by_frames".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;

            let before = seq.clone();
            let mut processed = HashSet::<ClipId>::new();
            let mut changed_count = 0usize;

            for clip_id in clip_ids {
                if !processed.insert(*clip_id) {
                    continue;
                }

                let linked = find_clip(seq, *clip_id).and_then(|clip| clip.linked_clip);
                match slip_clip_internal(seq, library.as_ref(), *clip_id, delta_frames) {
                    Ok(true) => {
                        changed_count += 1;
                    }
                    Ok(false) => {}
                    Err(mondrian_core::MondrianError::ClipNotFound { .. }) => continue,
                    Err(err) => return Err(err),
                }

                if let Some(linked_id) = linked {
                    if !processed.insert(linked_id) {
                        continue;
                    }
                    match slip_clip_internal(seq, library.as_ref(), linked_id, delta_frames) {
                        Ok(true) => {
                            changed_count += 1;
                            if let Some(primary) = find_clip_mut(seq, *clip_id) {
                                primary.linked_clip = Some(linked_id);
                            }
                            if let Some(linked_clip) = find_clip_mut(seq, linked_id) {
                                linked_clip.linked_clip = Some(*clip_id);
                            }
                        }
                        Ok(false) => {}
                        Err(mondrian_core::MondrianError::ClipNotFound { .. }) => {}
                        Err(err) => return Err(err),
                    }
                }
            }

            if changed_count == 0 {
                return Ok(0);
            }

            (seq.id, before, seq.clone(), changed_count)
        };

        self.record_sequence_snapshot_command("滑移片段", before, after);
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        let _ = self.save_project_file();
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
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "slide_clips_bulk_by_frames".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;

            let before = seq.clone();
            let mut processed = HashSet::<ClipId>::new();
            let mut changed_count = 0usize;

            for clip_id in clip_ids {
                if !processed.insert(*clip_id) {
                    continue;
                }

                let linked = find_clip(seq, *clip_id).and_then(|clip| clip.linked_clip);
                match slide_clip_internal(seq, *clip_id, delta_frames) {
                    Ok(true) => {
                        changed_count += 1;
                    }
                    Ok(false) => {}
                    Err(mondrian_core::MondrianError::ClipNotFound { .. }) => continue,
                    Err(err) => return Err(err),
                }

                if let Some(linked_id) = linked {
                    if !processed.insert(linked_id) {
                        continue;
                    }
                    match slide_clip_internal(seq, linked_id, delta_frames) {
                        Ok(true) => {
                            changed_count += 1;
                            if let Some(primary) = find_clip_mut(seq, *clip_id) {
                                primary.linked_clip = Some(linked_id);
                            }
                            if let Some(linked_clip) = find_clip_mut(seq, linked_id) {
                                linked_clip.linked_clip = Some(*clip_id);
                            }
                        }
                        Ok(false) => {}
                        Err(mondrian_core::MondrianError::ClipNotFound { .. }) => {}
                        Err(err) => return Err(err),
                    }
                }
            }

            if changed_count == 0 {
                return Ok(0);
            }

            (seq.id, before, seq.clone(), changed_count)
        };

        self.record_sequence_snapshot_command("滑动片段", before, after);
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        let _ = self.save_project_file();
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
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "split_clip".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;

            let before = seq.clone();
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
            (seq.id, before, seq.clone())
        };

        self.record_sequence_snapshot_command("分割片段", before, after);
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        let _ = self.save_project_file();
        Ok(true)
    }

    pub fn split_at_playhead(&mut self) -> mondrian_core::Result<usize> {
        let frame = self.current_frame();
        let targets = {
            let seq = self.sequence.as_ref().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "split_at_playhead".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;

            let mut targets: Vec<(TrackId, bool, ClipId)> = Vec::new();
            for track in &seq.video_tracks {
                if track.is_locked {
                    continue;
                }
                for clip in &track.clips {
                    let start = clip.position.frame;
                    let end = clip.end_position().frame;
                    if frame > start && frame < end {
                        targets.push((track.id, true, clip.id));
                    }
                }
            }
            for track in &seq.audio_tracks {
                if track.is_locked {
                    continue;
                }
                for clip in &track.clips {
                    let start = clip.position.frame;
                    let end = clip.end_position().frame;
                    if frame > start && frame < end {
                        targets.push((track.id, false, clip.id));
                    }
                }
            }

            targets
        };

        let mut history_snapshot: Option<(SequenceId, Sequence, Sequence)> = None;
        let split_count = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "split_at_playhead".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;
            let before = seq.clone();
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

            if split_count > 0 {
                history_snapshot = Some((seq.id, before, seq.clone()));
            }

            split_count
        };

        if let Some((sequence_id, before, after)) = history_snapshot {
            self.record_sequence_snapshot_command("在播放头分割片段", before, after);
            self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
            let _ = self.save_project_file();
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
        let track_locked = if is_video_track {
            seq.video_tracks
                .iter()
                .find(|t| t.id == track_id)
                .map(|t| t.is_locked)
                .unwrap_or(false)
        } else {
            seq.audio_tracks
                .iter()
                .find(|t| t.id == track_id)
                .map(|t| t.is_locked)
                .unwrap_or(false)
        };
        if track_locked {
            return Err(mondrian_core::MondrianError::TrackLocked {
                track_id: track_id.to_string(),
            });
        }

        let time_base = seq.time_base();
        let Some(primary) = split_clip_anywhere(seq, clip_id, split_frame, time_base) else {
            return Ok(false);
        };

        if let Some(linked_id) = primary.original_linked {
            let linked_split = split_clip_anywhere(seq, linked_id, split_frame, time_base);
            if let Some(linked_result) = linked_split {
                if let Some(primary_right) = find_clip_mut(seq, primary.right_clip_id) {
                    primary_right.linked_clip = Some(linked_result.right_clip_id);
                }
                if let Some(linked_right) = find_clip_mut(seq, linked_result.right_clip_id) {
                    linked_right.linked_clip = Some(primary.right_clip_id);
                }
            }
        }

        clear_broken_links(seq);
        Ok(true)
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

        if !matches!(dragging.kind, AssetKind::Video | AssetKind::AdjustmentLayer) {
            return Err(mondrian_core::MondrianError::UnsupportedFormat {
                format: "仅支持将视频素材或调整图层拖到视频轨".to_string(),
            });
        }

        let (sequence_id, clip_id, start_frame, before, after) = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "timeline_drop".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;

            let before = seq.clone();
            let fps = seq.settings.frame_rate.to_f64();
            let duration_frames = ((dragging.duration.as_secs_f64() * fps).ceil() as i64).max(1);
            let time_base = seq.time_base();
            let start_frame = timeline_frame.max(0);

            let mut clip = if dragging.kind == AssetKind::AdjustmentLayer {
                Clip::new_adjustment_layer(
                    dragging.asset_id,
                    TimeCode::new(start_frame, time_base),
                    TimeCode::new(duration_frames, time_base),
                )
            } else {
                Clip::new(
                    dragging.asset_id,
                    TimeCode::new(start_frame, time_base),
                    TimeCode::new(duration_frames, time_base),
                )
            };
            clip.label = Some(dragging.name.clone());
            let clip_id = clip.id;

            let should_create_linked_audio =
                dragging.kind == AssetKind::Video && dragging.has_linked_audio;

            let mut linked_audio_clip = if should_create_linked_audio {
                let mut audio_clip = Clip::new(
                    dragging.asset_id,
                    TimeCode::new(start_frame, time_base),
                    TimeCode::new(duration_frames, time_base),
                );
                audio_clip.label = Some(format!("{} (Audio)", dragging.name));
                let audio_clip_id = audio_clip.id;
                clip.linked_clip = Some(audio_clip_id);
                audio_clip.linked_clip = Some(clip.id);
                Some(audio_clip)
            } else {
                None
            };

            let track = seq.video_track_mut(track_id).ok_or_else(|| {
                mondrian_core::MondrianError::TrackNotFound { track_id: track_id.to_string() }
            })?;
            track.add_clip(clip)?;
            resolve_track_conflicts(track, clip_id, overlap_mode);

            if let Some(audio_clip) = linked_audio_clip.take() {
                let audio_clip_id = audio_clip.id;
                let target_video_index =
                    seq.video_tracks.iter().position(|t| t.id == track_id).ok_or_else(|| {
                        mondrian_core::MondrianError::TrackNotFound {
                            track_id: track_id.to_string(),
                        }
                    })?;
                ensure_audio_track_index(seq, target_video_index);
                if let Some(audio_track) = seq.audio_tracks.get_mut(target_video_index) {
                    audio_track.add_clip(audio_clip)?;
                    resolve_track_conflicts(audio_track, audio_clip_id, overlap_mode);
                }
            }
            clear_broken_links(seq);

            (seq.id, clip_id, start_frame, before, seq.clone())
        };

        self.record_sequence_snapshot_command("添加视频片段", before, after);
        self.event_bus.publish(AppEvent::ClipAdded { sequence_id, clip_id });
        self.seek(start_frame);
        self.clear_dragging_asset();
        let _ = self.save_project_file();
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
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "timeline_drop_audio".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;

            let before = seq.clone();
            let fps = seq.settings.frame_rate.to_f64();
            let duration_frames = ((dragging.duration.as_secs_f64() * fps).ceil() as i64).max(1);
            let time_base = seq.time_base();
            let start_frame = timeline_frame.max(0);

            let mut clip = Clip::new(
                dragging.asset_id,
                TimeCode::new(start_frame, time_base),
                TimeCode::new(duration_frames, time_base),
            );
            clip.label = Some(dragging.name.clone());
            let clip_id = clip.id;

            let track = seq.audio_track_mut(track_id).ok_or_else(|| {
                mondrian_core::MondrianError::TrackNotFound { track_id: track_id.to_string() }
            })?;
            track.add_clip(clip)?;
            resolve_track_conflicts(track, clip_id, overlap_mode);
            clear_broken_links(seq);

            (seq.id, clip_id, start_frame, before, seq.clone())
        };

        self.record_sequence_snapshot_command("添加音频片段", before, after);
        self.event_bus.publish(AppEvent::ClipAdded { sequence_id, clip_id });
        self.seek(start_frame);
        self.clear_dragging_asset();
        let _ = self.save_project_file();
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
        let seq = self.sequence.as_mut().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "move_clip".to_string(),
                reason: "当前无序列".to_string(),
            }
        })?;

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

        let linked_clip_id;
        if is_video_track {
            if seq.video_tracks[source_track_index].is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: seq.video_tracks[source_track_index].id.to_string(),
                });
            }
            if seq.video_tracks[target_track_index].is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: seq.video_tracks[target_track_index].id.to_string(),
                });
            }

            if source_track_index == target_track_index {
                let clip = seq.video_tracks[source_track_index]
                    .clips
                    .iter_mut()
                    .find(|c| c.id == clip_id)
                    .ok_or_else(|| mondrian_core::MondrianError::ClipNotFound {
                        clip_id: clip_id.to_string(),
                    })?;
                clip.position = TimeCode::new(new_start, time_base);
                linked_clip_id = clip.linked_clip;
            } else {
                let clip_index = seq.video_tracks[source_track_index]
                    .clips
                    .iter()
                    .position(|c| c.id == clip_id)
                    .ok_or_else(|| mondrian_core::MondrianError::ClipNotFound {
                        clip_id: clip_id.to_string(),
                    })?;

                let mut clip = seq.video_tracks[source_track_index].clips.remove(clip_index);
                clip.position = TimeCode::new(new_start, time_base);
                linked_clip_id = clip.linked_clip;
                seq.video_tracks[target_track_index].clips.push(clip);
            }
        } else {
            if seq.audio_tracks[source_track_index].is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: seq.audio_tracks[source_track_index].id.to_string(),
                });
            }
            if seq.audio_tracks[target_track_index].is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: seq.audio_tracks[target_track_index].id.to_string(),
                });
            }

            if source_track_index == target_track_index {
                let clip = seq.audio_tracks[source_track_index]
                    .clips
                    .iter_mut()
                    .find(|c| c.id == clip_id)
                    .ok_or_else(|| mondrian_core::MondrianError::ClipNotFound {
                        clip_id: clip_id.to_string(),
                    })?;
                clip.position = TimeCode::new(new_start, time_base);
                linked_clip_id = clip.linked_clip;
            } else {
                let clip_index = seq.audio_tracks[source_track_index]
                    .clips
                    .iter()
                    .position(|c| c.id == clip_id)
                    .ok_or_else(|| mondrian_core::MondrianError::ClipNotFound {
                        clip_id: clip_id.to_string(),
                    })?;

                let mut clip = seq.audio_tracks[source_track_index].clips.remove(clip_index);
                clip.position = TimeCode::new(new_start, time_base);
                linked_clip_id = clip.linked_clip;
                seq.audio_tracks[target_track_index].clips.push(clip);
            }
        }

        if let Some(linked_id) = linked_clip_id {
            if is_video_track {
                ensure_audio_track_index(seq, target_track_index);
                if !move_existing_clip_to_track_index(
                    seq,
                    false,
                    linked_id,
                    target_track_index,
                    new_start,
                    time_base,
                ) {
                    if let Some(linked) = find_clip_mut(seq, linked_id) {
                        linked.position = TimeCode::new(new_start, time_base);
                    }
                }
            } else if target_track_index < seq.video_tracks.len() {
                if !move_existing_clip_to_track_index(
                    seq,
                    true,
                    linked_id,
                    target_track_index,
                    new_start,
                    time_base,
                ) {
                    if let Some(linked) = find_clip_mut(seq, linked_id) {
                        linked.position = TimeCode::new(new_start, time_base);
                    }
                }
            } else if let Some(linked) = find_clip_mut(seq, linked_id) {
                linked.position = TimeCode::new(new_start, time_base);
            }
        }

        if is_video_track {
            if let Some(track) = seq.video_track_mut(target_track_id) {
                resolve_track_conflicts(track, clip_id, overlap_mode);
            }
        } else if let Some(track) = seq.audio_track_mut(target_track_id) {
            resolve_track_conflicts(track, clip_id, overlap_mode);
        }

        if let Some(linked_id) = linked_clip_id {
            apply_conflict_policy_for_existing_clip(seq, linked_id, overlap_mode);
        }

        for track in &mut seq.video_tracks {
            track.clips.sort_by_key(|c| c.position.frame);
        }
        for track in &mut seq.audio_tracks {
            track.clips.sort_by_key(|c| c.position.frame);
        }
        clear_broken_links(seq);

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

        let seq = self.sequence.as_mut().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "move_clip_group_by_delta_with_mode".to_string(),
                reason: "当前无序列".to_string(),
            }
        })?;

        let mut seen = HashSet::<ClipId>::new();
        let mut target_positions = Vec::<(ClipId, i64)>::new();
        for (clip_id, start_frame) in anchors {
            if !seen.insert(*clip_id) {
                continue;
            }

            let Some((track_id, _is_video, is_locked)) = find_clip_track_lock(seq, *clip_id) else {
                continue;
            };
            if is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track_id.to_string(),
                });
            }

            let target = start_frame.saturating_add(delta_frames).max(0);
            target_positions.push((*clip_id, target));
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
            track.clips.sort_by_key(|c| c.position.frame);
        }
        for track in &mut seq.audio_tracks {
            track.clips.sort_by_key(|c| c.position.frame);
        }

        let focus_ids: HashSet<ClipId> = target_positions.iter().map(|(id, _)| *id).collect();
        for track in &mut seq.video_tracks {
            apply_track_conflicts_for_focus_group(track, &focus_ids, overlap_mode);
        }
        for track in &mut seq.audio_tracks {
            apply_track_conflicts_for_focus_group(track, &focus_ids, overlap_mode);
        }

        clear_broken_links(seq);
        Ok(changed_count)
    }
}
