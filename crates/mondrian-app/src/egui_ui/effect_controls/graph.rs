//! Keyframe graph editor — rendering and interaction.
use super::*;

impl EffectControlsPanel {
    pub(crate) fn draw_graph_editor(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        selection: SelectedClipRef,
        clip: &Clip,
        current_time: TimeCode,
        properties: &[(&str, &mondrian_core::automation::AnimatedProperty)],
    ) {
        if properties.is_empty() {
            crate::egui_ui::widgets::empty_state(
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

    pub(crate) fn apply_interpolation_to_selection(
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

    pub(crate) fn delete_selected_keyframes(
        &mut self,
        app: &mut AppState,
        selection: SelectedClipRef,
    ) {
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

    pub(crate) fn scale_graph_selection_time(
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

    pub(crate) fn scale_graph_selection_values(
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

    pub(crate) fn selected_graph_keyframe_data(
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
    pub(crate) fn draw_graph_selection_transform_handles(
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

    pub(crate) fn draw_graph_selection_bubble(
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
    pub(crate) fn handle_graph_marquee(
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
        let pointer_over_floating_ui = ui.ctx().is_pointer_over_egui();

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
    pub(crate) fn draw_media_interpretation(
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

    pub(crate) fn clear_graph_marquee(&mut self) {
        self.graph_marquee_anchor = None;
        self.graph_marquee_current = None;
        self.graph_marquee_additive = false;
    }

    pub(crate) fn draw_pending_clear_animation_dialog(
        &mut self,
        ctx: &egui::Context,
        app: &mut AppState,
    ) {
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
}
