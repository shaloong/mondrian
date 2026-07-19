//! Timeline export orchestration shared by all UI frontends.

use super::*;
use mondrian_core::{JobId, MondrianError, Result};
use mondrian_export::preset::{
    Container, ExportConfig, ExportMediaDependency, ExportPreset, TimelineExportRange,
    TimelineExportSnapshot,
};
use mondrian_export::queue::{
    ExportCancelOutcome, ExportJobSnapshot, ExportQueueDiagnostics, RenderJob,
};

/// Request to enqueue a timeline export job.
#[derive(Debug, Clone)]
pub struct TimelineExportRequest {
    /// Preset used for codec/container defaults.
    pub preset: ExportPreset,
    /// Optional sequence id; falls back to the active sequence when absent.
    pub sequence_id: Option<SequenceId>,
    /// Timeline range to render.
    pub range: TimelineExportRange,
    /// Output media file path.
    pub output_path: PathBuf,
}

/// UI-stable draft state for timeline export panels.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineExportDraft {
    /// Selected preset index into [`builtin_export_presets`].
    pub selected_preset_idx: usize,
    /// Selected sequence; `None` means use the active/default sequence.
    pub selected_sequence_id: Option<SequenceId>,
    /// Timeline range to render.
    pub range: TimelineExportRange,
    /// User-entered output file path.
    pub output_path: String,
}

impl Default for TimelineExportDraft {
    fn default() -> Self {
        Self {
            selected_preset_idx: 0,
            selected_sequence_id: None,
            range: TimelineExportRange::SequenceInOut,
            output_path: String::new(),
        }
    }
}

/// One named export preset shown by UI frontends.
#[derive(Debug, Clone)]
pub struct ExportPresetOption {
    pub label: String,
    pub preset: ExportPreset,
}

/// Built-in export presets shared by app UI panels.
pub fn builtin_export_presets() -> Vec<ExportPresetOption> {
    vec![
        ExportPresetOption {
            label: "YouTube 1080p H.264".to_owned(),
            preset: ExportPreset::youtube_1080p(),
        },
        ExportPresetOption {
            label: "TikTok 竖屏 9:16".to_owned(),
            preset: ExportPreset::tiktok_vertical(),
        },
        ExportPresetOption {
            label: "代理文件 720p".to_owned(),
            preset: ExportPreset::proxy_720p(),
        },
        ExportPresetOption {
            label: "ProRes 4444 XQ + Alpha（12-bit）".to_owned(),
            preset: ExportPreset::prores_4444_alpha(),
        },
    ]
}

/// File extension implied by an export preset container.
pub fn export_preset_extension(preset: &ExportPreset) -> &'static str {
    match preset.container {
        Container::Mp4 => "mp4",
        Container::Mov => "mov",
        Container::Mkv => "mkv",
        Container::Gif => "gif",
        Container::Mxf => "mxf",
        Container::Webm => "webm",
    }
}

impl AppState {
    /// Update the export draft preset, clamped to available built-in presets.
    pub fn set_export_draft_preset_index(&mut self, index: usize) {
        let max_index = builtin_export_presets().len().saturating_sub(1);
        self.export_draft.selected_preset_idx = index.min(max_index);
    }

    /// Update the export draft sequence.
    pub fn set_export_draft_sequence_id(&mut self, sequence_id: Option<SequenceId>) {
        self.export_draft.selected_sequence_id = sequence_id;
    }

    /// Update the export draft timeline range.
    pub fn set_export_draft_range(&mut self, range: TimelineExportRange) {
        self.export_draft.range = range;
    }

    /// Update the export draft output path.
    pub fn set_export_draft_output_path(&mut self, output_path: impl Into<String>) {
        self.export_draft.output_path = output_path.into();
    }

    /// Build and enqueue a render job from a timeline export request.
    pub fn enqueue_timeline_export(&mut self, request: TimelineExportRequest) -> Result<JobId> {
        if request.output_path.as_os_str().is_empty() {
            let reason = "请指定输出路径".to_string();
            self.set_status_hint(format!("导出失败：{reason}"), true);
            return Err(export_error("enqueue_timeline_export", reason));
        }

        let sequences = self.export_sequences_snapshot();
        let sequence = match request.sequence_id {
            Some(sequence_id) => {
                sequences.iter().find(|sequence| sequence.id == sequence_id).cloned()
            }
            None => self.sequence.clone(),
        };
        let Some(sequence) = sequence else {
            let reason = "当前无序列".to_string();
            self.set_status_hint(format!("导出失败：{reason}"), true);
            return Err(export_error("enqueue_timeline_export", reason));
        };

        let timeline =
            match capture_timeline_export_snapshot(self, sequence, sequences, request.range) {
                Ok(timeline) => timeline,
                Err(reason) => {
                    self.set_status_hint(format!("导出失败：{reason}"), true);
                    return Err(export_error("enqueue_timeline_export", reason));
                }
            };

        let config = ExportConfig {
            preset: request.preset,
            timeline: Box::new(timeline),
            output_path: request.output_path,
        };
        let output_path = config.output_path.display().to_string();
        let job_id = self.render_queue.enqueue(RenderJob::new(config)).map_err(|error| {
            let reason = error.to_string();
            self.set_status_hint(format!("导出失败：{reason}"), true);
            export_error("enqueue_timeline_export", reason)
        })?;
        self.set_status_hint("已加入导出队列", false);
        tracing::info!(%job_id, "导出任务已加入队列: {output_path}");
        Ok(job_id)
    }

    /// Lightweight export snapshots for UI and Headless observers.
    pub fn export_jobs_snapshot(&self) -> Vec<ExportJobSnapshot> {
        self.render_queue.list_jobs()
    }

    /// Request cancellation without exposing queue internals to UI actions.
    pub fn cancel_export_job(&self, job_id: JobId) -> ExportCancelOutcome {
        self.render_queue.cancel(job_id)
    }

    /// Remove retained terminal export evidence after explicit user cleanup.
    pub fn clear_completed_exports(&self) {
        self.render_queue.clear_completed();
    }

    /// Snapshot bounded offline export execution evidence.
    pub fn export_queue_diagnostics(&self) -> ExportQueueDiagnostics {
        self.render_queue.diagnostics()
    }

    /// Observe queue changes without consuming evidence needed by another observer.
    pub fn poll_export_queue(&mut self) -> bool {
        let revision = self.render_queue.revision();
        if revision == self.export_queue_observed_revision {
            return false;
        }
        self.export_queue_observed_revision = revision;
        true
    }
}

pub(crate) fn capture_timeline_export_snapshot(
    state: &AppState,
    sequence: mondrian_timeline::sequence::Sequence,
    sequences: Vec<mondrian_timeline::sequence::Sequence>,
    range: TimelineExportRange,
) -> std::result::Result<TimelineExportSnapshot, String> {
    let mut asset_ids = HashSet::new();
    let mut visited_sequences = HashSet::new();
    let mut active_sequences = HashSet::new();
    collect_sequence_asset_ids(
        &sequence,
        &sequences,
        &mut visited_sequences,
        &mut active_sequences,
        &mut asset_ids,
    )?;

    let media = resolve_export_media_dependencies(state, asset_ids)?;
    let nested_sequences = sequences
        .into_iter()
        .filter(|candidate| {
            candidate.id != sequence.id && visited_sequences.contains(&candidate.id)
        })
        .collect();

    Ok(TimelineExportSnapshot {
        sequence,
        sequences: nested_sequences,
        media,
        range,
        project_color_management: state.project_settings.color_management.clone(),
    })
}

fn resolve_export_media_dependencies(
    state: &AppState,
    asset_ids: HashSet<AssetId>,
) -> std::result::Result<HashMap<AssetId, ExportMediaDependency>, String> {
    if asset_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let library = state.asset_library.as_ref().ok_or_else(|| "素材库未连接".to_string())?;

    let mut media = HashMap::new();
    for asset_id in asset_ids {
        let asset = library
            .get_asset(asset_id)
            .map_err(|err| format!("读取素材 {} 失败: {}", asset_id, err))?
            .ok_or_else(|| format!("素材不存在: {}", asset_id))?;

        if matches!(asset.kind, AssetKind::AdjustmentLayer) {
            continue;
        }

        let metadata = std::fs::metadata(&asset.path)
            .map_err(|error| format!("素材离线: {} ({error})", asset.path.display()))?;
        let detected_color_space =
            asset.media_info.primary_video().and_then(|video| video.detected_color_space);
        let color_diagnostic = asset
            .media_info
            .primary_video()
            .map(mondrian_media::VideoColorDiagnostic::from_stream);
        media.insert(
            asset_id,
            ExportMediaDependency {
                path: asset.path,
                source_fingerprint: mondrian_media::MediaFileFingerprint::from_metadata(&metadata),
                detected_color_space,
                interpretation: asset.interpretation,
                color_diagnostic,
            },
        );
    }

    Ok(media)
}

pub(crate) fn collect_sequence_asset_ids(
    sequence: &mondrian_timeline::sequence::Sequence,
    sequences: &[mondrian_timeline::sequence::Sequence],
    visited_sequences: &mut HashSet<SequenceId>,
    active_sequences: &mut HashSet<SequenceId>,
    asset_ids: &mut HashSet<mondrian_core::types::AssetId>,
) -> std::result::Result<(), String> {
    if active_sequences.contains(&sequence.id) {
        return Err(format!("嵌套序列形成循环: {}", sequence.id));
    }
    if visited_sequences.contains(&sequence.id) {
        return Ok(());
    }
    active_sequences.insert(sequence.id);

    for track in sequence.video_tracks.iter().chain(sequence.audio_tracks.iter()) {
        for clip in &track.clips {
            if clip.is_disabled {
                continue;
            }
            if clip.is_nested_sequence() {
                let Some(nested_sequence_id) = clip.nested_sequence_id else {
                    return Err(format!("嵌套序列片段缺少序列引用: {}", clip.id));
                };
                let nested_sequence = sequences
                    .iter()
                    .find(|sequence| sequence.id == nested_sequence_id)
                    .ok_or_else(|| format!("嵌套序列不存在: {nested_sequence_id}"))?;
                collect_sequence_asset_ids(
                    nested_sequence,
                    sequences,
                    visited_sequences,
                    active_sequences,
                    asset_ids,
                )?;
                continue;
            }
            asset_ids.insert(clip.asset_id);
        }
    }
    active_sequences.remove(&sequence.id);
    visited_sequences.insert(sequence.id);
    Ok(())
}

fn export_error(step_id: &'static str, reason: String) -> MondrianError {
    MondrianError::WorkflowStepFailed { step_id: step_id.to_string(), reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tt(frame: i64, time_base: mondrian_core::Rational) -> mondrian_core::TimelineTime {
        let numerator = frame.checked_mul(time_base.num).expect("test time fits i64");
        mondrian_core::TimelineTime::new(numerator, time_base.den).expect("valid test time")
    }
    use mondrian_assets::AssetLibrary;
    use mondrian_core::timeline_data::{AssetMediaInterpretation, MediaColorInterpretation};
    use mondrian_core::types::ColorSpace;
    use mondrian_timeline::{clip::Clip, sequence::Sequence};

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

    #[test]
    fn build_media_dependencies_skips_synthetic_adjustment_assets() {
        let mut state = AppState {
            sequence: Some(Sequence::new("export-adjustment")),
            ..Default::default()
        };
        let temp_root = std::env::temp_dir().join(format!(
            "mondrian-export-adjustment-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        state.asset_library = Some(AssetLibrary::open(temp_root.clone()).expect("open library"));

        let asset_id = state.create_adjustment_layer_asset(None).expect("create adjustment asset");
        {
            let seq = state.sequence.as_mut().expect("sequence should exist");
            let tb = seq.time_base();
            seq.video_tracks[0]
                .add_clip(
                    Clip::new_adjustment_layer(asset_id, tt(0, tb), tt(20, tb))
                        .expect("valid clip"),
                )
                .expect("add adjustment clip");
        }

        let seq = state.sequence.as_ref().expect("sequence should exist").clone();
        let snapshot = capture_timeline_export_snapshot(
            &state,
            seq.clone(),
            vec![seq],
            TimelineExportRange::EntireSequence,
        )
        .expect("capture export snapshot");
        assert!(!snapshot.media.contains_key(&asset_id));

        let _ = std::fs::remove_dir_all(temp_root);
    }

    #[test]
    fn build_media_dependencies_keeps_interpretation_and_source_revision_together() {
        let mut state = AppState {
            sequence: Some(Sequence::new("export-interpretation")),
            ..Default::default()
        };
        let temp_root = std::env::temp_dir().join(format!(
            "mondrian-export-interpretation-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        let media_path = temp_root.join("tone.wav");
        std::fs::create_dir_all(&temp_root).expect("create temp root");
        write_minimal_wav(&media_path);
        let library = AssetLibrary::open(temp_root.join("library")).expect("open library");
        let asset_id = library.import_media_file(&media_path).expect("import media");
        let interpretation = AssetMediaInterpretation {
            color: MediaColorInterpretation::Override {
                color_space: ColorSpace::SonySLog3SGamut3Cine,
            },
            ..AssetMediaInterpretation::default()
        };
        library
            .set_asset_interpretation(asset_id, interpretation)
            .expect("set interpretation");
        state.asset_library = Some(library);
        {
            let seq = state.sequence.as_mut().expect("sequence should exist");
            let tb = seq.time_base();
            seq.audio_tracks[0]
                .add_clip(Clip::new(asset_id, tt(0, tb), tt(20, tb)).expect("valid clip"))
                .expect("add audio clip");
        }

        let seq = state.sequence.as_ref().expect("sequence should exist").clone();
        let snapshot = capture_timeline_export_snapshot(
            &state,
            seq.clone(),
            vec![seq],
            TimelineExportRange::EntireSequence,
        )
        .expect("capture export snapshot");

        let dependency = snapshot.media.get(&asset_id).expect("captured dependency");
        assert_eq!(dependency.interpretation, interpretation);
        assert_eq!(
            std::fs::canonicalize(&dependency.path).expect("canonical dependency path"),
            std::fs::canonicalize(&media_path).expect("canonical fixture path")
        );
        assert_eq!(
            dependency.source_fingerprint,
            mondrian_media::MediaFileFingerprint::capture(dependency.path.as_path())
        );

        let _ = std::fs::remove_dir_all(temp_root);
    }

    #[test]
    fn collect_sequence_asset_ids_recurses_into_nested_sequences() {
        let mut parent = Sequence::new("parent");
        let mut child = Sequence::new("child");
        let tb = parent.time_base();
        let asset_id = mondrian_core::types::AssetId::new();

        child.video_tracks[0]
            .add_clip(Clip::new(asset_id, tt(0, tb), tt(12, tb)).expect("valid clip"))
            .expect("add media");
        parent.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(
                    child.id,
                    tt(0, tb),
                    tt(12, tb),
                    Some("child".to_string()),
                )
                .expect("valid clip"),
            )
            .expect("add nested");

        let sequences = vec![parent.clone(), child];
        let mut visited = HashSet::new();
        let mut active = HashSet::new();
        let mut assets = HashSet::new();
        collect_sequence_asset_ids(&parent, &sequences, &mut visited, &mut active, &mut assets)
            .expect("collect nested assets");

        assert!(assets.contains(&asset_id));
    }

    #[test]
    fn export_snapshot_contains_only_reachable_nested_sequence_closure() {
        let state = AppState::default();
        let mut parent = Sequence::new("parent");
        let child = Sequence::new("child");
        let unrelated = Sequence::new("unrelated");
        let tb = parent.time_base();
        parent.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(
                    child.id,
                    tt(0, tb),
                    tt(12, tb),
                    Some("child".to_string()),
                )
                .expect("valid nested clip"),
            )
            .expect("add nested clip");

        let snapshot = capture_timeline_export_snapshot(
            &state,
            parent.clone(),
            vec![parent, child.clone(), unrelated],
            TimelineExportRange::EntireSequence,
        )
        .expect("capture reachable closure");

        assert_eq!(snapshot.sequences.len(), 1);
        assert_eq!(snapshot.sequences[0].id, child.id);
    }

    #[test]
    fn export_snapshot_rejects_recursive_sequence_nesting() {
        let state = AppState::default();
        let mut parent = Sequence::new("parent");
        let mut child = Sequence::new("child");
        let tb = parent.time_base();
        parent.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(
                    child.id,
                    tt(0, tb),
                    tt(12, tb),
                    Some("child".to_string()),
                )
                .expect("valid nested clip"),
            )
            .expect("add child clip");
        child.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(
                    parent.id,
                    tt(0, tb),
                    tt(12, tb),
                    Some("parent".to_string()),
                )
                .expect("valid nested clip"),
            )
            .expect("add parent clip");

        let error = capture_timeline_export_snapshot(
            &state,
            parent.clone(),
            vec![parent, child],
            TimelineExportRange::EntireSequence,
        )
        .expect_err("recursive nesting must fail closed");

        assert!(error.contains("循环"));
    }

    #[test]
    fn enqueue_timeline_export_rejects_empty_output_path() {
        let mut state = AppState {
            sequence: Some(Sequence::new("empty-output")),
            ..Default::default()
        };

        let err = state
            .enqueue_timeline_export(TimelineExportRequest {
                preset: ExportPreset::youtube_1080p(),
                sequence_id: None,
                range: TimelineExportRange::EntireSequence,
                output_path: PathBuf::new(),
            })
            .expect_err("empty output path should be rejected");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
        assert!(state.render_queue.list_jobs().is_empty());
    }

    #[test]
    fn enqueue_timeline_export_rejects_missing_explicit_sequence_id() {
        let mut state = AppState {
            sequence: Some(Sequence::new("active")),
            ..Default::default()
        };
        let temp_root = std::env::temp_dir().join(format!(
            "mondrian-export-stale-sequence-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        state.asset_library = Some(AssetLibrary::open(temp_root.clone()).expect("open library"));

        let err = state
            .enqueue_timeline_export(TimelineExportRequest {
                preset: ExportPreset::youtube_1080p(),
                sequence_id: Some(SequenceId::new()),
                range: TimelineExportRange::EntireSequence,
                output_path: PathBuf::from("E:/renders/out.mp4"),
            })
            .expect_err("stale explicit sequence id should be rejected");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
        assert!(state.render_queue.list_jobs().is_empty());

        let _ = std::fs::remove_dir_all(temp_root);
    }
}
