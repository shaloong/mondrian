mod draw;
use crate::{
    app::AppState,
    ui::theme::{self, palette, tokens, typography},
};
pub(crate) use draw::*;
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
    CpuRgbaLayer, TimelineAdjustmentLayer, TimelineCompositeElement, TimelineCompositeOptions,
    TimelineCompositeScratch, TimelineMediaLayer, TimelineRenderPlanElement,
    TimelineSolidColorLayer,
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

use crate::ui::viewer::gpu_composite::{
    apply_gpu_color_conversion, create_rgba_texture, gpu_device, gpu_queue, recycle_texture,
    try_gpu_composite_rgba_layers, try_gpu_composite_to_texture, try_reuse_texture,
};
use crate::ui::viewer::gpu_texture::CompositedFrame;

#[derive(Clone)]
pub(crate) struct LayerDecodeRequest {
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
pub(crate) struct AdjustmentRenderRequest {
    effect_graph: std::sync::Arc<CompiledEffectGraph>,
    opacity: f32,
    blend_mode: Option<BlendMode>,
    frame_seed: i64,
}

#[derive(Clone)]
pub(crate) struct NestedSequenceRenderRequest {
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
pub(crate) struct SolidColorRenderRequest {
    color: Color,
    opacity: f32,
    blend_mode: BlendMode,
    transform: [f32; 6],
    effect_graph: std::sync::Arc<CompiledEffectGraph>,
    frame_seed: i64,
}

#[derive(Clone)]
pub(crate) enum RenderElement {
    Media(LayerDecodeRequest),
    Adjustment(AdjustmentRenderRequest),
    SolidColor(SolidColorRenderRequest),
    NestedSequence(NestedSequenceRenderRequest),
}

#[derive(Clone)]
pub(crate) struct DecodeRequest {
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
    /// Optional zero-copy GPU composited frame.
    gpu_frame: Option<CompositedFrame>,
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
pub(crate) enum LayerSignature {
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
pub(crate) struct CompositeFrameSignature {
    width: u32,
    height: u32,
    working_color_space: ColorSpace,
    output_color_space: ColorSpace,
    display_profile_key: u64,
    tone_map: bool,
    layers: Vec<LayerSignature>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct LayerFrameCacheKey {
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
pub(crate) struct LayerFrameCache {
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
    /// GPU composited frame for zero-copy callback rendering.
    gpu_composited_frame: Option<CompositedFrame>,
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
    context_menu_hit: Option<(SelectedClipRef, Option<mondrian_effects::mask::MaskId>)>,
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
pub(crate) enum MaskEditMode {
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

                let (decoded, gpu_frame) = match decode_composited_rgba(&request) {
                    Ok((rgba, gf)) => (Ok(rgba), gf),
                    Err(e) => (Err(e.to_string()), None),
                };
                let _ = tx.send(DecodeResult {
                    signature: request.signature,
                    decoded,
                    gpu_frame,
                    generation: request.generation,
                });
            });
        }

        let (proxy_done_tx, proxy_done_rx) = mpsc::channel();
        Self {
            preview_texture: None,
            gpu_composited_frame: None,
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
    fn set_clip_selection(&mut self, state: &mut AppState, sel: Option<(TrackId, bool, ClipId)>) {
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
        if !ui.ctx().egui_wants_keyboard_input() {
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
                    let scroll = ctx.input(|inp| inp.smooth_scroll_delta().y);
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
                            if let Some(old) = self.gpu_composited_frame.take() {
                                let (tex, w, h) = old.into_parts();
                                recycle_texture(tex, w, h);
                            }
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

                            if let Some(gpu_frame) = &self.gpu_composited_frame {
                                let callback = egui_wgpu::Callback::new_paint_callback(
                                    content_rect,
                                    gpu_frame.clone(),
                                );
                                painter.add(egui::Shape::Callback(callback));
                            } else if let Some(texture) = &self.preview_texture {
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
                        if let Some(old) = self.gpu_composited_frame.take() {
                            let (tex, w, h) = old.into_parts();
                            recycle_texture(tex, w, h);
                        }
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
            seq.settings.color_space,
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
            // Recycle previous frame's texture before creating a new one.
            if let Some(old_frame) = self.gpu_composited_frame.take() {
                let (tex, w, h) = old_frame.into_parts();
                recycle_texture(tex, w, h);
            }

            // GPU zero-copy frame from decode thread — skip CPU upload entirely.
            if let Some(gf) = result.gpu_frame {
                self.gpu_composited_frame = Some(gf);
                self.preview_signature = Some(result.signature);
                self.preview_error = None;
                self.last_committed_generation = Some(result.generation);
                self.last_texture_commit_at = Some(Instant::now());
            } else {
                match result.decoded {
                    Ok(frame) => {
                        let upload_started_at = Instant::now();
                        if let (Some(device), Some(queue)) = (gpu_device(), gpu_queue()) {
                            // Try to reuse a recycled texture before creating new.
                            let tex = try_reuse_texture(
                                &device,
                                &queue,
                                frame.width,
                                frame.height,
                                &frame.data,
                            )
                            .unwrap_or_else(|| {
                                create_rgba_texture(
                                    &device,
                                    &queue,
                                    frame.width,
                                    frame.height,
                                    &frame.data,
                                )
                            });
                            self.gpu_composited_frame = Some(CompositedFrame::new(
                                &device,
                                tex,
                                frame.width,
                                frame.height,
                            ));
                        } else {
                            // No GPU — fall back to egui texture manager.
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
pub(crate) struct PrefetchBudget {
    frames_ahead: i64,
    max_in_flight: usize,
    max_spawn_per_tick: usize,
}

mod helpers;
pub(crate) use helpers::*;

#[cfg(test)]
#[path = "../viewer_panel_perf_tests.rs"]
mod perf_tests;

#[cfg(test)]
#[path = "../viewer_panel_transform_tests.rs"]
mod transform_tests;
