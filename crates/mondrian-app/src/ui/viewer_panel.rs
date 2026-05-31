use crate::{
    app::AppState,
    ui::theme::{self, palette, tokens, typography},
};
use egui::{Pos2, Rect, Sense, Ui, Vec2};

use crate::ui::timeline_panel::SelectedClipRef;
use mondrian_core::{
    apply_display_profile_rgba8_in_place,
    automation::timecode_to_ticks,
    convert_rgba8_in_place,
    types::{
        AssetId, BlendMode, ClipId, Color, ColorEngine, ColorSpace, Rational, Resolution,
        SequenceId, TimeCode, TrackId,
    },
    ColorPipeline, DisplayColorProfile,
};
use mondrian_effects::mask::{BezierPoint, MaskKeyframe, MaskShape};
use mondrian_effects::CompiledEffectGraph;
use mondrian_media::cache::FrameCacheConfig;
use mondrian_media::{DecoderPool, FrameCache, RgbaFrame};
use mondrian_renderer::{
    build_timeline_render_plan, collect_timeline_color_diagnostics,
    composite_timeline_elements_float_linear, is_identity_transform, quantize_transform_signature,
    CompositorConfig, CpuRgbaLayer, FrameCompositor, GpuContext, TimelineAdjustmentLayer,
    TimelineCompositeElement, TimelineCompositeOptions, TimelineCompositeScratch,
    TimelineMediaLayer, TimelineRenderPlanElement, TimelineSolidColorLayer,
};
use mondrian_timeline::sequence::{
    ColorContext, ColorWorkflow, MissingColorMetadataPolicy, NestedColorProcessing, Sequence,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    sync::OnceLock,
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

#[derive(Clone)]
struct LayerDecodeRequest {
    frame_key: (AssetId, i64),
    path: PathBuf,
    input_color_space: ColorSpace,
    working_color_space: ColorSpace,
    engine: ColorEngine,
    tone_map: bool,
    source_secs: f64,
    source_time_base: Rational,
    opacity: f32,
    blend_mode: BlendMode,
    transform: [f32; 6],
    effect_graph: std::sync::Arc<CompiledEffectGraph>,
    frame_seed: i64,
}

#[derive(Clone)]
struct AdjustmentRenderRequest {
    effect_graph: std::sync::Arc<CompiledEffectGraph>,
    opacity: f32,
    blend_mode: Option<BlendMode>,
    frame_seed: i64,
}

#[derive(Clone)]
struct NestedSequenceRenderRequest {
    sequence_id: SequenceId,
    source_frame: i64,
    width: u32,
    height: u32,
    nested_processing: NestedColorProcessing,
    working_color_space: ColorSpace,
    engine: ColorEngine,
    tone_map: bool,
    opacity: f32,
    blend_mode: BlendMode,
    transform: [f32; 6],
    effect_graph: std::sync::Arc<CompiledEffectGraph>,
    frame_seed: i64,
    layers: Vec<RenderElement>,
}

#[derive(Clone)]
struct SolidColorRenderRequest {
    color: Color,
    opacity: f32,
    blend_mode: BlendMode,
    transform: [f32; 6],
    effect_graph: std::sync::Arc<CompiledEffectGraph>,
    frame_seed: i64,
}

#[derive(Clone)]
enum RenderElement {
    Media(LayerDecodeRequest),
    Adjustment(AdjustmentRenderRequest),
    SolidColor(SolidColorRenderRequest),
    NestedSequence(NestedSequenceRenderRequest),
}

#[derive(Clone)]
struct DecodeRequest {
    signature: CompositeFrameSignature,
    layers: Vec<RenderElement>,
    working_color_space: ColorSpace,
    output_color_space: ColorSpace,
    engine: ColorEngine,
    display_profile: DisplayColorProfile,
    ocio_display: Option<String>,
    ocio_view: Option<String>,
    tone_map: bool,
    playback_mode: bool,
    target_width: u32,
    target_height: u32,
    seq_width: u32,
    seq_height: u32,
    layer_cache_enabled: bool,
    layer_cache: SharedLayerFrameCache,
    decoder_pool: Arc<DecoderPool>,
    generation: u64,
    latest_generation: Arc<AtomicU64>,
}

#[derive(Clone)]
struct DecodeResult {
    signature: CompositeFrameSignature,
    decoded: Result<RgbaFrame, String>,
    generation: u64,
}

#[derive(Clone)]
struct PendingDecodeCommit {
    result: DecodeResult,
    allow_stale: bool,
}

#[derive(Debug, Clone)]
struct PrefetchTaskInfo {
    task_id: u64,
    generation: u64,
    direction: i64,
    target_frame: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum LayerSignature {
    Media {
        asset_id: AssetId,
        source_frame: i64,
        source_time_base: Rational,
        opacity_u8: u8,
        blend_mode: BlendMode,
        transform_key: [i32; 6],
        frame_seed: i64,
        effect_hash: u64,
        input_color_space: ColorSpace,
        working_color_space: ColorSpace,
        tone_map: bool,
    },
    Adjustment {
        opacity_u8: u8,
        blend_mode: Option<BlendMode>,
        frame_seed: i64,
        effect_hash: u64,
    },
    SolidColor {
        color_bits: [u32; 4],
        opacity_u8: u8,
        blend_mode: BlendMode,
        transform_key: [i32; 6],
        frame_seed: i64,
        effect_hash: u64,
    },
    NestedSequence {
        sequence_id: SequenceId,
        source_frame: i64,
        opacity_u8: u8,
        blend_mode: BlendMode,
        transform_key: [i32; 6],
        frame_seed: i64,
        effect_hash: u64,
        child_hash: u64,
        nested_processing: NestedColorProcessing,
        engine: ColorEngine,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompositeFrameSignature {
    width: u32,
    height: u32,
    working_color_space: ColorSpace,
    output_color_space: ColorSpace,
    display_profile_key: u64,
    tone_map: bool,
    layers: Vec<LayerSignature>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LayerFrameCacheKey {
    asset_id: AssetId,
    source_frame: i64,
    source_time_base: Rational,
    target_width: u32,
    target_height: u32,
    input_color_space: ColorSpace,
    working_color_space: ColorSpace,
    engine: ColorEngine,
    tone_map: bool,
}

#[derive(Default)]
struct LayerFrameCache {
    entries: HashMap<LayerFrameCacheKey, RgbaFrame>,
    order: VecDeque<LayerFrameCacheKey>,
}

type SharedLayerFrameCache = Arc<Mutex<LayerFrameCache>>;

#[derive(Debug, Clone)]
struct CachedAssetPreview {
    is_video: bool,
    source_path: PathBuf,
    color_space: ColorSpace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
enum PreviewScaleMode {
    #[default]
    Full,
    Half,
    Quarter,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ViewerPreferences {
    preview_scale_mode: PreviewScaleMode,
    proxy_config: mondrian_media::ProxyConfig,
    #[serde(default)]
    decode_backend: mondrian_media::PreviewDecodeBackend,
    #[serde(default = "default_prefetch_enabled")]
    prefetch_enabled: bool,
    #[serde(default = "default_layer_cache_enabled")]
    layer_cache_enabled: bool,
    #[serde(default = "DisplayColorProfile::rec709_reference")]
    display_profile: DisplayColorProfile,
    /// Canvas background color (letterbox/pillarbox), stored as 0xRRGGBB hex.
    #[serde(default = "default_canvas_bg")]
    canvas_bg_hex: u32,
}

fn default_canvas_bg() -> u32 {
    0x2a2a2a
}

fn hex_to_bg(hex: u32) -> egui::Color32 {
    let r = ((hex >> 16) & 0xFF) as u8;
    let g = ((hex >> 8) & 0xFF) as u8;
    let b = (hex & 0xFF) as u8;
    egui::Color32::from_rgb(r, g, b)
}

const fn default_prefetch_enabled() -> bool {
    true
}

const fn default_layer_cache_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Copy)]
pub struct MediaCachePolicy {
    pub max_size_bytes: u64,
    pub max_age_days: u64,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MediaCacheCleanupStats {
    pub deleted_files: usize,
    pub deleted_bytes: u64,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MediaCacheUsageStats {
    pub file_count: usize,
    pub total_bytes: u64,
}

impl Default for ViewerPreferences {
    fn default() -> Self {
        Self {
            preview_scale_mode: PreviewScaleMode::default(),
            proxy_config: mondrian_media::ProxyConfig::default(),
            decode_backend: mondrian_media::PreviewDecodeBackend::default(),
            prefetch_enabled: default_prefetch_enabled(),
            layer_cache_enabled: default_layer_cache_enabled(),
            display_profile: DisplayColorProfile::rec709_reference(),
            canvas_bg_hex: default_canvas_bg(),
        }
    }
}

impl PreviewScaleMode {
    fn label(self) -> &'static str {
        match self {
            Self::Full => "全分辨率",
            Self::Half => "1/2",
            Self::Quarter => "1/4",
        }
    }

    fn factor(self) -> f32 {
        match self {
            Self::Full => 1.0,
            Self::Half => 0.5,
            Self::Quarter => 0.25,
        }
    }
}

impl Hash for CompositeFrameSignature {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.width.hash(state);
        self.height.hash(state);
        self.working_color_space.hash(state);
        self.output_color_space.hash(state);
        self.display_profile_key.hash(state);
        self.tone_map.hash(state);
        self.layers.hash(state);
    }
}

/// 中央预览窗口面板
pub struct ViewerPanel {
    preview_texture: Option<egui::TextureHandle>,
    preview_signature: Option<CompositeFrameSignature>,
    desired_signature: Option<CompositeFrameSignature>,
    preview_error: Option<String>,
    decode_rx: Receiver<DecodeResult>,
    decode_request_tx: Sender<DecodeRequest>,
    decode_in_flight: bool,
    decode_in_flight_since: Option<Instant>,
    decode_in_flight_signature: Option<CompositeFrameSignature>,
    queued_request: Option<DecodeRequest>,
    texture_cache: VecDeque<(CompositeFrameSignature, egui::TextureHandle)>,
    layer_frame_cache: SharedLayerFrameCache,
    preview_scale_mode: PreviewScaleMode,
    next_decode_generation: u64,
    latest_decode_generation: Arc<AtomicU64>,
    prefetch_in_flight: Arc<Mutex<HashSet<LayerFrameCacheKey>>>,
    prefetch_generation: u64,
    decoder_pool: Arc<DecoderPool>,
    prefetch_tasks: HashMap<LayerFrameCacheKey, PrefetchTaskInfo>,
    asset_preview_cache: HashMap<AssetId, Option<CachedAssetPreview>>,
    last_prefetch_target_size: Option<(u32, u32)>,
    prefetch_resume_after: Option<Instant>,
    playback_prefill_until: Option<Instant>,
    playback_prefetch_buffering: bool,
    last_decode_submit_at: Option<Instant>,
    decoded_commit_queue: VecDeque<PendingDecodeCommit>,
    last_texture_commit_at: Option<Instant>,
    last_committed_generation: Option<u64>,
    was_playing_last_frame: bool,
    last_timeline_frame: Option<i64>,
    proxy_config: mondrian_media::ProxyConfig,
    decode_backend: mondrian_media::PreviewDecodeBackend,
    prefetch_enabled: bool,
    layer_cache_enabled: bool,
    display_profile: DisplayColorProfile,
    proxy_jobs_in_flight: Arc<Mutex<HashSet<AssetId>>>,
    proxy_done_tx: Sender<AssetId>,
    proxy_done_rx: Receiver<AssetId>,
    show_color_diagnostics: bool,
    ocio_display: Option<String>,
    ocio_view: Option<String>,
    canvas_bg_hex: u32,
    canvas_transform: crate::ui::viewer::canvas::CanvasTransform,
    /// Clip currently selected via canvas click (track_id, is_video, clip_id).
    canvas_selected_clip: Option<(
        mondrian_core::types::TrackId,
        bool,
        mondrian_core::types::ClipId,
    )>,
    /// Drag state for canvas move/resize.
    canvas_drag: Option<CanvasDragState>,
    /// Active mask creation tool (None = normal interaction mode).
    mask_tool: Option<MaskTool>,
    /// Mask creation drag state.
    mask_draw: Option<MaskDrawState>,
    /// Mask editing state (resize/move existing mask).
    mask_edit: Option<MaskEditState>,
    /// Currently selected mask for editing (mask_id, clip_id, track_id).
    selected_mask: Option<(
        mondrian_effects::mask::MaskId,
        mondrian_core::types::ClipId,
        mondrian_core::types::TrackId,
    )>,
    /// Cached context menu hit-test result (computed once, reused while menu is open).
    context_menu_hit: Option<(
        SelectedClipRef,
        Option<mondrian_effects::mask::MaskId>,
    )>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
enum DragMode {
    Move,
    Scale,
    Anchor,
}

struct CanvasDragState {
    track_id: mondrian_core::types::TrackId,
    is_video: bool,
    clip_id: mondrian_core::types::ClipId,
    mode: DragMode,
    start_pos: glam::Vec2,
    start_scale: glam::Vec2,
    start_anchor_val: glam::Vec2,
    start_anchor: Pos2,
    start_mouse: Pos2,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MaskTool {
    Rect,
    Ellipse,
    Pen,
}

#[derive(Debug, Clone)]
struct MaskDrawState {
    track_id: mondrian_core::types::TrackId,
    clip_id: mondrian_core::types::ClipId,
    start_seq: glam::Vec2,
    /// Pen tool accumulated points with Bézier control handles.
    pen_points: Vec<mondrian_effects::mask::BezierPoint>,
    /// Pen drag start for Bézier handle detection.
    pen_drag_start: Option<glam::Vec2>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MaskEditMode {
    Move,
    ResizeCorner(usize),
    MovePathPoint(usize),
    /// Dragging a control handle: (point_index, is_control_in)
    MovePathHandle(usize, bool),
}

#[derive(Debug, Clone)]
struct MaskEditState {
    mask_id: mondrian_effects::mask::MaskId,
    clip_id: mondrian_core::types::ClipId,
    #[allow(dead_code)]
    track_id: mondrian_core::types::TrackId,
    mode: MaskEditMode,
    start_shape: mondrian_effects::mask::MaskShape,
    start_mouse: Pos2,
}

/// Unified hit-test result for canvas interaction priority resolution.
enum HitResult {
    /// Hit a mask corner handle (highest priority).
    MaskCorner {
        track_id: mondrian_core::types::TrackId,
        clip_id: mondrian_core::types::ClipId,
        mask_id: mondrian_effects::mask::MaskId,
        corner: usize,
        shape: mondrian_effects::mask::MaskShape,
        pos: Pos2,
    },
    /// Hit a mask outline or interior (lower priority than clip handles).
    MaskMove {
        track_id: mondrian_core::types::TrackId,
        clip_id: mondrian_core::types::ClipId,
        mask_id: mondrian_effects::mask::MaskId,
        shape: mondrian_effects::mask::MaskShape,
        pos: Pos2,
    },
    /// Hit a clip resize corner.
    ClipCorner {
        track_id: mondrian_core::types::TrackId,
        is_video: bool,
        clip_id: mondrian_core::types::ClipId,
        pos_v: glam::Vec2,
        scale: glam::Vec2,
        anchor_val: glam::Vec2,
        anchor_screen: Pos2,
        pos: Pos2,
    },
    /// Hit the clip anchor point.
    ClipAnchor {
        track_id: mondrian_core::types::TrackId,
        is_video: bool,
        clip_id: mondrian_core::types::ClipId,
        pos_v: glam::Vec2,
        scale: glam::Vec2,
        anchor_val: glam::Vec2,
        anchor_screen: Pos2,
        pos: Pos2,
    },
    /// Hit clip interior (lowest priority).
    ClipMove {
        track_id: mondrian_core::types::TrackId,
        is_video: bool,
        clip_id: mondrian_core::types::ClipId,
        pos_v: glam::Vec2,
        scale: glam::Vec2,
        anchor_val: glam::Vec2,
        anchor_screen: Pos2,
        pos: Pos2,
    },
}

impl Default for ViewerPanel {
    fn default() -> Self {
        let (decode_tx, decode_rx) = mpsc::channel();
        let (decode_request_tx, decode_request_rx) = mpsc::channel::<DecodeRequest>();
        let decode_request_rx = Arc::new(Mutex::new(decode_request_rx));

        let worker_count =
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 2);

        for _ in 0..worker_count {
            let tx = decode_tx.clone();
            let rx = Arc::clone(&decode_request_rx);
            std::thread::spawn(move || loop {
                let request = {
                    let guard = match rx.lock() {
                        Ok(g) => g,
                        Err(_) => break,
                    };
                    let mut latest = match guard.recv() {
                        Ok(req) => req,
                        Err(_) => break,
                    };

                    if decode_request_coalescing_enabled() {
                        while let Ok(next) = guard.try_recv() {
                            if next.generation >= latest.generation {
                                latest = next;
                            }
                        }
                    }

                    latest
                };

                let decoded = decode_composited_rgba(&request).map_err(|e| e.to_string());
                let _ = tx.send(DecodeResult {
                    signature: request.signature,
                    decoded,
                    generation: request.generation,
                });
            });
        }

        let (proxy_done_tx, proxy_done_rx) = mpsc::channel();
        Self {
            preview_texture: None,
            preview_signature: None,
            desired_signature: None,
            preview_error: None,
            decode_rx,
            decode_request_tx,
            decode_in_flight: false,
            decode_in_flight_since: None,
            decode_in_flight_signature: None,
            queued_request: None,
            texture_cache: VecDeque::new(),
            layer_frame_cache: Arc::new(Mutex::new(LayerFrameCache::default())),
            preview_scale_mode: PreviewScaleMode::default(),
            next_decode_generation: 1,
            latest_decode_generation: Arc::new(AtomicU64::new(0)),
            prefetch_in_flight: Arc::new(Mutex::new(HashSet::new())),
            prefetch_generation: 1,
            decoder_pool: DecoderPool::new(FrameCache::new(FrameCacheConfig::default())),
            prefetch_tasks: HashMap::new(),
            asset_preview_cache: HashMap::new(),
            last_prefetch_target_size: None,
            prefetch_resume_after: None,
            playback_prefill_until: None,
            playback_prefetch_buffering: false,
            last_decode_submit_at: None,
            decoded_commit_queue: VecDeque::new(),
            last_texture_commit_at: None,
            last_committed_generation: None,
            was_playing_last_frame: false,
            last_timeline_frame: None,
            proxy_config: mondrian_media::ProxyConfig::default(),
            decode_backend: mondrian_media::PreviewDecodeBackend::default(),
            prefetch_enabled: default_prefetch_enabled(),
            layer_cache_enabled: default_layer_cache_enabled(),
            display_profile: DisplayColorProfile::rec709_reference(),
            proxy_jobs_in_flight: Arc::new(Mutex::new(HashSet::new())),
            proxy_done_tx,
            proxy_done_rx,
            show_color_diagnostics: false,
            ocio_display: None,
            ocio_view: None,
            canvas_bg_hex: default_canvas_bg(),
            canvas_selected_clip: None,
            canvas_drag: None,
            mask_tool: None,
            mask_draw: None,
            mask_edit: None,
            selected_mask: None,
            context_menu_hit: None,
            canvas_transform: crate::ui::viewer::canvas::CanvasTransform::fit(
                (1920, 1080),
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(960.0, 540.0)),
            ),
        }
    }
}

impl ViewerPanel {
    /// Write clip selection to local field + AppState.selection (single source of truth).
    fn set_clip_selection(
        &mut self,
        state: &mut AppState,
        sel: Option<(TrackId, bool, ClipId)>,
    ) {
        self.canvas_selected_clip = sel;
        state.selection.selected_clips = sel
            .map(|(track_id, is_video, clip_id)| {
                vec![SelectedClipRef { track_id, is_video_track: is_video, clip_id }]
            })
            .unwrap_or_default();
    }

    pub fn toggle_color_diagnostics(&mut self) {
        self.show_color_diagnostics = !self.show_color_diagnostics;
    }

    pub fn media_cache_dir(&self) -> PathBuf {
        self.proxy_config.cache_dir.clone()
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        show_dev_metrics: bool,
        show_video_metrics: bool,
        show_audio_metrics: bool,
        show_preview_perf_metrics: bool,
    ) {
        let show_started_at = Instant::now();
        let diag_enabled = preview_diag_enabled();
        let is_playing = state.is_playing();
        // Reset canvas selection at start of each frame. Interaction code
        // re-sets it when the user clicks a clip.
        state.selection.selected_clips.clear();
        let current_frame = state.current_frame();
        let playback_fps = state
            .sequence
            .as_ref()
            .map(|seq| seq.settings.frame_rate.to_f64())
            .unwrap_or(25.0)
            .max(1.0);
        self.handle_timeline_discontinuity(current_frame, is_playing, playback_fps);

        self.poll_proxy_events(ui.ctx());
        self.poll_decode_results(ui.ctx(), is_playing, playback_fps);
        self.recover_if_decode_stalled(ui.ctx());

        if !self.was_playing_last_frame && is_playing {
            self.on_playback_started();
        }

        if self.was_playing_last_frame && !is_playing {
            self.on_playback_stopped();
        }

        if self.decode_in_flight {
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        } else if self.queued_request.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(8));
        }

        // Minimum width to keep the transport bar legible.
        // Left timecode (160) + center controls (~210) + right info (170) ≈ 540.
        ui.set_min_width(540.0);

        // ── Keyboard shortcuts ──
        if !ui.ctx().wants_keyboard_input() {
            let current_frame = state.current_frame();
            if ui.input(|i| i.key_pressed(egui::Key::Space)) {
                if state.is_playing() {
                    state.pause();
                } else {
                    state.play();
                }
            }
            // JKL shuttle
            if ui.input(|i| i.key_pressed(egui::Key::K)) {
                state.pause();
            }
            if ui.input(|i| i.key_pressed(egui::Key::J) && !i.modifiers.command) {
                state.seek(current_frame - 1);
            }
            if ui.input(|i| i.key_pressed(egui::Key::L) && !i.modifiers.command) {
                state.seek(current_frame + 1);
            }
            // Arrow keys
            if ui.input(|i| i.key_pressed(egui::Key::ArrowLeft) && !i.modifiers.alt) {
                let step = if ui.input(|i| i.modifiers.shift) {
                    10
                } else {
                    1
                };
                state.seek((current_frame - step).max(0));
            }
            if ui.input(|i| i.key_pressed(egui::Key::ArrowRight) && !i.modifiers.alt) {
                let step = if ui.input(|i| i.modifiers.shift) {
                    10
                } else {
                    1
                };
                state.seek(current_frame + step);
            }
            // Home / End
            if ui.input(|i| i.key_pressed(egui::Key::Home)) {
                state.seek(0);
            }
            if ui.input(|i| i.key_pressed(egui::Key::End)) {
                state.seek(state.last_content_frame().max(0));
            }
            // Mask tool shortcuts
            if ui.input(|i| i.key_pressed(egui::Key::R) && !i.modifiers.command) {
                self.mask_tool = Some(MaskTool::Rect);
                self.mask_edit = None;
                self.selected_mask = None;
            }
            if ui.input(|i| i.key_pressed(egui::Key::E) && !i.modifiers.command) {
                self.mask_tool = Some(MaskTool::Ellipse);
                self.mask_edit = None;
                self.selected_mask = None;
            }
            if ui.input(|i| i.key_pressed(egui::Key::P) && !i.modifiers.command) {
                self.mask_tool = Some(MaskTool::Pen);
                self.mask_edit = None;
                self.selected_mask = None;
            }
            if ui.input(|i| i.key_pressed(egui::Key::V) && !i.modifiers.command) {
                self.mask_tool = None;
                self.mask_draw = None;
                self.mask_edit = None;
                self.selected_mask = None;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.mask_tool = None;
                self.mask_draw = None;
                self.mask_edit = None;
                self.selected_mask = None;
            }
        }

        ui.vertical(|ui| {
            // ── Mask tool toolbar ── (consumes its own space)
            draw_mask_toolbar(ui, self);

            let controls_height = tokens::viewer_transport_height();
            let transport_gap = 4.0;
            let canvas_slot_height =
                (ui.available_height() - controls_height - transport_gap).max(120.0);

            let (canvas_slot_rect, canvas_resp) = ui.allocate_exact_size(
                Vec2::new(ui.available_width(), canvas_slot_height),
                Sense::click(),
            );

            // Right-click context menu — same pattern as library_panel/timeline_panel.
            canvas_resp.context_menu(|ui| {
                ui.set_min_width(100.0);
                if self.mask_tool.is_some() {
                    ui.close();
                    return;
                }

                // Clear cached hit on a new right-click; reuse while menu stays open.
                if ui.ctx().input(|i| i.pointer.secondary_clicked()) {
                    self.context_menu_hit = None;
                }
                if self.context_menu_hit.is_none() {
                    let Some(pos) = ui.ctx().input(|i| i.pointer.interact_pos()) else { return };
                    let Some(seq) = state.sequence.as_ref() else { return };
                    let current_frame = state.current_frame();
                    let current = TimeCode::new(current_frame.max(0), seq.time_base());
                    let active = seq.active_clips_at(current);
                    let ticks = timecode_to_ticks(current);

                    let mut found: Option<(SelectedClipRef, Option<mondrian_effects::mask::MaskId>)> = None;
                    // Hit-test masks first.
                    'ht: for ac in active.iter().rev() {
                        if ac.clip.masks.is_empty() { continue; }
                        let (mw, mh) = state.asset_library.as_ref()
                            .and_then(|lib| lib.get_asset(ac.clip.asset_id).ok().flatten())
                            .and_then(|a| a.media_info.primary_video().cloned())
                            .map(|v| (v.width as f32, v.height as f32))
                            .unwrap_or((1.0, 1.0));
                        for mask in ac.clip.masks.iter().rev() {
                            if !mask.enabled { continue; }
                            let kf = mask.evaluate_at(ticks);
                            let (_, hit_mask) = mask_hit_test(
                                &kf.shape, mw, mh, &ac.transform_matrix,
                                &self.canvas_transform, pos,
                            );
                            if hit_mask {
                                let track_id = seq.video_tracks.get(ac.track_index).map(|t| t.id).unwrap_or_default();
                                let sel = SelectedClipRef { track_id, is_video_track: true, clip_id: ac.clip.id };
                                found = Some((sel, Some(mask.id)));
                                break 'ht;
                            }
                        }
                    }
                    // Hit-test clips.
                    if found.is_none() {
                        for ac in active.iter().rev() {
                            let bb = clip_screen_bounds_with_media(
                                &ac.clip, ac.transform_matrix,
                                &self.canvas_transform, state,
                            );
                            if bb.is_some_and(|r| r.contains(pos)) {
                                let track_id = seq.video_tracks.get(ac.track_index).map(|t| t.id).unwrap_or_default();
                                let sel = SelectedClipRef { track_id, is_video_track: true, clip_id: ac.clip.id };
                                found = Some((sel, None));
                                break;
                            }
                        }
                    }
                    self.context_menu_hit = found;
                }

                match &self.context_menu_hit {
                    Some((sel, Some(mask_id))) => {
                        if ui.button("删除蒙版").clicked() {
                            let _ = state.remove_mask_from_clip(*sel, *mask_id);
                            self.selected_mask = None;
                            self.context_menu_hit = None;
                            ui.close();
                        }
                    }
                    Some((sel, None)) => {
                        if ui.button("删除片段").clicked() {
                            let _ = state.remove_clip(sel.track_id, sel.is_video_track, sel.clip_id);
                            self.context_menu_hit = None;
                            ui.close();
                        }
                    }
                    None => {
                        ui.close();
                    }
                }
            });

            // Canvas: slot is fixed, content may extend beyond (clipped to slot).
            let canvas_rect = canvas_slot_rect;
            let seq_res = state
                .sequence
                .as_ref()
                .map(|s| s.settings.resolution)
                .unwrap_or(Resolution { width: 1920, height: 1080 });
            self.canvas_transform.update_canvas_rect(canvas_rect);
            if self.canvas_transform.seq_size != (seq_res.width, seq_res.height) {
                self.canvas_transform = crate::ui::viewer::canvas::CanvasTransform::fit(
                    (seq_res.width, seq_res.height),
                    canvas_rect,
                );
                self.canvas_transform.background = hex_to_bg(self.canvas_bg_hex);
            }
            let content_rect = self.canvas_transform.content_rect();

            // Canvas interaction: Ctrl+scroll zoom, middle-drag pan, select, drag.
            let ctx = ui.ctx();
            let canvas_hovered = ctx.input(|inp| {
                inp.pointer
                    .interact_pos()
                    .is_some_and(|p| canvas_rect.contains(p))
            });
            if canvas_hovered {
                // Ctrl+scroll wheel zoom from center.
                if ctx.input(|inp| inp.modifiers.command) {
                    let scroll = ctx.input(|inp| inp.raw_scroll_delta.y);
                    if scroll.abs() > 0.1 {
                        let factor = 1.0 + scroll.abs().min(10.0) * 0.001 * scroll.signum();
                        let ctr = self.canvas_transform.canvas_rect.center();
                        self.canvas_transform.zoom_at_screen(ctr, factor);
                    }
                }
                // Middle-button drag to pan.
                if ctx.input(|inp| inp.pointer.button_down(egui::PointerButton::Middle)) {
                    let delta = ctx.input(|inp| inp.pointer.delta());
                    self.canvas_transform.pan_by_screen(Vec2::new(-delta.x, -delta.y));
                }
                // Handle drag state.
                let ptr = ctx.input(|inp| inp.pointer.interact_pos());
                let primary_down = ctx.input(|inp| inp.pointer.button_down(egui::PointerButton::Primary));

                // ── Mask creation mode ──
                if self.mask_tool == Some(MaskTool::Pen) {
                    // Pen tool: click to add points; click near start to close; Enter to commit.
                    let finish_path = ctx.input(|i| i.key_pressed(egui::Key::Enter));
                    let mut should_finish = finish_path;

                    if let Some(pos) = ptr {
                        let btn_pressed = ctx.input(|inp| inp.pointer.button_pressed(egui::PointerButton::Primary));
                        let btn_released = ctx.input(|inp| inp.pointer.button_released(egui::PointerButton::Primary));

                        if btn_pressed {
                            if let Some(seq_pos) = self.canvas_transform.screen_to_seq(pos) {
                                let pt = glam::Vec2::new(seq_pos.0, seq_pos.1);
                                if let Some(ref mut md) = self.mask_draw {
                                    let dist_to_start = md.pen_points.first().map(|&bp| bp.position.distance(pt)).unwrap_or(f32::MAX);
                                    if md.pen_points.len() >= 2 && dist_to_start < 15.0 {
                                        should_finish = true;
                                    } else {
                                        md.pen_drag_start = Some(pt);
                                    }
                                } else {
                                    // First click: find clip and start path.
                                    let mut target = None;
                                    if let Some(seq) = state.sequence.as_ref() {
                                        let current = TimeCode::new(state.current_frame().max(0), seq.time_base());
                                        let active = seq.active_clips_at(current);
                                        for ac in active.iter().rev() {
                                            let bb = clip_screen_bounds_with_media(&ac.clip, ac.transform_matrix, &self.canvas_transform, state);
                                            if bb.is_some_and(|r| r.contains(pos)) {
                                                let track_id = seq.video_tracks.get(ac.track_index).map(|t| t.id).unwrap_or_default();
                                                target = Some((track_id, ac.clip.id));
                                                break;
                                            }
                                        }
                                    }
                                    if let Some((tid, cid)) = target {
                                        self.set_clip_selection(state, Some((tid, true, cid)));
                                        self.mask_draw = Some(MaskDrawState {
                                            track_id: tid, clip_id: cid,
                                            start_seq: pt,
                                            pen_points: vec![mondrian_effects::mask::BezierPoint::new(pt)],
                                            pen_drag_start: None,
                                        });
                                    }
                                }
                            }
                        }

                        if btn_released {
                            if let Some(ref mut md) = self.mask_draw {
                                if let Some(drag_start) = md.pen_drag_start.take() {
                                    if let Some(seq_pos) = self.canvas_transform.screen_to_seq(pos) {
                                        let end_pt = glam::Vec2::new(seq_pos.0, seq_pos.1);
                                        let delta = end_pt - drag_start;
                                        if delta.length() > 3.0 {
                                            // Smooth Bezier: symmetric handles.
                                            let h = delta * 0.37;
                                            md.pen_points.push(mondrian_effects::mask::BezierPoint {
                                                position: drag_start,
                                                control_in: -h,
                                                control_out: h,
                                            });
                                        } else {
                                            // Corner point: no handles, anchor at click position.
                                            md.pen_points.push(mondrian_effects::mask::BezierPoint::new(drag_start));
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Commit the pen path (Enter or auto-close near start).
                    if should_finish {
                        if let Some(ref md) = self.mask_draw {
                            if md.pen_points.len() >= 2 {
                                let normalized_pts: Vec<mondrian_effects::mask::BezierPoint> = md.pen_points.iter().map(|bp| {
                                    let norm = seq_point_to_clip_normalized(state, md.clip_id, bp.position);
                                    // Normalize control handles: convert handle position → clip-local → diff from position.
                                    let cp_in = glam::Vec2::new(bp.position.x + bp.control_in.x, bp.position.y + bp.control_in.y);
                                    let norm_cp_in = seq_point_to_clip_normalized(state, md.clip_id, cp_in);
                                    let cp_out = glam::Vec2::new(bp.position.x + bp.control_out.x, bp.position.y + bp.control_out.y);
                                    let norm_cp_out = seq_point_to_clip_normalized(state, md.clip_id, cp_out);
                                    mondrian_effects::mask::BezierPoint {
                                        position: glam::Vec2::new(norm.0, norm.1),
                                        control_in: glam::Vec2::new(norm_cp_in.0 - norm.0, norm_cp_in.1 - norm.1),
                                        control_out: glam::Vec2::new(norm_cp_out.0 - norm.0, norm_cp_out.1 - norm.1),
                                    }
                                }).collect();
                                let mask = MaskKeyframe {
                                    shape: MaskShape::Path { points: normalized_pts, closed: true },
                                    ..Default::default()
                                };
                                let sel = SelectedClipRef {
                                    track_id: md.track_id, is_video_track: true, clip_id: md.clip_id,
                                };
                                let mask_name = mask_next_name(state, md.clip_id);
                                if let Ok(mask_id) = state.add_mask_to_clip(sel, &mask_name) {
                                    let current_frame = state.current_frame();
                                    if let Some(seq) = state.sequence.as_ref() {
                                        let current = TimeCode::new(current_frame.max(0), seq.time_base());
                                        let ticks = timecode_to_ticks(current);
                                        let _ = state.set_mask_keyframe(sel, mask_id, mask, ticks);
                                    }
                                }
                            }
                            self.mask_draw = None;
                            self.mask_tool = None;
                        }
                    }
                } else if self.mask_tool.is_some() {
                    // Rect / Ellipse: click-drag-release.
                    if let Some(ref mut md) = self.mask_draw {
                        if primary_down {
                            // Dragging — preview below.
                        } else {
                            // Release: create the mask.
                            if let Some(seq_pos) = ptr.and_then(|p| self.canvas_transform.screen_to_seq(p)) {
                                let (x1, y1) = (md.start_seq.x.min(seq_pos.0), md.start_seq.y.min(seq_pos.1));
                                let (x2, y2) = (md.start_seq.x.max(seq_pos.0), md.start_seq.y.max(seq_pos.1));
                                let normalized = seq_rect_to_clip_normalized(
                                    state, md.clip_id, glam::Vec2::new(x1, y1), glam::Vec2::new(x2, y2),
                                );
                                let Some(tool) = self.mask_tool else { return; };
                                let mask = MaskKeyframe {
                                    shape: match tool {
                                        MaskTool::Rect => MaskShape::Rectangle {
                                            x: normalized.0.x, y: normalized.0.y,
                                            width: normalized.1.x - normalized.0.x,
                                            height: normalized.1.y - normalized.0.y,
                                            corner_radius: 0.0,
                                        },
                                        MaskTool::Ellipse => MaskShape::Ellipse {
                                            center: (normalized.0 + normalized.1) * 0.5,
                                            radii: (normalized.1 - normalized.0) * 0.5,
                                        },
                                        MaskTool::Pen => return,
                                    },
                                    ..Default::default()
                                };
                                let sel = SelectedClipRef {
                                    track_id: md.track_id,
                                    is_video_track: true,
                                    clip_id: md.clip_id,
                                };
                                let mask_name = mask_next_name(state, md.clip_id);
                                if let Ok(mask_id) = state.add_mask_to_clip(sel, &mask_name) {
                                    let current_frame = state.current_frame();
                                    let seq = state.sequence.as_ref();
                                    if let Some(seq) = seq {
                                        let current = TimeCode::new(current_frame.max(0), seq.time_base());
                                        let ticks = timecode_to_ticks(current);
                                        let _ = state.set_mask_keyframe(sel, mask_id, mask, ticks);
                                    }
                                }
                            }
                            self.mask_draw = None;
                            self.mask_tool = None;
                        }
                    } else if let Some(pos) = ptr {
                        if ctx.input(|inp| inp.pointer.button_pressed(egui::PointerButton::Primary)) {
                            if let Some(seq_pos) = self.canvas_transform.screen_to_seq(pos) {
                                // Find clip under cursor to attach mask to.
                                let mut target: Option<(mondrian_core::types::TrackId, mondrian_core::types::ClipId)> = None;
                                if let Some(seq) = state.sequence.as_ref() {
                                    let current = TimeCode::new(state.current_frame().max(0), seq.time_base());
                                    let active = seq.active_clips_at(current);
                                    for ac in active.iter().rev() {
                                        let bb = clip_screen_bounds_with_media(
                                            &ac.clip, ac.transform_matrix,
                                            &self.canvas_transform, state,
                                        );
                                        if bb.is_some_and(|r| r.contains(pos)) {
                                            let track_id = seq.video_tracks
                                                .get(ac.track_index)
                                                .map(|t| t.id)
                                                .unwrap_or_default();
                                            target = Some((track_id, ac.clip.id));
                                            break;
                                        }
                                    }
                                }
                                if let Some((tid, cid)) = target {
                                    self.set_clip_selection(state, Some((tid, true, cid)));
                                    self.mask_draw = Some(MaskDrawState {
                                        track_id: tid,
                                        clip_id: cid,
                                        start_seq: glam::Vec2::new(seq_pos.0, seq_pos.1),
                                        pen_points: Vec::new(),
                                        pen_drag_start: None,
                                    });
                                }
                            }
                        }
                    }
                } else if let Some(ref mut edit) = self.mask_edit {
                    if primary_down {
                        if let Some(now) = ctx.input(|inp| inp.pointer.interact_pos()) {
                            let screen_delta = now - edit.start_mouse;
                            let zoom = self.canvas_transform.zoom();
                            let seq_delta = glam::Vec2::new(
                                screen_delta.x / zoom.max(0.001),
                                screen_delta.y / zoom.max(0.001),
                            );
                            // Convert seq delta to normalized delta via inverse clip transform.
                            let norm_delta = seq_delta_to_norm(
                                state, edit.clip_id, seq_delta,
                            );
                            let current_frame = state.current_frame();
                            let mut new_shape = edit.start_shape.clone();
                            match edit.mode {
                                MaskEditMode::Move => {
                                    translate_shape(&mut new_shape, norm_delta);
                                }
                                MaskEditMode::ResizeCorner(corner) => {
                                    resize_shape_corner(&mut new_shape, corner, norm_delta);
                                }
                                MaskEditMode::MovePathPoint(idx) => {
                                    move_path_point(&mut new_shape, idx, norm_delta);
                                }
                                MaskEditMode::MovePathHandle(idx, is_in) => {
                                    move_path_handle(&mut new_shape, idx, norm_delta, is_in);
                                }
                            }
                            // Update mask in real-time (no undo per frame).
                            update_mask_shape_direct(state, edit.clip_id, edit.mask_id, new_shape, current_frame);
                        }
                    } else {
                        self.mask_edit = None;
                    }
                } else if let Some(ref mut drag) = self.canvas_drag {
                    if primary_down {
                        if let Some(now) = ctx.input(|inp| inp.pointer.interact_pos()) {
                            match drag.mode {
                                DragMode::Move => {
                                    let screen_delta = now - drag.start_mouse;
                                    let zoom = self.canvas_transform.zoom();
                                    let seq_delta = glam::Vec2::new(
                                        screen_delta.x / zoom.max(0.001),
                                        screen_delta.y / zoom.max(0.001),
                                    );
                                    let new_pos = drag.start_pos + seq_delta;
                                    let _ = state.set_clip_position_direct(
                                        SelectedClipRef {
                                            track_id: drag.track_id,
                                            is_video_track: drag.is_video,
                                            clip_id: drag.clip_id,
                                        },
                                        new_pos,
                                    );
                                }
                                DragMode::Scale => {
                                    // Signed radial from anchor (not content center).
                                    let ref_pt = drag.start_anchor;
                                    let corner_dir = (drag.start_mouse - ref_pt).normalized();
                                    let start_proj = (drag.start_mouse - ref_pt).dot(corner_dir);
                                    let now_proj = (now - ref_pt).dot(corner_dir);
                                    let ratio = (now_proj / start_proj.max(0.01)).clamp(0.01, 100.0);
                                    let new_scale = drag.start_scale * ratio;
                                    let _ = state.set_clip_scale_direct(
                                        SelectedClipRef {
                                            track_id: drag.track_id,
                                            is_video_track: drag.is_video,
                                            clip_id: drag.clip_id,
                                        },
                                        new_scale,
                                    );
                                }
                                DragMode::Anchor => {
                                    let zoom = self.canvas_transform.zoom();
                                    let screen_delta = now - drag.start_mouse;
                                    let seq_delta = glam::Vec2::new(
                                        screen_delta.x / zoom.max(0.001),
                                        screen_delta.y / zoom.max(0.001),
                                    );
                                    let inv_scale = 1.0 / drag.start_scale.x.max(0.001);
                                    let new_anchor = drag.start_anchor_val + seq_delta * inv_scale;
                                    let _ = state.set_clip_anchor_direct(
                                        SelectedClipRef {
                                            track_id: drag.track_id,
                                            is_video_track: drag.is_video,
                                            clip_id: drag.clip_id,
                                        },
                                        new_anchor,
                                    );
                                }
                            }
                        }
                    } else {
                        self.canvas_drag = None;
                    }
                } else if ptr.is_some_and(|p| canvas_rect.contains(p)) {
                    if let Some(pos) = ptr {
                        if ctx.input(|inp| inp.pointer.button_pressed(egui::PointerButton::Primary)) {
                            if let Some(seq) = state.sequence.as_ref() {
                                let current = TimeCode::new(
                                    state.current_frame().max(0),
                                    seq.time_base(),
                                );
                                let active = seq.active_clips_at(current);

                                if self.mask_tool.is_none() {
                                    // ──────────────────────────────────────
                                    //  Priority-based hit resolution:
                                    //  1. Mask corners          (explicit handles, 10px)
                                    //  2. Clip corners & anchor (explicit handles, 14px / 10px)
                                    //  3. Mask outline/interior (less precise, 8px + interior)
                                    //  4. Clip interior         (lowest)
                                    //  Within each level, topmost clip/mask wins.
                                    // ──────────────────────────────────────

                                    let ticks = timecode_to_ticks(current);

                                    // Level 1: Mask corner hits (highest priority).
                                    let mut hit: Option<HitResult> = None;
                                    'l1: for ac in active.iter().rev() {
                                        if ac.clip.masks.is_empty() { continue; }
                                        let (mw, mh) = state.asset_library.as_ref()
                                            .and_then(|lib| lib.get_asset(ac.clip.asset_id).ok().flatten())
                                            .and_then(|a| a.media_info.primary_video().cloned())
                                            .map(|v| (v.width as f32, v.height as f32))
                                            .unwrap_or((1.0, 1.0));
                                        for mask in ac.clip.masks.iter().rev() {
                                            if !mask.enabled { continue; }
                                            let kf = mask.evaluate_at(ticks);
                                            let (corner_hit, _) = mask_hit_test(
                                                &kf.shape, mw, mh, &ac.transform_matrix,
                                                &self.canvas_transform, pos,
                                            );
                                            if let Some(corner_idx) = corner_hit {
                                                let track_id = seq.video_tracks
                                                    .get(ac.track_index)
                                                    .map(|t| t.id)
                                                    .unwrap_or_default();
                                                hit = Some(HitResult::MaskCorner {
                                                    track_id, clip_id: ac.clip.id, mask_id: mask.id,
                                                    corner: corner_idx, shape: kf.shape.clone(),
                                                    pos,
                                                });
                                                break 'l1;
                                            }
                                        }
                                    }

                                    // Level 2: Clip corners & anchor (only if no mask corner hit).
                                    if hit.is_none() {
                                        // Already-selected clip corners get priority.
                                        if let Some((_tid, _iv, sel_cid)) = self.canvas_selected_clip {
                                            if let Some(ac) = active.iter().find(|a| a.clip.id == sel_cid) {
                                                let bb = clip_screen_bounds_with_media(
                                                    &ac.clip, ac.transform_matrix,
                                                    &self.canvas_transform, state,
                                                );
                                                let track_id = seq.video_tracks
                                                    .get(ac.track_index)
                                                    .map(|t| t.id)
                                                    .unwrap_or_default();
                                                let pos_v = ac.clip.transform.get_position(current);
                                                let anchor_val = ac.clip.transform.get_anchor_point(current);
                                                let anchor_screen = self.canvas_transform.seq_to_screen(pos_v.x, pos_v.y);
                                                if is_near_corner(bb, pos, 14.0) {
                                                    let scale = ac.clip.transform.get_scale(current);
                                                    hit = Some(HitResult::ClipCorner {
                                                        track_id, is_video: true, clip_id: ac.clip.id,
                                                        pos_v, scale, anchor_val, anchor_screen, pos,
                                                    });
                                                } else if pos.distance(anchor_screen) < 10.0 {
                                                    let scale = ac.clip.transform.get_scale(current);
                                                    hit = Some(HitResult::ClipAnchor {
                                                        track_id, is_video: true, clip_id: ac.clip.id,
                                                        pos_v, scale, anchor_val, anchor_screen, pos,
                                                    });
                                                }
                                            }
                                        }
                                        // Any clip corners/anchors.
                                        if hit.is_none() {
                                            for ac in active.iter().rev() {
                                                let bb = clip_screen_bounds_with_media(
                                                    &ac.clip, ac.transform_matrix,
                                                    &self.canvas_transform, state,
                                                );
                                                let track_id = seq.video_tracks
                                                    .get(ac.track_index)
                                                    .map(|t| t.id)
                                                    .unwrap_or_default();
                                                let pos_v = ac.clip.transform.get_position(current);
                                                let anchor_val = ac.clip.transform.get_anchor_point(current);
                                                let anchor_screen = self.canvas_transform.seq_to_screen(pos_v.x, pos_v.y);
                                                if is_near_corner(bb, pos, 14.0) {
                                                    let scale = ac.clip.transform.get_scale(current);
                                                    hit = Some(HitResult::ClipCorner {
                                                        track_id, is_video: true, clip_id: ac.clip.id,
                                                        pos_v, scale, anchor_val, anchor_screen, pos,
                                                    });
                                                    break;
                                                } else if pos.distance(anchor_screen) < 10.0 {
                                                    let scale = ac.clip.transform.get_scale(current);
                                                    hit = Some(HitResult::ClipAnchor {
                                                        track_id, is_video: true, clip_id: ac.clip.id,
                                                        pos_v, scale, anchor_val, anchor_screen, pos,
                                                    });
                                                    break;
                                                }
                                            }
                                        }
                                    }

                                    // Level 3: Mask outline/interior (only if no precise hit).
                                    if hit.is_none() {
                                        'l3: for ac in active.iter().rev() {
                                            if ac.clip.masks.is_empty() { continue; }
                                            let (mw, mh) = state.asset_library.as_ref()
                                                .and_then(|lib| lib.get_asset(ac.clip.asset_id).ok().flatten())
                                                .and_then(|a| a.media_info.primary_video().cloned())
                                                .map(|v| (v.width as f32, v.height as f32))
                                                .unwrap_or((1.0, 1.0));
                                            for mask in ac.clip.masks.iter().rev() {
                                                if !mask.enabled { continue; }
                                                let kf = mask.evaluate_at(ticks);
                                                let (corner_hit, area_hit) = mask_hit_test(
                                                    &kf.shape, mw, mh, &ac.transform_matrix,
                                                    &self.canvas_transform, pos,
                                                );
                                                if corner_hit.is_some() || area_hit {
                                                    let track_id = seq.video_tracks
                                                        .get(ac.track_index)
                                                        .map(|t| t.id)
                                                        .unwrap_or_default();
                                                    hit = Some(HitResult::MaskMove {
                                                        track_id, clip_id: ac.clip.id, mask_id: mask.id,
                                                        shape: kf.shape.clone(), pos,
                                                    });
                                                    break 'l3;
                                                }
                                            }
                                        }
                                    }

                                    // Level 4: Clip interior (lowest).
                                    if hit.is_none() {
                                        for ac in active.iter().rev() {
                                            let bb = clip_screen_bounds_with_media(
                                                &ac.clip, ac.transform_matrix,
                                                &self.canvas_transform, state,
                                            );
                                            if bb.is_some_and(|r| r.contains(pos)) {
                                                let track_id = seq.video_tracks
                                                    .get(ac.track_index)
                                                    .map(|t| t.id)
                                                    .unwrap_or_default();
                                                let pos_v = ac.clip.transform.get_position(current);
                                                let scale = ac.clip.transform.get_scale(current);
                                                let anchor_val = ac.clip.transform.get_anchor_point(current);
                                                let anchor_screen = self.canvas_transform.seq_to_screen(pos_v.x, pos_v.y);
                                                hit = Some(HitResult::ClipMove {
                                                    track_id, is_video: true, clip_id: ac.clip.id,
                                                    pos_v, scale, anchor_val, anchor_screen, pos,
                                                });
                                                break;
                                            }
                                        }
                                    }

                                    // Apply the hit.
                                    match hit {
                                        Some(HitResult::MaskCorner { track_id, clip_id, mask_id, corner, shape, pos }) => {
                                            let sel = (track_id, true, clip_id);
                                            self.set_clip_selection(state, Some(sel));
                                            self.selected_mask = Some((mask_id, clip_id, track_id));
                                            let edit_mode = if let MaskShape::Path { points, .. } = &shape {
                                                path_point_edit_mode(state, &self.canvas_transform, clip_id, points, corner, pos)
                                                    .unwrap_or(MaskEditMode::MovePathPoint(corner))
                                            } else {
                                                MaskEditMode::ResizeCorner(corner)
                                            };
                                            self.mask_edit = Some(MaskEditState {
                                                mask_id, clip_id, track_id,
                                                mode: edit_mode,
                                                start_shape: shape,
                                                start_mouse: pos,
                                            });
                                        }
                                        Some(HitResult::MaskMove { track_id, clip_id, mask_id, shape, pos }) => {
                                            let sel = (track_id, true, clip_id);
                                            self.set_clip_selection(state, Some(sel));
                                            self.selected_mask = Some((mask_id, clip_id, track_id));
                                            self.mask_edit = Some(MaskEditState {
                                                mask_id, clip_id, track_id,
                                                mode: MaskEditMode::Move,
                                                start_shape: shape,
                                                start_mouse: pos,
                                            });
                                        }
                                        Some(HitResult::ClipCorner { track_id, is_video, clip_id, pos_v, scale, anchor_val, anchor_screen, pos }) => {
                                            let sel = (track_id, is_video, clip_id);
                                            self.set_clip_selection(state, Some(sel));
                                            self.mask_edit = None;
                                            self.selected_mask = None;
                                            self.canvas_drag = Some(CanvasDragState {
                                                track_id, is_video, clip_id,
                                                mode: DragMode::Scale,
                                                start_pos: pos_v,
                                                start_scale: scale,
                                                start_anchor_val: anchor_val,
                                                start_anchor: anchor_screen,
                                                start_mouse: pos,
                                            });
                                        }
                                        Some(HitResult::ClipAnchor { track_id, is_video, clip_id, pos_v, scale, anchor_val, anchor_screen, pos }) => {
                                            let sel = (track_id, is_video, clip_id);
                                            self.set_clip_selection(state, Some(sel));
                                            self.mask_edit = None;
                                            self.selected_mask = None;
                                            self.canvas_drag = Some(CanvasDragState {
                                                track_id, is_video, clip_id,
                                                mode: DragMode::Anchor,
                                                start_pos: pos_v,
                                                start_scale: scale,
                                                start_anchor_val: anchor_val,
                                                start_anchor: anchor_screen,
                                                start_mouse: pos,
                                            });
                                        }
                                        Some(HitResult::ClipMove { track_id, is_video, clip_id, pos_v, scale, anchor_val, anchor_screen, pos }) => {
                                            let sel = (track_id, is_video, clip_id);
                                            self.set_clip_selection(state, Some(sel));
                                            self.mask_edit = None;
                                            self.selected_mask = None;
                                            self.canvas_drag = Some(CanvasDragState {
                                                track_id, is_video, clip_id,
                                                mode: DragMode::Move,
                                                start_pos: pos_v,
                                                start_scale: scale,
                                                start_anchor_val: anchor_val,
                                                start_anchor: anchor_screen,
                                                start_mouse: pos,
                                            });
                                        }
                                        None => {
                                            self.set_clip_selection(state, None);
                                            self.selected_mask = None;
                                        }
                                    }
                                } // end if mask_tool.is_none()
                            }
                        }
                    }
                }

            }

            let painter = ui.painter_at(canvas_rect);

                if state.sequence.is_none() {
                    self.invalidate_pending_decode();
                    self.desired_signature = None;
                    self.asset_preview_cache.clear();
                    draw_checkerboard(&painter, canvas_rect);
                    draw_empty_canvas_meta(&painter, canvas_rect, state, current_frame);
                }

                if let Some(seq) = state.sequence.as_ref() {
                    if let Some(lib) = state.asset_library.as_ref() {
                        let resolution = seq.settings.resolution;
                        // Decode at fixed resolution based on canvas slot, not zoom.
                        // Zoom is purely a display transform.
                        let (target_width, target_height) = playback_adjusted_target_size(
                            sequence_preview_target_size(
                                resolution,
                                canvas_rect.width().max(1.0),
                                canvas_rect.height().max(1.0),
                                self.preview_scale_mode.factor(),
                            ),
                            is_playing,
                        );

                        let layers_started_at = Instant::now();
                        let layers =
                            self.build_render_elements(seq, lib.as_ref(), state, current_frame);
                        if diag_enabled {
                            let elapsed_ms = layers_started_at.elapsed().as_millis() as u64;
                            if elapsed_ms >= preview_diag_slow_threshold_ms() {
                                tracing::warn!(
                                    "[preview-diag] build_render_elements slow: {}ms frame={} layers={}",
                                    elapsed_ms,
                                    current_frame,
                                    layers.len()
                                );
                            }
                        }
                        let layer_signatures =
                            layers.iter().map(render_element_signature).collect::<Vec<_>>();

                        if layers.is_empty() {
                            self.invalidate_pending_decode();
                            self.preview_texture = None;
                            self.preview_signature = None;
                            self.desired_signature = None;
                            self.preview_error = None;
                            self.clear_prefetch_in_flight();
                            let visible = content_rect
                                .intersect(self.canvas_transform.canvas_rect);
                            draw_checkerboard(&painter, visible);
                        } else {
                            let signature = CompositeFrameSignature {
                                width: target_width,
                                height: target_height,
                                working_color_space: seq.settings.color_space,
                                output_color_space: ColorSpace::Rec709,
                                display_profile_key: self.display_profile.signature_hash(),
                                tone_map: seq.settings.auto_tone_map_media,
                                layers: layer_signatures,
                            };
                            self.desired_signature = Some(signature.clone());

                            if self.preview_signature.as_ref() != Some(&signature) {
                                if let Some(cached) = self.cache_get(&signature) {
                                    self.preview_texture = Some(cached);
                                    self.preview_signature = Some(signature.clone());
                                    self.preview_error = None;
                                } else {
                                    let engine =
                                        if seq.settings.color_management.inherit {
                                            state.project_settings.color_management.engine.clone()
                                        } else {
                                            seq.settings.color_management.engine.clone()
                                        };
                                    self.request_decode(
                                        DecodeRequest {
                                            signature,
                                            layers,
                                            working_color_space: seq.settings.color_space,
                                            output_color_space: ColorSpace::Rec709,
                                            engine,
                                            display_profile: self.display_profile.clone(),
                                            ocio_display: self.ocio_display.clone(),
                                            ocio_view: self.ocio_view.clone(),
                                            seq_width: resolution.width,
                                            seq_height: resolution.height,
                                            tone_map: seq.settings.auto_tone_map_media,
                                            playback_mode: is_playing,
                                            target_width,
                                            target_height,
                                            layer_cache_enabled: self.layer_cache_enabled,
                                            layer_cache: Arc::clone(&self.layer_frame_cache),
                                            decoder_pool: Arc::clone(&self.decoder_pool),
                                            generation: 0,
                                            latest_generation: Arc::clone(
                                                &self.latest_decode_generation,
                                            ),
                                        },
                                        is_playing,
                                    );
                                }
                            }

                            if self.prefetch_allowed_for_target(
                                target_width,
                                target_height,
                                is_playing,
                            ) {
                                self.schedule_prefetch(
                                    seq,
                                    lib.as_ref(),
                                    state,
                                    current_frame,
                                    self.prefetch_direction(current_frame),
                                    target_width,
                                    target_height,
                                    is_playing,
                                );
                            }

                            if let Some(texture) = &self.preview_texture {
                                // Render only the visible portion of content_rect,
                                // adjusting UV to clip correctly when zoomed in.
                                let visible =
                                    content_rect.intersect(self.canvas_transform.canvas_rect);
                                let cr = content_rect;
                                let uv = Rect::from_min_max(
                                    Pos2::new(
                                        (visible.left() - cr.left()) / cr.width(),
                                        (visible.top() - cr.top()) / cr.height(),
                                    ),
                                    Pos2::new(
                                        (visible.right() - cr.left()) / cr.width(),
                                        (visible.bottom() - cr.top()) / cr.height(),
                                    ),
                                );
                                painter.image(texture.id(), visible, uv, palette::image_tint());
                            }
                            // When no texture yet, just show the background (no checkerboard).

                            if let Some(err) = &self.preview_error {
                                painter.text(
                                    canvas_rect.center_bottom() + Vec2::new(0.0, -14.0),
                                    egui::Align2::CENTER_BOTTOM,
                                    format!("预览解码失败：{}", err),
                                    typography::body(),
                                    palette::status_error(),
                                );
                            }
                            draw_mask_overlays(&painter, &self.canvas_transform, state, current_frame, self.selected_mask);

                            // Mask creation preview during drag.
                            if let (Some(tool), Some(ref md)) = (self.mask_tool, &self.mask_draw) {
                                if let Some(pos) = ui.ctx().input(|inp| inp.pointer.interact_pos()) {
                                    if let Some(cur_seq) = self.canvas_transform.screen_to_seq(pos) {
                                        let (x1, y1) = (md.start_seq.x.min(cur_seq.0), md.start_seq.y.min(cur_seq.1));
                                        let (x2, y2) = (md.start_seq.x.max(cur_seq.0), md.start_seq.y.max(cur_seq.1));
                                        let preview_color = egui::Color32::from_rgb(0, 200, 255);
                                        let stroke = egui::Stroke::new(2.0, preview_color);
                                        match tool {
                                            MaskTool::Rect => {
                                                let corners = [
                                                    glam::Vec2::new(x1, y1),
                                                    glam::Vec2::new(x2, y1),
                                                    glam::Vec2::new(x2, y2),
                                                    glam::Vec2::new(x1, y2),
                                                ];
                                                draw_mask_preview_polygon(&painter, &corners, &self.canvas_transform, stroke, true);
                                            }
                                            MaskTool::Ellipse => {
                                                let center = glam::Vec2::new((x1 + x2) * 0.5, (y1 + y2) * 0.5);
                                                let radii = glam::Vec2::new((x2 - x1) * 0.5, (y2 - y1) * 0.5);
                                                let n = 64usize;
                                                let mut pts = Vec::with_capacity(n + 1);
                                                for s in 0..=n {
                                                    let a = s as f32 * std::f32::consts::TAU / n as f32;
                                                    pts.push(glam::Vec2::new(
                                                        center.x + radii.x * a.cos(),
                                                        center.y + radii.y * a.sin(),
                                                    ));
                                                }
                                                draw_mask_preview_polygon(&painter, &pts, &self.canvas_transform, stroke, false);
                                            }
                                            MaskTool::Pen => {
                                                let preview_color = egui::Color32::from_rgb(0, 200, 255);
                                                let curve_stroke = egui::Stroke::new(2.0, preview_color);
                                                let handle_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgba_premultiplied(0, 180, 220, 120));
                                                let ghost_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgba_premultiplied(0, 200, 255, 100));
                                                let to_scr = |p: glam::Vec2| self.canvas_transform.seq_to_screen(p.x, p.y);

                                                // Build display points with in-progress drag point (if any).
                                                let mut display_pts: Vec<mondrian_effects::mask::BezierPoint> = md.pen_points.clone();
                                                let mut has_temp_pt = false;
                                                if let Some(drag_start) = md.pen_drag_start {
                                                    if let Some(cur_seq) = self.canvas_transform.screen_to_seq(pos) {
                                                        let end_pt = glam::Vec2::new(cur_seq.0, cur_seq.1);
                                                        let delta = end_pt - drag_start;
                                                        if delta.length() > 3.0 {
                                                            let h = delta * 0.37;
                                                            display_pts.push(mondrian_effects::mask::BezierPoint {
                                                                position: drag_start,
                                                                control_in: -h,
                                                                control_out: h,
                                                            });
                                                        } else {
                                                            display_pts.push(mondrian_effects::mask::BezierPoint::new(drag_start));
                                                        }
                                                        has_temp_pt = true;
                                                    }
                                                }

                                                // Draw committed curve segments.
                                                if display_pts.len() >= 2 {
                                                    let segs = mask_path_segments(&display_pts, false);
                                                    for &(a, b) in &segs {
                                                        painter.line_segment([to_scr(a), to_scr(b)], curve_stroke);
                                                    }
                                                }

                                                // ── Rubber band: preview curve from last point to cursor ──
                                                if !has_temp_pt && !display_pts.is_empty() {
                                                    if let Some(cur_seq) = self.canvas_transform.screen_to_seq(pos) {
                                                        let cur_pt = glam::Vec2::new(cur_seq.0, cur_seq.1);
                                                        let last = display_pts.last().unwrap();
                                                        // Build a 2-point path with last committed + cursor.
                                                        let mut rb_pts = vec![*last, mondrian_effects::mask::BezierPoint::new(cur_pt)];
                                                        // If last has control_out, preview with it.
                                                        if last.control_out.length_squared() > 0.01 {
                                                            rb_pts[0] = *last;
                                                        }
                                                        let segs = mask_path_segments(&rb_pts, false);
                                                        for &(a, b) in &segs {
                                                            painter.line_segment([to_scr(a), to_scr(b)], ghost_stroke);
                                                        }
                                                        // Dot at cursor.
                                                        let cursor_scr = to_scr(cur_pt);
                                                        painter.circle_filled(cursor_scr, 3.0, egui::Color32::from_rgba_premultiplied(0, 200, 255, 100));
                                                    }
                                                } else if display_pts.len() == 1 && !has_temp_pt {
                                                    // Single point, no drag: ghost line from first point to cursor.
                                                    if let Some(cur_seq) = self.canvas_transform.screen_to_seq(pos) {
                                                        if let Some(first) = display_pts.first() {
                                                            painter.line_segment(
                                                                [to_scr(first.position), to_scr(glam::Vec2::new(cur_seq.0, cur_seq.1))],
                                                                ghost_stroke,
                                                            );
                                                        }
                                                    }
                                                }

                                                // Draw anchor points and control handles for all points (including temp).
                                                for (i, bp) in display_pts.iter().enumerate() {
                                                    let is_temp = has_temp_pt && i == display_pts.len() - 1;
                                                    let sp = to_scr(bp.position);
                                                    let alpha: u8 = if is_temp { 140 } else { 255 };
                                                    let pt_color = egui::Color32::from_rgba_premultiplied(preview_color.r(), preview_color.g(), preview_color.b(), alpha);
                                                    painter.rect_filled(
                                                        egui::Rect::from_center_size(sp, egui::vec2(7.0, 7.0)),
                                                        1.0,
                                                        pt_color,
                                                    );
                                                    if bp.control_in.length_squared() > 0.01 {
                                                        let cp = to_scr(bp.position + bp.control_in);
                                                        painter.line_segment([sp, cp], handle_stroke);
                                                        painter.circle_filled(cp, 2.5, pt_color);
                                                    }
                                                    if bp.control_out.length_squared() > 0.01 {
                                                        let cp = to_scr(bp.position + bp.control_out);
                                                        painter.line_segment([sp, cp], handle_stroke);
                                                        painter.circle_filled(cp, 2.5, pt_color);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            // Transform handles for canvas-selected clip.
                            if let Some((_tid, _is_vid, clip_id)) = self.canvas_selected_clip {
                                draw_transform_handles(
                                    &painter, state, &self.canvas_transform, clip_id,
                                );
                            }

                            // Safe margins overlay.
                            if let Some(seq) = state.sequence.as_ref() {
                                draw_safe_margins(&painter, &self.canvas_transform, seq);
                            }

                            // ── Selection label overlay ──
                            draw_selection_labels(
                                &painter, &self.canvas_transform, state, current_frame,
                                self.canvas_selected_clip, self.selected_mask,
                            );

                        }
                    } else {
                        self.invalidate_pending_decode();
                        self.preview_texture = None;
                        self.preview_signature = None;
                        self.desired_signature = None;
                        self.preview_error = None;
                        self.clear_prefetch_in_flight();
                        let visible = content_rect
                            .intersect(self.canvas_transform.canvas_rect);
                        draw_checkerboard(&painter, visible);
                        draw_empty_canvas_meta(&painter, visible, state, current_frame);
                    }
                }

                if show_dev_metrics {
                    self.draw_dev_metrics_overlay(
                        ui,
                        canvas_rect,
                        state,
                        show_video_metrics,
                        show_audio_metrics,
                        show_preview_perf_metrics,
                    );
                }

                if self.show_color_diagnostics {
                    if let Some(seq) = state.sequence.as_ref() {
                        self.draw_color_diagnostics_overlay(ui, canvas_rect, seq, current_frame);
                    }
                }

            let controls_rect = Rect::from_min_size(
                Pos2::new(canvas_slot_rect.left(), canvas_rect.bottom() + transport_gap),
                Vec2::new(canvas_slot_rect.width(), controls_height.max(28.0)),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(controls_rect), |ui| {
                self.draw_transport_bar(ui, state, current_frame, is_playing);
            });
        });

        state.set_playback_buffering(self.playback_buffering_requested(is_playing));

        self.was_playing_last_frame = is_playing;
        self.last_timeline_frame = Some(current_frame);

        if diag_enabled {
            let elapsed_ms = show_started_at.elapsed().as_millis() as u64;
            if elapsed_ms >= preview_diag_slow_threshold_ms() {
                tracing::warn!(
                    "[preview-diag] show slow: {}ms frame={} playing={} in_flight={} queued={} tex={} prefetch_tasks={}",
                    elapsed_ms,
                    current_frame,
                    is_playing,
                    self.decode_in_flight,
                    self.queued_request.is_some(),
                    self.preview_texture.is_some(),
                    self.prefetch_tasks.len()
                );
            }
        }

        // Sync mask selection to AppState for effect controls panel.
        state.selection.selected_mask = self.selected_mask;

        // Change cursor for pen tool.
        if self.mask_tool == Some(MaskTool::Pen) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
    }

    fn poll_proxy_events(&mut self, ctx: &egui::Context) {
        let mut done_assets = Vec::new();
        while let Ok(asset_id) = self.proxy_done_rx.try_recv() {
            done_assets.push(asset_id);
        }

        if !done_assets.is_empty() {
            let path_cache = global_media_path_cache(self.proxy_config.cache_dir.join("index"));
            for asset_id in done_assets {
                path_cache.invalidate_asset(asset_id);
            }
            self.invalidate_pending_decode();
            ctx.request_repaint();
        }
    }

    fn playback_buffering_requested(&self, is_playing: bool) -> bool {
        if !is_playing {
            return false;
        }

        // 仅在播放中且预取处于 buffering 模式时请求短暂停留，
        // 避免刚进入播放时的瞬时状态误触发。
        self.playback_prefetch_buffering
            && (self.preview_texture.is_none()
                || self.decode_in_flight
                || self.queued_request.is_some())
    }

    fn draw_dev_metrics_overlay(
        &self,
        ui: &mut Ui,
        canvas_rect: Rect,
        state: &AppState,
        show_video_metrics: bool,
        show_audio_metrics: bool,
        show_preview_perf_metrics: bool,
    ) {
        let mut sections: Vec<String> = vec![format!("A/V 漂移: {:.1} ms", state.av_drift_ms)];

        if show_video_metrics {
            sections.push(
                self.developer_metrics_summary_with_options(show_preview_perf_metrics)
                    .replace(" | ", "\n"),
            );
        }

        if show_audio_metrics {
            sections.push(state.audio_developer_metrics_summary().replace(" | ", "\n"));
        }

        let text = sections.join("\n\n");
        let margin = 10.0;
        let max_w = (canvas_rect.width() * 0.46).max(320.0).min(canvas_rect.width() - margin * 2.0);
        let overlay_rect = Rect::from_min_size(
            canvas_rect.min + Vec2::new(margin, margin),
            Vec2::new(max_w.max(120.0), (canvas_rect.height() * 0.55).max(120.0)),
        );

        ui.painter().rect_filled(
            overlay_rect,
            tokens::section_rounding(),
            palette::overlay_fill(),
        );
        ui.painter().rect_stroke(
            overlay_rect,
            tokens::section_rounding(),
            egui::Stroke::new(tokens::border_standard(), palette::overlay_stroke()),
            egui::StrokeKind::Inside,
        );

        ui.scope_builder(
            egui::UiBuilder::new().max_rect(overlay_rect.shrink(8.0)),
            |ui| {
                ui.set_clip_rect(overlay_rect.shrink(8.0));
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(text)
                            .font(typography::mono_small())
                            .color(palette::text_primary()),
                    )
                    .wrap(),
                );
            },
        );
    }

    fn draw_color_diagnostics_overlay(
        &self,
        ui: &mut Ui,
        canvas_rect: Rect,
        seq: &mondrian_timeline::sequence::Sequence,
        current_frame: i64,
    ) {
        let diagnostics = collect_timeline_color_diagnostics(
            seq,
            current_frame,
            seq.settings.color_management.output_color_space,
        );
        if diagnostics.is_empty() {
            return;
        }

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!(
            "工作空间: {:?}  输出: {:?}  色调映射: {}",
            seq.settings.color_space,
            seq.settings.color_management.output_color_space,
            if seq.settings.auto_tone_map_media {
                "是"
            } else {
                "否"
            },
        ));
        lines.push(format!(
            "引擎: {}  工作流: {:?}  元数据策略: {:?}",
            seq.settings.color_management.engine.name(),
            seq.settings.color_management.workflow,
            seq.settings.color_management.missing_metadata_policy,
        ));
        lines.push(String::new());

        for (i, d) in diagnostics.iter().take(8).enumerate() {
            let input = d
                .input_color_space_override
                .map(|cs| format!("{:?}", cs))
                .unwrap_or_else(|| "自动检测".to_string());
            lines.push(format!(
                "L{}  asset={}  in={}  work={:?}  out={:?}  tonemap={}",
                i + 1,
                d.asset_id,
                input,
                d.working_color_space,
                d.output_color_space,
                if d.tone_map { "✓" } else { "—" },
            ));
        }

        if diagnostics.len() > 8 {
            lines.push(format!("... 还有 {} 层", diagnostics.len() - 8));
        }

        let text = lines.join("\n");
        let margin = 10.0;
        let overlay_w = (canvas_rect.width() * 0.40).clamp(280.0, 420.0);
        let overlay_rect = Rect::from_min_size(
            canvas_rect.min + Vec2::new(canvas_rect.width() - overlay_w - margin, margin),
            Vec2::new(overlay_w, (canvas_rect.height() * 0.60).max(160.0)),
        );

        ui.painter().rect_filled(
            overlay_rect,
            tokens::section_rounding(),
            palette::overlay_fill(),
        );
        ui.painter().rect_stroke(
            overlay_rect,
            tokens::section_rounding(),
            egui::Stroke::new(tokens::border_standard(), palette::overlay_stroke()),
            egui::StrokeKind::Inside,
        );

        ui.scope_builder(
            egui::UiBuilder::new().max_rect(overlay_rect.shrink(8.0)),
            |ui| {
                ui.set_clip_rect(overlay_rect.shrink(8.0));
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(text)
                            .font(typography::mono_small())
                            .color(palette::text_primary()),
                    )
                    .wrap(),
                );
            },
        );
    }

    fn draw_transport_bar(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        current_frame: i64,
        is_playing: bool,
    ) {
        let row_rect = ui.max_rect();
        let content_rect = row_rect.shrink2(Vec2::new(0.0, 4.0));
        let row_h = content_rect.height().max(30.0);
        let left_w = 160.0;
        let right_w = 170.0;

        let left_rect = Rect::from_min_size(content_rect.left_top(), Vec2::new(left_w, row_h));
        let right_rect = Rect::from_min_size(
            Pos2::new(
                (content_rect.right() - right_w).max(content_rect.left()),
                content_rect.top(),
            ),
            Vec2::new(right_w, row_h),
        );
        let center_rect = Rect::from_min_max(
            Pos2::new(
                left_rect.right().min(content_rect.right()),
                content_rect.top(),
            ),
            Pos2::new(
                right_rect.left().max(content_rect.left()),
                content_rect.bottom(),
            ),
        );

        let fps = state
            .sequence
            .as_ref()
            .map(|s| s.settings.frame_rate)
            .unwrap_or(Rational::new(24, 1));
        let tc = TimeCode::new(current_frame, Rational::new(fps.den.max(1), fps.num.max(1)));

        ui.scope_builder(egui::UiBuilder::new().max_rect(left_rect), |ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(tc.to_smpte())
                        .font(typography::mono_small())
                        .color(palette::text_primary()),
                );
                ui.add_space(6.0);
                let selected_zoom = self.canvas_transform.zoom_mode.display_label();
                egui::ComboBox::from_id_salt("viewer_zoom")
                    .width(60.0)
                    .selected_text(&selected_zoom)
                    .show_ui(ui, |ui| {
                        let items: &[(&str, f32)] = &[
                            ("Fit", -1.0),
                            ("10%", 0.1),
                            ("25%", 0.25),
                            ("50%", 0.5),
                            ("100%", 1.0),
                            ("200%", 2.0),
                            ("400%", 4.0),
                        ];
                        for &(label, zoom) in items {
                            if ui.selectable_label(false, label).clicked() {
                                if zoom < 0.0 {
                                    self.canvas_transform.set_zoom_mode(
                                        crate::ui::viewer::canvas::CanvasZoomMode::Fit,
                                    );
                                } else {
                                    self.canvas_transform.set_zoom_mode(
                                        crate::ui::viewer::canvas::CanvasZoomMode::Fixed(zoom),
                                    );
                                }
                            }
                        }
                    });
            });
        });

        ui.scope_builder(egui::UiBuilder::new().max_rect(center_rect), |ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let btn_w = 34.0;
                let btn_h = tokens::playback_button_height();
                let mark_w = tokens::playback_marker_width();
                let gap = tokens::playback_button_gap();
                let group_w = mark_w * 2.0 + btn_w * 5.0 + gap * 6.0;
                let left_pad = ((center_rect.width() - group_w) * 0.5).max(0.0);

                ui.add_space(left_pad);
                ui.spacing_mut().item_spacing.x = gap;

                if ui
                    .add_sized([mark_w, btn_h], egui::Button::new("I"))
                    .on_hover_text("标记入点 (I)")
                    .clicked()
                {
                    state.mark_in_at_current_frame();
                }
                if ui
                    .add_sized([mark_w, btn_h], egui::Button::new("O"))
                    .on_hover_text("标记出点 (O)")
                    .clicked()
                {
                    state.mark_out_at_current_frame();
                }

                if theme::icon_button(ui, [btn_w, btn_h], theme::UiIcon::JumpStart).clicked() {
                    state.jump_to_start_frame();
                }
                if theme::icon_button(ui, [btn_w, btn_h], theme::UiIcon::StepBack).clicked() {
                    state.step_prev_frame();
                }
                if theme::icon_button(
                    ui,
                    [btn_w, btn_h],
                    if is_playing {
                        theme::UiIcon::Pause
                    } else {
                        theme::UiIcon::Play
                    },
                )
                .clicked()
                {
                    if is_playing {
                        state.pause();
                    } else {
                        state.play();
                    }
                }
                if theme::icon_button(ui, [btn_w, btn_h], theme::UiIcon::StepForward).clicked() {
                    state.step_next_frame();
                }
                if theme::icon_button(ui, [btn_w, btn_h], theme::UiIcon::JumpEnd).clicked() {
                    state.jump_to_end_frame();
                }
            });
        });

        ui.scope_builder(egui::UiBuilder::new().max_rect(right_rect), |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let combo_id = ui.make_persistent_id("viewer_res");
                let combo_open = egui::Popup::is_id_open(ui.ctx(), combo_id)
                    || egui::Popup::is_id_open(ui.ctx(), combo_id.with("popup"));

                theme::with_minimal_dropdown(ui, combo_open, |ui| {
                    egui::ComboBox::from_id_salt("viewer_res")
                        .selected_text(egui::RichText::new(self.preview_scale_mode.label()).color(
                            if combo_open {
                                palette::text_primary()
                            } else {
                                palette::text_muted()
                            },
                        ))
                        .show_ui(ui, |ui| {
                            ui.set_min_width(112.0);
                            theme::checkmark_selectable_value(
                                ui,
                                &mut self.preview_scale_mode,
                                PreviewScaleMode::Full,
                                PreviewScaleMode::Full.label(),
                            );
                            theme::checkmark_selectable_value(
                                ui,
                                &mut self.preview_scale_mode,
                                PreviewScaleMode::Half,
                                PreviewScaleMode::Half.label(),
                            );
                            theme::checkmark_selectable_value(
                                ui,
                                &mut self.preview_scale_mode,
                                PreviewScaleMode::Quarter,
                                PreviewScaleMode::Quarter.label(),
                            );
                        });
                });
            });
        });
    }

    fn request_decode(&mut self, request: DecodeRequest, is_playing: bool) {
        const SCRUB_DECODE_MIN_INTERVAL_MS: u64 = 14;
        const PLAYBACK_DECODE_MIN_INTERVAL_MS: u64 = 22;

        if self.decode_in_flight_signature.as_ref() == Some(&request.signature) {
            return;
        }

        if self.queued_request.as_ref().map(|r| &r.signature) == Some(&request.signature) {
            return;
        }

        let mut request = request;
        request.generation = self.next_decode_generation;
        self.next_decode_generation = self.next_decode_generation.saturating_add(1);
        self.latest_decode_generation.store(request.generation, Ordering::Relaxed);

        if let Some(last_submit) = self.last_decode_submit_at {
            let min_interval = if is_playing {
                Duration::from_millis(PLAYBACK_DECODE_MIN_INTERVAL_MS)
            } else {
                Duration::from_millis(SCRUB_DECODE_MIN_INTERVAL_MS)
            };
            if Instant::now().saturating_duration_since(last_submit) < min_interval {
                if preview_diag_enabled() {
                    tracing::debug!(
                        "[preview-diag] queue decode (throttle): gen={} playing={} target={}x{} layers={}",
                        request.generation,
                        is_playing,
                        request.target_width,
                        request.target_height,
                        request.layers.len()
                    );
                }
                self.queued_request = Some(request);
                return;
            }
        }

        if self.decode_in_flight {
            if preview_diag_enabled() {
                tracing::debug!(
                    "[preview-diag] queue decode (in-flight): gen={} target={}x{} layers={}",
                    request.generation,
                    request.target_width,
                    request.target_height,
                    request.layers.len()
                );
            }
            self.queued_request = Some(request);
            return;
        }

        self.start_decode(request);
    }

    fn start_decode(&mut self, request: DecodeRequest) {
        if preview_diag_enabled() {
            tracing::debug!(
                "[preview-diag] start decode: gen={} playing={} target={}x{} layers={}",
                request.generation,
                request.playback_mode,
                request.target_width,
                request.target_height,
                request.layers.len()
            );
        }
        self.decode_in_flight = true;
        self.decode_in_flight_since = Some(Instant::now());
        self.decode_in_flight_signature = Some(request.signature.clone());
        self.last_decode_submit_at = Some(Instant::now());
        if self.decode_request_tx.send(request).is_err() {
            self.decode_in_flight = false;
            self.decode_in_flight_since = None;
            self.decode_in_flight_signature = None;
        }
    }

    fn poll_decode_results(&mut self, ctx: &egui::Context, is_playing: bool, playback_fps: f64) {
        const STALE_FALLBACK_MAX_AGE_MS: u64 = 180;
        const PLAYBACK_MAX_GENERATION_LAG: u64 = 8;

        let mut stale_candidate: Option<DecodeResult> = None;
        let mut latest_current: Option<DecodeResult> = None;
        let mut playback_fallback: Option<DecodeResult> = None;

        while let Ok(result) = self.decode_rx.try_recv() {
            self.decode_in_flight = false;
            self.decode_in_flight_since = None;
            self.decode_in_flight_signature = None;

            let latest_generation = self.latest_decode_generation.load(Ordering::Relaxed);
            if result.generation != latest_generation {
                if is_playing
                    && self.desired_signature.as_ref() == Some(&result.signature)
                    && latest_generation.saturating_sub(result.generation)
                        <= PLAYBACK_MAX_GENERATION_LAG
                {
                    let replace = playback_fallback
                        .as_ref()
                        .map(|existing| existing.generation < result.generation)
                        .unwrap_or(true);
                    if replace {
                        playback_fallback = Some(result);
                    }
                    self.try_start_queued_decode();
                    continue;
                }

                if self.should_consider_stale_result(&result, STALE_FALLBACK_MAX_AGE_MS) {
                    let replace = stale_candidate
                        .as_ref()
                        .map(|existing| existing.generation < result.generation)
                        .unwrap_or(true);
                    if replace {
                        stale_candidate = Some(result);
                    }
                }

                self.try_start_queued_decode();
                continue;
            }

            latest_current = Some(result);
        }

        if let Some(result) = latest_current {
            self.enqueue_decode_commit(result, false);
        } else if let Some(result) = playback_fallback {
            self.enqueue_decode_commit(result, true);
        }

        if let Some(stale) = stale_candidate {
            if self.decoded_commit_queue.is_empty() {
                self.enqueue_decode_commit(stale, true);
            }
        }

        self.try_start_queued_decode();
        self.commit_queued_decode_result(ctx, is_playing, playback_fps);
    }

    fn enqueue_decode_commit(&mut self, result: DecodeResult, allow_stale: bool) {
        const MAX_COMMIT_QUEUE: usize = 8;
        self.decoded_commit_queue.push_back(PendingDecodeCommit { result, allow_stale });
        while self.decoded_commit_queue.len() > MAX_COMMIT_QUEUE {
            self.decoded_commit_queue.pop_front();
        }
    }

    fn commit_queued_decode_result(
        &mut self,
        ctx: &egui::Context,
        is_playing: bool,
        playback_fps: f64,
    ) {
        if self.decoded_commit_queue.is_empty() {
            return;
        }

        let playback_commit_min_interval_ms =
            (1000.0 / playback_fps.max(1.0)).round().clamp(10.0, 24.0) as u64;

        if is_playing && !self.can_commit_texture_now(playback_commit_min_interval_ms) {
            ctx.request_repaint_after(Duration::from_millis(4));
            return;
        }

        let Some(commit) = self.decoded_commit_queue.pop_back() else {
            return;
        };
        self.decoded_commit_queue.clear();
        self.apply_decode_result(ctx, commit.result, commit.allow_stale, is_playing);
    }

    fn can_commit_texture_now(&self, min_interval_ms: u64) -> bool {
        match self.last_texture_commit_at {
            Some(last) => {
                Instant::now().saturating_duration_since(last)
                    >= Duration::from_millis(min_interval_ms)
            }
            None => true,
        }
    }

    fn apply_decode_result(
        &mut self,
        ctx: &egui::Context,
        result: DecodeResult,
        allow_stale: bool,
        is_playing: bool,
    ) {
        if self
            .last_committed_generation
            .map(|last| result.generation < last)
            .unwrap_or(false)
        {
            return;
        }

        if !allow_stale
            && result.generation != self.latest_decode_generation.load(Ordering::Relaxed)
        {
            return;
        }

        self.decode_in_flight = false;
        self.decode_in_flight_since = None;
        self.decode_in_flight_signature = None;

        let can_apply = self.desired_signature.as_ref() == Some(&result.signature);

        if can_apply {
            match result.decoded {
                Ok(frame) => {
                    let upload_started_at = Instant::now();
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [frame.width as usize, frame.height as usize],
                        &frame.data,
                    );
                    if is_playing {
                        if let Some(texture) = self.preview_texture.as_mut() {
                            texture.set(image, egui::TextureOptions::LINEAR);
                        } else {
                            self.preview_texture = Some(ctx.load_texture(
                                "preview-live-stream",
                                image,
                                egui::TextureOptions::LINEAR,
                            ));
                        }
                    } else {
                        self.preview_texture = Some(ctx.load_texture(
                            format!(
                                "preview-composited-{}",
                                composite_signature_hash(&result.signature)
                            ),
                            image,
                            egui::TextureOptions::LINEAR,
                        ));
                        if let Some(texture) = self.preview_texture.clone() {
                            self.cache_put(result.signature.clone(), texture);
                        }
                    }
                    self.preview_signature = Some(result.signature);
                    self.preview_error = None;
                    self.last_committed_generation = Some(result.generation);
                    self.last_texture_commit_at = Some(Instant::now());
                    record_preview_perf_upload(upload_started_at.elapsed());
                }
                Err(err) => {
                    if !allow_stale {
                        self.preview_texture = None;
                        self.preview_signature = Some(result.signature);
                        self.preview_error = Some(err);
                    }
                }
            }
        }

        self.try_start_queued_decode();

        ctx.request_repaint();
    }

    fn try_start_queued_decode(&mut self) {
        if let Some(next) = self.queued_request.take() {
            if self.desired_signature.as_ref() == Some(&next.signature) {
                self.start_decode(next);
            }
        }
    }

    fn should_consider_stale_result(&self, result: &DecodeResult, stale_max_age_ms: u64) -> bool {
        if self.desired_signature.as_ref() != Some(&result.signature) {
            return false;
        }

        if self.preview_texture.is_none() {
            return true;
        }

        match self.last_texture_commit_at {
            Some(last) => {
                Instant::now().saturating_duration_since(last)
                    >= Duration::from_millis(stale_max_age_ms)
            }
            None => true,
        }
    }

    fn reset_commit_timing(&mut self) {
        self.decoded_commit_queue.clear();
        self.last_texture_commit_at = None;
    }

    fn reset_decode_runtime(&mut self) {
        self.decode_in_flight = false;
        self.decode_in_flight_since = None;
        self.decode_in_flight_signature = None;
        self.queued_request = None;
        self.last_decode_submit_at = None;
        self.reset_commit_timing();
    }

    fn recover_if_decode_stalled(&mut self, ctx: &egui::Context) {
        if !self.decode_in_flight {
            return;
        }

        let Some(started_at) = self.decode_in_flight_since else {
            return;
        };

        let timeout_ms = decode_stall_timeout_ms();
        let decode_budget_ms = decode_timeout_budget_ms();
        if timeout_ms == 0 {
            return;
        }

        if Instant::now().saturating_duration_since(started_at) >= Duration::from_millis(timeout_ms)
        {
            if preview_diag_enabled() {
                tracing::warn!(
                    "[preview-diag] decode stalled: {}ms in_flight={} queued={} forcing reset",
                    timeout_ms,
                    self.decode_in_flight,
                    self.queued_request.is_some()
                );
                tracing::warn!(
                    "MONDRIAN_DECODE_STALL_JSON={{\"stall_timeout_ms\":{},\"decode_budget_ms\":{},\"in_flight\":{},\"queued\":{},\"reason\":\"ui decode stall reset\"}}",
                    timeout_ms,
                    decode_budget_ms,
                    self.decode_in_flight,
                    self.queued_request.is_some()
                );
            }
            self.invalidate_pending_decode();
            ctx.request_repaint();
        }
    }

    fn prefetch_direction(&self, current_frame: i64) -> i64 {
        match self.last_timeline_frame {
            Some(prev) if current_frame < prev => -1,
            _ => 1,
        }
    }

    fn on_playback_stopped(&mut self) {
        self.last_decode_submit_at = None;
        self.decoded_commit_queue.clear();
        self.last_texture_commit_at = None;
        self.playback_prefill_until = None;
        self.playback_prefetch_buffering = false;
        self.clear_prefetch_in_flight();
    }

    fn on_playback_started(&mut self) {
        self.playback_prefill_until =
            Some(Instant::now() + Duration::from_millis(playback_prefill_duration_ms()));
        self.playback_prefetch_buffering = true;
        self.prefetch_resume_after = None;
        self.last_prefetch_target_size = None;
        self.clear_prefetch_in_flight();
    }

    fn playback_prefill_active(&self) -> bool {
        self.playback_prefill_until.map(|until| Instant::now() < until).unwrap_or(false)
    }

    fn handle_timeline_discontinuity(
        &mut self,
        current_frame: i64,
        is_playing: bool,
        playback_fps: f64,
    ) {
        // 跳帧 ≥ 300 帧（~12秒 @ 25fps）视为大跳帧，额外清理 DecoderPool 内 RGBA 缓存，
        // 防止大 seek 后旧缓存帧污染新位置的画面。
        const LARGE_SEEK_THRESHOLD_FRAMES: i64 = 300;

        let Some(previous_frame) = self.last_timeline_frame else {
            return;
        };

        let delta = (current_frame - previous_frame).abs();
        let seek_reset_threshold_frames = if is_playing {
            playback_seek_reset_threshold_frames(playback_fps)
        } else {
            8
        };
        if delta < seek_reset_threshold_frames {
            return;
        }

        self.invalidate_pending_decode();
        self.playback_prefetch_buffering = false;
        self.clear_prefetch_in_flight();
        self.decoder_pool.cancel_all_prefetch_tasks();

        if delta >= LARGE_SEEK_THRESHOLD_FRAMES {
            // 大幅 seek 时清除 DecoderPool 内 RGBA 图层帧缓存（按资源顺序淘汰），
            // 避免 seek 到新位置后先显示数分钟前的旧帧。
            self.decoder_pool.evict_rgba_cache();
        }
    }

    fn clear_prefetch_in_flight(&mut self) {
        self.prefetch_generation = self.prefetch_generation.saturating_add(1);

        for task in self.prefetch_tasks.values() {
            self.decoder_pool.cancel_prefetch_task(task.task_id);
        }
        self.prefetch_tasks.clear();

        if let Ok(mut guard) = self.prefetch_in_flight.lock() {
            guard.clear();
        }
    }

    fn prefetch_allowed_for_target(
        &mut self,
        target_width: u32,
        target_height: u32,
        is_playing: bool,
    ) -> bool {
        if !self.prefetch_enabled {
            if !self.prefetch_tasks.is_empty() {
                self.clear_prefetch_in_flight();
            }
            return false;
        }

        let target = (target_width, target_height);
        if self.last_prefetch_target_size != Some(target) {
            self.last_prefetch_target_size = Some(target);
            let delay_ms = if is_playing {
                if self.playback_prefill_active() {
                    0
                } else {
                    220
                }
            } else {
                700
            };
            self.prefetch_resume_after = Some(Instant::now() + Duration::from_millis(delay_ms));
            self.clear_prefetch_in_flight();
            return false;
        }

        if self.decode_in_flight {
            return false;
        }

        if self.preview_texture.is_none() && !is_playing {
            return false;
        }

        if let Some(resume_at) = self.prefetch_resume_after {
            if Instant::now() < resume_at {
                return false;
            }
            self.prefetch_resume_after = None;
        }

        true
    }

    fn invalidate_pending_decode(&mut self) {
        let generation = self.next_decode_generation;
        self.next_decode_generation = self.next_decode_generation.saturating_add(1);
        self.latest_decode_generation.store(generation, Ordering::Relaxed);
        self.reset_decode_runtime();
    }

    fn schedule_prefetch(
        &mut self,
        seq: &mondrian_timeline::sequence::Sequence,
        lib: &mondrian_assets::AssetLibrary,
        state: &AppState,
        current_frame: i64,
        direction: i64,
        target_width: u32,
        target_height: u32,
        is_playing: bool,
    ) {
        let direction = if direction == 0 {
            1
        } else {
            direction.signum()
        };

        let mut layer_request_cache: HashMap<i64, Arc<Vec<RenderElement>>> = HashMap::new();

        let prefill_active = is_playing && self.playback_prefill_active();

        let active_layer_count = self
            .build_render_elements_cached(seq, lib, state, current_frame, &mut layer_request_cache)
            .len();
        let mut budget = prefetch_budget(active_layer_count, is_playing);

        if is_playing {
            let fps = seq.settings.frame_rate.to_f64().max(1.0);
            let target_frames = playback_prefetch_target_frames(fps);
            let hysteresis_frames = playback_prefetch_hysteresis_frames(fps)
                .min(target_frames.saturating_sub(1))
                .max(1);
            let low_watermark = target_frames.saturating_sub(hysteresis_frames).max(1);

            let ready_frames = self.playback_prefetch_ready_frames(
                seq,
                lib,
                state,
                current_frame,
                direction,
                target_width,
                target_height,
                target_frames,
                &mut layer_request_cache,
            );

            if self.playback_prefetch_buffering {
                if ready_frames >= target_frames {
                    self.playback_prefetch_buffering = false;
                    return;
                }
            } else {
                if ready_frames >= low_watermark {
                    return;
                }
                self.playback_prefetch_buffering = true;
            }

            budget.frames_ahead = budget.frames_ahead.max(target_frames);
            budget.max_in_flight =
                budget.max_in_flight.max((target_frames / 2).clamp(8, 24) as usize);
            budget.max_spawn_per_tick = budget.max_spawn_per_tick.max(6);
        }

        if prefill_active {
            budget.frames_ahead = (budget.frames_ahead + 4).min(64);
            budget.max_in_flight = (budget.max_in_flight + 4).min(24);
            budget.max_spawn_per_tick = (budget.max_spawn_per_tick + 3).min(10);
        }

        if !is_playing
            && self.idle_prefetch_coverage_ready(
                seq,
                lib,
                state,
                current_frame,
                target_width,
                target_height,
                budget.frames_ahead,
                &mut layer_request_cache,
            )
        {
            return;
        }

        self.prefetch_tasks.retain(|_, info| {
            info.generation == self.prefetch_generation
                && self.decoder_pool.is_prefetch_task_active(info.task_id)
        });

        let stale_distance = if is_playing {
            budget.frames_ahead * 2
        } else {
            budget.frames_ahead * 3
        };
        let mut stale_keys = Vec::new();
        for (key, info) in &self.prefetch_tasks {
            let wrong_direction = is_playing && info.direction != direction;
            let too_far = (info.target_frame - current_frame).abs() > stale_distance;
            if wrong_direction || too_far {
                self.decoder_pool.cancel_prefetch_task(info.task_id);
                stale_keys.push(key.clone());
            }
        }
        for key in stale_keys {
            self.prefetch_tasks.remove(&key);
        }

        let mut spawned_this_tick = 0usize;
        let chunk_len = if is_playing {
            if prefill_active {
                1
            } else {
                4
            }
        } else {
            6
        };

        for offset in prefetch_offsets(budget.frames_ahead, direction, is_playing, chunk_len) {
            let timeline_frame = current_frame + offset;
            if timeline_frame < 0 {
                continue;
            }
            let layers = self.build_render_elements_cached(
                seq,
                lib,
                state,
                timeline_frame,
                &mut layer_request_cache,
            );

            for layer in layers.iter() {
                let RenderElement::Media(layer) = layer else {
                    continue;
                };
                let cache_key = LayerFrameCacheKey {
                    asset_id: layer.frame_key.0,
                    source_frame: layer.frame_key.1,
                    source_time_base: layer.source_time_base,
                    target_width,
                    target_height,
                    input_color_space: layer.input_color_space,
                    working_color_space: layer.working_color_space,
                    engine: layer.engine.clone(),
                    tone_map: layer.tone_map,
                };

                if let Some(existing) = self.prefetch_tasks.get(&cache_key).cloned() {
                    if existing.generation == self.prefetch_generation
                        && self.decoder_pool.is_prefetch_task_active(existing.task_id)
                    {
                        continue;
                    }
                    self.prefetch_tasks.remove(&cache_key);
                }

                if self.layer_cache_enabled
                    && layer_cache_get(&self.layer_frame_cache, &cache_key).is_some()
                {
                    continue;
                }

                let mut guard = match self.prefetch_in_flight.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };

                if guard.len() >= budget.max_in_flight {
                    return;
                }
                if !guard.insert(cache_key.clone()) {
                    continue;
                }
                drop(guard);

                spawned_this_tick += 1;
                if spawned_this_tick >= budget.max_spawn_per_tick {
                    return;
                }

                let task_id = self.decoder_pool.spawn_prefetch_rgba(
                    layer.frame_key.0,
                    layer.path.clone(),
                    TimeCode::new(layer.frame_key.1, layer.source_time_base),
                    chunk_len as u32,
                    target_width,
                    target_height,
                );
                self.prefetch_tasks.insert(
                    cache_key,
                    PrefetchTaskInfo {
                        task_id,
                        generation: self.prefetch_generation,
                        direction: offset.signum(),
                        target_frame: timeline_frame,
                    },
                );
            }
        }
    }

    fn build_render_elements(
        &mut self,
        seq: &mondrian_timeline::sequence::Sequence,
        lib: &mondrian_assets::AssetLibrary,
        state: &AppState,
        timeline_frame: i64,
    ) -> Vec<RenderElement> {
        self.build_render_elements_with_depth(seq, lib, state, timeline_frame, 0)
    }

    fn build_render_elements_with_depth(
        &mut self,
        seq: &mondrian_timeline::sequence::Sequence,
        lib: &mondrian_assets::AssetLibrary,
        state: &AppState,
        timeline_frame: i64,
        depth: usize,
    ) -> Vec<RenderElement> {
        if depth > 16 {
            return Vec::new();
        }

        let mut layers: Vec<RenderElement> = Vec::new();

        for plan in build_timeline_render_plan(seq, timeline_frame) {
            match plan {
                TimelineRenderPlanElement::Adjustment(adjustment) => {
                    layers.push(RenderElement::Adjustment(AdjustmentRenderRequest {
                        effect_graph: adjustment.effect_graph,
                        opacity: adjustment.opacity,
                        blend_mode: Some(adjustment.blend_mode),
                        frame_seed: adjustment.frame_seed,
                    }));
                }
                TimelineRenderPlanElement::SolidColor(solid) => {
                    layers.push(RenderElement::SolidColor(SolidColorRenderRequest {
                        color: solid.color,
                        opacity: solid.opacity,
                        blend_mode: solid.blend_mode,
                        transform: solid.transform,
                        effect_graph: solid.effect_graph,
                        frame_seed: solid.frame_seed,
                    }));
                }
                TimelineRenderPlanElement::Media(media) => {
                    let asset_id = media.asset_id;
                    let cached = if let Some(hit) = self.asset_preview_cache.get(&asset_id) {
                        hit.clone()
                    } else {
                        let loaded = lib.get_asset(asset_id).ok().flatten().map(|asset| {
                            let color_space = asset
                                .media_info
                                .primary_video()
                                .map(|video| video.color_space)
                                .unwrap_or(ColorSpace::Rec709);
                            CachedAssetPreview {
                                is_video: matches!(asset.kind, mondrian_assets::AssetKind::Video),
                                source_path: asset.path,
                                color_space,
                            }
                        });
                        self.asset_preview_cache.insert(asset_id, loaded.clone());
                        loaded
                    };

                    let Some(asset) = cached else {
                        continue;
                    };
                    if !asset.is_video {
                        continue;
                    }

                    let path = self.resolve_preview_source_path(
                        asset_id,
                        asset.source_path.as_path(),
                        state.is_asset_proxy_mode(asset_id),
                    );
                    let layer_engine = if seq.settings.color_management.inherit {
                        state.project_settings.color_management.engine.clone()
                    } else {
                        seq.settings.color_management.engine.clone()
                    };
                    // Resolve input color space respecting the missing-metadata policy.
                    let input_color_space = match media.color_space_override {
                        Some(cs) => cs,
                        None => {
                            let policy = seq.settings.color_management.missing_metadata_policy;
                            match policy {
                                MissingColorMetadataPolicy::AssumeRec709 => asset.color_space,
                                MissingColorMetadataPolicy::AssumeSequenceWorkingSpace => {
                                    seq.settings.color_space
                                }
                                MissingColorMetadataPolicy::RejectMedia => {
                                    tracing::warn!(
                                        asset_id = %asset_id,
                                        "素材缺少色彩元数据，已按项目策略跳过渲染"
                                    );
                                    continue;
                                }
                            }
                        }
                    };
                    layers.push(RenderElement::Media(LayerDecodeRequest {
                        frame_key: (asset_id, media.source_frame),
                        path,
                        input_color_space,
                        working_color_space: seq.settings.color_space,
                        engine: layer_engine,
                        tone_map: seq.settings.auto_tone_map_media,
                        source_secs: media.source_secs,
                        source_time_base: media.source_time_base,
                        opacity: media.opacity,
                        blend_mode: media.blend_mode,
                        transform: media.transform,
                        effect_graph: media.effect_graph,
                        frame_seed: media.frame_seed,
                    }));
                }
                TimelineRenderPlanElement::NestedSequence(nested) => {
                    let Some(nested_sequence) = state.sequence_by_id(nested.sequence_id).cloned()
                    else {
                        continue;
                    };
                    let nested_frame = TimeCode::from_secs(
                        nested.source_secs,
                        nested_sequence.settings.frame_rate,
                    )
                    .frame
                    .max(0);
                    let child_layers = self.build_render_elements_with_depth(
                        &nested_sequence,
                        lib,
                        state,
                        nested_frame,
                        depth + 1,
                    );
                    let nested_engine = if nested_sequence.settings.color_management.inherit {
                        // Use parent's effective engine (already resolved for inherit).
                        if seq.settings.color_management.inherit {
                            state.project_settings.color_management.engine.clone()
                        } else {
                            seq.settings.color_management.engine.clone()
                        }
                    } else {
                        nested_sequence.settings.color_management.engine.clone()
                    };
                    layers.push(RenderElement::NestedSequence(NestedSequenceRenderRequest {
                        sequence_id: nested.sequence_id,
                        source_frame: nested_frame,
                        width: nested_sequence.settings.resolution.width.max(1),
                        height: nested_sequence.settings.resolution.height.max(1),
                        nested_processing: nested.nested_processing,
                        working_color_space: nested_sequence.settings.color_space,
                        engine: nested_engine,
                        tone_map: nested_sequence.settings.auto_tone_map_media,
                        opacity: nested.opacity,
                        blend_mode: nested.blend_mode,
                        transform: nested.transform,
                        effect_graph: nested.effect_graph,
                        frame_seed: nested.frame_seed,
                        layers: child_layers,
                    }));
                }
            }
        }

        layers
    }

    fn build_render_elements_cached(
        &mut self,
        seq: &mondrian_timeline::sequence::Sequence,
        lib: &mondrian_assets::AssetLibrary,
        state: &AppState,
        timeline_frame: i64,
        request_cache: &mut HashMap<i64, Arc<Vec<RenderElement>>>,
    ) -> Arc<Vec<RenderElement>> {
        if let Some(cached) = request_cache.get(&timeline_frame) {
            return Arc::clone(cached);
        }

        let layers = Arc::new(self.build_render_elements(seq, lib, state, timeline_frame));
        request_cache.insert(timeline_frame, Arc::clone(&layers));
        layers
    }

    fn idle_prefetch_coverage_ready(
        &mut self,
        seq: &mondrian_timeline::sequence::Sequence,
        lib: &mondrian_assets::AssetLibrary,
        state: &AppState,
        current_frame: i64,
        target_width: u32,
        target_height: u32,
        frames_ahead: i64,
        layer_request_cache: &mut HashMap<i64, Arc<Vec<RenderElement>>>,
    ) -> bool {
        let offsets = prefetch_offsets(
            frames_ahead,
            self.prefetch_direction(current_frame),
            false,
            6,
        );
        if offsets.is_empty() {
            return false;
        }

        let mut total = 0usize;
        let mut ready = 0usize;

        for offset in offsets {
            let timeline_frame = current_frame + offset;
            if timeline_frame < 0 {
                continue;
            }

            let layers = self.build_render_elements_cached(
                seq,
                lib,
                state,
                timeline_frame,
                layer_request_cache,
            );
            for layer in layers.iter() {
                let RenderElement::Media(layer) = layer else {
                    continue;
                };
                total += 1;
                let key = LayerFrameCacheKey {
                    asset_id: layer.frame_key.0,
                    source_frame: layer.frame_key.1,
                    source_time_base: layer.source_time_base,
                    target_width,
                    target_height,
                    input_color_space: layer.input_color_space,
                    working_color_space: layer.working_color_space,
                    engine: layer.engine.clone(),
                    tone_map: layer.tone_map,
                };

                let in_cache = layer_cache_get(&self.layer_frame_cache, &key).is_some();
                let in_flight = self.prefetch_tasks.contains_key(&key);
                if in_cache || in_flight {
                    ready += 1;
                }
            }
        }

        if total == 0 {
            return false;
        }

        let coverage = ready as f64 / total as f64;
        coverage >= 0.85
    }

    fn playback_prefetch_ready_frames(
        &mut self,
        seq: &mondrian_timeline::sequence::Sequence,
        lib: &mondrian_assets::AssetLibrary,
        state: &AppState,
        current_frame: i64,
        direction: i64,
        target_width: u32,
        target_height: u32,
        target_frames: i64,
        layer_request_cache: &mut HashMap<i64, Arc<Vec<RenderElement>>>,
    ) -> i64 {
        let mut ready_frames = 0i64;

        for step in 1..=target_frames.max(1) {
            let timeline_frame = current_frame + step * direction;
            if timeline_frame < 0 {
                break;
            }

            let layers = self.build_render_elements_cached(
                seq,
                lib,
                state,
                timeline_frame,
                layer_request_cache,
            );
            if layers.is_empty() {
                break;
            }

            let frame_ready = layers.iter().all(|layer| {
                let RenderElement::Media(layer) = layer else {
                    return true;
                };
                let key = LayerFrameCacheKey {
                    asset_id: layer.frame_key.0,
                    source_frame: layer.frame_key.1,
                    source_time_base: layer.source_time_base,
                    target_width,
                    target_height,
                    input_color_space: layer.input_color_space,
                    working_color_space: layer.working_color_space,
                    engine: layer.engine.clone(),
                    tone_map: layer.tone_map,
                };

                let in_cache =
                    self.layer_cache_enabled && layer_cache_contains(&self.layer_frame_cache, &key);
                let in_flight = self.prefetch_tasks.contains_key(&key);
                in_cache || in_flight
            });

            if !frame_ready {
                break;
            }

            ready_frames += 1;
        }

        ready_frames
    }

    fn resolve_preview_source_path(
        &mut self,
        asset_id: AssetId,
        source_path: &Path,
        proxy_mode: bool,
    ) -> PathBuf {
        if !proxy_mode {
            return source_path.to_path_buf();
        }

        let started_at = Instant::now();

        let proxy_generator = mondrian_media::ProxyGenerator::new(self.proxy_config.clone());
        let path_cache = global_media_path_cache(self.proxy_config.cache_dir.join("index"));
        let resolved =
            path_cache.resolve_playback_path(asset_id, source_path, proxy_mode, &proxy_generator);

        if preview_diag_enabled() {
            let elapsed_ms = started_at.elapsed().as_millis() as u64;
            if elapsed_ms >= preview_diag_slow_threshold_ms() {
                tracing::warn!(
                    "[preview-diag] resolve_preview_source_path slow: {}ms asset={} proxy_hit={}",
                    elapsed_ms,
                    asset_id,
                    resolved.is_proxy
                );
            }
        }

        if proxy_mode && !resolved.is_proxy {
            self.enqueue_proxy_generation(asset_id, source_path.to_path_buf());
        }

        resolved.path
    }

    fn enqueue_proxy_generation(&mut self, asset_id: AssetId, source_path: PathBuf) {
        {
            let mut guard = match self.proxy_jobs_in_flight.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            if !guard.insert(asset_id) {
                return;
            }
        }

        let config = self.proxy_config.clone();
        let in_flight = Arc::clone(&self.proxy_jobs_in_flight);
        let done_tx = self.proxy_done_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
            match runtime {
                Ok(rt) => {
                    let generator = mondrian_media::ProxyGenerator::new(config);
                    let (progress_tx, _progress_rx) = tokio::sync::mpsc::channel(8);
                    let _ = rt.block_on(generator.generate(asset_id, source_path, progress_tx));
                }
                Err(err) => {
                    tracing::error!("创建代理任务 runtime 失败: {}", err);
                }
            }

            if let Ok(mut guard) = in_flight.lock() {
                guard.remove(&asset_id);
            }
            let _ = done_tx.send(asset_id);
        });
    }

    fn cache_get(&mut self, key: &CompositeFrameSignature) -> Option<egui::TextureHandle> {
        if let Some(index) = self.texture_cache.iter().position(|(k, _)| k == key) {
            if let Some((entry_key, tex)) = self.texture_cache.remove(index) {
                let cloned = tex.clone();
                self.texture_cache.push_front((entry_key, tex));
                return Some(cloned);
            }
        }
        None
    }

    fn cache_put(&mut self, key: CompositeFrameSignature, tex: egui::TextureHandle) {
        if let Some(index) = self.texture_cache.iter().position(|(k, _)| *k == key) {
            self.texture_cache.remove(index);
        }
        self.texture_cache.push_front((key, tex));
        while self.texture_cache.len() > 24 {
            self.texture_cache.pop_back();
        }
    }

    pub fn developer_metrics_summary(&self) -> String {
        self.developer_metrics_summary_with_options(true)
    }

    pub fn developer_metrics_summary_with_options(&self, include_preview_perf: bool) -> String {
        let metrics = self.decoder_pool.metrics_snapshot();
        let yuv_hit_rate = if metrics.yuv_requests > 0 {
            metrics.yuv_cache_hits as f64 / metrics.yuv_requests as f64 * 100.0
        } else {
            0.0
        };
        let rgba_hit_rate = if metrics.rgba_requests > 0 {
            metrics.rgba_cache_hits as f64 / metrics.rgba_requests as f64 * 100.0
        } else {
            0.0
        };

        let base = format!(
            "Dec Y:{:.0}% RGBA:{:.0}% ReqAvg:{:.2}ms DecAvg:{:.2}ms Miss:{:.0}% Backend:{} Pref(active/start/done/cancel:{}/{}/{}/{}) Err:{}",
            yuv_hit_rate,
            rgba_hit_rate,
            metrics.avg_decode_ms,
            metrics.avg_decode_exec_ms,
            metrics.decode_miss_rate_pct,
            match self.decode_backend {
                mondrian_media::PreviewDecodeBackend::Auto => "auto",
                mondrian_media::PreviewDecodeBackend::Software => "cpu",
                mondrian_media::PreviewDecodeBackend::GpuAssist => "gpu-assist",
            },
            self.decoder_pool.active_prefetch_task_count(),
            metrics.prefetch_started,
            metrics.prefetch_completed,
            metrics.prefetch_cancelled,
            metrics.decode_failures,
        );

        if include_preview_perf {
            if let Some(snapshot) = preview_perf_snapshot() {
                return format!(
                    "{} | Perf1s f(d/c:{}/{}) ms(d/l/c/u:{:.1}/{:.1}/{:.1}/{:.1}) share(l/c/u:{}/{}/{}) hit:{}% pass:{}% comp_gpu:{}",
                    base,
                    snapshot.decode_frames,
                    snapshot.commit_frames,
                    snapshot.avg_decode_ms,
                    snapshot.avg_layer_ms,
                    snapshot.avg_comp_ms,
                    snapshot.avg_upload_ms,
                    snapshot.layer_share_pct,
                    snapshot.comp_share_pct,
                    snapshot.upload_share_pct,
                    snapshot.hit_rate_pct,
                    snapshot.passthrough_rate_pct,
                    if snapshot.gpu_comp_on { "on" } else { "off" }
                );
            }
        }

        base
    }

    pub fn prefetch_enabled(&self) -> bool {
        self.prefetch_enabled
    }

    pub fn set_prefetch_enabled(&mut self, enabled: bool) {
        self.prefetch_enabled = enabled;
        if !enabled {
            self.clear_prefetch_in_flight();
        }
    }

    pub fn layer_cache_enabled(&self) -> bool {
        self.layer_cache_enabled
    }

    pub fn set_layer_cache_enabled(&mut self, enabled: bool) {
        self.layer_cache_enabled = enabled;
        if !enabled {
            layer_cache_clear(&self.layer_frame_cache);
        }
    }

    pub fn clear_layer_cache(&mut self) {
        layer_cache_clear(&self.layer_frame_cache);
    }

    pub fn preferences_snapshot(&self) -> ViewerPreferences {
        ViewerPreferences {
            preview_scale_mode: self.preview_scale_mode,
            proxy_config: self.proxy_config.clone(),
            decode_backend: self.decode_backend,
            prefetch_enabled: self.prefetch_enabled,
            layer_cache_enabled: self.layer_cache_enabled,
            display_profile: self.display_profile.clone(),
            canvas_bg_hex: self.canvas_bg_hex,
        }
    }

    pub fn apply_preferences(&mut self, preferences: &ViewerPreferences) {
        self.preview_scale_mode = preferences.preview_scale_mode;
        self.proxy_config = preferences.proxy_config.clone();
        self.decode_backend = preferences.decode_backend;
        mondrian_media::set_preview_decode_backend(preferences.decode_backend);
        self.set_prefetch_enabled(preferences.prefetch_enabled);
        self.set_layer_cache_enabled(preferences.layer_cache_enabled);
        self.canvas_bg_hex = preferences.canvas_bg_hex;
        self.canvas_transform.background = hex_to_bg(self.canvas_bg_hex);
        if preferences.display_profile.validate().is_ok() {
            self.display_profile = preferences.display_profile.clone();
        }
    }

    pub fn display_profile_snapshot(&self) -> DisplayColorProfile {
        self.display_profile.clone()
    }

    pub fn set_display_profile(&mut self, profile: DisplayColorProfile) {
        if profile.validate().is_ok() {
            self.display_profile = profile;
        }
    }

    pub fn preview_decode_backend(&self) -> mondrian_media::PreviewDecodeBackend {
        self.decode_backend
    }

    pub fn set_preview_decode_backend(&mut self, backend: mondrian_media::PreviewDecodeBackend) {
        self.decode_backend = backend;
        mondrian_media::set_preview_decode_backend(backend);
    }

    pub fn run_media_cache_maintenance(
        &mut self,
        policy: MediaCachePolicy,
    ) -> anyhow::Result<MediaCacheCleanupStats> {
        run_media_cache_maintenance_for_dir(self.proxy_config.cache_dir.clone(), policy)
    }

    pub fn clear_all_media_cache(&mut self) -> anyhow::Result<MediaCacheCleanupStats> {
        let cache_dir = self.proxy_config.cache_dir.clone();
        let mut stats = MediaCacheCleanupStats::default();

        if cache_dir.exists() {
            let entries = list_cache_files(&cache_dir)?;
            stats.deleted_files = entries.len();
            stats.deleted_bytes = entries.iter().map(|e| e.size).sum();
            std::fs::remove_dir_all(&cache_dir)?;
        }

        std::fs::create_dir_all(&cache_dir)?;

        self.texture_cache.clear();
        layer_cache_clear(&self.layer_frame_cache);
        self.clear_prefetch_in_flight();
        self.decoder_pool.clear_all_caches();

        let path_cache = global_media_path_cache(self.proxy_config.cache_dir.join("index"));
        path_cache.clear_l1();

        Ok(stats)
    }

    pub fn media_cache_usage_stats(&self) -> anyhow::Result<MediaCacheUsageStats> {
        let cache_dir = self.proxy_config.cache_dir.clone();
        if !cache_dir.exists() {
            return Ok(MediaCacheUsageStats::default());
        }

        let entries = list_cache_files(&cache_dir)?;
        Ok(MediaCacheUsageStats {
            file_count: entries.len(),
            total_bytes: entries.iter().map(|entry| entry.size).sum(),
        })
    }
}

pub fn run_media_cache_maintenance_for_dir(
    cache_dir: PathBuf,
    policy: MediaCachePolicy,
) -> anyhow::Result<MediaCacheCleanupStats> {
    if !cache_dir.exists() {
        return Ok(MediaCacheCleanupStats::default());
    }

    let mut entries = list_cache_files(&cache_dir)?;
    let mut stats = MediaCacheCleanupStats::default();

    if policy.max_age_days > 0 {
        let max_age = Duration::from_secs(policy.max_age_days.saturating_mul(24 * 60 * 60));
        let now = std::time::SystemTime::now();
        for entry in &entries {
            if let Ok(age) = now.duration_since(entry.modified) {
                if age > max_age && std::fs::remove_file(&entry.path).is_ok() {
                    stats.deleted_files += 1;
                    stats.deleted_bytes = stats.deleted_bytes.saturating_add(entry.size);
                }
            }
        }
        entries = list_cache_files(&cache_dir)?;
    }

    let mut total_size: u64 = entries.iter().map(|e| e.size).sum();
    if policy.max_size_bytes > 0 && total_size > policy.max_size_bytes {
        entries.sort_by_key(|e| e.modified);
        for entry in entries {
            if total_size <= policy.max_size_bytes {
                break;
            }
            if std::fs::remove_file(&entry.path).is_ok() {
                stats.deleted_files += 1;
                stats.deleted_bytes = stats.deleted_bytes.saturating_add(entry.size);
                total_size = total_size.saturating_sub(entry.size);
            }
        }
    }

    Ok(stats)
}

#[derive(Debug, Clone, Copy)]
struct PrefetchBudget {
    frames_ahead: i64,
    max_in_flight: usize,
    max_spawn_per_tick: usize,
}

fn prefetch_budget(active_layer_count: usize, is_playing: bool) -> PrefetchBudget {
    if !is_playing {
        return if active_layer_count >= 8 {
            PrefetchBudget {
                frames_ahead: 8,
                max_in_flight: 8,
                max_spawn_per_tick: 3,
            }
        } else {
            PrefetchBudget {
                frames_ahead: 12,
                max_in_flight: 12,
                max_spawn_per_tick: 4,
            }
        };
    }

    if active_layer_count >= 10 {
        PrefetchBudget {
            frames_ahead: 2,
            max_in_flight: 6,
            max_spawn_per_tick: 3,
        }
    } else if active_layer_count >= 6 {
        PrefetchBudget {
            frames_ahead: 3,
            max_in_flight: 8,
            max_spawn_per_tick: 4,
        }
    } else {
        PrefetchBudget {
            frames_ahead: 4,
            max_in_flight: 10,
            max_spawn_per_tick: 5,
        }
    }
}

#[derive(Debug, Clone)]
struct CacheFileEntry {
    path: PathBuf,
    size: u64,
    modified: std::time::SystemTime,
}

fn list_cache_files(root: &Path) -> anyhow::Result<Vec<CacheFileEntry>> {
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();

    while let Some(dir) = stack.pop() {
        let read_dir = std::fs::read_dir(&dir)?;
        for entry in read_dir {
            let entry = entry?;
            let path = entry.path();
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            if metadata.is_dir() {
                stack.push(path);
                continue;
            }

            if !metadata.is_file() {
                continue;
            }

            files.push(CacheFileEntry {
                path,
                size: metadata.len(),
                modified: metadata.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            });
        }
    }

    Ok(files)
}

fn prefetch_offsets(
    frames_ahead: i64,
    direction: i64,
    is_playing: bool,
    chunk_len: i64,
) -> Vec<i64> {
    let chunk_len = chunk_len.max(1);

    if is_playing {
        let mut offsets = Vec::new();
        let mut offset = 1;
        while offset <= frames_ahead {
            offsets.push(offset * direction);
            offset += chunk_len;
        }
        return offsets;
    }

    let mut offsets = Vec::new();
    let mut offset = 1;
    while offset <= frames_ahead {
        offsets.push(offset);
        offsets.push(-offset);
        offset += chunk_len;
    }
    offsets
}

fn composite_signature_hash(signature: &CompositeFrameSignature) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    signature.hash(&mut hasher);
    hasher.finish()
}

fn render_element_signature(layer: &RenderElement) -> LayerSignature {
    match layer {
        RenderElement::Media(layer) => LayerSignature::Media {
            asset_id: layer.frame_key.0,
            source_frame: layer.frame_key.1,
            source_time_base: layer.source_time_base,
            opacity_u8: (layer.opacity * 255.0).round() as u8,
            blend_mode: layer.blend_mode,
            transform_key: quantize_transform_signature(layer.transform),
            frame_seed: layer.frame_seed,
            effect_hash: layer.effect_graph.signature_hash,
            input_color_space: layer.input_color_space,
            working_color_space: layer.working_color_space,
            tone_map: layer.tone_map,
        },
        RenderElement::Adjustment(layer) => LayerSignature::Adjustment {
            opacity_u8: (layer.opacity * 255.0).round() as u8,
            blend_mode: layer.blend_mode,
            frame_seed: layer.frame_seed,
            effect_hash: layer.effect_graph.signature_hash,
        },
        RenderElement::SolidColor(layer) => LayerSignature::SolidColor {
            color_bits: [
                layer.color.r.to_bits(),
                layer.color.g.to_bits(),
                layer.color.b.to_bits(),
                layer.color.a.to_bits(),
            ],
            opacity_u8: (layer.opacity * 255.0).round() as u8,
            blend_mode: layer.blend_mode,
            transform_key: quantize_transform_signature(layer.transform),
            frame_seed: layer.frame_seed,
            effect_hash: layer.effect_graph.signature_hash,
        },
        RenderElement::NestedSequence(layer) => {
            let child_hash = {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                for child in &layer.layers {
                    render_element_signature(child).hash(&mut hasher);
                }
                hasher.finish()
            };
            LayerSignature::NestedSequence {
                sequence_id: layer.sequence_id,
                source_frame: layer.source_frame,
                opacity_u8: (layer.opacity * 255.0).round() as u8,
                blend_mode: layer.blend_mode,
                transform_key: quantize_transform_signature(layer.transform),
                frame_seed: layer.frame_seed,
                effect_hash: layer.effect_graph.signature_hash,
                child_hash,
                nested_processing: layer.nested_processing,
                engine: layer.engine.clone(),
            }
        }
    }
}

fn decode_composited_rgba(request: &DecodeRequest) -> anyhow::Result<RgbaFrame> {
    let decode_started_at = Instant::now();
    if request.generation != request.latest_generation.load(Ordering::Relaxed) {
        return Err(anyhow::anyhow!("decode cancelled by newer generation"));
    }

    // Ensure the color engine is ready.
    if let Err(e) = request.engine.ensure_loaded() {
        tracing::warn!("色彩引擎加载失败: {}", e);
    }

    let width = request.target_width.max(1);
    let height = request.target_height.max(1);

    let mut decoded_layers = 0usize;
    let mut last_error: Option<anyhow::Error> = None;

    if request.layers.is_empty() {
        return Ok(RgbaFrame {
            width,
            height,
            data: vec![0u8; width as usize * height as usize * 4],
        });
    }

    let playback_mode = request.playback_mode;
    let layer_cache_enabled = request.layer_cache_enabled;
    let decode_generation = request.generation;
    let latest_generation = Arc::clone(&request.latest_generation);
    let layer_cache = Arc::clone(&request.layer_cache);
    let decoder_pool = Arc::clone(&request.decoder_pool);

    // Decode at native resolution — the transform maps canvas→media-native
    // coordinates, so the decoded frame must be at the source's native size.
    let decode_w = u32::MAX;
    let decode_h = u32::MAX;
    let layer_outputs = preview_decode_pool().install(|| {
        request
            .layers
            .par_iter()
            .cloned()
            .enumerate()
            .filter_map(|(index, layer)| {
                let RenderElement::Media(layer) = layer else {
                    return None;
                };
                if decode_generation != latest_generation.load(Ordering::Relaxed) {
                    return Some((
                        index,
                        Err("decode cancelled by newer generation".to_string()),
                    ));
                }

                let decoded = decode_layer_rgba(
                    &layer,
                    decode_w,
                    decode_h,
                    playback_mode,
                    layer_cache_enabled,
                    &layer_cache,
                    &decoder_pool,
                )
                .map_err(|e| e.to_string());
                Some((index, decoded))
            })
            .collect::<Vec<_>>()
    });

    let nested_outputs = request
        .layers
        .iter()
        .cloned()
        .enumerate()
        .filter_map(|(index, layer)| {
            let RenderElement::NestedSequence(layer) = layer else {
                return None;
            };
            if decode_generation != latest_generation.load(Ordering::Relaxed) {
                return Some((
                    index,
                    Err("decode cancelled by newer generation".to_string()),
                ));
            }
            let parent_context = ColorContext {
                working_color_space: request.working_color_space,
                output_color_space: request.output_color_space,
                tone_map: request.tone_map,
                workflow: ColorWorkflow::DisplayReferred,
                nested_processing: layer.nested_processing,
                engine: request.engine.clone(),
                missing_metadata_policy: MissingColorMetadataPolicy::default(),
                ocio_display: None,
                ocio_view: None,
            };
            let nested_context = match layer.nested_processing {
                NestedColorProcessing::PreserveChildWorkingSpace => ColorContext {
                    working_color_space: layer.working_color_space,
                    output_color_space: parent_context.working_color_space,
                    tone_map: layer.tone_map,
                    workflow: parent_context.workflow,
                    nested_processing: layer.nested_processing,
                    engine: layer.engine.clone(),
                    missing_metadata_policy: parent_context.missing_metadata_policy,
                    ocio_display: parent_context.ocio_display.clone(),
                    ocio_view: parent_context.ocio_view.clone(),
                },
                NestedColorProcessing::ForceParentWorkingSpace => ColorContext {
                    working_color_space: parent_context.working_color_space,
                    output_color_space: parent_context.working_color_space,
                    tone_map: parent_context.tone_map,
                    workflow: parent_context.workflow,
                    nested_processing: layer.nested_processing,
                    engine: parent_context.engine.clone(),
                    missing_metadata_policy: parent_context.missing_metadata_policy,
                    ocio_display: parent_context.ocio_display.clone(),
                    ocio_view: parent_context.ocio_view.clone(),
                },
                NestedColorProcessing::BakeChildOutputTransform => ColorContext {
                    working_color_space: layer.working_color_space,
                    output_color_space: parent_context.working_color_space,
                    tone_map: layer.tone_map || parent_context.tone_map,
                    workflow: parent_context.workflow,
                    nested_processing: layer.nested_processing,
                    engine: layer.engine.clone(),
                    missing_metadata_policy: parent_context.missing_metadata_policy,
                    ocio_display: parent_context.ocio_display.clone(),
                    ocio_view: parent_context.ocio_view.clone(),
                },
            };
            let signature = CompositeFrameSignature {
                width: layer.width,
                height: layer.height,
                working_color_space: nested_context.working_color_space,
                output_color_space: nested_context.output_color_space,
                display_profile_key: 0,
                tone_map: nested_context.tone_map,
                layers: layer.layers.iter().map(render_element_signature).collect(),
            };
            let nested_request = DecodeRequest {
                signature,
                layers: layer.layers.clone(),
                working_color_space: nested_context.working_color_space,
                output_color_space: nested_context.output_color_space,
                engine: nested_context.engine.clone(),
                display_profile: DisplayColorProfile::rec709_reference(),
                ocio_display: nested_context.ocio_display.clone(),
                ocio_view: nested_context.ocio_view.clone(),
                tone_map: nested_context.tone_map,
                playback_mode,
                target_width: layer.width,
                target_height: layer.height,
                seq_width: layer.width,
                seq_height: layer.height,
                layer_cache_enabled,
                layer_cache: Arc::clone(&layer_cache),
                decoder_pool: Arc::clone(&decoder_pool),
                generation: decode_generation,
                latest_generation: Arc::clone(&latest_generation),
            };
            Some((
                index,
                decode_composited_rgba(&nested_request).map_err(|e| e.to_string()),
            ))
        })
        .collect::<Vec<_>>();

    let mut layer_results: Vec<Option<Result<RgbaFrame, String>>> =
        vec![None; request.layers.len()];
    let mut decoded_media_frames =
        std::iter::repeat_with(|| None).take(request.layers.len()).collect::<Vec<_>>();
    let mut decoded_nested_frames =
        std::iter::repeat_with(|| None).take(request.layers.len()).collect::<Vec<_>>();
    for (index, decoded) in layer_outputs {
        if index < layer_results.len() {
            layer_results[index] = Some(decoded);
        }
    }

    for (index, decoded) in nested_outputs {
        let Some(RenderElement::NestedSequence(layer)) = request.layers.get(index) else {
            continue;
        };
        match decoded {
            Ok(frame) => {
                decoded_nested_frames[index] = Some((layer.clone(), frame));
                decoded_layers += 1;
            }
            Err(err) => {
                last_error = Some(anyhow::anyhow!(
                    "nested sequence {}@{} 解码失败: {}",
                    layer.sequence_id,
                    layer.source_frame,
                    err
                ));
            }
        }
    }

    for (index, layer) in request.layers.iter().enumerate() {
        let RenderElement::Media(layer) = layer else {
            continue;
        };
        match layer_results.get_mut(index).and_then(Option::take) {
            Some(Ok(frame)) => {
                decoded_media_frames[index] = Some((layer.clone(), frame));
                decoded_layers += 1;
            }
            Some(Err(err)) => {
                last_error = Some(anyhow::anyhow!(
                    "{}@{} 解码失败: {}",
                    layer.frame_key.0,
                    layer.frame_key.1,
                    err
                ));
            }
            None => {
                last_error = Some(anyhow::anyhow!(
                    "{}@{} 解码失败: worker 未返回结果",
                    layer.frame_key.0,
                    layer.frame_key.1
                ));
            }
        }
    }

    if decoded_layers == 0 {
        if let Some(err) = last_error {
            tracing::debug!("预览合成回退到透明帧：{}", err);
        }
        return Ok(RgbaFrame {
            width,
            height,
            data: vec![0u8; width as usize * height as usize * 4],
        });
    }

    let has_cpu_only_ops = request.layers.iter().any(|layer| match layer {
        RenderElement::Media(layer) => {
            !layer.effect_graph.graph.is_identity()
                || !is_identity_transform(layer.transform)
                || layer.blend_mode != BlendMode::Normal
        }
        RenderElement::Adjustment(_) => true,
        RenderElement::SolidColor(_) => true,
        RenderElement::NestedSequence(_) => true,
    });

    if !has_cpu_only_ops && decoded_layers == 1 {
        let only_layer =
            decoded_media_frames.iter().flatten().next().expect("single layer should exist");
        let (_, frame) = only_layer;
        if frame.width == width && frame.height == height {
            record_preview_perf_passthrough_frame();
            record_preview_perf_decode_total(decode_started_at.elapsed());
            let mut data = frame.data.clone();
            apply_preview_output_color(&mut data, request);
            return Ok(RgbaFrame { width, height, data });
        }
    }

    if !has_cpu_only_ops {
        let rgba_layers_for_gpu = decoded_media_frames
            .iter()
            .flatten()
            .map(|(layer, frame)| CpuRgbaLayer {
                width: frame.width,
                height: frame.height,
                data: frame.data.clone(),
                opacity: layer.opacity,
            })
            .collect::<Vec<_>>();
        if let Some(gpu_rgba) = try_gpu_composite_rgba_layers(width, height, &rgba_layers_for_gpu) {
            record_preview_perf_decode_total(decode_started_at.elapsed());
            let mut data = gpu_rgba;
            apply_preview_output_color(&mut data, request);
            return Ok(RgbaFrame { width, height, data });
        }
    }

    let cpu_composite_started_at = Instant::now();
    // Convert transforms from sequence space to canvas space.
    // The compositor canvas may differ from the sequence resolution.
    let seq_to_canvas = (width as f32 / request.seq_width.max(1) as f32)
        .min(height as f32 / request.seq_height.max(1) as f32);
    let mut composite_elements = Vec::with_capacity(request.layers.len());
    for (index, layer) in request.layers.iter().enumerate() {
        match layer {
            RenderElement::Media(_) => {
                let Some((layer, frame)) = decoded_media_frames[index].as_ref() else {
                    continue;
                };
                composite_elements.push(TimelineCompositeElement::Media(TimelineMediaLayer {
                    rgba: &frame.data,
                    width: frame.width,
                    height: frame.height,
                    opacity: layer.opacity,
                    blend_mode: layer.blend_mode,
                    transform: scale_affine(layer.transform, seq_to_canvas),
                    effect_graph: std::sync::Arc::clone(&layer.effect_graph),
                    frame_seed: layer.frame_seed,
                }));
            }
            RenderElement::Adjustment(adjustment) => {
                composite_elements.push(TimelineCompositeElement::Adjustment(
                    TimelineAdjustmentLayer {
                        effect_graph: std::sync::Arc::clone(&adjustment.effect_graph),
                        opacity: adjustment.opacity,
                        blend_mode: adjustment.blend_mode,
                        frame_seed: adjustment.frame_seed,
                    },
                ));
            }
            RenderElement::SolidColor(solid) => {
                composite_elements.push(TimelineCompositeElement::SolidColor(
                    TimelineSolidColorLayer {
                        color: solid.color,
                        opacity: solid.opacity,
                        blend_mode: solid.blend_mode,
                        transform: scale_affine(solid.transform, seq_to_canvas),
                        effect_graph: std::sync::Arc::clone(&solid.effect_graph),
                        frame_seed: solid.frame_seed,
                    },
                ));
            }
            RenderElement::NestedSequence(_) => {
                let Some((layer, frame)) = decoded_nested_frames[index].as_ref() else {
                    continue;
                };
                composite_elements.push(TimelineCompositeElement::Media(TimelineMediaLayer {
                    rgba: &frame.data,
                    width: frame.width,
                    height: frame.height,
                    opacity: layer.opacity,
                    blend_mode: layer.blend_mode,
                    transform: scale_affine(layer.transform, seq_to_canvas),
                    effect_graph: std::sync::Arc::clone(&layer.effect_graph),
                    frame_seed: layer.frame_seed,
                }));
            }
        }
    }

    let mut scratch = TimelineCompositeScratch::default();
    let mut canvas = composite_timeline_elements_float_linear(
        width,
        height,
        &composite_elements,
        TimelineCompositeOptions { empty_canvas_transparent: true },
        request.working_color_space,
        &mut scratch,
    );
    apply_preview_output_color(&mut canvas, request);

    record_preview_perf_composite_ns(cpu_composite_started_at.elapsed().as_nanos() as u64, false);
    record_preview_perf_decode_total(decode_started_at.elapsed());

    Ok(RgbaFrame { width, height, data: canvas })
}

fn apply_preview_output_color(data: &mut [u8], request: &DecodeRequest) {
    convert_rgba8_in_place(
        data,
        ColorPipeline::new(
            request.working_color_space,
            request.working_color_space,
            request.output_color_space,
            request.tone_map,
        )
        .with_engine(request.engine.clone()),
    );

    // Use explicit display/view from viewer settings, or fall back to OCIO defaults.
    let ocio_defaults = mondrian_core::ocio_default_display_view();
    let display = request
        .ocio_display
        .as_deref()
        .or_else(|| ocio_defaults.as_ref().map(|(d, _)| d.as_str()));
    let view = request
        .ocio_view
        .as_deref()
        .or_else(|| ocio_defaults.as_ref().map(|(_, v)| v.as_str()));

    if let (Some(display), Some(view)) = (display, view) {
        if request
            .engine
            .display_transform(data, request.output_color_space, display, view)
            .is_ok()
        {
            return;
        }
    }

    if let Err(err) = apply_display_profile_rgba8_in_place(
        data,
        request.output_color_space,
        &request.display_profile,
        request.tone_map,
    ) {
        tracing::warn!("显示色彩配置无效，已跳过显示校准: {}", err);
    }
}

fn try_gpu_composite_rgba_layers(
    width: u32,
    height: u32,
    rgba_layers_for_gpu: &[CpuRgbaLayer],
) -> Option<Vec<u8>> {
    if !gpu_compositor_enabled() {
        return None;
    }

    let gpu_compositor = global_gpu_compositor()?;
    let gpu_composite_started_at = Instant::now();
    let gpu_result = {
        let mut guard = match gpu_compositor.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.composite_rgba_layers(width, height, rgba_layers_for_gpu)
    };

    match gpu_result {
        Ok(gpu_rgba) => {
            record_gpu_compositor_result(true);
            record_preview_perf_composite_ns(
                gpu_composite_started_at.elapsed().as_nanos() as u64,
                true,
            );
            Some(gpu_rgba)
        }
        Err(err) => {
            record_gpu_compositor_result(false);
            tracing::warn!("GPU 合成失败，回退 CPU 路径: {}", err);
            None
        }
    }
}

fn global_gpu_compositor() -> Option<&'static Mutex<FrameCompositor>> {
    static GPU_COMPOSITOR: OnceLock<Option<Mutex<FrameCompositor>>> = OnceLock::new();
    GPU_COMPOSITOR
        .get_or_init(|| {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
            let gpu = rt.block_on(GpuContext::new()).ok()?;
            Some(Mutex::new(FrameCompositor::new(
                gpu,
                CompositorConfig::default(),
            )))
        })
        .as_ref()
}

/// GPU compositor is available if the global singleton was successfully initialized.
fn gpu_compositor_enabled() -> bool {
    global_gpu_compositor().is_some()
}

/// Record GPU compositor result — logs persistent failures for debugging.
fn record_gpu_compositor_result(success: bool) {
    static CONSECUTIVE_FAILURES: OnceLock<AtomicU64> = OnceLock::new();
    let counter = CONSECUTIVE_FAILURES.get_or_init(|| AtomicU64::new(0));

    if success {
        counter.store(0, Ordering::Relaxed);
    } else {
        let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
        // Log every 60 failures (~1 second at 60fps) instead of permanently disabling.
        if n % 60 == 0 {
            tracing::warn!("GPU compositor failed {n} times consecutively, retrying...");
        }
    }
}

fn global_media_path_cache(cache_root: PathBuf) -> Arc<mondrian_media::MultiLevelCache> {
    static MEDIA_PATH_CACHE: OnceLock<
        Mutex<HashMap<PathBuf, Arc<mondrian_media::MultiLevelCache>>>,
    > = OnceLock::new();
    let cache_map = MEDIA_PATH_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = match cache_map.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };

    guard
        .entry(cache_root.clone())
        .or_insert_with(|| mondrian_media::MultiLevelCache::new(cache_root, 256))
        .clone()
}

fn decode_layer_rgba(
    layer: &LayerDecodeRequest,
    width: u32,
    height: u32,
    playback_mode: bool,
    layer_cache_enabled: bool,
    layer_cache: &SharedLayerFrameCache,
    decoder_pool: &Arc<DecoderPool>,
) -> anyhow::Result<RgbaFrame> {
    let started_at = Instant::now();
    let cache_key = LayerFrameCacheKey {
        asset_id: layer.frame_key.0,
        source_frame: layer.frame_key.1,
        source_time_base: layer.source_time_base,
        target_width: width,
        target_height: height,
        input_color_space: layer.input_color_space,
        working_color_space: layer.working_color_space,
        engine: layer.engine.clone(),
        tone_map: layer.tone_map,
    };

    if layer_cache_enabled {
        if let Some(frame) = layer_cache_get(layer_cache, &cache_key) {
            record_preview_perf_layer_decode(started_at.elapsed(), true);
            if preview_diag_enabled() {
                tracing::debug!(
                    "[preview-diag] layer cache hit asset={} frame={} size={}x{}",
                    layer.frame_key.0,
                    layer.frame_key.1,
                    width,
                    height
                );
            }
            return Ok(frame);
        }
    }

    if layer_cache_enabled && playback_mode {
        let tolerance = playback_layer_cache_tolerance_frames();
        if tolerance > 0 {
            if let Some(frame) = layer_cache_get_with_tolerance(layer_cache, &cache_key, tolerance)
            {
                layer_cache_put(layer_cache, cache_key.clone(), frame.clone());
                record_preview_perf_layer_decode(started_at.elapsed(), true);
                if preview_diag_enabled() {
                    tracing::debug!(
                        "[preview-diag] layer tolerance cache hit asset={} frame={} tol={} size={}x{}",
                        layer.frame_key.0,
                        layer.frame_key.1,
                        tolerance,
                        width,
                        height
                    );
                }
                return Ok(frame);
            }
        }
    }

    let async_result = with_layer_decode_runtime(|rt| {
        rt.block_on(decoder_pool.get_video_frame_rgba(
            layer.frame_key.0,
            layer.path.clone(),
            TimeCode::new(layer.frame_key.1, layer.source_time_base),
            width,
            height,
        ))
    });

    if let Some(Ok(frame)) = async_result {
        let mut frame = (*frame).clone();
        apply_layer_input_color(&mut frame.data, layer);
        if layer_cache_enabled {
            layer_cache_put(layer_cache, cache_key.clone(), frame.clone());
        }
        record_preview_perf_layer_decode(started_at.elapsed(), false);
        if preview_diag_enabled() {
            tracing::debug!(
                "[preview-diag] layer async decode ok asset={} frame={} elapsed={}ms",
                layer.frame_key.0,
                layer.frame_key.1,
                started_at.elapsed().as_millis() as u64
            );
        }
        return Ok(frame);
    }

    if let Some(Err(err)) = async_result {
        if preview_diag_enabled() {
            tracing::warn!(
                "[preview-diag] layer async decode failed asset={} frame={} err={}",
                layer.frame_key.0,
                layer.frame_key.1,
                err
            );
        }
    }

    if !preview_sync_fallback_enabled() {
        return Err(anyhow::anyhow!(
            "async layer decode failed and sync fallback disabled: asset={} frame={}",
            layer.frame_key.0,
            layer.frame_key.1
        ));
    }

    if preview_diag_enabled() {
        tracing::warn!(
            "[preview-diag] entering sync fallback decode asset={} frame={}",
            layer.frame_key.0,
            layer.frame_key.1
        );
    }

    Ok(mondrian_media::decode_video_frame_at_time_rgba_scaled(
        layer.path.as_path(),
        layer.source_secs,
        None,
        None,
    )
    .map(|mut frame| {
        apply_layer_input_color(&mut frame.data, layer);
        if layer_cache_enabled {
            layer_cache_put(layer_cache, cache_key, frame.clone());
        }
        record_preview_perf_layer_decode(started_at.elapsed(), false);
        frame
    })?)
}

fn apply_layer_input_color(data: &mut [u8], layer: &LayerDecodeRequest) {
    convert_rgba8_in_place(
        data,
        ColorPipeline::new(
            layer.input_color_space,
            layer.working_color_space,
            layer.working_color_space,
            layer.tone_map,
        )
        .with_engine(layer.engine.clone()),
    );
}

#[derive(Default)]
struct PreviewPerfStats {
    decode_frames: AtomicU64,
    committed_frames: AtomicU64,
    passthrough_frames: AtomicU64,
    layer_cache_hits: AtomicU64,
    layer_cache_misses: AtomicU64,
    decode_total_ns: AtomicU64,
    layer_decode_ns: AtomicU64,
    composite_cpu_ns: AtomicU64,
    composite_gpu_ns: AtomicU64,
    upload_ns: AtomicU64,
    last_report_ms: AtomicU64,
    last_avg_decode_x100: AtomicU64,
    last_avg_layer_x100: AtomicU64,
    last_avg_comp_x100: AtomicU64,
    last_avg_upload_x100: AtomicU64,
    last_layer_share_pct: AtomicU64,
    last_comp_share_pct: AtomicU64,
    last_upload_share_pct: AtomicU64,
    last_hit_rate_pct: AtomicU64,
    last_passthrough_rate_pct: AtomicU64,
    last_gpu_comp_on: AtomicBool,
    last_decode_frames: AtomicU64,
    last_commit_frames: AtomicU64,
}

#[derive(Debug, Clone, Copy)]
struct PreviewPerfSnapshot {
    avg_decode_ms: f64,
    avg_layer_ms: f64,
    avg_comp_ms: f64,
    avg_upload_ms: f64,
    layer_share_pct: u64,
    comp_share_pct: u64,
    upload_share_pct: u64,
    hit_rate_pct: u64,
    passthrough_rate_pct: u64,
    gpu_comp_on: bool,
    decode_frames: u64,
    commit_frames: u64,
}

fn preview_perf_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("MONDRIAN_PREVIEW_PERF")
            .map(|v| {
                let value = v.trim().to_ascii_lowercase();
                matches!(value.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(false)
    })
}

fn preview_perf_report_interval_ms() -> u64 {
    static INTERVAL_MS: OnceLock<u64> = OnceLock::new();
    *INTERVAL_MS.get_or_init(|| {
        std::env::var("MONDRIAN_PREVIEW_PERF_INTERVAL_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(1000)
    })
}

fn preview_perf_stats() -> &'static PreviewPerfStats {
    static STATS: OnceLock<PreviewPerfStats> = OnceLock::new();
    STATS.get_or_init(PreviewPerfStats::default)
}

fn preview_perf_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn preview_perf_snapshot() -> Option<PreviewPerfSnapshot> {
    let stats = preview_perf_stats();
    let decode_frames = stats.last_decode_frames.load(Ordering::Relaxed);
    let commit_frames = stats.last_commit_frames.load(Ordering::Relaxed);
    if decode_frames == 0 && commit_frames == 0 {
        return None;
    }

    Some(PreviewPerfSnapshot {
        avg_decode_ms: stats.last_avg_decode_x100.load(Ordering::Relaxed) as f64 / 100.0,
        avg_layer_ms: stats.last_avg_layer_x100.load(Ordering::Relaxed) as f64 / 100.0,
        avg_comp_ms: stats.last_avg_comp_x100.load(Ordering::Relaxed) as f64 / 100.0,
        avg_upload_ms: stats.last_avg_upload_x100.load(Ordering::Relaxed) as f64 / 100.0,
        layer_share_pct: stats.last_layer_share_pct.load(Ordering::Relaxed),
        comp_share_pct: stats.last_comp_share_pct.load(Ordering::Relaxed),
        upload_share_pct: stats.last_upload_share_pct.load(Ordering::Relaxed),
        hit_rate_pct: stats.last_hit_rate_pct.load(Ordering::Relaxed),
        passthrough_rate_pct: stats.last_passthrough_rate_pct.load(Ordering::Relaxed),
        gpu_comp_on: stats.last_gpu_comp_on.load(Ordering::Relaxed),
        decode_frames,
        commit_frames,
    })
}

fn record_preview_perf_passthrough_frame() {
    if !preview_perf_enabled() {
        return;
    }
    preview_perf_stats().passthrough_frames.fetch_add(1, Ordering::Relaxed);
    maybe_report_preview_perf();
}

fn record_preview_perf_layer_decode(elapsed: Duration, cache_hit: bool) {
    if !preview_perf_enabled() {
        return;
    }
    let stats = preview_perf_stats();
    stats.layer_decode_ns.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
    if cache_hit {
        stats.layer_cache_hits.fetch_add(1, Ordering::Relaxed);
    } else {
        stats.layer_cache_misses.fetch_add(1, Ordering::Relaxed);
    }
    maybe_report_preview_perf();
}

fn record_preview_perf_composite_ns(elapsed_ns: u64, gpu: bool) {
    if !preview_perf_enabled() {
        return;
    }
    let stats = preview_perf_stats();
    if gpu {
        stats.composite_gpu_ns.fetch_add(elapsed_ns, Ordering::Relaxed);
    } else {
        stats.composite_cpu_ns.fetch_add(elapsed_ns, Ordering::Relaxed);
    }
    maybe_report_preview_perf();
}

fn record_preview_perf_decode_total(elapsed: Duration) {
    if !preview_perf_enabled() {
        return;
    }
    let stats = preview_perf_stats();
    stats.decode_frames.fetch_add(1, Ordering::Relaxed);
    stats.decode_total_ns.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
    maybe_report_preview_perf();
}

fn record_preview_perf_upload(elapsed: Duration) {
    if !preview_perf_enabled() {
        return;
    }
    let stats = preview_perf_stats();
    stats.committed_frames.fetch_add(1, Ordering::Relaxed);
    stats.upload_ns.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
    maybe_report_preview_perf();
}

fn maybe_report_preview_perf() {
    if !preview_perf_enabled() {
        return;
    }

    let stats = preview_perf_stats();
    let now_ms = preview_perf_now_ms();
    let interval = preview_perf_report_interval_ms();
    let last = stats.last_report_ms.load(Ordering::Relaxed);

    if now_ms < last.saturating_add(interval) {
        return;
    }

    if stats
        .last_report_ms
        .compare_exchange(last, now_ms, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return;
    }

    let decode_frames = stats.decode_frames.swap(0, Ordering::Relaxed);
    let committed_frames = stats.committed_frames.swap(0, Ordering::Relaxed);
    let passthrough_frames = stats.passthrough_frames.swap(0, Ordering::Relaxed);
    let cache_hits = stats.layer_cache_hits.swap(0, Ordering::Relaxed);
    let cache_misses = stats.layer_cache_misses.swap(0, Ordering::Relaxed);
    let decode_total_ns = stats.decode_total_ns.swap(0, Ordering::Relaxed);
    let layer_decode_ns = stats.layer_decode_ns.swap(0, Ordering::Relaxed);
    let composite_cpu_ns = stats.composite_cpu_ns.swap(0, Ordering::Relaxed);
    let composite_gpu_ns = stats.composite_gpu_ns.swap(0, Ordering::Relaxed);
    let upload_ns = stats.upload_ns.swap(0, Ordering::Relaxed);

    if decode_frames == 0 && committed_frames == 0 {
        return;
    }

    let avg_decode_ms = if decode_frames > 0 {
        decode_total_ns as f64 / decode_frames as f64 / 1_000_000.0
    } else {
        0.0
    };

    let layer_calls = cache_hits.saturating_add(cache_misses);
    let avg_layer_ms = if layer_calls > 0 {
        layer_decode_ns as f64 / layer_calls as f64 / 1_000_000.0
    } else {
        0.0
    };

    let composite_total_ns = composite_cpu_ns.saturating_add(composite_gpu_ns);
    let avg_composite_ms = if decode_frames > 0 {
        composite_total_ns as f64 / decode_frames as f64 / 1_000_000.0
    } else {
        0.0
    };

    let avg_upload_ms = if committed_frames > 0 {
        upload_ns as f64 / committed_frames as f64 / 1_000_000.0
    } else {
        0.0
    };

    let denominator_ns = layer_decode_ns
        .saturating_add(composite_total_ns)
        .saturating_add(upload_ns)
        .max(1);
    let layer_pct = layer_decode_ns as f64 * 100.0 / denominator_ns as f64;
    let composite_pct = composite_total_ns as f64 * 100.0 / denominator_ns as f64;
    let upload_pct = upload_ns as f64 * 100.0 / denominator_ns as f64;

    let hit_rate = if layer_calls > 0 {
        cache_hits as f64 * 100.0 / layer_calls as f64
    } else {
        0.0
    };
    let passthrough_rate = if decode_frames > 0 {
        passthrough_frames as f64 * 100.0 / decode_frames as f64
    } else {
        0.0
    };

    stats.last_avg_decode_x100.store(
        (avg_decode_ms * 100.0).round().max(0.0) as u64,
        Ordering::Relaxed,
    );
    stats.last_avg_layer_x100.store(
        (avg_layer_ms * 100.0).round().max(0.0) as u64,
        Ordering::Relaxed,
    );
    stats.last_avg_comp_x100.store(
        (avg_composite_ms * 100.0).round().max(0.0) as u64,
        Ordering::Relaxed,
    );
    stats.last_avg_upload_x100.store(
        (avg_upload_ms * 100.0).round().max(0.0) as u64,
        Ordering::Relaxed,
    );
    stats.last_layer_share_pct.store(
        layer_pct.round().clamp(0.0, 100.0) as u64,
        Ordering::Relaxed,
    );
    stats.last_comp_share_pct.store(
        composite_pct.round().clamp(0.0, 100.0) as u64,
        Ordering::Relaxed,
    );
    stats.last_upload_share_pct.store(
        upload_pct.round().clamp(0.0, 100.0) as u64,
        Ordering::Relaxed,
    );
    stats
        .last_hit_rate_pct
        .store(hit_rate.round().clamp(0.0, 100.0) as u64, Ordering::Relaxed);
    stats.last_passthrough_rate_pct.store(
        passthrough_rate.round().clamp(0.0, 100.0) as u64,
        Ordering::Relaxed,
    );
    stats.last_gpu_comp_on.store(composite_gpu_ns > 0, Ordering::Relaxed);
    stats.last_decode_frames.store(decode_frames, Ordering::Relaxed);
    stats.last_commit_frames.store(committed_frames, Ordering::Relaxed);

    tracing::info!(
        "[preview-perf] frames(dec/commit)={}/{} avg_ms(dec/layer/comp/upload)={:.2}/{:.2}/{:.2}/{:.2} share(layer/comp/upload)={:.0}%/{:.0}%/{:.0}% layer_hit={:.0}% pass={:.0}% gpu_comp={}",
        decode_frames,
        committed_frames,
        avg_decode_ms,
        avg_layer_ms,
        avg_composite_ms,
        avg_upload_ms,
        layer_pct,
        composite_pct,
        upload_pct,
        hit_rate,
        passthrough_rate,
        if composite_gpu_ns > 0 { "on" } else { "off" }
    );
}

fn with_layer_decode_runtime<T>(f: impl FnOnce(&tokio::runtime::Runtime) -> T) -> Option<T> {
    thread_local! {
        static LAYER_DECODE_RUNTIME: RefCell<Option<tokio::runtime::Runtime>> = const { RefCell::new(None) };
    }

    LAYER_DECODE_RUNTIME.with(|slot| {
        let mut runtime = slot.borrow_mut();
        if runtime.is_none() {
            *runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().ok();
        }
        runtime.as_ref().map(f)
    })
}

fn layer_cache_get(
    layer_cache: &SharedLayerFrameCache,
    key: &LayerFrameCacheKey,
) -> Option<RgbaFrame> {
    let guard = match layer_cache.lock() {
        Ok(g) => g,
        Err(_) => return None,
    };
    guard.entries.get(key).cloned()
}

fn layer_cache_get_with_tolerance(
    layer_cache: &SharedLayerFrameCache,
    key: &LayerFrameCacheKey,
    tolerance_frames: i64,
) -> Option<RgbaFrame> {
    let guard = match layer_cache.lock() {
        Ok(g) => g,
        Err(_) => return None,
    };

    let mut best_key: Option<LayerFrameCacheKey> = None;
    let mut best_distance = i64::MAX;

    for entry_key in guard.entries.keys() {
        if entry_key.asset_id != key.asset_id
            || entry_key.target_width != key.target_width
            || entry_key.target_height != key.target_height
        {
            continue;
        }

        let distance = (entry_key.source_frame - key.source_frame).abs();
        if distance <= tolerance_frames && distance < best_distance {
            best_distance = distance;
            best_key = Some(entry_key.clone());
            if distance == 0 {
                break;
            }
        }
    }

    best_key.and_then(|entry_key| guard.entries.get(&entry_key).cloned())
}

fn layer_cache_contains(layer_cache: &SharedLayerFrameCache, key: &LayerFrameCacheKey) -> bool {
    let guard = match layer_cache.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    guard.entries.contains_key(key)
}

fn layer_cache_put(layer_cache: &SharedLayerFrameCache, key: LayerFrameCacheKey, frame: RgbaFrame) {
    // 扩大图层帧缓存至 256 帧：
    // 预取 + scrub 场景下 96 帧容量不足，大 seek 后旧缓存无法复用，
    // 增大容量可显著减少 seek 后的重复解码次数。
    const LAYER_CACHE_CAPACITY: usize = 256;

    let mut guard = match layer_cache.lock() {
        Ok(g) => g,
        Err(_) => return,
    };

    if !guard.entries.contains_key(&key) {
        guard.order.push_front(key.clone());
    }

    guard.entries.insert(key, frame);
    while guard.entries.len() > LAYER_CACHE_CAPACITY {
        if let Some(oldest) = guard.order.pop_back() {
            guard.entries.remove(&oldest);
        } else {
            break;
        }
    }
}

fn layer_cache_clear(layer_cache: &SharedLayerFrameCache) {
    if let Ok(mut guard) = layer_cache.lock() {
        guard.entries.clear();
        guard.order.clear();
    }
}

fn scaled_dimension(raw: f32, factor: f32) -> u32 {
    (raw.max(1.0) * factor.max(0.05)).round().max(1.0) as u32
}

fn sequence_preview_target_size(
    resolution: mondrian_core::types::Resolution,
    available_width: f32,
    available_height: f32,
    scale_factor: f32,
) -> (u32, u32) {
    let max_width = scaled_dimension(available_width, scale_factor);
    let max_height = scaled_dimension(available_height, scale_factor);

    let width_scale = max_width as f64 / resolution.width.max(1) as f64;
    let height_scale = max_height as f64 / resolution.height.max(1) as f64;
    let scale = width_scale.min(height_scale).max(0.0001);

    let mut target_width = (resolution.width.max(1) as f64 * scale).round().max(1.0) as u32;
    let mut target_height = ((target_width as f64 / resolution.width.max(1) as f64)
        * resolution.height.max(1) as f64)
        .round()
        .max(1.0) as u32;

    if target_height > max_height {
        target_height = max_height.max(1);
        target_width = ((target_height as f64 / resolution.height.max(1) as f64)
            * resolution.width.max(1) as f64)
            .round()
            .max(1.0) as u32;
    }
    if target_width > max_width {
        target_width = max_width.max(1);
        target_height = ((target_width as f64 / resolution.width.max(1) as f64)
            * resolution.height.max(1) as f64)
            .round()
            .max(1.0) as u32;
    }

    (target_width.max(1), target_height.max(1))
}

fn playback_adjusted_target_size(size: (u32, u32), is_playing: bool) -> (u32, u32) {
    let (width, height) = size;
    let width_f = width.max(1) as f64;
    let height_f = height.max(1) as f64;
    let mut out_w = width.max(1);
    let mut out_h = height.max(1);

    if is_playing {
        let max_dim = playback_preview_max_dimension();
        if max_dim > 0 {
            let current_max = width_f.max(height_f);
            if current_max > max_dim as f64 {
                let scale = max_dim as f64 / current_max;
                out_w = (width_f * scale).round().max(1.0) as u32;
                out_h = (height_f * scale).round().max(1.0) as u32;
            }
        }
    }

    if out_w % 2 == 1 {
        out_w = out_w.saturating_sub(1).max(1);
    }
    if out_h % 2 == 1 {
        out_h = out_h.saturating_sub(1).max(1);
    }

    let step = if is_playing { 16 } else { 8 };
    let out_w = quantize_dimension(out_w.max(1), step);
    let out_h = quantize_dimension(out_h.max(1), step);

    (out_w, out_h)
}

fn preview_diag_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("MONDRIAN_PREVIEW_DIAG")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(cfg!(debug_assertions))
    })
}

fn preview_decode_worker_cap() -> usize {
    static WORKER_CAP: OnceLock<usize> = OnceLock::new();
    *WORKER_CAP.get_or_init(|| {
        std::env::var("MONDRIAN_PREVIEW_DECODE_WORKERS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .map(|v| v.clamp(1, 16))
            .unwrap_or(6)
    })
}

fn preview_decode_pool() -> &'static rayon::ThreadPool {
    static DECODE_POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    DECODE_POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(preview_decode_worker_cap())
            .thread_name(|idx| format!("preview-decode-{}", idx))
            .build()
            .expect("failed to build preview decode rayon pool")
    })
}

fn preview_diag_slow_threshold_ms() -> u64 {
    static THRESHOLD: OnceLock<u64> = OnceLock::new();
    *THRESHOLD.get_or_init(|| {
        std::env::var("MONDRIAN_PREVIEW_DIAG_SLOW_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v >= 5)
            .unwrap_or(40)
    })
}

fn preview_sync_fallback_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("MONDRIAN_PREVIEW_SYNC_FALLBACK")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

fn playback_preview_max_dimension() -> u32 {
    static MAX_DIM: OnceLock<u32> = OnceLock::new();
    *MAX_DIM.get_or_init(|| {
        std::env::var("MONDRIAN_PLAYBACK_PREVIEW_MAX_DIM")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|value| *value >= 320)
            .unwrap_or(1280)
    })
}

fn decode_stall_timeout_ms() -> u64 {
    static TIMEOUT_MS: OnceLock<u64> = OnceLock::new();
    *TIMEOUT_MS.get_or_init(|| {
        std::env::var("MONDRIAN_DECODE_STALL_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .or_else(|| {
                let budget = decode_timeout_budget_ms();
                let grace = std::env::var("MONDRIAN_DECODE_STALL_GRACE_MS")
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(300);
                Some(budget.saturating_add(grace))
            })
            .filter(|value| *value >= 150)
            .unwrap_or(1200)
    })
}

fn decode_timeout_budget_ms() -> u64 {
    static BUDGET_MS: OnceLock<u64> = OnceLock::new();
    *BUDGET_MS.get_or_init(|| {
        std::env::var("MONDRIAN_DECODE_TIMEOUT_BUDGET_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .or_else(|| {
                std::env::var("MONDRIAN_PREVIEW_DECODE_TIMEOUT_MS")
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
            })
            .filter(|value| *value >= 100)
            .unwrap_or(2500)
    })
}

fn decode_request_coalescing_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("MONDRIAN_DECODE_REQUEST_COALESCING")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(true)
    })
}

fn playback_prefill_duration_ms() -> u64 {
    static PREFILL_MS: OnceLock<u64> = OnceLock::new();
    *PREFILL_MS.get_or_init(|| {
        std::env::var("MONDRIAN_PLAYBACK_PREFILL_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(|value| value.clamp(0, 3000))
            .unwrap_or(600)
    })
}

fn playback_prefetch_target_frames(fps: f64) -> i64 {
    static TARGET: OnceLock<i64> = OnceLock::new();
    let configured = *TARGET.get_or_init(|| {
        std::env::var("MONDRIAN_PLAYBACK_PREFETCH_TARGET_FRAMES")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .map(|value| value.clamp(8, 96))
            .unwrap_or(-1)
    });

    if configured > 0 {
        configured
    } else {
        (fps.round() as i64).max(25).clamp(12, 64)
    }
}

fn playback_prefetch_hysteresis_frames(fps: f64) -> i64 {
    static HYSTERESIS: OnceLock<i64> = OnceLock::new();
    let configured = *HYSTERESIS.get_or_init(|| {
        std::env::var("MONDRIAN_PLAYBACK_PREFETCH_HYSTERESIS_FRAMES")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .map(|value| value.clamp(1, 48))
            .unwrap_or(-1)
    });

    if configured > 0 {
        configured
    } else {
        (fps / 4.0).round() as i64
    }
    .max(6)
    .clamp(2, 32)
}

fn playback_layer_cache_tolerance_frames() -> i64 {
    static TOLERANCE: OnceLock<i64> = OnceLock::new();
    *TOLERANCE.get_or_init(|| {
        std::env::var("MONDRIAN_LAYER_CACHE_TOLERANCE_PLAYBACK")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .map(|value| value.clamp(0, 3))
            .unwrap_or(0)
    })
}

fn playback_seek_reset_threshold_frames(fps: f64) -> i64 {
    static CONFIGURED: OnceLock<i64> = OnceLock::new();
    let configured = *CONFIGURED.get_or_init(|| {
        std::env::var("MONDRIAN_PLAYBACK_SEEK_RESET_FRAMES")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .map(|value| value.clamp(8, 240))
            .unwrap_or(-1)
    });

    if configured > 0 {
        configured
    } else {
        (fps * 1.2).round() as i64
    }
    .max(16)
    .clamp(12, 120)
}

fn quantize_dimension(value: u32, step: u32) -> u32 {
    let step = step.max(1);
    let rounded = ((value + (step / 2)) / step).saturating_mul(step);
    if rounded % 2 == 1 {
        rounded.saturating_sub(1).max(1)
    } else {
        rounded.max(1)
    }
}

/// Draw action-safe (90%) and title-safe (80%) overlays.
fn draw_safe_margins(
    painter: &egui::Painter,
    ct: &crate::ui::viewer::canvas::CanvasTransform,
    seq: &Sequence,
) {
    let w = seq.settings.resolution.width as f32;
    let h = seq.settings.resolution.height as f32;
    let action_margin = seq.settings.action_safe_margin * 0.5;
    let title_margin = seq.settings.title_safe_margin * 0.5;

    let action_rect = Rect::from_min_max(
        ct.seq_to_screen(w * action_margin, h * action_margin),
        ct.seq_to_screen(w * (1.0 - action_margin), h * (1.0 - action_margin)),
    );
    let title_rect = Rect::from_min_max(
        ct.seq_to_screen(w * title_margin, h * title_margin),
        ct.seq_to_screen(w * (1.0 - title_margin), h * (1.0 - title_margin)),
    );

    let action_color = egui::Color32::from_white_alpha(40);
    let title_color = egui::Color32::from_white_alpha(30);

    painter.rect_stroke(
        action_rect,
        egui::CornerRadius::same(0),
        egui::Stroke::new(1.0, action_color),
        egui::StrokeKind::Inside,
    );
    painter.rect_stroke(
        title_rect,
        egui::CornerRadius::same(0),
        egui::Stroke::new(1.0, title_color),
        egui::StrokeKind::Inside,
    );
}

/// Uniformly scale an affine matrix [a, b, tx, c, d, ty] by factor.
fn scale_affine(t: [f32; 6], factor: f32) -> [f32; 6] {
    [
        t[0] * factor,
        t[1] * factor,
        t[2] * factor,
        t[3] * factor,
        t[4] * factor,
        t[5] * factor,
    ]
}

fn is_near_corner(rect: Option<Rect>, point: Pos2, radius: f32) -> bool {
    rect.is_some_and(|r| {
        let corners = [
            r.left_top(),
            r.right_top(),
            r.right_bottom(),
            r.left_bottom(),
        ];
        corners.iter().any(|&c| c.distance(point) <= radius)
    })
}

/// Screen bounds for a clip, using actual media dimensions from the asset library.
fn clip_screen_bounds_with_media(
    clip: &mondrian_timeline::clip::Clip,
    mat: glam::Mat3,
    ct: &crate::ui::viewer::canvas::CanvasTransform,
    state: &AppState,
) -> Option<Rect> {
    let (mw, mh) = state
        .asset_library
        .as_ref()
        .and_then(|lib| lib.get_asset(clip.asset_id).ok().flatten())
        .and_then(|a| a.media_info.primary_video().cloned())
        .map(|v| (v.width as f32, v.height as f32))
        .unwrap_or((1.0, 1.0));
    let corners = [
        glam::Vec2::new(0.0, 0.0),
        glam::Vec2::new(mw, 0.0),
        glam::Vec2::new(mw, mh),
        glam::Vec2::new(0.0, mh),
    ];
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for &c in &corners {
        let t = mat * c.extend(1.0);
        let p = ct.seq_to_screen(t.x, t.y);
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    if min_x >= max_x || min_y >= max_y {
        None
    } else {
        Some(Rect::from_min_max(
            Pos2::new(min_x, min_y),
            Pos2::new(max_x, max_y),
        ))
    }
}

#[allow(dead_code)]
fn clip_screen_bounds(
    _clip: &mondrian_timeline::clip::Clip,
    mat: glam::Mat3,
    ct: &crate::ui::viewer::canvas::CanvasTransform,
) -> Option<Rect> {
    let corners = [
        glam::Vec2::new(0.0, 0.0),
        glam::Vec2::new(1.0, 0.0),
        glam::Vec2::new(1.0, 1.0),
        glam::Vec2::new(0.0, 1.0),
    ];
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for c in &corners {
        let t = mat * c.extend(1.0);
        let p = ct.seq_to_screen(t.x, t.y);
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    if min_x >= max_x || min_y >= max_y {
        return None;
    }
    Some(Rect::from_min_max(
        Pos2::new(min_x, min_y),
        Pos2::new(max_x, max_y),
    ))
}

/// Draw corner handles for the selected clip on the canvas.
fn draw_transform_handles(
    painter: &egui::Painter,
    state: &AppState,
    ct: &crate::ui::viewer::canvas::CanvasTransform,
    clip_id: mondrian_core::types::ClipId,
) {
    let Some(seq) = state.sequence.as_ref() else {
        return;
    };
    let Some(library) = state.asset_library.as_ref() else {
        return;
    };
    let current = TimeCode::new(state.current_frame().max(0), seq.time_base());
    let active = seq.active_clips_at(current);
    let Some(ac) = active.iter().find(|a| a.clip.id == clip_id) else {
        return;
    };

    // Get media dimensions for the bounding box.
    let (mw, mh) = library
        .get_asset(ac.clip.asset_id)
        .ok()
        .flatten()
        .and_then(|a| a.media_info.primary_video().cloned())
        .map(|v| (v.width as f32, v.height as f32))
        .unwrap_or((1.0, 1.0));

    let corners = [
        glam::Vec2::new(0.0, 0.0),
        glam::Vec2::new(mw, 0.0),
        glam::Vec2::new(mw, mh),
        glam::Vec2::new(0.0, mh),
    ];
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for &c in &corners {
        let t = ac.transform_matrix * c.extend(1.0);
        let p = ct.seq_to_screen(t.x, t.y);
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    if min_x >= max_x || min_y >= max_y {
        return;
    }
    let rect = Rect::from_min_max(Pos2::new(min_x, min_y), Pos2::new(max_x, max_y));

    let handle_color = egui::Color32::from_rgb(0, 180, 255);
    let handle_radius = 5.0;
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(0),
        egui::Stroke::new(2.0, handle_color),
        egui::StrokeKind::Inside,
    );
    for &c in &corners {
        painter.circle_filled(c, handle_radius, handle_color);
    }
    // Draw anchor point (position = anchor's location in sequence space).
    let anchor_pos = ac.clip.transform.get_position(current);
    let ap = ct.seq_to_screen(anchor_pos.x, anchor_pos.y);
    painter.circle_filled(ap, 5.0, egui::Color32::from_rgb(255, 200, 0));
}

/// 绘制棋盘格背景（表示空帧/透明）
fn draw_checkerboard(painter: &egui::Painter, rect: Rect) {
    let cell = 12.0_f32;
    let cols = ((rect.width() / cell).ceil() as usize).max(1);
    let rows = ((rect.height() / cell).ceil() as usize).max(1);

    for row in 0..rows {
        for col in 0..cols {
            let color = if (row + col) % 2 == 0 {
                palette::bg_surface()
            } else {
                palette::bg_surface_hover()
            };
            let x = rect.left() + col as f32 * cell;
            let y = rect.top() + row as f32 * cell;
            let cell_rect = Rect::from_min_size(Pos2::new(x, y), Vec2::splat(cell));
            painter.rect_filled(cell_rect, 0.0, color);
        }
    }
}

fn draw_empty_canvas_meta(
    painter: &egui::Painter,
    rect: Rect,
    state: &AppState,
    current_frame: i64,
) {
    let fps = state
        .sequence
        .as_ref()
        .map(|s| s.settings.frame_rate)
        .unwrap_or(Rational::new(25, 1));
    let tc = TimeCode::new(current_frame.max(0), Rational::new(fps.den, fps.num)).to_smpte();

    let resolution = state
        .sequence
        .as_ref()
        .map(|s| {
            format!(
                "{}x{}",
                s.settings.resolution.width, s.settings.resolution.height
            )
        })
        .unwrap_or_else(|| "--x--".to_string());

    painter.text(
        rect.center() + Vec2::new(0.0, -8.0),
        egui::Align2::CENTER_CENTER,
        tc,
        typography::mono_large(),
        palette::text_muted().gamma_multiply(0.6),
    );
    painter.text(
        rect.center() + Vec2::new(0.0, 12.0),
        egui::Align2::CENTER_CENTER,
        resolution,
        typography::body(),
        palette::text_muted().gamma_multiply(0.6),
    );
}

fn draw_mask_overlays(
    painter: &egui::Painter,
    ct: &crate::ui::viewer::canvas::CanvasTransform,
    state: &AppState,
    timeline_frame: i64,
    selected_mask: Option<(
        mondrian_effects::mask::MaskId,
        mondrian_core::types::ClipId,
        mondrian_core::types::TrackId,
    )>,
) {
    let Some(seq) = state.sequence.as_ref() else {
        return;
    };
    let Some(lib) = state.asset_library.as_ref() else {
        return;
    };
    let active = seq.active_clips_at(TimeCode::new(timeline_frame.max(0), seq.time_base()));
    if active.is_empty() {
        return;
    }
    let to_scr = |x: f32, y: f32| ct.seq_to_screen(x, y);
    let ticks = timecode_to_ticks(TimeCode::new(timeline_frame.max(0), seq.time_base()));
    let colors = [
        egui::Color32::from_rgb(0, 180, 240),
        egui::Color32::from_rgb(220, 120, 0),
        egui::Color32::from_rgb(120, 200, 80),
        egui::Color32::from_rgb(200, 80, 200),
    ];
    for ac in &active {
        if ac.clip.masks.is_empty() {
            continue;
        }
        let mat = ac.transform_matrix;
        // Get media dimensions for normalized → pixel conversion.
        let (mw, mh) = lib
            .get_asset(ac.clip.asset_id)
            .ok()
            .flatten()
            .and_then(|a| a.media_info.primary_video().cloned())
            .map(|v| (v.width as f32, v.height as f32))
            .unwrap_or((1.0, 1.0));
        for (i, mask) in ac.clip.masks.iter().enumerate() {
            if !mask.enabled {
                continue;
            }
            let is_selected =
                selected_mask.is_some_and(|(mid, cid, _)| mid == mask.id && cid == ac.clip.id);
            let p = mask.evaluate_at(ticks);
            let c = colors[i % colors.len()];
            let st = if is_selected {
                egui::Stroke::new(3.0, c)
            } else {
                egui::Stroke::new(2.0, c)
            };
            match &p.shape {
                MaskShape::Rectangle { x, y, width, height, .. } => {
                    let crn = [
                        glam::Vec2::new(x * mw, y * mh),
                        glam::Vec2::new((x + width) * mw, y * mh),
                        glam::Vec2::new((x + width) * mw, (y + height) * mh),
                        glam::Vec2::new(x * mw, (y + height) * mh),
                    ];
                    draw_mask_polygon(painter, &crn, &mat, &to_scr, st, true);
                    // Draw corner handles for selected mask.
                    if is_selected {
                        for &corner in &crn {
                            let sc = mask_xform(corner, &mat, &to_scr);
                            painter.rect_filled(
                                egui::Rect::from_center_size(sc, egui::vec2(8.0, 8.0)),
                                2.0,
                                egui::Color32::WHITE,
                            );
                        }
                    }
                }
                MaskShape::Ellipse { center, radii } => {
                    let n = 64usize;
                    let mut pts = Vec::with_capacity(n + 1);
                    let cx = center.x * mw;
                    let cy = center.y * mh;
                    let rx = radii.x * mw;
                    let ry = radii.y * mh;
                    for s in 0..=n {
                        let a = s as f32 * std::f32::consts::TAU / n as f32;
                        pts.push(glam::Vec2::new(cx + rx * a.cos(), cy + ry * a.sin()));
                    }
                    draw_mask_polygon(painter, &pts, &mat, &to_scr, st, false);
                    // Draw bounding-box handles for selected ellipse.
                    if is_selected {
                        let bbox_corners = [
                            glam::Vec2::new(cx - rx, cy - ry),
                            glam::Vec2::new(cx + rx, cy - ry),
                            glam::Vec2::new(cx + rx, cy + ry),
                            glam::Vec2::new(cx - rx, cy + ry),
                        ];
                        for &bc in &bbox_corners {
                            let sc = mask_xform(bc, &mat, &to_scr);
                            painter.rect_filled(
                                egui::Rect::from_center_size(sc, egui::vec2(8.0, 8.0)),
                                2.0,
                                egui::Color32::WHITE,
                            );
                        }
                    }
                }
                MaskShape::Path { points, closed } => {
                    // Convert normalized coords to media-pixel space and render Bézier segments.
                    let px_pts: Vec<mondrian_effects::mask::BezierPoint> = points
                        .iter()
                        .map(|p| mondrian_effects::mask::BezierPoint {
                            position: glam::Vec2::new(p.position.x * mw, p.position.y * mh),
                            control_in: glam::Vec2::new(p.control_in.x * mw, p.control_in.y * mh),
                            control_out: glam::Vec2::new(p.control_out.x * mw, p.control_out.y * mh),
                        })
                        .collect();
                    let segs = mask_path_segments(&px_pts, *closed);
                    for &(a, b) in &segs {
                        let ta = mask_xform(a, &mat, &to_scr);
                        let tb = mask_xform(b, &mat, &to_scr);
                        painter.line_segment([ta, tb], st);
                    }
                    // Draw anchor points as white squares.
                    for pt in &px_pts {
                        let sp = mask_xform(pt.position, &mat, &to_scr);
                        painter.rect_filled(
                            egui::Rect::from_center_size(sp, egui::vec2(8.0, 8.0)),
                            2.0,
                            egui::Color32::WHITE,
                        );
                        // Draw control handle lines and endpoints.
                        let handle_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgba_premultiplied(c.r(), c.g(), c.b(), 150));
                        if pt.control_in.length_squared() > 0.01 {
                            let cp = mask_xform(pt.position + pt.control_in, &mat, &to_scr);
                            painter.line_segment([sp, cp], handle_stroke);
                            painter.circle_filled(cp, 3.0, egui::Color32::WHITE);
                        }
                        if pt.control_out.length_squared() > 0.01 {
                            let cp = mask_xform(pt.position + pt.control_out, &mat, &to_scr);
                            painter.line_segment([sp, cp], handle_stroke);
                            painter.circle_filled(cp, 3.0, egui::Color32::WHITE);
                        }
                    }
                }
            }
        }
    }
}

fn mask_path_segments(points: &[BezierPoint], closed: bool) -> Vec<(glam::Vec2, glam::Vec2)> {
    let mut out = Vec::new();
    let n = points.len();
    for i in 0..n {
        let ni = if i + 1 < n {
            i + 1
        } else if closed {
            0
        } else {
            break;
        };
        let a = points[i];
        let b = points[ni];
        let s = 32usize;
        let mut prev = a.position;
        for k in 1..=s {
            let t = k as f32 / s as f32;
            let u = 1.0 - t;
            let pt = a.position * u * u * u
                + (a.position + a.control_out) * (3.0 * u * u * t)
                + (b.position + b.control_in) * (3.0 * u * t * t)
                + b.position * t * t * t;
            out.push((prev, pt));
            prev = pt;
        }
    }
    out
}

fn mask_xform(pt: glam::Vec2, mat: &glam::Mat3, to_scr: &impl Fn(f32, f32) -> Pos2) -> Pos2 {
    let t = *mat * pt.extend(1.0);
    to_scr(t.x, t.y)
}

fn draw_mask_polygon(
    painter: &egui::Painter,
    pts: &[glam::Vec2],
    mat: &glam::Mat3,
    to_scr: &impl Fn(f32, f32) -> Pos2,
    stroke: egui::Stroke,
    closed: bool,
) {
    if pts.len() < 2 {
        return;
    }
    let cp: Vec<Pos2> = pts.iter().map(|&p| mask_xform(p, mat, to_scr)).collect();
    let n = if closed { cp.len() } else { cp.len() - 1 };
    for i in 0..n {
        painter.line_segment([cp[i], cp[(i + 1) % cp.len()]], stroke);
    }
}

/// Mask tool toolbar — a thin row of tool buttons above the canvas.
fn draw_mask_toolbar(ui: &mut egui::Ui, panel: &mut ViewerPanel) {
    let btn_size = [26.0, 20.0];
    egui::Frame::default()
        .inner_margin(egui::Margin::symmetric(4, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 1.0;

            // Selection tool
            let sel_active = panel.mask_tool.is_none();
            if theme::icon_ghost_toggle_button(ui, btn_size, theme::UiIcon::Cursor, sel_active)
                .on_hover_text("选择工具 (V)")
                .clicked()
            {
                panel.mask_tool = None;
                panel.mask_draw = None;
                panel.mask_edit = None;
                panel.selected_mask = None;
            }

            // Rectangle mask
            let rect_active = panel.mask_tool == Some(MaskTool::Rect);
            if theme::icon_ghost_toggle_button(ui, btn_size, theme::UiIcon::Rectangle, rect_active)
                .on_hover_text("矩形蒙版 (R)")
                .clicked()
            {
                panel.mask_tool = if rect_active {
                    None
                } else {
                    Some(MaskTool::Rect)
                };
                panel.mask_draw = None;
                panel.mask_edit = None;
                panel.selected_mask = None;
            }

            // Ellipse mask
            let ell_active = panel.mask_tool == Some(MaskTool::Ellipse);
            if theme::icon_ghost_toggle_button(ui, btn_size, theme::UiIcon::Circle, ell_active)
                .on_hover_text("椭圆蒙版 (E)")
                .clicked()
            {
                panel.mask_tool = if ell_active {
                    None
                } else {
                    Some(MaskTool::Ellipse)
                };
                panel.mask_draw = None;
                panel.mask_edit = None;
                panel.selected_mask = None;
            }

            // Pen tool
            let pen_active = panel.mask_tool == Some(MaskTool::Pen);
            if theme::icon_ghost_toggle_button(ui, btn_size, theme::UiIcon::Pen, pen_active)
                .on_hover_text("钢笔工具 (P)")
                .clicked()
            {
                panel.mask_tool = if pen_active {
                    None
                } else {
                    Some(MaskTool::Pen)
                };
                panel.mask_draw = None;
                panel.mask_edit = None;
                panel.selected_mask = None;
            }
        });
    });
}

/// Convert a screen-space delta to normalized [0,1] mask coordinate delta.
fn seq_delta_to_norm(
    state: &AppState,
    clip_id: mondrian_core::types::ClipId,
    delta: glam::Vec2,
) -> glam::Vec2 {
    let Some(seq) = state.sequence.as_ref() else {
        return delta;
    };
    let current = TimeCode::new(state.current_frame().max(0), seq.time_base());
    let active = seq.active_clips_at(current);
    let Some(ac) = active.iter().find(|a| a.clip.id == clip_id) else {
        return delta;
    };
    let (mw, mh) = state
        .asset_library
        .as_ref()
        .and_then(|lib| lib.get_asset(ac.clip.asset_id).ok().flatten())
        .and_then(|a| a.media_info.primary_video().cloned())
        .map(|v| (v.width as f32, v.height as f32))
        .unwrap_or((1.0, 1.0));
    // Extract rotation-free scale from the inverse transform.
    let inv = ac.transform_matrix.inverse();
    let local = inv.transform_vector2(delta);
    glam::Vec2::new(local.x / mw, local.y / mh)
}

/// Translate a mask shape by a normalized delta.
fn translate_shape(shape: &mut MaskShape, delta: glam::Vec2) {
    match shape {
        MaskShape::Rectangle { x, y, .. } => {
            *x += delta.x;
            *y += delta.y;
        }
        MaskShape::Ellipse { center, .. } => {
            *center += delta;
        }
        MaskShape::Path { points, .. } => {
            for pt in points {
                pt.position += delta;
                pt.control_in += delta;
                pt.control_out += delta;
            }
        }
    }
}

/// Resize a mask shape by dragging a corner. `corner` is 0=TL, 1=TR, 2=BR, 3=BL.
fn resize_shape_corner(shape: &mut MaskShape, corner: usize, delta: glam::Vec2) {
    let bbox = shape_bbox(shape);
    let (mut x1, mut y1, mut x2, mut y2) = (bbox.0.x, bbox.0.y, bbox.1.x, bbox.1.y);
    match corner {
        0 => {
            x1 += delta.x;
            y1 += delta.y;
        }
        1 => {
            x2 += delta.x;
            y1 += delta.y;
        }
        2 => {
            x2 += delta.x;
            y2 += delta.y;
        }
        3 => {
            x1 += delta.x;
            y2 += delta.y;
        }
        _ => return,
    }
    // Maintain minimum size.
    if x2 - x1 < 0.01 {
        x2 = x1 + 0.01;
    }
    if y2 - y1 < 0.01 {
        y2 = y1 + 0.01;
    }
    bbox_to_shape(shape, glam::Vec2::new(x1, y1), glam::Vec2::new(x2, y2));
}

fn shape_bbox(shape: &MaskShape) -> (glam::Vec2, glam::Vec2) {
    match shape {
        MaskShape::Rectangle { x, y, width, height, .. } => (
            glam::Vec2::new(*x, *y),
            glam::Vec2::new(*x + *width, *y + *height),
        ),
        MaskShape::Ellipse { center, radii } => (*center - *radii, *center + *radii),
        MaskShape::Path { points, .. } => {
            let mut min = glam::Vec2::splat(f32::MAX);
            let mut max = glam::Vec2::splat(f32::MIN);
            for pt in points {
                min = min.min(pt.position);
                max = max.max(pt.position);
            }
            (min, max)
        }
    }
}

fn bbox_to_shape(shape: &mut MaskShape, min: glam::Vec2, max: glam::Vec2) {
    match shape {
        MaskShape::Rectangle { x, y, width, height, .. } => {
            *x = min.x;
            *y = min.y;
            *width = max.x - min.x;
            *height = max.y - min.y;
        }
        MaskShape::Ellipse { center, radii } => {
            *center = (min + max) * 0.5;
            *radii = (max - min) * 0.5;
        }
        MaskShape::Path { .. } => {
            // Path resize is more complex; skip for now.
        }
    }
}

/// Directly update mask shape without going through the full undo command.
/// Used for real-time drag updates. The undo snapshot is captured once on release.
fn update_mask_shape_direct(
    state: &mut AppState,
    clip_id: mondrian_core::types::ClipId,
    mask_id: mondrian_effects::mask::MaskId,
    new_shape: MaskShape,
    current_frame: i64,
) {
    let Some(seq) = state.sequence.as_mut() else {
        return;
    };
    let current = TimeCode::new(current_frame.max(0), seq.time_base());
    let ticks = timecode_to_ticks(current);
    for track in &mut seq.video_tracks {
        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
            if let Some(mask) = clip.masks.iter_mut().find(|m| m.id == mask_id) {
                if mask.shape_animation_enabled {
                    // Animated: write a keyframe at the current time.
                    if let Some(pos) = mask.shape_keyframes.iter().position(|(t, _)| *t == ticks) {
                        mask.shape_keyframes[pos].1 = new_shape;
                    } else {
                        mask.shape_keyframes.push((ticks, new_shape));
                        mask.shape_keyframes.sort_by_key(|(t, _)| *t);
                    }
                } else {
                    // Static: update the single stored shape directly.
                    if let Some(first) = mask.shape_keyframes.first_mut() {
                        first.1 = new_shape;
                    }
                }
            }
            break;
        }
    }
}

/// Hit-test a mask shape at screen position. Returns (corner_hit, outline_hit).
/// corner_hit is Some(index) if near a bounding-box corner (0=TL,1=TR,2=BR,3=BL).
/// outline_hit is true if near the shape outline but not a corner.
fn mask_hit_test(
    shape: &MaskShape,
    mw: f32,
    mh: f32,
    mat: &glam::Mat3,
    ct: &crate::ui::viewer::canvas::CanvasTransform,
    screen_pos: Pos2,
) -> (Option<usize>, bool) {
    let to_scr = |x: f32, y: f32| -> Pos2 {
        let t = *mat * glam::Vec3::new(x, y, 1.0);
        ct.seq_to_screen(t.x, t.y)
    };
    const CORNER_RADIUS: f32 = 10.0;

    // For Path shapes: check individual anchor points AND handle endpoints.
    if let MaskShape::Path { points, .. } = shape {
        for (i, pt) in points.iter().enumerate() {
            // Check handle endpoints first (smaller hit target).
            const HANDLE_RADIUS: f32 = 12.0;
            if pt.control_in.length_squared() > 0.01 {
                let cp = to_scr((pt.position.x + pt.control_in.x) * mw, (pt.position.y + pt.control_in.y) * mh);
                if cp.distance(screen_pos) <= HANDLE_RADIUS {
                    return (Some(i), true);
                }
            }
            if pt.control_out.length_squared() > 0.01 {
                let cp = to_scr((pt.position.x + pt.control_out.x) * mw, (pt.position.y + pt.control_out.y) * mh);
                if cp.distance(screen_pos) <= HANDLE_RADIUS {
                    return (Some(i), true);
                }
            }
            // Then check anchor point.
            let sp = to_scr(pt.position.x * mw, pt.position.y * mh);
            if sp.distance(screen_pos) <= CORNER_RADIUS {
                return (Some(i), true);
            }
        }
    }

    let bbox = shape_bbox(shape);
    let corners = [
        to_scr(bbox.0.x * mw, bbox.0.y * mh),
        to_scr(bbox.1.x * mw, bbox.0.y * mh),
        to_scr(bbox.1.x * mw, bbox.1.y * mh),
        to_scr(bbox.0.x * mw, bbox.1.y * mh),
    ];
    for (i, &c) in corners.iter().enumerate() {
        if c.distance(screen_pos) <= CORNER_RADIUS {
            return (Some(i), true);
        }
    }
    // Check proximity to outline by sampling points.
    let outline_pts = shape_outline_points(shape, mw, mh);
    const OUTLINE_RADIUS: f32 = 8.0;
    for pt in &outline_pts {
        let sp = to_scr(pt.x, pt.y);
        if sp.distance(screen_pos) <= OUTLINE_RADIUS {
            return (None, true);
        }
    }
    // Check if inside the shape.
    if is_point_in_mask(shape, mw, mh, mat, ct, screen_pos) {
        return (None, true);
    }
    (None, false)
}

/// Get outline sample points for a mask shape.
fn shape_outline_points(shape: &MaskShape, mw: f32, mh: f32) -> Vec<glam::Vec2> {
    match shape {
        MaskShape::Rectangle { x, y, width, height, .. } => {
            let (x1, y1) = (x * mw, y * mh);
            let (x2, y2) = ((x + width) * mw, (y + height) * mh);
            let mut pts = Vec::new();
            let n = 24;
            for i in 0..n {
                let t = i as f32 / n as f32;
                pts.push(glam::Vec2::new(x1 + (x2 - x1) * t, y1));
                pts.push(glam::Vec2::new(x2, y1 + (y2 - y1) * t));
                pts.push(glam::Vec2::new(x1 + (x2 - x1) * t, y2));
                pts.push(glam::Vec2::new(x1, y1 + (y2 - y1) * t));
            }
            pts
        }
        MaskShape::Ellipse { center, radii } => {
            let (cx, cy) = (center.x * mw, center.y * mh);
            let (rx, ry) = (radii.x * mw, radii.y * mh);
            let n = 64;
            (0..=n)
                .map(|i| {
                    let a = i as f32 * std::f32::consts::TAU / n as f32;
                    glam::Vec2::new(cx + rx * a.cos(), cy + ry * a.sin())
                })
                .collect()
        }
        MaskShape::Path { points, .. } => points
            .iter()
            .map(|p| glam::Vec2::new(p.position.x * mw, p.position.y * mh))
            .collect(),
    }
}

/// Check if a screen point is inside a mask shape.
fn is_point_in_mask(
    shape: &MaskShape,
    mw: f32,
    mh: f32,
    mat: &glam::Mat3,
    ct: &crate::ui::viewer::canvas::CanvasTransform,
    screen_pos: Pos2,
) -> bool {
    // Convert screen → seq → local media coords → normalized.
    let Some((sx, sy)) = ct.screen_to_seq(screen_pos) else {
        return false;
    };
    let inv = mat.inverse();
    let local = inv.transform_point2(glam::Vec2::new(sx, sy));
    let nx = local.x / mw;
    let ny = local.y / mh;
    match shape {
        MaskShape::Rectangle { x, y, width, height, .. } => {
            nx >= *x && nx <= *x + *width && ny >= *y && ny <= *y + *height
        }
        MaskShape::Ellipse { center, radii } => {
            let dx = (nx - center.x) / radii.x.max(0.001);
            let dy = (ny - center.y) / radii.y.max(0.001);
            dx * dx + dy * dy <= 1.0
        }
        MaskShape::Path { points, closed } => {
            if !closed {
                return false;
            }
            // Build polygon from sampled Bézier segments in media-pixel space.
            let segs = mask_path_segments(points, true);
            if segs.is_empty() {
                return false;
            }
            let mut poly: Vec<glam::Vec2> = Vec::with_capacity(segs.len());
            for &(a, _) in &segs {
                poly.push(glam::Vec2::new(a.x * mw, a.y * mh));
            }
            point_in_polygon(&poly, local)
        }
    }
}

/// Even-odd rule point-in-polygon test.
fn point_in_polygon(poly: &[glam::Vec2], pt: glam::Vec2) -> bool {
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let yi = poly[i].y;
        let yj = poly[j].y;
        if (yi > pt.y) != (yj > pt.y) {
            let x_intersect = poly[i].x + (poly[j].x - poly[i].x) * (pt.y - yi) / (yj - yi);
            if pt.x < x_intersect {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Convert a rectangle in sequence space to clip-local normalized [0,1] coordinates.
fn seq_rect_to_clip_normalized(
    state: &AppState,
    clip_id: mondrian_core::types::ClipId,
    min_seq: glam::Vec2,
    max_seq: glam::Vec2,
) -> (glam::Vec2, glam::Vec2) {
    let Some(seq) = state.sequence.as_ref() else {
        return (min_seq, max_seq);
    };
    let current = TimeCode::new(state.current_frame().max(0), seq.time_base());
    let active = seq.active_clips_at(current);
    let Some(ac) = active.iter().find(|a| a.clip.id == clip_id) else {
        return (min_seq, max_seq);
    };
    let (mw, mh) = state
        .asset_library
        .as_ref()
        .and_then(|lib| lib.get_asset(ac.clip.asset_id).ok().flatten())
        .and_then(|a| a.media_info.primary_video().cloned())
        .map(|v| (v.width as f32, v.height as f32))
        .unwrap_or((1.0, 1.0));
    let inv = ac.transform_matrix.inverse();
    let local_min = inv.transform_point2(min_seq);
    let local_max = inv.transform_point2(max_seq);
    let norm_min = glam::Vec2::new(local_min.x / mw, local_min.y / mh);
    let norm_max = glam::Vec2::new(local_max.x / mw, local_max.y / mh);
    (norm_min, norm_max)
}

/// Convert a single sequence-space point to clip-local normalized [0,1].
fn seq_point_to_clip_normalized(
    state: &AppState,
    clip_id: mondrian_core::types::ClipId,
    seq_pt: glam::Vec2,
) -> (f32, f32) {
    let Some(seq) = state.sequence.as_ref() else {
        return (seq_pt.x, seq_pt.y);
    };
    let current = TimeCode::new(state.current_frame().max(0), seq.time_base());
    let active = seq.active_clips_at(current);
    let Some(ac) = active.iter().find(|a| a.clip.id == clip_id) else {
        return (seq_pt.x, seq_pt.y);
    };
    let (mw, mh) = state
        .asset_library
        .as_ref()
        .and_then(|lib| lib.get_asset(ac.clip.asset_id).ok().flatten())
        .and_then(|a| a.media_info.primary_video().cloned())
        .map(|v| (v.width as f32, v.height as f32))
        .unwrap_or((1.0, 1.0));
    let inv = ac.transform_matrix.inverse();
    let local = inv.transform_point2(seq_pt);
    (local.x / mw, local.y / mh)
}

/// Draw a mask shape preview directly in sequence space (no clip transform).
fn draw_mask_preview_polygon(
    painter: &egui::Painter,
    pts: &[glam::Vec2],
    ct: &crate::ui::viewer::canvas::CanvasTransform,
    stroke: egui::Stroke,
    closed: bool,
) {
    if pts.len() < 2 {
        return;
    }
    let cp: Vec<Pos2> = pts.iter().map(|&p| ct.seq_to_screen(p.x, p.y)).collect();
    let n = if closed { cp.len() } else { cp.len() - 1 };
    for i in 0..n {
        painter.line_segment([cp[i], cp[(i + 1) % cp.len()]], stroke);
    }
}

/// Generate the next auto-incremented mask name for a clip.
fn mask_next_name(state: &AppState, clip_id: mondrian_core::types::ClipId) -> String {
    let seq = state.sequence.as_ref();
    let count = seq
        .and_then(|s| {
            s.video_tracks.iter().find_map(|t| {
                t.clips.iter().find(|c| c.id == clip_id).map(|c| c.masks.len())
            })
        })
        .unwrap_or(0);
    format!("蒙版 {}", count + 1)
}

/// Render the canvas context menu popup. Returns true when the menu should close.
/// Draw selection labels at top-left of selected clips and masks on the canvas.
fn draw_selection_labels(
    painter: &egui::Painter,
    ct: &crate::ui::viewer::canvas::CanvasTransform,
    state: &AppState,
    timeline_frame: i64,
    selected_clip: Option<(mondrian_core::types::TrackId, bool, mondrian_core::types::ClipId)>,
    selected_mask: Option<(mondrian_effects::mask::MaskId, mondrian_core::types::ClipId, mondrian_core::types::TrackId)>,
) {
    let Some(seq) = state.sequence.as_ref() else { return };
    let current = TimeCode::new(timeline_frame.max(0), seq.time_base());
    let active = seq.active_clips_at(current);
    let ticks = timecode_to_ticks(current);

    if let Some((_, _, sel_cid)) = selected_clip {
        if let Some(ac) = active.iter().find(|a| a.clip.id == sel_cid) {
            let bb = clip_screen_bounds_with_media(&ac.clip, ac.transform_matrix, ct, state);
            if let Some(bb) = bb {
                let label = ac.clip.label.as_deref().filter(|l| !l.is_empty()).unwrap_or("片段");
                draw_label_badge(painter, bb.left_top(), label, egui::Color32::from_rgb(0, 180, 255));

                // Mask labels
                if let Some((_mid, _mcid, _tid)) = selected_mask {
                    for mask in &ac.clip.masks {
                        if !mask.enabled { continue; }
                        if !selected_mask.is_some_and(|(mid, _, _)| mid == mask.id) { continue; }
                        let kf = mask.evaluate_at(ticks);
                        let bbox = shape_bbox(&kf.shape);
                        let (mw, mh) = state.asset_library.as_ref()
                            .and_then(|lib| lib.get_asset(ac.clip.asset_id).ok().flatten())
                            .and_then(|a| a.media_info.primary_video().cloned())
                            .map(|v| (v.width as f32, v.height as f32))
                            .unwrap_or((1.0, 1.0));
                        // Convert normalized bbox corners to screen
                        let tl = glam::Vec2::new(bbox.0.x * mw, bbox.0.y * mh);
                        let sp = ct.seq_to_screen(
                            (ac.transform_matrix * tl.extend(1.0)).x,
                            (ac.transform_matrix * tl.extend(1.0)).y,
                        );
                        draw_label_badge(painter, sp, &mask.name, egui::Color32::from_rgb(200, 120, 0));
                    }
                }
            }
        }
    }
}

/// Draw a rounded-rectangle label badge at a screen position.
fn draw_label_badge(painter: &egui::Painter, top_left: Pos2, text: &str, color: egui::Color32) {
    let font = typography::body_small();
    let galley = painter.layout_no_wrap(text.to_string(), font.clone(), egui::Color32::WHITE);
    let pad = egui::vec2(6.0, 3.0);
    let size = galley.size() + pad * 2.0;
    let rect = egui::Rect::from_min_size(top_left - egui::vec2(0.0, size.y + 4.0), size);
    let bg = egui::Color32::from_rgba_premultiplied(
        color.r(), color.g(), color.b(), 200,
    );
    painter.rect_filled(rect, egui::CornerRadius::same(4), bg);
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(4),
        egui::Stroke::new(1.0, color),
        egui::StrokeKind::Inside,
    );
    let max_w = 140.0;
    let display = if galley.size().x > max_w {
        let mut s = text.to_string();
        let mut best = s.clone();
        while s.len() > 3 {
            s.pop();
            let g = painter.layout_no_wrap(format!("{s}…"), font.clone(), egui::Color32::WHITE);
            if g.size().x <= max_w {
                best = format!("{s}…");
                break;
            }
        }
        best
    } else {
        text.to_string()
    };
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        display,
        font,
        egui::Color32::WHITE,
    );
}

/// Move a single path point by a normalized delta.
fn move_path_point(shape: &mut MaskShape, idx: usize, delta: glam::Vec2) {
    if let MaskShape::Path { points, .. } = shape {
        if let Some(pt) = points.get_mut(idx) {
            pt.position += delta;
            // Handles are relative to position; position move suffices.
        }
    }
}

/// Move a single path control handle by a normalized delta.
fn move_path_handle(shape: &mut MaskShape, idx: usize, delta: glam::Vec2, is_in: bool) {
    if let MaskShape::Path { points, .. } = shape {
        if let Some(pt) = points.get_mut(idx) {
            if is_in {
                pt.control_in += delta;
            } else {
                pt.control_out += delta;
            }
        }
    }
}

/// Determine edit mode for a path point click: handle drag vs anchor move.
fn path_point_edit_mode(
    state: &AppState,
    ct: &crate::ui::viewer::canvas::CanvasTransform,
    clip_id: mondrian_core::types::ClipId,
    points: &[mondrian_effects::mask::BezierPoint],
    idx: usize,
    screen_pos: Pos2,
) -> Option<MaskEditMode> {
    let pt = points.get(idx)?;
    let seq = state.sequence.as_ref()?;
    let current = TimeCode::new(state.current_frame().max(0), seq.time_base());
    let ac = seq.active_clips_at(current).into_iter().find(|a| a.clip.id == clip_id)?;
    let (mw, mh) = state.asset_library.as_ref()
        .and_then(|lib| lib.get_asset(ac.clip.asset_id).ok().flatten())
        .and_then(|a| a.media_info.primary_video().cloned())
        .map(|v| (v.width as f32, v.height as f32))
        .unwrap_or((1.0, 1.0));
    let mat = &ac.transform_matrix;
    let to_scr = |px: f32, py: f32| -> Pos2 {
        let t = *mat * glam::Vec3::new(px, py, 1.0);
        ct.seq_to_screen(t.x, t.y)
    };
    const HANDLE_RADIUS: f32 = 10.0;
    // Check control_in handle.
    if pt.control_in.length_squared() > 0.01 || pt.control_out.length_squared() > 0.01 {
        let cp_in = to_scr((pt.position.x + pt.control_in.x) * mw, (pt.position.y + pt.control_in.y) * mh);
        if cp_in.distance(screen_pos) <= HANDLE_RADIUS {
            return Some(MaskEditMode::MovePathHandle(idx, true));
        }
        let cp_out = to_scr((pt.position.x + pt.control_out.x) * mw, (pt.position.y + pt.control_out.y) * mh);
        if cp_out.distance(screen_pos) <= HANDLE_RADIUS {
            return Some(MaskEditMode::MovePathHandle(idx, false));
        }
    }
    // Default: move the anchor point.
    Some(MaskEditMode::MovePathPoint(idx))
}

#[cfg(test)]
#[path = "viewer_panel_perf_tests.rs"]
mod perf_tests;

#[cfg(test)]
#[path = "viewer_panel_transform_tests.rs"]
mod transform_tests;
