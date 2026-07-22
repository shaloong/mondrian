//! UI-independent editor contracts and canonical project authoring state.
//!
//! `AuthoringSession` owns the sole mutable Project document, navigation,
//! project-wide Undo/Redo, and durable-generation relation. `Action` carries
//! semantic UI intent; UI crates build typed view models from read-only state.

pub mod action;
pub mod animation_groups;
pub mod history;
pub mod session;
pub mod state;

pub use action::Action;
pub use animation_groups::{
    property_display_name, property_group_meta, property_order, qualified_property_display_name,
    AnimationGroupKind, AnimationGroupMeta,
};
pub use history::{
    AuthoringHistory, AuthoringHistoryBudget, AuthoringHistoryDiagnostics,
    AuthoringHistoryRecordOutcome,
};
pub use session::{
    AuthorGeneration, AuthoringCommit, AuthoringSession, AuthoringSessionId, AuthoringSnapshot,
};
