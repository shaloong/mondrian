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
    automation::{
        interpolation_mode_from_keyframe, timecode_to_ticks, InterpolationType, Keyframe,
        PropertyHost, PropertyMutation, PropertyValue, TimeTicks,
    },
    events::{AppEvent, EventBus},
    types::{
        AssetId, ClipId, EffectId, KeyframeId, Rational, Resolution, SequenceId, TimeCode, TrackId,
    },
};
use mondrian_effects::EffectType;
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
    effect_library_panel::EffectLibraryPanel,
    export_panel::ExportPanel,
    library_panel::LibraryPanel,
    startup::{BootstrapAction, BootstrapRecentProjectItem, BootstrapRecoveryItem},
    timeline_panel::{SelectedClipRef, TimelinePanel},
    viewer_panel::{MediaCacheCleanupStats, ViewerPanel, ViewerPreferences},
};

const PROJECT_EXTENSION: &str = "mdp";
const DEFAULT_ADJUSTMENT_LAYER_DURATION_SECS: f64 = 5.0;

mod animation_state;
mod audio_rendering;
mod bootstrap;
mod chrome;
mod new_project;
mod playback;
mod preferences;
mod project_lifecycle;
mod timeline_commands;
mod timeline_editing;

use audio_rendering::*;
use timeline_editing::*;

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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AnimationPropertySelection {
    pub clip_id: ClipId,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AnimationKeyframeSelection {
    pub clip_id: ClipId,
    pub path: String,
    pub time: mondrian_core::automation::TimeTicks,
}

#[derive(Debug, Clone, Default)]
pub struct AnimationSelectionState {
    pub active_property: Option<AnimationPropertySelection>,
    pub remembered_active_properties: HashMap<ClipId, String>,
    pub selected_keyframes: HashSet<AnimationKeyframeSelection>,
    pub bubble_host: Option<AnimationBubbleHost>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnimationBubbleHost {
    Timeline,
    Graph,
}

#[derive(Debug, Clone)]
pub struct AnimationClipboardEntry {
    pub path: String,
    pub relative_time: TimeTicks,
    pub keyframe: Keyframe<PropertyValue>,
}

#[derive(Debug, Clone, Default)]
pub struct AnimationClipboard {
    pub entries: Vec<AnimationClipboardEntry>,
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
    #[serde(default = "default_show_effect_controls")]
    show_effect_controls: bool,
    #[serde(default = "default_show_effect_library")]
    show_effect_library: bool,
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
    #[serde(default = "default_timeline_panel_height")]
    timeline_panel_height: f32,
    #[serde(default = "default_auto_save_enabled")]
    auto_save_enabled: bool,
    #[serde(default = "default_auto_save_interval_secs")]
    auto_save_interval_secs: u32,
    #[serde(default = "default_auto_save_max_recovery_points")]
    auto_save_max_recovery_points: u32,
    #[serde(default = "default_auto_save_retention_days")]
    auto_save_retention_days: u32,
    #[serde(default)]
    recent_projects: Vec<PathBuf>,
    viewer: ViewerPreferences,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            version: 1,
            theme: default_app_theme(),
            show_effect_controls: default_show_effect_controls(),
            show_effect_library: default_show_effect_library(),
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
            timeline_panel_height: default_timeline_panel_height(),
            auto_save_enabled: default_auto_save_enabled(),
            auto_save_interval_secs: default_auto_save_interval_secs(),
            auto_save_max_recovery_points: default_auto_save_max_recovery_points(),
            auto_save_retention_days: default_auto_save_retention_days(),
            recent_projects: Vec::new(),
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

const fn default_show_effect_controls() -> bool {
    true
}

const fn default_show_effect_library() -> bool {
    true
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

const fn default_timeline_panel_height() -> f32 {
    286.0
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

    // 动画选择状态（timeline / inspector / future graph 共用）
    pub animation_selection: AnimationSelectionState,
    pub animation_clipboard: Option<AnimationClipboard>,

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
            animation_selection: AnimationSelectionState::default(),
            animation_clipboard: None,
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
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

pub struct MondrianApp {
    state: AppState,

    // UI 面板
    timeline_panel: TimelinePanel,
    effect_library_panel: EffectLibraryPanel,
    effect_controls_panel: EffectControlsPanel,
    viewer_panel: ViewerPanel,
    library_panel: LibraryPanel,
    export_panel: ExportPanel,

    // 面板可见性
    show_effect_controls: bool,
    show_effect_library: bool,
    show_library: bool,
    show_export: bool,
    show_dev_metrics: bool,
    show_preferences_dialog: bool,
    theme: crate::ui::theme::Theme,
    preferences_tab: PreferencesTab,
    capturing_shortcut: Option<ShortcutAction>,
    show_new_project_dialog: bool,
    show_project_bootstrap_dialog: bool,
    startup_viewport_mode: bool,
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
    recent_projects: Vec<PathBuf>,
    crash_recovery_candidates: Vec<CrashRecoveryCandidate>,
    show_video_metrics: bool,
    show_audio_metrics: bool,
    timeline_panel_height: f32,
    timeline_resize_drag: Option<(f32, f32)>,
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
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let state = AppState::new();

        let mut app = Self {
            state,
            timeline_panel: TimelinePanel::default(),
            effect_library_panel: EffectLibraryPanel::default(),
            effect_controls_panel: EffectControlsPanel::default(),
            viewer_panel: ViewerPanel::default(),
            library_panel: LibraryPanel::default(),
            export_panel: ExportPanel::default(),
            show_effect_controls: true,
            show_effect_library: true,
            show_library: true,
            show_export: false,
            show_dev_metrics: false,
            show_preferences_dialog: false,
            theme: default_app_theme(),
            preferences_tab: PreferencesTab::default(),
            capturing_shortcut: None,
            show_new_project_dialog: false,
            show_project_bootstrap_dialog: true,
            startup_viewport_mode: false,
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
            recent_projects: Vec::new(),
            crash_recovery_candidates: discover_crash_recovery_candidates(),
            show_video_metrics: default_show_video_metrics(),
            show_audio_metrics: default_show_audio_metrics(),
            timeline_panel_height: default_timeline_panel_height(),
            timeline_resize_drag: None,
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

        // 在首帧前就切到启动窗口模式，避免 clear_color 首帧走到不透明分支。
        let startup_mode = !app.state.has_open_project();
        app.sync_startup_viewport_mode(&cc.egui_ctx, startup_mode);

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

        if !self.state.has_open_project() {
            self.show_project_bootstrap_dialog = true;
        }

        if self.show_new_project_dialog {
            new_project::draw_new_project_window(self, ctx);
        }

        self.sync_startup_viewport_mode(ctx, !self.state.has_open_project());

        if self.show_project_bootstrap_dialog {
            let recovery_items: Vec<BootstrapRecoveryItem> = self
                .crash_recovery_candidates
                .iter()
                .map(|candidate| BootstrapRecoveryItem {
                    project_name: candidate
                        .project_file
                        .file_name()
                        .and_then(|v| v.to_str())
                        .unwrap_or("未知项目")
                        .to_string(),
                    autosave_path: candidate.autosave_file.display().to_string(),
                    age_label: Self::bootstrap_recovery_age_label(candidate.saved_at_unix_ms),
                    total_snapshots: candidate.total_snapshots,
                })
                .collect();

            let recent_items = self
                .recent_projects
                .iter()
                .map(|project_path| {
                    let (last_edited_label, project_size_label) =
                        Self::bootstrap_recent_project_meta(project_path.as_path());
                    BootstrapRecentProjectItem {
                        project_name: project_path
                            .file_stem()
                            .or_else(|| project_path.file_name())
                            .and_then(|v| v.to_str())
                            .unwrap_or("未知项目")
                            .to_string(),
                        project_path: project_path.display().to_string(),
                        last_edited_label,
                        project_size_label,
                    }
                })
                .collect::<Vec<_>>();

            if let Some(action) = crate::ui::startup::show_project_bootstrap_window(
                ctx,
                PROJECT_EXTENSION,
                &recent_items,
                &recovery_items,
            ) {
                match action {
                    BootstrapAction::OpenProject => self.open_project_dialog(),
                    BootstrapAction::OpenRecent(path) => match self.open_project_by_path(path) {
                        Ok(()) => self.state.set_status_hint("项目已打开", false),
                        Err(err) => {
                            self.state.set_status_hint(format!("打开项目失败：{err}"), true);
                            tracing::error!("打开项目失败: {err}");
                        }
                    },
                    BootstrapAction::NewProject => self.show_new_project_dialog = true,
                    BootstrapAction::Quit => self.request_quit_app(ctx),
                    BootstrapAction::Recover(idx) => self.recover_project_from_candidate(idx),
                }
            }

            if !self.state.has_open_project() {
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

                return;
            }
        }

        self.sync_startup_viewport_mode(ctx, false);

        // ── 顶部菜单栏 ──
        let top_menu_started_at = std::time::Instant::now();
        egui::TopBottomPanel::top("top_menu")
            .frame(
                egui::Frame::new()
                    .fill(crate::ui::theme::palette::bg_surface())
                    .stroke(egui::Stroke::new(
                        1.0,
                        crate::ui::theme::palette::panel_divider_strong(),
                    ))
                    .inner_margin(egui::Margin::symmetric(10, 6)),
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
                egui::Frame::new()
                    .fill(crate::ui::theme::palette::bg_surface())
                    .stroke(egui::Stroke::new(
                        1.0,
                        crate::ui::theme::palette::panel_divider_strong(),
                    ))
                    .inner_margin(egui::Margin::symmetric(10, 0)),
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
            .exact_height(self.timeline_panel_height)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(crate::ui::theme::palette::bg_base())
                    .stroke(egui::Stroke::new(
                        1.0,
                        crate::ui::theme::palette::panel_divider_strong(),
                    ))
                    .inner_margin(egui::Margin { left: 12, right: 12, top: 0, bottom: 8 }),
            )
            .show(ctx, |ui| {
                let (_resize_rect, resize_response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 8.0),
                    egui::Sense::click_and_drag(),
                );
                resize_response.clone().on_hover_cursor(egui::CursorIcon::ResizeVertical);
                if resize_response.drag_started() {
                    if let Some(pointer) = resize_response.interact_pointer_pos() {
                        self.timeline_resize_drag = Some((pointer.y, self.timeline_panel_height));
                    }
                }
                if let Some((start_y, start_height)) = self.timeline_resize_drag {
                    if ctx.input(|i| i.pointer.primary_down()) {
                        if let Some(pointer) = ctx.input(|i| i.pointer.interact_pos()) {
                            let delta = start_y - pointer.y;
                            self.timeline_panel_height = (start_height + delta).clamp(160.0, 640.0);
                        }
                    } else {
                        self.timeline_resize_drag = None;
                    }
                }

                ui.add_space(4.0);
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
                .resizable(true)
                .frame(
                    egui::Frame::new()
                        .fill(crate::ui::theme::palette::bg_base())
                        .stroke(egui::Stroke::NONE)
                        .inner_margin(egui::Margin::symmetric(12, 8)),
                )
                .show(ctx, |ui| {
                    self.library_panel.show(ui, &mut self.state);
                });
            if ui_diag_enabled() {
                log_ui_stage_slow("library_panel", library_started_at.elapsed());
            }
        }

        let selected_clip_ref = self.timeline_panel.selected_clip_ref();
        if let Some(selection) = selected_clip_ref {
            if !ctx.wants_keyboard_input()
                && ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::C))
            {
                match self.state.copy_selected_animation_keyframes(selection) {
                    Ok(true) => self.state.set_status_hint("已复制关键帧", false),
                    Ok(false) => {}
                    Err(err) => {
                        self.state.set_status_hint(format!("复制关键帧失败：{err}"), true);
                    }
                }
            }

            if !ctx.wants_keyboard_input()
                && ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::V))
            {
                let destination_time =
                    self.state.current_time_code().map(timecode_to_ticks).unwrap_or(0);
                match self.state.paste_animation_keyframes(selection, destination_time) {
                    Ok(true) => self.state.set_status_hint("已粘贴关键帧", false),
                    Ok(false) => {}
                    Err(err) => {
                        self.state.set_status_hint(format!("粘贴关键帧失败：{err}"), true);
                    }
                }
            }
        }
        if self.show_effect_controls {
            let effect_controls_started_at = std::time::Instant::now();
            egui::SidePanel::right("effect_controls_panel")
                .default_width(crate::ui::theme::tokens::inspector_panel_width())
                .min_width(crate::ui::theme::tokens::inspector_panel_min_width())
                .resizable(true)
                .frame(
                    egui::Frame::new()
                        .fill(crate::ui::theme::palette::bg_base())
                        .stroke(egui::Stroke::NONE)
                        .inner_margin(egui::Margin::symmetric(12, 8)),
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
        if self.show_effect_library {
            let effect_library_started_at = std::time::Instant::now();
            egui::SidePanel::right("effect_library_panel")
                .default_width(252.0)
                .min_width(208.0)
                .resizable(true)
                .frame(
                    egui::Frame::new()
                        .fill(crate::ui::theme::palette::bg_base())
                        .stroke(egui::Stroke::NONE)
                        .inner_margin(egui::Margin::symmetric(12, 8)),
                )
                .show(ctx, |ui| {
                    self.effect_library_panel.show(ui, &mut self.state, selected_clip_ref);
                });
            if ui_diag_enabled() {
                log_ui_stage_slow("effect_library_panel", effect_library_started_at.elapsed());
            }
        }

        // ── 中央：预览窗口 ──
        let viewer_started_at = std::time::Instant::now();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(crate::ui::theme::palette::bg_base())
                    .inner_margin(egui::Margin::symmetric(12, 8)),
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

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        if self.startup_viewport_mode {
            return [0.0, 0.0, 0.0, 0.0];
        }

        visuals.window_fill().to_normalized_gamma_f32()
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
            .unwrap_or(true)
    })
}

fn audio_buffer_target_high_secs_playing() -> f64 {
    static TARGET: OnceLock<f64> = OnceLock::new();
    *TARGET.get_or_init(|| {
        std::env::var("MONDRIAN_AUDIO_BUFFER_HIGH_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| *v >= 0.20 && *v <= 2.0)
            .unwrap_or(0.46)
    })
}

fn audio_buffer_target_high_secs_buffering() -> f64 {
    static TARGET: OnceLock<f64> = OnceLock::new();
    *TARGET.get_or_init(|| {
        std::env::var("MONDRIAN_AUDIO_BUFFER_HIGH_SECS_BUFFERING")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| *v >= 0.30 && *v <= 3.0)
            .unwrap_or(0.90)
    })
}

fn audio_render_max_in_flight_playing() -> usize {
    static MAX: OnceLock<usize> = OnceLock::new();
    *MAX.get_or_init(|| {
        std::env::var("MONDRIAN_AUDIO_RENDER_MAX_IN_FLIGHT")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .map(|v| v.clamp(1, 24))
            .unwrap_or(8)
    })
}

fn audio_render_max_in_flight_buffering() -> usize {
    static MAX: OnceLock<usize> = OnceLock::new();
    *MAX.get_or_init(|| {
        std::env::var("MONDRIAN_AUDIO_RENDER_MAX_IN_FLIGHT_BUFFERING")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .map(|v| v.clamp(1, 32))
            .unwrap_or(12)
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
            output.set_muted(false);
            output.clear();
        }
        self.playback_last_tick = None;
        self.playback_subframe_accum = 0.0;
        self.playback_buffering_last_frame = false;
    }

    fn advance_playback_clock(&mut self) {
        if !self.state.is_playing() {
            if let Some(output) = &self.state.audio_output {
                output.set_muted(false);
            }
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
            if let Some(output) = &self.state.audio_output {
                output.set_muted(true);
            }
            self.state.pump_audio_output();

            self.playback_last_tick = None;
            self.playback_subframe_accum = 0.0;
            self.playback_buffering_last_frame = true;
            return;
        }

        if self.playback_buffering_last_frame {
            if let Some(output) = &self.state.audio_output {
                output.set_muted(false);
            }
            self.playback_last_tick = None;
            self.playback_subframe_accum = 0.0;
        }
        self.playback_buffering_last_frame = false;
        if let Some(output) = &self.state.audio_output {
            output.set_muted(false);
        }

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
mod animation_selection_tests;
#[cfg(test)]
mod autosave_tests;
#[cfg(test)]
mod perf_tests;
#[cfg(test)]
mod timeline_edit_tests;
