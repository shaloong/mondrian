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

pub(crate) struct InspectorGroup<'a> {
    meta: AnimationGroupMeta,
    properties: Vec<(&'a str, &'a mondrian_core::automation::AnimatedProperty)>,
}

pub(crate) struct GroupHeaderRowResponse {
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
pub(crate) enum GraphEditorMode {
    #[default]
    Value,
    Speed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraphHandleKind {
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
pub(crate) struct GraphKeyframeDragState {
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
pub(crate) struct GraphSpeedKeyframeDragState {
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
pub(crate) struct GraphEditorHandle {
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
pub(crate) struct GraphSelectionScaleDragState {
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
pub(crate) struct GraphKeyframeVisual {
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
pub(crate) struct SelectedGraphKeyframeData {
    selection: AnimationKeyframeSelection,
    channel_value: f64,
}

#[derive(Default, Clone, Copy)]
pub(crate) struct GraphSnapGuides {
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
        let effect_index: std::collections::HashMap<String, usize> =
            clip.effects.iter().enumerate().map(|(i, e)| (e.id.to_string(), i)).collect();
        inspector_groups.sort_by(|a, b| {
            let is_fx_a = a.meta.allows_effect_controls;
            let is_fx_b = b.meta.allows_effect_controls;
            // Fixed builtins: use original meta.order (0=Motion, 1=Opacity, 2=TimeRemap).
            // Effects: use position in clip.effects via UUID lookup (offset above builtins).
            let ord_a = if is_fx_a {
                // Extract UUID tail from group ID: "effect.<slug>.<uuid8>" → "<uuid8>"
                let uuid = a.meta.id.rsplit('.').next().unwrap_or("0");
                effect_index
                    .iter()
                    .find(|(full_id, _)| full_id.starts_with(uuid))
                    .map(|(_, &idx)| idx + 10)
                    .unwrap_or(0)
            } else {
                a.meta.order
            };
            let ord_b = if is_fx_b {
                let uuid = b.meta.id.rsplit('.').next().unwrap_or("0");
                effect_index
                    .iter()
                    .find(|(full_id, _)| full_id.starts_with(uuid))
                    .map(|(_, &idx)| idx + 10)
                    .unwrap_or(0)
            } else {
                b.meta.order
            };
            ord_a.cmp(&ord_b).then_with(|| a.meta.title.cmp(&b.meta.title))
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
                egui::ScrollArea::vertical().id_salt("effect_controls_scroll").show(ui, |ui| {
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
                if let Some(effect_type) = ui
                    .ctx()
                    .data_mut(|d| d.get_persisted::<mondrian_effects::EffectType>(effect_drag_id))
                {
                    if let Some(ptr) = ui.input(|i| i.pointer.interact_pos()) {
                        let panel_r = ui.max_rect();
                        if panel_r.contains(ptr) && !group_tops.is_empty() {
                            // Build (y, insert_index) pairs at valid effect boundaries
                            let mut slots: Vec<(f32, usize)> = Vec::new();
                            for i in 0..group_tops.len() {
                                if is_effect_group[i] {
                                    // Before this effect group → insert at its effect index
                                    let fx_idx =
                                        is_effect_group[..=i].iter().filter(|&&e| e).count() - 1;
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
                                .min_by(|a, b| {
                                    (ptr.y - a.0)
                                        .abs()
                                        .partial_cmp(&(ptr.y - b.0).abs())
                                        .unwrap_or(std::cmp::Ordering::Equal)
                                })
                                .copied()
                                .unwrap_or((end_y, total_fx));
                            // Draw snapped line
                            ui.painter().line_segment(
                                [
                                    egui::pos2(panel_r.left(), slot_y),
                                    egui::pos2(panel_r.right(), slot_y),
                                ],
                                egui::Stroke::new(2.0, palette::interaction_highlight()),
                            );
                            // Apply on release with correct index
                            if ui.input(|i| i.pointer.primary_released()) {
                                if selection.is_video_track {
                                    let _ = app.insert_effect_at_index(
                                        selection,
                                        effect_type.clone(),
                                        insert_idx,
                                    );
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
        let mask_id = group_mask_id(group);

        let enabled_state = if let Some(mid) = mask_id {
            app.clip_snapshot(selection)
                .and_then(|clip| clip.masks.iter().find(|m| m.id == mid).map(|m| m.enabled))
        } else {
            effect_id.and_then(|id| {
                app.clip_snapshot(selection).and_then(|clip| clip.effect_enabled(id))
            })
        };

        let show_controls = effect_id.is_some() || mask_id.is_some();
        let can_delete = effect_id.is_some() || mask_id.is_some();

        let header_response = draw_group_header_row(
            ui,
            &group.meta,
            collapsed,
            enabled_state,
            show_controls,
            can_delete,
        );
        let action_clicked =
            header_response.toggle_effect.as_ref().is_some_and(egui::Response::clicked)
                || header_response.delete_effect.as_ref().is_some_and(egui::Response::clicked);
        if header_response.row.clicked() && !action_clicked {
            collapsed = !collapsed;
        }

        if let Some(toggle) = header_response.toggle_effect.as_ref() {
            if toggle.clicked() {
                let next_enabled = !enabled_state.unwrap_or(true);
                if let Some(eid) = effect_id {
                    let _ = app.set_clip_effect_enabled(selection, eid, next_enabled);
                } else if let Some(mid) = mask_id {
                    let _ = app.set_mask_enabled(selection, mid, next_enabled);
                }
            }
        }

        if let Some(delete) = header_response.delete_effect.as_ref() {
            if delete.clicked() {
                if let Some(eid) = effect_id {
                    let _ = app.remove_effect_from_clip(selection, eid);
                } else if let Some(mid) = mask_id {
                    let _ = app.remove_mask_from_clip(selection, mid);
                }
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

            // For mask groups: draw a special Mask Path row (shape stored in shape_keyframes, not PropertyBag).
            if let Some(mid) = mask_id {
                if let Some(clip) = app.clip_snapshot(selection) {
                    if let Some(mask) = clip.masks.iter().find(|m| m.id == mid) {
                        let ticks = mondrian_core::automation::timecode_to_ticks(current_time);
                        let has_animation = mask.shape_animation_enabled;

                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            let timer_resp = theme::icon_ghost_toggle_button(
                                ui,
                                tokens::timeline_toolbar_button_size(),
                                theme::UiIcon::Timer,
                                has_animation,
                            )
                            .on_hover_text(if has_animation {
                                "已开启动画"
                            } else {
                                "点击开启形状关键帧动画"
                            });
                            if timer_resp.clicked() {
                                let _ = app.set_mask_shape_animation_enabled(
                                    selection,
                                    mid,
                                    !has_animation,
                                    ticks,
                                );
                            }

                            ui.label(
                                egui::RichText::new("Mask Path")
                                    .font(typography::body_small())
                                    .color(palette::text_muted()),
                            );
                        });
                    }
                }
            }
        }
        self.inspector_group_collapsed.insert(collapse_key, collapsed);
    }

    fn current_keyframe_time(
        &self,
        property: &mondrian_core::automation::AnimatedProperty,
        current_time_ticks: TimeTicks,
    ) -> Option<TimeTicks> {
        property.keyframe_times().into_iter().find(|time| *time == current_time_ticks)
    }
}

mod graph_helpers;
pub(crate) use graph_helpers::*;
mod graph;
mod inspector;

pub(crate) struct BlendModeOption {
    label: &'static str,
    value: &'static str,
}

mod labels;
pub(crate) use labels::*;

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
