//! Property inspector — row rendering, value editors, commit helpers.
use super::*;

impl EffectControlsPanel {
    pub(crate) fn draw_property_row(
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

    pub(crate) fn draw_property_value_editor(
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
                let is_scale = path == mondrian_timeline::clip::Transform2D::SCALE_PATH;
                if is_scale {
                    // Display scale as percentage (100.0% = 1.0)
                    let mut sx = edited.x * 100.0;
                    let mut sy = edited.y * 100.0;
                    let step = 0.01;
                    ui.horizontal(|ui| {
                        let xc = ui.add(DragValue::new(&mut sx).speed(step).suffix("%")).changed();
                        let yc = ui.add(DragValue::new(&mut sy).speed(step).suffix("%")).changed();
                        let mut channel_values = Vec::new();
                        if xc {
                            channel_values.push((0, (sx / 100.0) as f64));
                        }
                        if yc {
                            channel_values.push((1, (sy / 100.0) as f64));
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
                } else {
                    ui.horizontal(|ui| {
                        let x_changed = ui
                            .add(DragValue::new(&mut edited.x).speed(speed).prefix("X "))
                            .changed();
                        let y_changed = ui
                            .add(DragValue::new(&mut edited.y).speed(speed).prefix("Y "))
                            .changed();
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
                } // end else (non-scale Vec2)
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
                if path.ends_with(".mask_op") {
                    self.draw_mask_op_editor(
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
                    // Use add_sized to force exact width — the grid measures the
                    // allocated rect, and a 150px cap prevents the panel from
                    // auto-expanding while still being wide enough for a file path.
                    let text_w = 120.0;
                    if ui
                        .add_sized(
                            [text_w, ui.spacing().interact_size.y],
                            egui::TextEdit::singleline(buffer),
                        )
                        .changed()
                    {
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
                let picker_resp = crate::egui_ui::color_picker::color_picker_button(
                    ui,
                    &mut color,
                    crate::egui_ui::color_picker::ColorPickerVariant::Inline,
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

    pub(crate) fn draw_blend_mode_editor(
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

    pub(crate) fn draw_mask_op_editor(
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
            .selected_text(mask_op_display_label(selected.as_str()))
            .show_ui(ui, |ui| {
                for (value, label) in mask_op_options() {
                    ui.selectable_value(&mut selected, value.to_string(), *label);
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

    pub(crate) fn commit_value(
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

    pub(crate) fn commit_channel_values(
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

    pub(crate) fn current_time(&self, app: &AppState) -> TimeCode {
        app.current_time_code()
            .unwrap_or_else(|| TimeCode::new(0, mondrian_core::types::Rational::FPS_25))
    }

    pub(crate) fn current_interpolation(
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
}
