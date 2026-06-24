#![allow(dead_code)]
//! Legacy egui UI reference modules.
//!
//! This module is crate-private during self-hosted UI retirement. Unused egui
//! helpers are expected until P5-CODE deletes or extracts the remaining shared
//! reference code.

pub mod animation_groups;
pub mod color_picker;
pub mod effect_controls;
pub use effect_controls as effect_controls_panel;
pub mod effect_library_panel;
pub mod export_panel;
pub mod fonts;
pub mod library_panel;
pub mod node_graph_panel;
pub mod startup;
pub mod theme;
pub mod timeline;
pub use timeline as timeline_panel;
pub mod viewer;
pub mod viewer_panel;
pub mod widgets;
