use crate::{
    app::AppState,
    ui::{
        theme::{self, palette, tokens, typography},
        timeline_panel::SelectedClipRef,
    },
};
use egui::Ui;
use mondrian_effects::{effect_category_tree, EffectCategoryNode, EffectType};
use std::collections::HashSet;

pub const EFFECT_DRAG_ID: &str = "mondrian_effect_drag";

#[derive(Default)]
pub struct EffectLibraryPanel {
    collapsed_categories: HashSet<String>,
    /// Manual scroll offset (negative = scrolled down). Accumulated from mouse wheel.
    scroll_offset: f32,
}

impl EffectLibraryPanel {
    pub fn show(
        &mut self,
        ui: &mut Ui,
        _app: &mut AppState,
        _selected_clip: Option<SelectedClipRef>,
    ) {
        theme::panel_header(ui, "特效库", "", |_| ());
        ui.add_space(tokens::panel_gap() * 0.65);

        let tree = effect_category_tree();
        if tree.is_empty() {
            super::widgets::empty_state(
                ui,
                Some(theme::UiIcon::Info),
                "暂无可用特效",
                "注册特效后将在此处显示",
            );
            return;
        }

        // Manual scroll area: use mouse wheel, clip content.
        let available_rect = ui.available_rect_before_wrap();
        let clip_rect = available_rect;

        // Accumulate scroll delta
        let scroll_delta = ui.input(|i| i.smooth_scroll_delta).y;
        self.scroll_offset += scroll_delta;

        // Render content into a child Ui that uses a tall desired size,
        // then clip it to the available rect.
        let content_response = ui.scope_builder(egui::UiBuilder::new().max_rect(clip_rect), |ui| {
            ui.set_clip_rect(clip_rect);
            ui.add_space(-self.scroll_offset.max(0.0));
            for node in &tree {
                self.draw_category(ui, node, 0);
            }
        });

        // Clamp scroll after we know content height
        let total_content_h = content_response.response.rect.height();
        let viewport_h = clip_rect.height();
        let max_scroll = (total_content_h - viewport_h).max(0.0);
        self.scroll_offset = self.scroll_offset.clamp(-max_scroll, 0.0);
    }

    fn draw_category(&mut self, ui: &mut Ui, node: &EffectCategoryNode, depth: usize) {
        let indent = depth as f32 * tokens::inspector_group_indent();
        let row_h = tokens::inspector_group_header_height();
        let collapsed = self.collapsed_categories.contains(&node.name);

        // Full-width clickable category header
        let available_w = ui.available_width();
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(available_w, row_h), egui::Sense::click());

        if ui.is_rect_visible(rect) {
            if resp.hovered() {
                ui.painter().rect_filled(
                    rect,
                    theme::corner_radius(tokens::section_rounding()),
                    palette::bg_surface_hover(),
                );
            }
            let caret = if collapsed {
                theme::UiIcon::ArrowRight
            } else {
                theme::UiIcon::ArrowDown
            };
            let caret_x = rect.left() + indent + 4.0;
            let caret_rect = egui::Rect::from_center_size(
                egui::pos2(caret_x + tokens::icon_size() * 0.5, rect.center().y),
                egui::vec2(tokens::icon_size(), tokens::icon_size()),
            );
            theme::draw_icon(ui.painter(), caret_rect, caret, palette::text_muted());

            ui.painter().text(
                egui::pos2(caret_rect.right() + tokens::spacing_xs(), rect.center().y),
                egui::Align2::LEFT_CENTER,
                &node.name,
                typography::body_small(),
                palette::text_primary(),
            );
        }

        if resp.clicked() {
            if collapsed {
                self.collapsed_categories.remove(&node.name);
            } else {
                self.collapsed_categories.insert(node.name.clone());
            }
        }

        if !collapsed {
            for child in &node.children {
                self.draw_category(ui, child, depth + 1);
            }
            let item_indent = indent
                + tokens::inspector_group_indent()
                + tokens::icon_size()
                + tokens::spacing_xs()
                + 4.0;
            for effect_type in &node.effects {
                self.draw_effect_item(ui, effect_type, item_indent);
            }
        }
    }

    fn draw_effect_item(&self, ui: &mut Ui, effect_type: &EffectType, indent: f32) {
        let row_h = tokens::effect_item_height();
        let available_w = ui.available_width();
        let desired = egui::vec2(available_w, row_h);

        // Hover-only sense — no widget-level drag (drag is via raw input below)
        let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::hover());

        if ui.is_rect_visible(rect) {
            let text_color = if response.hovered() {
                palette::text_primary()
            } else {
                palette::text_primary().gamma_multiply(0.85)
            };

            ui.painter().text(
                egui::pos2(rect.left() + indent, rect.center().y),
                egui::Align2::LEFT_CENTER,
                effect_type.display_name(),
                typography::body_small(),
                text_color,
            );
        }

        // Use raw pointer position for mousedown check — widget hover is
        // unreliable during press because hover-only sense loses hover on click.
        let pointer_pos = ui.input(|i| i.pointer.interact_pos());
        let in_rect = pointer_pos.is_some_and(|p| rect.contains(p));
        if ui.input(|i| i.pointer.primary_pressed()) && in_rect {
            ui.ctx().data_mut(|d| {
                d.insert_persisted(egui::Id::new(EFFECT_DRAG_ID), effect_type.clone());
            });
        }
    }
}
