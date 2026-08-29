//! Local-monitor presentation Module.

use crate::{
    color::program_output::ProgramOutputBoundary,
    color_stage::{CpuSignalMonitoringError, RenderProgramMonitorPresentationRgba8},
    CpuColorFrame, RenderCpuColorExecutionSession, RenderMonitorAdaptation,
    RenderMonitorAdaptationError,
};
use mondrian_core::{types::ColorSpace, ProgramScopesTap, SignalMonitoringSettings};

/// Preview-only Program Output to local-monitor adaptation plan.
pub use crate::RenderMonitorAdaptation as MonitorColorPlan;

/// Monitor-plan construction and presentation execution Interface.
pub struct MonitorColorModule;

impl MonitorColorModule {
    /// Build a fail-closed monitor plan tied to the Program Output identity.
    pub fn plan(
        boundary: &ProgramOutputBoundary,
        monitor_color_space: ColorSpace,
    ) -> Result<MonitorColorPlan, RenderMonitorAdaptationError> {
        RenderMonitorAdaptation::new(
            boundary.output_color_space(),
            monitor_color_space,
            boundary.engine().clone(),
        )
    }

    /// Execute Program Output and monitor adaptation with one final RGBA8 quantization.
    pub fn present_cpu_rgba8(
        frame: &CpuColorFrame,
        boundary: &ProgramOutputBoundary,
        plan: &MonitorColorPlan,
        session: &mut RenderCpuColorExecutionSession,
    ) -> Result<RenderProgramMonitorPresentationRgba8, crate::RenderColorTransformError> {
        crate::color_stage::execute_cpu_program_monitor_presentation_rgba8_with_session(
            frame, boundary, plan, session,
        )
    }

    /// Execute monitor presentation with the shared Program/Monitor warning tap.
    pub fn present_cpu_rgba8_with_signal_monitoring(
        frame: &CpuColorFrame,
        boundary: &ProgramOutputBoundary,
        plan: &MonitorColorPlan,
        tap: ProgramScopesTap,
        settings: SignalMonitoringSettings,
        session: &mut RenderCpuColorExecutionSession,
    ) -> Result<RenderProgramMonitorPresentationRgba8, CpuSignalMonitoringError> {
        crate::color_stage::execute_cpu_program_monitor_presentation_rgba8_with_signal_monitoring_with_session(
            frame, boundary, plan, tap, settings, session,
        )
    }
}
