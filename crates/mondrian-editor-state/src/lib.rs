//! UI-independent editor contracts and canonical project authoring state.
//!
//! `AuthoringSession` owns the sole mutable Project document, navigation,
//! project-wide Undo/Redo, and durable-generation relation. `Action` carries
//! semantic UI intent; UI crates build typed view models from read-only state.

pub mod action;
pub mod animation_groups;
pub mod author_execution_snapshot;
pub mod history;
pub mod native_audio_authoring;
mod native_visual_view;
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
pub use native_visual_view::NativeVisualAuthorView;
pub use session::{
    AuthorGeneration, AuthoringCommit, AuthoringSession, AuthoringSessionId, AuthoringSnapshot,
    SequenceNavigationIntent,
};

/// Canonical native author gestures; execution/window origin remains a separate proof.
pub use native_audio_authoring::{NativeAudioAuthorGesture, NativeAudioAuthorOwner};

/// Canonical author-generation commit fence; native source closure is separate.
pub use author_execution_snapshot::{
    AuthorExecutionAdmissionError, AuthorExecutionCommit, AuthorExecutionSnapshot,
};
