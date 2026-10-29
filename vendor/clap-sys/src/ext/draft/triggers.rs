//! Reexports of declarations from official `clap/ext/draft/triggers.h`.
pub use crate::raw::{
    clap_event_header_t, clap_event_trigger, clap_event_trigger_t, clap_host, clap_host_t,
    clap_host_triggers, clap_host_triggers_t, clap_id, clap_param_info, clap_plugin, clap_plugin_t,
    clap_plugin_triggers, clap_plugin_triggers_t, clap_trigger_clear_flags, clap_trigger_info,
    clap_trigger_info_flags, clap_trigger_info_t, clap_trigger_rescan_flags, CLAP_EVENT_TRIGGER,
    CLAP_EXT_TRIGGERS, CLAP_NAME_SIZE, CLAP_PATH_SIZE, CLAP_TRIGGER_CLEAR_ALL,
    CLAP_TRIGGER_CLEAR_AUTOMATIONS, CLAP_TRIGGER_IS_AUTOMATABLE_PER_CHANNEL,
    CLAP_TRIGGER_IS_AUTOMATABLE_PER_KEY, CLAP_TRIGGER_IS_AUTOMATABLE_PER_NOTE_ID,
    CLAP_TRIGGER_IS_AUTOMATABLE_PER_PORT, CLAP_TRIGGER_RESCAN_ALL, CLAP_TRIGGER_RESCAN_INFO,
};
