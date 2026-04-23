use crate::{
    app::AppState,
    ui::{
        theme::{self, palette, tokens, typography},
        timeline_panel::SelectedClipRef,
    },
};
use egui::{RichText, Ui};
use mondrian_effects::EffectType;

#[derive(Default)]
pub struct EffectLibraryPanel;

impl EffectLibraryPanel {
    pub fn show(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        selected_clip: Option<SelectedClipRef>,
    ) {
        let subtitle = selected_clip
            .and_then(|selection| {
                app.clip_snapshot(selection).map(|clip| {
                    clip.label
                        .as_deref()
                        .filter(|label| !label.is_empty())
                        .unwrap_or(if clip.is_adjustment_layer() {
                            "调整图层"
                        } else {
                            "未命名片段"
                        })
                        .to_string()
                })
            })
            .unwrap_or_else(|| "（未选中片段）".to_string());

        theme::panel_header(ui, "特效库", &subtitle, |_| ());
        ui.add_space(tokens::panel_gap() * 0.65);

        let Some(selection) = selected_clip else {
            ui.label(
                RichText::new("选择一个视频片段或调整图层后，可在这里添加特效")
                    .font(typography::body())
                    .color(palette::text_muted()),
            );
            return;
        };

        if !selection.is_video_track {
            ui.label(
                RichText::new("当前仅视频轨道片段支持可视特效")
                    .font(typography::body())
                    .color(palette::text_muted()),
            );
            return;
        }

        egui::ScrollArea::vertical()
            .id_salt("effect_library_panel_scroll")
            .show(ui, |ui| {
                for effect_type in supported_video_effect_library() {
                    let clicked = ui
                        .add_sized(
                            [ui.available_width(), 34.0],
                            egui::Button::new(effect_type.display_name()),
                        )
                        .clicked();
                    if clicked {
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
                    ui.add_space(6.0);
                }
            });
    }
}

fn supported_video_effect_library() -> Vec<EffectType> {
    vec![
        EffectType::BasicCorrection,
        EffectType::WhiteBalance,
        EffectType::GaussianBlur,
        EffectType::Sharpen,
        EffectType::Vignette,
        EffectType::ChromaticAberration,
        EffectType::Grain,
    ]
}
