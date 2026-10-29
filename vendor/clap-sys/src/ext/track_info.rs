//! Reexports of declarations from official `clap/ext/track-info.h`.
pub use crate::raw::{
    clap_audio_port_info, clap_color_t, clap_host_t, clap_host_track_info, clap_host_track_info_t,
    clap_plugin_t, clap_plugin_track_info, clap_plugin_track_info_t, clap_track_info,
    clap_track_info_t, CLAP_EXT_TRACK_INFO, CLAP_EXT_TRACK_INFO_COMPAT, CLAP_NAME_SIZE,
    CLAP_TRACK_INFO_HAS_AUDIO_CHANNEL, CLAP_TRACK_INFO_HAS_TRACK_COLOR,
    CLAP_TRACK_INFO_HAS_TRACK_NAME, CLAP_TRACK_INFO_IS_FOR_BUS, CLAP_TRACK_INFO_IS_FOR_MASTER,
    CLAP_TRACK_INFO_IS_FOR_RETURN_TRACK,
};
