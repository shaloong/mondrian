//! Rust value/thread-marker support, without alternate FFI declarations.
use crate::raw::*;

/// Header-local version value; its fields come from generated official macros.
pub const CLAP_VERSION: clap_version = clap_version {
    major: CLAP_VERSION_MAJOR,
    minor: CLAP_VERSION_MINOR,
    revision: CLAP_VERSION_REVISION,
};

/// Header-local transparent color, independently compared with native color.h.
pub const CLAP_COLOR_TRANSPARENT: clap_color = clap_color { alpha: 0, red: 0, green: 0, blue: 0 };

/// The exact header-local compatibility predicate from official version.h.
pub const fn clap_version_is_compatible(version: clap_version) -> bool {
    version.major >= 1
}

// SAFETY: Maintainer binding src/audio_buffer.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_audio_buffer {}

// SAFETY: Maintainer binding src/audio_buffer.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_audio_buffer {}

// SAFETY: Maintainer binding src/events.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_event_param_value {}

// SAFETY: Maintainer binding src/events.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_event_param_value {}

// SAFETY: Maintainer binding src/events.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_event_param_mod {}

// SAFETY: Maintainer binding src/events.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_event_param_mod {}

// SAFETY: Maintainer binding src/events.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_event_midi_sysex {}

// SAFETY: Maintainer binding src/events.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_event_midi_sysex {}

// SAFETY: Maintainer binding src/events.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_input_events {}

// SAFETY: Maintainer binding src/events.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_input_events {}

// SAFETY: Maintainer binding src/events.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_output_events {}

// SAFETY: Maintainer binding src/events.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_output_events {}

// SAFETY: Maintainer binding src/ext/audio_ports.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_audio_port_info {}

// SAFETY: Maintainer binding src/ext/audio_ports.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_audio_port_info {}

// SAFETY: Maintainer binding src/ext/audio_ports_config.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_audio_ports_config {}

// SAFETY: Maintainer binding src/ext/audio_ports_config.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_audio_ports_config {}

// SAFETY: Maintainer binding src/ext/configurable_audio_ports.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_audio_port_configuration_request {}

// SAFETY: Maintainer binding src/ext/configurable_audio_ports.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_audio_port_configuration_request {}

// SAFETY: Maintainer binding src/ext/context_menu.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_context_menu_entry {}

// SAFETY: Maintainer binding src/ext/context_menu.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_context_menu_entry {}

// SAFETY: Maintainer binding src/ext/context_menu.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_context_menu_check_entry {}

// SAFETY: Maintainer binding src/ext/context_menu.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_context_menu_check_entry {}

// SAFETY: Maintainer binding src/ext/context_menu.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_context_menu_item_title {}

// SAFETY: Maintainer binding src/ext/context_menu.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_context_menu_item_title {}

// SAFETY: Maintainer binding src/ext/context_menu.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_context_menu_submenu {}

// SAFETY: Maintainer binding src/ext/context_menu.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_context_menu_submenu {}

// SAFETY: Maintainer binding src/ext/context_menu.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_context_menu_builder {}

// SAFETY: Maintainer binding src/ext/context_menu.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_context_menu_builder {}

// SAFETY: Maintainer binding src/ext/draft/triggers.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_event_trigger {}

// SAFETY: Maintainer binding src/ext/draft/triggers.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_event_trigger {}

// SAFETY: Maintainer binding src/ext/draft/triggers.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_trigger_info {}

// SAFETY: Maintainer binding src/ext/draft/triggers.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_trigger_info {}

// SAFETY: Maintainer binding src/ext/draft/tuning.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_tuning_info {}

// SAFETY: Maintainer binding src/ext/draft/tuning.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_tuning_info {}

// SAFETY: Maintainer binding src/ext/gui.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_window {}

// SAFETY: Maintainer binding src/ext/gui.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_window {}

// SAFETY: Maintainer binding src/ext/gui.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_window__bindgen_ty_1 {}

// SAFETY: Maintainer binding src/ext/gui.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_window__bindgen_ty_1 {}

// SAFETY: Maintainer binding src/ext/params.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_param_info {}

// SAFETY: Maintainer binding src/ext/params.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_param_info {}

// SAFETY: Maintainer binding src/ext/track_info.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_track_info {}

// SAFETY: Maintainer binding src/ext/track_info.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_track_info {}

// SAFETY: Maintainer binding src/factory/draft/plugin_invalidation.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_plugin_invalidation_source {}

// SAFETY: Maintainer binding src/factory/draft/plugin_invalidation.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_plugin_invalidation_source {}

// SAFETY: Maintainer binding src/factory/draft/plugin_state_converter.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_plugin_state_converter_descriptor {}

// SAFETY: Maintainer binding src/factory/draft/plugin_state_converter.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_plugin_state_converter_descriptor {}

// SAFETY: Maintainer binding src/factory/draft/plugin_state_converter.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_plugin_state_converter {}

// SAFETY: Maintainer binding src/factory/draft/plugin_state_converter.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_plugin_state_converter {}

// SAFETY: Maintainer binding src/factory/preset_discovery.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_preset_discovery_metadata_receiver {}

// SAFETY: Maintainer binding src/factory/preset_discovery.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_preset_discovery_metadata_receiver {}

// SAFETY: Maintainer binding src/factory/preset_discovery.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_preset_discovery_filetype {}

// SAFETY: Maintainer binding src/factory/preset_discovery.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_preset_discovery_filetype {}

// SAFETY: Maintainer binding src/factory/preset_discovery.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_preset_discovery_location {}

// SAFETY: Maintainer binding src/factory/preset_discovery.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_preset_discovery_location {}

// SAFETY: Maintainer binding src/factory/preset_discovery.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_preset_discovery_soundpack {}

// SAFETY: Maintainer binding src/factory/preset_discovery.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_preset_discovery_soundpack {}

// SAFETY: Maintainer binding src/factory/preset_discovery.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_preset_discovery_provider_descriptor {}

// SAFETY: Maintainer binding src/factory/preset_discovery.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_preset_discovery_provider_descriptor {}

// SAFETY: Maintainer binding src/factory/preset_discovery.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_preset_discovery_provider {}

// SAFETY: Maintainer binding src/factory/preset_discovery.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_preset_discovery_provider {}

// SAFETY: Maintainer binding src/factory/preset_discovery.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_preset_discovery_indexer {}

// SAFETY: Maintainer binding src/factory/preset_discovery.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_preset_discovery_indexer {}

// SAFETY: Maintainer binding src/host.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_host {}

// SAFETY: Maintainer binding src/host.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_host {}

// SAFETY: Maintainer binding src/plugin.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_plugin_descriptor {}

// SAFETY: Maintainer binding src/plugin.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_plugin_descriptor {}

// SAFETY: Maintainer binding src/plugin.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_plugin {}

// SAFETY: Maintainer binding src/plugin.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_plugin {}

// SAFETY: Maintainer binding src/process.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_process {}

// SAFETY: Maintainer binding src/process.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_process {}

// SAFETY: Maintainer binding src/stream.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_istream {}

// SAFETY: Maintainer binding src/stream.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_istream {}

// SAFETY: Maintainer binding src/stream.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_ostream {}

// SAFETY: Maintainer binding src/stream.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_ostream {}

// SAFETY: Maintainer binding src/universal_plugin_id.rs marks this same raw POD/table Send.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Send for clap_universal_plugin_id {}

// SAFETY: Maintainer binding src/universal_plugin_id.rs marks this same raw POD/table Sync.
// Moving/copying addresses does not dereference them or invoke a callback;
// unsafe callers still enforce official lifetime, thread, and synchronization contracts.
unsafe impl Sync for clap_universal_plugin_id {}
