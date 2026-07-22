//! Production Adapter for UI-independent recursive Preview Timeline execution.

use super::*;
use crate::app::preview_timeline_execution::{
    resolve_preview_timeline, PreviewTimelineExecutionFact, PreviewTimelineMediaRequest,
    PreviewTimelineResolution,
};

impl<O: Clone> PreviewProductionRuntime<O> {
    pub(super) fn resolve_timeline(
        &self,
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        width: u32,
        height: u32,
        color_context: ColorContext,
    ) -> PreviewTimelineResolution {
        let mut media_frame =
            |request: PreviewTimelineMediaRequest| self.media_frame_for_plan(state, request);
        let resolution = resolve_preview_timeline(
            sequence,
            state.sequences(),
            frame,
            Resolution { width, height },
            state.playback_preview_resolution_scale(),
            color_context,
            &mut media_frame,
        );
        match &resolution {
            PreviewTimelineResolution::Ready(resolved) => {
                for fact in &resolved.facts {
                    self.record_timeline_execution_fact(fact);
                }
            }
            PreviewTimelineResolution::Pending { asset_id } => {
                tracing::trace!(%asset_id, "viewer Timeline is waiting for media");
            }
            PreviewTimelineResolution::Unavailable { reason } => {
                tracing::warn!(
                    code = reason.code(),
                    detail = reason.detail(),
                    "viewer Timeline resolution failed"
                );
            }
            PreviewTimelineResolution::Empty => {}
        }
        resolution
    }

    fn record_timeline_execution_fact(&self, fact: &PreviewTimelineExecutionFact) {
        match fact {
            PreviewTimelineExecutionFact::Composite(diagnostics) => {
                self.record_composite(*diagnostics);
            }
            PreviewTimelineExecutionFact::ColorTransform(diagnostics) => {
                self.record_color_transform(*diagnostics);
            }
            PreviewTimelineExecutionFact::ColorStage(diagnostics) => {
                self.record_color_stage(*diagnostics);
            }
            PreviewTimelineExecutionFact::CpuExecution(durations) => {
                let total_us =
                    durations.working_prepare_us.saturating_add(durations.cpu_composite_us);
                self.record_render_stage_durations(
                    total_us,
                    PreviewRenderStageDurations::from_cpu_execution(*durations),
                );
            }
        }
    }
}
