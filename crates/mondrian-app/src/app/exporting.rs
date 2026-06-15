//! Timeline export orchestration shared by all UI frontends.

use super::*;
use mondrian_core::{MondrianError, Result};
use mondrian_export::preset::{
    ExportConfig, ExportInput, ExportPreset, TimelineExportInput, TimelineExportRange,
};
use mondrian_export::queue::RenderJob;

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

impl AppState {
    /// Build and enqueue a render job from a timeline export request.
    pub fn enqueue_timeline_export(&mut self, request: TimelineExportRequest) -> Result<()> {
        if request.output_path.as_os_str().is_empty() {
            let reason = "请指定输出路径".to_string();
            self.set_status_hint(format!("导出失败：{reason}"), true);
            return Err(export_error("enqueue_timeline_export", reason));
        }

        let sequences = self.export_sequences_snapshot();
        let Some(sequence) = request
            .sequence_id
            .and_then(|id| sequences.iter().find(|sequence| sequence.id == id))
            .cloned()
            .or_else(|| self.sequence.clone())
        else {
            let reason = "当前无序列".to_string();
            self.set_status_hint(format!("导出失败：{reason}"), true);
            return Err(export_error("enqueue_timeline_export", reason));
        };

        let (asset_paths, asset_color_spaces) =
            collect_timeline_asset_paths(self, &sequence, &sequences).map_err(|reason| {
                self.set_status_hint(format!("导出失败：{reason}"), true);
                export_error("enqueue_timeline_export", reason)
            })?;

        let config = ExportConfig {
            preset: request.preset,
            input: ExportInput::Timeline(Box::new(TimelineExportInput {
                sequence,
                sequences,
                asset_paths,
                asset_color_spaces,
                range: request.range,
                project_color_management: self.project_settings.color_management.clone(),
            })),
            output_path: request.output_path,
        };
        let output_path = config.output_path.display().to_string();
        self.render_queue.enqueue(RenderJob::new(config));
        self.set_status_hint("已加入导出队列", false);
        tracing::info!("导出任务已加入队列: {output_path}");
        Ok(())
    }
}

pub(crate) type TimelineAssetPaths = (
    HashMap<mondrian_core::types::AssetId, PathBuf>,
    HashMap<mondrian_core::types::AssetId, mondrian_core::types::ColorSpace>,
);

pub(crate) fn collect_timeline_asset_paths(
    state: &AppState,
    sequence: &mondrian_timeline::sequence::Sequence,
    sequences: &[mondrian_timeline::sequence::Sequence],
) -> std::result::Result<TimelineAssetPaths, String> {
    let library = state.asset_library.as_ref().ok_or_else(|| "素材库未连接".to_string())?;

    let mut asset_ids = HashSet::new();
    let mut visited_sequences = HashSet::new();
    collect_sequence_asset_ids(sequence, sequences, &mut visited_sequences, &mut asset_ids)?;

    let mut paths = HashMap::new();
    let mut color_spaces = HashMap::new();
    for asset_id in asset_ids {
        let asset = library
            .get_asset(asset_id)
            .map_err(|err| format!("读取素材 {} 失败: {}", asset_id, err))?
            .ok_or_else(|| format!("素材不存在: {}", asset_id))?;

        if matches!(asset.kind, AssetKind::AdjustmentLayer) {
            continue;
        }

        if !asset.path.exists() {
            return Err(format!("素材离线: {}", asset.path.display()));
        }
        let color_space = asset
            .media_info
            .primary_video()
            .map(|video| video.color_space)
            .unwrap_or(mondrian_core::types::ColorSpace::Rec709);
        paths.insert(asset_id, asset.path);
        color_spaces.insert(asset_id, color_space);
    }

    Ok((paths, color_spaces))
}

pub(crate) fn collect_sequence_asset_ids(
    sequence: &mondrian_timeline::sequence::Sequence,
    sequences: &[mondrian_timeline::sequence::Sequence],
    visited_sequences: &mut HashSet<SequenceId>,
    asset_ids: &mut HashSet<mondrian_core::types::AssetId>,
) -> std::result::Result<(), String> {
    if !visited_sequences.insert(sequence.id) {
        return Ok(());
    }

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
                    asset_ids,
                )?;
                continue;
            }
            asset_ids.insert(clip.asset_id);
        }
    }
    Ok(())
}

fn export_error(step_id: &'static str, reason: String) -> MondrianError {
    MondrianError::WorkflowStepFailed { step_id: step_id.to_string(), reason }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_assets::AssetLibrary;
    use mondrian_core::types::TimeCode;
    use mondrian_timeline::{clip::Clip, sequence::Sequence};

    #[test]
    fn build_asset_paths_skips_synthetic_adjustment_assets() {
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
                .add_clip(Clip::new_adjustment_layer(
                    asset_id,
                    TimeCode::new(0, tb),
                    TimeCode::new(20, tb),
                ))
                .expect("add adjustment clip");
        }

        let seq = state.sequence.as_ref().expect("sequence should exist");
        let (paths, color_spaces) =
            collect_timeline_asset_paths(&state, seq, std::slice::from_ref(seq))
                .expect("collect asset paths");
        assert!(!paths.contains_key(&asset_id));
        assert!(!color_spaces.contains_key(&asset_id));

        let _ = std::fs::remove_dir_all(temp_root);
    }

    #[test]
    fn collect_sequence_asset_ids_recurses_into_nested_sequences() {
        let mut parent = Sequence::new("parent");
        let mut child = Sequence::new("child");
        let tb = parent.time_base();
        let asset_id = mondrian_core::types::AssetId::new();

        child.video_tracks[0]
            .add_clip(Clip::new(
                asset_id,
                TimeCode::new(0, tb),
                TimeCode::new(12, tb),
            ))
            .expect("add media");
        parent.video_tracks[0]
            .add_clip(Clip::new_nested_sequence(
                child.id,
                TimeCode::new(0, tb),
                TimeCode::new(12, tb),
                Some("child".to_string()),
            ))
            .expect("add nested");

        let sequences = vec![parent.clone(), child];
        let mut visited = HashSet::new();
        let mut assets = HashSet::new();
        collect_sequence_asset_ids(&parent, &sequences, &mut visited, &mut assets)
            .expect("collect nested assets");

        assert!(assets.contains(&asset_id));
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
}
