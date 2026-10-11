//! Exact native editor/runtime owner contracts for the sole installed audio root.
//!
//! Current renderer claims and same-object native acknowledgements are distinct.
//! Only the installed root Runtime publishes a lease in its actual Hosted record.

use crate::{AudioContinuityEpoch, AudioProcessorOccurrence, AudioSessionCursorIdentity};
use mondrian_core::AudioProcessorBindingId;
use std::num::NonZeroU64;

/// Read-only exact address of a native occurrence in one installed audio root.
///
/// Shared authored processor IDs or immutable Plan aliases do not collapse
/// nested mutable Session occurrences. This address is metadata, not authority
/// to execute, edit, reopen, restore state or configure a native object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioNativeProcessorAddress {
    /// Current sole installed root Plan and exclusive Session.
    pub root: AudioSessionCursorIdentity,
    /// Exact root or nested local Plan and exclusive Session.
    pub local: AudioSessionCursorIdentity,
    /// Local owner/rack/author processor insertion address.
    pub occurrence: AudioProcessorOccurrence,
    /// Admitted Session continuity, or none before initial state entry.
    /// This is not a promise that the vendor keeps private DSP across lifecycle.
    pub epoch: Option<AudioContinuityEpoch>,
}

/// Opaque runtime-issued ownership ticket for the current native binding.
///
/// This ticket has no public constructor or Deserialize. Issuance belongs
/// exclusively to the actual installed AudioProgramRuntime, after every exact
/// root/nested Hosted binding acknowledgement and current slot claim succeeds.
/// It must hold and compare its actual private installed-owner record on every
/// editor/control admission; copying this ticket never makes it permanently
/// current. Enter/owner replacement, cancellation, seek, root installation,
/// retirement or superseding author generation revokes the old record.
///
/// A same-child StageSeed/CommitSeed ticket, cursor evidence or author instance
/// cannot manufacture this ticket. Native wire DTOs are checked against the
/// same parent-issued binding in the authenticated child command stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioNativeProcessorOwnerLease {
    address: AudioNativeProcessorAddress,
    binding: AudioProcessorBindingId,
    installed_root_serial: NonZeroU64,
}

impl AudioNativeProcessorOwnerLease {
    pub(crate) const fn issue(
        address: AudioNativeProcessorAddress,
        binding: AudioProcessorBindingId,
        installed_root_serial: NonZeroU64,
    ) -> Self {
        Self { address, binding, installed_root_serial }
    }
    /// Read-only format-neutral address facts for existing Media/UI transport.
    /// This projection grants no editor or native callback permission.
    pub fn editor_target(self) -> mondrian_core::NativeAudioEditorTarget {
        use mondrian_core::{
            NativeAudioEditorInsertion as Insertion, NativeAudioEditorOccurrenceOwner as Owner,
        };
        mondrian_core::NativeAudioEditorTarget {
            root_plan: self.address.root.prepared_plan_id,
            root_session: self.address.root.session_id,
            local_plan: self.address.local.prepared_plan_id,
            local_session: self.address.local.session_id,
            instance: self.address.occurrence.instance_id,
            owner: match self.address.occurrence.owner {
                crate::AudioProcessorOccurrenceOwner::Contribution { edit_id, scope_id } => {
                    Owner::Contribution { edit_id, scope_id }
                }
                crate::AudioProcessorOccurrenceOwner::Track(id) => Owner::Track(id),
                crate::AudioProcessorOccurrenceOwner::Bus(id) => Owner::Bus(id),
                crate::AudioProcessorOccurrenceOwner::Output(id) => Owner::Output(id),
            },
            insertion: match self.address.occurrence.insertion {
                crate::AudioProcessorInsertionPoint::Scope => Insertion::Scope,
                crate::AudioProcessorInsertionPoint::PreFader => Insertion::PreFader,
                crate::AudioProcessorInsertionPoint::PostFader => Insertion::PostFader,
            },
            continuity_epoch: self.address.epoch.map(crate::AudioContinuityEpoch::get),
            binding: self.binding,
            installed_generation: self.installed_root_serial,
        }
    }

    /// Exact root/nested occurrence and current continuity metadata.
    pub const fn address(self) -> AudioNativeProcessorAddress {
        self.address
    }
    /// Parent-issued native binding incarnation, never PID/class/author ID.
    /// A retained native object may receive a new binding when state entry
    /// changes its authority; this ID does not assert object recreation.
    pub const fn binding(self) -> AudioProcessorBindingId {
        self.binding
    }
    /// Sole installed root incarnation that must still match at admission.
    pub const fn installed_root_serial(self) -> NonZeroU64 {
        self.installed_root_serial
    }
}

/// Failed current-root native ownership admission, independently of valid DSP.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioNativeOwnerError {
    /// This actual Runtime has not been registered by its Media slot owner.
    #[error("audio Runtime has no renderer installation registration")]
    UnregisteredRuntime,
    /// The renderer slot claim is absent, stale, foreign or poisoned.
    #[error(transparent)]
    Installation(#[from] mondrian_core::AudioOwnerInstallationError),
    /// Exact same-object native acknowledgement was refused.
    #[error("native audio binding refused: {0:?}")]
    BindingRefused(crate::NativeAudioObjectBindRefusal),
    /// Actual native transport or object observation failed.
    #[error(transparent)]
    Processor(#[from] crate::AudioProcessorHostError),
    /// No current actual record matches this full root/local/occurrence/epoch.
    #[error("native audio owner is unavailable for the exact occurrence")]
    OwnerUnavailable,
    /// A deferred/failed continuity owner cannot grant editor permission.
    #[error("native audio owner continuity is unavailable")]
    ContinuityUnavailable,
    /// Superseding work cancelled this non-RT installation before publication.
    #[error("native audio binding installation was cancelled")]
    Cancelled,
    /// A bounded existing-record traversal could not represent its count.
    #[error("native audio binding record extent overflow")]
    RecordExtentOverflow,
    /// A consumed preparation serial cannot wrap and match an old ticket.
    #[error("native audio binding preparation serial space is exhausted")]
    PreparationSerialExhausted,
}

/// Read-only authority borrowed from the actual registered renderer slot.
/// Only cold entry and publication move this context; PCM uses no rebind loop.
#[derive(Clone)]
pub(crate) struct NativeStateEntryBindingContext {
    pub(crate) root: AudioSessionCursorIdentity,
    pub(crate) verifier: mondrian_core::AudioOwnerInstallationVerifier,
    pub(crate) claim: mondrian_core::AudioOwnerInstallationClaim,
}

impl NativeStateEntryBindingContext {
    pub(crate) fn validate(
        &self,
        cancellation: Option<&mondrian_core::ExecutionCancellationToken>,
    ) -> Result<(), AudioNativeOwnerError> {
        if cancellation.is_some_and(mondrian_core::ExecutionCancellationToken::is_canceled) {
            return Err(AudioNativeOwnerError::Cancelled);
        }
        self.verifier.validate(&self.claim)?;
        Ok(())
    }
}

pub(crate) struct NativeRuntimeInstallation {
    pub(crate) verifier: Option<mondrian_core::AudioOwnerInstallationVerifier>,
    pub(crate) claim: Option<mondrian_core::AudioOwnerInstallationClaim>,
    pub(crate) entry_context: Option<NativeStateEntryBindingContext>,
    pub(crate) next_preparation_serial: u64,
    pub(crate) prepared: Option<(NonZeroU64, AudioSessionCursorIdentity, usize)>,
}

impl Default for NativeRuntimeInstallation {
    fn default() -> Self {
        Self {
            verifier: None,
            claim: None,
            entry_context: None,
            next_preparation_serial: 1,
            prepared: None,
        }
    }
}

/// Non-Clone preparation from the one actual Runtime's Hosted native records.
/// No public constructor/Deserialize exists; this ticket alone is not installed
/// permission. Every same-object acknowledgement has already completed.
#[derive(Debug)]
pub struct NativeAudioBindingPreparation {
    pub(crate) root: AudioSessionCursorIdentity,
    pub(crate) serial: NonZeroU64,
    pub(crate) count: usize,
}

impl NativeAudioBindingPreparation {
    /// Root whose existing records were prepared; metadata, not installation.
    pub const fn root(&self) -> AudioSessionCursorIdentity {
        self.root
    }
    /// Number of actual native records acknowledged on this root closure.
    pub const fn binding_count(&self) -> usize {
        self.count
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) enum NativeHostedBinding {
    #[default]
    Unbound,
    Prepared {
        address: AudioNativeProcessorAddress,
        receipt: crate::NativeAudioObjectBindReceipt,
    },
    Installed {
        lease: AudioNativeProcessorOwnerLease,
        object: crate::NativeAudioObjectObservation,
    },
}

pub(crate) fn editor_target_address(
    target: mondrian_core::NativeAudioEditorTarget,
) -> AudioNativeProcessorAddress {
    use mondrian_core::{
        NativeAudioEditorInsertion as Insertion, NativeAudioEditorOccurrenceOwner as Owner,
    };
    AudioNativeProcessorAddress {
        root: AudioSessionCursorIdentity {
            prepared_plan_id: target.root_plan,
            session_id: target.root_session,
        },
        local: AudioSessionCursorIdentity {
            prepared_plan_id: target.local_plan,
            session_id: target.local_session,
        },
        occurrence: AudioProcessorOccurrence {
            instance_id: target.instance,
            owner: match target.owner {
                Owner::Contribution { edit_id, scope_id } => {
                    crate::AudioProcessorOccurrenceOwner::Contribution { edit_id, scope_id }
                }
                Owner::Track(id) => crate::AudioProcessorOccurrenceOwner::Track(id),
                Owner::Bus(id) => crate::AudioProcessorOccurrenceOwner::Bus(id),
                Owner::Output(id) => crate::AudioProcessorOccurrenceOwner::Output(id),
            },
            insertion: match target.insertion {
                Insertion::Scope => crate::AudioProcessorInsertionPoint::Scope,
                Insertion::PreFader => crate::AudioProcessorInsertionPoint::PreFader,
                Insertion::PostFader => crate::AudioProcessorInsertionPoint::PostFader,
            },
        },
        epoch: target.continuity_epoch.map(crate::AudioContinuityEpoch::new),
    }
}

pub(crate) fn project_editor_outcome(
    target: mondrian_core::NativeAudioEditorTarget,
    outcome: crate::NativeAudioEditorOutcome,
) -> mondrian_core::NativeAudioEditorControlOutcome {
    use mondrian_core::{
        NativeAudioEditorControlOutcome as Out, NativeAudioEditorControlRefusal as Refusal,
    };
    match outcome {
        crate::NativeAudioEditorOutcome::Opened { width, height, .. } => {
            Out::Opened { target, width, height }
        }
        crate::NativeAudioEditorOutcome::Closed { .. } => Out::Closed { target },
        crate::NativeAudioEditorOutcome::Refused(refusal) => Out::Refused(match refusal {
            crate::NativeAudioEditorRefusal::UnsupportedAdapter => Refusal::UnsupportedAdapter,
            crate::NativeAudioEditorRefusal::NotInstalled => Refusal::NotInstalled,
            crate::NativeAudioEditorRefusal::OwnerChanged => Refusal::OwnerChanged,
            crate::NativeAudioEditorRefusal::NoEditor => Refusal::NoEditor,
            crate::NativeAudioEditorRefusal::ConfigurationPending => Refusal::ConfigurationPending,
            crate::NativeAudioEditorRefusal::InvalidTitle => Refusal::InvalidTitle,
        }),
        crate::NativeAudioEditorOutcome::ClosePending { stage, .. } => Out::ClosePending {
            target,
            stage: match stage {
                crate::NativeAudioEditorCloseStage::RemoveView => {
                    mondrian_core::NativeAudioEditorDetachStage::RemoveView
                }
                crate::NativeAudioEditorCloseStage::ClearHostFrame => {
                    mondrian_core::NativeAudioEditorDetachStage::ClearHostFrame
                }
            },
        },
    }
}
