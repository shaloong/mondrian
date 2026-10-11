//! Owned native callback result oracle; see the adjacent README.
mod plugin;
use clack_plugin::prelude::*;
clack_export_entry!(SinglePluginEntry<plugin::OutputProbePlugin>);
