//! Bounded immutable observations from one authenticated native audio owner.

use crate::NativeAudioObjectObservation;

/// Maximum retained bytes of one native diagnostic; the terminator is excluded.
pub const NATIVE_AUDIO_LOG_MESSAGE_BYTES: usize = 512;
/// Maximum diagnostic records in one observation response.
pub const NATIVE_AUDIO_LOG_RECORDS: usize = 32;

/// One of the seven official CLAP log severities.
///
/// A fatal log is diagnostic severity, not an instruction to kill a valid DSP
/// instance. Host/plugin misbehavior remains distinct from an ordinary error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioLogSeverity {
    /// Developer detail.
    Debug,
    /// Informational message.
    Info,
    /// Warning.
    Warning,
    /// Error.
    Error,
    /// Fatal diagnostic.
    Fatal,
    /// The producer reports host misbehavior.
    HostMisbehaving,
    /// The producer reports plugin misbehavior.
    PluginMisbehaving,
}

impl NativeAudioLogSeverity {
    /// Interpret only the official signed values; preserve an unknown value.
    pub const fn from_raw(value: i32) -> Option<Self> {
        Some(match value {
            0 => Self::Debug,
            1 => Self::Info,
            2 => Self::Warning,
            3 => Self::Error,
            4 => Self::Fatal,
            5 => Self::HostMisbehaving,
            6 => Self::PluginMisbehaving,
            _ => return None,
        })
    }
}

pub(crate) const LOG_NULL: u16 = 1;
pub(crate) const LOG_TRUNCATED: u16 = 2;
pub(crate) const LOG_INVALID_UTF8: u16 = 4;
pub(crate) const LOG_UNKNOWN_SEVERITY: u16 = 8;
pub(crate) const LOG_INCOMPLETE_UTF8_SUFFIX: u16 = 16;
pub(crate) const LOG_FLAGS: u16 = 31;
pub(crate) const LOSS_FULL: u32 = 1;
pub(crate) const LOSS_CONTENTION: u32 = 2;
pub(crate) const LOSS_SEQUENCE_EXHAUSTED: u32 = 4;
pub(crate) const LOG_LOSS_FLAGS: u32 = 7;

/// A fixed immutable diagnostic record, never control or authoring authority.
///
/// Unknown severity and invalid UTF-8 preserve exact retained bytes. Records
/// are field-encoded on the wire; Rust padding is never transmitted or read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAudioLogRecord {
    pub(crate) sequence: u64,
    pub(crate) raw_severity: i32,
    pub(crate) length: u16,
    pub(crate) utf8_prefix_bytes: u16,
    pub(crate) flags: u16,
    pub(crate) bytes: [u8; NATIVE_AUDIO_LOG_MESSAGE_BYTES],
}

impl NativeAudioLogRecord {
    pub(crate) const EMPTY: Self = Self {
        sequence: 0,
        raw_severity: 0,
        length: 0,
        utf8_prefix_bytes: 0,
        flags: 0,
        bytes: [0; NATIVE_AUDIO_LOG_MESSAGE_BYTES],
    };
    /// Queue reservation order inside this native owner, not wall-clock time.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
    /// Exact signed value emitted by the native producer.
    pub const fn raw_severity(&self) -> i32 {
        self.raw_severity
    }
    /// Official severity or explicitly unknown.
    pub const fn severity(&self) -> Option<NativeAudioLogSeverity> {
        NativeAudioLogSeverity::from_raw(self.raw_severity)
    }
    /// Exact bounded retained bytes, without lossy UTF-8 replacement.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.length)]
    }
    /// Whether a legal native message exceeded the retained prefix.
    pub const fn truncated(&self) -> bool {
        self.flags & LOG_TRUNCATED != 0
    }
    /// Whether the callback supplied a null pointer instead of a legal string.
    pub const fn null_message(&self) -> bool {
        self.flags & LOG_NULL != 0
    }
    /// Whether the retained bytes contain a malformed UTF-8 sequence.
    pub const fn invalid_utf8(&self) -> bool {
        self.flags & LOG_INVALID_UTF8 != 0
    }
    /// Whether truncation retained an incomplete UTF-8 suffix.
    pub const fn incomplete_utf8_suffix(&self) -> bool {
        self.flags & LOG_INCOMPLETE_UTF8_SUFFIX != 0
    }
    /// Validate the retained bytes; no omitted bytes are fabricated.
    pub fn text(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(self.bytes())
    }
    /// Exact known-valid prefix of the retained bytes.
    pub fn valid_utf8_prefix(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.bytes[..usize::from(self.utf8_prefix_bytes)])
    }
}

/// Fixed response storage with explicit lifetime-sticky loss facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAudioLogBatch {
    pub(crate) records: [NativeAudioLogRecord; NATIVE_AUDIO_LOG_RECORDS],
    pub(crate) count: usize,
    pub(crate) loss_reasons: u32,
}

impl Default for NativeAudioLogBatch {
    fn default() -> Self {
        Self {
            records: [NativeAudioLogRecord::EMPTY; NATIVE_AUDIO_LOG_RECORDS],
            count: 0,
            loss_reasons: 0,
        }
    }
}

impl NativeAudioLogBatch {
    /// Retained records in producer reservation order.
    pub fn records(&self) -> &[NativeAudioLogRecord] {
        &self.records[..self.count]
    }
    /// Whether bounded capacity was unavailable for at least one callback.
    pub const fn capacity_loss(&self) -> bool {
        self.loss_reasons & LOSS_FULL != 0
    }
    /// Whether a callback exhausted its bounded reservation attempts.
    pub const fn contention_loss(&self) -> bool {
        self.loss_reasons & LOSS_CONTENTION != 0
    }
    /// Whether the non-wrapping owner sequence was exhausted.
    pub const fn sequence_exhausted(&self) -> bool {
        self.loss_reasons & LOSS_SEQUENCE_EXHAUSTED != 0
    }
    /// An explicit loss fact, not an invented exact dropped-message count.
    pub const fn incomplete(&self) -> bool {
        self.loss_reasons != 0
    }
}

/// Select whether the sole observation request consumes pending diagnostics.
///
/// Capability/binding preparation preserves logs. The actual serial diagnostic
/// consumer drains them; neither request creates author or editor permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioObservationConsumption {
    /// Observe only source/control capability facts, without consuming a
    /// diagnostic prefix or querying optional parameter presentations.
    Preserve,
    /// Consume at most one admitted fixed diagnostic batch.
    Drain,
    /// Consume diagnostics and a complete author batch only after the sole
    /// installed Runtime has admitted this exact same-object binding.
    DrainAuthor(crate::NativeAudioObjectBindRequest),
    /// Confirm a delivery only after canonical Session and live parameter-plan
    /// handoff acceptance. Public facts alone never authorize this operation.
    AcknowledgeAuthor(crate::NativeAudioAuthorAcknowledgement),
    /// Confirm a privately prepared old-prefix/fresh-object transition.
    AcknowledgeReenteredAuthor(crate::NativeAudioReenteredAuthorAcknowledgement),
}

impl NativeAudioObservationConsumption {
    /// Whether this request consumes the existing bounded diagnostic source.
    /// Ordered author work always remains a separate explicit admission.
    pub const fn drains_logs(self) -> bool {
        !matches!(self, Self::Preserve)
    }
    pub(crate) const fn author_binding(self) -> Option<crate::NativeAudioObjectBindRequest> {
        match self {
            Self::DrainAuthor(binding) => Some(binding),
            Self::AcknowledgeAuthor(ack) => Some(ack.installed_binding),
            Self::AcknowledgeReenteredAuthor(ack) => Some(ack.installed_binding),
            _ => None,
        }
    }
}

/// One response on the sole current native-owner observation protocol.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NativeAudioObservations {
    pub(crate) owner_process_id: u32,
    /// Actual editor object capability, independently checked against the child.
    pub object: Option<NativeAudioObjectObservation>,
    /// Read-only projection of the same actual native attachment cleanup stage.
    /// This source fact cannot issue a Runtime or author lease.
    pub editor_cleanup: Option<crate::NativeAudioEditorCloseStage>,
    /// Non-consuming lifecycle request read on the same actual native main
    /// thread as object/cleanup facts. This is evidence for root preflight, not
    /// an installed contract, cohort retirement or permission to run old DSP.
    pub pending_reprepare: Option<crate::NativeObservedRequest>,
    /// Actual ordered author source from this same main-thread snapshot.
    /// None means unsupported/unavailable, never a synthetic closed-empty proof.
    pub author: Option<crate::NativeAudioAuthorObservation>,
    /// Actual coherent main/source revision of these presentation facts.
    pub parameter_revision: u64,
    /// Readonly/DONT_RECORD/process-origin and metadata facts from the same Poll.
    /// No plain value establishes an author curve by being present here.
    pub parameters: Vec<mondrian_core::NativeAudioParameterFact>,
    /// Bounded diagnostics from the same supervised child owner.
    pub logs: NativeAudioLogBatch,
}

impl NativeAudioObservations {
    /// Actual reply producer checked against the retained authenticated child.
    /// Local non-native defaults have no supervised producer identity.
    pub const fn owner_process_id(&self) -> Option<std::num::NonZeroU32> {
        std::num::NonZeroU32::new(self.owner_process_id)
    }
}

/// Full-owner result of one selected native presentation query. The serial
/// parent projects its already validated target; the child carries no UI ticket.
pub type NativeAudioParameterQueryResult = Result<
    (u64, Vec<mondrian_core::NativeAudioParameterFact>),
    mondrian_core::NativeAudioParameterQueryRefusal,
>;
