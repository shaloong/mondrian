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

/// Persistent frozen Timeline materializer for validation Reference Output.
///
/// This session owns one immutable dependency snapshot and reuses Export's
/// production decoder, title, prepared-program, Effect, color, and composite
/// state across every contiguous frame. It stops before any delivery transform
/// so the caller receives the canonical full-raster working composite.
#[must_use = "a frozen Reference frame session must be driven or explicitly dropped"]
pub struct FrozenTimelineReferenceFrameSession {
    timeline: TimelineExportSnapshot,
    visual_session: ExportVisualRenderSession,
    cancellation: ExecutionCancellationToken,
    color_context: ProgramColorContext,
    resolution: Resolution,
    next_frame_index: u64,
    fault: Option<String>,
}

impl FrozenTimelineReferenceFrameSession {
    /// Prepare one persistent exact-source session at an explicit cadence phase.
    pub fn new(timeline: TimelineExportSnapshot, first_frame_index: u64) -> Result<Self, String> {
        let resolution = timeline.sequence.settings.resolution;
        let color_context = timeline
            .sequence
            .settings
            .root_program_color_context(&timeline.color_environment)
            .map_err(|error| format!("invalid root Program color context: {error}"))?;
        let visual_session = ExportVisualRenderSession::for_timeline(
            0,
            service::ExportExecutionResourcePolicy::default(),
            &timeline,
        )?;
        i64::try_from(first_frame_index).map_err(|_| {
            "Reference frame index exceeds the Timeline coordinate range".to_owned()
        })?;
        Ok(Self {
            timeline,
            visual_session,
            cancellation: ExecutionCancellationToken::new(),
            color_context,
            resolution,
            next_frame_index: first_frame_index,
            fault: None,
        })
    }

    /// Materialize the next contiguous full-raster working frame.
    ///
    /// A failed render permanently faults this generation; callers must create
    /// a new frozen session rather than continuing with partially advanced
    /// decoder or Effect state.
    pub fn render_next(&mut self) -> Result<ExportVisualFrameValidation, String> {
        if let Some(detail) = &self.fault {
            return Err(format!(
                "frozen Reference frame session is faulted: {detail}"
            ));
        }
        let timeline_frame = i64::try_from(self.next_frame_index).map_err(|_| {
            self.latch_fault(
                "Reference frame index exceeds the Timeline coordinate range".to_owned(),
            )
        })?;
        let result = render_frozen_working_frame(
            &self.timeline,
            &mut self.visual_session,
            &self.cancellation,
            timeline_frame,
            self.resolution,
            self.color_context.clone(),
            TimelineVisualExecutionIntent::ReferenceOutput,
        );
        match result {
            Ok(frame) => {
                self.next_frame_index = self.next_frame_index.checked_add(1).ok_or_else(|| {
                    self.latch_fault("Reference frame counter overflow".to_owned())
                })?;
                Ok(frame)
            }
            Err(detail) => Err(self.latch_fault(detail)),
        }
    }

    /// Exact physical/Timeline frame index that the next render will consume.
    pub const fn next_frame_index(&self) -> u64 {
        self.next_frame_index
    }

    /// Permanently cancel this generation before owner teardown.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    fn latch_fault(&mut self, detail: String) -> String {
        if self.fault.is_none() {
            self.fault = Some(detail);
        }
        self.fault
            .as_ref()
            .cloned()
            .unwrap_or_else(|| "frozen Reference frame session entered an unknown fault".to_owned())
    }
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
    render_frozen_working_frame(
        timeline,
        &mut visual_session,
        &cancellation,
        timeline_frame,
        resolution,
        color_context,
        TimelineVisualExecutionIntent::Export,
    )
}

fn render_frozen_working_frame(
    timeline: &TimelineExportSnapshot,
    visual_session: &mut ExportVisualRenderSession,
    cancellation: &ExecutionCancellationToken,
    timeline_frame: i64,
    resolution: Resolution,
    color_context: ProgramColorContext,
    intent: TimelineVisualExecutionIntent,
) -> Result<ExportVisualFrameValidation, String> {
    let closure = prepare_export_visual_frame_closure(
        timeline,
        visual_session,
        cancellation,
        &timeline.sequence,
        FramePosition::new(timeline_frame, timeline.sequence.time_base()),
        resolution,
        color_context,
        intent,
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
        delivery_pixels: ExportDeliveryPixelContract::unmodified(
            ExportFrameContract::from_bit_depth(timeline.sequence.settings.delivery.bit_depth),
        ),
        input_color_counts: None,
        stage_diagnostics: None,
        composite_diagnostics: None,
        export_diagnostics: None,
        visual_session,
        cancellation,
    };
    let mut adapter = ExportPreparedVisualAdapter {
        context: &mut context,
        root_target: Some(SequenceRenderTarget::Working(&mut working_frame)),
        mode: ExportPreparedVisualMode::Cpu,
    };
    match execute_prepared_visual_closure(&closure, &mut adapter) {
        Ok(PreparedExportVisualOutput::Root) => {}
        Ok(PreparedExportVisualOutput::NestedCpu(_) | PreparedExportVisualOutput::NestedGpu(_)) => {
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
