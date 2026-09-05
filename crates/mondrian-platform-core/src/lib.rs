//! Platform boundary interfaces shared by UI and desktop adapters.
//!
//! This crate intentionally contains no operating-system implementation. UI
//! crates depend on these contracts, while desktop shells provide concrete
//! adapters from `mondrian-platform`. Domain modules keep unrelated platform
//! facts from growing into one universal service interface.

mod desktop;
mod display;
mod endurance_qualification;
mod memory;
mod qualification_matrix;
mod user_state_directory;

pub use desktop::{
    ClipboardError, FileDialogError, FileDialogOutcome, FileFilter, FileRevealError,
    PlatformService,
};
pub use display::{
    DisplayHdrProbe, DisplayHdrProbeDetails, DisplayHdrProbeResult, DisplayIccProfileProbeResult,
    DisplayProbeBackend, DisplayProfileProbe, DisplayProfileProbeTarget,
};
pub use endurance_qualification::{
    EnduranceCounterRequirement, EnduranceCounters, EnduranceGauges, EnduranceMemoryRequirement,
    EndurancePhaseChunkReceipt, EndurancePhaseKind, EndurancePhaseManifest,
    EndurancePhaseProducerEvidence, EndurancePhaseReport, EndurancePhaseRequirement,
    EndurancePhaseTerminalEvidence, EndurancePhaseTerminalStatus, EnduranceProcessMemorySample,
    EnduranceQualificationError, EnduranceQualificationProfile, EnduranceQualificationReport,
    EnduranceQualificationStatus, EnduranceRunManifest, EnduranceRunOwnerClosureEvidence,
    EnduranceSample, EnduranceSampleChunk, PreparedEnduranceQualification,
    ProcessEventLoopOwnerClosureEvidence,
};
pub use memory::{
    ExecutionMemoryProbe, PhysicalMemoryCapacityProbe, PhysicalMemoryCapacityProbeBackend,
    PhysicalMemoryCapacityProbeResult, ProcessMemoryProbe, ProcessMemoryProbeBackend,
    ProcessMemoryProbeResult, ProcessMemoryScope, ProcessPrivateMemoryMetric, SystemMemoryProbe,
    SystemMemoryProbeBackend, SystemMemoryProbeResult,
};
pub use qualification_matrix::{
    PlatformDriverDisplayQualificationProfile, PlatformQualificationCampaign,
    PlatformQualificationCellObservation, PlatformQualificationCellReport,
    PlatformQualificationCellRequirement, PlatformQualificationDriverIdentity,
    PlatformQualificationEnvironment, PlatformQualificationError,
    PlatformQualificationEvidenceKind, PlatformQualificationEvidenceReport,
    PlatformQualificationEvidenceRequirement, PlatformQualificationLimits,
    PlatformQualificationProductArtifact, PlatformQualificationReport,
    PlatformQualificationScenarioEvidence, PlatformQualificationScenarioRequirement,
    PlatformQualificationStatus, PreparedPlatformDriverDisplayQualification,
    QualificationAdapterKind, QualificationDisplayScenario, QualificationGraphicsBackend,
    QualificationHdrPresentation, QualificationHdrTransferFunction, QualificationPlatform,
    QualificationPresentationTransfer, QualificationSurfaceColorSpace,
};
pub use user_state_directory::{UserStateDirectory, UserStateDirectoryError};

/// Empty platform implementation for tests and headless UI execution.
///
/// Every capability either returns its typed unavailable result or a benign
/// cancelled/no-op shell outcome. It never manufactures native evidence.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopPlatformService;
