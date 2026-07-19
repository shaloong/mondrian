//! Window Adapter for UI-independent recursive Preview Timeline execution.

use super::*;
use crate::app::preview_timeline_execution::{
    resolve_preview_timeline, PreviewTimelineExecutionFact, PreviewTimelineMediaRequest,
    PreviewTimelineResolution, ResolvedPreviewPlan,
};

impl AppUiPreviewService {
    pub(super) fn resolve_sequence_elements(
        &self,
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        width: u32,
        height: u32,
        color_context: ColorContext,
    ) -> Option<ResolvedPreviewPlan> {
        let mut media_frame =
            |request: PreviewTimelineMediaRequest| self.media_frame_for_plan(state, request);
        match resolve_preview_timeline(
            sequence,
            &state.sequences,
            frame,
            Resolution { width, height },
            state.playback_preview_resolution_scale(),
            color_context,
            &mut media_frame,
        ) {
            PreviewTimelineResolution::Ready(resolved) => {
                for fact in resolved.facts {
                    self.record_timeline_execution_fact(fact);
                }
                Some(resolved.plan)
            }
            PreviewTimelineResolution::Empty => None,
            PreviewTimelineResolution::Pending { asset_id } => {
                tracing::trace!(%asset_id, "viewer Timeline is waiting for media");
                None
            }
            PreviewTimelineResolution::Unavailable { reason } => {
                tracing::warn!(%reason, "viewer Timeline resolution failed");
                None
            }
        }
    }

    fn record_timeline_execution_fact(&self, fact: PreviewTimelineExecutionFact) {
        match fact {
            PreviewTimelineExecutionFact::Composite(diagnostics) => {
                self.record_composite(diagnostics);
            }
            PreviewTimelineExecutionFact::ColorTransform(diagnostics) => {
                self.record_color_transform(diagnostics);
            }
            PreviewTimelineExecutionFact::ColorStage(diagnostics) => {
                self.record_color_stage(diagnostics);
            }
            PreviewTimelineExecutionFact::CpuExecution(durations) => {
                let total_us =
                    durations.working_prepare_us.saturating_add(durations.cpu_composite_us);
                self.record_render_stage_durations(
                    total_us,
                    AppUiPreviewRenderStageDurations::from_cpu_execution(durations),
                );
            }
        }
    }
}
