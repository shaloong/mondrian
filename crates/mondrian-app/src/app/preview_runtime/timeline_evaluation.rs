//! Production Adapter for UI-independent prepared Preview materialization.

use std::sync::Arc;

use super::*;
use crate::app::preview_timeline_execution::{
    resolve_preview_timeline_with_programs_and_observer, PreviewTimelineExecutionBinding,
    PreviewTimelineExecutionFact, PreviewTimelineFrameRequest, PreviewTimelineMediaRequest,
    PreviewTimelinePendingDependency, PreviewTimelineResolution, PreviewTimelineSourceAdapters,
    PreviewTimelineTitleRequest,
};

/// Reconstruct the media timeline dependency for consumers that keep
/// branch-local pending handling.
fn media_pending_dependency_from_wait(
    dependencies: &[EvaluationDependency],
) -> Option<PreviewTimelinePendingDependency> {
    dependencies.first().map(|dependency| match dependency {
        EvaluationDependency::MediaProducer(key) => PreviewTimelinePendingDependency::Media {
            asset_id: key.asset_id,
            wait: crate::app::preview_timeline_execution::PreviewTimelineMediaWait::Producer,
        },
    })
}

impl<O: Clone> PreviewProductionRuntime<O> {
    /// Acquire one frame evaluation through the single authoritative entry.
    ///
    /// This is the only path that may call [`Self::resolve_timeline`]; every
    /// consumer (GPU production, presentation arbitration, and later headless)
    /// must go through here so one semantic evaluation has exactly one
    /// producer. Ready evaluations are deduplicated by [`FrameEvaluationKey`]
    /// in the bounded working set; repeated acquires for the same picture do
    /// not re-resolve.
    pub(super) fn acquire_frame_evaluation(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        sequence: &Sequence,
        frame: i64,
        width: u32,
        height: u32,
        color_context: ProgramColorContext,
        evaluation_key: FrameEvaluationKey,
    ) -> FrameResolutionOutcome {
        let clock = self.evaluation_working_set_clock.get();
        if let Some(evaluation) =
            self.evaluation_working_set.borrow_mut().get(evaluation_key, clock)
        {
            bump(&self.metrics.timeline_evaluation_hits);
            self.request_timeline_render_cache_lookup(&evaluation);
            return FrameResolutionOutcome::Ready(evaluation);
        }
        let request_priority = if snapshot.transport().is_speculative_preparation() {
            MediaPreviewRequestPriority::Prefetch
        } else {
            MediaPreviewRequestPriority::Current
        };
        if let Some(dependencies) = self
            .evaluation_working_set
            .borrow_mut()
            .waiting_for(evaluation_key, request_priority)
            && let Some(dependency) = media_pending_dependency_from_wait(&dependencies)
        {
            bump(&self.metrics.timeline_evaluation_wait_hits);
            // Presentation resets the per-turn level before acquiring an
            // evaluation. A retained producer wait is still backed by the
            // Broker even though timeline resolution is intentionally
            // deduplicated, so reassert the level on every cache hit.
            self.execution.borrow_mut().set_pending(true);
            return FrameResolutionOutcome::Pending(dependency);
        }
        bump(&self.metrics.timeline_evaluation_misses);
        self.bump_timeline_resolve_count();
        let (resolution, dependencies) = self.resolve_timeline(
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
                let evaluation = Arc::new(ResolvedFrameEvaluation {
                    key: evaluation_key,
                    output_key: plan.cache_key,
                    elements: plan.elements.into(),
                    color_context: plan.color_context,
                    resolved_quality,
                    reuse_policy,
                    dependencies: dependencies.into(),
                    render_cache_identity: plan.render_cache_identity,
                });
                // Far cold-source activation exists only to build native
                // import/color backend objects. Inserting it into the
                // four-entry ordinary LRU would evict the staged immediate
                // successor just before its current-frame promotion.
                if !snapshot.transport().is_cold_activation_preparation() {
                    let clock = self.evaluation_working_set_clock.get();
                    self.evaluation_working_set.borrow_mut().insert(
                        evaluation_key,
                        Arc::clone(&evaluation),
                        clock,
                    );
                    self.evaluation_working_set_clock.set(clock.saturating_add(1));
                }
                self.request_timeline_render_cache_lookup(&evaluation);
                FrameResolutionOutcome::Ready(evaluation)
            }
            PreviewTimelineResolution::Empty => FrameResolutionOutcome::Empty,
            PreviewTimelineResolution::Pending { dependency } => {
                let evaluation_dependency = match &dependency {
                    PreviewTimelinePendingDependency::Media { wait, .. }
                        if *wait
                            == crate::app::preview_timeline_execution::PreviewTimelineMediaWait::Producer =>
                    {
                        Some(dependencies)
                    }
                    PreviewTimelinePendingDependency::Media { .. }
                    | PreviewTimelinePendingDependency::BasicTitle(_)
                    | PreviewTimelinePendingDependency::Temporal { .. } => None,
                };
                if let Some(evaluation_dependencies) = evaluation_dependency {
                    let dependencies = evaluation_dependencies.into();
                    self.evaluation_working_set.borrow_mut().insert_waiting(
                        evaluation_key,
                        request_priority,
                        dependencies,
                    );
                }
                FrameResolutionOutcome::Pending(dependency)
            }
            PreviewTimelineResolution::Unavailable { reason } => {
                FrameResolutionOutcome::Unavailable(reason)
            }
        }
    }

    fn request_timeline_render_cache_lookup(&self, evaluation: &ResolvedFrameEvaluation) {
        if let Some(identity) = evaluation.render_cache_identity {
            let _ = self
                .timeline_render_cache
                .borrow()
                .request_lookup(identity, evaluation.color_context.working_color_space());
        }
    }

    pub(super) fn bump_timeline_resolve_count(&self) {
        bump(&self.metrics.timeline_resolve_count);
    }

    /// Test-only alias for resolving the timeline directly.
    ///
    /// Production code must use [`Self::acquire_frame_evaluation`]; this seam
    /// exists so unit tests can drive the raw timeline contract without
    /// bypassing the coordinator's resolve accounting.
    #[cfg(test)]
    pub(super) fn resolve_timeline_for_test(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        sequence: &Sequence,
        frame: i64,
        width: u32,
        height: u32,
        color_context: ProgramColorContext,
    ) -> PreviewTimelineResolution {
        self.bump_timeline_resolve_count();
        self.resolve_timeline(
            snapshot,
            proxy_demands,
            sequence,
            frame,
            width,
            height,
            color_context,
        )
        .0
    }

    /// Invalidate only evaluations that consumed one exact physical media key.
    pub(super) fn invalidate_evaluations_for_media_key(&self, key: &MediaPreviewKey) {
        self.evaluation_working_set.borrow_mut().invalidate_for_media_key(key);
    }

    /// Resolve the timeline exactly once per miss.
    ///
    /// Module-private by design: the evaluation coordinator is the only
    /// authoritative producer. Consumers must go through
    /// [`Self::acquire_frame_evaluation`]; a future consumer that needs the
    /// raw timeline result must be wired through the coordinator instead of
    /// calling this directly.
    fn resolve_timeline(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        sequence: &Sequence,
        frame: i64,
        width: u32,
        height: u32,
        color_context: ProgramColorContext,
    ) -> (PreviewTimelineResolution, Vec<EvaluationDependency>) {
        self.synchronize_visual_program_authoring_session(snapshot);
        let media_dependencies = RefCell::new(Vec::new());
        let mut media_frame = |request: PreviewTimelineMediaRequest| {
            self.media_frame_for_plan_observing_key(snapshot, proxy_demands, request, |key| {
                let dependency = EvaluationDependency::MediaProducer(key.clone());
                let mut dependencies = media_dependencies.borrow_mut();
                if !dependencies.contains(&dependency) {
                    dependencies.push(dependency);
                }
            })
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
            return (
                PreviewTimelineResolution::Unavailable {
                    reason: PreviewUnavailability::no_content(
                        PreviewOutputStage::Project,
                        "Timeline resolution requires an Authoring Snapshot",
                    ),
                },
                Vec::new(),
            );
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
                PreviewTimelinePendingDependency::Media { asset_id, wait } => {
                    let wait = match wait {
                        crate::app::preview_timeline_execution::PreviewTimelineMediaWait::Producer => "producer",
                        crate::app::preview_timeline_execution::PreviewTimelineMediaWait::RetryAdmission => "admission_retry",
                    };
                    tracing::trace!(%asset_id, wait, "viewer Timeline is waiting for media");
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
                tracing::debug!(
                    code = reason.code(),
                    detail = reason.detail(),
                    "viewer Timeline resolution failed"
                );
            }
            PreviewTimelineResolution::Empty => {}
        }
        (resolution, media_dependencies.into_inner())
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
