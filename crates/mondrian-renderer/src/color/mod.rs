//! Deep color execution Modules.
//!
//! Product callers enter color processing through four semantic boundaries:
//! source interpretation, working-space conversion, Program Output, and
//! monitor presentation. The generic stage IR and backend materialization
//! remain renderer-private implementation details.

mod gpu_session;
pub mod monitor;
pub mod program_output;
#[doc(hidden)]
pub mod qualification;
pub mod source;
pub mod working;

pub use gpu_session::{
    GpuColorBackendContext, GpuColorExecutionSession, GpuColorExecutionSessionError,
    GpuColorSessionReadbackError, GpuProgramInput, GpuProgramOutputError, GpuProgramOutputRecord,
};
pub use monitor::{MonitorColorModule, MonitorColorPlan};
pub use program_output::{
    ProgramOutputBoundary, ProgramOutputBoundaryError, ProgramOutputFloat, ProgramOutputModule,
    ProgramOutputRgba8, ProgramOutputRole,
};
pub use source::{SourceColorModule, SourceColorRecord};
pub use working::{CpuWorkingColorRecord, GpuWorkingColorRecord, WorkingColorModule};

pub use crate::color_stage::{
    CpuSignalMonitoringError, RenderColorStageDiagnostics, RenderColorStageGpuBlockerBreakdown,
    RenderGpuOutputBoundaryRuntimeDiagnostics, RenderGpuOutputDiagnosticArea,
    RenderGpuOutputFrameReport, RenderGpuOutputHealthAction, RenderGpuOutputHealthCheck,
    RenderGpuOutputHealthEvidence, RenderGpuOutputHealthReport, RenderGpuOutputHealthRootCause,
    RenderGpuOutputHealthSeverity, RenderGpuOutputHealthStatus, RenderGpuOutputHealthSummary,
    RenderGpuOutputHealthVerdict, RenderGpuOutputRuntimeDiagnosticsReport,
    RenderGpuOutputStageDiagnosticsReport, RenderProgramMonitorPresentationRgba8,
    RENDER_GPU_OUTPUT_HEALTH_REPORT_SCHEMA_VERSION,
};
