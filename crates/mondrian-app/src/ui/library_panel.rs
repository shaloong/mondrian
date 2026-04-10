use crate::{
    app::AppState,
    ui::theme::{self, palette, tokens, typography},
};
use egui::{Ui, Vec2};
use rfd::FileDialog;
use std::path::Path;

/// 左侧素材库面板
#[derive(Default)]
pub struct LibraryPanel {
    search_query: String,
    editing_asset: Option<mondrian_core::types::AssetId>,
    editing_name: String,
    accept_next_external_drop: bool,
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
                        if matches!(asset.kind, mondrian_assets::AssetKind::Video) {
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

        for asset in assets {
            let asset_name = asset.name.clone();
            let proxy_generator =
                mondrian_media::ProxyGenerator::new(mondrian_media::ProxyConfig::default());
            let has_proxy = proxy_generator.proxy_exists(asset.path.as_path());
            let proxy_mode = state.is_asset_proxy_mode(asset.id);
            let (row_rect, row_response) = ui.allocate_exact_size(
                Vec2::new(ui.available_width(), tokens::list_row_height()),
                egui::Sense::click_and_drag(),
            );
            let is_editing = self.editing_asset == Some(asset.id);

            if row_response.hovered() {
                ui.painter().rect_filled(
                    row_rect,
                    tokens::list_row_radius(),
                    palette::bg_surface_hover(),
                );
            }

            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(row_rect), |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = tokens::list_compact_spacing_x();
                    let icon_col_w = tokens::list_icon_column_width();

                    let kind_icon = match asset.kind {
                        mondrian_assets::AssetKind::Video => theme::UiIcon::Video,
                        mondrian_assets::AssetKind::Audio => theme::UiIcon::Audio,
                    };
                    ui.allocate_ui_with_layout(
                        Vec2::new(icon_col_w, tokens::list_row_content_height()),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            let _ = theme::icon(ui, kind_icon, palette::text_muted());
                        },
                    );

                    if is_editing {
                        let edit_w =
                            (row_rect.width() - icon_col_w - tokens::list_row_edit_padding())
                                .max(tokens::list_row_edit_min_width());
                        let edit_resp = ui.add_sized(
                            [edit_w, tokens::list_row_content_height()],
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
                        let proxy_tag_w = if proxy_mode {
                            tokens::list_proxy_tag_width()
                        } else {
                            0.0
                        };
                        let name_w = (row_rect.width()
                            - icon_col_w
                            - proxy_tag_w
                            - tokens::list_row_edit_padding() * 0.5)
                            .max(tokens::list_row_name_min_width());
                        let display_name = asset_name.trim_start();
                        ui.add_sized(
                            [name_w, tokens::list_row_content_height()],
                            egui::Label::new(
                                egui::RichText::new(display_name).color(palette::text_primary()),
                            )
                            .truncate(),
                        );

                        if proxy_mode {
                            ui.add_sized(
                                [proxy_tag_w, tokens::list_row_content_height()],
                                egui::Label::new(
                                    egui::RichText::new("代理")
                                        .color(palette::interaction_highlight())
                                        .size(tokens::list_proxy_tag_font_size()),
                                ),
                            );
                        }
                    }
                });
            });

            row_response.context_menu(|ui| {
                if matches!(asset.kind, mondrian_assets::AssetKind::Video) {
                    let mut proxy_mode_toggle = proxy_mode;
                    if theme::checkmark_toggle(ui, &mut proxy_mode_toggle, "代理模式").clicked()
                    {
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
                                state.set_status_hint(
                                    format!("已开启代理模式：{}", asset_name),
                                    false,
                                );
                            }
                        } else {
                            state.set_status_hint(format!("已关闭代理模式：{}", asset_name), false);
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
                            state.event_bus.publish(
                                mondrian_core::events::AppEvent::AssetDeleted {
                                    asset_id: asset.id,
                                },
                            );
                            let _ = state.save_project_file();
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

            if row_response.double_clicked() {
                self.editing_asset = Some(asset.id);
                self.editing_name = asset_name.clone();
            }

            if row_response.drag_started() && !is_editing {
                state.begin_drag_asset(
                    asset.id,
                    asset_name.clone(),
                    asset.kind.clone(),
                    asset.media_info.duration,
                    matches!(asset.kind, mondrian_assets::AssetKind::Video)
                        && asset.media_info.has_audio,
                );
                let lane = match asset.kind {
                    mondrian_assets::AssetKind::Video => "视频轨",
                    mondrian_assets::AssetKind::Audio => "音频轨",
                };
                state.set_status_hint(format!("拖拽中：{}（释放到{}）", asset_name, lane), false);
            }

            if state.dragging_asset().map(|d| d.asset_id) == Some(asset.id) {
                ui.colored_label(palette::interaction_highlight(), "正在拖拽到时间线...");
            }
            ui.separator();
        }
    }
}
