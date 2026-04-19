use std::collections::{HashMap, HashSet};

use crate::{
    app::{AnimationBubbleHost, AnimationKeyframeSelection, AppState},
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
        interpolation_mode_from_keyframe, timecode_to_ticks, BezierHandle, InterpolationType,
        KeyframeInterpolation, KeyframeTemporalFlags, PropertyHost, PropertyMutation,
        PropertyValue, TimeTicks, SUBFRAME_TICKS_PER_FRAME,
    },
    types::{ClipId, KeyframeId, TimeCode},
};
use mondrian_timeline::clip::Clip;

#[derive(Default)]
pub struct EffectControlsPanel {
    text_edit_buffers: HashMap<(ClipId, String), String>,
    view: EffectControlsView,
    graph_mode: GraphEditorMode,
    graph_channel_selection: HashMap<(ClipId, String), usize>,
    graph_handle_drag: Option<GraphHandleDragState>,
    graph_keyframe_drag: Option<GraphKeyframeDragState>,
    graph_speed_drag: Option<GraphSpeedKeyframeDragState>,
    graph_selection_scale_drag: Option<GraphSelectionScaleDragState>,
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

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum GraphEditorMode {
    #[default]
    Value,
    Speed,
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
struct GraphSpeedKeyframeDragState {
    clip_id: ClipId,
    path: String,
    channel_index: usize,
    start_pointer_pos: Pos2,
    pointer_pos: Pos2,
    anchors: Vec<GraphSpeedKeyframeDragAnchor>,
}

#[derive(Clone)]
struct GraphSpeedKeyframeDragAnchor {
    time: TimeTicks,
    speed: f64,
}

#[derive(Clone)]
struct GraphEditorHandle {
    kind: GraphHandleKind,
    position: Pos2,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GraphSelectionScaleAxis {
    Time,
    Value,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GraphSelectionScaleEdge {
    Min,
    Max,
}

#[derive(Clone)]
struct GraphSelectionScaleDragState {
    clip_id: ClipId,
    path: String,
    channel_index: usize,
    axis: GraphSelectionScaleAxis,
    edge: GraphSelectionScaleEdge,
    pointer_pos: Pos2,
    entries: Vec<SelectedGraphKeyframeData>,
    plot_rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
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

#[derive(Clone)]
struct SelectedGraphKeyframeData {
    selection: AnimationKeyframeSelection,
    channel_value: f64,
}

#[derive(Default, Clone, Copy)]
struct GraphSnapGuides {
    time: Option<TimeTicks>,
    value: Option<f64>,
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
                    .selected_text(property_display_name(property))
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        for (path, candidate) in properties {
                            if ui
                                .selectable_label(
                                    *path == active_path.as_str(),
                                    property_display_name(candidate),
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

                ui.separator();
                ui.selectable_value(&mut self.graph_mode, GraphEditorMode::Value, "值");
                ui.selectable_value(&mut self.graph_mode, GraphEditorMode::Speed, "速度");
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
        let (value_min, value_max) = match self.graph_mode {
            GraphEditorMode::Value => graph_value_range(
                property,
                channel_index,
                current_time_ticks,
                time_min,
                time_max,
            ),
            GraphEditorMode::Speed => graph_speed_range(
                property,
                channel_index,
                clip,
                current_time_ticks,
                time_min,
                time_max,
            ),
        };
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
        if self.graph_mode == GraphEditorMode::Speed && value_min <= 0.0 && value_max >= 0.0 {
            let zero_y = graph_y_for_value(plot_rect, value_min, value_max, 0.0);
            painter.line_segment(
                [
                    Pos2::new(plot_rect.left(), zero_y),
                    Pos2::new(plot_rect.right(), zero_y),
                ],
                Stroke::new(1.2, palette::border_emphasis().gamma_multiply(0.85)),
            );
        }

        let selected_times =
            selected_on_active.iter().map(|selected| selected.time).collect::<Vec<_>>();
        let time_snap_candidates = channel
            .keyframes()
            .iter()
            .map(|keyframe| keyframe.time)
            .filter(|time| !selected_times.contains(time))
            .chain([current_time_ticks, time_min, time_max])
            .collect::<Vec<_>>();
        let value_snap_candidates = channel
            .keyframes()
            .iter()
            .filter(|keyframe| !selected_times.contains(&keyframe.time))
            .map(|keyframe| keyframe.value)
            .chain([0.0])
            .collect::<Vec<_>>();
        let speed_snap_candidates = channel
            .keyframes()
            .iter()
            .filter(|keyframe| !selected_times.contains(&keyframe.time))
            .map(|keyframe| {
                speed_per_second_at_time(
                    property,
                    channel_index,
                    clip,
                    keyframe.time,
                    time_min,
                    time_max,
                )
            })
            .chain([0.0])
            .collect::<Vec<_>>();
        let mut snap_guides = GraphSnapGuides::default();

        let _preview_map = self.graph_keyframe_drag.as_ref().and_then(|drag| {
            if drag.clip_id == selection.clip_id
                && drag.path == active_path
                && drag.channel_index == channel_index
            {
                let (map, guides) = graph_drag_preview_map_with_snap(
                    drag,
                    ui.input(|i| i.pointer.interact_pos()),
                    plot_rect,
                    time_min,
                    time_max,
                    value_min,
                    value_max,
                    &time_snap_candidates,
                    &value_snap_candidates,
                );
                snap_guides = guides;
                Some(map)
            } else {
                None
            }
        });
        let preview_drag_property = self.graph_keyframe_drag.as_ref().and_then(|drag| {
            if drag.clip_id == selection.clip_id
                && drag.path == active_path
                && drag.channel_index == channel_index
            {
                graph_keyframe_preview_property(
                    property,
                    drag,
                    plot_rect,
                    time_min,
                    time_max,
                    value_min,
                    value_max,
                    &time_snap_candidates,
                    &value_snap_candidates,
                )
            } else {
                None
            }
        });
        let speed_preview_map = self.graph_speed_drag.as_ref().and_then(|drag| {
            if drag.clip_id == selection.clip_id
                && drag.path == active_path
                && drag.channel_index == channel_index
            {
                let (map, guides) = graph_speed_drag_preview_map_with_snap(
                    drag,
                    ui.input(|i| i.pointer.interact_pos()),
                    plot_rect,
                    time_min,
                    time_max,
                    value_min,
                    value_max,
                    &speed_snap_candidates,
                );
                snap_guides.value = guides.value;
                Some(map)
            } else {
                None
            }
        });
        let preview_speed_property = self.graph_speed_drag.as_ref().and_then(|drag| {
            if drag.clip_id == selection.clip_id
                && drag.path == active_path
                && drag.channel_index == channel_index
            {
                graph_speed_preview_property(
                    property,
                    drag,
                    clip,
                    time_min,
                    time_max,
                    plot_rect,
                    value_min,
                    value_max,
                    &speed_snap_candidates,
                )
            } else {
                None
            }
        });
        let scale_preview_map = self.graph_selection_scale_drag.as_ref().and_then(|drag| {
            if drag.clip_id == selection.clip_id
                && drag.path == active_path
                && drag.channel_index == channel_index
            {
                Some(graph_selection_scale_preview_map(drag))
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
        if let Some(drag) = &mut self.graph_speed_drag {
            if drag.clip_id == selection.clip_id
                && drag.path == active_path
                && drag.channel_index == channel_index
            {
                if let Some(pointer_pos) = ui.input(|i| i.pointer.interact_pos()) {
                    drag.pointer_pos = pointer_pos;
                }
            }
        }
        if let Some(drag) = &mut self.graph_selection_scale_drag {
            if drag.clip_id == selection.clip_id
                && drag.path == active_path
                && drag.channel_index == channel_index
            {
                if let Some(pointer_pos) = ui.input(|i| i.pointer.interact_pos()) {
                    drag.pointer_pos = pointer_pos;
                }
            }
        }

        let display_property = preview_drag_property.as_ref().unwrap_or(property);
        let display_channel = display_property.channel(channel_index).unwrap_or(channel);

        match self.graph_mode {
            GraphEditorMode::Value => draw_graph_curve(
                &painter,
                plot_rect,
                display_property,
                channel_index,
                time_min,
                time_max,
                value_min,
                value_max,
                None,
            ),
            GraphEditorMode::Speed => draw_speed_graph_curve(
                &painter,
                plot_rect,
                preview_speed_property.as_ref().unwrap_or(property),
                channel_index,
                clip,
                time_min,
                time_max,
                value_min,
                value_max,
            ),
        }

        if let Some(snapped_time) = snap_guides.time {
            let guide_x = graph_x_for_time(plot_rect, time_min, time_max, snapped_time);
            painter.line_segment(
                [
                    Pos2::new(guide_x, plot_rect.top()),
                    Pos2::new(guide_x, plot_rect.bottom()),
                ],
                Stroke::new(1.0, palette::timeline_playhead().gamma_multiply(0.85)),
            );
        }
        if let Some(snapped_value) = snap_guides.value {
            let guide_y = graph_y_for_value(plot_rect, value_min, value_max, snapped_value);
            painter.line_segment(
                [
                    Pos2::new(plot_rect.left(), guide_y),
                    Pos2::new(plot_rect.right(), guide_y),
                ],
                Stroke::new(1.0, palette::interaction_highlight().gamma_multiply(0.75)),
            );
        }

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
        let mut selected_temporal_flags: Option<KeyframeTemporalFlags> = None;
        let selected_keyframe_ids = selected_keyframe_ids(channel, &selected_on_active);
        let selected_active_keyframe_id = selected_active_keyframe_id(channel, &selected_on_active);
        let selected_active_time = if selected_on_active.len() == 1 {
            selected_on_active.first().map(|selection| selection.time)
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

        for (index, keyframe) in display_channel.keyframes().iter().enumerate() {
            let selection_item = AnimationKeyframeSelection {
                clip_id: selection.clip_id,
                path: active_path.clone(),
                time: keyframe.time,
            };
            if selected_active_keyframe_id == Some(keyframe.id) {
                selected_temporal_flags = Some(keyframe.temporal_flags);
            }
            let preview_override = speed_preview_map
                .as_ref()
                .and_then(|map| map.get(&keyframe.time))
                .copied()
                .or_else(|| {
                    scale_preview_map.as_ref().and_then(|map| map.get(&keyframe.time)).copied()
                });
            let point = preview_override.unwrap_or_else(|| match self.graph_mode {
                GraphEditorMode::Value => graph_point_for_keyframe(
                    plot_rect,
                    time_min,
                    time_max,
                    value_min,
                    value_max,
                    keyframe.time,
                    keyframe.value,
                ),
                GraphEditorMode::Speed => graph_point_for_keyframe(
                    plot_rect,
                    time_min,
                    time_max,
                    value_min,
                    value_max,
                    keyframe.time,
                    speed_per_second_at_time(
                        property,
                        channel_index,
                        clip,
                        keyframe.time,
                        time_min,
                        time_max,
                    ),
                ),
            });
            let selected = selected_keyframe_ids.contains(&keyframe.id);
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
                app.set_animation_bubble_host(AnimationBubbleHost::Graph);
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
                if self.graph_mode == GraphEditorMode::Value && !keyframe.temporal_flags.auto_bezier
                {
                    let toggle_label = if keyframe.temporal_flags.broken_handles {
                        "连续手柄"
                    } else {
                        "断开手柄"
                    };
                    if ui.button(toggle_label).clicked() {
                        let mut flags = keyframe.temporal_flags;
                        flags.continuous = true;
                        flags.broken_handles = !flags.broken_handles;
                        let _ = app
                            .mutate_clip_property(
                                selection,
                                PropertyMutation::UpdateKeyframeTemporalFlags {
                                    path: active_path.clone(),
                                    time: keyframe.time,
                                    temporal_flags: flags,
                                },
                                "更新关键帧手柄模式",
                            )
                            .map_err(|err| {
                                app.set_status_hint(format!("更新关键帧手柄模式失败：{err}"), true)
                            });
                        ui.close();
                    }
                }
                let visible_modes = visible_interpolation_modes();
                ui.separator();
                ui.menu_button("关键帧插值", |ui| {
                    draw_keyframe_interpolation_menu(
                        ui,
                        app.selected_animation_interpolation_mode(selection),
                        &visible_modes,
                        |preset| self.apply_interpolation_to_selection(app, selection, preset),
                    );
                });
            });
            if key_response.drag_started()
                && self.graph_handle_drag.is_none()
                && self.graph_selection_scale_drag.is_none()
            {
                let drag_targets = if app.is_animation_keyframe_selected(&selection_item) {
                    selected_on_active.clone()
                } else {
                    app.select_animation_keyframe_only(selection_item.clone());
                    vec![selection_item.clone()]
                };
                match self.graph_mode {
                    GraphEditorMode::Value => {
                        self.graph_keyframe_drag =
                            key_response.interact_pointer_pos().map(|start_pointer_pos| {
                                app.set_animation_bubble_host(AnimationBubbleHost::Graph);
                                GraphKeyframeDragState {
                                    clip_id: selection.clip_id,
                                    path: active_path.clone(),
                                    channel_index,
                                    start_pointer_pos,
                                    pointer_pos: start_pointer_pos,
                                    anchors: drag_targets
                                        .into_iter()
                                        .filter_map(|selected| {
                                            property.keyframe_at(selected.time).and_then(
                                                |keyframe| {
                                                    keyframe
                                                        .value
                                                        .to_channel_values()
                                                        .get(channel_index)
                                                        .copied()
                                                        .map(|value| GraphKeyframeDragAnchor {
                                                            time: selected.time,
                                                            value,
                                                        })
                                                },
                                            )
                                        })
                                        .collect(),
                                }
                            });
                    }
                    GraphEditorMode::Speed => {
                        self.graph_speed_drag =
                            key_response.interact_pointer_pos().map(|start_pointer_pos| {
                                app.set_animation_bubble_host(AnimationBubbleHost::Graph);
                                GraphSpeedKeyframeDragState {
                                    clip_id: selection.clip_id,
                                    path: active_path.clone(),
                                    channel_index,
                                    start_pointer_pos,
                                    pointer_pos: start_pointer_pos,
                                    anchors: drag_targets
                                        .into_iter()
                                        .map(|selected| GraphSpeedKeyframeDragAnchor {
                                            time: selected.time,
                                            speed: speed_per_second_at_time(
                                                property,
                                                channel_index,
                                                clip,
                                                selected.time,
                                                time_min,
                                                time_max,
                                            ),
                                        })
                                        .collect(),
                                }
                            });
                    }
                }
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

            if selected_active_keyframe_id == Some(keyframe.id)
                && self.graph_mode == GraphEditorMode::Value
                && !keyframe.temporal_flags.auto_bezier
            {
                let preview_keyframe = self.graph_handle_drag.as_ref().and_then(|drag| {
                    if drag.clip_id == selection.clip_id
                        && drag.path == active_path
                        && drag.time == keyframe.time
                        && drag.channel_index == channel_index
                    {
                        compute_handle_interpolation(
                            drag.kind,
                            drag.pointer_pos,
                            display_channel.keyframes(),
                            index,
                            keyframe.temporal_flags,
                            plot_rect,
                            time_min,
                            time_max,
                            value_min,
                            value_max,
                        )
                        .map(|(interp_in, interp_out)| {
                            mondrian_core::automation::Keyframe {
                                id: keyframe.id,
                                time: keyframe.time,
                                value: keyframe.value,
                                interp_in,
                                interp_out,
                                temporal_flags: keyframe.temporal_flags,
                            }
                        })
                    } else {
                        None
                    }
                });
                let handles = if let Some(preview_keyframe) = preview_keyframe {
                    graph_handles_for_keyframe(
                        &replace_keyframe(display_channel.keyframes(), index, preview_keyframe),
                        index,
                        plot_rect,
                        time_min,
                        time_max,
                        value_min,
                        value_max,
                    )
                } else {
                    graph_handles_for_keyframe(
                        display_channel.keyframes(),
                        index,
                        plot_rect,
                        time_min,
                        time_max,
                        value_min,
                        value_max,
                    )
                };
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
                                display_channel.keyframes(),
                                index,
                                keyframe.temporal_flags,
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
                                    display_channel.keyframes(),
                                    index,
                                    keyframe.temporal_flags,
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
                        app.set_animation_bubble_host(AnimationBubbleHost::Graph);
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
                    &time_snap_candidates,
                    &value_snap_candidates,
                );
            }
        }
        let mut graph_speed_commit: Option<(
            Vec<PropertyMutation>,
            Vec<AnimationKeyframeSelection>,
        )> = None;
        if let Some(drag) = &self.graph_speed_drag {
            if drag.clip_id == selection.clip_id
                && drag.path == active_path
                && drag.channel_index == channel_index
                && ui.input(|i| i.pointer.any_released())
            {
                graph_speed_commit = graph_speed_drag_mutations(
                    drag,
                    property,
                    clip,
                    time_min,
                    time_max,
                    plot_rect,
                    value_min,
                    value_max,
                    selection.clip_id,
                    &speed_snap_candidates,
                );
            }
        }
        let mut graph_scale_commit: Option<(
            Vec<PropertyMutation>,
            Vec<AnimationKeyframeSelection>,
        )> = None;
        if let Some(drag) = &self.graph_selection_scale_drag {
            if drag.clip_id == selection.clip_id
                && drag.path == active_path
                && drag.channel_index == channel_index
                && ui.input(|i| i.pointer.any_released())
            {
                graph_scale_commit = graph_selection_scale_mutations(drag, selection.clip_id);
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
        if let Some((mutations, selections)) = graph_speed_commit {
            self.graph_speed_drag = None;
            let _ = app
                .mutate_clip_properties(selection, mutations, "编辑速度曲线")
                .map(|_| app.set_animation_keyframe_selection(selections))
                .map_err(|err| app.set_status_hint(format!("编辑速度曲线失败：{err}"), true));
        } else if ui.input(|i| i.pointer.any_released()) {
            self.graph_speed_drag = None;
        }
        if let Some((mutations, selections)) = graph_scale_commit {
            self.graph_selection_scale_drag = None;
            let _ = app
                .mutate_clip_properties(selection, mutations, "缩放图形关键帧")
                .map(|_| app.set_animation_keyframe_selection(selections))
                .map_err(|err| app.set_status_hint(format!("缩放图形关键帧失败：{err}"), true));
        } else if ui.input(|i| i.pointer.any_released()) {
            self.graph_selection_scale_drag = None;
        }

        if let Some((_, preview)) = active_handle_preview {
            painter.circle_filled(
                preview,
                tokens::graph_editor_handle_size() * 0.45,
                palette::interaction_highlight(),
            );
        }

        let mut hovered_selection_transform = false;
        if self.graph_mode == GraphEditorMode::Value && selected_on_active.len() >= 2 {
            if let Some(entries) = self.selected_graph_keyframe_data(
                app,
                selection,
                active_path.as_str(),
                channel_index,
                &selected_on_active,
            ) {
                hovered_selection_transform = self.draw_graph_selection_transform_handles(
                    ui,
                    selection,
                    active_path.as_str(),
                    channel_index,
                    &entries,
                    &selected_points,
                    plot_rect,
                    time_min,
                    time_max,
                    value_min,
                    value_max,
                );
            }
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
            hovered_keyframe || hovered_handle || hovered_selection_transform,
        );

        self.draw_graph_selection_bubble(
            ui.ctx(),
            selection,
            current_time_ticks,
            &selected_points,
            &handle_points,
            plot_rect,
            active_path.as_str(),
            channel_index,
            &selected_on_active,
            selected_active_time,
            selected_temporal_flags,
            self.graph_mode,
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

    fn scale_graph_selection_time(
        &mut self,
        app: &mut AppState,
        selection: SelectedClipRef,
        active_path: &str,
        selected_on_active: &[AnimationKeyframeSelection],
        factor: f64,
    ) {
        if selected_on_active.len() < 2 {
            return;
        }

        let Some(entries) =
            self.selected_graph_keyframe_data(app, selection, active_path, 0, selected_on_active)
        else {
            return;
        };

        let min_time = entries.iter().map(|entry| entry.selection.time).min().unwrap_or(0);
        let max_time = entries.iter().map(|entry| entry.selection.time).max().unwrap_or(min_time);
        if min_time == max_time {
            return;
        }
        let pivot = (min_time + max_time) as f64 * 0.5;

        let new_times = entries
            .iter()
            .map(|entry| {
                let scaled = pivot + (entry.selection.time as f64 - pivot) * factor;
                let snapped = snap_time_ticks(scaled.round() as TimeTicks);
                (entry.selection.time, snapped.max(0))
            })
            .collect::<Vec<_>>();

        let mut dedup = std::collections::HashSet::new();
        if new_times.iter().any(|(_, new_time)| !dedup.insert(*new_time)) {
            app.set_status_hint("时间缩放后关键帧发生重叠，已取消", true);
            return;
        }

        let mut ordered = entries
            .iter()
            .map(|entry| entry.selection.time)
            .zip(new_times.iter().map(|(_, new_time)| *new_time))
            .collect::<Vec<_>>();
        ordered.sort_by(|a, b| {
            if factor >= 1.0 {
                b.0.cmp(&a.0)
            } else {
                a.0.cmp(&b.0)
            }
        });

        let mutations = ordered
            .into_iter()
            .filter(|(old_time, new_time)| old_time != new_time)
            .map(|(old_time, new_time)| PropertyMutation::MoveKeyframe {
                path: active_path.to_string(),
                from_time: old_time,
                to_time: new_time,
            })
            .collect::<Vec<_>>();
        if mutations.is_empty() {
            return;
        }

        let new_selection = selected_on_active
            .iter()
            .zip(new_times.into_iter())
            .map(|(selected, (_, new_time))| AnimationKeyframeSelection {
                clip_id: selected.clip_id,
                path: selected.path.clone(),
                time: new_time,
            })
            .collect::<Vec<_>>();

        let _ = app
            .mutate_clip_properties(selection, mutations, "缩放关键帧时间")
            .map(|_| app.set_animation_keyframe_selection(new_selection))
            .map_err(|err| app.set_status_hint(format!("缩放关键帧时间失败：{err}"), true));
    }

    fn scale_graph_selection_values(
        &mut self,
        app: &mut AppState,
        selection: SelectedClipRef,
        active_path: &str,
        channel_index: usize,
        selected_on_active: &[AnimationKeyframeSelection],
        factor: f64,
    ) {
        if selected_on_active.len() < 2 {
            return;
        }

        let Some(entries) = self.selected_graph_keyframe_data(
            app,
            selection,
            active_path,
            channel_index,
            selected_on_active,
        ) else {
            return;
        };

        let min_value =
            entries.iter().map(|entry| entry.channel_value).fold(f64::INFINITY, f64::min);
        let max_value = entries
            .iter()
            .map(|entry| entry.channel_value)
            .fold(f64::NEG_INFINITY, f64::max);
        if !min_value.is_finite() || !max_value.is_finite() {
            return;
        }
        let pivot = (min_value + max_value) * 0.5;

        let mutations = entries
            .iter()
            .map(|entry| {
                let scaled = pivot + (entry.channel_value - pivot) * factor;
                PropertyMutation::WriteChannels {
                    path: active_path.to_string(),
                    time: entry.selection.time,
                    channel_values: vec![(channel_index, scaled)],
                    interpolation: InterpolationType::Linear,
                }
            })
            .collect::<Vec<_>>();

        let _ = app
            .mutate_clip_properties(selection, mutations, "缩放关键帧数值")
            .map_err(|err| app.set_status_hint(format!("缩放关键帧数值失败：{err}"), true));
    }

    fn selected_graph_keyframe_data(
        &self,
        app: &AppState,
        selection: SelectedClipRef,
        active_path: &str,
        channel_index: usize,
        selected_on_active: &[AnimationKeyframeSelection],
    ) -> Option<Vec<SelectedGraphKeyframeData>> {
        let clip = app.clip_snapshot(selection)?;
        let property_bag = clip.property_bag().ok()?;
        let property = property_bag.property(active_path)?;
        let entries = selected_on_active
            .iter()
            .filter_map(|selected| {
                property.keyframe_at(selected.time).and_then(|keyframe| {
                    keyframe.value.to_channel_values().get(channel_index).copied().map(
                        |channel_value| SelectedGraphKeyframeData {
                            selection: selected.clone(),
                            channel_value,
                        },
                    )
                })
            })
            .collect::<Vec<_>>();
        Some(entries)
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_graph_selection_transform_handles(
        &mut self,
        ui: &mut Ui,
        selection: SelectedClipRef,
        active_path: &str,
        channel_index: usize,
        entries: &[SelectedGraphKeyframeData],
        selected_points: &[Pos2],
        plot_rect: Rect,
        time_min: TimeTicks,
        time_max: TimeTicks,
        value_min: f64,
        value_max: f64,
    ) -> bool {
        if selected_points.len() < 2 {
            return false;
        }

        let centroid = selected_points.iter().fold(Pos2::ZERO, |acc, point| {
            Pos2::new(acc.x + point.x, acc.y + point.y)
        });
        let centroid = Pos2::new(
            centroid.x / selected_points.len() as f32,
            centroid.y / selected_points.len() as f32,
        );
        let bounds = selected_points.iter().fold(
            Rect::from_center_size(centroid, Vec2::ZERO),
            |acc, point| {
                Rect::from_min_max(
                    Pos2::new(acc.min.x.min(point.x), acc.min.y.min(point.y)),
                    Pos2::new(acc.max.x.max(point.x), acc.max.y.max(point.y)),
                )
            },
        );

        ui.painter().rect_stroke(
            bounds.expand(6.0),
            egui::CornerRadius::same(4),
            Stroke::new(1.0, palette::interaction_highlight().gamma_multiply(0.55)),
            egui::StrokeKind::Inside,
        );

        let handle_size = Vec2::splat(10.0);
        let handles = [
            (
                GraphSelectionScaleAxis::Time,
                GraphSelectionScaleEdge::Min,
                Pos2::new(bounds.left() - 6.0, bounds.center().y),
                "压缩/扩展起始时间",
            ),
            (
                GraphSelectionScaleAxis::Time,
                GraphSelectionScaleEdge::Max,
                Pos2::new(bounds.right() + 6.0, bounds.center().y),
                "压缩/扩展结束时间",
            ),
            (
                GraphSelectionScaleAxis::Value,
                GraphSelectionScaleEdge::Max,
                Pos2::new(bounds.center().x, bounds.top() - 6.0),
                "缩放最大数值",
            ),
            (
                GraphSelectionScaleAxis::Value,
                GraphSelectionScaleEdge::Min,
                Pos2::new(bounds.center().x, bounds.bottom() + 6.0),
                "缩放最小数值",
            ),
        ];

        let mut hovered_any = false;
        for (axis, edge, center, tooltip) in handles {
            let rect = Rect::from_center_size(center, handle_size);
            let response = ui
                .interact(
                    rect,
                    ui.make_persistent_id((
                        "graph_selection_scale",
                        selection.clip_id,
                        active_path,
                        channel_index,
                        matches!(axis, GraphSelectionScaleAxis::Time),
                        matches!(edge, GraphSelectionScaleEdge::Max),
                    )),
                    Sense::click_and_drag(),
                )
                .on_hover_text(tooltip);
            hovered_any |= response.hovered();
            if response.drag_started() {
                self.graph_selection_scale_drag =
                    response.interact_pointer_pos().map(|pointer_pos| {
                        GraphSelectionScaleDragState {
                            clip_id: selection.clip_id,
                            path: active_path.to_string(),
                            channel_index,
                            axis,
                            edge,
                            pointer_pos,
                            entries: entries.to_vec(),
                            plot_rect,
                            time_min,
                            time_max,
                            value_min,
                            value_max,
                        }
                    });
            }
            ui.painter().rect_filled(
                rect,
                2.0,
                if response.hovered() {
                    palette::interaction_highlight()
                } else {
                    palette::bg_surface_hover()
                },
            );
            ui.painter().rect_stroke(
                rect,
                egui::CornerRadius::same(2),
                Stroke::new(1.0, palette::text_primary()),
                egui::StrokeKind::Inside,
            );
        }
        hovered_any
    }

    fn draw_graph_selection_bubble(
        &mut self,
        ctx: &egui::Context,
        selection: SelectedClipRef,
        current_time_ticks: TimeTicks,
        selected_points: &[Pos2],
        handle_points: &[Pos2],
        plot_rect: Rect,
        active_path: &str,
        channel_index: usize,
        selected_on_active: &[AnimationKeyframeSelection],
        selected_keyframe_time: Option<TimeTicks>,
        selected_temporal_flags: Option<KeyframeTemporalFlags>,
        graph_mode: GraphEditorMode,
        app: &mut AppState,
    ) {
        if selected_points.is_empty()
            || self.graph_handle_drag.is_some()
            || self.graph_keyframe_drag.is_some()
            || self.graph_speed_drag.is_some()
            || self.graph_selection_scale_drag.is_some()
            || app.animation_bubble_host() == Some(AnimationBubbleHost::Timeline)
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
        let visible_modes = visible_interpolation_modes();
        let scale_action_count =
            usize::from(graph_mode == GraphEditorMode::Value && selected_on_active.len() >= 2) * 4;
        let handle_toggle_label = selected_temporal_flags.and_then(|flags| {
            (!flags.auto_bezier).then_some(if flags.broken_handles {
                "连续手柄"
            } else {
                "断开手柄"
            })
        });
        let bubble_pos = floating_toolbar_position(
            selected_bounds,
            avoid_bounds,
            plot_rect.expand2(Vec2::new(16.0, 16.0)),
            3 + usize::from(app.has_animation_clipboard()),
            1 + usize::from(handle_toggle_label.is_some()) + scale_action_count,
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

                        if graph_mode == GraphEditorMode::Value && selected_on_active.len() >= 2 {
                            ui.separator();
                            if ui
                                .small_button("时-")
                                .on_hover_text("压缩所选关键帧时间范围")
                                .clicked()
                            {
                                self.scale_graph_selection_time(
                                    app,
                                    selection,
                                    active_path,
                                    selected_on_active,
                                    0.8,
                                );
                            }
                            if ui
                                .small_button("时+")
                                .on_hover_text("扩展所选关键帧时间范围")
                                .clicked()
                            {
                                self.scale_graph_selection_time(
                                    app,
                                    selection,
                                    active_path,
                                    selected_on_active,
                                    1.25,
                                );
                            }
                            if ui
                                .small_button("值-")
                                .on_hover_text("缩小所选关键帧数值幅度")
                                .clicked()
                            {
                                self.scale_graph_selection_values(
                                    app,
                                    selection,
                                    active_path,
                                    channel_index,
                                    selected_on_active,
                                    0.8,
                                );
                            }
                            if ui
                                .small_button("值+")
                                .on_hover_text("放大所选关键帧数值幅度")
                                .clicked()
                            {
                                self.scale_graph_selection_values(
                                    app,
                                    selection,
                                    active_path,
                                    channel_index,
                                    selected_on_active,
                                    1.25,
                                );
                            }
                        }

                        if let Some(label) =
                            handle_toggle_label.filter(|_| graph_mode == GraphEditorMode::Value)
                        {
                            ui.separator();
                            if ui.small_button(label).clicked() {
                                if let (Some(mut flags), Some(time)) =
                                    (selected_temporal_flags, selected_keyframe_time)
                                {
                                    flags.continuous = true;
                                    flags.broken_handles = !flags.broken_handles;
                                    let _ = app
                                        .mutate_clip_property(
                                            selection,
                                            PropertyMutation::UpdateKeyframeTemporalFlags {
                                                path: active_path.to_string(),
                                                time,
                                                temporal_flags: flags,
                                            },
                                            "更新关键帧手柄模式",
                                        )
                                        .map_err(|err| {
                                            app.set_status_hint(
                                                format!("更新关键帧手柄模式失败：{err}"),
                                                true,
                                            )
                                        });
                                }
                            }
                        }

                        ui.separator();
                        ui.menu_button("插值", |ui| {
                            draw_keyframe_interpolation_menu(
                                ui,
                                app.selected_animation_interpolation_mode(selection),
                                &visible_modes,
                                |preset| {
                                    self.apply_interpolation_to_selection(app, selection, preset)
                                },
                            );
                        });
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
        if self.graph_handle_drag.is_some()
            || self.graph_keyframe_drag.is_some()
            || self.graph_speed_drag.is_some()
            || self.graph_selection_scale_drag.is_some()
        {
            self.clear_graph_marquee();
            return;
        }

        let pointer_pos = ui.input(|i| i.pointer.interact_pos());
        let primary_pressed = ui.input(|i| i.pointer.primary_pressed());
        let primary_down = ui.input(|i| i.pointer.primary_down());
        let primary_released = ui.input(|i| i.pointer.primary_released());
        let pointer_over_floating_ui = ui.ctx().is_pointer_over_area();

        if primary_pressed {
            if let Some(pos) = pointer_pos {
                if bounds.contains(pos) && !pointer_on_anchor && !pointer_over_floating_ui {
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
                } else if bounds.contains(current)
                    && !pointer_on_anchor
                    && !pointer_over_floating_ui
                {
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
                RichText::new(property_display_name(property))
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
            &property.descriptor.ui_metadata,
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
        metadata: &mondrian_core::automation::AnimatablePropertyUiMetadata,
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
                let mut drag = DragValue::new(&mut edited).speed(metadata.step.unwrap_or(1.0));
                match (metadata.min, metadata.max) {
                    (Some(min), Some(max)) => {
                        drag = drag.range((min.round() as i64)..=(max.round() as i64));
                    }
                    (Some(min), None) => {
                        drag = drag.range((min.round() as i64)..=i64::MAX);
                    }
                    (None, Some(max)) => {
                        drag = drag.range(i64::MIN..=(max.round() as i64));
                    }
                    (None, None) => {}
                }
                if ui.add(drag).changed() {
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
                let mut drag = DragValue::new(&mut edited).speed(metadata.step.unwrap_or(0.01));
                match (metadata.min, metadata.max) {
                    (Some(min), Some(max)) => drag = drag.range(min..=max),
                    (Some(min), None) => drag = drag.range(min..=f64::INFINITY),
                    (None, Some(max)) => drag = drag.range(f64::NEG_INFINITY..=max),
                    (None, None) => {}
                }
                if ui.add(drag).changed() {
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
                let mut drag = DragValue::new(&mut edited).speed(metadata.step.unwrap_or(0.01));
                match (metadata.min, metadata.max) {
                    (Some(min), Some(max)) => drag = drag.range(min..=max),
                    (Some(min), None) => drag = drag.range(min..=f64::INFINITY),
                    (None, Some(max)) => drag = drag.range(f64::NEG_INFINITY..=max),
                    (None, None) => {}
                }
                if ui.add(drag).changed() {
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
                let speed = metadata.step.unwrap_or(0.05);
                ui.horizontal(|ui| {
                    let x_changed =
                        ui.add(DragValue::new(&mut edited.x).speed(speed).prefix("X ")).changed();
                    let y_changed =
                        ui.add(DragValue::new(&mut edited.y).speed(speed).prefix("Y ")).changed();
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
                let speed = metadata.step.unwrap_or(0.05);
                ui.horizontal(|ui| {
                    let x_changed =
                        ui.add(DragValue::new(&mut edited.x).speed(speed).prefix("X ")).changed();
                    let y_changed =
                        ui.add(DragValue::new(&mut edited.y).speed(speed).prefix("Y ")).changed();
                    let z_changed =
                        ui.add(DragValue::new(&mut edited.z).speed(speed).prefix("Z ")).changed();
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
            let interpolation = interpolation_mode_from_keyframe(
                keyframe.interp_in,
                keyframe.interp_out,
                keyframe.temporal_flags,
            );
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

fn visible_interpolation_modes() -> [InterpolationType; 5] {
    [
        InterpolationType::Linear,
        InterpolationType::Bezier,
        InterpolationType::AutoBezier,
        InterpolationType::ContinuousBezier,
        InterpolationType::Hold,
    ]
}

fn interpolation_mode_label(interpolation: InterpolationType) -> &'static str {
    match interpolation {
        InterpolationType::Linear => "线性",
        InterpolationType::Bezier => "贝塞尔曲线",
        InterpolationType::AutoBezier => "自动贝塞尔曲线",
        InterpolationType::ContinuousBezier => "连续贝塞尔曲线",
        InterpolationType::Hold => "定格",
        InterpolationType::EaseIn | InterpolationType::EaseOut => "",
    }
}

fn interpolation_action_label(interpolation: InterpolationType) -> &'static str {
    match interpolation {
        InterpolationType::EaseIn => "缓入",
        InterpolationType::EaseOut => "缓出",
        _ => "",
    }
}

fn draw_keyframe_interpolation_menu(
    ui: &mut Ui,
    current_mode: Option<InterpolationType>,
    available_modes: &[InterpolationType],
    mut apply: impl FnMut(InterpolationType),
) {
    let widest_label = available_modes
        .iter()
        .map(|mode| interpolation_mode_label(*mode))
        .chain(
            [InterpolationType::EaseIn, InterpolationType::EaseOut]
                .into_iter()
                .map(interpolation_action_label),
        )
        .max_by_key(|label| label.chars().count())
        .unwrap_or("线性");
    let text_width = ui
        .painter()
        .layout_no_wrap(
            widest_label.to_string(),
            typography::body_small(),
            palette::text_primary(),
        )
        .size()
        .x;
    ui.set_min_width((18.0 + 10.0 + text_width + 8.0).max(140.0));

    for mode in available_modes {
        if theme::checkmark_menu_action_fill(
            ui,
            current_mode == Some(*mode),
            interpolation_mode_label(*mode),
        )
        .clicked()
        {
            apply(*mode);
            ui.close();
        }
    }
    ui.separator();
    for action in [InterpolationType::EaseIn, InterpolationType::EaseOut] {
        if theme::menu_action_fill(ui, interpolation_action_label(action)).clicked() {
            apply(action);
            ui.close();
        }
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

fn property_display_name(property: &mondrian_core::automation::AnimatedProperty) -> String {
    if let Some(group) = property.descriptor.ui_metadata.group_name.as_deref() {
        format!("{group} · {}", property.descriptor.display_name)
    } else {
        property.descriptor.display_name.clone()
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

fn selected_keyframe_ids(
    channel: &mondrian_core::automation::AnimationChannel,
    selected_on_active: &[AnimationKeyframeSelection],
) -> HashSet<KeyframeId> {
    selected_on_active
        .iter()
        .filter_map(|selected| channel.keyframe_at(selected.time).map(|keyframe| keyframe.id))
        .collect()
}

fn selected_active_keyframe_id(
    channel: &mondrian_core::automation::AnimationChannel,
    selected_on_active: &[AnimationKeyframeSelection],
) -> Option<KeyframeId> {
    if selected_on_active.len() == 1 {
        channel.keyframe_at(selected_on_active[0].time).map(|keyframe| keyframe.id)
    } else {
        None
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

fn graph_speed_range(
    property: &mondrian_core::automation::AnimatedProperty,
    channel_index: usize,
    clip: &Clip,
    current_time: TimeTicks,
    time_min: TimeTicks,
    time_max: TimeTicks,
) -> (f64, f64) {
    let mut values = property
        .channel(channel_index)
        .map(|channel| {
            channel
                .keyframes()
                .iter()
                .map(|keyframe| {
                    speed_per_second_at_time(
                        property,
                        channel_index,
                        clip,
                        keyframe.time,
                        time_min,
                        time_max,
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    values.push(speed_per_second_at_time(
        property,
        channel_index,
        clip,
        current_time,
        time_min,
        time_max,
    ));

    let samples = 48usize;
    for sample in 0..=samples {
        let t = sample as f32 / samples as f32;
        let time = time_min + ((time_max - time_min) as f32 * t) as i64;
        values.push(speed_per_second_at_time(
            property,
            channel_index,
            clip,
            time,
            time_min,
            time_max,
        ));
    }

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

fn draw_speed_graph_curve(
    painter: &egui::Painter,
    rect: Rect,
    property: &mondrian_core::automation::AnimatedProperty,
    channel_index: usize,
    clip: &Clip,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
) {
    let samples = 128usize;
    let mut points = Vec::with_capacity(samples + 1);
    for sample in 0..=samples {
        let t = sample as f32 / samples as f32;
        let time = time_min + ((time_max - time_min) as f32 * t) as i64;
        let speed =
            speed_per_second_at_time(property, channel_index, clip, time, time_min, time_max);
        points.push(graph_point_for_keyframe(
            rect, time_min, time_max, value_min, value_max, time, speed,
        ));
    }
    painter.add(Shape::line(
        points,
        Stroke::new(
            tokens::graph_editor_curve_stroke_width(),
            graph_channel_color(channel_index),
        ),
    ));
}

fn speed_per_second_at_time(
    property: &mondrian_core::automation::AnimatedProperty,
    channel_index: usize,
    clip: &Clip,
    time: TimeTicks,
    time_min: TimeTicks,
    time_max: TimeTicks,
) -> f64 {
    let step = (SUBFRAME_TICKS_PER_FRAME / 8).max(1);
    let left = time.saturating_sub(step).max(time_min);
    let right = (time + step).min(time_max);
    if right <= left {
        return 0.0;
    }

    let left_value = property
        .evaluate(left)
        .to_channel_values()
        .get(channel_index)
        .copied()
        .unwrap_or(0.0);
    let right_value = property
        .evaluate(right)
        .to_channel_values()
        .get(channel_index)
        .copied()
        .unwrap_or(0.0);
    let seconds =
        (right - left) as f64 / SUBFRAME_TICKS_PER_FRAME as f64 * clip.position.time_base.to_f64();
    if seconds.abs() < f64::EPSILON {
        0.0
    } else {
        (right_value - left_value) / seconds
    }
}

fn graph_time_from_x(rect: Rect, time_min: TimeTicks, time_max: TimeTicks, x: f32) -> TimeTicks {
    if time_max <= time_min || rect.width() <= 1.0 {
        return time_min;
    }
    let t = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
    time_min + ((time_max - time_min) as f32 * t).round() as i64
}

fn graph_drag_preview_map_with_snap(
    drag: &GraphKeyframeDragState,
    pointer_pos: Option<Pos2>,
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
    time_snap_candidates: &[TimeTicks],
    value_snap_candidates: &[f64],
) -> (HashMap<TimeTicks, Pos2>, GraphSnapGuides) {
    let mut map = HashMap::new();
    let Some(pointer_pos) = pointer_pos else {
        return (map, GraphSnapGuides::default());
    };
    let raw_delta_time = graph_time_from_x(rect, time_min, time_max, pointer_pos.x)
        - graph_time_from_x(rect, time_min, time_max, drag.start_pointer_pos.x);
    let raw_delta_time = snap_graph_delta_ticks(raw_delta_time);
    let anchor_times = drag.anchors.iter().map(|anchor| anchor.time).collect::<Vec<_>>();
    let (delta_time, snapped_time) = snap_graph_time_delta(
        raw_delta_time,
        &anchor_times,
        time_snap_candidates,
        rect,
        time_min,
        time_max,
    );
    let raw_delta_value = graph_value_from_y(rect, value_min, value_max, pointer_pos.y)
        - graph_value_from_y(rect, value_min, value_max, drag.start_pointer_pos.y);
    let anchor_values = drag.anchors.iter().map(|anchor| anchor.value).collect::<Vec<_>>();
    let (delta_value, snapped_value) = snap_graph_value_delta(
        raw_delta_value,
        &anchor_values,
        value_snap_candidates,
        rect,
        value_min,
        value_max,
    );
    for anchor in &drag.anchors {
        let time = (anchor.time + delta_time).max(0);
        let value = anchor.value + delta_value;
        map.insert(
            anchor.time,
            graph_point_for_keyframe(rect, time_min, time_max, value_min, value_max, time, value),
        );
    }
    (
        map,
        GraphSnapGuides { time: snapped_time, value: snapped_value },
    )
}

#[allow(clippy::too_many_arguments)]
fn graph_keyframe_preview_property(
    property: &mondrian_core::automation::AnimatedProperty,
    drag: &GraphKeyframeDragState,
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
    time_snap_candidates: &[TimeTicks],
    value_snap_candidates: &[f64],
) -> Option<mondrian_core::automation::AnimatedProperty> {
    let mutations = graph_keyframe_drag_property_mutations(
        drag,
        rect,
        time_min,
        time_max,
        value_min,
        value_max,
        time_snap_candidates,
        value_snap_candidates,
    )?;
    let mut preview = property.clone();
    for mutation in mutations {
        preview.apply_mutation(mutation).ok()?;
    }
    Some(preview)
}

fn graph_speed_drag_preview_map_with_snap(
    drag: &GraphSpeedKeyframeDragState,
    pointer_pos: Option<Pos2>,
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
    speed_snap_candidates: &[f64],
) -> (HashMap<TimeTicks, Pos2>, GraphSnapGuides) {
    let mut map = HashMap::new();
    let Some(pointer_pos) = pointer_pos else {
        return (map, GraphSnapGuides::default());
    };
    let raw_delta_speed = graph_value_from_y(rect, value_min, value_max, pointer_pos.y)
        - graph_value_from_y(rect, value_min, value_max, drag.start_pointer_pos.y);
    let anchor_speeds = drag.anchors.iter().map(|anchor| anchor.speed).collect::<Vec<_>>();
    let (delta_speed, snapped_speed) = snap_graph_value_delta(
        raw_delta_speed,
        &anchor_speeds,
        speed_snap_candidates,
        rect,
        value_min,
        value_max,
    );
    for anchor in &drag.anchors {
        let speed = anchor.speed + delta_speed;
        map.insert(
            anchor.time,
            graph_point_for_keyframe(
                rect,
                time_min,
                time_max,
                value_min,
                value_max,
                anchor.time,
                speed,
            ),
        );
    }
    (map, GraphSnapGuides { time: None, value: snapped_speed })
}

fn graph_keyframe_drag_mutations(
    drag: &GraphKeyframeDragState,
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
    clip_id: ClipId,
    time_snap_candidates: &[TimeTicks],
    value_snap_candidates: &[f64],
) -> Option<(Vec<PropertyMutation>, Vec<AnimationKeyframeSelection>)> {
    let mutations = graph_keyframe_drag_property_mutations(
        drag,
        rect,
        time_min,
        time_max,
        value_min,
        value_max,
        time_snap_candidates,
        value_snap_candidates,
    )?;
    let raw_delta_time = graph_time_from_x(rect, time_min, time_max, drag.pointer_pos.x)
        - graph_time_from_x(rect, time_min, time_max, drag.start_pointer_pos.x);
    let raw_delta_time = snap_graph_delta_ticks(raw_delta_time);
    let anchor_times = drag.anchors.iter().map(|anchor| anchor.time).collect::<Vec<_>>();
    let (delta_time, _) = snap_graph_time_delta(
        raw_delta_time,
        &anchor_times,
        time_snap_candidates,
        rect,
        time_min,
        time_max,
    );
    let selections = drag
        .anchors
        .iter()
        .map(|anchor| AnimationKeyframeSelection {
            clip_id,
            path: drag.path.clone(),
            time: (anchor.time + delta_time).max(0),
        })
        .collect::<Vec<_>>();

    Some((mutations, selections))
}

#[allow(clippy::too_many_arguments)]
fn graph_keyframe_drag_property_mutations(
    drag: &GraphKeyframeDragState,
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
    time_snap_candidates: &[TimeTicks],
    value_snap_candidates: &[f64],
) -> Option<Vec<PropertyMutation>> {
    let raw_delta_time = graph_time_from_x(rect, time_min, time_max, drag.pointer_pos.x)
        - graph_time_from_x(rect, time_min, time_max, drag.start_pointer_pos.x);
    let raw_delta_time = snap_graph_delta_ticks(raw_delta_time);
    let anchor_times = drag.anchors.iter().map(|anchor| anchor.time).collect::<Vec<_>>();
    let (delta_time, _) = snap_graph_time_delta(
        raw_delta_time,
        &anchor_times,
        time_snap_candidates,
        rect,
        time_min,
        time_max,
    );
    let raw_delta_value = graph_value_from_y(rect, value_min, value_max, drag.pointer_pos.y)
        - graph_value_from_y(rect, value_min, value_max, drag.start_pointer_pos.y);
    let anchor_values = drag.anchors.iter().map(|anchor| anchor.value).collect::<Vec<_>>();
    let (delta_value, _) = snap_graph_value_delta(
        raw_delta_value,
        &anchor_values,
        value_snap_candidates,
        rect,
        value_min,
        value_max,
    );

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
        mutations.push(PropertyMutation::UpdateChannelKeyframeValue {
            path: drag.path.clone(),
            time: new_time,
            channel_index: drag.channel_index,
            value: new_value,
        });
    }
    Some(mutations)
}

#[allow(clippy::too_many_arguments)]
fn graph_speed_drag_mutations(
    drag: &GraphSpeedKeyframeDragState,
    property: &mondrian_core::automation::AnimatedProperty,
    clip: &Clip,
    time_min: TimeTicks,
    time_max: TimeTicks,
    rect: Rect,
    value_min: f64,
    value_max: f64,
    clip_id: ClipId,
    speed_snap_candidates: &[f64],
) -> Option<(Vec<PropertyMutation>, Vec<AnimationKeyframeSelection>)> {
    let raw_delta_speed = graph_value_from_y(rect, value_min, value_max, drag.pointer_pos.y)
        - graph_value_from_y(rect, value_min, value_max, drag.start_pointer_pos.y);
    let anchor_speeds = drag.anchors.iter().map(|anchor| anchor.speed).collect::<Vec<_>>();
    let (delta_speed, _) = snap_graph_value_delta(
        raw_delta_speed,
        &anchor_speeds,
        speed_snap_candidates,
        rect,
        value_min,
        value_max,
    );
    if delta_speed.abs() < f64::EPSILON {
        return None;
    }

    let channel = property.channel(drag.channel_index)?;
    let mut mutations = Vec::new();
    let mut selections = Vec::new();

    for anchor in &drag.anchors {
        let index = channel.keyframes().iter().position(|keyframe| keyframe.time == anchor.time)?;
        let keyframe = &channel.keyframes()[index];
        let target_speed = anchor.speed + delta_speed;
        let (interp_in, interp_out) = interpolations_for_target_speed(
            channel.keyframes(),
            index,
            keyframe.temporal_flags,
            clip,
            target_speed,
        )?;
        mutations.push(PropertyMutation::UpdateChannelKeyframeHandles {
            path: drag.path.clone(),
            time: anchor.time,
            channel_index: drag.channel_index,
            interp_in,
            interp_out,
        });
        selections.push(AnimationKeyframeSelection {
            clip_id,
            path: drag.path.clone(),
            time: anchor.time,
        });
    }

    let _ = (time_min, time_max);
    if mutations.is_empty() {
        None
    } else {
        Some((mutations, selections))
    }
}

#[allow(clippy::too_many_arguments)]
fn graph_speed_preview_property(
    property: &mondrian_core::automation::AnimatedProperty,
    drag: &GraphSpeedKeyframeDragState,
    clip: &Clip,
    time_min: TimeTicks,
    time_max: TimeTicks,
    rect: Rect,
    value_min: f64,
    value_max: f64,
    speed_snap_candidates: &[f64],
) -> Option<mondrian_core::automation::AnimatedProperty> {
    let mut preview = property.clone();
    let mutations = graph_speed_drag_mutations(
        drag,
        property,
        clip,
        time_min,
        time_max,
        rect,
        value_min,
        value_max,
        drag.clip_id,
        speed_snap_candidates,
    )?
    .0;
    for mutation in mutations {
        let _ = preview.apply_mutation(mutation);
    }
    Some(preview)
}

fn graph_selection_scale_preview_map(
    drag: &GraphSelectionScaleDragState,
) -> HashMap<TimeTicks, Pos2> {
    let mut map = HashMap::new();
    let factor = graph_selection_scale_factor(drag);
    for entry in &drag.entries {
        let (time, value) = graph_selection_scaled_point(entry, drag, factor);
        map.insert(
            entry.selection.time,
            graph_point_for_keyframe(
                drag.plot_rect,
                drag.time_min,
                drag.time_max,
                drag.value_min,
                drag.value_max,
                time,
                value,
            ),
        );
    }
    map
}

fn graph_selection_scale_mutations(
    drag: &GraphSelectionScaleDragState,
    clip_id: ClipId,
) -> Option<(Vec<PropertyMutation>, Vec<AnimationKeyframeSelection>)> {
    let factor = graph_selection_scale_factor(drag);
    if (factor - 1.0).abs() < 1e-3 {
        return None;
    }

    let mut mutations = Vec::new();
    let mut selections = Vec::new();

    match drag.axis {
        GraphSelectionScaleAxis::Time => {
            let remapped = drag
                .entries
                .iter()
                .map(|entry| {
                    let (time, _) = graph_selection_scaled_point(entry, drag, factor);
                    (entry.selection.time, time)
                })
                .collect::<Vec<_>>();

            let mut dedup = std::collections::HashSet::new();
            if remapped.iter().any(|(_, new_time)| !dedup.insert(*new_time)) {
                return None;
            }

            let mut ordered = remapped.clone();
            ordered.sort_by(|a, b| {
                if factor >= 1.0 {
                    b.0.cmp(&a.0)
                } else {
                    a.0.cmp(&b.0)
                }
            });
            for (old_time, new_time) in ordered {
                if old_time != new_time {
                    mutations.push(PropertyMutation::MoveKeyframe {
                        path: drag.path.clone(),
                        from_time: old_time,
                        to_time: new_time,
                    });
                }
            }
            selections.extend(drag.entries.iter().zip(remapped.into_iter()).map(
                |(entry, (_, new_time))| AnimationKeyframeSelection {
                    clip_id,
                    path: entry.selection.path.clone(),
                    time: new_time,
                },
            ));
        }
        GraphSelectionScaleAxis::Value => {
            for entry in &drag.entries {
                let (_, value) = graph_selection_scaled_point(entry, drag, factor);
                mutations.push(PropertyMutation::WriteChannels {
                    path: drag.path.clone(),
                    time: entry.selection.time,
                    channel_values: vec![(drag.channel_index, value)],
                    interpolation: InterpolationType::Linear,
                });
                selections.push(AnimationKeyframeSelection {
                    clip_id,
                    path: entry.selection.path.clone(),
                    time: entry.selection.time,
                });
            }
        }
    }

    if mutations.is_empty() {
        None
    } else {
        Some((mutations, selections))
    }
}

fn graph_selection_scale_factor(drag: &GraphSelectionScaleDragState) -> f64 {
    match drag.axis {
        GraphSelectionScaleAxis::Time => {
            let min_time = drag.entries.iter().map(|entry| entry.selection.time).min().unwrap_or(0);
            let max_time =
                drag.entries.iter().map(|entry| entry.selection.time).max().unwrap_or(min_time);
            let (pivot, start) = match drag.edge {
                GraphSelectionScaleEdge::Min => (max_time, min_time),
                GraphSelectionScaleEdge::Max => (min_time, max_time),
            };
            let current = graph_time_from_x(
                drag.plot_rect,
                drag.time_min,
                drag.time_max,
                drag.pointer_pos.x,
            );
            scale_factor_from_axis(start as f64, pivot as f64, current as f64, 0.1, 10.0)
        }
        GraphSelectionScaleAxis::Value => {
            let min_value = drag
                .entries
                .iter()
                .map(|entry| entry.channel_value)
                .fold(f64::INFINITY, f64::min);
            let max_value = drag
                .entries
                .iter()
                .map(|entry| entry.channel_value)
                .fold(f64::NEG_INFINITY, f64::max);
            let (pivot, start) = match drag.edge {
                GraphSelectionScaleEdge::Min => (max_value, min_value),
                GraphSelectionScaleEdge::Max => (min_value, max_value),
            };
            let current = graph_value_from_y(
                drag.plot_rect,
                drag.value_min,
                drag.value_max,
                drag.pointer_pos.y,
            );
            scale_factor_from_axis(start, pivot, current, 0.1, 10.0)
        }
    }
}

fn graph_selection_scaled_point(
    entry: &SelectedGraphKeyframeData,
    drag: &GraphSelectionScaleDragState,
    factor: f64,
) -> (TimeTicks, f64) {
    match drag.axis {
        GraphSelectionScaleAxis::Time => {
            let min_time = drag.entries.iter().map(|entry| entry.selection.time).min().unwrap_or(0);
            let max_time =
                drag.entries.iter().map(|entry| entry.selection.time).max().unwrap_or(min_time);
            let pivot = match drag.edge {
                GraphSelectionScaleEdge::Min => max_time as f64,
                GraphSelectionScaleEdge::Max => min_time as f64,
            };
            let scaled = pivot + (entry.selection.time as f64 - pivot) * factor;
            (
                snap_time_ticks(scaled.round() as TimeTicks).max(0),
                entry.channel_value,
            )
        }
        GraphSelectionScaleAxis::Value => {
            let min_value = drag
                .entries
                .iter()
                .map(|entry| entry.channel_value)
                .fold(f64::INFINITY, f64::min);
            let max_value = drag
                .entries
                .iter()
                .map(|entry| entry.channel_value)
                .fold(f64::NEG_INFINITY, f64::max);
            let pivot = match drag.edge {
                GraphSelectionScaleEdge::Min => max_value,
                GraphSelectionScaleEdge::Max => min_value,
            };
            (
                entry.selection.time,
                pivot + (entry.channel_value - pivot) * factor,
            )
        }
    }
}

fn interpolations_for_target_speed(
    keyframes: &[mondrian_core::automation::Keyframe<f64>],
    index: usize,
    temporal_flags: KeyframeTemporalFlags,
    clip: &Clip,
    target_speed: f64,
) -> Option<(KeyframeInterpolation, KeyframeInterpolation)> {
    let keyframe = keyframes.get(index)?;
    let mut interp_in = keyframe.interp_in;
    let mut interp_out = keyframe.interp_out;

    if index > 0 {
        let previous = &keyframes[index - 1];
        interp_in = KeyframeInterpolation::Bezier(speed_handle_for_segment(
            previous,
            keyframe,
            clip,
            target_speed,
            handle_time_offset_in(keyframe.interp_in),
        ));
    }
    if let Some(next) = keyframes.get(index + 1) {
        interp_out = KeyframeInterpolation::Bezier(speed_handle_for_segment(
            keyframe,
            next,
            clip,
            target_speed,
            handle_time_offset_out(keyframe.interp_out),
        ));
    }

    if temporal_flags.continuous && !temporal_flags.broken_handles {
        if index > 0 && index + 1 < keyframes.len() {
            let previous = &keyframes[index - 1];
            let next = &keyframes[index + 1];
            interp_in = KeyframeInterpolation::Bezier(speed_handle_for_segment(
                previous,
                keyframe,
                clip,
                target_speed,
                handle_time_offset_in(keyframe.interp_in),
            ));
            interp_out = KeyframeInterpolation::Bezier(speed_handle_for_segment(
                keyframe,
                next,
                clip,
                target_speed,
                handle_time_offset_out(keyframe.interp_out),
            ));
        }
    }

    Some((interp_in, interp_out))
}

fn speed_handle_for_segment(
    start: &mondrian_core::automation::Keyframe<f64>,
    end: &mondrian_core::automation::Keyframe<f64>,
    clip: &Clip,
    target_speed: f64,
    time_offset: f64,
) -> BezierHandle {
    let dv = end.value - start.value;
    let dt_seconds = (end.time - start.time) as f64 / SUBFRAME_TICKS_PER_FRAME as f64
        * clip.position.time_base.to_f64();
    if dv.abs() < f64::EPSILON || dt_seconds.abs() < f64::EPSILON {
        BezierHandle { time_offset, value_offset: 0.0 }
    } else {
        let value_offset = (target_speed * dt_seconds / dv * time_offset).clamp(-2.0, 2.0);
        BezierHandle { time_offset, value_offset }
    }
}

fn handle_time_offset_in(interpolation: KeyframeInterpolation) -> f64 {
    match interpolation {
        KeyframeInterpolation::Bezier(handle) => handle.time_offset.clamp(-0.95, -0.05),
        _ => -1.0 / 3.0,
    }
}

fn handle_time_offset_out(interpolation: KeyframeInterpolation) -> f64 {
    match interpolation {
        KeyframeInterpolation::Bezier(handle) => handle.time_offset.clamp(0.05, 0.95),
        _ => 1.0 / 3.0,
    }
}

fn scale_factor_from_axis(
    start: f64,
    pivot: f64,
    current: f64,
    min_factor: f64,
    max_factor: f64,
) -> f64 {
    let denom = start - pivot;
    if denom.abs() < f64::EPSILON {
        1.0
    } else {
        ((current - pivot) / denom).clamp(min_factor, max_factor)
    }
}

fn snap_graph_time_delta(
    raw_delta: TimeTicks,
    anchor_times: &[TimeTicks],
    candidate_times: &[TimeTicks],
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
) -> (TimeTicks, Option<TimeTicks>) {
    if anchor_times.is_empty() || candidate_times.is_empty() || rect.width() <= 1.0 {
        return (raw_delta, None);
    }
    let threshold_ticks = (((time_max - time_min).max(1) as f32)
        * (tokens::timeline_drag_snap_pixels() / rect.width()))
    .ceil()
    .max(1.0) as TimeTicks;

    let mut best: Option<(TimeTicks, TimeTicks)> = None;
    for anchor_time in anchor_times {
        let moved = (*anchor_time + raw_delta).max(0);
        for candidate in candidate_times {
            let delta = candidate.saturating_sub(*anchor_time);
            let diff = (candidate - moved).abs();
            if diff > threshold_ticks {
                continue;
            }
            if best.as_ref().is_none_or(|(_, best_diff)| diff < *best_diff) {
                best = Some((delta, diff));
            }
        }
    }

    if let Some((delta, _)) = best {
        let snapped_anchor = anchor_times[0].saturating_add(delta).max(0);
        (delta, Some(snapped_anchor))
    } else {
        (raw_delta, None)
    }
}

fn snap_graph_value_delta(
    raw_delta: f64,
    anchor_values: &[f64],
    candidate_values: &[f64],
    rect: Rect,
    value_min: f64,
    value_max: f64,
) -> (f64, Option<f64>) {
    if anchor_values.is_empty() || candidate_values.is_empty() || rect.height() <= 1.0 {
        return (raw_delta, None);
    }
    let threshold_value = ((value_max - value_min).abs()
        * (tokens::timeline_drag_snap_pixels() / rect.height()).max(0.0) as f64)
        .max(1e-6);

    let mut best: Option<(f64, f64)> = None;
    for anchor_value in anchor_values {
        let moved = *anchor_value + raw_delta;
        for candidate in candidate_values {
            let delta = *candidate - *anchor_value;
            let diff = (*candidate - moved).abs();
            if diff > threshold_value {
                continue;
            }
            if best.as_ref().is_none_or(|(_, best_diff)| diff < *best_diff) {
                best = Some((delta, diff));
            }
        }
    }

    if let Some((delta, _)) = best {
        (delta, Some(anchor_values[0] + delta))
    } else {
        (raw_delta, None)
    }
}

fn snap_graph_delta_ticks(delta_ticks: TimeTicks) -> TimeTicks {
    ((delta_ticks as f64 / SUBFRAME_TICKS_PER_FRAME as f64).round() as i64)
        * SUBFRAME_TICKS_PER_FRAME
}

fn snap_time_ticks(time_ticks: TimeTicks) -> TimeTicks {
    ((time_ticks as f64 / SUBFRAME_TICKS_PER_FRAME as f64).round() as i64)
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
    if matches!(keyframe.interp_in, KeyframeInterpolation::Bezier(_)) && index > 0 {
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
    if matches!(keyframe.interp_out, KeyframeInterpolation::Bezier(_))
        && index + 1 < keyframes.len()
    {
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
        KeyframeInterpolation::Linear => BezierHandle { time_offset: 1.0 / 3.0, value_offset: 0.0 },
        KeyframeInterpolation::Hold => BezierHandle { time_offset: 0.0, value_offset: 0.0 },
    }
}

fn handle_from_in(interpolation: KeyframeInterpolation) -> BezierHandle {
    match interpolation {
        KeyframeInterpolation::Bezier(handle) => handle,
        KeyframeInterpolation::Linear => {
            BezierHandle { time_offset: -1.0 / 3.0, value_offset: 0.0 }
        }
        KeyframeInterpolation::Hold => BezierHandle { time_offset: 0.0, value_offset: 0.0 },
    }
}

fn preview_handle_position(
    kind: GraphHandleKind,
    pointer_pos: Pos2,
    keyframes: &[mondrian_core::automation::Keyframe<f64>],
    index: usize,
    temporal_flags: KeyframeTemporalFlags,
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
        temporal_flags,
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
    temporal_flags: KeyframeTemporalFlags,
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
            if temporal_flags.continuous && !temporal_flags.broken_handles {
                if let Some(previous) = keyframes.get(index.wrapping_sub(1)).filter(|_| index > 0) {
                    interp_in = KeyframeInterpolation::Bezier(mirror_handle_for_in(
                        previous,
                        keyframe,
                        next,
                        BezierHandle { time_offset: dx as f64, value_offset: dy as f64 },
                    ));
                }
            }
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
            if temporal_flags.continuous && !temporal_flags.broken_handles {
                if let Some(next) = keyframes.get(index + 1) {
                    interp_out = KeyframeInterpolation::Bezier(mirror_handle_for_out(
                        previous,
                        keyframe,
                        next,
                        BezierHandle { time_offset: dx as f64, value_offset: dy as f64 },
                    ));
                }
            }
        }
        _ => return None,
    }
    Some((interp_in, interp_out))
}

fn mirror_handle_for_in(
    previous: &mondrian_core::automation::Keyframe<f64>,
    current: &mondrian_core::automation::Keyframe<f64>,
    next: &mondrian_core::automation::Keyframe<f64>,
    out_handle: BezierHandle,
) -> BezierHandle {
    let out_slope = actual_slope_from_out(current, next, out_handle);
    let time_offset = -out_handle.time_offset.abs().clamp(0.05, 0.95);
    let dt_prev = (current.time - previous.time).max(1) as f64;
    let dv_prev = current.value - previous.value;
    let value_offset = if dv_prev.abs() < f64::EPSILON {
        0.0
    } else {
        (out_slope * time_offset * dt_prev / dv_prev).clamp(-2.0, 2.0)
    };
    BezierHandle { time_offset, value_offset }
}

fn mirror_handle_for_out(
    previous: &mondrian_core::automation::Keyframe<f64>,
    current: &mondrian_core::automation::Keyframe<f64>,
    next: &mondrian_core::automation::Keyframe<f64>,
    in_handle: BezierHandle,
) -> BezierHandle {
    let in_slope = actual_slope_from_in(previous, current, in_handle);
    let time_offset = in_handle.time_offset.abs().clamp(0.05, 0.95);
    let dt_next = (next.time - current.time).max(1) as f64;
    let dv_next = next.value - current.value;
    let value_offset = if dv_next.abs() < f64::EPSILON {
        0.0
    } else {
        (in_slope * time_offset * dt_next / dv_next).clamp(-2.0, 2.0)
    };
    BezierHandle { time_offset, value_offset }
}

fn actual_slope_from_out(
    current: &mondrian_core::automation::Keyframe<f64>,
    next: &mondrian_core::automation::Keyframe<f64>,
    handle: BezierHandle,
) -> f64 {
    let dt = (next.time - current.time).max(1) as f64;
    let dv = next.value - current.value;
    let dx = handle.time_offset.abs().max(0.05);
    if dv.abs() < f64::EPSILON {
        0.0
    } else {
        handle.value_offset * dv / (dx * dt)
    }
}

fn actual_slope_from_in(
    previous: &mondrian_core::automation::Keyframe<f64>,
    current: &mondrian_core::automation::Keyframe<f64>,
    handle: BezierHandle,
) -> f64 {
    let dt = (current.time - previous.time).max(1) as f64;
    let dv = current.value - previous.value;
    let dx = handle.time_offset.abs().max(0.05);
    if dv.abs() < f64::EPSILON {
        0.0
    } else {
        handle.value_offset * dv / (dx * dt)
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::automation::{AnimatedProperty, Keyframe, PropertyDescriptor};
    use mondrian_core::types::{ClipId, KeyframeId};
    use std::panic::{catch_unwind, AssertUnwindSafe};

    fn graph_rect() -> Rect {
        Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(400.0, 240.0))
    }

    fn sample_property() -> AnimatedProperty {
        let mut property = AnimatedProperty::from_descriptor(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));
        property
            .apply_mutation(PropertyMutation::SetKeyframe {
                path: "transform.opacity".to_string(),
                keyframe: Keyframe::linear(0, PropertyValue::Float(0.0)),
            })
            .expect("set first keyframe");
        property
            .apply_mutation(PropertyMutation::SetKeyframe {
                path: "transform.opacity".to_string(),
                keyframe: Keyframe {
                    id: KeyframeId::new(),
                    time: SUBFRAME_TICKS_PER_FRAME * 10,
                    value: PropertyValue::Float(0.5),
                    interp_in: KeyframeInterpolation::Bezier(BezierHandle {
                        time_offset: -0.25,
                        value_offset: -0.1,
                    }),
                    interp_out: KeyframeInterpolation::Bezier(BezierHandle {
                        time_offset: 0.3,
                        value_offset: 0.12,
                    }),
                    temporal_flags: KeyframeTemporalFlags {
                        auto_bezier: false,
                        continuous: true,
                        broken_handles: false,
                    },
                },
            })
            .expect("set middle keyframe");
        property
            .apply_mutation(PropertyMutation::SetKeyframe {
                path: "transform.opacity".to_string(),
                keyframe: Keyframe::linear(
                    SUBFRAME_TICKS_PER_FRAME * 20,
                    PropertyValue::Float(1.0),
                ),
            })
            .expect("set last keyframe");
        property
    }

    #[test]
    fn selected_active_keyframe_helpers_handle_empty_selection() {
        let property = sample_property();
        let channel = property.channel(0).expect("channel");
        let selected = Vec::<AnimationKeyframeSelection>::new();

        let result = catch_unwind(AssertUnwindSafe(|| {
            (
                selected_keyframe_ids(channel, &selected),
                selected_active_keyframe_id(channel, &selected),
                if selected.len() == 1 {
                    selected.first().map(|selection| selection.time)
                } else {
                    None
                },
            )
        }))
        .expect("helpers should not panic");

        assert!(result.0.is_empty());
        assert_eq!(result.1, None);
        assert_eq!(result.2, None);
    }

    #[test]
    fn graph_drag_property_mutations_preserve_bezier_editing_path() {
        let drag = GraphKeyframeDragState {
            clip_id: ClipId::new(),
            path: "transform.opacity".to_string(),
            channel_index: 0,
            start_pointer_pos: Pos2::new(100.0, 120.0),
            pointer_pos: Pos2::new(140.0, 80.0),
            anchors: vec![GraphKeyframeDragAnchor {
                time: SUBFRAME_TICKS_PER_FRAME * 10,
                value: 0.5,
            }],
        };

        let mutations = graph_keyframe_drag_property_mutations(
            &drag,
            graph_rect(),
            0,
            SUBFRAME_TICKS_PER_FRAME * 20,
            0.0,
            1.0,
            &[],
            &[],
        )
        .expect("mutations");

        assert!(mutations.iter().any(|mutation| matches!(
            mutation,
            PropertyMutation::UpdateChannelKeyframeValue { .. }
        )));
        assert!(!mutations
            .iter()
            .any(|mutation| matches!(mutation, PropertyMutation::WriteChannels { .. })));
    }

    #[test]
    fn graph_drag_preview_property_keeps_continuous_handles_aligned() {
        let property = sample_property();
        let drag = GraphKeyframeDragState {
            clip_id: ClipId::new(),
            path: "transform.opacity".to_string(),
            channel_index: 0,
            start_pointer_pos: Pos2::new(200.0, 120.0),
            pointer_pos: Pos2::new(200.0, 70.0),
            anchors: vec![GraphKeyframeDragAnchor {
                time: SUBFRAME_TICKS_PER_FRAME * 10,
                value: 0.5,
            }],
        };

        let preview = graph_keyframe_preview_property(
            &property,
            &drag,
            graph_rect(),
            0,
            SUBFRAME_TICKS_PER_FRAME * 20,
            0.0,
            1.0,
            &[],
            &[],
        )
        .expect("preview property");

        let channel = preview.channel(0).expect("channel");
        let current = channel.keyframe_at(SUBFRAME_TICKS_PER_FRAME * 10).expect("current");
        let current_index = channel
            .keyframes()
            .iter()
            .position(|keyframe| keyframe.time == current.time)
            .expect("current index");
        let point = graph_point_for_keyframe(
            graph_rect(),
            0,
            SUBFRAME_TICKS_PER_FRAME * 20,
            0.0,
            1.0,
            current.time,
            current.value,
        );
        let handles = graph_handles_for_keyframe(
            channel.keyframes(),
            current_index,
            graph_rect(),
            0,
            SUBFRAME_TICKS_PER_FRAME * 20,
            0.0,
            1.0,
        );
        let in_handle = handles
            .iter()
            .find(|handle| handle.kind == GraphHandleKind::In)
            .expect("in handle")
            .position;
        let out_handle = handles
            .iter()
            .find(|handle| handle.kind == GraphHandleKind::Out)
            .expect("out handle")
            .position;
        let cross = (in_handle.x - point.x) * (out_handle.y - point.y)
            - (in_handle.y - point.y) * (out_handle.x - point.x);
        assert!(cross.abs() < 1e-3);
    }
}
