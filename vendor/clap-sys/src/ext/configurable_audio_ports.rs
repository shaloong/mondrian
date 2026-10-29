//! Reexports of declarations from official `clap/ext/configurable-audio-ports.h`.
pub use crate::raw::{
    clap_ambisonic_config_t, clap_audio_port_configuration_request,
    clap_audio_port_configuration_request_t, clap_audio_port_info, clap_host_audio_ports,
    clap_plugin_configurable_audio_ports, clap_plugin_configurable_audio_ports_t, clap_plugin_t,
    CLAP_EXT_CONFIGURABLE_AUDIO_PORTS, CLAP_EXT_CONFIGURABLE_AUDIO_PORTS_COMPAT,
    CLAP_PORT_AMBISONIC, CLAP_PORT_MONO, CLAP_PORT_STEREO, CLAP_PORT_SURROUND,
};
