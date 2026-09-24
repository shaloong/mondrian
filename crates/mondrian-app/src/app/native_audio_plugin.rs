//! Machine-local selection of installed audio plugin binaries.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Native audio plugin ABI chosen for one installed binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAudioPluginFormat {
    /// CLAP C ABI.
    Clap,
    /// VST3 component/controller ABI.
    Vst3,
}

/// One explicit machine-local native binary selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAudioPluginSelection {
    /// ABI used to discover and host the binary.
    pub format: NativeAudioPluginFormat,
    /// Absolute path to the selected native binary.
    pub path: PathBuf,
}
