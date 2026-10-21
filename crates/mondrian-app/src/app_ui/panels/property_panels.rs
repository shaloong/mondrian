//! Property-oriented Mixer and Inspector widget adapters.
//!
//! This deep Module owns widget construction and action lowering while the
//! parent panel Module retains aggregate projection and dock composition.

use super::*;
use crate::app::parameter_gesture::{
    ParameterCurveGestureTarget, ParameterGestureAction, ParameterGestureEdit,
};
use crate::app_ui::localization::keyframe_interpolation_message_id;
use mondrian_core::automation::AnimationChannelValue;
use mondrian_ui_widgets::CurveGestureEvent;
use mondrian_ui_widgets::{CurvePointTarget, CurvePreview, CurveTangent};

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
        let trim_label = format!("{} (dB)", localizer.text("mixer-input-trim"));
        section = section.with_row(PropertyRow::new(
            trim_label.clone(),
            audio_gain_scalar_control(
                channel.input_trim_db,
                audio_mixer_input_trim_target(channel),
                &trim_label,
            ),
        ));
        match channel.fader {
            AudioMixerGainModel::Static { value_db } => {
                let label = format!("{} (dB)", localizer.text("mixer-fader"));
                section = section.with_row(PropertyRow::new(
                    label.clone(),
                    audio_gain_scalar_control(value_db, audio_mixer_fader_target(channel), &label),
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
                    let label = format!("{} (dB)", localizer.text("mixer-level"));
                    route_section = route_section.with_row(PropertyRow::new(
                        label.clone(),
                        audio_gain_scalar_control(
                            value_db,
                            audio_mixer_route_gain_target(route),
                            &label,
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
    if let Some(message) = &model.empty_message {
        let message = localizer.message(message);
        let (title, description) =
            message.split_once('\n').map_or((message.as_str(), ""), |(title, description)| {
                (title, description)
            });
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
            move |curve, keyframe_ids, interpolation| {
                selected_clip.map(|selection| {
                    clip_edit_numeric_curve_action(ClipEditNumericCurvePayload {
                        clip_id: selection.clip_id,
                        parameter: curve.property.clone(),
                        edit: ClipCurveEditPayload::SetInterpolations {
                            keyframe_ids: keyframe_ids.to_vec(),
                            interpolation,
                        },
                    })
                })
            },
        )
    } else {
        CurveEditor::with_points(vec![
            CurvePoint::new(0.0, model.opacity / 100.0),
            CurvePoint::new(1.0, model.opacity / 100.0),
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
                    crate::app_ui::native_visual_gui_section::wrap_section(
                        effect.native_gui_controller.as_ref(),
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
                                move |curve, keyframe_ids, interpolation| {
                                    selected_clip.map(|selection| {
                                        visual_effect_edit_numeric_curve_action(
                                            VisualEffectEditNumericCurvePayload {
                                                clip_id: selection.clip_id,
                                                effect_id,
                                                parameter: curve.property.clone(),
                                                edit: ClipCurveEditPayload::SetInterpolations {
                                                    keyframe_ids: keyframe_ids.to_vec(),
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

pub(super) fn audio_scope_occurrence_control(
    selector: &crate::app_ui::audio_processor_rack::AudioScopeOccurrenceSelectorModel,
) -> Dropdown {
    let items = selector
        .options
        .iter()
        .map(|option| {
            MenuItem::new(
                option.label.clone(),
                crate::app::product_action::ProductAction::Audio(
                    crate::app::product_action::AudioProductAction::SelectScopeOccurrence(
                        option.intent,
                    ),
                )
                .into_external_action(),
            )
        })
        .collect();
    Dropdown::new(selector.selected_label.clone(), items).with_max_visible_items(8)
}

#[derive(serde::Serialize)]
enum AudioRackControlSubject<'a> {
    Rack,
    Parameter(&'a mondrian_core::automation::ParameterSchema),
    InputBus(&'a str),
    Route(mondrian_core::AudioRouteId),
}

fn audio_rack_control_inspection_key(
    rack: &AudioProcessorRackModel,
    processor: Option<&crate::app_ui::audio_processor_rack::AudioProcessorInstanceModel>,
    role: &str,
    subject: AudioRackControlSubject<'_>,
) -> Option<String> {
    serde_json::to_string(&(
        "audio-rack-control",
        rack.address,
        rack.input_inspection_target,
        processor.map(|processor| (processor.processor_id, &processor.definition)),
        role,
        subject,
    ))
    .ok()
}

fn assign_numeric_control_inspection_key(widget: &mut dyn Widget, key: Option<String>) {
    if let Some(node) = widget.accessibility() {
        match node.role {
            mondrian_ui_core::AccessibilityRole::Slider => {
                widget.set_control_inspection_key(key);
                return;
            }
            mondrian_ui_core::AccessibilityRole::TextInput => {
                widget.set_control_inspection_key(key.map(|key| format!("{key}:numeric-entry")));
                return;
            }
            _ => {}
        }
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child_mut(index) {
            assign_numeric_control_inspection_key(child, key.clone());
        }
    }
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
                        .with_control_inspection_key(audio_rack_control_inspection_key(
                            rack,
                            None,
                            "add-processor",
                            AudioRackControlSubject::Rack,
                        ))
                        .with_max_visible_items(8)
                        .enabled(rack_can_edit),
                ),
            ));
        if let Some(selector) = &rack.scope_occurrence_selector {
            rack_section = rack_section.with_row(PropertyRow::new(
                localizer.text("audio-rack-occurrence-row"),
                Box::new({
                    let mut control = audio_scope_occurrence_control(selector);
                    control.set_control_inspection_key(
                        serde_json::to_string(&(
                            "audio-scope-occurrence-navigation",
                            rack.address,
                            selector.clip_id,
                        ))
                        .ok(),
                    );
                    control
                }),
            ));
        }
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
            let mut move_up_control = effect_icon_button(
                AppIcon::CaretUp,
                localizer.text("audio-rack-move-up"),
                localizer.text("audio-rack-move-up-tooltip"),
                can_move_up,
                Some(move_up),
            );
            move_up_control.set_control_inspection_key(audio_rack_control_inspection_key(
                rack,
                Some(processor),
                "move-up",
                AudioRackControlSubject::Rack,
            ));
            let mut move_down_control = effect_icon_button(
                AppIcon::CaretDown,
                localizer.text("audio-rack-move-down"),
                localizer.text("audio-rack-move-down-tooltip"),
                can_move_down,
                Some(move_down),
            );
            move_down_control.set_control_inspection_key(audio_rack_control_inspection_key(
                rack,
                Some(processor),
                "move-down",
                AudioRackControlSubject::Rack,
            ));
            let mut remove_control = effect_icon_button(
                AppIcon::Trash,
                localizer.text("audio-rack-remove"),
                localizer.text("audio-rack-remove-tooltip"),
                rack_can_edit,
                Some(audio_processor_remove_action(rack, processor)),
            );
            remove_control.set_control_inspection_key(audio_rack_control_inspection_key(
                rack,
                Some(processor),
                "remove",
                AudioRackControlSubject::Rack,
            ));
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
                                    .with_control_inspection_key(audio_rack_control_inspection_key(
                                        rack,
                                        Some(processor),
                                        "bypass",
                                        AudioRackControlSubject::Rack,
                                    ))
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
                            FlexChild::fixed(move_up_control),
                            FlexChild::fixed(move_down_control),
                            FlexChild::fixed(remove_control),
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
                            .with_control_inspection_key(audio_rack_control_inspection_key(
                                rack,
                                Some(processor),
                                "rebind",
                                AudioRackControlSubject::Rack,
                            ))
                            .enabled(rebind.is_some())
                            .on_click(rebind),
                    ),
                ));
            }
            if let Some(editor) = &processor.native_editor {
                section = section.with_row(PropertyRow::new(
                    localizer.text("native-editor-row"),
                    Box::new(
                        Button::new(localizer.text(editor.label_key))
                            .with_control_inspection_key(audio_rack_control_inspection_key(
                                rack,
                                Some(processor),
                                "native-editor",
                                AudioRackControlSubject::Rack,
                            ))
                            .enabled(editor.action.is_some())
                            .on_click(editor.action.clone()),
                    ),
                ));
            }
            if let Some(query) = &processor.input_query {
                let action = crate::app::product_action::ProductAction::Audio(
                    crate::app::product_action::AudioProductAction::QueryProcessorInputs(
                        query.payload,
                    ),
                )
                .into_external_action();
                section = section.with_row(PropertyRow::new(
                    localizer.text("audio-input-row"),
                    Box::new(
                        Button::new(query.button_label.clone())
                            .with_control_inspection_key(audio_rack_control_inspection_key(
                                rack,
                                Some(processor),
                                "input-query",
                                AudioRackControlSubject::Rack,
                            ))
                            .on_click(Some(action)),
                    ),
                ));
                section = section.with_row(PropertyRow::new(
                    localizer.text("audio-input-status"),
                    Box::new(Label::new(query.status_label.clone()).muted()),
                ));
            }
            for parameter in &processor.parameters {
                let label = audio_processor_parameter_label(parameter, localizer);
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
                    let can_reset = rack_can_edit
                        && parameter
                            .schema
                            .default_value
                            .to_channel_values()
                            .first()
                            .is_some_and(|value| Some(*value) != parameter.static_value);
                    let reset_action = crate::app::ui_actions::audio_processor_rack_edit_action(
                        mondrian_timeline::AudioProcessorRackEditRequest {
                            address: rack.address,
                            edit:
                                mondrian_timeline::AudioProcessorRackEdit::ResetParameterToDefault {
                                    processor_id: processor.processor_id,
                                    parameter_id: parameter.parameter_id.clone(),
                                },
                        },
                    );
                    let reset = Button::new(localizer.text("parameter-reset-to-default"))
                        .with_control_inspection_key(audio_rack_control_inspection_key(
                            rack,
                            Some(processor),
                            "parameter-reset",
                            AudioRackControlSubject::Parameter(&parameter.schema),
                        ))
                        .minimal()
                        .enabled(can_reset)
                        .on_click(Some(reset_action));
                    let mut scalar = audio_processor_static_parameter_control(
                        rack,
                        processor,
                        parameter,
                        numeric,
                        rack_can_edit,
                        localizer,
                    );
                    name_numeric_control(scalar.as_mut(), &label);
                    let value = FlexContainer::row(vec![
                        FlexChild::flex(scalar, 1.0),
                        FlexChild::fixed(Box::new(reset)),
                    ])
                    .with_gap(mondrian_ui_theme::current_theme().spacing.xs);
                    section = section.with_row(PropertyRow::new(label.clone(), Box::new(value)));
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
            for input in &processor.sidechain_inputs {
                let items = input
                    .options
                    .iter()
                    .map(|option| {
                        MenuItem::new(
                            option.label.clone(),
                            crate::app_ui::audio_sidechain::connection_action(option),
                        )
                    })
                    .collect();
                section = section.with_row(PropertyRow::new(
                    input.label.clone(),
                    Box::new(
                        Dropdown::new(localizer.text("audio-sidechain-add-source"), items)
                            .with_control_inspection_key(audio_rack_control_inspection_key(
                                rack,
                                Some(processor),
                                "sidechain-add-source",
                                AudioRackControlSubject::InputBus(&input.bus_key),
                            ))
                            .with_max_visible_items(12),
                    ),
                ));
            }
            for route in &processor.sidechain_routes {
                let mut items = vec![MenuItem::new(
                    route.source_label.clone(),
                    None::<mondrian_editor_state::Action>,
                )
                .checked(true)];
                items.extend(route.rewire_options.iter().map(|option| {
                    MenuItem::new(
                        option.label.clone(),
                        crate::app_ui::audio_sidechain::rewire_action(option),
                    )
                }));
                section = section.with_row(PropertyRow::new(
                    localizer.text("audio-sidechain-source"),
                    Box::new(
                        Dropdown::new(route.source_label.clone(), items)
                            .with_control_inspection_key(audio_rack_control_inspection_key(
                                rack,
                                Some(processor),
                                "sidechain-source",
                                AudioRackControlSubject::Route(route.route_id),
                            ))
                            .with_max_visible_items(12)
                            .enabled(route.is_editable && !route.rewire_options.is_empty()),
                    ),
                ));
                let enabled_route = route.clone();
                section = section.with_row(PropertyRow::new(
                    route.label.clone(),
                    Box::new(
                        Checkbox::new(localizer.text("mixer-pass-signal"), route.enabled)
                            .with_control_inspection_key(audio_rack_control_inspection_key(
                                rack,
                                Some(processor),
                                "sidechain-enabled",
                                AudioRackControlSubject::Route(route.route_id),
                            ))
                            .enabled(route.is_editable)
                            .on_change(move |enabled| {
                                crate::app_ui::audio_sidechain::enabled_action(
                                    &enabled_route,
                                    enabled,
                                )
                            }),
                    ),
                ));
                if let Some(value) = route.static_gain {
                    let label = format!("{} (dB)", localizer.text("audio-sidechain-level"));
                    section = section.with_row(PropertyRow::new(
                        label.clone(),
                        audio_gain_scalar_control(value, route.scalar_target.clone(), &label),
                    ));
                } else {
                    section = section.with_row(PropertyRow::new(
                        localizer.text("audio-sidechain-level"),
                        Box::new(
                            Label::new(mixer_automation_label(
                                localizer,
                                route.automation_key_count,
                            ))
                            .muted(),
                        ),
                    ));
                }
                if let Some(automation) = &route.gain_automation {
                    section = section.with_row(
                        PropertyRow::new(
                            localizer.text("audio-sidechain-level-curve"),
                            audio_automation_curve_control(automation, localizer),
                        )
                        .with_height(118.0),
                    );
                }
                if let Some(reason) = &route.disabled_reason {
                    section = section.with_row(PropertyRow::new(
                        localizer.text("audio-sidechain-status"),
                        Box::new(Label::new(reason.clone()).muted()),
                    ));
                }
                let action = crate::app_ui::audio_sidechain::remove_action(route);
                section = section.with_row(PropertyRow::new(
                    localizer.text("audio-sidechain-controls"),
                    Box::new(
                        Button::new(localizer.text("audio-sidechain-disconnect"))
                            .with_control_inspection_key(audio_rack_control_inspection_key(
                                rack,
                                Some(processor),
                                "sidechain-disconnect",
                                AudioRackControlSubject::Route(route.route_id),
                            ))
                            .enabled(action.is_some())
                            .on_click(action),
                    ),
                ));
            }
            panel = panel.with_section(section);
        }
    }
    panel
}

// This single gain adapter captures only the closed scalar owner. It does
// not retain a Channel/Rack, routes, native metadata or old callbacks.
fn audio_gain_scalar_control(
    value_db: f64,
    target: Option<crate::app::parameter_gesture::AudioScalarGestureTarget>,
    label: &str,
) -> Box<dyn Widget> {
    let key = target
        .as_ref()
        .and_then(|target| serde_json::to_string(&("audio-scalar-control", target)).ok());
    let enabled = target.is_some();
    let mut control = numeric_slider_input_control_with_hard_range(
        value_db,
        -60.0,
        12.0,
        AUDIO_GAIN_DB_MIN,
        AUDIO_GAIN_DB_MAX,
        Some(0.1),
        1,
        enabled,
        move |value| target.as_ref()?.set_action(value),
    );
    assign_numeric_control_inspection_key(control.as_mut(), key);
    name_numeric_control(control.as_mut(), label);
    control
}

// Adopt the same current localized label painted by this concrete property
// row. Names are presentation metadata and never participate in owner identity.
fn name_numeric_control(widget: &mut dyn Widget, name: &str) {
    if widget.accessibility().is_some_and(|node| {
        matches!(
            node.role,
            mondrian_ui_core::AccessibilityRole::Slider
                | mondrian_ui_core::AccessibilityRole::TextInput
        )
    }) {
        widget.set_accessible_name(Some(name.to_owned()));
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child_mut(index) {
            name_numeric_control(child, name);
        }
    }
}

/// Format parameter units using localized words and standard scientific symbols.
pub(super) fn audio_processor_parameter_label(
    parameter: &crate::app_ui::audio_processor_rack::AudioProcessorParameterModel,
    localizer: &Localizer,
) -> String {
    let unit: std::borrow::Cow<'_, str> = match parameter.schema.unit {
        ParameterUnit::Decibels => "dB".into(),
        ParameterUnit::Milliseconds => "ms".into(),
        ParameterUnit::Samples => localizer.text("parameter-unit-samples").into(),
        ParameterUnit::Percent => "%".into(),
        ParameterUnit::Degrees => "°".into(),
        ParameterUnit::Pixels => "px".into(),
        ParameterUnit::Stops => localizer.text("parameter-unit-stops").into(),
        ParameterUnit::Nits => localizer.text("parameter-unit-nits").into(),
        ParameterUnit::Unitless | ParameterUnit::Normalized | ParameterUnit::TimelineTime => {
            "".into()
        }
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
    let reset_label = localizer.text("parameter-reset-to-default");
    let action_model = model.clone();
    let menu_model = model.clone();
    let clipboard_model = model.clone();
    let clipboard_menu_model = model.clone();
    let clipboard_choices = [
        (
            mondrian_ui_widgets::CurveClipboardCommand::Cut,
            "command-edit-cut",
        ),
        (
            mondrian_ui_widgets::CurveClipboardCommand::Copy,
            "command-edit-copy",
        ),
        (
            mondrian_ui_widgets::CurveClipboardCommand::Paste,
            "command-edit-paste",
        ),
    ]
    .map(|(command, message)| (command, localizer.text(message), message));
    let menu_choices = [
        InterpolationType::Hold,
        InterpolationType::Linear,
        InterpolationType::Bezier,
        InterpolationType::AutoBezier,
        InterpolationType::ContinuousBezier,
    ]
    .into_iter()
    .map(|mode| {
        let message = keyframe_interpolation_message_id(mode);
        (mode, localizer.text(message), message)
    })
    .collect::<Vec<_>>();
    let mut preview_model = model.clone();
    let mut preview_origin = None;
    let pending_action = std::rc::Rc::new(std::cell::RefCell::new(None));
    let preview_action = pending_action.clone();
    let ids = model.keys.iter().map(|key| key.keyframe.as_ref().map(|key| key.id)).collect();
    let mut editor = CurveEditor::with_author_points(model.points(), model.value_axis)
        .with_time_axis(model.viewport.range)
        .with_frame_time_step(model.frame_time_step)
        .with_keyboard_help(
            localizer.text("curve-editor-name"),
            localizer.text(if !model.is_editable {
                "curve-editor-read-only-clipboard-keyboard-help"
            } else if model.current_author_time.is_ok() {
                "curve-editor-clipboard-keyboard-help"
            } else {
                "curve-editor-keyboard-help"
            }),
        )
        .with_view_key(
            serde_json::to_string(&(
                ParameterCurveGestureTarget::Audio { target: model.target.clone() },
                model
                    .time_context
                    .scope_occurrence
                    .map(|address| (address.clip_id, address.edit_id)),
            ))
            .ok(),
        )
        .with_keyframe_ids(ids)
        .with_selection_projection(model.selected_keyframe_ids.iter().copied())
        .with_tangents(model.tangents())
        .with_preview_projection(move |event| match event {
            CurveGestureEvent::Begin(_) => {
                preview_origin = Some(preview_model.source_curve.clone());
                None
            }
            CurveGestureEvent::Edit { edit, .. } => {
                let mut candidate_model = preview_model.clone();
                if let Some(origin) = &preview_origin {
                    candidate_model.source_curve = origin.clone();
                }
                *preview_action.borrow_mut() = None;
                let projection = crate::app_ui::audio_automation::preview_audio_curve_edit(
                    &mut candidate_model,
                    edit,
                )?;
                if candidate_model.source_curve == preview_model.source_curve {
                    return None;
                }
                preview_model = candidate_model;
                let display_points = projection.display_points;
                *preview_action.borrow_mut() = Some(projection.action);
                Some(CurvePreview {
                    virtual_boundaries: crate::app_ui::curve_tangents::virtual_boundary_points(
                        preview_model.source_curve.keyframes.iter().map(|key| key.time),
                        preview_model.viewport.range,
                        &display_points,
                    )?,
                    display_points,
                    tangents: preview_model.tangents(),
                    key_points: preview_model.key_points(),
                })
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
        .on_clipboard(move |command, ids| {
            crate::app_ui::audio_automation::audio_curve_clipboard_action(
                &clipboard_model,
                command,
                ids,
            )
        })
        .with_selection_context_menu(move |ids| {
            let mut items = menu_choices
                .iter()
                .filter(|(mode, _, _)| {
                    let family = match mode {
                        InterpolationType::Hold => ParameterInterpolation::Hold,
                        InterpolationType::Linear => ParameterInterpolation::Linear,
                        _ => ParameterInterpolation::Bezier,
                    };
                    menu_model.allowed_interpolations.contains(&family)
                })
                .map(|(mode, label, message)| {
                    let action =
                        crate::app_ui::audio_automation::audio_automation_interpolations_action(
                            &menu_model,
                            ids,
                            *mode,
                        );
                    let checked = !ids.is_empty()
                        && ids.iter().all(|id| {
                            menu_model
                                .source_curve
                                .keyframes
                                .iter()
                                .find(|key| key.id == *id)
                                .is_some_and(|key| audio_keyframe_interpolation(key) == *mode)
                        });
                    MenuItem::new(label.clone(), action).with_message_id(*message).checked(checked)
                })
                .collect::<Vec<_>>();
            items.push(MenuItem::separator());
            items.extend(clipboard_choices.iter().map(|(command, label, message)| {
                MenuItem::new(
                    label.clone(),
                    crate::app_ui::audio_automation::audio_curve_clipboard_action(
                        &clipboard_menu_model,
                        *command,
                        ids,
                    ),
                )
                .with_message_id(*message)
            }));
            items.push(MenuItem::separator());
            items.push(
                MenuItem::new(
                    reset_label.clone(),
                    crate::app_ui::audio_automation::audio_parameter_reset_action(&menu_model),
                )
                .with_message_id("parameter-reset-to-default"),
            );
            items
        })
        .editable(model.is_editable);
    if model.current_author_time.is_ok() {
        editor = editor
            .with_selection_deletion()
            .with_selection_time_movement()
            .with_selection_value_movement();
    }
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
            CurveGestureEvent::Edit { gesture_id, .. } => {
                let action = pending_action.borrow_mut().take()?;
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
    localizer: &Localizer,
) -> Box<dyn Widget> {
    let Some(static_value) = parameter.static_value else {
        // Native plain/display data never supplies the canonical author value.
        // A snapshot stays native-owned until the explicit one-shot adoption.
        let display = parameter.native_presentation.as_ref().and_then(|fact| {
            fact.value_bits.and(fact.display.clone()).or_else(|| {
                fact.author_value_bits.map(f64::from_bits).filter(|value| value.is_finite())
                    .map(|value| {
                        let decimals = audio_processor_parameter_decimals(numeric.step);
                        format!("{value:.decimals$}")
                    })
            })
        });
        let text = display.as_ref()
            .map(|display| {
                format!(
                    "{} · {}",
                    localizer.text("audio-rack-native-controlled-parameter"),
                    display
                )
            })
            .unwrap_or_else(|| localizer.text("audio-rack-native-controlled-parameter"));
        let label = Box::new(Label::new(text).muted());
        if let Some(query) = parameter.native_query.as_ref().filter(|_| can_edit) {
            let action = crate::app::product_action::ProductAction::Audio(
                crate::app::product_action::AudioProductAction::QueryNativeParameter(query.clone()),
            )
            .into_external_action();
            let button = Button::new(localizer.text("audio-rack-query-native-parameter"))
                .with_control_inspection_key(audio_rack_control_inspection_key(
                    rack,
                    Some(processor),
                    "parameter-query-native",
                    AudioRackControlSubject::Parameter(&parameter.schema),
                ))
                .minimal()
                .on_click(Some(action));
            return Box::new(
                FlexContainer::row(vec![
                    FlexChild::flex(label, 1.0),
                    FlexChild::fixed(Box::new(button)),
                ])
                .with_gap(mondrian_ui_theme::current_theme().spacing.xs),
            );
        }
        let action = parameter.native_adoption.as_ref().filter(|_| can_edit).map(|edit| {
            crate::app::parameter_gesture::ParameterGestureAction::AdoptDisplayedNativeParameter {
                edit: edit.clone(),
            }.action()
        });
        let Some(action) = action else {
            return label;
        };
        let adopt = Button::new(localizer.text("audio-rack-adopt-displayed-value"))
            .with_control_inspection_key(audio_rack_control_inspection_key(
                rack, Some(processor), "parameter-adopt-displayed",
                AudioRackControlSubject::Parameter(&parameter.schema),
            ))
            .minimal()
            .on_click(Some(action));
        return Box::new(FlexContainer::row(vec![
            FlexChild::flex(label, 1.0),
            FlexChild::fixed(Box::new(adopt)),
        ]).with_gap(mondrian_ui_theme::current_theme().spacing.xs));
    };
    let target = can_edit
        .then(|| {
            crate::app_ui::audio_processor_rack::static_parameter_target(rack, processor, parameter)
        })
        .flatten();
    let enabled = target.is_some();
    // Only the exact scalar owner is captured. Rack contents, route menus,
    // native observations, curves and old callbacks are not retained here.
    let action = move |value| audio_processor_set_static_parameter_action(target.as_ref()?, value);
    let mut control = if parameter.schema.value_type == PropertyValueType::Int {
        use crate::app_ui::audio_processor_rack::MAX_EXACT_AUDIO_INTEGER;
        let hard_min = numeric.hard_range.min.ceil().max(-MAX_EXACT_AUDIO_INTEGER) as i64;
        let hard_max = numeric.hard_range.max.floor().min(MAX_EXACT_AUDIO_INTEGER) as i64;
        let min = (numeric.soft_range.min.ceil() as i64).clamp(hard_min, hard_max);
        let max = (numeric.soft_range.max.floor() as i64).clamp(hard_min, hard_max).max(min);
        let step = numeric.step.map_or(1, |value| value.round().max(1.0) as i64);
        numeric_slider_input_control_with_hard_range(
            static_value as i64,
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
            static_value,
            numeric.soft_range.min,
            numeric.soft_range.max,
            numeric.hard_range.min,
            numeric.hard_range.max,
            numeric.step,
            audio_processor_parameter_decimals(numeric.step),
            enabled,
            action,
        )
    };
    assign_numeric_control_inspection_key(
        control.as_mut(),
        audio_rack_control_inspection_key(
            rack,
            Some(processor),
            "parameter-slider",
            AudioRackControlSubject::Parameter(&parameter.schema),
        ),
    );
    control
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
    let presented_value = Rc::new(std::cell::Cell::new(value));
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
    let slider_value = Rc::clone(&presented_value);
    let drag_origin = std::cell::Cell::new(None);
    slider = slider.on_edit(move |phase, ratio| {
        use mondrian_ui_widgets::ValueEdit;
        match phase {
            ValueEdit::Begin(_) => {
                drag_origin.set(Some(slider_value.get()));
                slider_value.set(T::from_ratio(ratio, soft_min, soft_max, step));
            }
            ValueEdit::Single | ValueEdit::Update(_) => {
                slider_value.set(T::from_ratio(ratio, soft_min, soft_max, step));
            }
            ValueEdit::Finish(_) => drag_origin.set(None),
            ValueEdit::Cancel(_) => {
                if let Some(origin) = drag_origin.take() {
                    slider_value.set(origin);
                }
            }
        }
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
        let action = slider_action(slider_value.get());
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
    // The admitted model, rather than formatted text, owns the numeric
    // baseline and hard range used by this current presentation pair.
    presented_value.set(input.committed_value());
    let (entry_min, entry_max) = input.range();
    let semantic_value = Rc::clone(&presented_value);
    slider = slider.with_accessible_value(move || {
        semantic_value.get().accessibility_value(entry_min, entry_max)
    });
    let input_action = Rc::clone(&action);
    let input_value = Rc::clone(&presented_value);
    input = input.on_change(move |value| {
        input_value.set(value);
        input_action(value)
    });

    Box::new(NumericSliderInputControl {
        id: mondrian_ui_core::WidgetId::new(),
        slider,
        input,
        presented_value,
        soft_min,
        soft_max,
        layout: mondrian_ui_layout::FlexLayout {
            gap: 8.0,
            ..mondrian_ui_layout::FlexLayout::row()
        },
        bounds: mondrian_ui_core::Rect::ZERO,
    })
}

// The scalar here is the latest UI intent, not proof that an author command
// succeeded. The Host still validates the fresh typed owner/session, then
// rebuilds from the canonical model on refusal. No presentation/draft callback
// or pointer state is carried across that fresh projection boundary.
struct NumericSliderInputControl<T: mondrian_ui_widgets::NumberInputValue> {
    id: mondrian_ui_core::WidgetId,
    slider: Slider,
    input: NumberInput<T>,
    presented_value: Rc<std::cell::Cell<T>>,
    soft_min: T,
    soft_max: T,
    layout: mondrian_ui_layout::FlexLayout,
    bounds: mondrian_ui_core::Rect,
}

impl<T: mondrian_ui_widgets::NumberInputValue> NumericSliderInputControl<T> {
    fn adjust_slider_keyboard(
        &mut self,
        event: &mondrian_ui_core::UiEvent,
        ctx: &mut mondrian_ui_core::widget::EventContext,
    ) -> mondrian_ui_core::EventResult {
        if !self.slider.is_enabled() || ctx.focus.focused_widget() != Some(self.slider.id()) {
            return mondrian_ui_core::EventResult::Ignored;
        }
        let mondrian_ui_core::UiEvent::KeyDown { key, modifiers } = event else {
            return mondrian_ui_core::EventResult::Ignored;
        };
        let Some(adjustment) =
            mondrian_ui_widgets::NumericInputAdjustment::from_key(*key, *modifiers)
        else {
            return mondrian_ui_core::EventResult::Ignored;
        };
        // Finish the pointer gesture before the fresh discrete scalar command.
        // The actual entry model, not the Slider ratio, owns this next value.
        self.slider.finish_pointer_edit(ctx);
        let result = self.input.adjust_unfocused_from_keyboard(adjustment, ctx);
        if result == mondrian_ui_core::EventResult::Handled && self.synchronize_peers() {
            ctx.request_repaint();
        }
        result
    }

    fn synchronize_peers(&mut self) -> bool {
        let value = self.presented_value.get();
        let slider_changed = self.slider.set_value(value.ratio(self.soft_min, self.soft_max));
        let input_changed = self.input.set_unfocused_value(value);
        slider_changed || input_changed
    }
}

impl<T: mondrian_ui_widgets::NumberInputValue> Widget for NumericSliderInputControl<T> {
    fn id(&self) -> mondrian_ui_core::WidgetId {
        self.id
    }

    fn measure(&self, constraint: mondrian_ui_core::LayoutConstraint) -> mondrian_ui_core::Size {
        self.layout
            .measure_with_flex(constraint, &[&self.slider, &self.input], &[1.0, 0.0])
    }

    fn layout(&mut self, bounds: mondrian_ui_core::Rect) {
        self.bounds = bounds;
        self.synchronize_peers();
        let rects =
            self.layout.compute_with_flex(bounds, &[&self.slider, &self.input], &[1.0, 0.0]);
        self.slider.layout(rects[0]);
        self.input.layout(rects[1]);
    }

    fn event(
        &mut self,
        event: &mondrian_ui_core::UiEvent,
        ctx: &mut mondrian_ui_core::widget::EventContext,
    ) -> mondrian_ui_core::EventResult {
        if self.adjust_slider_keyboard(event, ctx) == mondrian_ui_core::EventResult::Handled {
            return mondrian_ui_core::EventResult::Handled;
        }
        let result = if self.input.event(event, ctx) == mondrian_ui_core::EventResult::Handled {
            mondrian_ui_core::EventResult::Handled
        } else {
            self.slider.event(event, ctx)
        };
        if result == mondrian_ui_core::EventResult::Handled {
            self.after_child_event(event, ctx);
        }
        result
    }

    fn before_child_event(
        &mut self,
        event: &mondrian_ui_core::UiEvent,
        ctx: &mut mondrian_ui_core::widget::EventContext,
    ) -> mondrian_ui_core::EventResult {
        self.adjust_slider_keyboard(event, ctx)
    }

    fn after_child_event(
        &mut self,
        _event: &mondrian_ui_core::UiEvent,
        ctx: &mut mondrian_ui_core::widget::EventContext,
    ) -> mondrian_ui_core::EventResult {
        if self.synchronize_peers() {
            ctx.request_repaint();
        }
        mondrian_ui_core::EventResult::Ignored
    }

    fn commit_pending_input(&mut self, ctx: &mut mondrian_ui_core::widget::EventContext) -> bool {
        let changed = self.input.commit_pending_input(ctx);
        if self.synchronize_peers() {
            ctx.request_repaint();
        }
        changed
    }

    fn paint(&self, ctx: &mut mondrian_ui_core::widget::PaintContext) {
        self.slider.paint(ctx);
        self.input.paint(ctx);
    }

    fn hit_test(&self, point: mondrian_ui_core::Point) -> bool {
        self.bounds.contains(point) || self.slider.hit_test(point) || self.input.hit_test(point)
    }

    fn child_count(&self) -> usize {
        2
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.slider),
            1 => Some(&self.input),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.slider),
            1 => Some(&mut self.input),
            _ => None,
        }
    }
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
    let end = match model.owner_range.end() {
        Ok(end) => end,
        Err(_) => return Vec::new(),
    };
    property
        .channel_keyframe_controls(0)
        .filter(|control| control.key_time >= model.owner_range.start && control.key_time <= end)
        .filter_map(|control| {
            crate::app_ui::curve_tangents::project(
                control.keyframe_id,
                control.side,
                (control.key_time, control.key_value),
                control.neighbor_time,
                control.handle,
                model.owner_range,
            )
        })
        .collect()
}

fn numeric_key_points(
    model: &InspectorCurveModel,
    source: &mondrian_core::automation::AnimatedProperty,
) -> Vec<(KeyframeId, CurvePoint)> {
    let Ok(end) = model.owner_range.end() else {
        return Vec::new();
    };
    source
        .channels()
        .first()
        .into_iter()
        .flat_map(|channel| channel.keyframes())
        .filter(|key| key.time >= model.owner_range.start && key.time <= end)
        .filter_map(|key| {
            Some((
                key.id,
                crate::app_ui::curve_tangents::project_key(
                    key.time,
                    key.value.projection(),
                    model.owner_range,
                )?,
            ))
        })
        .collect()
}

fn focused_curve_clipboard_action(
    clip_id: ClipId,
    parameter: &AnimationParameterAddress,
    command: mondrian_ui_widgets::CurveClipboardCommand,
    ids: &[KeyframeId],
) -> Action {
    use crate::app::product_action::{
        ClipCurveClipboardPayload, ClipProductAction, CurveClipboardOperation,
    };
    let operation = match command {
        mondrian_ui_widgets::CurveClipboardCommand::Copy => {
            CurveClipboardOperation::Copy { keyframe_ids: ids.to_vec() }
        }
        mondrian_ui_widgets::CurveClipboardCommand::Cut => {
            CurveClipboardOperation::Cut { keyframe_ids: ids.to_vec() }
        }
        mondrian_ui_widgets::CurveClipboardCommand::Paste => CurveClipboardOperation::Paste {},
    };
    ProductAction::Clip(ClipProductAction::CurveClipboard(Box::new(
        ClipCurveClipboardPayload { clip_id, parameter: parameter.clone(), operation },
    )))
    .into_external_action()
}

fn numeric_curve_editor(
    localizer: &Localizer,
    target: Option<ParameterCurveGestureTarget>,
    model: InspectorCurveModel,
    can_edit: bool,
    allowed_interpolations: Option<Vec<ParameterInterpolation>>,
    on_edit: impl Fn(&InspectorCurveModel, CurveEdit) -> Option<Action> + 'static,
    on_interpolation: impl Fn(&InspectorCurveModel, &[KeyframeId], InterpolationType) -> Option<Action>
        + 'static,
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
    let on_interpolation = std::rc::Rc::new(
        move |model: &InspectorCurveModel, ids: &[KeyframeId], interpolation| {
            if !can_edit {
                return None;
            }
            let edit = ClipCurveEditPayload::SetInterpolations {
                keyframe_ids: ids.to_vec(),
                interpolation,
            };
            let candidate = crate::app::preview_numeric_curve_edit(
                &model.curve_source,
                model.owner_range,
                &edit,
            )
            .ok()?;
            if candidate == *model.curve_source {
                return None;
            }
            on_interpolation(model, ids, interpolation)
        },
    );
    let interpolation_choices = [
        InterpolationType::Hold,
        InterpolationType::Linear,
        InterpolationType::Bezier,
        InterpolationType::AutoBezier,
        InterpolationType::ContinuousBezier,
        InterpolationType::EaseIn,
        InterpolationType::EaseOut,
    ]
    .into_iter()
    .filter(|mode| {
        let family = match mode {
            InterpolationType::Hold => ParameterInterpolation::Hold,
            InterpolationType::Linear => ParameterInterpolation::Linear,
            _ => ParameterInterpolation::Bezier,
        };
        allowed_interpolations.as_ref().is_none_or(|allowed| allowed.contains(&family))
    })
    .map(|mode| {
        let id = keyframe_interpolation_message_id(mode);
        (mode, localizer.text(id), id)
    })
    .collect::<Vec<_>>();
    let clipboard_target = target.as_ref().and_then(|target| match target {
        ParameterCurveGestureTarget::Clip { clip_id, parameter }
        | ParameterCurveGestureTarget::Effect { clip_id, parameter, .. } => {
            Some((*clip_id, parameter.clone()))
        }
        ParameterCurveGestureTarget::Audio { .. } => None,
    });
    let menu_clipboard_target = clipboard_target.clone();
    let cut_label = localizer.text("command-edit-cut");
    let copy_label = localizer.text("command-edit-copy");
    let paste_label = localizer.text("command-edit-paste");
    let menu_model = model.clone();
    let display_points = model.display_points.clone();
    let preview_model = model.clone();
    let mut preview_source = preview_model.curve_source.clone();
    let mut preview_origin = None;
    let pending_action = std::rc::Rc::new(std::cell::RefCell::new(None));
    let preview_action = pending_action.clone();
    let ids = model.keys.iter().map(|key| key.keyframe_id).collect();
    let mut editor = CurveEditor::with_author_points(points, model.value_axis)
        .with_time_axis(model.owner_range)
        .with_frame_time_step(model.frame_time_step)
        .with_keyboard_help(
            localizer.text("curve-editor-name"),
            localizer.text(if can_edit {
                "curve-editor-clipboard-keyboard-help"
            } else if clipboard_target.is_some() {
                "curve-editor-read-only-clipboard-keyboard-help"
            } else {
                "curve-editor-read-only-keyboard-help"
            }),
        )
        .with_view_key(target.as_ref().and_then(|target| serde_json::to_string(target).ok()))
        .with_keyframe_ids(ids)
        .with_selection_projection(model.selected_keyframe_ids.iter().copied())
        .with_selection_deletion()
        .with_selection_time_movement()
        .with_selection_value_movement()
        .with_tangents(tangents)
        .with_preview_projection(move |event| match event {
            CurveGestureEvent::Begin(_) => {
                preview_origin = Some(preview_source.clone());
                None
            }
            CurveGestureEvent::Edit { edit, .. } => {
                let mut candidate_source = preview_source.clone();
                if matches!(
                    edit,
                    CurveEdit::Move { .. }
                        | CurveEdit::Insert { .. }
                        | CurveEdit::Handle { .. }
                        | CurveEdit::TransformMany { .. }
                ) && let Some(origin) = &preview_origin
                {
                    candidate_source = origin.clone();
                }
                *preview_action.borrow_mut() = None;
                let mut live_model = preview_model.clone();
                live_model.curve_source = candidate_source.clone();
                let action = on_edit(&live_model, edit.clone())?;
                let display_points =
                    preview_numeric_curve_points(&live_model, &mut candidate_source, edit)?;
                if candidate_source == preview_source {
                    return None;
                }
                preview_source = candidate_source;
                *preview_action.borrow_mut() = Some(action);
                Some(CurvePreview {
                    virtual_boundaries: crate::app_ui::curve_tangents::virtual_boundary_points(
                        preview_source.channels().first()?.keyframes().iter().map(|key| key.time),
                        preview_model.owner_range,
                        &display_points,
                    )?,
                    key_points: numeric_key_points(&preview_model, &preview_source),
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
        .with_selection_context_menu(move |ids| {
            let mut items: Vec<_> = interpolation_choices
                .iter()
                .map(|(mode, label, message)| {
                    let checked = !ids.is_empty()
                        && ids.iter().all(|id| {
                            menu_model
                                .keys
                                .iter()
                                .find(|key| key.keyframe_id == Some(*id))
                                .is_some_and(|key| key.interpolation == Some(*mode))
                        });
                    MenuItem::new(label.clone(), on_interpolation(&menu_model, ids, *mode))
                        .with_message_id(*message)
                        .checked(checked)
                })
                .collect();
            if let Some((clip_id, parameter)) = &menu_clipboard_target {
                use mondrian_ui_widgets::CurveClipboardCommand;
                items.push(MenuItem::separator());
                items.push(
                    MenuItem::new(
                        cut_label.clone(),
                        (can_edit && !ids.is_empty()).then(|| {
                            focused_curve_clipboard_action(
                                *clip_id,
                                parameter,
                                CurveClipboardCommand::Cut,
                                ids,
                            )
                        }),
                    )
                    .with_message_id("command-edit-cut"),
                );
                items.push(
                    MenuItem::new(
                        copy_label.clone(),
                        (!ids.is_empty()).then(|| {
                            focused_curve_clipboard_action(
                                *clip_id,
                                parameter,
                                CurveClipboardCommand::Copy,
                                ids,
                            )
                        }),
                    )
                    .with_message_id("command-edit-copy"),
                );
                items.push(
                    MenuItem::new(
                        paste_label.clone(),
                        can_edit.then(|| {
                            focused_curve_clipboard_action(
                                *clip_id,
                                parameter,
                                CurveClipboardCommand::Paste,
                                ids,
                            )
                        }),
                    )
                    .with_message_id("command-edit-paste"),
                );
            }
            items
        })
        .editable(can_edit);
    if let Some((clip_id, parameter)) = clipboard_target {
        editor = editor.on_clipboard(move |command, ids| {
            if !can_edit && command != mondrian_ui_widgets::CurveClipboardCommand::Copy {
                return None;
            }
            Some(focused_curve_clipboard_action(
                clip_id, &parameter, command, ids,
            ))
        });
    }
    editor.on_gesture(move |event| match event {
        CurveGestureEvent::Begin(gesture_id) => target
            .clone()
            .map(|target| ParameterGestureAction::BeginCurve { gesture_id, target }.action()),
        CurveGestureEvent::Edit { gesture_id, .. } => {
            let action = pending_action.borrow_mut().take()?;
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
            let value = property.evaluate(time).as_numeric_f64()?;
            Some(CurvePoint { x: index as f32 / 128.0, y: value })
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
        CurveEdit::TransformMany { keyframe_ids, time_delta, value_delta } => {
            ClipCurveEditPayload::TransformMany { keyframe_ids, time_delta, value_delta, steps: 1 }
        }
        CurveEdit::NudgeValue { keyframe_id, delta, steps } => {
            let key = model
                .curve_source
                .channel(0)?
                .keyframes()
                .iter()
                .find(|key| key.id == keyframe_id)?;
            let contract = model.curve_source.descriptor.schema.numeric.as_ref()?.first()?;
            let scalar = match key.value {
                AnimationChannelValue::Integer(value) => PropertyValue::Int(value),
                AnimationChannelValue::Number(value) => PropertyValue::Double(value),
            };
            let value = match contract.offset_scalar(scalar, delta, steps)? {
                PropertyValue::Int(value) => AnimationChannelValue::Integer(value),
                PropertyValue::Double(value) => AnimationChannelValue::Number(value),
                _ => return None,
            };
            ClipCurveEditPayload::Move { keyframe_id, time: None, value: Some(value) }
        }
        CurveEdit::NudgeValueMany { keyframe_ids, delta, steps } => {
            ClipCurveEditPayload::OffsetValuesMany { keyframe_ids, delta, steps }
        }
        CurveEdit::NudgeTimeMany { keyframe_ids, delta } => {
            ClipCurveEditPayload::ShiftMany { keyframe_ids, delta }
        }
        CurveEdit::NudgeTime { keyframe_id, side, delta } => {
            if let Some(side) = side {
                let control = model
                    .curve_source
                    .channel_keyframe_controls(0)
                    .find(|control| control.keyframe_id == keyframe_id && control.side == side)?;
                ClipCurveEditPayload::EditHandle {
                    keyframe_id,
                    side,
                    edit: crate::app_ui::curve_tangents::nudge_handle_time(control, delta)?,
                }
            } else {
                let key = model.curve_source.keyframe_by_id(keyframe_id)?;
                let channel = model.curve_source.channels().first()?;
                let time = crate::app_ui::curve_tangents::nudge_key_time(
                    key.time,
                    delta,
                    model.owner_range,
                    channel.keyframes().iter().map(|key| key.time),
                )?;
                ClipCurveEditPayload::Move { keyframe_id, time: Some(time), value: None }
            }
        }
        CurveEdit::Handle { keyframe_id: id, side, point } => {
            let key = model.curve_source.channel(0)?.keyframes().iter().find(|key| key.id == id)?;
            let handle = model.curve_source.channel_keyframe_handle(0, id, side)?;
            let tangent = numeric_tangents(model, &model.curve_source)
                .into_iter()
                .find(|tangent| tangent.keyframe_id == id && tangent.side == side)?;
            let edit = crate::app_ui::curve_tangents::lower(
                tangent,
                point,
                (key.time, key.value.projection()),
                handle,
                model.owner_range,
            )?;
            ClipCurveEditPayload::EditHandle { keyframe_id: id, side, edit }
        }
        CurveEdit::Insert { keyframe_id, point, .. } => ClipCurveEditPayload::Upsert {
            keyframe_id: Some(keyframe_id),
            point: inspector_curve_point_payload(model, point)?,
        },
        CurveEdit::Move { target, point } => {
            let (identity, reference) = match target {
                CurvePointTarget::Keyframe(id) => {
                    let key = model
                        .curve_source
                        .channel(0)?
                        .keyframes()
                        .iter()
                        .find(|key| key.id == id)?;
                    let reference = crate::app_ui::curve_tangents::project_key(
                        key.time,
                        key.value.projection(),
                        model.owner_range,
                    )?;
                    (Some(id), reference)
                }
                CurvePointTarget::VirtualBoundary(boundary) => {
                    let (x, time) = match boundary {
                        mondrian_ui_widgets::CurveBoundary::Start => (0.0, model.owner_range.start),
                        mondrian_ui_widgets::CurveBoundary::End => {
                            (1.0, model.owner_range.end().ok()?)
                        }
                    };
                    (
                        None,
                        CurvePoint {
                            x,
                            y: model.curve_source.evaluate(time).as_numeric_f64()?,
                        },
                    )
                }
                CurvePointTarget::Snapshot(_) => return None,
            };
            if let Some(keyframe_id) = identity {
                let key = model
                    .curve_source
                    .channel(0)?
                    .keyframes()
                    .iter()
                    .find(|key| key.id == keyframe_id)?;
                let time = if point.x == reference.x {
                    key.time
                } else {
                    crate::app::numeric_curve_author_time(model.owner_range, f64::from(point.x))
                        .ok()?
                };
                let value = if point.y == reference.y {
                    key.value
                } else {
                    crate::app::numeric_curve_author_value(&model.curve_source, point.y).ok()?
                };
                ClipCurveEditPayload::Move { keyframe_id, time: Some(time), value: Some(value) }
            } else {
                ClipCurveEditPayload::Upsert {
                    keyframe_id: None,
                    point: inspector_curve_point_payload(model, point)?,
                }
            }
        }
        CurveEdit::DeleteMany { keyframe_ids } => {
            ClipCurveEditPayload::RemoveMany { keyframe_ids, retain_time: model.author_time }
        }
        CurveEdit::Delete { target } => {
            let CurvePointTarget::Keyframe(keyframe_id) = target else {
                return None;
            };
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

pub(super) fn inspector_curve_point_payload(
    model: &InspectorCurveModel,
    point: CurvePoint,
) -> Option<ClipCurveAuthorPoint> {
    Some(ClipCurveAuthorPoint {
        time: crate::app::numeric_curve_author_time(model.owner_range, f64::from(point.x)).ok()?,
        value: crate::app::numeric_curve_author_value(&model.curve_source, point.y).ok()?,
    })
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
    let reset = inspector_property_write_action(selection, target.clone(), |parameter| {
        ParameterValueWrite::ResetToDefault { parameter }
    });
    let value = Box::new(
        FlexContainer::row(vec![
            FlexChild::flex(value, 1.0),
            FlexChild::fixed(Box::new(
                Button::new(localizer.text("parameter-reset-to-default"))
                    .minimal()
                    .enabled(can_edit && property.can_reset && reset.is_some())
                    .on_click(reset),
            )),
        ])
        .with_gap(mondrian_ui_theme::current_theme().spacing.xs),
    );
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
            let points = curve
                .points()
                .iter()
                .map(|point| CurvePoint::new(point.x, f64::from(point.y)))
                .collect();
            let selected_clip = selection;
            Box::new(
                CurveEditor::with_points(points)
                    .with_view_key(selection.and_then(|selection| {
                        serde_json::to_string(&(selection.clip_id, &target)).ok()
                    }))
                    .with_keyboard_help(
                        localizer.text("curve-editor-name"),
                        localizer.text(if can_edit {
                            "curve-editor-shape-keyboard-help"
                        } else {
                            "curve-editor-read-only-keyboard-help"
                        }),
                    )
                    .with_point_insertion_limit(
                        mondrian_core::automation::MAX_NORMALIZED_CURVE_POINTS,
                    )
                    .editable(can_edit)
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
                                .map(|point| NormalizedCurvePoint::new(point.x, point.y as f32))
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

    fn temporal_curve_fixture(
        seed_keys: bool,
    ) -> (AppState, SelectedClipRef, InspectorCurveModel, KeyframeId) {
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
        ]
        .into_iter()
        .filter(|_| seed_keys)
        {
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
            state.active_sequence().expect("sequence").time_base(),
            before.find_clip(clip_id).expect("clip").clip_time_in,
        )
        .expect("model");
        (state, selection, model, key_id)
    }

    #[test]
    fn focused_numeric_curve_copy_uses_complete_local_selection() {
        use mondrian_ui_core::shortcut::ShortcutBinding;
        use mondrian_ui_core::FocusManager;
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let (mut state, selection, model, _) = temporal_curve_fixture(true);
        let localizer =
            Localizer::new(crate::app_ui::localization::AppUiLocale::EnUs).expect("locale");
        let before = state.active_sequence().expect("before").clone();
        let generation = state.project_author_generation();
        let history = state.authoring_history().expect("history").diagnostics();
        let expected_ids = model.keys.iter().filter_map(|key| key.keyframe_id).collect::<Vec<_>>();
        let mut editor = numeric_curve_editor(
            &localizer,
            Some(ParameterCurveGestureTarget::Clip {
                clip_id: selection.clip_id,
                parameter: model.property.clone(),
            }),
            model,
            true,
            None,
            move |model, edit| inspector_curve_edit_action(Some(selection), model, edit),
            |_, _, _| None,
        );
        editor.layout(Rect::new(0.0, 0.0, 480.0, 160.0));
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut router = EventRouter::new(editor.id());
        router
            .shortcut_manager_mut()
            .register_global(ShortcutBinding::ctrl(KeyCode::C), Action::Copy);
        for event in [
            UiEvent::KeyDown { key: KeyCode::Tab, modifiers: Modifiers::none() },
            UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
        ] {
            router.route(event, &mut WidgetTreeView::new(&mut editor), &dispatch);
            for action in actions.borrow_mut().drain(..) {
                state.dispatch_action(action).expect("selection action");
            }
        }
        assert_eq!(
            editor.selected_keyframe_ids(),
            expected_ids,
            "actual focused curve selection"
        );
        assert_eq!(router.focus_manager().focused_widget(), Some(editor.id()));
        router.route(
            UiEvent::KeyDown { key: KeyCode::C, modifiers: Modifiers::ctrl() },
            &mut WidgetTreeView::new(&mut editor),
            &dispatch,
        );
        let copy_actions = actions.borrow_mut().drain(..).collect::<Vec<_>>();
        assert!(
            !copy_actions.is_empty(),
            "Copy must produce an actual product request"
        );
        for action in copy_actions {
            state.dispatch_action(action).expect("focused Copy");
        }
        assert_eq!(
            state.active_clipboard_kind(),
            Some(crate::app::AppClipboardKind::AnimationKeyframes),
            "the focused key selection must not fall through to copying the Clip",
        );
        assert!(state
            .active_sequence()
            .expect("unchanged")
            .author_state_eq_ignoring_revision(&before));
        assert_eq!(state.project_author_generation(), generation);
        assert_eq!(
            state.authoring_history().expect("history").diagnostics(),
            history
        );
        let clipboard_source =
            before.find_clip(selection.clip_id).expect("source").intrinsic_parameter_bag();
        let property = clipboard_source.property(Transform2D::OPACITY_PATH).expect("source curve");
        let original_keys = expected_ids
            .iter()
            .map(|id| property.keyframe_by_id(*id).expect("source key"))
            .collect::<Vec<_>>();
        router
            .shortcut_manager_mut()
            .register_global(ShortcutBinding::ctrl(KeyCode::V), Action::Paste);
        router.route(
            UiEvent::KeyDown {
                key: KeyCode::V,
                modifiers: Modifiers { meta: true, ..Modifiers::none() },
            },
            &mut WidgetTreeView::new(&mut editor),
            &dispatch,
        );
        let paste_actions = actions.borrow_mut().drain(..).collect::<Vec<_>>();
        assert_eq!(
            paste_actions.len(),
            1,
            "Cmd+V is owned by the focused curve without a global binding"
        );
        let paste = paste_actions[0].clone();
        let seq_id = state.active_sequence().expect("sequence").id;
        state.active_sequence_mut_uncommitted().expect("test sequence").video_tracks[0].is_locked =
            true;
        assert!(
            state.dispatch_action(paste.clone()).is_err(),
            "late Track lock refuses the complete paste"
        );
        assert_eq!(state.project_author_generation(), generation);
        assert_eq!(
            state.authoring_history().expect("history").diagnostics(),
            history
        );
        state.active_sequence_mut_uncommitted().expect("test sequence").video_tracks[0].is_locked =
            false;
        state.dispatch_action(paste).expect("focused Paste");
        assert_eq!(state.active_sequence().expect("sequence").id, seq_id);
        assert_eq!(state.project_author_generation(), generation + 1);
        let pasted = state.active_sequence().expect("pasted").clone();
        let bag = pasted.find_clip(selection.clip_id).expect("clip").intrinsic_parameter_bag();
        let property = bag.property(Transform2D::OPACITY_PATH).expect("pasted property");
        let new_selection = state.selected_animation_keyframes_for_clip(selection.clip_id);
        assert_eq!(new_selection.len(), original_keys.len());
        for original in &original_keys {
            let id = new_selection
                .iter()
                .find(|entry| {
                    property
                        .keyframe_by_id(entry.keyframe_id)
                        .is_some_and(|key| key.time == original.time)
                })
                .expect("pasted time")
                .keyframe_id;
            let key = property.keyframe_by_id(id).expect("pasted key");
            assert!(!expected_ids.contains(&id), "fresh author key identities");
            assert_eq!(key.time, original.time);
            assert_eq!(key.value, original.value);
            assert_eq!(key.temporal_flags, original.temporal_flags);
            assert_eq!(key.interp_in, original.interp_in);
            assert_eq!(key.interp_out, original.interp_out);
        }
        assert!(state.undo_timeline().expect("one Undo"));
        assert!(state
            .active_sequence()
            .expect("undone")
            .author_state_eq_ignoring_revision(&before));
        assert!(
            !state.can_undo_action(),
            "Copy and selection created no additional history"
        );
        let directory = tempfile::tempdir().expect("archives");
        let undone_path = directory.path().join("曲线剪贴板撤销🙂.mdp");
        state.save_project_file_as(undone_path.clone()).expect("save undone");
        assert!(state.can_redo_action(), "save retains live Redo");
        {
            let mut reopened = AppState::new();
            reopened.open_project_file(undone_path).expect("fresh undone");
            assert!(reopened
                .active_sequence()
                .expect("undone reopen")
                .author_state_eq_ignoring_revision(&before));
            assert!(!reopened.can_undo_action() && !reopened.can_redo_action());
            assert_eq!(
                reopened.active_clipboard_kind(),
                None,
                "clipboard is transient"
            );
        }
        assert!(state.redo_timeline().expect("Redo"));
        assert!(state
            .active_sequence()
            .expect("redone")
            .author_state_eq_ignoring_revision(&pasted));
        let path = directory.path().join("曲线剪贴板重做🙂.mdp");
        state.save_project_file_as(path.clone()).expect("save edited");
        let mut reopened = AppState::new();
        reopened.open_project_file(path).expect("fresh edited");
        assert!(reopened
            .active_sequence()
            .expect("edited reopen")
            .author_state_eq_ignoring_revision(&pasted));
        assert!(!reopened.can_undo_action() && !reopened.can_redo_action());
        assert_eq!(reopened.active_clipboard_kind(), None);
    }

    #[test]
    fn temporal_insert_draft_never_paints_another_keys_controls() {
        use mondrian_core::Color;
        use mondrian_ui_core::{DrawCommandEncoder, PaintContext};
        use mondrian_ui_theme::ThemePreset;
        #[derive(Default)]
        struct Lines(Vec<(Point, Point, Color)>);
        impl DrawCommandEncoder for Lines {
            fn push_clip(&mut self, _: Rect) {}
            fn pop_clip(&mut self) {}
            fn draw_rect(&mut self, _: Rect, _: Color, _: f32) {}
            fn draw_line(&mut self, start: Point, end: Point, _: f32, color: Color) {
                self.0.push((start, end, color));
            }
            fn draw_text(&mut self, _: &str, _: f32, _: Point, _: Color) {}
            fn push_translate(&mut self, _: glam::Vec2) {}
            fn pop_transform(&mut self) {}
        }
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let (mut state, selection, model, key_id) = temporal_curve_fixture(true);
        let clip_id = selection.clip_id;
        let before = state.active_sequence().expect("before").clone();
        let localizer =
            Localizer::new(crate::app_ui::localization::AppUiLocale::EnUs).expect("locale");
        let mut editor = numeric_curve_editor(
            &localizer,
            Some(ParameterCurveGestureTarget::Clip { clip_id, parameter: model.property.clone() }),
            model.clone(),
            true,
            None,
            move |model, edit| inspector_curve_edit_action(Some(selection), model, edit),
            |_, _, _| None,
        );
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        let plot = editor
            .as_any()
            .and_then(|any| any.downcast_ref::<CurveEditor>())
            .expect("curve")
            .plot_bounds();
        let screen = |point: CurvePoint| {
            Point::new(
                plot.x + point.x * plot.width,
                plot.y + ((1.0 - point.y) * f64::from(plot.height)) as f32,
            )
        };
        let mut router = EventRouter::new(editor.id());
        let actions = RefCell::new(Vec::new());
        let route =
            |editor: &mut CurveEditor, router: &mut EventRouter, state: &mut AppState, event| {
                let dispatch = |action| actions.borrow_mut().push(action);
                router.route(event, &mut WidgetTreeView::new(editor), &dispatch);
                for action in actions.borrow_mut().drain(..) {
                    state.dispatch_action(action).expect("real author action");
                }
            };
        let inserted = screen(CurvePoint::new(0.25, 0.7));
        route(
            &mut editor,
            &mut router,
            &mut state,
            UiEvent::MouseDown {
                position: inserted,
                button: MouseButton::Left,
                modifiers: Modifiers::ctrl(),
            },
        );
        assert!(editor.has_active_pointer_interaction());
        assert_eq!(editor.selected_index(), Some(1));
        let bag = state
            .active_sequence()
            .expect("sequence")
            .find_clip(clip_id)
            .expect("clip")
            .intrinsic_parameter_bag();
        let property = bag.property_by_address(&model.property).expect("property").1;
        let new_key = property.channels()[0]
            .keyframes()
            .iter()
            .find(|key| key.time == TimelineTime::new(1, 4).expect("insert time"))
            .expect("live canonical inserted key");
        assert_ne!(new_key.id, key_id);
        assert!(
            property
                .channel_keyframe_controls(0)
                .all(|control| control.keyframe_id != new_key.id),
            "this linear inserted key has no canonical controls"
        );
        let theme = ThemePreset::Dark.build();
        let mut lines = Lines::default();
        editor.paint(&mut PaintContext {
            encoder: &mut lines,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 220.0, 104.0),
            pixels_per_point: 1.0,
        });
        assert!(
            lines.0.iter().all(|(_, _, color)| *color != theme.colors.muted_foreground),
            "selected inserted key must not inherit the previous index owners controls"
        );
        route(
            &mut editor,
            &mut router,
            &mut state,
            UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
        );
        assert_eq!(
            state
                .active_sequence()
                .expect("restored")
                .find_clip(clip_id)
                .expect("clip")
                .intrinsic_parameter_bag(),
            before.find_clip(clip_id).expect("before").intrinsic_parameter_bag()
        );
        assert!(!state.can_undo_action());
    }

    fn fixture_curve_editor(
        selection: SelectedClipRef,
        model: &InspectorCurveModel,
    ) -> CurveEditor {
        let localizer =
            Localizer::new(crate::app_ui::localization::AppUiLocale::EnUs).expect("locale");
        let mut editor = numeric_curve_editor(
            &localizer,
            Some(ParameterCurveGestureTarget::Clip {
                clip_id: selection.clip_id,
                parameter: model.property.clone(),
            }),
            model.clone(),
            true,
            None,
            move |model, edit| inspector_curve_edit_action(Some(selection), model, edit),
            |_, _, _| None,
        );
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        editor
    }

    fn route_temporal(
        editor: &mut dyn Widget,
        router: &mut EventRouter,
        state: &mut AppState,
        event: UiEvent,
    ) {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        router.route(event, &mut WidgetTreeView::new(editor), &dispatch);
        for action in actions.borrow_mut().drain(..) {
            state.dispatch_action(action).expect("real author action");
        }
    }

    #[test]
    fn zoomed_temporal_pointer_edits_and_frame_nudge_share_author_history() {
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let (mut state, selection, model, id) = temporal_curve_fixture(true);
        let original = state.active_sequence().expect("before").clone();
        let mut editor = fixture_curve_editor(selection, &model);
        let plot = editor
            .as_any()
            .and_then(|any| any.downcast_ref::<CurveEditor>())
            .expect("curve")
            .plot_bounds();
        let mut router = EventRouter::new(editor.id());
        let center = Point::new(plot.x + plot.width * 0.5, plot.y + plot.height * 0.5);
        for event in [
            UiEvent::KeyDown { key: KeyCode::Tab, modifiers: Modifiers::none() },
            UiEvent::MouseWheel {
                position: center,
                delta: -20.0,
                modifiers: Modifiers::ctrl(),
            },
            UiEvent::MouseWheel {
                position: center,
                delta: -20.0,
                modifiers: Modifiers { shift: true, ..Modifiers::ctrl() },
            },
            UiEvent::MouseDown {
                position: center,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            UiEvent::MouseMove {
                position: Point::new(
                    plot.x + plot.width * 0.6,
                    plot.y + plot.height * (32.0 / 84.0),
                ),
                modifiers: Modifiers::none(),
            },
            UiEvent::MouseWheel {
                position: center,
                delta: -100.0,
                modifiers: Modifiers::ctrl(),
            },
            UiEvent::MouseUp {
                position: Point::new(
                    plot.x + plot.width * 0.6,
                    plot.y + plot.height * (32.0 / 84.0),
                ),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
        ] {
            route_temporal(&mut editor, &mut router, &mut state, event);
        }
        assert!(router.captured().is_none());
        let key = |state: &AppState| {
            state
                .active_sequence()
                .expect("sequence")
                .find_clip(selection.clip_id)
                .expect("clip")
                .intrinsic_parameter_bag()
                .property(Transform2D::OPACITY_PATH)
                .expect("opacity")
                .keyframe_by_id(id)
                .expect("key")
                .clone()
        };
        let pointer = key(&state);
        assert!(
            (pointer.time.to_f64() - (0.5 + 0.1 * (-0.2_f64).exp())).abs() < 0.000001,
            "zoomed pixels lower to author time"
        );
        assert!(
            (pointer.value.as_f64().expect("value") - (0.5 + 10.0 / 84.0 * (-0.2_f64).exp())).abs()
                < 0.000001,
            "zoomed pixels lower to author value"
        );
        route_temporal(
            &mut editor,
            &mut router,
            &mut state,
            UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
        );
        let nudged = key(&state);
        assert_eq!(
            nudged.time,
            pointer.time.checked_add(model.frame_time_step).expect("exact frame")
        );
        assert_eq!(nudged.value, pointer.value);
        let accepted = state.active_sequence().expect("accepted").clone();
        assert!(state.undo_timeline().expect("Undo frame"));
        assert_eq!(key(&state), pointer);
        assert!(state.undo_timeline().expect("Undo pointer"));
        assert!(state
            .active_sequence()
            .expect("restored")
            .author_state_eq_ignoring_revision(&original));
        assert!(!state.can_undo_action());
        assert!(state.redo_timeline().expect("Redo pointer"));
        assert!(state.redo_timeline().expect("Redo frame"));
        assert!(state
            .active_sequence()
            .expect("redo")
            .author_state_eq_ignoring_revision(&accepted));
    }

    #[test]
    fn temporal_multiselection_deletes_atomically_retains_current_sample_and_undoes_once() {
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        for selection_mode in 0..3 {
            let (mut state, selection, _, _) = temporal_curve_fixture(true);
            let original = state.active_sequence().expect("before").clone();
            let time = TimelineTime::new(3, 8).expect("current exact owner time");
            let model = opacity_curve_model_for_clip(
                original.find_clip(selection.clip_id).expect("clip"),
                original.time_base(),
                time,
            )
            .expect("current model");
            assert_eq!(model.author_time, time);
            let retained = model.curve_source.evaluate(time);
            let ids = model.keys.iter().filter_map(|key| key.keyframe_id).collect::<Vec<_>>();
            let mut editor = fixture_curve_editor(selection, &model);
            let mut router = EventRouter::new(editor.id());
            route_temporal(
                &mut editor,
                &mut router,
                &mut state,
                UiEvent::KeyDown { key: KeyCode::Tab, modifiers: Modifiers::none() },
            );
            if selection_mode != 0 {
                route_temporal(
                    &mut editor,
                    &mut router,
                    &mut state,
                    UiEvent::KeyDown {
                        key: KeyCode::A,
                        modifiers: if selection_mode == 1 {
                            Modifiers::ctrl()
                        } else {
                            Modifiers { meta: true, ..Modifiers::none() }
                        },
                    },
                );
            } else {
                for point in model.keys.iter().map(|key| key.point) {
                    let position = editor.point_position(point);
                    route_temporal(
                        &mut editor,
                        &mut router,
                        &mut state,
                        UiEvent::MouseDown {
                            position,
                            button: MouseButton::Left,
                            modifiers: Modifiers::shift(),
                        },
                    );
                    route_temporal(
                        &mut editor,
                        &mut router,
                        &mut state,
                        UiEvent::MouseUp {
                            position,
                            button: MouseButton::Left,
                            modifiers: Modifiers::shift(),
                        },
                    );
                }
            }
            assert_eq!(editor.selected_keyframe_ids(), ids);
            assert!(router.captured().is_none());
            assert!(!state.can_undo_action(), "selection is not an author edit");
            route_temporal(
                &mut editor,
                &mut router,
                &mut state,
                UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
            );
            let accepted = state.active_sequence().expect("deleted").clone();
            let bag =
                accepted.find_clip(selection.clip_id).expect("clip").intrinsic_parameter_bag();
            let property = bag.property(Transform2D::OPACITY_PATH).expect("opacity");
            assert!(!property.is_enabled());
            assert_eq!(property.evaluate(time), retained);
            assert_eq!(property.evaluate(TimelineTime::ZERO), retained);
            assert_eq!(
                editor.points().len(),
                2,
                "canonical virtual boundaries restored"
            );
            assert!(editor.points().iter().all(|point| Some(point.y) == retained.as_numeric_f64()));
            assert!(editor.selected_keyframe_ids().is_empty());
            assert!(state.undo_timeline().expect("one Undo"));
            assert!(state
                .active_sequence()
                .expect("undone")
                .author_state_eq_ignoring_revision(&original));
            assert!(
                !state.can_undo_action(),
                "the set must create exactly one history entry"
            );
            assert!(state.redo_timeline().expect("one Redo"));
            assert!(state
                .active_sequence()
                .expect("redone")
                .author_state_eq_ignoring_revision(&accepted));
        }
    }

    #[test]
    fn temporal_keyboard_time_nudge_uses_the_exact_sequence_frame_grid() {
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        for rate in [
            mondrian_core::Rational::new(30, 1),
            mondrian_core::Rational::new(30_000, 1001),
            mondrian_core::Rational::new(24_000, 1001),
        ] {
            let (mut state, selection, _model, id) = temporal_curve_fixture(true);
            state.active_sequence_mut_uncommitted().expect("fixture").settings.frame_rate = rate;
            let model = opacity_curve_model_for_clip(
                state
                    .active_sequence()
                    .expect("sequence")
                    .find_clip(selection.clip_id)
                    .expect("clip"),
                state.active_sequence().expect("sequence").time_base(),
                state
                    .active_sequence()
                    .expect("sequence")
                    .find_clip(selection.clip_id)
                    .expect("clip")
                    .clip_time_in,
            )
            .expect("rate-bound model");
            let frame = TimelineTime::from_frame_position(mondrian_core::FramePosition::new(
                1,
                state.active_sequence().expect("sequence").time_base(),
            ))
            .expect("exact frame");
            let mut editor = fixture_curve_editor(selection, &model);
            let mut router = EventRouter::new(editor.id());
            for (key, modifiers) in [
                (KeyCode::Tab, Modifiers::none()),
                (KeyCode::Home, Modifiers::none()),
                (KeyCode::Right, Modifiers::ctrl()),
                (KeyCode::Right, Modifiers::none()),
            ] {
                route_temporal(
                    &mut editor,
                    &mut router,
                    &mut state,
                    UiEvent::KeyDown { key, modifiers },
                );
            }
            let bag = state
                .active_sequence()
                .expect("after")
                .find_clip(selection.clip_id)
                .expect("clip")
                .intrinsic_parameter_bag();
            let key = bag
                .property(Transform2D::OPACITY_PATH)
                .expect("opacity")
                .keyframe_by_id(id)
                .expect("stable key");
            assert_eq!(
                key.time,
                TimelineTime::new(1, 2).expect("origin").checked_add(frame).expect("one frame"),
                "one arrow is one exact sequence frame at {rate:?}"
            );
            assert_eq!(key.value, PropertyValue::Float(0.5));
            for (direction, modifiers, frames) in [
                (KeyCode::Right, Modifiers::none(), 2),
                (KeyCode::Left, Modifiers::shift(), -3),
            ] {
                route_temporal(
                    &mut editor,
                    &mut router,
                    &mut state,
                    UiEvent::KeyDown { key: direction, modifiers },
                );
                let expected = TimelineTime::new(1, 2)
                    .expect("origin")
                    .checked_add(
                        frame
                            .checked_scale(TimeScale::new(frames, 1).expect("steps"))
                            .expect("delta"),
                    )
                    .expect("time");
                let bag = state
                    .active_sequence()
                    .expect("sequence")
                    .find_clip(selection.clip_id)
                    .expect("clip")
                    .intrinsic_parameter_bag();
                let changed = bag
                    .property(Transform2D::OPACITY_PATH)
                    .expect("opacity")
                    .keyframe_by_id(id)
                    .expect("same key");
                assert_eq!(changed.time, expected);
                assert_eq!(changed.value, PropertyValue::Float(0.5));
                let point = editor.points()[editor.selected_index().expect("selection")];
                assert_eq!(
                    point.x,
                    expected.to_f64() as f32,
                    "canonical marker follows each author move"
                );
            }
            for _ in 0..3 {
                assert!(state.undo_timeline().expect("Undo"));
            }
            assert!(!state.can_undo_action());
            for _ in 0..3 {
                assert!(state.redo_timeline().expect("Redo"));
            }
            assert!(!state.can_redo_action());
        }
    }

    #[test]
    fn temporal_keyboard_only_selection_and_handle_edits_are_exact_and_reversible() {
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let (mut state, selection, model, id) = temporal_curve_fixture(true);
        let before = state.active_sequence().expect("before").clone();
        let mut editor = fixture_curve_editor(selection, &model);
        let mut router = EventRouter::new(editor.id());
        for (key, modifiers) in [
            (KeyCode::Tab, Modifiers::none()),
            (KeyCode::Home, Modifiers::none()),
            (KeyCode::Right, Modifiers::ctrl()),
            (KeyCode::Right, Modifiers { alt: true, ..Modifiers::none() }),
        ] {
            route_temporal(
                &mut editor,
                &mut router,
                &mut state,
                UiEvent::KeyDown { key, modifiers },
            );
        }
        assert_eq!(
            router.focused(),
            Some(editor.id()),
            "Tab reaches the curve without mouse input"
        );
        assert_eq!(
            editor.point_target(editor.selected_index().expect("selected point")),
            Some(CurvePointTarget::Keyframe(id))
        );
        assert!(
            !state.can_undo_action(),
            "selection creates no author history"
        );
        assert!(state
            .active_sequence()
            .expect("selection only")
            .author_state_eq_ignoring_revision(&before));
        assert_eq!(
            editor.accessibility().expect("a11y").name.as_deref(),
            Some("Curve editor")
        );
        route_temporal(
            &mut editor,
            &mut router,
            &mut state,
            UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
        );
        let handle_edited = state.active_sequence().expect("handle edit").clone();
        let bag = handle_edited
            .find_clip(selection.clip_id)
            .expect("clip")
            .intrinsic_parameter_bag();
        let property = bag.property(Transform2D::OPACITY_PATH).expect("opacity");
        let key = property.keyframe_by_id(id).expect("same key");
        assert_eq!(key.time, TimelineTime::new(1, 2).expect("time"));
        assert_eq!(key.value, PropertyValue::Float(0.5));
        let outgoing = property
            .channel_keyframe_handle(0, id, BezierHandleSide::Outgoing)
            .expect("outgoing");
        let incoming = property
            .channel_keyframe_handle(0, id, BezierHandleSide::Incoming)
            .expect("incoming");
        assert_eq!(
            outgoing.time_offset,
            TimelineTime::new(1, 6).expect("unchanged handle time")
        );
        assert!(outgoing.value_offset > 0.1);
        assert!(
            (incoming.value_offset + outgoing.value_offset).abs() < 1e-6,
            "continuous partner remains constrained"
        );
        route_temporal(
            &mut editor,
            &mut router,
            &mut state,
            UiEvent::KeyDown {
                key: KeyCode::Up,
                modifiers: Modifiers { alt: true, ..Modifiers::none() },
            },
        );
        assert!(state
            .active_sequence()
            .expect("selection only")
            .author_state_eq_ignoring_revision(&handle_edited));
        route_temporal(
            &mut editor,
            &mut router,
            &mut state,
            UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
        );
        let point_edited = state.active_sequence().expect("point edit").clone();
        assert!(!point_edited.author_state_eq_ignoring_revision(&handle_edited));
        assert!(state.undo_timeline().expect("Undo point"));
        assert!(state
            .active_sequence()
            .expect("Undo point state")
            .author_state_eq_ignoring_revision(&handle_edited));
        assert!(state.undo_timeline().expect("Undo handle"));
        assert!(state
            .active_sequence()
            .expect("Undo handle state")
            .author_state_eq_ignoring_revision(&before));
        assert!(!state.can_undo_action());
        assert!(state.redo_timeline().expect("Redo handle"));
        assert!(state.redo_timeline().expect("Redo point"));
        assert!(state
            .active_sequence()
            .expect("Redo state")
            .author_state_eq_ignoring_revision(&point_edited));
    }

    #[test]
    fn temporal_structural_edits_keep_following_keyboard_author_ids_and_two_undo_steps() {
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        for insertion in [true, false] {
            let (mut state, selection, model, _) = temporal_curve_fixture(true);
            let before = state.active_sequence().expect("before").clone();
            let mut editor = fixture_curve_editor(selection, &model);
            let mut router = EventRouter::new(editor.id());
            let plot = editor
                .as_any()
                .and_then(|any| any.downcast_ref::<CurveEditor>())
                .expect("curve")
                .plot_bounds();
            let point = if insertion {
                CurvePoint::new(0.25, 0.7)
            } else {
                model.keys[1].point
            };
            let position = Point::new(
                plot.x + point.x * plot.width,
                plot.y + ((1.0 - point.y) * f64::from(plot.height)) as f32,
            );
            for event in [
                UiEvent::MouseDown {
                    position,
                    button: MouseButton::Left,
                    modifiers: if insertion {
                        Modifiers::ctrl()
                    } else {
                        Modifiers::none()
                    },
                },
                UiEvent::MouseUp {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
            ] {
                route_temporal(&mut editor, &mut router, &mut state, event);
            }
            if !insertion {
                assert!(!state.can_undo_action(), "selection cannot author history");
                route_temporal(
                    &mut editor,
                    &mut router,
                    &mut state,
                    UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
                );
            }
            let CurvePointTarget::Keyframe(id) = editor
                .point_target(editor.selected_index().expect("selection"))
                .expect("target")
            else {
                panic!("selected canonical key");
            };
            if !insertion {
                assert_eq!(Some(id), model.keys[2].keyframe_id);
            }
            let property = |state: &AppState| {
                state
                    .active_sequence()
                    .expect("sequence")
                    .find_clip(selection.clip_id)
                    .expect("clip")
                    .intrinsic_parameter_bag()
                    .property_by_address(&model.property)
                    .expect("property")
                    .1
                    .clone()
            };
            let structural = property(&state);
            let original_key = structural.keyframe_by_id(id).expect("stable survivor");
            route_temporal(
                &mut editor,
                &mut router,
                &mut state,
                UiEvent::KeyDown {
                    key: if insertion {
                        KeyCode::Up
                    } else {
                        KeyCode::Down
                    },
                    modifiers: Modifiers::none(),
                },
            );
            let edited = property(&state);
            let changed_key = edited.keyframe_by_id(id).expect("same author identity");
            assert_eq!(
                changed_key.time, original_key.time,
                "value nudge preserves exact time"
            );
            assert_ne!(changed_key.value, original_key.value);
            for key in structural.channels()[0].keyframes().iter().filter(|key| key.id != id) {
                assert_eq!(
                    edited.keyframe_by_id(key.id).expect("untouched key"),
                    structural.keyframe_by_id(key.id).expect("before")
                );
            }
            assert!(state.undo_timeline().expect("undo keyboard nudge"));
            assert_eq!(property(&state), structural);
            assert!(state.undo_timeline().expect("undo structural edit"));
            assert_eq!(
                state
                    .active_sequence()
                    .expect("sequence")
                    .find_clip(selection.clip_id)
                    .expect("clip")
                    .intrinsic_parameter_bag(),
                before.find_clip(selection.clip_id).expect("before").intrinsic_parameter_bag()
            );
            assert!(!state.can_undo_action(), "exactly two history entries");
            assert!(state.redo_timeline().expect("redo structural edit"));
            assert!(state.redo_timeline().expect("redo keyboard nudge"));
            assert_eq!(property(&state), edited);
        }
    }

    #[test]
    fn temporal_virtual_boundary_return_to_origin_leaves_no_author_key_or_history() {
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        for move_away in [false, true] {
            let (mut state, selection, model, _) = temporal_curve_fixture(false);
            let before = state.active_sequence().expect("before").clone();
            assert!(model.keys.iter().all(|key| key.keyframe_id.is_none()));
            let mut editor = fixture_curve_editor(selection, &model);
            let plot = editor
                .as_any()
                .and_then(|any| any.downcast_ref::<CurveEditor>())
                .expect("curve")
                .plot_bounds();
            let mut router = EventRouter::new(editor.id());
            let point = model.keys[0].point;
            let origin = Point::new(
                plot.x + point.x * plot.width,
                plot.y + ((1.0 - point.y) * f64::from(plot.height)) as f32,
            );
            route_temporal(
                &mut editor,
                &mut router,
                &mut state,
                UiEvent::MouseDown {
                    position: origin,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
            );
            if move_away {
                route_temporal(
                    &mut editor,
                    &mut router,
                    &mut state,
                    UiEvent::MouseMove {
                        position: Point::new(origin.x, origin.y + 12.0),
                        modifiers: Modifiers::none(),
                    },
                );
                assert!(!state
                    .active_sequence()
                    .expect("sequence")
                    .find_clip(selection.clip_id)
                    .expect("clip")
                    .intrinsic_parameter_bag()
                    .property_by_address(&model.property)
                    .expect("property")
                    .1
                    .channels()[0]
                    .keyframes()
                    .is_empty());
                route_temporal(
                    &mut editor,
                    &mut router,
                    &mut state,
                    UiEvent::MouseMove { position: origin, modifiers: Modifiers::none() },
                );
            }
            route_temporal(
                &mut editor,
                &mut router,
                &mut state,
                UiEvent::MouseUp {
                    position: origin,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
            );
            assert_eq!(
                editor.point_target(0),
                Some(CurvePointTarget::VirtualBoundary(
                    mondrian_ui_widgets::CurveBoundary::Start
                ))
            );
            assert!(!editor.has_active_pointer_interaction());
            assert_eq!(
                state
                    .active_sequence()
                    .expect("sequence")
                    .find_clip(selection.clip_id)
                    .expect("clip")
                    .intrinsic_parameter_bag(),
                before.find_clip(selection.clip_id).expect("before").intrinsic_parameter_bag()
            );
            assert!(!state.can_undo_action());
        }
    }

    #[test]
    fn temporal_audio_group_pointer_refusal_return_cancel_and_saved_redo_use_one_history() {
        use crate::app_ui::audio_automation::{
            project_audio_automation, sequence_automation_viewport, AudioAutomationTimeContext,
        };
        use mondrian_core::ExactAutomationKeyframe;
        use mondrian_timeline::{
            apply_audio_automation_edit, inspect_audio_automation, AudioAutomationEdit,
            AudioAutomationEditRequest, AudioAutomationTarget, AudioChannelStripOwner,
        };
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        for interpolation in [
            InterpolationType::AutoBezier,
            InterpolationType::ContinuousBezier,
        ] {
            let mut sequence = mondrian_timeline::Sequence::new("Audio pointer group");
            let target = AudioAutomationTarget::ChannelFader {
                owner: AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id },
            };
            let mut ids = Vec::new();
            for (numerator, value) in [(1, -24.0), (2, -18.0), (4, -12.0)] {
                let keyframe = ExactAutomationKeyframe::linear(
                    TimelineTime::new(numerator, 5).expect("time"),
                    value,
                );
                ids.push(keyframe.id);
                apply_audio_automation_edit(
                    &mut sequence,
                    &AudioAutomationEditRequest {
                        target: target.clone(),
                        edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
                    },
                )
                .expect("seed");
            }
            apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest {
                    target: target.clone(),
                    edit: AudioAutomationEdit::SetInterpolations {
                        keyframe_ids: ids.clone(),
                        interpolation,
                    },
                },
            )
            .expect("constraints");
            let viewport = sequence_automation_viewport(&sequence).expect("viewport");
            let context =
                AudioAutomationTimeContext::at_sequence_time(&sequence, TimelineTime::ZERO);
            let model = project_audio_automation(&sequence, target.clone(), viewport, context)
                .expect("model");
            let original = sequence.clone();
            let mut state = AppState::new();
            state.test_set_sequence(Some(sequence));
            let history = state.authoring_history().expect("history").diagnostics();
            let localizer =
                Localizer::new(crate::app_ui::localization::AppUiLocale::EnUs).expect("locale");
            let mut widget = audio_automation_curve_control(&model, &localizer);
            widget.layout(Rect::new(0.0, 0.0, 600.0, 220.0));
            let editor = widget
                .as_any()
                .and_then(|any| any.downcast_ref::<CurveEditor>())
                .expect("editor");
            let positions = [
                editor.point_position(editor.points()[1]),
                editor.point_position(editor.points()[2]),
            ];
            let plot = editor.plot_bounds();
            let mut router = EventRouter::new(widget.id());
            for position in positions {
                for event in [
                    UiEvent::MouseDown {
                        position,
                        button: MouseButton::Left,
                        modifiers: Modifiers::shift(),
                    },
                    UiEvent::MouseUp {
                        position,
                        button: MouseButton::Left,
                        modifiers: Modifiers::shift(),
                    },
                ] {
                    route_temporal(widget.as_mut(), &mut router, &mut state, event);
                }
            }
            let start = positions[0];
            let accepted_point = Point::new(start.x + plot.width * 0.1, start.y - 5.0);
            let collision_point = Point::new(start.x + plot.width * 0.4, start.y - 5.0);
            let source = model.source_curve.clone();
            let mut accepted = None;
            for event in [
                UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                UiEvent::MouseMove {
                    position: accepted_point,
                    modifiers: Modifiers::none(),
                },
                UiEvent::MouseMove {
                    position: accepted_point,
                    modifiers: Modifiers::none(),
                },
                UiEvent::MouseMove {
                    position: collision_point,
                    modifiers: Modifiers::none(),
                },
            ] {
                route_temporal(widget.as_mut(), &mut router, &mut state, event);
                let current = state.active_sequence().expect("draft");
                let curve = inspect_audio_automation(current, &target)
                    .expect("curve")
                    .curve()
                    .expect("keys")
                    .clone();
                if router.captured().is_some() && curve != source {
                    if let Some(expected) = &accepted {
                        assert_eq!(
                            &curve, expected,
                            "repeat and rejected collision preserve last accepted joint draft"
                        );
                    } else {
                        accepted = Some(curve.clone());
                    }
                    let time_delta = curve.keyframes[0]
                        .time
                        .checked_sub(source.keyframes[0].time)
                        .expect("delta");
                    let value_delta = curve.keyframes[0].value - source.keyframes[0].value;
                    assert!(time_delta > TimelineTime::ZERO && value_delta > 0.0);
                    assert_eq!(
                        curve.keyframes[1]
                            .time
                            .checked_sub(source.keyframes[1].time)
                            .expect("second delta"),
                        time_delta
                    );
                    assert!(
                        (curve.keyframes[1].value - source.keyframes[1].value - value_delta).abs()
                            < 1e-9
                    );
                    assert_eq!(curve.keyframes[2].time, source.keyframes[2].time);
                    assert_eq!(curve.keyframes[2].value, source.keyframes[2].value);
                }
                let draft_history = state.authoring_history().expect("draft history").diagnostics();
                let pending_count = usize::from(curve != source);
                assert_eq!(draft_history.pending_entries, pending_count);
                assert_eq!(
                    draft_history.undo_entries,
                    history.undo_entries + pending_count
                );
                assert_eq!(draft_history.retained_bytes, history.retained_bytes);
                assert!(
                    draft_history.pending_retained_bytes <= draft_history.budget.max_pending_bytes
                );
                assert_eq!(
                    draft_history.branch_discarded_entries,
                    history.branch_discarded_entries
                );
                assert_eq!(
                    widget
                        .as_any()
                        .and_then(|any| any.downcast_ref::<CurveEditor>())
                        .expect("selection")
                        .selected_keyframe_ids(),
                    ids[..2]
                );
            }
            assert!(accepted.is_some(), "actual group moved");
            for event in [
                UiEvent::MouseMove { position: start, modifiers: Modifiers::none() },
                UiEvent::MouseUp {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
            ] {
                route_temporal(widget.as_mut(), &mut router, &mut state, event);
            }
            assert!(state
                .active_sequence()
                .expect("returned source")
                .author_state_eq_ignoring_revision(&original));
            assert_eq!(
                state.authoring_history().expect("no history").diagnostics(),
                history
            );
            assert!(router.captured().is_none());
            for event in [
                UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                UiEvent::MouseMove {
                    position: accepted_point,
                    modifiers: Modifiers::none(),
                },
                UiEvent::MouseUp {
                    position: accepted_point,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
            ] {
                route_temporal(widget.as_mut(), &mut router, &mut state, event);
            }
            let committed = state.active_sequence().expect("committed").clone();
            assert_eq!(
                inspect_audio_automation(&committed, &target).expect("committed target").curve(),
                accepted.as_ref()
            );
            assert_eq!(
                state.authoring_history().expect("one entry").diagnostics().undo_entries,
                history.undo_entries + 1
            );
            assert!(state.undo_timeline().expect("Undo"));
            assert!(state
                .active_sequence()
                .expect("undone")
                .author_state_eq_ignoring_revision(&original));
            let undo_history = state.authoring_history().expect("live Redo").diagnostics();
            let directory = tempfile::tempdir().expect("archives");
            let undone_path = directory.path().join("音频组撤销🙂.mdp");
            state.save_project_file_as(undone_path.clone()).expect("save Undo");
            assert_eq!(
                state.authoring_history().expect("saved live Redo").diagnostics(),
                undo_history
            );
            assert!(state.redo_timeline().expect("Redo"));
            assert!(state
                .active_sequence()
                .expect("redone")
                .author_state_eq_ignoring_revision(&committed));
            let committed_path = directory.path().join("音频组重做🙂.mdp");
            state.save_project_file_as(committed_path.clone()).expect("save Redo");
            drop(widget);
            drop(state);
            for (path, expected) in [(undone_path, original), (committed_path, committed)] {
                let mut reopened = AppState::new();
                reopened.open_project_file(path).expect("fresh reopen");
                assert!(reopened
                    .active_sequence()
                    .expect("archive author state")
                    .author_state_eq_ignoring_revision(&expected));
                assert!(!reopened.can_undo_action() && !reopened.can_redo_action());
            }
        }
    }

    #[test]
    fn temporal_audio_selection_uses_owner_context_is_one_undo_and_survives_reopen() {
        use crate::app_ui::audio_automation::{
            component_automation_viewport, processing_scope_automation_viewport,
            project_audio_automation, AudioAutomationTimeContext,
        };
        use mondrian_core::{
            AssetId, AudioSourceComponentId, AuthoringTimeDomain, DomainTime,
            ExactAutomationKeyframe,
        };
        use mondrian_timeline::{
            apply_audio_automation_edit, inspect_audio_automation, AudioAutomationEdit,
            AudioAutomationEditRequest, AudioAutomationTarget, AudioComponentAddress,
        };
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        for scope in [false, true] {
            let mut sequence = mondrian_timeline::Sequence::new("Audio selection acceptance");
            let track_id = sequence.audio_tracks[0].id;
            let clip = Clip::new(
                AssetId::new(),
                TimelineTime::new(10, 1).expect("placement"),
                TimelineTime::ONE,
            )
            .expect("clip");
            sequence
                .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
                .expect("audio clip");
            let clip = &mut sequence.audio_tracks[0].clips[0];
            let edit = &mut clip.audio_components[0];
            edit.local_time_in = TimelineTime::new(2, 1).expect("component origin");
            edit.processing.scope_in = TimelineTime::new(7, 1).expect("Scope origin");
            let address = AudioComponentAddress { track_id, clip_id: clip.id, edit_id: edit.id };
            let scope_id = edit.processing.scope_id;
            let target = if scope {
                AudioAutomationTarget::ProcessingScopeInputGain { scope_id }
            } else {
                AudioAutomationTarget::ComponentVolume {
                    track_id,
                    clip_id: clip.id,
                    edit_id: edit.id,
                }
            };
            let origin = if scope {
                edit.processing.scope_in
            } else {
                edit.local_time_in
            };
            let mut ids = Vec::new();
            for (offset, value) in [
                (TimelineTime::ZERO, -24.0),
                (TimelineTime::new(1, 2).expect("middle"), -6.0),
                (TimelineTime::ONE, -18.0),
            ] {
                let keyframe = ExactAutomationKeyframe::linear(
                    origin.checked_add(offset).expect("author time"),
                    value,
                );
                ids.push(keyframe.id);
                apply_audio_automation_edit(
                    &mut sequence,
                    &AudioAutomationEditRequest {
                        target: target.clone(),
                        edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
                    },
                )
                .expect("canonical seed");
            }
            let context = AudioAutomationTimeContext {
                sequence_time: DomainTime::new(
                    AuthoringTimeDomain::Sequence(sequence.id),
                    TimelineTime::new(41, 4).expect("live time"),
                ),
                scope_occurrence: scope.then_some(address),
            };
            let viewport = if scope {
                processing_scope_automation_viewport(&sequence, scope_id).expect("Scope viewport")
            } else {
                component_automation_viewport(address.edit_id, origin, TimelineTime::ONE)
                    .expect("component viewport")
            };
            let model = project_audio_automation(&sequence, target.clone(), viewport, context)
                .expect("model");
            let author_time = origin
                .checked_add(TimelineTime::new(1, 4).expect("offset"))
                .expect("exact current time");
            assert_eq!(
                model.current_author_time.as_ref().expect("current owner").time,
                author_time
            );
            let retained = model.source_curve.evaluate(author_time).expect("original sample");
            let original = sequence.clone();
            let mut state = AppState::new();
            state.test_set_sequence(Some(sequence));
            let localizer =
                Localizer::new(crate::app_ui::localization::AppUiLocale::EnUs).expect("locale");
            let mut widget = audio_automation_curve_control(&model, &localizer);
            widget.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
            let mut router = EventRouter::new(widget.id());
            for event in [
                UiEvent::KeyDown { key: KeyCode::Tab, modifiers: Modifiers::none() },
                UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            ] {
                route_temporal(widget.as_mut(), &mut router, &mut state, event);
            }
            assert_eq!(
                widget
                    .as_any()
                    .and_then(|any| any.downcast_ref::<CurveEditor>())
                    .expect("curve")
                    .selected_keyframe_ids(),
                ids
            );
            assert!(!state.can_undo_action());
            route_temporal(
                widget.as_mut(),
                &mut router,
                &mut state,
                UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
            );
            let accepted = state.active_sequence().expect("accepted").clone();
            let inspection = inspect_audio_automation(&accepted, &target).expect("inspection");
            assert!(inspection.curve().is_none());
            assert_eq!(inspection.default_value(), retained);
            let editor = widget
                .as_any()
                .and_then(|any| any.downcast_ref::<CurveEditor>())
                .expect("curve");
            assert_eq!(editor.points().len(), 2);
            assert!(editor.selected_keyframe_ids().is_empty());
            assert!(state.undo_timeline().expect("one Undo"));
            assert!(state
                .active_sequence()
                .expect("undone")
                .author_state_eq_ignoring_revision(&original));
            assert!(!state.can_undo_action());
            assert!(state.redo_timeline().expect("one Redo"));
            assert!(state
                .active_sequence()
                .expect("redone")
                .author_state_eq_ignoring_revision(&accepted));
            let directory = tempfile::tempdir().expect("archives");
            let path = directory.path().join("音频整组删除🙂.mdp");
            state.save_project_file_as(path.clone()).expect("save audio selection");
            let mut reopened = AppState::new();
            reopened.open_project_file(path).expect("fresh reopen");
            assert!(reopened
                .active_sequence()
                .expect("reopened")
                .author_state_eq_ignoring_revision(&accepted));
            assert!(!reopened.can_undo_action() && !reopened.can_redo_action());
            drop(reopened);
        }
    }

    #[test]
    fn temporal_audio_boundary_preview_matches_owner_and_retains_history() {
        use crate::app_ui::audio_automation::{project_audio_automation, AudioAutomationViewport};
        use mondrian_core::{
            AuthoringTimeDomain, ExactAutomationKeyframe, ExactAutomationTangentMode,
        };
        use mondrian_timeline::{
            apply_audio_automation_edit, AudioAutomationEdit, AudioAutomationEditRequest,
            AudioAutomationTarget, AudioChannelStripOwner,
        };
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        for mode in [
            ExactAutomationTangentMode::Auto,
            ExactAutomationTangentMode::Continuous,
        ] {
            for at_end in [false, true] {
                let mut sequence = mondrian_timeline::Sequence::new("Audio boundary authoring");
                let target = AudioAutomationTarget::ChannelFader {
                    owner: AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id },
                };
                let viewport = AudioAutomationViewport {
                    domain: AuthoringTimeDomain::Sequence(sequence.id),
                    range: TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE)
                        .expect("range"),
                };
                let soft = project_audio_automation(
                    &sequence,
                    target.clone(),
                    viewport,
                    crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                        &sequence,
                        sequence.playhead,
                    ),
                )
                .expect("default axis")
                .value_axis
                .soft_range();
                let middle_value = soft.min * 0.5 + soft.max * 0.5;
                let mut middle_id = None;
                for (index, time) in [
                    TimelineTime::new(1, 4).expect("first"),
                    TimelineTime::new(1, 2).expect("middle"),
                    TimelineTime::new(3, 4).expect("last"),
                ]
                .into_iter()
                .enumerate()
                {
                    let mut keyframe =
                        ExactAutomationKeyframe::linear(time, middle_value + index as f64 - 1.0);
                    keyframe.interpolation_to_next =
                        mondrian_core::AutomationSegmentInterpolation::Bezier;
                    if index == 1 {
                        middle_id = Some(keyframe.id);
                        keyframe.tangent_mode = mode;
                        keyframe.in_handle = Some(mondrian_core::ExactBezierHandle {
                            time_offset: TimelineTime::new(-1, 12).expect("in"),
                            value_offset: -0.5,
                        });
                        keyframe.out_handle = Some(mondrian_core::ExactBezierHandle {
                            time_offset: TimelineTime::new(1, 12).expect("out"),
                            value_offset: 0.5,
                        });
                    }
                    apply_audio_automation_edit(
                        &mut sequence,
                        &AudioAutomationEditRequest {
                            target: target.clone(),
                            edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
                        },
                    )
                    .expect("seed");
                }
                let id = middle_id.expect("middle identity");
                let original = sequence.clone();
                let model = project_audio_automation(
                    &sequence,
                    target.clone(),
                    viewport,
                    crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                        &sequence,
                        sequence.playhead,
                    ),
                )
                .expect("model");
                let mut state = AppState::new();
                state.test_set_sequence(Some(sequence));
                let localizer =
                    Localizer::new(crate::app_ui::localization::AppUiLocale::EnUs).expect("locale");
                let mut widget = audio_automation_curve_control(&model, &localizer);
                widget.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
                let plot = widget
                    .as_any()
                    .and_then(|any| any.downcast_ref::<CurveEditor>())
                    .expect("curve")
                    .plot_bounds();
                let mut router = EventRouter::new(widget.id());
                let center = Point::new(plot.x + plot.width * 0.5, plot.y + plot.height * 0.5);
                route_temporal(
                    widget.as_mut(),
                    &mut router,
                    &mut state,
                    UiEvent::MouseDown {
                        position: center,
                        button: MouseButton::Left,
                        modifiers: Modifiers::none(),
                    },
                );
                let boundary = Point::new(
                    plot.x + if at_end { plot.width } else { 0.0 },
                    plot.y + plot.height * 0.5,
                );
                for position in [boundary, center, boundary] {
                    route_temporal(
                        widget.as_mut(),
                        &mut router,
                        &mut state,
                        UiEvent::MouseMove { position, modifiers: Modifiers::none() },
                    );
                    let projected = project_audio_automation(
                        state.active_sequence().expect("draft"),
                        target.clone(),
                        viewport, crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(state.active_sequence().expect("draft"), state.active_sequence().expect("draft").playhead))
                    .expect("canonical projection");
                    let curve = widget
                        .as_any()
                        .and_then(|any| any.downcast_ref::<CurveEditor>())
                        .expect("widget");
                    assert_eq!(curve.points(), projected.points());
                    let displayed = (0..curve.points().len())
                        .filter_map(|index| match curve.point_target(index) {
                            Some(CurvePointTarget::Keyframe(id)) => Some(id),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(
                        displayed,
                        projected
                            .keys
                            .iter()
                            .filter_map(|key| key.keyframe.as_ref().map(|key| key.id))
                            .collect::<Vec<_>>()
                    );
                    assert_eq!(
                        curve.point_target(curve.selected_index().expect("selection")),
                        Some(CurvePointTarget::Keyframe(id))
                    );
                    assert!(widget.has_active_pointer_interaction());
                }
                route_temporal(
                    widget.as_mut(),
                    &mut router,
                    &mut state,
                    UiEvent::MouseUp {
                        position: boundary,
                        button: MouseButton::Left,
                        modifiers: Modifiers::none(),
                    },
                );
                let committed = state.active_sequence().expect("committed").clone();
                assert!(state.undo_timeline().expect("single Undo"));
                assert!(state
                    .active_sequence()
                    .expect("Undo")
                    .author_state_eq_ignoring_revision(&original));
                assert!(!state.can_undo_action());
                assert!(state.redo_timeline().expect("Redo"));
                assert!(state
                    .active_sequence()
                    .expect("Redo")
                    .author_state_eq_ignoring_revision(&committed));
            }
        }
    }

    #[test]
    fn sidechain_gain_panel_gesture_cancel_commit_and_keyboard_history() {
        use crate::app_ui::audio_automation::AudioAutomationTimeContext;
        use crate::app_ui::audio_processor_rack::project_audio_processor_rack;
        use mondrian_timeline::audio::{
            AudioChannelStripOutputPort, AudioProcessorInstance, AudioProcessorSidechainRoute,
            AudioRouteSource, BUILTIN_GAIN_DEFINITION_ID,
        };
        use mondrian_timeline::{
            AudioChannelStripOwner, AudioChannelStripRack, AudioProcessorRackAddress,
        };
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        for processing_scope in [false, true] {
            for locale in [
                crate::app_ui::localization::AppUiLocale::EnUs,
                crate::app_ui::localization::AppUiLocale::ZhCn,
                crate::app_ui::localization::AppUiLocale::Pseudo,
            ] {
                let localizer = Localizer::new(locale).expect("catalog");
                let mut sequence = mondrian_timeline::Sequence::new("侧链曲线 🙂");
                let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
                let processor_id = processor.id;
                let mut time_context =
                    AudioAutomationTimeContext::at_sequence_time(&sequence, TimelineTime::ZERO);
                let (address, source_track_id) = if processing_scope {
                    let main_track_id = sequence.audio_tracks[0].id;
                    sequence
                        .add_media_audio_clip(
                            main_track_id,
                            mondrian_timeline::Clip::new(
                                mondrian_core::AssetId::new(),
                                TimelineTime::ZERO,
                                TimelineTime::ONE,
                            )
                            .expect("Scope clip"),
                            mondrian_core::AudioSourceComponentId::primary(),
                        )
                        .expect("Scope occurrence");
                    let clip = &mut sequence.audio_tracks[0].clips[0];
                    let edit = &mut clip.audio_components[0];
                    edit.processing.scope_in = TimelineTime::new(5, 1).expect("Scope offset");
                    let scope_id = edit.processing.scope_id;
                    time_context.scope_occurrence =
                        Some(mondrian_timeline::AudioComponentAddress {
                            track_id: main_track_id,
                            clip_id: clip.id,
                            edit_id: edit.id,
                        });
                    sequence
                        .audio_program
                        .processing_scopes
                        .iter_mut()
                        .find(|scope| scope.id == scope_id)
                        .expect("Scope")
                        .processors
                        .processors
                        .push(processor);
                    let detector_id = sequence.add_audio_track();
                    (
                        AudioProcessorRackAddress::ProcessingScope { scope_id },
                        detector_id,
                    )
                } else {
                    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(processor);
                    (
                        AudioProcessorRackAddress::ChannelStrip {
                            owner: AudioChannelStripOwner::ProgramOutput {
                                output_id: sequence.audio_program.outputs[0].id,
                            },
                            rack: AudioChannelStripRack::PreFader,
                        },
                        sequence.audio_tracks[0].id,
                    )
                };
                let route = AudioProcessorSidechainRoute::new(
                    AudioRouteSource::Track {
                        track_id: source_track_id,
                        port: AudioChannelStripOutputPort::PostMute,
                    },
                    processor_id,
                    "offline.detector",
                );
                let route_id = route.id;
                sequence.audio_program.sidechain_routes.push(route);
                mondrian_timeline::inspect_audio_route_candidates(&sequence)
                    .expect("causal author graph");
                let rack =
                    project_audio_processor_rack(&sequence, address, &localizer, time_context);
                let mut panel = with_audio_processor_rack_sections(
                    PropertyPanel::new("Rack"),
                    &[rack],
                    true,
                    &localizer,
                );
                let index = (0..panel.child_count())
                    .rev()
                    .find(|index| {
                        panel
                            .child(*index)
                            .and_then(Widget::as_any)
                            .is_some_and(|any| any.is::<CurveEditor>())
                    })
                    .expect("sidechain gain curve exposed in actual panel");
                let editor = panel.child_mut(index).expect("curve widget");
                editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
                let plot = editor
                    .as_any()
                    .expect("type")
                    .downcast_ref::<CurveEditor>()
                    .expect("curve")
                    .plot_bounds();
                let position = Point::new(plot.x + plot.width * 0.25, plot.y + plot.height * 0.5);
                let original = sequence.clone();
                let mut state = AppState::new();
                state.test_set_sequence(Some(sequence));
                let mut router = EventRouter::new(editor.id());
                let down = UiEvent::MouseDown {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::ctrl(),
                };
                route_temporal(editor, &mut router, &mut state, down.clone());
                route_temporal(
                    editor,
                    &mut router,
                    &mut state,
                    UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
                );
                assert!(state
                    .active_sequence()
                    .expect("cancelled")
                    .author_state_eq_ignoring_revision(&original));
                assert!(!state.can_undo_action());
                route_temporal(editor, &mut router, &mut state, down);
                route_temporal(
                    editor,
                    &mut router,
                    &mut state,
                    UiEvent::MouseUp {
                        position,
                        button: MouseButton::Left,
                        modifiers: Modifiers::none(),
                    },
                );
                let committed = state.active_sequence().expect("commit").clone();
                let route = committed
                    .audio_program
                    .sidechain_routes
                    .iter()
                    .find(|route| route.id == route_id)
                    .expect("retained route");
                let key = route
                    .gain_automation
                    .as_ref()
                    .expect("route gain curve")
                    .keyframes
                    .iter()
                    .find(|key| key.time == TimelineTime::new(1, 4).expect("exact time"))
                    .expect("inserted key");
                let key_id = key.id;
                route_temporal(
                    editor,
                    &mut router,
                    &mut state,
                    UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
                );
                let edited = state.active_sequence().expect("nudge").clone();
                let edited_key = edited.audio_program.sidechain_routes[0]
                    .gain_automation
                    .as_ref()
                    .expect("curve")
                    .keyframes
                    .iter()
                    .find(|key| key.id == key_id)
                    .expect("stable ID");
                assert_eq!(edited_key.time, key.time);
                assert_ne!(edited_key.value, key.value);
                assert!(state.undo_timeline().expect("undo nudge"));
                assert!(state
                    .active_sequence()
                    .expect("restored")
                    .author_state_eq_ignoring_revision(&committed));
                assert!(state.undo_timeline().expect("undo insert"));
                assert!(state
                    .active_sequence()
                    .expect("original")
                    .author_state_eq_ignoring_revision(&original));
                assert!(!state.can_undo_action());
                assert!(state.redo_timeline().expect("redo insert"));
                assert!(state.redo_timeline().expect("redo nudge"));
                assert!(state
                    .active_sequence()
                    .expect("edited")
                    .author_state_eq_ignoring_revision(&edited));
            }
        }
    }

    #[test]
    fn temporal_audio_insert_then_keyboard_edit_retains_id_and_reversible_history() {
        use crate::app_ui::audio_automation::{project_audio_automation, AudioAutomationViewport};
        use mondrian_core::{AuthoringTimeDomain, ExactAutomationKeyframe};
        use mondrian_timeline::{
            apply_audio_automation_edit, inspect_audio_automation, AudioAutomationEdit,
            AudioAutomationEditRequest, AudioAutomationTarget, AudioChannelStripOwner,
        };
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let mut sequence = mondrian_timeline::Sequence::new("Audio structural edits");
        let target = AudioAutomationTarget::ChannelFader {
            owner: AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id },
        };
        for (time, value) in [
            (TimelineTime::ZERO, -15.0),
            (TimelineTime::new(1, 2).expect("middle"), -12.0),
            (TimelineTime::ONE, -9.0),
        ] {
            apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest {
                    target: target.clone(),
                    edit: AudioAutomationEdit::UpsertKeyframe {
                        keyframe: ExactAutomationKeyframe::linear(time, value),
                    },
                },
            )
            .expect("seed author curve");
        }
        let viewport = AudioAutomationViewport {
            domain: AuthoringTimeDomain::Sequence(sequence.id),
            range: TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE).expect("range"),
        };
        let model = project_audio_automation(
            &sequence,
            target.clone(),
            viewport,
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
        )
        .expect("model");
        let before = inspect_audio_automation(&sequence, &target)
            .expect("before")
            .curve()
            .expect("curve")
            .clone();
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        let localizer =
            Localizer::new(crate::app_ui::localization::AppUiLocale::EnUs).expect("locale");
        let mut editor = audio_automation_curve_control(&model, &localizer);
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        let plot = editor
            .as_any()
            .and_then(|any| any.downcast_ref::<CurveEditor>())
            .expect("curve")
            .plot_bounds();
        let mut router = EventRouter::new(editor.id());
        let position = Point::new(plot.x + plot.width * 0.25, plot.y + plot.height * 0.5);
        for event in [
            UiEvent::MouseDown {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::ctrl(),
            },
            UiEvent::MouseUp {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
        ] {
            route_temporal(editor.as_mut(), &mut router, &mut state, event);
        }
        let curve = |state: &AppState| {
            inspect_audio_automation(state.active_sequence().expect("sequence"), &target)
                .expect("inspect")
                .curve()
                .expect("curve")
                .clone()
        };
        let structural = curve(&state);
        let inserted = structural
            .keyframes
            .iter()
            .find(|key| key.time == TimelineTime::new(1, 4).expect("insert"))
            .expect("canonical insertion");
        let widget = editor
            .as_any()
            .expect("widget type")
            .downcast_ref::<CurveEditor>()
            .expect("curve editor");
        assert_eq!(
            widget.point_target(1),
            Some(CurvePointTarget::Keyframe(inserted.id))
        );
        route_temporal(
            editor.as_mut(),
            &mut router,
            &mut state,
            UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
        );
        let edited = curve(&state);
        let changed = edited
            .keyframes
            .iter()
            .find(|key| key.id == inserted.id)
            .expect("same new identity");
        assert_eq!(changed.time, inserted.time);
        assert_ne!(changed.value, inserted.value);
        for key in structural.keyframes.iter().filter(|key| key.id != inserted.id) {
            assert_eq!(
                edited.keyframes.iter().find(|candidate| candidate.id == key.id),
                Some(key)
            );
        }
        assert!(state.undo_timeline().expect("undo nudge"));
        assert_eq!(curve(&state), structural);
        assert!(state.undo_timeline().expect("undo insertion"));
        assert_eq!(curve(&state), before);
        assert!(!state.can_undo_action());
        assert!(state.redo_timeline().expect("redo insertion"));
        assert!(state.redo_timeline().expect("redo nudge"));
        assert_eq!(curve(&state), edited);
    }

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
        let model = opacity_curve_model_for_clip(
            &clip,
            mondrian_core::Rational::new(1, 30),
            clip.clip_time_in,
        )
        .expect("model");
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
        assert_eq!(Some(controls[0].keyframe_id), model.keys[1].keyframe_id);
        assert_eq!(controls[0].side, BezierHandleSide::Outgoing);
        assert_eq!(controls[0].point.x, 2.0 / 3.0);
        let before = clip.clone();
        clip.clip_time_in = TimelineTime::new(3, 10).expect("inner in");
        clip.duration = TimelineTime::new(2, 5).expect("inner span");
        let model = opacity_curve_model_for_clip(
            &clip,
            mondrian_core::Rational::new(1, 30),
            clip.clip_time_in,
        )
        .expect("inner model");
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
        assert_eq!(Some(controls[0].keyframe_id), model.keys[1].keyframe_id);
        assert_eq!(
            clip.transform, before.transform,
            "viewport changes preserve all outside author keys"
        );
    }

    #[test]
    fn temporal_value_projection_retains_legal_values_beyond_soft_range() {
        use mondrian_core::automation::{
            AnimatedProperty, ParameterInvalidValuePolicy, ParameterNumericContract,
            ParameterNumericRange, ParameterUnit, PropertyDescriptor,
        };
        let mut contract =
            ParameterNumericContract::closed(-2.0, 2.0, None, ParameterInvalidValuePolicy::Reject)
                .expect("hard range");
        contract.soft_range = ParameterNumericRange::new(0.0, 1.0).expect("soft display");
        let mut property = AnimatedProperty::from_descriptor(
            PropertyDescriptor::new("test.temporal.author", "Value", PropertyValue::Double(1.5))
                .with_animatable(true)
                .with_numeric_contract(ParameterUnit::Unitless, contract),
        );
        property.set_animation_enabled(true);
        let clip = mondrian_timeline::Clip::new_basic_title(
            "Curve",
            "Arial",
            TimelineTime::ZERO,
            TimelineTime::ONE,
        )
        .expect("owner");
        let model = numeric_curve_model_for_property(
            &property,
            &clip,
            mondrian_core::Rational::new(1, 30),
            clip.clip_time_in,
        )
        .expect("real model");
        assert_eq!(
            model.keys[0].point.y, 1.5,
            "legal author value must not be clamped to the soft display boundary"
        );
        assert!(
            model.display_points.iter().all(|point| point.y == 1.5),
            "canonical samples retain the actual author value"
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
            let visual = numeric_curve_model_for_property(
                &property,
                &clip,
                mondrian_core::Rational::new(1, 30),
                clip.clip_time_in,
            )
            .expect("actual visual model");
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
                crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                    &sequence,
                    sequence.playhead,
                ),
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
            let model = project_audio_automation(
                &sequence,
                target.clone(),
                viewport,
                crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                    &sequence,
                    sequence.playhead,
                ),
            )
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
                    tangent.keyframe_id == key_id && tangent.side == BezierHandleSide::Outgoing
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
            let plot = widget
                .as_any()
                .and_then(|any| any.downcast_ref::<CurveEditor>())
                .expect("curve")
                .plot_bounds();
            let screen = |point: CurvePoint| {
                Point::new(
                    plot.x + plot.width * point.x,
                    (f64::from(plot.y)
                        + f64::from(plot.height)
                            * (1.0
                                - model
                                    .value_axis
                                    .soft_range()
                                    .normalized_position(point.y)
                                    .expect("initial audio view coordinate")))
                        as f32,
                )
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
            let (mut state, selection, model, key_id) = temporal_curve_fixture(true);
            let clip_id = selection.clip_id;
            let before = state.active_sequence().expect("before").clone();
            let index =
                model.keys.iter().position(|key| key.keyframe_id == Some(key_id)).expect("key");
            let tangent = numeric_tangents(&model, &model.curve_source)
                .into_iter()
                .find(|tangent| {
                    Some(tangent.keyframe_id) == model.keys[index].keyframe_id
                        && tangent.side == BezierHandleSide::Outgoing
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
            let plot = editor
                .as_any()
                .and_then(|any| any.downcast_ref::<CurveEditor>())
                .expect("curve")
                .plot_bounds();
            let screen = |point: CurvePoint| {
                Point::new(
                    plot.x + point.x * plot.width,
                    plot.y + ((1.0 - point.y) * f64::from(plot.height)) as f32,
                )
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


#[cfg(test)]
mod displayed_native_parameter_control_contracts {
    use super::*;
    use super::super::tests::owned_panel_status_painted_text;
    use crate::app::parameter_gesture::ParameterGestureAction;
    use crate::app_ui::audio_processor_rack::{
        bind_audio_scalar_author_lease, project_audio_processor_rack, static_parameter_target,
    };

    #[test]
    fn native_owned_control_adopts_only_the_shown_canonical_snapshot_then_uses_old_slider() {
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let (_directory, mut state, edit) =
            crate::app::AppState::displayed_native_parameter_fixture_for_test(false, 0.375, true);
        let localizer = Localizer::new(crate::app_ui::localization::AppUiLocale::ZhCn).unwrap();
        let project = |state: &crate::app::AppState| {
            let sequence = state.active_sequence().unwrap();
            let mut rack = project_audio_processor_rack(
                sequence,
                edit.target.owner.processor_address().unwrap(),
                &localizer,
                crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                    sequence,
                    mondrian_core::TimelineTime::ZERO,
                ),
            );
            bind_audio_scalar_author_lease(std::slice::from_mut(&mut rack), state);
            rack
        };
        let (_missing_directory, missing_state, missing_edit) =
            crate::app::AppState::displayed_native_parameter_fixture_for_test(false, 0.375, false);
        let missing_sequence = missing_state.active_sequence().unwrap();
        let mut without_fact = project_audio_processor_rack(
            missing_sequence,
            missing_edit.target.owner.processor_address().unwrap(),
            &localizer,
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                missing_sequence,
                mondrian_core::TimelineTime::ZERO,
            ),
        );
        bind_audio_scalar_author_lease(std::slice::from_mut(&mut without_fact), &missing_state);
        assert!(without_fact.processors[0].parameters[0].static_value.is_none());
        assert!(without_fact.processors[0].parameters[0].native_presentation.is_none());
        assert!(without_fact.processors[0].parameters[0].native_adoption.is_none());

        let before = state.active_sequence().unwrap().clone();
        let generation = state.project_author_generation();
        let history = state.authoring_history().unwrap().diagnostics();
        let rack = project(&state);
        let processor = &rack.processors[0];
        let parameter = &processor.parameters[0];
        assert!(parameter.static_value.is_none());
        assert!(!parameter.is_static_editable());
        assert!(static_parameter_target(&rack, processor, parameter).is_none());
        let fact = parameter.native_presentation.as_ref().unwrap();
        assert!(fact.value_bits.is_none() && fact.display.is_none());
        assert_eq!(fact.author_value_bits, Some(0.375_f64.to_bits()));
        let numeric = parameter.schema.numeric.as_ref().unwrap()[0];

        let mut missing = parameter.clone();
        missing.native_presentation = None;
        missing.native_adoption = None;
        let mut unavailable = audio_processor_static_parameter_control(
            &rack, processor, &missing, numeric, true, &localizer,
        );
        assert_eq!(
            owned_panel_status_painted_text(unavailable.as_mut()),
            vec![localizer.text("audio-rack-native-controlled-parameter")]
        );
        assert_eq!(unavailable.child_count(), 0);
        assert!(!unavailable.can_focus() && unavailable.control_inspection_state().is_none());

        let mut control = audio_processor_static_parameter_control(
            &rack, processor, parameter, numeric, true, &localizer,
        );
        assert_eq!(control.child_count(), 2);
        let painted = owned_panel_status_painted_text(control.as_mut());
        let label = painted.iter().find(|text|
            text.contains(&localizer.text("audio-rack-native-controlled-parameter")))
            .expect("actual painted native-owned label");
        assert!(
            label.contains("0.38"),
            "canonical schema-formatted fallback"
        );
        assert!(painted.iter().any(|text| text == &localizer.text("audio-rack-adopt-displayed-value")));
        let button = control.child(1).unwrap();
        assert_eq!(button.control_inspection_state().unwrap().role,
            mondrian_ui_core::AccessibilityRole::Button);
        assert!(button.can_focus());
        let mut disabled = audio_processor_static_parameter_control(
            &rack, processor, parameter, numeric, false, &localizer,
        );
        assert_eq!(disabled.child_count(), 0);
        assert!(!disabled.can_focus() && disabled.control_inspection_state().is_none());
        let disabled_paint = owned_panel_status_painted_text(disabled.as_mut());
        assert_eq!(disabled_paint, vec![label.clone()]);
        assert!(state.active_sequence().unwrap().author_state_eq_ignoring_revision(&before));
        assert_eq!(state.project_author_generation(), generation);
        assert_eq!(state.authoring_history().unwrap().diagnostics(), history);

        let snapshot = parameter.native_adoption.clone().unwrap();
        assert_eq!(snapshot.expected.author_value_bits, 0.375_f64.to_bits());
        state
            .dispatch_action(
                ParameterGestureAction::AdoptDisplayedNativeParameter { edit: snapshot }.action(),
            )
            .unwrap();
        let adopted = project(&state);
        let processor = &adopted.processors[0];
        let parameter = &processor.parameters[0];
        assert_eq!(parameter.static_value, Some(0.375));
        assert!(parameter.native_adoption.is_none());
        assert!(parameter.is_static_editable());
        assert!(static_parameter_target(&adopted, processor, parameter).is_some());
    }
}

#[cfg(test)]
mod explicit_native_parameter_query_controls {
    use super::*;
    use crate::app::product_action::{AudioNativeParameterQueryPayload, AudioProductAction};
    use crate::app_ui::audio_processor_rack::{
        bind_audio_scalar_author_lease, project_audio_processor_rack,
    };
    use crate::app_ui::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_ui_core::widget::EventRequests;
    use mondrian_ui_core::{KeyCode, Modifiers, UiEvent};
    use std::cell::RefCell;

    #[test]
    fn native_owned_no_fact_query_button_is_manual_and_some_keeps_original_control() {
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let (_directory, state, edit) =
            crate::app::AppState::displayed_native_parameter_fixture_for_test(false, 0.375, false);
        let sequence = state.active_sequence().unwrap();
        let localizer = Localizer::new(crate::app_ui::localization::AppUiLocale::EnUs).unwrap();
        let mut rack = project_audio_processor_rack(
            sequence,
            edit.target.owner.processor_address().unwrap(),
            &localizer,
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                sequence,
                mondrian_core::TimelineTime::ZERO,
            ),
        );
        bind_audio_scalar_author_lease(std::slice::from_mut(&mut rack), &state);
        let processor = &rack.processors[0];
        let mut parameter = processor.parameters[0].clone();
        assert!(parameter.static_value.is_none() && parameter.native_presentation.is_none());
        // Widget contract only: App's independent controls qualify the actual
        // VST3/Opened/Source admission; this DTO models a prepared row action.
        let payload = AudioNativeParameterQueryPayload {
            target: edit.target,
            expected_author_generation: state.project_author_generation(),
        };
        parameter.native_query = Some(payload.clone());
        let numeric = parameter.schema.numeric.as_ref().unwrap()[0];
        let before = serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
        let mut control = audio_processor_static_parameter_control(
            &rack, processor, &parameter, numeric, true, &localizer,
        );
        assert_eq!(control.child_count(), 2);
        assert!(
            super::super::tests::owned_panel_status_painted_text(control.as_mut())
                .contains(&localizer.text("audio-rack-query-native-parameter"))
        );
        let actions = RefCell::new(Vec::new());
        assert!(
            actions.borrow().is_empty(),
            "building a row never runs its query"
        );
        let mut requests = EventRequests::default();
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut context = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );
        let button = control.child_mut(1).unwrap();
        assert!(button.can_focus());
        assert_eq!(
            button.control_inspection_state().unwrap().role,
            mondrian_ui_core::AccessibilityRole::Button
        );
        button.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut context,
        );
        let actions = actions.into_inner();
        assert_eq!(actions.len(), 1);
        assert_eq!(
            ProductAction::decode_external(&actions[0]).unwrap(),
            Some(ProductAction::Audio(
                AudioProductAction::QueryNativeParameter(payload)
            ))
        );
        let mut disabled = audio_processor_static_parameter_control(
            &rack, processor, &parameter, numeric, false, &localizer,
        );
        assert_eq!(disabled.child_count(), 0);
        parameter.static_value = Some(0.25);
        let mut owned = audio_processor_static_parameter_control(
            &rack, processor, &parameter, numeric, true, &localizer,
        );
        assert!(
            !super::super::tests::owned_panel_status_painted_text(owned.as_mut())
                .contains(&localizer.text("audio-rack-query-native-parameter"))
        );
        assert_eq!(
            serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap(),
            before
        );
    }
}
