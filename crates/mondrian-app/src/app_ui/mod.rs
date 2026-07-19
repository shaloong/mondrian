//! App UI application shell and panel adapters.
//!
//! This module is the bridge between the reusable `mondrian-ui-*` crates and
//! the Mondrian application layer. Binaries should stay thin and call into this
//! module instead of accumulating panel or runtime wiring.

pub mod about_dialog;
pub mod action_availability;
pub mod action_queue;
pub mod asset_thumbnails;
mod color_management_controls;
pub mod commands;
pub(crate) mod display_probe_impl;
pub mod host;
pub mod icons;
pub mod interpret_asset_dialog;
pub mod menu_bar;
pub mod modal;
pub mod new_project_dialog;
pub mod panels;
pub mod pending_close_dialog;
pub mod playback_feedback;
pub mod preferences_dialog;
pub mod preferences_store;
pub mod preview;
pub(crate) mod preview_frame_store;
pub(crate) mod preview_scale;
pub mod project_settings_dialog;
pub mod rendering;
pub mod runtime;
pub(crate) mod scopes;
pub mod sequence_settings_dialog;
pub mod shell;
pub mod shortcuts;
pub mod startup;
#[cfg(test)]
pub(crate) mod test_utils;
pub mod title_bar;
pub mod viewer_gpu_output_budget;
#[cfg(test)]
pub(crate) mod viewer_gpu_preview_headless;
pub mod waveform_cache;
pub mod window;
pub mod window_controls;
pub mod workspace_layout;
