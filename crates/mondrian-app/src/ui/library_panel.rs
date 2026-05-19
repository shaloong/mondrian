use crate::{
    app::AppState,
    ui::theme::{self, corner_radius, palette, tokens, typography},
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
struct AssetCardBadge {
    text: String,
    bg: egui::Color32,
    fg: egui::Color32,
    align_right: bool,
}

#[derive(Clone)]
struct AssetCardPresentation {
    placeholder_icon: theme::UiIcon,
    badges: Vec<AssetCardBadge>,
    duration_text: Option<String>,
    drag_duration: Duration,
    target_lane_label: &'static str,
}

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
#[derive(Clone)]
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

    fn create_adjustment_layer(&mut self, state: &mut AppState) {
        match state.create_adjustment_layer_asset(None) {
            Ok(asset_id) => {
                self.selected_asset = Some(asset_id);
            }
            Err(err) => {
                state.set_status_hint(format!("新建调整图层失败：{err}"), true);
            }
        }
    }

    fn next_folder_name(&self, state: &AppState) -> String {
        let Some(library) = state.asset_library.as_ref() else {
            return "文件夹 1".to_string();
        };
        let folders = library.list_folders().unwrap_or_default();
        let mut next = 1usize;
        for folder in &folders {
            if let Some(suffix) = folder.name.strip_prefix("文件夹 ") {
                if let Ok(n) = suffix.trim().parse::<usize>() {
                    next = next.max(n + 1);
                }
            }
        }
        format!("文件夹 {next}")
    }

    pub fn show(&mut self, ui: &mut Ui, state: &mut AppState) {
        let panel_rect = ui.max_rect();
        const SEARCH_MARGIN_TOP: f32 = 6.0;
        const SEARCH_MARGIN_BOTTOM: f32 = 12.0;

        let content = ui.vertical(|ui| {
            ui.add_space(SEARCH_MARGIN_TOP);
            let search_height = ui.spacing().interact_size.y + 4.0;
            let import_button_width = 28.0;
            let search_gap = 8.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let search_width =
                    (ui.available_width() - import_button_width - search_gap).max(48.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(search_width, search_height),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        super::widgets::search_bar(ui, &mut self.search_query, "搜索素材…");
                    },
                );
                ui.add_space(search_gap);
                let import_clicked = theme::icon_button(
                    ui,
                    [28.0, ui.spacing().interact_size.y],
                    theme::UiIcon::FolderOpen,
                )
                .on_hover_text("导入媒体")
                .clicked();
                if import_clicked {
                    self.open_import_dialog(state);
                }
            });
            ui.add_space(SEARCH_MARGIN_BOTTOM);

            // Blank-area context menu: response created BEFORE the ScrollArea
            // so cards (rendered later, higher z-order) consume clicks first.
            // Right-click on blank space → this response gets it.
            let blank_rect = ui.available_rect_before_wrap();
            let blank_resp = ui.interact(
                blank_rect,
                ui.id().with("library_blank_context"),
                Sense::click(),
            );

            let list_h = ui.available_height().max(tokens::list_min_height());
            egui::ScrollArea::vertical()
                .id_salt("library_scroll")
                .max_height(list_h)
                .show(ui, |ui| {
                    self.show_assets(ui, state);
                });

            blank_resp.context_menu(|ui| {
                ui.menu_button("新建", |ui| {
                    if ui.button("调整图层").clicked() {
                        self.create_adjustment_layer(state);
                        ui.close();
                    }
                    if ui.button("文件夹").clicked() {
                        let name = self.next_folder_name(state);
                        if let Err(err) = state.create_folder_in_library(&name) {
                            state.set_status_hint(format!("创建文件夹失败：{err}"), true);
                        }
                        ui.close();
                    }
                });
            });
        });

        let _ = content;
        self.handle_external_drop(ui, state, panel_rect);
    }

    fn import_media(&mut self, state: &mut AppState) {
        state.clear_status_hint();

        if state.asset_library.is_none() {
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
                corner_radius(tokens::card_rounding()),
                Stroke::new(tokens::border_standard(), palette::border_subtle()),
                egui::StrokeKind::Inside,
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
        let mut name_tooltip: Option<String> = None;
        let mut name_rect_for_click: Option<Rect> = None;
        let proxy_mode = state.is_asset_proxy_mode(asset.id);
        let is_offline = asset_is_offline(asset);
        let is_editing = self.editing_asset == Some(asset.id);
        let is_selected = self.selected_asset == Some(asset.id);

        let (card_rect, mut card_response) =
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
            Stroke::new(tokens::border_standard(), palette::interaction_highlight())
        } else if card_response.hovered() {
            Stroke::new(
                tokens::border_standard(),
                palette::border_subtle().gamma_multiply(0.7),
            )
        } else {
            Stroke::NONE
        };
        if card_fill != egui::Color32::TRANSPARENT {
            ui.painter().rect_filled(card_rect, card_rounding, card_fill);
        }
        if card_stroke != Stroke::NONE {
            ui.painter().rect_stroke(
                card_rect,
                corner_radius(card_rounding),
                card_stroke,
                egui::StrokeKind::Inside,
            );
        }

        let thumb_rect = Rect::from_min_size(
            card_rect.min + Vec2::new(8.0, CARD_PADDING_TOP),
            Vec2::new((card_w - 16.0).max(10.0), thumb_h.max(20.0)),
        );
        let presentation = asset_card_presentation(asset, state, proxy_mode, is_offline);

        self.draw_thumbnail(
            ui,
            asset,
            is_offline,
            thumb_rect,
            presentation.placeholder_icon,
        );
        self.draw_thumbnail_badges(ui, thumb_rect, &presentation.badges);

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
        ui.scope_builder(egui::UiBuilder::new().max_rect(info_rect), |ui| {
            ui.set_min_width(info_rect.width());
            ui.set_max_width(info_rect.width());
            if is_editing {
                let edit_rect = info_rect;
                let edit_resp = ui.add_sized(
                    [info_rect.width(), tokens::list_row_content_height()],
                    egui::TextEdit::singleline(&mut self.editing_name),
                );
                edit_resp.request_focus();
                // Esc: cancel without saving
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.editing_asset = None;
                    self.editing_name.clear();
                    return;
                }
                // Click outside the edit rect submits
                let ptr_in_edit = ui.input(|i| {
                    i.pointer.interact_pos().map(|p| edit_rect.contains(p)).unwrap_or(false)
                });
                let clicked_outside = ui.input(|i| i.pointer.primary_released()) && !ptr_in_edit;
                let submit = edit_resp.lost_focus()
                    || ui.input(|i| i.key_pressed(egui::Key::Enter))
                    || clicked_outside;
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
                let (name_rect, duration_rect) =
                    if presentation.duration_text.as_ref().is_some_and(|text| !text.is_empty()) {
                        let name_w = (info_rect.width() - duration_w - info_gap).max(24.0);
                        let name_rect =
                            Rect::from_min_size(info_rect.min, Vec2::new(name_w, row_h));
                        let duration_rect = Rect::from_min_size(
                            Pos2::new(name_rect.right() + info_gap, info_rect.top()),
                            Vec2::new(duration_w, row_h),
                        );
                        (name_rect, Some(duration_rect))
                    } else {
                        (info_rect, None)
                    };

                // Draw name with painter — no widget to intercept card_response clicks
                let name_text = asset_name.trim_start();
                let name_galley = ui.painter().layout_no_wrap(
                    name_text.to_string(),
                    typography::body_small(),
                    palette::text_primary(),
                );
                name_rect_for_click = Some(name_rect);
                let name_fits = name_galley.size().x <= name_rect.width();
                if !name_fits {
                    name_tooltip = Some(asset_name.clone());
                }
                let display_name: String = if name_fits {
                    name_text.to_string()
                } else {
                    // Manual character-by-character truncation with "..."
                    let dots = "...";
                    let dots_w = ui
                        .painter()
                        .layout_no_wrap(
                            dots.to_string(),
                            typography::body_small(),
                            palette::text_primary(),
                        )
                        .size()
                        .x;
                    let limit = (name_rect.width() - dots_w).max(0.0);
                    let mut chars: Vec<char> = name_text.chars().collect();
                    while !chars.is_empty() {
                        let s: String = chars.iter().collect();
                        let w = ui
                            .painter()
                            .layout_no_wrap(
                                s.clone(),
                                typography::body_small(),
                                palette::text_primary(),
                            )
                            .size()
                            .x;
                        if w <= limit {
                            break;
                        }
                        chars.pop();
                    }
                    format!("{}{dots}", chars.iter().collect::<String>())
                };
                ui.painter().text(
                    name_rect.left_center(),
                    egui::Align2::LEFT_CENTER,
                    &display_name,
                    typography::body_small(),
                    palette::text_primary(),
                );
                if let (Some(duration_rect), Some(duration_text)) =
                    (duration_rect, presentation.duration_text.as_ref())
                {
                    ui.scope_builder(
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
            }
        });

        // Tooltip: use card's hover but only when pointer is inside name rect
        if let Some(name_r) = name_rect_for_click {
            let in_name =
                ui.input(|i| i.pointer.interact_pos().map(|p| name_r.contains(p)).unwrap_or(false));
            if in_name {
                card_response = card_response.on_hover_text(asset_name.clone());
            }
        }

        if card_response.clicked() {
            self.selected_asset = Some(asset.id);
        }


        // Double-click on name area only — matches Pr/Ae/DaVinci behavior
        // where thumbnail double-click has different meaning (open in viewer)
        let double_clicked = ui
            .input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary))
            && name_rect_for_click.is_some_and(|nr| {
                ui.input(|i| i.pointer.interact_pos().map(|p| nr.contains(p)).unwrap_or(false))
            });
        if double_clicked {
            self.editing_asset = Some(asset.id);
            self.editing_name = asset_name.clone();
        }

        // Standard egui context menu per card.
        let is_offline2 = is_offline;
        card_response.context_menu(|ui| {
            if ui.button("重命名").clicked() {
                self.editing_asset = Some(asset.id);
                self.editing_name = asset_name.clone();
                ui.close();
            }
            if ui.button("在文件管理器中显示").clicked() {
                reveal_in_file_manager(&asset.path);
                ui.close();
            }
            ui.separator();
            if matches!(asset.kind, AssetKind::Video) {
                let proxy_gen =
                    mondrian_media::ProxyGenerator::new(mondrian_media::ProxyConfig::default());
                let has_proxy = proxy_gen.proxy_exists(asset.path.as_path());
                let mut proxy_toggle = state.is_asset_proxy_mode(asset.id);
                if theme::checkmark_menu_toggle(ui, &mut proxy_toggle, "代理模式").clicked() {
                    state.set_asset_proxy_mode(asset.id, proxy_toggle);
                    let _ = state.save_project_file();
                    if proxy_toggle && !has_proxy {
                        self.spawn_proxy_generation(asset.id, asset.path.clone());
                        state.set_status_hint(
                            format!("已开启代理模式：{}（后台生成中）", asset_name),
                            false,
                        );
                    } else {
                        state.set_status_hint(
                            format!("已开启代理模式：{}", asset_name), false
                        );
                    }
                    ui.close();
                }
                ui.separator();
            }
            if is_offline2 {
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
                    ui.close();
                }
                ui.separator();
            }
            if ui.button("删除素材").clicked() {
                match state.delete_asset_from_library(asset.id) {
                    Ok(()) => {
                        self.thumbnail_cache.remove(&asset.id);
                        self.thumbnail_failures.remove(&asset.id);
                        if self.selected_asset == Some(asset.id) {
                            self.selected_asset = None;
                        }
                        state.set_status_hint(format!("已删除素材：{}", asset_name), false);
                    }
                    Err(err) => {
                        state.set_status_hint(format!("删除素材失败：{err}"), true);
                    }
                }
                ui.close();
            }
        });

        if card_response.drag_started() && !is_editing {
            self.selected_asset = Some(asset.id);
            state.begin_drag_asset(
                asset.id,
                asset_name.clone(),
                asset.kind.clone(),
                presentation.drag_duration,
                matches!(asset.kind, AssetKind::Video) && asset.media_info.has_audio,
            );
            state.set_status_hint(
                format!(
                    "拖拽中：{}（释放到{}）",
                    asset_name, presentation.target_lane_label
                ),
                false,
            );
        }
    }

    fn draw_thumbnail_badges(&self, ui: &Ui, rect: Rect, badges: &[AssetCardBadge]) {
        for badge in badges {
            let anchor = if badge.align_right {
                rect.right_bottom() + Vec2::new(-6.0, -6.0)
            } else {
                rect.left_bottom() + Vec2::new(6.0, -6.0)
            };
            self.draw_thumbnail_badge(
                ui,
                anchor,
                badge.text.as_str(),
                badge.align_right,
                badge.bg,
                badge.fg,
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
            corner_radius(tokens::badge_rounding()),
            Stroke::new(tokens::border_standard(), fg.gamma_multiply(0.18)),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            font_id,
            fg,
        );
    }

    fn draw_thumbnail(
        &mut self,
        ui: &mut Ui,
        asset: &AssetRecord,
        is_offline: bool,
        rect: Rect,
        placeholder_icon: theme::UiIcon,
    ) {
        ui.painter().rect_filled(rect, tokens::section_rounding(), palette::canvas_bg());
        ui.painter().rect_stroke(
            rect,
            corner_radius(tokens::section_rounding()),
            Stroke::new(
                tokens::border_standard(),
                palette::border_subtle().gamma_multiply(0.7),
            ),
            egui::StrokeKind::Inside,
        );

        if is_offline {
            self.draw_thumbnail_placeholder(ui, rect, theme::UiIcon::Warning);
            return;
        }

        if !matches!(asset.kind, AssetKind::Video) {
            self.draw_thumbnail_placeholder(ui, rect, placeholder_icon);
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
            self.draw_thumbnail_placeholder(ui, rect, placeholder_icon);
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

fn asset_is_offline(asset: &AssetRecord) -> bool {
    !matches!(asset.kind, AssetKind::AdjustmentLayer) && !asset.path.exists()
}

fn asset_card_presentation(
    asset: &AssetRecord,
    state: &AppState,
    proxy_mode: bool,
    is_offline: bool,
) -> AssetCardPresentation {
    let mut badges = Vec::with_capacity(2);
    if let Some(status_badge) = asset_status_badge(proxy_mode, is_offline) {
        badges.push(status_badge);
    }
    badges.push(asset_kind_badge(&asset.kind));

    AssetCardPresentation {
        placeholder_icon: asset_placeholder_icon(&asset.kind),
        badges,
        duration_text: asset_duration_text(asset),
        drag_duration: asset_drag_duration(asset, state),
        target_lane_label: asset_target_lane_label(&asset.kind),
    }
}

fn asset_status_badge(proxy_mode: bool, is_offline: bool) -> Option<AssetCardBadge> {
    if is_offline {
        Some(AssetCardBadge {
            text: "OFFLINE".to_string(),
            bg: palette::status_warning().gamma_multiply(0.20),
            fg: palette::status_warning(),
            align_right: false,
        })
    } else if proxy_mode {
        Some(AssetCardBadge {
            text: "PROXY".to_string(),
            bg: palette::interaction_highlight().gamma_multiply(0.18),
            fg: palette::interaction_highlight(),
            align_right: false,
        })
    } else {
        None
    }
}

fn asset_kind_badge(kind: &AssetKind) -> AssetCardBadge {
    let (text, bg, fg) = match kind {
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
        AssetKind::AdjustmentLayer => (
            "调整",
            palette::accent_secondary().gamma_multiply(0.22),
            palette::text_primary(),
        ),
    };
    AssetCardBadge { text: text.to_string(), bg, fg, align_right: true }
}

fn asset_placeholder_icon(kind: &AssetKind) -> theme::UiIcon {
    match kind {
        AssetKind::Video => theme::UiIcon::Video,
        AssetKind::Audio => theme::UiIcon::Audio,
        AssetKind::AdjustmentLayer => theme::UiIcon::Plus,
    }
}

fn asset_target_lane_label(kind: &AssetKind) -> &'static str {
    match kind {
        AssetKind::Audio => "音频轨",
        AssetKind::Video | AssetKind::AdjustmentLayer => "视频轨",
    }
}

fn asset_has_display_duration(asset: &AssetRecord) -> bool {
    asset.media_info.duration > Duration::ZERO && !matches!(asset.kind, AssetKind::AdjustmentLayer)
}

fn asset_duration_text(asset: &AssetRecord) -> Option<String> {
    asset_has_display_duration(asset).then(|| format_duration_hhmmss(asset.media_info.duration))
}

fn asset_drag_duration(asset: &AssetRecord, state: &AppState) -> Duration {
    if asset_has_display_duration(asset) {
        asset.media_info.duration
    } else {
        state.default_adjustment_layer_drag_duration()
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

fn reveal_in_file_manager(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg("/select,").arg(path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(parent) = path.parent() {
            let _ = std::process::Command::new("xdg-open").arg(parent).spawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_assets::AssetKind;
    use mondrian_core::types::AssetId;
    use mondrian_media::MediaInfo;

    fn asset(kind: AssetKind, duration: Duration) -> AssetRecord {
        let mut media_info = match kind {
            AssetKind::AdjustmentLayer => MediaInfo::synthetic_adjustment_layer(),
            _ => MediaInfo::synthetic_adjustment_layer(),
        };
        media_info.duration = duration;
        AssetRecord {
            id: AssetId::new(),
            name: "Test".to_string(),
            kind,
            path: PathBuf::from("test"),
            folder_id: None,
            media_info,
            created_at: "now".to_string(),
            updated_at: "now".to_string(),
        }
    }

    #[test]
    fn asset_duration_text_hides_variable_length_assets() {
        assert_eq!(
            asset_duration_text(&asset(AssetKind::AdjustmentLayer, Duration::ZERO)),
            None
        );
        assert_eq!(
            asset_duration_text(&asset(AssetKind::Video, Duration::ZERO)),
            None
        );
        assert_eq!(
            asset_duration_text(&asset(AssetKind::Audio, Duration::from_secs(65))),
            Some("01:05".to_string())
        );
    }

    #[test]
    fn asset_card_presentation_uses_consistent_badge_slots() {
        let state = AppState::default();
        let video = asset_card_presentation(
            &asset(AssetKind::Video, Duration::from_secs(10)),
            &state,
            true,
            false,
        );
        assert_eq!(video.badges.len(), 2);
        assert!(!video.badges[0].align_right);
        assert!(video.badges[1].align_right);
        assert_eq!(video.badges[0].text, "PROXY");
        assert_eq!(video.badges[1].text, "视频");

        let adjustment = asset_card_presentation(
            &asset(AssetKind::AdjustmentLayer, Duration::ZERO),
            &state,
            false,
            false,
        );
        assert_eq!(adjustment.badges.len(), 1);
        assert_eq!(adjustment.badges[0].text, "调整");
        assert!(adjustment.badges[0].align_right);
        assert_eq!(adjustment.duration_text, None);
    }
}
