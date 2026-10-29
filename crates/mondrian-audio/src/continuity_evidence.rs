//! Read-only generated-cursor facts, never a DSP checkpoint or resume capability.

use crate::{AudioContinuityEpoch, AudioExecutionError, AudioProcessorOccurrence};
use mondrian_core::{
    AudioPreparedPlanId, AudioRenderSessionId, AudioSamplePosition, AudioSampleRate,
};

/// Exact immutable preparation and exclusive mutable Session owning cursor facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioSessionCursorIdentity {
    /// Immutable prepared graph/PDC identity.
    pub prepared_plan_id: AudioPreparedPlanId,
    /// Unique live Session; Plan aliases do not share this identity.
    pub session_id: AudioRenderSessionId,
}

/// Canonical Session continuity state at one exclusive read boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSessionCursorState {
    /// The prepared Session requires no persistent history and stores no contiguous generated head.
    Stateless,
    /// No state entry has been admitted; no generated head is asserted.
    Unentered,
    /// Every previously successful window has completed under this exact epoch.
    Active {
        /// Consumer-owned continuity epoch.
        epoch: AudioContinuityEpoch,
        /// First public Timeline-aligned sample not yet returned by this Session.
        next_public_sample: AudioSamplePosition,
        /// First internal execution-grid sample not yet evaluated by this Session.
        next_execution_sample: AudioSamplePosition,
        /// Whether initial public-output lookahead has been evaluated.
        alignment_primed: bool,
    },
    /// A window/reset may have advanced a prefix; no exact restore point exists.
    Poisoned {
        /// Epoch whose incomplete operation invalidated exact generated heads.
        epoch: AudioContinuityEpoch,
    },
}

/// Read-only Session facts projected from its sole execution authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioSessionCursorEvidence {
    /// Exact Plan/Session materialization.
    pub identity: AudioSessionCursorIdentity,
    /// Explicit grid for all sample coordinates belonging to this Session.
    pub sample_rate: AudioSampleRate,
    /// Immutable public output lookahead in this prepared contract.
    pub public_output_lookahead_frames: usize,
    /// Existing continuity state, with no invented cursor for failed/unentered work.
    pub state: AudioSessionCursorState,
}

/// Exact processor input-signal continuity, separately from public output head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioProcessorCursorState {
    /// The admitted factory declares no persistent history; no private head is asserted.
    Stateless,
    /// Stateful instance has not entered a continuity epoch.
    Unentered,
    /// Lazy entry is admitted but this occurrence has not yet received its first signal.
    WaitingForFirstSignal {
        /// Epoch reserved for its first actual input-signal sample.
        epoch: AudioContinuityEpoch,
    },
    /// Every successful callback has consumed exactly the preceding signal prefix.
    Active {
        /// Instance continuity epoch; not a project author generation.
        epoch: AudioContinuityEpoch,
        /// Next latency-adjusted input-signal sample, including legal negative PDC coordinates.
        next_input_signal_sample: AudioSamplePosition,
    },
    /// Native or host work may have partially advanced; no resumable head is asserted.
    Failed,
}

/// One exact local occurrence's generated input-signal cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioProcessorCursorEvidence {
    /// Exclusive materialization that owns this processor.
    pub identity: AudioSessionCursorIdentity,
    /// Full local owner/rack/author address; author instance ID alone is insufficient.
    pub occurrence: AudioProcessorOccurrence,
    /// Canonical host state following only actual successful process callbacks.
    pub state: AudioProcessorCursorState,
}

/// Bounded caller-owned visitor for canonical generated cursor observations.
///
/// Audio execution allocates no collection, calls no processor, resets no history,
/// and changes no Plan/Session state while visiting. The consumer must separately
/// admit its storage and match its renderer/generation/cohort authority. Evidence
/// is not a checkpoint, PDC migration proof, private-state guarantee or permission
/// to reset/replay/resume. A rejected callback stops traversal without mutation.
pub trait AudioContinuityEvidenceVisitor {
    /// Visit one actual root/nested Session before its local processor occurrences.
    fn session(&mut self, evidence: AudioSessionCursorEvidence) -> Result<(), AudioExecutionError>;
    /// Visit one actual local occurrence in its prepared canonical order.
    fn processor(
        &mut self,
        evidence: AudioProcessorCursorEvidence,
    ) -> Result<(), AudioExecutionError>;
}
