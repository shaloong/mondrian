use std::collections::HashMap;

use crate::{
    app::AppState,
    ui::{
        theme::{self, palette, tokens, typography},
        timeline_panel::SelectedClipRef,
    },
};
use egui::{ComboBox, DragValue, Grid, RichText, Sense, Ui, Vec2};
use mondrian_core::{
    automation::{
        timecode_to_ticks, InterpolationType, KeyframeInterpolation, PropertyHost,
        PropertyMutation, PropertyValue,
    },
    types::{ClipId, TimeCode},
};
use mondrian_timeline::clip::Clip;

#[derive(Default)]
pub struct EffectControlsPanel {
    text_edit_buffers: HashMap<(ClipId, String), String>,
}

impl EffectControlsPanel {
    pub fn show(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        selected_clip: Option<SelectedClipRef>,
    ) {
        let Some(selection) = selected_clip else {
            theme::panel_header(ui, "效果控制", "选中一个片段后即可编辑属性", |_| {});
            ui.add_space(tokens::panel_gap());
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("选中一个片段后即可编辑属性")
                        .font(typography::body())
                        .color(palette::text_muted()),
                );
            });
            return;
        };

        let Some(clip) = app.clip_snapshot(selection) else {
            theme::panel_header(ui, "效果控制", "片段已不可用", |_| {});
            ui.add_space(tokens::panel_gap());
            ui.label(
                RichText::new("片段已被移除或不可用")
                    .font(typography::body())
                    .color(palette::status_warning()),
            );
            return;
        };

        let current_time = app.current_time_code().unwrap_or(clip.position);
        let property_bag = match clip.property_bag() {
            Ok(bag) => bag,
            Err(err) => {
                theme::panel_header(ui, "效果控制", "属性读取失败", |_| {});
                ui.add_space(tokens::panel_gap());
                ui.label(
                    RichText::new(format!("读取片段属性失败：{err}"))
                        .font(typography::body())
                        .color(palette::status_error()),
                );
                return;
            }
        };

        let clip_label =
            clip.label.as_deref().filter(|label| !label.is_empty()).unwrap_or("未命名片段");
        let clip_role = if selection.is_video_track {
            "视频"
        } else {
            "音频"
        };
        let subtitle = format!("{} · {}", clip_role, clip_label);

        theme::panel_header(ui, "效果控制", &subtitle, |_| {});
        ui.add_space(tokens::panel_gap());

        let mut motion_properties = Vec::new();
        let mut opacity_properties = Vec::new();
        let mut other_properties = Vec::new();

        for (path, property) in property_bag.iter() {
            match property_section(path) {
                PropertySection::Motion => motion_properties.push((path, property)),
                PropertySection::Opacity => opacity_properties.push((path, property)),
                PropertySection::Other => other_properties.push((path, property)),
            }
        }

        motion_properties.sort_by_key(|(path, _)| property_order(path));
        opacity_properties.sort_by_key(|(path, _)| property_order(path));
        other_properties.sort_by_key(|(path, _)| property_order(path));

        self.draw_property_section(ui, "运动", &motion_properties, app, selection, current_time);
        self.draw_property_section(
            ui,
            "不透明度",
            &opacity_properties,
            app,
            selection,
            current_time,
        );
        if !other_properties.is_empty() {
            self.draw_property_section(ui, "其他", &other_properties, app, selection, current_time);
        }
    }

    fn draw_property_section(
        &mut self,
        ui: &mut Ui,
        title: &str,
        properties: &[(&str, &mondrian_core::automation::AnimatedProperty)],
        app: &mut AppState,
        selection: SelectedClipRef,
        current_time: TimeCode,
    ) {
        if properties.is_empty() {
            return;
        }

        ui.add_space(tokens::panel_gap() * 0.5);
        ui.label(
            RichText::new(title)
                .font(typography::body_small())
                .strong()
                .color(palette::text_muted()),
        );
        ui.add_space(6.0);

        Grid::new(format!("effect_controls_{}_grid", title))
            .num_columns(3)
            .spacing([8.0, 8.0])
            .striped(false)
            .show(ui, |ui| {
                for (path, property) in properties {
                    self.draw_property_row(ui, app, selection, path, property, current_time);
                }
            });

        ui.add_space(tokens::panel_gap() * 0.5);
    }

    fn draw_property_row(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        selection: SelectedClipRef,
        path: &str,
        property: &mondrian_core::automation::AnimatedProperty,
        current_time: TimeCode,
    ) {
        let current_time_ticks = timecode_to_ticks(current_time);
        let current_value = property.evaluate(current_time_ticks);
        let interpolation = self.current_interpolation(property, current_time);
        let animation_enabled = property.is_enabled();
        let is_animated = property.is_animated();

        if property.descriptor.is_animatable {
            let stopwatch_selected = animation_enabled || is_animated;
            if theme::icon_toggle_button(
                ui,
                tokens::timeline_toolbar_button_size(),
                theme::UiIcon::Timer,
                stopwatch_selected,
            )
            .on_hover_text(if animation_enabled {
                "禁用动画（保留关键帧）"
            } else {
                "启用动画并在当前播放头创建首关键帧"
            })
            .clicked()
            {
                let mutation = if animation_enabled {
                    PropertyMutation::DisableAnimation {
                        path: path.to_string(),
                        time: current_time_ticks,
                    }
                } else {
                    PropertyMutation::EnableAnimation {
                        path: path.to_string(),
                        time: current_time_ticks,
                    }
                };
                let _ = app
                    .mutate_clip_property(selection, mutation, "切换动画")
                    .map_err(|err| app.set_status_hint(format!("切换动画失败：{err}"), true));
            }
        } else {
            let _ = ui.allocate_exact_size(
                Vec2::new(
                    tokens::timeline_toolbar_button_size()[0],
                    tokens::timeline_toolbar_button_size()[1],
                ),
                Sense::hover(),
            );
        }

        ui.label(
            RichText::new(&property.descriptor.display_name)
                .font(typography::body_small())
                .color(palette::text_primary()),
        )
        .on_hover_text(path);

        self.draw_property_value_editor(
            ui,
            app,
            selection,
            path,
            &current_value,
            interpolation,
            property.descriptor.is_animatable,
        );
        ui.end_row();
    }

    fn draw_property_value_editor(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        selection: SelectedClipRef,
        path: &str,
        current_value: &PropertyValue,
        interpolation: InterpolationType,
        is_animatable: bool,
    ) {
        match current_value {
            PropertyValue::Bool(value) => {
                let mut edited = *value;
                if ui.checkbox(&mut edited, "").changed() {
                    self.commit_value(
                        app,
                        selection,
                        path,
                        PropertyValue::Bool(edited),
                        interpolation,
                        is_animatable,
                    );
                }
            }
            PropertyValue::Int(value) => {
                let mut edited = *value;
                if ui.add(DragValue::new(&mut edited).speed(1.0)).changed() {
                    self.commit_value(
                        app,
                        selection,
                        path,
                        PropertyValue::Int(edited),
                        interpolation,
                        is_animatable,
                    );
                }
            }
            PropertyValue::Float(value) => {
                let mut edited = *value as f64;
                if ui.add(DragValue::new(&mut edited).speed(0.01)).changed() {
                    self.commit_value(
                        app,
                        selection,
                        path,
                        PropertyValue::Float(edited as f32),
                        interpolation,
                        is_animatable,
                    );
                }
            }
            PropertyValue::Double(value) => {
                let mut edited = *value;
                if ui.add(DragValue::new(&mut edited).speed(0.01)).changed() {
                    self.commit_value(
                        app,
                        selection,
                        path,
                        PropertyValue::Double(edited),
                        interpolation,
                        is_animatable,
                    );
                }
            }
            PropertyValue::Vec2(value) => {
                let mut edited = *value;
                ui.horizontal(|ui| {
                    let x_changed =
                        ui.add(DragValue::new(&mut edited.x).speed(0.05).prefix("X ")).changed();
                    let y_changed =
                        ui.add(DragValue::new(&mut edited.y).speed(0.05).prefix("Y ")).changed();
                    let mut channel_values = Vec::new();
                    if x_changed {
                        channel_values.push((0, edited.x as f64));
                    }
                    if y_changed {
                        channel_values.push((1, edited.y as f64));
                    }
                    if !channel_values.is_empty() {
                        self.commit_channel_values(
                            app,
                            selection,
                            path,
                            &channel_values,
                            interpolation,
                            is_animatable,
                        );
                    }
                });
            }
            PropertyValue::Vec3(value) => {
                let mut edited = *value;
                ui.horizontal(|ui| {
                    let x_changed =
                        ui.add(DragValue::new(&mut edited.x).speed(0.05).prefix("X ")).changed();
                    let y_changed =
                        ui.add(DragValue::new(&mut edited.y).speed(0.05).prefix("Y ")).changed();
                    let z_changed =
                        ui.add(DragValue::new(&mut edited.z).speed(0.05).prefix("Z ")).changed();
                    let mut channel_values = Vec::new();
                    if x_changed {
                        channel_values.push((0, edited.x as f64));
                    }
                    if y_changed {
                        channel_values.push((1, edited.y as f64));
                    }
                    if z_changed {
                        channel_values.push((2, edited.z as f64));
                    }
                    if !channel_values.is_empty() {
                        self.commit_channel_values(
                            app,
                            selection,
                            path,
                            &channel_values,
                            interpolation,
                            is_animatable,
                        );
                    }
                });
            }
            PropertyValue::Vec4(value) => {
                let mut edited = *value;
                ui.horizontal(|ui| {
                    let mut channel_values = Vec::new();
                    for (index, comp) in edited.iter_mut().enumerate() {
                        if ui
                            .add(DragValue::new(comp).speed(0.01).prefix(format!("{} ", index + 1)))
                            .changed()
                        {
                            channel_values.push((index, *comp as f64));
                        }
                    }
                    if !channel_values.is_empty() {
                        self.commit_channel_values(
                            app,
                            selection,
                            path,
                            &channel_values,
                            interpolation,
                            is_animatable,
                        );
                    }
                });
            }
            PropertyValue::Text(value) => {
                if path == Clip::BLEND_MODE_PATH {
                    self.draw_blend_mode_editor(
                        ui,
                        app,
                        selection,
                        path,
                        value,
                        interpolation,
                        is_animatable,
                    );
                    return;
                }

                let key = (selection.clip_id, path.to_string());
                let mut next_text: Option<String> = None;
                {
                    let buffer = self.text_edit_buffers.entry(key).or_insert_with(|| value.clone());
                    if *buffer != *value {
                        *buffer = value.clone();
                    }
                    if ui.text_edit_singleline(buffer).changed() {
                        next_text = Some(buffer.clone());
                    }
                }
                if let Some(text) = next_text {
                    self.commit_value(
                        app,
                        selection,
                        path,
                        PropertyValue::Text(text),
                        interpolation,
                        is_animatable,
                    );
                }
            }
            PropertyValue::Color(value) => {
                ui.label(
                    RichText::new(format!(
                        "r:{:.2} g:{:.2} b:{:.2} a:{:.2}",
                        value.r, value.g, value.b, value.a
                    ))
                    .font(typography::body_small())
                    .color(palette::text_muted()),
                );
            }
        }
    }

    fn draw_blend_mode_editor(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        selection: SelectedClipRef,
        path: &str,
        current_value: &str,
        interpolation: InterpolationType,
        is_animatable: bool,
    ) {
        let mut selected = current_value.to_string();
        ComboBox::from_id_salt((selection.clip_id, path))
            .selected_text(blend_mode_display_label(selected.as_str()))
            .show_ui(ui, |ui| {
                for option in blend_mode_options() {
                    ui.selectable_value(&mut selected, option.value.to_string(), option.label);
                }
            });

        if selected != current_value {
            self.commit_value(
                app,
                selection,
                path,
                PropertyValue::Text(selected),
                interpolation,
                is_animatable,
            );
        }
    }

    fn commit_value(
        &mut self,
        app: &mut AppState,
        selection: SelectedClipRef,
        path: &str,
        value: PropertyValue,
        interpolation: InterpolationType,
        is_animatable: bool,
    ) {
        let mutation = if is_animatable {
            PropertyMutation::WriteValue {
                path: path.to_string(),
                time: timecode_to_ticks(self.current_time(app)),
                value,
                interpolation,
            }
        } else {
            PropertyMutation::SetStaticValue { path: path.to_string(), value }
        };

        let _ = app
            .mutate_clip_property(selection, mutation, "更新属性")
            .map_err(|err| app.set_status_hint(format!("更新属性失败：{err}"), true));
    }

    fn commit_channel_values(
        &mut self,
        app: &mut AppState,
        selection: SelectedClipRef,
        path: &str,
        channel_values: &[(usize, f64)],
        interpolation: InterpolationType,
        is_animatable: bool,
    ) {
        let mutation = if is_animatable {
            PropertyMutation::WriteChannels {
                path: path.to_string(),
                time: timecode_to_ticks(self.current_time(app)),
                channel_values: channel_values.to_vec(),
                interpolation,
            }
        } else {
            return;
        };

        let _ = app
            .mutate_clip_property(selection, mutation, "更新属性通道")
            .map_err(|err| app.set_status_hint(format!("更新属性失败：{err}"), true));
    }

    fn current_time(&self, app: &AppState) -> TimeCode {
        app.current_time_code()
            .unwrap_or_else(|| TimeCode::new(0, mondrian_core::types::Rational::FPS_25))
    }

    fn current_interpolation(
        &self,
        property: &mondrian_core::automation::AnimatedProperty,
        current_time: TimeCode,
    ) -> InterpolationType {
        let current_time_ticks = timecode_to_ticks(current_time);
        let mut fallback = InterpolationType::Linear;
        for keyframe in property.channel(0).map(|channel| channel.keyframes()).unwrap_or(&[]) {
            let interpolation = interpolation_from_handles(keyframe.interp_in, keyframe.interp_out);
            if keyframe.time == current_time_ticks {
                return interpolation;
            }
            if keyframe.time < current_time_ticks {
                fallback = interpolation;
            }
        }
        fallback
    }
}

fn interpolation_from_handles(
    interp_in: KeyframeInterpolation,
    interp_out: KeyframeInterpolation,
) -> InterpolationType {
    if matches!(interp_in, KeyframeInterpolation::Hold)
        || matches!(interp_out, KeyframeInterpolation::Hold)
    {
        InterpolationType::Hold
    } else if matches!(interp_in, KeyframeInterpolation::Linear)
        && matches!(interp_out, KeyframeInterpolation::Linear)
    {
        InterpolationType::Linear
    } else {
        InterpolationType::Bezier
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PropertySection {
    Motion,
    Opacity,
    Other,
}

fn property_section(path: &str) -> PropertySection {
    match path {
        Clip::BLEND_MODE_PATH | mondrian_timeline::clip::Transform2D::OPACITY_PATH => {
            PropertySection::Opacity
        }
        mondrian_timeline::clip::Transform2D::POSITION_PATH
        | mondrian_timeline::clip::Transform2D::SCALE_PATH
        | mondrian_timeline::clip::Transform2D::ROTATION_PATH
        | mondrian_timeline::clip::Transform2D::ANCHOR_POINT_PATH => PropertySection::Motion,
        _ => PropertySection::Other,
    }
}

fn property_order(path: &str) -> usize {
    match path {
        mondrian_timeline::clip::Transform2D::POSITION_PATH => 0,
        mondrian_timeline::clip::Transform2D::SCALE_PATH => 1,
        mondrian_timeline::clip::Transform2D::ROTATION_PATH => 2,
        mondrian_timeline::clip::Transform2D::ANCHOR_POINT_PATH => 3,
        mondrian_timeline::clip::Transform2D::OPACITY_PATH => 0,
        Clip::BLEND_MODE_PATH => 1,
        _ => 100,
    }
}

struct BlendModeOption {
    label: &'static str,
    value: &'static str,
}

fn blend_mode_options() -> &'static [BlendModeOption] {
    static OPTIONS: [BlendModeOption; 15] = [
        BlendModeOption { label: "继承轨道", value: "inherit" },
        BlendModeOption { label: "正常", value: "Normal" },
        BlendModeOption { label: "正片叠底", value: "Multiply" },
        BlendModeOption { label: "滤色", value: "Screen" },
        BlendModeOption { label: "叠加", value: "Overlay" },
        BlendModeOption { label: "变暗", value: "Darken" },
        BlendModeOption { label: "变亮", value: "Lighten" },
        BlendModeOption { label: "颜色减淡", value: "ColorDodge" },
        BlendModeOption { label: "颜色加深", value: "ColorBurn" },
        BlendModeOption { label: "强光", value: "HardLight" },
        BlendModeOption { label: "柔光", value: "SoftLight" },
        BlendModeOption { label: "差值", value: "Difference" },
        BlendModeOption { label: "排除", value: "Exclusion" },
        BlendModeOption { label: "相加", value: "Add" },
        BlendModeOption { label: "相减", value: "Subtract" },
    ];
    &OPTIONS
}

fn blend_mode_display_label(value: &str) -> String {
    match value {
        "inherit" => "继承轨道".to_string(),
        "Normal" => "正常".to_string(),
        "Multiply" => "正片叠底".to_string(),
        "Screen" => "滤色".to_string(),
        "Overlay" => "叠加".to_string(),
        "Darken" => "变暗".to_string(),
        "Lighten" => "变亮".to_string(),
        "ColorDodge" => "颜色减淡".to_string(),
        "ColorBurn" => "颜色加深".to_string(),
        "HardLight" => "强光".to_string(),
        "SoftLight" => "柔光".to_string(),
        "Difference" => "差值".to_string(),
        "Exclusion" => "排除".to_string(),
        "Add" => "相加".to_string(),
        "Subtract" => "相减".to_string(),
        _ => value.to_string(),
    }
}
