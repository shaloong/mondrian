//! Source-frame interpretation Module.

use crate::{
    CpuColorFrame, CpuSourceColorFrame, RenderColorStageDiagnostics,
    RenderColorTransformDiagnostics, RenderCpuColorExecutionSession, RenderInputTransform,
};
use mondrian_timeline::sequence::MediaInputColorContext;

/// Working-space source preparation result with generic stage IR hidden.
pub struct SourceColorRecord {
    frame: CpuColorFrame,
    transform_diagnostics: RenderColorTransformDiagnostics,
    stage_diagnostics: RenderColorStageDiagnostics,
}

impl SourceColorRecord {
    /// Prepared working-space frame.
    pub fn frame(&self) -> &CpuColorFrame {
        &self.frame
    }

    /// Color-transform diagnostics for source interpretation.
    pub const fn transform_diagnostics(&self) -> RenderColorTransformDiagnostics {
        self.transform_diagnostics
    }

    /// Stage diagnostics for source preparation.
    pub const fn stage_diagnostics(&self) -> RenderColorStageDiagnostics {
        self.stage_diagnostics
    }

    /// Consume the record and retain the prepared frame.
    pub fn into_frame(self) -> CpuColorFrame {
        self.frame
    }
}

/// Source interpretation and source-to-working execution Interface.
pub struct SourceColorModule;

impl SourceColorModule {
    /// Resolve a media-input context for CPU source preparation.
    pub fn cpu_intent(context: &MediaInputColorContext) -> RenderInputTransform {
        RenderInputTransform::to_working(
            context.working_color_space,
            context.input_tone_map,
            context.engine.clone(),
        )
    }

    /// Resolve a media-input context for required native GPU preparation.
    pub fn gpu_intent(context: &MediaInputColorContext) -> RenderInputTransform {
        RenderInputTransform::to_working_gpu(
            context.working_color_space,
            context.input_tone_map,
            context.engine.clone(),
        )
    }

    /// Execute any supported CPU source payload into the Timeline working space.
    pub fn execute_cpu(
        frame: &CpuSourceColorFrame,
        context: &MediaInputColorContext,
        session: &mut RenderCpuColorExecutionSession,
    ) -> Result<SourceColorRecord, crate::RenderColorTransformError> {
        Self::execute_cpu_with_intent(frame, &Self::cpu_intent(context), session)
    }

    /// Execute a source payload with a previously resolved source intent.
    pub fn execute_cpu_with_intent(
        frame: &CpuSourceColorFrame,
        intent: &RenderInputTransform,
        session: &mut RenderCpuColorExecutionSession,
    ) -> Result<SourceColorRecord, crate::RenderColorTransformError> {
        let execution = crate::color_stage::execute_cpu_source_input_stage_with_session(
            frame, intent, session,
        )?;
        Ok(SourceColorRecord {
            frame: execution.result.frame,
            transform_diagnostics: execution.result.diagnostics,
            stage_diagnostics: execution.stage_diagnostics,
        })
    }
}
