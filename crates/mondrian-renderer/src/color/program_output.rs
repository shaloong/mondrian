//! Program Output Module.

use crate::{
    color_stage::RenderOutputColorBoundary, CpuColorFrame, RenderCpuColorExecutionSession,
};
use mondrian_core::{OcioColorSpaceIdentity, OutputTransformIntentResolutionError};
use mondrian_timeline::sequence::ProgramColorContext;

/// Closed working-to-Program-Output contract.
pub use crate::color_stage::RenderOutputColorBoundary as ProgramOutputBoundary;
/// Encoded-float Program Output result.
pub use crate::color_stage::RenderOutputColorBoundaryFloat as ProgramOutputFloat;
/// Encoded RGBA8 Program Output result.
pub use crate::color_stage::RenderOutputColorBoundaryRgba8 as ProgramOutputRgba8;
/// Semantic role of the Program Output consumer.
pub use crate::color_stage::RenderOutputColorBoundaryTarget as ProgramOutputRole;

/// Failure to derive a renderer Program Output boundary from Timeline semantics.
#[derive(Debug, thiserror::Error)]
pub enum ProgramOutputBoundaryError {
    /// Nested/working-only contexts cannot become encoded Program Output.
    #[error("Program color context ends in working space {working_color_space:?}")]
    WorkingOnly {
        /// Working identity at the nested boundary.
        working_color_space: mondrian_core::WorkingColorSpace,
    },
    /// Product output intent could not be resolved by the pinned engine.
    #[error(transparent)]
    Intent(#[from] OutputTransformIntentResolutionError),
}

/// Program Output construction and execution Interface.
pub struct ProgramOutputModule;

impl ProgramOutputModule {
    /// Derive one closed Program Output boundary from the canonical Timeline context.
    pub fn boundary(
        role: ProgramOutputRole,
        context: &ProgramColorContext,
    ) -> Result<ProgramOutputBoundary, ProgramOutputBoundaryError> {
        let output_color_space = match context.output_color_space() {
            OcioColorSpaceIdentity::Color(space) => space,
            OcioColorSpaceIdentity::Working(working_color_space) => {
                return Err(ProgramOutputBoundaryError::WorkingOnly { working_color_space });
            }
        };
        Ok(RenderOutputColorBoundary::from_intent(
            role,
            output_color_space,
            context.output_transform(),
            context.output_tone_map(),
            context.engine().clone(),
        )?)
    }

    /// Execute Program Output as encoded RGBA8 through a shared CPU Session.
    pub fn execute_cpu_rgba8(
        frame: &CpuColorFrame,
        boundary: &ProgramOutputBoundary,
        session: &mut RenderCpuColorExecutionSession,
    ) -> Result<ProgramOutputRgba8, crate::RenderColorTransformError> {
        crate::color_stage::execute_cpu_output_boundary_rgba8_with_session(frame, boundary, session)
    }

    /// Execute Program Output in float precision through a shared CPU Session.
    pub fn execute_cpu_float(
        frame: &CpuColorFrame,
        boundary: &ProgramOutputBoundary,
        session: &mut RenderCpuColorExecutionSession,
    ) -> Result<ProgramOutputFloat, crate::RenderColorTransformError> {
        crate::color_stage::execute_cpu_output_boundary_float_with_session(frame, boundary, session)
    }
}
