//! Reexports of declarations from official `clap/ext/audio-ports.h`.
pub use crate::raw::{
    clap_audio_port_info, clap_audio_port_info_t, clap_host_audio_ports, clap_host_audio_ports_t,
    clap_host_t, clap_id, clap_plugin_audio_ports, clap_plugin_audio_ports_t, clap_plugin_t,
    CLAP_AUDIO_PORTS_RESCAN_CHANNEL_COUNT, CLAP_AUDIO_PORTS_RESCAN_FLAGS,
    CLAP_AUDIO_PORTS_RESCAN_IN_PLACE_PAIR, CLAP_AUDIO_PORTS_RESCAN_LIST,
    CLAP_AUDIO_PORTS_RESCAN_NAMES, CLAP_AUDIO_PORTS_RESCAN_PORT_TYPE, CLAP_AUDIO_PORT_IS_MAIN,
    CLAP_AUDIO_PORT_PREFERS_64BITS, CLAP_AUDIO_PORT_REQUIRES_COMMON_SAMPLE_SIZE,
    CLAP_AUDIO_PORT_SUPPORTS_64BITS, CLAP_EXT_AUDIO_PORTS, CLAP_INVALID_ID, CLAP_NAME_SIZE,
    CLAP_PORT_AMBISONIC, CLAP_PORT_MONO, CLAP_PORT_STEREO, CLAP_PORT_SURROUND,
};
