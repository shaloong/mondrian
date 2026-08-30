//! # mondrian-export
//!
//! 导出与渲染系统：渲染队列 / 硬件编码 / 格式预设

mod artifact_identity;
mod audio_stems;
pub mod capture;
pub mod delivery;
pub mod frame_contract;
mod hardware_encoding;
mod image_sequence;
pub mod mezzanine;
pub mod preset;
pub mod queue;
mod smart_render;
pub mod validator;
pub mod video_encoding;

pub use capture::{
    prepare_timeline_export_dependencies,
    prepare_timeline_export_dependencies_with_audio_selection,
    validate_timeline_export_execution_snapshot,
    validate_timeline_export_execution_snapshot_with_audio_selection,
    PreparedTimelineAudioOutputSnapshot, PreparedTimelineAudioSnapshot,
    PreparedTimelineExecutionSnapshot, PreparedTimelineExportDependencies,
    PreparedTimelineVisualSnapshot, TimelineExportDependencyError,
};
pub use delivery::{
    resolve_export_delivery, ExportDeliveryError, ExportDeliveryIssueCode,
    ResolvedExportDeliveryContract,
};
pub use frame_contract::{ExportFrameContract, ExportFramePackingError};
pub use preset::{
    AudioStemFormat, BuiltinExportPreset, ExportAudioProgramSelection, ExportConfig,
    ExportFrameSampling, ExportMediaDependency, ExportOutputPolicy, ExportPreset,
    ExportSmartRenderPolicy, ResolvedTimelineExportRange, TimelineExportRange,
    TimelineExportRangeError, TimelineExportSnapshot,
};
pub use queue::{
    expected_export_video_signal, ExportAdmissionError, ExportArtifactPublicationEvidence,
    ExportCancelOutcome, ExportColorHealthAction, ExportColorHealthArea, ExportColorHealthCheck,
    ExportColorHealthReport, ExportColorHealthRootCause, ExportColorHealthSeverity,
    ExportColorHealthVerdict, ExportExecutionResourcePolicy, ExportFailure, ExportFailureReason,
    ExportJobColorDiagnostics, ExportJobColorDiagnosticsSummary, ExportJobDiagnostics,
    ExportJobSnapshot, ExportProgress, ExportProgressDetail, ExportProgressPhase,
    ExportPublicationState, ExportQueueDiagnostics, ExportSmartRenderEvidence, JobStatus,
    RenderJob, RenderQueue, EXPORT_HETEROGENEOUS_ROUTE_CONTRACT_LOGICAL_BYTES,
};
pub use video_encoding::{
    resolve_video_coding_structure, ResolvedVideoCodingStructure, VideoCodingStructure,
    VideoSceneCutPolicy,
};
