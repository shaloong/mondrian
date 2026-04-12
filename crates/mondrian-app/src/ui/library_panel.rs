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

        let content = ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let _ = theme::icon(ui, theme::UiIcon::Search, palette::text_muted());
                ui.text_edit_singleline(&mut self.search_query);
            });

            ui.separator();

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
                .rect_filled(empty_rect, tokens::list_row_radius(), palette::bg_surface());
            ui.painter().text(
                empty_rect.center(),
                egui::Align2::CENTER_CENTER,
                "导入媒体以开始",
                typography::body_large(),
                palette::text_muted(),
            );
            if empty_resp.double_clicked() {
                self.open_import_dialog(state);
            }
            return;
        }

        self.gc_thumbnail_cache(&assets);

        const GRID_SPACING_X: f32 = 8.0;
        const GRID_SPACING_Y: f32 = 10.0;
        const CARD_MIN_WIDTH: f32 = 150.0;
        const CARD_MIN_HEIGHT: f32 = 152.0;
        const THUMB_ASPECT: f32 = 16.0 / 9.0;

        let available_w = ui.available_width().max(CARD_MIN_WIDTH);
        let cols = ((available_w + GRID_SPACING_X) / (CARD_MIN_WIDTH + GRID_SPACING_X))
            .floor()
            .max(1.0) as usize;
        let total_spacing = GRID_SPACING_X * cols.saturating_sub(1) as f32;
        let card_w = ((available_w - total_spacing) / cols as f32).max(CARD_MIN_WIDTH);
        let thumb_h = (card_w / THUMB_ASPECT).round();
        let card_h = (thumb_h + 38.0).max(CARD_MIN_HEIGHT);

        for row in assets.chunks(cols) {
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
                for _ in row.len()..cols {
                    let _ = ui.allocate_exact_size(Vec2::new(card_w, card_h), Sense::hover());
                }
            });
            ui.add_space(GRID_SPACING_Y);
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

        if is_selected {
            ui.painter().rect_filled(
                card_rect,
                tokens::list_row_radius(),
                palette::bg_surface_hover(),
            );
            ui.painter().rect_stroke(
                card_rect,
                tokens::list_row_radius(),
                Stroke::new(1.0, palette::interaction_highlight().gamma_multiply(0.7)),
            );
        } else if card_response.hovered() {
            ui.painter().rect_stroke(
                card_rect,
                tokens::list_row_radius(),
                Stroke::new(1.0, palette::border_subtle().gamma_multiply(0.6)),
            );
        }

        let thumb_rect = Rect::from_min_size(
            card_rect.min + Vec2::new(4.0, 4.0),
            Vec2::new((card_w - 8.0).max(10.0), thumb_h.max(20.0)),
        );
        self.draw_thumbnail(ui, asset, is_offline, thumb_rect);
        self.draw_thumbnail_badges(ui, thumb_rect, asset.kind.clone(), proxy_mode, is_offline);

        let info_rect = Rect::from_min_max(
            Pos2::new(card_rect.left() + 4.0, thumb_rect.bottom() + 4.0),
            Pos2::new(card_rect.right() - 4.0, card_rect.bottom() - 4.0),
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
                let row_h = tokens::list_row_content_height();
                let duration_w = 58.0;
                let name_w = (info_rect.width() - duration_w - 6.0).max(30.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let name_resp = ui.add_sized(
                        [name_w, row_h],
                        egui::Label::new(
                            egui::RichText::new(asset_name.trim_start())
                                .color(palette::text_primary()),
                        )
                        .truncate(),
                    );
                    let _ = name_resp.on_hover_text(asset_name.as_str());

                    ui.allocate_ui_with_layout(
                        Vec2::new(duration_w, row_h),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.label(
                                egui::RichText::new(duration_text.as_str())
                                    .size(tokens::list_proxy_tag_font_size())
                                    .color(palette::text_muted()),
                            );
                        },
                    );
                });
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
        let kind_text = match kind {
            AssetKind::Video => "VIDEO",
            AssetKind::Audio => "AUDIO",
        };
        self.draw_thumbnail_badge(
            ui,
            rect.right_bottom() - Vec2::new(6.0, 6.0),
            kind_text,
            true,
            palette::bg_base().gamma_multiply(0.78),
            palette::text_primary(),
        );

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
        let w = 10.0 + text.chars().count() as f32 * 6.6;
        let h = 16.0;
        let rect = if align_right {
            Rect::from_min_size(Pos2::new(anchor.x - w, anchor.y - h), Vec2::new(w, h))
        } else {
            Rect::from_min_size(Pos2::new(anchor.x, anchor.y - h), Vec2::new(w, h))
        };
        ui.painter().rect_filled(rect, 3.0, bg);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            typography::body_small(),
            fg,
        );
    }

    fn draw_thumbnail(&mut self, ui: &mut Ui, asset: &AssetRecord, is_offline: bool, rect: Rect) {
        ui.painter().rect_filled(rect, 4.0, palette::canvas_bg());

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
