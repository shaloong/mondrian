//! Reexports of declarations from official `clap/ext/log.h`.
pub use crate::raw::{
    clap_host_log, clap_host_log_t, clap_host_t, clap_log_severity, CLAP_EXT_LOG, CLAP_LOG_DEBUG,
    CLAP_LOG_ERROR, CLAP_LOG_FATAL, CLAP_LOG_HOST_MISBEHAVING, CLAP_LOG_INFO,
    CLAP_LOG_PLUGIN_MISBEHAVING, CLAP_LOG_WARNING,
};
