//! Timeline export orchestration shared by all UI frontends.

use super::*;
use mondrian_core::{JobId, MondrianError, Result};
use mondrian_export::delivery::resolve_export_delivery;
use mondrian_export::prepare_timeline_export_dependencies_with_audio_selection;
use mondrian_export::preset::{
    BuiltinExportPreset, Container, ExportAudioProgramSelection, ExportConfig,
    ExportMediaDependency, ExportOutputPolicy, ExportPreset, TimelineExportRange,
    TimelineExportSnapshot,
};
use mondrian_export::queue::{
    ExportCancelOutcome, ExportJobSnapshot, ExportQueueDiagnostics, RenderJob,
};
use serde::{Deserialize, Serialize};

/// Request to enqueue a timeline export job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineExportRequest {
    /// Preset used for codec/container defaults.
    pub preset: ExportPreset,
    /// Optional sequence id; falls back to the active sequence when absent.
    pub sequence_id: Option<SequenceId>,
    /// Timeline range to render.
    pub range: TimelineExportRange,
    /// Output media-file path or image-sequence directory path.
    pub output_path: PathBuf,
    /// Final namespace policy frozen at admission.
    pub output_policy: ExportOutputPolicy,
}

/// UI-stable draft state for timeline export panels.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineExportDraft {
    /// Stable built-in preset used as the reset point for the editable draft.
    pub selected_builtin_preset: BuiltinExportPreset,
    /// Fully materialized, user-editable delivery contract.
    ///
    /// This remains app-session draft state. An admitted job receives an
    /// immutable clone and never reads this value again.
    pub preset: ExportPreset,
    /// Selected sequence; `None` means use the active/default sequence.
    pub selected_sequence_id: Option<SequenceId>,
    /// Timeline range to render.
    pub range: TimelineExportRange,
    /// User-entered output file path.
    pub output_path: String,
}

impl Default for TimelineExportDraft {
    fn default() -> Self {
        let selected_builtin_preset = BuiltinExportPreset::default();
        Self {
            selected_builtin_preset,
            preset: selected_builtin_preset.preset(),
            selected_sequence_id: None,
            range: TimelineExportRange::SequenceInOut,
            output_path: String::new(),
        }
    }
}

/// One named export preset shown by UI frontends.
#[derive(Debug, Clone)]
pub struct ExportPresetOption {
    /// Stable product identity; UI order is presentation only.
    pub id: BuiltinExportPreset,
    pub label: String,
    pub preset: ExportPreset,
}

/// Built-in export presets shared by app UI panels.
pub fn builtin_export_presets() -> Vec<ExportPresetOption> {
    BuiltinExportPreset::ALL
        .into_iter()
        .map(|builtin| ExportPresetOption {
            id: builtin,
            label: builtin.label().to_owned(),
            preset: builtin.preset(),
        })
        .collect()
}

/// File or directory suffix implied by an export artifact.
pub fn export_preset_extension(preset: &ExportPreset) -> &'static str {
    if preset.audio_stem_format().is_some() {
        return "wavstems";
    }
    match preset.media_file().map(|media| media.container) {
        Some(Container::Mp4) => "mp4",
        Some(Container::Mov) => "mov",
        Some(Container::Mkv) => "mkv",
        Some(Container::Gif) => "gif",
        Some(Container::Mxf) => "mxf",
        Some(Container::Webm) => "webm",
        None => "pngseq",
    }
}

impl AppState {
    /// Reset the editable export draft to one stable built-in preset.
    ///
    /// Returns whether the preset identity, materialized settings, or a
    /// container-following output extension changed.
    pub fn set_export_draft_builtin_preset(&mut self, preset: BuiltinExportPreset) -> bool {
        let next = preset.preset();
        let changed =
            self.export_draft.selected_builtin_preset != preset || self.export_draft.preset != next;
        let extension_changed = self.rewrite_export_draft_extension_for(&next);
        self.export_draft.selected_builtin_preset = preset;
        self.export_draft.preset = next;
        changed || extension_changed
    }

    /// Replace the draft's materialized delivery settings.
    ///
    /// Syntactic editing stays permissive so a user can move between legal
    /// configurations without hidden auto-correction. The panel and queue both
    /// call `resolve_export_delivery` and fail closed until the complete
    /// combination is valid. Returns whether settings or a
    /// container-following output extension changed.
    pub fn set_export_draft_preset(&mut self, preset: ExportPreset) -> bool {
        let changed = self.export_draft.preset != preset;
        let extension_changed = self.rewrite_export_draft_extension_for(&preset);
        self.export_draft.preset = preset;
        changed || extension_changed
    }

    fn rewrite_export_draft_extension_for(&mut self, next: &ExportPreset) -> bool {
        let previous_extension = export_preset_extension(&self.export_draft.preset);
        let next_extension = export_preset_extension(next);
        if previous_extension == next_extension || self.export_draft.output_path.trim().is_empty() {
            return false;
        }
        let mut path = PathBuf::from(&self.export_draft.output_path);
        let follows_previous_container = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case(previous_extension));
        if follows_previous_container {
            path.set_extension(next_extension);
            self.export_draft.output_path = path.to_string_lossy().into_owned();
            true
        } else {
            false
        }
    }

    /// Update the export draft Sequence, returning whether it changed.
    pub fn set_export_draft_sequence_id(&mut self, sequence_id: Option<SequenceId>) -> bool {
        if self.export_draft.selected_sequence_id == sequence_id {
            return false;
        }
        self.export_draft.selected_sequence_id = sequence_id;
        true
    }

    /// Update the export draft Timeline range, returning whether it changed.
    pub fn set_export_draft_range(&mut self, range: TimelineExportRange) -> bool {
        if self.export_draft.range == range {
            return false;
        }
        self.export_draft.range = range;
        true
    }

    /// Update the export draft output path, returning whether it changed.
    pub fn set_export_draft_output_path(&mut self, output_path: impl Into<String>) -> bool {
        let output_path = output_path.into();
        if self.export_draft.output_path == output_path {
            return false;
        }
        self.export_draft.output_path = output_path;
        true
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
            None => self.active_sequence().cloned(),
        };
        let Some(sequence) = sequence else {
            let reason = "当前无序列".to_string();
            self.set_status_hint(format!("导出失败：{reason}"), true);
            return Err(export_error("enqueue_timeline_export", reason));
        };

        if let Err(error) = resolve_export_delivery(
            &request.preset,
            &sequence.settings,
            self.project_color_environment(),
        ) {
            let reason = error.to_string();
            self.set_status_hint(format!("导出失败：{reason}"), true);
            return Err(export_error("enqueue_timeline_export", reason));
        }

        let audio_selection = request.preset.audio_program_selection();
        let timeline = match capture_timeline_export_snapshot_with_audio_selection(
            self,
            sequence,
            sequences,
            request.range,
            audio_selection,
        ) {
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
            output_policy: request.output_policy,
            smart_render: mondrian_export::ExportSmartRenderPolicy::Automatic,
        };
        let output_path = config.output_path.display().to_string();
        let job_id = self.render_queue.enqueue(RenderJob::new(config)).map_err(|error| {
            let reason = error.to_string();
            self.set_status_hint(format!("导出失败：{reason}"), true);
            export_error("enqueue_timeline_export", reason)
        })?;
        let _ = self.refresh_internal_execution_resource_decision();
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
        let outcome = self.render_queue.cancel(job_id);
        let _ = self.refresh_internal_execution_resource_decision();
        outcome
    }

    /// Remove retained terminal export evidence after explicit user cleanup.
    ///
    /// Returns the exact number of terminal snapshots removed.
    pub fn clear_terminal_export_history(&self) -> usize {
        self.render_queue.clear_terminal_history()
    }

    /// Snapshot bounded offline export execution evidence.
    pub fn export_queue_diagnostics(&self) -> ExportQueueDiagnostics {
        self.render_queue.diagnostics()
    }

    /// Observe retained job-snapshot changes without consuming shared evidence.
    ///
    /// Queue policy and execution-yield diagnostics are intentionally excluded:
    /// they must not dirty the editor model or schedule unrelated Preview work.
    pub fn poll_export_queue(&mut self) -> bool {
        let revision = self.render_queue.jobs_revision();
        if revision == self.export_jobs_observed_revision {
            return false;
        }
        self.export_jobs_observed_revision = revision;
        let _ = self.refresh_internal_execution_resource_decision();
        true
    }
}

#[cfg(any(test, feature = "validation"))]
pub(crate) fn capture_timeline_export_snapshot(
    state: &AppState,
    sequence: mondrian_timeline::sequence::Sequence,
    sequences: Vec<mondrian_timeline::sequence::Sequence>,
    range: TimelineExportRange,
    include_audio: bool,
) -> std::result::Result<TimelineExportSnapshot, String> {
    capture_timeline_export_snapshot_with_audio_selection(
        state,
        sequence,
        sequences,
        range,
        if include_audio {
            ExportAudioProgramSelection::Primary
        } else {
            ExportAudioProgramSelection::Disabled
        },
    )
}

pub(crate) fn capture_timeline_export_snapshot_with_audio_selection(
    state: &AppState,
    sequence: mondrian_timeline::sequence::Sequence,
    sequences: Vec<mondrian_timeline::sequence::Sequence>,
    range: TimelineExportRange,
    audio_selection: ExportAudioProgramSelection,
) -> std::result::Result<TimelineExportSnapshot, String> {
    let dependencies = prepare_timeline_export_dependencies_with_audio_selection(
        &sequence,
        &sequences,
        range,
        audio_selection,
    )
    .map_err(|error| error.to_string())?;

    let media = resolve_export_media_dependencies(state, dependencies.media_components())?;
    let nested_sequences = sequences
        .into_iter()
        .filter(|candidate| {
            candidate.id != sequence.id && dependencies.sequence_ids().contains(&candidate.id)
        })
        .collect();

    Ok(TimelineExportSnapshot::captured(
        state.project_color_environment().clone(),
        sequence,
        nested_sequences,
        media,
        range,
        dependencies.execution_snapshot().clone(),
    ))
}

fn resolve_export_media_dependencies(
    state: &AppState,
    media_demands: &std::collections::BTreeMap<
        AssetId,
        std::collections::BTreeSet<AudioSourceComponentId>,
    >,
) -> std::result::Result<HashMap<AssetId, ExportMediaDependency>, String> {
    if media_demands.is_empty() {
        return Ok(HashMap::new());
    }
    let library = state.asset_library().ok_or_else(|| "素材库未连接".to_string())?;

    let mut media = HashMap::new();
    for (asset_id, component_ids) in media_demands {
        let asset = library
            .get_asset(*asset_id)
            .map_err(|err| format!("读取素材 {} 失败: {}", asset_id, err))?
            .ok_or_else(|| format!("素材不存在: {}", asset_id))?;

        if matches!(
            &asset.kind,
            AssetKind::AdjustmentLayer | AssetKind::SolidColor
        ) {
            continue;
        }

        let path = asset
            .file_path()
            .ok_or_else(|| format!("素材 {asset_id} 不是可导出的文件媒体源"))?
            .to_path_buf();
        let media_probe = asset
            .media_probe()
            .ok_or_else(|| format!("素材 {asset_id} 缺少与当前素材库状态一致的媒体探测结果"))?;
        let admitted_fingerprint = asset
            .source_fingerprint()
            .ok_or_else(|| format!("素材 {asset_id} 缺少已准入的文件修订标识"))?;
        let source_fingerprint = mondrian_media::MediaFileFingerprint::capture(&path);
        if !source_fingerprint.authorizes_reuse() {
            return Err(format!(
                "素材 {} 缺少完整的文件修订证据，无法安全导出",
                path.display()
            ));
        }
        if source_fingerprint != admitted_fingerprint {
            return Err(format!(
                "素材 {asset_id} 的文件修订已变化，必须重新探测后才能导出"
            ));
        }
        let primary_video = media_probe.primary_video();
        let color_diagnostic = primary_video.map(mondrian_media::VideoColorDiagnostic::from_stream);
        let mut audio_components = HashMap::new();
        for component_id in component_ids {
            let stream = asset
                .audio_components
                .resolve_current(*component_id, media_probe, source_fingerprint)
                .map_err(|error| {
                    format!("素材 {asset_id} 的音频 Component {component_id} 无法绑定: {error}")
                })?;
            let selection =
                mondrian_media::AudioSourceSelection::from_stream(stream, source_fingerprint);
            audio_components.insert(*component_id, selection);
        }
        media.insert(
            *asset_id,
            ExportMediaDependency {
                path,
                source_fingerprint,
                source_container: media_probe.container.clone(),
                source_video_stream: primary_video.cloned(),
                video_stream_index: primary_video.map(|video| video.index),
                picture_source_extent: primary_video.and_then(|video| {
                    export_picture_source_extent(&asset.kind, video.duration, video.total_frames)
                }),
                source_resolution: primary_video
                    .map(|video| Resolution { width: video.width, height: video.height }),
                picture: primary_video.map(|video| video.picture),
                audio_components,
                interpretation: asset.interpretation,
                color_diagnostic,
            },
        );
    }

    Ok(media)
}

fn export_picture_source_extent(
    asset_kind: &AssetKind,
    stream_duration: Option<std::time::Duration>,
    _total_frames: Option<u64>,
) -> Option<mondrian_timeline::PictureSourceExtent> {
    if matches!(asset_kind, AssetKind::StillImage) {
        return Some(mondrian_timeline::PictureSourceExtent::Still);
    }
    if !matches!(asset_kind, AssetKind::Video) {
        return None;
    }
    let duration = stream_duration.filter(|duration| !duration.is_zero())?;
    let nanoseconds = i64::try_from(duration.as_nanos()).ok()?;
    let duration = mondrian_core::TimelineTime::new(nanoseconds, 1_000_000_000).ok()?;
    let range =
        mondrian_core::TimelineTimeRange::new(mondrian_core::TimelineTime::ZERO, duration).ok()?;
    Some(mondrian_timeline::PictureSourceExtent::TimelineRange(range))
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

    #[test]
    fn still_authority_comes_from_asset_kind_not_one_frame_movie_count() {
        let one_frame_duration = std::time::Duration::from_millis(40);
        let movie =
            export_picture_source_extent(&AssetKind::Video, Some(one_frame_duration), Some(1))
                .expect("one-frame Movie has finite picture time");
        let expected_duration = mondrian_core::TimelineTime::new(40, 1_000).expect("40 ms");
        assert_eq!(
            movie,
            mondrian_timeline::PictureSourceExtent::TimelineRange(
                mondrian_core::TimelineTimeRange::new(
                    mondrian_core::TimelineTime::ZERO,
                    expected_duration,
                )
                .expect("finite Movie extent"),
            )
        );
        assert_eq!(
            export_picture_source_extent(&AssetKind::StillImage, None, Some(1)),
            Some(mondrian_timeline::PictureSourceExtent::Still)
        );
        assert_eq!(
            export_picture_source_extent(&AssetKind::Video, None, Some(1)),
            None,
            "one-frame Movie without selected-stream duration must fail closed"
        );
    }

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
    fn build_media_dependencies_skips_generated_library_assets() {
        let mut state = AppState::default();
        state.test_set_sequence(Some(Sequence::new("export-generated-content")));
        let temp_root = std::env::temp_dir().join(format!(
            "mondrian-export-generated-content-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        state.test_set_asset_library(Some(
            AssetLibrary::open(temp_root.clone()).expect("open library"),
        ));

        let adjustment_id =
            state.create_adjustment_layer_asset(None).expect("create adjustment asset");
        let solid_id = state.create_solid_color_asset(None).expect("create solid asset");
        {
            let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
            let tb = seq.time_base();
            seq.video_tracks[0]
                .add_clip(
                    Clip::new_adjustment_layer(adjustment_id, tt(0, tb), tt(20, tb))
                        .expect("valid clip"),
                )
                .expect("add adjustment clip");
            seq.video_tracks[0]
                .add_clip(
                    Clip::new_solid_color(
                        solid_id,
                        mondrian_core::Color::from_hex(0x336699),
                        tt(20, tb),
                        tt(20, tb),
                    )
                    .expect("valid solid Clip"),
                )
                .expect("add solid clip");
        }

        let seq = state.active_sequence().expect("sequence should exist").clone();
        let snapshot = capture_timeline_export_snapshot(
            &state,
            seq.clone(),
            vec![seq],
            TimelineExportRange::EntireSequence,
            false,
        )
        .expect("capture export snapshot");
        assert!(snapshot.media.is_empty());

        let _ = std::fs::remove_dir_all(temp_root);
    }

    #[test]
    fn selected_range_does_not_require_off_range_or_hidden_media() {
        let state = AppState::default();
        let mut sequence = Sequence::new("selected capture");
        let time_base = sequence.time_base();
        let off_range_asset = AssetId::new();
        sequence.video_tracks[0]
            .add_clip(
                Clip::new(off_range_asset, tt(20, time_base), tt(10, time_base))
                    .expect("off-range Clip"),
            )
            .expect("add off-range Clip");
        let mut hidden = mondrian_timeline::track::Track::new_video("hidden");
        hidden.is_visible = false;
        hidden
            .add_clip(
                Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base))
                    .expect("hidden Clip"),
            )
            .expect("add hidden Clip");
        sequence.video_tracks.push(hidden);

        let snapshot = capture_timeline_export_snapshot(
            &state,
            sequence.clone(),
            vec![sequence],
            TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 10 },
            false,
        )
        .expect("unselected media does not require an Asset Library");
        assert!(snapshot.media.is_empty());
    }

    #[test]
    fn selected_audio_capture_does_not_bind_muted_track_media() {
        let state = AppState::default();
        let mut sequence = Sequence::new("muted audio capture");
        let time_base = sequence.time_base();
        let track_id = sequence.audio_tracks[0].id;
        sequence
            .add_media_audio_clip(
                track_id,
                Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base)).expect("audio Clip"),
                AudioSourceComponentId::primary(),
            )
            .expect("add audio Clip");
        sequence.audio_tracks[0].is_muted = true;

        let snapshot = capture_timeline_export_snapshot(
            &state,
            sequence.clone(),
            vec![sequence],
            TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 10 },
            true,
        )
        .expect("muted audio is not a physical dependency");
        assert!(snapshot.media.is_empty());
    }

    #[test]
    fn build_media_dependencies_keeps_interpretation_and_source_revision_together() {
        let mut state = AppState::default();
        state.test_set_sequence(Some(Sequence::new("export-interpretation")));
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
        let media_path = std::fs::canonicalize(media_path).expect("canonical export media fixture");
        let library = AssetLibrary::open(temp_root.join("library")).expect("open library");
        let media_info = mondrian_media::probe_media_info(&media_path).expect("probe media");
        let fingerprint = mondrian_media::MediaFileFingerprint::capture(&media_path);
        let candidate = mondrian_assets::AssetMediaProbeCandidate::new(
            media_path.clone(),
            fingerprint,
            media_info,
        )
        .expect("valid media candidate");
        let asset_id = library.commit_media_probe(candidate, None).expect("import media");
        let interpretation = AssetMediaInterpretation {
            color: MediaColorInterpretation::Override {
                color_space: ColorSpace::SonySLog3SGamut3Cine,
            },
            ..AssetMediaInterpretation::default()
        };
        library
            .set_asset_interpretation(asset_id, interpretation)
            .expect("set interpretation");
        state.test_set_asset_library(Some(library));
        {
            let seq = state.active_sequence_mut_uncommitted().expect("sequence should exist");
            let tb = seq.time_base();
            let track_id = seq.audio_tracks[0].id;
            seq.add_media_audio_clip(
                track_id,
                Clip::new(asset_id, tt(0, tb), tt(20, tb)).expect("valid clip"),
                AudioSourceComponentId::primary(),
            )
            .expect("add audio clip");
        }

        let seq = state.active_sequence().expect("sequence should exist").clone();
        let snapshot = capture_timeline_export_snapshot(
            &state,
            seq.clone(),
            vec![seq],
            TimelineExportRange::EntireSequence,
            true,
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
        let audio_selection = dependency
            .audio_components
            .get(&AudioSourceComponentId::primary())
            .expect("frozen primary audio binding");
        assert_eq!(audio_selection.stream_index(), 0);
        assert_eq!(
            audio_selection.source_layout(),
            &mondrian_media::info::ChannelLayout::Unspecified(1)
        );

        let _ = std::fs::remove_dir_all(temp_root);
    }

    #[test]
    fn export_snapshot_freezes_exact_physical_video_stream_index() {
        const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/small/h264-bframes.mp4");
        let mut state = AppState::default();
        state.test_set_sequence(Some(Sequence::new("export-video-stream-binding")));
        let temp_root = std::env::temp_dir().join(format!(
            "mondrian-export-video-stream-binding-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_root).expect("create temp root");
        let media_path = temp_root.join("multi-stream-index.mp4");
        std::fs::write(&media_path, FIXTURE).expect("write synthetic video fixture");
        let media_path = std::fs::canonicalize(media_path).expect("canonical video fixture");
        let library = AssetLibrary::open(temp_root.join("library")).expect("open library");
        let mut media_info = mondrian_media::probe_media_info(&media_path).expect("probe media");
        media_info.video_streams.first_mut().expect("fixture video stream").index = 3;
        let fingerprint = mondrian_media::MediaFileFingerprint::capture(&media_path);
        let candidate =
            mondrian_assets::AssetMediaProbeCandidate::new(media_path, fingerprint, media_info)
                .expect("valid media candidate");
        let asset_id = library.commit_media_probe(candidate, None).expect("import media");
        state.test_set_asset_library(Some(library));
        {
            let sequence = state.active_sequence_mut_uncommitted().expect("sequence should exist");
            let time_base = sequence.time_base();
            sequence.video_tracks[0]
                .add_clip(Clip::new(asset_id, tt(0, time_base), tt(20, time_base)).expect("clip"))
                .expect("add video clip");
        }

        let sequence = state.active_sequence().expect("sequence should exist").clone();
        let snapshot = capture_timeline_export_snapshot(
            &state,
            sequence.clone(),
            vec![sequence],
            TimelineExportRange::EntireSequence,
            false,
        )
        .expect("capture export snapshot");
        let dependency = snapshot.media.get(&asset_id).expect("captured dependency");
        assert_eq!(dependency.video_stream_index, Some(3));

        let _ = std::fs::remove_dir_all(temp_root);
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
            false,
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
            false,
        )
        .expect_err("recursive nesting must fail closed");

        assert!(error.contains("cycle"));
    }

    #[test]
    fn enqueue_timeline_export_rejects_empty_output_path() {
        let mut state = AppState::default();
        state.test_set_sequence(Some(Sequence::new("empty-output")));

        let err = state
            .enqueue_timeline_export(TimelineExportRequest {
                preset: ExportPreset::h264_aac_sdr_1080p(),
                sequence_id: None,
                range: TimelineExportRange::EntireSequence,
                output_path: PathBuf::new(),
                output_policy: ExportOutputPolicy::CreateNew,
            })
            .expect_err("empty output path should be rejected");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
        assert!(state.render_queue.list_jobs().is_empty());
    }

    #[test]
    fn builtin_preset_resets_the_materialized_export_draft_without_using_catalog_order() {
        let mut state = AppState::default();
        let mut customized = ExportPreset::h264_aac_sdr_1080p();
        customized.video_signal.bit_depth =
            mondrian_export::preset::ExportParameter::FollowSequence;
        state.set_export_draft_preset(customized);

        state.set_export_draft_builtin_preset(BuiltinExportPreset::HevcMain10Aac);

        assert_eq!(
            state.export_draft.selected_builtin_preset,
            BuiltinExportPreset::HevcMain10Aac
        );
        assert_eq!(
            state.export_draft.preset,
            BuiltinExportPreset::HevcMain10Aac.preset()
        );
    }

    #[test]
    fn changing_container_rewrites_only_an_extension_that_followed_the_previous_contract() {
        let mut state = AppState::default();
        state.set_export_draft_output_path("E:/renders/delivery.mp4");
        let mut mov = state.export_draft.preset.clone();
        mov.media_file_mut().expect("media preset").container = Container::Mov;

        state.set_export_draft_preset(mov.clone());

        assert_eq!(
            PathBuf::from(&state.export_draft.output_path),
            PathBuf::from("E:/renders/delivery.mov")
        );

        state.set_export_draft_output_path("E:/renders/delivery.custom");
        mov.media_file_mut().expect("media preset").container = Container::Mxf;
        state.set_export_draft_preset(mov);

        assert_eq!(state.export_draft.output_path, "E:/renders/delivery.custom");
    }

    #[test]
    fn enqueue_timeline_export_rejects_missing_explicit_sequence_id() {
        let mut state = AppState::default();
        state.test_set_sequence(Some(Sequence::new("active")));
        let temp_root = std::env::temp_dir().join(format!(
            "mondrian-export-stale-sequence-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        state.test_set_asset_library(Some(
            AssetLibrary::open(temp_root.clone()).expect("open library"),
        ));

        let err = state
            .enqueue_timeline_export(TimelineExportRequest {
                preset: ExportPreset::h264_aac_sdr_1080p(),
                sequence_id: Some(SequenceId::new()),
                range: TimelineExportRange::EntireSequence,
                output_path: PathBuf::from("E:/renders/out.mp4"),
                output_policy: ExportOutputPolicy::CreateNew,
            })
            .expect_err("stale explicit sequence id should be rejected");

        assert!(matches!(err, MondrianError::WorkflowStepFailed { .. }));
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
        assert!(state.render_queue.list_jobs().is_empty());

        let _ = std::fs::remove_dir_all(temp_root);
    }
}
