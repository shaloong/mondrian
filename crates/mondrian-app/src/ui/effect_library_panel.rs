use crate::{
    app::AppState,
    ui::{
        theme::{self, palette, tokens, typography},
        timeline_panel::SelectedClipRef,
    },
};
use egui::{RichText, Ui};
use mondrian_effects::{effect_category_tree, EffectCategoryNode, EffectType};

#[derive(Default)]
pub struct EffectLibraryPanel;

impl EffectLibraryPanel {
    pub fn show(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        selected_clip: Option<SelectedClipRef>,
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
                    Self::draw_category_node(ui, app, selected_clip, node, 0);
                }
            });
    }

    fn draw_category_node(
        ui: &mut Ui,
        app: &mut AppState,
        selected_clip: Option<SelectedClipRef>,
        node: &EffectCategoryNode,
        depth: usize,
    ) {
        let indent = depth as f32 * tokens::inspector_group_indent();
        let header_id = ui.id().with(&node.name).with("cat_header");
        let mut collapsed = ui
            .memory_mut(|mem| mem.data.get_temp::<bool>(header_id))
            .unwrap_or(false);

        ui.horizontal(|ui| {
            ui.add_space(indent + 4.0);
            let caret = if collapsed {
                theme::UiIcon::ArrowRight
            } else {
                theme::UiIcon::ArrowDown
            };
            theme::icon(ui, caret, palette::text_muted());
            ui.add_space(tokens::spacing_xs());

            let header_resp = ui
                .add_sized(
                    [ui.available_width() - 4.0, tokens::inspector_group_header_height()],
                    egui::Button::selectable(false, RichText::new(&node.name)
                        .font(typography::body_small())
                        .color(palette::text_primary()))
                    .frame(false),
                );
            if header_resp.clicked() {
                collapsed = !collapsed;
                ui.memory_mut(|mem| mem.data.insert_temp(header_id, collapsed));
            }
        });

        if !collapsed {
            for child in &node.children {
                Self::draw_category_node(ui, app, selected_clip, child, depth + 1);
            }
        }
        // Effects always visible under their category
        let item_indent = indent + tokens::inspector_group_indent() + tokens::icon_size() + tokens::spacing_xs();
        for effect_type in &node.effects {
            ui.horizontal(|ui| {
                ui.add_space(item_indent);
                Self::draw_effect_item(ui, app, selected_clip, effect_type);
            });
        }
    }

    fn draw_effect_item(
        ui: &mut Ui,
        app: &mut AppState,
        selected_clip: Option<SelectedClipRef>,
        effect_type: &EffectType,
    ) {
        let desired = egui::vec2(ui.available_width(), tokens::effect_item_height());
        let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click_and_drag());

        if ui.is_rect_visible(rect) {
            let fill = if response.hovered() {
                palette::bg_surface_hover()
            } else {
                palette::bg_surface_raised()
            };
            ui.painter().rect_filled(
                rect,
                theme::corner_radius(tokens::button_rounding()),
                fill,
            );

            let text_pos = egui::pos2(rect.left() + tokens::spacing_sm(), rect.center().y);
            ui.painter().text(
                text_pos,
                egui::Align2::LEFT_CENTER,
                effect_type.display_name(),
                typography::body_small(),
                palette::text_primary(),
            );
        }

        if response.clicked() {
            if let Some(selection) = selected_clip {
                if selection.is_video_track {
                    match app.add_effect_to_clip(selection, effect_type.clone()) {
                        Ok(true) => {
                            app.set_status_hint(
                                format!("已添加{}", effect_type.display_name()),
                                false,
                            );
                        }
                        Ok(false) => {}
                        Err(err) => {
                            app.set_status_hint(format!("添加特效失败：{err}"), true);
                        }
                    }
                }
            } else {
                app.set_status_hint("请先在时间线中选择一个视频片段", false);
            }
        }

        ui.add_space(tokens::spacing_xs());
    }
}
