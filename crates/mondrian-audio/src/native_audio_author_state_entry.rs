//! Same-child native state preparation for an explicit fresh continuity.
//!
//! A prepared object has consumed no PCM. These receipts identify actual
//! previous/fresh incarnations and configuration bytes; they are not editor,
//! author, installation or DSP-history authority.

use crate::{
    AudioProcessorExecutionContract, NativeAudioObjectBindRequest, NativeAudioObjectObservation,
    NativeConfigurationFormat,
};
use std::num::NonZeroU64;

/// Actual source condition required before destructive native preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioAuthorStateEntrySource {
    /// The exact already-delivered closed terminal prefix and coherent bytes.
    SelectedTerminal { revision: NonZeroU64 },
    /// An actual peer whose source was observed closed at this exact revision.
    CanonicalPeer { revision: u64 },
}

/// Bounded borrowed request on the existing authenticated native stream.
///
/// The caller must be the sole installed Runtime, prove the complete canonical
/// accepted tree and frozen overlap grant, and settle physical output first.
/// Public fields alone do not grant that permission. The child independently
/// compares the exact old binding/object and actual source before native work.
pub struct NativeAudioAuthorStateEntryRequest<'a> {
    /// Exact previously installed actual object, before its native retirement.
    pub binding: NativeAudioObjectBindRequest,
    /// Actual source condition; no generation is inferred from the author UI.
    pub source: NativeAudioAuthorStateEntrySource,
    /// Existing canonical native state envelope.
    pub format: NativeConfigurationFormat,
    /// Accepted component/controller bytes; never a private DSP checkpoint.
    pub configuration: &'a [u8],
}

/// A refusal leaves the whole root control-only after any peer was retired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioAuthorStateEntryRefusal {
    /// This adapter has no actual same-child fresh-state preparation.
    UnsupportedAdapter,
    /// The actual previously installed binding/object does not match.
    OwnerChanged,
    /// Source suffix, gesture, native feedback or restart remains unaccepted.
    SourcePending,
    /// Expected retained terminal prefix or coherent bytes changed.
    DeliveryChanged,
    /// Real frame cleanup is still pending; retry that same removed view.
    EditorCleanupPending,
    /// The provided format/bytes do not fit the real native codec/grant.
    ConfigurationRejected,
    /// The restored real object changed its prepared contract or metadata.
    ContractChanged,
}

/// Actual old-to-fresh native preparation facts, not an installed lease.
///
/// Construction is crate-private and used only by the native adapter/decoder.
/// A successful receipt must be checked against that adapter's private retained
/// preparation before Bind/install/acknowledgement. Equal PIDs are insufficient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAudioAuthorStateEntryReceipt {
    pub(crate) source_binding: NativeAudioObjectBindRequest,
    pub(crate) source: NativeAudioAuthorStateEntrySource,
    pub(crate) object: NativeAudioObjectObservation,
    pub(crate) configuration_sha256: [u8; 32],
    pub(crate) configuration_bytes: usize,
    pub(crate) contract: AudioProcessorExecutionContract,
}

impl NativeAudioAuthorStateEntryReceipt {
    /// Previously installed source object that was actually retired.
    pub const fn source_binding(self) -> NativeAudioObjectBindRequest {
        self.source_binding
    }
    /// Exact prior source revision/boundary independently checked by the child.
    pub const fn source(self) -> NativeAudioAuthorStateEntrySource {
        self.source
    }
    /// Fresh actual incarnation on the same native child, before any PCM.
    pub const fn object(self) -> NativeAudioObjectObservation {
        self.object
    }
    /// Exact accepted configuration identity.
    pub const fn configuration_sha256(self) -> [u8; 32] {
        self.configuration_sha256
    }
    /// Complete accepted configuration length, not its owned capacity charge.
    pub const fn configuration_bytes(self) -> usize {
        self.configuration_bytes
    }
    /// Actual restored object contract, independently compared with the plan.
    pub const fn contract(self) -> AudioProcessorExecutionContract {
        self.contract
    }
}

/// Result of real same-child preparation, distinct from a root publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioAuthorStateEntryOutcome {
    /// Every actual restore/contract check finished and no PCM was processed.
    Prepared(NativeAudioAuthorStateEntryReceipt),
    /// No complete preparation/installation is asserted.
    Refused(NativeAudioAuthorStateEntryRefusal),
}

/// Exact old-prefix acknowledgement after fresh-object root installation.
///
/// The child must match the entire receipt with its own prepared-entry record.
/// This is deliberately distinct from same-object acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAudioReenteredAuthorAcknowledgement {
    /// Original exact native prefix revision and actual old/fresh incarnation.
    pub preparation: NativeAudioAuthorStateEntryReceipt,
    /// New binding already acknowledged by the same actual fresh object.
    pub installed_binding: NativeAudioObjectBindRequest,
}
