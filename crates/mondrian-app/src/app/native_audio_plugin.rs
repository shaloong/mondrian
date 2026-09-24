//! Machine-local selection of installed audio plugin files and bundles.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Native audio plugin ABI chosen for one installed file or bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAudioPluginFormat {
    /// CLAP C ABI.
    Clap,
    /// VST3 component/controller ABI.
    Vst3,
}

/// One explicit machine-local native plugin selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAudioPluginSelection {
    /// ABI used to discover and host the binary.
    pub format: NativeAudioPluginFormat,
    /// Absolute path to the selected native file or directory bundle.
    pub path: PathBuf,
}
