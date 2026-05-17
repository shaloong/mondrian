use std::collections::{HashMap, HashSet};

use crate::{
    app::{AnimationBubbleHost, AnimationKeyframeSelection, AppState},
    ui::{
        animation_groups::{
            property_display_name, property_group_meta, property_order,
            qualified_property_display_name, AnimationGroupKind, AnimationGroupMeta,
        },
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
    types::{ClipId, ColorSpace, EffectId, KeyframeId, Rational, TimeCode},
};
use mondrian_timeline::{
    clip::{AlphaInterpretation, Clip, ClipKind},
    sequence::{FieldOrder, PixelAspectRatio},
};

#[derive(Default)]
pub struct EffectControlsPanel {
    text_edit_buffers: HashMap<(ClipId, String), String>,
    inspector_group_collapsed: HashMap<(ClipId, String), bool>,
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

struct InspectorGroup<'a> {
    meta: AnimationGroupMeta,
    properties: Vec<(&'a str, &'a mondrian_core::automation::AnimatedProperty)>,
}

struct GroupHeaderRowResponse {
    row: egui::Response,
    toggle_effect: Option<egui::Response>,
    delete_effect: Option<egui::Response>,
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
        let subtitle = selected_clip
            .and_then(|selection| {
                app.clip_snapshot(selection).map(|clip| {
                    let clip_label =
                        clip.label.as_deref().filter(|label| !label.is_empty()).unwrap_or(
                            if clip.is_adjustment_layer() {
                                "调整图层"
                            } else {
                                "未命名片段"
                            },
                        );
                    let clip_role = if clip.is_adjustment_layer() {
                        "调整图层"
                    } else if selection.is_video_track {
                        "视频"
                    } else {
                        "音频"
                    };
                    format!("{clip_role} · {clip_label}")
                })
            })
            .unwrap_or_else(|| "（未选中片段）".to_string());

        self.draw_panel_switcher(ui, &subtitle);
        ui.add_space(tokens::panel_gap() * 0.65);

        let Some(selection) = selected_clip else {
            return;
        };

        let Some(clip) = app.clip_snapshot(selection) else {
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
                ui.label(
                    RichText::new(format!("读取片段属性失败：{err}"))
                        .font(typography::body())
                        .color(palette::status_error()),
                );
                return;
            }
        };

        let mut inspector_groups = collect_inspector_groups(&property_bag);
        // Sort effect groups by insertion order. Use effect UUID (from group ID
        // suffix) to map each group to its position in clip.effects.
        let effect_index: std::collections::HashMap<String, usize> = clip
            .effects
            .iter()
            .enumerate()
            .map(|(i, e)| (e.id.to_string(), i))
            .collect();
        inspector_groups.sort_by(|a, b| {
            let is_fx_a = a.meta.allows_effect_controls;
            let is_fx_b = b.meta.allows_effect_controls;
            // Fixed builtins: use original meta.order (0=Motion, 1=Opacity, 2=TimeRemap).
            // Effects: use position in clip.effects via UUID lookup (offset above builtins).
            let ord_a = if is_fx_a {
                // Extract UUID tail from group ID: "effect.<slug>.<uuid8>" → "<uuid8>"
                let uuid = a.meta.id.rsplit('.').next().unwrap_or("0");
                effect_index.iter()
                    .find(|(full_id, _)| full_id.starts_with(uuid))
                    .map(|(_, &idx)| idx + 10)
                    .unwrap_or(0)
            } else {
                a.meta.order
            };
            let ord_b = if is_fx_b {
                let uuid = b.meta.id.rsplit('.').next().unwrap_or("0");
                effect_index.iter()
                    .find(|(full_id, _)| full_id.starts_with(uuid))
                    .map(|(_, &idx)| idx + 10)
                    .unwrap_or(0)
            } else {
                b.meta.order
            };
            ord_a.cmp(&ord_b)
                .then_with(|| a.meta.title.cmp(&b.meta.title))
        });
        if app.active_animation_property_path(selection.clip_id).is_none() {
            if let Some((path, _)) = inspector_groups
                .iter()
                .flat_map(|group| group.properties.iter().copied())
                .find(|(_, property)| property.descriptor.is_animatable)
            {
                app.set_active_animation_property(selection.clip_id, path.to_string());
            }
        }

        match self.view {
            EffectControlsView::Inspector => {
                if !inspector_groups.is_empty() {
                    ui.add_space(tokens::panel_gap() * 0.4);
                    ui.separator();
                    ui.add_space(tokens::panel_gap() * 0.35);
                }
                // Track group Y positions (screen-space) for effect drag insertion
                let mut group_tops: Vec<f32> = Vec::new();
                let mut group_bottoms: Vec<f32> = Vec::new();
                let mut is_effect_group: Vec<bool> = Vec::new();
                egui::ScrollArea::vertical()
                    .id_salt("effect_controls_scroll")
                    .show(ui, |ui| {
                        for (index, group) in inspector_groups.iter().enumerate() {
                            let top = ui.next_widget_position().y;
                            group_tops.push(top);
                            is_effect_group.push(group.meta.allows_effect_controls);
                            if index > 0 {
                                ui.add_space(tokens::panel_gap() * 0.4);
                                ui.separator();
                                ui.add_space(tokens::panel_gap() * 0.35);
                            }
                            self.draw_property_group(ui, app, selection, current_time, group);
                            group_bottoms.push(ui.next_widget_position().y);
                        }
                    });
                // Effect drag insertion indicator
                let effect_drag_id = egui::Id::new(super::effect_library_panel::EFFECT_DRAG_ID);
                if let Some(effect_type) = ui.ctx().data_mut(|d| {
                    d.get_persisted::<mondrian_effects::EffectType>(effect_drag_id)
                }) {
                    if let Some(ptr) = ui.input(|i| i.pointer.interact_pos()) {
                        let panel_r = ui.max_rect();
                        if panel_r.contains(ptr) && !group_tops.is_empty() {
                            // Build (y, insert_index) pairs at valid effect boundaries
                            let mut slots: Vec<(f32, usize)> = Vec::new();
                            for i in 0..group_tops.len() {
                                if is_effect_group[i] {
                                    // Before this effect group → insert at its effect index
                                    let fx_idx = is_effect_group[..=i].iter().filter(|&&e| e).count() - 1;
                                    slots.push((group_tops[i], fx_idx));
                                    // After this group = midpoint to next, insert at fx_idx + 1
                                    if i + 1 < group_tops.len() {
                                        let mid = (group_bottoms[i] + group_tops[i + 1]) * 0.5;
                                        slots.push((mid, fx_idx + 1));
                                    }
                                }
                            }
                            // Append at end — clamp y to visible area
                            let total_fx = is_effect_group.iter().filter(|&&e| e).count();
                            let raw_end = group_bottoms.last().copied().unwrap_or(0.0) + 4.0;
                            let end_y = raw_end.min(panel_r.bottom() - 2.0);
                            slots.push((end_y, total_fx));
                            // Snap to nearest slot
                            let (slot_y, insert_idx) = slots
                                .iter()
                                .min_by(|a, b| (ptr.y - a.0).abs().partial_cmp(&(ptr.y - b.0).abs()).unwrap())
                                .copied()
                                .unwrap_or((end_y, total_fx));
                            // Draw snapped line
                            ui.painter().line_segment(
                                [egui::pos2(panel_r.left(), slot_y), egui::pos2(panel_r.right(), slot_y)],
                                egui::Stroke::new(2.0, palette::interaction_highlight()),
                            );
                            // Apply on release with correct index
                            if ui.input(|i| i.pointer.primary_released()) {
                                if selection.is_video_track {
                                    let _ = app.insert_effect_at_index(selection, effect_type.clone(), insert_idx);
                                }
                                ui.ctx().data_mut(|d| {
                                    d.remove::<mondrian_effects::EffectType>(effect_drag_id);
                                });
                            }
                        }
                    }
                }
            }
            EffectControlsView::Graph => {
                let animatable_properties = inspector_groups
                    .iter()
                    .flat_map(|group| group.properties.iter().copied())
                    .filter(|(_, property)| property.descriptor.is_animatable)
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

    fn draw_panel_switcher(&mut self, ui: &mut Ui, subtitle: &str) {
        let subtitle_height = ui
            .painter()
            .layout_no_wrap(
                "视频 · 占位".to_string(),
                typography::body_small(),
                palette::text_muted(),
            )
            .size()
            .y;
        let row_height = subtitle_height.max(28.0);
        let controls_width = 116.0;
        let total_width = ui.available_width();
        let (row_rect, _) =
            ui.allocate_exact_size(Vec2::new(total_width, row_height), Sense::hover());
        let controls_rect = Rect::from_min_max(
            Pos2::new(row_rect.right() - controls_width, row_rect.top()),
            row_rect.right_bottom(),
        );
        let label_rect = Rect::from_min_max(
            row_rect.min,
            Pos2::new(
                (controls_rect.left() - 8.0).max(row_rect.left()),
                row_rect.bottom(),
            ),
        );

        ui.scope_builder(egui::UiBuilder::new().max_rect(label_rect), |ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_max_width(label_rect.width());
                let _subtitle_response = ui.add(
                    egui::Label::new(
                        RichText::new(subtitle)
                            .font(typography::body_small())
                            .color(palette::text_muted()),
                    )
                    .truncate(),
                );
            });
        });

        ui.scope_builder(egui::UiBuilder::new().max_rect(controls_rect), |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.set_width(controls_rect.width());
                let _ = super::widgets::segmented_control(
                    ui,
                    &mut self.view,
                    &[
                        super::widgets::SegmentedOption {
                            value: EffectControlsView::Inspector,
                            label: "属性".to_string(),
                        },
                        super::widgets::SegmentedOption {
                            value: EffectControlsView::Graph,
                            label: "曲线".to_string(),
                        },
                    ],
                );
            });
        });
    }

    fn draw_property_group(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        selection: SelectedClipRef,
        current_time: TimeCode,
        group: &InspectorGroup<'_>,
    ) {
        if group.properties.is_empty() {
            return;
        }

        let collapse_key = (selection.clip_id, group.meta.id.clone());
        let mut collapsed = *self.inspector_group_collapsed.get(&collapse_key).unwrap_or(&false);

        let effect_id = group_effect_id(group);
        let enabled_state = effect_id
            .and_then(|id| app.clip_snapshot(selection).and_then(|clip| clip.effect_enabled(id)));
        let header_response = draw_group_header_row(
            ui,
            &group.meta,
            collapsed,
            enabled_state,
            effect_id.is_some(),
            effect_id.is_some(),
        );
        let action_clicked =
            header_response.toggle_effect.as_ref().is_some_and(egui::Response::clicked)
                || header_response.delete_effect.as_ref().is_some_and(egui::Response::clicked);
        if header_response.row.clicked() && !action_clicked {
            collapsed = !collapsed;
        }
        if let (Some(effect_id), Some(toggle)) = (effect_id, header_response.toggle_effect.as_ref())
        {
            if toggle.clicked() {
                let next_enabled = !enabled_state.unwrap_or(true);
                let _ = app.set_clip_effect_enabled(selection, effect_id, next_enabled);
            }
        }
        if let (Some(effect_id), Some(delete)) = (effect_id, header_response.delete_effect.as_ref())
        {
            if delete.clicked() {
                let _ = app.remove_effect_from_clip(selection, effect_id);
                self.inspector_group_collapsed.remove(&collapse_key);
                return;
            }
        }

        if !collapsed {
            ui.add_space(tokens::panel_gap() * 0.2);
            Grid::new(format!("effect_controls_group_grid_{}", group.meta.id))
                .num_columns(3)
                .spacing([8.0, 8.0])
                .striped(false)
                .show(ui, |ui| {
                    for (path, property) in &group.properties {
                        self.draw_property_row(ui, app, selection, path, property, current_time);
                    }
                });
        }
        self.inspector_group_collapsed.insert(collapse_key, collapsed);
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
            super::widgets::empty_state(
                ui,
                Some(theme::UiIcon::Info),
                "当前片段没有可动画属性",
                "在特效面板中为片段添加特效后将显示可编辑属性",
            );
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

        ui.horizontal(|ui| {
            ComboBox::from_id_salt((selection.clip_id, "graph_property"))
                .selected_text(qualified_property_display_name(
                    active_path.as_str(),
                    property,
                ))
                .width(140.0)
                .show_ui(ui, |ui| {
                    for (path, candidate) in properties {
                        if ui
                            .selectable_label(
                                *path == active_path.as_str(),
                                qualified_property_display_name(path, candidate),
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
        ui.add_space(tokens::panel_gap() * 0.7);

        let (rect, _response) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), tokens::graph_editor_height()),
            Sense::click_and_drag(),
        );
        let painter = ui.painter_at(rect);

        let ruler_height = 22.0;
        let axis_height = 22.0;
        let left_axis_width = 50.0;
        let outer_padding = 12.0;
        let right_axis_padding = 22.0;
        let plot_rect = Rect::from_min_max(
            Pos2::new(
                rect.left() + left_axis_width,
                rect.top() + ruler_height + outer_padding * 0.5,
            ),
            Pos2::new(
                rect.right() - outer_padding - right_axis_padding,
                rect.bottom() - axis_height - outer_padding,
            ),
        );
        let ruler_rect = Rect::from_min_max(
            Pos2::new(plot_rect.left(), rect.top() + 6.0),
            Pos2::new(plot_rect.right(), plot_rect.top() - 6.0),
        );
        let axis_rect = Rect::from_min_max(
            Pos2::new(plot_rect.left(), plot_rect.bottom() + 6.0),
            Pos2::new(plot_rect.right(), rect.bottom() - 6.0),
        );
        let plot_rounding = theme::corner_radius(tokens::section_rounding());
        painter.rect_filled(plot_rect, plot_rounding, palette::bg_surface_raised());
        painter.rect_stroke(
            plot_rect,
            plot_rounding,
            Stroke::new(tokens::border_standard(), palette::border_subtle()),
            egui::StrokeKind::Inside,
        );
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
        let time_ticks = graph_time_axis_ticks(
            time_min,
            time_max,
            clip.position.time_base,
            plot_rect.width(),
        );
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
        painter.line_segment(
            [
                Pos2::new(ruler_rect.left(), ruler_rect.bottom()),
                Pos2::new(ruler_rect.right(), ruler_rect.bottom()),
            ],
            Stroke::new(
                tokens::border_standard(),
                palette::border_subtle().gamma_multiply(0.7),
            ),
        );
        for row in 0..=4 {
            let t = row as f32 / 4.0;
            let y = egui::lerp(plot_rect.top()..=plot_rect.bottom(), t);
            painter.line_segment(
                [
                    Pos2::new(plot_rect.left(), y),
                    Pos2::new(plot_rect.right(), y),
                ],
                Stroke::new(
                    tokens::border_standard(),
                    palette::border_subtle().gamma_multiply(0.5),
                ),
            );
        }
        let time_label_positions = graph_time_label_positions(
            &painter,
            &time_ticks,
            plot_rect,
            time_min,
            time_max,
            clip.position.time_base,
        );
        for time in &time_ticks {
            let x = graph_x_for_time(plot_rect, time_min, time_max, *time);
            painter.line_segment(
                [
                    Pos2::new(x, plot_rect.top()),
                    Pos2::new(x, plot_rect.bottom()),
                ],
                Stroke::new(
                    tokens::border_standard(),
                    palette::border_subtle().gamma_multiply(0.35),
                ),
            );
            painter.line_segment(
                [
                    Pos2::new(x, ruler_rect.bottom() - 6.0),
                    Pos2::new(x, ruler_rect.bottom()),
                ],
                Stroke::new(
                    tokens::border_standard(),
                    palette::border_subtle().gamma_multiply(0.6),
                ),
            );
        }
        for label in &time_label_positions {
            painter.text(
                Pos2::new(label.x, axis_rect.top() + 2.0),
                egui::Align2::CENTER_TOP,
                &label.text,
                typography::body_small(),
                palette::text_muted(),
            );
        }
        painter.text(
            Pos2::new(rect.left() + 8.0, plot_rect.top()),
            egui::Align2::LEFT_TOP,
            format!("{:.2}", y_labels[0]),
            typography::body_small(),
            palette::text_muted(),
        );
        painter.text(
            Pos2::new(rect.left() + 8.0, plot_rect.center().y),
            egui::Align2::LEFT_CENTER,
            format!("{:.2}", y_labels[1]),
            typography::body_small(),
            palette::text_muted(),
        );
        painter.text(
            Pos2::new(rect.left() + 8.0, plot_rect.bottom()),
            egui::Align2::LEFT_BOTTOM,
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
                Stroke::new(
                    tokens::border_standard() * 1.2,
                    palette::border_emphasis().gamma_multiply(0.85),
                ),
            );
        }

        let selected_times =
            selected_on_active.iter().map(|selected| selected.time).collect::<Vec<_>>();
        let time_collision_candidates = channel
            .keyframes()
            .iter()
            .map(|keyframe| keyframe.time)
            .filter(|time| !selected_times.contains(time))
            .collect::<Vec<_>>();
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
                    &time_collision_candidates,
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
                    &time_collision_candidates,
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
                Stroke::new(
                    tokens::border_standard(),
                    palette::timeline_playhead().gamma_multiply(0.85),
                ),
            );
        }
        if let Some(snapped_value) = snap_guides.value {
            let guide_y = graph_y_for_value(plot_rect, value_min, value_max, snapped_value);
            painter.line_segment(
                [
                    Pos2::new(plot_rect.left(), guide_y),
                    Pos2::new(plot_rect.right(), guide_y),
                ],
                Stroke::new(
                    tokens::border_standard(),
                    palette::interaction_highlight().gamma_multiply(0.75),
                ),
            );
        }

        if current_time_ticks >= time_min && current_time_ticks <= time_max {
            let playhead_x = graph_x_for_time(plot_rect, time_min, time_max, current_time_ticks);
            painter.line_segment(
                [
                    Pos2::new(playhead_x, plot_rect.top()),
                    Pos2::new(playhead_x, plot_rect.bottom()),
                ],
                Stroke::new(
                    tokens::border_standard() * 1.2,
                    palette::timeline_playhead(),
                ),
            );
            painter.line_segment(
                [
                    Pos2::new(playhead_x, ruler_rect.top()),
                    Pos2::new(playhead_x, ruler_rect.bottom()),
                ],
                Stroke::new(
                    tokens::border_standard() * 1.2,
                    palette::timeline_playhead(),
                ),
            );
            let badge_text = format_graph_time_label(current_time_ticks, clip.position.time_base);
            let badge_size = painter
                .layout_no_wrap(
                    badge_text.clone(),
                    typography::body_small(),
                    palette::text_primary(),
                )
                .size();
            let badge_rect = Rect::from_min_size(
                Pos2::new(
                    (playhead_x - badge_size.x * 0.5 - 6.0)
                        .clamp(ruler_rect.left(), ruler_rect.right() - badge_size.x - 12.0),
                    ruler_rect.top() - 1.0,
                ),
                Vec2::new(badge_size.x + 12.0, ruler_rect.height() - 4.0),
            );
            painter.rect_filled(
                badge_rect,
                egui::CornerRadius::same(tokens::badge_rounding().round() as u8),
                palette::bg_surface_active(),
            );
            painter.text(
                badge_rect.center(),
                egui::Align2::CENTER_CENTER,
                badge_text,
                typography::body_small(),
                palette::text_primary(),
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
                        Stroke::new(
                            tokens::border_standard(),
                            graph_channel_color(channel_index).gamma_multiply(0.7),
                        ),
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
                        Stroke::new(tokens::border_standard(), palette::interaction_highlight()),
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
                    &time_collision_candidates,
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
            (&selected_points, &handle_points),
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
            .zip(new_times)
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
            theme::corner_radius(tokens::badge_rounding()),
            Stroke::new(
                tokens::border_standard(),
                palette::interaction_highlight().gamma_multiply(0.55),
            ),
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
                theme::corner_radius(tokens::border_standard() * 2.0),
                Stroke::new(tokens::border_standard(), palette::text_primary()),
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
        point_sets: (&[Pos2], &[Pos2]),
        plot_rect: Rect,
        active_path: &str,
        channel_index: usize,
        selected_on_active: &[AnimationKeyframeSelection],
        selected_keyframe_time: Option<TimeTicks>,
        selected_temporal_flags: Option<KeyframeTemporalFlags>,
        graph_mode: GraphEditorMode,
        app: &mut AppState,
    ) {
        let (selected_points, handle_points) = point_sets;
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
                    theme::corner_radius(tokens::border_standard() * 2.0),
                    Stroke::new(
                        tokens::border_standard() * 1.2,
                        palette::interaction_highlight(),
                    ),
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

    #[allow(dead_code)]
    fn draw_media_interpretation(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        selection: SelectedClipRef,
        clip: &Clip,
    ) {
        if clip.kind != ClipKind::Media {
            return;
        }

        let mut interpretation = clip.interpretation.clone();
        let before = interpretation.clone();
        ui.label(
            RichText::new("素材解释")
                .font(typography::body())
                .color(palette::text_primary()),
        );
        ui.add_space(tokens::spacing_xs());

        Grid::new(("media_interpretation_grid", selection.clip_id))
            .num_columns(2)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                ui.label(RichText::new("色彩空间").font(typography::body_small()));
                ComboBox::from_id_salt(("media_interpret_color", selection.clip_id))
                    .selected_text(match interpretation.color_space_override {
                        Some(color_space) => color_space_label(color_space),
                        None => "自动",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut interpretation.color_space_override, None, "自动");
                        for color_space in color_space_options() {
                            ui.selectable_value(
                                &mut interpretation.color_space_override,
                                Some(color_space),
                                color_space_label(color_space),
                            );
                        }
                    });
                ui.end_row();

                ui.label(RichText::new("Alpha").font(typography::body_small()));
                ComboBox::from_id_salt(("media_interpret_alpha", selection.clip_id))
                    .selected_text(alpha_interpretation_label(interpretation.alpha))
                    .show_ui(ui, |ui| {
                        for alpha in [
                            AlphaInterpretation::Straight,
                            AlphaInterpretation::Premultiplied,
                            AlphaInterpretation::Ignore,
                        ] {
                            ui.selectable_value(
                                &mut interpretation.alpha,
                                alpha,
                                alpha_interpretation_label(alpha),
                            );
                        }
                    });
                ui.end_row();

                ui.label(RichText::new("像素长宽比").font(typography::body_small()));
                ComboBox::from_id_salt(("media_interpret_par", selection.clip_id))
                    .selected_text(match interpretation.pixel_aspect_ratio_override {
                        Some(par) => pixel_aspect_ratio_label(par),
                        None => "跟随素材",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut interpretation.pixel_aspect_ratio_override,
                            None,
                            "跟随素材",
                        );
                        for par in [
                            PixelAspectRatio::Square,
                            PixelAspectRatio::D1DvNtsc,
                            PixelAspectRatio::D1DvNtscWidescreen,
                            PixelAspectRatio::D1DvPal,
                            PixelAspectRatio::D1DvPalWidescreen,
                            PixelAspectRatio::Anamorphic2x,
                            PixelAspectRatio::HdAnamorphic1080,
                            PixelAspectRatio::DvcproHd,
                            PixelAspectRatio::Unknown,
                        ] {
                            ui.selectable_value(
                                &mut interpretation.pixel_aspect_ratio_override,
                                Some(par),
                                pixel_aspect_ratio_label(par),
                            );
                        }
                    });
                ui.end_row();

                ui.label(RichText::new("场").font(typography::body_small()));
                ComboBox::from_id_salt(("media_interpret_field", selection.clip_id))
                    .selected_text(match interpretation.field_order_override {
                        Some(field_order) => field_order_label(field_order),
                        None => "跟随素材",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut interpretation.field_order_override,
                            None,
                            "跟随素材",
                        );
                        for field_order in [
                            FieldOrder::Progressive,
                            FieldOrder::UpperFirst,
                            FieldOrder::LowerFirst,
                        ] {
                            ui.selectable_value(
                                &mut interpretation.field_order_override,
                                Some(field_order),
                                field_order_label(field_order),
                            );
                        }
                    });
                ui.end_row();

                ui.label(RichText::new("帧率").font(typography::body_small()));
                ComboBox::from_id_salt(("media_interpret_fps", selection.clip_id))
                    .selected_text(match interpretation.frame_rate_override {
                        Some(frame_rate) => frame_rate_label(frame_rate),
                        None => "跟随素材".to_string(),
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut interpretation.frame_rate_override,
                            None,
                            "跟随素材",
                        );
                        for frame_rate in Rational::SEQUENCE_FRAME_RATES {
                            ui.selectable_value(
                                &mut interpretation.frame_rate_override,
                                Some(frame_rate),
                                frame_rate_label(frame_rate),
                            );
                        }
                    });
                ui.end_row();
            });

        if interpretation != before {
            let _ = app
                .set_clip_media_interpretation(selection, interpretation)
                .map_err(|err| app.set_status_hint(format!("解释素材失败：{err}"), true));
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
                let timer_response = theme::icon_ghost_toggle_button(
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

        let label_response = ui.selectable_label(
            is_active_property,
            RichText::new(property_display_name(property))
                .font(typography::body_small())
                .color(if is_active_property {
                    palette::text_primary()
                } else {
                    palette::text_muted()
                }),
        );
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
            let add_resp = theme::icon_ghost_toggle_button(
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
                let mut color = *value;
                let picker_resp = super::color_picker::color_picker_button(
                    ui,
                    &mut color,
                    super::color_picker::ColorPickerVariant::Inline,
                );
                if picker_resp.changed {
                    self.commit_value(
                        app,
                        selection,
                        path,
                        PropertyValue::Color(color),
                        interpolation,
                        is_animatable,
                    );
                }
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
    let menu_width = 18.0 + 10.0 + text_width + 16.0;
    ui.spacing_mut().menu_width = menu_width;
    ui.set_min_width(menu_width);
    ui.set_max_width(menu_width);

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

fn collect_inspector_groups(
    property_bag: &mondrian_core::automation::PropertyBag,
) -> Vec<InspectorGroup<'_>> {
    let mut entries = property_bag
        .iter()
        .map(|(path, property)| (path, property_group_meta(path, property), property))
        .collect::<Vec<_>>();
    entries.sort_by(
        |(path_a, meta_a, property_a), (path_b, meta_b, property_b)| {
            meta_a
                .order
                .cmp(&meta_b.order)
                .then_with(|| meta_a.title.cmp(&meta_b.title))
                .then_with(|| property_order(path_a).cmp(&property_order(path_b)))
                .then_with(|| {
                    property_display_name(property_a).cmp(&property_display_name(property_b))
                })
        },
    );

    let mut groups = Vec::<InspectorGroup<'_>>::new();
    for (path, meta, property) in entries {
        if let Some(group) = groups.iter_mut().find(|group| group.meta.id == meta.id) {
            group.properties.push((path, property));
        } else {
            groups.push(InspectorGroup { meta, properties: vec![(path, property)] });
        }
    }
    groups
}

fn draw_group_header_row(
    ui: &mut Ui,
    meta: &AnimationGroupMeta,
    collapsed: bool,
    enabled_state: Option<bool>,
    show_effect_controls: bool,
    can_delete: bool,
) -> GroupHeaderRowResponse {
    let desired_size = Vec2::new(
        ui.available_width(),
        tokens::inspector_group_header_height(),
    );
    let (rect, row_response) = ui.allocate_exact_size(desired_size, Sense::click());
    let visuals = ui.visuals();
    let mut toggle_effect = None;
    let mut delete_effect = None;

    if ui.is_rect_visible(rect) {
        let fill = if row_response.hovered() {
            palette::bg_surface_active()
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, visuals.menu_corner_radius, fill);

        let mut title = meta.title.clone();
        if meta.kind == AnimationGroupKind::TimeRemap {
            title = "时间重映射".to_string();
        }

        let ghost_button_size = tokens::timeline_toolbar_button_size();
        let button_size = Vec2::new(ghost_button_size[0], ghost_button_size[1]);
        let button_gap = 4.0;
        let button_count = usize::from(enabled_state.is_some()) + usize::from(can_delete);
        let action_width = if show_effect_controls && button_count > 0 {
            button_count as f32 * button_size.x + button_count.saturating_sub(1) as f32 * button_gap
        } else {
            0.0
        };
        let action_rect = if action_width > 0.0 {
            Some(Rect::from_min_max(
                Pos2::new(
                    (rect.right() - 4.0 - action_width).max(rect.left() + 4.0),
                    rect.top(),
                ),
                Pos2::new(rect.right() - 4.0, rect.bottom()),
            ))
        } else {
            None
        };
        let title_rect = Rect::from_min_max(
            Pos2::new(rect.left() + 4.0, rect.top()),
            Pos2::new(
                action_rect
                    .map(|action| (action.left() - 8.0).max(rect.left() + 4.0))
                    .unwrap_or(rect.right() - 4.0),
                rect.bottom(),
            ),
        );
        let content_center_y = rect.center().y;
        let caret_rect = Rect::from_center_size(
            Pos2::new(
                title_rect.left() + tokens::icon_size() * 0.5,
                content_center_y,
            ),
            Vec2::splat(tokens::icon_size()),
        );
        let title_x = caret_rect.right() + tokens::inspector_group_indent() * 0.45;
        let title_galley = ui.painter().layout_no_wrap(
            title.clone(),
            typography::body_small(),
            palette::text_primary(),
        );
        let label_rect = Rect::from_min_size(
            Pos2::new(title_x, content_center_y - title_galley.size().y * 0.5),
            title_galley.size(),
        );

        theme::draw_icon(
            ui.painter(),
            caret_rect,
            if collapsed {
                theme::UiIcon::ArrowRight
            } else {
                theme::UiIcon::ArrowDown
            },
            palette::text_muted(),
        );
        ui.painter().with_clip_rect(title_rect).galley(
            label_rect.min,
            title_galley,
            palette::text_primary(),
        );

        if let Some(action_bounds) = action_rect {
            let action_rect = Rect::from_center_size(
                Pos2::new(action_bounds.center().x, content_center_y),
                Vec2::new(action_width, button_size.y),
            );
            let delete_rect = can_delete.then(|| {
                Rect::from_center_size(
                    Pos2::new(action_rect.right() - button_size.x * 0.5, content_center_y),
                    button_size,
                )
            });
            let toggle_rect = enabled_state.map(|enabled| {
                let right_edge =
                    delete_rect.map(|rect| rect.left() - button_gap).unwrap_or(action_rect.right());
                let rect = Rect::from_center_size(
                    Pos2::new(right_edge - button_size.x * 0.5, content_center_y),
                    button_size,
                );
                let icon = if enabled {
                    theme::UiIcon::Eye
                } else {
                    theme::UiIcon::EyeOff
                };
                let response = theme::icon_ghost_button_at(
                    ui,
                    rect,
                    row_response.id.with("toggle_effect"),
                    icon,
                );
                toggle_effect = Some(response);
                rect
            });
            if let Some(rect) = delete_rect {
                let response = theme::icon_ghost_button_at(
                    ui,
                    rect,
                    row_response.id.with("delete_effect"),
                    theme::UiIcon::Trash,
                );
                delete_effect = Some(response);
            }
            let _ = toggle_rect;
        }
    }

    GroupHeaderRowResponse { row: row_response, toggle_effect, delete_effect }
}

fn group_effect_id(group: &InspectorGroup<'_>) -> Option<EffectId> {
    group
        .properties
        .first()
        .and_then(|(path, _)| parse_effect_id_from_property_path(path))
}

fn parse_effect_id_from_property_path(path: &str) -> Option<EffectId> {
    let mut segments = path.split('.');
    if segments.next()? != "effect" {
        return None;
    }
    let id_raw = segments.next()?;
    uuid::Uuid::parse_str(id_raw).ok().map(EffectId)
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

fn graph_time_axis_ticks(
    time_min: TimeTicks,
    time_max: TimeTicks,
    time_base: mondrian_core::types::Rational,
    width: f32,
) -> Vec<TimeTicks> {
    if time_max <= time_min {
        return vec![time_min];
    }

    let desired_tick_count = ((width / 92.0).round() as usize).clamp(3, 7);
    let frame_span = ((time_max - time_min) as f64 / SUBFRAME_TICKS_PER_FRAME as f64).max(1.0);
    let raw_step_frames = (frame_span / (desired_tick_count.saturating_sub(1)) as f64).max(1.0);
    let step_frames = nice_frame_step(raw_step_frames, time_base);
    let start_frame = (time_min / SUBFRAME_TICKS_PER_FRAME).max(0);
    let end_frame = (time_max / SUBFRAME_TICKS_PER_FRAME).max(start_frame);
    let first_tick_frame = (start_frame / step_frames) * step_frames;

    let mut ticks = Vec::new();
    let mut frame = first_tick_frame;
    while frame <= end_frame {
        let tick = frame * SUBFRAME_TICKS_PER_FRAME;
        if tick >= time_min && tick <= time_max {
            ticks.push(tick);
        }
        frame += step_frames;
    }
    if ticks.first().copied() != Some(time_min) {
        ticks.insert(0, time_min);
    }
    if ticks.last().copied() != Some(time_max) {
        ticks.push(time_max);
    }
    ticks.sort_unstable();
    ticks.dedup();
    ticks
}

fn nice_frame_step(raw_step_frames: f64, time_base: mondrian_core::types::Rational) -> i64 {
    let fps = (1.0 / time_base.to_f64()).round().max(1.0) as i64;
    let candidates = [1, 2, 5, 10, 15, fps / 2, fps, fps * 2, fps * 5, fps * 10];
    candidates
        .into_iter()
        .filter(|step| *step > 0)
        .find(|step| *step as f64 >= raw_step_frames)
        .unwrap_or((raw_step_frames.ceil() as i64).max(1))
}

fn format_graph_time_label(time: TimeTicks, time_base: mondrian_core::types::Rational) -> String {
    let frame = (time as f64 / SUBFRAME_TICKS_PER_FRAME as f64).round() as i64;
    let smpte = TimeCode::new(frame.max(0), time_base).to_smpte();
    if let Some(stripped) = smpte.strip_prefix("00:") {
        stripped.to_string()
    } else {
        smpte
    }
}

#[derive(Clone)]
struct GraphTimeAxisLabel {
    x: f32,
    text: String,
}

fn graph_time_label_positions(
    painter: &egui::Painter,
    ticks: &[TimeTicks],
    plot_rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    time_base: mondrian_core::types::Rational,
) -> Vec<GraphTimeAxisLabel> {
    if ticks.is_empty() {
        return Vec::new();
    }

    let candidates = ticks
        .iter()
        .enumerate()
        .map(|(index, time)| {
            let text = format_graph_time_label(*time, time_base);
            let width = painter
                .layout_no_wrap(
                    text.clone(),
                    typography::body_small(),
                    palette::text_muted(),
                )
                .size()
                .x;
            let x = graph_x_for_time(plot_rect, time_min, time_max, *time);
            let mut left = x - width * 0.5;
            let mut right = x + width * 0.5;
            if index == 0 {
                left = plot_rect.left();
                right = left + width;
            } else if index + 1 == ticks.len() {
                right = plot_rect.right();
                left = right - width;
            }
            (index, GraphTimeAxisLabel { x, text }, left, right)
        })
        .collect::<Vec<_>>();

    let min_gap = 10.0;
    let mut visible = Vec::<(GraphTimeAxisLabel, f32, f32)>::new();
    for (index, label, left, right) in candidates {
        if index == 0 {
            visible.push((label, left, right));
            continue;
        }

        if index + 1 == ticks.len() {
            while visible.len() > 1 {
                let Some((_, _, last_right)) = visible.last() else {
                    break;
                };
                if left >= *last_right + min_gap {
                    break;
                }
                visible.pop();
            }
            if visible.last().is_some_and(|(_, _, last_right)| left < *last_right + min_gap) {
                let keep_first =
                    visible.first().map(|(_, left, right)| right - left).unwrap_or(0.0)
                        <= right - left;
                return if keep_first {
                    visible.first().map(|(label, _, _)| vec![label.clone()]).unwrap_or_default()
                } else {
                    vec![label]
                };
            }
            visible.push((label, left, right));
            continue;
        }

        if visible.last().is_some_and(|(_, _, last_right)| left < *last_right + min_gap) {
            continue;
        }
        visible.push((label, left, right));
    }

    visible.into_iter().map(|(label, _, _)| label).collect()
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
    time_collision_candidates: &[TimeTicks],
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
    let (delta_time, snapped_time) = resolve_graph_drag_time_delta(
        raw_delta_time,
        &anchor_times,
        time_snap_candidates,
        time_collision_candidates,
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
    time_collision_candidates: &[TimeTicks],
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
        time_collision_candidates,
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
    time_collision_candidates: &[TimeTicks],
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
        time_collision_candidates,
        value_snap_candidates,
    )?;
    let raw_delta_time = graph_time_from_x(rect, time_min, time_max, drag.pointer_pos.x)
        - graph_time_from_x(rect, time_min, time_max, drag.start_pointer_pos.x);
    let raw_delta_time = snap_graph_delta_ticks(raw_delta_time);
    let anchor_times = drag.anchors.iter().map(|anchor| anchor.time).collect::<Vec<_>>();
    let (delta_time, _) = resolve_graph_drag_time_delta(
        raw_delta_time,
        &anchor_times,
        time_snap_candidates,
        time_collision_candidates,
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
    time_collision_candidates: &[TimeTicks],
    value_snap_candidates: &[f64],
) -> Option<Vec<PropertyMutation>> {
    let raw_delta_time = graph_time_from_x(rect, time_min, time_max, drag.pointer_pos.x)
        - graph_time_from_x(rect, time_min, time_max, drag.start_pointer_pos.x);
    let raw_delta_time = snap_graph_delta_ticks(raw_delta_time);
    let anchor_times = drag.anchors.iter().map(|anchor| anchor.time).collect::<Vec<_>>();
    let (delta_time, _) = resolve_graph_drag_time_delta(
        raw_delta_time,
        &anchor_times,
        time_snap_candidates,
        time_collision_candidates,
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
            selections.extend(
                drag.entries.iter().zip(remapped).map(|(entry, (_, new_time))| {
                    AnimationKeyframeSelection {
                        clip_id,
                        path: entry.selection.path.clone(),
                        time: new_time,
                    }
                }),
            );
        }
        GraphSelectionScaleAxis::Value => {
            for entry in &drag.entries {
                let (_, value) = graph_selection_scaled_point(entry, drag, factor);
                mutations.push(PropertyMutation::UpdateChannelKeyframeValue {
                    path: drag.path.clone(),
                    time: entry.selection.time,
                    channel_index: drag.channel_index,
                    value,
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

    if temporal_flags.continuous
        && !temporal_flags.broken_handles
        && index > 0
        && index + 1 < keyframes.len()
    {
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
            if best.as_ref().map(|(_, best_diff)| diff < *best_diff).unwrap_or(true) {
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

fn graph_drag_time_delta_is_valid(
    delta: TimeTicks,
    anchor_times: &[TimeTicks],
    occupied_times: &[TimeTicks],
) -> bool {
    let mut moved_times = HashSet::with_capacity(anchor_times.len());
    for anchor_time in anchor_times {
        let moved_time = (*anchor_time + delta).max(0);
        if occupied_times.contains(&moved_time) || !moved_times.insert(moved_time) {
            return false;
        }
    }
    true
}

fn resolve_graph_drag_time_delta(
    raw_delta: TimeTicks,
    anchor_times: &[TimeTicks],
    snap_candidates: &[TimeTicks],
    occupied_times: &[TimeTicks],
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
) -> (TimeTicks, Option<TimeTicks>) {
    let (snapped_delta, snapped_time) = snap_graph_time_delta(
        raw_delta,
        anchor_times,
        snap_candidates,
        rect,
        time_min,
        time_max,
    );

    if snapped_time.is_some()
        && graph_drag_time_delta_is_valid(snapped_delta, anchor_times, occupied_times)
    {
        return (snapped_delta, snapped_time);
    }

    if graph_drag_time_delta_is_valid(raw_delta, anchor_times, occupied_times) {
        (raw_delta, None)
    } else {
        (0, None)
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
            if best.as_ref().map(|(_, best_diff)| diff < *best_diff).unwrap_or(true) {
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

#[allow(dead_code)]
fn color_space_options() -> [ColorSpace; 9] {
    [
        ColorSpace::Rec709,
        ColorSpace::Rec2100Hlg,
        ColorSpace::Rec2100Pq,
        ColorSpace::Srgb,
        ColorSpace::Rec2020,
        ColorSpace::DciP3,
        ColorSpace::AppleLog,
        ColorSpace::SLog3,
        ColorSpace::ArriLogC4,
    ]
}

fn color_space_label(value: ColorSpace) -> &'static str {
    match value {
        ColorSpace::Rec709 => "Rec. 709",
        ColorSpace::Rec2100Hlg => "Rec. 2100 HLG",
        ColorSpace::Rec2100Pq => "Rec. 2100 PQ",
        ColorSpace::Srgb => "sRGB",
        ColorSpace::Rec2020 => "Rec. 2020",
        ColorSpace::DciP3 => "DCI-P3",
        ColorSpace::AppleLog => "Apple Log",
        ColorSpace::SLog3 => "S-Log3",
        ColorSpace::ArriLogC4 => "ARRI LogC4",
    }
}

fn pixel_aspect_ratio_label(value: PixelAspectRatio) -> &'static str {
    match value {
        PixelAspectRatio::Square => "方形像素",
        PixelAspectRatio::D1DvNtsc => "D1/DV NTSC",
        PixelAspectRatio::D1DvNtscWidescreen => "D1/DV NTSC 16:9",
        PixelAspectRatio::D1DvPal => "D1/DV PAL",
        PixelAspectRatio::D1DvPalWidescreen => "D1/DV PAL 16:9",
        PixelAspectRatio::Anamorphic2x => "变形 2:1",
        PixelAspectRatio::HdAnamorphic1080 => "HD 变形 1080",
        PixelAspectRatio::DvcproHd => "DVCPRO HD",
        PixelAspectRatio::Unknown => "未知 PAR",
    }
}

fn field_order_label(value: FieldOrder) -> &'static str {
    match value {
        FieldOrder::Progressive => "逐行扫描",
        FieldOrder::UpperFirst => "高场优先",
        FieldOrder::LowerFirst => "低场优先",
    }
}

fn alpha_interpretation_label(value: AlphaInterpretation) -> &'static str {
    match value {
        AlphaInterpretation::Straight => "直通 Alpha",
        AlphaInterpretation::Premultiplied => "预乘 Alpha",
        AlphaInterpretation::Ignore => "忽略 Alpha",
    }
}

fn frame_rate_label(value: Rational) -> String {
    let fps = value.to_f64();
    if (fps.fract()).abs() < 0.001 {
        format!("{fps:.0} fps")
    } else if (fps * 10.0).fract().abs() < 0.001 {
        format!("{fps:.1} fps")
    } else {
        format!("{fps:.3} fps")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::automation::{
        AnimatablePropertyUiMetadata, AnimatedProperty, Keyframe, PropertyBag, PropertyDescriptor,
    };
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

    fn sample_selection(time: TimeTicks) -> AnimationKeyframeSelection {
        AnimationKeyframeSelection {
            clip_id: ClipId::new(),
            path: "transform.opacity".to_string(),
            time,
        }
    }

    fn grouped_test_property(
        path: &str,
        display_name: &str,
        group_name: Option<&str>,
    ) -> AnimatedProperty {
        let mut descriptor = PropertyDescriptor::new(path, display_name, PropertyValue::Float(0.0));
        descriptor.ui_metadata = AnimatablePropertyUiMetadata {
            group_name: group_name.map(str::to_string),
            ..Default::default()
        };
        AnimatedProperty::from_descriptor(descriptor)
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
    fn collect_inspector_groups_orders_builtin_and_effect_sections() {
        let mut bag = PropertyBag::default();
        bag.upsert(grouped_test_property(
            mondrian_timeline::clip::Transform2D::ROTATION_PATH,
            "旋转",
            None,
        ));
        bag.upsert(grouped_test_property(
            mondrian_timeline::clip::Transform2D::POSITION_PATH,
            "位置",
            None,
        ));
        bag.upsert(grouped_test_property(
            mondrian_timeline::clip::Transform2D::OPACITY_PATH,
            "不透明度",
            None,
        ));
        bag.upsert(grouped_test_property(
            "effect.gaussian_blur.radius",
            "模糊半径",
            Some("模糊"),
        ));

        let groups = collect_inspector_groups(&bag);
        let titles = groups.iter().map(|group| group.meta.title.as_str()).collect::<Vec<_>>();
        assert_eq!(titles, vec!["运动", "不透明度", "模糊"]);
        assert_eq!(
            groups[0]
                .properties
                .iter()
                .map(|(_, property)| property.descriptor.display_name.as_str())
                .collect::<Vec<_>>(),
            vec!["位置", "旋转"]
        );
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
    fn graph_keyframe_drag_property_mutations_return_none_without_delta() {
        let drag = GraphKeyframeDragState {
            clip_id: ClipId::new(),
            path: "transform.opacity".to_string(),
            channel_index: 0,
            start_pointer_pos: Pos2::new(120.0, 110.0),
            pointer_pos: Pos2::new(120.0, 110.0),
            anchors: vec![GraphKeyframeDragAnchor {
                time: SUBFRAME_TICKS_PER_FRAME * 10,
                value: 0.5,
            }],
        };

        assert!(graph_keyframe_drag_property_mutations(
            &drag,
            graph_rect(),
            0,
            SUBFRAME_TICKS_PER_FRAME * 20,
            0.0,
            1.0,
            &[],
            &[],
            &[],
        )
        .is_none());
    }

    #[test]
    fn graph_keyframe_drag_mutations_clamp_time_to_zero() {
        let drag = GraphKeyframeDragState {
            clip_id: ClipId::new(),
            path: "transform.opacity".to_string(),
            channel_index: 0,
            start_pointer_pos: Pos2::new(200.0, 110.0),
            pointer_pos: Pos2::new(-100.0, 110.0),
            anchors: vec![GraphKeyframeDragAnchor {
                time: SUBFRAME_TICKS_PER_FRAME * 3,
                value: 0.5,
            }],
        };

        let (_, selections) = graph_keyframe_drag_mutations(
            &drag,
            graph_rect(),
            0,
            SUBFRAME_TICKS_PER_FRAME * 20,
            0.0,
            1.0,
            ClipId::new(),
            &[],
            &[],
            &[],
        )
        .expect("drag mutations");
        assert_eq!(selections[0].time, 0);
    }

    #[test]
    fn graph_handles_respect_single_and_boundary_segments() {
        let mut single = AnimatedProperty::from_descriptor(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));
        single
            .apply_mutation(PropertyMutation::SetKeyframe {
                path: "transform.opacity".to_string(),
                keyframe: Keyframe::from_preset(
                    SUBFRAME_TICKS_PER_FRAME * 5,
                    PropertyValue::Float(0.5),
                    InterpolationType::Bezier,
                ),
            })
            .expect("single keyframe");
        let single_channel = single.channel(0).expect("single channel");
        assert!(graph_handles_for_keyframe(
            single_channel.keyframes(),
            0,
            graph_rect(),
            0,
            SUBFRAME_TICKS_PER_FRAME * 20,
            0.0,
            1.0,
        )
        .is_empty());

        let mut boundary = AnimatedProperty::from_descriptor(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(0.0),
        ));
        boundary
            .apply_mutation(PropertyMutation::SetKeyframe {
                path: "transform.opacity".to_string(),
                keyframe: Keyframe::from_preset(
                    0,
                    PropertyValue::Float(0.0),
                    InterpolationType::Bezier,
                ),
            })
            .expect("boundary first");
        boundary
            .apply_mutation(PropertyMutation::SetKeyframe {
                path: "transform.opacity".to_string(),
                keyframe: Keyframe::from_preset(
                    SUBFRAME_TICKS_PER_FRAME * 20,
                    PropertyValue::Float(1.0),
                    InterpolationType::Bezier,
                ),
            })
            .expect("boundary last");
        let channel = boundary.channel(0).expect("channel");
        let first_handles = graph_handles_for_keyframe(
            channel.keyframes(),
            0,
            graph_rect(),
            0,
            SUBFRAME_TICKS_PER_FRAME * 20,
            0.0,
            1.0,
        );
        let last_handles = graph_handles_for_keyframe(
            channel.keyframes(),
            channel.keyframes().len() - 1,
            graph_rect(),
            0,
            SUBFRAME_TICKS_PER_FRAME * 20,
            0.0,
            1.0,
        );
        assert_eq!(first_handles.len(), 1);
        assert!(first_handles[0].kind == GraphHandleKind::Out);
        assert_eq!(last_handles.len(), 1);
        assert!(last_handles[0].kind == GraphHandleKind::In);
    }

    #[test]
    fn preview_handle_position_returns_none_for_missing_boundary_side() {
        let property = sample_property();
        let channel = property.channel(0).expect("channel");
        let first = &channel.keyframes()[0];
        let last_index = channel.keyframes().len() - 1;
        let last = &channel.keyframes()[last_index];

        assert!(preview_handle_position(
            GraphHandleKind::In,
            Pos2::new(100.0, 100.0),
            channel.keyframes(),
            0,
            first.temporal_flags,
            graph_rect(),
            0,
            SUBFRAME_TICKS_PER_FRAME * 20,
            0.0,
            1.0,
        )
        .is_none());
        assert!(preview_handle_position(
            GraphHandleKind::Out,
            Pos2::new(100.0, 100.0),
            channel.keyframes(),
            last_index,
            last.temporal_flags,
            graph_rect(),
            0,
            SUBFRAME_TICKS_PER_FRAME * 20,
            0.0,
            1.0,
        )
        .is_none());
    }

    #[test]
    fn graph_selection_value_scale_preserves_bezier_editing_path() {
        let property = sample_property();
        let drag = GraphSelectionScaleDragState {
            clip_id: ClipId::new(),
            path: "transform.opacity".to_string(),
            channel_index: 0,
            axis: GraphSelectionScaleAxis::Value,
            edge: GraphSelectionScaleEdge::Max,
            pointer_pos: Pos2::new(200.0, 40.0),
            entries: vec![
                SelectedGraphKeyframeData { selection: sample_selection(0), channel_value: 0.0 },
                SelectedGraphKeyframeData {
                    selection: sample_selection(SUBFRAME_TICKS_PER_FRAME * 10),
                    channel_value: 0.5,
                },
            ],
            plot_rect: graph_rect(),
            time_min: 0,
            time_max: SUBFRAME_TICKS_PER_FRAME * 20,
            value_min: 0.0,
            value_max: 1.0,
        };

        let (mutations, _) =
            graph_selection_scale_mutations(&drag, ClipId::new()).expect("scale mutations");
        assert!(mutations.iter().all(|mutation| matches!(
            mutation,
            PropertyMutation::UpdateChannelKeyframeValue { .. }
        )));

        let mut preview = property.clone();
        for mutation in mutations {
            preview.apply_mutation(mutation).expect("apply scale mutation");
        }
        let stored = preview
            .channel(0)
            .and_then(|channel| {
                channel
                    .keyframes()
                    .iter()
                    .find(|keyframe| keyframe.time == SUBFRAME_TICKS_PER_FRAME * 10)
            })
            .expect("scaled keyframe");
        assert!(matches!(stored.interp_in, KeyframeInterpolation::Bezier(_)));
        assert!(matches!(
            stored.interp_out,
            KeyframeInterpolation::Bezier(_)
        ));
    }

    #[test]
    fn graph_selection_time_scale_rejects_duplicate_targets() {
        let min_time = SUBFRAME_TICKS_PER_FRAME * 10;
        let max_time = SUBFRAME_TICKS_PER_FRAME * 11;
        let duplicate_target = min_time + ((max_time - min_time) * 3 / 5);
        let drag = GraphSelectionScaleDragState {
            clip_id: ClipId::new(),
            path: "transform.opacity".to_string(),
            channel_index: 0,
            axis: GraphSelectionScaleAxis::Time,
            edge: GraphSelectionScaleEdge::Min,
            pointer_pos: Pos2::new(
                graph_x_for_time(
                    graph_rect(),
                    0,
                    SUBFRAME_TICKS_PER_FRAME * 20,
                    duplicate_target,
                ),
                120.0,
            ),
            entries: vec![
                SelectedGraphKeyframeData {
                    selection: sample_selection(min_time),
                    channel_value: 0.4,
                },
                SelectedGraphKeyframeData {
                    selection: sample_selection(max_time),
                    channel_value: 0.6,
                },
            ],
            plot_rect: graph_rect(),
            time_min: 0,
            time_max: SUBFRAME_TICKS_PER_FRAME * 20,
            value_min: 0.0,
            value_max: 1.0,
        };

        assert!(graph_selection_scale_mutations(&drag, ClipId::new()).is_none());
    }

    #[test]
    fn speed_per_second_is_zero_for_flat_curve() {
        let mut property = AnimatedProperty::from_descriptor(PropertyDescriptor::new(
            "transform.opacity",
            "Opacity",
            PropertyValue::Float(1.0),
        ));
        property
            .apply_mutation(PropertyMutation::SetKeyframe {
                path: "transform.opacity".to_string(),
                keyframe: Keyframe::linear(0, PropertyValue::Float(1.0)),
            })
            .expect("set first");
        property
            .apply_mutation(PropertyMutation::SetKeyframe {
                path: "transform.opacity".to_string(),
                keyframe: Keyframe::linear(
                    SUBFRAME_TICKS_PER_FRAME * 20,
                    PropertyValue::Float(1.0),
                ),
            })
            .expect("set second");
        let clip = Clip::new(
            mondrian_core::types::AssetId::new(),
            TimeCode::new(0, mondrian_core::types::Rational::FPS_25),
            TimeCode::new(20, mondrian_core::types::Rational::FPS_25),
        );

        let speed = speed_per_second_at_time(
            &property,
            0,
            &clip,
            SUBFRAME_TICKS_PER_FRAME * 10,
            0,
            SUBFRAME_TICKS_PER_FRAME * 20,
        );
        assert!(speed.abs() < 1e-9);
    }

    #[test]
    fn graph_speed_drag_mutations_return_none_without_delta() {
        let property = sample_property();
        let clip = Clip::new(
            mondrian_core::types::AssetId::new(),
            TimeCode::new(0, mondrian_core::types::Rational::FPS_25),
            TimeCode::new(20, mondrian_core::types::Rational::FPS_25),
        );
        let drag = GraphSpeedKeyframeDragState {
            clip_id: ClipId::new(),
            path: "transform.opacity".to_string(),
            channel_index: 0,
            start_pointer_pos: Pos2::new(120.0, 110.0),
            pointer_pos: Pos2::new(120.0, 110.0),
            anchors: vec![GraphSpeedKeyframeDragAnchor {
                time: SUBFRAME_TICKS_PER_FRAME * 10,
                speed: 0.25,
            }],
        };

        assert!(graph_speed_drag_mutations(
            &drag,
            &property,
            &clip,
            0,
            SUBFRAME_TICKS_PER_FRAME * 20,
            graph_rect(),
            -4.0,
            4.0,
            ClipId::new(),
            &[],
        )
        .is_none());
    }

    #[test]
    fn graph_speed_drag_mutations_update_only_existing_boundary_side() {
        let property = sample_property();
        let clip = Clip::new(
            mondrian_core::types::AssetId::new(),
            TimeCode::new(0, mondrian_core::types::Rational::FPS_25),
            TimeCode::new(20, mondrian_core::types::Rational::FPS_25),
        );
        let drag = GraphSpeedKeyframeDragState {
            clip_id: ClipId::new(),
            path: "transform.opacity".to_string(),
            channel_index: 0,
            start_pointer_pos: Pos2::new(100.0, 120.0),
            pointer_pos: Pos2::new(100.0, 40.0),
            anchors: vec![GraphSpeedKeyframeDragAnchor { time: 0, speed: 0.0 }],
        };

        let (mutations, _) = graph_speed_drag_mutations(
            &drag,
            &property,
            &clip,
            0,
            SUBFRAME_TICKS_PER_FRAME * 20,
            graph_rect(),
            -4.0,
            4.0,
            ClipId::new(),
            &[0.5],
        )
        .expect("speed mutations");

        let PropertyMutation::UpdateChannelKeyframeHandles { interp_in, interp_out, .. } =
            &mutations[0]
        else {
            panic!("expected handle mutation");
        };
        assert!(matches!(interp_in, KeyframeInterpolation::Linear));
        assert!(matches!(interp_out, KeyframeInterpolation::Bezier(_)));
    }

    #[test]
    fn graph_speed_preview_property_preserves_handle_time_offsets() {
        let property = sample_property();
        let clip = Clip::new(
            mondrian_core::types::AssetId::new(),
            TimeCode::new(0, mondrian_core::types::Rational::FPS_25),
            TimeCode::new(20, mondrian_core::types::Rational::FPS_25),
        );
        let drag = GraphSpeedKeyframeDragState {
            clip_id: ClipId::new(),
            path: "transform.opacity".to_string(),
            channel_index: 0,
            start_pointer_pos: Pos2::new(200.0, 120.0),
            pointer_pos: Pos2::new(200.0, 60.0),
            anchors: vec![GraphSpeedKeyframeDragAnchor {
                time: SUBFRAME_TICKS_PER_FRAME * 10,
                speed: 0.0,
            }],
        };

        let preview = graph_speed_preview_property(
            &property,
            &drag,
            &clip,
            0,
            SUBFRAME_TICKS_PER_FRAME * 20,
            graph_rect(),
            -4.0,
            4.0,
            &[0.5],
        )
        .expect("preview");
        let original = property
            .channel(0)
            .and_then(|channel| channel.keyframe_at(SUBFRAME_TICKS_PER_FRAME * 10))
            .expect("original");
        let preview_keyframe = preview
            .channel(0)
            .and_then(|channel| channel.keyframe_at(SUBFRAME_TICKS_PER_FRAME * 10))
            .expect("preview keyframe");
        let KeyframeInterpolation::Bezier(original_in) = original.interp_in else {
            panic!("expected original in handle");
        };
        let KeyframeInterpolation::Bezier(original_out) = original.interp_out else {
            panic!("expected original out handle");
        };
        let KeyframeInterpolation::Bezier(preview_in) = preview_keyframe.interp_in else {
            panic!("expected preview in handle");
        };
        let KeyframeInterpolation::Bezier(preview_out) = preview_keyframe.interp_out else {
            panic!("expected preview out handle");
        };
        assert!((preview_in.time_offset - original_in.time_offset).abs() < 1e-9);
        assert!((preview_out.time_offset - original_out.time_offset).abs() < 1e-9);
    }

    #[test]
    fn snap_graph_time_delta_prefers_nearest_candidate() {
        let (delta, snapped) = snap_graph_time_delta(
            SUBFRAME_TICKS_PER_FRAME * 5,
            &[SUBFRAME_TICKS_PER_FRAME * 10],
            &[
                SUBFRAME_TICKS_PER_FRAME * 14,
                SUBFRAME_TICKS_PER_FRAME * 16,
                SUBFRAME_TICKS_PER_FRAME * 20,
            ],
            graph_rect(),
            0,
            SUBFRAME_TICKS_PER_FRAME * 40,
        );
        assert_eq!(delta, SUBFRAME_TICKS_PER_FRAME * 4);
        assert_eq!(snapped, Some(SUBFRAME_TICKS_PER_FRAME * 14));
    }

    #[test]
    fn resolve_graph_drag_time_delta_suppresses_invalid_snapped_collision() {
        let anchor = SUBFRAME_TICKS_PER_FRAME * 10;
        let raw_delta = SUBFRAME_TICKS_PER_FRAME * 3;
        let occupied = SUBFRAME_TICKS_PER_FRAME * 14;

        let (delta, snapped) = resolve_graph_drag_time_delta(
            raw_delta,
            &[anchor],
            &[occupied],
            &[occupied],
            graph_rect(),
            0,
            SUBFRAME_TICKS_PER_FRAME * 40,
        );

        assert_eq!(delta, raw_delta);
        assert_eq!(snapped, None);
    }

    #[test]
    fn graph_drag_preview_map_hides_time_guide_for_invalid_collision_snap() {
        let drag = GraphKeyframeDragState {
            clip_id: ClipId::new(),
            path: "transform.opacity".to_string(),
            channel_index: 0,
            start_pointer_pos: Pos2::new(120.0, 110.0),
            pointer_pos: Pos2::new(150.0, 110.0),
            anchors: vec![GraphKeyframeDragAnchor {
                time: SUBFRAME_TICKS_PER_FRAME * 10,
                value: 0.5,
            }],
        };

        let (_map, guides) = graph_drag_preview_map_with_snap(
            &drag,
            Some(drag.pointer_pos),
            graph_rect(),
            0,
            SUBFRAME_TICKS_PER_FRAME * 40,
            0.0,
            1.0,
            &[SUBFRAME_TICKS_PER_FRAME * 14],
            &[SUBFRAME_TICKS_PER_FRAME * 14],
            &[],
        );

        assert_eq!(guides.time, None);
    }

    #[test]
    fn snap_graph_value_delta_snaps_only_within_threshold() {
        let (close_delta, close_snap) =
            snap_graph_value_delta(0.11, &[0.2], &[0.3], graph_rect(), 0.0, 1.0);
        assert!(close_snap.is_some());
        assert!((close_delta - 0.1).abs() < 1e-6);

        let (far_delta, far_snap) =
            snap_graph_value_delta(0.3, &[0.2], &[0.8], graph_rect(), 0.0, 1.0);
        assert!(far_snap.is_none());
        assert!((far_delta - 0.3).abs() < 1e-6);
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
