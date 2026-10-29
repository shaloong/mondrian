//! Reexports of declarations from official `clap/version.h`.
pub use crate::raw::{
    clap_version, clap_version_t, CLAP_VERSION_MAJOR, CLAP_VERSION_MINOR, CLAP_VERSION_REVISION,
};
pub use crate::support::{clap_version_is_compatible, CLAP_VERSION};
