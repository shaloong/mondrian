use crate::{
    app::AppState,
    ui::theme::{self, palette, tokens, typography},
};
use egui::{Pos2, Rect, Sense, Ui, Vec2};

use mondrian_core::types::{AssetId, Rational, TimeCode};
use mondrian_media::cache::FrameCacheConfig;
use mondrian_media::{DecoderPool, FrameCache, RgbaFrame};
use mondrian_renderer::{CompositorConfig, CpuRgbaLayer, FrameCompositor, GpuContext};
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
    source_secs: f64,
    source_time_base: Rational,
    opacity: f32,
    transform: [f32; 6],
}

#[derive(Clone)]
struct DecodeRequest {
    signature: CompositeFrameSignature,
    layers: Vec<LayerDecodeRequest>,
    playback_mode: bool,
    target_width: u32,
    target_height: u32,
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
struct LayerSignature {
    asset_id: AssetId,
    source_frame: i64,
    opacity_u8: u8,
    transform_key: [i32; 6],
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompositeFrameSignature {
    width: u32,
    height: u32,
    layers: Vec<LayerSignature>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LayerFrameCacheKey {
    asset_id: AssetId,
    source_frame: i64,
    target_width: u32,
    target_height: u32,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
enum PreviewScaleMode {
    #[default]
    Full,
    Half,
    Quarter,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ViewerPreferences {
    preview_scale_mode: PreviewScaleMode,
    proxy_config: mondrian_media::ProxyConfig,
    #[serde(default)]
    decode_backend: mondrian_media::PreviewDecodeBackend,
    #[serde(default = "default_prefetch_enabled")]
    prefetch_enabled: bool,
    #[serde(default = "default_layer_cache_enabled")]
    layer_cache_enabled: bool,
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
    was_playing_last_frame: bool,
    last_timeline_frame: Option<i64>,
    proxy_config: mondrian_media::ProxyConfig,
    decode_backend: mondrian_media::PreviewDecodeBackend,
    prefetch_enabled: bool,
    layer_cache_enabled: bool,
    proxy_jobs_in_flight: Arc<Mutex<HashSet<AssetId>>>,
    proxy_done_tx: Sender<AssetId>,
    proxy_done_rx: Receiver<AssetId>,
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
            was_playing_last_frame: false,
            last_timeline_frame: None,
            proxy_config: mondrian_media::ProxyConfig::default(),
            decode_backend: mondrian_media::PreviewDecodeBackend::default(),
            prefetch_enabled: default_prefetch_enabled(),
            layer_cache_enabled: default_layer_cache_enabled(),
            proxy_jobs_in_flight: Arc::new(Mutex::new(HashSet::new())),
            proxy_done_tx,
            proxy_done_rx,
        }
    }
}

impl ViewerPanel {
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
        let current_frame = state.current_frame();
        let playback_fps = state
            .sequence
            .as_ref()
            .map(|seq| seq.settings.frame_rate.to_f64())
            .unwrap_or(25.0)
            .max(1.0);
        self.handle_timeline_discontinuity(current_frame);

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

        ui.vertical(|ui| {
            let controls_height = tokens::viewer_transport_height();
            let transport_gap = 4.0;
            let canvas_slot_height =
                (ui.available_height() - controls_height - transport_gap).max(120.0);
            let (canvas_slot_rect, _) = ui.allocate_exact_size(
                Vec2::new(ui.available_width(), canvas_slot_height),
                Sense::hover(),
            );

            let aspect = state
                .sequence
                .as_ref()
                .map(|s| s.settings.resolution.aspect_ratio())
                .unwrap_or(16.0 / 9.0)
                .max(0.01);
            let fitted = fit_aspect(canvas_slot_rect, aspect);
            let canvas_rect = Rect::from_min_size(
                Pos2::new(fitted.left(), canvas_slot_rect.top()),
                fitted.size(),
            );

            let painter = ui.painter_at(canvas_rect);
            painter.rect_filled(canvas_rect, 0.0, palette::canvas_bg());

                if state.sequence.is_none() {
                    self.invalidate_pending_decode();
                    self.desired_signature = None;
                    self.asset_preview_cache.clear();
                    draw_checkerboard(&painter, canvas_rect);
                    draw_empty_canvas_meta(&painter, canvas_rect, state, current_frame);
                }

                if let Some(seq) = state.sequence.as_ref() {
                    if let Some(lib) = state.asset_library.as_ref() {
                        let base_width =
                            scaled_dimension(canvas_rect.width(), self.preview_scale_mode.factor());
                        let base_height =
                            scaled_dimension(canvas_rect.height(), self.preview_scale_mode.factor());
                        let (target_width, target_height) =
                            playback_adjusted_target_size(base_width, base_height, is_playing);

                        let layers_started_at = Instant::now();
                        let layers =
                            self.build_layer_decode_requests(seq, lib.as_ref(), state, current_frame);
                        if diag_enabled {
                            let elapsed_ms = layers_started_at.elapsed().as_millis() as u64;
                            if elapsed_ms >= preview_diag_slow_threshold_ms() {
                                tracing::warn!(
                                    "[preview-diag] build_layer_decode_requests slow: {}ms frame={} layers={}",
                                    elapsed_ms,
                                    current_frame,
                                    layers.len()
                                );
                            }
                        }
                        let layer_signatures = layers
                            .iter()
                            .map(|layer| LayerSignature {
                                asset_id: layer.frame_key.0,
                                source_frame: layer.frame_key.1,
                                opacity_u8: (layer.opacity * 255.0).round() as u8,
                                transform_key: quantize_transform_signature(layer.transform),
                            })
                            .collect::<Vec<_>>();

                        if layers.is_empty() {
                            self.invalidate_pending_decode();
                            self.preview_texture = None;
                            self.preview_signature = None;
                            self.desired_signature = None;
                            self.preview_error = None;
                            self.clear_prefetch_in_flight();
                            draw_checkerboard(&painter, canvas_rect);
                        } else {
                            let signature = CompositeFrameSignature {
                                width: target_width,
                                height: target_height,
                                layers: layer_signatures,
                            };
                            self.desired_signature = Some(signature.clone());

                            if self.preview_signature.as_ref() != Some(&signature) {
                                if let Some(cached) = self.cache_get(&signature) {
                                    self.preview_texture = Some(cached);
                                    self.preview_signature = Some(signature.clone());
                                    self.preview_error = None;
                                } else {
                                    self.request_decode(
                                        DecodeRequest {
                                            signature,
                                            layers,
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
                                painter.image(
                                    texture.id(),
                                    canvas_rect,
                                    Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                                    palette::image_tint(),
                                );
                            } else {
                                draw_checkerboard(&painter, canvas_rect);
                                draw_empty_canvas_meta(&painter, canvas_rect, state, current_frame);
                            }

                            if let Some(err) = &self.preview_error {
                                painter.text(
                                    canvas_rect.center_bottom() + Vec2::new(0.0, -14.0),
                                    egui::Align2::CENTER_BOTTOM,
                                    format!("预览解码失败：{}", err),
                                    typography::body(),
                                    palette::status_error(),
                                );
                            }
                        }
                    } else {
                        self.invalidate_pending_decode();
                        self.preview_texture = None;
                        self.preview_signature = None;
                        self.desired_signature = None;
                        self.preview_error = None;
                        self.clear_prefetch_in_flight();
                        draw_checkerboard(&painter, canvas_rect);
                        draw_empty_canvas_meta(&painter, canvas_rect, state, current_frame);
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

            let controls_rect = Rect::from_min_size(
                Pos2::new(canvas_slot_rect.left(), canvas_rect.bottom() + transport_gap),
                Vec2::new(canvas_slot_rect.width(), controls_height.max(28.0)),
            );
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(controls_rect), |ui| {
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
            egui::Stroke::new(1.0, palette::overlay_stroke()),
        );

        ui.allocate_new_ui(
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

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(left_rect), |ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(tc.to_smpte())
                        .font(typography::mono_small())
                        .color(palette::text_primary()),
                );
            });
        });

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(center_rect), |ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let btn_w = 34.0;
                let btn_h = 22.0;
                let mark_w = 30.0;
                let gap = 6.0;
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

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(right_rect), |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let combo_id = ui.make_persistent_id("viewer_res");
                let combo_open = ui.memory(|m| {
                    m.is_popup_open(combo_id) || m.is_popup_open(combo_id.with("popup"))
                });

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

    fn handle_timeline_discontinuity(&mut self, current_frame: i64) {
        // 跳帧 ≥ 8 帧视为一次 seek，取消所有预取并重置预取缓冲状态。
        const SEEK_RESET_THRESHOLD_FRAMES: i64 = 8;
        // 跳帧 ≥ 300 帧（~12秒 @ 25fps）视为大跳帧，额外清理 DecoderPool 内 RGBA 缓存，
        // 防止大 seek 后旧缓存帧污染新位置的画面。
        const LARGE_SEEK_THRESHOLD_FRAMES: i64 = 300;

        let Some(previous_frame) = self.last_timeline_frame else {
            return;
        };

        let delta = (current_frame - previous_frame).abs();
        if delta < SEEK_RESET_THRESHOLD_FRAMES {
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

        let mut layer_request_cache: HashMap<i64, Arc<Vec<LayerDecodeRequest>>> = HashMap::new();

        let prefill_active = is_playing && self.playback_prefill_active();

        let active_layer_count = self
            .build_layer_decode_requests_cached(
                seq,
                lib,
                state,
                current_frame,
                &mut layer_request_cache,
            )
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
            let layers = self.build_layer_decode_requests_cached(
                seq,
                lib,
                state,
                timeline_frame,
                &mut layer_request_cache,
            );

            for layer in layers.iter() {
                let cache_key = LayerFrameCacheKey {
                    asset_id: layer.frame_key.0,
                    source_frame: layer.frame_key.1,
                    target_width,
                    target_height,
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

    fn build_layer_decode_requests(
        &mut self,
        seq: &mondrian_timeline::sequence::Sequence,
        lib: &mondrian_assets::AssetLibrary,
        state: &AppState,
        timeline_frame: i64,
    ) -> Vec<LayerDecodeRequest> {
        let current = TimeCode::new(timeline_frame, seq.time_base());
        let active = seq.active_clips_at(current);
        let mut layers = Vec::new();

        for active_clip in active {
            let asset_id = active_clip.clip.asset_id;
            let cached = if let Some(hit) = self.asset_preview_cache.get(&asset_id) {
                hit.clone()
            } else {
                let loaded =
                    lib.get_asset(asset_id).ok().flatten().map(|asset| CachedAssetPreview {
                        is_video: matches!(asset.kind, mondrian_assets::AssetKind::Video),
                        source_path: asset.path,
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

            let source_frame = active_clip.source_time.frame.max(0);
            let opacity = active_clip.opacity.clamp(0.0, 1.0);
            let path = self.resolve_preview_source_path(
                asset_id,
                asset.source_path.as_path(),
                state.is_asset_proxy_mode(asset_id),
            );
            layers.push(LayerDecodeRequest {
                frame_key: (asset_id, source_frame),
                path,
                source_secs: active_clip.source_time.to_secs().max(0.0),
                source_time_base: active_clip.source_time.time_base,
                opacity,
                transform: mat3_to_affine(active_clip.transform_matrix.to_cols_array()),
            });
        }

        layers
    }

    fn build_layer_decode_requests_cached(
        &mut self,
        seq: &mondrian_timeline::sequence::Sequence,
        lib: &mondrian_assets::AssetLibrary,
        state: &AppState,
        timeline_frame: i64,
        request_cache: &mut HashMap<i64, Arc<Vec<LayerDecodeRequest>>>,
    ) -> Arc<Vec<LayerDecodeRequest>> {
        if let Some(cached) = request_cache.get(&timeline_frame) {
            return Arc::clone(cached);
        }

        let layers = Arc::new(self.build_layer_decode_requests(seq, lib, state, timeline_frame));
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
        layer_request_cache: &mut HashMap<i64, Arc<Vec<LayerDecodeRequest>>>,
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

            let layers = self.build_layer_decode_requests_cached(
                seq,
                lib,
                state,
                timeline_frame,
                layer_request_cache,
            );
            for layer in layers.iter() {
                total += 1;
                let key = LayerFrameCacheKey {
                    asset_id: layer.frame_key.0,
                    source_frame: layer.frame_key.1,
                    target_width,
                    target_height,
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
        layer_request_cache: &mut HashMap<i64, Arc<Vec<LayerDecodeRequest>>>,
    ) -> i64 {
        let mut ready_frames = 0i64;

        for step in 1..=target_frames.max(1) {
            let timeline_frame = current_frame + step * direction;
            if timeline_frame < 0 {
                break;
            }

            let layers = self.build_layer_decode_requests_cached(
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
                let key = LayerFrameCacheKey {
                    asset_id: layer.frame_key.0,
                    source_frame: layer.frame_key.1,
                    target_width,
                    target_height,
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
        }
    }

    pub fn apply_preferences(&mut self, preferences: &ViewerPreferences) {
        self.preview_scale_mode = preferences.preview_scale_mode;
        self.proxy_config = preferences.proxy_config.clone();
        self.decode_backend = preferences.decode_backend;
        mondrian_media::set_preview_decode_backend(preferences.decode_backend);
        self.set_prefetch_enabled(preferences.prefetch_enabled);
        self.set_layer_cache_enabled(preferences.layer_cache_enabled);
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

fn decode_composited_rgba(request: &DecodeRequest) -> anyhow::Result<RgbaFrame> {
    let decode_started_at = Instant::now();
    if request.generation != request.latest_generation.load(Ordering::Relaxed) {
        return Err(anyhow::anyhow!("decode cancelled by newer generation"));
    }

    let width = request.target_width.max(1);
    let height = request.target_height.max(1);

    let mut decoded_layers = 0usize;
    let mut last_error: Option<anyhow::Error> = None;
    let mut rgba_layers_for_gpu: Vec<CpuRgbaLayer> = Vec::with_capacity(request.layers.len());
    let mut layer_transforms: Vec<[f32; 6]> = Vec::with_capacity(request.layers.len());

    if request.layers.is_empty() {
        return Err(anyhow::anyhow!("无可用图层可解码"));
    }

    if request.layers.len() == 1 {
        let layer = &request.layers[0];
        match decode_layer_rgba(
            layer,
            width,
            height,
            request.playback_mode,
            request.layer_cache_enabled,
            &request.layer_cache,
            &request.decoder_pool,
        ) {
            Ok(frame) => {
                let RgbaFrame { width, height, data } = frame;
                rgba_layers_for_gpu.push(CpuRgbaLayer {
                    width,
                    height,
                    data,
                    opacity: layer.opacity,
                });
                layer_transforms.push(layer.transform);
                decoded_layers = 1;
            }
            Err(err) => {
                last_error = Some(anyhow::anyhow!(
                    "{}@{} 解码失败: {}",
                    layer.frame_key.0,
                    layer.frame_key.1,
                    err
                ));
            }
        }
    } else {
        let playback_mode = request.playback_mode;
        let layer_cache_enabled = request.layer_cache_enabled;
        let decode_generation = request.generation;
        let latest_generation = Arc::clone(&request.latest_generation);
        let layer_cache = Arc::clone(&request.layer_cache);
        let decoder_pool = Arc::clone(&request.decoder_pool);

        let layer_outputs = preview_decode_pool().install(|| {
            request
                .layers
                .par_iter()
                .cloned()
                .enumerate()
                .map(|(index, layer)| {
                    if decode_generation != latest_generation.load(Ordering::Relaxed) {
                        return (
                            index,
                            Err("decode cancelled by newer generation".to_string()),
                        );
                    }

                    let decoded = decode_layer_rgba(
                        &layer,
                        width,
                        height,
                        playback_mode,
                        layer_cache_enabled,
                        &layer_cache,
                        &decoder_pool,
                    )
                    .map_err(|e| e.to_string());
                    (index, decoded)
                })
                .collect::<Vec<_>>()
        });

        let mut layer_results: Vec<Option<Result<RgbaFrame, String>>> =
            vec![None; request.layers.len()];
        for (index, decoded) in layer_outputs {
            if index < layer_results.len() {
                layer_results[index] = Some(decoded);
            }
        }

        for (index, layer) in request.layers.iter().enumerate() {
            match layer_results.get_mut(index).and_then(Option::take) {
                Some(Ok(frame)) => {
                    let RgbaFrame { width, height, data } = frame;
                    rgba_layers_for_gpu.push(CpuRgbaLayer {
                        width,
                        height,
                        data,
                        opacity: layer.opacity,
                    });
                    layer_transforms.push(layer.transform);
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
    }

    if decoded_layers == 0 {
        return Err(last_error.unwrap_or_else(|| anyhow::anyhow!("无可用图层可解码")));
    }

    let has_non_identity_transform =
        layer_transforms.iter().any(|transform| !is_identity_transform(*transform));

    if rgba_layers_for_gpu.len() == 1 && !has_non_identity_transform {
        let only_layer = rgba_layers_for_gpu.pop().expect("single layer should exist");
        if only_layer.opacity >= 0.999 && only_layer.width == width && only_layer.height == height {
            record_preview_perf_passthrough_frame();
            record_preview_perf_decode_total(decode_started_at.elapsed());
            return Ok(RgbaFrame { width, height, data: only_layer.data });
        }
        rgba_layers_for_gpu.push(only_layer);
    }

    if !has_non_identity_transform {
        if let Some(gpu_rgba) = try_gpu_composite_rgba_layers(width, height, &rgba_layers_for_gpu) {
            record_preview_perf_decode_total(decode_started_at.elapsed());
            return Ok(RgbaFrame { width, height, data: gpu_rgba });
        }
    }

    let cpu_composite_started_at = Instant::now();
    let mut layer_pairs = rgba_layers_for_gpu.into_iter().zip(layer_transforms);
    let Some((first_layer, first_transform)) = layer_pairs.next() else {
        return Err(anyhow::anyhow!("无可用图层可合成"));
    };

    let mut canvas = vec![0u8; (width as usize) * (height as usize) * 4];
    initialize_canvas_alpha_opaque(&mut canvas);
    alpha_blend_layer(
        &mut canvas,
        width,
        height,
        &first_layer.data,
        first_layer.width,
        first_layer.height,
        first_layer.opacity,
        first_transform,
    );

    for (layer, transform) in layer_pairs {
        alpha_blend_layer(
            &mut canvas,
            width,
            height,
            &layer.data,
            layer.width,
            layer.height,
            layer.opacity,
            transform,
        );
    }

    record_preview_perf_composite_ns(cpu_composite_started_at.elapsed().as_nanos() as u64, false);
    record_preview_perf_decode_total(decode_started_at.elapsed());

    Ok(RgbaFrame { width, height, data: canvas })
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

fn mat3_to_affine(cols: [f32; 9]) -> [f32; 6] {
    [cols[0], cols[3], cols[6], cols[1], cols[4], cols[7]]
}

fn quantize_transform_signature(transform: [f32; 6]) -> [i32; 6] {
    const SCALE: f32 = 1024.0;
    [
        (transform[0] * SCALE).round() as i32,
        (transform[1] * SCALE).round() as i32,
        (transform[2] * SCALE).round() as i32,
        (transform[3] * SCALE).round() as i32,
        (transform[4] * SCALE).round() as i32,
        (transform[5] * SCALE).round() as i32,
    ]
}

fn is_identity_transform(transform: [f32; 6]) -> bool {
    const EPS: f32 = 1.0e-4;
    (transform[0] - 1.0).abs() <= EPS
        && transform[1].abs() <= EPS
        && transform[2].abs() <= EPS
        && transform[3].abs() <= EPS
        && (transform[4] - 1.0).abs() <= EPS
        && transform[5].abs() <= EPS
}

fn invert_affine(transform: [f32; 6]) -> Option<[f32; 6]> {
    let a = transform[0];
    let c = transform[1];
    let tx = transform[2];
    let b = transform[3];
    let d = transform[4];
    let ty = transform[5];

    let det = a * d - b * c;
    if det.abs() <= 1.0e-6 {
        return None;
    }

    let inv_det = 1.0 / det;
    let ia = d * inv_det;
    let ic = -c * inv_det;
    let ib = -b * inv_det;
    let id = a * inv_det;
    let itx = -(ia * tx + ic * ty);
    let ity = -(ib * tx + id * ty);
    Some([ia, ic, itx, ib, id, ity])
}

fn sample_src_rgba(
    src_rgba: &[u8],
    src_w: usize,
    src_h: usize,
    sx: f32,
    sy: f32,
) -> Option<[u8; 4]> {
    let x = sx.round() as isize;
    let y = sy.round() as isize;
    if x < 0 || y < 0 || x >= src_w as isize || y >= src_h as isize {
        return None;
    }

    let idx = (y as usize * src_w + x as usize) * 4;
    if idx + 3 >= src_rgba.len() {
        return None;
    }
    Some([
        src_rgba[idx],
        src_rgba[idx + 1],
        src_rgba[idx + 2],
        src_rgba[idx + 3],
    ])
}

fn blend_pixel_with_alpha(
    dst_px: &mut [u8],
    src_px: [u8; 4],
    opacity_u8: u32,
    alpha_table: &[u8; 65_536],
) {
    let opacity_key = (opacity_u8.min(255) as usize) << 8;
    let alpha = alpha_table[opacity_key | src_px[3] as usize] as u32;
    if alpha == 0 {
        return;
    }

    if alpha >= 255 {
        dst_px[0] = src_px[0];
        dst_px[1] = src_px[1];
        dst_px[2] = src_px[2];
        dst_px[3] = 255;
        return;
    }

    let inv_alpha = 255 - alpha;
    let src_r = src_px[0] as u32;
    let src_g = src_px[1] as u32;
    let src_b = src_px[2] as u32;
    let dst_r = dst_px[0] as u32;
    let dst_g = dst_px[1] as u32;
    let dst_b = dst_px[2] as u32;

    dst_px[0] = blend_channel_u8(src_r, dst_r, alpha, inv_alpha);
    dst_px[1] = blend_channel_u8(src_g, dst_g, alpha, inv_alpha);
    dst_px[2] = blend_channel_u8(src_b, dst_b, alpha, inv_alpha);
}

fn alpha_blend_layer(
    dst_rgba: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    src_rgba: &[u8],
    src_w: u32,
    src_h: u32,
    opacity: f32,
    transform: [f32; 6],
) {
    let width = dst_w.min(src_w) as usize;
    let height = dst_h.min(src_h) as usize;
    let opacity_u8 = (opacity.clamp(0.0, 1.0) * 255.0).round() as u32;

    if opacity_u8 == 0 {
        return;
    }

    let alpha_table = alpha_blend_table();
    let dst_stride = dst_w as usize * 4;
    let src_stride = src_w as usize * 4;

    if is_identity_transform(transform) {
        if should_parallel_blend(width, height) {
            dst_rgba
                .par_chunks_mut(dst_stride)
                .take(height)
                .zip(src_rgba.par_chunks(src_stride).take(height))
                .with_min_len(blend_parallel_min_rows())
                .for_each(|(dst_row, src_row)| {
                    blend_row_with_table(dst_row, src_row, width, opacity_u8, alpha_table)
                });
        } else {
            for y in 0..height {
                let dst_row = &mut dst_rgba[y * dst_stride..(y + 1) * dst_stride];
                let src_row = &src_rgba[y * src_stride..(y + 1) * src_stride];
                blend_row_with_table(dst_row, src_row, width, opacity_u8, alpha_table);
            }
        }
        return;
    }

    let Some(inv) = invert_affine(transform) else {
        return;
    };

    let dst_width = dst_w as usize;
    let dst_height = dst_h as usize;
    let src_width = src_w as usize;
    let src_height = src_h as usize;

    for dy in 0..dst_height {
        for dx in 0..dst_width {
            let fx = dx as f32 + 0.5;
            let fy = dy as f32 + 0.5;
            let sx = inv[0] * fx + inv[1] * fy + inv[2];
            let sy = inv[3] * fx + inv[4] * fy + inv[5];
            let Some(src_px) = sample_src_rgba(src_rgba, src_width, src_height, sx - 0.5, sy - 0.5)
            else {
                continue;
            };

            let dst_idx = (dy * dst_width + dx) * 4;
            if dst_idx + 3 >= dst_rgba.len() {
                continue;
            }
            let dst_px = &mut dst_rgba[dst_idx..dst_idx + 4];
            blend_pixel_with_alpha(dst_px, src_px, opacity_u8, alpha_table);
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

fn gpu_compositor_enabled() -> bool {
    !gpu_compositor_disabled_flag().load(Ordering::Relaxed)
}

fn record_gpu_compositor_result(success: bool) {
    const MAX_FAILURES: u64 = 4;

    if success {
        gpu_compositor_failures().store(0, Ordering::Relaxed);
        gpu_compositor_disabled_flag().store(false, Ordering::Relaxed);
        return;
    }

    let failures = gpu_compositor_failures().fetch_add(1, Ordering::Relaxed) + 1;
    if failures >= MAX_FAILURES {
        gpu_compositor_disabled_flag().store(true, Ordering::Relaxed);
    }
}

fn gpu_compositor_failures() -> &'static AtomicU64 {
    static FAILURES: OnceLock<AtomicU64> = OnceLock::new();
    FAILURES.get_or_init(|| AtomicU64::new(0))
}

fn gpu_compositor_disabled_flag() -> &'static AtomicBool {
    static DISABLED: OnceLock<AtomicBool> = OnceLock::new();
    DISABLED.get_or_init(|| AtomicBool::new(false))
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
        target_width: width,
        target_height: height,
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
        let frame = (*frame).clone();
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
        Some(width),
        Some(height),
    )
    .map(|frame| {
        if layer_cache_enabled {
            layer_cache_put(layer_cache, cache_key, frame.clone());
        }
        record_preview_perf_layer_decode(started_at.elapsed(), false);
        frame
    })?)
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

fn playback_adjusted_target_size(width: u32, height: u32, is_playing: bool) -> (u32, u32) {
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

fn quantize_dimension(value: u32, step: u32) -> u32 {
    let step = step.max(1);
    let rounded = ((value + (step / 2)) / step).saturating_mul(step);
    if rounded % 2 == 1 {
        rounded.saturating_sub(1).max(1)
    } else {
        rounded.max(1)
    }
}

fn initialize_canvas_alpha_opaque(canvas: &mut [u8]) {
    let pixels = canvas.len() / 4;
    if pixels >= 1_000_000 {
        canvas
            .par_chunks_mut(4)
            .with_min_len(alpha_init_parallel_min_chunk_pixels())
            .for_each(|px| px[3] = 255);
    } else {
        for px in canvas.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }
}

fn should_parallel_blend(width: usize, height: usize) -> bool {
    let pixels = width.saturating_mul(height);
    pixels >= 1_000_000
}

fn blend_parallel_min_rows() -> usize {
    16
}

fn alpha_init_parallel_min_chunk_pixels() -> usize {
    8_192
}

fn alpha_blend_table() -> &'static [u8; 65_536] {
    static TABLE: OnceLock<Box<[u8; 65_536]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = Box::new([0u8; 65_536]);
        for opacity in 0u32..=255 {
            let base = (opacity as usize) << 8;
            for src_alpha in 0u32..=255 {
                table[base | src_alpha as usize] = ((src_alpha * opacity + 127) / 255) as u8;
            }
        }
        table
    })
}

fn blend_row_with_table(
    dst_row: &mut [u8],
    src_row: &[u8],
    width: usize,
    opacity_u8: u32,
    alpha_table: &[u8; 65_536],
) {
    let pixel_bytes = width.saturating_mul(4);
    if dst_row.len() < pixel_bytes || src_row.len() < pixel_bytes {
        return;
    }

    let opacity_key = (opacity_u8.min(255) as usize) << 8;

    for (dst_px, src_px) in dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(4)).take(width) {
        // Integer alpha blend keeps math branch-light on CPU hot path.
        let alpha = alpha_table[opacity_key | src_px[3] as usize] as u32;
        if alpha == 0 {
            continue;
        }

        if alpha >= 255 {
            dst_px[0] = src_px[0];
            dst_px[1] = src_px[1];
            dst_px[2] = src_px[2];
            dst_px[3] = 255;
            continue;
        }

        let inv_alpha = 255 - alpha;

        let src_r = src_px[0] as u32;
        let src_g = src_px[1] as u32;
        let src_b = src_px[2] as u32;

        let dst_r = dst_px[0] as u32;
        let dst_g = dst_px[1] as u32;
        let dst_b = dst_px[2] as u32;

        dst_px[0] = blend_channel_u8(src_r, dst_r, alpha, inv_alpha);
        dst_px[1] = blend_channel_u8(src_g, dst_g, alpha, inv_alpha);
        dst_px[2] = blend_channel_u8(src_b, dst_b, alpha, inv_alpha);
    }
}

#[inline]
fn blend_channel_u8(src: u32, dst: u32, alpha: u32, inv_alpha: u32) -> u8 {
    let value = src * alpha + dst * inv_alpha + 127;
    ((value + (value >> 8)) >> 8) as u8
}

/// 将矩形按指定宽高比居中裁剪（letterbox / pillarbox）
fn fit_aspect(outer: Rect, aspect: f32) -> Rect {
    let outer_aspect = outer.width() / outer.height();
    if outer_aspect > aspect {
        // 左右留黑边
        let w = outer.height() * aspect;
        let x = outer.left() + (outer.width() - w) * 0.5;
        Rect::from_min_size(Pos2::new(x, outer.top()), Vec2::new(w, outer.height()))
    } else {
        // 上下留黑边
        let h = outer.width() / aspect;
        let y = outer.top() + (outer.height() - h) * 0.5;
        Rect::from_min_size(Pos2::new(outer.left(), y), Vec2::new(outer.width(), h))
    }
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

#[cfg(test)]
#[path = "viewer_panel_perf_tests.rs"]
mod perf_tests;

#[cfg(test)]
#[path = "viewer_panel_transform_tests.rs"]
mod transform_tests;
