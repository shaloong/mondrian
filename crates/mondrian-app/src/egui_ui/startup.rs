use std::path::PathBuf;
use std::sync::OnceLock;

use crate::egui_ui::theme::{corner_radius, margin_px};

#[derive(Debug, Clone)]
pub struct BootstrapRecentProjectItem {
    pub project_name: String,
    pub project_path: String,
    pub last_edited_label: String,
    pub project_size_label: String,
}

#[derive(Debug, Clone)]
pub struct BootstrapRecoveryItem {
    pub project_name: String,
    pub autosave_path: String,
    pub age_label: String,
    pub total_snapshots: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapAction {
    OpenProject,
    OpenRecent(PathBuf),
    NewProject,
    Quit,
    Recover(usize),
}

pub fn show_project_bootstrap_window(
    ui: &mut egui::Ui,
    _project_extension: &str,
    recent_items: &[BootstrapRecentProjectItem],
    recovery_items: &[BootstrapRecoveryItem],
) -> Option<BootstrapAction> {
    let ctx = ui.ctx().clone();
    let mut action = None;

    let bg_surface = crate::egui_ui::theme::palette::bg_surface();
    let bg_surface_raised = crate::egui_ui::theme::palette::bg_surface_raised();
    let bg_surface_hover = crate::egui_ui::theme::palette::bg_surface_hover();
    let border_subtle = crate::egui_ui::theme::palette::border_subtle();
    let text_primary = crate::egui_ui::theme::palette::text_primary();
    let text_muted = crate::egui_ui::theme::palette::text_muted();
    let interaction_highlight = crate::egui_ui::theme::palette::interaction_highlight();
    let overlay_fill = crate::egui_ui::theme::palette::overlay_fill();
    let panel_rounding = crate::egui_ui::theme::tokens::panel_rounding();
    let section_rounding = crate::egui_ui::theme::tokens::section_rounding();
    let startup_content_padding_x = crate::egui_ui::theme::tokens::startup_content_padding_x();
    let startup_content_padding_y = crate::egui_ui::theme::tokens::startup_content_padding_y();
    let startup_left_panel_width = crate::egui_ui::theme::tokens::startup_left_panel_width();
    let startup_close_button_size = crate::egui_ui::theme::tokens::startup_close_button_size();
    let startup_close_button_margin_x =
        crate::egui_ui::theme::tokens::startup_close_button_margin_x();
    let startup_close_button_margin_y =
        crate::egui_ui::theme::tokens::startup_close_button_margin_y();
    let startup_right_content_top_offset =
        crate::egui_ui::theme::tokens::startup_right_content_top_offset();
    let startup_panel_margin_x = crate::egui_ui::theme::tokens::startup_panel_margin_x();
    let startup_panel_margin_y = crate::egui_ui::theme::tokens::startup_panel_margin_y();
    let button_rounding = crate::egui_ui::theme::tokens::button_rounding();
    let list_row_radius = crate::egui_ui::theme::tokens::list_row_radius();

    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(egui::Color32::TRANSPARENT)
                .stroke(egui::Stroke::NONE)
                .inner_margin(egui::Margin::same(0)),
        )
        .show_inside(ui, |ui| {
            let root = ui.max_rect();
            let panel_rect =
                root.shrink2(egui::vec2(startup_panel_margin_x, startup_panel_margin_y));

            let drag_area = ui.interact(
                panel_rect,
                egui::Id::new("bootstrap_drag_area"),
                egui::Sense::click_and_drag(),
            );
            if drag_area.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }

            let close_rect = egui::Rect::from_min_size(
                egui::pos2(
                    panel_rect.right() - startup_close_button_size - startup_close_button_margin_x,
                    panel_rect.top() + startup_close_button_margin_y,
                ),
                egui::vec2(startup_close_button_size, startup_close_button_size),
            );
            let close_resp = ui.interact(
                close_rect,
                egui::Id::new("bootstrap_custom_close_button"),
                egui::Sense::click(),
            );

            let left_width = startup_left_panel_width;
            let card_gap = 0.0;
            let left_rect = egui::Rect::from_min_size(
                panel_rect.min,
                egui::vec2(left_width, panel_rect.height()),
            );
            let right_rect = egui::Rect::from_min_size(
                egui::pos2(left_rect.right() + card_gap, panel_rect.top()),
                egui::vec2(
                    panel_rect.right() - left_width - card_gap - panel_rect.left(),
                    panel_rect.height(),
                ),
            );

            paint_banner_card(ui, left_rect, panel_rounding, bg_surface, overlay_fill);
            paint_action_card(
                ui,
                right_rect,
                section_rounding,
                bg_surface_raised,
                bg_surface_hover,
                border_subtle,
                text_primary,
                text_muted,
                interaction_highlight,
                button_rounding,
                list_row_radius,
                startup_content_padding_x,
                startup_content_padding_y,
                startup_right_content_top_offset,
                recent_items,
                recovery_items,
                &mut action,
            );

            if close_resp.hovered() {
                ui.painter().rect_stroke(
                    close_rect,
                    corner_radius(crate::egui_ui::theme::tokens::button_rounding()),
                    egui::Stroke::new(
                        crate::egui_ui::theme::tokens::border_standard(),
                        bg_surface_hover,
                    ),
                    egui::StrokeKind::Inside,
                );
            }
            let x_color = text_primary;
            let x_inset = crate::egui_ui::theme::tokens::startup_close_button_size() * 0.285;
            let x_stroke = egui::Stroke::new(
                crate::egui_ui::theme::tokens::border_standard() * 2.0,
                x_color,
            );
            ui.painter().line_segment(
                [
                    close_rect.left_top() + egui::vec2(x_inset, x_inset),
                    close_rect.right_bottom() - egui::vec2(x_inset, x_inset),
                ],
                x_stroke,
            );
            ui.painter().line_segment(
                [
                    close_rect.left_bottom() + egui::vec2(x_inset, -x_inset),
                    close_rect.right_top() + egui::vec2(-x_inset, x_inset),
                ],
                x_stroke,
            );
            if close_resp.clicked() {
                action = Some(BootstrapAction::Quit);
            }
        });

    action
}

#[allow(clippy::too_many_arguments)]
fn paint_action_card(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    section_rounding: f32,
    bg_surface_raised: egui::Color32,
    bg_surface_hover: egui::Color32,
    border_subtle: egui::Color32,
    text_primary: egui::Color32,
    text_muted: egui::Color32,
    interaction_highlight: egui::Color32,
    button_rounding: f32,
    list_row_radius: f32,
    startup_content_padding_x: f32,
    startup_content_padding_y: f32,
    startup_right_content_top_offset: f32,
    recent_items: &[BootstrapRecentProjectItem],
    recovery_items: &[BootstrapRecoveryItem],
    action: &mut Option<BootstrapAction>,
) {
    ui.painter().rect_filled(
        rect,
        egui::CornerRadius {
            nw: 0,
            ne: section_rounding.round().clamp(0.0, 255.0) as u8,
            sw: 0,
            se: section_rounding.round().clamp(0.0, 255.0) as u8,
        },
        bg_surface_raised,
    );

    let content = rect.shrink2(egui::vec2(
        startup_content_padding_x,
        startup_content_padding_y,
    ));
    ui.scope_builder(egui::UiBuilder::new().max_rect(content), |ui| {
        ui.set_clip_rect(content.expand(2.0));
        let sp = crate::egui_ui::theme::tokens::spacing_md();
        ui.spacing_mut().item_spacing = egui::vec2(sp, sp + 2.0);

        ui.add_space(startup_right_content_top_offset);

        ui.label(egui::RichText::new("开始工作").size(17.0).strong().color(text_primary));
        ui.add_space(crate::egui_ui::theme::tokens::spacing_sm());

        ui.horizontal(|ui| {
            let gap = crate::egui_ui::theme::tokens::panel_gap();
            let icon_size = crate::egui_ui::theme::tokens::icon_size();
            let button_h = crate::egui_ui::theme::tokens::effect_item_height();
            let text_font = crate::egui_ui::theme::typography::button();
            let horizontal_padding = crate::egui_ui::theme::tokens::panel_inner_margin_x();
            let available = ui.available_width().max(160.0);
            let max_per_button = ((available - gap).max(120.0) * 0.5).floor();

            let new_button_w = icon_labeled_button_width(
                ui,
                "新建项目",
                text_font.clone(),
                icon_size,
                horizontal_padding,
            )
            .min(max_per_button)
            .max(92.0);

            let new_resp = draw_icon_labeled_button(
                ui,
                [new_button_w, button_h],
                "新建项目",
                crate::egui_ui::theme::UiIcon::Plus,
                interaction_highlight,
                egui::Stroke::NONE,
                button_rounding,
                egui::Color32::WHITE,
                text_font.clone(),
                horizontal_padding,
            );
            if new_resp.clicked() {
                *action = Some(BootstrapAction::NewProject);
            }

            ui.add_space(gap);

            let open_button_w = icon_labeled_button_width(
                ui,
                "打开项目",
                text_font.clone(),
                icon_size,
                horizontal_padding,
            )
            .min(max_per_button)
            .max(92.0);

            let open_resp = draw_icon_labeled_button(
                ui,
                [open_button_w, button_h],
                "打开项目",
                crate::egui_ui::theme::UiIcon::FolderOpen,
                bg_surface_raised,
                egui::Stroke::new(
                    crate::egui_ui::theme::tokens::border_standard(),
                    border_subtle,
                ),
                button_rounding,
                text_primary,
                text_font,
                horizontal_padding,
            );
            if open_resp.clicked() {
                *action = Some(BootstrapAction::OpenProject);
            }
        });

        ui.add_space(crate::egui_ui::theme::tokens::spacing_lg());
        ui.label(
            egui::RichText::new("最近项目")
                .font(crate::egui_ui::theme::typography::metadata())
                .strong()
                .color(text_muted),
        );

        // 统一卡片垂直节奏，避免出现”上紧下松”的视觉错觉。
        let item_card_margin_x = crate::egui_ui::theme::tokens::search_bar_margin_x();
        let item_card_margin_y_loose = crate::egui_ui::theme::tokens::search_bar_margin_y() + 5.0;
        let item_card_margin_y_tight = crate::egui_ui::theme::tokens::search_bar_margin_y() - 1.0;
        let startup_item_card_inner_margin = egui::Margin {
            left: margin_px(item_card_margin_x),
            right: margin_px(item_card_margin_x),
            top: margin_px(item_card_margin_y_loose),
            bottom: margin_px(item_card_margin_y_tight),
        };
        let startup_item_line_gap = 0.0;
        let startup_meta_icon_size = crate::egui_ui::theme::tokens::font_metadata();

        let recent_list_max_h = (content.height() * 0.34).clamp(120.0, 180.0);
        egui::ScrollArea::vertical()
            .id_salt("startup_recent_projects_scroll")
            .auto_shrink([false, false])
            .max_height(recent_list_max_h)
            .show(ui, |ui| {
                let recent_card_width = (ui.available_width() - 4.0).max(220.0);
                let recent_card_inner_width = (recent_card_width - 20.0).max(120.0);

                if recent_items.is_empty() {
                    ui.scope(|ui| {
                        ui.set_min_width(recent_card_width);
                        ui.set_max_width(recent_card_width);
                        egui::Frame::new()
                            .fill(bg_surface_raised)
                            .stroke(egui::Stroke::new(
                                crate::egui_ui::theme::tokens::border_standard(),
                                border_subtle,
                            ))
                            .corner_radius(corner_radius(section_rounding))
                            .inner_margin(startup_item_card_inner_margin)
                            .show(ui, |ui| {
                                ui.set_min_width(recent_card_inner_width);
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new("暂无最近项目")
                                            .font(crate::egui_ui::theme::typography::body_small())
                                            .color(text_muted),
                                    )
                                    .selectable(false),
                                );
                            });
                    });
                    return;
                }

                for item in recent_items {
                    let card = egui::Frame::new()
                        .fill(bg_surface_raised)
                        .stroke(egui::Stroke::new(
                            crate::egui_ui::theme::tokens::border_standard(),
                            border_subtle,
                        ))
                        .corner_radius(corner_radius(list_row_radius.min(section_rounding + 2.0)))
                        .inner_margin(startup_item_card_inner_margin);

                    let response = ui
                        .scope(|ui| {
                            ui.set_min_width(recent_card_width);
                            ui.set_max_width(recent_card_width);
                            card.show(ui, |ui| {
                                ui.set_min_width(recent_card_inner_width);
                                ui.spacing_mut().item_spacing.y = startup_item_line_gap;
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&item.project_name)
                                            .font(crate::egui_ui::theme::typography::body())
                                            .color(text_primary),
                                    )
                                    .truncate()
                                    .selectable(false),
                                );
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x =
                                        crate::egui_ui::theme::tokens::spacing_xs();
                                    let (icon_rect, _) = ui.allocate_exact_size(
                                        egui::vec2(startup_meta_icon_size, startup_meta_icon_size),
                                        egui::Sense::hover(),
                                    );
                                    crate::egui_ui::theme::draw_icon(
                                        ui.painter(),
                                        icon_rect,
                                        crate::egui_ui::theme::UiIcon::Clock,
                                        text_muted,
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(format!(
                                                "{} · {}",
                                                item.last_edited_label, item.project_size_label
                                            ))
                                            .font(crate::egui_ui::theme::typography::metadata())
                                            .color(text_muted),
                                        )
                                        .selectable(false),
                                    );
                                });
                            })
                            .response
                        })
                        .inner
                        .interact(egui::Sense::click())
                        .on_hover_text(&item.project_path);

                    if response.hovered() {
                        ui.painter().rect_stroke(
                            response.rect,
                            corner_radius(list_row_radius.min(section_rounding + 2.0)),
                            egui::Stroke::new(
                                crate::egui_ui::theme::tokens::border_standard(),
                                bg_surface_hover,
                            ),
                            egui::StrokeKind::Inside,
                        );
                    }

                    if response.clicked() {
                        *action = Some(BootstrapAction::OpenRecent(PathBuf::from(
                            &item.project_path,
                        )));
                    }

                    ui.add_space(crate::egui_ui::theme::tokens::spacing_sm() + 2.0);
                }
            });

        if !recovery_items.is_empty() {
            ui.add_space(crate::egui_ui::theme::tokens::spacing_md() + 4.0);
            ui.label(
                egui::RichText::new("崩溃恢复")
                    .font(crate::egui_ui::theme::typography::metadata())
                    .strong()
                    .color(text_muted),
            );

            let recovery_list_max_h = (content.height() * 0.22).clamp(96.0, 140.0);
            egui::ScrollArea::vertical()
                .id_salt("startup_recovery_scroll")
                .auto_shrink([false, false])
                .max_height(recovery_list_max_h)
                .show(ui, |ui| {
                    let recovery_card_width = (ui.available_width() - 4.0).max(220.0);
                    let recovery_card_inner_width = (recovery_card_width - 20.0).max(120.0);

                    for (idx, item) in recovery_items.iter().enumerate() {
                        let card = egui::Frame::new()
                            .fill(bg_surface_raised)
                            .stroke(egui::Stroke::new(
                                crate::egui_ui::theme::tokens::border_standard(),
                                border_subtle,
                            ))
                            .corner_radius(corner_radius(
                                list_row_radius.min(section_rounding + 2.0),
                            ))
                            .inner_margin(startup_item_card_inner_margin);

                        let response = ui
                            .scope(|ui| {
                                ui.set_min_width(recovery_card_width);
                                ui.set_max_width(recovery_card_width);
                                card.show(ui, |ui| {
                                    ui.set_min_width(recovery_card_inner_width);
                                    ui.spacing_mut().item_spacing.y = startup_item_line_gap;
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(&item.project_name)
                                                .font(crate::egui_ui::theme::typography::body())
                                                .color(text_primary),
                                        )
                                        .truncate()
                                        .selectable(false),
                                    );
                                    let detail = if item.total_snapshots > 1 {
                                        format!(
                                            "{} · {} 个恢复点",
                                            item.age_label, item.total_snapshots
                                        )
                                    } else {
                                        item.age_label.clone()
                                    };
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x =
                                            crate::egui_ui::theme::tokens::spacing_xs();
                                        let (icon_rect, _) = ui.allocate_exact_size(
                                            egui::vec2(
                                                startup_meta_icon_size,
                                                startup_meta_icon_size,
                                            ),
                                            egui::Sense::hover(),
                                        );
                                        crate::egui_ui::theme::draw_icon(
                                            ui.painter(),
                                            icon_rect,
                                            crate::egui_ui::theme::UiIcon::Clock,
                                            text_muted,
                                        );
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(detail)
                                                    .font(
                                                        crate::egui_ui::theme::typography::metadata(
                                                        ),
                                                    )
                                                    .color(text_muted),
                                            )
                                            .selectable(false),
                                        );
                                    });
                                })
                                .response
                            })
                            .inner
                            .interact(egui::Sense::click())
                            .on_hover_text(&item.autosave_path);

                        if response.hovered() {
                            ui.painter().rect_stroke(
                                response.rect,
                                corner_radius(list_row_radius.min(section_rounding + 2.0)),
                                egui::Stroke::new(
                                    crate::egui_ui::theme::tokens::border_standard(),
                                    bg_surface_hover,
                                ),
                                egui::StrokeKind::Inside,
                            );
                        }

                        if response.clicked() {
                            *action = Some(BootstrapAction::Recover(idx));
                        }

                        ui.add_space(crate::egui_ui::theme::tokens::spacing_sm() + 2.0);
                    }
                });
        }
    });
}

fn paint_banner_card(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    panel_rounding: f32,
    bg_surface: egui::Color32,
    overlay_fill: egui::Color32,
) {
    let mut painter = ui.painter_at(rect);

    let rounding = egui::CornerRadius {
        nw: panel_rounding.round().clamp(0.0, 255.0) as u8,
        ne: 0,
        sw: panel_rounding.round().clamp(0.0, 255.0) as u8,
        se: 0,
    };
    painter.rect_filled(rect, rounding, bg_surface);

    let image_rect = rect;
    let image = banner_texture(ui.ctx());
    let texture_size = image.size_vec2();
    let scale = (image_rect.width() / texture_size.x).max(image_rect.height() / texture_size.y);
    let draw_size = texture_size * scale;
    let draw_rect = egui::Rect::from_center_size(image_rect.center(), draw_size);
    let old_clip = painter.clip_rect();
    painter.set_clip_rect(image_rect);
    painter.image(
        image.id(),
        draw_rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    painter.set_clip_rect(old_clip);

    // Full-banner overlay mask.
    painter.rect_filled(image_rect, rounding, overlay_fill.gamma_multiply(0.7));

    let overlay_padding = 20.0;
    let logo_size = 22.0;
    let logo_rect = egui::Rect::from_min_size(
        rect.min + egui::vec2(overlay_padding + 2.0, overlay_padding + 7.0),
        egui::vec2(logo_size, logo_size),
    );

    let app_icon = app_icon_texture(ui.ctx());
    painter.image(
        app_icon.id(),
        logo_rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );

    let banner_title_color = egui::Color32::from_rgb(0xF2, 0xF2, 0xF2);
    painter.text(
        egui::pos2(logo_rect.right() + 10.0, logo_rect.center().y + 1.5),
        egui::Align2::LEFT_CENTER,
        "Mondrian",
        egui::FontId::proportional(17.0),
        banner_title_color,
    );

    let slogan_primary = egui::Color32::from_rgb(0xF2, 0xF2, 0xF2);
    let slogan_accent = egui::Color32::from_rgb(0x00, 0x6E, 0xFF);
    let slogan_left = rect.left() + 20.0;
    let slogan_top = rect.top() + rect.height() * 0.42;

    let slogan_main = "重构节奏，";
    let slogan_sub = "帧帧精彩";

    let main_pos = egui::pos2(slogan_left, slogan_top);
    let sub_pos = egui::pos2(slogan_left, slogan_top + 48.0);

    // Two-pass text draws give a subtle faux-bold feel without introducing new fonts.
    painter.text(
        main_pos + egui::vec2(0.85, 0.0),
        egui::Align2::LEFT_TOP,
        slogan_main,
        egui::FontId::proportional(39.0),
        slogan_primary.gamma_multiply(0.92),
    );
    painter.text(
        main_pos,
        egui::Align2::LEFT_TOP,
        slogan_main,
        egui::FontId::proportional(39.0),
        slogan_primary,
    );

    painter.text(
        sub_pos + egui::vec2(0.75, 0.0),
        egui::Align2::LEFT_TOP,
        slogan_sub,
        egui::FontId::proportional(43.0),
        slogan_accent.gamma_multiply(0.92),
    );
    painter.text(
        sub_pos,
        egui::Align2::LEFT_TOP,
        slogan_sub,
        egui::FontId::proportional(43.0),
        slogan_accent,
    );
}

fn banner_texture(ctx: &egui::Context) -> egui::TextureHandle {
    static BANNER_ID: OnceLock<egui::Id> = OnceLock::new();
    let id = *BANNER_ID.get_or_init(|| egui::Id::new("bootstrap_banner_texture"));

    if let Some(texture) = ctx.data(|data| data.get_temp::<egui::TextureHandle>(id)) {
        return texture;
    }

    let bytes = include_bytes!("../../assets/banner.png");
    let decoded = image::load_from_memory(bytes)
        .expect("banner.png should be a valid embedded image")
        .into_rgba8();
    let size = [decoded.width() as usize, decoded.height() as usize];
    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, decoded.as_raw());
    let texture = ctx.load_texture("bootstrap_banner_texture", color_image, Default::default());
    ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
    texture
}

fn app_icon_texture(ctx: &egui::Context) -> egui::TextureHandle {
    static ICON_ID: OnceLock<egui::Id> = OnceLock::new();
    let id = *ICON_ID.get_or_init(|| egui::Id::new("bootstrap_app_icon_texture"));

    if let Some(texture) = ctx.data(|data| data.get_temp::<egui::TextureHandle>(id)) {
        return texture;
    }

    const ICON_SIZE: usize = 64;
    let rgba = crate::product_assets::rasterize_svg_rgba(
        include_str!("../../assets/favicon.svg"),
        ICON_SIZE as u32,
        ICON_SIZE as u32,
    )
    .expect("favicon.svg should be a valid embedded image");
    let color_image =
        egui::ColorImage::from_rgba_unmultiplied([ICON_SIZE, ICON_SIZE], rgba.as_slice());
    let texture = ctx.load_texture(
        "bootstrap_app_icon_texture",
        color_image,
        Default::default(),
    );
    ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
    texture
}

fn draw_icon_labeled_button(
    ui: &mut egui::Ui,
    size: [f32; 2],
    label: &str,
    icon: crate::egui_ui::theme::UiIcon,
    fill: egui::Color32,
    stroke: egui::Stroke,
    rounding: f32,
    text_color: egui::Color32,
    text_font: egui::FontId,
    horizontal_padding: f32,
) -> egui::Response {
    let response = ui.add_sized(
        size,
        egui::Button::new("")
            .fill(fill)
            .stroke(stroke)
            .corner_radius(corner_radius(rounding)),
    );

    let icon_size = crate::egui_ui::theme::tokens::icon_size();
    let text_galley = ui.painter().layout_no_wrap(label.to_owned(), text_font.clone(), text_color);
    let icon_text_gap = 6.0;
    let group_w = icon_size + icon_text_gap + text_galley.size().x;
    let group_left =
        (response.rect.center().x - group_w * 0.5).max(response.rect.left() + horizontal_padding);

    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(group_left + icon_size * 0.5, response.rect.center().y),
        egui::vec2(icon_size, icon_size),
    );
    crate::egui_ui::theme::draw_icon(ui.painter(), icon_rect, icon, text_color);

    ui.painter().text(
        egui::pos2(
            icon_rect.right() + icon_text_gap,
            response.rect.center().y + 0.5,
        ),
        egui::Align2::LEFT_CENTER,
        label,
        text_font,
        text_color,
    );

    response
}

fn icon_labeled_button_width(
    ui: &egui::Ui,
    label: &str,
    text_font: egui::FontId,
    icon_size: f32,
    horizontal_padding: f32,
) -> f32 {
    let text_width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), text_font, egui::Color32::WHITE)
        .size()
        .x
        .max(0.0);
    let icon_text_gap = 6.0;
    icon_size + icon_text_gap + text_width + horizontal_padding * 2.0
}
