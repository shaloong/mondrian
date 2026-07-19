use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use std::{collections::hash_map::DefaultHasher, hash::Hash, hash::Hasher};
use std::{fs, path::Path, path::PathBuf};

use mondrian_assets::{AssetKind, AssetLibrary};
use mondrian_core::{
    automation::{
        interpolation_mode_from_keyframe, InterpolationType, Keyframe, PropertyHost,
        PropertyMutation, PropertyValue,
    },
    events::{AppEvent, EventBus},
    types::{
        AssetId, AudioSourceComponentId, ClipId, Color, EffectId, FramePosition, KeyframeId,
        Rational, Resolution, SequenceId, TrackId,
    },
    AudioChannelLayout, AudioSamplePosition, AudioSampleRate, AudioSampleRounding, FrameRounding,
    ProjectId, ProjectMeta, ProjectSettings, TimelineTime,
};
use mondrian_effects::{
    EffectNode, EffectNodeExt, EffectType, MaskComponent, MaskId, MaskKeyframe, MaskShape,
};
use mondrian_export::queue::RenderQueue;
use mondrian_media::audio::{AudioBuffer, RealtimeAudioOutputSnapshot};
use mondrian_media::{
    AudioPcmContinuity, AudioPcmContinuityModel, AudioPcmRenderGeneration, AudioPcmRenderRequest,
    AudioPcmRenderer, AudioPlayback, AudioPlaybackEvent, AudioPlaybackMode, AudioPlaybackSnapshot,
    AudioSourceCache, AudioSourceCacheDiagnostics,
};
use mondrian_playback::{
    AudioClockObservationGrade, AudioDeviceClockObservation, AudioDeviceClockState, ClockMaster,
    FrameDelivery, FrameDeliveryKind, FramePresentationQuality, FramePresentationTicket,
    MonotonicTimestamp, PlaybackEngine, PlaybackEvidenceCollector, PlaybackEvidenceReport,
    PlaybackSeekKind, PreviewResolutionScale, TransportState, VideoPrerollObservation,
};
use mondrian_timeline::clip::{Clip, TrimEdge};
use mondrian_timeline::command::SequenceSnapshotCommand;
use mondrian_timeline::sequence::{Sequence, SequenceCollection, SequenceSettings};
use serde::{Deserialize, Serialize};

const PROJECT_EXTENSION: &str = "mdp";
const DEFAULT_ADJUSTMENT_LAYER_DURATION_SECS: f64 = 5.0;
const MAX_STATUS_LOG_ENTRIES: usize = 64;
const AUDIO_OUTPUT_LAYOUT: AudioChannelLayout = AudioChannelLayout::Stereo;
const AUDIO_IDLE_WARMUP_CHUNK_MILLIS: u32 = 80;

#[cfg(test)]
pub(crate) fn tt(frame: i64, time_base: Rational) -> TimelineTime {
    let numerator = frame.checked_mul(time_base.num).expect("test time fits i64");
    TimelineTime::new(numerator, time_base.den).expect("valid test time")
}

mod action_handler;
mod animation_state;
#[cfg(test)]
mod audio_playback_acceptance;
mod audio_rendering;
mod clip_clipboard;
pub(crate) mod exporting;
#[cfg(test)]
pub(crate) mod headless_viewer_gpu;
mod media_import;
pub(crate) mod native_video_import;
mod playback;
#[cfg(test)]
mod playback_acceptance;
pub(crate) mod playback_preview;
pub(crate) mod preview_access_mode;
pub(crate) mod preview_cpu_execution;
pub(crate) mod preview_display_contract;
pub(crate) mod preview_execution;
pub(crate) mod preview_frame_store;
pub(crate) mod preview_gpu_output_blocker;
pub(crate) mod preview_hardware_admission;
pub(crate) mod preview_media_frame;
pub(crate) mod preview_media_source;
pub(crate) mod preview_media_task;
pub(crate) mod preview_quality;
pub(crate) mod preview_raster_frame;
pub mod preview_runtime;
pub(crate) mod preview_scheduler_policy;
pub(crate) mod preview_timeline_execution;
pub(crate) mod preview_viewer_plan;
mod project_lifecycle;
pub mod proxy_generation;
mod selection;
pub mod thumbnail_service;
mod timeline_commands;
mod timeline_editing;
pub mod ui_actions;
pub mod waveform_service;

use self::ui_actions::TimelineSeekSource;
use audio_rendering::*;
use exporting::TimelineExportDraft;
use media_import::*;
use proxy_generation::{
    ProxyGenerationDiagnostics, ProxyGenerationOrigin, ProxyGenerationRequestOutcome,
    ProxyGenerationService,
};
pub use selection::{SelectedClipRef, SelectedEffectRef, SelectedTrackRef};
use timeline_editing::*;

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
    pub time: TimelineTime,
}

/// Unified timeline, clip, mask, and effect selection — single source of truth.
///
/// All panels read from and write to this struct. No panel maintains its
/// own copy of selection state.
#[derive(Debug, Clone, Default)]
pub struct SelectionState {
    /// Selected timeline tracks (supports multi-select from track headers).
    pub selected_track_ids: Vec<TrackId>,
    /// Selected clips (supports multi-select from timeline).
    pub selected_clips: Vec<SelectedClipRef>,
    /// Selected effect inside the primary clip, shared by Inspector and graph views.
    pub selected_effect: Option<SelectedEffectRef>,
    /// Currently selected mask (canvas → effect controls).
    pub selected_mask: Option<(MaskId, ClipId, TrackId)>,
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
    pub relative_time: TimelineTime,
    pub keyframe: Keyframe<PropertyValue>,
}

#[derive(Debug, Clone, Default)]
pub struct AnimationClipboard {
    pub entries: Vec<AnimationClipboardEntry>,
}

/// Clip clipboard content used by app-level Copy/Cut/Paste actions.
#[derive(Debug, Clone, Default)]
pub struct ClipClipboard {
    entries: Vec<ClipClipboardEntry>,
    audio_transitions: Vec<mondrian_timeline::audio::AudioTransition>,
}

/// One copied clip plus enough context to paste it back into the active sequence.
#[derive(Debug, Clone)]
struct ClipClipboardEntry {
    original_clip_id: ClipId,
    track_id: TrackId,
    is_video_track: bool,
    relative_start: TimelineTime,
    clip: Clip,
    audio_processing_scopes: Vec<mondrian_timeline::audio::AudioProcessingScope>,
}

/// Active app clipboard payload kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppClipboardKind {
    /// Animation keyframes copied from the selected clip.
    AnimationKeyframes,
    /// Timeline clips copied from the active sequence.
    Clips,
}

/// One user-visible runtime status message retained for diagnostics panels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLogEntry {
    /// Human-readable status message.
    pub message: String,
    /// Whether the message represents an error.
    pub is_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ClipOverlapMode {
    #[default]
    Overwrite,
    Insert,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AutosaveManifest {
    project_file: PathBuf,
    #[serde(default)]
    snapshots: Vec<AutosaveSnapshotEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AutosaveSnapshotEntry {
    file: PathBuf,
    saved_at_unix_ms: u64,
}

impl AutosaveManifest {
    fn normalize(&mut self) {
        self.snapshots.retain(|s| s.file.exists());
        self.snapshots.sort_by_key(|s| std::cmp::Reverse(s.saved_at_unix_ms));
        self.snapshots.dedup_by_key(|s| s.file.clone());
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CrashRecoveryCandidate {
    pub(crate) project_file: PathBuf,
    pub(crate) autosave_file: PathBuf,
    pub(crate) saved_at_unix_ms: u64,
    pub(crate) total_snapshots: usize,
}

// ─────────────────────────────────────────────
//  AppState — 单向数据流中心
// ─────────────────────────────────────────────

pub struct AppState {
    // 全局事件总线
    pub event_bus: Arc<EventBus>,

    // 当前打开的序列（None = 无项目）
    pub sequence: Option<Sequence>,
    pub sequences: Vec<Sequence>,
    pub active_sequence_id: Option<SequenceId>,
    pub default_sequence_id: Option<SequenceId>,
    pub sequence_navigation_stack: Vec<SequenceId>,
    pub project_id: Option<ProjectId>,
    pub project_meta: Option<ProjectMeta>,
    pub project_document_revision: u64,

    // 当前打开的项目文件
    pub current_project_path: Option<PathBuf>,

    // 当前项目运行时工作目录（用于素材库 SQLite）
    pub project_runtime_dir: Option<PathBuf>,

    // 项目级色彩管理设置（所有序列默认继承）
    pub project_settings: ProjectSettings,

    // 撤销/重做历史（封装在 timeline crate 中）
    pub cmd_history: mondrian_timeline::command::CommandHistory,

    // 播放状态
    /// Sole authority for transport position, epoch, and Clock Master.
    playback_engine: PlaybackEngine,
    /// Bounded production Adapter for versioned Playback Evidence.
    playback_evidence: PlaybackEvidenceCollector,
    /// High-water mark preventing evidence from regressing between event-loop ticks.
    playback_evidence_now: MonotonicTimestamp,
    /// App-adapter monotonic origin advanced only by event-loop elapsed time.
    playback_now: MonotonicTimestamp,
    /// Wall-clock anchor corresponding exactly to `playback_presentation_time_anchor`.
    playback_presentation_wall_anchor: Instant,
    /// Playback timestamp paired with the presentation wall-clock anchor.
    playback_presentation_time_anchor: MonotonicTimestamp,
    /// Most recent timeline seek interaction source used by preview access-mode selection.
    pub last_timeline_seek_source: TimelineSeekSource,

    // 素材库
    pub asset_library: Option<Arc<AssetLibrary>>,

    // 正在拖拽的素材（从素材库拖向时间线）
    pub dragging_asset: Option<DraggingAsset>,
    /// Unified selection state — single source of truth for all panels.
    pub selection: SelectionState,

    // 渲染导出队列
    pub(crate) render_queue: Arc<RenderQueue>,
    /// Last export queue revision consumed by the app event-loop Adapter.
    export_queue_observed_revision: u64,
    /// UI-stable timeline export draft shared by app UI export panels.
    pub export_draft: TimelineExportDraft,

    // 底部状态栏提示（message, is_error）
    pub status_hint: Option<(String, bool)>,
    /// Bounded history of user-visible status messages for diagnostics panels.
    pub status_log: Vec<StatusLogEntry>,

    // 动画选择状态（timeline / inspector / future graph 共用）
    pub animation_selection: AnimationSelectionState,
    pub animation_clipboard: Option<AnimationClipboard>,
    pub clip_clipboard: Option<ClipClipboard>,
    pub active_clipboard_kind: Option<AppClipboardKind>,

    // 代理策略
    pub auto_proxy_enabled: bool,
    pub proxy_mode_assets: HashSet<AssetId>,
    proxy_generation: ProxyGenerationService,

    // 音频时钟与 A/V 同步
    pub audio_sample_rate: u32,
    audio_playback: AudioPlayback,
    pub audio_source_cache: Arc<AudioSourceCache>,
    audio_idle_warmup_last: Option<std::time::Instant>,

    media_import_tx: mpsc::Sender<MediaImportResult>,
    media_import_rx: mpsc::Receiver<MediaImportResult>,
    next_media_import_batch_id: u64,
    media_import_batches: HashMap<u64, PendingMediaImportBatch>,
}

impl AppState {
    pub fn new() -> Self {
        let audio_sample_rate = 48_000;
        let audio_source_cache = Arc::new(AudioSourceCache::new(
            audio_sample_rate,
            AUDIO_OUTPUT_LAYOUT,
        ));
        let (media_import_tx, media_import_rx) = mpsc::channel::<MediaImportResult>();
        let playback_presentation_wall_anchor = Instant::now();

        Self {
            event_bus: EventBus::new(),
            sequence: None,
            sequences: Vec::new(),
            active_sequence_id: None,
            default_sequence_id: None,
            sequence_navigation_stack: Vec::new(),
            project_id: None,
            project_meta: None,
            project_document_revision: 0,
            current_project_path: None,
            project_runtime_dir: None,
            project_settings: ProjectSettings::default(),
            cmd_history: mondrian_timeline::command::CommandHistory::default(),
            playback_engine: PlaybackEngine::default(),
            playback_evidence: PlaybackEvidenceCollector::default(),
            playback_evidence_now: MonotonicTimestamp::ZERO,
            playback_now: MonotonicTimestamp::ZERO,
            playback_presentation_wall_anchor,
            playback_presentation_time_anchor: MonotonicTimestamp::ZERO,
            last_timeline_seek_source: TimelineSeekSource::Settled,
            asset_library: None,
            dragging_asset: None,
            selection: SelectionState::default(),
            render_queue: RenderQueue::new(),
            export_queue_observed_revision: 0,
            export_draft: TimelineExportDraft::default(),
            status_hint: None,
            status_log: Vec::new(),
            animation_selection: AnimationSelectionState::default(),
            animation_clipboard: None,
            clip_clipboard: None,
            active_clipboard_kind: None,
            auto_proxy_enabled: false,
            proxy_mode_assets: HashSet::new(),
            proxy_generation: ProxyGenerationService::new(),
            audio_sample_rate,
            audio_playback: AudioPlayback::product_default(),
            audio_source_cache,
            audio_idle_warmup_last: None,
            media_import_tx,
            media_import_rx,
            next_media_import_batch_id: 1,
            media_import_batches: HashMap::new(),
        }
    }

    pub fn set_status_hint(&mut self, message: impl Into<String>, is_error: bool) {
        let message = message.into();
        self.status_hint = Some((message.clone(), is_error));
        self.push_status_log(message, is_error);
    }

    pub fn clear_status_hint(&mut self) {
        self.status_hint = None;
    }

    fn push_status_log(&mut self, message: String, is_error: bool) {
        if message.trim().is_empty() {
            return;
        }
        if self
            .status_log
            .last()
            .is_some_and(|entry| entry.message == message && entry.is_error == is_error)
        {
            return;
        }
        self.status_log.push(StatusLogEntry { message, is_error });
        let overflow = self.status_log.len().saturating_sub(MAX_STATUS_LOG_ENTRIES);
        if overflow > 0 {
            self.status_log.drain(0..overflow);
        }
    }

    pub fn set_auto_proxy_enabled(&mut self, enabled: bool) {
        self.auto_proxy_enabled = enabled;
    }

    pub(crate) fn request_proxy_generation(
        &self,
        asset_id: AssetId,
        source_path: PathBuf,
        config: mondrian_media::ProxyConfig,
        color: mondrian_media::ProxyColorContract,
        origin: ProxyGenerationOrigin,
    ) -> ProxyGenerationRequestOutcome {
        self.proxy_generation.request(asset_id, source_path, config, color, origin)
    }

    /// Observe completed/canceled proxy work for background UI refresh.
    pub fn poll_proxy_generation(&mut self) -> bool {
        if !self.proxy_generation.poll_finished() {
            return false;
        }
        let terminal = self.proxy_generation.diagnostics().terminal_records.last().cloned();
        if let Some(terminal) = terminal {
            if terminal.evidence.disposition == mondrian_core::ExecutionTerminalDisposition::Failed
                && terminal.executed
            {
                if let Some(detail) = terminal.failure_detail {
                    self.set_status_hint(format!("代理生成失败：{detail}"), true);
                }
            }
        }
        true
    }

    /// Snapshot bounded proxy-generation execution evidence.
    pub fn proxy_generation_diagnostics(&self) -> ProxyGenerationDiagnostics {
        self.proxy_generation.diagnostics()
    }

    /// Whether newly imported video media should enter proxy playback and start proxy generation.
    pub fn should_auto_generate_proxy_for_import(&self) -> bool {
        self.project_settings.proxy_enabled
    }

    /// Resolve project proxy settings into the media-layer proxy generator config.
    pub fn proxy_config(&self) -> mondrian_media::ProxyConfig {
        let mut config = mondrian_media::ProxyConfig {
            resolution: proxy_resolution_from_project(self.project_settings.proxy_resolution),
            ..mondrian_media::ProxyConfig::default()
        };
        if let Some(cache_dir) = self.project_settings.cache_dir.as_ref() {
            config.cache_dir = cache_dir.join("proxy");
        }
        config
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

fn proxy_resolution_from_project(resolution: Resolution) -> mondrian_media::ProxyResolution {
    match resolution.height {
        0..=360 => mondrian_media::ProxyResolution::P360,
        361..=480 => mondrian_media::ProxyResolution::P480,
        481..=720 => mondrian_media::ProxyResolution::P720,
        _ => mondrian_media::ProxyResolution::P1080,
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod status_log_tests {
    use super::*;

    #[test]
    fn set_status_hint_records_bounded_status_log_without_duplicate_tail() {
        let mut state = AppState::new();

        state.set_status_hint("Ready", false);
        state.set_status_hint("Ready", false);
        state.set_status_hint("Failed", true);

        assert_eq!(
            state.status_log,
            vec![
                StatusLogEntry { message: "Ready".to_owned(), is_error: false },
                StatusLogEntry { message: "Failed".to_owned(), is_error: true },
            ]
        );

        for index in 0..(MAX_STATUS_LOG_ENTRIES + 4) {
            state.set_status_hint(format!("Message {index}"), false);
        }

        assert_eq!(state.status_log.len(), MAX_STATUS_LOG_ENTRIES);
        assert_eq!(
            state.status_log.first().expect("first status").message,
            "Message 4"
        );
        assert_eq!(
            state.status_log.last().expect("last status").message,
            format!("Message {}", MAX_STATUS_LOG_ENTRIES + 3)
        );
    }

    #[test]
    fn clear_status_hint_preserves_status_log_history() {
        let mut state = AppState::new();

        state.set_status_hint("Saved", false);
        state.clear_status_hint();

        assert!(state.status_hint.is_none());
        assert_eq!(state.status_log.len(), 1);
        assert_eq!(state.status_log[0].message, "Saved");
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

// ─────────────────────────────────────────────

pub(crate) fn app_data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    base.join("mondrian")
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
    manifest.normalize();
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
    manifest.normalize();
}

pub(crate) fn discover_crash_recovery_candidates() -> Vec<CrashRecoveryCandidate> {
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
        manifest.normalize();
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
