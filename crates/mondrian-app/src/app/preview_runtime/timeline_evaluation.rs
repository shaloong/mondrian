//! Production Adapter for UI-independent prepared Preview materialization.

use std::sync::Arc;

use super::*;
use crate::app::preview_timeline_execution::{
    resolve_preview_timeline_with_programs_and_observer, PreviewTimelineExecutionBinding,
    PreviewTimelineExecutionFact, PreviewTimelineFrameRequest, PreviewTimelineMediaRequest,
    PreviewTimelinePendingDependency, PreviewTimelineResolution, PreviewTimelineSourceAdapters,
    PreviewTimelineTitleRequest,
};

impl<O: Clone> PreviewProductionRuntime<O> {
    /// Resolve one frame evaluation through the single authoritative entry.
    ///
    /// This is the only path that may call [`Self::resolve_timeline`]; every
    /// consumer (GPU production, presentation arbitration, and later headless)
    /// must go through here so one semantic evaluation has exactly one
    /// producer. `monitor_adaptation` is already-proven by the GPU producer;
    /// optional consumers let the helper prove it best-effort.
    pub(super) fn resolve_frame_evaluation(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        sequence: &Sequence,
        frame: i64,
        width: u32,
        height: u32,
        color_context: ProgramColorContext,
        evaluation_key: FrameEvaluationKey,
        monitor_adaptation: Option<&RenderMonitorAdaptation>,
    ) -> FrameResolutionOutcome {
        let resolution = self.resolve_timeline(
            snapshot,
            proxy_demands,
            sequence,
            frame,
            width,
            height,
            color_context,
        );
        match resolution {
            PreviewTimelineResolution::Ready(resolved) => {
                let plan = resolved.plan;
                let output_key = plan.cache_key.with_monitor_adaptation_opt(monitor_adaptation);
                let resolved_quality = match resolved_preview_presentation_quality(&plan.elements) {
                    mondrian_playback::FramePresentationQuality::Ready => {
                        ResolvedFrameQuality::Full
                    }
                    mondrian_playback::FramePresentationQuality::Degraded => {
                        ResolvedFrameQuality::Half
                    }
                };
                let reuse_policy = if plan.cache_reusable {
                    EvaluationReusePolicy::Reusable
                } else {
                    EvaluationReusePolicy::Transient
                };
                FrameResolutionOutcome::Ready(Arc::new(ResolvedFrameEvaluation {
                    key: evaluation_key,
                    output_key,
                    elements: plan.elements.into(),
                    color_context: plan.color_context,
                    resolved_quality,
                    reuse_policy,
                    // P6 commit 2: dependency tracking arrives with the
                    // EvaluationCoordinator; until then re-resolution is
                    // driven by the existing per-call flow.
                    dependencies: Arc::from([]),
                }))
            }
            PreviewTimelineResolution::Empty => FrameResolutionOutcome::Empty,
            PreviewTimelineResolution::Pending { dependency } => {
                FrameResolutionOutcome::Pending(dependency)
            }
            PreviewTimelineResolution::Unavailable { reason } => {
                FrameResolutionOutcome::Unavailable(reason)
            }
        }
    }

    pub(super) fn resolve_timeline(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        sequence: &Sequence,
        frame: i64,
        width: u32,
        height: u32,
        color_context: ProgramColorContext,
    ) -> PreviewTimelineResolution {
        self.synchronize_visual_program_authoring_session(snapshot);
        let transport = snapshot.transport();
        if transport.is_playing()
            && let Some(active) = transport.demand().map(PreviewFrameDemandSnapshot::identity)
        {
            self.scheduler.synchronize_playback_current_demand(active);
        }
        let mut media_frame = |request: PreviewTimelineMediaRequest| {
            self.media_frame_for_plan(snapshot, proxy_demands, request)
        };
        let mut title_frame =
            |request: PreviewTimelineTitleRequest| self.title_frame_for_plan(request);
        let sequences = snapshot
            .authoring()
            .map(PreviewAuthoringSnapshot::sequences)
            .unwrap_or_default();
        let Some(author_snapshot) = snapshot
            .authoring()
            .map(PreviewAuthoringSnapshot::visual_author_snapshot_identity)
        else {
            return PreviewTimelineResolution::Unavailable {
                reason: PreviewUnavailability::no_content(
                    PreviewOutputStage::Project,
                    "Timeline resolution requires an Authoring Snapshot",
                ),
            };
        };
        let (generation, cancellation) = {
            let execution = self.execution.borrow();
            (execution.generation(), execution.generation_cancellation())
        };
        let heterogeneous_graph_budget =
            self.heterogeneous_effect_decision.get().cpu_prefix_grant().graph_execution();
        let resolution = resolve_preview_timeline_with_programs_and_observer(
            PreviewTimelineFrameRequest::new(
                sequence,
                sequences,
                frame,
                Resolution { width, height },
                snapshot.transport().runtime_scale(),
                color_context,
            ),
            PreviewTimelineSourceAdapters::new(&mut media_frame, &mut title_frame),
            PreviewTimelineExecutionBinding::new(
                &self.visual_programs,
                &self.scratch,
                generation,
                cancellation,
                author_snapshot,
                &self.visual_dependencies,
                heterogeneous_graph_budget,
            ),
        );
        match &resolution {
            PreviewTimelineResolution::Ready(resolved) => {
                for fact in &resolved.facts {
                    self.record_timeline_execution_fact(fact);
                }
            }
            PreviewTimelineResolution::Pending { dependency } => match dependency {
                PreviewTimelinePendingDependency::Media(asset_id) => {
                    tracing::trace!(%asset_id, "viewer Timeline is waiting for media");
                }
                PreviewTimelinePendingDependency::BasicTitle(request_identity) => {
                    tracing::trace!(
                        request_identity = %request_identity,
                        "viewer Timeline is waiting for Basic Title generation"
                    );
                }
                PreviewTimelinePendingDependency::Temporal { clip_id, pending_sources } => {
                    tracing::trace!(
                        %clip_id,
                        pending_sources,
                        "viewer Timeline is waiting for a complete temporal source set"
                    );
                }
            },
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
