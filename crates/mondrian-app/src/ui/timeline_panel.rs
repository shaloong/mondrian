use crate::{
    app::{AppState, ClipOverlapMode},
    ui::theme::{self, palette, tokens, typography},
};
use egui::{Color32, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use mondrian_core::types::{ClipId, Rational, TrackId};
use mondrian_timeline::clip::TrimEdge;
use mondrian_timeline::sequence::{Sequence, VideoDisplayFormat};
use std::collections::{HashMap, HashSet};

fn corner_radius(value: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(value.round().clamp(0.0, 255.0) as u8)
}

// ─── TimelinePanel ──────────────────────────

#[derive(Default)]
pub struct TimelinePanel {
    /// 水平缩放：每帧占多少像素
    pixels_per_frame: f32,
    track_height: f32,
    /// 水平滚动偏移（帧数）
    scroll_offset_frames: f64,
    timeline_viewport_width: f32,
    vertical_scroll_offset: f32,
    timeline_vertical_viewport_height: f32,
    timeline_track_content_height: f32,
    clip_drag: Option<ClipDragState>,
    clip_drag_anchors: Vec<ClipDragAnchor>,
    clip_drag_moved: bool,
    clip_drag_before_sequence: Option<Sequence>,
    selected_clips: HashSet<ClipSelection>,
    track_area_bounds: Option<Rect>,
    marquee_anchor: Option<Pos2>,
    marquee_current: Option<Pos2>,
    marquee_additive: bool,
    active_tool: TimelineTool,
    snap_enabled: bool,
    active_snap_guide_frame: Option<i64>,
    active_insert_guide_frame: Option<i64>,
    track_drag: Option<TrackDragState>,
    track_drag_target: Option<TrackDragTarget>,
    scrollbar_drag: Option<TimelineScrollbarDragState>,
    vertical_scrollbar_drag: Option<TimelineVerticalScrollbarDragState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum TimelineTool {
    #[default]
    Select,
    Blade,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SnapPriority {
    Playhead,
    AdjacentClipEdge,
    Marker,
    InOutPoint,
}

#[derive(Debug, Clone, Copy)]
struct SnapCandidate {
    frame: i64,
    priority: SnapPriority,
}

#[derive(Debug, Clone, Copy)]
struct SnapDecision {
    frame: i64,
    snapped: bool,
}

#[derive(Clone, Copy)]
struct ClipDragState {
    clip_id: ClipId,
    is_video_track: bool,
    pointer_offset_frames: i64,
}

#[derive(Clone, Copy)]
struct ClipDragAnchor {
    clip_id: ClipId,
    start_frame: i64,
}

#[derive(Debug, Clone, Copy)]
struct TrackDragState {
    track_id: TrackId,
    is_video_track: bool,
    source_index: usize,
}

#[derive(Debug, Clone, Copy)]
struct TrackDragTarget {
    is_video_track: bool,
    target_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ClipSelection {
    track_id: mondrian_core::types::TrackId,
    is_video_track: bool,
    clip_id: ClipId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SelectedClipRef {
    pub track_id: TrackId,
    pub is_video_track: bool,
    pub clip_id: ClipId,
}

#[derive(Clone, Copy)]
struct ClipVisual {
    selection: ClipSelection,
    rect: Rect,
}

#[derive(Clone, Copy)]
struct TrackRowVisual {
    track_id: TrackId,
    is_video_track: bool,
    track_index: usize,
    rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimelineScrollbarDragKind {
    Thumb,
    LeadingHandle,
    TrailingHandle,
}

#[derive(Debug, Clone, Copy)]
struct TimelineScrollbarDragState {
    kind: TimelineScrollbarDragKind,
    start_pointer_x: f32,
    start_offset_frames: f64,
    start_visible_span_frames: f64,
}

#[derive(Debug, Clone, Copy)]
struct TimelineScrollbarMetrics {
    total_frames: f64,
    visible_span_frames: f64,
    offset_frames: f64,
    thumb_left: f32,
    thumb_width: f32,
    track_width: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimelineVerticalScrollbarDragKind {
    Thumb,
    LeadingHandle,
    TrailingHandle,
}

#[derive(Debug, Clone, Copy)]
struct TimelineVerticalScrollbarDragState {
    kind: TimelineVerticalScrollbarDragKind,
    start_pointer_y: f32,
    start_offset: f32,
    start_track_height: f32,
}

#[derive(Debug, Clone, Copy)]
struct TimelineVerticalScrollbarMetrics {
    total_rows: f32,
    visible_rows: f32,
    thumb_top: f32,
    thumb_height: f32,
    track_height: f32,
}

impl TimelinePanel {
    pub fn selected_clip_count(&self) -> usize {
        self.selected_clips.len()
    }

    pub fn selected_clip_ref(&self) -> Option<SelectedClipRef> {
        if self.selected_clips.len() != 1 {
            return None;
        }

        self.selected_clips.iter().next().map(|selection| SelectedClipRef {
            track_id: selection.track_id,
            is_video_track: selection.is_video_track,
            clip_id: selection.clip_id,
        })
    }

    pub fn show(&mut self, ui: &mut Ui, state: &mut AppState) {
        // 初始化默认缩放
        if self.pixels_per_frame == 0.0 {
            self.pixels_per_frame = tokens::timeline_default_pixels_per_frame();
            self.snap_enabled = true;
        }
        if self.track_height == 0.0 {
            self.track_height = tokens::timeline_track_height();
        }
        self.active_snap_guide_frame = None;
        self.active_insert_guide_frame = None;

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                self.draw_timeline_tools_toolbar(ui);
            });
            ui.add_space(tokens::panel_gap());

            if state.sequence.is_none() {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        egui::RichText::new("暂无序列")
                            .font(typography::body())
                            .color(palette::text_muted()),
                    );
                });
                return;
            }

            let vertical_bar_w = tokens::timeline_scrollbar_width();
            let scrollbar_gap = tokens::panel_gap() * 0.4;
            let bottom_scrollbar_height = tokens::timeline_scrollbar_height();
            let min_main_height = tokens::timeline_ruler_height()
                + self.track_height.max(tokens::timeline_min_track_height()) * 2.0;
            let main_height = (ui.available_height() - bottom_scrollbar_height - scrollbar_gap)
                .max(min_main_height);

            ui.allocate_ui_with_layout(
                Vec2::new(ui.available_width(), main_height),
                egui::Layout::left_to_right(egui::Align::Min),
                |ui| {
                    let left_width = (ui.available_width() - vertical_bar_w - 6.0).max(120.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(left_width, ui.available_height()),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            egui::ScrollArea::horizontal()
                                .id_salt("timeline_hscroll")
                                .horizontal_scroll_offset(
                                    (self.scroll_offset_frames.max(0.0) as f32)
                                        * self.pixels_per_frame.max(0.01),
                                )
                                .scroll_bar_visibility(
                                    egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                                )
                                .show_viewport(ui, |ui, viewport| {
                                    let content_w =
                                        self.timeline_content_width(state).max(viewport.width());
                                    ui.set_min_width(content_w);
                                    self.timeline_viewport_width = viewport.width();

                                    self.scroll_offset_frames =
                                        (viewport.left().max(0.0) / self.pixels_per_frame) as f64;

                                    let ruler_resp = self.draw_ruler(ui, state);
                                    if let Some(clicked_frame) = ruler_resp {
                                        state.seek(clicked_frame);
                                    }

                                    self.clamp_vertical_scroll_offset_for_viewport(
                                        state,
                                        ui.available_height(),
                                    );
                                    egui::ScrollArea::vertical()
                                        .id_salt("timeline_vscroll")
                                        .vertical_scroll_offset(self.vertical_scroll_offset)
                                        .scroll_bar_visibility(
                                            egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                                        )
                                        .show_viewport(ui, |ui, viewport| {
                                            self.timeline_vertical_viewport_height =
                                                viewport.height();
                                            self.vertical_scroll_offset = viewport.top().max(0.0);
                                            self.timeline_track_content_height =
                                                self.timeline_total_track_height(state);

                                            if !ui.ctx().wants_keyboard_input() {
                                                if ui.input(|i| {
                                                    i.key_pressed(egui::Key::I)
                                                        && !i.modifiers.command
                                                        && !i.modifiers.alt
                                                }) {
                                                    state.mark_in_at_current_frame();
                                                }

                                                if ui.input(|i| {
                                                    i.key_pressed(egui::Key::O)
                                                        && !i.modifiers.command
                                                        && !i.modifiers.alt
                                                }) {
                                                    state.mark_out_at_current_frame();
                                                }

                                                if ui.input(|i| {
                                                    i.modifiers.command
                                                        && i.key_pressed(egui::Key::B)
                                                }) {
                                                    let _ = state.split_at_playhead();
                                                }

                                                if ui.input(|i| {
                                                    i.modifiers.alt
                                                        && i.modifiers.shift
                                                        && i.key_pressed(egui::Key::ArrowLeft)
                                                }) {
                                                    self.slide_selected_clips_by_frames(state, -1);
                                                }

                                                if ui.input(|i| {
                                                    i.modifiers.alt
                                                        && i.modifiers.shift
                                                        && i.key_pressed(egui::Key::ArrowRight)
                                                }) {
                                                    self.slide_selected_clips_by_frames(state, 1);
                                                }

                                                if ui.input(|i| {
                                                    i.modifiers.alt
                                                        && !i.modifiers.shift
                                                        && i.key_pressed(egui::Key::ArrowLeft)
                                                }) {
                                                    self.slip_selected_clips_by_frames(state, -1);
                                                }

                                                if ui.input(|i| {
                                                    i.modifiers.alt
                                                        && !i.modifiers.shift
                                                        && i.key_pressed(egui::Key::ArrowRight)
                                                }) {
                                                    self.slip_selected_clips_by_frames(state, 1);
                                                }

                                                if ui.input(|i| {
                                                    i.modifiers.command
                                                        && !i.modifiers.shift
                                                        && i.key_pressed(egui::Key::Z)
                                                }) {
                                                    let _ = state.undo_timeline();
                                                }

                                                if ui.input(|i| {
                                                    i.modifiers.command
                                                        && ((i.modifiers.shift
                                                            && i.key_pressed(egui::Key::Z))
                                                            || i.key_pressed(egui::Key::Y))
                                                }) {
                                                    let _ = state.redo_timeline();
                                                }
                                            }

                                            let dropped = self.draw_tracks(ui, state);
                                            if ui.input(|i| i.pointer.any_released()) && !dropped {
                                                state.clear_dragging_asset();
                                            }

                                            if ui.input(|i| {
                                                i.key_pressed(egui::Key::Delete)
                                                    || i.key_pressed(egui::Key::Backspace)
                                            }) {
                                                let ripple = ui.input(|i| i.modifiers.shift);
                                                self.delete_selected_clips(state, ripple);
                                            }

                                            if ui.input(|i| i.pointer.any_released()) {
                                                if let Some(track_drag) = self.track_drag.take() {
                                                    if let Some(target) =
                                                        self.track_drag_target.take()
                                                    {
                                                        if track_drag.is_video_track
                                                            == target.is_video_track
                                                            && track_drag.source_index
                                                                != target.target_index
                                                        {
                                                            if let Err(err) = state.move_track(
                                                                track_drag.track_id,
                                                                track_drag.is_video_track,
                                                                target.target_index,
                                                            ) {
                                                                state.set_status_hint(
                                                                    format!("移动轨道失败：{err}"),
                                                                    true,
                                                                );
                                                            }
                                                        }
                                                    }
                                                }

                                                if self.clip_drag.take().is_some() {
                                                    let pointer =
                                                        ui.input(|i| i.pointer.interact_pos());
                                                    let dropped_outside =
                                                        match (pointer, self.track_area_bounds) {
                                                            (Some(p), Some(bounds)) => {
                                                                !bounds.contains(p)
                                                            }
                                                            _ => false,
                                                        };

                                                    if dropped_outside && self.clip_drag_moved {
                                                        self.delete_selected_clips(state, false);
                                                    } else if self.clip_drag_moved {
                                                        if let Some(before) =
                                                            self.clip_drag_before_sequence.take()
                                                        {
                                                            state.record_timeline_edit_snapshot(
                                                                "移动片段",
                                                                before,
                                                            );
                                                        } else {
                                                            let _ = state.save_project();
                                                        }
                                                    }
                                                }
                                                self.clip_drag_anchors.clear();
                                                self.clip_drag_before_sequence = None;
                                                self.clip_drag_moved = false;
                                                self.track_drag_target = None;
                                            }
                                        });
                                });
                        },
                    );

                    ui.add_space(6.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(vertical_bar_w, ui.available_height()),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.add_space(tokens::timeline_ruler_height());
                            self.draw_right_scrollbar(ui, state);
                        },
                    );
                },
            );
            ui.add_space(scrollbar_gap);
            self.draw_bottom_scrollbar(ui, state);
        });
    }

    fn timeline_content_width(&self, state: &AppState) -> f32 {
        let track_label_w = tokens::timeline_track_label_width();
        if state.sequence.is_none() {
            return track_label_w + 1200.0;
        }

        track_label_w + self.timeline_total_frame_span(state) as f32 * self.pixels_per_frame
    }

    fn draw_timeline_tools_toolbar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if theme::icon_toggle_button(
                ui,
                tokens::timeline_toolbar_button_size(),
                theme::UiIcon::Cursor,
                self.active_tool == TimelineTool::Select,
            )
            .clicked()
            {
                self.active_tool = TimelineTool::Select;
            }
            if theme::icon_toggle_button(
                ui,
                tokens::timeline_toolbar_button_size(),
                theme::UiIcon::Scissors,
                self.active_tool == TimelineTool::Blade,
            )
            .clicked()
            {
                self.active_tool = TimelineTool::Blade;
            }
            let _ = theme::icon_toggle_button(
                ui,
                tokens::timeline_toolbar_button_size(),
                theme::UiIcon::Magnet,
                self.snap_enabled,
            )
            .on_hover_text("自动吸附")
            .clicked()
            .then(|| self.snap_enabled = !self.snap_enabled);
        });
    }

    fn timeline_total_frame_span(&self, state: &AppState) -> i64 {
        let Some(seq) = state.sequence.as_ref() else {
            return tokens::timeline_right_padding_frames_min();
        };

        let fps = seq.settings.frame_rate.to_f64().round() as i64;
        let right_padding_frames = (fps.max(1)
            * tokens::timeline_right_padding_frames_multiplier())
        .max(tokens::timeline_right_padding_frames_min());
        seq.total_duration()
            .frame
            .max(state.current_frame())
            .max(tokens::timeline_right_padding_frames_min())
            + right_padding_frames
    }

    fn visible_frame_span(&self) -> f64 {
        if self.timeline_viewport_width <= 0.0 || self.pixels_per_frame <= 0.0 {
            return 1.0;
        }
        (self.timeline_viewport_width / self.pixels_per_frame).max(1.0) as f64
    }

    fn update_visible_frame_span(&mut self, visible_span_frames: f64, total_frames: f64) {
        if self.timeline_viewport_width <= 0.0 || total_frames <= 0.0 {
            return;
        }
        let (min_visible, max_visible) =
            timeline_visible_span_bounds(total_frames, self.timeline_viewport_width);
        let clamped_visible = visible_span_frames.clamp(min_visible, max_visible);
        self.pixels_per_frame =
            (self.timeline_viewport_width.max(1.0) as f64 / clamped_visible).max(0.01) as f32;
        let max_offset = (total_frames - clamped_visible).max(0.0);
        self.scroll_offset_frames = self.scroll_offset_frames.clamp(0.0, max_offset);
    }

    fn clamp_vertical_scroll_offset_for_viewport(&mut self, state: &AppState, visible_height: f32) {
        let total_height = self.timeline_total_track_height(state);
        let max_offset = (total_height - visible_height.max(0.0)).max(0.0);
        self.vertical_scroll_offset = self.vertical_scroll_offset.clamp(0.0, max_offset);
    }

    fn timeline_total_track_height(&self, state: &AppState) -> f32 {
        state.sequence.as_ref().map_or(0.0, |seq| {
            (seq.video_tracks.len() + seq.audio_tracks.len()) as f32 * self.track_height
        })
    }

    fn draw_right_scrollbar(&mut self, ui: &mut Ui, state: &AppState) {
        let total_rows = state.sequence.as_ref().map_or(0.0, |seq| {
            (seq.video_tracks.len() + seq.audio_tracks.len()) as f32
        });
        let visible_height = self.timeline_vertical_viewport_height.max(0.0);
        if visible_height <= 0.0 {
            self.vertical_scrollbar_drag = None;
            ui.allocate_exact_size(
                Vec2::new(tokens::timeline_scrollbar_width(), 0.0),
                Sense::hover(),
            );
            return;
        }

        let gutter_width = tokens::timeline_scrollbar_width() + 4.0;
        let side_padding = ((gutter_width - tokens::timeline_scrollbar_width()) * 0.5).max(0.0);
        let handle_radius = (tokens::timeline_scrollbar_width() * 0.5).min(5.0);
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(gutter_width, visible_height), Sense::hover());
        let track_rect = Rect::from_min_max(
            Pos2::new(
                rect.left() + 2.0 + side_padding,
                rect.top() + 2.0 + handle_radius,
            ),
            Pos2::new(
                rect.right() - 2.0 - side_padding,
                rect.bottom() - 2.0 - handle_radius,
            ),
        );
        let metrics = timeline_vertical_scrollbar_metrics(
            track_rect.height(),
            total_rows,
            visible_height,
            self.track_height.max(tokens::timeline_min_track_height()),
            self.vertical_scroll_offset,
        );
        let thumb_rect = Rect::from_min_max(
            Pos2::new(track_rect.left(), track_rect.top() + metrics.thumb_top),
            Pos2::new(
                track_rect.right(),
                track_rect.top() + metrics.thumb_top + metrics.thumb_height,
            ),
        );
        let painter = ui.painter_at(rect);
        painter.rect_filled(track_rect, corner_radius(4.0), palette::bg_surface_raised());
        painter.rect_stroke(
            track_rect,
            corner_radius(4.0),
            Stroke::new(1.0, palette::border_subtle().gamma_multiply(0.9)),
            egui::StrokeKind::Inside,
        );

        let top_handle_center = Pos2::new(thumb_rect.center().x, thumb_rect.top());
        let bottom_handle_center = Pos2::new(thumb_rect.center().x, thumb_rect.bottom());
        let top_handle_rect =
            Rect::from_center_size(top_handle_center, Vec2::splat(handle_radius * 2.0));
        let bottom_handle_rect =
            Rect::from_center_size(bottom_handle_center, Vec2::splat(handle_radius * 2.0));
        let thumb_drag_rect = Rect::from_min_max(
            Pos2::new(
                thumb_rect.left(),
                (thumb_rect.top() + handle_radius).min(thumb_rect.bottom()),
            ),
            Pos2::new(
                thumb_rect.right(),
                (thumb_rect.bottom() - handle_radius).max(thumb_rect.top()),
            ),
        );

        let top_handle_response = ui.interact(
            top_handle_rect.expand2(Vec2::splat(4.0)),
            ui.make_persistent_id("timeline_scrollbar_vertical_top_handle"),
            Sense::click_and_drag(),
        );
        let bottom_handle_response = ui.interact(
            bottom_handle_rect.expand2(Vec2::splat(4.0)),
            ui.make_persistent_id("timeline_scrollbar_vertical_bottom_handle"),
            Sense::click_and_drag(),
        );
        let thumb_response = ui.interact(
            thumb_drag_rect,
            ui.make_persistent_id("timeline_scrollbar_vertical_thumb"),
            Sense::click_and_drag(),
        );
        let track_response = ui.interact(
            track_rect,
            ui.make_persistent_id("timeline_scrollbar_vertical_track"),
            Sense::click(),
        );

        if top_handle_response.drag_started() {
            if let Some(pointer) = top_handle_response.interact_pointer_pos() {
                self.vertical_scrollbar_drag = Some(TimelineVerticalScrollbarDragState {
                    kind: TimelineVerticalScrollbarDragKind::LeadingHandle,
                    start_pointer_y: pointer.y,
                    start_offset: self.vertical_scroll_offset,
                    start_track_height: self.track_height,
                });
            }
        } else if bottom_handle_response.drag_started() {
            if let Some(pointer) = bottom_handle_response.interact_pointer_pos() {
                self.vertical_scrollbar_drag = Some(TimelineVerticalScrollbarDragState {
                    kind: TimelineVerticalScrollbarDragKind::TrailingHandle,
                    start_pointer_y: pointer.y,
                    start_offset: self.vertical_scroll_offset,
                    start_track_height: self.track_height,
                });
            }
        } else if thumb_response.drag_started() {
            if let Some(pointer) = thumb_response.interact_pointer_pos() {
                self.vertical_scrollbar_drag = Some(TimelineVerticalScrollbarDragState {
                    kind: TimelineVerticalScrollbarDragKind::Thumb,
                    start_pointer_y: pointer.y,
                    start_offset: self.vertical_scroll_offset,
                    start_track_height: self.track_height,
                });
            }
        } else if track_response.clicked() {
            if let Some(pointer) = track_response.interact_pointer_pos() {
                if !thumb_rect.contains(pointer) {
                    let fraction =
                        ((pointer.y - track_rect.top()) / track_rect.height()).clamp(0.0, 1.0);
                    let target_center = fraction * metrics.total_rows;
                    let offset_rows = (target_center - metrics.visible_rows * 0.5)
                        .clamp(0.0, (metrics.total_rows - metrics.visible_rows).max(0.0));
                    self.vertical_scroll_offset = offset_rows * self.track_height.max(1.0);
                }
            }
        }

        if let Some(drag) = self.vertical_scrollbar_drag {
            if ui.input(|i| i.pointer.primary_down()) {
                if let Some(pointer) = ui.input(|i| i.pointer.interact_pos()) {
                    let delta = pointer.y - drag.start_pointer_y;
                    match drag.kind {
                        TimelineVerticalScrollbarDragKind::Thumb => {
                            self.vertical_scroll_offset =
                                apply_timeline_vertical_scrollbar_drag(metrics, drag, delta);
                        }
                        TimelineVerticalScrollbarDragKind::LeadingHandle
                        | TimelineVerticalScrollbarDragKind::TrailingHandle => {
                            let (new_track_height, new_offset) = apply_timeline_vertical_zoom_drag(
                                metrics,
                                drag,
                                delta,
                                visible_height,
                            );
                            self.track_height = new_track_height;
                            self.vertical_scroll_offset = new_offset;
                            self.clamp_vertical_scroll_offset_for_viewport(state, visible_height);
                        }
                    }
                    ui.ctx().request_repaint();
                }
            } else {
                self.vertical_scrollbar_drag = None;
            }
        }

        let active_kind = self.vertical_scrollbar_drag.map(|drag| drag.kind);
        let thumb_fill = if thumb_response.hovered()
            || active_kind == Some(TimelineVerticalScrollbarDragKind::Thumb)
        {
            palette::interaction_highlight()
        } else {
            palette::bg_surface_hover()
        };
        painter.rect_filled(thumb_rect, corner_radius(4.0), thumb_fill);
        painter.rect_stroke(
            thumb_rect,
            corner_radius(4.0),
            Stroke::new(1.0, palette::interaction_highlight().gamma_multiply(0.8)),
            egui::StrokeKind::Inside,
        );
        for (center, active) in [
            (
                top_handle_center,
                top_handle_response.hovered()
                    || active_kind == Some(TimelineVerticalScrollbarDragKind::LeadingHandle),
            ),
            (
                bottom_handle_center,
                bottom_handle_response.hovered()
                    || active_kind == Some(TimelineVerticalScrollbarDragKind::TrailingHandle),
            ),
        ] {
            painter.circle_filled(
                center,
                handle_radius,
                if active {
                    palette::interaction_highlight()
                } else {
                    palette::bg_surface_hover()
                },
            );
            painter.circle_stroke(
                center,
                handle_radius,
                Stroke::new(1.0, palette::text_primary().gamma_multiply(0.7)),
            );
        }
    }

    fn draw_bottom_scrollbar(&mut self, ui: &mut Ui, state: &AppState) {
        let Some(_seq) = state.sequence.as_ref() else {
            self.scrollbar_drag = None;
            return;
        };

        let full_rect = ui
            .allocate_exact_size(
                Vec2::new(ui.available_width(), tokens::timeline_scrollbar_height()),
                Sense::hover(),
            )
            .0;
        let track_label_w = tokens::timeline_track_label_width();
        let right_scrollbar_gutter_w = tokens::timeline_scrollbar_width() + 10.0;
        let track_rect = Rect::from_min_max(
            Pos2::new(full_rect.left() + track_label_w, full_rect.top()),
            Pos2::new(
                (full_rect.right() - right_scrollbar_gutter_w)
                    .max(full_rect.left() + track_label_w),
                full_rect.bottom(),
            ),
        )
        .shrink2(Vec2::new(0.0, 2.0));
        if track_rect.width() <= 0.0 {
            self.scrollbar_drag = None;
            return;
        }

        let total_frames = self.timeline_total_frame_span(state) as f64;
        self.update_visible_frame_span(self.visible_frame_span(), total_frames);
        let metrics = timeline_scrollbar_metrics(
            track_rect.width(),
            total_frames,
            self.timeline_viewport_width.max(track_rect.width()),
            self.pixels_per_frame.max(0.01),
            self.scroll_offset_frames,
        );
        let thumb_rect = Rect::from_min_max(
            Pos2::new(track_rect.left() + metrics.thumb_left, track_rect.top()),
            Pos2::new(
                track_rect.left() + metrics.thumb_left + metrics.thumb_width,
                track_rect.bottom(),
            ),
        );

        let painter = ui.painter_at(full_rect);
        painter.rect_filled(track_rect, corner_radius(4.0), palette::bg_surface_raised());
        painter.rect_stroke(
            track_rect,
            corner_radius(4.0),
            Stroke::new(1.0, palette::border_subtle().gamma_multiply(0.9)),
            egui::StrokeKind::Inside,
        );

        let handle_radius =
            (track_rect.height() * 0.5).min(tokens::timeline_scrollbar_handle_width());
        let left_handle_center = Pos2::new(thumb_rect.left(), thumb_rect.center().y);
        let right_handle_center = Pos2::new(thumb_rect.right(), thumb_rect.center().y);
        let left_handle_rect =
            Rect::from_center_size(left_handle_center, Vec2::splat(handle_radius * 2.0));
        let right_handle_rect =
            Rect::from_center_size(right_handle_center, Vec2::splat(handle_radius * 2.0));
        let thumb_drag_rect = Rect::from_min_max(
            Pos2::new(
                (thumb_rect.left() + handle_radius).min(thumb_rect.right()),
                thumb_rect.top(),
            ),
            Pos2::new(
                (thumb_rect.right() - handle_radius).max(thumb_rect.left()),
                thumb_rect.bottom(),
            ),
        );

        let left_handle_response = ui.interact(
            left_handle_rect.expand2(Vec2::splat(4.0)),
            ui.make_persistent_id("timeline_scrollbar_left_handle"),
            Sense::click_and_drag(),
        );
        let right_handle_response = ui.interact(
            right_handle_rect.expand2(Vec2::splat(4.0)),
            ui.make_persistent_id("timeline_scrollbar_right_handle"),
            Sense::click_and_drag(),
        );
        let thumb_response = ui.interact(
            thumb_drag_rect,
            ui.make_persistent_id("timeline_scrollbar_thumb"),
            Sense::click_and_drag(),
        );
        let track_response = ui.interact(
            track_rect,
            ui.make_persistent_id("timeline_scrollbar_track"),
            Sense::click(),
        );

        if left_handle_response.drag_started() {
            if let Some(pointer) = left_handle_response.interact_pointer_pos() {
                self.scrollbar_drag = Some(TimelineScrollbarDragState {
                    kind: TimelineScrollbarDragKind::LeadingHandle,
                    start_pointer_x: pointer.x,
                    start_offset_frames: metrics.offset_frames,
                    start_visible_span_frames: metrics.visible_span_frames,
                });
            }
        } else if right_handle_response.drag_started() {
            if let Some(pointer) = right_handle_response.interact_pointer_pos() {
                self.scrollbar_drag = Some(TimelineScrollbarDragState {
                    kind: TimelineScrollbarDragKind::TrailingHandle,
                    start_pointer_x: pointer.x,
                    start_offset_frames: metrics.offset_frames,
                    start_visible_span_frames: metrics.visible_span_frames,
                });
            }
        } else if thumb_response.drag_started() {
            if let Some(pointer) = thumb_response.interact_pointer_pos() {
                self.scrollbar_drag = Some(TimelineScrollbarDragState {
                    kind: TimelineScrollbarDragKind::Thumb,
                    start_pointer_x: pointer.x,
                    start_offset_frames: metrics.offset_frames,
                    start_visible_span_frames: metrics.visible_span_frames,
                });
            }
        } else if track_response.clicked() {
            if let Some(pointer) = track_response.interact_pointer_pos() {
                if !thumb_rect.contains(pointer) {
                    let fraction = ((pointer.x - track_rect.left()) / track_rect.width())
                        .clamp(0.0, 1.0) as f64;
                    let target_center = fraction * metrics.total_frames;
                    let new_offset = (target_center - metrics.visible_span_frames * 0.5).clamp(
                        0.0,
                        (metrics.total_frames - metrics.visible_span_frames).max(0.0),
                    );
                    self.scroll_offset_frames = new_offset;
                }
            }
        }

        if let Some(drag) = self.scrollbar_drag {
            if ui.input(|i| i.pointer.primary_down()) {
                if let Some(pointer) = ui.input(|i| i.pointer.interact_pos()) {
                    let delta_px = pointer.x - drag.start_pointer_x;
                    let (min_visible, max_visible) = timeline_visible_span_bounds(
                        metrics.total_frames,
                        self.timeline_viewport_width.max(track_rect.width()),
                    );
                    let (new_offset, new_visible) = apply_timeline_scrollbar_drag(
                        metrics,
                        drag,
                        delta_px,
                        min_visible,
                        max_visible,
                    );
                    self.scroll_offset_frames = new_offset;
                    self.update_visible_frame_span(new_visible, metrics.total_frames);
                    ui.ctx().request_repaint();
                }
            } else {
                self.scrollbar_drag = None;
            }
        }

        let active_drag_kind = self.scrollbar_drag.map(|drag| drag.kind);
        let thumb_fill = if thumb_response.hovered()
            || active_drag_kind == Some(TimelineScrollbarDragKind::Thumb)
        {
            palette::interaction_highlight()
        } else {
            palette::bg_surface_hover()
        };
        painter.rect_filled(thumb_rect, corner_radius(4.0), thumb_fill);
        painter.rect_stroke(
            thumb_rect,
            corner_radius(4.0),
            Stroke::new(1.0, palette::interaction_highlight().gamma_multiply(0.8)),
            egui::StrokeKind::Inside,
        );

        for (center, active) in [
            (
                left_handle_center,
                left_handle_response.hovered()
                    || active_drag_kind == Some(TimelineScrollbarDragKind::LeadingHandle),
            ),
            (
                right_handle_center,
                right_handle_response.hovered()
                    || active_drag_kind == Some(TimelineScrollbarDragKind::TrailingHandle),
            ),
        ] {
            painter.circle_filled(
                center,
                handle_radius,
                if active {
                    palette::interaction_highlight()
                } else {
                    palette::bg_surface_hover()
                },
            );
            painter.circle_stroke(
                center,
                handle_radius,
                Stroke::new(1.0, palette::text_primary().gamma_multiply(0.7)),
            );
        }
    }

    // ─── 时间标尺 ─────────────────────────────
    /// 返回 Some(frame) 若用户点击或拖拽了标尺
    fn draw_ruler(&self, ui: &mut Ui, state: &AppState) -> Option<i64> {
        let track_label_w = tokens::timeline_track_label_width();
        let available_w = ui.available_width() - track_label_w;
        let (rect, resp) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), tokens::timeline_ruler_height()),
            Sense::click_and_drag(), // 支持拖拽以实现标尺 scrub
        );

        let painter = ui.painter_at(rect);
        painter.rect_filled(
            rect,
            tokens::section_rounding(),
            palette::bg_surface_raised(),
        );

        if let Some(out_point) = state.out_point_frame() {
            let in_point = state.in_point_frame().max(0);
            let out_point = out_point.max(in_point);
            let x0 = rect.left() + track_label_w + in_point as f32 * self.pixels_per_frame;
            let x1 = rect.left() + track_label_w + (out_point + 1) as f32 * self.pixels_per_frame;
            let range_rect = Rect::from_min_max(
                Pos2::new(x0.max(rect.left() + track_label_w), rect.top()),
                Pos2::new(x1.min(rect.right()), rect.bottom()),
            );
            if range_rect.min.x < range_rect.max.x {
                painter.rect_filled(
                    range_rect,
                    0.0,
                    palette::interaction_highlight().gamma_multiply(0.28),
                );
            }
        }

        let fps = state
            .sequence
            .as_ref()
            .map(|s| s.settings.frame_rate)
            .unwrap_or(Rational::new(24, 1));
        let display_format = state
            .sequence
            .as_ref()
            .map(|s| s.settings.video_display_format)
            .unwrap_or(VideoDisplayFormat::Frames);
        let ruler_scale = choose_ruler_scale(self.pixels_per_frame, fps);

        let start_frame = self.scroll_offset_frames as i64;
        let end_frame = start_frame
            + (available_w / self.pixels_per_frame) as i64
            + ruler_scale.major_step_frames;

        // 次刻度（更细分辨率）
        if ruler_scale.minor_step_frames < ruler_scale.major_step_frames {
            let mut f =
                (start_frame / ruler_scale.minor_step_frames) * ruler_scale.minor_step_frames;
            while f <= end_frame {
                if f % ruler_scale.major_step_frames != 0 {
                    let x = rect.left() + track_label_w + f as f32 * self.pixels_per_frame;
                    if x >= rect.left() + track_label_w && x <= rect.right() {
                        painter.line_segment(
                            [
                                Pos2::new(
                                    x,
                                    rect.bottom() - tokens::timeline_ruler_minor_tick_height(),
                                ),
                                Pos2::new(x, rect.bottom()),
                            ],
                            Stroke::new(1.0, palette::border_subtle().gamma_multiply(0.75)),
                        );
                    }
                }
                f += ruler_scale.minor_step_frames;
            }
        }

        // 主刻度 + 标签（按缩放自动切换帧/秒/分钟）
        let mut f = (start_frame / ruler_scale.major_step_frames) * ruler_scale.major_step_frames;
        while f <= end_frame {
            let x = rect.left() + track_label_w + f as f32 * self.pixels_per_frame;
            if x >= rect.left() + track_label_w && x <= rect.right() {
                painter.line_segment(
                    [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                    Stroke::new(1.0, palette::border_subtle()),
                );
                painter.text(
                    Pos2::new(
                        x + tokens::timeline_ruler_label_inset_x(),
                        rect.top() + tokens::timeline_ruler_label_inset_y(),
                    ),
                    egui::Align2::LEFT_TOP,
                    format_ruler_label(f, fps, display_format, ruler_scale.granularity),
                    typography::mono_small(),
                    palette::text_muted(),
                );
            }
            f += ruler_scale.major_step_frames;
        }

        // 播放头
        let playhead_x =
            rect.left() + track_label_w + state.current_frame() as f32 * self.pixels_per_frame;
        painter.line_segment(
            [
                Pos2::new(playhead_x, rect.top()),
                Pos2::new(playhead_x, rect.bottom()),
            ],
            Stroke::new(
                tokens::timeline_playhead_stroke_width(),
                palette::timeline_playhead(),
            ),
        );
        painter.circle_filled(
            Pos2::new(playhead_x, rect.top() + 6.0),
            4.0,
            palette::timeline_playhead(),
        );

        // 点击或拖拽标尺跳转（scrub）
        if resp.clicked() || resp.dragged() || resp.drag_stopped() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let clicked_frame =
                    ((pos.x - rect.left() - track_label_w) / self.pixels_per_frame) as i64;
                return Some(clicked_frame.max(0));
            }
        }
        None
    }

    // ─── 轨道列表 ─────────────────────────────
    fn draw_tracks(&mut self, ui: &mut Ui, state: &mut AppState) -> bool {
        let seq = match &state.sequence {
            Some(s) => s,
            None => return false,
        };

        let original_row_spacing = ui.spacing().item_spacing.y;
        ui.spacing_mut().item_spacing.y = 0.0;

        let video_tracks = seq.video_tracks.clone();
        let audio_tracks = seq.audio_tracks.clone();
        let mut dropped = false;
        let mut first_track_top: Option<f32> = None;
        let mut last_track_bottom: Option<f32> = None;
        let mut content_left: Option<f32> = None;
        let mut visible_clips: Vec<ClipVisual> = Vec::new();
        let mut track_rows: Vec<TrackRowVisual> = Vec::new();
        let audio_track_ids: Vec<TrackId> = audio_tracks.iter().map(|t| t.id).collect();
        let mut linked_audio_target_track_id: Option<TrackId> = None;
        let track_height = self.track_height;
        let mut visual_row_index = 0usize;

        for track_index in (0..video_tracks.len()).rev() {
            let track = &video_tracks[track_index];
            dropped |= self.draw_track_row(
                ui,
                state,
                track,
                track_index,
                visual_row_index,
                palette::timeline_clip_video(),
                true,
                track_index == video_tracks.len() - 1,
                track_index == 0 && audio_tracks.is_empty(),
                &mut first_track_top,
                &mut last_track_bottom,
                &mut content_left,
                &mut visible_clips,
                &mut track_rows,
                &audio_track_ids,
                &mut linked_audio_target_track_id,
            );
            visual_row_index += 1;
        }
        for (track_index, track) in audio_tracks.iter().enumerate() {
            dropped |= self.draw_track_row(
                ui,
                state,
                track,
                track_index,
                visual_row_index,
                palette::timeline_clip_audio(),
                false,
                video_tracks.is_empty() && track_index == 0,
                track_index + 1 == audio_tracks.len(),
                &mut first_track_top,
                &mut last_track_bottom,
                &mut content_left,
                &mut visible_clips,
                &mut track_rows,
                &audio_track_ids,
                &mut linked_audio_target_track_id,
            );
            visual_row_index += 1;
        }

        self.track_drag_target = match (self.track_drag, ui.input(|i| i.pointer.interact_pos())) {
            (Some(track_drag), Some(pointer)) => {
                choose_track_drag_target(pointer, &track_rows, track_drag)
            }
            _ => None,
        };

        if let (Some(top), Some(bottom), Some(left)) =
            (first_track_top, last_track_bottom, content_left)
        {
            self.track_area_bounds = Some(Rect::from_min_max(
                Pos2::new(left, top),
                Pos2::new(ui.max_rect().right(), bottom),
            ));

            let playhead_x = left + state.current_frame() as f32 * self.pixels_per_frame;
            ui.painter().line_segment(
                [Pos2::new(playhead_x, top), Pos2::new(playhead_x, bottom)],
                Stroke::new(
                    tokens::timeline_playhead_secondary_stroke_width(),
                    palette::timeline_playhead(),
                ),
            );

            if let Some(snap_frame) = self.active_snap_guide_frame {
                let snap_x = left + snap_frame as f32 * self.pixels_per_frame;
                ui.painter().line_segment(
                    [Pos2::new(snap_x, top), Pos2::new(snap_x, bottom)],
                    Stroke::new(1.4, palette::interaction_highlight()),
                );
            }

            if let Some(insert_frame) = self.active_insert_guide_frame {
                let insert_x = left + insert_frame as f32 * self.pixels_per_frame;
                ui.painter().line_segment(
                    [Pos2::new(insert_x, top), Pos2::new(insert_x, bottom)],
                    Stroke::new(
                        tokens::timeline_insert_guide_width(),
                        palette::status_warning(),
                    ),
                );
            }

            if let Some(track_target) = self.track_drag_target {
                if let Some(target_row) = track_rows.iter().find(|row| {
                    row.is_video_track == track_target.is_video_track
                        && row.track_index == track_target.target_index
                }) {
                    ui.painter().rect_stroke(
                        target_row.rect.shrink(1.0),
                        corner_radius(2.0),
                        Stroke::new(2.0, palette::interaction_highlight()),
                        egui::StrokeKind::Inside,
                    );
                }
            }

            if let (Some(pos), Some(dragging)) = (
                ui.input(|i| i.pointer.interact_pos()),
                state.dragging_asset(),
            ) {
                if (((dragging.kind == mondrian_assets::AssetKind::Video
                    || dragging.kind == mondrian_assets::AssetKind::AdjustmentLayer)
                    && pos.y <= top + track_height * video_tracks.len() as f32)
                    || (dragging.kind == mondrian_assets::AssetKind::Audio
                        && pos.y > top + track_height * video_tracks.len() as f32))
                    && pos.x >= left
                    && pos.y >= top
                    && pos.y <= bottom
                {
                    ui.painter().line_segment(
                        [Pos2::new(pos.x, top), Pos2::new(pos.x, bottom)],
                        Stroke::new(1.0, palette::interaction_highlight()),
                    );
                }
            }

            self.handle_marquee(ui, state, &visible_clips);
        }

        if first_track_top.is_none() || last_track_bottom.is_none() || content_left.is_none() {
            self.track_area_bounds = None;
            self.clear_marquee();
        }

        ui.spacing_mut().item_spacing.y = original_row_spacing;

        dropped
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_track_row(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        track: &mondrian_timeline::track::Track,
        track_index: usize,
        visual_row_index: usize,
        clip_color: Color32,
        is_video_track: bool,
        round_top: bool,
        round_bottom: bool,
        first_track_top: &mut Option<f32>,
        last_track_bottom: &mut Option<f32>,
        content_left: &mut Option<f32>,
        visible_clips: &mut Vec<ClipVisual>,
        track_rows: &mut Vec<TrackRowVisual>,
        audio_track_ids: &[TrackId],
        linked_audio_target_track_id: &mut Option<TrackId>,
    ) -> bool {
        let available_w = ui.available_width();
        let track_height = self.track_height;
        let track_label_w = tokens::timeline_track_label_width();
        let clip_top_inset = tokens::timeline_clip_top_inset();
        let clip_bottom_inset = tokens::timeline_clip_bottom_inset();
        let (rect, resp) = ui.allocate_exact_size(
            Vec2::new(available_w, track_height),
            Sense::click_and_drag(),
        );

        if first_track_top.is_none() {
            *first_track_top = Some(rect.top());
        }
        *last_track_bottom = Some(rect.bottom());
        if content_left.is_none() {
            *content_left = Some(rect.left() + track_label_w);
        }

        let painter = ui.painter_at(rect);
        let lane_fill = if visual_row_index % 2 == 0 {
            palette::bg_surface()
        } else {
            palette::bg_surface_raised()
        };

        let label_rect = Rect::from_min_size(rect.min, Vec2::new(track_label_w, track_height));
        let content_rect = Rect::from_min_max(
            Pos2::new(label_rect.right(), rect.top()),
            rect.right_bottom(),
        );
        painter.rect_filled(content_rect, 0.0, lane_fill);
        let label_fill_rect = label_rect;
        let label_fill = lane_fill;
        let label_rounding = egui::CornerRadius {
            nw: if round_top {
                tokens::section_rounding().round().clamp(0.0, 255.0) as u8
            } else {
                0
            },
            ne: 0,
            sw: if round_bottom {
                tokens::section_rounding().round().clamp(0.0, 255.0) as u8
            } else {
                0
            },
            se: 0,
        };
        painter.rect_filled(label_fill_rect, label_rounding, label_fill);
        painter.line_segment(
            [
                Pos2::new(rect.left(), rect.bottom()),
                Pos2::new(rect.right(), rect.bottom()),
            ],
            Stroke::new(1.0, palette::border_subtle().gamma_multiply(0.8)),
        );
        painter.line_segment(
            [
                Pos2::new(label_rect.right(), rect.top()),
                Pos2::new(label_rect.right(), rect.bottom()),
            ],
            Stroke::new(1.0, palette::border_subtle().gamma_multiply(0.8)),
        );

        let icon_size = Vec2::new(
            tokens::timeline_track_icon_size(),
            tokens::timeline_track_icon_size(),
        );
        let lock_rect = Rect::from_center_size(
            Pos2::new(
                label_rect.right() - tokens::timeline_track_lock_offset_x(),
                label_rect.center().y,
            ),
            icon_size,
        );
        let mode_rect = Rect::from_center_size(
            Pos2::new(
                label_rect.right() - tokens::timeline_track_mode_offset_x(),
                label_rect.center().y,
            ),
            icon_size,
        );
        let header_drag_rect = Rect::from_min_max(
            label_rect.min,
            Pos2::new(mode_rect.left() - 4.0, label_rect.max.y),
        );
        track_rows.push(TrackRowVisual {
            track_id: track.id,
            is_video_track,
            track_index,
            rect,
        });

        let track_drag_resp = ui.interact(
            header_drag_rect,
            ui.make_persistent_id(("track_reorder", track.id, is_video_track)),
            Sense::click_and_drag(),
        );
        if track_drag_resp.drag_started()
            && self.clip_drag.is_none()
            && state.dragging_asset().is_none()
        {
            self.track_drag = Some(TrackDragState {
                track_id: track.id,
                is_video_track,
                source_index: track_index,
            });
            self.track_drag_target =
                Some(TrackDragTarget { is_video_track, target_index: track_index });
            self.clear_marquee();
        }
        let lock_resp = ui.interact(
            lock_rect,
            ui.make_persistent_id(("track_lock", track.id, is_video_track)),
            Sense::click(),
        );
        if lock_resp.clicked() {
            let _ = state.set_track_locked(track.id, is_video_track, !track.is_locked);
        }
        let mode_resp = ui.interact(
            mode_rect,
            ui.make_persistent_id(("track_mode", track.id, is_video_track)),
            Sense::click(),
        );
        if mode_resp.clicked() {
            if is_video_track {
                let _ = state.set_track_visible(track.id, true, !track.is_visible);
            } else {
                let _ = state.set_track_muted(track.id, false, !track.is_muted);
            }
        }

        painter.text(
            Pos2::new(
                label_rect.left() + tokens::timeline_track_label_text_inset_x(),
                label_rect.center().y,
            ),
            egui::Align2::LEFT_CENTER,
            &track.name,
            typography::body_small(),
            palette::text_primary(),
        );
        let mode_icon = if is_video_track {
            if track.is_visible {
                theme::UiIcon::Eye
            } else {
                theme::UiIcon::EyeOff
            }
        } else if track.is_muted {
            theme::UiIcon::Mute
        } else {
            theme::UiIcon::Speaker
        };
        let lock_icon = if track.is_locked {
            theme::UiIcon::Lock
        } else {
            theme::UiIcon::Unlock
        };
        theme::draw_icon(ui.painter(), mode_rect, mode_icon, palette::text_primary());
        theme::draw_icon(ui.painter(), lock_rect, lock_icon, palette::text_primary());

        if self
            .track_drag
            .map(|drag| drag.track_id == track.id && drag.is_video_track == is_video_track)
            .unwrap_or(false)
        {
            painter.rect_stroke(
                rect.shrink(1.0),
                corner_radius(2.0),
                Stroke::new(1.5, palette::status_warning()),
                egui::StrokeKind::Inside,
            );
        }

        let mut clip_outlines: Vec<(Rect, bool)> = Vec::new();

        for clip in &track.clips {
            let clip_x =
                rect.left() + track_label_w + clip.position.frame as f32 * self.pixels_per_frame;
            let clip_w = clip.duration.frame as f32 * self.pixels_per_frame;
            if clip_x + clip_w < rect.left() + track_label_w || clip_x > rect.right() {
                continue;
            }

            let clip_rect = Rect::from_min_size(
                Pos2::new(
                    clip_x.max(rect.left() + track_label_w),
                    rect.top() + clip_top_inset,
                ),
                Vec2::new(clip_w, track_height - clip_bottom_inset),
            );
            let clip_draw_rect = clip_rect.intersect(Rect::from_min_max(
                Pos2::new(rect.left() + track_label_w, rect.top() + clip_top_inset),
                Pos2::new(rect.right() - 1.0, rect.bottom() - 1.0),
            ));
            let selection = ClipSelection {
                track_id: track.id,
                is_video_track,
                clip_id: clip.id,
            };
            visible_clips.push(ClipVisual { selection, rect: clip_draw_rect });

            let base_clip_color = if clip.is_adjustment_layer() {
                palette::accent_secondary().gamma_multiply(0.82)
            } else {
                clip_color
            };
            let clip_fill = if clip.is_disabled {
                base_clip_color.gamma_multiply(0.35)
            } else {
                base_clip_color
            };
            painter.rect_filled(clip_draw_rect, tokens::timeline_clip_radius(), clip_fill);
            clip_outlines.push((clip_draw_rect, self.selected_clips.contains(&selection)));

            if clip_w > tokens::timeline_clip_label_min_width() {
                let label_rect = Rect::from_min_max(
                    clip_draw_rect.left_top()
                        + Vec2::new(tokens::timeline_clip_label_padding_x(), 0.0),
                    clip_draw_rect.right_bottom()
                        - Vec2::new(tokens::timeline_clip_label_padding_x(), 0.0),
                );
                draw_single_line_ellipsis(
                    &painter,
                    label_rect,
                    clip.label.as_deref().unwrap_or(if clip.is_adjustment_layer() {
                        "调整图层"
                    } else {
                        "clip"
                    }),
                    typography::body_small(),
                    palette::text_primary(),
                );
            }

            if clip.is_disabled {
                painter.text(
                    clip_draw_rect.right_center()
                        - Vec2::new(tokens::timeline_clip_label_padding_x(), 0.0),
                    egui::Align2::RIGHT_CENTER,
                    "禁用",
                    typography::body_small(),
                    palette::text_muted(),
                );
            }

            if state.dragging_asset().is_none() && self.track_drag.is_none() {
                let clip_resp = ui.interact(
                    clip_draw_rect,
                    ui.make_persistent_id(("timeline_clip_drag", track.id, clip.id)),
                    Sense::click_and_drag(),
                );

                if self.active_tool == TimelineTool::Blade {
                    if clip_resp.clicked() {
                        let split_frame = clip_resp
                            .interact_pointer_pos()
                            .map(|pointer| {
                                ((pointer.x - rect.left() - track_label_w) / self.pixels_per_frame)
                                    .round() as i64
                            })
                            .unwrap_or(state.current_frame())
                            .max(0);

                        match state.split_clip_at_frame(
                            track.id,
                            is_video_track,
                            clip.id,
                            split_frame,
                        ) {
                            Ok(true) => {}
                            Ok(false) => {}
                            Err(_) => {}
                        }
                    }
                    continue;
                }

                if clip_resp.clicked() {
                    let shift_pressed = ui.input(|i| i.modifiers.shift);
                    if shift_pressed {
                        if !self.selected_clips.insert(selection) {
                            self.selected_clips.remove(&selection);
                        }
                    } else {
                        self.selected_clips.clear();
                        self.selected_clips.insert(selection);
                    }
                    state.clear_animation_selection();
                }

                if clip_resp.drag_started() {
                    let shift_pressed = ui.input(|i| i.modifiers.shift);
                    let already_selected = self.selected_clips.contains(&selection);
                    if !shift_pressed && !already_selected {
                        self.selected_clips.clear();
                    }
                    self.selected_clips.insert(selection);
                    state.clear_animation_selection();
                    self.clip_drag_anchors = self.build_clip_drag_anchors(state);
                    self.clip_drag_before_sequence = state.sequence.clone();

                    if let Some(pointer) = clip_resp.interact_pointer_pos() {
                        let pointer_frame = ((pointer.x - rect.left() - track_label_w)
                            / self.pixels_per_frame)
                            .round() as i64;
                        let anchor_start = self
                            .clip_drag_anchors
                            .iter()
                            .find(|anchor| anchor.clip_id == clip.id)
                            .map(|anchor| anchor.start_frame)
                            .unwrap_or(clip.position.frame);
                        self.clip_drag = Some(ClipDragState {
                            clip_id: clip.id,
                            is_video_track,
                            pointer_offset_frames: pointer_frame - anchor_start,
                        });
                    }
                }

                clip_resp.context_menu(|ui| {
                    if !self.selected_clips.contains(&selection) {
                        self.selected_clips.clear();
                        self.selected_clips.insert(selection);
                    }
                    if ui.button("删除片段").clicked() {
                        self.delete_selected_clips(state, false);
                        ui.close();
                    }
                    if ui.button("波纹删除片段").clicked() {
                        self.delete_selected_clips(state, true);
                        ui.close();
                    }
                    if ui.button("修剪入点到播放头").clicked() {
                        self.trim_selected_clips_to_playhead(state, TrimEdge::In);
                        ui.close();
                    }
                    if ui.button("修剪出点到播放头").clicked() {
                        self.trim_selected_clips_to_playhead(state, TrimEdge::Out);
                        ui.close();
                    }
                    if ui.button("滚动切点到播放头").clicked() {
                        self.roll_selected_cut_to_playhead(state);
                        ui.close();
                    }
                    if ui.button("预合成为序列").clicked() {
                        self.precompose_selected_clips(state);
                        ui.close();
                    }
                    if let Some(nested_sequence_id) =
                        selected_nested_sequence_id(state, self.selected_clip_ref())
                    {
                        if ui.button("打开嵌套序列").clicked() {
                            match state.open_nested_sequence(nested_sequence_id) {
                                Ok(()) => {
                                    self.selected_clips.clear();
                                }
                                Err(err) => {
                                    state.set_status_hint(format!("打开嵌套序列失败：{err}"), true);
                                }
                            }
                            ui.close();
                        }
                    }
                    let all_disabled = self.selected_clips_all_disabled(state);
                    let toggle_label = if all_disabled {
                        "启用片段"
                    } else {
                        "禁用片段"
                    };
                    if ui.button(toggle_label).clicked() {
                        self.set_selected_clips_disabled(state, !all_disabled);
                        ui.close();
                    }
                    if ui.button("清除选择").clicked() {
                        self.selected_clips.clear();
                        ui.close();
                    }
                });

                if let Some(drag) = self.clip_drag {
                    if drag.clip_id == clip.id && drag.is_video_track == is_video_track {
                        if let Some(pointer) = ui.input(|i| i.pointer.interact_pos()) {
                            let pointer_frame = ((pointer.x - rect.left() - track_label_w)
                                / self.pixels_per_frame)
                                .round() as i64;
                            let raw_target = (pointer_frame - drag.pointer_offset_frames).max(0);
                            let target_frame = self.resolve_snap_target_frame(
                                state,
                                raw_target,
                                Some(drag.clip_id),
                            );
                            let overlap_mode = Self::current_overlap_mode(ui);
                            if overlap_mode == ClipOverlapMode::Insert {
                                self.active_insert_guide_frame = Some(target_frame);
                            }

                            let anchors = if self.clip_drag_anchors.is_empty() {
                                vec![ClipDragAnchor {
                                    clip_id: drag.clip_id,
                                    start_frame: clip.position.frame,
                                }]
                            } else {
                                self.clip_drag_anchors.clone()
                            };
                            let Some(drag_anchor) =
                                anchors.iter().find(|anchor| anchor.clip_id == drag.clip_id)
                            else {
                                continue;
                            };

                            let min_start = anchors
                                .iter()
                                .map(|anchor| anchor.start_frame)
                                .min()
                                .unwrap_or(drag_anchor.start_frame)
                                .max(0);
                            let clamped_delta =
                                (target_frame - drag_anchor.start_frame).max(-min_start);
                            let anchor_pairs: Vec<(ClipId, i64)> = anchors
                                .iter()
                                .map(|anchor| (anchor.clip_id, anchor.start_frame))
                                .collect();

                            if let Err(err) = state.move_clip_group_by_delta_with_mode(
                                &anchor_pairs,
                                clamped_delta,
                                overlap_mode,
                            ) {
                                let _ = err;
                            } else {
                                self.clip_drag_moved = true;
                            }
                        }
                    }
                }
            }
        }

        for (clip_rect, selected) in clip_outlines {
            painter.rect_stroke(
                clip_rect.shrink(0.5),
                corner_radius(tokens::timeline_clip_radius()),
                Stroke::new(
                    1.0,
                    if is_video_track {
                        palette::interaction_highlight().gamma_multiply(0.55)
                    } else {
                        palette::accent_audio().gamma_multiply(0.60)
                    },
                ),
                egui::StrokeKind::Inside,
            );
            if selected {
                painter.rect_stroke(
                    clip_rect.shrink(0.5),
                    corner_radius(tokens::timeline_clip_radius()),
                    Stroke::new(
                        tokens::timeline_selection_stroke_width(),
                        palette::interaction_highlight(),
                    ),
                    egui::StrokeKind::Inside,
                );
            }
        }

        let mut dropped_here = false;
        let dragging_asset = if self.track_drag.is_none() {
            state.dragging_asset().cloned()
        } else {
            None
        };
        let can_drop_here = dragging_asset
            .as_ref()
            .map(|asset| {
                (is_video_track
                    && matches!(
                        asset.kind,
                        mondrian_assets::AssetKind::Video
                            | mondrian_assets::AssetKind::AdjustmentLayer
                    ))
                    || (!is_video_track && asset.kind == mondrian_assets::AssetKind::Audio)
            })
            .unwrap_or(false);
        let pointer_pos = ui.input(|i| i.pointer.interact_pos());
        let pointer_in_row = pointer_pos.map(|p| rect.contains(p)).unwrap_or(false);
        let overlap_mode = Self::current_overlap_mode(ui);

        if is_video_track && can_drop_here && pointer_in_row {
            if let Some(asset) = dragging_asset.as_ref() {
                if asset.kind == mondrian_assets::AssetKind::Video && asset.has_linked_audio {
                    *linked_audio_target_track_id = audio_track_ids.get(track_index).copied();
                }
            }
        }

        let linked_audio_row_highlight = !is_video_track
            && dragging_asset
                .as_ref()
                .map(|asset| {
                    asset.kind == mondrian_assets::AssetKind::Video && asset.has_linked_audio
                })
                .unwrap_or(false)
            && linked_audio_target_track_id.map(|id| id == track.id).unwrap_or(false);

        if linked_audio_row_highlight {
            painter.rect_stroke(
                rect.shrink(1.0),
                corner_radius(2.0),
                Stroke::new(1.5, palette::interaction_highlight()),
                egui::StrokeKind::Inside,
            );
        }

        if can_drop_here && pointer_in_row {
            painter.rect_stroke(
                rect.shrink(1.0),
                corner_radius(2.0),
                Stroke::new(1.5, palette::interaction_highlight()),
                egui::StrokeKind::Inside,
            );

            if let (Some(dragging), Some(pos), Some(seq)) = (
                dragging_asset.as_ref(),
                pointer_pos,
                state.sequence.as_ref(),
            ) {
                let raw_ghost_frame =
                    (((pos.x - rect.left() - track_label_w) / self.pixels_per_frame) as i64).max(0);
                let ghost_frame = self.resolve_snap_target_frame(state, raw_ghost_frame, None);
                if overlap_mode == ClipOverlapMode::Insert {
                    self.active_insert_guide_frame = Some(ghost_frame);
                }
                let ghost_frames = ((dragging.duration.as_secs_f64()
                    * seq.settings.frame_rate.to_f64())
                .ceil() as i64)
                    .max(1);
                let ghost_x =
                    rect.left() + track_label_w + ghost_frame as f32 * self.pixels_per_frame;
                let ghost_w = (ghost_frames as f32 * self.pixels_per_frame)
                    .max(tokens::timeline_clip_ghost_min_width());
                let ghost_rect = Rect::from_min_size(
                    Pos2::new(
                        ghost_x.max(rect.left() + track_label_w),
                        rect.top() + tokens::timeline_clip_ghost_padding_y(),
                    ),
                    Vec2::new(
                        ghost_w,
                        track_height - tokens::timeline_clip_ghost_padding_y() * 2.0,
                    ),
                );
                painter.rect_filled(
                    ghost_rect,
                    tokens::timeline_clip_radius(),
                    clip_color.gamma_multiply(0.35),
                );
                painter.rect_stroke(
                    ghost_rect,
                    corner_radius(tokens::timeline_clip_radius()),
                    Stroke::new(
                        tokens::timeline_linked_audio_highlight_width(),
                        palette::interaction_highlight(),
                    ),
                    egui::StrokeKind::Inside,
                );
                painter.text(
                    ghost_rect.left_center()
                        + Vec2::new(tokens::timeline_clip_ghost_padding_x(), 0.0),
                    egui::Align2::LEFT_CENTER,
                    format!("{} (预放置)", dragging.name),
                    typography::body_small(),
                    palette::text_primary(),
                );

                if is_video_track && dragging.has_linked_audio {
                    if let Some(audio_track_id) = audio_track_ids.get(track_index).copied() {
                        *linked_audio_target_track_id = Some(audio_track_id);
                    }
                }
            }

            if ui.input(|i| i.pointer.any_released()) {
                if let Some(pos) = pointer_pos {
                    let raw_drop_frame =
                        ((pos.x - rect.left() - track_label_w) / self.pixels_per_frame) as i64;
                    let raw_drop_frame = raw_drop_frame.max(0);
                    let drop_frame = self.resolve_snap_target_frame(state, raw_drop_frame, None);
                    if overlap_mode == ClipOverlapMode::Insert {
                        self.active_insert_guide_frame = Some(drop_frame);
                    }
                    let drop_result = if is_video_track {
                        state.drop_dragging_asset_to_video_track_with_mode(
                            track.id,
                            drop_frame,
                            overlap_mode,
                        )
                    } else {
                        state.drop_dragging_asset_to_audio_track_with_mode(
                            track.id,
                            drop_frame,
                            overlap_mode,
                        )
                    };
                    match drop_result {
                        Ok(_) => {
                            dropped_here = true;
                        }
                        Err(err) => {
                            state.set_status_hint(format!("放置素材失败：{err}"), true);
                        }
                    }
                }
            }
        } else if linked_audio_row_highlight {
            if let (Some(dragging), Some(pos), Some(seq)) = (
                dragging_asset.as_ref(),
                pointer_pos,
                state.sequence.as_ref(),
            ) {
                let raw_ghost_frame =
                    (((pos.x - rect.left() - track_label_w) / self.pixels_per_frame) as i64).max(0);
                let ghost_frame = self.resolve_snap_target_frame(state, raw_ghost_frame, None);
                if overlap_mode == ClipOverlapMode::Insert {
                    self.active_insert_guide_frame = Some(ghost_frame);
                }
                let ghost_frames = ((dragging.duration.as_secs_f64()
                    * seq.settings.frame_rate.to_f64())
                .ceil() as i64)
                    .max(1);
                let ghost_x =
                    rect.left() + track_label_w + ghost_frame as f32 * self.pixels_per_frame;
                let ghost_w = (ghost_frames as f32 * self.pixels_per_frame).max(8.0);
                let ghost_rect = Rect::from_min_size(
                    Pos2::new(
                        ghost_x.max(rect.left() + track_label_w),
                        rect.top() + tokens::timeline_clip_ghost_padding_y(),
                    ),
                    Vec2::new(
                        ghost_w,
                        track_height - tokens::timeline_clip_ghost_padding_y() * 2.0,
                    ),
                );
                painter.rect_filled(
                    ghost_rect,
                    tokens::timeline_clip_radius(),
                    palette::timeline_clip_audio().gamma_multiply(0.35),
                );
                painter.rect_stroke(
                    ghost_rect,
                    corner_radius(tokens::timeline_clip_radius()),
                    Stroke::new(
                        tokens::timeline_linked_audio_highlight_width(),
                        palette::interaction_highlight(),
                    ),
                    egui::StrokeKind::Inside,
                );
            }
        }

        resp.context_menu(|ui| {
            if is_video_track {
                ui.menu_button("新建图层", |ui| {
                    if ui.button("调整图层").clicked() {
                        let start_frame = state.current_frame().max(0);
                        let overlap_mode = Self::current_overlap_mode(ui);
                        if let Err(err) = state.create_adjustment_layer_on_video_track(
                            track.id,
                            Some(start_frame),
                            overlap_mode,
                        ) {
                            state.set_status_hint(format!("创建调整图层失败：{err}"), true);
                        }
                        ui.close();
                    }
                });
                ui.separator();
            }

            if ui.button("新增视频轨道").clicked() {
                let _ = state.add_video_track();
                ui.close();
            }

            if ui.button("新增音频轨道").clicked() {
                let _ = state.add_audio_track();
                ui.close();
            }

            ui.separator();
            if ui.button("删除已选片段").clicked() {
                self.delete_selected_clips(state, false);
                ui.close();
            }
            if ui.button("波纹删除已选片段").clicked() {
                self.delete_selected_clips(state, true);
                ui.close();
            }
            if ui.button("修剪已选入点到播放头").clicked() {
                self.trim_selected_clips_to_playhead(state, TrimEdge::In);
                ui.close();
            }
            if ui.button("修剪已选出点到播放头").clicked() {
                self.trim_selected_clips_to_playhead(state, TrimEdge::Out);
                ui.close();
            }
            if ui.button("滚动已选切点到播放头").clicked() {
                self.roll_selected_cut_to_playhead(state);
                ui.close();
            }
            let all_disabled = self.selected_clips_all_disabled(state);
            let toggle_label = if all_disabled {
                "启用已选片段"
            } else {
                "禁用已选片段"
            };
            if ui.button(toggle_label).clicked() {
                self.set_selected_clips_disabled(state, !all_disabled);
                ui.close();
            }

            ui.separator();
            let remove_label = if is_video_track {
                "删除当前视频轨"
            } else {
                "删除当前音频轨"
            };
            if ui.button(remove_label).clicked() {
                let _ = state.remove_track(track.id, is_video_track);
                ui.close();
            }
        });

        dropped_here
    }

    fn handle_marquee(&mut self, ui: &mut Ui, state: &AppState, visible_clips: &[ClipVisual]) {
        if state.dragging_asset().is_some() || self.clip_drag.is_some() || self.track_drag.is_some()
        {
            self.clear_marquee();
            return;
        }

        let Some(bounds) = self.track_area_bounds else {
            self.clear_marquee();
            return;
        };

        let pointer_pos = ui.input(|i| i.pointer.interact_pos());
        let primary_pressed = ui.input(|i| i.pointer.primary_pressed());
        let primary_down = ui.input(|i| i.pointer.primary_down());
        let primary_released = ui.input(|i| i.pointer.primary_released());

        if primary_pressed {
            if let Some(pos) = pointer_pos {
                let in_bounds = bounds.contains(pos) && pos.x >= bounds.left();
                let on_clip = visible_clips.iter().any(|visual| visual.rect.contains(pos));
                if in_bounds && !on_clip {
                    self.marquee_anchor = Some(pos);
                    self.marquee_current = Some(pos);
                    self.marquee_additive = ui.input(|i| i.modifiers.shift);
                    if !self.marquee_additive {
                        self.selected_clips.clear();
                    }
                }
            }
        }

        if primary_down && self.marquee_anchor.is_some() {
            if let Some(pos) = pointer_pos {
                self.marquee_current = Some(pos);
            }
        }

        if let (Some(anchor), Some(current)) = (self.marquee_anchor, self.marquee_current) {
            let rect = Rect::from_two_pos(anchor, current).intersect(bounds);
            if rect.width() > 2.0 && rect.height() > 2.0 {
                ui.painter().rect_filled(
                    rect,
                    2.0,
                    palette::interaction_highlight().gamma_multiply(0.16),
                );
                ui.painter().rect_stroke(
                    rect,
                    corner_radius(2.0),
                    Stroke::new(1.2, palette::interaction_highlight()),
                    egui::StrokeKind::Inside,
                );
            }
        }

        if primary_released {
            if let (Some(anchor), Some(current)) = (self.marquee_anchor, self.marquee_current) {
                let rect = Rect::from_two_pos(anchor, current).intersect(bounds);
                if rect.width() > 2.0 && rect.height() > 2.0 {
                    let picks = visible_clips
                        .iter()
                        .filter(|visual| visual.rect.intersects(rect))
                        .map(|visual| visual.selection)
                        .collect::<Vec<_>>();

                    if self.marquee_additive {
                        for selection in picks {
                            self.selected_clips.insert(selection);
                        }
                    } else {
                        self.selected_clips = picks.into_iter().collect();
                    }
                }
            }
            self.clear_marquee();
        }
    }

    fn clear_marquee(&mut self) {
        self.marquee_anchor = None;
        self.marquee_current = None;
        self.marquee_additive = false;
    }

    fn delete_selected_clips(&mut self, state: &mut AppState, ripple: bool) {
        if self.selected_clips.is_empty() {
            return;
        }

        let selections: Vec<(TrackId, bool, ClipId)> = self
            .selected_clips
            .iter()
            .map(|selection| {
                (
                    selection.track_id,
                    selection.is_video_track,
                    selection.clip_id,
                )
            })
            .collect();

        if state.remove_clips_bulk(&selections, ripple).is_ok() {
            self.selected_clips.clear();
        }
    }

    fn precompose_selected_clips(&mut self, state: &mut AppState) {
        if self.selected_clips.is_empty() {
            return;
        }

        let selections: Vec<(TrackId, bool, ClipId)> = self
            .selected_clips
            .iter()
            .map(|selection| {
                (
                    selection.track_id,
                    selection.is_video_track,
                    selection.clip_id,
                )
            })
            .collect();
        let name = format!("预合成 {}", state.sequences.len() + 1);
        match state.precompose_clips_as_sequence(&selections, &name) {
            Ok(nested_clip_id) => {
                self.selected_clips.clear();
                if let Some(seq) = state.sequence.as_ref() {
                    if let Some((track_id, is_video_track, _)) =
                        find_clip_track_ref(seq, nested_clip_id)
                    {
                        self.selected_clips.insert(ClipSelection {
                            track_id,
                            is_video_track,
                            clip_id: nested_clip_id,
                        });
                    }
                }
                state.set_status_hint(format!("已创建嵌套序列：{name}"), false);
            }
            Err(err) => {
                state.set_status_hint(format!("预合成失败：{err}"), true);
            }
        }
    }

    fn trim_selected_clips_to_playhead(&mut self, state: &mut AppState, edge: TrimEdge) {
        if self.selected_clips.is_empty() {
            return;
        }

        let clip_ids: Vec<ClipId> = self.selected_clips.iter().map(|sel| sel.clip_id).collect();
        let target_frame = match edge {
            TrimEdge::In => state.current_frame(),
            // 出点为右开区间，播放头所在帧应被保留，所以 +1
            TrimEdge::Out => state.current_frame().saturating_add(1),
        };

        if let Err(err) = state.trim_clips_bulk_to_frame(&clip_ids, edge, target_frame) {
            state.set_status_hint(format!("修剪片段失败：{err}"), true);
        }
    }

    fn roll_selected_cut_to_playhead(&mut self, state: &mut AppState) {
        if self.selected_clips.len() != 1 {
            return;
        }

        let clip_id = match self.selected_clips.iter().next() {
            Some(selection) => selection.clip_id,
            None => return,
        };

        match state.roll_cut_to_frame(clip_id, state.current_frame()) {
            Ok(true) => {}
            Ok(false) => {
                state.set_status_hint("未找到可滚动切点，或播放头不在可滚动范围", true);
            }
            Err(err) => {
                state.set_status_hint(format!("滚动修剪失败：{err}"), true);
            }
        }
    }

    fn slip_selected_clips_by_frames(&mut self, state: &mut AppState, delta_frames: i64) {
        if self.selected_clips.is_empty() || delta_frames == 0 {
            return;
        }

        let clip_ids: Vec<ClipId> = self.selected_clips.iter().map(|sel| sel.clip_id).collect();
        if let Err(err) = state.slip_clips_bulk_by_frames(&clip_ids, delta_frames) {
            state.set_status_hint(format!("滑移片段失败：{err}"), true);
        }
    }

    fn slide_selected_clips_by_frames(&mut self, state: &mut AppState, delta_frames: i64) {
        if self.selected_clips.is_empty() || delta_frames == 0 {
            return;
        }

        let clip_ids: Vec<ClipId> = self.selected_clips.iter().map(|sel| sel.clip_id).collect();
        if let Err(err) = state.slide_clips_bulk_by_frames(&clip_ids, delta_frames) {
            state.set_status_hint(format!("滑动片段失败：{err}"), true);
        }
    }

    fn resolve_snap_target_frame(
        &mut self,
        state: &AppState,
        raw_target_frame: i64,
        exclude_clip_id: Option<ClipId>,
    ) -> i64 {
        let raw_target_frame = raw_target_frame.max(0);
        if !self.snap_enabled || self.pixels_per_frame <= 0.0 {
            return raw_target_frame;
        }

        let Some(seq) = state.sequence.as_ref() else {
            return raw_target_frame;
        };

        let candidates = collect_snap_candidates(seq, state, exclude_clip_id);
        let decision = decide_snap_target(
            raw_target_frame,
            &candidates,
            self.pixels_per_frame,
            tokens::timeline_drag_snap_pixels(),
        );
        if decision.snapped {
            self.active_snap_guide_frame = Some(decision.frame);
        }
        decision.frame
    }

    fn build_clip_drag_anchors(&self, state: &AppState) -> Vec<ClipDragAnchor> {
        let Some(seq) = state.sequence.as_ref() else {
            return Vec::new();
        };

        let mut anchors = Vec::new();
        let mut queued: Vec<ClipId> = self.selected_clips.iter().map(|sel| sel.clip_id).collect();
        let mut seen = HashSet::<ClipId>::new();

        while let Some(clip_id) = queued.pop() {
            if !seen.insert(clip_id) {
                continue;
            }
            let Some(clip) = find_clip_in_sequence(seq, clip_id) else {
                continue;
            };
            anchors.push(ClipDragAnchor { clip_id, start_frame: clip.position.frame.max(0) });
            if let Some(linked_id) = clip.linked_clip {
                if !seen.contains(&linked_id) {
                    queued.push(linked_id);
                }
            }
        }

        anchors
    }

    fn current_overlap_mode(ui: &Ui) -> ClipOverlapMode {
        if ui.input(|i| i.modifiers.command) {
            ClipOverlapMode::Insert
        } else {
            ClipOverlapMode::Overwrite
        }
    }

    fn selected_clips_all_disabled(&self, state: &AppState) -> bool {
        let Some(seq) = state.sequence.as_ref() else {
            return false;
        };

        let mut has_any = false;
        for sel in &self.selected_clips {
            if let Some(disabled) = clip_disabled_state(seq, *sel) {
                has_any = true;
                if !disabled {
                    return false;
                }
            }
        }
        has_any
    }

    fn set_selected_clips_disabled(&mut self, state: &mut AppState, disabled: bool) {
        if self.selected_clips.is_empty() {
            return;
        }

        let selections: Vec<(TrackId, bool, ClipId)> = self
            .selected_clips
            .iter()
            .map(|s| (s.track_id, s.is_video_track, s.clip_id))
            .collect();

        if let Err(err) = state.set_clips_disabled_bulk(&selections, disabled) {
            state.set_status_hint(format!("更新片段状态失败：{err}"), true);
        }
    }
}

fn choose_track_drag_target(
    pointer: Pos2,
    rows: &[TrackRowVisual],
    drag: TrackDragState,
) -> Option<TrackDragTarget> {
    let mut same_type_rows = rows
        .iter()
        .filter(|row| row.is_video_track == drag.is_video_track)
        .collect::<Vec<_>>();
    if same_type_rows.is_empty() {
        return None;
    }

    let top = same_type_rows.iter().map(|row| row.rect.top()).fold(f32::INFINITY, f32::min);
    let bottom = same_type_rows
        .iter()
        .map(|row| row.rect.bottom())
        .fold(f32::NEG_INFINITY, f32::max);
    if pointer.y < top || pointer.y > bottom {
        return None;
    }

    same_type_rows.sort_by(|a, b| {
        let a_dist = (a.rect.center().y - pointer.y).abs();
        let b_dist = (b.rect.center().y - pointer.y).abs();
        a_dist
            .partial_cmp(&b_dist)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.track_index.cmp(&b.track_index))
            .then_with(|| a.track_id.to_string().cmp(&b.track_id.to_string()))
    });

    let target_row = same_type_rows[0];
    Some(TrackDragTarget {
        is_video_track: drag.is_video_track,
        target_index: target_row.track_index,
    })
}

fn clip_disabled_state(seq: &Sequence, sel: ClipSelection) -> Option<bool> {
    if sel.is_video_track {
        seq.video_tracks
            .iter()
            .find(|track| track.id == sel.track_id)
            .and_then(|track| track.clips.iter().find(|clip| clip.id == sel.clip_id))
            .map(|clip| clip.is_disabled)
    } else {
        seq.audio_tracks
            .iter()
            .find(|track| track.id == sel.track_id)
            .and_then(|track| track.clips.iter().find(|clip| clip.id == sel.clip_id))
            .map(|clip| clip.is_disabled)
    }
}

fn find_clip_track_ref(seq: &Sequence, clip_id: ClipId) -> Option<(TrackId, bool, usize)> {
    for (index, track) in seq.video_tracks.iter().enumerate() {
        if track.clips.iter().any(|clip| clip.id == clip_id) {
            return Some((track.id, true, index));
        }
    }
    for (index, track) in seq.audio_tracks.iter().enumerate() {
        if track.clips.iter().any(|clip| clip.id == clip_id) {
            return Some((track.id, false, index));
        }
    }
    None
}

fn selected_nested_sequence_id(
    state: &AppState,
    selection: Option<SelectedClipRef>,
) -> Option<mondrian_core::types::SequenceId> {
    let selection = selection?;
    let seq = state.sequence.as_ref()?;
    let clip = if selection.is_video_track {
        seq.video_tracks
            .iter()
            .find(|track| track.id == selection.track_id)
            .and_then(|track| track.clips.iter().find(|clip| clip.id == selection.clip_id))
    } else {
        seq.audio_tracks
            .iter()
            .find(|track| track.id == selection.track_id)
            .and_then(|track| track.clips.iter().find(|clip| clip.id == selection.clip_id))
    }?;
    if clip.is_nested_sequence() {
        clip.nested_sequence_id
    } else {
        None
    }
}

fn draw_single_line_ellipsis(
    painter: &egui::Painter,
    rect: Rect,
    text: &str,
    font_id: egui::FontId,
    color: Color32,
) {
    if rect.width() <= 4.0 || text.is_empty() {
        return;
    }

    let fits = |candidate: &str| {
        painter.layout_no_wrap(candidate.to_owned(), font_id.clone(), color).size().x
            <= rect.width()
    };

    let final_text = if fits(text) {
        text.to_owned()
    } else {
        let ellipsis = "...";
        let chars: Vec<char> = text.chars().collect();
        let mut truncated = ellipsis.to_owned();

        for keep in (0..chars.len()).rev() {
            let candidate = format!("{}{}", chars[..keep].iter().collect::<String>(), ellipsis);
            if fits(&candidate) {
                truncated = candidate;
                break;
            }
        }

        truncated
    };

    painter.text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        final_text,
        font_id,
        color,
    );
}

fn find_clip_in_sequence(
    seq: &Sequence,
    clip_id: ClipId,
) -> Option<&mondrian_timeline::clip::Clip> {
    for track in seq.video_tracks.iter().chain(seq.audio_tracks.iter()) {
        if let Some(clip) = track.clips.iter().find(|clip| clip.id == clip_id) {
            return Some(clip);
        }
    }
    None
}

#[derive(Copy, Clone)]
enum RulerGranularity {
    Frame,
    Second,
    Minute,
}

#[derive(Copy, Clone)]
struct RulerScale {
    major_step_frames: i64,
    minor_step_frames: i64,
    granularity: RulerGranularity,
}

fn choose_ruler_scale(pixels_per_frame: f32, fps: Rational) -> RulerScale {
    let fps_nominal = fps.to_f64().round().max(1.0) as i64;
    let pixels_per_second = pixels_per_frame * fps_nominal as f32;

    let (granularity, mut candidates): (RulerGranularity, Vec<i64>) = if pixels_per_second >= 120.0
    {
        (
            RulerGranularity::Frame,
            vec![
                1,
                2,
                5,
                10,
                15,
                (fps_nominal / 2).max(1),
                fps_nominal,
                fps_nominal * 2,
                fps_nominal * 5,
            ],
        )
    } else if pixels_per_second >= 12.0 {
        (
            RulerGranularity::Second,
            vec![1, 2, 5, 10, 15, 30].into_iter().map(|s| s * fps_nominal).collect(),
        )
    } else {
        (
            RulerGranularity::Minute,
            vec![1, 2, 5, 10, 15, 30, 60]
                .into_iter()
                .map(|m| m * 60 * fps_nominal)
                .collect(),
        )
    };

    candidates.sort_unstable();
    candidates.dedup();

    let min_major_pixels = 72.0;
    let major_step = candidates
        .iter()
        .copied()
        .find(|&step| pixels_per_frame * step as f32 >= min_major_pixels)
        .unwrap_or_else(|| candidates.last().copied().unwrap_or(1));

    let minor_step = choose_minor_step(major_step, pixels_per_frame);

    RulerScale {
        major_step_frames: major_step,
        minor_step_frames: minor_step,
        granularity,
    }
}

fn choose_minor_step(major_step_frames: i64, pixels_per_frame: f32) -> i64 {
    let min_minor_pixels = 8.0;
    for div in [10, 5, 4, 3, 2] {
        if major_step_frames % div == 0 {
            let minor = major_step_frames / div;
            if pixels_per_frame * minor as f32 >= min_minor_pixels {
                return minor.max(1);
            }
        }
    }
    major_step_frames.max(1)
}

fn format_ruler_label(
    frame: i64,
    fps: Rational,
    display_format: VideoDisplayFormat,
    granularity: RulerGranularity,
) -> String {
    if display_format == VideoDisplayFormat::Frames {
        return match granularity {
            RulerGranularity::Frame => frame.max(0).to_string(),
            RulerGranularity::Second | RulerGranularity::Minute => {
                let seconds = frame_to_seconds(frame, fps).floor() as i64;
                format_seconds_label(seconds, granularity)
            }
        };
    }

    if display_format == VideoDisplayFormat::FeetAndFrames16mm {
        return format_feet_and_frames(frame, 40);
    }

    if display_format == VideoDisplayFormat::FeetAndFrames35mm {
        return format_feet_and_frames(frame, 16);
    }

    if display_format == VideoDisplayFormat::Timecode2997DropFrame {
        return format_drop_frame_timecode(frame, 30);
    }

    let fps_nominal = fps.to_f64().round().max(1.0) as i64;
    let total_seconds = frame.div_euclid(fps_nominal);
    let frame_in_second = frame.rem_euclid(fps_nominal);

    match granularity {
        RulerGranularity::Frame => {
            let hours = total_seconds / 3600;
            let minutes = (total_seconds % 3600) / 60;
            let seconds = total_seconds % 60;
            format!("{hours:02}:{minutes:02}:{seconds:02}:{frame_in_second:02}")
        }
        RulerGranularity::Second | RulerGranularity::Minute => {
            format_seconds_label(total_seconds, granularity)
        }
    }
}

fn frame_to_seconds(frame: i64, fps: Rational) -> f64 {
    frame.max(0) as f64 / fps.to_f64().max(1.0)
}

fn format_seconds_label(total_seconds: i64, granularity: RulerGranularity) -> String {
    let total_seconds = total_seconds.max(0);
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    match granularity {
        RulerGranularity::Frame | RulerGranularity::Second => {
            format!("{hours:02}:{minutes:02}:{seconds:02}")
        }
        RulerGranularity::Minute => format!("{hours:02}:{minutes:02}"),
    }
}

fn format_feet_and_frames(frame: i64, frames_per_foot: i64) -> String {
    let frame = frame.max(0);
    let feet = frame / frames_per_foot;
    let frames = frame % frames_per_foot;
    format!("{feet}+{frames:02}")
}

fn format_drop_frame_timecode(frame: i64, nominal_fps: i64) -> String {
    let frame = frame.max(0);
    let drop_frames = ((nominal_fps as f64) * 0.066_666_666_7).round() as i64;
    let frames_per_hour = nominal_fps * 60 * 60;
    let frames_per_24_hours = frames_per_hour * 24;
    let frames_per_10_minutes = nominal_fps * 60 * 10 - drop_frames * 9;
    let frames_per_minute = nominal_fps * 60 - drop_frames;

    let mut d = frame % frames_per_24_hours;
    let hours = d / frames_per_hour;
    d %= frames_per_hour;
    let tens_of_minutes = d / frames_per_10_minutes;
    d %= frames_per_10_minutes;
    let minutes = tens_of_minutes * 10 + d / frames_per_minute;

    let dropped = drop_frames * (minutes - minutes / 10);
    let tc_frame = frame + dropped;
    let seconds = (tc_frame / nominal_fps) % 60;
    let frames = tc_frame % nominal_fps;
    format!("{hours:02}:{minutes:02}:{seconds:02};{frames:02}")
}

fn collect_snap_candidates(
    seq: &Sequence,
    state: &AppState,
    exclude_clip_id: Option<ClipId>,
) -> Vec<SnapCandidate> {
    let mut by_frame: HashMap<i64, SnapPriority> = HashMap::new();
    let mut register = |frame: i64, priority: SnapPriority| {
        let frame = frame.max(0);
        match by_frame.get_mut(&frame) {
            Some(existing) if priority < *existing => *existing = priority,
            Some(_) => {}
            None => {
                by_frame.insert(frame, priority);
            }
        }
    };

    register(seq.playhead.frame, SnapPriority::Playhead);

    for clip in seq
        .video_tracks
        .iter()
        .chain(seq.audio_tracks.iter())
        .flat_map(|track| track.clips.iter())
    {
        if exclude_clip_id.is_some_and(|exclude| clip.id == exclude) {
            continue;
        }
        register(clip.position.frame, SnapPriority::AdjacentClipEdge);
        register(clip.end_position().frame, SnapPriority::AdjacentClipEdge);
    }

    for marker_frame in collect_marker_snap_frames(state) {
        register(marker_frame, SnapPriority::Marker);
    }

    let in_point = state.in_point_frame().max(0);
    let out_point = state.out_point_frame();
    if in_point > 0 || out_point.is_some() {
        register(in_point, SnapPriority::InOutPoint);
        if let Some(out) = out_point {
            register(out, SnapPriority::InOutPoint);
        }
    }

    by_frame
        .into_iter()
        .map(|(frame, priority)| SnapCandidate { frame, priority })
        .collect()
}

fn collect_marker_snap_frames(_state: &AppState) -> Vec<i64> {
    // 当前项目模型尚未持久化 marker；这里预留吸附入口，后续接入 marker 数据即可生效。
    Vec::new()
}

fn decide_snap_target(
    target_frame: i64,
    candidates: &[SnapCandidate],
    pixels_per_frame: f32,
    snap_pixels: f32,
) -> SnapDecision {
    let target_frame = target_frame.max(0);
    if pixels_per_frame <= 0.0 || candidates.is_empty() {
        return SnapDecision { frame: target_frame, snapped: false };
    }

    let threshold_frames = (snap_pixels / pixels_per_frame).ceil().max(1.0) as i64;
    let mut best: Option<(SnapCandidate, i64)> = None;

    for candidate in candidates {
        let distance = (candidate.frame - target_frame).abs();
        if distance > threshold_frames {
            continue;
        }

        let should_replace = match best {
            None => true,
            Some((current, current_distance)) => {
                candidate.priority < current.priority
                    || (candidate.priority == current.priority && distance < current_distance)
                    || (candidate.priority == current.priority
                        && distance == current_distance
                        && candidate.frame < current.frame)
            }
        };
        if should_replace {
            best = Some((*candidate, distance));
        }
    }

    match best {
        Some((candidate, _)) => SnapDecision { frame: candidate.frame.max(0), snapped: true },
        None => SnapDecision { frame: target_frame, snapped: false },
    }
}

fn timeline_visible_span_bounds(total_frames: f64, viewport_width: f32) -> (f64, f64) {
    if total_frames <= 0.0 || viewport_width <= 0.0 {
        return (1.0, 1.0);
    }

    let min_visible = (viewport_width as f64 / tokens::timeline_max_pixels_per_frame() as f64)
        .max(1.0)
        .min(total_frames);
    let max_visible = (viewport_width as f64 / tokens::timeline_min_pixels_per_frame() as f64)
        .max(min_visible)
        .min(total_frames);
    (min_visible, max_visible)
}

fn timeline_scrollbar_metrics(
    track_width: f32,
    total_frames: f64,
    viewport_width: f32,
    pixels_per_frame: f32,
    offset_frames: f64,
) -> TimelineScrollbarMetrics {
    let total_frames = total_frames.max(1.0);
    let visible_span_frames = (viewport_width.max(1.0) as f64 / pixels_per_frame.max(0.01) as f64)
        .max(1.0)
        .min(total_frames);
    let max_offset = (total_frames - visible_span_frames).max(0.0);
    let offset_frames = offset_frames.clamp(0.0, max_offset);
    let min_thumb_width = (tokens::timeline_scrollbar_handle_width() * 2.0 + 10.0).min(track_width);
    let thumb_width = ((visible_span_frames / total_frames) as f32 * track_width)
        .clamp(min_thumb_width, track_width);
    let usable_track_width = (track_width - thumb_width).max(0.0);
    let thumb_left = if max_offset <= f64::EPSILON || usable_track_width <= 0.0 {
        0.0
    } else {
        (offset_frames / max_offset) as f32 * usable_track_width
    };

    TimelineScrollbarMetrics {
        total_frames,
        visible_span_frames,
        offset_frames,
        thumb_left,
        thumb_width,
        track_width,
    }
}

fn apply_timeline_scrollbar_drag(
    metrics: TimelineScrollbarMetrics,
    drag: TimelineScrollbarDragState,
    delta_px: f32,
    min_visible_span: f64,
    max_visible_span: f64,
) -> (f64, f64) {
    if metrics.track_width <= 0.0 || metrics.total_frames <= 0.0 {
        return (metrics.offset_frames, metrics.visible_span_frames);
    }

    let delta_frames = delta_px as f64 * (metrics.total_frames / metrics.track_width as f64);
    match drag.kind {
        TimelineScrollbarDragKind::Thumb => {
            let max_offset = (metrics.total_frames - drag.start_visible_span_frames).max(0.0);
            (
                (drag.start_offset_frames + delta_frames).clamp(0.0, max_offset),
                drag.start_visible_span_frames,
            )
        }
        TimelineScrollbarDragKind::LeadingHandle => {
            let right_edge = drag.start_offset_frames + drag.start_visible_span_frames;
            let min_left = (right_edge - max_visible_span).max(0.0);
            let max_left = (right_edge - min_visible_span).max(0.0);
            let new_left = (drag.start_offset_frames + delta_frames).clamp(min_left, max_left);
            let new_visible = (right_edge - new_left)
                .clamp(min_visible_span, max_visible_span)
                .min(metrics.total_frames.max(min_visible_span));
            let max_offset = (metrics.total_frames - new_visible).max(0.0);
            (new_left.min(max_offset), new_visible)
        }
        TimelineScrollbarDragKind::TrailingHandle => {
            let left_edge = drag.start_offset_frames;
            let min_right = left_edge + min_visible_span;
            let max_right = (left_edge + max_visible_span).min(metrics.total_frames);
            let start_right = left_edge + drag.start_visible_span_frames;
            let new_right = (start_right + delta_frames).clamp(min_right, max_right);
            let new_visible = (new_right - left_edge)
                .clamp(min_visible_span, max_visible_span)
                .min((metrics.total_frames - left_edge).max(min_visible_span));
            let max_offset = (metrics.total_frames - new_visible).max(0.0);
            (left_edge.min(max_offset), new_visible)
        }
    }
}

fn timeline_vertical_scrollbar_metrics(
    track_height: f32,
    total_rows: f32,
    visible_height: f32,
    current_track_height: f32,
    offset: f32,
) -> TimelineVerticalScrollbarMetrics {
    let total_rows = total_rows.max(1.0);
    let visible_rows = (visible_height.max(1.0) / current_track_height.max(1.0))
        .max(1.0)
        .min(total_rows);
    let offset_rows =
        (offset / current_track_height.max(1.0)).clamp(0.0, (total_rows - visible_rows).max(0.0));
    let min_thumb_height = 28.0f32.min(track_height);
    let thumb_height =
        ((visible_rows / total_rows) * track_height).clamp(min_thumb_height, track_height);
    let usable_track_height = (track_height - thumb_height).max(0.0);
    let max_offset_rows = (total_rows - visible_rows).max(0.0);
    let thumb_top = if max_offset_rows <= f32::EPSILON || usable_track_height <= 0.0 {
        0.0
    } else {
        (offset_rows / max_offset_rows) * usable_track_height
    };

    TimelineVerticalScrollbarMetrics {
        total_rows,
        visible_rows,
        thumb_top,
        thumb_height,
        track_height,
    }
}

fn apply_timeline_vertical_scrollbar_drag(
    metrics: TimelineVerticalScrollbarMetrics,
    drag: TimelineVerticalScrollbarDragState,
    delta_px: f32,
) -> f32 {
    if metrics.track_height <= 0.0 || metrics.total_rows <= metrics.visible_rows {
        return 0.0;
    }

    let delta_rows = delta_px * (metrics.total_rows / metrics.track_height);
    let max_offset_rows = (metrics.total_rows - metrics.visible_rows).max(0.0);
    let offset_rows = (drag.start_offset / drag.start_track_height.max(1.0) + delta_rows)
        .clamp(0.0, max_offset_rows);
    offset_rows * drag.start_track_height.max(1.0)
}

fn apply_timeline_vertical_zoom_drag(
    metrics: TimelineVerticalScrollbarMetrics,
    drag: TimelineVerticalScrollbarDragState,
    delta_px: f32,
    visible_height: f32,
) -> (f32, f32) {
    const ZOOM_SENSITIVITY: f32 = 0.35;

    let start_track_height = drag.start_track_height.max(tokens::timeline_min_track_height());
    let start_visible_rows =
        (visible_height / start_track_height.max(1.0)).max(1.0).min(metrics.total_rows);
    let start_top_row = drag.start_offset / start_track_height.max(1.0);
    let start_bottom_row = (start_top_row + start_visible_rows).min(metrics.total_rows);
    let row_delta =
        delta_px * (metrics.total_rows / metrics.track_height.max(1.0)) * ZOOM_SENSITIVITY;
    let target_visible_rows = match drag.kind {
        TimelineVerticalScrollbarDragKind::LeadingHandle => {
            (start_visible_rows - row_delta).max(1.0)
        }
        TimelineVerticalScrollbarDragKind::TrailingHandle => {
            (start_visible_rows + row_delta).max(1.0)
        }
        TimelineVerticalScrollbarDragKind::Thumb => start_visible_rows,
    }
    .min(metrics.total_rows);

    let new_track_height = visible_height / target_visible_rows.max(1.0);
    let new_track_height = new_track_height.clamp(
        tokens::timeline_min_track_height(),
        tokens::timeline_max_track_height(),
    );
    let new_visible_rows =
        (visible_height / new_track_height.max(1.0)).max(1.0).min(metrics.total_rows);
    let max_top_row = (metrics.total_rows - new_visible_rows).max(0.0);
    let new_top_row = match drag.kind {
        TimelineVerticalScrollbarDragKind::LeadingHandle => {
            (start_bottom_row - new_visible_rows).clamp(0.0, max_top_row)
        }
        TimelineVerticalScrollbarDragKind::TrailingHandle => start_top_row.clamp(0.0, max_top_row),
        TimelineVerticalScrollbarDragKind::Thumb => start_top_row.clamp(0.0, max_top_row),
    };

    (new_track_height, new_top_row * new_track_height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track_row(
        track_id: TrackId,
        is_video_track: bool,
        track_index: usize,
        top: f32,
    ) -> TrackRowVisual {
        let rect = Rect::from_min_size(Pos2::new(0.0, top), Vec2::new(120.0, 40.0));
        TrackRowVisual { track_id, is_video_track, track_index, rect }
    }
    #[test]
    fn snap_stays_free_when_no_candidate_in_threshold() {
        let decision = decide_snap_target(
            120,
            &[SnapCandidate { frame: 140, priority: SnapPriority::Playhead }],
            4.0,
            10.0,
        );

        assert_eq!(decision.frame, 120);
        assert!(!decision.snapped);
    }

    #[test]
    fn snap_prefers_priority_over_distance_within_threshold() {
        let decision = decide_snap_target(
            100,
            &[
                SnapCandidate {
                    frame: 99,
                    priority: SnapPriority::AdjacentClipEdge,
                },
                SnapCandidate { frame: 107, priority: SnapPriority::Playhead },
            ],
            1.0,
            10.0,
        );

        assert_eq!(decision.frame, 107);
        assert!(decision.snapped);
    }

    #[test]
    fn snap_prefers_nearest_when_priority_same() {
        let decision = decide_snap_target(
            100,
            &[
                SnapCandidate {
                    frame: 96,
                    priority: SnapPriority::AdjacentClipEdge,
                },
                SnapCandidate {
                    frame: 103,
                    priority: SnapPriority::AdjacentClipEdge,
                },
            ],
            2.0,
            8.0,
        );

        assert_eq!(decision.frame, 103);
        assert!(decision.snapped);
    }

    #[test]
    fn snap_clamps_negative_target_to_zero() {
        let decision = decide_snap_target(-3, &[], 4.0, 10.0);
        assert_eq!(decision.frame, 0);
        assert!(!decision.snapped);
    }

    #[test]
    fn ruler_label_formats_drop_frame_timecode() {
        let label = format_ruler_label(
            1800,
            Rational::FPS_2997,
            VideoDisplayFormat::Timecode2997DropFrame,
            RulerGranularity::Frame,
        );
        assert_eq!(label, "00:01:00;02");
    }

    #[test]
    fn ruler_label_formats_feet_and_frames() {
        assert_eq!(
            format_ruler_label(
                41,
                Rational::FPS_24,
                VideoDisplayFormat::FeetAndFrames16mm,
                RulerGranularity::Frame,
            ),
            "1+01"
        );
        assert_eq!(
            format_ruler_label(
                17,
                Rational::FPS_24,
                VideoDisplayFormat::FeetAndFrames35mm,
                RulerGranularity::Frame,
            ),
            "1+01"
        );
    }

    #[test]
    fn track_drag_target_uses_current_row_order_for_video_tracks() {
        let rows = vec![
            track_row(TrackId::new(), true, 2, 0.0),
            track_row(TrackId::new(), true, 1, 40.0),
            track_row(TrackId::new(), true, 0, 80.0),
        ];
        let drag = TrackDragState {
            track_id: rows[2].track_id,
            is_video_track: true,
            source_index: 0,
        };

        let target =
            choose_track_drag_target(Pos2::new(20.0, 12.0), &rows, drag).expect("target row");
        assert_eq!(target.target_index, 2);
    }

    #[test]
    fn track_drag_target_ignores_other_media_section() {
        let rows = vec![
            track_row(TrackId::new(), true, 1, 0.0),
            track_row(TrackId::new(), true, 0, 40.0),
            track_row(TrackId::new(), false, 0, 80.0),
        ];
        let drag = TrackDragState {
            track_id: rows[0].track_id,
            is_video_track: true,
            source_index: 1,
        };

        let target = choose_track_drag_target(Pos2::new(20.0, 96.0), &rows, drag);
        assert!(target.is_none());
    }

    #[test]
    fn timeline_scrollbar_dragging_thumb_pans_offset() {
        let metrics = timeline_scrollbar_metrics(200.0, 400.0, 100.0, 2.0, 40.0);
        let drag = TimelineScrollbarDragState {
            kind: TimelineScrollbarDragKind::Thumb,
            start_pointer_x: 0.0,
            start_offset_frames: metrics.offset_frames,
            start_visible_span_frames: metrics.visible_span_frames,
        };

        let (new_offset, new_visible) =
            apply_timeline_scrollbar_drag(metrics, drag, 25.0, 10.0, 200.0);

        assert!(new_offset > metrics.offset_frames);
        assert_eq!(new_visible, metrics.visible_span_frames);
    }

    #[test]
    fn timeline_scrollbar_leading_handle_changes_visible_span() {
        let metrics = timeline_scrollbar_metrics(240.0, 600.0, 120.0, 2.0, 100.0);
        let drag = TimelineScrollbarDragState {
            kind: TimelineScrollbarDragKind::LeadingHandle,
            start_pointer_x: 0.0,
            start_offset_frames: metrics.offset_frames,
            start_visible_span_frames: metrics.visible_span_frames,
        };

        let (new_offset, new_visible) =
            apply_timeline_scrollbar_drag(metrics, drag, 20.0, 20.0, 260.0);

        assert!(new_offset > metrics.offset_frames);
        assert!(new_visible < metrics.visible_span_frames);
    }

    #[test]
    fn timeline_scrollbar_trailing_handle_respects_max_visible_span() {
        let metrics = timeline_scrollbar_metrics(240.0, 600.0, 120.0, 2.0, 60.0);
        let drag = TimelineScrollbarDragState {
            kind: TimelineScrollbarDragKind::TrailingHandle,
            start_pointer_x: 0.0,
            start_offset_frames: metrics.offset_frames,
            start_visible_span_frames: metrics.visible_span_frames,
        };

        let (new_offset, new_visible) =
            apply_timeline_scrollbar_drag(metrics, drag, 500.0, 20.0, 180.0);

        assert_eq!(new_offset, metrics.offset_frames);
        assert!(new_visible <= 180.0);
    }

    #[test]
    fn vertical_scrollbar_dragging_thumb_pans_offset() {
        let metrics = timeline_vertical_scrollbar_metrics(240.0, 20.0, 200.0, 40.0, 80.0);
        let drag = TimelineVerticalScrollbarDragState {
            kind: TimelineVerticalScrollbarDragKind::Thumb,
            start_pointer_y: 0.0,
            start_offset: 80.0,
            start_track_height: 40.0,
        };

        let new_offset = apply_timeline_vertical_scrollbar_drag(metrics, drag, 24.0);

        assert!(new_offset > drag.start_offset);
    }

    #[test]
    fn vertical_scrollbar_thumb_fills_track_when_content_fits() {
        let metrics = timeline_vertical_scrollbar_metrics(240.0, 3.0, 240.0, 80.0, 0.0);

        assert_eq!(metrics.thumb_top, 0.0);
        assert_eq!(metrics.thumb_height, 240.0);
    }

    #[test]
    fn vertical_scrollbar_handle_drag_changes_track_height() {
        let metrics = timeline_vertical_scrollbar_metrics(240.0, 10.0, 200.0, 40.0, 0.0);
        let drag = TimelineVerticalScrollbarDragState {
            kind: TimelineVerticalScrollbarDragKind::LeadingHandle,
            start_pointer_y: 0.0,
            start_offset: 0.0,
            start_track_height: 40.0,
        };

        let (new_height, _new_offset) =
            apply_timeline_vertical_zoom_drag(metrics, drag, 24.0, 200.0);

        assert!(new_height > 40.0);
    }

    #[test]
    fn vertical_scrollbar_leading_zoom_keeps_bottom_anchor() {
        let metrics = timeline_vertical_scrollbar_metrics(240.0, 12.0, 200.0, 40.0, 80.0);
        let drag = TimelineVerticalScrollbarDragState {
            kind: TimelineVerticalScrollbarDragKind::LeadingHandle,
            start_pointer_y: 0.0,
            start_offset: 80.0,
            start_track_height: 40.0,
        };
        let start_visible_rows = 200.0 / 40.0;
        let start_bottom_row = drag.start_offset / drag.start_track_height + start_visible_rows;

        let (new_height, new_offset) =
            apply_timeline_vertical_zoom_drag(metrics, drag, 24.0, 200.0);
        let new_visible_rows = 200.0 / new_height;
        let new_bottom_row = new_offset / new_height + new_visible_rows;

        assert!((new_bottom_row - start_bottom_row).abs() < 0.001);
    }

    #[test]
    fn vertical_scrollbar_trailing_zoom_keeps_top_anchor() {
        let metrics = timeline_vertical_scrollbar_metrics(240.0, 12.0, 200.0, 40.0, 80.0);
        let drag = TimelineVerticalScrollbarDragState {
            kind: TimelineVerticalScrollbarDragKind::TrailingHandle,
            start_pointer_y: 0.0,
            start_offset: 80.0,
            start_track_height: 40.0,
        };
        let start_top_row = drag.start_offset / drag.start_track_height;

        let (new_height, new_offset) =
            apply_timeline_vertical_zoom_drag(metrics, drag, 24.0, 200.0);
        let new_top_row = new_offset / new_height;

        assert!((new_top_row - start_top_row).abs() < 0.001);
    }

    #[test]
    fn vertical_scrollbar_zoom_is_gentler_than_one_row_per_small_drag() {
        let metrics = timeline_vertical_scrollbar_metrics(240.0, 10.0, 200.0, 40.0, 0.0);
        let drag = TimelineVerticalScrollbarDragState {
            kind: TimelineVerticalScrollbarDragKind::LeadingHandle,
            start_pointer_y: 0.0,
            start_offset: 0.0,
            start_track_height: 40.0,
        };

        let (new_height, _new_offset) =
            apply_timeline_vertical_zoom_drag(metrics, drag, 24.0, 200.0);

        assert!(new_height < 45.0);
    }
}
