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
    automation::{PropertyHost, PropertyMutation},
    events::{AppEvent, EventBus},
    types::{AssetId, ClipId, Rational, Resolution, SequenceId, TimeCode, TrackId},
};
use mondrian_export::queue::{JobStatus, RenderQueue};
use mondrian_media::audio::{
    AudioBuffer, AudioClock, AudioMixer, AudioSourceCache, AudioSyncController, AudioTrackConfig,
    AudioTrackData, ClockRole, RealtimeAudioOutput,
};
use mondrian_timeline::clip::{Clip, TrimEdge};
use mondrian_timeline::command::SequenceSnapshotCommand;
use mondrian_timeline::sequence::Sequence;
use rfd::FileDialog;
use serde::{Deserialize, Serialize};

use crate::shortcuts::{ShortcutAction, ShortcutBinding, ShortcutKey, ShortcutPreferences};
use crate::ui::{
    effect_controls_panel::EffectControlsPanel,
    export_panel::ExportPanel,
    library_panel::LibraryPanel,
    timeline_panel::{SelectedClipRef, TimelinePanel},
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AutosaveManifest {
    project_file: PathBuf,
    #[serde(default)]
    snapshots: Vec<AutosaveSnapshotEntry>,
    #[serde(default)]
    autosave_file: Option<PathBuf>,
    #[serde(default)]
    saved_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AutosaveSnapshotEntry {
    file: PathBuf,
    saved_at_unix_ms: u64,
}

impl AutosaveManifest {
    fn normalize_legacy_fields(&mut self) {
        if self.snapshots.is_empty() {
            if let Some(file) = self.autosave_file.clone() {
                self.snapshots.push(AutosaveSnapshotEntry {
                    file,
                    saved_at_unix_ms: self.saved_at_unix_ms.unwrap_or(0),
                });
            }
        }

        self.snapshots.retain(|s| s.file.exists());
        self.snapshots.sort_by_key(|s| std::cmp::Reverse(s.saved_at_unix_ms));
        self.snapshots.dedup_by_key(|s| s.file.clone());

        if let Some(latest) = self.snapshots.first() {
            self.autosave_file = Some(latest.file.clone());
            self.saved_at_unix_ms = Some(latest.saved_at_unix_ms);
        } else {
            self.autosave_file = None;
            self.saved_at_unix_ms = None;
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingCloseAction {
    CloseProject,
    QuitApp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct AppPreferences {
    version: u32,
    #[serde(default = "default_app_theme")]
    theme: crate::ui::theme::Theme,
    show_library: bool,
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
    #[serde(default = "default_auto_save_enabled")]
    auto_save_enabled: bool,
    #[serde(default = "default_auto_save_interval_secs")]
    auto_save_interval_secs: u32,
    #[serde(default = "default_auto_save_max_recovery_points")]
    auto_save_max_recovery_points: u32,
    #[serde(default = "default_auto_save_retention_days")]
    auto_save_retention_days: u32,
    viewer: ViewerPreferences,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            version: 1,
            theme: default_app_theme(),
            show_library: true,
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
            auto_save_enabled: default_auto_save_enabled(),
            auto_save_interval_secs: default_auto_save_interval_secs(),
            auto_save_max_recovery_points: default_auto_save_max_recovery_points(),
            auto_save_retention_days: default_auto_save_retention_days(),
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

const fn default_auto_save_enabled() -> bool {
    true
}

const fn default_auto_save_interval_secs() -> u32 {
    60
}

const fn default_auto_save_max_recovery_points() -> u32 {
    10
}

const fn default_auto_save_retention_days() -> u32 {
    7
}

#[derive(Debug, Clone)]
struct CrashRecoveryCandidate {
    project_file: PathBuf,
    autosave_file: PathBuf,
    saved_at_unix_ms: u64,
    total_snapshots: usize,
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

        let saved = Self::read_project_data_from_archive(project_file)?;

        let file = fs::File::open(project_file)?;
        let mut archive = zip::ZipArchive::new(file)?;

        let mut db_entry = archive.by_name("library/index.db")?;
        let mut db_file = fs::File::create(runtime_root.join("library").join("index.db"))?;
        std::io::copy(&mut db_entry, &mut db_file)?;
        db_file.flush()?;

        Ok(saved)
    }

    fn read_project_data_from_archive(project_file: &Path) -> anyhow::Result<ProjectFile> {
        let file = fs::File::open(project_file)?;
        let mut archive = zip::ZipArchive::new(file)?;
        let mut project_json = String::new();
        archive.by_name("project.json")?.read_to_string(&mut project_json)?;
        let saved = serde_json::from_str::<ProjectFile>(&project_json)?;
        Ok(saved)
    }

    fn save_project_container(&self, project_data: &ProjectFile) -> anyhow::Result<()> {
        let project_file = self.project_file_path()?.to_path_buf();
        self.save_project_container_to(project_data, project_file.as_path())
    }

    fn save_project_container_to(
        &self,
        project_data: &ProjectFile,
        target_file: &Path,
    ) -> anyhow::Result<()> {
        let started_at = std::time::Instant::now();
        let runtime_library_root = self.runtime_library_root()?;
        let db_path = runtime_library_root.join("index.db");

        if !db_path.exists() {
            anyhow::bail!("素材库数据库不存在：{}", db_path.display());
        }

        if let Some(parent) = target_file.parent() {
            fs::create_dir_all(parent)?;
        }

        let tmp_extension = target_file
            .extension()
            .and_then(|v| v.to_str())
            .map(|ext| format!("{ext}.tmp"))
            .unwrap_or_else(|| "tmp".to_string());
        let tmp_path = target_file.with_extension(tmp_extension);

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

        if target_file.exists() {
            fs::remove_file(target_file)?;
        }
        fs::rename(&tmp_path, target_file)?;

        if ui_diag_enabled() {
            let elapsed_ms = started_at.elapsed().as_millis() as u64;
            if elapsed_ms >= ui_diag_slow_threshold_ms() {
                tracing::warn!("[ui-diag] save_project_container slow: {}ms", elapsed_ms);
            }
        }
        Ok(())
    }

    fn current_project_data(&self) -> Option<ProjectFile> {
        let sequence = self.sequence.as_ref()?;
        let mut proxy_mode_assets: Vec<AssetId> = self.proxy_mode_assets.iter().copied().collect();
        proxy_mode_assets.sort_by_key(|id| id.to_string());
        Some(ProjectFile {
            name: sequence.name.clone(),
            sequence: sequence.clone(),
            in_point_frame: self.project_in_point,
            out_point_frame: self.project_out_point,
            proxy_mode_assets,
        })
    }

    fn autosave_root(&self) -> anyhow::Result<PathBuf> {
        let runtime_root = self
            .project_runtime_dir
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("项目运行目录未初始化"))?;
        Ok(runtime_root.join("autosave"))
    }

    fn legacy_autosave_archive_path(runtime_root: &Path) -> PathBuf {
        runtime_root.join("autosave").join("project.autosave.mdp")
    }

    fn autosave_manifest_path(runtime_root: &Path) -> PathBuf {
        runtime_root.join("autosave").join("manifest.json")
    }

    fn load_autosave_manifest(
        runtime_root: &Path,
        project_file: &Path,
    ) -> anyhow::Result<AutosaveManifest> {
        let manifest_path = Self::autosave_manifest_path(runtime_root);
        if !manifest_path.exists() {
            let legacy_file = Self::legacy_autosave_archive_path(runtime_root);
            let mut manifest = AutosaveManifest {
                project_file: project_file.to_path_buf(),
                snapshots: Vec::new(),
                autosave_file: if legacy_file.exists() {
                    Some(legacy_file)
                } else {
                    None
                },
                saved_at_unix_ms: Some(0),
            };
            manifest.normalize_legacy_fields();
            return Ok(manifest);
        }

        let bytes = fs::read(&manifest_path)?;
        let mut manifest = serde_json::from_slice::<AutosaveManifest>(&bytes)?;
        if manifest.project_file.as_os_str().is_empty() {
            manifest.project_file = project_file.to_path_buf();
        }
        manifest.normalize_legacy_fields();
        Ok(manifest)
    }

    pub fn write_autosave_snapshot(
        &self,
        max_recovery_points: usize,
        retention_days: u32,
    ) -> anyhow::Result<PathBuf> {
        let project_file = self.project_file_path()?.to_path_buf();
        let data = self
            .current_project_data()
            .ok_or_else(|| anyhow::anyhow!("当前无可自动保存的项目"))?;

        let autosave_root = self.autosave_root()?;
        fs::create_dir_all(&autosave_root)?;

        let runtime_root = self
            .project_runtime_dir
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("项目运行目录未初始化"))?;
        let mut manifest = Self::load_autosave_manifest(runtime_root, project_file.as_path())
            .unwrap_or(AutosaveManifest {
                project_file: project_file.clone(),
                snapshots: Vec::new(),
                autosave_file: None,
                saved_at_unix_ms: None,
            });

        let saved_at = unix_now_ms();
        let autosave_file = autosave_root.join(format!("project-{saved_at}.autosave.mdp"));
        self.save_project_container_to(&data, autosave_file.as_path())?;

        manifest.project_file = project_file;
        manifest.snapshots.push(AutosaveSnapshotEntry {
            file: autosave_file.clone(),
            saved_at_unix_ms: saved_at,
        });
        apply_autosave_retention(
            &mut manifest,
            max_recovery_points.max(1),
            retention_days.max(1),
        );
        manifest.normalize_legacy_fields();

        write_json_atomic(
            Self::autosave_manifest_path(runtime_root).as_path(),
            &manifest,
        )?;
        Ok(autosave_file)
    }

    pub fn open_project_from_autosave_snapshot(
        &mut self,
        project_file: PathBuf,
        autosave_file: PathBuf,
    ) -> anyhow::Result<()> {
        if !autosave_file.exists() {
            anyhow::bail!("未找到自动保存文件：{}", autosave_file.display());
        }

        let staged = std::env::temp_dir().join(format!(
            "mondrian-autosave-recover-{}-{}.mdp",
            std::process::id(),
            unix_now_ms()
        ));
        fs::copy(&autosave_file, &staged)?;

        let open_result = self.open_project_archive(project_file.clone(), staged.as_path());
        let _ = fs::remove_file(&staged);
        open_result?;

        self.save_project_file()?;
        let autosave_root = Self::project_runtime_root(project_file.as_path()).join("autosave");
        let _ = fs::remove_dir_all(autosave_root);
        Ok(())
    }

    pub fn open_project_from_autosave(&mut self, project_file: PathBuf) -> anyhow::Result<()> {
        let candidate = discover_crash_recovery_candidates()
            .into_iter()
            .find(|c| c.project_file == project_file)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "未找到自动保存文件：{}",
                    Self::legacy_autosave_archive_path(
                        Self::project_runtime_root(project_file.as_path()).as_path()
                    )
                    .display()
                )
            })?;

        self.open_project_from_autosave_snapshot(project_file, candidate.autosave_file)
    }

    pub fn has_open_project(&self) -> bool {
        self.sequence.is_some() && self.current_project_path.is_some()
    }

    fn open_project_archive(
        &mut self,
        project_file: PathBuf,
        archive_file: &Path,
    ) -> anyhow::Result<()> {
        if let Some(prev_runtime) = self.project_runtime_dir.as_ref() {
            let _ = fs::remove_dir_all(prev_runtime);
        }

        let runtime_root = Self::project_runtime_root(&project_file);
        if runtime_root.exists() {
            let _ = fs::remove_dir_all(&runtime_root);
        }

        let saved = Self::load_project_container(archive_file, &runtime_root)?;

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

    pub fn open_project_file(&mut self, project_file: PathBuf) -> anyhow::Result<()> {
        self.open_project_archive(project_file.clone(), project_file.as_path())
    }

    pub fn save_project_file(&self) -> anyhow::Result<()> {
        let Some(data) = self.current_project_data() else {
            return Ok(());
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
            seq.normalize_track_names();
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
        let offline_assets: Vec<_> =
            assets.into_iter().filter(|asset| !asset.path.exists()).collect();
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

    pub fn current_time_code(&self) -> Option<TimeCode> {
        let seq = self.sequence.as_ref()?;
        Some(TimeCode::new(self.current_frame().max(0), seq.time_base()))
    }

    pub fn clip_snapshot(&self, selection: SelectedClipRef) -> Option<Clip> {
        let seq = self.sequence.as_ref()?;
        find_clip_by_selection(seq, selection).cloned()
    }

    pub fn mutate_clip_property(
        &mut self,
        selection: SelectedClipRef,
        mutation: PropertyMutation,
        description: impl Into<String>,
    ) -> mondrian_core::Result<bool> {
        let description = description.into();
        let (sequence_id, before, after) = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "mutate_clip_property".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;

            let before = seq.clone();
            let Some((track_id, _is_video, is_locked)) =
                find_clip_track_lock(seq, selection.clip_id)
            else {
                return Err(mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                });
            };
            if is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track_id.to_string(),
                });
            }

            let clip = find_clip_mut_by_selection(seq, selection).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                }
            })?;
            clip.apply_property_mutation(mutation)?;
            (seq.id, before, seq.clone())
        };

        self.record_sequence_snapshot_command(description, before, after);
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        let _ = self.save_project_file();
        Ok(true)
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

#[derive(Clone, Copy)]
struct RollBoundary {
    left_index: usize,
    right_index: usize,
    current_cut_frame: i64,
    min_frame: i64,
    max_frame: i64,
}

fn roll_cut_for_clip_internal(
    seq: &mut Sequence,
    clip_id: ClipId,
    target_frame: i64,
) -> mondrian_core::Result<bool> {
    for track in &mut seq.video_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            return roll_cut_in_track(track, index, target_frame);
        }
    }

    for track in &mut seq.audio_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            return roll_cut_in_track(track, index, target_frame);
        }
    }

    Err(mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() })
}

fn roll_cut_in_track(
    track: &mut mondrian_timeline::track::Track,
    clip_index: usize,
    target_frame: i64,
) -> mondrian_core::Result<bool> {
    let Some(current_clip) = track.clips.get(clip_index).cloned() else {
        return Ok(false);
    };

    let mut boundaries = Vec::with_capacity(2);

    if clip_index > 0 {
        let left = &track.clips[clip_index - 1];
        let right = &current_clip;
        if left.end_position().frame == right.position.frame {
            let min_frame_from_source_in =
                right.position.frame.saturating_sub(right.source_in.frame);
            let min_frame = (left.position.frame + 1).max(min_frame_from_source_in);
            let max_frame = right.end_position().frame - 1;
            if min_frame <= max_frame {
                boundaries.push(RollBoundary {
                    left_index: clip_index - 1,
                    right_index: clip_index,
                    current_cut_frame: right.position.frame,
                    min_frame,
                    max_frame,
                });
            }
        }
    }

    if clip_index + 1 < track.clips.len() {
        let left = &current_clip;
        let right = &track.clips[clip_index + 1];
        if left.end_position().frame == right.position.frame {
            let min_frame_from_source_in =
                right.position.frame.saturating_sub(right.source_in.frame);
            let min_frame = (left.position.frame + 1).max(min_frame_from_source_in);
            let max_frame = right.end_position().frame - 1;
            if min_frame <= max_frame {
                boundaries.push(RollBoundary {
                    left_index: clip_index,
                    right_index: clip_index + 1,
                    current_cut_frame: left.end_position().frame,
                    min_frame,
                    max_frame,
                });
            }
        }
    }

    let Some(boundary) = boundaries
        .into_iter()
        .min_by_key(|candidate| (target_frame as i128 - candidate.current_cut_frame as i128).abs())
    else {
        return Ok(false);
    };

    let new_cut_frame = target_frame.clamp(boundary.min_frame, boundary.max_frame);
    if new_cut_frame == boundary.current_cut_frame {
        return Ok(false);
    }

    let left_original = track.clips[boundary.left_index].clone();
    let right_original = track.clips[boundary.right_index].clone();

    let new_left_duration = new_cut_frame - left_original.position.frame;
    let new_right_duration = right_original.end_position().frame - new_cut_frame;
    if new_left_duration <= 0 || new_right_duration <= 0 {
        return Err(mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: "roll_cut".to_string(),
            reason: "滚动修剪后片段时长无效".to_string(),
        });
    }

    let new_left_source_out = left_original.timeline_to_source_time(TimeCode::new(
        new_cut_frame,
        left_original.position.time_base,
    ));
    let new_right_source_in = right_original.timeline_to_source_time(TimeCode::new(
        new_cut_frame,
        right_original.position.time_base,
    ));

    let mut left_updated = left_original;
    left_updated.duration = TimeCode::new(new_left_duration, left_updated.duration.time_base);
    left_updated.source_out = new_left_source_out;

    let mut right_updated = right_original;
    right_updated.position = TimeCode::new(new_cut_frame, right_updated.position.time_base);
    right_updated.duration = TimeCode::new(new_right_duration, right_updated.duration.time_base);
    right_updated.source_in = new_right_source_in;

    track.clips[boundary.left_index] = left_updated;
    track.clips[boundary.right_index] = right_updated;
    track.clips.sort_by_key(|clip| clip.position.frame);
    Ok(true)
}

fn slip_clip_internal(
    seq: &mut Sequence,
    library: &AssetLibrary,
    clip_id: ClipId,
    delta_frames: i64,
) -> mondrian_core::Result<bool> {
    for track in &mut seq.video_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            let estimated_total_source_frames =
                estimate_asset_total_source_frames(library, &track.clips[index]);
            return slip_clip_in_track(track, index, delta_frames, estimated_total_source_frames);
        }
    }

    for track in &mut seq.audio_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            let estimated_total_source_frames =
                estimate_asset_total_source_frames(library, &track.clips[index]);
            return slip_clip_in_track(track, index, delta_frames, estimated_total_source_frames);
        }
    }

    Err(mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() })
}

fn slip_clip_in_track(
    track: &mut mondrian_timeline::track::Track,
    clip_index: usize,
    delta_frames: i64,
    estimated_total_source_frames: Option<i64>,
) -> mondrian_core::Result<bool> {
    let Some(original) = track.clips.get(clip_index).cloned() else {
        return Ok(false);
    };

    let source_span = original.source_out.frame - original.source_in.frame;
    if source_span <= 0 {
        return Err(mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: "slip_clip".to_string(),
            reason: "片段源时间范围无效".to_string(),
        });
    }

    let max_source_in = if let Some(total_frames) = estimated_total_source_frames {
        total_frames.saturating_sub(source_span).max(0)
    } else {
        i64::MAX.saturating_sub(source_span)
    };

    let old_source_in = original.source_in.frame;
    let proposed = old_source_in.saturating_add(delta_frames);
    let new_source_in = proposed.clamp(0, max_source_in);
    if new_source_in == old_source_in {
        return Ok(false);
    }

    let new_source_out = new_source_in.saturating_add(source_span);
    let mut updated = original;
    updated.source_in = TimeCode::new(new_source_in, updated.source_in.time_base);
    updated.source_out = TimeCode::new(new_source_out, updated.source_out.time_base);
    track.clips[clip_index] = updated;
    Ok(true)
}

fn estimate_asset_total_source_frames(library: &AssetLibrary, clip: &Clip) -> Option<i64> {
    let asset = match library.get_asset(clip.asset_id) {
        Ok(Some(asset)) => asset,
        Ok(None) => return None,
        Err(err) => {
            tracing::debug!("读取素材时长失败 {}: {}", clip.asset_id, err);
            return None;
        }
    };

    let frames_from_stream =
        asset.media_info.estimated_frames().map(|v| v as i64).filter(|v| *v > 0);
    if frames_from_stream.is_some() {
        return frames_from_stream;
    }

    let duration_secs = asset.media_info.duration.as_secs_f64();
    if duration_secs <= 0.0 {
        return None;
    }

    let frame_duration_secs = clip.position.time_base.to_f64();
    if frame_duration_secs <= f64::EPSILON {
        return None;
    }

    Some((duration_secs / frame_duration_secs).ceil() as i64)
}

fn slide_clip_internal(
    seq: &mut Sequence,
    clip_id: ClipId,
    delta_frames: i64,
) -> mondrian_core::Result<bool> {
    for track in &mut seq.video_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            return slide_clip_in_track(track, index, delta_frames);
        }
    }

    for track in &mut seq.audio_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            return slide_clip_in_track(track, index, delta_frames);
        }
    }

    Err(mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() })
}

fn slide_clip_in_track(
    track: &mut mondrian_timeline::track::Track,
    clip_index: usize,
    delta_frames: i64,
) -> mondrian_core::Result<bool> {
    if clip_index == 0 || clip_index + 1 >= track.clips.len() {
        return Ok(false);
    }

    let left_original = track.clips[clip_index - 1].clone();
    let center_original = track.clips[clip_index].clone();
    let right_original = track.clips[clip_index + 1].clone();

    let center_start = center_original.position.frame;
    let center_end = center_original.end_position().frame;
    let left_end = left_original.end_position().frame;
    let right_start = right_original.position.frame;

    if left_end != center_start || center_end != right_start {
        return Ok(false);
    }

    let min_start_from_left = left_original.position.frame + 1;
    let min_start_from_right_source = right_original
        .position
        .frame
        .saturating_sub(center_original.duration.frame)
        .saturating_sub(right_original.source_in.frame);
    let min_start = min_start_from_left.max(min_start_from_right_source);
    let max_start = right_original.end_position().frame - center_original.duration.frame - 1;
    if min_start > max_start {
        return Ok(false);
    }

    let proposed_start = center_start.saturating_add(delta_frames);
    let new_center_start = proposed_start.clamp(min_start, max_start);
    if new_center_start == center_start {
        return Ok(false);
    }

    let new_center_end = new_center_start + center_original.duration.frame;
    let new_left_duration = new_center_start - left_original.position.frame;
    let new_right_duration = right_original.end_position().frame - new_center_end;
    if new_left_duration <= 0 || new_right_duration <= 0 {
        return Err(mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: "slide_clip".to_string(),
            reason: "滑动后片段时长无效".to_string(),
        });
    }

    let new_left_source_out = left_original.timeline_to_source_time(TimeCode::new(
        new_center_start,
        left_original.position.time_base,
    ));
    let new_right_source_in = right_original.timeline_to_source_time(TimeCode::new(
        new_center_end,
        right_original.position.time_base,
    ));

    let mut left_updated = left_original;
    left_updated.duration = TimeCode::new(new_left_duration, left_updated.duration.time_base);
    left_updated.source_out = new_left_source_out;

    let mut center_updated = center_original;
    center_updated.position = TimeCode::new(new_center_start, center_updated.position.time_base);

    let mut right_updated = right_original;
    right_updated.position = TimeCode::new(new_center_end, right_updated.position.time_base);
    right_updated.duration = TimeCode::new(new_right_duration, right_updated.duration.time_base);
    right_updated.source_in = new_right_source_in;

    track.clips[clip_index - 1] = left_updated;
    track.clips[clip_index] = center_updated;
    track.clips[clip_index + 1] = right_updated;
    track.clips.sort_by_key(|clip| clip.position.frame);
    Ok(true)
}

fn trim_clip_edge_internal(
    seq: &mut Sequence,
    clip_id: ClipId,
    edge: TrimEdge,
    target_frame: i64,
) -> mondrian_core::Result<bool> {
    for track in &mut seq.video_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            return trim_clip_in_track(track, index, edge, target_frame);
        }
    }

    for track in &mut seq.audio_tracks {
        if let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            if track.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track.id.to_string(),
                });
            }
            return trim_clip_in_track(track, index, edge, target_frame);
        }
    }

    Err(mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() })
}

fn trim_clip_in_track(
    track: &mut mondrian_timeline::track::Track,
    index: usize,
    edge: TrimEdge,
    target_frame: i64,
) -> mondrian_core::Result<bool> {
    let Some(original) = track.clips.get(index).cloned() else {
        return Ok(false);
    };
    let start = original.position.frame;
    let end = original.end_position().frame;
    if end <= start {
        return Ok(false);
    }

    let mut updated = original.clone();
    match edge {
        TrimEdge::In => {
            let new_start = target_frame.max(start).min(end - 1);
            if new_start == start {
                return Ok(false);
            }
            let new_in = original
                .timeline_to_source_time(TimeCode::new(new_start, original.position.time_base));
            updated.position = TimeCode::new(new_start, original.position.time_base);
            updated.duration = TimeCode::new(end - new_start, original.duration.time_base);
            updated.source_in = new_in;
        }
        TrimEdge::Out => {
            let new_end = target_frame.max(start + 1).min(end);
            if new_end == end {
                return Ok(false);
            }
            let new_out = original
                .timeline_to_source_time(TimeCode::new(new_end, original.position.time_base));
            updated.duration = TimeCode::new(new_end - start, original.duration.time_base);
            updated.source_out = new_out;
        }
    }

    if updated.duration.frame <= 0 {
        return Err(mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: "trim_clip".to_string(),
            reason: "修剪后片段时长无效".to_string(),
        });
    }

    track.clips[index] = updated;
    track.clips.sort_by_key(|clip| clip.position.frame);
    Ok(true)
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

fn merge_ranges(mut ranges: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    ranges.retain(|(start, end)| end > start);
    if ranges.is_empty() {
        return ranges;
    }
    ranges.sort_by_key(|(start, _)| *start);

    let mut merged = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        if let Some((_, last_end)) = merged.last_mut() {
            if start <= *last_end {
                *last_end = (*last_end).max(end);
                continue;
            }
        }
        merged.push((start, end));
    }
    merged
}

fn subtract_overwrite_range_from_clip(
    clip: Clip,
    overlap_start: i64,
    overlap_end: i64,
) -> Vec<Clip> {
    if overlap_end <= overlap_start {
        return vec![clip];
    }

    let clip_start = clip.position.frame;
    let clip_end = clip.end_position().frame;
    let cut_start = overlap_start.max(clip_start);
    let cut_end = overlap_end.min(clip_end);
    if cut_end <= cut_start {
        return vec![clip];
    }
    if cut_start <= clip_start && cut_end >= clip_end {
        return Vec::new();
    }

    if cut_start <= clip_start {
        let mut right = clip;
        let new_start = cut_end.max(clip_start);
        let new_source_in =
            right.timeline_to_source_time(TimeCode::new(new_start, right.position.time_base));
        right.position = TimeCode::new(new_start, right.position.time_base);
        right.duration = TimeCode::new((clip_end - new_start).max(0), right.duration.time_base);
        right.source_in = new_source_in;
        return if right.duration.frame > 0 {
            vec![right]
        } else {
            Vec::new()
        };
    }

    if cut_end >= clip_end {
        let mut left = clip;
        let new_end = cut_start.min(clip_end);
        let new_source_out =
            left.timeline_to_source_time(TimeCode::new(new_end, left.position.time_base));
        left.duration = TimeCode::new((new_end - clip_start).max(0), left.duration.time_base);
        left.source_out = new_source_out;
        return if left.duration.frame > 0 {
            vec![left]
        } else {
            Vec::new()
        };
    }

    let mut left = clip.clone();
    let left_new_end = cut_start;
    let left_new_source_out =
        left.timeline_to_source_time(TimeCode::new(left_new_end, left.position.time_base));
    left.duration = TimeCode::new((left_new_end - clip_start).max(0), left.duration.time_base);
    left.source_out = left_new_source_out;

    let mut right = clip;
    let right_new_start = cut_end;
    let right_new_source_in =
        right.timeline_to_source_time(TimeCode::new(right_new_start, right.position.time_base));
    right.id = ClipId::new();
    right.position = TimeCode::new(right_new_start, right.position.time_base);
    right.duration = TimeCode::new(
        (clip_end - right_new_start).max(0),
        right.duration.time_base,
    );
    right.source_in = right_new_source_in;
    right.linked_clip = None;

    let mut result = Vec::with_capacity(2);
    if left.duration.frame > 0 {
        result.push(left);
    }
    if right.duration.frame > 0 {
        result.push(right);
    }
    result
}

fn apply_overwrite_conflicts(
    track: &mut mondrian_timeline::track::Track,
    focus_ids: &HashSet<ClipId>,
    focus_ranges: Vec<(i64, i64)>,
) {
    let merged_ranges = merge_ranges(focus_ranges);
    if merged_ranges.is_empty() {
        track.clips.sort_by_key(|c| c.position.frame);
        return;
    }

    let mut resolved = Vec::<Clip>::with_capacity(track.clips.len());
    for clip in std::mem::take(&mut track.clips) {
        if focus_ids.contains(&clip.id) {
            resolved.push(clip);
            continue;
        }

        let mut segments = vec![clip];
        for (range_start, range_end) in &merged_ranges {
            if segments.is_empty() {
                break;
            }
            let mut next_segments = Vec::with_capacity(segments.len());
            for segment in segments {
                next_segments.extend(subtract_overwrite_range_from_clip(
                    segment,
                    *range_start,
                    *range_end,
                ));
            }
            segments = next_segments;
        }
        resolved.extend(segments);
    }

    track.clips = resolved;
    track.clips.sort_by_key(|c| c.position.frame);
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
            let focus_ids = HashSet::from([focus_clip_id]);
            apply_overwrite_conflicts(
                track,
                &focus_ids,
                vec![(focus.position.frame, focus.end_position().frame)],
            );
        }
    }
}

fn apply_track_conflicts_for_focus_group(
    track: &mut mondrian_timeline::track::Track,
    focus_ids: &HashSet<ClipId>,
    mode: ClipOverlapMode,
) {
    if focus_ids.is_empty() {
        return;
    }
    match mode {
        ClipOverlapMode::Insert => resolve_track_overlaps(track),
        ClipOverlapMode::Overwrite => {
            let focus_ranges: Vec<(i64, i64)> = track
                .clips
                .iter()
                .filter(|clip| focus_ids.contains(&clip.id))
                .map(|clip| (clip.position.frame, clip.end_position().frame))
                .collect();
            if focus_ranges.is_empty() {
                track.clips.sort_by_key(|c| c.position.frame);
                return;
            }
            apply_overwrite_conflicts(track, focus_ids, focus_ranges);
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

fn find_clip(seq: &Sequence, clip_id: ClipId) -> Option<&Clip> {
    for track in &seq.video_tracks {
        if let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) {
            return Some(clip);
        }
    }
    for track in &seq.audio_tracks {
        if let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) {
            return Some(clip);
        }
    }
    None
}

fn find_clip_by_selection(seq: &Sequence, selection: SelectedClipRef) -> Option<&Clip> {
    if selection.is_video_track {
        seq.video_tracks
            .iter()
            .find(|track| track.id == selection.track_id)
            .and_then(|track| track.clips.iter().find(|clip| clip.id == selection.clip_id))
    } else {
        seq.audio_tracks
            .iter()
            .find(|track| track.id == selection.track_id)
            .and_then(|track| track.clips.iter().find(|clip| clip.id == selection.clip_id))
    }
}

fn find_clip_track_lock(seq: &Sequence, clip_id: ClipId) -> Option<(TrackId, bool, bool)> {
    for track in &seq.video_tracks {
        if track.clips.iter().any(|c| c.id == clip_id) {
            return Some((track.id, true, track.is_locked));
        }
    }
    for track in &seq.audio_tracks {
        if track.clips.iter().any(|c| c.id == clip_id) {
            return Some((track.id, false, track.is_locked));
        }
    }
    None
}

fn set_clip_disabled(seq: &mut Sequence, clip_id: ClipId, disabled: bool) -> bool {
    if let Some(clip) = find_clip_mut(seq, clip_id) {
        if clip.is_disabled == disabled {
            return false;
        }
        clip.is_disabled = disabled;
        return true;
    }
    false
}

fn set_clip_position(seq: &mut Sequence, clip_id: ClipId, frame: i64) -> bool {
    if let Some(clip) = find_clip_mut(seq, clip_id) {
        let frame = frame.max(0);
        if clip.position.frame == frame {
            return false;
        }
        clip.position = TimeCode::new(frame, clip.position.time_base);
        return true;
    }
    false
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

fn find_clip_mut_by_selection(seq: &mut Sequence, selection: SelectedClipRef) -> Option<&mut Clip> {
    if selection.is_video_track {
        seq.video_tracks
            .iter_mut()
            .find(|track| track.id == selection.track_id)
            .and_then(|track| track.clips.iter_mut().find(|clip| clip.id == selection.clip_id))
    } else {
        seq.audio_tracks
            .iter_mut()
            .find(|track| track.id == selection.track_id)
            .and_then(|track| track.clips.iter_mut().find(|clip| clip.id == selection.clip_id))
    }
}

// ─────────────────────────────────────────────
//  MondrianApp — eframe::App 实现
// ─────────────────────────────────────────────

pub struct MondrianApp {
    state: AppState,

    // UI 面板
    timeline_panel: TimelinePanel,
    effect_controls_panel: EffectControlsPanel,
    viewer_panel: ViewerPanel,
    library_panel: LibraryPanel,
    export_panel: ExportPanel,

    // 面板可见性
    show_library: bool,
    show_export: bool,
    show_dev_metrics: bool,
    show_preferences_dialog: bool,
    theme: crate::ui::theme::Theme,
    preferences_tab: PreferencesTab,
    capturing_shortcut: Option<ShortcutAction>,
    show_new_project_dialog: bool,
    show_project_bootstrap_dialog: bool,
    pending_close_action: Option<PendingCloseAction>,
    allow_next_viewport_close: bool,
    new_project_draft: NewProjectDraft,
    playback_last_tick: Option<std::time::Instant>,
    playback_subframe_accum: f64,
    playback_buffering_last_frame: bool,
    app_config_path: PathBuf,
    shortcuts: ShortcutPreferences,
    media_cache_auto_cleanup: bool,
    media_cache_max_size_gb: u32,
    media_cache_max_age_days: u32,
    auto_save_enabled: bool,
    auto_save_interval_secs: u32,
    auto_save_max_recovery_points: u32,
    auto_save_retention_days: u32,
    last_auto_save_at: Option<std::time::Instant>,
    auto_save_error_reported: bool,
    crash_recovery_candidates: Vec<CrashRecoveryCandidate>,
    show_video_metrics: bool,
    show_audio_metrics: bool,
    last_saved_preferences: Option<AppPreferences>,
    persist_error_reported: bool,
    last_cache_maintenance_at: Option<std::time::Instant>,
    cache_maintenance_in_flight: bool,
    cache_maintenance_rx: Option<mpsc::Receiver<anyhow::Result<MediaCacheCleanupStats>>>,
}

impl MondrianApp {
    const MENU_POPUP_MIN_WIDTH: f32 = 176.0;
    const MENU_POPUP_WIDTH: f32 = 196.0;

    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::ui::fonts::configure_fonts(&cc.egui_ctx);

        let state = AppState::new();

        let mut app = Self {
            state,
            timeline_panel: TimelinePanel::default(),
            effect_controls_panel: EffectControlsPanel::default(),
            viewer_panel: ViewerPanel::default(),
            library_panel: LibraryPanel::default(),
            export_panel: ExportPanel::default(),
            show_library: true,
            show_export: false,
            show_dev_metrics: false,
            show_preferences_dialog: false,
            theme: default_app_theme(),
            preferences_tab: PreferencesTab::default(),
            capturing_shortcut: None,
            show_new_project_dialog: false,
            show_project_bootstrap_dialog: true,
            pending_close_action: None,
            allow_next_viewport_close: false,
            new_project_draft: NewProjectDraft::default(),
            playback_last_tick: None,
            playback_subframe_accum: 0.0,
            playback_buffering_last_frame: false,
            app_config_path: app_preferences_path(),
            shortcuts: ShortcutPreferences::default(),
            media_cache_auto_cleanup: default_media_cache_auto_cleanup(),
            media_cache_max_size_gb: default_media_cache_max_size_gb(),
            media_cache_max_age_days: default_media_cache_max_age_days(),
            auto_save_enabled: default_auto_save_enabled(),
            auto_save_interval_secs: default_auto_save_interval_secs(),
            auto_save_max_recovery_points: default_auto_save_max_recovery_points(),
            auto_save_retention_days: default_auto_save_retention_days(),
            last_auto_save_at: None,
            auto_save_error_reported: false,
            crash_recovery_candidates: discover_crash_recovery_candidates(),
            show_video_metrics: default_show_video_metrics(),
            show_audio_metrics: default_show_audio_metrics(),
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

        self.handle_viewport_close_requested(ctx);

        let cache_maintenance_started_at = std::time::Instant::now();
        self.run_cache_maintenance_if_needed();
        if ui_diag_enabled() {
            log_ui_stage_slow(
                "run_cache_maintenance_if_needed",
                cache_maintenance_started_at.elapsed(),
            );
        }

        let auto_save_started_at = std::time::Instant::now();
        self.run_project_autosave_if_needed();
        if ui_diag_enabled() {
            log_ui_stage_slow(
                "run_project_autosave_if_needed",
                auto_save_started_at.elapsed(),
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
                    .fill(crate::ui::theme::palette::bg_surface())
                    .stroke(egui::Stroke::new(
                        1.0,
                        crate::ui::theme::palette::panel_divider_strong(),
                    ))
                    .inner_margin(egui::Margin::symmetric(10.0, 6.0)),
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
            .exact_height(28.0)
            .frame(
                egui::Frame::none()
                    .fill(crate::ui::theme::palette::bg_surface())
                    .stroke(egui::Stroke::new(
                        1.0,
                        crate::ui::theme::palette::panel_divider_strong(),
                    ))
                    .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
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
            .default_height(248.0)
            .height_range(120.0..=480.0)
            .frame(
                egui::Frame::none()
                    .fill(crate::ui::theme::palette::bg_base())
                    .stroke(egui::Stroke::new(
                        1.0,
                        crate::ui::theme::palette::panel_divider_strong(),
                    ))
                    .inner_margin(egui::Margin::symmetric(12.0, 8.0)),
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
                .default_width(296.0)
                .min_width(220.0)
                .frame(
                    egui::Frame::none()
                        .fill(crate::ui::theme::palette::bg_base())
                        .stroke(egui::Stroke::NONE)
                        .inner_margin(egui::Margin::symmetric(12.0, 8.0)),
                )
                .show(ctx, |ui| {
                    self.library_panel.show(ui, &mut self.state);
                });
            if ui_diag_enabled() {
                log_ui_stage_slow("library_panel", library_started_at.elapsed());
            }
        }

        let selected_clip_count = self.timeline_panel.selected_clip_count();
        let selected_clip_ref = self.timeline_panel.selected_clip_ref();
        if selected_clip_count > 0 {
            let effect_controls_started_at = std::time::Instant::now();
            egui::SidePanel::right("effect_controls_panel")
                .default_width(crate::ui::theme::tokens::inspector_panel_width())
                .min_width(crate::ui::theme::tokens::inspector_panel_min_width())
                .frame(
                    egui::Frame::none()
                        .fill(crate::ui::theme::palette::bg_base())
                        .stroke(egui::Stroke::NONE)
                        .inner_margin(egui::Margin::symmetric(12.0, 8.0)),
                )
                .show(ctx, |ui| {
                    self.effect_controls_panel.show(ui, &mut self.state, selected_clip_ref);
                });
            if ui_diag_enabled() {
                log_ui_stage_slow(
                    "effect_controls_panel",
                    effect_controls_started_at.elapsed(),
                );
            }
        }

        // ── 中央：预览窗口 ──
        let viewer_started_at = std::time::Instant::now();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(crate::ui::theme::palette::bg_base())
                    .inner_margin(egui::Margin::symmetric(12.0, 8.0)),
            )
            .show(ctx, |ui| {
                self.viewer_panel.show(
                    ui,
                    &mut self.state,
                    self.show_dev_metrics,
                    self.show_video_metrics,
                    self.show_audio_metrics,
                    true,
                );
            });
        if ui_diag_enabled() {
            log_ui_stage_slow("viewer_panel", viewer_started_at.elapsed());
        }

        // ── 导出弹窗 ──
        if self.show_export {
            let export_started_at = std::time::Instant::now();
            let mut open = self.show_export;
            egui::Window::new("导出")
                .open(&mut open)
                .default_size([560.0, 680.0])
                .frame(crate::ui::theme::dialog_frame())
                .show(ctx, |ui| {
                    self.export_panel.show(ui, &mut self.state);
                });
            self.show_export = open;
            if ui_diag_enabled() {
                log_ui_stage_slow("export_panel", export_started_at.elapsed());
            }
        }

        if self.show_new_project_dialog {
            let mut open = self.show_new_project_dialog;
            egui::Window::new("新建项目")
                .open(&mut open)
                .default_size([420.0, 260.0])
                .frame(crate::ui::theme::dialog_frame())
                .show(ctx, |ui| {
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
                                self.last_auto_save_at = None;
                                self.auto_save_error_reported = false;
                                self.crash_recovery_candidates =
                                    discover_crash_recovery_candidates();
                            }
                        }
                    }
                });
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
                .default_size([520.0, 220.0])
                .frame(crate::ui::theme::dialog_frame())
                .show(ctx, |ui| {
                    ui.label(format!(
                        "开始前需要先打开一个项目文件，或新建一个项目。\n项目后缀：.{}",
                        PROJECT_EXTENSION
                    ));
                    ui.add_space(8.0);
                    let mut recover_index: Option<usize> = None;

                    ui.horizontal(|ui| {
                        if ui.button("打开项目...").clicked() {
                            self.open_project_dialog();
                        }
                        if ui.button("新建项目...").clicked() {
                            self.show_new_project_dialog = true;
                        }
                        if ui.button("退出").clicked() {
                            self.request_quit_app(ui.ctx());
                        }
                    });

                    if !self.crash_recovery_candidates.is_empty() {
                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                        ui.label("检测到可恢复的自动保存：");

                        let max_items = 3usize;
                        for (idx, candidate) in
                            self.crash_recovery_candidates.iter().take(max_items).enumerate()
                        {
                            let project_name = candidate
                                .project_file
                                .file_name()
                                .and_then(|v| v.to_str())
                                .unwrap_or("未知项目");
                            let age_secs = ((unix_now_ms()
                                .saturating_sub(candidate.saved_at_unix_ms))
                                / 1000) as u64;
                            let age_label = if age_secs < 60 {
                                format!("{age_secs}s 前")
                            } else if age_secs < 3600 {
                                format!("{}m 前", age_secs / 60)
                            } else {
                                format!("{}h 前", age_secs / 3600)
                            };
                            let label = format!("恢复 {project_name}（{age_label}）");
                            let clicked = ui
                                .button(label)
                                .on_hover_text(candidate.autosave_file.display().to_string())
                                .clicked();
                            if clicked {
                                recover_index = Some(idx);
                            }
                            if candidate.total_snapshots > 1 {
                                ui.small(format!("该项目可恢复点：{}", candidate.total_snapshots));
                            }
                        }

                        if self.crash_recovery_candidates.len() > max_items {
                            ui.label(format!(
                                "还有 {} 个恢复点可用",
                                self.crash_recovery_candidates.len() - max_items
                            ));
                        }
                    }

                    if let Some(idx) = recover_index {
                        self.recover_project_from_candidate(idx);
                    }
                });
        }

        if self.show_preferences_dialog {
            self.capture_shortcut_input(ctx);
            self.draw_preferences_window(ctx);
        }

        self.draw_pending_close_action_dialog(ctx);

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

    fn run_project_autosave_if_needed(&mut self) {
        preferences::run_project_autosave_if_needed(self);
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

    fn handle_viewport_close_requested(&mut self, ctx: &egui::Context) {
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if !close_requested {
            return;
        }

        if self.allow_next_viewport_close {
            self.allow_next_viewport_close = false;
            return;
        }

        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        self.request_quit_app(ctx);
    }

    fn close_project_and_refresh_state(&mut self) {
        self.state.close_project();
        self.last_auto_save_at = None;
        self.auto_save_error_reported = false;
        self.crash_recovery_candidates = discover_crash_recovery_candidates();
    }

    fn has_unsaved_project_changes(&self) -> bool {
        if !self.state.has_open_project() {
            return false;
        }

        let Some(current) = self.state.current_project_data() else {
            return false;
        };

        let current_fingerprint = match project_data_fingerprint(current) {
            Ok(data) => data,
            Err(err) => {
                tracing::warn!("计算当前项目指纹失败，按未保存处理: {err}");
                return true;
            }
        };

        let project_file = match self.state.project_file_path() {
            Ok(path) => path,
            Err(err) => {
                tracing::warn!("读取当前项目路径失败，按未保存处理: {err}");
                return true;
            }
        };

        let saved = match AppState::read_project_data_from_archive(project_file) {
            Ok(data) => data,
            Err(err) => {
                tracing::warn!("读取磁盘项目数据失败，按未保存处理: {err}");
                return true;
            }
        };

        let saved_fingerprint = match project_data_fingerprint(saved) {
            Ok(data) => data,
            Err(err) => {
                tracing::warn!("计算磁盘项目指纹失败，按未保存处理: {err}");
                return true;
            }
        };

        current_fingerprint != saved_fingerprint
    }

    fn request_close_project(&mut self) {
        if !self.state.has_open_project() {
            return;
        }

        if self.has_unsaved_project_changes() {
            self.pending_close_action = Some(PendingCloseAction::CloseProject);
        } else {
            self.close_project_and_refresh_state();
        }
    }

    fn request_quit_app(&mut self, ctx: &egui::Context) {
        if self.state.has_open_project() {
            if self.has_unsaved_project_changes() {
                self.pending_close_action = Some(PendingCloseAction::QuitApp);
                return;
            }

            self.close_project_and_refresh_state();
        }

        self.allow_next_viewport_close = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn execute_pending_close_action(&mut self, ctx: &egui::Context) {
        let Some(action) = self.pending_close_action.take() else {
            return;
        };

        match action {
            PendingCloseAction::CloseProject => {
                self.close_project_and_refresh_state();
            }
            PendingCloseAction::QuitApp => {
                self.close_project_and_refresh_state();
                self.allow_next_viewport_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn draw_pending_close_action_dialog(&mut self, ctx: &egui::Context) {
        let Some(action) = self.pending_close_action else {
            return;
        };

        let mut keep_open = true;
        egui::Window::new("关闭前保存项目")
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .default_size([460.0, 160.0])
            .open(&mut keep_open)
            .frame(crate::ui::theme::dialog_frame())
            .show(ctx, |ui| {
                let action_text = match action {
                    PendingCloseAction::CloseProject => "关闭项目",
                    PendingCloseAction::QuitApp => "退出应用",
                };
                ui.label(format!("正在{action_text}，是否先保存当前项目？"));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("保存并继续").clicked() {
                        if let Err(err) = self.state.save_project() {
                            tracing::error!("关闭前保存失败: {err}");
                            self.state.set_status_hint(format!("保存项目失败：{err}"), true);
                        } else {
                            self.execute_pending_close_action(ctx);
                        }
                    }

                    if ui.button("不保存").clicked() {
                        self.execute_pending_close_action(ctx);
                    }

                    if ui.button("取消").clicked() {
                        self.pending_close_action = None;
                    }
                });
            });

        if !keep_open {
            self.pending_close_action = None;
        }
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
                self.last_auto_save_at = None;
                self.auto_save_error_reported = false;
                self.crash_recovery_candidates = discover_crash_recovery_candidates();
                self.state.set_status_hint("项目已打开", false);
            }
            Err(err) => {
                self.state.set_status_hint(format!("打开项目失败：{err}"), true);
                tracing::error!("打开项目失败: {err}");
            }
        }
    }

    fn recover_project_from_candidate(&mut self, index: usize) {
        let Some(candidate) = self.crash_recovery_candidates.get(index).cloned() else {
            return;
        };

        match self.state.open_project_from_autosave_snapshot(
            candidate.project_file.clone(),
            candidate.autosave_file.clone(),
        ) {
            Ok(()) => {
                self.show_project_bootstrap_dialog = false;
                self.show_library = true;
                self.last_auto_save_at = None;
                self.auto_save_error_reported = false;
                self.crash_recovery_candidates = discover_crash_recovery_candidates();
                self.state.set_status_hint(
                    format!("已从自动保存恢复：{}", candidate.project_file.display()),
                    false,
                );
            }
            Err(err) => {
                self.state.set_status_hint(format!("恢复自动保存失败：{err}"), true);
                tracing::error!("恢复自动保存失败: {err}");
                self.crash_recovery_candidates = discover_crash_recovery_candidates();
            }
        }
    }

    fn draw_menu_bar(&mut self, ui: &mut egui::Ui) {
        ui.ctx().style_mut(|style| {
            style.spacing.menu_width = Self::MENU_POPUP_WIDTH;
        });
        egui::menu::bar(ui, |ui| {
            ui.menu_button("文件", |ui| {
                ui.set_min_width(Self::MENU_POPUP_MIN_WIDTH);

                if Self::menu_action(ui, "新建项目...", None).clicked() {
                    self.show_new_project_dialog = true;
                    ui.close_menu();
                }
                if Self::menu_action(
                    ui,
                    "打开项目...",
                    Some(self.shortcuts.open_project_label().as_str()),
                )
                .clicked()
                {
                    self.open_project_dialog();
                    ui.close_menu();
                }
                if Self::menu_action(
                    ui,
                    "保存",
                    Some(self.shortcuts.save_project_label().as_str()),
                )
                .clicked()
                {
                    if let Err(err) = self.state.save_project() {
                        tracing::error!("保存项目失败: {err}");
                        self.state.set_status_hint(format!("保存项目失败：{err}"), true);
                    } else {
                        self.state.set_status_hint("项目已保存", false);
                    }
                    ui.close_menu();
                }
                if Self::menu_action(
                    ui,
                    "另存为...",
                    Some(self.shortcuts.save_project_as_label().as_str()),
                )
                .clicked()
                {
                    self.save_project_as_dialog();
                    ui.close_menu();
                }
                if Self::menu_action(
                    ui,
                    "关闭项目",
                    Some(self.shortcuts.close_project_label().as_str()),
                )
                .clicked()
                {
                    self.request_close_project();
                    ui.close_menu();
                }
                ui.separator();
                if Self::menu_action(
                    ui,
                    "导入媒体",
                    Some(self.shortcuts.import_media_label().as_str()),
                )
                .clicked()
                {
                    self.trigger_import_media();
                    ui.close_menu();
                }
                ui.separator();
                if Self::menu_action(ui, "退出", Some(self.shortcuts.quit_app_label().as_str()))
                    .clicked()
                {
                    self.request_quit_app(ui.ctx());
                }
            });

            ui.menu_button("编辑", |ui| {
                ui.set_min_width(Self::MENU_POPUP_MIN_WIDTH);
                let can_undo = self.state.cmd_history.can_undo();
                let can_redo = self.state.cmd_history.can_redo();

                if Self::menu_action_enabled(ui, "撤销", Some("Ctrl+Z"), can_undo).clicked() {
                    if let Err(err) = self.state.undo_timeline() {
                        self.state.set_status_hint(format!("撤销失败：{err}"), true);
                    }
                    ui.close_menu();
                }
                if Self::menu_action_enabled(ui, "重做", Some("Ctrl+Shift+Z"), can_redo).clicked()
                {
                    if let Err(err) = self.state.redo_timeline() {
                        self.state.set_status_hint(format!("重做失败：{err}"), true);
                    }
                    ui.close_menu();
                }

                ui.separator();
                if Self::menu_action(ui, "首选项...", None).clicked() {
                    self.show_preferences_dialog = true;
                    ui.close_menu();
                }
            });

            ui.menu_button("视图", |ui| {
                ui.set_min_width(Self::MENU_POPUP_MIN_WIDTH);
                let _ =
                    crate::ui::theme::checkmark_menu_toggle(ui, &mut self.show_library, "素材库");
                if cfg!(debug_assertions) {
                    let _ = crate::ui::theme::checkmark_menu_toggle(
                        ui,
                        &mut self.show_dev_metrics,
                        "开发指标",
                    );
                }
            });

            ui.menu_button("导出", |ui| {
                ui.set_min_width(Self::MENU_POPUP_MIN_WIDTH);
                if Self::menu_action(ui, "导出视频…", None).clicked() {
                    self.show_export = true;
                    ui.close_menu();
                }
            });

            ui.menu_button("帮助", |ui| {
                ui.set_min_width(Self::MENU_POPUP_MIN_WIDTH);
                let _ = Self::menu_action(ui, "关于Mondrian", None);
            });
        });
    }

    fn menu_action(ui: &mut egui::Ui, label: &str, shortcut: Option<&str>) -> egui::Response {
        Self::menu_action_enabled(ui, label, shortcut, true)
    }

    fn menu_action_enabled(
        ui: &mut egui::Ui,
        label: &str,
        shortcut: Option<&str>,
        enabled: bool,
    ) -> egui::Response {
        let desired_size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
        let sense = if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        };
        let (rect, response) = ui.allocate_exact_size(desired_size, sense);

        if ui.is_rect_visible(rect) {
            let visuals = ui.visuals();
            let fill = if enabled && response.hovered() {
                visuals.widgets.hovered.weak_bg_fill
            } else {
                egui::Color32::TRANSPARENT
            };
            let rounding = visuals.menu_rounding;
            ui.painter().rect_filled(rect, rounding, fill);

            let label_color = if enabled {
                crate::ui::theme::palette::text_primary()
            } else {
                crate::ui::theme::palette::text_muted()
            };
            let shortcut_color = crate::ui::theme::palette::text_muted().gamma_multiply(0.78);

            ui.painter().text(
                rect.left_center() + egui::vec2(10.0, 0.0),
                egui::Align2::LEFT_CENTER,
                label,
                crate::ui::theme::typography::body_small(),
                label_color,
            );

            if let Some(shortcut) = shortcut {
                ui.painter().text(
                    rect.right_center() - egui::vec2(10.0, 0.0),
                    egui::Align2::RIGHT_CENTER,
                    shortcut,
                    crate::ui::theme::typography::body_small(),
                    shortcut_color,
                );
            }
        }

        response
    }

    fn draw_status_bar(&self, ui: &mut egui::Ui) {
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), ui.available_height()),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
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
            },
        );
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

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_vec_pretty(value)?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn project_data_fingerprint(mut data: ProjectFile) -> anyhow::Result<Vec<u8>> {
    data.proxy_mode_assets.sort_by_key(|id| id.to_string());
    Ok(serde_json::to_vec(&data)?)
}

fn unix_now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as u64,
        Err(_) => 0,
    }
}

fn apply_autosave_retention(
    manifest: &mut AutosaveManifest,
    max_recovery_points: usize,
    retention_days: u32,
) {
    manifest.normalize_legacy_fields();
    let now_ms = unix_now_ms();
    let retention_ms = (retention_days as u64)
        .saturating_mul(24)
        .saturating_mul(60)
        .saturating_mul(60)
        .saturating_mul(1000);
    let cutoff_ms = now_ms.saturating_sub(retention_ms);

    let mut dropped_files: Vec<PathBuf> = Vec::new();
    let mut retained = Vec::<AutosaveSnapshotEntry>::new();
    for snapshot in &manifest.snapshots {
        if snapshot.saved_at_unix_ms < cutoff_ms {
            dropped_files.push(snapshot.file.clone());
        } else {
            retained.push(snapshot.clone());
        }
    }

    retained.sort_by_key(|s| std::cmp::Reverse(s.saved_at_unix_ms));
    if retained.len() > max_recovery_points {
        for snapshot in retained.drain(max_recovery_points..) {
            dropped_files.push(snapshot.file);
        }
    }

    for file in dropped_files {
        let _ = fs::remove_file(file);
    }

    manifest.snapshots = retained;
    manifest.normalize_legacy_fields();
}

fn discover_crash_recovery_candidates() -> Vec<CrashRecoveryCandidate> {
    let root = std::env::temp_dir().join("mondrian-runtime");
    let mut candidates = Vec::<CrashRecoveryCandidate>::new();

    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return candidates,
    };

    for entry in entries.flatten() {
        let runtime_root = entry.path();
        let manifest_path = AppState::autosave_manifest_path(runtime_root.as_path());
        if !manifest_path.exists() {
            continue;
        }

        let bytes = match fs::read(&manifest_path) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let mut manifest = match serde_json::from_slice::<AutosaveManifest>(&bytes) {
            Ok(v) => v,
            Err(_) => continue,
        };
        manifest.normalize_legacy_fields();
        let total = manifest.snapshots.len();
        if total == 0 {
            continue;
        }

        for snapshot in manifest.snapshots {
            candidates.push(CrashRecoveryCandidate {
                project_file: manifest.project_file.clone(),
                autosave_file: snapshot.file,
                saved_at_unix_ms: snapshot.saved_at_unix_ms,
                total_snapshots: total,
            });
        }
    }

    candidates.sort_by_key(|m| std::cmp::Reverse(m.saved_at_unix_ms));
    candidates
}

fn clear_all_crash_recovery_points() -> anyhow::Result<usize> {
    let root = std::env::temp_dir().join("mondrian-runtime");
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return Ok(0),
    };

    let mut removed_files = 0usize;
    for entry in entries {
        let entry = match entry {
            Ok(v) => v,
            Err(_) => continue,
        };
        let autosave_dir = entry.path().join("autosave");
        if !autosave_dir.exists() {
            continue;
        }

        if let Ok(files) = fs::read_dir(&autosave_dir) {
            removed_files += files.filter_map(Result::ok).count();
        }
        let _ = fs::remove_dir_all(&autosave_dir);
    }

    Ok(removed_files)
}

fn collect_files_by_name(
    root: &Path,
    index: &mut HashMap<String, Vec<PathBuf>>,
) -> mondrian_core::Result<()> {
    if !root.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();

        if file_type.is_dir() {
            collect_files_by_name(path.as_path(), index)?;
            continue;
        }

        if !file_type.is_file() {
            continue;
        }

        if let Some(name) = path.file_name().and_then(|v| v.to_str()) {
            index.entry(name.to_ascii_lowercase()).or_default().push(path);
        }
    }

    Ok(())
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

    fn video_clip_is_disabled(state: &AppState, clip_id: ClipId) -> bool {
        state
            .sequence
            .as_ref()
            .and_then(|seq| seq.video_tracks[0].clips.iter().find(|clip| clip.id == clip_id))
            .map(|clip| clip.is_disabled)
            .unwrap_or(false)
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

    #[test]
    fn set_clip_disabled_is_undoable() {
        let mut state = create_state_with_sequence();
        let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
        let track_id = state.sequence.as_ref().expect("sequence should exist").video_tracks[0].id;

        let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(30, tb));
        let clip_id = clip.id;
        state.sequence.as_mut().expect("sequence should exist").video_tracks[0]
            .add_clip(clip)
            .expect("add clip");

        state
            .set_clips_disabled_bulk(&[(track_id, true, clip_id)], true)
            .expect("disable clip should succeed");
        assert!(video_clip_is_disabled(&state, clip_id));
        assert_eq!(state.cmd_history.undo_description(), Some("禁用片段"));

        assert!(state.undo_timeline().expect("undo should succeed"));
        assert!(!video_clip_is_disabled(&state, clip_id));

        assert!(state.redo_timeline().expect("redo should succeed"));
        assert!(video_clip_is_disabled(&state, clip_id));
    }

    #[test]
    fn move_clip_conflict_respects_insert_mode() {
        let mut state = create_state_with_sequence();
        let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
        let track_id = state.sequence.as_ref().expect("sequence should exist").video_tracks[0].id;

        let clip_a = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
        let clip_b = Clip::new(AssetId::new(), TimeCode::new(20, tb), TimeCode::new(10, tb));
        let clip_b_id = clip_b.id;

        {
            let seq = state.sequence.as_mut().expect("sequence should exist");
            seq.video_tracks[0].add_clip(clip_a).expect("add clip a");
            seq.video_tracks[0].add_clip(clip_b).expect("add clip b");
        }

        state
            .move_clip_in_track_with_mode(track_id, true, clip_b_id, 5, ClipOverlapMode::Insert)
            .expect("move should succeed");

        let clips = &state.sequence.as_ref().expect("sequence should exist").video_tracks[0].clips;
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0].position.frame, 0);
        assert_eq!(clips[1].id, clip_b_id);
        assert_eq!(clips[1].position.frame, 10);
    }

    #[test]
    fn move_clip_conflict_respects_overwrite_mode() {
        let mut state = create_state_with_sequence();
        let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
        let track_id = state.sequence.as_ref().expect("sequence should exist").video_tracks[0].id;

        let clip_a = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
        let clip_a_id = clip_a.id;
        let clip_b = Clip::new(AssetId::new(), TimeCode::new(20, tb), TimeCode::new(10, tb));
        let clip_b_id = clip_b.id;

        {
            let seq = state.sequence.as_mut().expect("sequence should exist");
            seq.video_tracks[0].add_clip(clip_a).expect("add clip a");
            seq.video_tracks[0].add_clip(clip_b).expect("add clip b");
        }

        state
            .move_clip_in_track_with_mode(track_id, true, clip_b_id, 5, ClipOverlapMode::Overwrite)
            .expect("move should succeed");

        let clips = &state.sequence.as_ref().expect("sequence should exist").video_tracks[0].clips;
        assert_eq!(clips.len(), 2);
        let kept_a = clips.iter().find(|clip| clip.id == clip_a_id).expect("clip a should exist");
        let moved_b = clips.iter().find(|clip| clip.id == clip_b_id).expect("clip b should exist");
        assert_eq!(kept_a.position.frame, 0);
        assert_eq!(kept_a.duration.frame, 5);
        assert_eq!(moved_b.position.frame, 5);
    }

    #[test]
    fn overwrite_only_removes_intersection_and_keeps_both_sides() {
        let mut state = create_state_with_sequence();
        let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
        let track_id = state.sequence.as_ref().expect("sequence should exist").video_tracks[0].id;

        let clip_a = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
        let clip_a_id = clip_a.id;
        let clip_b = Clip::new(AssetId::new(), TimeCode::new(40, tb), TimeCode::new(4, tb));
        let clip_b_id = clip_b.id;

        {
            let seq = state.sequence.as_mut().expect("sequence should exist");
            seq.video_tracks[0].add_clip(clip_a).expect("add clip a");
            seq.video_tracks[0].add_clip(clip_b).expect("add clip b");
        }

        state
            .move_clip_in_track_with_mode(track_id, true, clip_b_id, 8, ClipOverlapMode::Overwrite)
            .expect("move should succeed");

        let clips = &state.sequence.as_ref().expect("sequence should exist").video_tracks[0].clips;
        assert_eq!(clips.len(), 3);

        let left = clips.iter().find(|clip| clip.id == clip_a_id).expect("left part should exist");
        let moved =
            clips.iter().find(|clip| clip.id == clip_b_id).expect("moved clip should exist");
        let right = clips
            .iter()
            .find(|clip| clip.id != clip_a_id && clip.id != clip_b_id)
            .expect("right part should exist");

        assert_eq!(left.position.frame, 0);
        assert_eq!(left.duration.frame, 8);
        assert_eq!(left.source_in.frame, 0);
        assert_eq!(left.source_out.frame, 8);

        assert_eq!(moved.position.frame, 8);
        assert_eq!(moved.duration.frame, 4);

        assert_eq!(right.position.frame, 12);
        assert_eq!(right.duration.frame, 8);
        assert_eq!(right.source_in.frame, 12);
        assert_eq!(right.source_out.frame, 20);
    }

    #[test]
    fn move_clip_group_overwrite_keeps_all_selected_clips() {
        let mut state = create_state_with_sequence();
        let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

        let clip_a = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
        let clip_a_id = clip_a.id;
        let clip_b = Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(10, tb));
        let clip_b_id = clip_b.id;
        let clip_c = Clip::new(AssetId::new(), TimeCode::new(40, tb), TimeCode::new(10, tb));
        let clip_c_id = clip_c.id;

        {
            let seq = state.sequence.as_mut().expect("sequence should exist");
            seq.video_tracks[0].add_clip(clip_a).expect("add clip a");
            seq.video_tracks[0].add_clip(clip_b).expect("add clip b");
            seq.video_tracks[0].add_clip(clip_c).expect("add clip c");
        }

        state
            .move_clip_group_by_delta_with_mode(
                &[(clip_a_id, 0), (clip_b_id, 10)],
                5,
                ClipOverlapMode::Overwrite,
            )
            .expect("group move should succeed");

        let clips = &state.sequence.as_ref().expect("sequence should exist").video_tracks[0].clips;
        assert_eq!(clips.len(), 3);
        let moved_a = clips.iter().find(|clip| clip.id == clip_a_id).expect("clip a should exist");
        let moved_b = clips.iter().find(|clip| clip.id == clip_b_id).expect("clip b should exist");
        let untouched_c =
            clips.iter().find(|clip| clip.id == clip_c_id).expect("clip c should exist");
        assert_eq!(moved_a.position.frame, 5);
        assert_eq!(moved_b.position.frame, 15);
        assert_eq!(untouched_c.position.frame, 40);
    }

    #[test]
    fn trim_in_is_undoable() {
        let mut state = create_state_with_sequence();
        let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
        let clip = Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(20, tb));
        let clip_id = clip.id;

        state.sequence.as_mut().expect("sequence should exist").video_tracks[0]
            .add_clip(clip)
            .expect("add clip");

        let changed = state
            .trim_clips_bulk_to_frame(&[clip_id], TrimEdge::In, 15)
            .expect("trim in should succeed");
        assert_eq!(changed, 1);

        let trimmed = state.sequence.as_ref().expect("sequence should exist").video_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == clip_id)
            .expect("clip should exist");
        assert_eq!(trimmed.position.frame, 15);
        assert_eq!(trimmed.duration.frame, 15);
        assert_eq!(trimmed.source_in.frame, 5);
        assert_eq!(state.cmd_history.undo_description(), Some("修剪入点"));

        assert!(state.undo_timeline().expect("undo should succeed"));
        let restored = state.sequence.as_ref().expect("sequence should exist").video_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == clip_id)
            .expect("clip should exist after undo");
        assert_eq!(restored.position.frame, 10);
        assert_eq!(restored.duration.frame, 20);
        assert_eq!(restored.source_in.frame, 0);
    }

    #[test]
    fn trim_out_updates_linked_clip() {
        let mut state = create_state_with_sequence();
        let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

        let mut video = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(30, tb));
        let mut audio = Clip::new(video.asset_id, TimeCode::new(0, tb), TimeCode::new(30, tb));
        let video_id = video.id;
        let audio_id = audio.id;
        video.linked_clip = Some(audio_id);
        audio.linked_clip = Some(video_id);

        {
            let seq = state.sequence.as_mut().expect("sequence should exist");
            seq.video_tracks[0].add_clip(video).expect("add video");
            seq.audio_tracks[0].add_clip(audio).expect("add audio");
        }

        let changed = state
            .trim_clips_bulk_to_frame(&[video_id], TrimEdge::Out, 21)
            .expect("trim out should succeed");
        assert_eq!(changed, 2);

        let seq = state.sequence.as_ref().expect("sequence should exist");
        let video_after = seq.video_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == video_id)
            .expect("video should exist");
        let audio_after = seq.audio_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == audio_id)
            .expect("audio should exist");

        assert_eq!(video_after.duration.frame, 21);
        assert_eq!(audio_after.duration.frame, 21);
        assert_eq!(video_after.source_out.frame, 21);
        assert_eq!(audio_after.source_out.frame, 21);
    }

    #[test]
    fn removing_track_renumbers_tracks_and_clears_broken_links() {
        let mut state = create_state_with_sequence();
        let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

        let mut video = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
        let mut audio = Clip::new(video.asset_id, TimeCode::new(0, tb), TimeCode::new(20, tb));
        let video_id = video.id;
        let audio_id = audio.id;
        video.linked_clip = Some(audio_id);
        audio.linked_clip = Some(video_id);

        {
            let seq = state.sequence.as_mut().expect("sequence should exist");
            seq.video_tracks[1].add_clip(video).expect("add video");
            seq.audio_tracks[1].add_clip(audio).expect("add audio");
        }

        let removed_track_id =
            state.sequence.as_ref().expect("sequence should exist").video_tracks[1].id;
        state.remove_track(removed_track_id, true).expect("remove track");

        let seq = state.sequence.as_ref().expect("sequence should exist");
        assert_eq!(seq.video_tracks.len(), 2);
        assert_eq!(seq.video_tracks[0].name, "V1");
        assert_eq!(seq.video_tracks[1].name, "V2");

        let audio_after = seq.audio_tracks[1]
            .clips
            .iter()
            .find(|clip| clip.id == audio_id)
            .expect("audio clip should remain");
        assert_eq!(audio_after.linked_clip, None);
    }

    #[test]
    fn moving_track_is_undoable_and_preserves_clips() {
        let mut state = create_state_with_sequence();
        let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
        let moved_track_id =
            state.sequence.as_ref().expect("sequence should exist").video_tracks[2].id;
        let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
        let clip_id = clip.id;
        state.sequence.as_mut().expect("sequence should exist").video_tracks[2]
            .add_clip(clip)
            .expect("add clip");

        state.move_track(moved_track_id, true, 0).expect("move track");

        let seq = state.sequence.as_ref().expect("sequence should exist");
        assert_eq!(seq.video_tracks[0].id, moved_track_id);
        assert_eq!(seq.video_tracks[0].name, "V1");
        assert_eq!(seq.video_tracks[0].clips[0].id, clip_id);
        assert_eq!(seq.video_tracks[1].name, "V2");
        assert_eq!(seq.video_tracks[2].name, "V3");
        assert_eq!(state.cmd_history.undo_description(), Some("移动轨道"));

        assert!(state.undo_timeline().expect("undo should succeed"));
        let seq_undo = state.sequence.as_ref().expect("sequence should exist after undo");
        assert_eq!(seq_undo.video_tracks[2].id, moved_track_id);
        assert_eq!(seq_undo.video_tracks[2].clips[0].id, clip_id);
    }

    #[test]
    fn dropping_linked_clip_creates_missing_audio_track_at_target_index() {
        let mut state = create_state_with_sequence();
        let removed_audio_id =
            state.sequence.as_ref().expect("sequence should exist").audio_tracks[2].id;
        state
            .sequence
            .as_mut()
            .expect("sequence should exist")
            .remove_audio_track(removed_audio_id)
            .expect("remove third audio track");
        state.ensure_minimum_tracks();

        let target_track_id =
            state.sequence.as_ref().expect("sequence should exist").video_tracks[2].id;
        let asset_id = AssetId::new();
        state.begin_drag_asset(
            asset_id,
            "AV Clip".to_string(),
            AssetKind::Video,
            Duration::from_secs(2),
            true,
        );

        let video_clip_id = state
            .drop_dragging_asset_to_video_track(target_track_id, 0)
            .expect("drop linked clip");

        let seq = state.sequence.as_ref().expect("sequence should exist");
        assert_eq!(seq.audio_tracks.len(), 3);
        let video_clip = seq.video_tracks[2]
            .clips
            .iter()
            .find(|clip| clip.id == video_clip_id)
            .expect("video clip should exist");
        let audio_clip = seq.audio_tracks[2]
            .clips
            .iter()
            .find(|clip| clip.linked_clip == Some(video_clip_id))
            .expect("linked audio clip should exist");
        assert_eq!(video_clip.linked_clip, Some(audio_clip.id));
        assert_eq!(audio_clip.asset_id, asset_id);
    }

    #[test]
    fn dropping_linked_clip_after_video_reorder_uses_current_track_index() {
        let mut state = create_state_with_sequence();
        let moved_video_track_id =
            state.sequence.as_ref().expect("sequence should exist").video_tracks[2].id;
        state.move_track(moved_video_track_id, true, 0).expect("move track before drop");

        state.begin_drag_asset(
            AssetId::new(),
            "Moved Track AV".to_string(),
            AssetKind::Video,
            Duration::from_secs(1),
            true,
        );
        let video_clip_id = state
            .drop_dragging_asset_to_video_track(moved_video_track_id, 0)
            .expect("drop linked clip");

        let seq = state.sequence.as_ref().expect("sequence should exist");
        assert_eq!(seq.video_tracks[0].id, moved_video_track_id);
        assert!(seq.video_tracks[0].clips.iter().any(|clip| clip.id == video_clip_id));
        assert!(seq.audio_tracks[0]
            .clips
            .iter()
            .any(|clip| clip.linked_clip == Some(video_clip_id)));
    }

    #[test]
    fn moving_video_track_keeps_existing_linked_audio_on_its_audio_track() {
        let mut state = create_state_with_sequence();
        let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

        let mut video = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(15, tb));
        let mut audio = Clip::new(video.asset_id, TimeCode::new(0, tb), TimeCode::new(15, tb));
        let video_id = video.id;
        let audio_id = audio.id;
        video.linked_clip = Some(audio_id);
        audio.linked_clip = Some(video_id);

        {
            let seq = state.sequence.as_mut().expect("sequence should exist");
            seq.video_tracks[2].add_clip(video).expect("add video");
            seq.audio_tracks[2].add_clip(audio).expect("add audio");
        }

        let moved_video_track_id =
            state.sequence.as_ref().expect("sequence should exist").video_tracks[2].id;
        state.move_track(moved_video_track_id, true, 0).expect("move video track");

        let seq = state.sequence.as_ref().expect("sequence should exist");
        assert!(seq.video_tracks[0].clips.iter().any(|clip| clip.id == video_id));
        let audio_after = seq.audio_tracks[2]
            .clips
            .iter()
            .find(|clip| clip.id == audio_id)
            .expect("audio clip should stay on original audio track");
        assert_eq!(audio_after.linked_clip, Some(video_id));
    }

    #[test]
    fn roll_cut_to_frame_is_undoable() {
        let mut state = create_state_with_sequence();
        let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

        let clip_a = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
        let clip_a_id = clip_a.id;
        let clip_b = Clip::new(AssetId::new(), TimeCode::new(20, tb), TimeCode::new(20, tb));
        let clip_b_id = clip_b.id;

        {
            let seq = state.sequence.as_mut().expect("sequence should exist");
            seq.video_tracks[0].add_clip(clip_a).expect("add clip a");
            seq.video_tracks[0].add_clip(clip_b).expect("add clip b");
        }

        let changed = state.roll_cut_to_frame(clip_a_id, 25).expect("roll cut should succeed");
        assert!(changed);
        assert_eq!(state.cmd_history.undo_description(), Some("滚动修剪"));

        let seq = state.sequence.as_ref().expect("sequence should exist");
        let clip_a_after = seq.video_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == clip_a_id)
            .expect("clip a should exist");
        let clip_b_after = seq.video_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == clip_b_id)
            .expect("clip b should exist");

        assert_eq!(clip_a_after.duration.frame, 25);
        assert_eq!(clip_b_after.position.frame, 25);
        assert_eq!(clip_b_after.duration.frame, 15);
        assert_eq!(clip_b_after.source_in.frame, 5);

        assert!(state.undo_timeline().expect("undo should succeed"));
        let seq_undo = state.sequence.as_ref().expect("sequence should exist");
        let clip_a_undo = seq_undo.video_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == clip_a_id)
            .expect("clip a should exist after undo");
        let clip_b_undo = seq_undo.video_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == clip_b_id)
            .expect("clip b should exist after undo");
        assert_eq!(clip_a_undo.duration.frame, 20);
        assert_eq!(clip_b_undo.position.frame, 20);
        assert_eq!(clip_b_undo.source_in.frame, 0);
    }

    #[test]
    fn slip_clip_negative_delta_is_clamped_and_undoable() {
        let mut state = create_state_with_sequence();
        let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
        let library_root = std::env::temp_dir().join(format!(
            "mondrian_timeline_slip_test_{}_{}",
            std::process::id(),
            unix_now_ms()
        ));
        std::fs::create_dir_all(&library_root).expect("create temp library root");
        state.asset_library = Some(AssetLibrary::open(library_root.clone()).expect("open library"));

        let mut clip = Clip::new(AssetId::new(), TimeCode::new(8, tb), TimeCode::new(20, tb));
        clip.source_in = TimeCode::new(10, tb);
        clip.source_out = TimeCode::new(30, tb);
        let clip_id = clip.id;
        state.sequence.as_mut().expect("sequence should exist").video_tracks[0]
            .add_clip(clip)
            .expect("add clip");

        let changed =
            state.slip_clips_bulk_by_frames(&[clip_id], -15).expect("slip should succeed");
        assert_eq!(changed, 1);
        assert_eq!(state.cmd_history.undo_description(), Some("滑移片段"));

        let seq = state.sequence.as_ref().expect("sequence should exist");
        let slipped = seq.video_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == clip_id)
            .expect("clip should exist");
        assert_eq!(slipped.position.frame, 8);
        assert_eq!(slipped.duration.frame, 20);
        assert_eq!(slipped.source_in.frame, 0);
        assert_eq!(slipped.source_out.frame, 20);

        assert!(state.undo_timeline().expect("undo should succeed"));
        let restored = state.sequence.as_ref().expect("sequence should exist").video_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == clip_id)
            .expect("clip should exist after undo");
        assert_eq!(restored.source_in.frame, 10);
        assert_eq!(restored.source_out.frame, 30);

        state.asset_library = None;
        let _ = std::fs::remove_dir_all(&library_root);
    }

    #[test]
    fn slide_clip_updates_neighbors_and_is_undoable() {
        let mut state = create_state_with_sequence();
        let tb = state.sequence.as_ref().expect("sequence should exist").time_base();

        let left = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
        let center = Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(10, tb));
        let center_id = center.id;
        let right = Clip::new(AssetId::new(), TimeCode::new(20, tb), TimeCode::new(10, tb));

        {
            let seq = state.sequence.as_mut().expect("sequence should exist");
            seq.video_tracks[0].add_clip(left).expect("add left");
            seq.video_tracks[0].add_clip(center).expect("add center");
            seq.video_tracks[0].add_clip(right).expect("add right");
        }

        let changed =
            state.slide_clips_bulk_by_frames(&[center_id], 3).expect("slide should succeed");
        assert_eq!(changed, 1);
        assert_eq!(state.cmd_history.undo_description(), Some("滑动片段"));

        let seq = state.sequence.as_ref().expect("sequence should exist");
        let clips = &seq.video_tracks[0].clips;
        assert_eq!(clips.len(), 3);
        assert_eq!(clips[0].position.frame, 0);
        assert_eq!(clips[0].duration.frame, 13);
        assert_eq!(clips[1].id, center_id);
        assert_eq!(clips[1].position.frame, 13);
        assert_eq!(clips[1].duration.frame, 10);
        assert_eq!(clips[2].position.frame, 23);
        assert_eq!(clips[2].duration.frame, 7);
        assert_eq!(clips[2].source_in.frame, 3);

        assert!(state.undo_timeline().expect("undo should succeed"));
        let seq_undo = state.sequence.as_ref().expect("sequence should exist");
        let clips_undo = &seq_undo.video_tracks[0].clips;
        assert_eq!(clips_undo[0].duration.frame, 10);
        assert_eq!(clips_undo[1].position.frame, 10);
        assert_eq!(clips_undo[2].position.frame, 20);
        assert_eq!(clips_undo[2].source_in.frame, 0);
    }
}

#[cfg(test)]
mod autosave_tests {
    use super::*;

    #[test]
    fn autosave_retention_trims_by_count() {
        let root = std::env::temp_dir().join(format!(
            "mondrian_autosave_retention_count_{}_{}",
            std::process::id(),
            unix_now_ms()
        ));
        fs::create_dir_all(&root).expect("create temp root");

        let now = unix_now_ms();
        let f1 = root.join("s1.mdp");
        let f2 = root.join("s2.mdp");
        let f3 = root.join("s3.mdp");
        fs::write(&f1, b"a").expect("write f1");
        fs::write(&f2, b"b").expect("write f2");
        fs::write(&f3, b"c").expect("write f3");

        let mut manifest = AutosaveManifest {
            project_file: root.join("project.mdp"),
            snapshots: vec![
                AutosaveSnapshotEntry {
                    file: f1.clone(),
                    saved_at_unix_ms: now.saturating_sub(3),
                },
                AutosaveSnapshotEntry {
                    file: f2.clone(),
                    saved_at_unix_ms: now.saturating_sub(2),
                },
                AutosaveSnapshotEntry {
                    file: f3.clone(),
                    saved_at_unix_ms: now.saturating_sub(1),
                },
            ],
            autosave_file: None,
            saved_at_unix_ms: None,
        };

        apply_autosave_retention(&mut manifest, 2, 365);
        assert_eq!(manifest.snapshots.len(), 2);
        assert!(manifest.snapshots.iter().any(|s| s.file == f3));
        assert!(manifest.snapshots.iter().any(|s| s.file == f2));
        assert!(!f1.exists(), "oldest snapshot should be removed from disk");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn autosave_retention_trims_by_age() {
        let root = std::env::temp_dir().join(format!(
            "mondrian_autosave_retention_age_{}_{}",
            std::process::id(),
            unix_now_ms()
        ));
        fs::create_dir_all(&root).expect("create temp root");

        let now = unix_now_ms();
        let recent = root.join("recent.mdp");
        let old = root.join("old.mdp");
        fs::write(&recent, b"r").expect("write recent");
        fs::write(&old, b"o").expect("write old");

        let one_day_ms = 24_u64 * 60 * 60 * 1000;
        let mut manifest = AutosaveManifest {
            project_file: root.join("project.mdp"),
            snapshots: vec![
                AutosaveSnapshotEntry {
                    file: recent.clone(),
                    saved_at_unix_ms: now.saturating_sub(one_day_ms / 2),
                },
                AutosaveSnapshotEntry {
                    file: old.clone(),
                    saved_at_unix_ms: now.saturating_sub(one_day_ms * 3),
                },
            ],
            autosave_file: None,
            saved_at_unix_ms: None,
        };

        apply_autosave_retention(&mut manifest, 10, 1);
        assert_eq!(manifest.snapshots.len(), 1);
        assert_eq!(manifest.snapshots[0].file, recent);
        assert!(
            !old.exists(),
            "expired snapshot should be removed from disk"
        );

        let _ = fs::remove_dir_all(&root);
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
