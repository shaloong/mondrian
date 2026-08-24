//! Property-oriented Mixer and Inspector widget adapters.
//!
//! This deep Module owns widget construction and action lowering while the
//! parent panel Module retains aggregate projection and dock composition.

use super::*;

pub(super) fn audio_mixer_panel(model: &AudioMixerPanelModel) -> PropertyPanel {
    if let Some(message) = model.empty_message.as_deref().filter(|message| !message.is_empty()) {
        let (title, description) = message
            .split_once('\n')
            .map_or((message, ""), |(title, description)| (title, description));
        let mut panel = PropertyPanel::with_options(
            "音频混音器",
            PropertyPanelOptions {
                label_width: 0.0,
                control_gap: 0.0,
                row_height: 28.0,
                section_gap: 0.0,
                ..PropertyPanelOptions::default()
            },
        )
        .with_embedded_panel_chrome()
        .with_empty_state(title, description);
        if let Ok(icon) = AppIcon::Music.vector_icon() {
            panel = panel.with_empty_state_icon(icon);
        }
        return panel;
    }

    let mut panel = PropertyPanel::with_options(
        "音频混音器",
        PropertyPanelOptions {
            label_width: 104.0,
            control_gap: 8.0,
            row_height: 30.0,
            section_gap: 8.0,
            ..PropertyPanelOptions::default()
        },
    )
    .with_embedded_panel_chrome();

    let create_bus = audio_mixer_create_bus_action(model);
    let create_bus_label = model
        .next_bus_name
        .as_deref()
        .map_or("新建 Bus".to_owned(), |name| format!("新建 {name}"));
    panel = panel.with_section(PropertySection::new("路由图").with_row(PropertyRow::new(
        "Bus",
        Box::new(Button::new(create_bus_label).enabled(create_bus.is_some()).on_click(create_bus)),
    )));

    for channel in &model.channels {
        let kind = match channel.kind {
            AudioMixerChannelKind::Track => "轨道",
            AudioMixerChannelKind::Bus => "Bus",
            AudioMixerChannelKind::ProgramOutput => "节目输出",
        };
        let mut section = PropertySection::new(format!("{kind} · {}", channel.name));
        if let (Some(muted), mondrian_timeline::AudioChannelStripOwner::Track { track_id }) =
            (channel.track_muted, channel.owner)
        {
            section = section.with_row(PropertyRow::new(
                "静音",
                Box::new(
                    Checkbox::new("M", muted)
                        .on_change(move |value| audio_mixer_set_track_mute_action(track_id, value)),
                ),
            ));
        }
        if let (Some(soloed), mondrian_timeline::AudioChannelStripOwner::Track { track_id }) =
            (channel.track_soloed, channel.owner)
        {
            section = section.with_row(PropertyRow::new(
                "独奏",
                Box::new(
                    Checkbox::new("S", soloed)
                        .on_change(move |value| audio_mixer_set_track_solo_action(track_id, value)),
                ),
            ));
        }
        section = section.with_row(PropertyRow::new(
            "电平",
            Box::new(Label::new(audio_mixer_meter_label(channel)).muted()),
        ));
        let trim_channel = channel.clone();
        section = section.with_row(PropertyRow::new(
            "输入增益",
            numeric_slider_input_control_with_hard_range(
                channel.input_trim_db as f32,
                -60.0,
                12.0,
                AUDIO_GAIN_DB_MIN as f32,
                AUDIO_GAIN_DB_MAX as f32,
                Some(0.1),
                1,
                channel.is_editable,
                move |value| audio_mixer_set_input_trim_action(&trim_channel, value),
            ),
        ));
        match channel.fader {
            AudioMixerGainModel::Static { value_db } => {
                let fader_channel = channel.clone();
                section = section.with_row(PropertyRow::new(
                    "推子",
                    numeric_slider_input_control_with_hard_range(
                        value_db as f32,
                        -60.0,
                        12.0,
                        AUDIO_GAIN_DB_MIN as f32,
                        AUDIO_GAIN_DB_MAX as f32,
                        Some(0.1),
                        1,
                        channel.is_editable,
                        move |value| audio_mixer_set_fader_action(&fader_channel, value),
                    ),
                ));
            }
            AudioMixerGainModel::Automated { keyframe_count } => {
                section = section.with_row(PropertyRow::new(
                    "推子",
                    Box::new(Label::new(format!("自动化 · {keyframe_count} 个关键帧")).muted()),
                ));
            }
        }
        if let Some(automation) = &channel.fader_automation {
            section = section.with_row(
                PropertyRow::new("推子曲线", audio_automation_curve_control(automation))
                    .with_height(118.0),
            );
        }
        if let Some(reason) = &channel.edit_disabled_reason {
            section = section.with_row(PropertyRow::new(
                "只读",
                Box::new(Label::new(reason.clone()).muted()),
            ));
        }
        if channel.incoming_route_count > 0
            || matches!(
                channel.kind,
                AudioMixerChannelKind::Bus | AudioMixerChannelKind::ProgramOutput
            )
        {
            section = section.with_row(PropertyRow::new(
                "输入路由",
                Box::new(Label::new(format!("{} 条", channel.incoming_route_count)).muted()),
            ));
        }
        if matches!(channel.kind, AudioMixerChannelKind::Bus) {
            let rename_channel = channel.clone();
            section = section.with_row(PropertyRow::new(
                "名称",
                Box::new(
                    TextInput::new("Bus 名称")
                        .with_text(&channel.name)
                        .enabled(channel.is_editable)
                        .on_commit(move |name| {
                            audio_mixer_rename_bus_action(&rename_channel, name)
                        }),
                ),
            ));
        }
        if !channel.route_create_options.is_empty() {
            let items = channel
                .route_create_options
                .iter()
                .map(|option| {
                    MenuItem::new(
                        option.label.clone(),
                        audio_mixer_create_route_action(option),
                    )
                })
                .collect();
            section = section.with_row(PropertyRow::new(
                "添加路由",
                Box::new(Dropdown::new("选择 tap 与目标…", items).with_max_visible_items(12)),
            ));
        }
        if let Some(removal) = &channel.bus_removal {
            let action = audio_mixer_remove_bus_action(removal);
            let label = if removal.connected_route_count == 0 {
                "删除 Bus".to_owned()
            } else {
                format!("删除 Bus 与 {} 条路由", removal.connected_route_count)
            };
            section = section.with_row(PropertyRow::new(
                "Bus",
                Box::new(Button::new(label).enabled(action.is_some()).on_click(action)),
            ));
            if let Some(reason) = &removal.edit_disabled_reason {
                section = section.with_row(PropertyRow::new(
                    "删除受阻",
                    Box::new(Label::new(reason.clone()).muted()),
                ));
            }
        }
        panel = panel.with_section(section);
        for route in &channel.outbound_routes {
            let enabled_route = route.clone();
            let remove_action = audio_mixer_remove_route_action(route);
            let mut route_section =
                PropertySection::new(format!("Route · {}", route.destination_label))
                    .with_row(PropertyRow::new(
                        "Tap",
                        Box::new(Label::new(route.source_port_label).muted()),
                    ))
                    .with_row(PropertyRow::new(
                        "启用",
                        Box::new(
                            Checkbox::new("传递信号", route.enabled)
                                .enabled(route.is_editable)
                                .on_change(move |enabled| {
                                    audio_mixer_set_route_enabled_action(&enabled_route, enabled)
                                }),
                        ),
                    ));
            match route.gain {
                AudioMixerGainModel::Static { value_db } => {
                    let gain_route = route.clone();
                    route_section = route_section.with_row(PropertyRow::new(
                        "电平",
                        numeric_slider_input_control_with_hard_range(
                            value_db as f32,
                            -60.0,
                            12.0,
                            AUDIO_GAIN_DB_MIN as f32,
                            AUDIO_GAIN_DB_MAX as f32,
                            Some(0.1),
                            1,
                            route.is_editable,
                            move |value| audio_mixer_set_route_gain_action(&gain_route, value),
                        ),
                    ));
                }
                AudioMixerGainModel::Automated { keyframe_count } => {
                    route_section = route_section.with_row(PropertyRow::new(
                        "电平",
                        Box::new(Label::new(format!("自动化 · {keyframe_count} 个关键帧")).muted()),
                    ));
                }
            }
            if let Some(automation) = &route.gain_automation {
                route_section = route_section.with_row(
                    PropertyRow::new("电平曲线", audio_automation_curve_control(automation))
                        .with_height(118.0),
                );
            }
            route_section = route_section.with_row(PropertyRow::new(
                "控制",
                effect_icon_button(
                    AppIcon::Trash,
                    "删除 Route",
                    "删除这条 Route 或 Send",
                    remove_action.is_some(),
                    remove_action,
                ),
            ));
            let rewire_items = channel
                .route_create_options
                .iter()
                .filter_map(|option| {
                    audio_mixer_rewire_route_action(route, option)
                        .map(|action| MenuItem::new(option.label.clone(), action))
                })
                .collect::<Vec<_>>();
            if !rewire_items.is_empty() {
                route_section = route_section.with_row(PropertyRow::new(
                    "重连",
                    Box::new(
                        Dropdown::new("选择新的 tap 与目标…", rewire_items)
                            .with_max_visible_items(12),
                    ),
                ));
            }
            if let Some(reason) = &route.edit_disabled_reason {
                route_section = route_section.with_row(PropertyRow::new(
                    "只读",
                    Box::new(Label::new(reason.clone()).muted()),
                ));
            }
            panel = panel.with_section(route_section);
        }
        panel = with_audio_processor_rack_sections(
            panel,
            &channel.processor_racks,
            channel.is_editable,
        );
    }
    panel
}

pub(super) fn audio_mixer_meter_label(
    channel: &crate::app_ui::audio_mixer::AudioMixerChannelModel,
) -> String {
    let Some(meter) = &channel.meter else {
        return "未执行".to_owned();
    };
    let peak = meter
        .channels
        .iter()
        .map(|reading| reading.sample_peak_linear)
        .fold(0.0_f32, f32::max);
    let rms = meter.channels.iter().map(|reading| reading.rms_linear).fold(0.0_f64, f64::max);
    let clipped = meter.channels.iter().map(|reading| reading.clipped_sample_count).sum::<u64>();
    let invalid = meter
        .channels
        .iter()
        .map(|reading| reading.non_finite_sample_count)
        .sum::<u64>();
    let warning = match (clipped, invalid) {
        (0, 0) => String::new(),
        (clipped, 0) => format!(" · CLIP {clipped}"),
        (0, invalid) => format!(" · 非有限 {invalid}"),
        (clipped, invalid) => format!(" · CLIP {clipped} · 非有限 {invalid}"),
    };
    format!(
        "P {} · RMS {}{warning}",
        audio_meter_dbfs_label(f64::from(peak)),
        audio_meter_dbfs_label(rms),
    )
}

pub(super) fn audio_meter_dbfs_label(linear: f64) -> String {
    if linear <= 0.0 {
        "−∞ dBFS".to_owned()
    } else {
        format!("{:.1} dBFS", 20.0 * linear.log10())
    }
}

pub(super) fn inspector_panel(model: &InspectorPanelModel) -> PropertyPanel {
    let selected_clip = model.selected_clip;
    let opacity_parameter =
        model.visual_parameters.as_ref().and_then(|targets| targets.opacity.clone());
    let position_x_parameter =
        model.visual_parameters.as_ref().and_then(|targets| targets.position.clone());
    let position_y_parameter = position_x_parameter.clone();
    let scale_x_parameter =
        model.visual_parameters.as_ref().and_then(|targets| targets.scale.clone());
    let scale_y_parameter = scale_x_parameter.clone();
    let anchor_x_parameter =
        model.visual_parameters.as_ref().and_then(|targets| targets.anchor.clone());
    let anchor_y_parameter = anchor_x_parameter.clone();
    let rotation_parameter =
        model.visual_parameters.as_ref().and_then(|targets| targets.rotation.clone());
    let position_x = model.position_x;
    let position_y = model.position_y;
    let scale_x = model.scale_x_percent;
    let scale_y = model.scale_y_percent;
    let anchor_x = model.anchor_x;
    let anchor_y = model.anchor_y;
    let has_target = selected_clip.is_some();
    let can_edit = has_target && model.is_editable;
    let subtitle = model.edit_disabled_reason.as_deref().unwrap_or(if has_target {
        "Selected clip"
    } else {
        "No clip selected"
    });
    if let Some(message) = model.empty_message.as_deref().filter(|message| !message.is_empty()) {
        let (title, description) = message
            .split_once('\n')
            .map_or((message, ""), |(title, description)| (title, description));
        let mut panel = PropertyPanel::with_options(
            "检查器",
            PropertyPanelOptions {
                label_width: 0.0,
                control_gap: 0.0,
                row_height: 28.0,
                section_gap: 0.0,
                ..PropertyPanelOptions::default()
            },
        )
        .with_embedded_panel_chrome()
        .with_empty_state(title, description);
        if let Ok(icon) = AppIcon::Info.vector_icon() {
            panel = panel.with_empty_state_icon(icon);
        }
        return panel;
    }
    let curve = if let Some(curve_model) = model.opacity_curve.clone() {
        let points = curve_model.keys.iter().map(|key| key.point).collect();
        let point_policies = curve_model
            .keys
            .iter()
            .map(|key| {
                if key.keyframe_id.is_some() {
                    CurvePointPolicy::editable()
                } else {
                    CurvePointPolicy::anchor()
                }
            })
            .collect();
        let display_points = curve_model.display_points.clone();
        CurveEditor::with_points(points)
            .with_point_policies(point_policies)
            .with_display_points(display_points)
            .enabled(can_edit)
            .on_edit(move |edit| inspector_curve_edit_action(selected_clip, &curve_model, edit))
    } else {
        CurveEditor::with_points(vec![
            CurvePoint::new(0.0, model.opacity / 100.0),
            CurvePoint::new(1.0, model.opacity / 100.0),
        ])
        .disabled()
    };
    let mut style_section = PropertySection::new("剪辑样式")
        .with_row(PropertyRow::new(
            "启用",
            Box::new(
                Checkbox::new("启用效果", model.enabled)
                    .enabled(can_edit)
                    .on_change(move |value| inspector_bool_action(selected_clip, value)),
            ),
        ))
        .with_row(PropertyRow::new(
            "不透明度",
            numeric_slider_input_control(
                model.opacity,
                0.0,
                100.0,
                Some(1.0),
                0,
                can_edit,
                move |value| {
                    inspector_parameter_action(
                        selected_clip,
                        opacity_parameter.clone(),
                        PropertyValue::Float(value / 100.0),
                    )
                },
            ),
        ));
    if model.shows_tint {
        let mut tint = color_picker_trigger(model.tint).enabled(can_edit);
        tint.picker_mut().set_area_mode(model.tint_area_mode);
        style_section = style_section.with_row(PropertyRow::new(
            "颜色",
            Box::new(tint.on_change(move |color| inspector_color_action(selected_clip, color))),
        ));
    }
    let mut panel = PropertyPanel::new("检查器")
        .with_subtitle(subtitle)
        .with_embedded_panel_chrome()
        .with_section(style_section);

    if !model.clip_properties.is_empty() {
        let mut section = PropertySection::new("基础标题");
        for property in &model.clip_properties {
            section = section.with_row(clip_property_row(property, can_edit, selected_clip));
        }
        panel = panel.with_section(section);
    }

    if !model.audio_components.is_empty() {
        for (index, component) in model.audio_components.iter().enumerate() {
            let edit_id = component.edit_id;
            let volume_static_editable = can_edit
                && component.volume_automation.as_ref().is_none_or(|curve| !curve.is_automated());
            let pan_static_editable = can_edit
                && component.pan_automation.as_ref().is_none_or(|curve| !curve.is_automated());
            let section_title = if model.audio_components.len() == 1 {
                "音频 Component".to_owned()
            } else {
                format!("音频 Component {}", index + 1)
            };
            let mut section = PropertySection::new(section_title)
                .with_row(PropertyRow::new(
                    "启用",
                    Box::new(
                        Checkbox::new("参与混音", component.enabled).enabled(can_edit).on_change(
                            move |value| {
                                audio_component_mutation_action(
                                    selected_clip,
                                    edit_id,
                                    AudioComponentMutation::SetEnabled { value },
                                )
                            },
                        ),
                    ),
                ))
                .with_row(PropertyRow::new(
                    "音量 (dB)",
                    numeric_slider_input_control_with_hard_range(
                        component.volume_db as f32,
                        -60.0,
                        12.0,
                        AUDIO_GAIN_DB_MIN as f32,
                        AUDIO_GAIN_DB_MAX as f32,
                        Some(0.1),
                        1,
                        volume_static_editable,
                        move |value| {
                            audio_component_mutation_action(
                                selected_clip,
                                edit_id,
                                AudioComponentMutation::SetVolumeDb { value: f64::from(value) },
                            )
                        },
                    ),
                ))
                .with_row(PropertyRow::new(
                    "声像 / Balance",
                    numeric_slider_input_control(
                        (component.pan * 100.0) as f32,
                        -100.0,
                        100.0,
                        Some(1.0),
                        0,
                        pan_static_editable,
                        move |value| {
                            audio_component_mutation_action(
                                selected_clip,
                                edit_id,
                                AudioComponentMutation::SetPan { value: f64::from(value) / 100.0 },
                            )
                        },
                    ),
                ));
            if let Some(automation) = &component.volume_automation {
                section = section.with_row(
                    PropertyRow::new("音量曲线", audio_automation_curve_control(automation))
                        .with_height(118.0),
                );
            }
            if let Some(automation) = &component.pan_automation {
                section = section.with_row(
                    PropertyRow::new("声像曲线", audio_automation_curve_control(automation))
                        .with_height(118.0),
                );
            }
            let source_items = component
                .source_options
                .iter()
                .map(|option| {
                    let mut item = MenuItem::new(
                        option.label.clone(),
                        inspector_audio_source_action(
                            selected_clip,
                            edit_id,
                            option.source.clone(),
                        ),
                    )
                    .checked(option.selected);
                    if !option.selectable {
                        item = item.disabled();
                    }
                    item
                })
                .collect::<Vec<_>>();
            let source_enabled = can_edit && !source_items.is_empty();
            section = section.with_row(PropertyRow::new(
                "逻辑源",
                Box::new(
                    Dropdown::new(component.source_label.clone(), source_items)
                        .with_max_visible_items(8)
                        .enabled(source_enabled),
                ),
            ));

            if let Some(binding) = &component.binding {
                let mut binding_items = vec![
                    MenuItem::new(
                        "重新探测当前文件…",
                        inspector_audio_refresh_action(binding.asset_id),
                    ),
                    MenuItem::separator(),
                ];
                binding_items.extend(binding.options.iter().map(|option| {
                    MenuItem::new(
                        option.label.clone(),
                        inspector_audio_rebind_action(
                            binding.asset_id,
                            binding.component_id,
                            option.stream_index,
                        ),
                    )
                    .checked(option.selected)
                }));
                let binding_enabled = can_edit;
                section = section.with_row(PropertyRow::new(
                    "资产流映射",
                    Box::new(
                        Dropdown::new(binding.label.clone(), binding_items)
                            .with_max_visible_items(8)
                            .enabled(binding_enabled),
                    ),
                ));
            }
            section = with_audio_channel_mapping_rows(
                section,
                selected_clip,
                edit_id,
                &component.channel_mapping,
                can_edit,
            );
            let max_fade_seconds = component.clip_duration.to_f64().max(0.0) as f32;
            let fade_in_curve =
                component.fade_in.map(|fade| fade.curve).unwrap_or(AudioFadeCurve::EqualPower);
            let fade_out_curve =
                component.fade_out.map(|fade| fade.curve).unwrap_or(AudioFadeCurve::EqualPower);
            let fade_in_seconds =
                component.fade_in.map(|fade| fade.duration.to_f64() as f32).unwrap_or(0.0);
            let fade_out_seconds =
                component.fade_out.map(|fade| fade.duration.to_f64() as f32).unwrap_or(0.0);
            section = section
                .with_row(PropertyRow::new(
                    "淡入 (s)",
                    numeric_slider_input_control(
                        fade_in_seconds,
                        0.0,
                        max_fade_seconds,
                        Some(0.01),
                        3,
                        can_edit,
                        move |value| {
                            inspector_audio_fade_duration_action(
                                selected_clip,
                                edit_id,
                                true,
                                value,
                                fade_in_curve,
                            )
                        },
                    ),
                ))
                .with_row(PropertyRow::new(
                    "淡入曲线",
                    Box::new(
                        Dropdown::new(
                            audio_fade_curve_label(fade_in_curve),
                            audio_fade_curve_items(selected_clip, edit_id, true, component.fade_in),
                        )
                        .enabled(can_edit && component.fade_in.is_some()),
                    ),
                ))
                .with_row(PropertyRow::new(
                    "淡出 (s)",
                    numeric_slider_input_control(
                        fade_out_seconds,
                        0.0,
                        max_fade_seconds,
                        Some(0.01),
                        3,
                        can_edit,
                        move |value| {
                            inspector_audio_fade_duration_action(
                                selected_clip,
                                edit_id,
                                false,
                                value,
                                fade_out_curve,
                            )
                        },
                    ),
                ))
                .with_row(PropertyRow::new(
                    "淡出曲线",
                    Box::new(
                        Dropdown::new(
                            audio_fade_curve_label(fade_out_curve),
                            audio_fade_curve_items(
                                selected_clip,
                                edit_id,
                                false,
                                component.fade_out,
                            ),
                        )
                        .enabled(can_edit && component.fade_out.is_some()),
                    ),
                ));
            panel = panel.with_section(section);
        }
    }

    panel = with_audio_processor_rack_sections(panel, &model.audio_processor_racks, can_edit);

    panel = panel.with_section(
        PropertySection::new("变换")
            .with_row(PropertyRow::new(
                "位置 X (px)",
                numeric_slider_input_control_with_hard_range(
                    model.position_x,
                    -4096.0,
                    4096.0,
                    -1_000_000.0,
                    1_000_000.0,
                    Some(1.0),
                    0,
                    can_edit,
                    move |value| {
                        inspector_parameter_action(
                            selected_clip,
                            position_x_parameter.clone(),
                            PropertyValue::Vec2(glam::Vec2::new(value, position_y)),
                        )
                    },
                ),
            ))
            .with_row(PropertyRow::new(
                "位置 Y (px)",
                numeric_slider_input_control_with_hard_range(
                    model.position_y,
                    -4096.0,
                    4096.0,
                    -1_000_000.0,
                    1_000_000.0,
                    Some(1.0),
                    0,
                    can_edit,
                    move |value| {
                        inspector_parameter_action(
                            selected_clip,
                            position_y_parameter.clone(),
                            PropertyValue::Vec2(glam::Vec2::new(position_x, value)),
                        )
                    },
                ),
            ))
            .with_row(PropertyRow::new(
                "缩放 X (%)",
                numeric_slider_input_control_with_hard_range(
                    model.scale_x_percent,
                    -400.0,
                    400.0,
                    -100_000.0,
                    100_000.0,
                    Some(1.0),
                    0,
                    can_edit,
                    move |value| {
                        inspector_parameter_action(
                            selected_clip,
                            scale_x_parameter.clone(),
                            PropertyValue::Vec2(glam::Vec2::new(value / 100.0, scale_y / 100.0)),
                        )
                    },
                ),
            ))
            .with_row(PropertyRow::new(
                "缩放 Y (%)",
                numeric_slider_input_control_with_hard_range(
                    model.scale_y_percent,
                    -400.0,
                    400.0,
                    -100_000.0,
                    100_000.0,
                    Some(1.0),
                    0,
                    can_edit,
                    move |value| {
                        inspector_parameter_action(
                            selected_clip,
                            scale_y_parameter.clone(),
                            PropertyValue::Vec2(glam::Vec2::new(scale_x / 100.0, value / 100.0)),
                        )
                    },
                ),
            ))
            .with_row(PropertyRow::new(
                "锚点 X (px)",
                numeric_slider_input_control_with_hard_range(
                    model.anchor_x,
                    -4096.0,
                    4096.0,
                    -1_000_000.0,
                    1_000_000.0,
                    Some(1.0),
                    0,
                    can_edit,
                    move |value| {
                        inspector_parameter_action(
                            selected_clip,
                            anchor_x_parameter.clone(),
                            PropertyValue::Vec2(glam::Vec2::new(value, anchor_y)),
                        )
                    },
                ),
            ))
            .with_row(PropertyRow::new(
                "锚点 Y (px)",
                numeric_slider_input_control_with_hard_range(
                    model.anchor_y,
                    -4096.0,
                    4096.0,
                    -1_000_000.0,
                    1_000_000.0,
                    Some(1.0),
                    0,
                    can_edit,
                    move |value| {
                        inspector_parameter_action(
                            selected_clip,
                            anchor_y_parameter.clone(),
                            PropertyValue::Vec2(glam::Vec2::new(anchor_x, value)),
                        )
                    },
                ),
            ))
            .with_row(PropertyRow::new(
                "旋转 (°)",
                numeric_slider_input_control(
                    model.rotation_degrees,
                    -180.0,
                    180.0,
                    Some(0.1),
                    1,
                    can_edit,
                    move |value| {
                        inspector_parameter_action(
                            selected_clip,
                            rotation_parameter.clone(),
                            PropertyValue::Float(value),
                        )
                    },
                ),
            )),
    );

    let timeline_time_base = model.timeline_time_base;
    let mut timing_section = PropertySection::new("时间")
        .with_row(PropertyRow::new(
            "In",
            numeric_slider_input_control(
                model.in_frame,
                0.0,
                model.max_frame,
                Some(1.0),
                0,
                can_edit,
                move |value| {
                    inspector_timing_action(
                        selected_clip,
                        TimelineTrimPayloadEdge::In,
                        value,
                        timeline_time_base,
                    )
                },
            ),
        ))
        .with_row(PropertyRow::new(
            "Out",
            numeric_slider_input_control(
                model.out_frame,
                0.0,
                model.max_frame,
                Some(1.0),
                0,
                can_edit,
                move |value| {
                    inspector_timing_action(
                        selected_clip,
                        TimelineTrimPayloadEdge::Out,
                        value,
                        timeline_time_base,
                    )
                },
            ),
        ));
    if let Some(source_timing) = model.source_timing {
        match source_timing.mode {
            InspectorSourceTimingMode::Rate { rate_percent } => {
                let rate_enabled = can_edit && source_timing.can_set_rate;
                timing_section = timing_section.with_row(PropertyRow::new(
                    "速度 (%)",
                    numeric_slider_input_control_with_hard_range(
                        rate_percent,
                        -400.0,
                        400.0,
                        -10_000.0,
                        10_000.0,
                        Some(0.01),
                        2,
                        rate_enabled,
                        move |value| inspector_rate_action(selected_clip, value),
                    ),
                ));
            }
            InspectorSourceTimingMode::Hold => {
                timing_section = timing_section.with_row(PropertyRow::new(
                    "源时间",
                    Box::new(Label::new("定格").muted()),
                ));
                let rate_enabled = can_edit && source_timing.can_set_rate;
                timing_section = timing_section.with_row(PropertyRow::new(
                    "恢复速度 (%)",
                    numeric_slider_input_control_with_hard_range(
                        100.0,
                        1.0,
                        400.0,
                        0.01,
                        10_000.0,
                        Some(0.01),
                        2,
                        rate_enabled,
                        move |value| inspector_rate_action(selected_clip, value),
                    ),
                ));
            }
        }
        if source_timing.supports_picture_hold {
            let freeze_target = source_timing.freeze_at_playhead;
            let label = match (source_timing.mode, freeze_target) {
                (_, None) => "先将播放头移入片段",
                (InspectorSourceTimingMode::Hold, Some(_)) => "更新为播放头画面",
                _ => "在播放头创建定格",
            };
            timing_section = timing_section.with_row(PropertyRow::new(
                "定格帧",
                Box::new(
                    Button::new(label)
                        .enabled(can_edit && freeze_target.is_some())
                        .on_click(inspector_hold_action(selected_clip, freeze_target)),
                ),
            ));
        }
    }
    panel = panel.with_section(timing_section);

    if selected_clip.is_some_and(|selection| selection.is_video_track) {
        panel = panel.with_section(
            PropertySection::new("蒙版").with_row(PropertyRow::new(
                "添加",
                Box::new(
                    Button::new("添加矩形蒙版")
                        .enabled(can_edit)
                        .on_click(inspector_add_mask_action(selected_clip)),
                ),
            )),
        );
    }

    for (index, mask) in model.masks.iter().enumerate() {
        let mask_id = mask.mask_id;
        let mask_can_edit = can_edit && !mask.locked;
        let can_move_up = mask_can_edit && index > 0;
        let can_move_down = mask_can_edit && index + 1 < model.masks.len();
        let shape_items = vec![
            MenuItem::new(
                "矩形",
                inspector_mask_shape_action(selected_clip, mask_id, MaskShape::default()),
            ),
            MenuItem::new(
                "椭圆",
                inspector_mask_shape_action(
                    selected_clip,
                    mask_id,
                    MaskShape::Ellipse {
                        center: glam::Vec2::splat(0.5),
                        radii: glam::Vec2::splat(0.4),
                    },
                ),
            ),
        ];
        let mut section = PropertySection::new(mask.label.clone())
            .selected(model.selected_mask_id == Some(mask_id))
            .on_select(inspector_mask_select_action(selected_clip, mask_id))
            .with_row(PropertyRow::new(
                "控制",
                Box::new(
                    FlexContainer::row(vec![
                        FlexChild::flex(
                            Box::new(
                                Checkbox::new("启用", mask.enabled).enabled(can_edit).on_change(
                                    move |enabled| {
                                        inspector_mask_enabled_action(
                                            selected_clip,
                                            mask_id,
                                            enabled,
                                        )
                                    },
                                ),
                            ),
                            1.0,
                        ),
                        FlexChild::fixed(effect_icon_button(
                            AppIcon::CaretUp,
                            "Up",
                            "Move Mask up",
                            can_move_up,
                            can_move_up
                                .then(|| {
                                    inspector_reorder_mask_action(
                                        selected_clip,
                                        mask_id,
                                        MaskRelativePlacement::Before(
                                            model.masks[index - 1].mask_id,
                                        ),
                                    )
                                })
                                .flatten(),
                        )),
                        FlexChild::fixed(effect_icon_button(
                            AppIcon::CaretDown,
                            "Down",
                            "Move Mask down",
                            can_move_down,
                            can_move_down
                                .then(|| {
                                    inspector_reorder_mask_action(
                                        selected_clip,
                                        mask_id,
                                        MaskRelativePlacement::After(
                                            model.masks[index + 1].mask_id,
                                        ),
                                    )
                                })
                                .flatten(),
                        )),
                        FlexChild::fixed(effect_icon_button(
                            AppIcon::Trash,
                            "Remove",
                            "Remove Mask",
                            mask_can_edit,
                            inspector_remove_mask_action(selected_clip, mask_id),
                        )),
                    ])
                    .with_gap(8.0),
                ),
            ))
            .with_row(PropertyRow::new(
                "锁定",
                Box::new(
                    Checkbox::new("锁定编辑", mask.locked).enabled(can_edit).on_change(
                        move |locked| inspector_mask_locked_action(selected_clip, mask_id, locked),
                    ),
                ),
            ))
            .with_row(PropertyRow::new(
                "形状",
                Box::new(
                    Dropdown::new(mask.shape_label.clone(), shape_items).enabled(mask_can_edit),
                ),
            ))
            .with_row(PropertyRow::new(
                "形状动画",
                Box::new(
                    Checkbox::new("关键帧", mask.shape_animation_enabled)
                        .enabled(mask_can_edit)
                        .on_change(move |enabled| {
                            inspector_mask_shape_animation_action(selected_clip, mask_id, enabled)
                        }),
                ),
            ));
        for property in &mask.properties {
            section = section.with_row(mask_property_row(
                property,
                mask_can_edit,
                selected_clip,
                mask_id,
            ));
        }
        panel = panel.with_section(section);
    }

    if !model.effects.is_empty() {
        for (index, effect) in model.effects.iter().enumerate() {
            let effect_id = effect.effect_id;
            let can_move_up = can_edit && index > 0;
            let can_move_down = can_edit && index + 1 < model.effects.len();
            let mut section = PropertySection::new(effect.label.clone())
                .selected(model.selected_effect_id == Some(effect_id))
                .on_select(inspector_effect_select_action(selected_clip, effect_id))
                .with_row(PropertyRow::new(
                    "控制",
                    Box::new(
                        FlexContainer::row(vec![
                            FlexChild::flex(
                                Box::new(
                                    Checkbox::new("启用", effect.enabled)
                                        .enabled(can_edit)
                                        .on_change(move |enabled| {
                                            inspector_effect_enabled_action(
                                                selected_clip,
                                                effect_id,
                                                enabled,
                                            )
                                        }),
                                ),
                                1.0,
                            ),
                            FlexChild::fixed(effect_icon_button(
                                AppIcon::CaretUp,
                                "Up",
                                "Move effect up",
                                can_move_up,
                                can_move_up
                                    .then(|| {
                                        inspector_reorder_effect_action(
                                            selected_clip,
                                            effect_id,
                                            EffectRelativePlacement::Before(
                                                model.effects[index - 1].effect_id,
                                            ),
                                        )
                                    })
                                    .flatten(),
                            )),
                            FlexChild::fixed(effect_icon_button(
                                AppIcon::CaretDown,
                                "Down",
                                "Move effect down",
                                can_move_down,
                                can_move_down
                                    .then(|| {
                                        inspector_reorder_effect_action(
                                            selected_clip,
                                            effect_id,
                                            EffectRelativePlacement::After(
                                                model.effects[index + 1].effect_id,
                                            ),
                                        )
                                    })
                                    .flatten(),
                            )),
                            FlexChild::fixed(effect_icon_button(
                                AppIcon::Trash,
                                "Remove",
                                "Remove effect",
                                can_edit,
                                inspector_remove_effect_row_action(selected_clip, effect_id),
                            )),
                        ])
                        .with_gap(8.0),
                    ),
                ));
            for property in &effect.properties {
                section = section.with_row(effect_property_row(
                    property,
                    can_edit,
                    selected_clip,
                    effect_id,
                ));
            }
            panel = panel.with_section(section);
        }
    }

    panel.with_section(
        PropertySection::new("动画")
            .with_row(PropertyRow::new("曲线", Box::new(curve)).with_height(118.0)),
    )
}

pub(super) fn with_audio_processor_rack_sections(
    mut panel: PropertyPanel,
    racks: &[AudioProcessorRackModel],
    surface_editable: bool,
) -> PropertyPanel {
    for (rack_index, rack) in racks.iter().enumerate() {
        let rack_can_edit = surface_editable && rack.is_editable;
        let duplicate_title_count =
            racks.iter().filter(|candidate| candidate.title == rack.title).count();
        let title = if duplicate_title_count == 1 {
            rack.title.clone()
        } else {
            format!("{} {}", rack.title, rack_index + 1)
        };
        let insert_items = rack
            .insert_options
            .iter()
            .map(|option| {
                MenuItem::new(
                    option.label,
                    audio_processor_insert_action(rack, option.preset),
                )
            })
            .collect();
        let mut rack_section = PropertySection::new(title)
            .with_row(PropertyRow::new(
                "作用域",
                Box::new(Label::new(rack.ownership_label.clone()).muted()),
            ))
            .with_row(PropertyRow::new(
                "添加",
                Box::new(
                    Dropdown::new("添加处理器…", insert_items)
                        .with_max_visible_items(8)
                        .enabled(rack_can_edit),
                ),
            ));
        if let Some(reason) = &rack.edit_disabled_reason {
            rack_section = rack_section.with_row(PropertyRow::new(
                "只读",
                Box::new(Label::new(reason.clone()).muted()),
            ));
        }
        if let Some(automation) = &rack.scope_input_gain_automation {
            rack_section = rack_section.with_row(
                PropertyRow::new("Scope 输入曲线", audio_automation_curve_control(automation))
                    .with_height(118.0),
            );
        }
        panel = panel.with_section(rack_section);

        for (processor_index, processor) in rack.processors.iter().enumerate() {
            let rack_for_bypass = rack.clone();
            let processor_for_bypass = processor.clone();
            let can_move_up = rack_can_edit && processor_index > 0;
            let can_move_down = rack_can_edit && processor_index + 1 < rack.processors.len();
            let move_up = if processor_index > 0 {
                audio_processor_move_before_action(
                    rack,
                    processor,
                    rack.processors[processor_index - 1].processor_id,
                )
            } else {
                audio_processor_move_before_action(rack, processor, processor.processor_id)
            };
            let move_down = if processor_index + 2 < rack.processors.len() {
                audio_processor_move_before_action(
                    rack,
                    processor,
                    rack.processors[processor_index + 2].processor_id,
                )
            } else {
                audio_processor_move_to_end_action(rack, processor)
            };
            let mut section =
                PropertySection::new(processor.label.clone()).with_row(PropertyRow::new(
                    "控制",
                    Box::new(
                        FlexContainer::row(vec![
                            FlexChild::flex(
                                Box::new(
                                    Checkbox::new("旁路", processor.bypassed)
                                        .enabled(rack_can_edit)
                                        .on_change(move |bypassed| {
                                            audio_processor_bypass_action(
                                                &rack_for_bypass,
                                                &processor_for_bypass,
                                                bypassed,
                                            )
                                        }),
                                ),
                                1.0,
                            ),
                            FlexChild::fixed(effect_icon_button(
                                AppIcon::CaretUp,
                                "Up",
                                "Move processor up",
                                can_move_up,
                                Some(move_up),
                            )),
                            FlexChild::fixed(effect_icon_button(
                                AppIcon::CaretDown,
                                "Down",
                                "Move processor down",
                                can_move_down,
                                Some(move_down),
                            )),
                            FlexChild::fixed(effect_icon_button(
                                AppIcon::Trash,
                                "Remove",
                                "Remove processor",
                                rack_can_edit,
                                Some(audio_processor_remove_action(rack, processor)),
                            )),
                        ])
                        .with_gap(8.0),
                    ),
                ));
            for parameter in &processor.parameters {
                let label = audio_processor_parameter_label(parameter);
                if parameter.keyframe_count > 0 {
                    section = section.with_row(PropertyRow::new(
                        label.clone(),
                        Box::new(
                            Label::new(format!("自动化 · {} 个关键帧", parameter.keyframe_count))
                                .muted(),
                        ),
                    ));
                } else if let Some(numeric) = parameter.schema.numeric {
                    let rack_for_parameter = rack.clone();
                    let processor_for_parameter = processor.clone();
                    let parameter_for_action = parameter.clone();
                    section = section.with_row(PropertyRow::new(
                        label.clone(),
                        numeric_slider_input_control_with_hard_range(
                            parameter.static_value as f32,
                            numeric.soft_range.min as f32,
                            numeric.soft_range.max as f32,
                            numeric.hard_range.min as f32,
                            numeric.hard_range.max as f32,
                            numeric.step.map(|step| step as f32),
                            audio_processor_parameter_decimals(numeric.step),
                            rack_can_edit && parameter.is_static_editable(),
                            move |value| {
                                audio_processor_set_static_parameter_action(
                                    &rack_for_parameter,
                                    &processor_for_parameter,
                                    &parameter_for_action,
                                    value,
                                )
                            },
                        ),
                    ));
                } else {
                    section = section.with_row(PropertyRow::new(
                        label.clone(),
                        Box::new(Label::new("此参数没有数值编辑契约").muted()),
                    ));
                }
                if let Some(automation) = &parameter.automation {
                    section = section.with_row(
                        PropertyRow::new(
                            format!("{label} 曲线"),
                            audio_automation_curve_control(automation),
                        )
                        .with_height(118.0),
                    );
                }
            }
            panel = panel.with_section(section);
        }
    }
    panel
}

pub(super) fn audio_processor_parameter_label(
    parameter: &crate::app_ui::audio_processor_rack::AudioProcessorParameterModel,
) -> String {
    let unit = match parameter.schema.unit {
        ParameterUnit::Decibels => "dB",
        ParameterUnit::Milliseconds => "ms",
        ParameterUnit::Samples => "samples",
        ParameterUnit::Percent => "%",
        ParameterUnit::Degrees => "°",
        ParameterUnit::Pixels => "px",
        ParameterUnit::Stops => "stops",
        ParameterUnit::Nits => "nits",
        ParameterUnit::Unitless | ParameterUnit::Normalized | ParameterUnit::TimelineTime => "",
    };
    if unit.is_empty() {
        parameter.label.clone()
    } else {
        format!("{} ({unit})", parameter.label)
    }
}

pub(super) fn audio_processor_parameter_decimals(step: Option<f64>) -> usize {
    match step {
        Some(step) if step >= 1.0 => 0,
        Some(step) if step >= 0.1 => 1,
        Some(step) if step >= 0.01 => 2,
        Some(_) => 3,
        None => 2,
    }
}

pub(super) fn audio_automation_curve_control(model: &AudioAutomationCurveModel) -> Box<dyn Widget> {
    let action_model = model.clone();
    Box::new(
        CurveEditor::with_points(model.points())
            .with_point_policies(model.point_policies())
            .with_display_points(model.display_points.clone())
            .enabled(model.is_editable)
            .on_edit(move |edit| audio_automation_curve_edit_action(&action_model, edit)),
    )
}

pub(super) fn numeric_slider_input_control<R>(
    value: f32,
    min: f32,
    max: f32,
    step: Option<f32>,
    decimals: usize,
    enabled: bool,
    action: impl Fn(f32) -> R + 'static,
) -> Box<dyn Widget>
where
    R: Into<Option<Action>>,
{
    numeric_slider_input_control_with_hard_range(
        value, min, max, min, max, step, decimals, enabled, action,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn numeric_slider_input_control_with_hard_range<R>(
    value: f32,
    soft_min: f32,
    soft_max: f32,
    hard_min: f32,
    hard_max: f32,
    step: Option<f32>,
    decimals: usize,
    enabled: bool,
    action: impl Fn(f32) -> R + 'static,
) -> Box<dyn Widget>
where
    R: Into<Option<Action>>,
{
    let action: Rc<dyn Fn(f32) -> Option<Action>> = Rc::new(move |value| action(value).into());
    let mut slider =
        Slider::new(value.clamp(soft_min, soft_max), soft_min, soft_max).enabled(enabled);
    if let Some(step) = step.filter(|step| step.is_finite() && *step > 0.0) {
        slider = slider.with_step(step);
    }
    let slider_action = Rc::clone(&action);
    slider = slider.on_change(move |value| slider_action(value));

    let mut input = NumberInput::new(value as f64, hard_min as f64, hard_max as f64)
        .with_width(72.0)
        .with_decimals(decimals)
        .enabled(enabled);
    if let Some(step) = step.filter(|step| step.is_finite() && *step > 0.0) {
        input = input.with_step(step as f64);
    }
    let input_action = Rc::clone(&action);
    input = input.on_change(move |value| input_action(value as f32));

    Box::new(
        FlexContainer::row(vec![
            FlexChild::flex(Box::new(slider), 1.0),
            FlexChild::fixed(Box::new(input)),
        ])
        .with_gap(8.0),
    )
}

pub(super) fn inspector_bool_action(
    selection: Option<SelectedClipRef>,
    value: bool,
) -> Option<Action> {
    selection.map(|selection| {
        clip_set_enabled_action(ClipSetEnabledPayload {
            clip_id: selection.clip_id,
            enabled: value,
        })
    })
}

pub(super) fn inspector_audio_source_action(
    selection: Option<SelectedClipRef>,
    edit_id: AudioComponentEditId,
    source: AudioComponentSource,
) -> Option<Action> {
    audio_component_mutation_action(
        selection,
        edit_id,
        AudioComponentMutation::SetSource { value: source },
    )
}

const INSPECTOR_AUDIO_FADE_TIMESCALE: u32 = 1_000;

pub(super) fn inspector_audio_fade_duration_action(
    selection: Option<SelectedClipRef>,
    edit_id: AudioComponentEditId,
    fade_in: bool,
    seconds: f32,
    curve: AudioFadeCurve,
) -> Option<Action> {
    if !seconds.is_finite() {
        return None;
    }
    let fade = if seconds <= 0.0 {
        None
    } else {
        let Ok(duration) =
            TimelineTime::from_f64_quantized(f64::from(seconds), INSPECTOR_AUDIO_FADE_TIMESCALE)
        else {
            return None;
        };
        (duration > TimelineTime::ZERO).then_some(AudioFade { duration, curve })
    };
    let mutation = if fade_in {
        AudioComponentMutation::SetFadeIn { value: fade }
    } else {
        AudioComponentMutation::SetFadeOut { value: fade }
    };
    audio_component_mutation_action(selection, edit_id, mutation)
}

pub(super) fn audio_fade_curve_items(
    selection: Option<SelectedClipRef>,
    edit_id: AudioComponentEditId,
    fade_in: bool,
    fade: Option<AudioFade>,
) -> Vec<MenuItem> {
    let Some(fade) = fade else {
        return Vec::new();
    };
    [AudioFadeCurve::ConstantGain, AudioFadeCurve::EqualPower]
        .into_iter()
        .map(|curve| {
            let updated = Some(AudioFade { duration: fade.duration, curve });
            let mutation = if fade_in {
                AudioComponentMutation::SetFadeIn { value: updated }
            } else {
                AudioComponentMutation::SetFadeOut { value: updated }
            };
            MenuItem::new(
                audio_fade_curve_label(curve),
                audio_component_mutation_action(selection, edit_id, mutation),
            )
            .checked(curve == fade.curve)
        })
        .collect()
}

pub(super) fn audio_fade_curve_label(curve: AudioFadeCurve) -> &'static str {
    match curve {
        AudioFadeCurve::ConstantGain => "Constant Gain",
        AudioFadeCurve::EqualPower => "Equal Power",
    }
}

pub(super) fn inspector_audio_rebind_action(
    asset_id: AssetId,
    component_id: AudioSourceComponentId,
    stream_index: u32,
) -> Action {
    assets_rebind_audio_component_action(AssetsRebindAudioComponentPayload {
        asset_id,
        component_id,
        stream_index,
    })
}

pub(super) fn inspector_audio_refresh_action(asset_id: AssetId) -> Action {
    assets_refresh_audio_components_action(AssetsRefreshAudioComponentsPayload { asset_id })
}

pub(super) fn inspector_color_action(
    selection: Option<SelectedClipRef>,
    color: Color,
) -> Option<Action> {
    selection.map(|selection| {
        clip_set_solid_color_action(ClipSetSolidColorPayload { clip_id: selection.clip_id, color })
    })
}

pub(super) fn inspector_parameter_action(
    selection: Option<SelectedClipRef>,
    parameter: Option<AnimationParameterAddress>,
    value: PropertyValue,
) -> Option<Action> {
    let selection = selection?;
    let parameter = parameter?;
    Some(clip_write_parameter_values_action(
        ClipWriteParameterValuesPayload {
            clip_id: selection.clip_id,
            writes: vec![ClipParameterValueWrite { parameter, value }],
        },
    ))
}

pub(super) fn inspector_timing_action(
    selection: Option<SelectedClipRef>,
    edge: TimelineTrimPayloadEdge,
    frame: f32,
    time_base: Rational,
) -> Option<Action> {
    let frame = if frame.is_finite() {
        frame.round() as i64
    } else {
        0
    };
    if let Some(selection) = selection {
        return Some(timeline_trim_clips_action(TimelineTrimClipsPayload {
            clip_ids: vec![selection.clip_id],
            edge,
            position: FramePosition::new(frame.max(0), time_base),
        }));
    }
    None
}

pub(super) fn inspector_effect_enabled_action(
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
    enabled: bool,
) -> Option<Action> {
    selection.map(|selection| {
        visual_effect_set_enabled_action(VisualEffectSetEnabledPayload {
            clip_id: selection.clip_id,
            effect_id,
            enabled,
        })
    })
}

pub(super) fn inspector_effect_select_action(
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
) -> Option<Action> {
    selection.map(|selection| {
        visual_effect_select_action(VisualEffectTargetPayload {
            clip_id: selection.clip_id,
            effect_id,
        })
    })
}

pub(super) fn inspector_remove_effect_row_action(
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
) -> Option<Action> {
    selection.map(|selection| {
        visual_effect_remove_action(VisualEffectTargetPayload {
            clip_id: selection.clip_id,
            effect_id,
        })
    })
}

pub(super) fn inspector_reorder_effect_action(
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
    placement: EffectRelativePlacement,
) -> Option<Action> {
    selection.map(|selection| {
        visual_effect_reorder_action(VisualEffectReorderPayload {
            clip_id: selection.clip_id,
            effect_id,
            placement,
        })
    })
}

pub(super) fn inspector_add_mask_action(selection: Option<SelectedClipRef>) -> Option<Action> {
    selection.filter(|selection| selection.is_video_track).map(|selection| {
        visual_mask_add_to_clip_action(VisualMaskAddToClipPayload {
            clip_id: selection.clip_id,
            shape: MaskShape::default(),
        })
    })
}

pub(super) fn inspector_mask_select_action(
    selection: Option<SelectedClipRef>,
    mask_id: MaskId,
) -> Option<Action> {
    selection.map(|selection| {
        visual_mask_select_action(VisualMaskTargetPayload { clip_id: selection.clip_id, mask_id })
    })
}

pub(super) fn inspector_mask_enabled_action(
    selection: Option<SelectedClipRef>,
    mask_id: MaskId,
    enabled: bool,
) -> Option<Action> {
    selection.map(|selection| {
        visual_mask_set_enabled_action(VisualMaskSetEnabledPayload {
            clip_id: selection.clip_id,
            mask_id,
            enabled,
        })
    })
}

pub(super) fn inspector_mask_locked_action(
    selection: Option<SelectedClipRef>,
    mask_id: MaskId,
    locked: bool,
) -> Option<Action> {
    selection.map(|selection| {
        visual_mask_set_locked_action(VisualMaskSetLockedPayload {
            clip_id: selection.clip_id,
            mask_id,
            locked,
        })
    })
}

pub(super) fn inspector_mask_shape_animation_action(
    selection: Option<SelectedClipRef>,
    mask_id: MaskId,
    enabled: bool,
) -> Option<Action> {
    selection.map(|selection| {
        visual_mask_set_shape_animation_enabled_action(VisualMaskSetShapeAnimationEnabledPayload {
            clip_id: selection.clip_id,
            mask_id,
            enabled,
        })
    })
}

pub(super) fn inspector_mask_shape_action(
    selection: Option<SelectedClipRef>,
    mask_id: MaskId,
    shape: MaskShape,
) -> Option<Action> {
    selection.map(|selection| {
        visual_mask_write_shape_action(VisualMaskWriteShapePayload {
            clip_id: selection.clip_id,
            mask_id,
            shape,
            interpolation: MaskShapeInterpolation::Hold,
        })
    })
}

pub(super) fn inspector_remove_mask_action(
    selection: Option<SelectedClipRef>,
    mask_id: MaskId,
) -> Option<Action> {
    selection.map(|selection| {
        visual_mask_remove_action(VisualMaskTargetPayload { clip_id: selection.clip_id, mask_id })
    })
}

pub(super) fn inspector_reorder_mask_action(
    selection: Option<SelectedClipRef>,
    mask_id: MaskId,
    placement: MaskRelativePlacement,
) -> Option<Action> {
    selection.map(|selection| {
        visual_mask_reorder_action(VisualMaskReorderPayload {
            clip_id: selection.clip_id,
            mask_id,
            placement,
        })
    })
}

pub(super) fn inspector_curve_edit_action(
    selection: Option<SelectedClipRef>,
    model: &InspectorCurveModel,
    edit: CurveEdit,
) -> Option<Action> {
    let selection = selection?;
    let edit = match edit {
        CurveEdit::Insert { point, .. } => ClipCurveEditPayload::Upsert {
            keyframe_id: None,
            point: inspector_curve_point_payload(point),
        },
        CurveEdit::Move { index, point } => {
            let key = model.keys.get(index)?;
            ClipCurveEditPayload::Upsert {
                keyframe_id: key.keyframe_id,
                point: inspector_curve_point_payload(point),
            }
        }
        CurveEdit::Delete { index } => {
            let keyframe_id = model.keys.get(index).and_then(|key| key.keyframe_id)?;
            ClipCurveEditPayload::Remove { keyframe_id }
        }
    };
    Some(clip_edit_numeric_curve_action(
        ClipEditNumericCurvePayload {
            clip_id: selection.clip_id,
            parameter: model.property.clone(),
            edit,
        },
    ))
}

pub(super) fn inspector_curve_point_payload(point: CurvePoint) -> ClipNormalizedCurvePointPayload {
    ClipNormalizedCurvePointPayload {
        time_ratio: f64::from(point.x.clamp(0.0, 1.0)),
        value_ratio: f64::from(point.y.clamp(0.0, 1.0)),
    }
}

pub(super) fn node_graph_node_action(
    selection: Option<SelectedClipRef>,
    targets: &[NodeGraphNodeTarget],
    node_id: &str,
) -> Option<Action> {
    let selection = selection?;
    match targets
        .iter()
        .find_map(|entry| (entry.node_id == node_id).then_some(entry.target))
    {
        Some(NodeGraphTarget::Effect(effect_id)) => {
            Some(visual_effect_select_action(VisualEffectTargetPayload {
                clip_id: selection.clip_id,
                effect_id,
            }))
        }
        Some(NodeGraphTarget::Clip | NodeGraphTarget::Output) => {
            node_graph_clip_action(Some(selection))
        }
        None => None,
    }
}

pub(super) fn node_graph_clip_action(selection: Option<SelectedClipRef>) -> Option<Action> {
    selection.map(|selection| {
        timeline_select_clip_action(TimelineSelectClipPayload {
            clip_id: selection.clip_id,
            mode: TimelineClipSelectionModePayload::Replace,
        })
    })
}

pub(super) fn effect_property_row(
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
) -> PropertyRow {
    inspector_property_row(
        property,
        can_edit,
        selection,
        InspectorPropertyTarget::Effect { effect_id, parameter: property.address.clone() },
    )
}

pub(super) fn mask_property_row(
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    mask_id: MaskId,
) -> PropertyRow {
    inspector_property_row(
        property,
        can_edit,
        selection,
        InspectorPropertyTarget::Mask { mask_id, parameter: property.address.clone() },
    )
}

pub(super) fn clip_property_row(
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
) -> PropertyRow {
    inspector_property_row(
        property,
        can_edit,
        selection,
        InspectorPropertyTarget::Clip { parameter: property.address.clone() },
    )
}

pub(super) fn inspector_property_row(
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    target: InspectorPropertyTarget,
) -> PropertyRow {
    let row = PropertyRow::new(
        property.label.clone(),
        inspector_property_value_widget(
            property,
            can_edit,
            selection,
            target.clone(),
            property.path.clone(),
        ),
    );
    let height = if matches!(&target, InspectorPropertyTarget::Clip { .. })
        && property.path == mondrian_core::BasicTitle::TEXT_PATH
    {
        Some(92.0)
    } else {
        effect_property_row_height(&property.value)
    };
    if let Some(height) = height {
        row.with_height(height)
    } else {
        row
    }
}

pub(super) fn effect_property_row_height(value: &PropertyValue) -> Option<f32> {
    let components: usize = match value {
        PropertyValue::Vec2(_) => 2,
        PropertyValue::Vec3(_) => 3,
        PropertyValue::Vec4(_) => 4,
        _ => return None,
    };
    Some(components as f32 * 30.0 + components.saturating_sub(1) as f32 * 4.0)
}

/// Build a typed value widget for one effect property row.
///
/// Widget construction depends on the `PropertyValue` variant present in the
/// snapshot. The returned widget dispatches a stable-address visual Effect
/// Product Action through the unique external codec.
#[cfg(test)]
pub(super) fn effect_property_value_widget(
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
    path: String,
) -> Box<dyn Widget> {
    inspector_property_value_widget(
        property,
        can_edit,
        selection,
        InspectorPropertyTarget::Effect { effect_id, parameter: property.address.clone() },
        path,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum InspectorPropertyTarget {
    Clip {
        parameter: AnimationParameterAddress,
    },
    Effect {
        effect_id: EffectId,
        parameter: AnimationParameterAddress,
    },
    Mask {
        mask_id: MaskId,
        parameter: AnimationParameterAddress,
    },
}

pub(super) fn inspector_property_value_widget(
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    target: InspectorPropertyTarget,
    path: String,
) -> Box<dyn Widget> {
    match &property.value {
        PropertyValue::Bool(value) => {
            let selected_clip = selection;
            Box::new(
                Checkbox::new(&property.label, *value).enabled(can_edit).on_change(move |v| {
                    inspector_property_action(
                        selected_clip,
                        target.clone(),
                        &path,
                        PropertyValue::Bool(v),
                    )
                }),
            )
        }
        PropertyValue::Float(value) => {
            let ((min, max), (hard_min, hard_max)) = parameter_numeric_ranges(property, 0.0, 1.0);
            let selected_clip = selection;
            let path = path.clone();
            numeric_slider_input_control_with_hard_range(
                *value,
                min,
                max,
                hard_min,
                hard_max,
                property_step(property.step, None),
                numeric_decimals(property.step, *value),
                can_edit,
                move |v| {
                    inspector_property_action(
                        selected_clip,
                        target.clone(),
                        &path,
                        PropertyValue::Float(v.clamp(hard_min, hard_max)),
                    )
                },
            )
        }
        PropertyValue::Double(value) => {
            let ((min, max), (hard_min, hard_max)) = parameter_numeric_ranges(property, 0.0, 1.0);
            let selected_clip = selection;
            let path = path.clone();
            numeric_slider_input_control_with_hard_range(
                *value as f32,
                min,
                max,
                hard_min,
                hard_max,
                property_step(property.step, None),
                numeric_decimals(property.step, *value as f32),
                can_edit,
                move |v: f32| {
                    inspector_property_action(
                        selected_clip,
                        target.clone(),
                        &path,
                        PropertyValue::Double((v as f64).clamp(hard_min as f64, hard_max as f64)),
                    )
                },
            )
        }
        PropertyValue::Int(value) => {
            let ((min, max), (hard_min, hard_max)) = parameter_numeric_ranges(property, 0.0, 100.0);
            let selected_clip = selection;
            let path = path.clone();
            numeric_slider_input_control_with_hard_range(
                *value as f32,
                min,
                max,
                hard_min,
                hard_max,
                property_step(property.step, Some(1.0)),
                0,
                can_edit,
                move |v: f32| {
                    inspector_property_action(
                        selected_clip,
                        target.clone(),
                        &path,
                        PropertyValue::Int(
                            (v.round() as i64).clamp(hard_min as i64, hard_max as i64),
                        ),
                    )
                },
            )
        }
        PropertyValue::Color(value) => {
            let selected_clip = selection;
            let path = path.clone();
            let trigger = color_picker_trigger(*value).enabled(can_edit);
            Box::new(trigger.on_change(move |color| {
                inspector_property_action(
                    selected_clip,
                    target.clone(),
                    &path,
                    PropertyValue::Color(color),
                )
            }))
        }
        PropertyValue::Text(value) => {
            let text = value.clone();
            if matches!(&target, InspectorPropertyTarget::Clip { .. })
                && path == mondrian_core::BasicTitle::TEXT_PATH
            {
                let selected_clip = selection;
                return Box::new(
                    MultilineTextInput::new("标题文本")
                        .with_text(text)
                        .min_lines(3)
                        .enabled(can_edit)
                        .on_change(move |text| {
                            inspector_property_action(
                                selected_clip,
                                target.clone(),
                                &path,
                                PropertyValue::Text(text.to_owned()),
                            )
                        }),
                );
            }
            let max_width = 180.0;
            if !matches!(&target, InspectorPropertyTarget::Clip { .. }) && text.len() > 60 {
                Box::new(Label::new(text).with_max_width(max_width))
            } else {
                let selected_clip = selection;
                let path = path.clone();
                Box::new(
                    TextInput::new(text).enabled(can_edit).on_change(move |text| {
                        inspector_property_action(
                            selected_clip,
                            target.clone(),
                            &path,
                            PropertyValue::Text(text.to_string()),
                        )
                    }),
                )
            }
        }
        PropertyValue::Enum(value) => {
            let items = property
                .schema
                .enum_options
                .iter()
                .map(|option| {
                    MenuItem::new(
                        option.key.clone(),
                        inspector_property_action(
                            selection,
                            target.clone(),
                            &path,
                            PropertyValue::Enum(option.key.clone()),
                        ),
                    )
                })
                .collect();
            Box::new(Dropdown::new(value.clone(), items).enabled(can_edit))
        }
        PropertyValue::Resource(reference) => {
            let text = match reference {
                ParameterResourceReference::Unbound => String::new(),
                ParameterResourceReference::ExternalFile { path } => path.display().to_string(),
                ParameterResourceReference::ProjectAsset { asset_id } => asset_id.to_string(),
                ParameterResourceReference::Uri { uri } => uri.clone(),
            };
            let selected_clip = selection;
            let path = path.clone();
            Box::new(
                TextInput::new(text).enabled(can_edit).on_change(move |text| {
                    let value = if text.trim().is_empty() {
                        ParameterResourceReference::Unbound
                    } else {
                        ParameterResourceReference::ExternalFile { path: PathBuf::from(text) }
                    };
                    inspector_property_action(
                        selected_clip,
                        target.clone(),
                        &path,
                        PropertyValue::Resource(value),
                    )
                }),
            )
        }
        PropertyValue::Vec2(value) => vector_property_widget(
            &["X", "Y"],
            &[value.x, value.y],
            property,
            can_edit,
            selection,
            target,
            path,
            |values| PropertyValue::Vec2(glam::Vec2::new(values[0], values[1])),
        ),
        PropertyValue::Vec3(value) => vector_property_widget(
            &["X", "Y", "Z"],
            &[value.x, value.y, value.z],
            property,
            can_edit,
            selection,
            target,
            path,
            |values| PropertyValue::Vec3(glam::Vec3::new(values[0], values[1], values[2])),
        ),
        PropertyValue::Vec4(value) => vector_property_widget(
            &["X", "Y", "Z", "W"],
            value,
            property,
            can_edit,
            selection,
            target,
            path,
            |values| PropertyValue::Vec4([values[0], values[1], values[2], values[3]]),
        ),
    }
}

pub(super) fn vector_property_widget(
    labels: &[&'static str],
    values: &[f32],
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    target: InspectorPropertyTarget,
    path: String,
    build_value: fn(&[f32]) -> PropertyValue,
) -> Box<dyn Widget> {
    let ((min, max), (hard_min, hard_max)) = parameter_numeric_ranges(property, 0.0, 1.0);
    let values = values
        .iter()
        .map(|value| finite_f32_from_f32(*value).unwrap_or(hard_min).clamp(hard_min, hard_max))
        .collect::<Vec<_>>();
    let rows = labels
        .iter()
        .zip(values.iter())
        .enumerate()
        .map(|(component_index, (label, value))| {
            let base_values = values.to_vec();
            let selected_clip = selection;
            let target = target.clone();
            let path = path.clone();
            let control = numeric_slider_input_control_with_hard_range(
                *value,
                min,
                max,
                hard_min,
                hard_max,
                property_step(property.step, None),
                numeric_decimals(property.step, *value),
                can_edit,
                move |v| {
                    let mut next_values = base_values.clone();
                    next_values[component_index] = v.clamp(hard_min, hard_max);
                    inspector_property_action(
                        selected_clip,
                        target.clone(),
                        &path,
                        build_value(&next_values),
                    )
                },
            );
            FlexChild::fixed(Box::new(
                FlexContainer::row(vec![
                    FlexChild::fixed(Box::new(
                        Label::new(*label).muted().with_font_size(11.0).with_padding(0.0, 0.0),
                    )),
                    FlexChild::flex(control, 1.0),
                ])
                .with_gap(8.0),
            ))
        })
        .collect();
    Box::new(FlexContainer::column(rows).with_gap(4.0))
}

pub(super) fn numeric_property_range(
    descriptor_min: Option<f64>,
    descriptor_max: Option<f64>,
    default_min: f32,
    default_max: f32,
) -> (f32, f32) {
    let (default_min, default_max) = ordered_numeric_range(default_min, default_max);
    let min = descriptor_min.and_then(finite_f32);
    let max = descriptor_max.and_then(finite_f32);
    match (min, max) {
        (Some(min), Some(max)) => ordered_numeric_range(min, max),
        (Some(min), None) => (min, default_max.max(min)),
        (None, Some(max)) => (default_min.min(max), max),
        (None, None) => (default_min, default_max),
    }
}

pub(super) fn parameter_numeric_ranges(
    property: &InspectorEffectPropertyModel,
    default_min: f32,
    default_max: f32,
) -> ((f32, f32), (f32, f32)) {
    let soft = numeric_property_range(property.min, property.max, default_min, default_max);
    let hard = numeric_property_range(property.hard_min, property.hard_max, soft.0, soft.1);
    (soft, hard)
}

pub(super) fn ordered_numeric_range(min: f32, max: f32) -> (f32, f32) {
    let min = finite_f32_from_f32(min).unwrap_or(0.0);
    let max = finite_f32_from_f32(max).unwrap_or(1.0);
    if min <= max {
        (min, max)
    } else {
        (max, min)
    }
}

pub(super) fn finite_f32(value: f64) -> Option<f32> {
    finite_f32_from_f32(value as f32)
}

pub(super) fn finite_f32_from_f32(value: f32) -> Option<f32> {
    value.is_finite().then_some(value)
}

pub(super) fn property_step(
    descriptor_step: Option<f64>,
    fallback_step: Option<f32>,
) -> Option<f32> {
    descriptor_step
        .filter(|step| step.is_finite() && *step > 0.0)
        .map(|step| step as f32)
        .or(fallback_step)
        .filter(|step| step.is_finite() && *step > 0.0)
}

pub(super) fn numeric_decimals(descriptor_step: Option<f64>, value: f32) -> usize {
    if let Some(step) = descriptor_step.filter(|step| step.is_finite() && *step > 0.0) {
        return decimal_places_for_step(step);
    }
    if value.fract().abs() > f32::EPSILON {
        2
    } else {
        0
    }
}

pub(super) fn decimal_places_for_step(step: f64) -> usize {
    let mut scaled = step.abs();
    for decimals in 0..=4 {
        if (scaled.round() - scaled).abs() < 1.0e-6 {
            return decimals;
        }
        scaled *= 10.0;
    }
    4
}

#[cfg(test)]
pub(super) fn inspector_effect_property_action(
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
    parameter: AnimationParameterAddress,
    value: PropertyValue,
) -> Option<Action> {
    inspector_property_action(
        selection,
        InspectorPropertyTarget::Effect { effect_id, parameter },
        "",
        value,
    )
}

pub(super) fn inspector_property_action(
    selection: Option<SelectedClipRef>,
    target: InspectorPropertyTarget,
    _path: &str,
    value: PropertyValue,
) -> Option<Action> {
    let selection = selection?;
    Some(match target {
        InspectorPropertyTarget::Clip { parameter } => {
            clip_write_parameter_values_action(ClipWriteParameterValuesPayload {
                clip_id: selection.clip_id,
                writes: vec![ClipParameterValueWrite { parameter, value }],
            })
        }
        InspectorPropertyTarget::Effect { effect_id, parameter } => {
            visual_effect_set_parameter_value_action(VisualEffectSetParameterValuePayload {
                clip_id: selection.clip_id,
                effect_id,
                parameter,
                value,
            })
        }
        InspectorPropertyTarget::Mask { mask_id, parameter } => {
            visual_mask_set_parameter_value_action(VisualMaskSetParameterValuePayload {
                clip_id: selection.clip_id,
                mask_id,
                parameter,
                value,
            })
        }
    })
}
