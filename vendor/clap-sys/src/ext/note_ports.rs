//! Reexports of declarations from official `clap/ext/note-ports.h`.
pub use crate::raw::{
    clap_event_midi, clap_event_midi2, clap_event_note, clap_event_note_expression,
    clap_host_note_ports, clap_host_note_ports_t, clap_host_t, clap_id, clap_note_dialect,
    clap_note_port_info, clap_note_port_info_t, clap_plugin_note_ports, clap_plugin_note_ports_t,
    clap_plugin_t, CLAP_EXT_NOTE_PORTS, CLAP_NAME_SIZE, CLAP_NOTE_DIALECT_CLAP,
    CLAP_NOTE_DIALECT_MIDI, CLAP_NOTE_DIALECT_MIDI2, CLAP_NOTE_DIALECT_MIDI_MPE,
    CLAP_NOTE_PORTS_RESCAN_ALL, CLAP_NOTE_PORTS_RESCAN_NAMES,
};
