use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use std::{collections::hash_map::DefaultHasher, hash::Hash, hash::Hasher};
use std::{fs, path::Path, path::PathBuf};
use std::{io::Read, io::Write};
use std::{sync::mpsc, thread};

use mondrian_assets::{AssetKind, AssetLibrary};
use mondrian_core::{
    events::{AppEvent, EventBus},
    types::{AssetId, ClipId, Rational, Resolution, SequenceId, TimeCode, TrackId},
};
use mondrian_export::queue::{JobStatus, RenderQueue};
use mondrian_media::audio::{
    AudioBuffer, AudioClock, AudioMixer, AudioSourceCache, AudioSyncController, AudioTrackConfig,
    AudioTrackData, ClockRole, RealtimeAudioOutput,
};
use mondrian_timeline::clip::Clip;
use mondrian_timeline::command::SequenceSnapshotCommand;
use mondrian_timeline::sequence::Sequence;
use rfd::FileDialog;
use serde::{Deserialize, Serialize};

use crate::shortcuts::{ShortcutAction, ShortcutBinding, ShortcutKey, ShortcutPreferences};
use crate::ui::{
    ai_panel::AiPanel,
    export_panel::ExportPanel,
    library_panel::LibraryPanel,
    timeline_panel::TimelinePanel,
    viewer_panel::{MediaCacheCleanupStats, ViewerPanel, ViewerPreferences},
};

const PROJECT_EXTENSION: &str = "mdp";

mod preferences;

// ─────────────────────────────────────────────
//  PlaybackState
// ─────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default)]
pub enum PlaybackState {
    #[default]
    Stopped,
    Playing {
        timecode_frames: i64,
    },
    Paused {
        timecode_frames: i64,
    },
}

#[derive(Debug, Clone)]
pub struct DraggingAsset {
    pub asset_id: AssetId,
    pub name: String,
    pub kind: AssetKind,
    pub duration: Duration,
    pub has_linked_audio: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ClipOverlapMode {
    #[default]
    Overwrite,
    Insert,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectFile {
    pub name: String,
    pub sequence: Sequence,
    pub in_point_frame: Option<i64>,
    pub out_point_frame: Option<i64>,
    pub proxy_mode_assets: Vec<AssetId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct NewProjectDraft {
    name: String,
    width: u32,
    height: u32,
    fps_num: i64,
    fps_den: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PreferencesTab {
    #[default]
    General,
    Media,
    Shortcuts,
    Developer,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct AppPreferences {
    version: u32,
    #[serde(default = "default_app_theme")]
    theme: crate::ui::theme::Theme,
    show_library: bool,
    show_ai: bool,
    auto_proxy_enabled: bool,
    show_dev_metrics: bool,
    av_clock_role: ClockRole,
    new_project_draft: NewProjectDraft,
    #[serde(default)]
    shortcuts: ShortcutPreferences,
    #[serde(default = "default_media_cache_auto_cleanup")]
    media_cache_auto_cleanup: bool,
    #[serde(default = "default_media_cache_max_size_gb")]
    media_cache_max_size_gb: u32,
    #[serde(default = "default_media_cache_max_age_days")]
    media_cache_max_age_days: u32,
    #[serde(default = "default_show_video_metrics")]
    show_video_metrics: bool,
    #[serde(default = "default_show_audio_metrics")]
    show_audio_metrics: bool,
    #[serde(default = "default_show_preview_perf_metrics")]
    show_preview_perf_metrics: bool,
    viewer: ViewerPreferences,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            version: 1,
            theme: default_app_theme(),
            show_library: true,
            show_ai: false,
            auto_proxy_enabled: false,
            show_dev_metrics: false,
            av_clock_role: ClockRole::AudioMaster,
            new_project_draft: NewProjectDraft::default(),
            shortcuts: ShortcutPreferences::default(),
            media_cache_auto_cleanup: default_media_cache_auto_cleanup(),
            media_cache_max_size_gb: default_media_cache_max_size_gb(),
            media_cache_max_age_days: default_media_cache_max_age_days(),
            show_video_metrics: default_show_video_metrics(),
            show_audio_metrics: default_show_audio_metrics(),
            show_preview_perf_metrics: default_show_preview_perf_metrics(),
            viewer: ViewerPreferences::default(),
        }
    }
}

impl Default for NewProjectDraft {
    fn default() -> Self {
        Self {
            name: "未命名项目".to_string(),
            width: 1920,
            height: 1080,
            fps_num: 25,
            fps_den: 1,
        }
    }
}

const fn default_media_cache_auto_cleanup() -> bool {
    false
}

const fn default_app_theme() -> crate::ui::theme::Theme {
    crate::ui::theme::Theme::System
}

const fn default_media_cache_max_size_gb() -> u32 {
    60
}

const fn default_media_cache_max_age_days() -> u32 {
    30
}

const fn default_show_video_metrics() -> bool {
    true
}

const fn default_show_audio_metrics() -> bool {
    true
}

const fn default_show_preview_perf_metrics() -> bool {
    true
}

// ─────────────────────────────────────────────
//  AppState — 单向数据流中心
// ─────────────────────────────────────────────

pub struct AppState {
    // 全局事件总线
    pub event_bus: Arc<EventBus>,

    // 当前打开的序列（None = 无项目）
    pub sequence: Option<Sequence>,

    // 当前打开的项目文件
    pub current_project_path: Option<PathBuf>,

    // 当前项目运行时工作目录（用于素材库 SQLite）
    pub project_runtime_dir: Option<PathBuf>,

    // 项目级入/出点（帧）
    pub project_in_point: Option<i64>,
    pub project_out_point: Option<i64>,

    // 撤销/重做历史（封装在 timeline crate 中）
    pub cmd_history: mondrian_timeline::command::CommandHistory,

    // 播放状态
    pub playback: PlaybackState,
    /// 播放器自然播放到终点后暂停的标志（区别于用户手动跳帧）。
    /// 当 advance_playback_clock 到达 playback_end 时置 true，
    /// seek() / stop() 时清除，play() 据此决定是否回到 in_point。
    pub playback_reached_end: bool,
    /// 播放时由 Viewer 面板上报：当前是否处于短暂停留缓冲状态。
    pub playback_buffering: bool,

    // 素材库
    pub asset_library: Option<Arc<AssetLibrary>>,

    // 正在拖拽的素材（从素材库拖向时间线）
    pub dragging_asset: Option<DraggingAsset>,

    // 渲染导出队列
    pub render_queue: Arc<RenderQueue>,

    // 底部状态栏提示（message, is_error）
    pub status_hint: Option<(String, bool)>,

    // 代理策略
    pub auto_proxy_enabled: bool,
    pub proxy_mode_assets: HashSet<AssetId>,

    // 音频时钟与 A/V 同步
    pub audio_sample_rate: u32,
    pub audio_clock: AudioClock,
    pub audio_sync: AudioSyncController,
    pub av_drift_ms: f64,
    pub audio_output: Option<RealtimeAudioOutput>,
    pub audio_mixer: AudioMixer,
    pub audio_source_cache: Arc<AudioSourceCache>,
    pub audio_chunk_secs: f64,
    audio_idle_warmup_last: Option<std::time::Instant>,
    audio_render_tx: mpsc::Sender<AudioRenderRequest>,
    audio_render_rx: mpsc::Receiver<AudioRenderResponse>,
    audio_render_generation: u64,
    audio_render_in_flight: usize,
    audio_render_next_start_secs: f64,
}

struct AudioRenderRequest {
    generation: u64,
    window_start_secs: f64,
    duration_secs: f64,
    sequence: Sequence,
    library: Arc<AssetLibrary>,
}

struct AudioRenderResponse {
    generation: u64,
    chunk: mondrian_core::Result<AudioBuffer>,
}

impl AppState {
    pub fn new() -> Self {
        let audio_sample_rate = 48_000;
        let audio_channels = 2;
        let audio_source_cache = Arc::new(AudioSourceCache::new(audio_sample_rate, audio_channels));
        let (audio_render_tx, audio_render_rx) = mpsc::channel::<AudioRenderRequest>();
        let (audio_done_tx, audio_done_rx) = mpsc::channel::<AudioRenderResponse>();

        let worker_cache = Arc::clone(&audio_source_cache);
        thread::spawn(move || {
            while let Ok(req) = audio_render_rx.recv() {
                let chunk = render_audio_chunk_with_cache(
                    &req.sequence,
                    req.library.as_ref(),
                    worker_cache.as_ref(),
                    audio_sample_rate,
                    audio_channels,
                    req.window_start_secs,
                    req.duration_secs,
                );

                let _ =
                    audio_done_tx.send(AudioRenderResponse { generation: req.generation, chunk });
            }
        });

        Self {
            event_bus: EventBus::new(),
            sequence: None,
            current_project_path: None,
            project_runtime_dir: None,
            project_in_point: None,
            project_out_point: None,
            cmd_history: mondrian_timeline::command::CommandHistory::new(200),
            playback: PlaybackState::default(),
            playback_reached_end: false,
            playback_buffering: false,
            asset_library: None,
            dragging_asset: None,
            render_queue: RenderQueue::new(),
            status_hint: None,
            auto_proxy_enabled: false,
            proxy_mode_assets: HashSet::new(),
            audio_sample_rate,
            audio_clock: AudioClock::new(audio_sample_rate),
            audio_sync: AudioSyncController { role: ClockRole::AudioMaster, ..Default::default() },
            av_drift_ms: 0.0,
            audio_output: RealtimeAudioOutput::try_new(audio_sample_rate, audio_channels).ok(),
            audio_mixer: AudioMixer::new(audio_sample_rate, audio_channels),
            audio_source_cache,
            audio_chunk_secs: 0.08,
            audio_idle_warmup_last: None,
            audio_render_tx,
            audio_render_rx: audio_done_rx,
            audio_render_generation: 1,
            audio_render_in_flight: 0,
            audio_render_next_start_secs: 0.0,
        }
    }

    fn fps(&self) -> f64 {
        self.sequence
            .as_ref()
            .map(|seq| seq.settings.frame_rate.to_f64())
            .unwrap_or(25.0)
            .max(1.0)
    }

    fn sync_audio_clock_to_frame(&mut self, frame: i64) {
        let video_secs = frame.max(0) as f64 / self.fps();
        let sample_pos = (video_secs * self.audio_sample_rate as f64).round() as i64;
        self.audio_clock.seek_to_samples(sample_pos.max(0));
    }

    pub fn set_status_hint(&mut self, message: impl Into<String>, is_error: bool) {
        self.status_hint = Some((message.into(), is_error));
    }

    pub fn clear_status_hint(&mut self) {
        self.status_hint = None;
    }

    pub fn set_auto_proxy_enabled(&mut self, enabled: bool) {
        self.auto_proxy_enabled = enabled;
    }

    pub fn is_asset_proxy_mode(&self, asset_id: AssetId) -> bool {
        self.proxy_mode_assets.contains(&asset_id)
    }

    pub fn set_asset_proxy_mode(&mut self, asset_id: AssetId, enabled: bool) {
        if enabled {
            self.proxy_mode_assets.insert(asset_id);
        } else {
            self.proxy_mode_assets.remove(&asset_id);
        }
    }

    fn project_file_path(&self) -> anyhow::Result<&Path> {
        self.current_project_path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("未打开项目文件"))
    }

    fn project_runtime_root(project_file: &Path) -> PathBuf {
        let stem = project_file.file_stem().and_then(|s| s.to_str()).unwrap_or("project");
        let mut hasher = DefaultHasher::new();
        project_file.to_string_lossy().hash(&mut hasher);
        let hash = hasher.finish();
        let dir_name = format!("mondrian_{stem}_{hash:x}");
        std::env::temp_dir().join("mondrian-runtime").join(dir_name)
    }

    fn runtime_library_root(&self) -> anyhow::Result<PathBuf> {
        let root = self
            .project_runtime_dir
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("项目运行目录未初始化"))?;
        Ok(root.join("library"))
    }

    fn load_project_container(
        project_file: &Path,
        runtime_root: &Path,
    ) -> anyhow::Result<ProjectFile> {
        fs::create_dir_all(runtime_root.join("library"))?;

        let file = fs::File::open(project_file)?;
        let mut archive = zip::ZipArchive::new(file)?;

        let mut project_json = String::new();
        archive.by_name("project.json")?.read_to_string(&mut project_json)?;

        let saved = serde_json::from_str::<ProjectFile>(&project_json)?;

        let mut db_entry = archive.by_name("library/index.db")?;
        let mut db_file = fs::File::create(runtime_root.join("library").join("index.db"))?;
        std::io::copy(&mut db_entry, &mut db_file)?;
        db_file.flush()?;

        Ok(saved)
    }

    fn save_project_container(&self, project_data: &ProjectFile) -> anyhow::Result<()> {
        let started_at = std::time::Instant::now();
        let project_file = self.project_file_path()?.to_path_buf();
        let runtime_library_root = self.runtime_library_root()?;
        let db_path = runtime_library_root.join("index.db");

        if !db_path.exists() {
            anyhow::bail!("素材库数据库不存在：{}", db_path.display());
        }

        if let Some(parent) = project_file.parent() {
            fs::create_dir_all(parent)?;
        }

        let tmp_path = project_file.with_extension(format!("{}.tmp", PROJECT_EXTENSION));
        let tmp_file = fs::File::create(&tmp_path)?;
        let mut writer = zip::ZipWriter::new(tmp_file);
        let options = zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);

        writer.start_file("project.json", options)?;
        let json = serde_json::to_vec_pretty(project_data)?;
        writer.write_all(&json)?;

        writer.start_file("library/index.db", options)?;
        let mut db_file = fs::File::open(db_path)?;
        std::io::copy(&mut db_file, &mut writer)?;

        writer.finish()?;

        if project_file.exists() {
            fs::remove_file(&project_file)?;
        }
        fs::rename(tmp_path, project_file)?;

        if ui_diag_enabled() {
            let elapsed_ms = started_at.elapsed().as_millis() as u64;
            if elapsed_ms >= ui_diag_slow_threshold_ms() {
                tracing::warn!("[ui-diag] save_project_container slow: {}ms", elapsed_ms);
            }
        }
        Ok(())
    }

    pub fn has_open_project(&self) -> bool {
        self.sequence.is_some() && self.current_project_path.is_some()
    }

    pub fn open_project_file(&mut self, project_file: PathBuf) -> anyhow::Result<()> {
        if let Some(prev_runtime) = self.project_runtime_dir.as_ref() {
            let _ = fs::remove_dir_all(prev_runtime);
        }

        let runtime_root = Self::project_runtime_root(&project_file);
        if runtime_root.exists() {
            let _ = fs::remove_dir_all(&runtime_root);
        }

        let saved = Self::load_project_container(&project_file, &runtime_root)?;

        self.sequence = Some(saved.sequence);
        self.current_project_path = Some(project_file.clone());
        self.project_runtime_dir = Some(runtime_root.clone());
        self.project_in_point = saved.in_point_frame.map(|f| f.max(0));
        self.project_out_point = saved
            .out_point_frame
            .map(|f| f.max(0))
            .filter(|&f| f >= self.project_in_point.unwrap_or(0));
        self.proxy_mode_assets = saved.proxy_mode_assets.into_iter().collect();
        self.playback = PlaybackState::Stopped;
        self.dragging_asset = None;
        self.cmd_history = mondrian_timeline::command::CommandHistory::new(200);
        self.ensure_minimum_tracks();

        let library_root = runtime_root.join("library");
        self.asset_library = Some(AssetLibrary::open(library_root)?);
        Ok(())
    }

    pub fn save_project_file(&self) -> anyhow::Result<()> {
        let Some(sequence) = self.sequence.as_ref() else {
            return Ok(());
        };

        let mut proxy_mode_assets: Vec<AssetId> = self.proxy_mode_assets.iter().copied().collect();
        proxy_mode_assets.sort_by_key(|id| id.to_string());

        let data = ProjectFile {
            name: sequence.name.clone(),
            sequence: sequence.clone(),
            in_point_frame: self.project_in_point,
            out_point_frame: self.project_out_point,
            proxy_mode_assets,
        };

        self.save_project_container(&data)?;
        Ok(())
    }

    pub fn ensure_minimum_tracks(&mut self) {
        if let Some(seq) = self.sequence.as_mut() {
            if seq.video_tracks.is_empty() {
                seq.video_tracks.push(mondrian_timeline::track::Track::new_video("V1"));
            }
            if seq.audio_tracks.is_empty() {
                seq.audio_tracks.push(mondrian_timeline::track::Track::new_audio("A1"));
            }
        }
    }

    pub fn create_new_project_at(
        &mut self,
        project_file: PathBuf,
        name: &str,
        width: u32,
        height: u32,
        frame_rate: Rational,
    ) -> anyhow::Result<()> {
        if project_file.exists() {
            anyhow::bail!("项目文件已存在：{}", project_file.display());
        }

        if let Some(parent) = project_file.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut sequence = Sequence::new(name);
        sequence.settings.resolution = Resolution { width, height };
        sequence.settings.frame_rate = frame_rate;
        sequence.playhead = TimeCode::new(0, sequence.time_base());

        let runtime_root = Self::project_runtime_root(&project_file);
        if runtime_root.exists() {
            let _ = fs::remove_dir_all(&runtime_root);
        }
        fs::create_dir_all(runtime_root.join("library"))?;

        self.sequence = Some(sequence);
        self.current_project_path = Some(project_file.clone());
        self.project_runtime_dir = Some(runtime_root.clone());
        self.project_in_point = None;
        self.project_out_point = None;
        self.playback = PlaybackState::Stopped;
        self.cmd_history = mondrian_timeline::command::CommandHistory::new(200);
        self.proxy_mode_assets.clear();

        let library_root = runtime_root.join("library");
        let library = AssetLibrary::open(library_root)?;
        library.clear_assets()?;
        self.asset_library = Some(library);

        self.save_project_file()?;
        Ok(())
    }

    pub fn save_project(&self) -> anyhow::Result<()> {
        self.save_project_file()
    }

    fn record_sequence_snapshot_command(
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
            before
        };

        self.record_timeline_edit_snapshot("删除轨道", before);
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

    pub fn play(&mut self) {
        let end_frame = self.last_content_frame();
        let mut frames = self.current_frame();

        if matches!(self.playback, PlaybackState::Stopped) {
            frames = 0;
        }

        // 回到起点的条件：
        // 1. 播放自然到达终点后再次按 Play（playback_reached_end 标志）
        // 2. 当前帧严格超过有效内容结束帧（用户 seek 到内容之外）
        // 注意：frames == end_frame 时不再自动跳回开头——允许从最后一帧开始播放，
        // advance_playback_clock 会在推进后正常停在该帧。
        // 入/出点不影响播放逻辑，仅影响导出。
        if self.playback_reached_end || (end_frame >= 0 && frames > end_frame) {
            frames = 0;
        }
        self.playback_reached_end = false;
        self.playback_buffering = false;

        self.playback = PlaybackState::Playing { timecode_frames: frames };
        self.sync_audio_clock_to_frame(frames);
        self.reset_audio_render_pipeline(self.audio_clock.now_seconds().max(0.0));
    }

    pub fn pause(&mut self) {
        let frames = self.current_frame();
        self.playback_buffering = false;
        self.playback = PlaybackState::Paused { timecode_frames: frames };
        self.sync_audio_clock_to_frame(frames);
        self.reset_audio_render_pipeline(self.audio_clock.now_seconds().max(0.0));
        if let Some(output) = &self.audio_output {
            output.clear();
        }
    }

    pub fn stop(&mut self) {
        self.playback = PlaybackState::Stopped;
        self.playback_reached_end = false;
        self.playback_buffering = false;
        self.av_drift_ms = 0.0;
        self.reset_audio_render_pipeline(0.0);
        if let Some(output) = &self.audio_output {
            output.clear();
        }
    }

    pub fn seek(&mut self, frame: i64) {
        // 任何手动跳帧操作都清除「自然到达终点」标志，
        // 这样下一次 play() 不会误跳回 in_point。
        self.playback_reached_end = false;
        self.playback_buffering = false;
        self.playback = match &self.playback {
            PlaybackState::Playing { .. } => PlaybackState::Playing { timecode_frames: frame },
            _ => PlaybackState::Paused { timecode_frames: frame },
        };
        self.sync_audio_clock_to_frame(frame);
        self.reset_audio_render_pipeline(self.audio_clock.now_seconds().max(0.0));
        if let Some(output) = &self.audio_output {
            output.clear();
        }
    }

    pub fn set_playback_frame_running(&mut self, frame: i64) {
        self.playback = PlaybackState::Playing { timecode_frames: frame.max(0) };
    }

    pub fn set_playback_buffering(&mut self, buffering: bool) {
        self.playback_buffering = buffering;
    }

    pub fn is_playback_buffering(&self) -> bool {
        self.playback_buffering
    }

    pub fn pump_audio_output(&mut self) {
        let Some(output) = self.audio_output.as_ref() else {
            return;
        };

        while let Ok(done) = self.audio_render_rx.try_recv() {
            self.audio_render_in_flight = self.audio_render_in_flight.saturating_sub(1);
            if done.generation != self.audio_render_generation {
                continue;
            }
            match done.chunk {
                Ok(chunk) => output.enqueue(&chunk),
                Err(err) => tracing::debug!("音频后台渲染块失败: {}", err),
            }
        }

        if !self.is_playing() {
            output.clear();
            if audio_idle_warmup_enabled() {
                self.warm_audio_cache_when_idle();
            }
            return;
        }

        self.audio_idle_warmup_last = None;

        let Some(seq) = self.sequence.as_ref() else {
            return;
        };
        let Some(library) = self.asset_library.as_ref() else {
            return;
        };

        let sample_rate_f64 = self.audio_sample_rate as f64;
        let chunk_frames = ((self.audio_chunk_secs * sample_rate_f64).round() as usize).max(1);
        let target_high_frames = (sample_rate_f64 * 0.46) as usize;
        let max_in_flight = 8usize;

        while output.buffered_frames() + self.audio_render_in_flight * chunk_frames
            < target_high_frames
        {
            if self.audio_render_in_flight >= max_in_flight {
                break;
            }

            let request = AudioRenderRequest {
                generation: self.audio_render_generation,
                window_start_secs: self.audio_render_next_start_secs.max(0.0),
                duration_secs: self.audio_chunk_secs,
                sequence: seq.clone(),
                library: Arc::clone(library),
            };

            if self.audio_render_tx.send(request).is_err() {
                break;
            }

            self.audio_render_in_flight += 1;
            self.audio_render_next_start_secs += self.audio_chunk_secs;
        }
    }

    fn reset_audio_render_pipeline(&mut self, anchor_secs: f64) {
        self.audio_render_generation = self.audio_render_generation.saturating_add(1);
        self.audio_render_in_flight = 0;
        self.audio_render_next_start_secs = anchor_secs.max(0.0);
        while self.audio_render_rx.try_recv().is_ok() {}
    }

    fn warm_audio_cache_when_idle(&mut self) {
        let now = std::time::Instant::now();
        if let Some(last) = self.audio_idle_warmup_last {
            if now.saturating_duration_since(last) < Duration::from_millis(900) {
                return;
            }
        }

        let Some(seq) = self.sequence.as_ref() else {
            return;
        };
        let Some(library) = self.asset_library.as_ref() else {
            return;
        };

        let center_secs = self.current_frame().max(0) as f64 / self.fps();
        let chunk = self.audio_chunk_secs.max(0.08);

        let _ = self.render_audio_chunk(seq, library.as_ref(), center_secs, chunk);
        let before = (center_secs - chunk).max(0.0);
        let _ = self.render_audio_chunk(seq, library.as_ref(), before, chunk);
        let _ = self.render_audio_chunk(seq, library.as_ref(), center_secs + chunk, chunk);

        self.audio_idle_warmup_last = Some(now);
    }

    fn render_audio_chunk(
        &self,
        seq: &Sequence,
        library: &AssetLibrary,
        window_start_secs: f64,
        duration_secs: f64,
    ) -> mondrian_core::Result<AudioBuffer> {
        render_audio_chunk_with_cache(
            seq,
            library,
            self.audio_source_cache.as_ref(),
            self.audio_sample_rate,
            self.audio_mixer.output_channels,
            window_start_secs,
            duration_secs,
        )
    }

    pub fn audio_developer_metrics_summary(&self) -> String {
        let buffered_frames = self.audio_output.as_ref().map(|o| o.buffered_frames()).unwrap_or(0);
        let buffered_ms = buffered_frames as f64 / self.audio_sample_rate as f64 * 1000.0;
        let source_cache_entries = self.audio_source_cache.cache_entry_count();
        format!(
            "Aud out:{:.0}ms inflight:{} srcCache:{}",
            buffered_ms, self.audio_render_in_flight, source_cache_entries
        )
    }

    pub fn update_av_sync(&mut self) -> f64 {
        if !self.is_playing() {
            self.av_drift_ms = 0.0;
            return 1.0;
        }

        let video_secs = self.current_frame().max(0) as f64 / self.fps();
        let audio_secs = self.audio_clock.now_seconds();
        let correction =
            self.audio_sync
                .compute_correction(video_secs, audio_secs, self.audio_sample_rate);
        self.av_drift_ms = correction.drift_seconds * 1000.0;

        correction.playback_rate
    }

    pub fn current_frame(&self) -> i64 {
        match &self.playback {
            PlaybackState::Stopped => 0,
            PlaybackState::Playing { timecode_frames } => *timecode_frames,
            PlaybackState::Paused { timecode_frames } => *timecode_frames,
        }
    }

    pub fn is_playing(&self) -> bool {
        matches!(self.playback, PlaybackState::Playing { .. })
    }

    pub fn in_point_frame(&self) -> i64 {
        self.project_in_point.unwrap_or(0).max(0)
    }

    pub fn out_point_frame(&self) -> Option<i64> {
        self.project_out_point.map(|f| f.max(0)).filter(|&f| f >= self.in_point_frame())
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
        let dragging =
            self.dragging_asset.clone().ok_or(mondrian_core::MondrianError::Cancelled)?;

        if dragging.kind != AssetKind::Video {
            return Err(mondrian_core::MondrianError::UnsupportedFormat {
                format: "仅支持将视频素材拖到视频轨".to_string(),
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

            let mut clip = Clip::new(
                dragging.asset_id,
                TimeCode::new(start_frame, time_base),
                TimeCode::new(duration_frames, time_base),
            );
            clip.label = Some(dragging.name.clone());
            let clip_id = clip.id;

            let should_create_linked_audio = dragging.has_linked_audio;

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
            resolve_track_conflicts(track, clip_id, ClipOverlapMode::Overwrite);

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
                    resolve_track_conflicts(audio_track, audio_clip_id, ClipOverlapMode::Overwrite);
                }
            }

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
            resolve_track_conflicts(track, clip_id, ClipOverlapMode::Overwrite);

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
        self.move_clip_to_track(track_id, is_video_track, clip_id, timeline_frame)
    }

    pub fn move_clip_to_track(
        &mut self,
        target_track_id: TrackId,
        is_video_track: bool,
        clip_id: ClipId,
        timeline_frame: i64,
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
                resolve_track_conflicts(track, clip_id, ClipOverlapMode::Overwrite);
            }
        } else if let Some(track) = seq.audio_track_mut(target_track_id) {
            resolve_track_conflicts(track, clip_id, ClipOverlapMode::Overwrite);
        }

        if let Some(linked_id) = linked_clip_id {
            apply_conflict_policy_for_existing_clip(seq, linked_id, ClipOverlapMode::Overwrite);
        }

        for track in &mut seq.video_tracks {
            track.clips.sort_by_key(|c| c.position.frame);
        }
        for track in &mut seq.audio_tracks {
            track.clips.sort_by_key(|c| c.position.frame);
        }

        Ok(())
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

fn find_clip_track_index(seq: &Sequence, is_video_track: bool, clip_id: ClipId) -> Option<usize> {
    if is_video_track {
        seq.video_tracks.iter().position(|t| t.clips.iter().any(|c| c.id == clip_id))
    } else {
        seq.audio_tracks.iter().position(|t| t.clips.iter().any(|c| c.id == clip_id))
    }
}

fn render_audio_chunk_with_cache(
    seq: &Sequence,
    library: &AssetLibrary,
    audio_source_cache: &AudioSourceCache,
    sample_rate: u32,
    channels: u8,
    window_start_secs: f64,
    duration_secs: f64,
) -> mondrian_core::Result<AudioBuffer> {
    let chunk_frames = ((duration_secs * sample_rate as f64).round() as usize).max(1);
    let window_end_secs = window_start_secs + duration_secs;

    let has_solo = seq.audio_tracks.iter().any(|t| t.is_solo && !t.is_muted);
    let mut tracks = Vec::new();

    for track in &seq.audio_tracks {
        if track.is_muted || (has_solo && !track.is_solo) {
            continue;
        }

        for clip in &track.clips {
            if clip.is_disabled {
                continue;
            }

            let clip_start_secs = clip.position.to_secs();
            let clip_end_secs = clip.end_position().to_secs();
            let overlap_start = window_start_secs.max(clip_start_secs);
            let overlap_end = window_end_secs.min(clip_end_secs);
            if overlap_end <= overlap_start {
                continue;
            }

            let Some(asset) = library.get_asset(clip.asset_id)? else {
                continue;
            };

            let source = match audio_source_cache.get_or_decode(asset.path.as_path()) {
                Ok(decoded) => decoded,
                Err(err) => {
                    tracing::debug!("音频解码失败，已跳过素材 {}: {}", asset.id, err);
                    continue;
                }
            };

            let overlap_tc = TimeCode::from_secs(overlap_start, seq.settings.frame_rate);
            let source_start_secs = clip.timeline_to_source_time(overlap_tc).to_secs().max(0.0);
            let source_start_frame = (source_start_secs * sample_rate as f64).floor() as usize;
            let segment_frames =
                ((overlap_end - overlap_start) * sample_rate as f64).ceil() as usize;
            let segment = source.slice_frames(source_start_frame, segment_frames.max(1));
            if segment.samples.is_empty() {
                continue;
            }

            let place_offset = ((overlap_start - window_start_secs) * sample_rate as f64)
                .round()
                .max(0.0) as usize;
            let mut placed = AudioBuffer::silent(sample_rate, channels, chunk_frames);
            let max_place_frames = chunk_frames.saturating_sub(place_offset);
            let copy_frames = segment.frame_count().min(max_place_frames);

            let dst_channels = channels as usize;
            let src_channels = segment.channels as usize;
            for frame in 0..copy_frames {
                let dst_base = (place_offset + frame) * dst_channels;
                let src_base = frame * src_channels;
                for ch in 0..dst_channels {
                    let v = segment
                        .samples
                        .get(src_base + ch.min(src_channels.saturating_sub(1)))
                        .copied()
                        .unwrap_or(0.0);
                    placed.samples[dst_base + ch] = v;
                }
            }

            tracks.push(AudioTrackData {
                buffer: placed,
                config: AudioTrackConfig {
                    volume: 1.0,
                    pan: 0.0,
                    is_muted: false,
                    is_solo: false,
                },
            });
        }
    }

    let mixer = AudioMixer::new(sample_rate, channels);
    Ok(mixer.mix(&tracks))
}

#[derive(Clone, Copy)]
struct ClipSplitResult {
    right_clip_id: ClipId,
    original_linked: Option<ClipId>,
}

fn split_clip_anywhere(
    seq: &mut Sequence,
    clip_id: ClipId,
    split_frame: i64,
    time_base: Rational,
) -> Option<ClipSplitResult> {
    for track in &mut seq.video_tracks {
        if let Some(result) = split_clip_in_track(track, clip_id, split_frame, time_base) {
            return Some(result);
        }
    }
    for track in &mut seq.audio_tracks {
        if let Some(result) = split_clip_in_track(track, clip_id, split_frame, time_base) {
            return Some(result);
        }
    }
    None
}

fn split_clip_in_track(
    track: &mut mondrian_timeline::track::Track,
    clip_id: ClipId,
    split_frame: i64,
    time_base: Rational,
) -> Option<ClipSplitResult> {
    let index = track.clips.iter().position(|c| c.id == clip_id)?;
    let clip = track.clips.get(index)?.clone();
    let start = clip.position.frame;
    let end = clip.end_position().frame;
    if split_frame <= start || split_frame >= end {
        return None;
    }

    let left_duration = split_frame - start;
    let right_duration = end - split_frame;
    if left_duration <= 0 || right_duration <= 0 {
        return None;
    }

    let split_tc = TimeCode::new(split_frame, time_base);
    let new_source_in = clip.timeline_to_source_time(split_tc);

    let mut left = clip.clone();
    left.duration = TimeCode::new(left_duration, left.duration.time_base);
    left.source_out = new_source_in;

    let mut right = clip;
    right.id = ClipId::new();
    right.position = TimeCode::new(split_frame, right.position.time_base);
    right.duration = TimeCode::new(right_duration, right.duration.time_base);
    right.source_in = new_source_in;
    right.linked_clip = None;

    track.clips[index] = left;
    let right_id = right.id;
    track.clips.insert(index + 1, right);

    Some(ClipSplitResult {
        right_clip_id: right_id,
        original_linked: track.clips[index].linked_clip,
    })
}

fn ensure_audio_track_index(seq: &mut Sequence, index: usize) {
    while seq.audio_tracks.len() <= index {
        seq.add_audio_track();
    }
}

fn move_existing_clip_to_track_index(
    seq: &mut Sequence,
    is_video_track: bool,
    clip_id: ClipId,
    target_track_index: usize,
    new_start: i64,
    time_base: Rational,
) -> bool {
    if is_video_track {
        if target_track_index >= seq.video_tracks.len() {
            return false;
        }

        let Some(source_track_index) = find_clip_track_index(seq, true, clip_id) else {
            return false;
        };

        if source_track_index == target_track_index {
            if let Some(clip) =
                seq.video_tracks[source_track_index].clips.iter_mut().find(|c| c.id == clip_id)
            {
                clip.position = TimeCode::new(new_start, time_base);
                return true;
            }
            return false;
        }

        let Some(clip_index) =
            seq.video_tracks[source_track_index].clips.iter().position(|c| c.id == clip_id)
        else {
            return false;
        };

        let mut clip = seq.video_tracks[source_track_index].clips.remove(clip_index);
        clip.position = TimeCode::new(new_start, time_base);
        seq.video_tracks[target_track_index].clips.push(clip);
        true
    } else {
        if target_track_index >= seq.audio_tracks.len() {
            return false;
        }

        let Some(source_track_index) = find_clip_track_index(seq, false, clip_id) else {
            return false;
        };

        if source_track_index == target_track_index {
            if let Some(clip) =
                seq.audio_tracks[source_track_index].clips.iter_mut().find(|c| c.id == clip_id)
            {
                clip.position = TimeCode::new(new_start, time_base);
                return true;
            }
            return false;
        }

        let Some(clip_index) =
            seq.audio_tracks[source_track_index].clips.iter().position(|c| c.id == clip_id)
        else {
            return false;
        };

        let mut clip = seq.audio_tracks[source_track_index].clips.remove(clip_index);
        clip.position = TimeCode::new(new_start, time_base);
        seq.audio_tracks[target_track_index].clips.push(clip);
        true
    }
}

fn remove_clip_from_sequence(seq: &mut Sequence, clip_id: ClipId) -> bool {
    for track in &mut seq.video_tracks {
        if track.remove_clip(clip_id).is_some() {
            return true;
        }
    }
    for track in &mut seq.audio_tracks {
        if track.remove_clip(clip_id).is_some() {
            return true;
        }
    }
    false
}

fn remove_clip_from_sequence_with_ripple(
    seq: &mut Sequence,
    clip_id: ClipId,
    ripple: bool,
) -> bool {
    for track in &mut seq.video_tracks {
        if let Some(index) = track.clips.iter().position(|c| c.id == clip_id) {
            let removed = track.clips.remove(index);
            if ripple {
                let start = removed.position.frame;
                let end = start + removed.duration.frame.max(0);
                for clip in &mut track.clips {
                    if clip.position.frame >= end {
                        clip.position = TimeCode::new(
                            (clip.position.frame - removed.duration.frame.max(0)).max(0),
                            clip.position.time_base,
                        );
                    }
                }
            }
            resolve_track_overlaps(track);
            return true;
        }
    }
    for track in &mut seq.audio_tracks {
        if let Some(index) = track.clips.iter().position(|c| c.id == clip_id) {
            let removed = track.clips.remove(index);
            if ripple {
                let start = removed.position.frame;
                let end = start + removed.duration.frame.max(0);
                for clip in &mut track.clips {
                    if clip.position.frame >= end {
                        clip.position = TimeCode::new(
                            (clip.position.frame - removed.duration.frame.max(0)).max(0),
                            clip.position.time_base,
                        );
                    }
                }
            }
            resolve_track_overlaps(track);
            return true;
        }
    }
    false
}

fn clear_broken_links(seq: &mut Sequence) {
    let existing: HashSet<ClipId> = seq
        .video_tracks
        .iter()
        .flat_map(|t| t.clips.iter().map(|c| c.id))
        .chain(seq.audio_tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.id)))
        .collect();

    for track in &mut seq.video_tracks {
        for clip in &mut track.clips {
            if let Some(linked) = clip.linked_clip {
                if !existing.contains(&linked) {
                    clip.linked_clip = None;
                }
            }
        }
    }
    for track in &mut seq.audio_tracks {
        for clip in &mut track.clips {
            if let Some(linked) = clip.linked_clip {
                if !existing.contains(&linked) {
                    clip.linked_clip = None;
                }
            }
        }
    }
}

fn remove_asset_clips_from_tracks(
    tracks: &mut [mondrian_timeline::track::Track],
    asset_id: AssetId,
) -> usize {
    let mut removed = 0usize;
    for track in tracks {
        let before = track.clips.len();
        track.clips.retain(|clip| clip.asset_id != asset_id);
        removed += before.saturating_sub(track.clips.len());
        resolve_track_overlaps(track);
    }
    removed
}

fn resolve_track_overlaps(track: &mut mondrian_timeline::track::Track) {
    track.clips.sort_by_key(|c| c.position.frame);
    let mut cursor = 0i64;
    for clip in &mut track.clips {
        if clip.position.frame < cursor {
            clip.position = TimeCode::new(cursor, clip.position.time_base);
        }
        cursor = clip.end_position().frame;
    }
}

fn resolve_track_conflicts(
    track: &mut mondrian_timeline::track::Track,
    focus_clip_id: ClipId,
    mode: ClipOverlapMode,
) {
    match mode {
        ClipOverlapMode::Insert => resolve_track_overlaps(track),
        ClipOverlapMode::Overwrite => {
            let Some(focus) = track.clips.iter().find(|c| c.id == focus_clip_id).cloned() else {
                track.clips.sort_by_key(|c| c.position.frame);
                return;
            };

            let focus_start = focus.position.frame;
            let focus_end = focus.end_position().frame;
            track.clips.retain(|clip| {
                if clip.id == focus_clip_id {
                    true
                } else {
                    let start = clip.position.frame;
                    let end = clip.end_position().frame;
                    end <= focus_start || start >= focus_end
                }
            });
            track.clips.sort_by_key(|c| c.position.frame);
        }
    }
}

fn apply_conflict_policy_for_existing_clip(
    seq: &mut Sequence,
    clip_id: ClipId,
    mode: ClipOverlapMode,
) {
    for track in &mut seq.video_tracks {
        if track.clips.iter().any(|c| c.id == clip_id) {
            resolve_track_conflicts(track, clip_id, mode);
            return;
        }
    }
    for track in &mut seq.audio_tracks {
        if track.clips.iter().any(|c| c.id == clip_id) {
            resolve_track_conflicts(track, clip_id, mode);
            return;
        }
    }
}

fn find_clip_mut(seq: &mut Sequence, clip_id: ClipId) -> Option<&mut Clip> {
    for track in &mut seq.video_tracks {
        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
            return Some(clip);
        }
    }
    for track in &mut seq.audio_tracks {
        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
            return Some(clip);
        }
    }
    None
}

// ─────────────────────────────────────────────
//  MondrianApp — eframe::App 实现
// ─────────────────────────────────────────────

pub struct MondrianApp {
    state: AppState,

    // UI 面板
    timeline_panel: TimelinePanel,
    viewer_panel: ViewerPanel,
    library_panel: LibraryPanel,
    ai_panel: AiPanel,
    export_panel: ExportPanel,

    // 面板可见性
    show_library: bool,
    show_ai: bool,
    show_export: bool,
    show_dev_metrics: bool,
    show_preferences_dialog: bool,
    theme: crate::ui::theme::Theme,
    preferences_tab: PreferencesTab,
    capturing_shortcut: Option<ShortcutAction>,
    show_new_project_dialog: bool,
    show_project_bootstrap_dialog: bool,
    new_project_draft: NewProjectDraft,
    playback_last_tick: Option<std::time::Instant>,
    playback_subframe_accum: f64,
    playback_buffering_last_frame: bool,
    app_config_path: PathBuf,
    shortcuts: ShortcutPreferences,
    media_cache_auto_cleanup: bool,
    media_cache_max_size_gb: u32,
    media_cache_max_age_days: u32,
    show_video_metrics: bool,
    show_audio_metrics: bool,
    show_preview_perf_metrics: bool,
    last_saved_preferences: Option<AppPreferences>,
    persist_error_reported: bool,
    last_cache_maintenance_at: Option<std::time::Instant>,
    cache_maintenance_in_flight: bool,
    cache_maintenance_rx: Option<mpsc::Receiver<anyhow::Result<MediaCacheCleanupStats>>>,
}

impl MondrianApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::ui::fonts::configure_fonts(&cc.egui_ctx);

        let state = AppState::new();

        let mut app = Self {
            state,
            timeline_panel: TimelinePanel::default(),
            viewer_panel: ViewerPanel::default(),
            library_panel: LibraryPanel::default(),
            ai_panel: AiPanel::default(),
            export_panel: ExportPanel::default(),
            show_library: true,
            show_ai: false,
            show_export: false,
            show_dev_metrics: false,
            show_preferences_dialog: false,
            theme: default_app_theme(),
            preferences_tab: PreferencesTab::default(),
            capturing_shortcut: None,
            show_new_project_dialog: false,
            show_project_bootstrap_dialog: true,
            new_project_draft: NewProjectDraft::default(),
            playback_last_tick: None,
            playback_subframe_accum: 0.0,
            playback_buffering_last_frame: false,
            app_config_path: app_preferences_path(),
            shortcuts: ShortcutPreferences::default(),
            media_cache_auto_cleanup: default_media_cache_auto_cleanup(),
            media_cache_max_size_gb: default_media_cache_max_size_gb(),
            media_cache_max_age_days: default_media_cache_max_age_days(),
            show_video_metrics: default_show_video_metrics(),
            show_audio_metrics: default_show_audio_metrics(),
            show_preview_perf_metrics: default_show_preview_perf_metrics(),
            last_saved_preferences: None,
            persist_error_reported: false,
            last_cache_maintenance_at: None,
            cache_maintenance_in_flight: false,
            cache_maintenance_rx: None,
        };

        app.load_app_preferences();
        crate::ui::theme::apply_theme(&cc.egui_ctx, app.theme);
        cc.egui_ctx
            .send_viewport_cmd(egui::ViewportCommand::SetTheme(app.theme.to_system_theme()));
        app
    }
}

impl eframe::App for MondrianApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let update_started_at = std::time::Instant::now();

        self.advance_playback_clock();
        if ui_diag_enabled() {
            log_ui_stage_slow("advance_playback_clock", update_started_at.elapsed());
        }

        let is_playing = self.state.is_playing();

        let theme_started_at = std::time::Instant::now();
        crate::ui::theme::apply_theme(ctx, self.theme);
        ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(
            self.theme.to_system_theme(),
        ));
        if ui_diag_enabled() {
            log_ui_stage_slow("apply_theme", theme_started_at.elapsed());
        }

        let shortcuts_started_at = std::time::Instant::now();
        self.process_global_shortcuts(ctx);
        if ui_diag_enabled() {
            log_ui_stage_slow("process_global_shortcuts", shortcuts_started_at.elapsed());
        }

        let cache_maintenance_started_at = std::time::Instant::now();
        self.run_cache_maintenance_if_needed();
        if ui_diag_enabled() {
            log_ui_stage_slow(
                "run_cache_maintenance_if_needed",
                cache_maintenance_started_at.elapsed(),
            );
        }

        if self.state.dragging_asset().is_some() {
            ctx.output_mut(|o| o.cursor_icon = egui::CursorIcon::Default);
        }

        // 播放中持续请求重绘（60 fps 上限）
        if is_playing {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }

        // ── 顶部菜单栏 ──
        let top_menu_started_at = std::time::Instant::now();
        egui::TopBottomPanel::top("top_menu")
            .frame(
                egui::Frame::none()
                    .fill(crate::ui::theme::palette::bg_base())
                    .stroke(egui::Stroke::new(
                        1.0,
                        crate::ui::theme::palette::border_subtle(),
                    ))
                    .inner_margin(egui::Margin::symmetric(8.0, 4.0)),
            )
            .show(ctx, |ui| {
                self.draw_menu_bar(ui);
            });
        if ui_diag_enabled() {
            log_ui_stage_slow("top_menu", top_menu_started_at.elapsed());
        }

        // ── 底部状态栏 ──
        let status_bar_started_at = std::time::Instant::now();
        egui::TopBottomPanel::bottom("status_bar")
            .exact_height(24.0)
            .frame(
                egui::Frame::none()
                    .fill(crate::ui::theme::palette::bg_base())
                    .stroke(egui::Stroke::new(
                        1.0,
                        crate::ui::theme::palette::border_subtle(),
                    ))
                    .inner_margin(egui::Margin::symmetric(8.0, 2.0)),
            )
            .show(ctx, |ui| {
                self.draw_status_bar(ui);
            });
        if ui_diag_enabled() {
            log_ui_stage_slow("status_bar", status_bar_started_at.elapsed());
        }

        // ── 底部：时间线（全宽） ──
        let timeline_started_at = std::time::Instant::now();
        egui::TopBottomPanel::bottom("timeline_panel")
            .default_height(220.0)
            .height_range(120.0..=480.0)
            .frame(
                egui::Frame::none()
                    .fill(crate::ui::theme::palette::bg_base())
                    .stroke(egui::Stroke::new(
                        1.0,
                        crate::ui::theme::palette::border_subtle(),
                    ))
                    .inner_margin(egui::Margin::symmetric(8.0, 6.0)),
            )
            .show(ctx, |ui| {
                self.timeline_panel.show(ui, &mut self.state);
            });
        if ui_diag_enabled() {
            log_ui_stage_slow("timeline_panel", timeline_started_at.elapsed());
        }

        // ── 左侧：素材库 ──
        if self.show_library {
            let library_started_at = std::time::Instant::now();
            egui::SidePanel::left("library_panel")
                .default_width(260.0)
                .width_range(160.0..=420.0)
                .frame(
                    egui::Frame::none()
                        .fill(crate::ui::theme::palette::bg_base())
                        .stroke(egui::Stroke::NONE)
                        .inner_margin(egui::Margin::symmetric(8.0, 6.0)),
                )
                .show(ctx, |ui| {
                    self.library_panel.show(ui, &mut self.state);
                });
            if ui_diag_enabled() {
                log_ui_stage_slow("library_panel", library_started_at.elapsed());
            }
        }

        // ── 右侧：AI 面板 ──
        if self.show_ai {
            let ai_started_at = std::time::Instant::now();
            egui::SidePanel::right("ai_panel")
                .default_width(320.0)
                .width_range(240.0..=520.0)
                .frame(
                    egui::Frame::none()
                        .fill(crate::ui::theme::palette::bg_base())
                        .stroke(egui::Stroke::new(
                            1.0,
                            crate::ui::theme::palette::border_subtle(),
                        ))
                        .inner_margin(egui::Margin::symmetric(8.0, 6.0)),
                )
                .show(ctx, |ui| {
                    self.ai_panel.show(ui, &mut self.state);
                });
            if ui_diag_enabled() {
                log_ui_stage_slow("ai_panel", ai_started_at.elapsed());
            }
        }

        // ── 中央：预览窗口 ──
        let viewer_started_at = std::time::Instant::now();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(crate::ui::theme::palette::bg_base())
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0)),
            )
            .show(ctx, |ui| {
                self.viewer_panel.show(
                    ui,
                    &mut self.state,
                    self.show_dev_metrics,
                    self.show_video_metrics,
                    self.show_audio_metrics,
                    self.show_preview_perf_metrics,
                );
            });
        if ui_diag_enabled() {
            log_ui_stage_slow("viewer_panel", viewer_started_at.elapsed());
        }

        // ── 导出弹窗 ──
        if self.show_export {
            let export_started_at = std::time::Instant::now();
            let mut open = self.show_export;
            egui::Window::new("导出").open(&mut open).default_size([520.0, 640.0]).show(
                ctx,
                |ui| {
                    self.export_panel.show(ui, &mut self.state);
                },
            );
            self.show_export = open;
            if ui_diag_enabled() {
                log_ui_stage_slow("export_panel", export_started_at.elapsed());
            }
        }

        if self.show_new_project_dialog {
            let mut open = self.show_new_project_dialog;
            egui::Window::new("新建项目").open(&mut open).default_size([420.0, 260.0]).show(
                ctx,
                |ui| {
                    ui.label("项目名称");
                    ui.text_edit_singleline(&mut self.new_project_draft.name);
                    ui.separator();

                    ui.horizontal(|ui| {
                        ui.label("宽");
                        ui.add(
                            egui::DragValue::new(&mut self.new_project_draft.width)
                                .range(320..=8192),
                        );
                        ui.label("高");
                        ui.add(
                            egui::DragValue::new(&mut self.new_project_draft.height)
                                .range(240..=4320),
                        );
                    });

                    ui.horizontal(|ui| {
                        ui.label("帧率");
                        ui.add(
                            egui::DragValue::new(&mut self.new_project_draft.fps_num)
                                .range(1..=240),
                        );
                        ui.label("/");
                        ui.add(
                            egui::DragValue::new(&mut self.new_project_draft.fps_den)
                                .range(1..=1001),
                        );
                    });

                    ui.separator();
                    if ui.button("创建项目").clicked() {
                        let fps = Rational::new(
                            self.new_project_draft.fps_num.max(1),
                            self.new_project_draft.fps_den.max(1),
                        );
                        let name = if self.new_project_draft.name.trim().is_empty() {
                            "未命名项目"
                        } else {
                            self.new_project_draft.name.trim()
                        };

                        let default_name =
                            format!("{}.{}", sanitize_filename(name), PROJECT_EXTENSION);
                        let picked = FileDialog::new()
                            .add_filter("Mondrian Project", &[PROJECT_EXTENSION])
                            .set_file_name(&default_name)
                            .save_file();

                        if let Some(path) = picked {
                            let project_path = ensure_project_extension(path);
                            if let Err(err) = self.state.create_new_project_at(
                                project_path,
                                name,
                                self.new_project_draft.width.max(1),
                                self.new_project_draft.height.max(1),
                                fps,
                            ) {
                                self.state.set_status_hint(format!("新建项目失败：{err}"), true);
                                tracing::error!("新建项目失败: {err}");
                            } else {
                                self.show_library = true;
                                self.show_new_project_dialog = false;
                                self.show_project_bootstrap_dialog = false;
                            }
                        }
                    }
                },
            );
            self.show_new_project_dialog = open;
        }

        if !self.state.has_open_project() {
            self.show_project_bootstrap_dialog = true;
        }

        if self.show_project_bootstrap_dialog {
            egui::Window::new("打开或新建项目")
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .collapsible(false)
                .resizable(false)
                .default_size([360.0, 140.0])
                .show(ctx, |ui| {
                    ui.label(format!(
                        "开始前需要先打开一个项目文件，或新建一个项目。\n项目后缀：.{}",
                        PROJECT_EXTENSION
                    ));
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        if ui.button("打开项目...").clicked() {
                            self.open_project_dialog();
                        }
                        if ui.button("新建项目...").clicked() {
                            self.show_new_project_dialog = true;
                        }
                        if ui.button("退出").clicked() {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
        }

        if self.show_preferences_dialog {
            self.capture_shortcut_input(ctx);
            self.draw_preferences_window(ctx);
        }

        let persist_started_at = std::time::Instant::now();
        self.persist_preferences_if_needed();

        if ui_diag_enabled() {
            log_ui_stage_slow(
                "persist_preferences_if_needed",
                persist_started_at.elapsed(),
            );
            log_ui_stage_slow("update_total", update_started_at.elapsed());
        }
    }
}

fn ui_diag_enabled() -> bool {
    static UI_DIAG: OnceLock<bool> = OnceLock::new();
    *UI_DIAG.get_or_init(|| {
        std::env::var("MONDRIAN_UI_DIAG")
            .map(|v| {
                let value = v.trim().to_ascii_lowercase();
                matches!(value.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(false)
    })
}

fn ui_diag_slow_threshold_ms() -> u64 {
    static THRESHOLD: OnceLock<u64> = OnceLock::new();
    *THRESHOLD.get_or_init(|| {
        std::env::var("MONDRIAN_UI_DIAG_SLOW_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(30)
    })
}

fn log_ui_stage_slow(stage: &str, elapsed: std::time::Duration) {
    let elapsed_ms = elapsed.as_millis() as u64;
    if elapsed_ms >= ui_diag_slow_threshold_ms() {
        tracing::warn!("[ui-diag] {} slow: {}ms", stage, elapsed_ms);
    }
}

fn audio_idle_warmup_enabled() -> bool {
    static AUDIO_IDLE_WARMUP: OnceLock<bool> = OnceLock::new();
    *AUDIO_IDLE_WARMUP.get_or_init(|| {
        std::env::var("MONDRIAN_AUDIO_IDLE_WARMUP")
            .map(|v| {
                let value = v.trim().to_ascii_lowercase();
                matches!(value.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(false)
    })
}

// ─────────────────────────────────────────────
//  MondrianApp — 私有 UI helpers
// ─────────────────────────────────────────────

impl MondrianApp {
    fn load_app_preferences(&mut self) {
        preferences::load_app_preferences(self);
    }

    fn process_global_shortcuts(&mut self, ctx: &egui::Context) {
        preferences::process_global_shortcuts(self, ctx);
    }

    fn run_cache_maintenance_if_needed(&mut self) {
        preferences::run_cache_maintenance_if_needed(self);
    }

    fn trigger_import_media(&mut self) {
        preferences::trigger_import_media(self);
    }

    fn save_project_as_dialog(&mut self) {
        preferences::save_project_as_dialog(self);
    }

    fn capture_shortcut_input(&mut self, ctx: &egui::Context) {
        preferences::capture_shortcut_input(self, ctx);
    }

    fn draw_preferences_window(&mut self, ctx: &egui::Context) {
        preferences::draw_preferences_window(self, ctx);
    }

    fn persist_preferences_if_needed(&mut self) {
        preferences::persist_preferences_if_needed(self);
    }

    /// 播放自然到达终点时调用：将播放头停在 `end_frame`，并标记自然到达标志。
    /// 与 `state.seek()` 不同，此方法**不会清除** `playback_reached_end`，
    /// 因此下次 `play()` 将从 in_point 重新开始（经典循环行为）。
    fn end_playback_at(&mut self, end_frame: i64) {
        self.state.playback_reached_end = true;
        self.state.playback_buffering = false;
        self.state.playback = PlaybackState::Paused { timecode_frames: end_frame };
        self.state.sync_audio_clock_to_frame(end_frame);
        self.state
            .reset_audio_render_pipeline(self.state.audio_clock.now_seconds().max(0.0));
        if let Some(output) = &self.state.audio_output {
            output.clear();
        }
        self.playback_last_tick = None;
        self.playback_subframe_accum = 0.0;
        self.playback_buffering_last_frame = false;
    }

    fn advance_playback_clock(&mut self) {
        if !self.state.is_playing() {
            self.playback_last_tick = None;
            self.playback_subframe_accum = 0.0;
            self.playback_buffering_last_frame = false;
            return;
        }

        if self.state.is_playback_buffering() {
            if !self.playback_buffering_last_frame {
                self.state.sync_audio_clock_to_frame(self.state.current_frame());
                self.state
                    .reset_audio_render_pipeline(self.state.audio_clock.now_seconds().max(0.0));
                if let Some(output) = &self.state.audio_output {
                    output.clear();
                }
            }

            self.playback_last_tick = None;
            self.playback_subframe_accum = 0.0;
            self.playback_buffering_last_frame = true;
            return;
        }

        if self.playback_buffering_last_frame {
            self.state.sync_audio_clock_to_frame(self.state.current_frame());
            self.state
                .reset_audio_render_pipeline(self.state.audio_clock.now_seconds().max(0.0));
        }
        self.playback_buffering_last_frame = false;

        self.state.pump_audio_output();

        let now = std::time::Instant::now();
        let previous = self.playback_last_tick.replace(now).unwrap_or(now);
        let elapsed_secs = now.saturating_duration_since(previous).as_secs_f64();
        if elapsed_secs <= 0.0 {
            return;
        }

        let mut fps = self
            .state
            .sequence
            .as_ref()
            .map(|seq| seq.settings.frame_rate.to_f64())
            .unwrap_or(25.0)
            .max(1.0);

        let nominal_fps = fps;
        fps *= self.state.update_av_sync().clamp(0.97, 1.03);

        let frame_budget = elapsed_secs * fps + self.playback_subframe_accum;
        let mut advance_frames = frame_budget.floor() as i64;
        self.playback_subframe_accum = frame_budget - advance_frames as f64;

        let drift_secs = self.state.av_drift_ms / 1000.0;
        let frame_duration_secs = 1.0 / nominal_fps.max(1.0);
        let sync_threshold_secs = (frame_duration_secs * 0.5).clamp(
            self.state.audio_sync.max_soft_drift.as_secs_f64(),
            self.state.audio_sync.max_hard_drift.as_secs_f64(),
        );
        let hard_drift_secs = self.state.audio_sync.max_hard_drift.as_secs_f64();
        let no_sync_threshold_secs = self.state.audio_sync.no_sync_threshold.as_secs_f64();

        if drift_secs.abs() < no_sync_threshold_secs {
            if drift_secs > hard_drift_secs {
                self.playback_subframe_accum = 0.0;
                return;
            }

            if drift_secs < -sync_threshold_secs {
                let late_secs = (-drift_secs - sync_threshold_secs).max(0.0);
                let catch_up_frames = (late_secs * nominal_fps).ceil() as i64;
                let catch_up_frames = catch_up_frames.clamp(1, 8);
                advance_frames = (advance_frames + catch_up_frames).max(1);
            }
        }

        if advance_frames <= 0 {
            return;
        }

        let current = self.state.current_frame();
        // 入/出点不影响播放逻辑，仅影响导出。
        let playback_end = self.state.last_content_frame().max(0);

        if current >= playback_end {
            // 播放头已在/超过终点：标记「自然到达终点」，暂停于终点帧
            self.end_playback_at(playback_end);
            return;
        }

        let next = current + advance_frames;
        if next >= playback_end {
            // 本帧推进后越界：停在终点并设置自然到达标志
            self.end_playback_at(playback_end);
        } else {
            self.state.set_playback_frame_running(next.max(0));
        }
    }

    fn open_project_dialog(&mut self) {
        let picked = FileDialog::new()
            .add_filter("Mondrian Project", &[PROJECT_EXTENSION])
            .pick_file();

        let Some(path) = picked else {
            return;
        };

        match self.state.open_project_file(path) {
            Ok(()) => {
                self.show_project_bootstrap_dialog = false;
                self.show_library = true;
                self.state.set_status_hint("项目已打开", false);
            }
            Err(err) => {
                self.state.set_status_hint(format!("打开项目失败：{err}"), true);
                tracing::error!("打开项目失败: {err}");
            }
        }
    }

    fn draw_menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::menu::bar(ui, |ui| {
            ui.menu_button("文件", |ui| {
                if ui.button("新建项目...").clicked() {
                    self.show_new_project_dialog = true;
                    ui.close_menu();
                }
                let open_label = format!("打开项目...    {}", self.shortcuts.open_project_label());
                if ui.button(open_label).clicked() {
                    self.open_project_dialog();
                    ui.close_menu();
                }
                let save_label = format!("保存    {}", self.shortcuts.save_project_label());
                if ui.button(save_label).clicked() {
                    if let Err(err) = self.state.save_project() {
                        tracing::error!("保存项目失败: {err}");
                        self.state.set_status_hint(format!("保存项目失败：{err}"), true);
                    } else {
                        self.state.set_status_hint("项目已保存", false);
                    }
                    ui.close_menu();
                }
                let save_as_label =
                    format!("另存为...    {}", self.shortcuts.save_project_as_label());
                if ui.button(save_as_label).clicked() {
                    self.save_project_as_dialog();
                    ui.close_menu();
                }
                let close_label = format!("关闭项目    {}", self.shortcuts.close_project_label());
                if ui.button(close_label).clicked() {
                    self.state.close_project();
                    ui.close_menu();
                }
                ui.separator();
                let import_label = format!("导入媒体    {}", self.shortcuts.import_media_label());
                if ui.button(import_label).clicked() {
                    self.trigger_import_media();
                    ui.close_menu();
                }
                ui.separator();
                let quit_label = format!("退出    {}", self.shortcuts.quit_app_label());
                if ui.button(quit_label).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });

            ui.menu_button("编辑", |ui| {
                let can_undo = self.state.cmd_history.can_undo();
                let can_redo = self.state.cmd_history.can_redo();

                if ui.add_enabled(can_undo, egui::Button::new("撤销")).clicked() {
                    if let Err(err) = self.state.undo_timeline() {
                        self.state.set_status_hint(format!("撤销失败：{err}"), true);
                    }
                    ui.close_menu();
                }
                if ui.add_enabled(can_redo, egui::Button::new("重做")).clicked() {
                    if let Err(err) = self.state.redo_timeline() {
                        self.state.set_status_hint(format!("重做失败：{err}"), true);
                    }
                    ui.close_menu();
                }

                ui.separator();
                if ui.button("首选项...").clicked() {
                    self.show_preferences_dialog = true;
                    ui.close_menu();
                }
            });

            ui.menu_button("视图", |ui| {
                let _ = crate::ui::theme::checkmark_toggle(ui, &mut self.show_library, "素材库");
                let _ = crate::ui::theme::checkmark_toggle(ui, &mut self.show_ai, "AI 工作流");
                if cfg!(debug_assertions) {
                    let _ = crate::ui::theme::checkmark_toggle(
                        ui,
                        &mut self.show_dev_metrics,
                        "开发指标",
                    );
                }
            });

            ui.menu_button("导出", |ui| {
                if ui.button("导出视频…").clicked() {
                    self.show_export = true;
                    ui.close_menu();
                }
            });

            ui.menu_button("帮助", |ui| {
                ui.label("关于Mondrian");
            });
        });
    }

    fn draw_status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (status_text, is_error, is_busy) = self.status_bar_text();
            let status_color = if is_error {
                crate::ui::theme::palette::status_error()
            } else if is_busy {
                crate::ui::theme::palette::interaction_highlight()
            } else {
                crate::ui::theme::palette::text_muted()
            };

            let _ = crate::ui::theme::icon(
                ui,
                crate::ui::theme::UiIcon::Info,
                crate::ui::theme::palette::text_muted(),
            );
            ui.add(
                egui::Label::new(egui::RichText::new(status_text).color(status_color)).truncate(),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let project_name = self
                    .state
                    .sequence
                    .as_ref()
                    .map(|seq| seq.name.as_str())
                    .unwrap_or("未命名项目");
                ui.label(
                    egui::RichText::new(project_name)
                        .color(crate::ui::theme::palette::text_muted()),
                );
            });
        });
    }

    fn status_bar_text(&self) -> (String, bool, bool) {
        let jobs = self.state.render_queue.list_jobs();
        let active_jobs: Vec<_> = jobs
            .into_iter()
            .filter(|job| {
                matches!(
                    job.status,
                    JobStatus::Pending | JobStatus::Rendering { .. } | JobStatus::Encoding
                )
            })
            .collect();

        if let Some(job) = active_jobs.first() {
            let label = match &job.status {
                JobStatus::Pending => format!("导出队列处理中（{}）", active_jobs.len()),
                JobStatus::Rendering { frame, total_frames } => {
                    format!(
                        "正在导出帧 {}/{}（队列 {}）",
                        frame,
                        total_frames,
                        active_jobs.len()
                    )
                }
                JobStatus::Encoding => format!("正在编码（队列 {}）", active_jobs.len()),
                _ => "导出处理中".to_string(),
            };
            return (label, false, true);
        }

        if let Some((message, is_error)) = &self.state.status_hint {
            return (message.clone(), *is_error, false);
        }

        ("就绪".to_string(), false, false)
    }
}

fn app_preferences_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    base.join("mondrian").join("app_preferences.json")
}

fn sanitize_filename(raw: &str) -> String {
    let mut s = raw
        .chars()
        .map(|c| {
            if c == '\\'
                || c == '/'
                || c == ':'
                || c == '*'
                || c == '?'
                || c == '"'
                || c == '<'
                || c == '>'
                || c == '|'
            {
                '_'
            } else {
                c
            }
        })
        .collect::<String>();

    s = s.trim().to_string();
    if s.is_empty() {
        "未命名项目".to_string()
    } else {
        s
    }
}

fn ensure_project_extension(path: PathBuf) -> PathBuf {
    let has_expected_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case(PROJECT_EXTENSION))
        .unwrap_or(false);

    if has_expected_ext {
        path
    } else {
        path.with_extension(PROJECT_EXTENSION)
    }
}

#[cfg(test)]
mod timeline_edit_tests {
    use super::*;

    fn create_state_with_sequence() -> AppState {
        let mut state = AppState::new();
        state.sequence = Some(Sequence::new("test"));
        state
    }

    fn primary_track_clip_lens(state: &AppState) -> (usize, usize) {
        let seq = state.sequence.as_ref().expect("sequence should exist");
        (
            seq.video_tracks[0].clips.len(),
            seq.audio_tracks[0].clips.len(),
        )
    }

    #[test]
    fn split_at_playhead_records_single_undo_step() {
        let mut state = create_state_with_sequence();
        let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

        let video_clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(40, tb));
        let audio_clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(40, tb));

        state.sequence.as_mut().expect("sequence should exist").video_tracks[0]
            .add_clip(video_clip)
            .expect("add video clip");
        state.sequence.as_mut().expect("sequence should exist").audio_tracks[0]
            .add_clip(audio_clip)
            .expect("add audio clip");

        state.seek(10);

        let split_count = state.split_at_playhead().expect("split should succeed");
        assert_eq!(split_count, 2);
        assert_eq!(
            state.cmd_history.undo_description(),
            Some("在播放头分割片段")
        );
        assert_eq!(primary_track_clip_lens(&state), (2, 2));

        assert!(state.undo_timeline().expect("undo should succeed"));
        assert_eq!(primary_track_clip_lens(&state), (1, 1));

        assert!(state.redo_timeline().expect("redo should succeed"));
        assert_eq!(primary_track_clip_lens(&state), (2, 2));
    }

    #[test]
    fn track_lock_change_is_undoable() {
        let mut state = create_state_with_sequence();
        let track_id = state.sequence.as_ref().expect("sequence should exist").video_tracks[0].id;

        state
            .set_track_locked(track_id, true, true)
            .expect("set track locked should succeed");
        assert!(state.sequence.as_ref().expect("sequence should exist").video_tracks[0].is_locked);
        assert_eq!(state.cmd_history.undo_description(), Some("切换轨道锁定"));

        assert!(state.undo_timeline().expect("undo should succeed"));
        assert!(!state.sequence.as_ref().expect("sequence should exist").video_tracks[0].is_locked);

        assert!(state.redo_timeline().expect("redo should succeed"));
        assert!(state.sequence.as_ref().expect("sequence should exist").video_tracks[0].is_locked);
    }
}

#[cfg(test)]
mod perf_tests {
    use super::*;
    use serde::Serialize;
    use std::cmp;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    #[derive(Debug, Serialize)]
    struct PerfCaseReport {
        case: &'static str,
        iterations: usize,
        samples_ms: Vec<u128>,
        avg_ms: u128,
        max_ms: u128,
        threshold_ms: u128,
        passed: bool,
    }

    fn perf_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn env_u128(key: &str, default: u128) -> u128 {
        std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<u128>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(default)
    }

    fn env_usize(key: &str, default: usize) -> usize {
        std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(default)
    }

    fn perf_output_path() -> Option<PathBuf> {
        std::env::var_os("MONDRIAN_PERF_OUTPUT").map(PathBuf::from)
    }

    fn write_report_if_needed(report_json: &str) {
        if let Some(path) = perf_output_path() {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }

            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(file, "{report_json}");
            }
        }
    }

    fn run_case<F>(
        case: &'static str,
        iterations: usize,
        threshold_ms: u128,
        mut f: F,
    ) -> anyhow::Result<PerfCaseReport>
    where
        F: FnMut() -> anyhow::Result<()>,
    {
        let mut samples_ms = Vec::with_capacity(iterations);
        for _ in 0..iterations {
            let started_at = Instant::now();
            f()?;
            samples_ms.push(started_at.elapsed().as_millis());
        }

        let total_ms = samples_ms.iter().copied().sum::<u128>();
        let avg_ms = total_ms / cmp::max(iterations as u128, 1);
        let max_ms = samples_ms.iter().copied().max().unwrap_or(0);
        let passed = max_ms <= threshold_ms;

        Ok(PerfCaseReport {
            case,
            iterations,
            samples_ms,
            avg_ms,
            max_ms,
            threshold_ms,
            passed,
        })
    }

    #[test]
    #[ignore = "development performance smoke test; run manually"]
    fn perf_project_lifecycle_smoke() -> anyhow::Result<()> {
        let _guard = perf_lock().lock().expect("perf lock poisoned");

        let create_threshold_ms = env_u128("MONDRIAN_PERF_CREATE_MS", 8_000);
        let open_threshold_ms = env_u128("MONDRIAN_PERF_OPEN_MS", 6_000);
        let save_threshold_ms = env_u128("MONDRIAN_PERF_SAVE_MS", 6_000);

        let open_iters = env_usize("MONDRIAN_PERF_OPEN_ITERS", 3);
        let save_iters = env_usize("MONDRIAN_PERF_SAVE_ITERS", 5);

        let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let root = std::env::temp_dir().join(format!("mondrian_perf_smoke_{uniq}"));
        fs::create_dir_all(&root)?;

        let result = (|| -> anyhow::Result<Vec<PerfCaseReport>> {
            let project_path = root.join("perf-smoke.mdp");
            let mut state = AppState::new();

            let create_case =
                run_case("project.create_new_project", 1, create_threshold_ms, || {
                    state.create_new_project_at(
                        project_path.clone(),
                        "perf-smoke",
                        1920,
                        1080,
                        Rational::FPS_25,
                    )
                })?;

            let open_case = run_case(
                "project.open_existing",
                open_iters,
                open_threshold_ms,
                || state.open_project_file(project_path.clone()),
            )?;

            let save_case = run_case(
                "project.save_existing",
                save_iters,
                save_threshold_ms,
                || state.save_project_file(),
            )?;

            Ok(vec![create_case, open_case, save_case])
        })();

        let _ = fs::remove_dir_all(&root);

        let report = result?;
        let report_json = serde_json::to_string(&report)?;
        eprintln!("MONDRIAN_PERF_JSON={report_json}");
        write_report_if_needed(&report_json);

        let failed_cases: Vec<_> = report.iter().filter(|c| !c.passed).map(|c| c.case).collect();
        if !failed_cases.is_empty() {
            anyhow::bail!(
                "performance smoke test failed: {:?}; report: {}",
                failed_cases,
                report_json
            );
        }

        Ok(())
    }
}
