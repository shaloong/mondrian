//! Property-oriented Mixer and Inspector widget adapters.
//!
//! This deep Module owns widget construction and action lowering while the
//! parent panel Module retains aggregate projection and dock composition.

use super::*;
use crate::app::parameter_gesture::{
    ParameterCurveGestureTarget, ParameterGestureAction, ParameterGestureEdit,
};
use crate::app_ui::localization::keyframe_interpolation_message_id;
use mondrian_ui_widgets::CurveGestureEvent;
use mondrian_ui_widgets::{CurvePreview, CurveTangent};

pub(super) fn audio_mixer_panel(
    model: &AudioMixerPanelModel,
    localizer: &Localizer,
) -> PropertyPanel {
    if let Some(message) = model.empty_message.as_deref().filter(|message| !message.is_empty()) {
        let (title, description) = message
            .split_once('\n')
            .map_or((message, ""), |(title, description)| (title, description));
        let mut panel = PropertyPanel::with_options(
            localizer.text("mixer-title"),
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
        localizer.text("mixer-title"),
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
    let create_bus_label = model.next_bus_name.as_deref().map_or_else(
        || localizer.text("mixer-create-bus"),
        |name| localizer.format_text("mixer-create-named-bus", "name", name),
    );
    panel = panel.with_section(
        PropertySection::new(localizer.text("mixer-routing-graph")).with_row(PropertyRow::new(
            "Bus",
            Box::new(
                Button::new(create_bus_label).enabled(create_bus.is_some()).on_click(create_bus),
            ),
        )),
    );

    for channel in &model.channels {
        let kind = match channel.kind {
            AudioMixerChannelKind::Track => localizer.text("mixer-track"),
            AudioMixerChannelKind::Bus => localizer.text("mixer-bus"),
            AudioMixerChannelKind::ProgramOutput => localizer.text("mixer-program-output"),
        };
        let mut section = PropertySection::new(format!("{kind} · {}", channel.name));
        if let (Some(muted), mondrian_timeline::AudioChannelStripOwner::Track { track_id }) =
            (channel.track_muted, channel.owner)
        {
            section = section.with_row(PropertyRow::new(
                localizer.text("mixer-mute"),
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
                localizer.text("mixer-solo"),
                Box::new(
                    Checkbox::new("S", soloed)
                        .on_change(move |value| audio_mixer_set_track_solo_action(track_id, value)),
                ),
            ));
        }
        section = section.with_row(PropertyRow::new(
            localizer.text("mixer-level"),
            Box::new(Label::new(audio_mixer_meter_label(channel, localizer)).muted()),
        ));
        let trim_channel = channel.clone();
        section = section.with_row(PropertyRow::new(
            localizer.text("mixer-input-trim"),
            numeric_slider_input_control_with_hard_range(
                channel.input_trim_db,
                -60.0,
                12.0,
                AUDIO_GAIN_DB_MIN,
                AUDIO_GAIN_DB_MAX,
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
                    localizer.text("mixer-fader"),
                    numeric_slider_input_control_with_hard_range(
                        value_db,
                        -60.0,
                        12.0,
                        AUDIO_GAIN_DB_MIN,
                        AUDIO_GAIN_DB_MAX,
                        Some(0.1),
                        1,
                        channel.is_editable,
                        move |value| audio_mixer_set_fader_action(&fader_channel, value),
                    ),
                ));
            }
            AudioMixerGainModel::Automated { keyframe_count } => {
                section = section.with_row(PropertyRow::new(
                    localizer.text("mixer-fader"),
                    Box::new(Label::new(mixer_automation_label(localizer, keyframe_count)).muted()),
                ));
            }
        }
        if let Some(automation) = &channel.fader_automation {
            section = section.with_row(
                PropertyRow::new(
                    localizer.text("mixer-fader-curve"),
                    audio_automation_curve_control(automation, localizer),
                )
                .with_height(118.0),
            );
        }
        if let Some(reason) = &channel.edit_disabled_reason {
            section = section.with_row(PropertyRow::new(
                localizer.text("mixer-read-only"),
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
                localizer.text("mixer-incoming-routes"),
                Box::new(
                    Label::new(localizer.format_text(
                        "mixer-route-count",
                        "count",
                        &channel.incoming_route_count.to_string(),
                    ))
                    .muted(),
                ),
            ));
        }
        if matches!(channel.kind, AudioMixerChannelKind::Bus) {
            let rename_channel = channel.clone();
            section = section.with_row(PropertyRow::new(
                localizer.text("mixer-name"),
                Box::new(
                    TextInput::new(localizer.text("mixer-bus-name"))
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
                localizer.text("mixer-add-route"),
                Box::new(
                    Dropdown::new(localizer.text("mixer-choose-route"), items)
                        .with_max_visible_items(12),
                ),
            ));
        }
        if let Some(removal) = &channel.bus_removal {
            let action = audio_mixer_remove_bus_action(removal);
            let label = if removal.connected_route_count == 0 {
                localizer.text("mixer-remove-bus")
            } else {
                localizer.format_text(
                    "mixer-remove-bus-routes",
                    "count",
                    &removal.connected_route_count.to_string(),
                )
            };
            section = section.with_row(PropertyRow::new(
                "Bus",
                Box::new(Button::new(label).enabled(action.is_some()).on_click(action)),
            ));
            if let Some(reason) = &removal.edit_disabled_reason {
                section = section.with_row(PropertyRow::new(
                    localizer.text("mixer-remove-blocked"),
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
                        Box::new(Label::new(route.source_port_label.clone()).muted()),
                    ))
                    .with_row(PropertyRow::new(
                        localizer.text("mixer-enabled"),
                        Box::new(
                            Checkbox::new(localizer.text("mixer-pass-signal"), route.enabled)
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
                        localizer.text("mixer-level"),
                        numeric_slider_input_control_with_hard_range(
                            value_db,
                            -60.0,
                            12.0,
                            AUDIO_GAIN_DB_MIN,
                            AUDIO_GAIN_DB_MAX,
                            Some(0.1),
                            1,
                            route.is_editable,
                            move |value| audio_mixer_set_route_gain_action(&gain_route, value),
                        ),
                    ));
                }
                AudioMixerGainModel::Automated { keyframe_count } => {
                    route_section = route_section.with_row(PropertyRow::new(
                        localizer.text("mixer-level"),
                        Box::new(
                            Label::new(mixer_automation_label(localizer, keyframe_count)).muted(),
                        ),
                    ));
                }
            }
            if let Some(automation) = &route.gain_automation {
                route_section = route_section.with_row(
                    PropertyRow::new(
                        localizer.text("mixer-level-curve"),
                        audio_automation_curve_control(automation, localizer),
                    )
                    .with_height(118.0),
                );
            }
            route_section = route_section.with_row(PropertyRow::new(
                localizer.text("mixer-controls"),
                effect_icon_button(
                    AppIcon::Trash,
                    localizer.text("mixer-remove-route"),
                    localizer.text("mixer-remove-route-tooltip"),
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
                    localizer.text("mixer-rewire"),
                    Box::new(
                        Dropdown::new(localizer.text("mixer-choose-new-route"), rewire_items)
                            .with_max_visible_items(12),
                    ),
                ));
            }
            if let Some(reason) = &route.edit_disabled_reason {
                route_section = route_section.with_row(PropertyRow::new(
                    localizer.text("mixer-read-only"),
                    Box::new(Label::new(reason.clone()).muted()),
                ));
            }
            panel = panel.with_section(route_section);
        }
        panel = with_audio_processor_rack_sections(
            panel,
            &channel.processor_racks,
            channel.is_editable,
            localizer,
        );
    }
    panel
}

pub(super) fn audio_mixer_meter_label(
    channel: &crate::app_ui::audio_mixer::AudioMixerChannelModel,
    localizer: &Localizer,
) -> String {
    let Some(meter) = &channel.meter else {
        return localizer.text("mixer-meter-not-running");
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
        (0, invalid) => localizer.format_text("mixer-meter-invalid", "count", &invalid.to_string()),
        (clipped, invalid) => format!(
            " · CLIP {clipped}{}",
            localizer.format_text("mixer-meter-invalid", "count", &invalid.to_string())
        ),
    };
    format!(
        "P {} · RMS {}{warning}",
        audio_meter_dbfs_label(f64::from(peak)),
        audio_meter_dbfs_label(rms),
    )
}

pub(super) fn mixer_automation_label(localizer: &Localizer, keyframe_count: usize) -> String {
    localizer.format_text(
        "mixer-automation-keyframes",
        "count",
        &keyframe_count.to_string(),
    )
}

pub(super) fn audio_meter_dbfs_label(linear: f64) -> String {
    if linear <= 0.0 {
        "−∞ dBFS".to_owned()
    } else {
        format!("{:.1} dBFS", 20.0 * linear.log10())
    }
}

pub(super) fn inspector_panel(model: &InspectorPanelModel, localizer: &Localizer) -> PropertyPanel {
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
    let has_target = selected_clip.is_some();
    let can_edit = has_target && model.is_editable;
    let subtitle = model.edit_disabled_reason.clone().unwrap_or_else(|| {
        localizer.text(if has_target {
            "inspector-selected-clip"
        } else {
            "inspector-no-clip-selected"
        })
    });
    if let Some(message) = model.empty_message.as_deref().filter(|message| !message.is_empty()) {
        let (title, description) = message
            .split_once('\n')
            .map_or((message, ""), |(title, description)| (title, description));
        let mut panel = PropertyPanel::with_options(
            localizer.text("inspector-title"),
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
        numeric_curve_editor(
            localizer,
            selected_clip.map(|selection| ParameterCurveGestureTarget::Clip {
                clip_id: selection.clip_id,
                parameter: curve_model.property.clone(),
            }),
            curve_model,
            can_edit,
            None,
            move |curve, edit| inspector_curve_edit_action(selected_clip, curve, edit),
            move |curve, keyframe_id, interpolation| {
                selected_clip.map(|selection| {
                    clip_edit_numeric_curve_action(ClipEditNumericCurvePayload {
                        clip_id: selection.clip_id,
                        parameter: curve.property.clone(),
                        edit: ClipCurveEditPayload::SetInterpolation { keyframe_id, interpolation },
                    })
                })
            },
        )
    } else {
        CurveEditor::with_points(vec![
            CurvePoint::new(0.0, (model.opacity / 100.0) as f32),
            CurvePoint::new(1.0, (model.opacity / 100.0) as f32),
        ])
        .disabled()
    };
    let mut style_section = PropertySection::new(localizer.text("inspector-clip-style"))
        .with_row(PropertyRow::new(
            localizer.text("inspector-enabled"),
            Box::new(
                Checkbox::new(localizer.text("inspector-enable-effect"), model.enabled)
                    .enabled(can_edit)
                    .on_change(move |value| inspector_bool_action(selected_clip, value)),
            ),
        ))
        .with_row(PropertyRow::new(
            localizer.text("inspector-opacity"),
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
                        PropertyValue::Float((value / 100.0) as f32),
                    )
                },
            ),
        ));
    if let (Some(selection), Some(blend_mode)) = (selected_clip, model.blend_mode.as_ref()) {
        style_section = style_section.with_row(PropertyRow::new(
            &blend_mode.row_label,
            Box::new(inspector_blend_mode_dropdown(
                blend_mode,
                selection.clip_id,
                can_edit,
            )),
        ));
    }
    if model.shows_tint {
        let mut tint = color_picker_trigger(model.tint).enabled(can_edit);
        tint.picker_mut().set_area_mode(model.tint_area_mode);
        style_section = style_section.with_row(PropertyRow::new(
            localizer.text("inspector-color"),
            Box::new(tint.on_change(move |color| inspector_color_action(selected_clip, color))),
        ));
    }
    let mut panel = PropertyPanel::new(localizer.text("inspector-title"))
        .with_subtitle(subtitle)
        .with_embedded_panel_chrome()
        .with_section(style_section);

    if !model.clip_properties.is_empty() {
        let mut section = PropertySection::new(localizer.text("inspector-basic-title"));
        for property in &model.clip_properties {
            section = section.with_row(clip_property_row(
                localizer,
                property,
                can_edit,
                selected_clip,
            ));
        }
        panel = panel.with_section(section);
    }

    if !model.audio_components.is_empty() {
        for (index, component) in model.audio_components.iter().enumerate() {
            let edit_id = component.edit_id;
            let original_pan = component.pan;
            let volume_static_editable = can_edit
                && component.volume_automation.as_ref().is_none_or(|curve| !curve.is_automated());
            let pan_static_editable = can_edit
                && component.pan_automation.as_ref().is_none_or(|curve| !curve.is_automated());
            let section_title = if model.audio_components.len() == 1 {
                localizer.text("inspector-audio-component")
            } else {
                localizer.format_text(
                    "inspector-audio-component-numbered",
                    "number",
                    &(index + 1).to_string(),
                )
            };
            let mut section = PropertySection::new(section_title)
                .with_row(PropertyRow::new(
                    localizer.text("inspector-enabled"),
                    Box::new(
                        Checkbox::new(localizer.text("inspector-audio-in-mix"), component.enabled)
                            .enabled(can_edit)
                            .on_change(move |value| {
                                audio_component_mutation_action(
                                    selected_clip,
                                    edit_id,
                                    AudioComponentMutation::SetEnabled { value },
                                )
                            }),
                    ),
                ))
                .with_row(PropertyRow::new(
                    localizer.text("inspector-audio-volume"),
                    numeric_slider_input_control_with_hard_range(
                        component.volume_db,
                        -60.0,
                        12.0,
                        AUDIO_GAIN_DB_MIN,
                        AUDIO_GAIN_DB_MAX,
                        Some(0.1),
                        1,
                        volume_static_editable,
                        move |value| {
                            audio_component_mutation_action(
                                selected_clip,
                                edit_id,
                                AudioComponentMutation::SetVolumeDb { value },
                            )
                        },
                    ),
                ))
                .with_row(PropertyRow::new(
                    localizer.text("inspector-audio-pan"),
                    numeric_slider_input_control(
                        original_pan * 100.0,
                        -100.0,
                        100.0,
                        Some(1.0),
                        0,
                        pan_static_editable,
                        move |value| {
                            inspector_audio_pan_action(selected_clip, edit_id, original_pan, value)
                        },
                    ),
                ));
            if let Some(automation) = &component.volume_automation {
                section = section.with_row(
                    PropertyRow::new(
                        localizer.text("inspector-audio-volume-curve"),
                        audio_automation_curve_control(automation, localizer),
                    )
                    .with_height(118.0),
                );
            }
            if let Some(automation) = &component.pan_automation {
                section = section.with_row(
                    PropertyRow::new(
                        localizer.text("inspector-audio-pan-curve"),
                        audio_automation_curve_control(automation, localizer),
                    )
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
                localizer.text("inspector-audio-logical-source"),
                Box::new(
                    Dropdown::new(component.source_label.clone(), source_items)
                        .with_max_visible_items(8)
                        .enabled(source_enabled),
                ),
            ));

            if let Some(binding) = &component.binding {
                let mut binding_items = vec![
                    MenuItem::new(
                        localizer.text("inspector-audio-reprobe-file"),
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
                    localizer.text("inspector-audio-stream-binding"),
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
                localizer,
            );
            let original_fade_in = component.fade_in;
            let original_fade_out = component.fade_out;
            let max_fade_seconds = component.clip_duration.to_f64().max(0.0);
            let fade_in_curve =
                component.fade_in.map(|fade| fade.curve).unwrap_or(AudioFadeCurve::EqualPower);
            let fade_out_curve =
                component.fade_out.map(|fade| fade.curve).unwrap_or(AudioFadeCurve::EqualPower);
            let fade_in_seconds =
                component.fade_in.map(|fade| fade.duration.to_f64()).unwrap_or(0.0);
            let fade_out_seconds =
                component.fade_out.map(|fade| fade.duration.to_f64()).unwrap_or(0.0);
            section = section
                .with_row(PropertyRow::new(
                    localizer.text("inspector-audio-fade-in"),
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
                                original_fade_in,
                            )
                        },
                    ),
                ))
                .with_row(PropertyRow::new(
                    localizer.text("inspector-audio-fade-in-curve"),
                    Box::new(
                        Dropdown::new(
                            audio_fade_curve_label(fade_in_curve, localizer),
                            audio_fade_curve_items(
                                selected_clip,
                                edit_id,
                                true,
                                component.fade_in,
                                localizer,
                            ),
                        )
                        .enabled(can_edit && component.fade_in.is_some()),
                    ),
                ))
                .with_row(PropertyRow::new(
                    localizer.text("inspector-audio-fade-out"),
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
                                original_fade_out,
                            )
                        },
                    ),
                ))
                .with_row(PropertyRow::new(
                    localizer.text("inspector-audio-fade-out-curve"),
                    Box::new(
                        Dropdown::new(
                            audio_fade_curve_label(fade_out_curve, localizer),
                            audio_fade_curve_items(
                                selected_clip,
                                edit_id,
                                false,
                                component.fade_out,
                                localizer,
                            ),
                        )
                        .enabled(can_edit && component.fade_out.is_some()),
                    ),
                ));
            panel = panel.with_section(section);
        }
    }

    panel = with_audio_processor_rack_sections(
        panel,
        &model.audio_processor_racks,
        can_edit,
        localizer,
    );

    panel = panel.with_section(
        PropertySection::new(localizer.text("inspector-transform"))
            .with_row(PropertyRow::new(
                localizer.text("inspector-position-x"),
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
                        inspector_parameter_channel_action(
                            selected_clip,
                            position_x_parameter.clone(),
                            0,
                            f64::from(value),
                        )
                    },
                ),
            ))
            .with_row(PropertyRow::new(
                localizer.text("inspector-position-y"),
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
                        inspector_parameter_channel_action(
                            selected_clip,
                            position_y_parameter.clone(),
                            1,
                            f64::from(value),
                        )
                    },
                ),
            ))
            .with_row(PropertyRow::new(
                localizer.text("inspector-scale-x"),
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
                        inspector_parameter_channel_action(
                            selected_clip,
                            scale_x_parameter.clone(),
                            0,
                            value / 100.0,
                        )
                    },
                ),
            ))
            .with_row(PropertyRow::new(
                localizer.text("inspector-scale-y"),
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
                        inspector_parameter_channel_action(
                            selected_clip,
                            scale_y_parameter.clone(),
                            1,
                            value / 100.0,
                        )
                    },
                ),
            ))
            .with_row(PropertyRow::new(
                localizer.text("inspector-anchor-x"),
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
                        inspector_parameter_channel_action(
                            selected_clip,
                            anchor_x_parameter.clone(),
                            0,
                            f64::from(value),
                        )
                    },
                ),
            ))
            .with_row(PropertyRow::new(
                localizer.text("inspector-anchor-y"),
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
                        inspector_parameter_channel_action(
                            selected_clip,
                            anchor_y_parameter.clone(),
                            1,
                            f64::from(value),
                        )
                    },
                ),
            ))
            .with_row(PropertyRow::new(
                localizer.text("inspector-rotation"),
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
    let mut timing_section = PropertySection::new(localizer.text("inspector-timing"))
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
                    localizer.text("inspector-speed"),
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
                    localizer.text("inspector-source-time"),
                    Box::new(Label::new(localizer.text("inspector-hold")).muted()),
                ));
                let rate_enabled = can_edit && source_timing.can_set_rate;
                timing_section = timing_section.with_row(PropertyRow::new(
                    localizer.text("inspector-resume-speed"),
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
                (_, None) => "inspector-place-playhead-in-clip",
                (InspectorSourceTimingMode::Hold, Some(_)) => "inspector-update-hold-frame",
                _ => "inspector-create-hold-frame",
            };
            timing_section = timing_section.with_row(PropertyRow::new(
                localizer.text("inspector-freeze-frame"),
                Box::new(
                    Button::new(localizer.text(label))
                        .enabled(can_edit && freeze_target.is_some())
                        .on_click(inspector_hold_action(selected_clip, freeze_target)),
                ),
            ));
        }
    }
    panel = panel.with_section(timing_section);

    if selected_clip.is_some_and(|selection| selection.is_video_track) {
        panel = panel.with_section(
            PropertySection::new(localizer.text("mask-section")).with_row(PropertyRow::new(
                localizer.text("mask-add"),
                Box::new(
                    FlexContainer::row(vec![
                        FlexChild::flex(
                            Box::new(
                                Button::new(localizer.text("mask-shape-rectangle"))
                                    .enabled(can_edit)
                                    .on_click(inspector_add_mask_action(
                                        selected_clip,
                                        MaskShape::default(),
                                    )),
                            ),
                            1.0,
                        ),
                        FlexChild::flex(
                            Box::new(
                                Button::new(localizer.text("mask-shape-ellipse"))
                                    .enabled(can_edit)
                                    .on_click(inspector_add_mask_action(
                                        selected_clip,
                                        MaskShape::Ellipse {
                                            center: glam::Vec2::splat(0.5),
                                            radii: glam::Vec2::splat(0.35),
                                        },
                                    )),
                            ),
                            1.0,
                        ),
                        FlexChild::flex(
                            Box::new(
                                Button::new(localizer.text("mask-shape-bezier"))
                                    .enabled(can_edit)
                                    .on_click(inspector_add_mask_action(
                                        selected_clip,
                                        default_bezier_power_window(),
                                    )),
                            ),
                            1.0,
                        ),
                    ])
                    .with_gap(6.0),
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
                localizer.text("mask-shape-rectangle"),
                inspector_mask_shape_action(selected_clip, mask_id, MaskShape::default()),
            ),
            MenuItem::new(
                localizer.text("mask-shape-ellipse"),
                inspector_mask_shape_action(
                    selected_clip,
                    mask_id,
                    MaskShape::Ellipse {
                        center: glam::Vec2::splat(0.5),
                        radii: glam::Vec2::splat(0.4),
                    },
                ),
            ),
            MenuItem::new(
                localizer.text("mask-shape-bezier"),
                inspector_mask_shape_action(selected_clip, mask_id, default_bezier_power_window()),
            ),
        ];
        let mut section = PropertySection::new(mask.label.clone())
            .selected(model.selected_mask_id == Some(mask_id))
            .on_select(inspector_mask_select_action(selected_clip, mask_id))
            .with_row(PropertyRow::new(
                localizer.text("mask-controls"),
                Box::new(
                    FlexContainer::row(vec![
                        FlexChild::flex(
                            Box::new(
                                Checkbox::new(localizer.text("mask-enabled"), mask.enabled)
                                    .enabled(can_edit)
                                    .on_change(move |enabled| {
                                        inspector_mask_enabled_action(
                                            selected_clip,
                                            mask_id,
                                            enabled,
                                        )
                                    }),
                            ),
                            1.0,
                        ),
                        FlexChild::fixed(effect_icon_button(
                            AppIcon::CaretUp,
                            localizer.text("mask-move-up"),
                            localizer.text("mask-move-up-tooltip"),
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
                            localizer.text("mask-move-down"),
                            localizer.text("mask-move-down-tooltip"),
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
                            localizer.text("mask-remove"),
                            localizer.text("mask-remove-tooltip"),
                            mask_can_edit,
                            inspector_remove_mask_action(selected_clip, mask_id),
                        )),
                    ])
                    .with_gap(8.0),
                ),
            ))
            .with_row(PropertyRow::new(
                localizer.text("mask-lock"),
                Box::new(
                    Checkbox::new(localizer.text("mask-lock-editing"), mask.locked)
                        .enabled(can_edit)
                        .on_change(move |locked| {
                            inspector_mask_locked_action(selected_clip, mask_id, locked)
                        }),
                ),
            ))
            .with_row(PropertyRow::new(
                localizer.text("mask-shape"),
                Box::new(
                    Dropdown::new(localizer.text(mask.shape_kind.message_id()), shape_items)
                        .enabled(mask_can_edit),
                ),
            ))
            .with_row(PropertyRow::new(
                localizer.text("mask-shape-animation"),
                Box::new(
                    Checkbox::new(
                        localizer.text("mask-keyframes"),
                        mask.shape_animation_enabled,
                    )
                    .enabled(mask_can_edit)
                    .on_change(move |enabled| {
                        inspector_mask_shape_animation_action(selected_clip, mask_id, enabled)
                    }),
                ),
            ));
        if mask.shape_kind == InspectorMaskShapeKind::Path
            && model.selected_mask_id == Some(mask_id)
        {
            section = section.with_row(PropertyRow::new(
                "Viewer",
                Box::new(Label::new(localizer.text("mask-insert-point-hint")).muted()),
            ));
        }
        let tracking_active = mask.tracking_status.as_ref().is_some_and(|status| {
            matches!(
                status.phase,
                crate::app::visual_tracking::VisualTrackingPhase::Queued
                    | crate::app::visual_tracking::VisualTrackingPhase::Analyzing
                    | crate::app::visual_tracking::VisualTrackingPhase::Canceling
            )
        });
        let tracking_items = [
            (
                "mask-track-object-forward",
                MaskTrackingModel::ObjectTranslation,
                MaskTrackingDirection::Forward,
            ),
            (
                "mask-track-object-backward",
                MaskTrackingModel::ObjectTranslation,
                MaskTrackingDirection::Backward,
            ),
            (
                "mask-track-object-both",
                MaskTrackingModel::ObjectTranslation,
                MaskTrackingDirection::Both,
            ),
            (
                "mask-track-planar-forward",
                MaskTrackingModel::PlanarHomography,
                MaskTrackingDirection::Forward,
            ),
            (
                "mask-track-planar-backward",
                MaskTrackingModel::PlanarHomography,
                MaskTrackingDirection::Backward,
            ),
            (
                "mask-track-planar-both",
                MaskTrackingModel::PlanarHomography,
                MaskTrackingDirection::Both,
            ),
        ]
        .into_iter()
        .map(|(message_id, tracking_model, direction)| {
            MenuItem::new(
                localizer.text(message_id),
                inspector_mask_start_tracking_action(
                    selected_clip,
                    mask_id,
                    tracking_model,
                    direction,
                ),
            )
        })
        .collect();
        section = section
            .with_row(PropertyRow::new(
                localizer.text("mask-tracking"),
                Box::new(
                    Dropdown::new(localizer.text("mask-select-tracking"), tracking_items)
                        .enabled(mask_can_edit && !tracking_active),
                ),
            ))
            .with_row(PropertyRow::new(
                localizer.text("mask-tracking-status"),
                Box::new(Label::new(inspector_tracking_status_label(mask, localizer)).muted()),
            ))
            .with_row(PropertyRow::new(
                localizer.text("mask-tracking-controls"),
                Box::new(
                    FlexContainer::row(vec![
                        FlexChild::flex(
                            Box::new(
                                Button::new(localizer.text("mask-cancel"))
                                    .enabled(tracking_active)
                                    .on_click(inspector_mask_cancel_tracking_action(
                                        selected_clip,
                                        mask_id,
                                    )),
                            ),
                            1.0,
                        ),
                        FlexChild::flex(
                            Box::new(
                                Button::new(localizer.text("mask-recompute"))
                                    .enabled(
                                        mask_can_edit
                                            && mask.has_tracking_recipe
                                            && !tracking_active,
                                    )
                                    .on_click(inspector_mask_recompute_tracking_action(
                                        selected_clip,
                                        mask_id,
                                    )),
                            ),
                            1.0,
                        ),
                    ])
                    .with_gap(6.0),
                ),
            ));
        for property in &mask.properties {
            section = section.with_row(mask_property_row(
                localizer,
                property,
                mask_can_edit,
                selected_clip,
                mask_id,
            ));
        }
        panel = panel.with_section(section);
    }

    let grade = &model.grade;
    let mut grade_section =
        PropertySection::new(localizer.text("inspector-grade-graph")).with_row(PropertyRow::new(
            localizer.text("inspector-grade-clip"),
            Box::new(
                Button::new(
                    grade
                        .clip_grade
                        .clone()
                        .unwrap_or_else(|| localizer.text("inspector-grade-create")),
                )
                .enabled(grade.create_clip_grade_action.is_some())
                .on_click(grade.create_clip_grade_action.clone()),
            ),
        ));
    if let Some(group) = &grade.group {
        grade_section = grade_section.with_row(PropertyRow::new(
            localizer.text("inspector-grade-group"),
            Box::new(Label::new(group.clone()).muted()),
        ));
    }
    if grade.group_pre_grade.is_some() || grade.group_post_grade.is_some() {
        let identity = localizer.text("inspector-grade-identity");
        grade_section = grade_section.with_row(PropertyRow::new(
            localizer.text("inspector-grade-pre-post"),
            Box::new(
                Label::new(format!(
                    "{} → {}",
                    grade.group_pre_grade.as_deref().unwrap_or(&identity),
                    grade.group_post_grade.as_deref().unwrap_or(&identity)
                ))
                .muted(),
            ),
        ));
    }
    if let Some(timeline_grade) = &grade.timeline_grade {
        grade_section = grade_section.with_row(PropertyRow::new(
            localizer.text("inspector-grade-timeline"),
            Box::new(Label::new(timeline_grade.clone()).muted()),
        ));
    }
    if let Some(version) = &grade.active_version {
        grade_section = grade_section.with_row(PropertyRow::new(
            localizer.text("inspector-grade-version-nodes"),
            Box::new(
                Label::new(localizer.format(
                    "inspector-grade-summary",
                    Some(&{
                        let mut args = fluent_bundle::FluentArgs::new();
                        args.set("version", version.as_str());
                        args.set("versions", grade.version_count as i64);
                        args.set("nodes", grade.node_count as i64);
                        args
                    }),
                ))
                .muted(),
            ),
        ));
    }
    if grade.clip_definition_id.is_some() {
        let items = grade
            .add_node_actions
            .iter()
            .map(|item| MenuItem::new(item.label.clone(), item.action.clone()))
            .collect();
        let enabled = !grade.add_node_actions.is_empty();
        grade_section = grade_section.with_row(PropertyRow::new(
            localizer.text("inspector-grade-add-node"),
            Box::new(
                Dropdown::new(
                    localizer.text("inspector-grade-add-node-placeholder"),
                    items,
                )
                .enabled(enabled),
            ),
        ));
    }
    panel = panel.with_section(grade_section);

    if !model.effects.is_empty() {
        for (index, effect) in model.effects.iter().enumerate() {
            let effect_id = effect.effect_id;
            let can_move_up = can_edit && index > 0;
            let can_move_down = can_edit && index + 1 < model.effects.len();
            let mut section = PropertySection::new(effect.label.clone())
                .selected(model.selected_effect_id == Some(effect_id))
                .on_select(inspector_effect_select_action(selected_clip, effect_id))
                .with_row(PropertyRow::new(
                    localizer.text("inspector-effect-controls"),
                    Box::new(
                        FlexContainer::row(vec![
                            FlexChild::flex(
                                Box::new(
                                    Checkbox::new(
                                        localizer.text("inspector-enabled"),
                                        effect.enabled,
                                    )
                                    .enabled(can_edit)
                                    .on_change(
                                        move |enabled| {
                                            inspector_effect_enabled_action(
                                                selected_clip,
                                                effect_id,
                                                enabled,
                                            )
                                        },
                                    ),
                                ),
                                1.0,
                            ),
                            FlexChild::fixed(effect_icon_button(
                                AppIcon::CaretUp,
                                localizer.text("inspector-effect-move-up"),
                                localizer.text("inspector-effect-move-up-tooltip"),
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
                                localizer.text("inspector-effect-move-down"),
                                localizer.text("inspector-effect-move-down-tooltip"),
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
                                localizer.text("inspector-effect-remove"),
                                localizer.text("inspector-effect-remove-tooltip"),
                                can_edit,
                                inspector_remove_effect_row_action(selected_clip, effect_id),
                            )),
                        ])
                        .with_gap(8.0),
                    ),
                ));
            let mut previous_group = None;
            for property in &effect.properties {
                if property.group_name.as_deref() != previous_group {
                    if let Some(group_name) = &property.group_name {
                        section = section.with_row(PropertyRow::new(
                            property.group_label.as_ref().unwrap_or(group_name).clone(),
                            Box::new(Label::new("")),
                        ));
                    }
                    previous_group = property.group_name.as_deref();
                }
                section = section.with_row(effect_property_row(
                    localizer,
                    property,
                    can_edit,
                    selected_clip,
                    effect_id,
                ));
                if let Some(curve) = property.numeric_curve.as_ref()
                    && curve.curve.keys.iter().any(|key| key.keyframe_id.is_some())
                {
                    let curve_model = curve.curve.clone();
                    section = section.with_row(
                        PropertyRow::new(
                            localizer.format_text(
                                "inspector-effect-curve",
                                "name",
                                &property.label,
                            ),
                            Box::new(numeric_curve_editor(
                                localizer,
                                selected_clip.map(|selection| {
                                    ParameterCurveGestureTarget::Effect {
                                        clip_id: selection.clip_id,
                                        effect_id,
                                        parameter: curve_model.property.clone(),
                                    }
                                }),
                                curve_model,
                                can_edit,
                                Some(property.schema.allowed_interpolations.clone()),
                                move |curve, edit| {
                                    effect_curve_edit_action(selected_clip, effect_id, curve, edit)
                                },
                                move |curve, keyframe_id, interpolation| {
                                    selected_clip.map(|selection| {
                                        visual_effect_edit_numeric_curve_action(
                                            VisualEffectEditNumericCurvePayload {
                                                clip_id: selection.clip_id,
                                                effect_id,
                                                parameter: curve.property.clone(),
                                                edit: ClipCurveEditPayload::SetInterpolation {
                                                    keyframe_id,
                                                    interpolation,
                                                },
                                            },
                                        )
                                    })
                                },
                            )),
                        )
                        .with_height(118.0),
                    );
                }
            }
            panel = panel.with_section(section);
        }
    }

    panel.with_section(
        PropertySection::new(localizer.text("inspector-animation")).with_row(
            PropertyRow::new(localizer.text("inspector-curve"), Box::new(curve)).with_height(118.0),
        ),
    )
}

pub(super) fn inspector_blend_mode_dropdown(
    model: &InspectorBlendModeModel,
    clip_id: ClipId,
    can_edit: bool,
) -> Dropdown {
    let mut items = Vec::with_capacity(model.options.len() + 6);
    for option in &model.options {
        if option.separator_before {
            items.push(MenuItem::separator());
        }
        let action = (option.mode != model.selected).then(|| {
            crate::app::ui_actions::clip_set_blend_mode_action(
                crate::app::ui_actions::ClipSetBlendModePayload {
                    clip_id,
                    blend_mode: option.mode,
                },
            )
        });
        items.push(
            MenuItem::new(option.label.clone(), action).checked(option.mode == model.selected),
        );
    }
    Dropdown::new(model.selected_label.clone(), items)
        .with_max_visible_items(14)
        .enabled(can_edit)
}

pub(super) fn with_audio_processor_rack_sections(
    mut panel: PropertyPanel,
    racks: &[AudioProcessorRackModel],
    surface_editable: bool,
    localizer: &Localizer,
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
        let mut insert_items: Vec<MenuItem> = rack
            .insert_options
            .iter()
            .map(|option| {
                MenuItem::new(
                    option.label.clone(),
                    audio_processor_insert_option_action(rack, &option.choice),
                )
            })
            .collect();
        insert_items.push(MenuItem::separator());
        insert_items.push(MenuItem::new(
            localizer.text("audio-rack-install-clap"),
            app_shell_install_clap_library_dialog_action(),
        ));
        insert_items.push(MenuItem::new(
            localizer.text("audio-rack-install-vst3-bundle"),
            app_shell_install_vst3_bundle_dialog_action(),
        ));
        insert_items.push(MenuItem::new(
            localizer.text("audio-rack-install-vst3-file"),
            app_shell_install_vst3_binary_dialog_action(),
        ));
        let mut rack_section = PropertySection::new(title)
            .with_row(PropertyRow::new(
                localizer.text("audio-rack-scope-row"),
                Box::new(Label::new(rack.ownership_label.clone()).muted()),
            ))
            .with_row(PropertyRow::new(
                localizer.text("audio-rack-add-row"),
                Box::new(
                    Dropdown::new(localizer.text("audio-rack-add-processor"), insert_items)
                        .with_max_visible_items(8)
                        .enabled(rack_can_edit),
                ),
            ));
        if let Some(reason) = &rack.edit_disabled_reason {
            rack_section = rack_section.with_row(PropertyRow::new(
                localizer.text("mixer-read-only"),
                Box::new(Label::new(reason.clone()).muted()),
            ));
        }
        if let Some(automation) = &rack.scope_input_gain_automation {
            rack_section = rack_section.with_row(
                PropertyRow::new(
                    localizer.text("audio-rack-scope-input-curve"),
                    audio_automation_curve_control(automation, localizer),
                )
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
                    localizer.text("mixer-controls"),
                    Box::new(
                        FlexContainer::row(vec![
                            FlexChild::flex(
                                Box::new(
                                    Checkbox::new(
                                        localizer.text("audio-rack-bypass"),
                                        processor.bypassed,
                                    )
                                    .enabled(rack_can_edit)
                                    .on_change(
                                        move |bypassed| {
                                            audio_processor_bypass_action(
                                                &rack_for_bypass,
                                                &processor_for_bypass,
                                                bypassed,
                                            )
                                        },
                                    ),
                                ),
                                1.0,
                            ),
                            FlexChild::fixed(effect_icon_button(
                                AppIcon::CaretUp,
                                localizer.text("audio-rack-move-up"),
                                localizer.text("audio-rack-move-up-tooltip"),
                                can_move_up,
                                Some(move_up),
                            )),
                            FlexChild::fixed(effect_icon_button(
                                AppIcon::CaretDown,
                                localizer.text("audio-rack-move-down"),
                                localizer.text("audio-rack-move-down-tooltip"),
                                can_move_down,
                                Some(move_down),
                            )),
                            FlexChild::fixed(effect_icon_button(
                                AppIcon::Trash,
                                localizer.text("audio-rack-remove"),
                                localizer.text("audio-rack-remove-tooltip"),
                                rack_can_edit,
                                Some(audio_processor_remove_action(rack, processor)),
                            )),
                        ])
                        .with_gap(8.0),
                    ),
                ));
            if processor.native_format.is_some() {
                let rebind = audio_processor_rebind_native_action(rack, processor);
                section = section.with_row(PropertyRow::new(
                    localizer.text("audio-rack-plugin-binding"),
                    Box::new(
                        Button::new(localizer.text("audio-rack-rebind-installed"))
                            .enabled(rebind.is_some())
                            .on_click(rebind),
                    ),
                ));
            }
            for parameter in &processor.parameters {
                let label = audio_processor_parameter_label(parameter);
                if parameter.keyframe_count > 0 {
                    section = section.with_row(PropertyRow::new(
                        label.clone(),
                        Box::new(
                            Label::new(mixer_automation_label(localizer, parameter.keyframe_count))
                                .muted(),
                        ),
                    ));
                } else if let Some(numeric) = parameter
                    .schema
                    .numeric
                    .as_ref()
                    .and_then(|contracts| contracts.first())
                    .copied()
                {
                    section = section.with_row(PropertyRow::new(
                        label.clone(),
                        audio_processor_static_parameter_control(
                            rack,
                            processor,
                            parameter,
                            numeric,
                            rack_can_edit,
                        ),
                    ));
                } else {
                    section = section.with_row(PropertyRow::new(
                        label.clone(),
                        Box::new(
                            Label::new(localizer.text("audio-rack-nonnumeric-parameter")).muted(),
                        ),
                    ));
                }
                if let Some(automation) = &parameter.automation {
                    section = section.with_row(
                        PropertyRow::new(
                            localizer.format_text("audio-rack-parameter-curve", "name", &label),
                            audio_automation_curve_control(automation, localizer),
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

pub(super) fn audio_automation_curve_control(
    model: &AudioAutomationCurveModel,
    localizer: &Localizer,
) -> Box<dyn Widget> {
    let action_model = model.clone();
    let interpolation_menus = model
        .keys
        .iter()
        .enumerate()
        .map(|(index, key)| {
            let keyframe = key.keyframe.as_ref()?;
            Some(
                [
                    InterpolationType::Hold,
                    InterpolationType::Linear,
                    InterpolationType::Bezier,
                    InterpolationType::AutoBezier,
                    InterpolationType::ContinuousBezier,
                ]
                .into_iter()
                .filter_map(|interpolation| {
                    let message_id = keyframe_interpolation_message_id(interpolation);
                    audio_automation_interpolation_action(model, index, interpolation).map(
                        |action| {
                            MenuItem::new(localizer.text(message_id), action)
                                .with_message_id(message_id)
                                .checked(audio_keyframe_interpolation(keyframe) == interpolation)
                        },
                    )
                })
                .collect(),
            )
        })
        .collect();
    let mut preview_model = model.clone();
    let mut preview_origin = None;
    let editor = CurveEditor::with_points(model.points())
        .with_tangents(model.tangents())
        .with_preview_projection(move |event| match event {
            CurveGestureEvent::Begin(_) => {
                preview_origin = Some(preview_model.source_curve.clone());
                None
            }
            CurveGestureEvent::Edit { edit, .. } => {
                if let Some(origin) = &preview_origin {
                    preview_model.source_curve = origin.clone();
                }
                let display_points = crate::app_ui::audio_automation::preview_audio_curve_edit(
                    &mut preview_model,
                    edit,
                )?;
                Some(CurvePreview { display_points, tangents: preview_model.tangents() })
            }
            CurveGestureEvent::Cancel(_) => {
                if let Some(origin) = preview_origin.take() {
                    preview_model.source_curve = origin;
                }
                None
            }
            CurveGestureEvent::Finish(_) => {
                preview_origin = None;
                None
            }
        })
        .with_point_policies(model.point_policies())
        .with_display_points(model.display_points.clone())
        .with_point_context_menus(interpolation_menus)
        .enabled(model.is_editable);
    let editor = if model.view_min == model.view_max {
        editor.with_fixed_value(0.5)
    } else {
        editor
    };
    Box::new(editor.on_gesture(move |event| {
        match event {
            CurveGestureEvent::Begin(gesture_id) => Some(
                ParameterGestureAction::BeginCurve {
                    gesture_id,
                    target: ParameterCurveGestureTarget::Audio {
                        target: action_model.target.clone(),
                    },
                }
                .action(),
            ),
            CurveGestureEvent::Edit { gesture_id, edit } => {
                let action = audio_automation_curve_edit_action(&action_model, edit)?;
                if let Some(gesture_id) = gesture_id {
                    ParameterGestureEdit::from_action(&action)
                        .map(|edit| ParameterGestureAction::Update { gesture_id, edit }.action())
                } else {
                    Some(action)
                }
            }
            CurveGestureEvent::Finish(gesture_id) => {
                Some(ParameterGestureAction::Finish { gesture_id }.action())
            }
            CurveGestureEvent::Cancel(gesture_id) => {
                Some(ParameterGestureAction::Cancel { gesture_id }.action())
            }
        }
    }))
}

pub(super) fn audio_processor_static_parameter_control(
    rack: &AudioProcessorRackModel,
    processor: &crate::app_ui::audio_processor_rack::AudioProcessorInstanceModel,
    parameter: &crate::app_ui::audio_processor_rack::AudioProcessorParameterModel,
    numeric: mondrian_core::automation::ParameterNumericContract,
    can_edit: bool,
) -> Box<dyn Widget> {
    let rack_for_action = rack.clone();
    let processor_for_action = processor.clone();
    let parameter_for_action = parameter.clone();
    let action = move |value| {
        audio_processor_set_static_parameter_action(
            &rack_for_action,
            &processor_for_action,
            &parameter_for_action,
            value,
        )
    };
    let enabled = can_edit && parameter.is_static_editable();
    if parameter.schema.value_type == PropertyValueType::Int {
        use crate::app_ui::audio_processor_rack::MAX_EXACT_AUDIO_INTEGER;
        let hard_min = numeric.hard_range.min.ceil().max(-MAX_EXACT_AUDIO_INTEGER) as i64;
        let hard_max = numeric.hard_range.max.floor().min(MAX_EXACT_AUDIO_INTEGER) as i64;
        let min = (numeric.soft_range.min.ceil() as i64).clamp(hard_min, hard_max);
        let max = (numeric.soft_range.max.floor() as i64).clamp(hard_min, hard_max).max(min);
        let step = numeric.step.map_or(1, |value| value.round().max(1.0) as i64);
        numeric_slider_input_control_with_hard_range(
            parameter.static_value as i64,
            min,
            max,
            hard_min,
            hard_max,
            Some(step),
            0,
            enabled,
            move |value: i64| action(value as f64),
        )
    } else {
        numeric_slider_input_control_with_hard_range(
            parameter.static_value,
            numeric.soft_range.min,
            numeric.soft_range.max,
            numeric.hard_range.min,
            numeric.hard_range.max,
            numeric.step,
            audio_processor_parameter_decimals(numeric.step),
            enabled,
            action,
        )
    }
}

pub(super) fn numeric_slider_input_control<T: mondrian_ui_widgets::NumberInputValue, R>(
    value: T,
    min: T,
    max: T,
    step: Option<T>,
    decimals: usize,
    enabled: bool,
    action: impl Fn(T) -> R + 'static,
) -> Box<dyn Widget>
where
    R: Into<Option<Action>>,
{
    numeric_slider_input_control_with_hard_range(
        value, min, max, min, max, step, decimals, enabled, action,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn numeric_slider_input_control_with_hard_range<
    T: mondrian_ui_widgets::NumberInputValue,
    R,
>(
    value: T,
    soft_min: T,
    soft_max: T,
    hard_min: T,
    hard_max: T,
    step: Option<T>,
    decimals: usize,
    enabled: bool,
    action: impl Fn(T) -> R + 'static,
) -> Box<dyn Widget>
where
    R: Into<Option<Action>>,
{
    let action: Rc<dyn Fn(T) -> Option<Action>> = Rc::new(move |value| action(value).into());
    let mut slider = Slider::new(value.ratio(soft_min, soft_max), 0.0, 1.0)
        .enabled(enabled && soft_min != soft_max);
    let step = step.filter(|step| step.is_positive());
    if let Some(step) = step {
        let ratio_step = soft_min.offset(step, true).ratio(soft_min, soft_max);
        if ratio_step > 0.0 {
            slider = slider.with_step(ratio_step);
        }
    }
    let slider_action = Rc::clone(&action);
    let author_interaction = std::cell::Cell::new(false);
    slider = slider.on_edit(move |phase, ratio| {
        use mondrian_ui_widgets::ValueEdit;
        if author_interaction.get() {
            let boundary = match phase {
                ValueEdit::Finish(gesture_id) => {
                    Some(ParameterGestureAction::Finish { gesture_id })
                }
                ValueEdit::Cancel(gesture_id) => {
                    Some(ParameterGestureAction::Cancel { gesture_id })
                }
                _ => None,
            };
            if let Some(boundary) = boundary {
                author_interaction.set(false);
                return Some(boundary.action());
            }
        }
        let action = slider_action(if matches!(phase, ValueEdit::Cancel(_)) {
            value
        } else {
            T::from_ratio(ratio, soft_min, soft_max, step)
        });
        let Some(edit) = action.as_ref().and_then(ParameterGestureEdit::from_action) else {
            return if matches!(phase, ValueEdit::Finish(_)) {
                None
            } else {
                action
            };
        };
        match phase {
            ValueEdit::Single => action,
            ValueEdit::Begin(gesture_id) => {
                author_interaction.set(true);
                Some(ParameterGestureAction::Begin { gesture_id, edit }.action())
            }
            ValueEdit::Update(gesture_id) => {
                Some(ParameterGestureAction::Update { gesture_id, edit }.action())
            }
            ValueEdit::Finish(gesture_id) => {
                Some(ParameterGestureAction::Finish { gesture_id }.action())
            }
            ValueEdit::Cancel(gesture_id) => {
                Some(ParameterGestureAction::Cancel { gesture_id }.action())
            }
        }
    });

    let mut input = NumberInput::new(value, hard_min, hard_max)
        .with_width(72.0)
        .with_decimals(decimals)
        .enabled(enabled);
    if let Some(step) = step {
        input = input.with_step(step);
    }
    let input_action = Rc::clone(&action);
    input = input.on_change(move |value| input_action(value));

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

/// Map percent edits back to canonical pan, retaining the exact focus baseline.
pub(super) fn inspector_audio_pan_action(
    selection: Option<SelectedClipRef>,
    edit_id: AudioComponentEditId,
    original: f64,
    percent: f64,
) -> Option<Action> {
    let value = if percent == original * 100.0 {
        original
    } else {
        percent / 100.0
    };
    audio_component_mutation_action(selection, edit_id, AudioComponentMutation::SetPan { value })
}

const INSPECTOR_AUDIO_FADE_TIMESCALE: u32 = 1_000;

pub(super) fn inspector_audio_fade_duration_action(
    selection: Option<SelectedClipRef>,
    edit_id: AudioComponentEditId,
    fade_in: bool,
    seconds: f64,
    original: Option<AudioFade>,
) -> Option<Action> {
    if !seconds.is_finite() {
        return None;
    }
    let baseline = original.map(|fade| fade.duration.to_f64()).unwrap_or(0.0);
    let curve = original.map(|fade| fade.curve).unwrap_or(AudioFadeCurve::EqualPower);
    let fade = if seconds == baseline {
        original
    } else if seconds <= 0.0 {
        None
    } else {
        let Ok(duration) =
            TimelineTime::from_f64_quantized(seconds, INSPECTOR_AUDIO_FADE_TIMESCALE)
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
    localizer: &Localizer,
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
                audio_fade_curve_label(curve, localizer),
                audio_component_mutation_action(selection, edit_id, mutation),
            )
            .checked(curve == fade.curve)
        })
        .collect()
}

pub(super) fn audio_fade_curve_label(curve: AudioFadeCurve, localizer: &Localizer) -> String {
    localizer.text(match curve {
        AudioFadeCurve::ConstantGain => "inspector-audio-fade-constant-gain",
        AudioFadeCurve::EqualPower => "inspector-audio-fade-equal-power",
    })
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
            writes: vec![ParameterValueWrite::Value { parameter, value }],
        },
    ))
}

pub(super) fn inspector_parameter_channel_action(
    selection: Option<SelectedClipRef>,
    parameter: Option<AnimationParameterAddress>,
    index: usize,
    value: f64,
) -> Option<Action> {
    let selection = selection?;
    let parameter = parameter?;
    Some(clip_write_parameter_values_action(
        ClipWriteParameterValuesPayload {
            clip_id: selection.clip_id,
            writes: vec![ParameterValueWrite::Channels {
                parameter,
                channel_values: vec![(index, value)],
            }],
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

pub(super) fn inspector_add_mask_action(
    selection: Option<SelectedClipRef>,
    shape: MaskShape,
) -> Option<Action> {
    selection.filter(|selection| selection.is_video_track).map(|selection| {
        visual_mask_add_to_clip_action(VisualMaskAddToClipPayload {
            clip_id: selection.clip_id,
            shape,
        })
    })
}

pub(super) fn default_bezier_power_window() -> MaskShape {
    const HANDLE: f32 = 0.193_299_9;
    MaskShape::Path {
        points: vec![
            BezierPoint {
                position: glam::Vec2::new(0.5, 0.15),
                control_in: glam::Vec2::new(-HANDLE, 0.0),
                control_out: glam::Vec2::new(HANDLE, 0.0),
            },
            BezierPoint {
                position: glam::Vec2::new(0.85, 0.5),
                control_in: glam::Vec2::new(0.0, -HANDLE),
                control_out: glam::Vec2::new(0.0, HANDLE),
            },
            BezierPoint {
                position: glam::Vec2::new(0.5, 0.85),
                control_in: glam::Vec2::new(HANDLE, 0.0),
                control_out: glam::Vec2::new(-HANDLE, 0.0),
            },
            BezierPoint {
                position: glam::Vec2::new(0.15, 0.5),
                control_in: glam::Vec2::new(0.0, HANDLE),
                control_out: glam::Vec2::new(0.0, -HANDLE),
            },
        ],
        closed: true,
    }
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

pub(super) fn inspector_mask_start_tracking_action(
    selection: Option<SelectedClipRef>,
    mask_id: MaskId,
    model: MaskTrackingModel,
    direction: MaskTrackingDirection,
) -> Option<Action> {
    selection.map(|selection| {
        visual_mask_start_tracking_action(VisualMaskStartTrackingPayload {
            clip_id: selection.clip_id,
            mask_id,
            model,
            direction,
            settings: MaskTrackingSettings::default(),
        })
    })
}

pub(super) fn inspector_mask_cancel_tracking_action(
    selection: Option<SelectedClipRef>,
    mask_id: MaskId,
) -> Option<Action> {
    selection.map(|selection| {
        visual_mask_cancel_tracking_action(VisualMaskTargetPayload {
            clip_id: selection.clip_id,
            mask_id,
        })
    })
}

pub(super) fn inspector_mask_recompute_tracking_action(
    selection: Option<SelectedClipRef>,
    mask_id: MaskId,
) -> Option<Action> {
    selection.map(|selection| {
        visual_mask_recompute_tracking_action(VisualMaskTargetPayload {
            clip_id: selection.clip_id,
            mask_id,
        })
    })
}

pub(super) fn inspector_tracking_status_label(
    mask: &InspectorMaskModel,
    localizer: &Localizer,
) -> String {
    let Some(status) = &mask.tracking_status else {
        return if mask.has_tracking_recipe {
            localizer.text("mask-status-generated")
        } else {
            localizer.text("mask-status-untracked")
        };
    };
    use crate::app::visual_tracking::VisualTrackingPhase;
    match status.phase {
        VisualTrackingPhase::Queued => localizer.text("mask-status-queued"),
        VisualTrackingPhase::Analyzing => {
            let mut args = fluent_bundle::FluentArgs::new();
            args.set("completed", status.completed_pairs as i64);
            args.set("total", status.total_pairs as i64);
            localizer.format("mask-status-analyzing", Some(&args))
        }
        VisualTrackingPhase::Canceling => localizer.text("mask-status-canceling"),
        VisualTrackingPhase::Canceled => localizer.text("mask-status-canceled"),
        VisualTrackingPhase::Completed => localizer.text("mask-status-completed"),
        VisualTrackingPhase::CacheHit => localizer.text("mask-status-cache-hit"),
        VisualTrackingPhase::Stale => status.detail.as_deref().map_or_else(
            || localizer.text("mask-status-stale"),
            |detail| localizer.format_text("mask-status-stale-detail", "detail", detail),
        ),
        VisualTrackingPhase::Failed => status.detail.as_deref().map_or_else(
            || localizer.text("mask-status-failed"),
            |detail| localizer.format_text("mask-status-failed-detail", "detail", detail),
        ),
    }
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

fn numeric_tangents(
    model: &InspectorCurveModel,
    property: &mondrian_core::AnimatedProperty,
) -> Vec<CurveTangent> {
    let visible = model
        .keys
        .iter()
        .enumerate()
        .filter_map(|(index, key)| Some((key.keyframe_id?, index)))
        .collect::<std::collections::HashMap<_, _>>();
    property
        .channel_keyframe_controls(0)
        .filter_map(|control| {
            crate::app_ui::curve_tangents::project(
                *visible.get(&control.keyframe_id)?,
                control.side,
                (control.key_time, control.key_value),
                control.neighbor_time,
                control.handle,
                model.owner_range,
                [model.value_range.min, model.value_range.max],
            )
        })
        .collect()
}

fn numeric_curve_editor(
    localizer: &Localizer,
    target: Option<ParameterCurveGestureTarget>,
    model: InspectorCurveModel,
    can_edit: bool,
    allowed_interpolations: Option<Vec<ParameterInterpolation>>,
    on_edit: impl Fn(&InspectorCurveModel, CurveEdit) -> Option<Action> + 'static,
    on_interpolation: impl Fn(&InspectorCurveModel, KeyframeId, InterpolationType) -> Option<Action>,
) -> CurveEditor {
    let tangents = numeric_tangents(&model, &model.curve_source);
    let points = model.keys.iter().map(|key| key.point).collect();
    let point_policies = model
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
    let interpolation_menus = model
        .keys
        .iter()
        .map(|key| {
            let id = key.keyframe_id?;
            Some(
                [
                    InterpolationType::Hold,
                    InterpolationType::Linear,
                    InterpolationType::Bezier,
                    InterpolationType::AutoBezier,
                    InterpolationType::ContinuousBezier,
                    InterpolationType::EaseIn,
                    InterpolationType::EaseOut,
                ]
                .into_iter()
                .filter(|interpolation| {
                    allowed_interpolations.as_ref().is_none_or(|allowed| {
                        let schema_interpolation = match interpolation {
                            InterpolationType::Hold => ParameterInterpolation::Hold,
                            InterpolationType::Linear => ParameterInterpolation::Linear,
                            _ => ParameterInterpolation::Bezier,
                        };
                        allowed.contains(&schema_interpolation)
                    })
                })
                .map(|interpolation| {
                    let message_id = keyframe_interpolation_message_id(interpolation);
                    MenuItem::new(
                        localizer.text(message_id),
                        on_interpolation(&model, id, interpolation),
                    )
                    .with_message_id(message_id)
                    .checked(key.interpolation == Some(interpolation))
                })
                .collect(),
            )
        })
        .collect();
    let display_points = model.display_points.clone();
    let preview_model = model.clone();
    let mut preview_source = preview_model.curve_source.clone();
    let mut preview_origin = None;
    let editor = CurveEditor::with_points(points)
        .with_tangents(tangents)
        .with_preview_projection(move |event| match event {
            CurveGestureEvent::Begin(_) => {
                preview_origin = Some(preview_source.clone());
                None
            }
            CurveGestureEvent::Edit { edit, .. } => {
                if matches!(
                    edit,
                    CurveEdit::Move { .. } | CurveEdit::Insert { .. } | CurveEdit::Handle { .. }
                ) && let Some(origin) = &preview_origin
                {
                    preview_source = origin.clone();
                }
                let display_points =
                    preview_numeric_curve_points(&preview_model, &mut preview_source, edit)?;
                Some(CurvePreview {
                    display_points,
                    tangents: numeric_tangents(&preview_model, &preview_source),
                })
            }
            CurveGestureEvent::Cancel(_) => {
                if let Some(origin) = preview_origin.take() {
                    preview_source = origin;
                }
                None
            }
            CurveGestureEvent::Finish(_) => {
                preview_origin = None;
                None
            }
        })
        .with_point_policies(point_policies)
        .with_display_points(display_points)
        .with_point_context_menus(interpolation_menus)
        .enabled(can_edit);
    let editor = if model.value_range.min == model.value_range.max {
        editor.with_fixed_value(0.5)
    } else {
        editor
    };
    editor.on_gesture(move |event| match event {
        CurveGestureEvent::Begin(gesture_id) => target
            .clone()
            .map(|target| ParameterGestureAction::BeginCurve { gesture_id, target }.action()),
        CurveGestureEvent::Edit { gesture_id, edit } => {
            let action = on_edit(&model, edit)?;
            if let Some(gesture_id) = gesture_id {
                ParameterGestureEdit::from_action(&action)
                    .map(|edit| ParameterGestureAction::Update { gesture_id, edit }.action())
            } else {
                Some(action)
            }
        }
        CurveGestureEvent::Finish(gesture_id) => {
            Some(ParameterGestureAction::Finish { gesture_id }.action())
        }
        CurveGestureEvent::Cancel(gesture_id) => {
            Some(ParameterGestureAction::Cancel { gesture_id }.action())
        }
    })
}

/// Derive graph samples through the same canonical preparation as the author action.
pub(super) fn preview_numeric_curve_points(
    model: &InspectorCurveModel,
    source: &mut std::sync::Arc<mondrian_core::automation::AnimatedProperty>,
    edit: CurveEdit,
) -> Option<Vec<CurvePoint>> {
    let edit = numeric_curve_edit_payload(model, edit)?;
    let property = crate::app::preview_numeric_curve_edit(source, model.owner_range, &edit).ok()?;
    let display = (0..=128)
        .map(|index| {
            let time = model
                .owner_range
                .start
                .checked_add(
                    model
                        .owner_range
                        .duration
                        .checked_scale(TimeScale::new(index, 128).ok()?)
                        .ok()?,
                )
                .ok()?;
            let value = property.evaluate(time).as_f64()?;
            Some(CurvePoint::new(
                index as f32 / 128.0,
                model.value_range.normalized_position(value)? as f32,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    *source = std::sync::Arc::new(property);
    Some(display)
}

fn numeric_curve_edit_payload(
    model: &InspectorCurveModel,
    edit: CurveEdit,
) -> Option<ClipCurveEditPayload> {
    let edit = match edit {
        CurveEdit::Handle { index, side, point } => {
            let id = model.keys.get(index)?.keyframe_id?;
            let key = model.curve_source.keyframe_by_id(id)?;
            let handle = model.curve_source.channel_keyframe_handle(0, id, side)?;
            let tangent = numeric_tangents(model, &model.curve_source)
                .into_iter()
                .find(|tangent| tangent.index == index && tangent.side == side)?;
            let edit = crate::app_ui::curve_tangents::lower(
                tangent,
                point,
                key.time,
                handle,
                model.owner_range,
                [model.value_range.min, model.value_range.max],
            )?;
            ClipCurveEditPayload::EditHandle { keyframe_id: id, side, edit }
        }
        CurveEdit::Insert { keyframe_id, point, .. } => ClipCurveEditPayload::Upsert {
            keyframe_id: Some(keyframe_id),
            point: inspector_curve_point_payload(point),
        },
        CurveEdit::Move { index, point } => {
            let key = model.keys.get(index)?;
            let projected = inspector_curve_point_payload(point);
            if let Some(keyframe_id) = key.keyframe_id {
                let time = if point.x == key.point.x {
                    model.curve_source.keyframe_by_id(keyframe_id)?.time
                } else {
                    crate::app::numeric_curve_author_time(model.owner_range, projected.time_ratio)
                        .ok()?
                };
                let value = if point.y == key.point.y {
                    model.curve_source.keyframe_by_id(keyframe_id)?.value
                } else {
                    crate::app::numeric_curve_author_value(
                        &model.curve_source,
                        projected.value_ratio,
                    )
                    .ok()?
                };
                ClipCurveEditPayload::Move { keyframe_id, time: Some(time), value: Some(value) }
            } else {
                ClipCurveEditPayload::Upsert { keyframe_id: None, point: projected }
            }
        }
        CurveEdit::Delete { index } => {
            let keyframe_id = model.keys.get(index).and_then(|key| key.keyframe_id)?;
            ClipCurveEditPayload::Remove { keyframe_id }
        }
    };
    Some(edit)
}

pub(super) fn inspector_curve_edit_action(
    selection: Option<SelectedClipRef>,
    model: &InspectorCurveModel,
    edit: CurveEdit,
) -> Option<Action> {
    let selection = selection?;
    Some(clip_edit_numeric_curve_action(
        ClipEditNumericCurvePayload {
            clip_id: selection.clip_id,
            parameter: model.property.clone(),
            edit: numeric_curve_edit_payload(model, edit)?,
        },
    ))
}

/// Lower a projected Effect curve interaction to its canonical author action.
pub(super) fn effect_curve_edit_action(
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
    model: &InspectorCurveModel,
    edit: CurveEdit,
) -> Option<Action> {
    let selection = selection?;
    Some(visual_effect_edit_numeric_curve_action(
        VisualEffectEditNumericCurvePayload {
            clip_id: selection.clip_id,
            effect_id,
            parameter: model.property.clone(),
            edit: numeric_curve_edit_payload(model, edit)?,
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
    localizer: &Localizer,
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
) -> PropertyRow {
    inspector_property_row(
        localizer,
        property,
        can_edit,
        selection,
        InspectorPropertyTarget::Effect { effect_id, parameter: property.address.clone() },
    )
}

pub(super) fn mask_property_row(
    localizer: &Localizer,
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    mask_id: MaskId,
) -> PropertyRow {
    inspector_property_row(
        localizer,
        property,
        can_edit,
        selection,
        InspectorPropertyTarget::Mask { mask_id, parameter: property.address.clone() },
    )
}

pub(super) fn clip_property_row(
    localizer: &Localizer,
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
) -> PropertyRow {
    inspector_property_row(
        localizer,
        property,
        can_edit,
        selection,
        InspectorPropertyTarget::Clip { parameter: property.address.clone() },
    )
}

pub(super) fn inspector_property_row(
    localizer: &Localizer,
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    target: InspectorPropertyTarget,
) -> PropertyRow {
    let value =
        inspector_property_value_widget(localizer, property, can_edit, selection, target.clone());
    let value: Box<dyn Widget> =
        if let (InspectorPropertyTarget::Effect { effect_id, .. }, Some(curve), Some(selection)) =
            (&target, &property.numeric_curve, selection)
        {
            let toggle =
                visual_effect_toggle_current_key_action(VisualEffectParameterTargetPayload {
                    clip_id: selection.clip_id,
                    effect_id: *effect_id,
                    parameter: property.address.clone(),
                });
            Box::new(
                FlexContainer::row(vec![
                    FlexChild::flex(value, 1.0),
                    FlexChild::fixed(Box::new(
                        Button::new(if curve.current_keyframe_id.is_some() {
                            "◆"
                        } else {
                            "◇"
                        })
                        .minimal()
                        .active(curve.current_keyframe_id.is_some())
                        .enabled(can_edit)
                        .on_click(Some(toggle)),
                    )),
                ])
                .with_gap(4.0),
            )
        } else {
            value
        };
    let label = if let PropertyValue::Curve(curve) = &property.value {
        let mut args = fluent_bundle::FluentArgs::new();
        args.set("label", property.label.as_str());
        args.set("count", curve.points().len() as i64);
        args.set(
            "maximum",
            mondrian_core::automation::MAX_NORMALIZED_CURVE_POINTS as i64,
        );
        localizer.format("curve-point-count-label", Some(&args))
    } else {
        property.label.clone()
    };
    let row = PropertyRow::new(label, value);
    let height = if matches!(&target, InspectorPropertyTarget::Clip { .. })
        && property.path == mondrian_core::BasicTitle::TEXT_PATH
    {
        Some(92.0)
    } else {
        effect_property_row_height(&property.value)
    };
    if let Some(height) = height {
        row.with_height(height)
    } else if property.schema.text_mode == Some(mondrian_core::ParameterTextMode::MultiLine) {
        row.with_content_height()
    } else {
        row
    }
}

pub(super) fn effect_property_row_height(value: &PropertyValue) -> Option<f32> {
    let components: usize = match value {
        PropertyValue::Curve(_) => return Some(150.0),
        PropertyValue::QualifierSamples(samples) => {
            return Some((samples.samples().len() as f32 + 1.0) * 34.0);
        }
        PropertyValue::Vec2(_) | PropertyValue::DoubleVec2(_) | PropertyValue::IntVec2(_) => 2,
        PropertyValue::Vec3(_)
        | PropertyValue::DoubleVec3(_)
        | PropertyValue::DoubleRgb(_)
        | PropertyValue::IntVec3(_) => 3,
        PropertyValue::Vec4(_) | PropertyValue::DoubleColor(_) => 4,
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
    localizer: &Localizer,
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
) -> Box<dyn Widget> {
    inspector_property_value_widget(
        localizer,
        property,
        can_edit,
        selection,
        InspectorPropertyTarget::Effect { effect_id, parameter: property.address.clone() },
    )
}

pub(super) fn inspector_property_value_widget(
    localizer: &Localizer,
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    target: InspectorPropertyTarget,
) -> Box<dyn Widget> {
    match &property.value {
        PropertyValue::Bool(value) => {
            let selected_clip = selection;
            Box::new(
                Checkbox::new(&property.label, *value).enabled(can_edit).on_change(move |v| {
                    inspector_property_action(selected_clip, target.clone(), PropertyValue::Bool(v))
                }),
            )
        }
        PropertyValue::Float(value) => {
            let ((min, max), (hard_min, hard_max)) = parameter_float_ranges(property);
            let selected_clip = selection;
            numeric_slider_input_control_with_hard_range(
                f64::from(*value),
                min,
                max,
                hard_min,
                hard_max,
                property.step.filter(|step| step.is_finite() && *step > 0.0),
                numeric_decimals(property.step, *value),
                can_edit,
                move |v: f64| {
                    inspector_property_channel_action(
                        selected_clip,
                        target.clone(),
                        0,
                        v.clamp(hard_min, hard_max),
                    )
                },
            )
        }
        PropertyValue::Double(value) => {
            let ((min, max), (hard_min, hard_max)) = parameter_double_ranges(property);
            let selected_clip = selection;
            numeric_slider_input_control_with_hard_range(
                *value,
                min,
                max,
                hard_min,
                hard_max,
                property.step.filter(|step| step.is_finite() && *step > 0.0),
                property.step.map_or(2, decimal_places_for_step),
                can_edit,
                move |v: f64| {
                    inspector_property_action(
                        selected_clip,
                        target.clone(),
                        PropertyValue::Double(v),
                    )
                },
            )
        }
        PropertyValue::Int(value) => {
            let ((min, max), (hard_min, hard_max)) = parameter_integer_ranges(property);
            let selected_clip = selection;
            numeric_slider_input_control_with_hard_range(
                *value,
                min,
                max,
                hard_min,
                hard_max,
                Some(
                    property
                        .step
                        .filter(|step| step.is_finite() && *step >= 1.0)
                        .map_or(1, |step| step.round() as i64),
                ),
                0,
                can_edit,
                move |v: i64| {
                    inspector_property_action(selected_clip, target.clone(), PropertyValue::Int(v))
                },
            )
        }
        PropertyValue::Color(value) => {
            let selected_clip = selection;
            let trigger = color_picker_trigger(*value).enabled(can_edit);
            Box::new(trigger.on_change(move |color| {
                inspector_property_action(
                    selected_clip,
                    target.clone(),
                    PropertyValue::Color(color),
                )
            }))
        }
        PropertyValue::Text(value) => {
            let text = value.clone();
            if matches!(&target, InspectorPropertyTarget::Clip { .. })
                && property.schema.parameter_id
                    == mondrian_core::ParameterId::new_static("mondrian.title.text")
            {
                let selected_clip = selection;
                return Box::new(
                    MultilineTextInput::new(&property.label)
                        .with_text(text)
                        .min_lines(3)
                        .enabled(can_edit)
                        .on_change(move |text| {
                            inspector_property_action(
                                selected_clip,
                                target.clone(),
                                PropertyValue::Text(text.to_owned()),
                            )
                        }),
                );
            }
            if property.schema.text_mode == Some(mondrian_core::ParameterTextMode::MultiLine) {
                let selected_clip = selection;
                Box::new(
                    MultilineTextInput::new(&property.label)
                        .with_text(text)
                        .min_lines(3)
                        .enabled(can_edit)
                        .on_commit(move |text| {
                            inspector_property_action(
                                selected_clip,
                                target.clone(),
                                PropertyValue::Text(text.to_owned()),
                            )
                        }),
                )
            } else {
                let selected_clip = selection;
                Box::new(
                    TextInput::new(&property.label).with_text(text).enabled(can_edit).on_commit(
                        move |text| {
                            inspector_property_action(
                                selected_clip,
                                target.clone(),
                                PropertyValue::Text(text.to_string()),
                            )
                        },
                    ),
                )
            }
        }
        PropertyValue::Enum(value) => {
            let mut items = Vec::with_capacity(property.schema.enum_options.len());
            let mut choices: Vec<_> = property.schema.enum_options.iter().enumerate().collect();
            choices.sort_by_key(|(index, option)| (option.display_rank.unwrap_or(0), *index));
            for (_, option) in choices {
                items.push(
                    MenuItem::new(
                        property
                            .enum_labels
                            .get(&option.key)
                            .unwrap_or(&option.display_name)
                            .clone(),
                        inspector_property_action(
                            selection,
                            target.clone(),
                            PropertyValue::Enum(option.key.clone()),
                        ),
                    )
                    .checked(option.key == *value),
                );
            }
            Box::new(
                Dropdown::new(
                    property.enum_labels.get(value).cloned().unwrap_or_else(|| {
                        property
                            .schema
                            .enum_options
                            .iter()
                            .find(|option| option.key == *value)
                            .map_or_else(|| value.clone(), |option| option.display_name.clone())
                    }),
                    items,
                )
                .enabled(can_edit),
            )
        }
        PropertyValue::Resource(reference) => {
            let label = match reference {
                ParameterResourceReference::Unbound => localizer.text("resource-unbound"),
                ParameterResourceReference::ExternalFile { path } => path.display().to_string(),
                ParameterResourceReference::ProjectAsset { .. } => {
                    localizer.text("resource-project-asset")
                }
                ParameterResourceReference::Uri { uri } => uri.clone(),
            };
            let action = selection.map(|selection| {
                crate::app_ui::resource_dialog::ResourceEditorAction::Open {
                    clip_id: selection.clip_id,
                    target,
                }
                .action()
            });
            Box::new(Button::new(label).enabled(can_edit && action.is_some()).on_click(action))
        }
        PropertyValue::Curve(curve) => {
            let points =
                curve.points().iter().map(|point| CurvePoint::new(point.x, point.y)).collect();
            let selected_clip = selection;
            Box::new(
                CurveEditor::with_points(points)
                    .with_point_insertion_limit(
                        mondrian_core::automation::MAX_NORMALIZED_CURVE_POINTS,
                    )
                    .enabled(can_edit)
                    .on_value_edit(move |phase, points| {
                        use mondrian_ui_widgets::ValueEdit;
                        match phase {
                            ValueEdit::Finish(gesture_id) => {
                                return Some(
                                    ParameterGestureAction::Finish { gesture_id }.action(),
                                );
                            }
                            ValueEdit::Cancel(gesture_id) => {
                                return Some(
                                    ParameterGestureAction::Cancel { gesture_id }.action(),
                                );
                            }
                            _ => {}
                        }
                        let curve = NormalizedCurve::new(
                            points
                                .iter()
                                .map(|point| NormalizedCurvePoint::new(point.x, point.y))
                                .collect(),
                        )
                        .ok()?;
                        let action = inspector_property_action(
                            selected_clip,
                            target.clone(),
                            PropertyValue::Curve(curve),
                        )?;
                        let edit = ParameterGestureEdit::from_action(&action)?;
                        Some(match phase {
                            ValueEdit::Single => action,
                            ValueEdit::Begin(gesture_id) => {
                                ParameterGestureAction::Begin { gesture_id, edit }.action()
                            }
                            ValueEdit::Update(gesture_id) => {
                                ParameterGestureAction::Update { gesture_id, edit }.action()
                            }
                            ValueEdit::Finish(_) | ValueEdit::Cancel(_) => return None,
                        })
                    }),
            )
        }
        PropertyValue::QualifierSamples(samples) => {
            qualifier_sample_editor(localizer, samples, can_edit, selection, target)
        }
        PropertyValue::DoubleVec2(value) => {
            vector_property_widget(&["X", "Y"], value, property, can_edit, selection, target)
        }
        PropertyValue::DoubleVec3(value) => vector_property_widget(
            &["X", "Y", "Z"],
            value,
            property,
            can_edit,
            selection,
            target,
        ),
        PropertyValue::DoubleRgb(value) => vector_property_widget(
            &["R", "G", "B"],
            value,
            property,
            can_edit,
            selection,
            target,
        ),
        PropertyValue::DoubleColor(value) => vector_property_widget(
            &["R", "G", "B", "A"],
            value,
            property,
            can_edit,
            selection,
            target,
        ),
        PropertyValue::IntVec2(value) => vector_property_widget(
            &["X", "Y"],
            &value.map(f64::from),
            property,
            can_edit,
            selection,
            target,
        ),
        PropertyValue::IntVec3(value) => vector_property_widget(
            &["X", "Y", "Z"],
            &value.map(f64::from),
            property,
            can_edit,
            selection,
            target,
        ),
        PropertyValue::Vec2(value) => vector_property_widget(
            &["X", "Y"],
            &[f64::from(value.x), f64::from(value.y)],
            property,
            can_edit,
            selection,
            target,
        ),
        PropertyValue::Vec3(value) => vector_property_widget(
            &["X", "Y", "Z"],
            &[f64::from(value.x), f64::from(value.y), f64::from(value.z)],
            property,
            can_edit,
            selection,
            target,
        ),
        PropertyValue::Vec4(value) => vector_property_widget(
            &["X", "Y", "Z", "W"],
            &value.map(f64::from),
            property,
            can_edit,
            selection,
            target,
        ),
    }
}

fn qualifier_sample_editor(
    localizer: &Localizer,
    samples: &QualifierSampleSet,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    target: InspectorPropertyTarget,
) -> Box<dyn Widget> {
    let mut rows = Vec::with_capacity(samples.samples().len() + 1);
    for (index, sample) in samples.samples().iter().copied().enumerate() {
        let base_for_color = samples.clone();
        let color_target = target.clone();
        let color = Color {
            r: sample.rgb[0],
            g: sample.rgb[1],
            b: sample.rgb[2],
            a: 1.0,
        };
        let color_picker = color_picker_trigger(color).enabled(can_edit).on_change(move |color| {
            let mut changed = base_for_color.samples().to_vec();
            changed[index].rgb = [
                color.r.clamp(0.0, 1.0),
                color.g.clamp(0.0, 1.0),
                color.b.clamp(0.0, 1.0),
            ];
            qualifier_sample_set_action(selection, color_target.clone(), changed)
        });

        let operation_items = [
            (
                QualifierSampleOperation::Include,
                "inspector-qualifier-include",
            ),
            (
                QualifierSampleOperation::Exclude,
                "inspector-qualifier-exclude",
            ),
        ]
        .into_iter()
        .map(|(operation, message_id)| {
            let mut changed = samples.samples().to_vec();
            changed[index].operation = operation;
            MenuItem::new(
                localizer.text(message_id),
                qualifier_sample_set_action(selection, target.clone(), changed),
            )
        })
        .collect();
        let operation_label = match sample.operation {
            QualifierSampleOperation::Include => "inspector-qualifier-include",
            QualifierSampleOperation::Exclude => "inspector-qualifier-exclude",
        };
        let mut without = samples.samples().to_vec();
        without.remove(index);
        let remove = qualifier_sample_set_action(selection, target.clone(), without);
        rows.push(FlexChild::fixed(Box::new(
            FlexContainer::row(vec![
                FlexChild::fixed(Box::new(color_picker)),
                FlexChild::flex(
                    Box::new(
                        Dropdown::new(localizer.text(operation_label), operation_items)
                            .enabled(can_edit),
                    ),
                    1.0,
                ),
                FlexChild::fixed(Box::new(
                    Button::new(localizer.text("inspector-qualifier-remove-sample"))
                        .enabled(can_edit && remove.is_some())
                        .on_click(remove),
                )),
            ])
            .with_gap(4.0),
        )));
    }

    let can_add = can_edit && samples.samples().len() < MAX_QUALIFIER_SAMPLES;
    let add_action = |operation| {
        let mut changed = samples.samples().to_vec();
        changed.push(QualifierSample::new([0.5, 0.5, 0.5], operation));
        qualifier_sample_set_action(selection, target.clone(), changed)
    };
    let add_include = add_action(QualifierSampleOperation::Include);
    let add_exclude = add_action(QualifierSampleOperation::Exclude);
    rows.push(FlexChild::fixed(Box::new(
        FlexContainer::row(vec![
            FlexChild::flex(
                Box::new(
                    Button::new(localizer.text("inspector-qualifier-add-include"))
                        .enabled(can_add && add_include.is_some())
                        .on_click(add_include),
                ),
                1.0,
            ),
            FlexChild::flex(
                Box::new(
                    Button::new(localizer.text("inspector-qualifier-add-exclude"))
                        .enabled(can_add && add_exclude.is_some())
                        .on_click(add_exclude),
                ),
                1.0,
            ),
        ])
        .with_gap(4.0),
    )));
    Box::new(FlexContainer::column(rows).with_gap(4.0))
}

pub(super) fn qualifier_sample_set_action(
    selection: Option<SelectedClipRef>,
    target: InspectorPropertyTarget,
    samples: Vec<QualifierSample>,
) -> Option<Action> {
    let samples = QualifierSampleSet::new(samples).ok()?;
    inspector_property_action(selection, target, PropertyValue::QualifierSamples(samples))
}

pub(super) fn vector_property_widget(
    labels: &[&'static str],
    values: &[f64],
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    target: InspectorPropertyTarget,
) -> Box<dyn Widget> {
    let (type_min, type_max) = match property.value.value_type() {
        mondrian_core::PropertyValueType::IntVec2 | mondrian_core::PropertyValueType::IntVec3 => {
            (f64::from(i32::MIN), f64::from(i32::MAX))
        }
        mondrian_core::PropertyValueType::Vec2
        | mondrian_core::PropertyValueType::Vec3
        | mondrian_core::PropertyValueType::Vec4 => (-f64::from(f32::MAX), f64::from(f32::MAX)),
        _ => (-f64::MAX, f64::MAX),
    };
    let integer = matches!(
        property.value,
        PropertyValue::IntVec2(_) | PropertyValue::IntVec3(_)
    );
    let rows = labels
        .iter()
        .zip(values.iter())
        .enumerate()
        .map(|(component_index, (label, value))| {
            let numeric = property
                .schema
                .numeric
                .as_ref()
                .and_then(|contracts| contracts.get(component_index));
            let ((min, max), (hard_min, hard_max), step) = if let Some(numeric) = numeric {
                let hard = (
                    numeric.hard_range.min.max(type_min),
                    numeric.hard_range.max.min(type_max),
                );
                let soft = (
                    numeric.soft_range.min.clamp(hard.0, hard.1),
                    numeric.soft_range.max.clamp(hard.0, hard.1),
                );
                (soft, hard, numeric.step)
            } else {
                let (soft, hard) = parameter_double_ranges(property);
                let hard = (hard.0.max(type_min), hard.1.min(type_max));
                let soft = (soft.0.clamp(hard.0, hard.1), soft.1.clamp(hard.0, hard.1));
                (soft, hard, property.step)
            };
            let step = if integer {
                Some(step.unwrap_or(1.0).max(1.0))
            } else {
                step
            };
            let value = *value;
            let value = if value.is_finite() { value } else { hard_min }.clamp(hard_min, hard_max);
            let selected_clip = selection;
            let target = target.clone();
            let control = numeric_slider_input_control_with_hard_range(
                value,
                min,
                max,
                hard_min,
                hard_max,
                step.filter(|step| step.is_finite() && *step > 0.0),
                if integer {
                    0
                } else if matches!(
                    property.value,
                    PropertyValue::Vec2(_) | PropertyValue::Vec3(_) | PropertyValue::Vec4(_)
                ) {
                    numeric_decimals(step, value as f32)
                } else {
                    step.map_or(2, decimal_places_for_step)
                },
                can_edit,
                move |v: f64| {
                    inspector_property_channel_action(
                        selected_clip,
                        target.clone(),
                        component_index,
                        v.clamp(hard_min, hard_max),
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

fn parameter_float_ranges(property: &InspectorEffectPropertyModel) -> ((f64, f64), (f64, f64)) {
    let (soft, hard) = parameter_double_ranges(property);
    let hard = (
        hard.0.max(-f64::from(f32::MAX)),
        hard.1.min(f64::from(f32::MAX)),
    );
    (
        (soft.0.clamp(hard.0, hard.1), soft.1.clamp(hard.0, hard.1)),
        hard,
    )
}

fn parameter_double_ranges(property: &InspectorEffectPropertyModel) -> ((f64, f64), (f64, f64)) {
    let hard_min = property.hard_min.filter(|value| value.is_finite()).unwrap_or(-f64::MAX);
    let hard_max = property.hard_max.filter(|value| value.is_finite()).unwrap_or(f64::MAX);
    let (hard_min, hard_max) = (hard_min.min(hard_max), hard_min.max(hard_max));
    let min = property
        .min
        .filter(|value| value.is_finite())
        .unwrap_or(0.0)
        .clamp(hard_min, hard_max);
    let max = property
        .max
        .filter(|value| value.is_finite())
        .unwrap_or(1.0)
        .clamp(hard_min, hard_max);
    ((min.min(max), min.max(max)), (hard_min, hard_max))
}

fn parameter_integer_ranges(property: &InspectorEffectPropertyModel) -> ((i64, i64), (i64, i64)) {
    let hard_min = property
        .hard_min
        .filter(|value| value.is_finite())
        .map_or(i64::MIN, |value| value.ceil() as i64);
    let hard_max = property
        .hard_max
        .filter(|value| value.is_finite())
        .map_or(i64::MAX, |value| value.floor() as i64);
    let (hard_min, hard_max) = (hard_min.min(hard_max), hard_min.max(hard_max));
    let min = property
        .min
        .filter(|value| value.is_finite())
        .map_or(0, |value| value.ceil() as i64)
        .clamp(hard_min, hard_max);
    let max = property
        .max
        .filter(|value| value.is_finite())
        .map_or(100, |value| value.floor() as i64)
        .clamp(hard_min, hard_max);
    ((min.min(max), min.max(max)), (hard_min, hard_max))
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
        value,
    )
}

pub(super) fn inspector_property_action(
    selection: Option<SelectedClipRef>,
    target: InspectorPropertyTarget,
    value: PropertyValue,
) -> Option<Action> {
    inspector_property_write_action(selection, target, |parameter| ParameterValueWrite::Value {
        parameter,
        value,
    })
}

fn inspector_property_channel_action(
    selection: Option<SelectedClipRef>,
    target: InspectorPropertyTarget,
    index: usize,
    value: f64,
) -> Option<Action> {
    inspector_property_write_action(selection, target, |parameter| {
        ParameterValueWrite::Channels { parameter, channel_values: vec![(index, value)] }
    })
}

fn inspector_property_write_action(
    selection: Option<SelectedClipRef>,
    target: InspectorPropertyTarget,
    write: impl FnOnce(AnimationParameterAddress) -> ParameterValueWrite,
) -> Option<Action> {
    let selection = selection?;
    Some(target.write_action(selection.clip_id, write))
}

#[cfg(test)]
mod temporal_handle_tests {
    use super::*;
    use crate::app::AppState;
    use mondrian_core::automation::{Keyframe, KeyframeInterpolation, PropertyMutation};
    use mondrian_core::BezierHandleSide;
    use mondrian_ui_core::tree::WidgetTreeView;
    use mondrian_ui_core::types::{KeyCode, Modifiers, MouseButton, Point, Rect};
    use mondrian_ui_core::UiEvent;
    use mondrian_ui_events::EventRouter;
    use std::cell::RefCell;

    #[test]
    fn temporal_control_projection_preserves_real_boundaries_and_virtual_anchor_indices() {
        let mut clip = mondrian_timeline::Clip::new_basic_title(
            "Visible keys",
            "Arial",
            TimelineTime::ZERO,
            TimelineTime::new(1, 2).expect("duration"),
        )
        .expect("clip");
        clip.clip_time_in = TimelineTime::new(1, 4).expect("in");
        let mut identities = Vec::new();
        for (index, interpolation) in [
            InterpolationType::Linear,
            InterpolationType::Hold,
            InterpolationType::Bezier,
            InterpolationType::Linear,
            InterpolationType::Linear,
        ]
        .into_iter()
        .enumerate()
        {
            let key = Keyframe::from_preset(
                TimelineTime::new(index as i64, 4).expect("time"),
                PropertyValue::Float(index as f32 / 4.0),
                interpolation,
            );
            identities.push(key.id);
            clip.transform
                .apply_property_mutation(PropertyMutation::SetKeyframe {
                    path: Transform2D::OPACITY_PATH.to_owned(),
                    keyframe: key,
                })
                .expect("real author key");
        }
        let model = opacity_curve_model_for_clip(&clip, TimelineTime::ZERO).expect("model");
        assert_eq!(
            model.keys.iter().map(|key| key.keyframe_id).collect::<Vec<_>>(),
            identities[1..4].iter().copied().map(Some).collect::<Vec<_>>()
        );
        assert_eq!(
            model.keys.iter().map(|key| key.point.x).collect::<Vec<_>>(),
            vec![0.0, 0.5, 1.0]
        );
        assert_eq!(
            model.keys.iter().map(|key| key.interpolation).collect::<Vec<_>>(),
            vec![
                Some(InterpolationType::Hold),
                Some(InterpolationType::Bezier),
                Some(InterpolationType::Linear)
            ]
        );
        let controls = numeric_tangents(&model, &model.curve_source);
        assert_eq!(controls.len(), 1);
        assert_eq!(controls[0].index, 1);
        assert_eq!(controls[0].side, BezierHandleSide::Outgoing);
        assert_eq!(controls[0].point.x, 2.0 / 3.0);
        let before = clip.clone();
        clip.clip_time_in = TimelineTime::new(3, 10).expect("inner in");
        clip.duration = TimelineTime::new(2, 5).expect("inner span");
        let model = opacity_curve_model_for_clip(&clip, TimelineTime::ZERO).expect("inner model");
        assert_eq!(
            model.keys.iter().map(|key| key.keyframe_id).collect::<Vec<_>>(),
            vec![None, Some(identities[2]), None]
        );
        assert_eq!(
            model.keys.iter().map(|key| key.point.x).collect::<Vec<_>>(),
            vec![0.0, 0.5, 1.0]
        );
        let controls = numeric_tangents(&model, &model.curve_source);
        assert_eq!(controls.len(), 1);
        assert_eq!(controls[0].index, 1);
        assert_eq!(
            clip.transform, before.transform,
            "viewport changes preserve all outside author keys"
        );
    }

    #[test]
    #[ignore = "performance smoke for actual visual/audio temporal control projection"]
    fn temporal_handle_large_curve_projection_smoke() {
        use crate::app_ui::audio_automation::{
            project_audio_automation, AudioAutomationCurveKeyModel, AudioAutomationViewport,
        };
        use mondrian_core::{
            AuthoringTimeDomain, AutomationSegmentInterpolation, ExactAutomationKeyframe,
        };
        use mondrian_timeline::{AudioAutomationTarget, AudioChannelStripOwner};
        use std::time::Instant;
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        for count in [1024usize, 4096, 16384] {
            let clip = mondrian_timeline::Clip::new_basic_title(
                "Projection",
                "Arial",
                TimelineTime::ZERO,
                TimelineTime::ONE,
            )
            .expect("clip");
            let bag = clip.transform.to_property_bag();
            let mut encoded =
                serde_json::to_value(bag.property(Transform2D::OPACITY_PATH).expect("property"))
                    .expect("property fixture");
            let keys = (0..count)
                .map(|index| {
                    Keyframe::from_preset(
                        TimelineTime::new(index as i64, (count - 1) as i64).expect("time"),
                        0.5f64,
                        InterpolationType::Bezier,
                    )
                })
                .collect::<Vec<_>>();
            encoded["channels"][0]["keyframes"] = serde_json::to_value(&keys).expect("keys");
            encoded["animation_enabled"] = serde_json::Value::Bool(true);
            let property: mondrian_core::AnimatedProperty =
                serde_json::from_value(encoded).expect("canonical fixture");
            property.validate().expect("valid author fixture");
            let model_start = Instant::now();
            let visual =
                numeric_curve_model_for_property(&property, &clip).expect("actual visual model");
            println!(
                "{}",
                serde_json::json!({"domain":"visual_model","keys":count,"nanoseconds":model_start.elapsed().as_nanos(),"projected_keys":visual.keys.len()})
            );
            assert_eq!(visual.keys.len(), count);
            let sequence = mondrian_timeline::Sequence::new("Projection");
            let mut audio = project_audio_automation(
                &sequence,
                AudioAutomationTarget::ChannelFader {
                    owner: AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id },
                },
                AudioAutomationViewport {
                    domain: AuthoringTimeDomain::Sequence(sequence.id),
                    range: mondrian_core::TimelineTimeRange::new(
                        TimelineTime::ZERO,
                        TimelineTime::ONE,
                    )
                    .expect("range"),
                },
            )
            .expect("actual audio model");
            let audio_keys = keys
                .iter()
                .map(|key| {
                    let mut audio_key = ExactAutomationKeyframe::linear(key.time, 0.5);
                    audio_key.interpolation_to_next = AutomationSegmentInterpolation::Bezier;
                    audio_key
                })
                .collect::<Vec<_>>();
            audio.source_curve.keyframes = audio_keys.clone().into();
            audio.source_curve.validate().expect("valid audio curve");
            audio.keys = audio_keys
                .into_iter()
                .map(|key| AudioAutomationCurveKeyModel {
                    point: CurvePoint::new(key.time.to_f64() as f32, 0.5),
                    keyframe: Some(key),
                })
                .collect();
            audio.view_min = 0.0;
            audio.view_max = 1.0;
            for domain in ["visual", "audio"] {
                let start = Instant::now();
                let tangents = if domain == "visual" {
                    numeric_tangents(std::hint::black_box(&visual), &property)
                } else {
                    std::hint::black_box(&audio).tangents()
                };
                let elapsed = start.elapsed().as_nanos();
                assert_eq!(tangents.len(), 2 * (count - 1));
                assert!(tangents.iter().all(|control| control.point.y == 0.5));
                assert!(tangents
                    .iter()
                    .all(|control| control.point.x >= 0.0 && control.point.x <= 1.0));
                println!(
                    "{}",
                    serde_json::json!({"domain":domain,"keys":count,"nanoseconds":elapsed,"controls":tangents.len()})
                );
                std::hint::black_box(tangents);
            }
        }
    }

    #[test]
    fn temporal_audio_handles_route_into_one_reversible_author_gesture() {
        use crate::app_ui::audio_automation::{project_audio_automation, AudioAutomationViewport};
        use mondrian_core::{
            AuthoringTimeDomain, AutomationSegmentInterpolation, ExactAutomationKeyframe,
            ExactAutomationTangentMode,
        };
        use mondrian_timeline::{
            apply_audio_automation_edit, inspect_audio_automation, AudioAutomationEdit,
            AudioAutomationEditRequest, AudioAutomationTarget, AudioChannelStripOwner,
        };
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        for terminal in 0..3 {
            let mut sequence = mondrian_timeline::Sequence::new("Audio handles");
            let target = AudioAutomationTarget::ChannelFader {
                owner: AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id },
            };
            let mut first = ExactAutomationKeyframe::linear(TimelineTime::ZERO, -15.0);
            first.interpolation_to_next = AutomationSegmentInterpolation::Bezier;
            let last = ExactAutomationKeyframe::linear(TimelineTime::ONE, -9.0);
            let mut middle =
                ExactAutomationKeyframe::linear(TimelineTime::new(1, 2).expect("middle"), -12.0);
            middle.interpolation_to_next = AutomationSegmentInterpolation::Bezier;
            middle.tangent_mode = ExactAutomationTangentMode::Continuous;
            middle.in_handle = Some(mondrian_core::ExactBezierHandle {
                time_offset: TimelineTime::new(-1, 6).expect("incoming"),
                value_offset: -1.0,
            });
            middle.out_handle = Some(mondrian_core::ExactBezierHandle {
                time_offset: TimelineTime::new(1, 6).expect("outgoing"),
                value_offset: 1.0,
            });
            let key_id = middle.id;
            for keyframe in [first, last, middle] {
                apply_audio_automation_edit(
                    &mut sequence,
                    &AudioAutomationEditRequest {
                        target: target.clone(),
                        edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
                    },
                )
                .expect("seed exact curve");
            }
            let viewport = AudioAutomationViewport {
                domain: AuthoringTimeDomain::Sequence(sequence.id),
                range: TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE)
                    .expect("range"),
            };
            let model = project_audio_automation(&sequence, target.clone(), viewport)
                .expect("actual audio projection");
            let index = model
                .keys
                .iter()
                .position(|key| key.keyframe.as_ref().is_some_and(|key| key.id == key_id))
                .expect("stable key");
            let tangent = model
                .tangents()
                .into_iter()
                .find(|tangent| {
                    tangent.index == index && tangent.side == BezierHandleSide::Outgoing
                })
                .expect("outgoing");
            let before = sequence.clone();
            let original = model.source_curve.clone();
            let mut state = AppState::new();
            state.test_set_sequence(Some(sequence));
            let locale =
                Localizer::new(crate::app_ui::localization::AppUiLocale::EnUs).expect("locale");
            let mut widget = audio_automation_curve_control(&model, &locale);
            widget.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
            let screen = |point: CurvePoint| {
                Point::new(10.0 + 200.0 * point.x, 10.0 + 84.0 * (1.0 - point.y))
            };
            let mut router = EventRouter::new(widget.id());
            let actions = RefCell::new(Vec::new());
            let route =
                |widget: &mut dyn Widget, router: &mut EventRouter, state: &mut AppState, event| {
                    let dispatch = |action| actions.borrow_mut().push(action);
                    router.route(event, &mut WidgetTreeView::new(widget), &dispatch);
                    for action in actions.borrow_mut().drain(..) {
                        state.dispatch_action(action).expect("actual audio author action");
                    }
                };
            let owner = screen(model.keys[index].point);
            for event in [
                UiEvent::MouseDown {
                    position: owner,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                UiEvent::MouseUp {
                    position: owner,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
            ] {
                route(widget.as_mut(), &mut router, &mut state, event);
            }
            let start = screen(tangent.point);
            route(
                widget.as_mut(),
                &mut router,
                &mut state,
                UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
            );
            assert!(widget.has_active_pointer_interaction());
            let moved = Point::new(start.x, start.y - 6.0);
            route(
                widget.as_mut(),
                &mut router,
                &mut state,
                UiEvent::MouseMove { position: moved, modifiers: Modifiers::none() },
            );
            let inspection =
                inspect_audio_automation(state.active_sequence().expect("sequence"), &target)
                    .expect("inspection");
            let curve = inspection.curve().expect("canonical curve");
            curve.validate().expect("continuous constraint");
            let key = curve.keyframes.iter().find(|key| key.id == key_id).expect("stable key");
            assert_eq!(key.time, TimelineTime::new(1, 2).expect("exact key time"));
            assert_eq!(key.value, -12.0);
            assert_eq!(
                curve
                    .keyframe_handle(key_id, BezierHandleSide::Outgoing)
                    .expect("out")
                    .time_offset,
                TimelineTime::new(1, 6).expect("exact unchanged time")
            );
            assert_ne!(
                curve.keyframe_handle(key_id, BezierHandleSide::Incoming),
                original.keyframe_handle(key_id, BezierHandleSide::Incoming)
            );
            if terminal == 2 {
                route(
                    widget.as_mut(),
                    &mut router,
                    &mut state,
                    UiEvent::MouseMove { position: start, modifiers: Modifiers::none() },
                );
            }
            let event = if terminal == 1 {
                UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() }
            } else {
                UiEvent::MouseUp {
                    position: moved,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                }
            };
            route(widget.as_mut(), &mut router, &mut state, event);
            assert!(!widget.has_active_pointer_interaction());
            if terminal == 0 {
                let after = state.active_sequence().expect("after").clone();
                assert!(state.undo_timeline().expect("undo"));
                assert!(state
                    .active_sequence()
                    .expect("undo")
                    .author_state_eq_ignoring_revision(&before));
                assert!(!state.can_undo_action());
                assert!(state.redo_timeline().expect("redo"));
                assert!(state
                    .active_sequence()
                    .expect("redo")
                    .author_state_eq_ignoring_revision(&after));
            } else {
                assert!(state
                    .active_sequence()
                    .expect("restored")
                    .author_state_eq_ignoring_revision(&before));
                assert!(!state.can_undo_action());
            }
        }
    }

    #[test]
    fn temporal_handles_route_into_one_reversible_author_gesture() {
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        for terminal in 0..4 {
            let mut clip = mondrian_timeline::Clip::new_basic_title(
                "Handles",
                "Arial",
                TimelineTime::ZERO,
                TimelineTime::ONE,
            )
            .expect("clip");
            let clip_id = clip.id;
            let mut key = Keyframe::linear(
                TimelineTime::new(1, 2).expect("middle"),
                PropertyValue::Float(0.5),
            );
            key.interp_in = KeyframeInterpolation::Bezier(mondrian_core::ExactBezierHandle {
                time_offset: TimelineTime::new(-1, 6).expect("incoming"),
                value_offset: -0.1,
            });
            key.interp_out = KeyframeInterpolation::Bezier(mondrian_core::ExactBezierHandle {
                time_offset: TimelineTime::new(1, 6).expect("outgoing"),
                value_offset: 0.1,
            });
            key.temporal_flags.continuous = true;
            let key_id = key.id;
            for seeded in [
                Keyframe::linear(TimelineTime::ZERO, PropertyValue::Float(0.25)),
                key,
                Keyframe::linear(TimelineTime::ONE, PropertyValue::Float(0.75)),
            ] {
                clip.transform
                    .apply_property_mutation(PropertyMutation::SetKeyframe {
                        path: Transform2D::OPACITY_PATH.to_owned(),
                        keyframe: seeded,
                    })
                    .expect("seed canonical curve");
            }
            let mut sequence = mondrian_timeline::Sequence::new("Handles");
            let selection = SelectedClipRef {
                track_id: sequence.video_tracks[0].id,
                is_video_track: true,
                clip_id,
            };
            sequence.video_tracks[0].add_clip(clip).expect("clip");
            let mut state = AppState::new();
            state.test_set_sequence(Some(sequence));
            state.select_clip_by_id(clip_id).expect("current Inspector target");
            let before = state.active_sequence().expect("sequence").clone();
            let model = opacity_curve_model_for_clip(
                before.find_clip(clip_id).expect("clip"),
                TimelineTime::ZERO,
            )
            .expect("model");
            let index =
                model.keys.iter().position(|key| key.keyframe_id == Some(key_id)).expect("key");
            let tangent = numeric_tangents(&model, &model.curve_source)
                .into_iter()
                .find(|tangent| {
                    tangent.index == index && tangent.side == BezierHandleSide::Outgoing
                })
                .expect("actual outgoing control");
            let localizer =
                Localizer::new(crate::app_ui::localization::AppUiLocale::EnUs).expect("locale");
            let mut editor = numeric_curve_editor(
                &localizer,
                Some(ParameterCurveGestureTarget::Clip {
                    clip_id,
                    parameter: model.property.clone(),
                }),
                model.clone(),
                true,
                None,
                move |model, edit| inspector_curve_edit_action(Some(selection), model, edit),
                |_, _, _| None,
            );
            editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
            let screen = |point: CurvePoint| {
                Point::new(10.0 + point.x * 200.0, 10.0 + (1.0 - point.y) * 84.0)
            };
            let mut router = EventRouter::new(editor.id());
            let actions = RefCell::new(Vec::new());
            let route = |editor: &mut CurveEditor,
                         router: &mut EventRouter,
                         state: &mut AppState,
                         event| {
                let dispatch = |action| actions.borrow_mut().push(action);
                router.route(event, &mut WidgetTreeView::new(editor), &dispatch);
                for action in actions.borrow_mut().drain(..) {
                    state.dispatch_action(action).expect("real author action");
                }
            };
            let owner = screen(model.keys[index].point);
            for event in [
                UiEvent::MouseDown {
                    position: owner,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                UiEvent::MouseUp {
                    position: owner,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
            ] {
                route(&mut editor, &mut router, &mut state, event);
            }
            assert!(!state.can_undo_action(), "selecting the key is not an edit");
            let start = screen(tangent.point);
            route(
                &mut editor,
                &mut router,
                &mut state,
                UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
            );
            assert!(editor.has_active_pointer_interaction());
            let moved = Point::new(start.x, start.y - 12.0);
            route(
                &mut editor,
                &mut router,
                &mut state,
                UiEvent::MouseMove { position: moved, modifiers: Modifiers::none() },
            );
            let bag = state
                .active_sequence()
                .expect("sequence")
                .find_clip(clip_id)
                .expect("clip")
                .intrinsic_parameter_bag();
            let property = bag.property_by_address(&model.property).expect("property").1;
            property.validate().expect("continuous constraint admitted");
            let changed_key = property.keyframe_by_id(key_id).expect("stable key");
            let original_key = model.curve_source.keyframe_by_id(key_id).expect("original");
            assert_eq!(changed_key.time, original_key.time);
            assert_eq!(changed_key.value, original_key.value);
            assert!(changed_key.temporal_flags.continuous);
            assert_eq!(
                property
                    .channel_keyframe_handle(0, key_id, BezierHandleSide::Outgoing)
                    .expect("out")
                    .time_offset,
                TimelineTime::new(1, 6).expect("exact unchanged time")
            );
            assert_ne!(
                changed_key.interp_out, original_key.interp_out,
                "real handle action must change outgoing geometry"
            );
            assert_ne!(
                changed_key.interp_in, original_key.interp_in,
                "constrained partner follows the edited slope"
            );
            if terminal == 2 {
                route(
                    &mut editor,
                    &mut router,
                    &mut state,
                    UiEvent::MouseMove { position: start, modifiers: Modifiers::none() },
                );
            }
            if terminal == 3 {
                editor.set_enabled(false);
            }
            let event = match terminal {
                1 => UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
                3 => UiEvent::MouseMove { position: moved, modifiers: Modifiers::none() },
                _ => UiEvent::MouseUp {
                    position: moved,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
            };
            route(&mut editor, &mut router, &mut state, event);
            assert!(!editor.has_active_pointer_interaction());
            if terminal == 0 {
                let after = state.active_sequence().expect("after").clone();
                assert!(state.undo_timeline().expect("undo"));
                assert!(state
                    .active_sequence()
                    .expect("undo state")
                    .author_state_eq_ignoring_revision(&before));
                assert!(
                    !state.can_undo_action(),
                    "one pointer draft is one history entry"
                );
                assert!(state.redo_timeline().expect("redo"));
                assert!(state
                    .active_sequence()
                    .expect("redo state")
                    .author_state_eq_ignoring_revision(&after));
            } else {
                assert!(
                    state
                        .active_sequence()
                        .expect("restored")
                        .author_state_eq_ignoring_revision(&before),
                    "terminal={terminal}"
                );
                assert!(
                    !state.can_undo_action(),
                    "cancel or exact origin creates no history"
                );
            }
        }
    }
}
