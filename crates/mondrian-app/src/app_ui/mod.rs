//! App UI application shell and panel adapters.
//!
//! This module is the bridge between the reusable `mondrian-ui-*` crates and
//! the Mondrian application layer. Binaries should stay thin and call into this
//! module instead of accumulating panel or runtime wiring.

pub mod about_dialog;
#[cfg(target_os = "macos")]
pub mod accessibility;
pub mod action_availability;
pub mod action_queue;
pub mod asset_thumbnails;
mod audio_automation;
mod audio_component_mapping;
mod audio_device_catalog;
mod audio_mixer;
mod audio_processor_rack;
mod audio_sidechain;
mod background_runtime;
mod color_management_controls;
pub mod commands;
mod curve_tangents;
pub(crate) mod display_probe_impl;
#[cfg(feature = "validation")]
mod event_loop_owner;
pub mod host;
pub mod icons;
mod inspector_source_timing;
pub mod interpret_asset_dialog;
pub mod localization;
pub mod menu_bar;
pub mod modal;
pub(crate) mod musical_time_dialog;
#[cfg(feature = "validation")]
pub mod native_question_fixture;
#[cfg(feature = "validation")]
pub mod native_question_validation;
pub(crate) mod native_visual_gui_section;
pub(crate) mod native_visual_question_dialog;
pub mod new_project_dialog;
mod notifications;
pub mod panels;
pub mod pending_close_dialog;
pub mod playback_feedback;
pub mod preferences_dialog;
pub mod preferences_store;
pub mod preview;
pub(crate) mod preview_scale;
mod product_logging;
pub mod project_settings_dialog;
pub mod recovery_dialog;
pub mod rendering;
pub(crate) mod resource_dialog;
pub mod runtime;
pub(crate) mod scopes;
pub mod sequence_settings_dialog;
pub mod shell;
pub mod shortcuts;
pub mod startup;
pub(crate) mod surface_acquisition;
#[cfg(feature = "validation")]
pub mod surface_reopen_batch_receipt;
#[cfg(feature = "validation")]
pub mod surface_reopen_report;
#[cfg(test)]
pub(crate) mod test_utils;
pub mod title_bar;
pub mod window;
pub mod window_controls;
#[cfg(any(test, feature = "validation"))]
pub(crate) mod window_generation_history;
#[cfg(any(test, feature = "validation"))]
pub(crate) mod window_outer_receipt;
pub mod workspace_layout;
