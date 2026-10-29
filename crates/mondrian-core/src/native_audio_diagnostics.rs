//! Bounded diagnostic observations; never native execution or author permission.

use crate::{
    AudioComponentEditId, AudioPreparedPlanId, AudioProcessingScopeId, AudioProcessorInstanceId,
    AudioRenderSessionId, MixBusId, ProgramOutputId, TrackId,
};
use std::num::NonZeroU32;

/// Maximum retained native message prefix, excluding its terminator.
pub const NATIVE_AUDIO_DIAGNOSTIC_MESSAGE_BYTES: usize = 512;
/// Maximum observations delivered by one serial control completion.
pub const NATIVE_AUDIO_DIAGNOSTIC_RECORDS: usize = 32;

/// Product diagnostic urgency projected by the actual format adapter.
/// A fatal report is not a request to stop or kill a valid processor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioDiagnosticSeverity {
    /// Developer detail.
    Debug,
    /// Informational report.
    Info,
    /// Warning requiring attention.
    Warning,
    /// Error reported by the native producer.
    Error,
    /// Fatal diagnostic, independently of the host's execution state.
    Fatal,
    /// Producer reports that the host misbehaved.
    HostMisbehaving,
    /// Producer reports that the plugin misbehaved.
    PluginMisbehaving,
    /// The source format adapter did not recognize its native signed value.
    Unknown,
}

/// Exact generated location reported by the audio execution adapter.
/// These IDs are provenance and cannot select or mutate a live native owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioDiagnosticLocation {
    /// One materialization of a shared processing scope.
    Contribution {
        /// Placement-local edit.
        edit_id: AudioComponentEditId,
        /// Shared authored scope.
        scope_id: AudioProcessingScopeId,
    },
    /// Track channel strip.
    Track(TrackId),
    /// Mix bus channel strip.
    Bus(MixBusId),
    /// Public output channel strip.
    Output(ProgramOutputId),
}

/// Read-only rack placement, with no alternate rack interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioDiagnosticInsertion {
    /// Scope processor rack.
    Scope,
    /// Before the channel fader.
    PreFader,
    /// After the fader and before mute.
    PostFader,
}

/// Actual root/local execution identities attached before diagnostic publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAudioDiagnosticSource {
    /// Sole root's immutable prepared plan.
    pub root_plan: AudioPreparedPlanId,
    /// Sole root's exclusive mutable session.
    pub root_session: AudioRenderSessionId,
    /// Exact local immutable plan, including nested occurrences.
    pub local_plan: AudioPreparedPlanId,
    /// Exact local exclusive session; immutable aliases do not share it.
    pub local_session: AudioRenderSessionId,
    /// Authored processor insertion identity.
    pub instance: AudioProcessorInstanceId,
    /// Exact generated owner.
    pub location: NativeAudioDiagnosticLocation,
    /// Exact insertion point within that owner.
    pub insertion: NativeAudioDiagnosticInsertion,
    /// Actual reply PID authenticated against the retained child handle.
    pub process_id: NonZeroU32,
}

/// Explicit retained-prefix and producer-loss facts; no fabricated message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NativeAudioDiagnosticMessageFacts {
    /// The original native message exceeded the retained prefix.
    pub truncated: bool,
    /// Native callback supplied null instead of its required string.
    pub null_message: bool,
    /// A malformed UTF-8 sequence occurs in the retained bytes.
    pub invalid_utf8: bool,
    /// Truncation retained an incomplete UTF-8 suffix.
    pub incomplete_utf8_suffix: bool,
}

/// An immutable projection of one native record into the foundation interface.
/// Native ABI/severity/UTF-8 interpretation remains in the format adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAudioDiagnosticRecord {
    sequence: u64,
    raw_severity: i32,
    severity: NativeAudioDiagnosticSeverity,
    facts: NativeAudioDiagnosticMessageFacts,
    bytes: [u8; NATIVE_AUDIO_DIAGNOSTIC_MESSAGE_BYTES],
    length: usize,
}

impl NativeAudioDiagnosticRecord {
    const EMPTY: Self = Self {
        sequence: 0,
        raw_severity: 0,
        severity: NativeAudioDiagnosticSeverity::Debug,
        facts: NativeAudioDiagnosticMessageFacts {
            truncated: false,
            null_message: false,
            invalid_utf8: false,
            incomplete_utf8_suffix: false,
        },
        bytes: [0; NATIVE_AUDIO_DIAGNOSTIC_MESSAGE_BYTES],
        length: 0,
    };
    /// Copy an already-interpreted source prefix into bounded owned storage.
    /// This does not admit a native execution owner or author transaction.
    pub fn project(
        sequence: u64,
        raw_severity: i32,
        severity: NativeAudioDiagnosticSeverity,
        facts: NativeAudioDiagnosticMessageFacts,
        prefix: &[u8],
    ) -> Result<Self, NativeAudioDiagnosticExtentError> {
        if prefix.len() > NATIVE_AUDIO_DIAGNOSTIC_MESSAGE_BYTES {
            return Err(NativeAudioDiagnosticExtentError::Message);
        }
        let mut record = Self {
            sequence,
            raw_severity,
            severity,
            facts,
            length: prefix.len(),
            ..Self::EMPTY
        };
        record.bytes[..prefix.len()].copy_from_slice(prefix);
        Ok(record)
    }
    /// Source queue reservation order; not wall-clock time.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
    /// Exact signed native severity value.
    pub const fn raw_severity(&self) -> i32 {
        self.raw_severity
    }
    /// Format adapter's explicit diagnostic classification.
    pub const fn severity(&self) -> NativeAudioDiagnosticSeverity {
        self.severity
    }
    /// Exact retained bytes; invalid UTF-8 is never replaced silently.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
    /// Retention and defensive callback facts supplied by the format adapter.
    pub const fn facts(&self) -> NativeAudioDiagnosticMessageFacts {
        self.facts
    }
}

/// Explicit lifetime-sticky native loss, without an invented dropped count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NativeAudioDiagnosticLoss {
    /// At least one producer found its fixed queue full.
    pub capacity: bool,
    /// At least one producer exhausted its bounded contention attempts.
    pub contention: bool,
    /// The non-wrapping owner sequence was exhausted.
    pub sequence_exhausted: bool,
}

impl NativeAudioDiagnosticLoss {
    /// Whether the native diagnostic prefix is explicitly incomplete.
    pub const fn incomplete(self) -> bool {
        self.capacity || self.contention || self.sequence_exhausted
    }
}

/// One fixed observation from one actual source, moved through the serial worker.
/// This is a completion DTO, not a second message queue or event-bus authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAudioDiagnosticBatch {
    source: NativeAudioDiagnosticSource,
    loss: NativeAudioDiagnosticLoss,
    records: [NativeAudioDiagnosticRecord; NATIVE_AUDIO_DIAGNOSTIC_RECORDS],
    length: usize,
}

impl NativeAudioDiagnosticBatch {
    /// Start one source observation with its exact retained loss facts.
    pub const fn new(source: NativeAudioDiagnosticSource, loss: NativeAudioDiagnosticLoss) -> Self {
        Self {
            source,
            loss,
            records: [NativeAudioDiagnosticRecord::EMPTY; NATIVE_AUDIO_DIAGNOSTIC_RECORDS],
            length: 0,
        }
    }
    /// Append an interpreted record, refusing excess rather than dropping it.
    pub fn push(
        &mut self,
        record: NativeAudioDiagnosticRecord,
    ) -> Result<(), NativeAudioDiagnosticExtentError> {
        if self.length == NATIVE_AUDIO_DIAGNOSTIC_RECORDS {
            return Err(NativeAudioDiagnosticExtentError::Batch);
        }
        self.records[self.length] = record;
        self.length += 1;
        Ok(())
    }
    /// Exact execution and actual child provenance, never author permission.
    pub const fn source(&self) -> NativeAudioDiagnosticSource {
        self.source
    }
    /// Retained records in their source queue order.
    pub fn records(&self) -> &[NativeAudioDiagnosticRecord] {
        &self.records[..self.length]
    }
    /// Native lifetime-sticky producer loss facts.
    pub const fn loss(&self) -> NativeAudioDiagnosticLoss {
        self.loss
    }
    /// An empty complete observation need not be published to product UI.
    pub const fn is_empty_complete(&self) -> bool {
        self.length == 0 && !self.loss.incomplete()
    }
}

/// Refusal of a bounded diagnostic projection, independently of PCM validity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NativeAudioDiagnosticExtentError {
    /// One message exceeded the admitted retained-prefix size.
    #[error("native audio diagnostic message exceeds its bound")]
    Message,
    /// One completion exceeded its admitted number of records.
    #[error("native audio diagnostic batch exceeds its bound")]
    Batch,
}
