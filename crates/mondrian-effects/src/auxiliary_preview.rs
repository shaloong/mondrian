//! Explicit Preview-only provenance for a Definition skipped for missing input.
//! Strict preparation never manufactures these records.

use mondrian_core::{EffectId, ParameterId, VisualSourceReference};

/// Why one complete Definition was replaced with identity in Preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuxiliaryPreviewFallbackCause {
    /// Its required typed source has no authored address.
    Unbound,
    /// The host proved this addressed source unavailable for this evaluation.
    Unavailable,
}

/// A visible host advisory, separate from processor runtime feedback.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuxiliaryPreviewFallback {
    effect_id: EffectId,
    parameter_id: ParameterId,
    reference: VisualSourceReference,
    cause: AuxiliaryPreviewFallbackCause,
}

impl AuxiliaryPreviewFallback {
    pub(crate) fn new(
        effect_id: EffectId,
        parameter_id: ParameterId,
        reference: VisualSourceReference,
        cause: AuxiliaryPreviewFallbackCause,
    ) -> Self {
        Self { effect_id, parameter_id, reference, cause }
    }
    /// Frozen effect instance owning this consumer.
    pub const fn effect_id(&self) -> EffectId {
        self.effect_id
    }
    /// Exact source parameter, not a guessed slot or stack index.
    pub fn parameter_id(&self) -> &ParameterId {
        &self.parameter_id
    }
    /// Effective source request retained without changing the author state.
    pub const fn reference(&self) -> VisualSourceReference {
        self.reference
    }
    /// Typed source-admission reason.
    pub const fn cause(&self) -> AuxiliaryPreviewFallbackCause {
        self.cause
    }
    pub(crate) fn heap_bytes_estimate(&self) -> usize {
        self.parameter_id.storage_owner().capacity()
    }
}

/// Witness emitted only by the real typed-input builder, never by an error string.
#[derive(Debug, Clone)]
pub(crate) struct MissingAuxiliaryPreviewRequest {
    pub(crate) effect_id: EffectId,
    pub(crate) parameter_id: ParameterId,
    pub(crate) authored_reference: VisualSourceReference,
    pub(crate) reference: VisualSourceReference,
    pub(crate) slot: u32,
}
