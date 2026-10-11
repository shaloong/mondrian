//! Reexports of declarations from official `clap/process.h`.
pub use crate::raw::{
    clap_audio_buffer_t, clap_event_transport_t, clap_input_events_t, clap_output_events_t,
    clap_plugin_audio_ports, clap_plugin_tail, clap_process, clap_process_status, clap_process_t,
    CLAP_PROCESS_CONTINUE, CLAP_PROCESS_CONTINUE_IF_NOT_QUIET, CLAP_PROCESS_ERROR,
    CLAP_PROCESS_SLEEP, CLAP_PROCESS_TAIL,
};
