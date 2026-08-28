//! Validation-only observation of the real Export visual frame path.

use super::*;
use mondrian_renderer::{
    prepared_visual_execution_semantic_trace, PreparedVisualExecutionSemanticTrace,
};

/// Working-space pixels and canonical prepared semantics observed from one
/// real Export frame execution.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportVisualFrameValidation {
    /// Root working-linear composite before the delivery output boundary.
    pub working_frame: CpuColorFrame,
    /// Canonical semantic ledger read from the exact closure that produced
    /// `working_frame`.
    pub semantic_trace: PreparedVisualExecutionSemanticTrace,
}

/// Execute one frame through Export's immutable Program, recursive closure,
/// temporal preparation, Effect executor, and working compositor.
///
/// The validation feature deliberately stops before codec and publication.
/// It calls the same private preparation and materialization functions as an
/// Export job and adds no Timeline, nesting, temporal, ROI, or Effect
/// interpretation.
pub fn export_visual_frame_validation(
    timeline: &TimelineExportSnapshot,
    timeline_frame: i64,
    resolution: Resolution,
) -> Result<ExportVisualFrameValidation, String> {
    let cancellation = ExecutionCancellationToken::new();
    let mut visual_session = ExportVisualRenderSession::for_timeline(
        0,
        service::ExportExecutionResourcePolicy::default(),
        timeline,
    )?;
    let color_context = timeline
        .sequence
        .settings
        .root_program_color_context(&timeline.color_environment)
        .map_err(|error| format!("invalid root Program color context: {error}"))?;
    let closure = prepare_export_visual_frame_closure(
        timeline,
        &mut visual_session,
        &cancellation,
        &timeline.sequence,
        timeline_frame,
        resolution,
        color_context,
    )?;
    let semantic_trace = prepared_visual_execution_semantic_trace(&closure)?;
    let materialization_bytes = closure
        .conservative_cpu_materialization_active_bytes()
        .map_err(|error| error.to_string())?;
    visual_session
        .composite_scratch
        .admit_cpu_active_working_set(
            materialization_bytes,
            TimelineCpuCompositePrecision::Float32,
        )
        .map_err(|error| {
            format!("export visual closure exceeds its CPU working-set grant: {error}")
        })?;

    let mut working_frame = None;
    let mut context = ExportFrameRenderContext {
        media: &timeline.media,
        color_environment: &timeline.color_environment,
        alpha_mode: ExportAlphaMode::Preserve,
        delivery_pixels: ExportDeliveryPixelContract::unmodified(export_frame_contract(
            timeline.sequence.settings.delivery.bit_depth,
        )),
        input_color_counts: None,
        stage_diagnostics: None,
        composite_diagnostics: None,
        export_diagnostics: None,
        visual_session: &mut visual_session,
        cancellation: &cancellation,
    };
    let mut adapter = ExportPreparedVisualAdapter {
        context: &mut context,
        root_target: Some(SequenceRenderTarget::Working(&mut working_frame)),
    };
    match execute_prepared_visual_closure(&closure, &mut adapter) {
        Ok(PreparedExportVisualOutput::Root) => {}
        Ok(PreparedExportVisualOutput::Nested(_)) => {
            return Err(
                "prepared Export validation returned a nested frame for the root".to_owned(),
            );
        }
        Err(PreparedVisualExecutionError::Structure(error)) => {
            return Err(format!(
                "prepared Export validation execution failed closed: {error}"
            ));
        }
        Err(PreparedVisualExecutionError::Adapter(error)) => return Err(error),
    }
    let working_frame = working_frame
        .ok_or_else(|| "Export visual frame did not produce a root working composite".to_owned())?;
    Ok(ExportVisualFrameValidation { working_frame, semantic_trace })
}
