//! Self-hosted UI application shell and panel adapters.
//!
//! This module is the bridge between the reusable `mondrian-ui-*` crates and
//! the Mondrian application layer. Binaries should stay thin and call into this
//! module instead of accumulating panel or runtime wiring.

pub mod about_dialog;
pub mod action_queue;
pub mod host;
pub mod icons;
pub mod menu_bar;
pub mod modal;
pub mod new_project_dialog;
pub mod panels;
pub mod preferences_dialog;
pub mod rendering;
pub mod runtime;
pub mod shell;
pub mod shortcuts;
#[cfg(test)]
pub(crate) mod test_utils;
pub mod window;
