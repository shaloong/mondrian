//! All published official CLAP records, constants and callback carriers.
//! Native report must be built independently from the unchanged official headers.
//! This test qualifies ABI representation, not host service or vendor semantics.

use clap_sys::raw;
use std::mem::{align_of, offset_of, size_of};

#[test]
#[ignore = "requires independent native official CLAP 1.2.10 ABI report"]
fn clap_latest_full_abi_matches_native_headers() {
    let path = std::env::var_os("MONDRIAN_CLAP_LATEST_ABI_REPORT")
        .expect("set independent native report path");
    let bytes = std::fs::read(path).expect("read independent native report");
    assert!(bytes.len() <= 262_144, "unexpected native report extent");
    let report: serde_json::Value = serde_json::from_slice(&bytes).expect("parse native report");
    assert_eq!(report["schema"], 2);
    assert_eq!(
        report["official_commit"],
        "195b42a004144fab0b3cf95e9c067187d15365b7"
    );
    assert_eq!(report["platform"], std::env::consts::OS);
    assert_eq!(report["pointer_bytes"], size_of::<usize>());
    assert_eq!(report["c_ulong_bytes"], size_of::<core::ffi::c_ulong>());
    assert_eq!(
        report["records"].as_object().expect("record object").len(),
        137
    );
    assert_eq!(
        report["constants"].as_object().expect("constant object").len(),
        295
    );
    assert_eq!(
        report["aliases"].as_object().expect("alias object").len(),
        170
    );
    macro_rules! record { ($ty:ident; $($field:tt => $offset:expr),* $(,)?) => {
        assert_eq!(report["records"][stringify!($ty)]["size"], size_of::<raw::$ty>(), "{} size", stringify!($ty));
        assert_eq!(report["records"][stringify!($ty)]["align"], align_of::<raw::$ty>(), "{} alignment", stringify!($ty));
        $(assert_eq!(report["records"][stringify!($ty)]["offsets"][$field], $offset, "{}.{} offset", stringify!($ty), $field);)*
    }; }
    record!(clap_version;
        "major" => offset_of!(raw::clap_version, major),
        "minor" => offset_of!(raw::clap_version, minor),
        "revision" => offset_of!(raw::clap_version, revision),
    );
    record!(clap_plugin_entry;
        "clap_version" => offset_of!(raw::clap_plugin_entry, clap_version),
        "init" => offset_of!(raw::clap_plugin_entry, init),
        "deinit" => offset_of!(raw::clap_plugin_entry, deinit),
        "get_factory" => offset_of!(raw::clap_plugin_entry, get_factory),
    );
    record!(clap_host;
        "clap_version" => offset_of!(raw::clap_host, clap_version),
        "host_data" => offset_of!(raw::clap_host, host_data),
        "name" => offset_of!(raw::clap_host, name),
        "vendor" => offset_of!(raw::clap_host, vendor),
        "url" => offset_of!(raw::clap_host, url),
        "version" => offset_of!(raw::clap_host, version),
        "get_extension" => offset_of!(raw::clap_host, get_extension),
        "request_restart" => offset_of!(raw::clap_host, request_restart),
        "request_process" => offset_of!(raw::clap_host, request_process),
        "request_callback" => offset_of!(raw::clap_host, request_callback),
    );
    record!(clap_event_header;
        "size" => offset_of!(raw::clap_event_header, size),
        "time" => offset_of!(raw::clap_event_header, time),
        "space_id" => offset_of!(raw::clap_event_header, space_id),
        "type_" => offset_of!(raw::clap_event_header, type_),
        "flags" => offset_of!(raw::clap_event_header, flags),
    );
    record!(clap_event_note;
        "header" => offset_of!(raw::clap_event_note, header),
        "note_id" => offset_of!(raw::clap_event_note, note_id),
        "port_index" => offset_of!(raw::clap_event_note, port_index),
        "channel" => offset_of!(raw::clap_event_note, channel),
        "key" => offset_of!(raw::clap_event_note, key),
        "velocity" => offset_of!(raw::clap_event_note, velocity),
    );
    record!(clap_event_note_expression;
        "header" => offset_of!(raw::clap_event_note_expression, header),
        "expression_id" => offset_of!(raw::clap_event_note_expression, expression_id),
        "note_id" => offset_of!(raw::clap_event_note_expression, note_id),
        "port_index" => offset_of!(raw::clap_event_note_expression, port_index),
        "channel" => offset_of!(raw::clap_event_note_expression, channel),
        "key" => offset_of!(raw::clap_event_note_expression, key),
        "value" => offset_of!(raw::clap_event_note_expression, value),
    );
    record!(clap_event_param_value;
        "header" => offset_of!(raw::clap_event_param_value, header),
        "param_id" => offset_of!(raw::clap_event_param_value, param_id),
        "cookie" => offset_of!(raw::clap_event_param_value, cookie),
        "note_id" => offset_of!(raw::clap_event_param_value, note_id),
        "port_index" => offset_of!(raw::clap_event_param_value, port_index),
        "channel" => offset_of!(raw::clap_event_param_value, channel),
        "key" => offset_of!(raw::clap_event_param_value, key),
        "value" => offset_of!(raw::clap_event_param_value, value),
    );
    record!(clap_event_param_mod;
        "header" => offset_of!(raw::clap_event_param_mod, header),
        "param_id" => offset_of!(raw::clap_event_param_mod, param_id),
        "cookie" => offset_of!(raw::clap_event_param_mod, cookie),
        "note_id" => offset_of!(raw::clap_event_param_mod, note_id),
        "port_index" => offset_of!(raw::clap_event_param_mod, port_index),
        "channel" => offset_of!(raw::clap_event_param_mod, channel),
        "key" => offset_of!(raw::clap_event_param_mod, key),
        "amount" => offset_of!(raw::clap_event_param_mod, amount),
    );
    record!(clap_event_param_gesture;
        "header" => offset_of!(raw::clap_event_param_gesture, header),
        "param_id" => offset_of!(raw::clap_event_param_gesture, param_id),
    );
    record!(clap_event_transport;
        "header" => offset_of!(raw::clap_event_transport, header),
        "flags" => offset_of!(raw::clap_event_transport, flags),
        "song_pos_beats" => offset_of!(raw::clap_event_transport, song_pos_beats),
        "song_pos_seconds" => offset_of!(raw::clap_event_transport, song_pos_seconds),
        "tempo" => offset_of!(raw::clap_event_transport, tempo),
        "tempo_inc" => offset_of!(raw::clap_event_transport, tempo_inc),
        "loop_start_beats" => offset_of!(raw::clap_event_transport, loop_start_beats),
        "loop_end_beats" => offset_of!(raw::clap_event_transport, loop_end_beats),
        "loop_start_seconds" => offset_of!(raw::clap_event_transport, loop_start_seconds),
        "loop_end_seconds" => offset_of!(raw::clap_event_transport, loop_end_seconds),
        "bar_start" => offset_of!(raw::clap_event_transport, bar_start),
        "bar_number" => offset_of!(raw::clap_event_transport, bar_number),
        "tsig_num" => offset_of!(raw::clap_event_transport, tsig_num),
        "tsig_denom" => offset_of!(raw::clap_event_transport, tsig_denom),
    );
    record!(clap_event_midi;
        "header" => offset_of!(raw::clap_event_midi, header),
        "port_index" => offset_of!(raw::clap_event_midi, port_index),
        "data" => offset_of!(raw::clap_event_midi, data),
    );
    record!(clap_event_midi_sysex;
        "header" => offset_of!(raw::clap_event_midi_sysex, header),
        "port_index" => offset_of!(raw::clap_event_midi_sysex, port_index),
        "buffer" => offset_of!(raw::clap_event_midi_sysex, buffer),
        "size" => offset_of!(raw::clap_event_midi_sysex, size),
    );
    record!(clap_event_midi2;
        "header" => offset_of!(raw::clap_event_midi2, header),
        "port_index" => offset_of!(raw::clap_event_midi2, port_index),
        "data" => offset_of!(raw::clap_event_midi2, data),
    );
    record!(clap_input_events;
        "ctx" => offset_of!(raw::clap_input_events, ctx),
        "size" => offset_of!(raw::clap_input_events, size),
        "get" => offset_of!(raw::clap_input_events, get),
    );
    record!(clap_output_events;
        "ctx" => offset_of!(raw::clap_output_events, ctx),
        "try_push" => offset_of!(raw::clap_output_events, try_push),
    );
    record!(clap_audio_buffer;
        "data32" => offset_of!(raw::clap_audio_buffer, data32),
        "data64" => offset_of!(raw::clap_audio_buffer, data64),
        "channel_count" => offset_of!(raw::clap_audio_buffer, channel_count),
        "latency" => offset_of!(raw::clap_audio_buffer, latency),
        "constant_mask" => offset_of!(raw::clap_audio_buffer, constant_mask),
    );
    record!(clap_process;
        "steady_time" => offset_of!(raw::clap_process, steady_time),
        "frames_count" => offset_of!(raw::clap_process, frames_count),
        "transport" => offset_of!(raw::clap_process, transport),
        "audio_inputs" => offset_of!(raw::clap_process, audio_inputs),
        "audio_outputs" => offset_of!(raw::clap_process, audio_outputs),
        "audio_inputs_count" => offset_of!(raw::clap_process, audio_inputs_count),
        "audio_outputs_count" => offset_of!(raw::clap_process, audio_outputs_count),
        "in_events" => offset_of!(raw::clap_process, in_events),
        "out_events" => offset_of!(raw::clap_process, out_events),
    );
    record!(clap_plugin_descriptor;
        "clap_version" => offset_of!(raw::clap_plugin_descriptor, clap_version),
        "id" => offset_of!(raw::clap_plugin_descriptor, id),
        "name" => offset_of!(raw::clap_plugin_descriptor, name),
        "vendor" => offset_of!(raw::clap_plugin_descriptor, vendor),
        "url" => offset_of!(raw::clap_plugin_descriptor, url),
        "manual_url" => offset_of!(raw::clap_plugin_descriptor, manual_url),
        "support_url" => offset_of!(raw::clap_plugin_descriptor, support_url),
        "version" => offset_of!(raw::clap_plugin_descriptor, version),
        "description" => offset_of!(raw::clap_plugin_descriptor, description),
        "features" => offset_of!(raw::clap_plugin_descriptor, features),
    );
    record!(clap_plugin;
        "desc" => offset_of!(raw::clap_plugin, desc),
        "plugin_data" => offset_of!(raw::clap_plugin, plugin_data),
        "init" => offset_of!(raw::clap_plugin, init),
        "destroy" => offset_of!(raw::clap_plugin, destroy),
        "activate" => offset_of!(raw::clap_plugin, activate),
        "deactivate" => offset_of!(raw::clap_plugin, deactivate),
        "start_processing" => offset_of!(raw::clap_plugin, start_processing),
        "stop_processing" => offset_of!(raw::clap_plugin, stop_processing),
        "reset" => offset_of!(raw::clap_plugin, reset),
        "process" => offset_of!(raw::clap_plugin, process),
        "get_extension" => offset_of!(raw::clap_plugin, get_extension),
        "on_main_thread" => offset_of!(raw::clap_plugin, on_main_thread),
    );
    record!(clap_plugin_factory;
        "get_plugin_count" => offset_of!(raw::clap_plugin_factory, get_plugin_count),
        "get_plugin_descriptor" => offset_of!(raw::clap_plugin_factory, get_plugin_descriptor),
        "create_plugin" => offset_of!(raw::clap_plugin_factory, create_plugin),
    );
    record!(clap_universal_plugin_id;
        "abi" => offset_of!(raw::clap_universal_plugin_id, abi),
        "id" => offset_of!(raw::clap_universal_plugin_id, id),
    );
    record!(clap_preset_discovery_metadata_receiver;
        "receiver_data" => offset_of!(raw::clap_preset_discovery_metadata_receiver, receiver_data),
        "on_error" => offset_of!(raw::clap_preset_discovery_metadata_receiver, on_error),
        "begin_preset" => offset_of!(raw::clap_preset_discovery_metadata_receiver, begin_preset),
        "add_plugin_id" => offset_of!(raw::clap_preset_discovery_metadata_receiver, add_plugin_id),
        "set_soundpack_id" => offset_of!(raw::clap_preset_discovery_metadata_receiver, set_soundpack_id),
        "set_flags" => offset_of!(raw::clap_preset_discovery_metadata_receiver, set_flags),
        "add_creator" => offset_of!(raw::clap_preset_discovery_metadata_receiver, add_creator),
        "set_description" => offset_of!(raw::clap_preset_discovery_metadata_receiver, set_description),
        "set_timestamps" => offset_of!(raw::clap_preset_discovery_metadata_receiver, set_timestamps),
        "add_feature" => offset_of!(raw::clap_preset_discovery_metadata_receiver, add_feature),
        "add_extra_info" => offset_of!(raw::clap_preset_discovery_metadata_receiver, add_extra_info),
    );
    record!(clap_preset_discovery_filetype;
        "name" => offset_of!(raw::clap_preset_discovery_filetype, name),
        "description" => offset_of!(raw::clap_preset_discovery_filetype, description),
        "file_extension" => offset_of!(raw::clap_preset_discovery_filetype, file_extension),
    );
    record!(clap_preset_discovery_location;
        "flags" => offset_of!(raw::clap_preset_discovery_location, flags),
        "name" => offset_of!(raw::clap_preset_discovery_location, name),
        "kind" => offset_of!(raw::clap_preset_discovery_location, kind),
        "location" => offset_of!(raw::clap_preset_discovery_location, location),
    );
    record!(clap_preset_discovery_soundpack;
        "flags" => offset_of!(raw::clap_preset_discovery_soundpack, flags),
        "id" => offset_of!(raw::clap_preset_discovery_soundpack, id),
        "name" => offset_of!(raw::clap_preset_discovery_soundpack, name),
        "description" => offset_of!(raw::clap_preset_discovery_soundpack, description),
        "homepage_url" => offset_of!(raw::clap_preset_discovery_soundpack, homepage_url),
        "vendor" => offset_of!(raw::clap_preset_discovery_soundpack, vendor),
        "image_path" => offset_of!(raw::clap_preset_discovery_soundpack, image_path),
        "release_timestamp" => offset_of!(raw::clap_preset_discovery_soundpack, release_timestamp),
    );
    record!(clap_preset_discovery_provider_descriptor;
        "clap_version" => offset_of!(raw::clap_preset_discovery_provider_descriptor, clap_version),
        "id" => offset_of!(raw::clap_preset_discovery_provider_descriptor, id),
        "name" => offset_of!(raw::clap_preset_discovery_provider_descriptor, name),
        "vendor" => offset_of!(raw::clap_preset_discovery_provider_descriptor, vendor),
    );
    record!(clap_preset_discovery_provider;
        "desc" => offset_of!(raw::clap_preset_discovery_provider, desc),
        "provider_data" => offset_of!(raw::clap_preset_discovery_provider, provider_data),
        "init" => offset_of!(raw::clap_preset_discovery_provider, init),
        "destroy" => offset_of!(raw::clap_preset_discovery_provider, destroy),
        "get_metadata" => offset_of!(raw::clap_preset_discovery_provider, get_metadata),
        "get_extension" => offset_of!(raw::clap_preset_discovery_provider, get_extension),
    );
    record!(clap_preset_discovery_indexer;
        "clap_version" => offset_of!(raw::clap_preset_discovery_indexer, clap_version),
        "name" => offset_of!(raw::clap_preset_discovery_indexer, name),
        "vendor" => offset_of!(raw::clap_preset_discovery_indexer, vendor),
        "url" => offset_of!(raw::clap_preset_discovery_indexer, url),
        "version" => offset_of!(raw::clap_preset_discovery_indexer, version),
        "indexer_data" => offset_of!(raw::clap_preset_discovery_indexer, indexer_data),
        "declare_filetype" => offset_of!(raw::clap_preset_discovery_indexer, declare_filetype),
        "declare_location" => offset_of!(raw::clap_preset_discovery_indexer, declare_location),
        "declare_soundpack" => offset_of!(raw::clap_preset_discovery_indexer, declare_soundpack),
        "get_extension" => offset_of!(raw::clap_preset_discovery_indexer, get_extension),
    );
    record!(clap_preset_discovery_factory;
        "count" => offset_of!(raw::clap_preset_discovery_factory, count),
        "get_descriptor" => offset_of!(raw::clap_preset_discovery_factory, get_descriptor),
        "create" => offset_of!(raw::clap_preset_discovery_factory, create),
    );
    record!(clap_ambisonic_config;
        "ordering" => offset_of!(raw::clap_ambisonic_config, ordering),
        "normalization" => offset_of!(raw::clap_ambisonic_config, normalization),
    );
    record!(clap_plugin_ambisonic;
        "is_config_supported" => offset_of!(raw::clap_plugin_ambisonic, is_config_supported),
        "get_config" => offset_of!(raw::clap_plugin_ambisonic, get_config),
    );
    record!(clap_host_ambisonic;
        "changed" => offset_of!(raw::clap_host_ambisonic, changed),
    );
    record!(clap_plugin_audio_ports_activation;
        "can_activate_while_processing" => offset_of!(raw::clap_plugin_audio_ports_activation, can_activate_while_processing),
        "set_active" => offset_of!(raw::clap_plugin_audio_ports_activation, set_active),
    );
    record!(clap_audio_port_info;
        "id" => offset_of!(raw::clap_audio_port_info, id),
        "name" => offset_of!(raw::clap_audio_port_info, name),
        "flags" => offset_of!(raw::clap_audio_port_info, flags),
        "channel_count" => offset_of!(raw::clap_audio_port_info, channel_count),
        "port_type" => offset_of!(raw::clap_audio_port_info, port_type),
        "in_place_pair" => offset_of!(raw::clap_audio_port_info, in_place_pair),
    );
    record!(clap_plugin_audio_ports;
        "count" => offset_of!(raw::clap_plugin_audio_ports, count),
        "get" => offset_of!(raw::clap_plugin_audio_ports, get),
    );
    record!(clap_host_audio_ports;
        "is_rescan_flag_supported" => offset_of!(raw::clap_host_audio_ports, is_rescan_flag_supported),
        "rescan" => offset_of!(raw::clap_host_audio_ports, rescan),
    );
    record!(clap_audio_ports_config;
        "id" => offset_of!(raw::clap_audio_ports_config, id),
        "name" => offset_of!(raw::clap_audio_ports_config, name),
        "input_port_count" => offset_of!(raw::clap_audio_ports_config, input_port_count),
        "output_port_count" => offset_of!(raw::clap_audio_ports_config, output_port_count),
        "has_main_input" => offset_of!(raw::clap_audio_ports_config, has_main_input),
        "main_input_channel_count" => offset_of!(raw::clap_audio_ports_config, main_input_channel_count),
        "main_input_port_type" => offset_of!(raw::clap_audio_ports_config, main_input_port_type),
        "has_main_output" => offset_of!(raw::clap_audio_ports_config, has_main_output),
        "main_output_channel_count" => offset_of!(raw::clap_audio_ports_config, main_output_channel_count),
        "main_output_port_type" => offset_of!(raw::clap_audio_ports_config, main_output_port_type),
    );
    record!(clap_plugin_audio_ports_config;
        "count" => offset_of!(raw::clap_plugin_audio_ports_config, count),
        "get" => offset_of!(raw::clap_plugin_audio_ports_config, get),
        "select" => offset_of!(raw::clap_plugin_audio_ports_config, select),
    );
    record!(clap_plugin_audio_ports_config_info;
        "current_config" => offset_of!(raw::clap_plugin_audio_ports_config_info, current_config),
        "get" => offset_of!(raw::clap_plugin_audio_ports_config_info, get),
    );
    record!(clap_host_audio_ports_config;
        "rescan" => offset_of!(raw::clap_host_audio_ports_config, rescan),
    );
    record!(clap_audio_port_configuration_request;
        "is_input" => offset_of!(raw::clap_audio_port_configuration_request, is_input),
        "port_index" => offset_of!(raw::clap_audio_port_configuration_request, port_index),
        "channel_count" => offset_of!(raw::clap_audio_port_configuration_request, channel_count),
        "port_type" => offset_of!(raw::clap_audio_port_configuration_request, port_type),
        "port_details" => offset_of!(raw::clap_audio_port_configuration_request, port_details),
    );
    record!(clap_plugin_configurable_audio_ports;
        "can_apply_configuration" => offset_of!(raw::clap_plugin_configurable_audio_ports, can_apply_configuration),
        "apply_configuration" => offset_of!(raw::clap_plugin_configurable_audio_ports, apply_configuration),
    );
    record!(clap_context_menu_target;
        "kind" => offset_of!(raw::clap_context_menu_target, kind),
        "id" => offset_of!(raw::clap_context_menu_target, id),
    );
    record!(clap_context_menu_entry;
        "label" => offset_of!(raw::clap_context_menu_entry, label),
        "is_enabled" => offset_of!(raw::clap_context_menu_entry, is_enabled),
        "action_id" => offset_of!(raw::clap_context_menu_entry, action_id),
    );
    record!(clap_context_menu_check_entry;
        "label" => offset_of!(raw::clap_context_menu_check_entry, label),
        "is_enabled" => offset_of!(raw::clap_context_menu_check_entry, is_enabled),
        "is_checked" => offset_of!(raw::clap_context_menu_check_entry, is_checked),
        "action_id" => offset_of!(raw::clap_context_menu_check_entry, action_id),
    );
    record!(clap_context_menu_item_title;
        "title" => offset_of!(raw::clap_context_menu_item_title, title),
        "is_enabled" => offset_of!(raw::clap_context_menu_item_title, is_enabled),
    );
    record!(clap_context_menu_submenu;
        "label" => offset_of!(raw::clap_context_menu_submenu, label),
        "is_enabled" => offset_of!(raw::clap_context_menu_submenu, is_enabled),
    );
    record!(clap_context_menu_builder;
        "ctx" => offset_of!(raw::clap_context_menu_builder, ctx),
        "add_item" => offset_of!(raw::clap_context_menu_builder, add_item),
        "supports" => offset_of!(raw::clap_context_menu_builder, supports),
    );
    record!(clap_plugin_context_menu;
        "populate" => offset_of!(raw::clap_plugin_context_menu, populate),
        "perform" => offset_of!(raw::clap_plugin_context_menu, perform),
    );
    record!(clap_host_context_menu;
        "populate" => offset_of!(raw::clap_host_context_menu, populate),
        "perform" => offset_of!(raw::clap_host_context_menu, perform),
        "can_popup" => offset_of!(raw::clap_host_context_menu, can_popup),
        "popup" => offset_of!(raw::clap_host_context_menu, popup),
    );
    record!(clap_host_event_registry;
        "query" => offset_of!(raw::clap_host_event_registry, query),
    );
    record!(clap_window;
        "api" => offset_of!(raw::clap_window, api),
        "__bindgen_anon_1.cocoa" => offset_of!(raw::clap_window, __bindgen_anon_1.cocoa),
        "__bindgen_anon_1.uikit" => offset_of!(raw::clap_window, __bindgen_anon_1.uikit),
        "__bindgen_anon_1.x11" => offset_of!(raw::clap_window, __bindgen_anon_1.x11),
        "__bindgen_anon_1.win32" => offset_of!(raw::clap_window, __bindgen_anon_1.win32),
        "__bindgen_anon_1.ptr" => offset_of!(raw::clap_window, __bindgen_anon_1.ptr),
    );
    record!(clap_gui_resize_hints;
        "can_resize_horizontally" => offset_of!(raw::clap_gui_resize_hints, can_resize_horizontally),
        "can_resize_vertically" => offset_of!(raw::clap_gui_resize_hints, can_resize_vertically),
        "preserve_aspect_ratio" => offset_of!(raw::clap_gui_resize_hints, preserve_aspect_ratio),
        "aspect_ratio_width" => offset_of!(raw::clap_gui_resize_hints, aspect_ratio_width),
        "aspect_ratio_height" => offset_of!(raw::clap_gui_resize_hints, aspect_ratio_height),
    );
    record!(clap_plugin_gui;
        "is_api_supported" => offset_of!(raw::clap_plugin_gui, is_api_supported),
        "get_preferred_api" => offset_of!(raw::clap_plugin_gui, get_preferred_api),
        "create" => offset_of!(raw::clap_plugin_gui, create),
        "destroy" => offset_of!(raw::clap_plugin_gui, destroy),
        "set_scale" => offset_of!(raw::clap_plugin_gui, set_scale),
        "get_size" => offset_of!(raw::clap_plugin_gui, get_size),
        "can_resize" => offset_of!(raw::clap_plugin_gui, can_resize),
        "get_resize_hints" => offset_of!(raw::clap_plugin_gui, get_resize_hints),
        "adjust_size" => offset_of!(raw::clap_plugin_gui, adjust_size),
        "set_size" => offset_of!(raw::clap_plugin_gui, set_size),
        "set_parent" => offset_of!(raw::clap_plugin_gui, set_parent),
        "set_transient" => offset_of!(raw::clap_plugin_gui, set_transient),
        "suggest_title" => offset_of!(raw::clap_plugin_gui, suggest_title),
        "show" => offset_of!(raw::clap_plugin_gui, show),
        "hide" => offset_of!(raw::clap_plugin_gui, hide),
    );
    record!(clap_host_gui;
        "resize_hints_changed" => offset_of!(raw::clap_host_gui, resize_hints_changed),
        "request_resize" => offset_of!(raw::clap_host_gui, request_resize),
        "request_show" => offset_of!(raw::clap_host_gui, request_show),
        "request_hide" => offset_of!(raw::clap_host_gui, request_hide),
        "closed" => offset_of!(raw::clap_host_gui, closed),
    );
    record!(clap_plugin_latency;
        "get" => offset_of!(raw::clap_plugin_latency, get),
    );
    record!(clap_host_latency;
        "changed" => offset_of!(raw::clap_host_latency, changed),
    );
    record!(clap_host_log;
        "log" => offset_of!(raw::clap_host_log, log),
    );
    record!(clap_note_name;
        "name" => offset_of!(raw::clap_note_name, name),
        "port" => offset_of!(raw::clap_note_name, port),
        "key" => offset_of!(raw::clap_note_name, key),
        "channel" => offset_of!(raw::clap_note_name, channel),
    );
    record!(clap_plugin_note_name;
        "count" => offset_of!(raw::clap_plugin_note_name, count),
        "get" => offset_of!(raw::clap_plugin_note_name, get),
    );
    record!(clap_host_note_name;
        "changed" => offset_of!(raw::clap_host_note_name, changed),
    );
    record!(clap_note_port_info;
        "id" => offset_of!(raw::clap_note_port_info, id),
        "supported_dialects" => offset_of!(raw::clap_note_port_info, supported_dialects),
        "preferred_dialect" => offset_of!(raw::clap_note_port_info, preferred_dialect),
        "name" => offset_of!(raw::clap_note_port_info, name),
    );
    record!(clap_plugin_note_ports;
        "count" => offset_of!(raw::clap_plugin_note_ports, count),
        "get" => offset_of!(raw::clap_plugin_note_ports, get),
    );
    record!(clap_host_note_ports;
        "supported_dialects" => offset_of!(raw::clap_host_note_ports, supported_dialects),
        "rescan" => offset_of!(raw::clap_host_note_ports, rescan),
    );
    record!(clap_param_info;
        "id" => offset_of!(raw::clap_param_info, id),
        "flags" => offset_of!(raw::clap_param_info, flags),
        "cookie" => offset_of!(raw::clap_param_info, cookie),
        "name" => offset_of!(raw::clap_param_info, name),
        "module" => offset_of!(raw::clap_param_info, module),
        "min_value" => offset_of!(raw::clap_param_info, min_value),
        "max_value" => offset_of!(raw::clap_param_info, max_value),
        "default_value" => offset_of!(raw::clap_param_info, default_value),
    );
    record!(clap_plugin_params;
        "count" => offset_of!(raw::clap_plugin_params, count),
        "get_info" => offset_of!(raw::clap_plugin_params, get_info),
        "get_value" => offset_of!(raw::clap_plugin_params, get_value),
        "value_to_text" => offset_of!(raw::clap_plugin_params, value_to_text),
        "text_to_value" => offset_of!(raw::clap_plugin_params, text_to_value),
        "flush" => offset_of!(raw::clap_plugin_params, flush),
    );
    record!(clap_host_params;
        "rescan" => offset_of!(raw::clap_host_params, rescan),
        "clear" => offset_of!(raw::clap_host_params, clear),
        "request_flush" => offset_of!(raw::clap_host_params, request_flush),
    );
    record!(clap_color;
        "alpha" => offset_of!(raw::clap_color, alpha),
        "red" => offset_of!(raw::clap_color, red),
        "green" => offset_of!(raw::clap_color, green),
        "blue" => offset_of!(raw::clap_color, blue),
    );
    record!(clap_plugin_param_indication;
        "set_mapping" => offset_of!(raw::clap_plugin_param_indication, set_mapping),
        "set_automation" => offset_of!(raw::clap_plugin_param_indication, set_automation),
    );
    record!(clap_plugin_posix_fd_support;
        "on_fd" => offset_of!(raw::clap_plugin_posix_fd_support, on_fd),
    );
    record!(clap_host_posix_fd_support;
        "register_fd" => offset_of!(raw::clap_host_posix_fd_support, register_fd),
        "modify_fd" => offset_of!(raw::clap_host_posix_fd_support, modify_fd),
        "unregister_fd" => offset_of!(raw::clap_host_posix_fd_support, unregister_fd),
    );
    record!(clap_plugin_preset_load;
        "from_location" => offset_of!(raw::clap_plugin_preset_load, from_location),
    );
    record!(clap_host_preset_load;
        "on_error" => offset_of!(raw::clap_host_preset_load, on_error),
        "loaded" => offset_of!(raw::clap_host_preset_load, loaded),
    );
    record!(clap_remote_controls_page;
        "section_name" => offset_of!(raw::clap_remote_controls_page, section_name),
        "page_id" => offset_of!(raw::clap_remote_controls_page, page_id),
        "page_name" => offset_of!(raw::clap_remote_controls_page, page_name),
        "param_ids" => offset_of!(raw::clap_remote_controls_page, param_ids),
        "is_for_preset" => offset_of!(raw::clap_remote_controls_page, is_for_preset),
    );
    record!(clap_plugin_remote_controls;
        "count" => offset_of!(raw::clap_plugin_remote_controls, count),
        "get" => offset_of!(raw::clap_plugin_remote_controls, get),
    );
    record!(clap_host_remote_controls;
        "changed" => offset_of!(raw::clap_host_remote_controls, changed),
        "suggest_page" => offset_of!(raw::clap_host_remote_controls, suggest_page),
    );
    record!(clap_plugin_render;
        "has_hard_realtime_requirement" => offset_of!(raw::clap_plugin_render, has_hard_realtime_requirement),
        "set" => offset_of!(raw::clap_plugin_render, set),
    );
    record!(clap_istream;
        "ctx" => offset_of!(raw::clap_istream, ctx),
        "read" => offset_of!(raw::clap_istream, read),
    );
    record!(clap_ostream;
        "ctx" => offset_of!(raw::clap_ostream, ctx),
        "write" => offset_of!(raw::clap_ostream, write),
    );
    record!(clap_plugin_state_context;
        "save" => offset_of!(raw::clap_plugin_state_context, save),
        "load" => offset_of!(raw::clap_plugin_state_context, load),
    );
    record!(clap_plugin_state;
        "save" => offset_of!(raw::clap_plugin_state, save),
        "load" => offset_of!(raw::clap_plugin_state, load),
    );
    record!(clap_host_state;
        "mark_dirty" => offset_of!(raw::clap_host_state, mark_dirty),
    );
    record!(clap_plugin_surround;
        "is_channel_mask_supported" => offset_of!(raw::clap_plugin_surround, is_channel_mask_supported),
        "get_channel_map" => offset_of!(raw::clap_plugin_surround, get_channel_map),
    );
    record!(clap_host_surround;
        "changed" => offset_of!(raw::clap_host_surround, changed),
    );
    record!(clap_plugin_tail;
        "get" => offset_of!(raw::clap_plugin_tail, get),
    );
    record!(clap_host_tail;
        "changed" => offset_of!(raw::clap_host_tail, changed),
    );
    record!(clap_host_thread_check;
        "is_main_thread" => offset_of!(raw::clap_host_thread_check, is_main_thread),
        "is_audio_thread" => offset_of!(raw::clap_host_thread_check, is_audio_thread),
    );
    record!(clap_plugin_thread_pool;
        "exec" => offset_of!(raw::clap_plugin_thread_pool, exec),
    );
    record!(clap_host_thread_pool;
        "request_exec" => offset_of!(raw::clap_host_thread_pool, request_exec),
    );
    record!(clap_plugin_timer_support;
        "on_timer" => offset_of!(raw::clap_plugin_timer_support, on_timer),
    );
    record!(clap_host_timer_support;
        "register_timer" => offset_of!(raw::clap_host_timer_support, register_timer),
        "unregister_timer" => offset_of!(raw::clap_host_timer_support, unregister_timer),
    );
    record!(clap_track_info;
        "flags" => offset_of!(raw::clap_track_info, flags),
        "name" => offset_of!(raw::clap_track_info, name),
        "color" => offset_of!(raw::clap_track_info, color),
        "audio_channel_count" => offset_of!(raw::clap_track_info, audio_channel_count),
        "audio_port_type" => offset_of!(raw::clap_track_info, audio_port_type),
    );
    record!(clap_plugin_track_info;
        "changed" => offset_of!(raw::clap_plugin_track_info, changed),
    );
    record!(clap_host_track_info;
        "get" => offset_of!(raw::clap_host_track_info, get),
    );
    record!(clap_voice_info;
        "voice_count" => offset_of!(raw::clap_voice_info, voice_count),
        "voice_capacity" => offset_of!(raw::clap_voice_info, voice_capacity),
        "flags" => offset_of!(raw::clap_voice_info, flags),
    );
    record!(clap_plugin_voice_info;
        "get" => offset_of!(raw::clap_plugin_voice_info, get),
    );
    record!(clap_host_voice_info;
        "changed" => offset_of!(raw::clap_host_voice_info, changed),
    );
    record!(clap_plugin_invalidation_source;
        "directory" => offset_of!(raw::clap_plugin_invalidation_source, directory),
        "filename_glob" => offset_of!(raw::clap_plugin_invalidation_source, filename_glob),
        "recursive_scan" => offset_of!(raw::clap_plugin_invalidation_source, recursive_scan),
    );
    record!(clap_plugin_invalidation_factory;
        "count" => offset_of!(raw::clap_plugin_invalidation_factory, count),
        "get" => offset_of!(raw::clap_plugin_invalidation_factory, get),
        "refresh" => offset_of!(raw::clap_plugin_invalidation_factory, refresh),
    );
    record!(clap_plugin_state_converter_descriptor;
        "clap_version" => offset_of!(raw::clap_plugin_state_converter_descriptor, clap_version),
        "src_plugin_id" => offset_of!(raw::clap_plugin_state_converter_descriptor, src_plugin_id),
        "dst_plugin_id" => offset_of!(raw::clap_plugin_state_converter_descriptor, dst_plugin_id),
        "id" => offset_of!(raw::clap_plugin_state_converter_descriptor, id),
        "name" => offset_of!(raw::clap_plugin_state_converter_descriptor, name),
        "vendor" => offset_of!(raw::clap_plugin_state_converter_descriptor, vendor),
        "version" => offset_of!(raw::clap_plugin_state_converter_descriptor, version),
        "description" => offset_of!(raw::clap_plugin_state_converter_descriptor, description),
    );
    record!(clap_plugin_state_converter;
        "desc" => offset_of!(raw::clap_plugin_state_converter, desc),
        "converter_data" => offset_of!(raw::clap_plugin_state_converter, converter_data),
        "destroy" => offset_of!(raw::clap_plugin_state_converter, destroy),
        "convert_state" => offset_of!(raw::clap_plugin_state_converter, convert_state),
        "convert_normalized_value" => offset_of!(raw::clap_plugin_state_converter, convert_normalized_value),
        "convert_plain_value" => offset_of!(raw::clap_plugin_state_converter, convert_plain_value),
    );
    record!(clap_plugin_state_converter_factory;
        "count" => offset_of!(raw::clap_plugin_state_converter_factory, count),
        "get_descriptor" => offset_of!(raw::clap_plugin_state_converter_factory, get_descriptor),
        "create" => offset_of!(raw::clap_plugin_state_converter_factory, create),
    );
    record!(clap_plugin_background_activation;
        "activate_from_background_thread" => offset_of!(raw::clap_plugin_background_activation, activate_from_background_thread),
        "deactivate_from_background_thread" => offset_of!(raw::clap_plugin_background_activation, deactivate_from_background_thread),
    );
    record!(clap_host_background_progress;
        "is_canceled" => offset_of!(raw::clap_host_background_progress, is_canceled),
        "progress" => offset_of!(raw::clap_host_background_progress, progress),
    );
    record!(clap_plugin_background_state_context;
        "save_from_background_thread" => offset_of!(raw::clap_plugin_background_state_context, save_from_background_thread),
        "load_from_background_thread" => offset_of!(raw::clap_plugin_background_state_context, load_from_background_thread),
    );
    record!(clap_plugin_extensible_audio_ports;
        "add_port" => offset_of!(raw::clap_plugin_extensible_audio_ports, add_port),
        "remove_port" => offset_of!(raw::clap_plugin_extensible_audio_ports, remove_port),
    );
    record!(clap_plugin_flush_events;
        "flush" => offset_of!(raw::clap_plugin_flush_events, flush),
    );
    record!(clap_host_flush_events;
        "request_flush" => offset_of!(raw::clap_host_flush_events, request_flush),
    );
    record!(clap_plugin_gain_adjustment_metering;
        "get" => offset_of!(raw::clap_plugin_gain_adjustment_metering, get),
    );
    record!(clap_mini_curve_display_curve_hints;
        "x_min" => offset_of!(raw::clap_mini_curve_display_curve_hints, x_min),
        "x_max" => offset_of!(raw::clap_mini_curve_display_curve_hints, x_max),
        "y_min" => offset_of!(raw::clap_mini_curve_display_curve_hints, y_min),
        "y_max" => offset_of!(raw::clap_mini_curve_display_curve_hints, y_max),
    );
    record!(clap_mini_curve_display_curve_data;
        "curve_kind" => offset_of!(raw::clap_mini_curve_display_curve_data, curve_kind),
        "values" => offset_of!(raw::clap_mini_curve_display_curve_data, values),
        "values_count" => offset_of!(raw::clap_mini_curve_display_curve_data, values_count),
    );
    record!(clap_plugin_mini_curve_display;
        "get_curve_count" => offset_of!(raw::clap_plugin_mini_curve_display, get_curve_count),
        "render" => offset_of!(raw::clap_plugin_mini_curve_display, render),
        "set_observed" => offset_of!(raw::clap_plugin_mini_curve_display, set_observed),
        "get_axis_name" => offset_of!(raw::clap_plugin_mini_curve_display, get_axis_name),
    );
    record!(clap_host_mini_curve_display;
        "get_hints" => offset_of!(raw::clap_host_mini_curve_display, get_hints),
        "set_dynamic" => offset_of!(raw::clap_host_mini_curve_display, set_dynamic),
        "changed" => offset_of!(raw::clap_host_mini_curve_display, changed),
    );
    record!(clap_plugin_octave_number;
        "set_note60_octave" => offset_of!(raw::clap_plugin_octave_number, set_note60_octave),
    );
    record!(clap_plugin_params_origin;
        "get" => offset_of!(raw::clap_plugin_params_origin, get),
    );
    record!(clap_host_params_origin;
        "changed" => offset_of!(raw::clap_host_params_origin, changed),
    );
    record!(clap_host_param_hovered;
        "update" => offset_of!(raw::clap_host_param_hovered, update),
    );
    record!(clap_project_location_element;
        "flags" => offset_of!(raw::clap_project_location_element, flags),
        "kind" => offset_of!(raw::clap_project_location_element, kind),
        "track_kind" => offset_of!(raw::clap_project_location_element, track_kind),
        "index" => offset_of!(raw::clap_project_location_element, index),
        "id" => offset_of!(raw::clap_project_location_element, id),
        "name" => offset_of!(raw::clap_project_location_element, name),
        "color" => offset_of!(raw::clap_project_location_element, color),
    );
    record!(clap_plugin_project_location;
        "set" => offset_of!(raw::clap_plugin_project_location, set),
    );
    record!(clap_plugin_resource_directory;
        "set_directory" => offset_of!(raw::clap_plugin_resource_directory, set_directory),
        "collect" => offset_of!(raw::clap_plugin_resource_directory, collect),
        "get_files_count" => offset_of!(raw::clap_plugin_resource_directory, get_files_count),
        "get_file_path" => offset_of!(raw::clap_plugin_resource_directory, get_file_path),
    );
    record!(clap_host_resource_directory;
        "request_directory" => offset_of!(raw::clap_host_resource_directory, request_directory),
        "release_directory" => offset_of!(raw::clap_host_resource_directory, release_directory),
    );
    record!(clap_host_scratch_memory;
        "reserve" => offset_of!(raw::clap_host_scratch_memory, reserve),
        "access" => offset_of!(raw::clap_host_scratch_memory, access),
    );
    record!(clap_host_transport_control;
        "request_start" => offset_of!(raw::clap_host_transport_control, request_start),
        "request_stop" => offset_of!(raw::clap_host_transport_control, request_stop),
        "request_continue" => offset_of!(raw::clap_host_transport_control, request_continue),
        "request_pause" => offset_of!(raw::clap_host_transport_control, request_pause),
        "request_toggle_play" => offset_of!(raw::clap_host_transport_control, request_toggle_play),
        "request_jump" => offset_of!(raw::clap_host_transport_control, request_jump),
        "request_loop_region" => offset_of!(raw::clap_host_transport_control, request_loop_region),
        "request_toggle_loop" => offset_of!(raw::clap_host_transport_control, request_toggle_loop),
        "request_enable_loop" => offset_of!(raw::clap_host_transport_control, request_enable_loop),
        "request_record" => offset_of!(raw::clap_host_transport_control, request_record),
        "request_toggle_record" => offset_of!(raw::clap_host_transport_control, request_toggle_record),
        "request_tempo" => offset_of!(raw::clap_host_transport_control, request_tempo),
        "request_time_signature" => offset_of!(raw::clap_host_transport_control, request_time_signature),
    );
    record!(clap_event_trigger;
        "header" => offset_of!(raw::clap_event_trigger, header),
        "trigger_id" => offset_of!(raw::clap_event_trigger, trigger_id),
        "cookie" => offset_of!(raw::clap_event_trigger, cookie),
        "note_id" => offset_of!(raw::clap_event_trigger, note_id),
        "port_index" => offset_of!(raw::clap_event_trigger, port_index),
        "channel" => offset_of!(raw::clap_event_trigger, channel),
        "key" => offset_of!(raw::clap_event_trigger, key),
    );
    record!(clap_trigger_info;
        "id" => offset_of!(raw::clap_trigger_info, id),
        "flags" => offset_of!(raw::clap_trigger_info, flags),
        "cookie" => offset_of!(raw::clap_trigger_info, cookie),
        "name" => offset_of!(raw::clap_trigger_info, name),
        "module" => offset_of!(raw::clap_trigger_info, module),
    );
    record!(clap_plugin_triggers;
        "count" => offset_of!(raw::clap_plugin_triggers, count),
        "get_info" => offset_of!(raw::clap_plugin_triggers, get_info),
    );
    record!(clap_host_triggers;
        "rescan" => offset_of!(raw::clap_host_triggers, rescan),
        "clear" => offset_of!(raw::clap_host_triggers, clear),
    );
    record!(clap_event_tuning;
        "header" => offset_of!(raw::clap_event_tuning, header),
        "port_index" => offset_of!(raw::clap_event_tuning, port_index),
        "channel" => offset_of!(raw::clap_event_tuning, channel),
        "tunning_id" => offset_of!(raw::clap_event_tuning, tunning_id),
    );
    record!(clap_tuning_info;
        "tuning_id" => offset_of!(raw::clap_tuning_info, tuning_id),
        "name" => offset_of!(raw::clap_tuning_info, name),
        "is_dynamic" => offset_of!(raw::clap_tuning_info, is_dynamic),
    );
    record!(clap_plugin_tuning;
        "changed" => offset_of!(raw::clap_plugin_tuning, changed),
    );
    record!(clap_host_tuning;
        "get_relative" => offset_of!(raw::clap_host_tuning, get_relative),
        "should_play" => offset_of!(raw::clap_host_tuning, should_play),
        "get_tuning_count" => offset_of!(raw::clap_host_tuning, get_tuning_count),
        "get_info" => offset_of!(raw::clap_host_tuning, get_info),
    );
    record!(clap_undo_delta_properties;
        "has_delta" => offset_of!(raw::clap_undo_delta_properties, has_delta),
        "are_deltas_persistent" => offset_of!(raw::clap_undo_delta_properties, are_deltas_persistent),
        "format_version" => offset_of!(raw::clap_undo_delta_properties, format_version),
    );
    record!(clap_plugin_undo_delta;
        "get_delta_properties" => offset_of!(raw::clap_plugin_undo_delta, get_delta_properties),
        "can_use_delta_format_version" => offset_of!(raw::clap_plugin_undo_delta, can_use_delta_format_version),
        "undo" => offset_of!(raw::clap_plugin_undo_delta, undo),
        "redo" => offset_of!(raw::clap_plugin_undo_delta, redo),
    );
    record!(clap_plugin_undo_context;
        "set_can_undo" => offset_of!(raw::clap_plugin_undo_context, set_can_undo),
        "set_can_redo" => offset_of!(raw::clap_plugin_undo_context, set_can_redo),
        "set_undo_name" => offset_of!(raw::clap_plugin_undo_context, set_undo_name),
        "set_redo_name" => offset_of!(raw::clap_plugin_undo_context, set_redo_name),
    );
    record!(clap_host_undo;
        "begin_change" => offset_of!(raw::clap_host_undo, begin_change),
        "cancel_change" => offset_of!(raw::clap_host_undo, cancel_change),
        "change_made" => offset_of!(raw::clap_host_undo, change_made),
        "request_undo" => offset_of!(raw::clap_host_undo, request_undo),
        "request_redo" => offset_of!(raw::clap_host_undo, request_redo),
        "set_wants_context_updates" => offset_of!(raw::clap_host_undo, set_wants_context_updates),
    );
    record!(clap_plugin_webview;
        "get_uri" => offset_of!(raw::clap_plugin_webview, get_uri),
        "get_resource" => offset_of!(raw::clap_plugin_webview, get_resource),
        "receive" => offset_of!(raw::clap_plugin_webview, receive),
    );
    record!(clap_host_webview;
        "send" => offset_of!(raw::clap_host_webview, send),
    );
    assert_eq!(
        report["constants"]["CLAP_VERSION_MAJOR"]
            .as_u64()
            .expect("integer ABI constant"),
        u64::from(raw::CLAP_VERSION_MAJOR),
        "CLAP_VERSION_MAJOR value"
    );
    assert_eq!(
        report["constants"]["CLAP_VERSION_MINOR"]
            .as_u64()
            .expect("integer ABI constant"),
        u64::from(raw::CLAP_VERSION_MINOR),
        "CLAP_VERSION_MINOR value"
    );
    assert_eq!(
        report["constants"]["CLAP_VERSION_REVISION"]
            .as_u64()
            .expect("integer ABI constant"),
        u64::from(raw::CLAP_VERSION_REVISION),
        "CLAP_VERSION_REVISION value"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_INSTRUMENT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_INSTRUMENT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_INSTRUMENT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_AUDIO_EFFECT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_AUDIO_EFFECT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_AUDIO_EFFECT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_NOTE_EFFECT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_NOTE_EFFECT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_NOTE_EFFECT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_NOTE_DETECTOR"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_NOTE_DETECTOR
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_NOTE_DETECTOR bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_ANALYZER"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_ANALYZER
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_ANALYZER bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_SYNTHESIZER"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_SYNTHESIZER
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_SYNTHESIZER bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_SAMPLER"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_SAMPLER
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_SAMPLER bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_DRUM"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_DRUM
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_DRUM bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_DRUM_MACHINE"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_DRUM_MACHINE
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_DRUM_MACHINE bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_FILTER"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_FILTER
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_FILTER bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_PHASER"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_PHASER
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_PHASER bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_EQUALIZER"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_EQUALIZER
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_EQUALIZER bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_DEESSER"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_DEESSER
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_DEESSER bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_PHASE_VOCODER"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_PHASE_VOCODER
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_PHASE_VOCODER bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_GRANULAR"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_GRANULAR
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_GRANULAR bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_FREQUENCY_SHIFTER"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_FREQUENCY_SHIFTER
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_FREQUENCY_SHIFTER bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_PITCH_SHIFTER"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_PITCH_SHIFTER
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_PITCH_SHIFTER bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_DISTORTION"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_DISTORTION
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_DISTORTION bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_TRANSIENT_SHAPER"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_TRANSIENT_SHAPER
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_TRANSIENT_SHAPER bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_COMPRESSOR"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_COMPRESSOR
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_COMPRESSOR bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_EXPANDER"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_EXPANDER
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_EXPANDER bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_GATE"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_GATE
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_GATE bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_LIMITER"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_LIMITER
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_LIMITER bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_FLANGER"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_FLANGER
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_FLANGER bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_CHORUS"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_CHORUS
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_CHORUS bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_DELAY"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_DELAY
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_DELAY bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_REVERB"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_REVERB
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_REVERB bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_TREMOLO"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_TREMOLO
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_TREMOLO bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_GLITCH"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_GLITCH
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_GLITCH bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_UTILITY"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_UTILITY
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_UTILITY bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_PITCH_CORRECTION"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_PITCH_CORRECTION
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_PITCH_CORRECTION bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_RESTORATION"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_RESTORATION
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_RESTORATION bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_MULTI_EFFECTS"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_MULTI_EFFECTS
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_MULTI_EFFECTS bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_MIXING"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_MIXING
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_MIXING bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_MASTERING"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_MASTERING
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_MASTERING bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_MONO"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_MONO
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_MONO bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_STEREO"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_STEREO
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_STEREO bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_SURROUND"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_SURROUND
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_SURROUND bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FEATURE_AMBISONIC"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FEATURE_AMBISONIC
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FEATURE_AMBISONIC bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_BEATTIME_FACTOR"]
            .as_i64()
            .expect("integer ABI constant"),
        raw::CLAP_BEATTIME_FACTOR,
        "CLAP_BEATTIME_FACTOR value"
    );
    assert_eq!(
        report["constants"]["CLAP_SECTIME_FACTOR"]
            .as_i64()
            .expect("integer ABI constant"),
        raw::CLAP_SECTIME_FACTOR,
        "CLAP_SECTIME_FACTOR value"
    );
    assert_eq!(
        report["constants"]["CLAP_INVALID_ID"].as_u64().expect("integer ABI constant"),
        u64::from(raw::CLAP_INVALID_ID),
        "CLAP_INVALID_ID value"
    );
    assert_eq!(
        report["constants"]["CLAP_CORE_EVENT_SPACE_ID"]
            .as_u64()
            .expect("integer ABI constant"),
        u64::from(raw::CLAP_CORE_EVENT_SPACE_ID),
        "CLAP_CORE_EVENT_SPACE_ID value"
    );
    assert_eq!(
        report["constants"]["CLAP_EVENT_IS_LIVE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_EVENT_IS_LIVE),
        "CLAP_EVENT_IS_LIVE value"
    );
    assert_eq!(
        report["constants"]["CLAP_EVENT_DONT_RECORD"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_EVENT_DONT_RECORD),
        "CLAP_EVENT_DONT_RECORD value"
    );
    assert_eq!(
        report["constants"]["CLAP_EVENT_NOTE_ON"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_EVENT_NOTE_ON),
        "CLAP_EVENT_NOTE_ON value"
    );
    assert_eq!(
        report["constants"]["CLAP_EVENT_NOTE_OFF"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_EVENT_NOTE_OFF),
        "CLAP_EVENT_NOTE_OFF value"
    );
    assert_eq!(
        report["constants"]["CLAP_EVENT_NOTE_CHOKE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_EVENT_NOTE_CHOKE),
        "CLAP_EVENT_NOTE_CHOKE value"
    );
    assert_eq!(
        report["constants"]["CLAP_EVENT_NOTE_END"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_EVENT_NOTE_END),
        "CLAP_EVENT_NOTE_END value"
    );
    assert_eq!(
        report["constants"]["CLAP_EVENT_NOTE_EXPRESSION"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_EVENT_NOTE_EXPRESSION),
        "CLAP_EVENT_NOTE_EXPRESSION value"
    );
    assert_eq!(
        report["constants"]["CLAP_EVENT_PARAM_VALUE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_EVENT_PARAM_VALUE),
        "CLAP_EVENT_PARAM_VALUE value"
    );
    assert_eq!(
        report["constants"]["CLAP_EVENT_PARAM_MOD"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_EVENT_PARAM_MOD),
        "CLAP_EVENT_PARAM_MOD value"
    );
    assert_eq!(
        report["constants"]["CLAP_EVENT_PARAM_GESTURE_BEGIN"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_EVENT_PARAM_GESTURE_BEGIN),
        "CLAP_EVENT_PARAM_GESTURE_BEGIN value"
    );
    assert_eq!(
        report["constants"]["CLAP_EVENT_PARAM_GESTURE_END"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_EVENT_PARAM_GESTURE_END),
        "CLAP_EVENT_PARAM_GESTURE_END value"
    );
    assert_eq!(
        report["constants"]["CLAP_EVENT_TRANSPORT"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_EVENT_TRANSPORT),
        "CLAP_EVENT_TRANSPORT value"
    );
    assert_eq!(
        report["constants"]["CLAP_EVENT_MIDI"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_EVENT_MIDI),
        "CLAP_EVENT_MIDI value"
    );
    assert_eq!(
        report["constants"]["CLAP_EVENT_MIDI_SYSEX"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_EVENT_MIDI_SYSEX),
        "CLAP_EVENT_MIDI_SYSEX value"
    );
    assert_eq!(
        report["constants"]["CLAP_EVENT_MIDI2"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_EVENT_MIDI2),
        "CLAP_EVENT_MIDI2 value"
    );
    assert_eq!(
        report["constants"]["CLAP_NOTE_EXPRESSION_VOLUME"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_NOTE_EXPRESSION_VOLUME),
        "CLAP_NOTE_EXPRESSION_VOLUME value"
    );
    assert_eq!(
        report["constants"]["CLAP_NOTE_EXPRESSION_PAN"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_NOTE_EXPRESSION_PAN),
        "CLAP_NOTE_EXPRESSION_PAN value"
    );
    assert_eq!(
        report["constants"]["CLAP_NOTE_EXPRESSION_TUNING"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_NOTE_EXPRESSION_TUNING),
        "CLAP_NOTE_EXPRESSION_TUNING value"
    );
    assert_eq!(
        report["constants"]["CLAP_NOTE_EXPRESSION_VIBRATO"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_NOTE_EXPRESSION_VIBRATO),
        "CLAP_NOTE_EXPRESSION_VIBRATO value"
    );
    assert_eq!(
        report["constants"]["CLAP_NOTE_EXPRESSION_EXPRESSION"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_NOTE_EXPRESSION_EXPRESSION),
        "CLAP_NOTE_EXPRESSION_EXPRESSION value"
    );
    assert_eq!(
        report["constants"]["CLAP_NOTE_EXPRESSION_BRIGHTNESS"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_NOTE_EXPRESSION_BRIGHTNESS),
        "CLAP_NOTE_EXPRESSION_BRIGHTNESS value"
    );
    assert_eq!(
        report["constants"]["CLAP_NOTE_EXPRESSION_PRESSURE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_NOTE_EXPRESSION_PRESSURE),
        "CLAP_NOTE_EXPRESSION_PRESSURE value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRANSPORT_HAS_TEMPO"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRANSPORT_HAS_TEMPO),
        "CLAP_TRANSPORT_HAS_TEMPO value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRANSPORT_HAS_BEATS_TIMELINE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRANSPORT_HAS_BEATS_TIMELINE),
        "CLAP_TRANSPORT_HAS_BEATS_TIMELINE value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRANSPORT_HAS_SECONDS_TIMELINE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRANSPORT_HAS_SECONDS_TIMELINE),
        "CLAP_TRANSPORT_HAS_SECONDS_TIMELINE value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRANSPORT_HAS_TIME_SIGNATURE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRANSPORT_HAS_TIME_SIGNATURE),
        "CLAP_TRANSPORT_HAS_TIME_SIGNATURE value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRANSPORT_IS_PLAYING"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRANSPORT_IS_PLAYING),
        "CLAP_TRANSPORT_IS_PLAYING value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRANSPORT_IS_RECORDING"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRANSPORT_IS_RECORDING),
        "CLAP_TRANSPORT_IS_RECORDING value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRANSPORT_IS_LOOP_ACTIVE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRANSPORT_IS_LOOP_ACTIVE),
        "CLAP_TRANSPORT_IS_LOOP_ACTIVE value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRANSPORT_IS_WITHIN_PRE_ROLL"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRANSPORT_IS_WITHIN_PRE_ROLL),
        "CLAP_TRANSPORT_IS_WITHIN_PRE_ROLL value"
    );
    assert_eq!(
        report["constants"]["CLAP_PROCESS_ERROR"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PROCESS_ERROR),
        "CLAP_PROCESS_ERROR value"
    );
    assert_eq!(
        report["constants"]["CLAP_PROCESS_CONTINUE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PROCESS_CONTINUE),
        "CLAP_PROCESS_CONTINUE value"
    );
    assert_eq!(
        report["constants"]["CLAP_PROCESS_CONTINUE_IF_NOT_QUIET"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PROCESS_CONTINUE_IF_NOT_QUIET),
        "CLAP_PROCESS_CONTINUE_IF_NOT_QUIET value"
    );
    assert_eq!(
        report["constants"]["CLAP_PROCESS_TAIL"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_PROCESS_TAIL),
        "CLAP_PROCESS_TAIL value"
    );
    assert_eq!(
        report["constants"]["CLAP_PROCESS_SLEEP"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PROCESS_SLEEP),
        "CLAP_PROCESS_SLEEP value"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_FACTORY_ID"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_FACTORY_ID
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_FACTORY_ID bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_TIMESTAMP_UNKNOWN"]
            .as_u64()
            .expect("integer ABI constant"),
        raw::CLAP_TIMESTAMP_UNKNOWN,
        "CLAP_TIMESTAMP_UNKNOWN value"
    );
    assert_eq!(
        report["constants"]["CLAP_PRESET_DISCOVERY_FACTORY_ID"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PRESET_DISCOVERY_FACTORY_ID
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PRESET_DISCOVERY_FACTORY_ID bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PRESET_DISCOVERY_FACTORY_ID_COMPAT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PRESET_DISCOVERY_FACTORY_ID_COMPAT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PRESET_DISCOVERY_FACTORY_ID_COMPAT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PRESET_DISCOVERY_LOCATION_FILE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PRESET_DISCOVERY_LOCATION_FILE),
        "CLAP_PRESET_DISCOVERY_LOCATION_FILE value"
    );
    assert_eq!(
        report["constants"]["CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN),
        "CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN value"
    );
    assert_eq!(
        report["constants"]["CLAP_PRESET_DISCOVERY_IS_FACTORY_CONTENT"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PRESET_DISCOVERY_IS_FACTORY_CONTENT),
        "CLAP_PRESET_DISCOVERY_IS_FACTORY_CONTENT value"
    );
    assert_eq!(
        report["constants"]["CLAP_PRESET_DISCOVERY_IS_USER_CONTENT"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PRESET_DISCOVERY_IS_USER_CONTENT),
        "CLAP_PRESET_DISCOVERY_IS_USER_CONTENT value"
    );
    assert_eq!(
        report["constants"]["CLAP_PRESET_DISCOVERY_IS_DEMO_CONTENT"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PRESET_DISCOVERY_IS_DEMO_CONTENT),
        "CLAP_PRESET_DISCOVERY_IS_DEMO_CONTENT value"
    );
    assert_eq!(
        report["constants"]["CLAP_PRESET_DISCOVERY_IS_FAVORITE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PRESET_DISCOVERY_IS_FAVORITE),
        "CLAP_PRESET_DISCOVERY_IS_FAVORITE value"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_AMBISONIC"].as_str().expect("hex string constant"),
        raw::CLAP_EXT_AMBISONIC
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_AMBISONIC bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_AMBISONIC_COMPAT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_AMBISONIC_COMPAT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_AMBISONIC_COMPAT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PORT_AMBISONIC"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PORT_AMBISONIC
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PORT_AMBISONIC bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_AMBISONIC_ORDERING_FUMA"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_AMBISONIC_ORDERING_FUMA),
        "CLAP_AMBISONIC_ORDERING_FUMA value"
    );
    assert_eq!(
        report["constants"]["CLAP_AMBISONIC_ORDERING_ACN"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_AMBISONIC_ORDERING_ACN),
        "CLAP_AMBISONIC_ORDERING_ACN value"
    );
    assert_eq!(
        report["constants"]["CLAP_AMBISONIC_NORMALIZATION_MAXN"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_AMBISONIC_NORMALIZATION_MAXN),
        "CLAP_AMBISONIC_NORMALIZATION_MAXN value"
    );
    assert_eq!(
        report["constants"]["CLAP_AMBISONIC_NORMALIZATION_SN3D"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_AMBISONIC_NORMALIZATION_SN3D),
        "CLAP_AMBISONIC_NORMALIZATION_SN3D value"
    );
    assert_eq!(
        report["constants"]["CLAP_AMBISONIC_NORMALIZATION_N3D"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_AMBISONIC_NORMALIZATION_N3D),
        "CLAP_AMBISONIC_NORMALIZATION_N3D value"
    );
    assert_eq!(
        report["constants"]["CLAP_AMBISONIC_NORMALIZATION_SN2D"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_AMBISONIC_NORMALIZATION_SN2D),
        "CLAP_AMBISONIC_NORMALIZATION_SN2D value"
    );
    assert_eq!(
        report["constants"]["CLAP_AMBISONIC_NORMALIZATION_N2D"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_AMBISONIC_NORMALIZATION_N2D),
        "CLAP_AMBISONIC_NORMALIZATION_N2D value"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_AUDIO_PORTS_ACTIVATION"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_AUDIO_PORTS_ACTIVATION
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_AUDIO_PORTS_ACTIVATION bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_AUDIO_PORTS_ACTIVATION_COMPAT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_AUDIO_PORTS_ACTIVATION_COMPAT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_AUDIO_PORTS_ACTIVATION_COMPAT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_NAME_SIZE"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_NAME_SIZE),
        "CLAP_NAME_SIZE value"
    );
    assert_eq!(
        report["constants"]["CLAP_PATH_SIZE"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_PATH_SIZE),
        "CLAP_PATH_SIZE value"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_AUDIO_PORTS"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_AUDIO_PORTS
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_AUDIO_PORTS bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PORT_MONO"].as_str().expect("hex string constant"),
        raw::CLAP_PORT_MONO
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PORT_MONO bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PORT_STEREO"].as_str().expect("hex string constant"),
        raw::CLAP_PORT_STEREO
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PORT_STEREO bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_AUDIO_PORT_IS_MAIN"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_AUDIO_PORT_IS_MAIN),
        "CLAP_AUDIO_PORT_IS_MAIN value"
    );
    assert_eq!(
        report["constants"]["CLAP_AUDIO_PORT_SUPPORTS_64BITS"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_AUDIO_PORT_SUPPORTS_64BITS),
        "CLAP_AUDIO_PORT_SUPPORTS_64BITS value"
    );
    assert_eq!(
        report["constants"]["CLAP_AUDIO_PORT_PREFERS_64BITS"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_AUDIO_PORT_PREFERS_64BITS),
        "CLAP_AUDIO_PORT_PREFERS_64BITS value"
    );
    assert_eq!(
        report["constants"]["CLAP_AUDIO_PORT_REQUIRES_COMMON_SAMPLE_SIZE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_AUDIO_PORT_REQUIRES_COMMON_SAMPLE_SIZE),
        "CLAP_AUDIO_PORT_REQUIRES_COMMON_SAMPLE_SIZE value"
    );
    assert_eq!(
        report["constants"]["CLAP_AUDIO_PORTS_RESCAN_NAMES"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_AUDIO_PORTS_RESCAN_NAMES),
        "CLAP_AUDIO_PORTS_RESCAN_NAMES value"
    );
    assert_eq!(
        report["constants"]["CLAP_AUDIO_PORTS_RESCAN_FLAGS"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_AUDIO_PORTS_RESCAN_FLAGS),
        "CLAP_AUDIO_PORTS_RESCAN_FLAGS value"
    );
    assert_eq!(
        report["constants"]["CLAP_AUDIO_PORTS_RESCAN_CHANNEL_COUNT"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_AUDIO_PORTS_RESCAN_CHANNEL_COUNT),
        "CLAP_AUDIO_PORTS_RESCAN_CHANNEL_COUNT value"
    );
    assert_eq!(
        report["constants"]["CLAP_AUDIO_PORTS_RESCAN_PORT_TYPE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_AUDIO_PORTS_RESCAN_PORT_TYPE),
        "CLAP_AUDIO_PORTS_RESCAN_PORT_TYPE value"
    );
    assert_eq!(
        report["constants"]["CLAP_AUDIO_PORTS_RESCAN_IN_PLACE_PAIR"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_AUDIO_PORTS_RESCAN_IN_PLACE_PAIR),
        "CLAP_AUDIO_PORTS_RESCAN_IN_PLACE_PAIR value"
    );
    assert_eq!(
        report["constants"]["CLAP_AUDIO_PORTS_RESCAN_LIST"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_AUDIO_PORTS_RESCAN_LIST),
        "CLAP_AUDIO_PORTS_RESCAN_LIST value"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_AUDIO_PORTS_CONFIG"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_AUDIO_PORTS_CONFIG
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_AUDIO_PORTS_CONFIG bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_AUDIO_PORTS_CONFIG_INFO"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_AUDIO_PORTS_CONFIG_INFO
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_AUDIO_PORTS_CONFIG_INFO bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_AUDIO_PORTS_CONFIG_INFO_COMPAT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_AUDIO_PORTS_CONFIG_INFO_COMPAT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_AUDIO_PORTS_CONFIG_INFO_COMPAT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_CONFIGURABLE_AUDIO_PORTS"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_CONFIGURABLE_AUDIO_PORTS
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_CONFIGURABLE_AUDIO_PORTS bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_CONFIGURABLE_AUDIO_PORTS_COMPAT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_CONFIGURABLE_AUDIO_PORTS_COMPAT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_CONFIGURABLE_AUDIO_PORTS_COMPAT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_CONTEXT_MENU"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_CONTEXT_MENU
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_CONTEXT_MENU bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_CONTEXT_MENU_COMPAT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_CONTEXT_MENU_COMPAT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_CONTEXT_MENU_COMPAT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_CONTEXT_MENU_TARGET_KIND_GLOBAL"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_CONTEXT_MENU_TARGET_KIND_GLOBAL),
        "CLAP_CONTEXT_MENU_TARGET_KIND_GLOBAL value"
    );
    assert_eq!(
        report["constants"]["CLAP_CONTEXT_MENU_TARGET_KIND_PARAM"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_CONTEXT_MENU_TARGET_KIND_PARAM),
        "CLAP_CONTEXT_MENU_TARGET_KIND_PARAM value"
    );
    assert_eq!(
        report["constants"]["CLAP_CONTEXT_MENU_ITEM_ENTRY"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_CONTEXT_MENU_ITEM_ENTRY),
        "CLAP_CONTEXT_MENU_ITEM_ENTRY value"
    );
    assert_eq!(
        report["constants"]["CLAP_CONTEXT_MENU_ITEM_CHECK_ENTRY"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_CONTEXT_MENU_ITEM_CHECK_ENTRY),
        "CLAP_CONTEXT_MENU_ITEM_CHECK_ENTRY value"
    );
    assert_eq!(
        report["constants"]["CLAP_CONTEXT_MENU_ITEM_SEPARATOR"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_CONTEXT_MENU_ITEM_SEPARATOR),
        "CLAP_CONTEXT_MENU_ITEM_SEPARATOR value"
    );
    assert_eq!(
        report["constants"]["CLAP_CONTEXT_MENU_ITEM_BEGIN_SUBMENU"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_CONTEXT_MENU_ITEM_BEGIN_SUBMENU),
        "CLAP_CONTEXT_MENU_ITEM_BEGIN_SUBMENU value"
    );
    assert_eq!(
        report["constants"]["CLAP_CONTEXT_MENU_ITEM_END_SUBMENU"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_CONTEXT_MENU_ITEM_END_SUBMENU),
        "CLAP_CONTEXT_MENU_ITEM_END_SUBMENU value"
    );
    assert_eq!(
        report["constants"]["CLAP_CONTEXT_MENU_ITEM_TITLE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_CONTEXT_MENU_ITEM_TITLE),
        "CLAP_CONTEXT_MENU_ITEM_TITLE value"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_EVENT_REGISTRY"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_EVENT_REGISTRY
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_EVENT_REGISTRY bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_GUI"].as_str().expect("hex string constant"),
        raw::CLAP_EXT_GUI
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_GUI bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_WINDOW_API_WIN32"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_WINDOW_API_WIN32
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_WINDOW_API_WIN32 bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_WINDOW_API_COCOA"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_WINDOW_API_COCOA
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_WINDOW_API_COCOA bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_WINDOW_API_UIKIT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_WINDOW_API_UIKIT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_WINDOW_API_UIKIT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_WINDOW_API_X11"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_WINDOW_API_X11
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_WINDOW_API_X11 bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_WINDOW_API_WAYLAND"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_WINDOW_API_WAYLAND
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_WINDOW_API_WAYLAND bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_LATENCY"].as_str().expect("hex string constant"),
        raw::CLAP_EXT_LATENCY
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_LATENCY bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_LOG"].as_str().expect("hex string constant"),
        raw::CLAP_EXT_LOG
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_LOG bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_LOG_DEBUG"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_LOG_DEBUG),
        "CLAP_LOG_DEBUG value"
    );
    assert_eq!(
        report["constants"]["CLAP_LOG_INFO"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_LOG_INFO),
        "CLAP_LOG_INFO value"
    );
    assert_eq!(
        report["constants"]["CLAP_LOG_WARNING"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_LOG_WARNING),
        "CLAP_LOG_WARNING value"
    );
    assert_eq!(
        report["constants"]["CLAP_LOG_ERROR"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_LOG_ERROR),
        "CLAP_LOG_ERROR value"
    );
    assert_eq!(
        report["constants"]["CLAP_LOG_FATAL"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_LOG_FATAL),
        "CLAP_LOG_FATAL value"
    );
    assert_eq!(
        report["constants"]["CLAP_LOG_HOST_MISBEHAVING"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_LOG_HOST_MISBEHAVING),
        "CLAP_LOG_HOST_MISBEHAVING value"
    );
    assert_eq!(
        report["constants"]["CLAP_LOG_PLUGIN_MISBEHAVING"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_LOG_PLUGIN_MISBEHAVING),
        "CLAP_LOG_PLUGIN_MISBEHAVING value"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_NOTE_NAME"].as_str().expect("hex string constant"),
        raw::CLAP_EXT_NOTE_NAME
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_NOTE_NAME bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_NOTE_PORTS"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_NOTE_PORTS
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_NOTE_PORTS bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_NOTE_DIALECT_CLAP"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_NOTE_DIALECT_CLAP),
        "CLAP_NOTE_DIALECT_CLAP value"
    );
    assert_eq!(
        report["constants"]["CLAP_NOTE_DIALECT_MIDI"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_NOTE_DIALECT_MIDI),
        "CLAP_NOTE_DIALECT_MIDI value"
    );
    assert_eq!(
        report["constants"]["CLAP_NOTE_DIALECT_MIDI_MPE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_NOTE_DIALECT_MIDI_MPE),
        "CLAP_NOTE_DIALECT_MIDI_MPE value"
    );
    assert_eq!(
        report["constants"]["CLAP_NOTE_DIALECT_MIDI2"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_NOTE_DIALECT_MIDI2),
        "CLAP_NOTE_DIALECT_MIDI2 value"
    );
    assert_eq!(
        report["constants"]["CLAP_NOTE_PORTS_RESCAN_ALL"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_NOTE_PORTS_RESCAN_ALL),
        "CLAP_NOTE_PORTS_RESCAN_ALL value"
    );
    assert_eq!(
        report["constants"]["CLAP_NOTE_PORTS_RESCAN_NAMES"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_NOTE_PORTS_RESCAN_NAMES),
        "CLAP_NOTE_PORTS_RESCAN_NAMES value"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_PARAMS"].as_str().expect("hex string constant"),
        raw::CLAP_EXT_PARAMS
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_PARAMS bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_IS_STEPPED"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_IS_STEPPED),
        "CLAP_PARAM_IS_STEPPED value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_IS_PERIODIC"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_IS_PERIODIC),
        "CLAP_PARAM_IS_PERIODIC value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_IS_HIDDEN"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_IS_HIDDEN),
        "CLAP_PARAM_IS_HIDDEN value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_IS_READONLY"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_IS_READONLY),
        "CLAP_PARAM_IS_READONLY value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_IS_BYPASS"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_IS_BYPASS),
        "CLAP_PARAM_IS_BYPASS value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_IS_AUTOMATABLE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_IS_AUTOMATABLE),
        "CLAP_PARAM_IS_AUTOMATABLE value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_IS_AUTOMATABLE_PER_NOTE_ID"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_IS_AUTOMATABLE_PER_NOTE_ID),
        "CLAP_PARAM_IS_AUTOMATABLE_PER_NOTE_ID value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_IS_AUTOMATABLE_PER_KEY"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_IS_AUTOMATABLE_PER_KEY),
        "CLAP_PARAM_IS_AUTOMATABLE_PER_KEY value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_IS_AUTOMATABLE_PER_CHANNEL"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_IS_AUTOMATABLE_PER_CHANNEL),
        "CLAP_PARAM_IS_AUTOMATABLE_PER_CHANNEL value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_IS_AUTOMATABLE_PER_PORT"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_IS_AUTOMATABLE_PER_PORT),
        "CLAP_PARAM_IS_AUTOMATABLE_PER_PORT value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_IS_MODULATABLE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_IS_MODULATABLE),
        "CLAP_PARAM_IS_MODULATABLE value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID),
        "CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_IS_MODULATABLE_PER_KEY"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_IS_MODULATABLE_PER_KEY),
        "CLAP_PARAM_IS_MODULATABLE_PER_KEY value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_IS_MODULATABLE_PER_CHANNEL"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_IS_MODULATABLE_PER_CHANNEL),
        "CLAP_PARAM_IS_MODULATABLE_PER_CHANNEL value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_IS_MODULATABLE_PER_PORT"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_IS_MODULATABLE_PER_PORT),
        "CLAP_PARAM_IS_MODULATABLE_PER_PORT value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_REQUIRES_PROCESS"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_REQUIRES_PROCESS),
        "CLAP_PARAM_REQUIRES_PROCESS value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_IS_ENUM"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_IS_ENUM),
        "CLAP_PARAM_IS_ENUM value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_RESCAN_VALUES"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_RESCAN_VALUES),
        "CLAP_PARAM_RESCAN_VALUES value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_RESCAN_TEXT"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_RESCAN_TEXT),
        "CLAP_PARAM_RESCAN_TEXT value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_RESCAN_INFO"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_RESCAN_INFO),
        "CLAP_PARAM_RESCAN_INFO value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_RESCAN_ALL"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_RESCAN_ALL),
        "CLAP_PARAM_RESCAN_ALL value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_CLEAR_ALL"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_CLEAR_ALL),
        "CLAP_PARAM_CLEAR_ALL value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_CLEAR_AUTOMATIONS"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_CLEAR_AUTOMATIONS),
        "CLAP_PARAM_CLEAR_AUTOMATIONS value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_CLEAR_MODULATIONS"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_CLEAR_MODULATIONS),
        "CLAP_PARAM_CLEAR_MODULATIONS value"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_PARAM_INDICATION"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_PARAM_INDICATION
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_PARAM_INDICATION bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_PARAM_INDICATION_COMPAT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_PARAM_INDICATION_COMPAT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_PARAM_INDICATION_COMPAT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_INDICATION_AUTOMATION_NONE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_INDICATION_AUTOMATION_NONE),
        "CLAP_PARAM_INDICATION_AUTOMATION_NONE value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_INDICATION_AUTOMATION_PRESENT"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_INDICATION_AUTOMATION_PRESENT),
        "CLAP_PARAM_INDICATION_AUTOMATION_PRESENT value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_INDICATION_AUTOMATION_PLAYING"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_INDICATION_AUTOMATION_PLAYING),
        "CLAP_PARAM_INDICATION_AUTOMATION_PLAYING value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_INDICATION_AUTOMATION_RECORDING"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_INDICATION_AUTOMATION_RECORDING),
        "CLAP_PARAM_INDICATION_AUTOMATION_RECORDING value"
    );
    assert_eq!(
        report["constants"]["CLAP_PARAM_INDICATION_AUTOMATION_OVERRIDING"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PARAM_INDICATION_AUTOMATION_OVERRIDING),
        "CLAP_PARAM_INDICATION_AUTOMATION_OVERRIDING value"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_POSIX_FD_SUPPORT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_POSIX_FD_SUPPORT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_POSIX_FD_SUPPORT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_POSIX_FD_READ"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_POSIX_FD_READ),
        "CLAP_POSIX_FD_READ value"
    );
    assert_eq!(
        report["constants"]["CLAP_POSIX_FD_WRITE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_POSIX_FD_WRITE),
        "CLAP_POSIX_FD_WRITE value"
    );
    assert_eq!(
        report["constants"]["CLAP_POSIX_FD_ERROR"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_POSIX_FD_ERROR),
        "CLAP_POSIX_FD_ERROR value"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_PRESET_LOAD"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_PRESET_LOAD
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_PRESET_LOAD bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_PRESET_LOAD_COMPAT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_PRESET_LOAD_COMPAT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_PRESET_LOAD_COMPAT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_REMOTE_CONTROLS"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_REMOTE_CONTROLS
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_REMOTE_CONTROLS bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_REMOTE_CONTROLS_COMPAT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_REMOTE_CONTROLS_COMPAT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_REMOTE_CONTROLS_COMPAT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_REMOTE_CONTROLS_COUNT"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_REMOTE_CONTROLS_COUNT),
        "CLAP_REMOTE_CONTROLS_COUNT value"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_RENDER"].as_str().expect("hex string constant"),
        raw::CLAP_EXT_RENDER
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_RENDER bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_RENDER_REALTIME"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_RENDER_REALTIME),
        "CLAP_RENDER_REALTIME value"
    );
    assert_eq!(
        report["constants"]["CLAP_RENDER_OFFLINE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_RENDER_OFFLINE),
        "CLAP_RENDER_OFFLINE value"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_STATE_CONTEXT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_STATE_CONTEXT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_STATE_CONTEXT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_STATE_CONTEXT_FOR_PRESET"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_STATE_CONTEXT_FOR_PRESET),
        "CLAP_STATE_CONTEXT_FOR_PRESET value"
    );
    assert_eq!(
        report["constants"]["CLAP_STATE_CONTEXT_FOR_DUPLICATE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_STATE_CONTEXT_FOR_DUPLICATE),
        "CLAP_STATE_CONTEXT_FOR_DUPLICATE value"
    );
    assert_eq!(
        report["constants"]["CLAP_STATE_CONTEXT_FOR_PROJECT"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_STATE_CONTEXT_FOR_PROJECT),
        "CLAP_STATE_CONTEXT_FOR_PROJECT value"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_STATE"].as_str().expect("hex string constant"),
        raw::CLAP_EXT_STATE
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_STATE bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_SURROUND"].as_str().expect("hex string constant"),
        raw::CLAP_EXT_SURROUND
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_SURROUND bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_SURROUND_COMPAT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_SURROUND_COMPAT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_SURROUND_COMPAT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PORT_SURROUND"].as_str().expect("hex string constant"),
        raw::CLAP_PORT_SURROUND
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PORT_SURROUND bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_FL"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_FL),
        "CLAP_SURROUND_FL value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_FR"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_FR),
        "CLAP_SURROUND_FR value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_FC"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_FC),
        "CLAP_SURROUND_FC value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_LFE"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_LFE),
        "CLAP_SURROUND_LFE value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_BL"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_BL),
        "CLAP_SURROUND_BL value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_BR"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_BR),
        "CLAP_SURROUND_BR value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_FLC"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_FLC),
        "CLAP_SURROUND_FLC value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_FRC"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_FRC),
        "CLAP_SURROUND_FRC value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_BC"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_BC),
        "CLAP_SURROUND_BC value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_SL"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_SL),
        "CLAP_SURROUND_SL value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_SR"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_SR),
        "CLAP_SURROUND_SR value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_TC"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_TC),
        "CLAP_SURROUND_TC value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_TFL"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_TFL),
        "CLAP_SURROUND_TFL value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_TFC"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_TFC),
        "CLAP_SURROUND_TFC value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_TFR"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_TFR),
        "CLAP_SURROUND_TFR value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_TBL"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_TBL),
        "CLAP_SURROUND_TBL value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_TBC"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_TBC),
        "CLAP_SURROUND_TBC value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_TBR"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_TBR),
        "CLAP_SURROUND_TBR value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_TSL"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_TSL),
        "CLAP_SURROUND_TSL value"
    );
    assert_eq!(
        report["constants"]["CLAP_SURROUND_TSR"].as_i64().expect("integer ABI constant"),
        i64::from(raw::CLAP_SURROUND_TSR),
        "CLAP_SURROUND_TSR value"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_TAIL"].as_str().expect("hex string constant"),
        raw::CLAP_EXT_TAIL
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_TAIL bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_THREAD_CHECK"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_THREAD_CHECK
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_THREAD_CHECK bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_THREAD_POOL"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_THREAD_POOL
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_THREAD_POOL bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_TIMER_SUPPORT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_TIMER_SUPPORT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_TIMER_SUPPORT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_TRACK_INFO"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_TRACK_INFO
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_TRACK_INFO bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_TRACK_INFO_COMPAT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_TRACK_INFO_COMPAT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_TRACK_INFO_COMPAT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_TRACK_INFO_HAS_TRACK_NAME"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRACK_INFO_HAS_TRACK_NAME),
        "CLAP_TRACK_INFO_HAS_TRACK_NAME value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRACK_INFO_HAS_TRACK_COLOR"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRACK_INFO_HAS_TRACK_COLOR),
        "CLAP_TRACK_INFO_HAS_TRACK_COLOR value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRACK_INFO_HAS_AUDIO_CHANNEL"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRACK_INFO_HAS_AUDIO_CHANNEL),
        "CLAP_TRACK_INFO_HAS_AUDIO_CHANNEL value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRACK_INFO_IS_FOR_RETURN_TRACK"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRACK_INFO_IS_FOR_RETURN_TRACK),
        "CLAP_TRACK_INFO_IS_FOR_RETURN_TRACK value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRACK_INFO_IS_FOR_BUS"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRACK_INFO_IS_FOR_BUS),
        "CLAP_TRACK_INFO_IS_FOR_BUS value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRACK_INFO_IS_FOR_MASTER"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRACK_INFO_IS_FOR_MASTER),
        "CLAP_TRACK_INFO_IS_FOR_MASTER value"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_VOICE_INFO"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_VOICE_INFO
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_VOICE_INFO bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_VOICE_INFO_SUPPORTS_OVERLAPPING_NOTES"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_VOICE_INFO_SUPPORTS_OVERLAPPING_NOTES),
        "CLAP_VOICE_INFO_SUPPORTS_OVERLAPPING_NOTES value"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_INVALIDATION_FACTORY_ID"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_INVALIDATION_FACTORY_ID
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_INVALIDATION_FACTORY_ID bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PLUGIN_STATE_CONVERTER_FACTORY_ID"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_PLUGIN_STATE_CONVERTER_FACTORY_ID
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_PLUGIN_STATE_CONVERTER_FACTORY_ID bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_BACKGROUND_ACTIVATION"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_BACKGROUND_ACTIVATION
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_BACKGROUND_ACTIVATION bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_BACKGROUND_PROGRESS"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_BACKGROUND_PROGRESS
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_BACKGROUND_PROGRESS bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_BACKGROUND_STATE_CONTEXT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_BACKGROUND_STATE_CONTEXT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_BACKGROUND_STATE_CONTEXT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_EXTENSIBLE_AUDIO_PORTS"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_EXTENSIBLE_AUDIO_PORTS
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_EXTENSIBLE_AUDIO_PORTS bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_FLUSH_EVENTS"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_FLUSH_EVENTS
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_FLUSH_EVENTS bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_GAIN_ADJUSTMENT_METERING"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_GAIN_ADJUSTMENT_METERING
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_GAIN_ADJUSTMENT_METERING bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_MINI_CURVE_DISPLAY"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_MINI_CURVE_DISPLAY
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_MINI_CURVE_DISPLAY bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_MINI_CURVE_DISPLAY_CURVE_KIND_UNSPECIFIED"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_MINI_CURVE_DISPLAY_CURVE_KIND_UNSPECIFIED),
        "CLAP_MINI_CURVE_DISPLAY_CURVE_KIND_UNSPECIFIED value"
    );
    assert_eq!(
        report["constants"]["CLAP_MINI_CURVE_DISPLAY_CURVE_KIND_GAIN_RESPONSE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_MINI_CURVE_DISPLAY_CURVE_KIND_GAIN_RESPONSE),
        "CLAP_MINI_CURVE_DISPLAY_CURVE_KIND_GAIN_RESPONSE value"
    );
    assert_eq!(
        report["constants"]["CLAP_MINI_CURVE_DISPLAY_CURVE_KIND_PHASE_RESPONSE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_MINI_CURVE_DISPLAY_CURVE_KIND_PHASE_RESPONSE),
        "CLAP_MINI_CURVE_DISPLAY_CURVE_KIND_PHASE_RESPONSE value"
    );
    assert_eq!(
        report["constants"]["CLAP_MINI_CURVE_DISPLAY_CURVE_KIND_TRANSFER_CURVE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_MINI_CURVE_DISPLAY_CURVE_KIND_TRANSFER_CURVE),
        "CLAP_MINI_CURVE_DISPLAY_CURVE_KIND_TRANSFER_CURVE value"
    );
    assert_eq!(
        report["constants"]["CLAP_MINI_CURVE_DISPLAY_CURVE_KIND_GAIN_REDUCTION"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_MINI_CURVE_DISPLAY_CURVE_KIND_GAIN_REDUCTION),
        "CLAP_MINI_CURVE_DISPLAY_CURVE_KIND_GAIN_REDUCTION value"
    );
    assert_eq!(
        report["constants"]["CLAP_MINI_CURVE_DISPLAY_CURVE_KIND_TIME_SERIES"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_MINI_CURVE_DISPLAY_CURVE_KIND_TIME_SERIES),
        "CLAP_MINI_CURVE_DISPLAY_CURVE_KIND_TIME_SERIES value"
    );
    assert_eq!(
        report["constants"]["CLAP_MINI_CURVE_DISPLAY_CURVE_CHANGED"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_MINI_CURVE_DISPLAY_CURVE_CHANGED),
        "CLAP_MINI_CURVE_DISPLAY_CURVE_CHANGED value"
    );
    assert_eq!(
        report["constants"]["CLAP_MINI_CURVE_DISPLAY_AXIS_NAME_CHANGED"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_MINI_CURVE_DISPLAY_AXIS_NAME_CHANGED),
        "CLAP_MINI_CURVE_DISPLAY_AXIS_NAME_CHANGED value"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_OCTAVE_NUMBER"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_OCTAVE_NUMBER
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_OCTAVE_NUMBER bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_PARAMS_ORIGIN"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_PARAMS_ORIGIN
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_PARAMS_ORIGIN bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_PARAM_HOVERED"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_PARAM_HOVERED
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_PARAM_HOVERED bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_PROJECT_LOCATION"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_PROJECT_LOCATION
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_PROJECT_LOCATION bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_PROJECT_LOCATION_PROJECT"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PROJECT_LOCATION_PROJECT),
        "CLAP_PROJECT_LOCATION_PROJECT value"
    );
    assert_eq!(
        report["constants"]["CLAP_PROJECT_LOCATION_TRACK_GROUP"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PROJECT_LOCATION_TRACK_GROUP),
        "CLAP_PROJECT_LOCATION_TRACK_GROUP value"
    );
    assert_eq!(
        report["constants"]["CLAP_PROJECT_LOCATION_TRACK"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PROJECT_LOCATION_TRACK),
        "CLAP_PROJECT_LOCATION_TRACK value"
    );
    assert_eq!(
        report["constants"]["CLAP_PROJECT_LOCATION_DEVICE"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PROJECT_LOCATION_DEVICE),
        "CLAP_PROJECT_LOCATION_DEVICE value"
    );
    assert_eq!(
        report["constants"]["CLAP_PROJECT_LOCATION_NESTED_DEVICE_CHAIN"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PROJECT_LOCATION_NESTED_DEVICE_CHAIN),
        "CLAP_PROJECT_LOCATION_NESTED_DEVICE_CHAIN value"
    );
    assert_eq!(
        report["constants"]["CLAP_PROJECT_LOCATION_INSTUMENT_TRACK"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PROJECT_LOCATION_INSTUMENT_TRACK),
        "CLAP_PROJECT_LOCATION_INSTUMENT_TRACK value"
    );
    assert_eq!(
        report["constants"]["CLAP_PROJECT_LOCATION_AUDIO_TRACK"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PROJECT_LOCATION_AUDIO_TRACK),
        "CLAP_PROJECT_LOCATION_AUDIO_TRACK value"
    );
    assert_eq!(
        report["constants"]["CLAP_PROJECT_LOCATION_HYBRID_TRACK"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PROJECT_LOCATION_HYBRID_TRACK),
        "CLAP_PROJECT_LOCATION_HYBRID_TRACK value"
    );
    assert_eq!(
        report["constants"]["CLAP_PROJECT_LOCATION_RETURN_TRACK"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PROJECT_LOCATION_RETURN_TRACK),
        "CLAP_PROJECT_LOCATION_RETURN_TRACK value"
    );
    assert_eq!(
        report["constants"]["CLAP_PROJECT_LOCATION_MASTER_TRACK"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PROJECT_LOCATION_MASTER_TRACK),
        "CLAP_PROJECT_LOCATION_MASTER_TRACK value"
    );
    assert_eq!(
        report["constants"]["CLAP_PROJECT_LOCATION_HAS_INDEX"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PROJECT_LOCATION_HAS_INDEX),
        "CLAP_PROJECT_LOCATION_HAS_INDEX value"
    );
    assert_eq!(
        report["constants"]["CLAP_PROJECT_LOCATION_HAS_COLOR"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_PROJECT_LOCATION_HAS_COLOR),
        "CLAP_PROJECT_LOCATION_HAS_COLOR value"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_RESOURCE_DIRECTORY"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_RESOURCE_DIRECTORY
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_RESOURCE_DIRECTORY bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_SCRATCH_MEMORY"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_SCRATCH_MEMORY
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_SCRATCH_MEMORY bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_TRANSPORT_CONTROL"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_TRANSPORT_CONTROL
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_TRANSPORT_CONTROL bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_TRIGGERS"].as_str().expect("hex string constant"),
        raw::CLAP_EXT_TRIGGERS
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_TRIGGERS bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_TRIGGER_IS_AUTOMATABLE_PER_NOTE_ID"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRIGGER_IS_AUTOMATABLE_PER_NOTE_ID),
        "CLAP_TRIGGER_IS_AUTOMATABLE_PER_NOTE_ID value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRIGGER_IS_AUTOMATABLE_PER_KEY"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRIGGER_IS_AUTOMATABLE_PER_KEY),
        "CLAP_TRIGGER_IS_AUTOMATABLE_PER_KEY value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRIGGER_IS_AUTOMATABLE_PER_CHANNEL"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRIGGER_IS_AUTOMATABLE_PER_CHANNEL),
        "CLAP_TRIGGER_IS_AUTOMATABLE_PER_CHANNEL value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRIGGER_IS_AUTOMATABLE_PER_PORT"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRIGGER_IS_AUTOMATABLE_PER_PORT),
        "CLAP_TRIGGER_IS_AUTOMATABLE_PER_PORT value"
    );
    assert_eq!(
        report["constants"]["CLAP_EVENT_TRIGGER"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_EVENT_TRIGGER),
        "CLAP_EVENT_TRIGGER value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRIGGER_RESCAN_INFO"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRIGGER_RESCAN_INFO),
        "CLAP_TRIGGER_RESCAN_INFO value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRIGGER_RESCAN_ALL"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRIGGER_RESCAN_ALL),
        "CLAP_TRIGGER_RESCAN_ALL value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRIGGER_CLEAR_ALL"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRIGGER_CLEAR_ALL),
        "CLAP_TRIGGER_CLEAR_ALL value"
    );
    assert_eq!(
        report["constants"]["CLAP_TRIGGER_CLEAR_AUTOMATIONS"]
            .as_i64()
            .expect("integer ABI constant"),
        i64::from(raw::CLAP_TRIGGER_CLEAR_AUTOMATIONS),
        "CLAP_TRIGGER_CLEAR_AUTOMATIONS value"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_TUNING"].as_str().expect("hex string constant"),
        raw::CLAP_EXT_TUNING
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_TUNING bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_UNDO"].as_str().expect("hex string constant"),
        raw::CLAP_EXT_UNDO
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_UNDO bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_UNDO_CONTEXT"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_UNDO_CONTEXT
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_UNDO_CONTEXT bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_UNDO_DELTA"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_EXT_UNDO_DELTA
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_UNDO_DELTA bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_EXT_WEBVIEW"].as_str().expect("hex string constant"),
        raw::CLAP_EXT_WEBVIEW
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_EXT_WEBVIEW bytes"
    );
    assert_eq!(
        report["constants"]["CLAP_WINDOW_API_WEBVIEW"]
            .as_str()
            .expect("hex string constant"),
        raw::CLAP_WINDOW_API_WEBVIEW
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "CLAP_WINDOW_API_WEBVIEW bytes"
    );
    assert_eq!(
        report["aliases"]["clap_ambisonic_config_t"]["size"],
        size_of::<raw::clap_ambisonic_config_t>(),
        "clap_ambisonic_config_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_ambisonic_config_t"]["align"],
        align_of::<raw::clap_ambisonic_config_t>(),
        "clap_ambisonic_config_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_ambisonic_config_t"]["carrier"], "record",
        "clap_ambisonic_config_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_ambisonic_normalization"]["size"],
        size_of::<raw::clap_ambisonic_normalization>(),
        "clap_ambisonic_normalization alias size"
    );
    assert_eq!(
        report["aliases"]["clap_ambisonic_normalization"]["align"],
        align_of::<raw::clap_ambisonic_normalization>(),
        "clap_ambisonic_normalization alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_ambisonic_normalization"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_ambisonic_normalization native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_ambisonic_ordering"]["size"],
        size_of::<raw::clap_ambisonic_ordering>(),
        "clap_ambisonic_ordering alias size"
    );
    assert_eq!(
        report["aliases"]["clap_ambisonic_ordering"]["align"],
        align_of::<raw::clap_ambisonic_ordering>(),
        "clap_ambisonic_ordering alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_ambisonic_ordering"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_ambisonic_ordering native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_audio_buffer_t"]["size"],
        size_of::<raw::clap_audio_buffer_t>(),
        "clap_audio_buffer_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_audio_buffer_t"]["align"],
        align_of::<raw::clap_audio_buffer_t>(),
        "clap_audio_buffer_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_audio_buffer_t"]["carrier"], "record",
        "clap_audio_buffer_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_audio_port_configuration_request_t"]["size"],
        size_of::<raw::clap_audio_port_configuration_request_t>(),
        "clap_audio_port_configuration_request_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_audio_port_configuration_request_t"]["align"],
        align_of::<raw::clap_audio_port_configuration_request_t>(),
        "clap_audio_port_configuration_request_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_audio_port_configuration_request_t"]["carrier"], "record",
        "clap_audio_port_configuration_request_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_audio_port_info_t"]["size"],
        size_of::<raw::clap_audio_port_info_t>(),
        "clap_audio_port_info_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_audio_port_info_t"]["align"],
        align_of::<raw::clap_audio_port_info_t>(),
        "clap_audio_port_info_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_audio_port_info_t"]["carrier"], "record",
        "clap_audio_port_info_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_audio_ports_config_t"]["size"],
        size_of::<raw::clap_audio_ports_config_t>(),
        "clap_audio_ports_config_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_audio_ports_config_t"]["align"],
        align_of::<raw::clap_audio_ports_config_t>(),
        "clap_audio_ports_config_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_audio_ports_config_t"]["carrier"], "record",
        "clap_audio_ports_config_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_beattime"]["size"],
        size_of::<raw::clap_beattime>(),
        "clap_beattime alias size"
    );
    assert_eq!(
        report["aliases"]["clap_beattime"]["align"],
        align_of::<raw::clap_beattime>(),
        "clap_beattime alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_beattime"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_beattime native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_color_t"]["size"],
        size_of::<raw::clap_color_t>(),
        "clap_color_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_color_t"]["align"],
        align_of::<raw::clap_color_t>(),
        "clap_color_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_color_t"]["carrier"], "record",
        "clap_color_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_builder_t"]["size"],
        size_of::<raw::clap_context_menu_builder_t>(),
        "clap_context_menu_builder_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_builder_t"]["align"],
        align_of::<raw::clap_context_menu_builder_t>(),
        "clap_context_menu_builder_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_builder_t"]["carrier"], "record",
        "clap_context_menu_builder_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_check_entry_t"]["size"],
        size_of::<raw::clap_context_menu_check_entry_t>(),
        "clap_context_menu_check_entry_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_check_entry_t"]["align"],
        align_of::<raw::clap_context_menu_check_entry_t>(),
        "clap_context_menu_check_entry_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_check_entry_t"]["carrier"], "record",
        "clap_context_menu_check_entry_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_entry_t"]["size"],
        size_of::<raw::clap_context_menu_entry_t>(),
        "clap_context_menu_entry_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_entry_t"]["align"],
        align_of::<raw::clap_context_menu_entry_t>(),
        "clap_context_menu_entry_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_entry_t"]["carrier"], "record",
        "clap_context_menu_entry_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_item_kind_t"]["size"],
        size_of::<raw::clap_context_menu_item_kind_t>(),
        "clap_context_menu_item_kind_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_item_kind_t"]["align"],
        align_of::<raw::clap_context_menu_item_kind_t>(),
        "clap_context_menu_item_kind_t alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_context_menu_item_kind_t"]["carrier"].as_str(),
            Some("unsigned" | "enum")
        ),
        "clap_context_menu_item_kind_t native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_item_title_t"]["size"],
        size_of::<raw::clap_context_menu_item_title_t>(),
        "clap_context_menu_item_title_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_item_title_t"]["align"],
        align_of::<raw::clap_context_menu_item_title_t>(),
        "clap_context_menu_item_title_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_item_title_t"]["carrier"], "record",
        "clap_context_menu_item_title_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_submenu_t"]["size"],
        size_of::<raw::clap_context_menu_submenu_t>(),
        "clap_context_menu_submenu_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_submenu_t"]["align"],
        align_of::<raw::clap_context_menu_submenu_t>(),
        "clap_context_menu_submenu_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_submenu_t"]["carrier"], "record",
        "clap_context_menu_submenu_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_target_t"]["size"],
        size_of::<raw::clap_context_menu_target_t>(),
        "clap_context_menu_target_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_target_t"]["align"],
        align_of::<raw::clap_context_menu_target_t>(),
        "clap_context_menu_target_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_context_menu_target_t"]["carrier"], "record",
        "clap_context_menu_target_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_event_flags"]["size"],
        size_of::<raw::clap_event_flags>(),
        "clap_event_flags alias size"
    );
    assert_eq!(
        report["aliases"]["clap_event_flags"]["align"],
        align_of::<raw::clap_event_flags>(),
        "clap_event_flags alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_event_flags"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_event_flags native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_event_header_t"]["size"],
        size_of::<raw::clap_event_header_t>(),
        "clap_event_header_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_event_header_t"]["align"],
        align_of::<raw::clap_event_header_t>(),
        "clap_event_header_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_event_header_t"]["carrier"], "record",
        "clap_event_header_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_event_midi2_t"]["size"],
        size_of::<raw::clap_event_midi2_t>(),
        "clap_event_midi2_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_event_midi2_t"]["align"],
        align_of::<raw::clap_event_midi2_t>(),
        "clap_event_midi2_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_event_midi2_t"]["carrier"], "record",
        "clap_event_midi2_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_event_midi_sysex_t"]["size"],
        size_of::<raw::clap_event_midi_sysex_t>(),
        "clap_event_midi_sysex_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_event_midi_sysex_t"]["align"],
        align_of::<raw::clap_event_midi_sysex_t>(),
        "clap_event_midi_sysex_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_event_midi_sysex_t"]["carrier"], "record",
        "clap_event_midi_sysex_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_event_midi_t"]["size"],
        size_of::<raw::clap_event_midi_t>(),
        "clap_event_midi_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_event_midi_t"]["align"],
        align_of::<raw::clap_event_midi_t>(),
        "clap_event_midi_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_event_midi_t"]["carrier"], "record",
        "clap_event_midi_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_event_note_expression_t"]["size"],
        size_of::<raw::clap_event_note_expression_t>(),
        "clap_event_note_expression_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_event_note_expression_t"]["align"],
        align_of::<raw::clap_event_note_expression_t>(),
        "clap_event_note_expression_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_event_note_expression_t"]["carrier"], "record",
        "clap_event_note_expression_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_event_note_t"]["size"],
        size_of::<raw::clap_event_note_t>(),
        "clap_event_note_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_event_note_t"]["align"],
        align_of::<raw::clap_event_note_t>(),
        "clap_event_note_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_event_note_t"]["carrier"], "record",
        "clap_event_note_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_event_param_gesture_t"]["size"],
        size_of::<raw::clap_event_param_gesture_t>(),
        "clap_event_param_gesture_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_event_param_gesture_t"]["align"],
        align_of::<raw::clap_event_param_gesture_t>(),
        "clap_event_param_gesture_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_event_param_gesture_t"]["carrier"], "record",
        "clap_event_param_gesture_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_event_param_mod_t"]["size"],
        size_of::<raw::clap_event_param_mod_t>(),
        "clap_event_param_mod_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_event_param_mod_t"]["align"],
        align_of::<raw::clap_event_param_mod_t>(),
        "clap_event_param_mod_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_event_param_mod_t"]["carrier"], "record",
        "clap_event_param_mod_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_event_param_value_t"]["size"],
        size_of::<raw::clap_event_param_value_t>(),
        "clap_event_param_value_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_event_param_value_t"]["align"],
        align_of::<raw::clap_event_param_value_t>(),
        "clap_event_param_value_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_event_param_value_t"]["carrier"], "record",
        "clap_event_param_value_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_event_transport_t"]["size"],
        size_of::<raw::clap_event_transport_t>(),
        "clap_event_transport_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_event_transport_t"]["align"],
        align_of::<raw::clap_event_transport_t>(),
        "clap_event_transport_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_event_transport_t"]["carrier"], "record",
        "clap_event_transport_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_event_trigger_t"]["size"],
        size_of::<raw::clap_event_trigger_t>(),
        "clap_event_trigger_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_event_trigger_t"]["align"],
        align_of::<raw::clap_event_trigger_t>(),
        "clap_event_trigger_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_event_trigger_t"]["carrier"], "record",
        "clap_event_trigger_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_event_tuning_t"]["size"],
        size_of::<raw::clap_event_tuning_t>(),
        "clap_event_tuning_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_event_tuning_t"]["align"],
        align_of::<raw::clap_event_tuning_t>(),
        "clap_event_tuning_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_event_tuning_t"]["carrier"], "record",
        "clap_event_tuning_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_gui_resize_hints_t"]["size"],
        size_of::<raw::clap_gui_resize_hints_t>(),
        "clap_gui_resize_hints_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_gui_resize_hints_t"]["align"],
        align_of::<raw::clap_gui_resize_hints_t>(),
        "clap_gui_resize_hints_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_gui_resize_hints_t"]["carrier"], "record",
        "clap_gui_resize_hints_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_ambisonic_t"]["size"],
        size_of::<raw::clap_host_ambisonic_t>(),
        "clap_host_ambisonic_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_ambisonic_t"]["align"],
        align_of::<raw::clap_host_ambisonic_t>(),
        "clap_host_ambisonic_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_ambisonic_t"]["carrier"], "record",
        "clap_host_ambisonic_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_audio_ports_config_t"]["size"],
        size_of::<raw::clap_host_audio_ports_config_t>(),
        "clap_host_audio_ports_config_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_audio_ports_config_t"]["align"],
        align_of::<raw::clap_host_audio_ports_config_t>(),
        "clap_host_audio_ports_config_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_audio_ports_config_t"]["carrier"], "record",
        "clap_host_audio_ports_config_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_audio_ports_t"]["size"],
        size_of::<raw::clap_host_audio_ports_t>(),
        "clap_host_audio_ports_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_audio_ports_t"]["align"],
        align_of::<raw::clap_host_audio_ports_t>(),
        "clap_host_audio_ports_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_audio_ports_t"]["carrier"], "record",
        "clap_host_audio_ports_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_background_progress_t"]["size"],
        size_of::<raw::clap_host_background_progress_t>(),
        "clap_host_background_progress_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_background_progress_t"]["align"],
        align_of::<raw::clap_host_background_progress_t>(),
        "clap_host_background_progress_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_background_progress_t"]["carrier"], "record",
        "clap_host_background_progress_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_context_menu_t"]["size"],
        size_of::<raw::clap_host_context_menu_t>(),
        "clap_host_context_menu_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_context_menu_t"]["align"],
        align_of::<raw::clap_host_context_menu_t>(),
        "clap_host_context_menu_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_context_menu_t"]["carrier"], "record",
        "clap_host_context_menu_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_event_registry_t"]["size"],
        size_of::<raw::clap_host_event_registry_t>(),
        "clap_host_event_registry_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_event_registry_t"]["align"],
        align_of::<raw::clap_host_event_registry_t>(),
        "clap_host_event_registry_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_event_registry_t"]["carrier"], "record",
        "clap_host_event_registry_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_flush_events_t"]["size"],
        size_of::<raw::clap_host_flush_events_t>(),
        "clap_host_flush_events_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_flush_events_t"]["align"],
        align_of::<raw::clap_host_flush_events_t>(),
        "clap_host_flush_events_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_flush_events_t"]["carrier"], "record",
        "clap_host_flush_events_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_gui_t"]["size"],
        size_of::<raw::clap_host_gui_t>(),
        "clap_host_gui_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_gui_t"]["align"],
        align_of::<raw::clap_host_gui_t>(),
        "clap_host_gui_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_gui_t"]["carrier"], "record",
        "clap_host_gui_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_latency_t"]["size"],
        size_of::<raw::clap_host_latency_t>(),
        "clap_host_latency_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_latency_t"]["align"],
        align_of::<raw::clap_host_latency_t>(),
        "clap_host_latency_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_latency_t"]["carrier"], "record",
        "clap_host_latency_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_log_t"]["size"],
        size_of::<raw::clap_host_log_t>(),
        "clap_host_log_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_log_t"]["align"],
        align_of::<raw::clap_host_log_t>(),
        "clap_host_log_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_log_t"]["carrier"], "record",
        "clap_host_log_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_mini_curve_display_t"]["size"],
        size_of::<raw::clap_host_mini_curve_display_t>(),
        "clap_host_mini_curve_display_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_mini_curve_display_t"]["align"],
        align_of::<raw::clap_host_mini_curve_display_t>(),
        "clap_host_mini_curve_display_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_mini_curve_display_t"]["carrier"], "record",
        "clap_host_mini_curve_display_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_note_name_t"]["size"],
        size_of::<raw::clap_host_note_name_t>(),
        "clap_host_note_name_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_note_name_t"]["align"],
        align_of::<raw::clap_host_note_name_t>(),
        "clap_host_note_name_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_note_name_t"]["carrier"], "record",
        "clap_host_note_name_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_note_ports_t"]["size"],
        size_of::<raw::clap_host_note_ports_t>(),
        "clap_host_note_ports_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_note_ports_t"]["align"],
        align_of::<raw::clap_host_note_ports_t>(),
        "clap_host_note_ports_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_note_ports_t"]["carrier"], "record",
        "clap_host_note_ports_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_param_hovered_t"]["size"],
        size_of::<raw::clap_host_param_hovered_t>(),
        "clap_host_param_hovered_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_param_hovered_t"]["align"],
        align_of::<raw::clap_host_param_hovered_t>(),
        "clap_host_param_hovered_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_param_hovered_t"]["carrier"], "record",
        "clap_host_param_hovered_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_params_origin_t"]["size"],
        size_of::<raw::clap_host_params_origin_t>(),
        "clap_host_params_origin_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_params_origin_t"]["align"],
        align_of::<raw::clap_host_params_origin_t>(),
        "clap_host_params_origin_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_params_origin_t"]["carrier"], "record",
        "clap_host_params_origin_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_params_t"]["size"],
        size_of::<raw::clap_host_params_t>(),
        "clap_host_params_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_params_t"]["align"],
        align_of::<raw::clap_host_params_t>(),
        "clap_host_params_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_params_t"]["carrier"], "record",
        "clap_host_params_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_posix_fd_support_t"]["size"],
        size_of::<raw::clap_host_posix_fd_support_t>(),
        "clap_host_posix_fd_support_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_posix_fd_support_t"]["align"],
        align_of::<raw::clap_host_posix_fd_support_t>(),
        "clap_host_posix_fd_support_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_posix_fd_support_t"]["carrier"], "record",
        "clap_host_posix_fd_support_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_preset_load_t"]["size"],
        size_of::<raw::clap_host_preset_load_t>(),
        "clap_host_preset_load_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_preset_load_t"]["align"],
        align_of::<raw::clap_host_preset_load_t>(),
        "clap_host_preset_load_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_preset_load_t"]["carrier"], "record",
        "clap_host_preset_load_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_remote_controls_t"]["size"],
        size_of::<raw::clap_host_remote_controls_t>(),
        "clap_host_remote_controls_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_remote_controls_t"]["align"],
        align_of::<raw::clap_host_remote_controls_t>(),
        "clap_host_remote_controls_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_remote_controls_t"]["carrier"], "record",
        "clap_host_remote_controls_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_resource_directory_t"]["size"],
        size_of::<raw::clap_host_resource_directory_t>(),
        "clap_host_resource_directory_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_resource_directory_t"]["align"],
        align_of::<raw::clap_host_resource_directory_t>(),
        "clap_host_resource_directory_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_resource_directory_t"]["carrier"], "record",
        "clap_host_resource_directory_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_scratch_memory_t"]["size"],
        size_of::<raw::clap_host_scratch_memory_t>(),
        "clap_host_scratch_memory_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_scratch_memory_t"]["align"],
        align_of::<raw::clap_host_scratch_memory_t>(),
        "clap_host_scratch_memory_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_scratch_memory_t"]["carrier"], "record",
        "clap_host_scratch_memory_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_state_t"]["size"],
        size_of::<raw::clap_host_state_t>(),
        "clap_host_state_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_state_t"]["align"],
        align_of::<raw::clap_host_state_t>(),
        "clap_host_state_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_state_t"]["carrier"], "record",
        "clap_host_state_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_surround_t"]["size"],
        size_of::<raw::clap_host_surround_t>(),
        "clap_host_surround_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_surround_t"]["align"],
        align_of::<raw::clap_host_surround_t>(),
        "clap_host_surround_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_surround_t"]["carrier"], "record",
        "clap_host_surround_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_t"]["size"],
        size_of::<raw::clap_host_t>(),
        "clap_host_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_t"]["align"],
        align_of::<raw::clap_host_t>(),
        "clap_host_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_t"]["carrier"], "record",
        "clap_host_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_tail_t"]["size"],
        size_of::<raw::clap_host_tail_t>(),
        "clap_host_tail_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_tail_t"]["align"],
        align_of::<raw::clap_host_tail_t>(),
        "clap_host_tail_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_tail_t"]["carrier"], "record",
        "clap_host_tail_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_thread_check_t"]["size"],
        size_of::<raw::clap_host_thread_check_t>(),
        "clap_host_thread_check_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_thread_check_t"]["align"],
        align_of::<raw::clap_host_thread_check_t>(),
        "clap_host_thread_check_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_thread_check_t"]["carrier"], "record",
        "clap_host_thread_check_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_thread_pool_t"]["size"],
        size_of::<raw::clap_host_thread_pool_t>(),
        "clap_host_thread_pool_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_thread_pool_t"]["align"],
        align_of::<raw::clap_host_thread_pool_t>(),
        "clap_host_thread_pool_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_thread_pool_t"]["carrier"], "record",
        "clap_host_thread_pool_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_timer_support_t"]["size"],
        size_of::<raw::clap_host_timer_support_t>(),
        "clap_host_timer_support_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_timer_support_t"]["align"],
        align_of::<raw::clap_host_timer_support_t>(),
        "clap_host_timer_support_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_timer_support_t"]["carrier"], "record",
        "clap_host_timer_support_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_track_info_t"]["size"],
        size_of::<raw::clap_host_track_info_t>(),
        "clap_host_track_info_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_track_info_t"]["align"],
        align_of::<raw::clap_host_track_info_t>(),
        "clap_host_track_info_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_track_info_t"]["carrier"], "record",
        "clap_host_track_info_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_transport_control_t"]["size"],
        size_of::<raw::clap_host_transport_control_t>(),
        "clap_host_transport_control_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_transport_control_t"]["align"],
        align_of::<raw::clap_host_transport_control_t>(),
        "clap_host_transport_control_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_transport_control_t"]["carrier"], "record",
        "clap_host_transport_control_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_triggers_t"]["size"],
        size_of::<raw::clap_host_triggers_t>(),
        "clap_host_triggers_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_triggers_t"]["align"],
        align_of::<raw::clap_host_triggers_t>(),
        "clap_host_triggers_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_triggers_t"]["carrier"], "record",
        "clap_host_triggers_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_tuning_t"]["size"],
        size_of::<raw::clap_host_tuning_t>(),
        "clap_host_tuning_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_tuning_t"]["align"],
        align_of::<raw::clap_host_tuning_t>(),
        "clap_host_tuning_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_tuning_t"]["carrier"], "record",
        "clap_host_tuning_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_undo_t"]["size"],
        size_of::<raw::clap_host_undo_t>(),
        "clap_host_undo_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_undo_t"]["align"],
        align_of::<raw::clap_host_undo_t>(),
        "clap_host_undo_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_undo_t"]["carrier"], "record",
        "clap_host_undo_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_voice_info_t"]["size"],
        size_of::<raw::clap_host_voice_info_t>(),
        "clap_host_voice_info_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_voice_info_t"]["align"],
        align_of::<raw::clap_host_voice_info_t>(),
        "clap_host_voice_info_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_voice_info_t"]["carrier"], "record",
        "clap_host_voice_info_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_host_webview_t"]["size"],
        size_of::<raw::clap_host_webview_t>(),
        "clap_host_webview_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_host_webview_t"]["align"],
        align_of::<raw::clap_host_webview_t>(),
        "clap_host_webview_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_host_webview_t"]["carrier"], "record",
        "clap_host_webview_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_hwnd"]["size"],
        size_of::<raw::clap_hwnd>(),
        "clap_hwnd alias size"
    );
    assert_eq!(
        report["aliases"]["clap_hwnd"]["align"],
        align_of::<raw::clap_hwnd>(),
        "clap_hwnd alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_hwnd"]["carrier"], "pointer",
        "clap_hwnd native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_id"]["size"],
        size_of::<raw::clap_id>(),
        "clap_id alias size"
    );
    assert_eq!(
        report["aliases"]["clap_id"]["align"],
        align_of::<raw::clap_id>(),
        "clap_id alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_id"]["carrier"].as_str(),
            Some("unsigned" | "enum")
        ),
        "clap_id native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_input_events_t"]["size"],
        size_of::<raw::clap_input_events_t>(),
        "clap_input_events_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_input_events_t"]["align"],
        align_of::<raw::clap_input_events_t>(),
        "clap_input_events_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_input_events_t"]["carrier"], "record",
        "clap_input_events_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_istream_t"]["size"],
        size_of::<raw::clap_istream_t>(),
        "clap_istream_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_istream_t"]["align"],
        align_of::<raw::clap_istream_t>(),
        "clap_istream_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_istream_t"]["carrier"], "record",
        "clap_istream_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_log_severity"]["size"],
        size_of::<raw::clap_log_severity>(),
        "clap_log_severity alias size"
    );
    assert_eq!(
        report["aliases"]["clap_log_severity"]["align"],
        align_of::<raw::clap_log_severity>(),
        "clap_log_severity alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_log_severity"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_log_severity native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_mini_curve_display_change_flags"]["size"],
        size_of::<raw::clap_mini_curve_display_change_flags>(),
        "clap_mini_curve_display_change_flags alias size"
    );
    assert_eq!(
        report["aliases"]["clap_mini_curve_display_change_flags"]["align"],
        align_of::<raw::clap_mini_curve_display_change_flags>(),
        "clap_mini_curve_display_change_flags alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_mini_curve_display_change_flags"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_mini_curve_display_change_flags native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_mini_curve_display_curve_data_t"]["size"],
        size_of::<raw::clap_mini_curve_display_curve_data_t>(),
        "clap_mini_curve_display_curve_data_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_mini_curve_display_curve_data_t"]["align"],
        align_of::<raw::clap_mini_curve_display_curve_data_t>(),
        "clap_mini_curve_display_curve_data_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_mini_curve_display_curve_data_t"]["carrier"], "record",
        "clap_mini_curve_display_curve_data_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_mini_curve_display_curve_hints_t"]["size"],
        size_of::<raw::clap_mini_curve_display_curve_hints_t>(),
        "clap_mini_curve_display_curve_hints_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_mini_curve_display_curve_hints_t"]["align"],
        align_of::<raw::clap_mini_curve_display_curve_hints_t>(),
        "clap_mini_curve_display_curve_hints_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_mini_curve_display_curve_hints_t"]["carrier"], "record",
        "clap_mini_curve_display_curve_hints_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_mini_curve_display_curve_kind"]["size"],
        size_of::<raw::clap_mini_curve_display_curve_kind>(),
        "clap_mini_curve_display_curve_kind alias size"
    );
    assert_eq!(
        report["aliases"]["clap_mini_curve_display_curve_kind"]["align"],
        align_of::<raw::clap_mini_curve_display_curve_kind>(),
        "clap_mini_curve_display_curve_kind alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_mini_curve_display_curve_kind"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_mini_curve_display_curve_kind native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_note_dialect"]["size"],
        size_of::<raw::clap_note_dialect>(),
        "clap_note_dialect alias size"
    );
    assert_eq!(
        report["aliases"]["clap_note_dialect"]["align"],
        align_of::<raw::clap_note_dialect>(),
        "clap_note_dialect alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_note_dialect"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_note_dialect native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_note_expression"]["size"],
        size_of::<raw::clap_note_expression>(),
        "clap_note_expression alias size"
    );
    assert_eq!(
        report["aliases"]["clap_note_expression"]["align"],
        align_of::<raw::clap_note_expression>(),
        "clap_note_expression alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_note_expression"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_note_expression native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_note_name_t"]["size"],
        size_of::<raw::clap_note_name_t>(),
        "clap_note_name_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_note_name_t"]["align"],
        align_of::<raw::clap_note_name_t>(),
        "clap_note_name_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_note_name_t"]["carrier"], "record",
        "clap_note_name_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_note_port_info_t"]["size"],
        size_of::<raw::clap_note_port_info_t>(),
        "clap_note_port_info_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_note_port_info_t"]["align"],
        align_of::<raw::clap_note_port_info_t>(),
        "clap_note_port_info_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_note_port_info_t"]["carrier"], "record",
        "clap_note_port_info_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_nsview"]["size"],
        size_of::<raw::clap_nsview>(),
        "clap_nsview alias size"
    );
    assert_eq!(
        report["aliases"]["clap_nsview"]["align"],
        align_of::<raw::clap_nsview>(),
        "clap_nsview alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_nsview"]["carrier"], "pointer",
        "clap_nsview native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_ostream_t"]["size"],
        size_of::<raw::clap_ostream_t>(),
        "clap_ostream_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_ostream_t"]["align"],
        align_of::<raw::clap_ostream_t>(),
        "clap_ostream_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_ostream_t"]["carrier"], "record",
        "clap_ostream_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_output_events_t"]["size"],
        size_of::<raw::clap_output_events_t>(),
        "clap_output_events_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_output_events_t"]["align"],
        align_of::<raw::clap_output_events_t>(),
        "clap_output_events_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_output_events_t"]["carrier"], "record",
        "clap_output_events_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_param_clear_flags"]["size"],
        size_of::<raw::clap_param_clear_flags>(),
        "clap_param_clear_flags alias size"
    );
    assert_eq!(
        report["aliases"]["clap_param_clear_flags"]["align"],
        align_of::<raw::clap_param_clear_flags>(),
        "clap_param_clear_flags alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_param_clear_flags"]["carrier"].as_str(),
            Some("unsigned" | "enum")
        ),
        "clap_param_clear_flags native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_param_info_flags"]["size"],
        size_of::<raw::clap_param_info_flags>(),
        "clap_param_info_flags alias size"
    );
    assert_eq!(
        report["aliases"]["clap_param_info_flags"]["align"],
        align_of::<raw::clap_param_info_flags>(),
        "clap_param_info_flags alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_param_info_flags"]["carrier"].as_str(),
            Some("unsigned" | "enum")
        ),
        "clap_param_info_flags native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_param_info_t"]["size"],
        size_of::<raw::clap_param_info_t>(),
        "clap_param_info_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_param_info_t"]["align"],
        align_of::<raw::clap_param_info_t>(),
        "clap_param_info_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_param_info_t"]["carrier"], "record",
        "clap_param_info_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_param_rescan_flags"]["size"],
        size_of::<raw::clap_param_rescan_flags>(),
        "clap_param_rescan_flags alias size"
    );
    assert_eq!(
        report["aliases"]["clap_param_rescan_flags"]["align"],
        align_of::<raw::clap_param_rescan_flags>(),
        "clap_param_rescan_flags alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_param_rescan_flags"]["carrier"].as_str(),
            Some("unsigned" | "enum")
        ),
        "clap_param_rescan_flags native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_ambisonic_t"]["size"],
        size_of::<raw::clap_plugin_ambisonic_t>(),
        "clap_plugin_ambisonic_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_ambisonic_t"]["align"],
        align_of::<raw::clap_plugin_ambisonic_t>(),
        "clap_plugin_ambisonic_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_ambisonic_t"]["carrier"], "record",
        "clap_plugin_ambisonic_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_audio_ports_activation_t"]["size"],
        size_of::<raw::clap_plugin_audio_ports_activation_t>(),
        "clap_plugin_audio_ports_activation_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_audio_ports_activation_t"]["align"],
        align_of::<raw::clap_plugin_audio_ports_activation_t>(),
        "clap_plugin_audio_ports_activation_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_audio_ports_activation_t"]["carrier"], "record",
        "clap_plugin_audio_ports_activation_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_audio_ports_config_info_t"]["size"],
        size_of::<raw::clap_plugin_audio_ports_config_info_t>(),
        "clap_plugin_audio_ports_config_info_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_audio_ports_config_info_t"]["align"],
        align_of::<raw::clap_plugin_audio_ports_config_info_t>(),
        "clap_plugin_audio_ports_config_info_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_audio_ports_config_info_t"]["carrier"], "record",
        "clap_plugin_audio_ports_config_info_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_audio_ports_config_t"]["size"],
        size_of::<raw::clap_plugin_audio_ports_config_t>(),
        "clap_plugin_audio_ports_config_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_audio_ports_config_t"]["align"],
        align_of::<raw::clap_plugin_audio_ports_config_t>(),
        "clap_plugin_audio_ports_config_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_audio_ports_config_t"]["carrier"], "record",
        "clap_plugin_audio_ports_config_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_audio_ports_t"]["size"],
        size_of::<raw::clap_plugin_audio_ports_t>(),
        "clap_plugin_audio_ports_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_audio_ports_t"]["align"],
        align_of::<raw::clap_plugin_audio_ports_t>(),
        "clap_plugin_audio_ports_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_audio_ports_t"]["carrier"], "record",
        "clap_plugin_audio_ports_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_background_activation_t"]["size"],
        size_of::<raw::clap_plugin_background_activation_t>(),
        "clap_plugin_background_activation_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_background_activation_t"]["align"],
        align_of::<raw::clap_plugin_background_activation_t>(),
        "clap_plugin_background_activation_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_background_activation_t"]["carrier"], "record",
        "clap_plugin_background_activation_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_background_state_context_t"]["size"],
        size_of::<raw::clap_plugin_background_state_context_t>(),
        "clap_plugin_background_state_context_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_background_state_context_t"]["align"],
        align_of::<raw::clap_plugin_background_state_context_t>(),
        "clap_plugin_background_state_context_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_background_state_context_t"]["carrier"], "record",
        "clap_plugin_background_state_context_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_configurable_audio_ports_t"]["size"],
        size_of::<raw::clap_plugin_configurable_audio_ports_t>(),
        "clap_plugin_configurable_audio_ports_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_configurable_audio_ports_t"]["align"],
        align_of::<raw::clap_plugin_configurable_audio_ports_t>(),
        "clap_plugin_configurable_audio_ports_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_configurable_audio_ports_t"]["carrier"], "record",
        "clap_plugin_configurable_audio_ports_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_context_menu_t"]["size"],
        size_of::<raw::clap_plugin_context_menu_t>(),
        "clap_plugin_context_menu_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_context_menu_t"]["align"],
        align_of::<raw::clap_plugin_context_menu_t>(),
        "clap_plugin_context_menu_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_context_menu_t"]["carrier"], "record",
        "clap_plugin_context_menu_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_descriptor_t"]["size"],
        size_of::<raw::clap_plugin_descriptor_t>(),
        "clap_plugin_descriptor_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_descriptor_t"]["align"],
        align_of::<raw::clap_plugin_descriptor_t>(),
        "clap_plugin_descriptor_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_descriptor_t"]["carrier"], "record",
        "clap_plugin_descriptor_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_entry_t"]["size"],
        size_of::<raw::clap_plugin_entry_t>(),
        "clap_plugin_entry_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_entry_t"]["align"],
        align_of::<raw::clap_plugin_entry_t>(),
        "clap_plugin_entry_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_entry_t"]["carrier"], "record",
        "clap_plugin_entry_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_extensible_audio_ports_t"]["size"],
        size_of::<raw::clap_plugin_extensible_audio_ports_t>(),
        "clap_plugin_extensible_audio_ports_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_extensible_audio_ports_t"]["align"],
        align_of::<raw::clap_plugin_extensible_audio_ports_t>(),
        "clap_plugin_extensible_audio_ports_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_extensible_audio_ports_t"]["carrier"], "record",
        "clap_plugin_extensible_audio_ports_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_factory_t"]["size"],
        size_of::<raw::clap_plugin_factory_t>(),
        "clap_plugin_factory_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_factory_t"]["align"],
        align_of::<raw::clap_plugin_factory_t>(),
        "clap_plugin_factory_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_factory_t"]["carrier"], "record",
        "clap_plugin_factory_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_flush_events_t"]["size"],
        size_of::<raw::clap_plugin_flush_events_t>(),
        "clap_plugin_flush_events_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_flush_events_t"]["align"],
        align_of::<raw::clap_plugin_flush_events_t>(),
        "clap_plugin_flush_events_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_flush_events_t"]["carrier"], "record",
        "clap_plugin_flush_events_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_gain_adjustment_metering_t"]["size"],
        size_of::<raw::clap_plugin_gain_adjustment_metering_t>(),
        "clap_plugin_gain_adjustment_metering_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_gain_adjustment_metering_t"]["align"],
        align_of::<raw::clap_plugin_gain_adjustment_metering_t>(),
        "clap_plugin_gain_adjustment_metering_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_gain_adjustment_metering_t"]["carrier"], "record",
        "clap_plugin_gain_adjustment_metering_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_gui_t"]["size"],
        size_of::<raw::clap_plugin_gui_t>(),
        "clap_plugin_gui_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_gui_t"]["align"],
        align_of::<raw::clap_plugin_gui_t>(),
        "clap_plugin_gui_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_gui_t"]["carrier"], "record",
        "clap_plugin_gui_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_invalidation_factory_t"]["size"],
        size_of::<raw::clap_plugin_invalidation_factory_t>(),
        "clap_plugin_invalidation_factory_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_invalidation_factory_t"]["align"],
        align_of::<raw::clap_plugin_invalidation_factory_t>(),
        "clap_plugin_invalidation_factory_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_invalidation_factory_t"]["carrier"], "record",
        "clap_plugin_invalidation_factory_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_invalidation_source_t"]["size"],
        size_of::<raw::clap_plugin_invalidation_source_t>(),
        "clap_plugin_invalidation_source_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_invalidation_source_t"]["align"],
        align_of::<raw::clap_plugin_invalidation_source_t>(),
        "clap_plugin_invalidation_source_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_invalidation_source_t"]["carrier"], "record",
        "clap_plugin_invalidation_source_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_latency_t"]["size"],
        size_of::<raw::clap_plugin_latency_t>(),
        "clap_plugin_latency_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_latency_t"]["align"],
        align_of::<raw::clap_plugin_latency_t>(),
        "clap_plugin_latency_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_latency_t"]["carrier"], "record",
        "clap_plugin_latency_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_mini_curve_display_t"]["size"],
        size_of::<raw::clap_plugin_mini_curve_display_t>(),
        "clap_plugin_mini_curve_display_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_mini_curve_display_t"]["align"],
        align_of::<raw::clap_plugin_mini_curve_display_t>(),
        "clap_plugin_mini_curve_display_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_mini_curve_display_t"]["carrier"], "record",
        "clap_plugin_mini_curve_display_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_note_name_t"]["size"],
        size_of::<raw::clap_plugin_note_name_t>(),
        "clap_plugin_note_name_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_note_name_t"]["align"],
        align_of::<raw::clap_plugin_note_name_t>(),
        "clap_plugin_note_name_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_note_name_t"]["carrier"], "record",
        "clap_plugin_note_name_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_note_ports_t"]["size"],
        size_of::<raw::clap_plugin_note_ports_t>(),
        "clap_plugin_note_ports_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_note_ports_t"]["align"],
        align_of::<raw::clap_plugin_note_ports_t>(),
        "clap_plugin_note_ports_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_note_ports_t"]["carrier"], "record",
        "clap_plugin_note_ports_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_octave_number_t"]["size"],
        size_of::<raw::clap_plugin_octave_number_t>(),
        "clap_plugin_octave_number_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_octave_number_t"]["align"],
        align_of::<raw::clap_plugin_octave_number_t>(),
        "clap_plugin_octave_number_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_octave_number_t"]["carrier"], "record",
        "clap_plugin_octave_number_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_param_indication_t"]["size"],
        size_of::<raw::clap_plugin_param_indication_t>(),
        "clap_plugin_param_indication_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_param_indication_t"]["align"],
        align_of::<raw::clap_plugin_param_indication_t>(),
        "clap_plugin_param_indication_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_param_indication_t"]["carrier"], "record",
        "clap_plugin_param_indication_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_params_origin_t"]["size"],
        size_of::<raw::clap_plugin_params_origin_t>(),
        "clap_plugin_params_origin_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_params_origin_t"]["align"],
        align_of::<raw::clap_plugin_params_origin_t>(),
        "clap_plugin_params_origin_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_params_origin_t"]["carrier"], "record",
        "clap_plugin_params_origin_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_params_t"]["size"],
        size_of::<raw::clap_plugin_params_t>(),
        "clap_plugin_params_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_params_t"]["align"],
        align_of::<raw::clap_plugin_params_t>(),
        "clap_plugin_params_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_params_t"]["carrier"], "record",
        "clap_plugin_params_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_posix_fd_support_t"]["size"],
        size_of::<raw::clap_plugin_posix_fd_support_t>(),
        "clap_plugin_posix_fd_support_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_posix_fd_support_t"]["align"],
        align_of::<raw::clap_plugin_posix_fd_support_t>(),
        "clap_plugin_posix_fd_support_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_posix_fd_support_t"]["carrier"], "record",
        "clap_plugin_posix_fd_support_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_preset_load_t"]["size"],
        size_of::<raw::clap_plugin_preset_load_t>(),
        "clap_plugin_preset_load_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_preset_load_t"]["align"],
        align_of::<raw::clap_plugin_preset_load_t>(),
        "clap_plugin_preset_load_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_preset_load_t"]["carrier"], "record",
        "clap_plugin_preset_load_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_project_location_t"]["size"],
        size_of::<raw::clap_plugin_project_location_t>(),
        "clap_plugin_project_location_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_project_location_t"]["align"],
        align_of::<raw::clap_plugin_project_location_t>(),
        "clap_plugin_project_location_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_project_location_t"]["carrier"], "record",
        "clap_plugin_project_location_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_remote_controls_t"]["size"],
        size_of::<raw::clap_plugin_remote_controls_t>(),
        "clap_plugin_remote_controls_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_remote_controls_t"]["align"],
        align_of::<raw::clap_plugin_remote_controls_t>(),
        "clap_plugin_remote_controls_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_remote_controls_t"]["carrier"], "record",
        "clap_plugin_remote_controls_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_render_mode"]["size"],
        size_of::<raw::clap_plugin_render_mode>(),
        "clap_plugin_render_mode alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_render_mode"]["align"],
        align_of::<raw::clap_plugin_render_mode>(),
        "clap_plugin_render_mode alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_plugin_render_mode"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_plugin_render_mode native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_render_t"]["size"],
        size_of::<raw::clap_plugin_render_t>(),
        "clap_plugin_render_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_render_t"]["align"],
        align_of::<raw::clap_plugin_render_t>(),
        "clap_plugin_render_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_render_t"]["carrier"], "record",
        "clap_plugin_render_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_resource_directory_t"]["size"],
        size_of::<raw::clap_plugin_resource_directory_t>(),
        "clap_plugin_resource_directory_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_resource_directory_t"]["align"],
        align_of::<raw::clap_plugin_resource_directory_t>(),
        "clap_plugin_resource_directory_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_resource_directory_t"]["carrier"], "record",
        "clap_plugin_resource_directory_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_state_context_t"]["size"],
        size_of::<raw::clap_plugin_state_context_t>(),
        "clap_plugin_state_context_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_state_context_t"]["align"],
        align_of::<raw::clap_plugin_state_context_t>(),
        "clap_plugin_state_context_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_state_context_t"]["carrier"], "record",
        "clap_plugin_state_context_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_state_context_type"]["size"],
        size_of::<raw::clap_plugin_state_context_type>(),
        "clap_plugin_state_context_type alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_state_context_type"]["align"],
        align_of::<raw::clap_plugin_state_context_type>(),
        "clap_plugin_state_context_type alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_plugin_state_context_type"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_plugin_state_context_type native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_state_converter_descriptor_t"]["size"],
        size_of::<raw::clap_plugin_state_converter_descriptor_t>(),
        "clap_plugin_state_converter_descriptor_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_state_converter_descriptor_t"]["align"],
        align_of::<raw::clap_plugin_state_converter_descriptor_t>(),
        "clap_plugin_state_converter_descriptor_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_state_converter_descriptor_t"]["carrier"], "record",
        "clap_plugin_state_converter_descriptor_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_state_converter_factory_t"]["size"],
        size_of::<raw::clap_plugin_state_converter_factory_t>(),
        "clap_plugin_state_converter_factory_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_state_converter_factory_t"]["align"],
        align_of::<raw::clap_plugin_state_converter_factory_t>(),
        "clap_plugin_state_converter_factory_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_state_converter_factory_t"]["carrier"], "record",
        "clap_plugin_state_converter_factory_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_state_converter_t"]["size"],
        size_of::<raw::clap_plugin_state_converter_t>(),
        "clap_plugin_state_converter_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_state_converter_t"]["align"],
        align_of::<raw::clap_plugin_state_converter_t>(),
        "clap_plugin_state_converter_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_state_converter_t"]["carrier"], "record",
        "clap_plugin_state_converter_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_state_t"]["size"],
        size_of::<raw::clap_plugin_state_t>(),
        "clap_plugin_state_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_state_t"]["align"],
        align_of::<raw::clap_plugin_state_t>(),
        "clap_plugin_state_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_state_t"]["carrier"], "record",
        "clap_plugin_state_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_surround_t"]["size"],
        size_of::<raw::clap_plugin_surround_t>(),
        "clap_plugin_surround_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_surround_t"]["align"],
        align_of::<raw::clap_plugin_surround_t>(),
        "clap_plugin_surround_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_surround_t"]["carrier"], "record",
        "clap_plugin_surround_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_t"]["size"],
        size_of::<raw::clap_plugin_t>(),
        "clap_plugin_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_t"]["align"],
        align_of::<raw::clap_plugin_t>(),
        "clap_plugin_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_t"]["carrier"], "record",
        "clap_plugin_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_tail_t"]["size"],
        size_of::<raw::clap_plugin_tail_t>(),
        "clap_plugin_tail_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_tail_t"]["align"],
        align_of::<raw::clap_plugin_tail_t>(),
        "clap_plugin_tail_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_tail_t"]["carrier"], "record",
        "clap_plugin_tail_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_thread_pool_t"]["size"],
        size_of::<raw::clap_plugin_thread_pool_t>(),
        "clap_plugin_thread_pool_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_thread_pool_t"]["align"],
        align_of::<raw::clap_plugin_thread_pool_t>(),
        "clap_plugin_thread_pool_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_thread_pool_t"]["carrier"], "record",
        "clap_plugin_thread_pool_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_timer_support_t"]["size"],
        size_of::<raw::clap_plugin_timer_support_t>(),
        "clap_plugin_timer_support_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_timer_support_t"]["align"],
        align_of::<raw::clap_plugin_timer_support_t>(),
        "clap_plugin_timer_support_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_timer_support_t"]["carrier"], "record",
        "clap_plugin_timer_support_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_track_info_t"]["size"],
        size_of::<raw::clap_plugin_track_info_t>(),
        "clap_plugin_track_info_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_track_info_t"]["align"],
        align_of::<raw::clap_plugin_track_info_t>(),
        "clap_plugin_track_info_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_track_info_t"]["carrier"], "record",
        "clap_plugin_track_info_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_triggers_t"]["size"],
        size_of::<raw::clap_plugin_triggers_t>(),
        "clap_plugin_triggers_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_triggers_t"]["align"],
        align_of::<raw::clap_plugin_triggers_t>(),
        "clap_plugin_triggers_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_triggers_t"]["carrier"], "record",
        "clap_plugin_triggers_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_tuning_t"]["size"],
        size_of::<raw::clap_plugin_tuning_t>(),
        "clap_plugin_tuning_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_tuning_t"]["align"],
        align_of::<raw::clap_plugin_tuning_t>(),
        "clap_plugin_tuning_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_tuning_t"]["carrier"], "record",
        "clap_plugin_tuning_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_undo_context_t"]["size"],
        size_of::<raw::clap_plugin_undo_context_t>(),
        "clap_plugin_undo_context_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_undo_context_t"]["align"],
        align_of::<raw::clap_plugin_undo_context_t>(),
        "clap_plugin_undo_context_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_undo_context_t"]["carrier"], "record",
        "clap_plugin_undo_context_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_undo_delta_t"]["size"],
        size_of::<raw::clap_plugin_undo_delta_t>(),
        "clap_plugin_undo_delta_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_undo_delta_t"]["align"],
        align_of::<raw::clap_plugin_undo_delta_t>(),
        "clap_plugin_undo_delta_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_undo_delta_t"]["carrier"], "record",
        "clap_plugin_undo_delta_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_voice_info_t"]["size"],
        size_of::<raw::clap_plugin_voice_info_t>(),
        "clap_plugin_voice_info_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_voice_info_t"]["align"],
        align_of::<raw::clap_plugin_voice_info_t>(),
        "clap_plugin_voice_info_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_voice_info_t"]["carrier"], "record",
        "clap_plugin_voice_info_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_webview_t"]["size"],
        size_of::<raw::clap_plugin_webview_t>(),
        "clap_plugin_webview_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_webview_t"]["align"],
        align_of::<raw::clap_plugin_webview_t>(),
        "clap_plugin_webview_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_plugin_webview_t"]["carrier"], "record",
        "clap_plugin_webview_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_posix_fd_flags_t"]["size"],
        size_of::<raw::clap_posix_fd_flags_t>(),
        "clap_posix_fd_flags_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_posix_fd_flags_t"]["align"],
        align_of::<raw::clap_posix_fd_flags_t>(),
        "clap_posix_fd_flags_t alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_posix_fd_flags_t"]["carrier"].as_str(),
            Some("unsigned" | "enum")
        ),
        "clap_posix_fd_flags_t native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_factory_t"]["size"],
        size_of::<raw::clap_preset_discovery_factory_t>(),
        "clap_preset_discovery_factory_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_factory_t"]["align"],
        align_of::<raw::clap_preset_discovery_factory_t>(),
        "clap_preset_discovery_factory_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_factory_t"]["carrier"], "record",
        "clap_preset_discovery_factory_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_filetype_t"]["size"],
        size_of::<raw::clap_preset_discovery_filetype_t>(),
        "clap_preset_discovery_filetype_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_filetype_t"]["align"],
        align_of::<raw::clap_preset_discovery_filetype_t>(),
        "clap_preset_discovery_filetype_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_filetype_t"]["carrier"], "record",
        "clap_preset_discovery_filetype_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_flags"]["size"],
        size_of::<raw::clap_preset_discovery_flags>(),
        "clap_preset_discovery_flags alias size"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_flags"]["align"],
        align_of::<raw::clap_preset_discovery_flags>(),
        "clap_preset_discovery_flags alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_preset_discovery_flags"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_preset_discovery_flags native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_indexer_t"]["size"],
        size_of::<raw::clap_preset_discovery_indexer_t>(),
        "clap_preset_discovery_indexer_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_indexer_t"]["align"],
        align_of::<raw::clap_preset_discovery_indexer_t>(),
        "clap_preset_discovery_indexer_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_indexer_t"]["carrier"], "record",
        "clap_preset_discovery_indexer_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_location_kind"]["size"],
        size_of::<raw::clap_preset_discovery_location_kind>(),
        "clap_preset_discovery_location_kind alias size"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_location_kind"]["align"],
        align_of::<raw::clap_preset_discovery_location_kind>(),
        "clap_preset_discovery_location_kind alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_preset_discovery_location_kind"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_preset_discovery_location_kind native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_location_t"]["size"],
        size_of::<raw::clap_preset_discovery_location_t>(),
        "clap_preset_discovery_location_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_location_t"]["align"],
        align_of::<raw::clap_preset_discovery_location_t>(),
        "clap_preset_discovery_location_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_location_t"]["carrier"], "record",
        "clap_preset_discovery_location_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_metadata_receiver_t"]["size"],
        size_of::<raw::clap_preset_discovery_metadata_receiver_t>(),
        "clap_preset_discovery_metadata_receiver_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_metadata_receiver_t"]["align"],
        align_of::<raw::clap_preset_discovery_metadata_receiver_t>(),
        "clap_preset_discovery_metadata_receiver_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_metadata_receiver_t"]["carrier"], "record",
        "clap_preset_discovery_metadata_receiver_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_provider_descriptor_t"]["size"],
        size_of::<raw::clap_preset_discovery_provider_descriptor_t>(),
        "clap_preset_discovery_provider_descriptor_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_provider_descriptor_t"]["align"],
        align_of::<raw::clap_preset_discovery_provider_descriptor_t>(),
        "clap_preset_discovery_provider_descriptor_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_provider_descriptor_t"]["carrier"], "record",
        "clap_preset_discovery_provider_descriptor_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_provider_t"]["size"],
        size_of::<raw::clap_preset_discovery_provider_t>(),
        "clap_preset_discovery_provider_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_provider_t"]["align"],
        align_of::<raw::clap_preset_discovery_provider_t>(),
        "clap_preset_discovery_provider_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_provider_t"]["carrier"], "record",
        "clap_preset_discovery_provider_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_soundpack_t"]["size"],
        size_of::<raw::clap_preset_discovery_soundpack_t>(),
        "clap_preset_discovery_soundpack_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_soundpack_t"]["align"],
        align_of::<raw::clap_preset_discovery_soundpack_t>(),
        "clap_preset_discovery_soundpack_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_preset_discovery_soundpack_t"]["carrier"], "record",
        "clap_preset_discovery_soundpack_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_process_status"]["size"],
        size_of::<raw::clap_process_status>(),
        "clap_process_status alias size"
    );
    assert_eq!(
        report["aliases"]["clap_process_status"]["align"],
        align_of::<raw::clap_process_status>(),
        "clap_process_status alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_process_status"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_process_status native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_process_t"]["size"],
        size_of::<raw::clap_process_t>(),
        "clap_process_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_process_t"]["align"],
        align_of::<raw::clap_process_t>(),
        "clap_process_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_process_t"]["carrier"], "record",
        "clap_process_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_project_location_element_t"]["size"],
        size_of::<raw::clap_project_location_element_t>(),
        "clap_project_location_element_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_project_location_element_t"]["align"],
        align_of::<raw::clap_project_location_element_t>(),
        "clap_project_location_element_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_project_location_element_t"]["carrier"], "record",
        "clap_project_location_element_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_project_location_flags"]["size"],
        size_of::<raw::clap_project_location_flags>(),
        "clap_project_location_flags alias size"
    );
    assert_eq!(
        report["aliases"]["clap_project_location_flags"]["align"],
        align_of::<raw::clap_project_location_flags>(),
        "clap_project_location_flags alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_project_location_flags"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_project_location_flags native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_project_location_kind"]["size"],
        size_of::<raw::clap_project_location_kind>(),
        "clap_project_location_kind alias size"
    );
    assert_eq!(
        report["aliases"]["clap_project_location_kind"]["align"],
        align_of::<raw::clap_project_location_kind>(),
        "clap_project_location_kind alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_project_location_kind"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_project_location_kind native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_project_location_track_kind"]["size"],
        size_of::<raw::clap_project_location_track_kind>(),
        "clap_project_location_track_kind alias size"
    );
    assert_eq!(
        report["aliases"]["clap_project_location_track_kind"]["align"],
        align_of::<raw::clap_project_location_track_kind>(),
        "clap_project_location_track_kind alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_project_location_track_kind"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_project_location_track_kind native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_remote_controls_page_t"]["size"],
        size_of::<raw::clap_remote_controls_page_t>(),
        "clap_remote_controls_page_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_remote_controls_page_t"]["align"],
        align_of::<raw::clap_remote_controls_page_t>(),
        "clap_remote_controls_page_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_remote_controls_page_t"]["carrier"], "record",
        "clap_remote_controls_page_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_sectime"]["size"],
        size_of::<raw::clap_sectime>(),
        "clap_sectime alias size"
    );
    assert_eq!(
        report["aliases"]["clap_sectime"]["align"],
        align_of::<raw::clap_sectime>(),
        "clap_sectime alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_sectime"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_sectime native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_timestamp"]["size"],
        size_of::<raw::clap_timestamp>(),
        "clap_timestamp alias size"
    );
    assert_eq!(
        report["aliases"]["clap_timestamp"]["align"],
        align_of::<raw::clap_timestamp>(),
        "clap_timestamp alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_timestamp"]["carrier"].as_str(),
            Some("unsigned" | "enum")
        ),
        "clap_timestamp native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_track_info_t"]["size"],
        size_of::<raw::clap_track_info_t>(),
        "clap_track_info_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_track_info_t"]["align"],
        align_of::<raw::clap_track_info_t>(),
        "clap_track_info_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_track_info_t"]["carrier"], "record",
        "clap_track_info_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_transport_flags"]["size"],
        size_of::<raw::clap_transport_flags>(),
        "clap_transport_flags alias size"
    );
    assert_eq!(
        report["aliases"]["clap_transport_flags"]["align"],
        align_of::<raw::clap_transport_flags>(),
        "clap_transport_flags alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_transport_flags"]["carrier"].as_str(),
            Some("signed" | "enum")
        ),
        "clap_transport_flags native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_trigger_clear_flags"]["size"],
        size_of::<raw::clap_trigger_clear_flags>(),
        "clap_trigger_clear_flags alias size"
    );
    assert_eq!(
        report["aliases"]["clap_trigger_clear_flags"]["align"],
        align_of::<raw::clap_trigger_clear_flags>(),
        "clap_trigger_clear_flags alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_trigger_clear_flags"]["carrier"].as_str(),
            Some("unsigned" | "enum")
        ),
        "clap_trigger_clear_flags native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_trigger_info_flags"]["size"],
        size_of::<raw::clap_trigger_info_flags>(),
        "clap_trigger_info_flags alias size"
    );
    assert_eq!(
        report["aliases"]["clap_trigger_info_flags"]["align"],
        align_of::<raw::clap_trigger_info_flags>(),
        "clap_trigger_info_flags alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_trigger_info_flags"]["carrier"].as_str(),
            Some("unsigned" | "enum")
        ),
        "clap_trigger_info_flags native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_trigger_info_t"]["size"],
        size_of::<raw::clap_trigger_info_t>(),
        "clap_trigger_info_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_trigger_info_t"]["align"],
        align_of::<raw::clap_trigger_info_t>(),
        "clap_trigger_info_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_trigger_info_t"]["carrier"], "record",
        "clap_trigger_info_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_trigger_rescan_flags"]["size"],
        size_of::<raw::clap_trigger_rescan_flags>(),
        "clap_trigger_rescan_flags alias size"
    );
    assert_eq!(
        report["aliases"]["clap_trigger_rescan_flags"]["align"],
        align_of::<raw::clap_trigger_rescan_flags>(),
        "clap_trigger_rescan_flags alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_trigger_rescan_flags"]["carrier"].as_str(),
            Some("unsigned" | "enum")
        ),
        "clap_trigger_rescan_flags native integer/enum carrier"
    );
    assert_eq!(
        report["aliases"]["clap_tuning_info_t"]["size"],
        size_of::<raw::clap_tuning_info_t>(),
        "clap_tuning_info_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_tuning_info_t"]["align"],
        align_of::<raw::clap_tuning_info_t>(),
        "clap_tuning_info_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_tuning_info_t"]["carrier"], "record",
        "clap_tuning_info_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_uiview"]["size"],
        size_of::<raw::clap_uiview>(),
        "clap_uiview alias size"
    );
    assert_eq!(
        report["aliases"]["clap_uiview"]["align"],
        align_of::<raw::clap_uiview>(),
        "clap_uiview alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_uiview"]["carrier"], "pointer",
        "clap_uiview native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_undo_delta_properties_t"]["size"],
        size_of::<raw::clap_undo_delta_properties_t>(),
        "clap_undo_delta_properties_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_undo_delta_properties_t"]["align"],
        align_of::<raw::clap_undo_delta_properties_t>(),
        "clap_undo_delta_properties_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_undo_delta_properties_t"]["carrier"], "record",
        "clap_undo_delta_properties_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_universal_plugin_id_t"]["size"],
        size_of::<raw::clap_universal_plugin_id_t>(),
        "clap_universal_plugin_id_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_universal_plugin_id_t"]["align"],
        align_of::<raw::clap_universal_plugin_id_t>(),
        "clap_universal_plugin_id_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_universal_plugin_id_t"]["carrier"], "record",
        "clap_universal_plugin_id_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_version_t"]["size"],
        size_of::<raw::clap_version_t>(),
        "clap_version_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_version_t"]["align"],
        align_of::<raw::clap_version_t>(),
        "clap_version_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_version_t"]["carrier"], "record",
        "clap_version_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_voice_info_t"]["size"],
        size_of::<raw::clap_voice_info_t>(),
        "clap_voice_info_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_voice_info_t"]["align"],
        align_of::<raw::clap_voice_info_t>(),
        "clap_voice_info_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_voice_info_t"]["carrier"], "record",
        "clap_voice_info_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_window_t"]["size"],
        size_of::<raw::clap_window_t>(),
        "clap_window_t alias size"
    );
    assert_eq!(
        report["aliases"]["clap_window_t"]["align"],
        align_of::<raw::clap_window_t>(),
        "clap_window_t alias alignment"
    );
    assert_eq!(
        report["aliases"]["clap_window_t"]["carrier"], "record",
        "clap_window_t native carrier"
    );
    assert_eq!(
        report["aliases"]["clap_xwnd"]["size"],
        size_of::<raw::clap_xwnd>(),
        "clap_xwnd alias size"
    );
    assert_eq!(
        report["aliases"]["clap_xwnd"]["align"],
        align_of::<raw::clap_xwnd>(),
        "clap_xwnd alias alignment"
    );
    assert!(
        matches!(
            report["aliases"]["clap_xwnd"]["carrier"].as_str(),
            Some("unsigned" | "enum")
        ),
        "clap_xwnd native integer/enum carrier"
    );
    let version = clap_sys::version::CLAP_VERSION;
    assert_eq!(
        report["header_local"]["version"],
        serde_json::json!([version.major, version.minor, version.revision])
    );
    let color = clap_sys::color::CLAP_COLOR_TRANSPARENT;
    assert_eq!(
        report["header_local"]["transparent"],
        serde_json::json!([color.alpha, color.red, color.green, color.blue])
    );
    let compatible = [
        clap_sys::version::clap_version { major: 0, minor: 99, revision: 99 },
        clap_sys::version::clap_version { major: 1, minor: 0, revision: 0 },
        clap_sys::version::clap_version { major: 2, minor: 0, revision: 0 },
    ]
    .map(clap_sys::version::clap_version_is_compatible);
    assert_eq!(
        report["header_local"]["compatible"],
        serde_json::json!(compatible)
    );
}

const _: fn(raw::clap_plugin_entry) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin_path: *const core::ffi::c_char) -> bool,
    > = value.init;
};
const _: fn(raw::clap_plugin_entry) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn()> = value.deinit;
};
const _: fn(raw::clap_plugin_entry) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(factory_id: *const core::ffi::c_char) -> *const core::ffi::c_void,
    > = value.get_factory;
};
const _: fn(raw::clap_host) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host,
            extension_id: *const core::ffi::c_char,
        ) -> *const core::ffi::c_void,
    > = value.get_extension;
};
const _: fn(raw::clap_host) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host)> =
        value.request_restart;
};
const _: fn(raw::clap_host) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host)> =
        value.request_process;
};
const _: fn(raw::clap_host) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host)> =
        value.request_callback;
};
const _: fn(raw::clap_input_events) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(list: *const raw::clap_input_events) -> u32,
    > = value.size;
};
const _: fn(raw::clap_input_events) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            list: *const raw::clap_input_events,
            index: u32,
        ) -> *const raw::clap_event_header_t,
    > = value.get;
};
const _: fn(raw::clap_output_events) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            list: *const raw::clap_output_events,
            event: *const raw::clap_event_header_t,
        ) -> bool,
    > = value.try_push;
};
const _: fn(raw::clap_plugin) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin) -> bool> =
        value.init;
};
const _: fn(raw::clap_plugin) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin)> =
        value.destroy;
};
const _: fn(raw::clap_plugin) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin,
            sample_rate: f64,
            min_frames_count: u32,
            max_frames_count: u32,
        ) -> bool,
    > = value.activate;
};
const _: fn(raw::clap_plugin) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin)> =
        value.deactivate;
};
const _: fn(raw::clap_plugin) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin) -> bool> =
        value.start_processing;
};
const _: fn(raw::clap_plugin) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin)> =
        value.stop_processing;
};
const _: fn(raw::clap_plugin) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin)> =
        value.reset;
};
const _: fn(raw::clap_plugin) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin,
            process: *const raw::clap_process_t,
        ) -> raw::clap_process_status,
    > = value.process;
};
const _: fn(raw::clap_plugin) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin,
            id: *const core::ffi::c_char,
        ) -> *const core::ffi::c_void,
    > = value.get_extension;
};
const _: fn(raw::clap_plugin) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin)> =
        value.on_main_thread;
};
const _: fn(raw::clap_plugin_factory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(factory: *const raw::clap_plugin_factory) -> u32,
    > = value.get_plugin_count;
};
const _: fn(raw::clap_plugin_factory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            factory: *const raw::clap_plugin_factory,
            index: u32,
        ) -> *const raw::clap_plugin_descriptor_t,
    > = value.get_plugin_descriptor;
};
const _: fn(raw::clap_plugin_factory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            factory: *const raw::clap_plugin_factory,
            host: *const raw::clap_host_t,
            plugin_id: *const core::ffi::c_char,
        ) -> *const raw::clap_plugin_t,
    > = value.create_plugin;
};
const _: fn(raw::clap_preset_discovery_metadata_receiver) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            receiver: *const raw::clap_preset_discovery_metadata_receiver,
            os_error: i32,
            error_message: *const core::ffi::c_char,
        ),
    > = value.on_error;
};
const _: fn(raw::clap_preset_discovery_metadata_receiver) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            receiver: *const raw::clap_preset_discovery_metadata_receiver,
            name: *const core::ffi::c_char,
            load_key: *const core::ffi::c_char,
        ) -> bool,
    > = value.begin_preset;
};
const _: fn(raw::clap_preset_discovery_metadata_receiver) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            receiver: *const raw::clap_preset_discovery_metadata_receiver,
            plugin_id: *const raw::clap_universal_plugin_id_t,
        ),
    > = value.add_plugin_id;
};
const _: fn(raw::clap_preset_discovery_metadata_receiver) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            receiver: *const raw::clap_preset_discovery_metadata_receiver,
            soundpack_id: *const core::ffi::c_char,
        ),
    > = value.set_soundpack_id;
};
const _: fn(raw::clap_preset_discovery_metadata_receiver) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            receiver: *const raw::clap_preset_discovery_metadata_receiver,
            flags: u32,
        ),
    > = value.set_flags;
};
const _: fn(raw::clap_preset_discovery_metadata_receiver) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            receiver: *const raw::clap_preset_discovery_metadata_receiver,
            creator: *const core::ffi::c_char,
        ),
    > = value.add_creator;
};
const _: fn(raw::clap_preset_discovery_metadata_receiver) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            receiver: *const raw::clap_preset_discovery_metadata_receiver,
            description: *const core::ffi::c_char,
        ),
    > = value.set_description;
};
const _: fn(raw::clap_preset_discovery_metadata_receiver) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            receiver: *const raw::clap_preset_discovery_metadata_receiver,
            creation_time: raw::clap_timestamp,
            modification_time: raw::clap_timestamp,
        ),
    > = value.set_timestamps;
};
const _: fn(raw::clap_preset_discovery_metadata_receiver) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            receiver: *const raw::clap_preset_discovery_metadata_receiver,
            feature: *const core::ffi::c_char,
        ),
    > = value.add_feature;
};
const _: fn(raw::clap_preset_discovery_metadata_receiver) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            receiver: *const raw::clap_preset_discovery_metadata_receiver,
            key: *const core::ffi::c_char,
            value: *const core::ffi::c_char,
        ),
    > = value.add_extra_info;
};
const _: fn(raw::clap_preset_discovery_provider) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(provider: *const raw::clap_preset_discovery_provider) -> bool,
    > = value.init;
};
const _: fn(raw::clap_preset_discovery_provider) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(provider: *const raw::clap_preset_discovery_provider),
    > = value.destroy;
};
const _: fn(raw::clap_preset_discovery_provider) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            provider: *const raw::clap_preset_discovery_provider,
            location_kind: u32,
            location: *const core::ffi::c_char,
            metadata_receiver: *const raw::clap_preset_discovery_metadata_receiver_t,
        ) -> bool,
    > = value.get_metadata;
};
const _: fn(raw::clap_preset_discovery_provider) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            provider: *const raw::clap_preset_discovery_provider,
            extension_id: *const core::ffi::c_char,
        ) -> *const core::ffi::c_void,
    > = value.get_extension;
};
const _: fn(raw::clap_preset_discovery_indexer) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            indexer: *const raw::clap_preset_discovery_indexer,
            filetype: *const raw::clap_preset_discovery_filetype_t,
        ) -> bool,
    > = value.declare_filetype;
};
const _: fn(raw::clap_preset_discovery_indexer) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            indexer: *const raw::clap_preset_discovery_indexer,
            location: *const raw::clap_preset_discovery_location_t,
        ) -> bool,
    > = value.declare_location;
};
const _: fn(raw::clap_preset_discovery_indexer) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            indexer: *const raw::clap_preset_discovery_indexer,
            soundpack: *const raw::clap_preset_discovery_soundpack_t,
        ) -> bool,
    > = value.declare_soundpack;
};
const _: fn(raw::clap_preset_discovery_indexer) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            indexer: *const raw::clap_preset_discovery_indexer,
            extension_id: *const core::ffi::c_char,
        ) -> *const core::ffi::c_void,
    > = value.get_extension;
};
const _: fn(raw::clap_preset_discovery_factory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(factory: *const raw::clap_preset_discovery_factory) -> u32,
    > = value.count;
};
const _: fn(raw::clap_preset_discovery_factory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            factory: *const raw::clap_preset_discovery_factory,
            index: u32,
        ) -> *const raw::clap_preset_discovery_provider_descriptor_t,
    > = value.get_descriptor;
};
const _: fn(raw::clap_preset_discovery_factory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            factory: *const raw::clap_preset_discovery_factory,
            indexer: *const raw::clap_preset_discovery_indexer_t,
            provider_id: *const core::ffi::c_char,
        ) -> *const raw::clap_preset_discovery_provider_t,
    > = value.create;
};
const _: fn(raw::clap_plugin_ambisonic) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            config: *const raw::clap_ambisonic_config_t,
        ) -> bool,
    > = value.is_config_supported;
};
const _: fn(raw::clap_plugin_ambisonic) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            is_input: bool,
            port_index: u32,
            config: *mut raw::clap_ambisonic_config_t,
        ) -> bool,
    > = value.get_config;
};
const _: fn(raw::clap_host_ambisonic) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.changed;
};
const _: fn(raw::clap_plugin_audio_ports_activation) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin_t) -> bool> =
        value.can_activate_while_processing;
};
const _: fn(raw::clap_plugin_audio_ports_activation) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            is_input: bool,
            port_index: u32,
            is_active: bool,
            sample_size: u32,
        ) -> bool,
    > = value.set_active;
};
const _: fn(raw::clap_plugin_audio_ports) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin: *const raw::clap_plugin_t, is_input: bool) -> u32,
    > = value.count;
};
const _: fn(raw::clap_plugin_audio_ports) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            index: u32,
            is_input: bool,
            info: *mut raw::clap_audio_port_info_t,
        ) -> bool,
    > = value.get;
};
const _: fn(raw::clap_host_audio_ports) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t, flag: u32) -> bool,
    > = value.is_rescan_flag_supported;
};
const _: fn(raw::clap_host_audio_ports) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t, flags: u32)> =
        value.rescan;
};
const _: fn(raw::clap_plugin_audio_ports_config) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin_t) -> u32> =
        value.count;
};
const _: fn(raw::clap_plugin_audio_ports_config) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            index: u32,
            config: *mut raw::clap_audio_ports_config_t,
        ) -> bool,
    > = value.get;
};
const _: fn(raw::clap_plugin_audio_ports_config) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin: *const raw::clap_plugin_t, config_id: raw::clap_id) -> bool,
    > = value.select;
};
const _: fn(raw::clap_plugin_audio_ports_config_info) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin: *const raw::clap_plugin_t) -> raw::clap_id,
    > = value.current_config;
};
const _: fn(raw::clap_plugin_audio_ports_config_info) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            config_id: raw::clap_id,
            port_index: u32,
            is_input: bool,
            info: *mut raw::clap_audio_port_info_t,
        ) -> bool,
    > = value.get;
};
const _: fn(raw::clap_host_audio_ports_config) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.rescan;
};
const _: fn(raw::clap_plugin_configurable_audio_ports) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            requests: *const raw::clap_audio_port_configuration_request,
            request_count: u32,
        ) -> bool,
    > = value.can_apply_configuration;
};
const _: fn(raw::clap_plugin_configurable_audio_ports) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            requests: *const raw::clap_audio_port_configuration_request,
            request_count: u32,
        ) -> bool,
    > = value.apply_configuration;
};
const _: fn(raw::clap_context_menu_builder) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            builder: *const raw::clap_context_menu_builder,
            item_kind: raw::clap_context_menu_item_kind_t,
            item_data: *const core::ffi::c_void,
        ) -> bool,
    > = value.add_item;
};
const _: fn(raw::clap_context_menu_builder) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            builder: *const raw::clap_context_menu_builder,
            item_kind: raw::clap_context_menu_item_kind_t,
        ) -> bool,
    > = value.supports;
};
const _: fn(raw::clap_plugin_context_menu) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            target: *const raw::clap_context_menu_target_t,
            builder: *const raw::clap_context_menu_builder_t,
        ) -> bool,
    > = value.populate;
};
const _: fn(raw::clap_plugin_context_menu) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            target: *const raw::clap_context_menu_target_t,
            action_id: raw::clap_id,
        ) -> bool,
    > = value.perform;
};
const _: fn(raw::clap_host_context_menu) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            target: *const raw::clap_context_menu_target_t,
            builder: *const raw::clap_context_menu_builder_t,
        ) -> bool,
    > = value.populate;
};
const _: fn(raw::clap_host_context_menu) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            target: *const raw::clap_context_menu_target_t,
            action_id: raw::clap_id,
        ) -> bool,
    > = value.perform;
};
const _: fn(raw::clap_host_context_menu) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t) -> bool> =
        value.can_popup;
};
const _: fn(raw::clap_host_context_menu) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            target: *const raw::clap_context_menu_target_t,
            screen_index: i32,
            x: i32,
            y: i32,
        ) -> bool,
    > = value.popup;
};
const _: fn(raw::clap_host_event_registry) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            space_name: *const core::ffi::c_char,
            space_id: *mut u16,
        ) -> bool,
    > = value.query;
};
const _: fn(raw::clap_plugin_gui) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            api: *const core::ffi::c_char,
            is_floating: bool,
        ) -> bool,
    > = value.is_api_supported;
};
const _: fn(raw::clap_plugin_gui) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            api: *mut *const core::ffi::c_char,
            is_floating: *mut bool,
        ) -> bool,
    > = value.get_preferred_api;
};
const _: fn(raw::clap_plugin_gui) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            api: *const core::ffi::c_char,
            is_floating: bool,
        ) -> bool,
    > = value.create;
};
const _: fn(raw::clap_plugin_gui) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin_t)> =
        value.destroy;
};
const _: fn(raw::clap_plugin_gui) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin: *const raw::clap_plugin_t, scale: f64) -> bool,
    > = value.set_scale;
};
const _: fn(raw::clap_plugin_gui) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            width: *mut u32,
            height: *mut u32,
        ) -> bool,
    > = value.get_size;
};
const _: fn(raw::clap_plugin_gui) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin_t) -> bool> =
        value.can_resize;
};
const _: fn(raw::clap_plugin_gui) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            hints: *mut raw::clap_gui_resize_hints_t,
        ) -> bool,
    > = value.get_resize_hints;
};
const _: fn(raw::clap_plugin_gui) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            width: *mut u32,
            height: *mut u32,
        ) -> bool,
    > = value.adjust_size;
};
const _: fn(raw::clap_plugin_gui) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin: *const raw::clap_plugin_t, width: u32, height: u32) -> bool,
    > = value.set_size;
};
const _: fn(raw::clap_plugin_gui) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            window: *const raw::clap_window_t,
        ) -> bool,
    > = value.set_parent;
};
const _: fn(raw::clap_plugin_gui) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            window: *const raw::clap_window_t,
        ) -> bool,
    > = value.set_transient;
};
const _: fn(raw::clap_plugin_gui) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin: *const raw::clap_plugin_t, title: *const core::ffi::c_char),
    > = value.suggest_title;
};
const _: fn(raw::clap_plugin_gui) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin_t) -> bool> =
        value.show;
};
const _: fn(raw::clap_plugin_gui) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin_t) -> bool> =
        value.hide;
};
const _: fn(raw::clap_host_gui) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.resize_hints_changed;
};
const _: fn(raw::clap_host_gui) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t, width: u32, height: u32) -> bool,
    > = value.request_resize;
};
const _: fn(raw::clap_host_gui) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t) -> bool> =
        value.request_show;
};
const _: fn(raw::clap_host_gui) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t) -> bool> =
        value.request_hide;
};
const _: fn(raw::clap_host_gui) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t, was_destroyed: bool),
    > = value.closed;
};
const _: fn(raw::clap_plugin_latency) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin_t) -> u32> =
        value.get;
};
const _: fn(raw::clap_host_latency) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.changed;
};
const _: fn(raw::clap_host_log) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            severity: raw::clap_log_severity,
            msg: *const core::ffi::c_char,
        ),
    > = value.log;
};
const _: fn(raw::clap_plugin_note_name) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin_t) -> u32> =
        value.count;
};
const _: fn(raw::clap_plugin_note_name) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            index: u32,
            note_name: *mut raw::clap_note_name_t,
        ) -> bool,
    > = value.get;
};
const _: fn(raw::clap_host_note_name) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.changed;
};
const _: fn(raw::clap_plugin_note_ports) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin: *const raw::clap_plugin_t, is_input: bool) -> u32,
    > = value.count;
};
const _: fn(raw::clap_plugin_note_ports) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            index: u32,
            is_input: bool,
            info: *mut raw::clap_note_port_info_t,
        ) -> bool,
    > = value.get;
};
const _: fn(raw::clap_host_note_ports) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t) -> u32> =
        value.supported_dialects;
};
const _: fn(raw::clap_host_note_ports) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t, flags: u32)> =
        value.rescan;
};
const _: fn(raw::clap_plugin_params) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin_t) -> u32> =
        value.count;
};
const _: fn(raw::clap_plugin_params) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            param_index: u32,
            param_info: *mut raw::clap_param_info_t,
        ) -> bool,
    > = value.get_info;
};
const _: fn(raw::clap_plugin_params) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            param_id: raw::clap_id,
            out_value: *mut f64,
        ) -> bool,
    > = value.get_value;
};
const _: fn(raw::clap_plugin_params) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            param_id: raw::clap_id,
            value: f64,
            out_buffer: *mut core::ffi::c_char,
            out_buffer_capacity: u32,
        ) -> bool,
    > = value.value_to_text;
};
const _: fn(raw::clap_plugin_params) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            param_id: raw::clap_id,
            param_value_text: *const core::ffi::c_char,
            out_value: *mut f64,
        ) -> bool,
    > = value.text_to_value;
};
const _: fn(raw::clap_plugin_params) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            in_: *const raw::clap_input_events_t,
            out: *const raw::clap_output_events_t,
        ),
    > = value.flush;
};
const _: fn(raw::clap_host_params) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t, flags: raw::clap_param_rescan_flags),
    > = value.rescan;
};
const _: fn(raw::clap_host_params) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            param_id: raw::clap_id,
            flags: raw::clap_param_clear_flags,
        ),
    > = value.clear;
};
const _: fn(raw::clap_host_params) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.request_flush;
};
const _: fn(raw::clap_plugin_param_indication) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            param_id: raw::clap_id,
            has_mapping: bool,
            color: *const raw::clap_color_t,
            label: *const core::ffi::c_char,
            description: *const core::ffi::c_char,
        ),
    > = value.set_mapping;
};
const _: fn(raw::clap_plugin_param_indication) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            param_id: raw::clap_id,
            automation_state: u32,
            color: *const raw::clap_color_t,
        ),
    > = value.set_automation;
};
const _: fn(raw::clap_plugin_posix_fd_support) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            fd: core::ffi::c_int,
            flags: raw::clap_posix_fd_flags_t,
        ),
    > = value.on_fd;
};
const _: fn(raw::clap_host_posix_fd_support) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            fd: core::ffi::c_int,
            flags: raw::clap_posix_fd_flags_t,
        ) -> bool,
    > = value.register_fd;
};
const _: fn(raw::clap_host_posix_fd_support) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            fd: core::ffi::c_int,
            flags: raw::clap_posix_fd_flags_t,
        ) -> bool,
    > = value.modify_fd;
};
const _: fn(raw::clap_host_posix_fd_support) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t, fd: core::ffi::c_int) -> bool,
    > = value.unregister_fd;
};
const _: fn(raw::clap_plugin_preset_load) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            location_kind: u32,
            location: *const core::ffi::c_char,
            load_key: *const core::ffi::c_char,
        ) -> bool,
    > = value.from_location;
};
const _: fn(raw::clap_host_preset_load) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            location_kind: u32,
            location: *const core::ffi::c_char,
            load_key: *const core::ffi::c_char,
            os_error: i32,
            msg: *const core::ffi::c_char,
        ),
    > = value.on_error;
};
const _: fn(raw::clap_host_preset_load) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            location_kind: u32,
            location: *const core::ffi::c_char,
            load_key: *const core::ffi::c_char,
        ),
    > = value.loaded;
};
const _: fn(raw::clap_plugin_remote_controls) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin_t) -> u32> =
        value.count;
};
const _: fn(raw::clap_plugin_remote_controls) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            page_index: u32,
            page: *mut raw::clap_remote_controls_page_t,
        ) -> bool,
    > = value.get;
};
const _: fn(raw::clap_host_remote_controls) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.changed;
};
const _: fn(raw::clap_host_remote_controls) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t, page_id: raw::clap_id),
    > = value.suggest_page;
};
const _: fn(raw::clap_plugin_render) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin_t) -> bool> =
        value.has_hard_realtime_requirement;
};
const _: fn(raw::clap_plugin_render) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            mode: raw::clap_plugin_render_mode,
        ) -> bool,
    > = value.set;
};
const _: fn(raw::clap_istream) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            stream: *const raw::clap_istream,
            buffer: *mut core::ffi::c_void,
            size: u64,
        ) -> i64,
    > = value.read;
};
const _: fn(raw::clap_ostream) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            stream: *const raw::clap_ostream,
            buffer: *const core::ffi::c_void,
            size: u64,
        ) -> i64,
    > = value.write;
};
const _: fn(raw::clap_plugin_state_context) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            stream: *const raw::clap_ostream_t,
            context_type: u32,
        ) -> bool,
    > = value.save;
};
const _: fn(raw::clap_plugin_state_context) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            stream: *const raw::clap_istream_t,
            context_type: u32,
        ) -> bool,
    > = value.load;
};
const _: fn(raw::clap_plugin_state) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            stream: *const raw::clap_ostream_t,
        ) -> bool,
    > = value.save;
};
const _: fn(raw::clap_plugin_state) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            stream: *const raw::clap_istream_t,
        ) -> bool,
    > = value.load;
};
const _: fn(raw::clap_host_state) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.mark_dirty;
};
const _: fn(raw::clap_plugin_surround) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin: *const raw::clap_plugin_t, channel_mask: u64) -> bool,
    > = value.is_channel_mask_supported;
};
const _: fn(raw::clap_plugin_surround) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            is_input: bool,
            port_index: u32,
            channel_map: *mut u8,
            channel_map_capacity: u32,
        ) -> u32,
    > = value.get_channel_map;
};
const _: fn(raw::clap_host_surround) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.changed;
};
const _: fn(raw::clap_plugin_tail) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin_t) -> u32> =
        value.get;
};
const _: fn(raw::clap_host_tail) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.changed;
};
const _: fn(raw::clap_host_thread_check) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t) -> bool> =
        value.is_main_thread;
};
const _: fn(raw::clap_host_thread_check) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t) -> bool> =
        value.is_audio_thread;
};
const _: fn(raw::clap_plugin_thread_pool) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin: *const raw::clap_plugin_t, task_index: u32),
    > = value.exec;
};
const _: fn(raw::clap_host_thread_pool) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t, num_tasks: u32) -> bool,
    > = value.request_exec;
};
const _: fn(raw::clap_plugin_timer_support) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin: *const raw::clap_plugin_t, timer_id: raw::clap_id),
    > = value.on_timer;
};
const _: fn(raw::clap_host_timer_support) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            period_ms: u32,
            timer_id: *mut raw::clap_id,
        ) -> bool,
    > = value.register_timer;
};
const _: fn(raw::clap_host_timer_support) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t, timer_id: raw::clap_id) -> bool,
    > = value.unregister_timer;
};
const _: fn(raw::clap_plugin_track_info) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin_t)> =
        value.changed;
};
const _: fn(raw::clap_host_track_info) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            info: *mut raw::clap_track_info_t,
        ) -> bool,
    > = value.get;
};
const _: fn(raw::clap_plugin_voice_info) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            info: *mut raw::clap_voice_info_t,
        ) -> bool,
    > = value.get;
};
const _: fn(raw::clap_host_voice_info) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.changed;
};
const _: fn(raw::clap_plugin_invalidation_factory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(factory: *const raw::clap_plugin_invalidation_factory) -> u32,
    > = value.count;
};
const _: fn(raw::clap_plugin_invalidation_factory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            factory: *const raw::clap_plugin_invalidation_factory,
            index: u32,
        ) -> *const raw::clap_plugin_invalidation_source_t,
    > = value.get;
};
const _: fn(raw::clap_plugin_invalidation_factory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(factory: *const raw::clap_plugin_invalidation_factory) -> bool,
    > = value.refresh;
};
const _: fn(raw::clap_plugin_state_converter) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(converter: *mut raw::clap_plugin_state_converter),
    > = value.destroy;
};
const _: fn(raw::clap_plugin_state_converter) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            converter: *mut raw::clap_plugin_state_converter,
            src: *const raw::clap_istream_t,
            dst: *const raw::clap_ostream_t,
            error_buffer: *mut core::ffi::c_char,
            error_buffer_size: usize,
        ) -> bool,
    > = value.convert_state;
};
const _: fn(raw::clap_plugin_state_converter) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            converter: *mut raw::clap_plugin_state_converter,
            src_param_id: raw::clap_id,
            src_normalized_value: f64,
            dst_param_id: *mut raw::clap_id,
            dst_normalized_value: *mut f64,
        ) -> bool,
    > = value.convert_normalized_value;
};
const _: fn(raw::clap_plugin_state_converter) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            converter: *mut raw::clap_plugin_state_converter,
            src_param_id: raw::clap_id,
            src_plain_value: f64,
            dst_param_id: *mut raw::clap_id,
            dst_plain_value: *mut f64,
        ) -> bool,
    > = value.convert_plain_value;
};
const _: fn(raw::clap_plugin_state_converter_factory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(factory: *const raw::clap_plugin_state_converter_factory) -> u32,
    > = value.count;
};
const _: fn(raw::clap_plugin_state_converter_factory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            factory: *const raw::clap_plugin_state_converter_factory,
            index: u32,
        ) -> *const raw::clap_plugin_state_converter_descriptor_t,
    > = value.get_descriptor;
};
const _: fn(raw::clap_plugin_state_converter_factory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            factory: *const raw::clap_plugin_state_converter_factory,
            converter_id: *const core::ffi::c_char,
        ) -> *mut raw::clap_plugin_state_converter_t,
    > = value.create;
};
const _: fn(raw::clap_plugin_background_activation) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *mut raw::clap_plugin_t,
            sample_rate: f64,
            min_frames_count: u32,
            max_frames_count: u32,
        ) -> bool,
    > = value.activate_from_background_thread;
};
const _: fn(raw::clap_plugin_background_activation) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin)> =
        value.deactivate_from_background_thread;
};
const _: fn(raw::clap_host_background_progress) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t) -> bool> =
        value.is_canceled;
};
const _: fn(raw::clap_host_background_progress) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            progress: f64,
            msg: *const core::ffi::c_char,
        ),
    > = value.progress;
};
const _: fn(raw::clap_plugin_background_state_context) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            stream: *const raw::clap_ostream_t,
            context_type: u32,
        ) -> bool,
    > = value.save_from_background_thread;
};
const _: fn(raw::clap_plugin_background_state_context) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            stream: *const raw::clap_istream_t,
            context_type: u32,
        ) -> bool,
    > = value.load_from_background_thread;
};
const _: fn(raw::clap_plugin_extensible_audio_ports) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            is_input: bool,
            channel_count: u32,
            port_type: *const core::ffi::c_char,
            port_details: *const core::ffi::c_void,
        ) -> bool,
    > = value.add_port;
};
const _: fn(raw::clap_plugin_extensible_audio_ports) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin: *const raw::clap_plugin_t, is_input: bool, index: u32) -> bool,
    > = value.remove_port;
};
const _: fn(raw::clap_plugin_flush_events) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            in_: *const raw::clap_input_events_t,
            out: *const raw::clap_output_events_t,
        ),
    > = value.flush;
};
const _: fn(raw::clap_host_flush_events) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.request_flush;
};
const _: fn(raw::clap_plugin_gain_adjustment_metering) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin_t) -> f64> =
        value.get;
};
const _: fn(raw::clap_plugin_mini_curve_display) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin_t) -> u32> =
        value.get_curve_count;
};
const _: fn(raw::clap_plugin_mini_curve_display) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            curves: *mut raw::clap_mini_curve_display_curve_data_t,
            curves_size: u32,
        ) -> u32,
    > = value.render;
};
const _: fn(raw::clap_plugin_mini_curve_display) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin: *const raw::clap_plugin_t, is_observed: bool),
    > = value.set_observed;
};
const _: fn(raw::clap_plugin_mini_curve_display) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            curve_index: u32,
            x_name: *mut core::ffi::c_char,
            y_name: *mut core::ffi::c_char,
            name_capacity: u32,
        ) -> bool,
    > = value.get_axis_name;
};
const _: fn(raw::clap_host_mini_curve_display) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            kind: u32,
            hints: *mut raw::clap_mini_curve_display_curve_hints_t,
        ) -> bool,
    > = value.get_hints;
};
const _: fn(raw::clap_host_mini_curve_display) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t, is_dynamic: bool),
    > = value.set_dynamic;
};
const _: fn(raw::clap_host_mini_curve_display) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t, flags: u32)> =
        value.changed;
};
const _: fn(raw::clap_plugin_octave_number) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin: *const raw::clap_plugin_t, octave_number: i8),
    > = value.set_note60_octave;
};
const _: fn(raw::clap_plugin_params_origin) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            param_id: raw::clap_id,
            out_value: *mut f64,
        ) -> bool,
    > = value.get;
};
const _: fn(raw::clap_host_params_origin) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.changed;
};
const _: fn(raw::clap_host_param_hovered) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t, hovered_param_id: raw::clap_id),
    > = value.update;
};
const _: fn(raw::clap_plugin_project_location) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            path: *const raw::clap_project_location_element_t,
            num_elements: u32,
        ),
    > = value.set;
};
const _: fn(raw::clap_plugin_resource_directory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            path: *const core::ffi::c_char,
            is_shared: bool,
        ),
    > = value.set_directory;
};
const _: fn(raw::clap_plugin_resource_directory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin: *const raw::clap_plugin_t, all: bool),
    > = value.collect;
};
const _: fn(raw::clap_plugin_resource_directory) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin_t) -> u32> =
        value.get_files_count;
};
const _: fn(raw::clap_plugin_resource_directory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            index: u32,
            path: *mut core::ffi::c_char,
            path_size: u32,
        ) -> i32,
    > = value.get_file_path;
};
const _: fn(raw::clap_host_resource_directory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t, is_shared: bool) -> bool,
    > = value.request_directory;
};
const _: fn(raw::clap_host_resource_directory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t, is_shared: bool),
    > = value.release_directory;
};
const _: fn(raw::clap_host_scratch_memory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            scratch_size_bytes: u32,
            max_concurrency_hint: u32,
        ) -> bool,
    > = value.reserve;
};
const _: fn(raw::clap_host_scratch_memory) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t) -> *mut core::ffi::c_void,
    > = value.access;
};
const _: fn(raw::clap_host_transport_control) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.request_start;
};
const _: fn(raw::clap_host_transport_control) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.request_stop;
};
const _: fn(raw::clap_host_transport_control) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.request_continue;
};
const _: fn(raw::clap_host_transport_control) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.request_pause;
};
const _: fn(raw::clap_host_transport_control) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.request_toggle_play;
};
const _: fn(raw::clap_host_transport_control) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t, position: raw::clap_beattime),
    > = value.request_jump;
};
const _: fn(raw::clap_host_transport_control) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            start: raw::clap_beattime,
            duration: raw::clap_beattime,
        ),
    > = value.request_loop_region;
};
const _: fn(raw::clap_host_transport_control) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.request_toggle_loop;
};
const _: fn(raw::clap_host_transport_control) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t, is_enabled: bool),
    > = value.request_enable_loop;
};
const _: fn(raw::clap_host_transport_control) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t, is_recording: bool),
    > = value.request_record;
};
const _: fn(raw::clap_host_transport_control) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.request_toggle_record;
};
const _: fn(raw::clap_host_transport_control) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t, tempo: f64)> =
        value.request_tempo;
};
const _: fn(raw::clap_host_transport_control) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t, tsig_num: u16, tsig_denom: u16),
    > = value.request_time_signature;
};
const _: fn(raw::clap_plugin_triggers) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin_t) -> u32> =
        value.count;
};
const _: fn(raw::clap_plugin_triggers) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            index: u32,
            trigger_info: *mut raw::clap_trigger_info_t,
        ) -> bool,
    > = value.get_info;
};
const _: fn(raw::clap_host_triggers) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t, flags: raw::clap_trigger_rescan_flags),
    > = value.rescan;
};
const _: fn(raw::clap_host_triggers) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            trigger_id: raw::clap_id,
            flags: raw::clap_trigger_clear_flags,
        ),
    > = value.clear;
};
const _: fn(raw::clap_plugin_tuning) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(plugin: *const raw::clap_plugin_t)> =
        value.changed;
};
const _: fn(raw::clap_host_tuning) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            tuning_id: raw::clap_id,
            channel: i32,
            key: i32,
            sample_offset: u32,
        ) -> f64,
    > = value.get_relative;
};
const _: fn(raw::clap_host_tuning) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            tuning_id: raw::clap_id,
            channel: i32,
            key: i32,
        ) -> bool,
    > = value.should_play;
};
const _: fn(raw::clap_host_tuning) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t) -> u32> =
        value.get_tuning_count;
};
const _: fn(raw::clap_host_tuning) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            tuning_index: u32,
            info: *mut raw::clap_tuning_info_t,
        ) -> bool,
    > = value.get_info;
};
const _: fn(raw::clap_plugin_undo_delta) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            properties: *mut raw::clap_undo_delta_properties_t,
        ),
    > = value.get_delta_properties;
};
const _: fn(raw::clap_plugin_undo_delta) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            format_version: raw::clap_id,
        ) -> bool,
    > = value.can_use_delta_format_version;
};
const _: fn(raw::clap_plugin_undo_delta) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            format_version: raw::clap_id,
            delta: *const core::ffi::c_void,
            delta_size: usize,
        ) -> bool,
    > = value.undo;
};
const _: fn(raw::clap_plugin_undo_delta) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            format_version: raw::clap_id,
            delta: *const core::ffi::c_void,
            delta_size: usize,
        ) -> bool,
    > = value.redo;
};
const _: fn(raw::clap_plugin_undo_context) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin: *const raw::clap_plugin_t, can_undo: bool),
    > = value.set_can_undo;
};
const _: fn(raw::clap_plugin_undo_context) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin: *const raw::clap_plugin_t, can_redo: bool),
    > = value.set_can_redo;
};
const _: fn(raw::clap_plugin_undo_context) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin: *const raw::clap_plugin_t, name: *const core::ffi::c_char),
    > = value.set_undo_name;
};
const _: fn(raw::clap_plugin_undo_context) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(plugin: *const raw::clap_plugin_t, name: *const core::ffi::c_char),
    > = value.set_redo_name;
};
const _: fn(raw::clap_host_undo) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.begin_change;
};
const _: fn(raw::clap_host_undo) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.cancel_change;
};
const _: fn(raw::clap_host_undo) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            name: *const core::ffi::c_char,
            delta: *const core::ffi::c_void,
            delta_size: usize,
            delta_can_undo: bool,
        ),
    > = value.change_made;
};
const _: fn(raw::clap_host_undo) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.request_undo;
};
const _: fn(raw::clap_host_undo) = |value| {
    let _: ::core::option::Option<unsafe extern "C" fn(host: *const raw::clap_host_t)> =
        value.request_redo;
};
const _: fn(raw::clap_host_undo) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(host: *const raw::clap_host_t, is_subscribed: bool),
    > = value.set_wants_context_updates;
};
const _: fn(raw::clap_plugin_webview) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            uri: *mut core::ffi::c_char,
            uri_capacity: u32,
        ) -> i32,
    > = value.get_uri;
};
const _: fn(raw::clap_plugin_webview) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            path: *const core::ffi::c_char,
            mime: *mut core::ffi::c_char,
            mime_capacity: u32,
            data_stream: *const raw::clap_ostream_t,
        ) -> bool,
    > = value.get_resource;
};
const _: fn(raw::clap_plugin_webview) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            plugin: *const raw::clap_plugin_t,
            buffer: *const core::ffi::c_void,
            size: u32,
        ) -> bool,
    > = value.receive;
};
const _: fn(raw::clap_host_webview) = |value| {
    let _: ::core::option::Option<
        unsafe extern "C" fn(
            host: *const raw::clap_host_t,
            buffer: *const core::ffi::c_void,
            size: u32,
        ) -> bool,
    > = value.send;
};
