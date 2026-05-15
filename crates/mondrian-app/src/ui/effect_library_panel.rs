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

        egui::ScrollArea::vertical()
            .id_salt("effect_library_panel_scroll")
            .show(ui, |ui| {
                for node in &tree {
                    self.draw_category(ui, node, 0);
                }
            });
    }

    fn draw_category(
        &mut self,
        ui: &mut Ui,
        node: &EffectCategoryNode,
        depth: usize,
    ) {
        let indent = depth as f32 * tokens::inspector_group_indent();
        let row_h = tokens::inspector_group_header_height();
        let collapsed = self.collapsed_categories.contains(&node.name);

        // Full-width clickable category header
        let available_w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(available_w, row_h),
            egui::Sense::click(),
        );

        if ui.is_rect_visible(rect) {
            if resp.hovered() {
                ui.painter().rect_filled(
                    rect,
                    theme::corner_radius(tokens::section_rounding()),
                    palette::bg_surface_hover(),
                );
            }
            let caret = if collapsed { theme::UiIcon::ArrowRight } else { theme::UiIcon::ArrowDown };
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
            let item_indent = indent + tokens::inspector_group_indent() + tokens::icon_size() + tokens::spacing_xs() + 4.0;
            for effect_type in &node.effects {
                self.draw_effect_item(ui, effect_type, item_indent);
            }
        }
    }

    fn draw_effect_item(
        &self,
        ui: &mut Ui,
        effect_type: &EffectType,
        indent: f32,
    ) {
        let row_h = tokens::effect_item_height();
        let available_w = ui.available_width();
        let desired = egui::vec2(available_w, row_h);

        let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::drag());

        if ui.is_rect_visible(rect) {
            if response.hovered() || response.dragged() {
                ui.painter().rect_filled(
                    rect,
                    theme::corner_radius(tokens::section_rounding()),
                    palette::bg_surface_hover(),
                );
            }

            ui.painter().text(
                egui::pos2(rect.left() + indent, rect.center().y),
                egui::Align2::LEFT_CENTER,
                effect_type.display_name(),
                typography::body_small(),
                palette::text_primary(),
            );
        }

        if response.drag_started() {
            let drag_id = egui::Id::new(EFFECT_DRAG_ID);
            ui.ctx().data_mut(|d| {
                d.insert_persisted(drag_id, effect_type.clone());
            });
        }
    }
}
