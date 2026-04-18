use std::collections::HashMap;

use crate::{
    app::{AnimationKeyframeSelection, AppState},
    ui::{
        theme::{self, palette, tokens, typography},
        timeline_panel::SelectedClipRef,
    },
};
use egui::{
    Color32, ComboBox, DragValue, Grid, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui, Vec2,
};
use mondrian_core::{
    automation::{
        timecode_to_ticks, BezierHandle, InterpolationType, KeyframeInterpolation, PropertyHost,
        PropertyMutation, PropertyValue, TimeTicks, SUBFRAME_TICKS_PER_FRAME,
    },
    types::{ClipId, TimeCode},
};
use mondrian_timeline::clip::Clip;

#[derive(Default)]
pub struct EffectControlsPanel {
    text_edit_buffers: HashMap<(ClipId, String), String>,
    view: EffectControlsView,
    graph_channel_selection: HashMap<(ClipId, String), usize>,
    graph_handle_drag: Option<GraphHandleDragState>,
    graph_keyframe_drag: Option<GraphKeyframeDragState>,
    graph_marquee_anchor: Option<Pos2>,
    graph_marquee_current: Option<Pos2>,
    graph_marquee_additive: bool,
    pending_clear_animation: Option<PendingClearAnimation>,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum EffectControlsView {
    #[default]
    Inspector,
    Graph,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GraphHandleKind {
    In,
    Out,
}

#[derive(Clone)]
struct GraphHandleDragState {
    clip_id: ClipId,
    path: String,
    time: TimeTicks,
    channel_index: usize,
    kind: GraphHandleKind,
    pointer_pos: Pos2,
}

#[derive(Clone)]
struct GraphKeyframeDragState {
    clip_id: ClipId,
    path: String,
    channel_index: usize,
    start_pointer_pos: Pos2,
    pointer_pos: Pos2,
    anchors: Vec<GraphKeyframeDragAnchor>,
}

#[derive(Clone)]
struct GraphKeyframeDragAnchor {
    time: TimeTicks,
    value: f64,
}

#[derive(Clone)]
struct GraphEditorHandle {
    kind: GraphHandleKind,
    position: Pos2,
}

#[derive(Clone)]
struct GraphKeyframeVisual {
    selection: AnimationKeyframeSelection,
    hit_rect: Rect,
}

#[derive(Clone)]
struct PendingClearAnimation {
    selection: SelectedClipRef,
    path: String,
    display_name: String,
    time: TimeTicks,
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

        theme::panel_header(ui, "效果控制", &subtitle, |ui| {
            ui.selectable_value(&mut self.view, EffectControlsView::Inspector, "属性");
            ui.selectable_value(&mut self.view, EffectControlsView::Graph, "曲线");
        });
        ui.add_space(tokens::panel_gap());

        if app.active_animation_property_path(selection.clip_id).is_none() {
            if let Some((path, _)) =
                property_bag.iter().find(|(_, property)| property.descriptor.is_animatable)
            {
                app.set_active_animation_property(selection.clip_id, path.to_string());
            }
        }

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

        match self.view {
            EffectControlsView::Inspector => {
                self.draw_property_section(
                    ui,
                    "运动",
                    &motion_properties,
                    app,
                    selection,
                    current_time,
                );
                self.draw_property_section(
                    ui,
                    "不透明度",
                    &opacity_properties,
                    app,
                    selection,
                    current_time,
                );
                if !other_properties.is_empty() {
                    self.draw_property_section(
                        ui,
                        "其他",
                        &other_properties,
                        app,
                        selection,
                        current_time,
                    );
                }
            }
            EffectControlsView::Graph => {
                let animatable_properties = motion_properties
                    .iter()
                    .chain(opacity_properties.iter())
                    .chain(other_properties.iter())
                    .filter(|(_, property)| property.descriptor.is_animatable)
                    .map(|(path, property)| (*path, *property))
                    .collect::<Vec<_>>();
                self.draw_graph_editor(
                    ui,
                    app,
                    selection,
                    &clip,
                    current_time,
                    &animatable_properties,
                );
            }
        }

        self.draw_pending_clear_animation_dialog(ui.ctx(), app);
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

    fn draw_graph_editor(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        selection: SelectedClipRef,
        clip: &Clip,
        current_time: TimeCode,
        properties: &[(&str, &mondrian_core::automation::AnimatedProperty)],
    ) {
        if properties.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("当前片段没有可动画属性")
                        .font(typography::body())
                        .color(palette::text_muted()),
                );
            });
            return;
        }

        let mut active_path = app
            .active_animation_property_path(selection.clip_id)
            .map(str::to_owned)
            .filter(|path| properties.iter().any(|(candidate, _)| *candidate == path))
            .unwrap_or_else(|| properties[0].0.to_string());
        if app.active_animation_property_path(selection.clip_id) != Some(active_path.as_str()) {
            app.set_active_animation_property(selection.clip_id, active_path.clone());
        }

        let Some((_, property)) = properties.iter().find(|(path, _)| *path == active_path.as_str())
        else {
            return;
        };

        let channel_count = property.channel_count().max(1);
        let channel_key = (selection.clip_id, active_path.clone());
        let channel_index = self.graph_channel_selection.entry(channel_key.clone()).or_insert(0);
        *channel_index = (*channel_index).min(channel_count.saturating_sub(1));
        let channel_index = *channel_index;

        let selected_keyframes = app
            .selected_animation_keyframes_for_clip(selection.clip_id)
            .into_iter()
            .collect::<Vec<_>>();
        let selected_on_active = selected_keyframes
            .iter()
            .filter(|selected| selected.path == active_path)
            .cloned()
            .collect::<Vec<_>>();

        theme::toolbar_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ComboBox::from_id_salt((selection.clip_id, "graph_property"))
                    .selected_text(property.descriptor.display_name.as_str())
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        for (path, candidate) in properties {
                            if ui
                                .selectable_label(
                                    *path == active_path.as_str(),
                                    &candidate.descriptor.display_name,
                                )
                                .clicked()
                            {
                                active_path = (*path).to_string();
                                app.set_active_animation_property(
                                    selection.clip_id,
                                    active_path.clone(),
                                );
                                self.graph_handle_drag = None;
                                ui.close();
                            }
                        }
                    });

                if property.channel_count() > 1 {
                    for (index, label) in
                        graph_channel_labels(property.static_value()).iter().enumerate()
                    {
                        ui.selectable_value(
                            self.graph_channel_selection
                                .entry(channel_key.clone())
                                .or_insert(channel_index),
                            index,
                            *label,
                        );
                    }
                }
            });
        });
        ui.add_space(tokens::panel_gap() * 0.6);

        let (rect, _response) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), tokens::graph_editor_height()),
            Sense::click_and_drag(),
        );
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, palette::bg_surface());
        painter.rect_stroke(
            rect,
            egui::CornerRadius::same(tokens::section_rounding().round() as u8),
            Stroke::new(1.0, palette::border_subtle()),
            egui::StrokeKind::Inside,
        );

        let plot_rect = rect.shrink2(Vec2::new(12.0, 12.0));
        let Some(channel) = property.channel(channel_index) else {
            return;
        };
        if channel.keyframes().is_empty() {
            painter.text(
                plot_rect.center(),
                egui::Align2::CENTER_CENTER,
                "启用动画并创建关键帧后可编辑曲线",
                typography::body_small(),
                palette::text_muted(),
            );
            return;
        }

        let current_time_ticks = timecode_to_ticks(current_time);
        let (time_min, time_max) = graph_time_range(clip);
        let (value_min, value_max) = graph_value_range(
            property,
            channel_index,
            current_time_ticks,
            time_min,
            time_max,
        );
        let y_labels = [value_max, (value_min + value_max) * 0.5, value_min];
        for row in 0..=4 {
            let t = row as f32 / 4.0;
            let y = egui::lerp(plot_rect.top()..=plot_rect.bottom(), t);
            painter.line_segment(
                [
                    Pos2::new(plot_rect.left(), y),
                    Pos2::new(plot_rect.right(), y),
                ],
                Stroke::new(1.0, palette::border_subtle().gamma_multiply(0.5)),
            );
        }
        for column in 0..=5 {
            let t = column as f32 / 5.0;
            let x = egui::lerp(plot_rect.left()..=plot_rect.right(), t);
            painter.line_segment(
                [
                    Pos2::new(x, plot_rect.top()),
                    Pos2::new(x, plot_rect.bottom()),
                ],
                Stroke::new(1.0, palette::border_subtle().gamma_multiply(0.35)),
            );
        }
        painter.text(
            Pos2::new(plot_rect.left(), plot_rect.top() - 2.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{:.2}", y_labels[0]),
            typography::body_small(),
            palette::text_muted(),
        );
        painter.text(
            Pos2::new(plot_rect.left(), plot_rect.bottom() + 2.0),
            egui::Align2::LEFT_TOP,
            format!("{:.2}", y_labels[2]),
            typography::body_small(),
            palette::text_muted(),
        );

        let preview_map = self.graph_keyframe_drag.as_ref().and_then(|drag| {
            if drag.clip_id == selection.clip_id
                && drag.path == active_path
                && drag.channel_index == channel_index
            {
                Some(graph_drag_preview_map(
                    drag,
                    ui.input(|i| i.pointer.interact_pos()),
                    plot_rect,
                    time_min,
                    time_max,
                    value_min,
                    value_max,
                ))
            } else {
                None
            }
        });
        if let Some(drag) = &mut self.graph_keyframe_drag {
            if drag.clip_id == selection.clip_id
                && drag.path == active_path
                && drag.channel_index == channel_index
            {
                if let Some(pointer_pos) = ui.input(|i| i.pointer.interact_pos()) {
                    drag.pointer_pos = pointer_pos;
                }
            }
        }

        draw_graph_curve(
            &painter,
            plot_rect,
            property,
            channel_index,
            time_min,
            time_max,
            value_min,
            value_max,
            preview_map.as_ref(),
        );

        if current_time_ticks >= time_min && current_time_ticks <= time_max {
            let playhead_x = graph_x_for_time(plot_rect, time_min, time_max, current_time_ticks);
            painter.line_segment(
                [
                    Pos2::new(playhead_x, plot_rect.top()),
                    Pos2::new(playhead_x, plot_rect.bottom()),
                ],
                Stroke::new(1.2, palette::timeline_playhead()),
            );
        }

        let mut active_handle_preview: Option<(GraphHandleKind, Pos2)> = None;
        let mut handle_commit: Option<(KeyframeInterpolation, KeyframeInterpolation)> = None;
        let selected_active_time = if selected_on_active.len() == 1 {
            Some(selected_on_active[0].time)
        } else {
            None
        };
        let mut selected_points = Vec::new();
        let mut handle_points = Vec::new();
        let mut keyframe_visuals = Vec::new();

        let mut graph_drag_commit: Option<(
            Vec<PropertyMutation>,
            Vec<AnimationKeyframeSelection>,
        )> = None;
        let mut hovered_handle = false;
        let mut hovered_keyframe = false;

        for (index, keyframe) in channel.keyframes().iter().enumerate() {
            let selection_item = AnimationKeyframeSelection {
                clip_id: selection.clip_id,
                path: active_path.clone(),
                time: keyframe.time,
            };
            let preview_override =
                preview_map.as_ref().and_then(|map| map.get(&keyframe.time)).copied();
            let point = preview_override.unwrap_or_else(|| {
                graph_point_for_keyframe(
                    plot_rect,
                    time_min,
                    time_max,
                    value_min,
                    value_max,
                    keyframe.time,
                    keyframe.value,
                )
            });
            let selected = app.is_animation_keyframe_selected(&selection_item);
            if selected {
                selected_points.push(point);
            }
            let hit_rect =
                Rect::from_center_size(point, Vec2::splat(tokens::timeline_keyframe_hit_size()));
            let key_response = ui.interact(
                hit_rect,
                ui.make_persistent_id((
                    "graph_keyframe",
                    selection.clip_id,
                    active_path.as_str(),
                    keyframe.time,
                )),
                Sense::click_and_drag(),
            );
            hovered_keyframe |= key_response.hovered();
            keyframe_visuals
                .push(GraphKeyframeVisual { selection: selection_item.clone(), hit_rect });
            if key_response.clicked() {
                if ui.input(|i| i.modifiers.shift) {
                    app.toggle_animation_keyframe_selection(selection_item.clone());
                } else {
                    app.select_animation_keyframe_only(selection_item.clone());
                }
            }
            key_response.context_menu(|ui| {
                if !app.is_animation_keyframe_selected(&selection_item) {
                    app.select_animation_keyframe_only(selection_item.clone());
                }
                if ui.button("复制关键帧").clicked() {
                    let _ = app
                        .copy_selected_animation_keyframes(selection)
                        .map_err(|err| app.set_status_hint(format!("复制关键帧失败：{err}"), true));
                    ui.close();
                }
                if ui
                    .add_enabled(
                        app.has_animation_clipboard(),
                        egui::Button::new("粘贴关键帧"),
                    )
                    .clicked()
                {
                    let _ = app
                        .paste_animation_keyframes(selection, current_time_ticks)
                        .map_err(|err| app.set_status_hint(format!("粘贴关键帧失败：{err}"), true));
                    ui.close();
                }
                if ui.button("删除关键帧").clicked() {
                    self.delete_selected_keyframes(app, selection);
                    ui.close();
                }
                let visible_presets = visible_interpolation_presets();
                let available_presets =
                    app.available_animation_interpolation_presets(selection, &visible_presets);
                if !available_presets.is_empty() {
                    ui.separator();
                }
                for preset in available_presets {
                    if ui.button(interpolation_label(preset)).clicked() {
                        self.apply_interpolation_to_selection(app, selection, preset);
                        ui.close();
                    }
                }
            });
            if key_response.drag_started() && self.graph_handle_drag.is_none() {
                let drag_targets = if app.is_animation_keyframe_selected(&selection_item) {
                    selected_on_active.clone()
                } else {
                    app.select_animation_keyframe_only(selection_item.clone());
                    vec![selection_item.clone()]
                };
                self.graph_keyframe_drag =
                    key_response.interact_pointer_pos().map(|start_pointer_pos| {
                        GraphKeyframeDragState {
                            clip_id: selection.clip_id,
                            path: active_path.clone(),
                            channel_index,
                            start_pointer_pos,
                            pointer_pos: start_pointer_pos,
                            anchors: drag_targets
                                .into_iter()
                                .filter_map(|selected| {
                                    property.keyframe_at(selected.time).and_then(|keyframe| {
                                        keyframe
                                            .value
                                            .to_channel_values()
                                            .get(channel_index)
                                            .copied()
                                            .map(|value| GraphKeyframeDragAnchor {
                                                time: selected.time,
                                                value,
                                            })
                                    })
                                })
                                .collect(),
                        }
                    });
            }
            painter.add(Shape::circle_filled(
                point,
                tokens::timeline_keyframe_size() * 0.5,
                if selected {
                    palette::interaction_highlight()
                } else {
                    graph_channel_color(channel_index).gamma_multiply(0.9)
                },
            ));
            painter.circle_stroke(
                point,
                tokens::timeline_keyframe_size() * 0.5,
                Stroke::new(
                    1.0,
                    if selected {
                        palette::text_primary()
                    } else {
                        palette::bg_surface()
                    },
                ),
            );

            if selected_active_time == Some(keyframe.time) {
                let handles = graph_handles_for_keyframe(
                    channel.keyframes(),
                    index,
                    plot_rect,
                    time_min,
                    time_max,
                    value_min,
                    value_max,
                );
                for handle in handles {
                    let mut handle_position = handle.position;
                    if let Some(drag) = &mut self.graph_handle_drag {
                        if drag.clip_id == selection.clip_id
                            && drag.path == active_path
                            && drag.time == keyframe.time
                            && drag.channel_index == channel_index
                            && drag.kind == handle.kind
                        {
                            if let Some(pointer_pos) = ui.input(|i| i.pointer.interact_pos()) {
                                drag.pointer_pos = pointer_pos;
                            }
                            if let Some(preview) = preview_handle_position(
                                handle.kind,
                                drag.pointer_pos,
                                channel.keyframes(),
                                index,
                                plot_rect,
                                time_min,
                                time_max,
                                value_min,
                                value_max,
                            ) {
                                handle_position = preview;
                                active_handle_preview = Some((handle.kind, preview));
                            }
                            if ui.input(|i| i.pointer.any_released()) {
                                handle_commit = compute_handle_interpolation(
                                    handle.kind,
                                    drag.pointer_pos,
                                    channel.keyframes(),
                                    index,
                                    plot_rect,
                                    time_min,
                                    time_max,
                                    value_min,
                                    value_max,
                                );
                            }
                        }
                    }

                    painter.line_segment(
                        [point, handle_position],
                        Stroke::new(1.0, graph_channel_color(channel_index).gamma_multiply(0.7)),
                    );
                    let handle_rect = Rect::from_center_size(
                        handle_position,
                        Vec2::splat(tokens::graph_editor_handle_size() * 2.0),
                    );
                    let handle_response = ui.interact(
                        handle_rect,
                        ui.make_persistent_id((
                            "graph_handle",
                            selection.clip_id,
                            active_path.as_str(),
                            keyframe.time,
                            channel_index,
                            matches!(handle.kind, GraphHandleKind::In),
                        )),
                        Sense::click_and_drag(),
                    );
                    hovered_handle |= handle_response.hovered();
                    handle_points.push(handle_position);
                    if handle_response.drag_started() {
                        self.graph_handle_drag =
                            handle_response.interact_pointer_pos().map(|pointer_pos| {
                                GraphHandleDragState {
                                    clip_id: selection.clip_id,
                                    path: active_path.clone(),
                                    time: keyframe.time,
                                    channel_index,
                                    kind: handle.kind,
                                    pointer_pos,
                                }
                            });
                    }
                    painter.add(Shape::circle_filled(
                        handle_position,
                        tokens::graph_editor_handle_size() * 0.5,
                        palette::bg_surface_hover(),
                    ));
                    painter.circle_stroke(
                        handle_position,
                        tokens::graph_editor_handle_size() * 0.5,
                        Stroke::new(1.0, palette::interaction_highlight()),
                    );
                }
            }
        }

        if let Some(drag) = &self.graph_keyframe_drag {
            if drag.clip_id == selection.clip_id
                && drag.path == active_path
                && drag.channel_index == channel_index
                && ui.input(|i| i.pointer.any_released())
            {
                graph_drag_commit = graph_keyframe_drag_mutations(
                    drag,
                    plot_rect,
                    time_min,
                    time_max,
                    value_min,
                    value_max,
                    selection.clip_id,
                );
            }
        }

        if let Some((interp_in, interp_out)) = handle_commit {
            let drag = self.graph_handle_drag.take();
            if let Some(drag) = drag {
                let _ = app
                    .mutate_clip_property(
                        selection,
                        PropertyMutation::UpdateChannelKeyframeHandles {
                            path: drag.path,
                            time: drag.time,
                            channel_index: drag.channel_index,
                            interp_in,
                            interp_out,
                        },
                        "更新曲线手柄",
                    )
                    .map_err(|err| app.set_status_hint(format!("更新曲线手柄失败：{err}"), true));
            }
        }
        if let Some((mutations, selections)) = graph_drag_commit {
            self.graph_keyframe_drag = None;
            let _ = app
                .mutate_clip_properties(selection, mutations, "图形编辑关键帧")
                .map(|_| app.set_animation_keyframe_selection(selections))
                .map_err(|err| app.set_status_hint(format!("图形编辑关键帧失败：{err}"), true));
        } else if ui.input(|i| i.pointer.any_released()) {
            self.graph_keyframe_drag = None;
        }

        if let Some((_, preview)) = active_handle_preview {
            painter.circle_filled(
                preview,
                tokens::graph_editor_handle_size() * 0.45,
                palette::interaction_highlight(),
            );
        }

        self.handle_graph_marquee(
            ui,
            app,
            selection,
            active_path.clone(),
            plot_rect,
            time_min,
            time_max,
            &keyframe_visuals,
            hovered_keyframe || hovered_handle,
        );

        self.draw_graph_selection_bubble(
            ui.ctx(),
            selection,
            current_time_ticks,
            &selected_points,
            &handle_points,
            plot_rect,
            app,
        );
    }

    fn apply_interpolation_to_selection(
        &mut self,
        app: &mut AppState,
        selection: SelectedClipRef,
        interpolation: InterpolationType,
    ) {
        let mutations = app
            .selected_animation_keyframes_for_clip(selection.clip_id)
            .into_iter()
            .map(|selected| PropertyMutation::UpdateKeyframeInterpolation {
                path: selected.path.clone(),
                time: selected.time,
                interpolation,
            })
            .collect::<Vec<_>>();
        let _ = app
            .mutate_clip_properties(selection, mutations, "更新关键帧插值")
            .map_err(|err| app.set_status_hint(format!("更新关键帧插值失败：{err}"), true));
    }

    fn delete_selected_keyframes(&mut self, app: &mut AppState, selection: SelectedClipRef) {
        let selected = app.selected_animation_keyframes_for_clip(selection.clip_id);
        if selected.is_empty() {
            return;
        }
        let mutations = selected
            .iter()
            .map(|selected| PropertyMutation::RemoveKeyframe {
                path: selected.path.clone(),
                time: selected.time,
            })
            .collect::<Vec<_>>();
        let _ = app
            .mutate_clip_properties(selection, mutations, "删除关键帧")
            .map(|_| app.clear_animation_keyframe_selection_for_clip(selection.clip_id))
            .map_err(|err| app.set_status_hint(format!("删除关键帧失败：{err}"), true));
    }

    fn draw_graph_selection_bubble(
        &mut self,
        ctx: &egui::Context,
        selection: SelectedClipRef,
        current_time_ticks: TimeTicks,
        selected_points: &[Pos2],
        handle_points: &[Pos2],
        plot_rect: Rect,
        app: &mut AppState,
    ) {
        if selected_points.is_empty()
            || self.graph_handle_drag.is_some()
            || self.graph_keyframe_drag.is_some()
        {
            return;
        }

        let centroid = selected_points.iter().fold(Pos2::ZERO, |acc, point| {
            Pos2::new(acc.x + point.x, acc.y + point.y)
        });
        let centroid = Pos2::new(
            centroid.x / selected_points.len() as f32,
            centroid.y / selected_points.len() as f32,
        );
        let selected_bounds = selected_points.iter().fold(
            Rect::from_center_size(centroid, Vec2::ZERO),
            |acc, point| {
                Rect::from_min_max(
                    Pos2::new(acc.min.x.min(point.x), acc.min.y.min(point.y)),
                    Pos2::new(acc.max.x.max(point.x), acc.max.y.max(point.y)),
                )
            },
        );
        let avoid_bounds = handle_points.iter().copied().fold(selected_bounds, |acc, point| {
            Rect::from_min_max(
                Pos2::new(acc.min.x.min(point.x), acc.min.y.min(point.y)),
                Pos2::new(acc.max.x.max(point.x), acc.max.y.max(point.y)),
            )
        });
        let visible_presets = visible_interpolation_presets();
        let available_presets =
            app.available_animation_interpolation_presets(selection, &visible_presets);
        let bubble_pos = floating_toolbar_position(
            selected_bounds,
            avoid_bounds,
            plot_rect.expand2(Vec2::new(16.0, 16.0)),
            2 + usize::from(app.has_animation_clipboard()) + 1,
            available_presets.len(),
        );

        egui::Area::new(egui::Id::new(("graph_keyframe_bubble", selection.clip_id)))
            .order(egui::Order::Tooltip)
            .fixed_pos(bubble_pos)
            .show(ctx, |ui| {
                theme::toolbar_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let copy = theme::icon_button(
                            ui,
                            tokens::timeline_toolbar_button_size(),
                            theme::UiIcon::Copy,
                        )
                        .on_hover_text("复制关键帧");
                        if copy.clicked() {
                            let _ =
                                app.copy_selected_animation_keyframes(selection).map_err(|err| {
                                    app.set_status_hint(format!("复制关键帧失败：{err}"), true)
                                });
                        }

                        let paste = ui.add_enabled_ui(app.has_animation_clipboard(), |ui| {
                            theme::icon_button(
                                ui,
                                tokens::timeline_toolbar_button_size(),
                                theme::UiIcon::ClipboardText,
                            )
                        });
                        if paste.inner.clicked() {
                            let _ = app
                                .paste_animation_keyframes(selection, current_time_ticks)
                                .map_err(|err| {
                                    app.set_status_hint(format!("粘贴关键帧失败：{err}"), true)
                                });
                        }

                        let delete = theme::icon_button(
                            ui,
                            tokens::timeline_toolbar_button_size(),
                            theme::UiIcon::Trash,
                        )
                        .on_hover_text("删除关键帧");
                        if delete.clicked() {
                            self.delete_selected_keyframes(app, selection);
                        }

                        if !available_presets.is_empty() {
                            ui.separator();
                        }
                        for preset in available_presets {
                            if ui.small_button(interpolation_label(preset)).clicked() {
                                self.apply_interpolation_to_selection(app, selection, preset);
                            }
                        }
                    });
                });
            });
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_graph_marquee(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        selection: SelectedClipRef,
        active_path: String,
        bounds: Rect,
        time_min: TimeTicks,
        time_max: TimeTicks,
        visuals: &[GraphKeyframeVisual],
        pointer_on_anchor: bool,
    ) {
        if self.graph_handle_drag.is_some() || self.graph_keyframe_drag.is_some() {
            self.clear_graph_marquee();
            return;
        }

        let pointer_pos = ui.input(|i| i.pointer.interact_pos());
        let primary_pressed = ui.input(|i| i.pointer.primary_pressed());
        let primary_down = ui.input(|i| i.pointer.primary_down());
        let primary_released = ui.input(|i| i.pointer.primary_released());

        if primary_pressed {
            if let Some(pos) = pointer_pos {
                if bounds.contains(pos) && !pointer_on_anchor {
                    self.graph_marquee_anchor = Some(pos);
                    self.graph_marquee_current = Some(pos);
                    self.graph_marquee_additive = ui.input(|i| i.modifiers.shift);
                    app.set_active_animation_property(selection.clip_id, active_path);
                }
            }
        }

        if primary_down && self.graph_marquee_anchor.is_some() {
            if let Some(pos) = pointer_pos {
                self.graph_marquee_current = Some(pos);
            }
        }

        if let (Some(anchor), Some(current)) =
            (self.graph_marquee_anchor, self.graph_marquee_current)
        {
            let rect = Rect::from_two_pos(anchor, current).intersect(bounds);
            if rect.width() > 2.0 && rect.height() > 2.0 {
                ui.painter().rect_filled(
                    rect,
                    2.0,
                    palette::interaction_highlight().gamma_multiply(0.16),
                );
                ui.painter().rect_stroke(
                    rect,
                    egui::CornerRadius::same(2),
                    Stroke::new(1.2, palette::interaction_highlight()),
                    egui::StrokeKind::Inside,
                );
            }
        }

        if primary_released {
            if let (Some(anchor), Some(current)) =
                (self.graph_marquee_anchor, self.graph_marquee_current)
            {
                let rect = Rect::from_two_pos(anchor, current).intersect(bounds);
                let picks = visuals
                    .iter()
                    .filter(|visual| visual.hit_rect.intersects(rect))
                    .map(|visual| visual.selection.clone())
                    .collect::<Vec<_>>();

                if rect.width() > 2.0 && rect.height() > 2.0 {
                    if self.graph_marquee_additive {
                        let mut combined = app
                            .selected_animation_keyframes_for_clip(selection.clip_id)
                            .into_iter()
                            .collect::<std::collections::HashSet<_>>();
                        combined.extend(picks);
                        app.set_animation_keyframe_selection(combined.into_iter().collect());
                    } else if picks.is_empty() {
                        app.clear_animation_keyframe_selection_for_clip(selection.clip_id);
                    } else {
                        app.set_animation_keyframe_selection(picks);
                    }
                } else if bounds.contains(current) && !pointer_on_anchor {
                    let had_selection =
                        !app.selected_animation_keyframes_for_clip(selection.clip_id).is_empty();
                    if !self.graph_marquee_additive {
                        app.clear_animation_keyframe_selection_for_clip(selection.clip_id);
                        if !had_selection {
                            let frame = (graph_time_from_x(bounds, time_min, time_max, current.x)
                                as f64
                                / SUBFRAME_TICKS_PER_FRAME as f64)
                                .round() as i64;
                            app.seek(frame.max(0));
                        }
                    }
                }
            }
            self.clear_graph_marquee();
        }
    }

    fn clear_graph_marquee(&mut self) {
        self.graph_marquee_anchor = None;
        self.graph_marquee_current = None;
        self.graph_marquee_additive = false;
    }

    fn draw_pending_clear_animation_dialog(&mut self, ctx: &egui::Context, app: &mut AppState) {
        let Some(pending) = self.pending_clear_animation.clone() else {
            return;
        };

        let mut open = true;
        egui::Window::new("关闭动画")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .frame(theme::dialog_frame())
            .show(ctx, |ui| {
                ui.set_min_width(320.0);
                ui.label(
                    RichText::new(format!(
                        "关闭“{}”动画会删除该属性在当前片段上的全部关键帧。",
                        pending.display_name
                    ))
                    .font(typography::body())
                    .color(palette::text_primary()),
                );
                ui.add_space(tokens::panel_gap() * 0.6);
                ui.label(
                    RichText::new(
                        "该操作不可恢复为原关键帧，保留的静态值会取当前播放头时刻的属性值。",
                    )
                    .font(typography::body_small())
                    .color(palette::text_muted()),
                );
                ui.add_space(tokens::panel_gap());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("删除关键帧并关闭").clicked() {
                        let _ = app
                            .mutate_clip_property(
                                pending.selection,
                                PropertyMutation::ClearAnimation {
                                    path: pending.path.clone(),
                                    time: pending.time,
                                },
                                "关闭动画",
                            )
                            .map_err(|err| {
                                app.set_status_hint(format!("关闭动画失败：{err}"), true)
                            });
                        self.pending_clear_animation = None;
                    }
                    if ui.button("取消").clicked() {
                        self.pending_clear_animation = None;
                    }
                });
            });
        if !open {
            self.pending_clear_animation = None;
        }
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
        let current_keyframe_time = self.current_keyframe_time(property, current_time_ticks);
        let is_active_property =
            app.active_animation_property_path(selection.clip_id) == Some(path);

        ui.horizontal(|ui| {
            if property.descriptor.is_animatable {
                let stopwatch_selected = animation_enabled || is_animated;
                let timer_response = theme::icon_toggle_button(
                    ui,
                    tokens::timeline_toolbar_button_size(),
                    theme::UiIcon::Timer,
                    stopwatch_selected,
                );
                let timer_response = timer_response.on_hover_text(if animation_enabled {
                    "关闭动画并删除该属性全部关键帧"
                } else {
                    "启用动画并在当前播放头创建首关键帧"
                });
                if timer_response.clicked() {
                    app.set_active_animation_property(selection.clip_id, path.to_string());
                    if animation_enabled {
                        self.pending_clear_animation = Some(PendingClearAnimation {
                            selection,
                            path: path.to_string(),
                            display_name: property.descriptor.display_name.clone(),
                            time: current_time_ticks,
                        });
                    } else {
                        let mutation = PropertyMutation::EnableAnimation {
                            path: path.to_string(),
                            time: current_time_ticks,
                        };
                        let _ = app.mutate_clip_property(selection, mutation, "启用动画").map_err(
                            |err| app.set_status_hint(format!("启用动画失败：{err}"), true),
                        );
                    }
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
        });

        let label_response = ui
            .selectable_label(
                is_active_property,
                RichText::new(&property.descriptor.display_name)
                    .font(typography::body_small())
                    .color(if is_active_property {
                        palette::text_primary()
                    } else {
                        palette::text_muted()
                    }),
            )
            .on_hover_text(path);
        if label_response.clicked() {
            app.set_active_animation_property(selection.clip_id, path.to_string());
        }

        self.draw_property_value_editor(
            ui,
            app,
            selection,
            path,
            &current_value,
            interpolation,
            property.descriptor.is_animatable,
        );

        if property.descriptor.is_animatable {
            let add_selected = current_keyframe_time.is_some();
            let add_resp = theme::icon_toggle_button(
                ui,
                tokens::timeline_toolbar_button_size(),
                theme::UiIcon::Anchor,
                add_selected,
            )
            .on_hover_text(if add_selected {
                "删除当前关键帧"
            } else {
                "在当前播放头添加关键帧"
            });
            if add_resp.clicked() {
                app.set_active_animation_property(selection.clip_id, path.to_string());
                let mutation = if add_selected {
                    PropertyMutation::RemoveKeyframe {
                        path: path.to_string(),
                        time: current_time_ticks,
                    }
                } else {
                    PropertyMutation::WriteValue {
                        path: path.to_string(),
                        time: current_time_ticks,
                        value: current_value.clone(),
                        interpolation,
                    }
                };
                let description = if add_selected {
                    "删除关键帧"
                } else {
                    "添加关键帧"
                };
                let _ = app
                    .mutate_clip_property(selection, mutation, description)
                    .map_err(|err| app.set_status_hint(format!("{description}失败：{err}"), true));
            }
        } else {
            ui.label("");
        }
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
        app.set_active_animation_property(selection.clip_id, path.to_string());
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
        app.set_active_animation_property(selection.clip_id, path.to_string());
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

    fn current_keyframe_time(
        &self,
        property: &mondrian_core::automation::AnimatedProperty,
        current_time_ticks: TimeTicks,
    ) -> Option<TimeTicks> {
        property.keyframe_times().into_iter().find(|time| *time == current_time_ticks)
    }
}

fn visible_interpolation_presets() -> [InterpolationType; 4] {
    [
        InterpolationType::Linear,
        InterpolationType::EaseIn,
        InterpolationType::EaseOut,
        InterpolationType::EaseInOut,
    ]
}

fn interpolation_label(interpolation: InterpolationType) -> &'static str {
    match interpolation {
        InterpolationType::Hold => "Hold",
        InterpolationType::Linear => "Linear",
        InterpolationType::Bezier => "Bezier",
        InterpolationType::EaseIn => "Ease In",
        InterpolationType::EaseOut => "Ease Out",
        InterpolationType::EaseInOut => "Ease InOut",
    }
}

fn interpolation_from_handles(
    interp_in: KeyframeInterpolation,
    interp_out: KeyframeInterpolation,
) -> InterpolationType {
    const EPSILON: f64 = 1e-6;

    let approx_handle =
        |handle: KeyframeInterpolation, time_offset: f64, value_offset: f64| match handle {
            KeyframeInterpolation::Bezier(handle) => {
                (handle.time_offset - time_offset).abs() <= EPSILON
                    && (handle.value_offset - value_offset).abs() <= EPSILON
            }
            _ => false,
        };

    if matches!(interp_in, KeyframeInterpolation::Hold)
        || matches!(interp_out, KeyframeInterpolation::Hold)
    {
        InterpolationType::Hold
    } else if matches!(interp_in, KeyframeInterpolation::Linear)
        && matches!(interp_out, KeyframeInterpolation::Linear)
    {
        InterpolationType::Linear
    } else if matches!(interp_in, KeyframeInterpolation::Linear)
        && approx_handle(interp_out, 1.0 / 3.0, 0.0)
    {
        InterpolationType::EaseIn
    } else if approx_handle(interp_in, -1.0 / 3.0, 0.0)
        && matches!(interp_out, KeyframeInterpolation::Linear)
    {
        InterpolationType::EaseOut
    } else if approx_handle(interp_in, -1.0 / 3.0, 0.0) && approx_handle(interp_out, 1.0 / 3.0, 0.0)
    {
        InterpolationType::EaseInOut
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

fn graph_channel_labels(value: &PropertyValue) -> &'static [&'static str] {
    match value {
        PropertyValue::Vec2(_) => &["X", "Y"],
        PropertyValue::Vec3(_) => &["X", "Y", "Z"],
        PropertyValue::Color(_) => &["R", "G", "B", "A"],
        PropertyValue::Vec4(_) => &["1", "2", "3", "4"],
        _ => &["值"],
    }
}

fn graph_channel_color(index: usize) -> Color32 {
    match index {
        0 => palette::interaction_highlight(),
        1 => palette::accent_audio(),
        2 => palette::status_warning(),
        3 => palette::status_success(),
        _ => palette::text_primary(),
    }
}

fn graph_time_range(clip: &Clip) -> (TimeTicks, TimeTicks) {
    let start = timecode_to_ticks(clip.position);
    let end = timecode_to_ticks(clip.end_position());
    if start == end {
        let pad = SUBFRAME_TICKS_PER_FRAME * 2;
        (start.saturating_sub(pad), end + pad)
    } else {
        (start, end)
    }
}

fn graph_value_range(
    property: &mondrian_core::automation::AnimatedProperty,
    channel_index: usize,
    current_time: TimeTicks,
    time_min: TimeTicks,
    time_max: TimeTicks,
) -> (f64, f64) {
    let mut values = property
        .channel(channel_index)
        .map(|channel| {
            channel.keyframes().iter().map(|keyframe| keyframe.value).collect::<Vec<_>>()
        })
        .unwrap_or_default();
    values.push(
        property
            .evaluate(time_min)
            .to_channel_values()
            .get(channel_index)
            .copied()
            .unwrap_or(0.0),
    );
    values.push(
        property
            .evaluate(time_max)
            .to_channel_values()
            .get(channel_index)
            .copied()
            .unwrap_or(0.0),
    );
    values.push(
        property
            .evaluate(current_time)
            .to_channel_values()
            .get(channel_index)
            .copied()
            .unwrap_or(0.0),
    );

    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if !min.is_finite() || !max.is_finite() {
        return (-1.0, 1.0);
    }
    if (max - min).abs() < f64::EPSILON {
        let pad = max.abs().max(1.0) * 0.25;
        (min - pad, max + pad)
    } else {
        let pad = (max - min) * 0.12;
        (min - pad, max + pad)
    }
}

fn graph_x_for_time(rect: Rect, time_min: TimeTicks, time_max: TimeTicks, time: TimeTicks) -> f32 {
    if time_max <= time_min {
        return rect.left();
    }
    let t = ((time - time_min) as f32 / (time_max - time_min) as f32).clamp(0.0, 1.0);
    egui::lerp(rect.left()..=rect.right(), t)
}

fn graph_y_for_value(rect: Rect, value_min: f64, value_max: f64, value: f64) -> f32 {
    if (value_max - value_min).abs() < f64::EPSILON {
        return rect.center().y;
    }
    let t = ((value - value_min) / (value_max - value_min)).clamp(0.0, 1.0) as f32;
    egui::lerp(rect.bottom()..=rect.top(), t)
}

fn graph_point_for_keyframe(
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
    time: TimeTicks,
    value: f64,
) -> Pos2 {
    Pos2::new(
        graph_x_for_time(rect, time_min, time_max, time),
        graph_y_for_value(rect, value_min, value_max, value),
    )
}

fn floating_toolbar_position(
    selected_bounds: Rect,
    avoid_bounds: Rect,
    container_rect: Rect,
    icon_count: usize,
    preset_count: usize,
) -> Pos2 {
    let button_size = tokens::timeline_toolbar_button_size();
    let estimated_width = 18.0
        + icon_count as f32 * button_size[0]
        + preset_count as f32 * 64.0
        + if preset_count > 0 { 16.0 } else { 0.0 };
    let estimated_height = button_size[1] + 12.0;
    let gap = 10.0;
    let x = (selected_bounds.center().x - estimated_width * 0.5).clamp(
        container_rect.left() + 8.0,
        (container_rect.right() - estimated_width - 8.0).max(container_rect.left() + 8.0),
    );

    let above = Rect::from_min_size(
        Pos2::new(x, selected_bounds.top() - estimated_height - gap),
        Vec2::new(estimated_width, estimated_height),
    );
    let below = Rect::from_min_size(
        Pos2::new(x, selected_bounds.bottom() + gap),
        Vec2::new(estimated_width, estimated_height),
    );
    let avoid = avoid_bounds.expand(8.0);
    let fits_above = above.top() >= container_rect.top() + 4.0 && !above.intersects(avoid);
    let fits_below = below.bottom() <= container_rect.bottom() - 4.0 && !below.intersects(avoid);

    if fits_above {
        above.min
    } else if fits_below {
        below.min
    } else if selected_bounds.top() - container_rect.top()
        >= container_rect.bottom() - selected_bounds.bottom()
    {
        above.min
    } else {
        below.min
    }
}

fn draw_graph_curve(
    painter: &egui::Painter,
    rect: Rect,
    property: &mondrian_core::automation::AnimatedProperty,
    channel_index: usize,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
    preview_map: Option<&HashMap<TimeTicks, Pos2>>,
) {
    let samples = 96usize;
    let mut points = Vec::with_capacity(samples + 1);
    for sample in 0..=samples {
        let t = sample as f32 / samples as f32;
        let time = time_min + ((time_max - time_min) as f32 * t) as i64;
        let value = property
            .evaluate(time)
            .to_channel_values()
            .get(channel_index)
            .copied()
            .unwrap_or(0.0);
        points.push(
            preview_map.and_then(|map| map.get(&time).copied()).unwrap_or_else(|| {
                graph_point_for_keyframe(
                    rect, time_min, time_max, value_min, value_max, time, value,
                )
            }),
        );
    }
    painter.add(Shape::line(
        points,
        Stroke::new(
            tokens::graph_editor_curve_stroke_width(),
            graph_channel_color(channel_index),
        ),
    ));
}

fn graph_time_from_x(rect: Rect, time_min: TimeTicks, time_max: TimeTicks, x: f32) -> TimeTicks {
    if time_max <= time_min || rect.width() <= 1.0 {
        return time_min;
    }
    let t = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
    time_min + ((time_max - time_min) as f32 * t).round() as i64
}

fn graph_drag_preview_map(
    drag: &GraphKeyframeDragState,
    pointer_pos: Option<Pos2>,
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
) -> HashMap<TimeTicks, Pos2> {
    let mut map = HashMap::new();
    let Some(pointer_pos) = pointer_pos else {
        return map;
    };
    let delta_time = graph_time_from_x(rect, time_min, time_max, pointer_pos.x)
        - graph_time_from_x(rect, time_min, time_max, drag.start_pointer_pos.x);
    let delta_time = snap_graph_delta_ticks(delta_time);
    let delta_value = graph_value_from_y(rect, value_min, value_max, pointer_pos.y)
        - graph_value_from_y(rect, value_min, value_max, drag.start_pointer_pos.y);
    for anchor in &drag.anchors {
        let time = (anchor.time + delta_time).max(0);
        let value = anchor.value + delta_value;
        map.insert(
            anchor.time,
            graph_point_for_keyframe(rect, time_min, time_max, value_min, value_max, time, value),
        );
    }
    map
}

fn graph_keyframe_drag_mutations(
    drag: &GraphKeyframeDragState,
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
    clip_id: ClipId,
) -> Option<(Vec<PropertyMutation>, Vec<AnimationKeyframeSelection>)> {
    let delta_time = graph_time_from_x(rect, time_min, time_max, drag.pointer_pos.x)
        - graph_time_from_x(rect, time_min, time_max, drag.start_pointer_pos.x);
    let delta_time = snap_graph_delta_ticks(delta_time);
    let delta_value = graph_value_from_y(rect, value_min, value_max, drag.pointer_pos.y)
        - graph_value_from_y(rect, value_min, value_max, drag.start_pointer_pos.y);

    if delta_time == 0 && delta_value.abs() < f64::EPSILON {
        return None;
    }

    let mut anchors = drag.anchors.clone();
    anchors.sort_by(|a, b| {
        if delta_time >= 0 {
            b.time.cmp(&a.time)
        } else {
            a.time.cmp(&b.time)
        }
    });

    let mut mutations = Vec::new();
    let mut selections = Vec::new();
    for anchor in anchors {
        let new_time = (anchor.time + delta_time).max(0);
        let new_value = anchor.value + delta_value;
        if new_time != anchor.time {
            mutations.push(PropertyMutation::MoveKeyframe {
                path: drag.path.clone(),
                from_time: anchor.time,
                to_time: new_time,
            });
        }
        mutations.push(PropertyMutation::WriteChannels {
            path: drag.path.clone(),
            time: new_time,
            channel_values: vec![(drag.channel_index, new_value)],
            interpolation: InterpolationType::Linear,
        });
        selections.push(AnimationKeyframeSelection {
            clip_id,
            path: drag.path.clone(),
            time: new_time,
        });
    }
    Some((mutations, selections))
}

fn snap_graph_delta_ticks(delta_ticks: TimeTicks) -> TimeTicks {
    ((delta_ticks as f64 / SUBFRAME_TICKS_PER_FRAME as f64).round() as i64)
        * SUBFRAME_TICKS_PER_FRAME
}

fn graph_handles_for_keyframe(
    keyframes: &[mondrian_core::automation::Keyframe<f64>],
    index: usize,
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
) -> Vec<GraphEditorHandle> {
    let mut handles = Vec::new();
    let keyframe = &keyframes[index];
    if !matches!(keyframe.interp_in, KeyframeInterpolation::Hold) && index > 0 {
        let previous = &keyframes[index - 1];
        let handle = handle_from_in(keyframe.interp_in);
        let point = Pos2::new(
            graph_x_for_time(
                rect,
                time_min,
                time_max,
                keyframe.time
                    + ((keyframe.time - previous.time) as f64 * handle.time_offset) as i64,
            ),
            graph_y_for_value(
                rect,
                value_min,
                value_max,
                previous.value + (keyframe.value - previous.value) * (1.0 + handle.value_offset),
            ),
        );
        handles.push(GraphEditorHandle { kind: GraphHandleKind::In, position: point });
    }
    if !matches!(keyframe.interp_out, KeyframeInterpolation::Hold) && index + 1 < keyframes.len() {
        let next = &keyframes[index + 1];
        let handle = handle_from_out(keyframe.interp_out);
        let point = Pos2::new(
            graph_x_for_time(
                rect,
                time_min,
                time_max,
                keyframe.time + ((next.time - keyframe.time) as f64 * handle.time_offset) as i64,
            ),
            graph_y_for_value(
                rect,
                value_min,
                value_max,
                keyframe.value + (next.value - keyframe.value) * handle.value_offset,
            ),
        );
        handles.push(GraphEditorHandle { kind: GraphHandleKind::Out, position: point });
    }
    handles
}

fn handle_from_out(interpolation: KeyframeInterpolation) -> BezierHandle {
    match interpolation {
        KeyframeInterpolation::Bezier(handle) => handle,
        KeyframeInterpolation::Linear => {
            BezierHandle { time_offset: 1.0 / 3.0, value_offset: 1.0 / 3.0 }
        }
        KeyframeInterpolation::Hold => BezierHandle { time_offset: 0.0, value_offset: 0.0 },
    }
}

fn handle_from_in(interpolation: KeyframeInterpolation) -> BezierHandle {
    match interpolation {
        KeyframeInterpolation::Bezier(handle) => handle,
        KeyframeInterpolation::Linear => {
            BezierHandle { time_offset: -1.0 / 3.0, value_offset: -1.0 / 3.0 }
        }
        KeyframeInterpolation::Hold => BezierHandle { time_offset: 0.0, value_offset: 0.0 },
    }
}

fn preview_handle_position(
    kind: GraphHandleKind,
    pointer_pos: Pos2,
    keyframes: &[mondrian_core::automation::Keyframe<f64>],
    index: usize,
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
) -> Option<Pos2> {
    let (interp_in, interp_out) = compute_handle_interpolation(
        kind,
        pointer_pos,
        keyframes,
        index,
        rect,
        time_min,
        time_max,
        value_min,
        value_max,
    )?;
    let fake = mondrian_core::automation::Keyframe {
        id: keyframes[index].id,
        time: keyframes[index].time,
        value: keyframes[index].value,
        interp_in,
        interp_out,
        temporal_flags: keyframes[index].temporal_flags,
    };
    graph_handles_for_keyframe(
        &replace_keyframe(keyframes, index, fake),
        index,
        rect,
        time_min,
        time_max,
        value_min,
        value_max,
    )
    .into_iter()
    .find(|handle| handle.kind == kind)
    .map(|handle| handle.position)
}

fn replace_keyframe(
    keyframes: &[mondrian_core::automation::Keyframe<f64>],
    index: usize,
    keyframe: mondrian_core::automation::Keyframe<f64>,
) -> Vec<mondrian_core::automation::Keyframe<f64>> {
    let mut replaced = keyframes.to_vec();
    replaced[index] = keyframe;
    replaced
}

fn compute_handle_interpolation(
    kind: GraphHandleKind,
    pointer_pos: Pos2,
    keyframes: &[mondrian_core::automation::Keyframe<f64>],
    index: usize,
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
) -> Option<(KeyframeInterpolation, KeyframeInterpolation)> {
    let keyframe = &keyframes[index];
    let mut interp_in = keyframe.interp_in;
    let mut interp_out = keyframe.interp_out;
    match kind {
        GraphHandleKind::Out if index + 1 < keyframes.len() => {
            let next = &keyframes[index + 1];
            let segment_width = (next.time - keyframe.time).max(1) as f32;
            let dx = ((pointer_pos.x - graph_x_for_time(rect, time_min, time_max, keyframe.time))
                / (graph_x_for_time(rect, time_min, time_max, next.time)
                    - graph_x_for_time(rect, time_min, time_max, keyframe.time))
                .max(1.0))
            .clamp(0.05, 0.95);
            let dy = normalized_handle_value(
                pointer_pos.y,
                keyframe.value,
                next.value,
                rect,
                value_min,
                value_max,
            )
            .clamp(-2.0, 2.0);
            let _ = segment_width;
            interp_out = KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: dx as f64,
                value_offset: dy as f64,
            });
        }
        GraphHandleKind::In if index > 0 => {
            let previous = &keyframes[index - 1];
            let dx = ((pointer_pos.x - graph_x_for_time(rect, time_min, time_max, keyframe.time))
                / (graph_x_for_time(rect, time_min, time_max, keyframe.time)
                    - graph_x_for_time(rect, time_min, time_max, previous.time))
                .max(1.0))
            .clamp(-0.95, -0.05);
            let dy = normalized_handle_value_from_end(
                pointer_pos.y,
                previous.value,
                keyframe.value,
                rect,
                value_min,
                value_max,
            )
            .clamp(-2.0, 2.0);
            interp_in = KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: dx as f64,
                value_offset: dy as f64,
            });
        }
        _ => return None,
    }
    Some((interp_in, interp_out))
}

fn normalized_handle_value(
    pointer_y: f32,
    start: f64,
    end: f64,
    rect: Rect,
    value_min: f64,
    value_max: f64,
) -> f32 {
    let actual_value = graph_value_from_y(rect, value_min, value_max, pointer_y);
    if (end - start).abs() < f64::EPSILON {
        0.0
    } else {
        ((actual_value - start) / (end - start)) as f32
    }
}

fn normalized_handle_value_from_end(
    pointer_y: f32,
    start: f64,
    end: f64,
    rect: Rect,
    value_min: f64,
    value_max: f64,
) -> f32 {
    let actual_value = graph_value_from_y(rect, value_min, value_max, pointer_y);
    if (end - start).abs() < f64::EPSILON {
        0.0
    } else {
        ((actual_value - start) / (end - start) - 1.0) as f32
    }
}

fn graph_value_from_y(rect: Rect, value_min: f64, value_max: f64, y: f32) -> f64 {
    let t = ((rect.bottom() - y) / rect.height()).clamp(0.0, 1.0);
    value_min + (value_max - value_min) * t as f64
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
