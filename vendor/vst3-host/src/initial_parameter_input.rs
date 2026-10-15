//! Initial-only handoff into the existing processor input queue.
//!
//! These local control-thread operations never call the plugin, set controller
//! values or run DSP. The caller must hold the actual initial lifecycle right.

/// Maximum distinct parameters admitted by this initial-only operation.
pub const MAX_INITIAL_PARAMETERS: usize = 1024;

/// One current controller value proved against the admitted parameter metadata.
#[derive(Debug, Clone, Copy, Default)]
pub struct InitialParameterValue {
    /// Definition-local parameter identity.
    pub id: u32,
    /// Exact finite normalized controller value.
    pub normalized: f64,
}

/// Additional retained input storage request, with coverage kept explicit.
///
/// Vector bytes are complete allocator-requested element storage. COM bytes
/// cover the actual header plus Rust payload only; std Arc counters/padding
/// remain a separate qualification frontier. This is never a complete native
/// heap receipt, or permission to allocate before Session resource admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InitialParameterStorage {
    /// Complete pool and single-point Vec element-storage ceiling.
    pub vector_bytes: usize,
    /// COM queue payload ceiling, excluding the std Arc allocation prefix.
    pub com_payload_bytes: usize,
}
impl InitialParameterStorage {
    /// Derive a baseline-only request without constructing queues or plugins.
    pub fn for_parameter_count(count: usize) -> Result<Self, InitialParameterHandoffError> {
        crate::internal::com_implementations::initial_parameter_input::storage_request(count)
    }
    /// Checked additional known storage request, preserving coverage above.
    pub fn total_bytes(self) -> Option<usize> {
        self.vector_bytes.checked_add(self.com_payload_bytes)
    }
    /// Whether an actual census fits each independent prepared ceiling.
    pub fn admits(self, actual: Self) -> bool {
        actual.vector_bytes <= self.vector_bytes
            && actual.com_payload_bytes <= self.com_payload_bytes
    }
}

/// Refusal before any initial source/cohort/input publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InitialParameterHandoffError {
    /// This backend does not expose the local owning input queue.
    #[error("initial parameter handoff is unavailable on this backend")]
    Unsupported,
    /// Initial service must run on the component control thread.
    #[error("initial parameter handoff requires the native control thread")]
    WrongThread,
    /// IDs, values, count or arithmetic cannot form the bounded request.
    #[error("initial parameter identity, value or extent is invalid")]
    InvalidParameters,
    /// Host storage allocation failed before any source consumption.
    #[error("initial parameter storage allocation failed")]
    AllocationFailed,
    /// An owning mutex cannot supply trustworthy complete state.
    #[error("initial parameter owning storage is poisoned")]
    PoisonedOwner,
    /// The actual new pool exceeds the prepared storage ceiling.
    #[error("initial parameter storage exceeds the admitted request")]
    StorageCapacity,
    /// Existing real input obligations cannot be replaced by initialization.
    #[error("processor input already has retained parameter storage or values")]
    InputPending,
    /// The complete source is not a stable matching Value-only initial prefix.
    #[error("initial parameter source is incomplete or changed")]
    SourceChanged,
    /// The exact restart cohort changed or is not an admitted initial request.
    #[error("initial parameter restart cohort is unsupported or changed")]
    CohortChanged,
}
