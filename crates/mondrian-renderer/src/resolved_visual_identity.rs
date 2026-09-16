//! Canonical identity for one fully resolved visual materialization.
//!
//! The Prepared Visual Module owns the semantic envelope while product
//! Adapters own the exact materialized leaves. This typed Interface prevents
//! persistent caches from accepting an arbitrary collection of optional
//! fingerprints.

use crate::PreparedVisualFrameNode;
use mondrian_core::{ResolvedVisualFrameIdentity, ResolvedVisualNodeMaterializationIdentity};

/// Failure while encoding Renderer-owned resolved semantics.
pub type ResolvedVisualIdentityError = mondrian_core::ResolvedVisualIdentityError;

/// Bind an exhaustive Adapter materialization to its Prepared Visual node.
///
/// Program Output and monitoring transforms are intentionally absent: the
/// result identifies post-composite working-linear pixels. Scheduling,
/// revisions, provider/backend selection, and process-local generations are
/// likewise not pixel semantics.
pub fn resolved_visual_frame_identity<T>(
    node: &PreparedVisualFrameNode<T>,
    materialization: ResolvedVisualNodeMaterializationIdentity,
) -> Result<ResolvedVisualFrameIdentity, ResolvedVisualIdentityError> {
    ResolvedVisualFrameIdentity::from_prepared_visual_semantics(
        materialization,
        node.time(),
        node.author_resolution(),
        node.execution_resolution(),
        node.color_context().working_color_space(),
        node.color_context().engine(),
    )
}
