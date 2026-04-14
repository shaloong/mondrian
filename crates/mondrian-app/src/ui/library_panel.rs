use crate::{
    app::AppState,
    ui::theme::{self, palette, tokens, typography},
};
use egui::{Pos2, Rect, Sense, Stroke, Ui, Vec2};
use mondrian_assets::{AssetKind, AssetRecord};
use mondrian_core::types::AssetId;
use rfd::FileDialog;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

#[derive(Clone)]
struct ThumbnailCacheEntry {
    path: PathBuf,
    modified: Option<SystemTime>,
    texture: egui::TextureHandle,
}

#[derive(Clone)]
struct ThumbnailFailureEntry {
    path: PathBuf,
    modified: Option<SystemTime>,
}

const GRID_SPACING_X: f32 = 20.0;
const GRID_SPACING_Y: f32 = 12.0;
const CARD_TARGET_WIDTH: f32 = 176.0;
const CARD_MIN_WIDTH: f32 = 150.0;
const THUMB_ASPECT: f32 = 16.0 / 9.0;
const CARD_PADDING_TOP: f32 = 8.0;
const CARD_PADDING_BOTTOM: f32 = 10.0;
const CARD_INFO_GAP_Y: f32 = 6.0;

/// 左侧素材库面板
#[derive(Default)]
pub struct LibraryPanel {
    search_query: String,
    editing_asset: Option<AssetId>,
    editing_name: String,
    selected_asset: Option<AssetId>,
    accept_next_external_drop: bool,
    thumbnail_cache: HashMap<AssetId, ThumbnailCacheEntry>,
    thumbnail_failures: HashMap<AssetId, ThumbnailFailureEntry>,
}

impl LibraryPanel {
    pub fn open_import_dialog(&mut self, state: &mut AppState) {
        self.import_media(state);
    }

    pub fn show(&mut self, ui: &mut Ui, state: &mut AppState) {
        let panel_rect = ui.max_rect();
        const SEARCH_MARGIN_TOP: f32 = 6.0;
        const SEARCH_MARGIN_BOTTOM: f32 = 12.0;

        let content = ui.vertical(|ui| {
            ui.add_space(SEARCH_MARGIN_TOP);
            let search_height = ui.spacing().interact_size.y + 8.0;
            ui.allocate_ui_with_layout(
                Vec2::new(ui.available_width(), search_height),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    egui::Frame::none()
                        .fill(palette::bg_surface_raised())
                        .stroke(Stroke::new(1.0, palette::border_subtle()))
                        .rounding(egui::Rounding::same(tokens::button_rounding()))
                        .inner_margin(egui::Margin::symmetric(10.0, 4.0))
                        .show(ui, |ui| {
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    let _ = theme::icon(
                                        ui,
                                        theme::UiIcon::Search,
                                        palette::text_muted(),
                                    );
                                    ui.add_sized(
                                        [ui.available_width(), ui.spacing().interact_size.y],
                                        egui::TextEdit::singleline(&mut self.search_query)
                                            .frame(false)
                                            .margin(egui::Margin::ZERO)
                                            .vertical_align(egui::Align::Center),
                                    );
                                },
                            );
                        });
                },
            );
            ui.add_space(SEARCH_MARGIN_BOTTOM);

            let list_h = ui.available_height().max(tokens::list_min_height());
            egui::ScrollArea::vertical().id_salt("library_scroll").max_height(list_h).show(
                ui,
                |ui| {
                    self.show_assets(ui, state);
                },
            );
        });

        let _ = content;
        self.handle_external_drop(ui, state, panel_rect);
    }

    fn import_media(&mut self, state: &mut AppState) {
        state.clear_status_hint();

        if state.asset_library.is_none() {
            state.set_status_hint("素材库未连接", true);
            return;
        }

        let Some(path) = FileDialog::new()
            .add_filter("Video", &["mp4", "mov", "mkv", "webm", "avi"])
            .add_filter("Audio", &["mp3", "wav", "aac", "flac", "m4a"])
            .pick_file()
        else {
            return;
        };

        self.import_from_path(state, &path);
    }

    fn import_from_path(&mut self, state: &mut AppState, path: &Path) {
        let Some(library) = state.asset_library.as_ref() else {
            state.set_status_hint("素材库未连接", true);
            return;
        };

        match library.import_media_file(path) {
            Ok(asset_id) => {
                let mut proxy_task_started = false;
                if state.auto_proxy_enabled {
                    if let Ok(Some(asset)) = library.get_asset(asset_id) {
                        if matches!(asset.kind, AssetKind::Video) {
                            state.set_asset_proxy_mode(asset_id, true);
                            self.spawn_proxy_generation(asset_id, asset.path.clone());
                            proxy_task_started = true;
                        }
                    }
                } else {
                    state.set_asset_proxy_mode(asset_id, false);
                }

                state
                    .event_bus
                    .publish(mondrian_core::events::AppEvent::AssetImported { asset_id });
                let _ = state.save_project_file();
                let display_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("untitled");
                if proxy_task_started {
                    state.set_status_hint(
                        format!("已导入并设为代理模式：{}（后台生成代理中）", display_name),
                        false,
                    );
                } else {
                    state.set_status_hint(format!("已导入：{}", display_name), false);
                }
            }
            Err(err) => {
                state.set_status_hint(format!("导入失败：{err}"), true);
            }
        }
    }

    fn spawn_proxy_generation(
        &self,
        asset_id: mondrian_core::types::AssetId,
        source_path: std::path::PathBuf,
    ) {
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
            if let Ok(rt) = runtime {
                let generator =
                    mondrian_media::ProxyGenerator::new(mondrian_media::ProxyConfig::default());
                let (progress_tx, _progress_rx) = tokio::sync::mpsc::channel(8);
                let _ = rt.block_on(generator.generate(asset_id, source_path, progress_tx));
            }
        });
    }

    fn handle_external_drop(&mut self, ui: &Ui, state: &mut AppState, panel_rect: egui::Rect) {
        let hovered_files = ui.ctx().input(|i| i.raw.hovered_files.clone());
        let pointer_pos = ui.ctx().input(|i| i.pointer.hover_pos());
        let hovering_on_panel = !hovered_files.is_empty()
            && pointer_pos.map(|pos| panel_rect.contains(pos)).unwrap_or(true);

        if hovering_on_panel {
            self.accept_next_external_drop = true;
            theme::draw_drop_overlay(ui.painter(), panel_rect, "释放以导入媒体");
        }

        let dropped_files = ui.ctx().input(|i| i.raw.dropped_files.clone());
        if dropped_files.is_empty() {
            if hovered_files.is_empty() {
                self.accept_next_external_drop = false;
            }
            return;
        }

        let should_import = self.accept_next_external_drop
            || pointer_pos.map(|pos| panel_rect.contains(pos)).unwrap_or(false);
        self.accept_next_external_drop = false;

        if !should_import {
            return;
        }

        let mut imported_count = 0usize;
        state.clear_status_hint();

        for file in dropped_files {
            if let Some(path) = file.path {
                self.import_from_path(state, &path);
                if state.status_hint.as_ref().map(|(_, is_error)| !is_error).unwrap_or(false) {
                    imported_count += 1;
                }
            }
        }

        if imported_count > 0 {
            state.set_status_hint(format!("已导入 {} 个媒体文件", imported_count), false);
        }
    }

    fn show_assets(&mut self, ui: &mut Ui, state: &mut AppState) {
        ui.style_mut().interaction.selectable_labels = false;

        let Some(library) = state.asset_library.clone() else {
            ui.colored_label(palette::status_warning(), "素材库未连接");
            return;
        };

        let Ok(mut assets) = library.list_assets() else {
            ui.colored_label(palette::status_error(), "素材列表读取失败");
            return;
        };

        if !self.search_query.trim().is_empty() {
            let query = self.search_query.to_lowercase();
            assets.retain(|asset| asset.name.to_lowercase().contains(&query));
        }

        if assets.is_empty() {
            let fill_h = ui.available_height().max(tokens::list_empty_height());
            let (empty_rect, empty_resp) = ui.allocate_exact_size(
                Vec2::new(ui.available_width(), fill_h),
                egui::Sense::click(),
            );
            ui.painter()
                .rect_filled(empty_rect, tokens::card_rounding(), palette::bg_surface());
            ui.painter().rect_stroke(
                empty_rect,
                tokens::card_rounding(),
                Stroke::new(1.0, palette::border_subtle()),
            );
            ui.painter().text(
                empty_rect.center(),
                egui::Align2::CENTER_CENTER,
                "导入媒体以开始",
                typography::body(),
                palette::text_muted(),
            );
            if empty_resp.double_clicked() {
                self.open_import_dialog(state);
            }
            return;
        }

        self.gc_thumbnail_cache(&assets);

        let available_w = ui.available_width().max(CARD_MIN_WIDTH);
        let cols = ((available_w + GRID_SPACING_X) / (CARD_TARGET_WIDTH + GRID_SPACING_X))
            .floor()
            .max(1.0) as usize;
        let total_spacing = GRID_SPACING_X * cols.saturating_sub(1) as f32;
        let card_w = ((available_w - total_spacing) / cols as f32).floor().max(CARD_MIN_WIDTH);
        let thumb_h = (card_w / THUMB_ASPECT).round();
        let card_h = CARD_PADDING_TOP
            + thumb_h
            + CARD_INFO_GAP_Y
            + tokens::list_row_content_height()
            + CARD_PADDING_BOTTOM;

        let row_count = assets.len().div_ceil(cols);
        for (row_index, row) in assets.chunks(cols).enumerate() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = GRID_SPACING_X;
                for asset in row {
                    self.draw_asset_card(
                        ui,
                        state,
                        library.as_ref(),
                        asset,
                        card_w,
                        card_h,
                        thumb_h,
                    );
                }
            });
            if row_index + 1 < row_count {
                ui.add_space(GRID_SPACING_Y);
            }
        }
    }

    fn draw_asset_card(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        library: &mondrian_assets::AssetLibrary,
        asset: &AssetRecord,
        card_w: f32,
        card_h: f32,
        thumb_h: f32,
    ) {
        let asset_name = asset.name.clone();
        let proxy_generator =
            mondrian_media::ProxyGenerator::new(mondrian_media::ProxyConfig::default());
        let has_proxy = proxy_generator.proxy_exists(asset.path.as_path());
        let proxy_mode = state.is_asset_proxy_mode(asset.id);
        let is_offline = !asset.path.exists();
        let is_editing = self.editing_asset == Some(asset.id);
        let is_selected = self.selected_asset == Some(asset.id);

        let (card_rect, card_response) =
            ui.allocate_exact_size(Vec2::new(card_w, card_h), Sense::click_and_drag());

        let card_rounding = tokens::card_rounding();
        let card_fill = if is_selected {
            palette::accent_secondary().gamma_multiply(0.42)
        } else if card_response.hovered() {
            palette::bg_surface_hover()
        } else {
            egui::Color32::TRANSPARENT
        };
        let card_stroke = if is_selected {
            Stroke::new(1.0, palette::interaction_highlight())
        } else if card_response.hovered() {
            Stroke::new(1.0, palette::border_subtle().gamma_multiply(0.7))
        } else {
            Stroke::NONE
        };
        if card_fill != egui::Color32::TRANSPARENT {
            ui.painter().rect_filled(card_rect, card_rounding, card_fill);
        }
        if card_stroke != Stroke::NONE {
            ui.painter().rect_stroke(card_rect, card_rounding, card_stroke);
        }

        let thumb_rect = Rect::from_min_size(
            card_rect.min + Vec2::new(8.0, CARD_PADDING_TOP),
            Vec2::new((card_w - 16.0).max(10.0), thumb_h.max(20.0)),
        );
        self.draw_thumbnail(ui, asset, is_offline, thumb_rect);
        self.draw_thumbnail_badges(ui, thumb_rect, asset.kind.clone(), proxy_mode, is_offline);

        let row_h = tokens::list_row_content_height();
        let info_rect = Rect::from_min_max(
            Pos2::new(
                card_rect.left() + 10.0,
                thumb_rect.bottom() + CARD_INFO_GAP_Y,
            ),
            Pos2::new(
                card_rect.right() - 10.0,
                thumb_rect.bottom() + CARD_INFO_GAP_Y + row_h,
            ),
        );
        let duration_text = format_duration_hhmmss(asset.media_info.duration);

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(info_rect), |ui| {
            ui.set_min_width(info_rect.width());
            ui.set_max_width(info_rect.width());
            if is_editing {
                let edit_resp = ui.add_sized(
                    [info_rect.width(), tokens::list_row_content_height()],
                    egui::TextEdit::singleline(&mut self.editing_name),
                );
                let submit =
                    edit_resp.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter));
                if submit {
                    match library.rename_asset(asset.id, &self.editing_name) {
                        Ok(_) => {
                            let _ = state.save_project_file();
                            state.set_status_hint(
                                format!("已重命名为：{}", self.editing_name.trim()),
                                false,
                            );
                        }
                        Err(err) => {
                            state.set_status_hint(format!("重命名失败：{err}"), true);
                        }
                    }
                    self.editing_asset = None;
                    self.editing_name.clear();
                }
            } else {
                let duration_w = 46.0;
                let info_gap = 8.0;
                let name_w = (info_rect.width() - duration_w - info_gap).max(24.0);
                let name_rect = Rect::from_min_size(info_rect.min, Vec2::new(name_w, row_h));
                let duration_rect = Rect::from_min_size(
                    Pos2::new(name_rect.right() + info_gap, info_rect.top()),
                    Vec2::new(duration_w, row_h),
                );

                ui.allocate_new_ui(
                    egui::UiBuilder::new()
                        .max_rect(name_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(asset_name.trim_start())
                                    .font(typography::body_small())
                                    .color(palette::text_primary()),
                            )
                            .truncate()
                            .halign(egui::Align::LEFT),
                        );
                    },
                );
                ui.allocate_new_ui(
                    egui::UiBuilder::new()
                        .max_rect(duration_rect)
                        .layout(egui::Layout::right_to_left(egui::Align::Center)),
                    |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(duration_text.as_str())
                                    .font(typography::body_small())
                                    .color(palette::text_muted()),
                            )
                            .halign(egui::Align::RIGHT),
                        );
                    },
                );
            }
        });

        if card_response.clicked() {
            self.selected_asset = Some(asset.id);
        }

        card_response.context_menu(|ui| {
            if matches!(asset.kind, AssetKind::Video) {
                let mut proxy_mode_toggle = proxy_mode;
                if theme::checkmark_toggle(ui, &mut proxy_mode_toggle, "代理模式").clicked() {
                    state.set_asset_proxy_mode(asset.id, proxy_mode_toggle);
                    let _ = state.save_project_file();

                    if proxy_mode_toggle {
                        if !has_proxy {
                            self.spawn_proxy_generation(asset.id, asset.path.clone());
                            state.set_status_hint(
                                format!("已开启代理模式：{}（后台生成中）", asset_name),
                                false,
                            );
                        } else {
                            state.set_status_hint(format!("已开启代理模式：{}", asset_name), false);
                        }
                    } else {
                        state.set_status_hint(format!("已关闭代理模式：{}", asset_name), false);
                    }

                    ui.close_menu();
                }

                ui.separator();
            }

            if is_offline {
                if ui.button("重新链接素材…").clicked() {
                    if let Some(path) = FileDialog::new().pick_file() {
                        match state.relink_asset(asset.id, &path) {
                            Ok(()) => {
                                state.set_status_hint(
                                    format!("已重新链接：{} -> {}", asset_name, path.display()),
                                    false,
                                );
                                self.thumbnail_cache.remove(&asset.id);
                                self.thumbnail_failures.remove(&asset.id);
                            }
                            Err(err) => {
                                state.set_status_hint(format!("重连失败：{err}"), true);
                            }
                        }
                    }
                    ui.close_menu();
                }
                ui.separator();
            }

            if ui.button("删除素材").clicked() {
                match library.delete_asset(asset.id) {
                    Ok(_) => {
                        let removed_timeline_clips =
                            state.delete_asset_and_cleanup_timeline(asset.id).unwrap_or(0);
                        state.event_bus.publish(mondrian_core::events::AppEvent::AssetDeleted {
                            asset_id: asset.id,
                        });
                        let _ = state.save_project_file();
                        self.thumbnail_cache.remove(&asset.id);
                        self.thumbnail_failures.remove(&asset.id);
                        self.selected_asset = self.selected_asset.filter(|id| *id != asset.id);
                        state.set_status_hint(
                            format!(
                                "已删除：{}（时间轴移除 {} 个片段）",
                                asset_name, removed_timeline_clips
                            ),
                            false,
                        );
                    }
                    Err(err) => {
                        state.set_status_hint(format!("删除失败：{err}"), true);
                    }
                }
                ui.close_menu();
            }
        });

        if card_response.double_clicked() {
            self.editing_asset = Some(asset.id);
            self.editing_name = asset_name.clone();
        }

        if card_response.drag_started() && !is_editing {
            self.selected_asset = Some(asset.id);
            state.begin_drag_asset(
                asset.id,
                asset_name.clone(),
                asset.kind.clone(),
                asset.media_info.duration,
                matches!(asset.kind, AssetKind::Video) && asset.media_info.has_audio,
            );
            let lane = match asset.kind {
                AssetKind::Video => "视频轨",
                AssetKind::Audio => "音频轨",
            };
            state.set_status_hint(format!("拖拽中：{}（释放到{}）", asset_name, lane), false);
        }
    }

    fn draw_thumbnail_badges(
        &self,
        ui: &Ui,
        rect: Rect,
        kind: AssetKind,
        proxy_mode: bool,
        is_offline: bool,
    ) {
        if is_offline {
            self.draw_thumbnail_badge(
                ui,
                rect.left_bottom() + Vec2::new(6.0, -6.0),
                "OFFLINE",
                false,
                palette::status_warning().gamma_multiply(0.20),
                palette::status_warning(),
            );
        } else if proxy_mode {
            self.draw_thumbnail_badge(
                ui,
                rect.left_bottom() + Vec2::new(6.0, -6.0),
                "PROXY",
                false,
                palette::interaction_highlight().gamma_multiply(0.18),
                palette::interaction_highlight(),
            );
        }

        let (label, bg, fg) = match kind {
            AssetKind::Video => (
                "视频",
                palette::bg_surface_raised().gamma_multiply(0.92),
                palette::text_muted(),
            ),
            AssetKind::Audio => (
                "音频",
                palette::bg_surface_raised().gamma_multiply(0.92),
                palette::text_muted(),
            ),
        };
        self.draw_thumbnail_badge(
            ui,
            rect.right_bottom() + Vec2::new(-6.0, -6.0),
            label,
            true,
            bg,
            fg,
        );
    }

    fn draw_thumbnail_badge(
        &self,
        ui: &Ui,
        anchor: Pos2,
        text: &str,
        align_right: bool,
        bg: egui::Color32,
        fg: egui::Color32,
    ) {
        let font_id = typography::body_small();
        let text_size = ui.painter().layout_no_wrap(text.to_owned(), font_id.clone(), fg).size();
        let padding_x = 6.0;
        let padding_y = 3.0;
        let w = (text_size.x + padding_x * 2.0).ceil().max(24.0);
        let h = (text_size.y + padding_y * 2.0).ceil().max(16.0);
        let rect = if align_right {
            Rect::from_min_size(Pos2::new(anchor.x - w, anchor.y - h), Vec2::new(w, h))
        } else {
            Rect::from_min_size(Pos2::new(anchor.x, anchor.y - h), Vec2::new(w, h))
        };
        ui.painter().rect_filled(rect, tokens::badge_rounding(), bg);
        ui.painter().rect_stroke(
            rect,
            tokens::badge_rounding(),
            Stroke::new(1.0, fg.gamma_multiply(0.18)),
        );
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            font_id,
            fg,
        );
    }

    fn draw_thumbnail(&mut self, ui: &mut Ui, asset: &AssetRecord, is_offline: bool, rect: Rect) {
        ui.painter().rect_filled(rect, tokens::section_rounding(), palette::canvas_bg());
        ui.painter().rect_stroke(
            rect,
            tokens::section_rounding(),
            Stroke::new(1.0, palette::border_subtle().gamma_multiply(0.7)),
        );

        if is_offline {
            self.draw_thumbnail_placeholder(ui, rect, theme::UiIcon::Warning);
            return;
        }

        if matches!(asset.kind, AssetKind::Audio) {
            self.draw_thumbnail_placeholder(ui, rect, theme::UiIcon::Audio);
            return;
        }

        if let Some(texture) = self.video_thumbnail_texture(ui, asset) {
            ui.painter().image(
                texture.id(),
                rect,
                Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                palette::image_tint(),
            );
        } else {
            self.draw_thumbnail_placeholder(ui, rect, theme::UiIcon::Video);
        }
    }

    fn draw_thumbnail_placeholder(&self, ui: &Ui, rect: Rect, icon: theme::UiIcon) {
        let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(20.0));
        theme::draw_icon(ui.painter(), icon_rect, icon, palette::text_muted());
    }

    fn video_thumbnail_texture(
        &mut self,
        ui: &Ui,
        asset: &AssetRecord,
    ) -> Option<egui::TextureHandle> {
        if !matches!(asset.kind, AssetKind::Video) {
            return None;
        }
        if !asset.path.exists() {
            return None;
        }

        let modified = std::fs::metadata(&asset.path).ok().and_then(|m| m.modified().ok());
        let path = asset.path.clone();

        if let Some(found) = self.thumbnail_cache.get(&asset.id) {
            if found.path == path && found.modified == modified {
                return Some(found.texture.clone());
            }
        }
        if let Some(failed) = self.thumbnail_failures.get(&asset.id) {
            if failed.path == path && failed.modified == modified {
                return None;
            }
        }

        let decoded = match mondrian_media::decode_video_frame_at_time_rgba_scaled(
            path.as_path(),
            0.0,
            Some(320),
            Some(180),
        ) {
            Ok(frame) => frame,
            Err(_) => {
                self.thumbnail_failures
                    .insert(asset.id, ThumbnailFailureEntry { path, modified });
                return None;
            }
        };

        let image = egui::ColorImage::from_rgba_unmultiplied(
            [decoded.width as usize, decoded.height as usize],
            &decoded.data,
        );
        let texture = ui.ctx().load_texture(
            format!(
                "library-thumb-{}-{}x{}",
                asset.id, decoded.width, decoded.height
            ),
            image,
            egui::TextureOptions::LINEAR,
        );

        self.thumbnail_cache.insert(
            asset.id,
            ThumbnailCacheEntry { path, modified, texture: texture.clone() },
        );
        self.thumbnail_failures.remove(&asset.id);
        Some(texture)
    }

    fn gc_thumbnail_cache(&mut self, assets: &[AssetRecord]) {
        let alive: std::collections::HashSet<AssetId> = assets.iter().map(|a| a.id).collect();
        self.thumbnail_cache.retain(|id, _| alive.contains(id));
        self.thumbnail_failures.retain(|id, _| alive.contains(id));
    }
}

fn format_duration_hhmmss(duration: Duration) -> String {
    let secs = duration.as_secs();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}
