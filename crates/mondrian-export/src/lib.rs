//! # mondrian-export
//!
//! 导出与渲染系统：渲染队列 / 硬件编码 / 格式预设

pub mod capture;
pub mod delivery;
pub mod preset;
pub mod queue;
pub mod validator;

pub use capture::{
    prepare_timeline_export_dependencies, validate_timeline_export_execution_snapshot,
    PreparedTimelineAudioSnapshot, PreparedTimelineExecutionSnapshot,
    PreparedTimelineExportDependencies, PreparedTimelineVisualSnapshot,
    TimelineExportDependencyError,
};
pub use delivery::{
    resolve_export_delivery, ExportDeliveryError, ExportDeliveryIssueCode,
    ResolvedExportDeliveryContract,
};
pub use preset::{
    BuiltinExportPreset, ExportConfig, ExportFrameSampling, ExportMediaDependency,
    ExportOutputPolicy, ExportPreset, ResolvedTimelineExportRange, TimelineExportRange,
    TimelineExportRangeError, TimelineExportSnapshot,
};
pub use queue::{
    expected_export_video_signal, ExportAdmissionError, ExportArtifactPublicationEvidence,
    ExportCancelOutcome, ExportColorHealthAction, ExportColorHealthArea, ExportColorHealthCheck,
    ExportColorHealthReport, ExportColorHealthRootCause, ExportColorHealthSeverity,
    ExportColorHealthVerdict, ExportExecutionResourcePolicy, ExportFailure, ExportFailureReason,
    ExportJobColorDiagnostics, ExportJobColorDiagnosticsSummary, ExportJobDiagnostics,
    ExportJobSnapshot, ExportProgress, ExportProgressDetail, ExportProgressPhase,
    ExportPublicationState, ExportQueueDiagnostics, JobStatus, RenderJob, RenderQueue,
    EXPORT_HETEROGENEOUS_ROUTE_CONTRACT_LOGICAL_BYTES,
};
