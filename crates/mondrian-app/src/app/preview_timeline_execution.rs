//! UI-independent materialization of prepared Viewer Timeline execution.
//!
//! The renderer-owned [`PreparedVisualFrameClosure`] is the sole authority for
//! nested source-time projection, lookup, cycle/depth validation, child canvas
//! and color context, Transition endpoints, temporal samples, and instance
//! paths. This Module only materializes those prepared nodes into working-space
//! pixels, resolved Viewer elements, pending/unavailable outcomes, and stable
//! cache identity. A caller supplies immutable Sequence snapshots only to the
//! Program/closure preparation Seam plus narrow media/title Adapters;
//! materialization consumes the closure and cannot reopen author state.

use std::cell::RefCell;
use std::collections::{hash_map::Entry, HashMap, HashSet};
use std::hash::Hash;
use std::sync::Arc;

use mondrian_core::timeline_data::{AlphaInterpretation, TimelineClipExecutionRef};
use mondrian_core::types::{AssetId, ColorSpace, SequenceId};
use mondrian_core::{ExecutionCancellationToken, FramePosition, Resolution, WorkingRgbaF32Frame};
use mondrian_effects::{
    CompiledEffectGraph, EffectFrameExtent, EffectFrameTileF32, EffectGraphExecutionBudget,
    EffectTemporalSourceIdentity, PreparedTemporalFrameSet,
};
use mondrian_playback::{FramePresentationQuality, PreviewResolutionScale};
use mondrian_render_cache::{TimelineRenderCacheFormat, TimelineRenderCacheIdentity};
use mondrian_renderer::{
    basic_title_raster_request_identity,
    color::{RenderColorStageDiagnostics, WorkingColorModule},
    execute_prepared_visual_closure, prepare_bound_visual_frame_closure,
    project_basic_title_transform, BasicTitleRasterFrame, BasicTitleRasterRequestIdentity,
    CpuColorFrame, PreparedHeterogeneousEffectRoute, PreparedTimelinePreviewEffectRoutes,
    PreparedVisualAuthorSnapshotIdentity, PreparedVisualChildCanvasPolicy,
    PreparedVisualExecutionAdapter, PreparedVisualExecutionError,
    PreparedVisualExecutionNodeInputs, PreparedVisualFrameClosure,
    PreparedVisualFrameClosureRequest, PreparedVisualFrameEvaluation, PreparedVisualFrameNode,
    PreparedVisualFrameNodeId, PreparedVisualMaterializationContract, PreparedVisualNestedSample,
    PreparedVisualProgramBinding, PreparedVisualProgramCache, RenderColorTransformDiagnostics,
    TimelineAdjustmentLayer, TimelineBasicTitlePlan, TimelineCompositeDiagnostics,
    TimelineCompositeScratch, TimelineCpuCompositePrecision, TimelineEvaluationRequest,
    TimelineFrameExecutionRequest, TimelineMediaPlan, TimelinePreviewEffectRouteError,
    TimelineRenderPlanElement, TimelineSolidColorLayer, TimelineTemporalDemandBatch,
    TimelineTemporalSource, TimelineTemporalSourceDemand, TimelineTransitionInputPlan,
};
#[cfg(test)]
use mondrian_renderer::{
    prepared_visual_execution_semantic_trace, PreparedVisualExecutionSemanticTrace,
};
use mondrian_timeline::sequence::{MediaInputColorContext, ProgramColorContext, Sequence};

use super::preview_cpu_execution::{PreviewCpuExecutionDurations, PreviewCpuExecutionError};
use super::preview_execution::{
    PreviewDecodeExecutionSummary, PreviewOutputKey, PreviewSemanticIdentity,
    PreviewSemanticIdentityBuilder,
};
use super::preview_media_frame::{project_preview_media_transform, MediaPreviewFrame};
use super::preview_quality::PreviewRasterRequirement;
use super::preview_unavailability::{PreviewOutputStage, PreviewUnavailability};
use super::preview_viewer_plan::{
    resolved_preview_decode_execution, resolved_preview_presentation_quality,
    viewer_preview_cache_key_for_resolved_plan, viewer_preview_plan_allows_cross_call_reuse,
    ResolvedPreviewElement, ResolvedPreviewTransitionInput,
};

/// Complete media-layer request emitted while materializing a prepared node.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PreviewTimelineMediaRequest {
    /// Frozen requirement of the original reachable Clip graph.
    pub(crate) image_scale_requirement: mondrian_effects::EffectImageScaleRequirement,
    pub(crate) asset_id: AssetId,
    pub(crate) color_space_override: Option<ColorSpace>,
    pub(crate) alpha_interpretation: AlphaInterpretation,
    pub(crate) picture_overrides: mondrian_core::PictureInterpretationOverrides,
    /// Exact source-local decode target. The media Adapter alone lowers this
    /// value into an FFmpeg stream PTS.
    pub(crate) source_sample: mondrian_core::SourceSampleTarget,
    pub(crate) target_resolution: Resolution,
    pub(crate) input_color: MediaInputColorContext,
    /// Temporal CPU execution and renderer-owned heterogeneous Effect
    /// execution require a working-memory result and therefore cannot accept
    /// an opaque native decoder surface. Ordinary full-GPU graphs retain the
    /// native/source-domain path.
    pub(crate) cpu_working_required: bool,
}

/// Exhaustive Adapter response for one media layer needed by Timeline execution.
#[derive(Clone)]
pub(crate) enum PreviewTimelineMediaFrame {
    Ready(MediaPreviewFrame),
    Pending { wait: PreviewTimelineMediaWait },
    Unavailable { reason: PreviewUnavailability },
}

/// Why an exact media request is pending.
///
/// A producer wait may be memoized until that producer publishes a result.
/// An admission retry has no producer yet and must be re-evaluated on the
/// next candidate pass; retaining it would suppress the retry that can make
/// forward progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewTimelineMediaWait {
    Producer,
    RetryAdmission,
}

/// Complete generated-title request emitted while materializing a prepared node.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PreviewTimelineTitleRequest {
    pub(crate) title: mondrian_core::EvaluatedBasicTitle,
    pub(crate) author_resolution: Resolution,
    pub(crate) sample_aspect_ratio: mondrian_core::SampleAspectRatio,
    pub(crate) title_safe_margin: f32,
    pub(crate) target_resolution: Resolution,
    pub(crate) working_color_space: mondrian_core::WorkingColorSpace,
}

impl PreviewTimelineTitleRequest {
    pub(crate) fn identity(&self) -> BasicTitleRasterRequestIdentity {
        basic_title_raster_request_identity(
            &self.title,
            self.author_resolution,
            self.title_safe_margin,
            self.target_resolution,
            self.working_color_space,
            self.sample_aspect_ratio,
        )
    }
}

/// Exhaustive Adapter response for one generated title source.
pub(crate) enum PreviewTimelineTitleFrame {
    Ready(BasicTitleRasterFrame),
    Pending,
    Unavailable { reason: PreviewUnavailability },
}

/// Typed dependency that keeps media identity distinct from generated work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewTimelinePendingDependency {
    Media {
        asset_id: AssetId,
        wait: PreviewTimelineMediaWait,
    },
    BasicTitle(BasicTitleRasterRequestIdentity),
    Execution {
        key: super::preview_visual_execution_task::VisualExecutionTaskKey,
    },
    Temporal {
        clip_id: mondrian_core::ClipId,
        /// Unresolved planned temporal source requests across the frozen closure.
        pending_sources: usize,
    },
}

/// Resolved root Viewer plan with an always-present semantic output identity
/// and separate cross-call reuse evidence.
pub(crate) struct ResolvedPreviewPlan {
    /// Whole-closure CPU cost, admitted only if the root actually executes on CPU.
    pub(crate) cpu_materialization_active_bytes: u64,
    pub(crate) elements: Vec<ResolvedPreviewElement>,
    pub(crate) cache_key: PreviewOutputKey,
    pub(crate) cache_reusable: bool,
    pub(crate) color_context: ProgramColorContext,
    pub(crate) render_cache_identity: Option<TimelineRenderCacheIdentity>,
    pub(crate) sampling: mondrian_core::ImageSampling,
}

/// Ordered execution fact emitted while materializing nested Sequences.
pub(crate) enum PreviewTimelineExecutionFact {
    Composite(TimelineCompositeDiagnostics),
    ColorTransform(RenderColorTransformDiagnostics),
    ColorStage(RenderColorStageDiagnostics),
    CpuExecution(PreviewCpuExecutionDurations),
}

/// Ready plan plus all facts produced before final root presentation.
pub(crate) struct ResolvedPreviewTimeline {
    pub(crate) plan: ResolvedPreviewPlan,
    pub(crate) facts: Vec<PreviewTimelineExecutionFact>,
    #[cfg(test)]
    pub(crate) semantic_trace: PreparedVisualExecutionSemanticTrace,
}

/// Exhaustive root result without Window-owned pending inference.
#[expect(
    clippy::large_enum_variant,
    reason = "One resolution transfers existing frame and preparation owners inline; do not add a per-frame variant allocation"
)]
pub(crate) enum PreviewTimelineResolution {
    Ready(ResolvedPreviewTimeline),
    Empty,
    Pending {
        dependency: PreviewTimelinePendingDependency,
        /// Original pre-DSP attempt and any inputs already acquired.
        preparation: Box<PreviewTimelinePreparation>,
    },
    Unavailable {
        reason: PreviewUnavailability,
    },
}

impl std::fmt::Debug for PreviewTimelineResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready(_) => f.write_str("Ready"),
            Self::Empty => f.write_str("Empty"),
            Self::Pending { dependency, .. } => f.debug_tuple("Pending").field(dependency).finish(),
            Self::Unavailable { reason } => {
                f.debug_tuple("Unavailable").field(&reason.detail()).finish()
            }
        }
    }
}

/// Complete immutable frame contract for one Preview Timeline evaluation.
pub(crate) struct PreviewTimelineFrameRequest<'a> {
    root_sequence: &'a Sequence,
    sequences: &'a [Sequence],
    frame: i64,
    target_resolution: Resolution,
    runtime_scale: PreviewResolutionScale,
    raster_requirement: PreviewRasterRequirement,
    color_context: ProgramColorContext,
}

impl<'a> PreviewTimelineFrameRequest<'a> {
    /// Bind one root frame to its exact author closure, raster, scale, and color context.
    pub(crate) fn new(
        root_sequence: &'a Sequence,
        sequences: &'a [Sequence],
        frame: i64,
        target_resolution: Resolution,
        runtime_scale: PreviewResolutionScale,
        color_context: ProgramColorContext,
    ) -> Self {
        Self {
            root_sequence,
            sequences,
            frame,
            target_resolution,
            runtime_scale,
            raster_requirement: PreviewRasterRequirement::Viewer,
            color_context,
        }
    }

    pub(crate) fn with_raster_requirement(mut self, requirement: PreviewRasterRequirement) -> Self {
        self.raster_requirement = requirement;
        self
    }
}

/// Narrow source-materialization Adapters used by prepared Timeline execution.
pub(crate) struct PreviewTimelineSourceAdapters<'a, MediaFrame, TitleFrame> {
    media_frame: &'a mut MediaFrame,
    title_frame: &'a mut TitleFrame,
    execute: Option<&'a mut dyn FnMut(Arc<PreparedPreviewTimelineWork>) -> PreviewTimelineWorkPoll>,
}

impl<'a, MediaFrame, TitleFrame> PreviewTimelineSourceAdapters<'a, MediaFrame, TitleFrame> {
    /// Bind media and generated-title resolution for one evaluation call.
    pub(crate) fn new(media_frame: &'a mut MediaFrame, title_frame: &'a mut TitleFrame) -> Self {
        Self { media_frame, title_frame, execute: None }
    }
}

impl<'a, MediaFrame, TitleFrame> PreviewTimelineSourceAdapters<'a, MediaFrame, TitleFrame> {
    pub(crate) fn with_execution(
        mut self,
        execute: &'a mut dyn FnMut(Arc<PreparedPreviewTimelineWork>) -> PreviewTimelineWorkPoll,
    ) -> Self {
        self.execute = Some(execute);
        self
    }
}

/// Generation-scoped execution authorities for production Preview evaluation.
pub(crate) struct PreviewTimelineExecutionBinding<'a> {
    programs: &'a RefCell<PreparedVisualProgramCache>,
    scratch: &'a RefCell<TimelineCompositeScratch>,
    generation: u64,
    cancellation: ExecutionCancellationToken,
    author_snapshot: PreparedVisualAuthorSnapshotIdentity,
    dependency_observer:
        &'a crate::app::preview_visual_dependencies::PreviewVisualDependencyObserver,
    heterogeneous_graph_budget: EffectGraphExecutionBudget,
}

impl<'a> PreviewTimelineExecutionBinding<'a> {
    /// Bind Program-cache trust, continuity generation, cancellation, and dependency observation.
    pub(crate) fn new(
        programs: &'a RefCell<PreparedVisualProgramCache>,
        scratch: &'a RefCell<TimelineCompositeScratch>,
        generation: u64,
        cancellation: ExecutionCancellationToken,
        author_snapshot: PreparedVisualAuthorSnapshotIdentity,
        dependency_observer: &'a crate::app::preview_visual_dependencies::PreviewVisualDependencyObserver,
        heterogeneous_graph_budget: EffectGraphExecutionBudget,
    ) -> Self {
        Self {
            programs,
            scratch,
            generation,
            cancellation,
            author_snapshot,
            dependency_observer,
            heterogeneous_graph_budget,
        }
    }
}

/// Collect the media dependencies of one frame without executing or scheduling them.
///
/// Prefetch and preroll iterate the same renderer-owned closure as real
/// execution, so neither Adapter can reinterpret nesting, quality, or color.
pub(crate) fn collect_preview_timeline_media_demands(
    sequence: &Sequence,
    sequences: &[Sequence],
    frame: i64,
    target_resolution: Resolution,
    runtime_scale: PreviewResolutionScale,
    color_context: ProgramColorContext,
) -> Result<Vec<PreviewTimelineMediaRequest>, PreviewUnavailability> {
    let programs = RefCell::new(PreparedVisualProgramCache::default());
    let scratch = RefCell::new(TimelineCompositeScratch::default());
    collect_preview_timeline_media_demands_with_programs(
        sequence,
        sequences,
        frame,
        target_resolution,
        runtime_scale,
        PreviewRasterRequirement::Viewer,
        color_context,
        &programs,
        &scratch,
        None,
        standalone_preview_heterogeneous_graph_budget(),
    )
}

pub(crate) fn collect_preview_timeline_media_demands_with_programs(
    sequence: &Sequence,
    sequences: &[Sequence],
    frame: i64,
    target_resolution: Resolution,
    runtime_scale: PreviewResolutionScale,
    raster_requirement: PreviewRasterRequirement,
    color_context: ProgramColorContext,
    programs: &RefCell<PreparedVisualProgramCache>,
    scratch: &RefCell<TimelineCompositeScratch>,
    author_snapshot: Option<PreparedVisualAuthorSnapshotIdentity>,
    heterogeneous_graph_budget: EffectGraphExecutionBudget,
) -> Result<Vec<PreviewTimelineMediaRequest>, PreviewUnavailability> {
    let cancellation = ExecutionCancellationToken::new();
    let graph = PreviewTimelineGraph {
        programs,
        scratch,
        dependency_observer: None,
        generation: 0,
        cancellation: &cancellation,
        author_snapshot,
        heterogeneous_graph_budget,
    };
    let closure = prepare_preview_frame_closure(
        graph,
        sequence,
        sequences,
        frame.max(0),
        target_resolution,
        color_context,
        runtime_scale,
        raster_requirement,
    )?;
    collect_prepared_visual_source_demands(&closure).map(|demands| {
        demands
            .into_iter()
            .filter_map(|demand| match demand {
                PreparedPreviewSourceDemand::Media { request, .. } => Some(request),
                PreparedPreviewSourceDemand::BasicTitle(_) => None,
            })
            .collect()
    })
}

/// Materialize one prepared root closure for both Window and Headless.
#[cfg(any(test, feature = "validation"))]
pub(crate) fn resolve_preview_timeline(
    sequence: &Sequence,
    sequences: &[Sequence],
    frame: i64,
    target_resolution: Resolution,
    runtime_scale: PreviewResolutionScale,
    color_context: ProgramColorContext,
    media_frame: &mut impl FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    title_frame: &mut impl FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
) -> PreviewTimelineResolution {
    let programs = RefCell::new(PreparedVisualProgramCache::default());
    let scratch = RefCell::new(TimelineCompositeScratch::default());
    resolve_preview_timeline_with_programs(
        sequence,
        sequences,
        frame,
        target_resolution,
        runtime_scale,
        color_context,
        media_frame,
        title_frame,
        &programs,
        &scratch,
    )
}

#[cfg(any(test, feature = "validation"))]
pub(crate) fn resolve_preview_timeline_with_programs(
    sequence: &Sequence,
    sequences: &[Sequence],
    frame: i64,
    target_resolution: Resolution,
    runtime_scale: PreviewResolutionScale,
    color_context: ProgramColorContext,
    media_frame: &mut impl FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    title_frame: &mut impl FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
    programs: &RefCell<PreparedVisualProgramCache>,
    scratch: &RefCell<TimelineCompositeScratch>,
) -> PreviewTimelineResolution {
    let cancellation = ExecutionCancellationToken::new();
    let graph = PreviewTimelineGraph {
        programs,
        scratch,
        dependency_observer: None,
        generation: 0,
        cancellation: &cancellation,
        author_snapshot: None,
        heterogeneous_graph_budget: standalone_preview_heterogeneous_graph_budget(),
    };
    resolve_preview_timeline_with_graph(
        PreviewTimelineFrameRequest::new(
            sequence,
            sequences,
            frame,
            target_resolution,
            runtime_scale,
            color_context,
        ),
        PreviewTimelineSourceAdapters::new(media_frame, title_frame),
        graph,
        None,
    )
}

pub(crate) fn resolve_preview_timeline_with_programs_and_observer<MediaFrame, TitleFrame>(
    request: PreviewTimelineFrameRequest<'_>,
    adapters: PreviewTimelineSourceAdapters<'_, MediaFrame, TitleFrame>,
    binding: PreviewTimelineExecutionBinding<'_>,
    preparation: Option<Box<PreviewTimelinePreparation>>,
) -> PreviewTimelineResolution
where
    MediaFrame: FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    TitleFrame: FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
{
    let cancellation = binding.cancellation;
    let graph = PreviewTimelineGraph {
        programs: binding.programs,
        scratch: binding.scratch,
        dependency_observer: Some(binding.dependency_observer),
        generation: binding.generation,
        cancellation: &cancellation,
        author_snapshot: Some(binding.author_snapshot),
        heterogeneous_graph_budget: binding.heterogeneous_graph_budget,
    };
    resolve_preview_timeline_with_graph(request, adapters, graph, preparation)
}

fn resolve_preview_timeline_with_graph<MediaFrame, TitleFrame>(
    request: PreviewTimelineFrameRequest<'_>,
    adapters: PreviewTimelineSourceAdapters<'_, MediaFrame, TitleFrame>,
    graph: PreviewTimelineGraph<'_>,
    preparation: Option<Box<PreviewTimelinePreparation>>,
) -> PreviewTimelineResolution
where
    MediaFrame: FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    TitleFrame: FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
{
    if graph.cancellation.is_canceled()
        || preparation.as_ref().is_some_and(|prepared| {
            prepared.request().generation() == graph.generation
                && (prepared.request().cancellation().is_canceled()
                    || prepared.request().cancellation().deadline()
                        != graph.cancellation.deadline())
        })
    {
        return PreviewTimelineResolution::Unavailable {
            reason: PreviewUnavailability::stopped(
                PreviewOutputStage::TimelineEvaluation,
                "Preview preparation lost its original cancellation or deadline scope",
            ),
        };
    }
    let preparation = preparation.filter(|prepared| prepared.matches(&request, graph));
    let PreviewTimelineFrameRequest {
        root_sequence: sequence,
        sequences,
        frame,
        target_resolution,
        runtime_scale,
        raster_requirement,
        color_context,
    } = request;
    let PreviewTimelineSourceAdapters { media_frame, title_frame, execute } = adapters;
    let scratch = graph.scratch;
    let mut preparation = match preparation {
        Some(prepared) => prepared,
        None => {
            let closure = match prepare_preview_frame_closure(
                graph,
                sequence,
                sequences,
                frame.max(0),
                target_resolution,
                color_context.clone(),
                runtime_scale,
                raster_requirement,
            ) {
                Ok(closure) => closure,
                Err(reason) => return PreviewTimelineResolution::Unavailable { reason },
            };
            let demands = match collect_prepared_visual_source_demands(&closure) {
                Ok(demands) => demands,
                Err(reason) => return PreviewTimelineResolution::Unavailable { reason },
            };
            Box::new(PreviewTimelinePreparation {
                closure: Arc::new(closure),
                demands,
                state: PreviewPreparationState::Sources(PreviewSourcePreparation::default()),
                author_snapshot: graph.author_snapshot,
                graph_budget: graph.heterogeneous_graph_budget,
                runtime_scale,
                raster_requirement,
            })
        }
    };
    if let PreviewPreparationState::Executing(owner) = &preparation.state {
        let work = Arc::clone(&owner.work);
        return poll_prepared_preview_work(preparation, work, scratch, execute);
    }
    let closure = &preparation.closure;
    let sampling = match closure
        .node(closure.root())
        .and_then(|node| node.materialization_contract().image_sampling(target_resolution).ok())
    {
        Some(sampling) => sampling,
        None => {
            return PreviewTimelineResolution::Unavailable {
                reason: PreviewUnavailability::failed(
                    PreviewOutputStage::TimelineEvaluation,
                    "frozen Preview owner has invalid output sampling",
                ),
            }
        }
    };
    let mut materialization_bytes = match closure.conservative_cpu_materialization_active_bytes() {
        Ok(bytes) => bytes,
        Err(error) => {
            return PreviewTimelineResolution::Unavailable {
                reason: PreviewUnavailability::blocked(
                    PreviewOutputStage::TimelineEvaluation,
                    format!("Preview visual-closure resource estimate failed closed: {error}"),
                ),
            };
        }
    };
    let mut nested_materialization_bytes =
        match closure.conservative_cpu_nested_materialization_active_bytes() {
            Ok(bytes) => bytes,
            Err(error) => {
                return PreviewTimelineResolution::Unavailable {
                    reason: PreviewUnavailability::blocked(
                        PreviewOutputStage::TimelineEvaluation,
                        error.to_string(),
                    ),
                }
            }
        };
    if let Err(error) = scratch.borrow().admit_cpu_active_working_set(
        nested_materialization_bytes,
        TimelineCpuCompositePrecision::Float32,
    ) {
        return PreviewTimelineResolution::Unavailable {
            reason: PreviewUnavailability::blocked(
                PreviewOutputStage::TimelineEvaluation,
                format!("Preview nested visual closure exceeds its CPU working-set grant: {error}"),
            ),
        };
    }
    // Source admission and readiness precede all DSP, including child nodes.
    // Otherwise a later Pending can replay an already-executed child on retry.
    let admission = admit_prepared_preview_sources(
        match &mut preparation.state {
            PreviewPreparationState::Sources(sources) => sources,
            PreviewPreparationState::Executing(_) => unreachable!("handled dispatched work"),
        },
        &preparation.demands,
        media_frame,
        title_frame,
    );
    if graph.cancellation.is_canceled() || preparation.request().cancellation().is_canceled() {
        return PreviewTimelineResolution::Unavailable {
            reason: PreviewUnavailability::stopped(
                PreviewOutputStage::TimelineEvaluation,
                "Preview preparation was canceled",
            ),
        };
    }
    let pending = match admission {
        Ok(()) => None,
        Err(PreviewTimelineAbort::Pending { dependency }) => Some(dependency),
        Err(PreviewTimelineAbort::Unavailable(reason)) => {
            return PreviewTimelineResolution::Unavailable { reason }
        }
    };
    if pending.is_none() {
        let sources = match &preparation.state {
            PreviewPreparationState::Sources(sources) => sources,
            PreviewPreparationState::Executing(_) => unreachable!("handled dispatched work"),
        };
        let bound = Arc::make_mut(&mut preparation.closure).bind_temporal_media_image_domains(
            |node, batch| preview_temporal_media_image_domain(node, batch, sources),
        );
        if let Err(error) = bound {
            return PreviewTimelineResolution::Unavailable {
                reason: PreviewUnavailability::blocked(
                    PreviewOutputStage::InputAdaptation,
                    error.to_string(),
                ),
            };
        }
        let bound = &preparation.closure;
        materialization_bytes = match bound.conservative_cpu_materialization_active_bytes() {
            Ok(bytes) => bytes,
            Err(error) => {
                return PreviewTimelineResolution::Unavailable {
                    reason: PreviewUnavailability::blocked(
                        PreviewOutputStage::TimelineEvaluation,
                        error.to_string(),
                    ),
                }
            }
        };
        nested_materialization_bytes =
            match bound.conservative_cpu_nested_materialization_active_bytes() {
                Ok(bytes) => bytes,
                Err(error) => {
                    return PreviewTimelineResolution::Unavailable {
                        reason: PreviewUnavailability::blocked(
                            PreviewOutputStage::TimelineEvaluation,
                            error.to_string(),
                        ),
                    }
                }
            };
    }
    let Some(title_bytes) = preparation.retained_title_bytes() else {
        return PreviewTimelineResolution::Unavailable {
            reason: PreviewUnavailability::blocked(
                PreviewOutputStage::GeneratedSource,
                "retained title input size overflowed",
            ),
        };
    };
    let Some(active_bytes) = nested_materialization_bytes.checked_add(title_bytes) else {
        return PreviewTimelineResolution::Unavailable {
            reason: PreviewUnavailability::blocked(
                PreviewOutputStage::GeneratedSource,
                "Preview active input size overflowed",
            ),
        };
    };
    if let Err(error) = scratch
        .borrow()
        .admit_cpu_active_working_set(active_bytes, TimelineCpuCompositePrecision::Float32)
    {
        return PreviewTimelineResolution::Unavailable {
            reason: PreviewUnavailability::blocked(
                PreviewOutputStage::GeneratedSource,
                error.to_string(),
            ),
        };
    }
    materialization_bytes = match materialization_bytes.checked_add(title_bytes) {
        Some(bytes) => bytes,
        None => {
            return PreviewTimelineResolution::Unavailable {
                reason: PreviewUnavailability::blocked(
                    PreviewOutputStage::GeneratedSource,
                    "Preview materialization size overflowed",
                ),
            }
        }
    };
    if let Some(dependency) = pending {
        return PreviewTimelineResolution::Pending { dependency, preparation };
    }
    #[cfg(test)]
    let semantic_trace = match prepared_visual_execution_semantic_trace(&preparation.closure) {
        Ok(trace) => trace,
        Err(error) => {
            return PreviewTimelineResolution::Unavailable {
                reason: PreviewUnavailability::failed(
                    PreviewOutputStage::TimelineEvaluation,
                    format!("Preview visual validation trace failed closed: {error}"),
                ),
            }
        }
    };
    let sources = match &mut preparation.state {
        PreviewPreparationState::Sources(sources) => std::mem::take(sources),
        PreviewPreparationState::Executing(_) => unreachable!("handled dispatched work"),
    };
    let mut identity =
        PreviewSemanticIdentityBuilder::new(b"mondrian.preview.prepared-execution.v1");
    uuid::Uuid::new_v4().hash(&mut identity);
    let interest =
        Arc::new(PreviewTimelineWorkInterest { cancellation: std::sync::OnceLock::new() });
    let work = Arc::new(PreparedPreviewTimelineWork {
        key: super::preview_visual_execution_task::VisualExecutionTaskKey::from_complete_semantic_fingerprint(
            identity.finish_identity().semantic_fingerprint()),
        closure: Arc::clone(&preparation.closure),
        sources,
        materialization_bytes,
        sampling,
        cache_enabled: sequence.settings.preview.cache_enabled,
        interest: Arc::downgrade(&interest),
        #[cfg(test)]
        semantic_trace,
    });
    preparation.state = PreviewPreparationState::Executing(PreviewTimelineWorkOwner {
        work: Arc::clone(&work),
        _interest: interest,
    });
    poll_prepared_preview_work(preparation, work, scratch, execute)
}

fn poll_prepared_preview_work(
    preparation: Box<PreviewTimelinePreparation>,
    work: Arc<PreparedPreviewTimelineWork>,
    scratch: &RefCell<TimelineCompositeScratch>,
    execute: Option<&mut dyn FnMut(Arc<PreparedPreviewTimelineWork>) -> PreviewTimelineWorkPoll>,
) -> PreviewTimelineResolution {
    // Ordinary root frames keep their cheap synchronous planning path. Any
    // temporal/nested DSP belongs to the existing visual worker in production.
    let needs_worker = work
        .closure
        .nodes()
        .iter()
        .any(|node| !node.evaluation().temporal_batches().is_empty())
        || work.closure.nodes().len() > 1;
    match execute.filter(|_| needs_worker) {
        Some(execute) => match execute(Arc::clone(&work)) {
            PreviewTimelineWorkPoll::Pending => PreviewTimelineResolution::Pending {
                dependency: PreviewTimelinePendingDependency::Execution { key: work.key },
                preparation,
            },
            PreviewTimelineWorkPoll::Finished(output) => output,
        },
        None => {
            let mut checkpoint = || {
                work.request()
                    .cancellation()
                    .is_canceled()
                    .then_some(mondrian_effects::EffectProcessorStopReason::Canceled)
            };
            let mut control = mondrian_effects::EffectProcessorControl::new(&mut checkpoint);
            work.execute(scratch, &mut control, None, &mut None)
        }
    }
}

#[expect(
    clippy::large_enum_variant,
    reason = "Forward the existing resolution and its leases directly through the single work poll"
)]
pub(crate) enum PreviewTimelineWorkPoll {
    Pending,
    Finished(PreviewTimelineResolution),
}

/// Immutable, source-ready work. Its owners survive producer/Broker waits;
/// execution never reopens author state or calls the initial source adapters.
pub(crate) struct PreparedPreviewTimelineWork {
    key: super::preview_visual_execution_task::VisualExecutionTaskKey,
    closure: Arc<PreparedVisualFrameClosure<PreparedTimelinePreviewEffectRoutes>>,
    sources: PreviewSourcePreparation,
    materialization_bytes: u64,
    sampling: mondrian_core::ImageSampling,
    cache_enabled: bool,
    interest: std::sync::Weak<PreviewTimelineWorkInterest>,
    #[cfg(test)]
    semantic_trace: PreparedVisualExecutionSemanticTrace,
}

impl std::fmt::Debug for PreparedPreviewTimelineWork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedPreviewTimelineWork")
            .field("key", &self.key)
            .field("generation", &self.request().generation())
            .finish()
    }
}

impl PreparedPreviewTimelineWork {
    pub(crate) fn processor_sources(
        &self,
        policy: super::preview_access_mode::PreviewMediaWorkPolicy,
    ) -> Result<super::preview_processor_sources::PreviewProcessorSourceWork, PreviewUnavailability>
    {
        let mut bindings = Vec::new();
        let mut bind = |plan: Arc<mondrian_renderer::PreparedTimelineClipInputPlan>,
                        request: PreviewTimelineMediaRequest| {
            let key = match self.sources.media.get(&request) {
                Some(PreviewTimelineMediaFrame::Ready(frame)) => frame.source_key(),
                _ => None,
            }
            .ok_or_else(|| {
                PreviewUnavailability::failed(
                    PreviewOutputStage::TimelineEvaluation,
                    "prepared processor input lost its original media source key",
                )
            })?;
            bindings.push((plan, key.clone()));
            Ok::<_, PreviewUnavailability>(())
        };
        for node in self.closure.nodes() {
            let color = node.color_context();
            let resolution = node.execution_resolution();
            for batch in node.evaluation().temporal_batches() {
                if !batch.graph().requires_processor_input_service() {
                    continue;
                }
                let input_plan = Arc::new(
                    node.program()
                        .clip_processor_input_plan(
                            batch.placement(),
                            node.evaluation().execution_request(),
                        )
                        .map_err(|error| {
                            PreviewUnavailability::failed(
                                PreviewOutputStage::TimelineEvaluation,
                                error.to_string(),
                            )
                        })?,
                );
                for demand in batch.source_demands() {
                    if let TimelineTemporalSource::Media {
                        asset_id,
                        source_sample,
                        color_space_override,
                        alpha_interpretation,
                        picture_overrides,
                        auto_tone_map,
                    } = demand.source
                    {
                        bind(
                            Arc::clone(&input_plan),
                            preview_temporal_media_request(
                                asset_id,
                                source_sample,
                                color_space_override,
                                alpha_interpretation,
                                picture_overrides,
                                auto_tone_map,
                                resolution,
                                color,
                                batch.graph().image_scale_requirement(),
                            ),
                        )?;
                    }
                }
            }
            let routes = node.evaluation().payload();
            let mut media = |media: &TimelineMediaPlan| -> Result<(), PreviewUnavailability> {
                let plan = routes
                    .processor_input_plan_for(media.placement, &media.effect_graph)
                    .map_err(|error| {
                        PreviewUnavailability::failed(
                            PreviewOutputStage::TimelineEvaluation,
                            error.to_string(),
                        )
                    })?;
                if let Some(plan) = plan {
                    let request = preview_timeline_media_request(media, resolution, color, routes)
                        .map_err(|error| {
                            PreviewUnavailability::failed(
                                PreviewOutputStage::TimelineEvaluation,
                                error.to_string(),
                            )
                        })?;
                    bind(Arc::clone(plan), request)?;
                }
                Ok(())
            };
            for element in &node.evaluation().plan().elements {
                match element {
                    TimelineRenderPlanElement::Media(input) => media(input)?,
                    TimelineRenderPlanElement::CrossDissolve(transition) => {
                        for input in [&transition.left, &transition.right] {
                            if let TimelineTransitionInputPlan::Media(input) = input {
                                media(input)?;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(
            super::preview_processor_sources::PreviewProcessorSourceWork::from_bindings(
                policy, bindings,
            ),
        )
    }

    pub(crate) fn key(&self) -> super::preview_visual_execution_task::VisualExecutionTaskKey {
        self.key
    }
    pub(crate) fn sequence_id(&self) -> SequenceId {
        self.closure
            .node(self.closure.root())
            .expect("prepared closure root")
            .sequence_id()
    }
    pub(crate) fn request(&self) -> &TimelineFrameExecutionRequest {
        self.closure
            .node(self.closure.root())
            .expect("prepared closure root")
            .evaluation()
            .execution_request()
    }
    pub(crate) fn is_active(&self) -> bool {
        self.interest.upgrade().is_some() && !self.request().cancellation().is_canceled()
    }
    pub(crate) fn interest(&self) -> std::sync::Weak<PreviewTimelineWorkInterest> {
        self.interest.clone()
    }
    pub(crate) fn bind_cancellation(
        &self,
        cancellation: super::preview_visual_execution_task::VisualExecutionCancellation,
    ) {
        if let Some(interest) = self.interest.upgrade() {
            let _ = interest.cancellation.set(cancellation);
        }
    }
    pub(crate) fn execute(
        &self,
        scratch: &RefCell<TimelineCompositeScratch>,
        control: &mut mondrian_effects::EffectProcessorControl<'_>,
        inputs: Option<&mut mondrian_renderer::TimelineProcessorInputExecution<'_>>,
        blocked: &mut Option<Arc<mondrian_effects::EffectProcessorFeedbackReceipt>>,
    ) -> PreviewTimelineResolution {
        let work = self;
        let closure = &self.closure;
        let root_node = closure.node(closure.root()).expect("prepared closure root");
        let target_resolution = root_node.execution_resolution();
        let color_context = root_node.color_context().clone();
        let sampling = self.sampling;
        let sources = &work.sources;
        let mut admitted_media =
            |demand: PreviewTimelineMediaRequest| match sources.media.get(&demand) {
                Some(PreviewTimelineMediaFrame::Ready(frame)) => {
                    PreviewTimelineMediaFrame::Ready(frame.clone())
                }
                _ => PreviewTimelineMediaFrame::Unavailable {
                    reason: PreviewUnavailability::failed(
                        PreviewOutputStage::TimelineEvaluation,
                        "prepared media execution requested an input outside its ready closure",
                    ),
                },
            };
        let mut admitted_title =
            |demand: PreviewTimelineTitleRequest| match sources.titles.get(&demand.identity()) {
                Some(PreviewTimelineTitleFrame::Ready(frame)) => {
                    PreviewTimelineTitleFrame::Ready(frame.clone())
                }
                _ => PreviewTimelineTitleFrame::Unavailable {
                    reason: PreviewUnavailability::failed(
                        PreviewOutputStage::TimelineEvaluation,
                        "prepared title execution requested an input outside its ready closure",
                    ),
                },
            };
        let mut adapter = PreviewTimelineExecutionAdapter {
            closure,
            media_frame: &mut admitted_media,
            title_frame: &mut admitted_title,
            scratch,
            control,
            processor_inputs: inputs,
            blocked,
            facts: Vec::new(),
        };
        let root = match execute_prepared_visual_closure(closure, &mut adapter) {
            Ok(root) => root,
            Err(PreparedVisualExecutionError::Adapter(PreviewTimelineAbort::Pending {
                ..
            })) => {
                return PreviewTimelineResolution::Unavailable {
                    reason: PreviewUnavailability::failed(
                        PreviewOutputStage::TimelineEvaluation,
                        "prepared graph requested an unresolved input after source readiness",
                    ),
                };
            }
            Err(PreparedVisualExecutionError::Adapter(PreviewTimelineAbort::Unavailable(
                reason,
            ))) => {
                return PreviewTimelineResolution::Unavailable { reason };
            }
            Err(PreparedVisualExecutionError::Structure(error)) => {
                return PreviewTimelineResolution::Unavailable {
                    reason: PreviewUnavailability::failed(
                        PreviewOutputStage::TimelineEvaluation,
                        format!("Preview prepared visual execution failed closed: {error}"),
                    ),
                };
            }
        };
        let PreparedPreviewVisualOutput::Root { elements, resolved_visual_identity } = root else {
            return PreviewTimelineResolution::Unavailable {
                reason: PreviewUnavailability::failed(
                    PreviewOutputStage::TimelineEvaluation,
                    "Preview prepared visual execution returned a nested frame for the root",
                ),
            };
        };
        let Some(elements) = elements else {
            return PreviewTimelineResolution::Empty;
        };
        let cache_key = viewer_preview_cache_key_for_resolved_plan(
            root_node.sequence_id(),
            target_resolution.width,
            target_resolution.height,
            &elements,
            &color_context,
            sampling,
        );
        let cache_reusable = viewer_preview_plan_allows_cross_call_reuse(&elements);
        let render_cache_identity = (cache_reusable && work.cache_enabled)
            .then_some(resolved_visual_identity)
            .flatten()
            .map(|visual| {
                TimelineRenderCacheIdentity::for_resolved_visual(
                    visual,
                    target_resolution.width,
                    target_resolution.height,
                    TimelineRenderCacheFormat::LosslessRgba32FloatZstd,
                    mondrian_core::ColorFrameAlphaFormats::ALL,
                    sampling,
                )
            });
        PreviewTimelineResolution::Ready(ResolvedPreviewTimeline {
            plan: ResolvedPreviewPlan {
                sampling,
                cpu_materialization_active_bytes: work.materialization_bytes,
                elements,
                cache_key,
                cache_reusable,
                color_context,
                render_cache_identity,
            },
            facts: adapter.facts,
            #[cfg(test)]
            semantic_trace: work.semantic_trace.clone(),
        })
    }
}

#[derive(Clone, Copy)]
struct PreviewTimelineGraph<'a> {
    programs: &'a RefCell<PreparedVisualProgramCache>,
    scratch: &'a RefCell<TimelineCompositeScratch>,
    dependency_observer:
        Option<&'a crate::app::preview_visual_dependencies::PreviewVisualDependencyObserver>,
    generation: u64,
    cancellation: &'a ExecutionCancellationToken,
    author_snapshot: Option<PreparedVisualAuthorSnapshotIdentity>,
    heterogeneous_graph_budget: EffectGraphExecutionBudget,
}

impl<'a> PreviewTimelineGraph<'a> {
    fn resolve_program(
        self,
        sequence: &Sequence,
    ) -> Result<PreparedVisualProgramBinding, PreviewUnavailability> {
        let binding = match self.author_snapshot {
            Some(author_snapshot) => self
                .programs
                .borrow_mut()
                .bind_author_snapshot(author_snapshot, sequence)
                .map_err(|error| {
                    PreviewUnavailability::blocked(
                        PreviewOutputStage::TimelineEvaluation,
                        format!(
                            "Sequence {} visual-program binding failed: {error}",
                            sequence.id
                        ),
                    )
                })?,
            None => {
                let program = self.programs.borrow_mut().prepare(sequence).map_err(|error| {
                    PreviewUnavailability::blocked(
                        PreviewOutputStage::TimelineEvaluation,
                        format!(
                            "Sequence {} visual-program preparation failed: {error}",
                            sequence.id
                        ),
                    )
                })?;
                PreparedVisualProgramBinding::checked(sequence, program).map_err(|error| {
                    PreviewUnavailability::blocked(
                        PreviewOutputStage::TimelineEvaluation,
                        error.to_string(),
                    )
                })?
            }
        };
        if let Some(observer) = self.dependency_observer {
            observer.observe(Arc::clone(binding.program()));
            if !observer.is_healthy() {
                return Err(PreviewUnavailability::blocked(
                    PreviewOutputStage::TimelineEvaluation,
                    "Preview visual dependency observer is unavailable",
                ));
            }
        }
        Ok(binding)
    }

    fn evaluate(
        self,
        program: &Arc<mondrian_renderer::PreparedVisualProgram>,
        position: FramePosition,
        target_resolution: Resolution,
        normalized_preview_resolution_scale: f32,
    ) -> Result<PreparedVisualFrameEvaluation<()>, PreviewUnavailability> {
        let extent = EffectFrameExtent::new(target_resolution.width, target_resolution.height);
        let prepared = self
            .scratch
            .borrow_mut()
            .prepare_timeline_frame_execution(
                program.as_ref(),
                TimelineFrameExecutionRequest::new(
                    TimelineEvaluationRequest::preview(
                        position,
                        normalized_preview_resolution_scale,
                    ),
                    self.generation,
                    mondrian_effects::EffectExecutionContinuity::Discontinuous,
                    extent,
                    extent.full_frame_roi(),
                    self.cancellation.clone(),
                ),
            )
            .map_err(|error| {
                PreviewUnavailability::blocked(
                    PreviewOutputStage::TimelineEvaluation,
                    format!(
                        "Sequence {} frame {} execution preparation failed closed: {error}",
                        program.sequence_id(),
                        position.frame.max(0)
                    ),
                )
            })?;
        self.scratch
            .borrow()
            .admit_cpu_active_working_set(
                prepared.source_coverage_bytes(),
                TimelineCpuCompositePrecision::Float32,
            )
            .map_err(|error| {
                PreviewUnavailability::blocked(
                    PreviewOutputStage::TimelineEvaluation,
                    format!(
                        "Sequence {} temporal source coverage exceeds the Preview CPU grant: {error}",
                        program.sequence_id()
                    ),
                )
            })?;
        Ok(PreparedVisualFrameEvaluation::new(prepared, ()))
    }
}

enum PreparedPreviewVisualOutput {
    Root {
        elements: Option<Vec<ResolvedPreviewElement>>,
        resolved_visual_identity: Option<mondrian_renderer::ResolvedVisualFrameIdentity>,
    },
    Nested(MediaPreviewFrame),
}

struct PreviewTimelineExecutionAdapter<'a, 'control, 'input, MediaFrame, TitleFrame> {
    closure: &'a PreparedVisualFrameClosure<PreparedTimelinePreviewEffectRoutes>,
    media_frame: &'a mut MediaFrame,
    title_frame: &'a mut TitleFrame,
    scratch: &'a RefCell<TimelineCompositeScratch>,
    facts: Vec<PreviewTimelineExecutionFact>,
    control: &'a mut mondrian_effects::EffectProcessorControl<'control>,
    processor_inputs: Option<&'a mut mondrian_renderer::TimelineProcessorInputExecution<'input>>,
    blocked: &'a mut Option<Arc<mondrian_effects::EffectProcessorFeedbackReceipt>>,
}

type PreviewNodeInputs<'a> = PreparedVisualExecutionNodeInputs<
    'a,
    PreparedTimelinePreviewEffectRoutes,
    PreparedPreviewVisualOutput,
>;

impl<MediaFrame, TitleFrame> PreparedVisualExecutionAdapter<PreparedTimelinePreviewEffectRoutes>
    for PreviewTimelineExecutionAdapter<'_, '_, '_, MediaFrame, TitleFrame>
where
    MediaFrame: FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    TitleFrame: FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
{
    type Output = PreparedPreviewVisualOutput;
    type Error = PreviewTimelineAbort;

    fn materialize_node(
        &mut self,
        inputs: PreviewNodeInputs<'_>,
    ) -> Result<Self::Output, Self::Error> {
        self.control.check().map_err(|_| {
            PreviewTimelineAbort::Unavailable(PreviewUnavailability::stopped(
                PreviewOutputStage::TimelineEvaluation,
                "prepared Timeline execution was stopped",
            ))
        })?;
        let node = inputs.node();
        let resolved = resolve_prepared_visual_node(self, &inputs)?;
        let resolved_visual_identity =
            super::preview_render_cache_identity::canonical_resolved_node_materialization(
                resolved.as_deref().unwrap_or_default(),
            )
            .ok()
            .and_then(|materialization| {
                mondrian_renderer::resolved_visual_frame_identity(node, materialization).ok()
            });
        if inputs.is_root() {
            return Ok(PreparedPreviewVisualOutput::Root {
                elements: resolved,
                resolved_visual_identity,
            });
        }
        let inbound = inputs.inbound_binding().ok_or_else(|| {
            PreviewTimelineAbort::Unavailable(PreviewUnavailability::failed(
                PreviewOutputStage::TimelineEvaluation,
                format!(
                    "prepared nested visual node {} has no inbound binding",
                    node.id().index()
                ),
            ))
        })?;
        let frame = materialize_prepared_nested_node(
            node.sequence_id(),
            node.author_resolution(),
            node.frame(),
            node.execution_resolution(),
            node.materialization_contract(),
            inbound.parent_working_color_space(),
            node.color_context().clone(),
            resolved.unwrap_or_default(),
            resolved_visual_identity,
            self.scratch,
            &mut self.facts,
            self.control,
            self.processor_inputs.as_deref_mut(),
            self.blocked,
        )?;
        Ok(PreparedPreviewVisualOutput::Nested(frame))
    }
}

fn prepared_visual_node<T>(
    closure: &PreparedVisualFrameClosure<T>,
    node_id: PreparedVisualFrameNodeId,
) -> Result<&PreparedVisualFrameNode<T>, PreviewTimelineAbort> {
    closure.node(node_id).ok_or_else(|| {
        PreviewTimelineAbort::Unavailable(PreviewUnavailability::failed(
            PreviewOutputStage::TimelineEvaluation,
            format!(
                "prepared visual closure references missing node {}",
                node_id.index()
            ),
        ))
    })
}

fn prepared_nested_child<T>(
    closure: &PreparedVisualFrameClosure<T>,
    parent_id: PreparedVisualFrameNodeId,
    placement: TimelineClipExecutionRef,
    sample: PreparedVisualNestedSample,
) -> Result<PreparedVisualFrameNodeId, PreviewTimelineAbort> {
    let parent = prepared_visual_node(closure, parent_id)?;
    parent.nested_child(placement, sample).ok_or_else(|| {
        PreviewTimelineAbort::Unavailable(PreviewUnavailability::failed(
            PreviewOutputStage::TimelineEvaluation,
            format!(
                "prepared visual closure has no {sample:?} child binding for Clip {}",
                placement.clip_id
            ),
        ))
    })
}

fn prepared_preview_nested_output<'a>(
    inputs: &'a PreviewNodeInputs<'_>,
    placement: TimelineClipExecutionRef,
    sample: PreparedVisualNestedSample,
) -> Result<&'a MediaPreviewFrame, PreviewTimelineAbort> {
    match inputs.nested_output(placement, sample) {
        Some(PreparedPreviewVisualOutput::Nested(frame)) => Ok(frame),
        Some(PreparedPreviewVisualOutput::Root { .. }) => Err(PreviewTimelineAbort::Unavailable(
            PreviewUnavailability::failed(
                PreviewOutputStage::TimelineEvaluation,
                format!(
                    "prepared visual child for Clip {} returned a root plan",
                    placement.clip_id
                ),
            ),
        )),
        None => Err(PreviewTimelineAbort::Unavailable(
            PreviewUnavailability::failed(
                PreviewOutputStage::TimelineEvaluation,
                format!(
                    "prepared visual child output is unavailable for Clip {} ({sample:?})",
                    placement.clip_id
                ),
            ),
        )),
    }
}

#[derive(Clone)]
struct PreparedPreviewTemporalLayer {
    frame: MediaPreviewFrame,
}

struct ResolvedPreviewTemporalSource {
    tile: EffectFrameTileF32,
    logical_resolution: Resolution,
    source_to_display_affine: [f32; 6],
    identity: PreviewSemanticIdentity,
    presentation_quality: FramePresentationQuality,
    decode_execution: PreviewDecodeExecutionSummary,
    cross_call_reusable: bool,
}

enum PreviewTemporalSourceResolution {
    Ready(ResolvedPreviewTemporalSource),
    Pending,
}

struct ReadyPreviewTemporalBatch {
    batch_index: usize,
    prepared: PreparedTemporalFrameSet,
    logical_resolution: Resolution,
    source_to_display_affine: [f32; 6],
    presentation_quality: FramePresentationQuality,
    decode_execution: PreviewDecodeExecutionSummary,
    cross_call_reusable: bool,
}

fn preview_temporal_media_image_domain(
    node: &mondrian_renderer::PreparedVisualFrameNode<PreparedTimelinePreviewEffectRoutes>,
    input: &mondrian_renderer::PreparedTimelineTemporalMediaInput,
    sources: &PreviewSourcePreparation,
) -> Result<
    (EffectFrameExtent, mondrian_core::ImageSampling),
    mondrian_renderer::TimelineTemporalPreparationError,
> {
    let media = input.media();
    let invalid =
        |reason: &str| mondrian_renderer::TimelineTemporalPreparationError::SourceSampling {
            clip_id: media.placement.clip_id,
            reason: reason.into(),
        };
    let mut selected = None;
    for (_, source) in input.source_samples() {
        let TimelineTemporalSource::Media {
            asset_id,
            source_sample,
            color_space_override,
            alpha_interpretation,
            picture_overrides,
            auto_tone_map,
        } = source
        else {
            return Err(invalid(
                "frozen Media program contains a different source kind",
            ));
        };
        let request = preview_temporal_media_request(
            *asset_id,
            *source_sample,
            *color_space_override,
            *alpha_interpretation,
            *picture_overrides,
            *auto_tone_map,
            node.execution_resolution(),
            node.color_context(),
            media.effect_graph.image_scale_requirement(),
        );
        let Some(PreviewTimelineMediaFrame::Ready(frame)) = sources.media.get(&request) else {
            return Err(invalid(
                "temporal image-domain binding requires its original Ready sources",
            ));
        };
        let domain = frame.cpu_image_domain().ok_or_else(|| {
            invalid("selected temporal source representation has no CPU working image domain")
        })?;
        if selected.replace(domain).is_some_and(|previous| previous != domain) {
            return Err(invalid(
                "temporal source image domains vary across selected samples",
            ));
        }
    }
    selected.ok_or_else(|| invalid("frozen Media program has no source image metadata"))
}

fn resolve_preview_temporal_batches<MediaFrame, TitleFrame>(
    execution: &mut PreviewTimelineExecutionAdapter<'_, '_, '_, MediaFrame, TitleFrame>,
    inputs: &PreviewNodeInputs<'_>,
    batches: &[TimelineTemporalDemandBatch],
) -> Result<HashMap<TimelineClipExecutionRef, PreparedPreviewTemporalLayer>, PreviewTimelineAbort>
where
    MediaFrame: FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    TitleFrame: FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
{
    let node_id = inputs.node().id();
    let (materialization, target_resolution, color_context) = {
        let node = prepared_visual_node(execution.closure, node_id)?;
        (
            node.materialization_contract(),
            node.execution_resolution(),
            node.color_context().clone(),
        )
    };
    let mut ready_batches = Vec::with_capacity(batches.len());
    let mut pending_sources = 0usize;
    let mut first_pending_clip = None;

    for (batch_index, batch) in batches.iter().enumerate() {
        let mut resolved = Vec::with_capacity(batch.source_demands().len());
        let mut logical_resolution = None;
        let mut source_to_display_affine = None;
        let mut presentation_quality = FramePresentationQuality::Ready;
        let mut decode_execution = PreviewDecodeExecutionSummary::default();
        let mut cross_call_reusable = batch.graph().output_cache_enabled();
        let mut identity =
            PreviewSemanticIdentityBuilder::new(b"mondrian.preview.temporal-source.v1");
        batch.placement().hash(&mut identity);
        batch.graph().semantic_fingerprint().hash(&mut identity);
        color_context.working_color_space().hash(&mut identity);
        let mut batch_pending = false;

        for demand in batch.source_demands() {
            demand.effect_request.hash(&mut identity);
            match resolve_preview_temporal_source(
                execution,
                inputs,
                materialization,
                target_resolution,
                &color_context,
                demand,
                batch.graph().image_scale_requirement(),
            )? {
                PreviewTemporalSourceResolution::Pending => {
                    batch_pending = true;
                    pending_sources = pending_sources.saturating_add(1);
                    first_pending_clip.get_or_insert(batch.placement().clip_id);
                }
                PreviewTemporalSourceResolution::Ready(source) => {
                    source.identity.hash(&mut identity);
                    if logical_resolution
                        .replace(source.logical_resolution)
                        .is_some_and(|previous| previous != source.logical_resolution)
                    {
                        return Err(PreviewTimelineAbort::Unavailable(
                            PreviewUnavailability::blocked(
                                PreviewOutputStage::TimelineEvaluation,
                                format!(
                                    "Clip {} temporal requests resolved with inconsistent logical extents",
                                    batch.placement().clip_id
                                ),
                            ),
                        ));
                    }
                    if source_to_display_affine
                        .replace(source.source_to_display_affine)
                        .is_some_and(|previous| previous != source.source_to_display_affine)
                    {
                        return Err(PreviewTimelineAbort::Unavailable(
                            PreviewUnavailability::blocked(
                                PreviewOutputStage::InputAdaptation,
                                "temporal source picture mapping varies across selected samples",
                            ),
                        ));
                    }
                    if source.presentation_quality == FramePresentationQuality::Degraded {
                        presentation_quality = FramePresentationQuality::Degraded;
                    }
                    decode_execution.accumulate(source.decode_execution);
                    cross_call_reusable &= source.cross_call_reusable;
                    resolved.push((demand.effect_request, source.tile));
                }
            }
        }
        if batch_pending {
            continue;
        }
        let source_identity = EffectTemporalSourceIdentity::from_complete_semantic_fingerprint(
            identity.finish_identity().semantic_fingerprint(),
        );
        let prepared = PreparedTemporalFrameSet::prepare(
            source_identity,
            batch.effect_demands().clone(),
            resolved,
        )
        .map_err(|error| {
            PreviewTimelineAbort::Unavailable(PreviewUnavailability::failed(
                PreviewOutputStage::TimelineComposite,
                format!("Preview temporal frame set is invalid: {error}"),
            ))
        })?;
        ready_batches.push(ReadyPreviewTemporalBatch {
            batch_index,
            prepared,
            logical_resolution: logical_resolution.unwrap_or(target_resolution),
            source_to_display_affine: source_to_display_affine
                .unwrap_or([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]),
            presentation_quality,
            decode_execution,
            cross_call_reusable,
        });
    }

    if pending_sources > 0 {
        let Some(clip_id) = first_pending_clip else {
            return Err(PreviewTimelineAbort::Unavailable(
                PreviewUnavailability::failed(
                    PreviewOutputStage::TimelineEvaluation,
                    "temporal pending accounting lost its Clip identity",
                ),
            ));
        };
        return Err(PreviewTimelineAbort::Pending {
            dependency: PreviewTimelinePendingDependency::Temporal { clip_id, pending_sources },
        });
    }

    let mut layers = HashMap::with_capacity(ready_batches.len());
    for mut ready in ready_batches {
        let batch = &batches[ready.batch_index];
        let node = inputs.node();
        let plan = batch
            .graph()
            .requires_processor_input_service()
            .then(|| {
                node.program().clip_processor_input_plan(
                    batch.placement(),
                    node.evaluation().execution_request(),
                )
            })
            .transpose()
            .map_err(|error| {
                PreviewTimelineAbort::Unavailable(PreviewUnavailability::failed(
                    PreviewOutputStage::TimelineComposite,
                    error.to_string(),
                ))
            })?;
        let mut scratch = execution.scratch.borrow_mut();
        let mut run = |control: &mut mondrian_effects::EffectProcessorControl<'_>| {
            scratch.execute_prepared_temporal_batch_with_control(
                batch,
                &mut ready.prepared,
                control,
            )
        };
        let result = if let (Some(plan), Some(inputs)) =
            (plan.as_ref(), execution.processor_inputs.as_deref_mut())
        {
            inputs
                .with_clip_inputs(
                    plan,
                    None,
                    color_context.working_color_space(),
                    execution.control,
                    run,
                )
                .map_err(|error| {
                    PreviewTimelineAbort::Unavailable(PreviewUnavailability::failed(
                        PreviewOutputStage::TimelineComposite,
                        error.to_string(),
                    ))
                })?
        } else {
            run(execution.control)
        };
        let output = result.map_err(|error| {
            if let mondrian_effects::EffectTemporalExecutionError::CustomProcessor(source) = &error
            {
                *execution.blocked = source.processor_blocked().map(Arc::clone);
            }
            PreviewTimelineAbort::Unavailable(PreviewUnavailability::failed(
                PreviewOutputStage::TimelineComposite,
                format!(
                    "Clip {} temporal Effect execution failed: {error}",
                    batch.placement().clip_id
                ),
            ))
        })?;
        let tile = output.tile();
        if tile.roi() != tile.frame_extent().full_frame_roi() {
            return Err(PreviewTimelineAbort::Unavailable(
                PreviewUnavailability::failed(
                    PreviewOutputStage::TimelineComposite,
                    "Preview temporal execution did not produce a full-frame result",
                ),
            ));
        }
        let frame = MediaPreviewFrame::from_working(
            CpuColorFrame::working(WorkingRgbaF32Frame {
                alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
                sampling: tile.sampling(),
                width: tile.frame_extent().width(),
                height: tile.frame_extent().height(),
                data: tile.pixels().to_vec(),
                color_space: color_context.working_color_space(),
            }),
            ready.logical_resolution,
            PreviewSemanticIdentity::from_complete_fingerprint(output.cache_identity()),
            ready.presentation_quality,
            ready.decode_execution,
        )
        .with_cross_call_reuse(ready.cross_call_reusable)
        .with_source_to_display_affine(ready.source_to_display_affine);
        if layers
            .insert(batch.placement(), PreparedPreviewTemporalLayer { frame })
            .is_some()
        {
            return Err(PreviewTimelineAbort::Unavailable(
                PreviewUnavailability::failed(
                    PreviewOutputStage::TimelineEvaluation,
                    format!(
                        "Clip {} produced more than one temporal execution batch",
                        batch.placement().clip_id
                    ),
                ),
            ));
        }
    }
    Ok(layers)
}

fn resolve_preview_temporal_source<MediaFrame, TitleFrame>(
    execution: &mut PreviewTimelineExecutionAdapter<'_, '_, '_, MediaFrame, TitleFrame>,
    inputs: &PreviewNodeInputs<'_>,
    materialization: PreparedVisualMaterializationContract,
    target_resolution: Resolution,
    color_context: &ProgramColorContext,
    demand: &TimelineTemporalSourceDemand,
    image_scale_requirement: mondrian_effects::EffectImageScaleRequirement,
) -> Result<PreviewTemporalSourceResolution, PreviewTimelineAbort>
where
    MediaFrame: FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    TitleFrame: FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
{
    match &demand.source {
        TimelineTemporalSource::Media {
            asset_id,
            source_sample,
            color_space_override,
            alpha_interpretation,
            picture_overrides,
            auto_tone_map,
        } => {
            let request = preview_temporal_media_request(
                *asset_id,
                *source_sample,
                *color_space_override,
                *alpha_interpretation,
                *picture_overrides,
                *auto_tone_map,
                target_resolution,
                color_context,
                image_scale_requirement,
            );
            let frame = match (execution.media_frame)(request) {
                PreviewTimelineMediaFrame::Ready(frame) => frame,
                PreviewTimelineMediaFrame::Pending { .. } => {
                    return Ok(PreviewTemporalSourceResolution::Pending);
                }
                PreviewTimelineMediaFrame::Unavailable { reason } => {
                    return Err(PreviewTimelineAbort::Unavailable(
                        PreviewMediaSourceRole::Temporal {
                            clip_id: demand.placement.clip_id,
                            time: demand.effect_request.time(),
                        }
                        .failure(reason, *asset_id),
                    ));
                }
            };
            preview_temporal_source_from_media_frame(
                execution,
                demand,
                frame,
                color_context.working_color_space(),
            )
            .map(PreviewTemporalSourceResolution::Ready)
        }
        TimelineTemporalSource::NestedSequence { sequence_id, source_sample, .. } => {
            let child_id = prepared_nested_child(
                execution.closure,
                inputs.node().id(),
                demand.placement,
                PreparedVisualNestedSample::Temporal(demand.effect_request),
            )?;
            let child_node = prepared_visual_node(execution.closure, child_id)?;
            if child_node.sequence_id() != *sequence_id {
                return Err(PreviewTimelineAbort::Unavailable(
                    PreviewUnavailability::failed(
                        PreviewOutputStage::TimelineEvaluation,
                        format!(
                            "temporal nested binding expected Sequence {sequence_id} but closure resolved {}",
                            child_node.sequence_id()
                        ),
                    ),
                ));
            }
            let expected = demand.effect_request.frame_extent();
            if child_node.execution_resolution().width != expected.width()
                || child_node.execution_resolution().height != expected.height()
            {
                return Err(PreviewTimelineAbort::Unavailable(
                    PreviewUnavailability::blocked(
                        PreviewOutputStage::TimelineEvaluation,
                        format!(
                            "nested Sequence {sequence_id} temporal Preview extent {}x{} differs from admitted Effect extent {}x{}",
                            child_node.execution_resolution().width,
                            child_node.execution_resolution().height,
                            expected.width(),
                            expected.height()
                        ),
                    ),
                ));
            }
            let projection_is_valid = match source_sample.boundary() {
                mondrian_core::SourceSamplingBoundary::Covering => {
                    child_node.time() <= source_sample.time()
                }
                mondrian_core::SourceSamplingBoundary::StrictPredecessor => {
                    child_node.time() < source_sample.time()
                }
            };
            if !projection_is_valid {
                return Err(PreviewTimelineAbort::Unavailable(
                    PreviewUnavailability::failed(
                        PreviewOutputStage::TimelineEvaluation,
                        format!(
                            "temporal nested closure projected Sequence {sequence_id} outside requested source sample {source_sample:?}"
                        ),
                    ),
                ));
            }
            let frame = prepared_preview_nested_output(
                inputs,
                demand.placement,
                PreparedVisualNestedSample::Temporal(demand.effect_request),
            )?
            .clone();
            preview_temporal_source_from_media_frame(
                execution,
                demand,
                frame,
                color_context.working_color_space(),
            )
            .map(PreviewTemporalSourceResolution::Ready)
        }
        TimelineTemporalSource::SolidColor { color } => {
            let extent = demand.effect_request.frame_extent();
            let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
                alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
                sampling: materialization
                    .image_sampling(Resolution { width: extent.width(), height: extent.height() })
                    .map_err(|error| {
                        PreviewTimelineAbort::Unavailable(PreviewUnavailability::failed(
                            PreviewOutputStage::GeneratedSource,
                            error.to_string(),
                        ))
                    })?,
                width: extent.width(),
                height: extent.height(),
                data: vec![
                    [color.r, color.g, color.b, color.a];
                    extent.width() as usize * extent.height() as usize
                ],
                color_space: color_context.working_color_space(),
            });
            let mut identity =
                PreviewSemanticIdentityBuilder::new(b"mondrian.preview.temporal-solid.v1");
            demand.placement.hash(&mut identity);
            demand.effect_request.hash(&mut identity);
            for component in [color.r, color.g, color.b, color.a] {
                component.to_bits().hash(&mut identity);
            }
            Ok(PreviewTemporalSourceResolution::Ready(
                ResolvedPreviewTemporalSource {
                    tile: preview_temporal_tile(&frame, demand)?,
                    logical_resolution: materialization.author_resolution(),
                    source_to_display_affine: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    identity: identity.finish_identity(),
                    presentation_quality: FramePresentationQuality::Ready,
                    decode_execution: PreviewDecodeExecutionSummary::default(),
                    cross_call_reusable: true,
                },
            ))
        }
    }
}

fn preview_temporal_source_from_media_frame<MediaFrame, TitleFrame>(
    execution: &mut PreviewTimelineExecutionAdapter<'_, '_, '_, MediaFrame, TitleFrame>,
    demand: &TimelineTemporalSourceDemand,
    frame: MediaPreviewFrame,
    working_color_space: mondrian_core::WorkingColorSpace,
) -> Result<ResolvedPreviewTemporalSource, PreviewTimelineAbort> {
    let identity = frame.identity();
    let logical_resolution = frame.logical_resolution();
    let source_to_display_affine = frame.source_to_display_affine();
    let presentation_quality = frame.presentation_quality();
    let decode_execution = frame.decode_execution();
    let cross_call_reusable = frame.permits_cross_call_reuse();
    let working = {
        let mut scratch = execution.scratch.borrow_mut();
        frame
            .working_frame_with_session(scratch.color_execution_mut())
            .map_err(|error| {
                PreviewTimelineAbort::Unavailable(
                    PreviewCpuExecutionError::WorkingFrame(error).unavailability(),
                )
            })?
    };
    if let Some(diagnostics) = working.color_diagnostics {
        execution.facts.push(PreviewTimelineExecutionFact::ColorTransform(diagnostics));
    }
    execution.facts.push(PreviewTimelineExecutionFact::ColorStage(
        working.stage_diagnostics,
    ));
    let descriptor = working.frame.descriptor();
    let expected = demand.effect_request.frame_extent();
    if descriptor.width != expected.width()
        || descriptor.height != expected.height()
        || descriptor.color_space.working() != Some(working_color_space)
    {
        return Err(PreviewTimelineAbort::Unavailable(
            PreviewUnavailability::blocked(
                PreviewOutputStage::InputAdaptation,
                format!(
                    "Clip {} temporal source does not satisfy {}x{} straight-alpha working input",
                    demand.placement.clip_id,
                    expected.width(),
                    expected.height()
                ),
            ),
        ));
    }
    Ok(ResolvedPreviewTemporalSource {
        tile: preview_temporal_tile(&working.frame, demand)?,
        logical_resolution,
        source_to_display_affine,
        identity,
        presentation_quality,
        decode_execution,
        cross_call_reusable,
    })
}

fn preview_temporal_tile(
    frame: &CpuColorFrame,
    demand: &TimelineTemporalSourceDemand,
) -> Result<EffectFrameTileF32, PreviewTimelineAbort> {
    let roi = demand.effect_request.input_roi().region();
    let source = frame.rgba_f32();
    let mut pixels = Vec::with_capacity(roi.width() as usize * roi.height() as usize);
    for y in roi.y()..roi.y().saturating_add(roi.height()) {
        let start = y as usize * source.width as usize + roi.x() as usize;
        let end = start.saturating_add(roi.width() as usize);
        let row = source.data.get(start..end).ok_or_else(|| {
            PreviewTimelineAbort::Unavailable(PreviewUnavailability::failed(
                PreviewOutputStage::InputAdaptation,
                "temporal ROI exceeded its resolved Preview source frame",
            ))
        })?;
        for sample in row {
            pixels.push(
                mondrian_effects::convert_alpha_sample(
                    *sample,
                    source.alpha,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                )
                .map_err(|error| {
                    PreviewTimelineAbort::Unavailable(PreviewUnavailability::failed(
                        PreviewOutputStage::InputAdaptation,
                        format!("temporal Coverage source materialization refused: {error}"),
                    ))
                })?,
            );
        }
    }
    EffectFrameTileF32::new(
        demand.effect_request.time(),
        demand.effect_request.frame_extent(),
        source.sampling,
        roi,
        demand.effect_request.time().numerator(),
        pixels,
    )
    .map_err(|error| {
        PreviewTimelineAbort::Unavailable(PreviewUnavailability::failed(
            PreviewOutputStage::InputAdaptation,
            format!("Preview temporal source tile is invalid: {error}"),
        ))
    })
}

fn prepare_preview_frame_closure(
    graph: PreviewTimelineGraph<'_>,
    root_sequence: &Sequence,
    sequences: &[Sequence],
    frame: i64,
    root_resolution: Resolution,
    root_color_context: ProgramColorContext,
    runtime_scale: PreviewResolutionScale,
    raster_requirement: PreviewRasterRequirement,
) -> Result<PreparedVisualFrameClosure<PreparedTimelinePreviewEffectRoutes>, PreviewUnavailability>
{
    let child_canvas_policy = match raster_requirement {
        PreviewRasterRequirement::CanonicalProgram => PreparedVisualChildCanvasPolicy::Authored,
        PreviewRasterRequirement::Viewer => {
            PreparedVisualChildCanvasPolicy::preview_scaled(runtime_scale.dimension_divisor())
                .map_err(|error| {
                    PreviewUnavailability::blocked(
                        PreviewOutputStage::TimelineEvaluation,
                        error.to_string(),
                    )
                })?
        }
    };
    let closure = prepare_bound_visual_frame_closure(
        PreparedVisualFrameClosureRequest {
            root_sequence,
            sequences,
            root_position: FramePosition::new(frame, root_sequence.time_base()),
            root_resolution,
            root_color_context,
            child_canvas_policy,
        },
        |sequence| graph.resolve_program(sequence).map_err(|error| error.to_string()),
        |program, frame, resolution, _color_context, normalized_preview_resolution_scale| {
            graph
                .evaluate(
                    program,
                    frame,
                    resolution,
                    normalized_preview_resolution_scale,
                )
                .map_err(|error| error.to_string())
        },
    )
    .map_err(|error| {
        PreviewUnavailability::blocked(PreviewOutputStage::TimelineEvaluation, error.to_string())
    })?;
    let mut scratch = graph.scratch.borrow_mut();
    let closure = closure
        .try_map_payload(|node, closure| {
            // Temporal extraction rewrites the current Clip graph. Inspect the
            // retained original graph before any source demand can be materialized.
            for batch in node.evaluation().temporal_batches() {
                if batch.graph().image_scale_requirement()
                    == mondrian_effects::EffectImageScaleRequirement::Unit
                {
                    return Err(
                        TimelinePreviewEffectRouteError::UnitScaleTemporalUnsupported {
                            placement: batch.placement(),
                        },
                    );
                }
            }
            let node_resolution = node.execution_resolution();
            PreparedTimelinePreviewEffectRoutes::prepare(
                node.evaluation().plan(),
                EffectFrameExtent::new(node_resolution.width, node_resolution.height),
                graph.heterogeneous_graph_budget,
                &mut scratch,
                |placement| {
                    let child =
                        node.nested_child(placement, PreparedVisualNestedSample::Current)?;
                    let child_resolution = closure.node(child)?.execution_resolution();
                    Some(EffectFrameExtent::new(
                        child_resolution.width,
                        child_resolution.height,
                    ))
                },
            )
            .and_then(|routes| {
                routes.bind_clip_processor_inputs(
                    node.program(),
                    node.evaluation().execution_request(),
                )
            })
        })
        .map_err(|error| {
            PreviewUnavailability::blocked(
                PreviewOutputStage::TimelineEvaluation,
                error.to_string(),
            )
        })?;
    drop(scratch);
    let root = closure.root();
    for node in closure.nodes() {
        let admission = if node.id() == root {
            node.evaluation().payload().admit_root()
        } else {
            node.evaluation().payload().admit_nested_materialization()
        };
        admission.map_err(|error| {
            PreviewUnavailability::blocked(
                PreviewOutputStage::TimelineEvaluation,
                format!(
                    "Sequence {} frame {} Effect route admission failed: {error}",
                    node.sequence_id(),
                    node.frame()
                ),
            )
        })?;
    }
    Ok(closure)
}

enum PreparedPreviewSourceDemand {
    Media {
        request: PreviewTimelineMediaRequest,
        role: PreviewMediaSourceRole,
    },
    BasicTitle(PreviewTimelineTitleRequest),
}

#[derive(Clone, Copy)]
enum PreviewMediaSourceRole {
    Current,
    Transition,
    Temporal {
        clip_id: mondrian_core::ClipId,
        time: mondrian_core::TimelineTime,
    },
}

impl PreviewMediaSourceRole {
    fn failure(self, reason: PreviewUnavailability, asset_id: AssetId) -> PreviewUnavailability {
        match self {
            Self::Current => reason.with_context(format_args!("media {asset_id}")),
            Self::Transition => reason.with_context(format_args!("Transition media {asset_id}")),
            Self::Temporal { clip_id, time } => reason.with_context(format_args!(
                "temporal media source for Clip {clip_id} at {time}"
            )),
        }
    }
}

/// One original attempt retained by the evaluation working set. Sources may
/// resume producer waits before DSP; Executing only polls the same owned work
/// and never replays a partially executed graph.
pub(crate) struct PreviewTimelinePreparation {
    closure: Arc<PreparedVisualFrameClosure<PreparedTimelinePreviewEffectRoutes>>,
    demands: Vec<PreparedPreviewSourceDemand>,
    state: PreviewPreparationState,
    author_snapshot: Option<PreparedVisualAuthorSnapshotIdentity>,
    graph_budget: EffectGraphExecutionBudget,
    runtime_scale: PreviewResolutionScale,
    raster_requirement: PreviewRasterRequirement,
}

/// Only the evaluation owns this interest. Worker/result payloads hold Weak,
/// so neither a queued payload nor its Broker handle forms an ownership cycle.
#[derive(Debug)]
pub(crate) struct PreviewTimelineWorkInterest {
    cancellation:
        std::sync::OnceLock<super::preview_visual_execution_task::VisualExecutionCancellation>,
}
impl Drop for PreviewTimelineWorkInterest {
    fn drop(&mut self) {
        if let Some(cancellation) = self.cancellation.get() {
            cancellation.cancel();
        }
    }
}
struct PreviewTimelineWorkOwner {
    work: Arc<PreparedPreviewTimelineWork>,
    _interest: Arc<PreviewTimelineWorkInterest>,
}

enum PreviewPreparationState {
    Sources(PreviewSourcePreparation),
    Executing(PreviewTimelineWorkOwner),
}

impl PreviewTimelinePreparation {
    pub(crate) fn retained_title_bytes(&self) -> Option<u64> {
        match &self.state {
            PreviewPreparationState::Sources(sources) => sources.title_bytes(),
            PreviewPreparationState::Executing(owner) => owner.work.sources.title_bytes(),
        }
    }
    pub(crate) fn request(&self) -> &TimelineFrameExecutionRequest {
        self.closure
            .node(self.closure.root())
            .expect("prepared closure root")
            .evaluation()
            .execution_request()
    }

    pub(crate) fn compatible_scope(
        &self,
        generation: u64,
        author_snapshot: Option<PreparedVisualAuthorSnapshotIdentity>,
        budget: EffectGraphExecutionBudget,
    ) -> bool {
        self.request().generation() == generation
            && !self.request().cancellation().is_canceled()
            && self.author_snapshot == author_snapshot
            && self.graph_budget == budget
    }

    fn matches(
        &self,
        request: &PreviewTimelineFrameRequest<'_>,
        graph: PreviewTimelineGraph<'_>,
    ) -> bool {
        let root = self.closure.node(self.closure.root()).expect("prepared closure root");
        if !self.compatible_scope(
            graph.generation,
            graph.author_snapshot,
            graph.heterogeneous_graph_budget,
        ) || root.sequence_id() != request.root_sequence.id
            || root.sequence_revision() != request.root_sequence.revision
            || root.position()
                != FramePosition::new(request.frame.max(0), request.root_sequence.time_base())
            || root.execution_resolution() != request.target_resolution
            || root.color_context() != &request.color_context
            || self.runtime_scale != request.runtime_scale
            || self.raster_requirement != request.raster_requirement
        {
            return false;
        }
        // Production has a trusted immutable author snapshot. Validation callers
        // without it must still detect in-place edits to any frozen child.
        graph.author_snapshot.is_some()
            || self.closure.nodes().iter().all(|node| {
                let sequence = if node.sequence_id() == request.root_sequence.id {
                    Some(request.root_sequence)
                } else {
                    request.sequences.iter().find(|sequence| sequence.id == node.sequence_id())
                };
                sequence.is_some_and(|sequence| {
                    graph.resolve_program(sequence).is_ok_and(|binding| {
                        binding.program().visual_author_fingerprint()
                            == node.program().visual_author_fingerprint()
                            && binding.program().effect_registry_revision()
                                == node.program().effect_registry_revision()
                    })
                })
            })
    }
}

/// Exact input results within this attempt; the original producers remain the
/// cache authority. Full Ready values survive retries and retain their leases.
#[derive(Default)]
struct PreviewSourcePreparation {
    media: HashMap<PreviewTimelineMediaRequest, PreviewTimelineMediaFrame>,
    titles: HashMap<BasicTitleRasterRequestIdentity, PreviewTimelineTitleFrame>,
}

impl PreviewSourcePreparation {
    fn title_bytes(&self) -> Option<u64> {
        self.titles.values().try_fold(0_u64, |total, value| match value {
            PreviewTimelineTitleFrame::Ready(frame) => {
                total.checked_add(u64::try_from(frame.retained_bytes()).ok()?)
            }
            _ => Some(total),
        })
    }
}

fn admit_prepared_preview_sources(
    sources: &mut PreviewSourcePreparation,
    demands: &[PreparedPreviewSourceDemand],
    media_frame: &mut impl FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    title_frame: &mut impl FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
) -> Result<(), PreviewTimelineAbort> {
    let PreviewSourcePreparation { media, titles } = sources;
    let mut pending = None;
    let mut retry_admission = None;
    let mut unavailable = None;
    let mut temporal_pending = 0usize;
    let mut queried_media = HashSet::new();
    let mut queried_titles = HashSet::new();
    // Admit the whole source batch even if an earlier result waits or fails.
    // This lets the original media/raster producers make progress together.
    for demand in demands {
        match demand {
            PreparedPreviewSourceDemand::Media { request, role } => {
                let role = *role;
                let asset_id = request.asset_id;
                let first_query = queried_media.insert(request);
                let response = match media.entry(request.clone()) {
                    Entry::Occupied(mut entry) => {
                        if first_query
                            && !matches!(entry.get(), PreviewTimelineMediaFrame::Ready(frame)
                            if frame.presentation_quality() == FramePresentationQuality::Ready)
                        {
                            entry.insert(media_frame(request.clone()));
                        }
                        entry.into_mut()
                    }
                    Entry::Vacant(entry) => entry.insert(media_frame(request.clone())),
                };
                match response {
                    PreviewTimelineMediaFrame::Ready(_) => {}
                    PreviewTimelineMediaFrame::Pending { wait } => {
                        let dependency = match role {
                            PreviewMediaSourceRole::Temporal { clip_id, .. } => {
                                temporal_pending = temporal_pending.saturating_add(1);
                                PreviewTimelinePendingDependency::Temporal {
                                    clip_id,
                                    pending_sources: 0,
                                }
                            }
                            PreviewMediaSourceRole::Current
                            | PreviewMediaSourceRole::Transition => {
                                PreviewTimelinePendingDependency::Media { asset_id, wait: *wait }
                            }
                        };
                        pending.get_or_insert(dependency);
                        if *wait == PreviewTimelineMediaWait::RetryAdmission {
                            // A temporal request without a producer must also retry;
                            // waiting for producer publication would suppress admission.
                            retry_admission.get_or_insert(
                                PreviewTimelinePendingDependency::Media { asset_id, wait: *wait },
                            );
                        }
                    }
                    PreviewTimelineMediaFrame::Unavailable { reason } => {
                        unavailable.get_or_insert_with(|| role.failure(reason.clone(), asset_id));
                    }
                }
            }
            PreparedPreviewSourceDemand::BasicTitle(request) => {
                let identity = request.identity();
                let first_query = queried_titles.insert(identity);
                let response = match titles.entry(identity) {
                    Entry::Occupied(mut entry) => {
                        if first_query
                            && !matches!(entry.get(), PreviewTimelineTitleFrame::Ready(_))
                        {
                            entry.insert(title_frame(request.clone()));
                        }
                        entry.into_mut()
                    }
                    Entry::Vacant(entry) => entry.insert(title_frame(request.clone())),
                };
                match response {
                    PreviewTimelineTitleFrame::Ready(_) => {}
                    PreviewTimelineTitleFrame::Pending => {
                        pending
                            .get_or_insert(PreviewTimelinePendingDependency::BasicTitle(identity));
                    }
                    PreviewTimelineTitleFrame::Unavailable { reason } => {
                        unavailable.get_or_insert_with(|| {
                            reason
                                .clone()
                                .with_context(format_args!("Basic Title raster request {identity}"))
                        });
                    }
                }
            }
        }
    }
    if let Some(reason) = unavailable {
        return Err(PreviewTimelineAbort::Unavailable(reason));
    }
    if let Some(mut dependency) = retry_admission.or(pending) {
        if let PreviewTimelinePendingDependency::Temporal { pending_sources, .. } = &mut dependency
        {
            *pending_sources = temporal_pending;
        }
        return Err(PreviewTimelineAbort::Pending { dependency });
    }
    Ok(())
}

fn collect_prepared_visual_source_demands(
    closure: &PreparedVisualFrameClosure<PreparedTimelinePreviewEffectRoutes>,
) -> Result<Vec<PreparedPreviewSourceDemand>, PreviewUnavailability> {
    let mut demands = Vec::new();
    for node in closure.nodes() {
        let color_context = node.color_context();
        let target_resolution = node.execution_resolution();
        let temporal_placements = node
            .evaluation()
            .temporal_batches()
            .iter()
            .map(TimelineTemporalDemandBatch::placement)
            .chain(
                node.evaluation()
                    .temporal_media_inputs()
                    .iter()
                    .map(|input| input.media().placement),
            )
            .collect::<HashSet<_>>();
        // Time discovery is image-domain independent. Admit the complete
        // original source wave now, including signed history and lookahead.
        for input in node.evaluation().temporal_media_inputs() {
            for (time, source) in input.source_samples() {
                let TimelineTemporalSource::Media {
                    asset_id,
                    source_sample,
                    color_space_override,
                    alpha_interpretation,
                    picture_overrides,
                    auto_tone_map,
                } = source
                else {
                    continue;
                };
                demands.push(PreparedPreviewSourceDemand::Media {
                    request: preview_temporal_media_request(
                        *asset_id,
                        *source_sample,
                        *color_space_override,
                        *alpha_interpretation,
                        *picture_overrides,
                        *auto_tone_map,
                        target_resolution,
                        color_context,
                        input.media().effect_graph.image_scale_requirement(),
                    ),
                    role: PreviewMediaSourceRole::Temporal {
                        clip_id: input.media().placement.clip_id,
                        time,
                    },
                });
            }
        }
        for element in &node.evaluation().plan().elements {
            match element {
                TimelineRenderPlanElement::Media(media) => {
                    if !temporal_placements.contains(&media.placement) {
                        demands.push(PreparedPreviewSourceDemand::Media {
                            request: preview_timeline_media_request(
                                media,
                                target_resolution,
                                color_context,
                                node.evaluation().payload(),
                            )
                            .map_err(preview_route_unavailability)?,
                            role: PreviewMediaSourceRole::Current,
                        });
                    }
                }
                TimelineRenderPlanElement::NestedSequence(_) => {}
                TimelineRenderPlanElement::BasicTitle(title) => {
                    demands.push(PreparedPreviewSourceDemand::BasicTitle(
                        preview_timeline_title_request(
                            node.materialization_contract(),
                            title,
                            target_resolution,
                            color_context.working_color_space(),
                        ),
                    ));
                }
                TimelineRenderPlanElement::SolidColor(_)
                | TimelineRenderPlanElement::Adjustment(_)
                | TimelineRenderPlanElement::TimelineGrade(_) => {}
                TimelineRenderPlanElement::CrossDissolve(transition) => {
                    collect_prepared_transition_input_source_demand(
                        &transition.left,
                        node.materialization_contract(),
                        target_resolution,
                        color_context,
                        node.evaluation().payload(),
                        &temporal_placements,
                        &mut demands,
                    )?;
                    collect_prepared_transition_input_source_demand(
                        &transition.right,
                        node.materialization_contract(),
                        target_resolution,
                        color_context,
                        node.evaluation().payload(),
                        &temporal_placements,
                        &mut demands,
                    )?;
                }
            }
        }
    }
    Ok(demands)
}

fn standalone_preview_heterogeneous_graph_budget() -> EffectGraphExecutionBudget {
    EffectGraphExecutionBudget::new(
        64 * 1024 * 1024,
        64 * 1024 * 1024,
        64 * 1024 * 1024,
        64,
        192,
    )
}

fn resolve_prepared_visual_node<MediaFrame, TitleFrame>(
    execution: &mut PreviewTimelineExecutionAdapter<'_, '_, '_, MediaFrame, TitleFrame>,
    inputs: &PreviewNodeInputs<'_>,
) -> Result<Option<Vec<ResolvedPreviewElement>>, PreviewTimelineAbort>
where
    MediaFrame: FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    TitleFrame: FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
{
    let node_id = inputs.node().id();
    let (materialization, target_resolution, color_context, elements, temporal_batches, routes) = {
        let node = prepared_visual_node(execution.closure, node_id)?;
        (
            node.materialization_contract(),
            node.execution_resolution(),
            node.color_context().clone(),
            node.evaluation().plan().elements.clone(),
            node.evaluation().temporal_batches().to_vec(),
            node.evaluation().payload().clone(),
        )
    };
    let author_resolution = materialization.author_resolution();
    if elements.is_empty() {
        return Ok(None);
    }
    let temporal_layers = resolve_preview_temporal_batches(execution, inputs, &temporal_batches)?;

    let mut resolved = Vec::with_capacity(elements.len());
    for element in elements {
        match element {
            TimelineRenderPlanElement::SolidColor(solid) => {
                if let Some(temporal) = temporal_layers.get(&solid.placement) {
                    let transform = project_preview_media_transform(
                        solid.transform,
                        &temporal.frame,
                        author_resolution,
                        target_resolution,
                    )
                    .ok_or_else(|| {
                        PreviewTimelineAbort::Unavailable(PreviewUnavailability::blocked(
                            PreviewOutputStage::TimelineEvaluation,
                            format!(
                                "temporal Solid Color {} has invalid Preview transform geometry",
                                solid.placement.clip_id
                            ),
                        ))
                    })?;
                    resolved.push(ResolvedPreviewElement::Media {
                        frame: temporal.frame.clone(),
                        opacity: solid.opacity,
                        blend_mode: solid.blend_mode,
                        transform,
                        prepared_effect_route: prepared_preview_effect_route(
                            &routes,
                            solid.placement,
                            &solid.effect_graph,
                        )?,
                        effect_graph: solid.effect_graph,
                        frame_seed: solid.frame_seed,
                    });
                } else {
                    let prepared_route = prepared_preview_heterogeneous_route(
                        &routes,
                        solid.placement,
                        &solid.effect_graph,
                    )?;
                    let layer = TimelineSolidColorLayer {
                        processor_input_plan: prepared_preview_processor_input_plan(
                            &routes,
                            solid.placement,
                            &solid.effect_graph,
                        )?,
                        color: solid.color,
                        opacity: solid.opacity,
                        blend_mode: solid.blend_mode,
                        transform: solid.transform,
                        effect_graph: solid.effect_graph,
                        frame_seed: solid.frame_seed,
                    };
                    resolved.push(match prepared_route {
                        Some(prepared_route) => ResolvedPreviewElement::HeterogeneousSolidColor {
                            layer,
                            prepared_route,
                        },
                        None => ResolvedPreviewElement::SolidColor(layer),
                    });
                }
            }
            TimelineRenderPlanElement::Media(media) => {
                let asset_id = media.asset_id;
                let frame = if let Some(temporal) = temporal_layers.get(&media.placement) {
                    temporal.frame.clone()
                } else {
                    let request = preview_timeline_media_request(
                        &media,
                        target_resolution,
                        &color_context,
                        &routes,
                    )
                    .map_err(preview_route_abort)?;
                    match (execution.media_frame)(request) {
                        PreviewTimelineMediaFrame::Ready(frame) => frame,
                        PreviewTimelineMediaFrame::Pending { wait } => {
                            return Err(PreviewTimelineAbort::Pending {
                                dependency: PreviewTimelinePendingDependency::Media {
                                    asset_id,
                                    wait,
                                },
                            });
                        }
                        PreviewTimelineMediaFrame::Unavailable { reason } => {
                            return Err(PreviewTimelineAbort::Unavailable(
                                PreviewMediaSourceRole::Current.failure(reason, asset_id),
                            ));
                        }
                    }
                };
                let transform = project_preview_media_transform(
                    media.transform,
                    &frame,
                    author_resolution,
                    target_resolution,
                )
                .ok_or_else(|| {
                    PreviewTimelineAbort::Unavailable(PreviewUnavailability::blocked(
                        PreviewOutputStage::TimelineEvaluation,
                        format!("media {asset_id} has invalid Preview transform geometry"),
                    ))
                })?;
                resolved.push(ResolvedPreviewElement::Media {
                    frame,
                    opacity: media.opacity,
                    blend_mode: media.blend_mode,
                    transform,
                    prepared_effect_route: prepared_preview_effect_route(
                        &routes,
                        media.placement,
                        &media.effect_graph,
                    )?,
                    effect_graph: media.effect_graph,
                    frame_seed: media.frame_seed,
                });
            }
            TimelineRenderPlanElement::BasicTitle(title) => {
                let (frame, transform) = resolve_basic_title_frame(
                    execution,
                    materialization,
                    &title,
                    target_resolution,
                    color_context.working_color_space(),
                )?;
                resolved.push(ResolvedPreviewElement::Media {
                    frame,
                    opacity: title.opacity,
                    blend_mode: title.blend_mode,
                    transform,
                    prepared_effect_route: prepared_preview_effect_route(
                        &routes,
                        title.placement,
                        &title.effect_graph,
                    )?,
                    effect_graph: title.effect_graph,
                    frame_seed: title.frame_seed,
                });
            }
            TimelineRenderPlanElement::Adjustment(adjustment) => {
                resolved.push(ResolvedPreviewElement::Adjustment(
                    TimelineAdjustmentLayer {
                        effect_graph: adjustment.effect_graph,
                        opacity: adjustment.opacity,
                        blend_policy: adjustment.blend_policy,
                        frame_seed: adjustment.frame_seed,
                    },
                ));
            }
            TimelineRenderPlanElement::TimelineGrade(grade) => {
                resolved.push(ResolvedPreviewElement::Adjustment(
                    TimelineAdjustmentLayer {
                        effect_graph: grade.effect_graph,
                        opacity: 1.0,
                        blend_policy: mondrian_effects::EffectPassBlendPolicy::Mix,
                        frame_seed: grade.frame_seed,
                    },
                ));
            }
            TimelineRenderPlanElement::NestedSequence(nested) => {
                let frame = if let Some(temporal) = temporal_layers.get(&nested.placement) {
                    temporal.frame.clone()
                } else {
                    prepared_preview_nested_output(
                        inputs,
                        nested.placement,
                        PreparedVisualNestedSample::Current,
                    )?
                    .clone()
                };
                let transform = project_preview_media_transform(
                    nested.transform,
                    &frame,
                    author_resolution,
                    target_resolution,
                )
                .ok_or_else(|| {
                    PreviewTimelineAbort::Unavailable(PreviewUnavailability::blocked(
                        PreviewOutputStage::TimelineEvaluation,
                        format!(
                            "nested Sequence {} has invalid Preview transform geometry",
                            nested.sequence_id
                        ),
                    ))
                })?;
                resolved.push(ResolvedPreviewElement::Media {
                    frame,
                    opacity: nested.opacity,
                    blend_mode: nested.blend_mode,
                    transform,
                    prepared_effect_route: prepared_preview_effect_route(
                        &routes,
                        nested.placement,
                        &nested.effect_graph,
                    )?,
                    effect_graph: nested.effect_graph,
                    frame_seed: nested.frame_seed,
                });
            }
            TimelineRenderPlanElement::CrossDissolve(transition) => {
                let left = resolve_transition_input(
                    execution,
                    inputs,
                    materialization,
                    transition.left,
                    target_resolution,
                    color_context.clone(),
                    &temporal_layers,
                )?;
                let right = resolve_transition_input(
                    execution,
                    inputs,
                    materialization,
                    transition.right,
                    target_resolution,
                    color_context.clone(),
                    &temporal_layers,
                )?;
                resolved.push(ResolvedPreviewElement::CrossDissolve {
                    left: Box::new(left),
                    right: Box::new(right),
                    progress: transition.progress,
                });
            }
        }
    }
    Ok(Some(resolved))
}

fn collect_prepared_transition_input_source_demand(
    input: &TimelineTransitionInputPlan,
    materialization: PreparedVisualMaterializationContract,
    target_resolution: Resolution,
    color_context: &ProgramColorContext,
    routes: &PreparedTimelinePreviewEffectRoutes,
    temporal_placements: &HashSet<TimelineClipExecutionRef>,
    demands: &mut Vec<PreparedPreviewSourceDemand>,
) -> Result<(), PreviewUnavailability> {
    if transition_input_placement(input)
        .is_some_and(|placement| temporal_placements.contains(&placement))
    {
        return Ok(());
    }
    match input {
        TimelineTransitionInputPlan::Transparent
        | TimelineTransitionInputPlan::SolidColor(_)
        | TimelineTransitionInputPlan::NestedSequence(_) => {}
        TimelineTransitionInputPlan::BasicTitle(title) => demands.push(
            PreparedPreviewSourceDemand::BasicTitle(preview_timeline_title_request(
                materialization,
                title,
                target_resolution,
                color_context.working_color_space(),
            )),
        ),
        TimelineTransitionInputPlan::Media(media) => {
            demands.push(PreparedPreviewSourceDemand::Media {
                request: preview_timeline_media_request(
                    media,
                    target_resolution,
                    color_context,
                    routes,
                )
                .map_err(preview_route_unavailability)?,
                role: PreviewMediaSourceRole::Transition,
            })
        }
    }
    Ok(())
}

fn transition_input_placement(
    input: &TimelineTransitionInputPlan,
) -> Option<TimelineClipExecutionRef> {
    match input {
        TimelineTransitionInputPlan::Transparent => None,
        TimelineTransitionInputPlan::Media(media) => Some(media.placement),
        TimelineTransitionInputPlan::SolidColor(solid) => Some(solid.placement),
        TimelineTransitionInputPlan::BasicTitle(title) => Some(title.placement),
        TimelineTransitionInputPlan::NestedSequence(nested) => Some(nested.placement),
    }
}

fn resolve_transition_input<MediaFrame, TitleFrame>(
    execution: &mut PreviewTimelineExecutionAdapter<'_, '_, '_, MediaFrame, TitleFrame>,
    inputs: &PreviewNodeInputs<'_>,
    parent_materialization: PreparedVisualMaterializationContract,
    input: TimelineTransitionInputPlan,
    target_resolution: Resolution,
    color_context: ProgramColorContext,
    temporal_layers: &HashMap<TimelineClipExecutionRef, PreparedPreviewTemporalLayer>,
) -> Result<ResolvedPreviewTransitionInput, PreviewTimelineAbort>
where
    MediaFrame: FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    TitleFrame: FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
{
    let parent_author_resolution = parent_materialization.author_resolution();
    let routes = prepared_visual_node(execution.closure, inputs.node().id())?
        .evaluation()
        .payload()
        .clone();
    if let Some(placement) = transition_input_placement(&input)
        && let Some(temporal) = temporal_layers.get(&placement)
    {
        let (opacity, blend_mode, transform, effect_graph, frame_seed) = match &input {
            TimelineTransitionInputPlan::Media(media) => (
                media.opacity,
                media.blend_mode,
                media.transform,
                Arc::clone(&media.effect_graph),
                media.frame_seed,
            ),
            TimelineTransitionInputPlan::SolidColor(solid) => (
                solid.opacity,
                solid.blend_mode,
                solid.transform,
                Arc::clone(&solid.effect_graph),
                solid.frame_seed,
            ),
            TimelineTransitionInputPlan::NestedSequence(nested) => (
                nested.opacity,
                nested.blend_mode,
                nested.transform,
                Arc::clone(&nested.effect_graph),
                nested.frame_seed,
            ),
            TimelineTransitionInputPlan::BasicTitle(title) => (
                title.opacity,
                title.blend_mode,
                title.transform,
                Arc::clone(&title.effect_graph),
                title.frame_seed,
            ),
            TimelineTransitionInputPlan::Transparent => {
                return Err(PreviewTimelineAbort::Unavailable(
                    PreviewUnavailability::failed(
                        PreviewOutputStage::TimelineEvaluation,
                        "transparent Transition input cannot own temporal pixels",
                    ),
                ));
            }
        };
        let transform = project_preview_media_transform(
            transform,
            &temporal.frame,
            parent_author_resolution,
            target_resolution,
        )
        .ok_or_else(|| {
            PreviewTimelineAbort::Unavailable(PreviewUnavailability::blocked(
                PreviewOutputStage::TimelineEvaluation,
                format!(
                    "temporal Transition input {} has invalid Preview transform geometry",
                    placement.clip_id
                ),
            ))
        })?;
        return Ok(ResolvedPreviewTransitionInput::Media {
            frame: temporal.frame.clone(),
            opacity,
            blend_mode,
            transform,
            prepared_effect_route: prepared_preview_effect_route(
                &routes,
                placement,
                &effect_graph,
            )?,
            effect_graph,
            frame_seed,
        });
    }
    Ok(match input {
        TimelineTransitionInputPlan::Transparent => ResolvedPreviewTransitionInput::Transparent,
        TimelineTransitionInputPlan::SolidColor(solid) => {
            let prepared_route = prepared_preview_heterogeneous_route(
                &routes,
                solid.placement,
                &solid.effect_graph,
            )?;
            let layer = TimelineSolidColorLayer {
                processor_input_plan: prepared_preview_processor_input_plan(
                    &routes,
                    solid.placement,
                    &solid.effect_graph,
                )?,
                color: solid.color,
                opacity: solid.opacity,
                blend_mode: solid.blend_mode,
                transform: solid.transform,
                effect_graph: solid.effect_graph,
                frame_seed: solid.frame_seed,
            };
            match prepared_route {
                Some(prepared_route) => ResolvedPreviewTransitionInput::HeterogeneousSolidColor {
                    layer,
                    prepared_route,
                },
                None => ResolvedPreviewTransitionInput::SolidColor(layer),
            }
        }
        TimelineTransitionInputPlan::Media(media) => {
            let asset_id = media.asset_id;
            let request =
                preview_timeline_media_request(&media, target_resolution, &color_context, &routes)
                    .map_err(preview_route_abort)?;
            let frame = match (execution.media_frame)(request) {
                PreviewTimelineMediaFrame::Ready(frame) => frame,
                PreviewTimelineMediaFrame::Pending { wait } => {
                    return Err(PreviewTimelineAbort::Pending {
                        dependency: PreviewTimelinePendingDependency::Media { asset_id, wait },
                    });
                }
                PreviewTimelineMediaFrame::Unavailable { reason } => {
                    return Err(PreviewTimelineAbort::Unavailable(
                        PreviewMediaSourceRole::Transition.failure(reason, asset_id),
                    ));
                }
            };
            let transform = project_preview_media_transform(
                media.transform,
                &frame,
                parent_author_resolution,
                target_resolution,
            )
            .ok_or_else(|| {
                PreviewTimelineAbort::Unavailable(PreviewUnavailability::blocked(
                    PreviewOutputStage::TimelineEvaluation,
                    format!("Transition media {asset_id} has invalid Preview transform geometry"),
                ))
            })?;
            ResolvedPreviewTransitionInput::Media {
                frame,
                opacity: media.opacity,
                blend_mode: media.blend_mode,
                transform,
                prepared_effect_route: prepared_preview_effect_route(
                    &routes,
                    media.placement,
                    &media.effect_graph,
                )?,
                effect_graph: media.effect_graph,
                frame_seed: media.frame_seed,
            }
        }
        TimelineTransitionInputPlan::BasicTitle(title) => {
            let (frame, transform) = resolve_basic_title_frame(
                execution,
                parent_materialization,
                &title,
                target_resolution,
                color_context.working_color_space(),
            )?;
            ResolvedPreviewTransitionInput::Media {
                frame,
                opacity: title.opacity,
                blend_mode: title.blend_mode,
                transform,
                prepared_effect_route: prepared_preview_effect_route(
                    &routes,
                    title.placement,
                    &title.effect_graph,
                )?,
                effect_graph: title.effect_graph,
                frame_seed: title.frame_seed,
            }
        }
        TimelineTransitionInputPlan::NestedSequence(nested) => {
            let frame = prepared_preview_nested_output(
                inputs,
                nested.placement,
                PreparedVisualNestedSample::Current,
            )?
            .clone();
            let transform = project_preview_media_transform(
                nested.transform,
                &frame,
                parent_author_resolution,
                target_resolution,
            )
            .ok_or_else(|| {
                PreviewTimelineAbort::Unavailable(PreviewUnavailability::blocked(
                    PreviewOutputStage::TimelineEvaluation,
                    format!(
                        "nested Transition input {} has invalid Preview transform geometry",
                        nested.sequence_id
                    ),
                ))
            })?;
            ResolvedPreviewTransitionInput::Media {
                frame,
                opacity: nested.opacity,
                blend_mode: nested.blend_mode,
                transform,
                prepared_effect_route: prepared_preview_effect_route(
                    &routes,
                    nested.placement,
                    &nested.effect_graph,
                )?,
                effect_graph: nested.effect_graph,
                frame_seed: nested.frame_seed,
            }
        }
    })
}

fn preview_timeline_title_request(
    materialization: PreparedVisualMaterializationContract,
    title: &TimelineBasicTitlePlan,
    target_resolution: Resolution,
    working_color_space: mondrian_core::WorkingColorSpace,
) -> PreviewTimelineTitleRequest {
    PreviewTimelineTitleRequest {
        title: title.title.clone(),
        author_resolution: materialization.author_resolution(),
        sample_aspect_ratio: materialization.sample_aspect_ratio(),
        title_safe_margin: materialization.title_safe_margin(),
        target_resolution,
        working_color_space,
    }
}

fn resolve_basic_title_frame<MediaFrame, TitleFrame>(
    execution: &mut PreviewTimelineExecutionAdapter<'_, '_, '_, MediaFrame, TitleFrame>,
    materialization: PreparedVisualMaterializationContract,
    title: &TimelineBasicTitlePlan,
    target_resolution: Resolution,
    working_color_space: mondrian_core::WorkingColorSpace,
) -> Result<(MediaPreviewFrame, [f32; 6]), PreviewTimelineAbort>
where
    MediaFrame: FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    TitleFrame: FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
{
    let author_resolution = materialization.author_resolution();
    let request = preview_timeline_title_request(
        materialization,
        title,
        target_resolution,
        working_color_space,
    );
    let request_identity = request.identity();
    let raster = match (execution.title_frame)(request) {
        PreviewTimelineTitleFrame::Ready(frame) => frame,
        PreviewTimelineTitleFrame::Pending => {
            return Err(PreviewTimelineAbort::Pending {
                dependency: PreviewTimelinePendingDependency::BasicTitle(request_identity),
            });
        }
        PreviewTimelineTitleFrame::Unavailable { reason } => {
            return Err(PreviewTimelineAbort::Unavailable(reason.with_context(
                format_args!("Basic Title raster request {request_identity}"),
            )));
        }
    };
    let transform = project_basic_title_transform(
        title.transform,
        raster.sampled_source_to_author(),
        author_resolution,
        target_resolution,
    )
    .ok_or_else(|| {
        PreviewTimelineAbort::Unavailable(PreviewUnavailability::blocked(
            PreviewOutputStage::GeneratedSource,
            "Basic Title has invalid Preview transform geometry",
        ))
    })?;
    let frame_identity = basic_title_preview_frame_identity(&raster);
    let render_cache_source_fingerprint = raster.identity().digest();
    Ok((
        MediaPreviewFrame::from_working(
            raster.into_frame(),
            author_resolution,
            frame_identity,
            mondrian_playback::FramePresentationQuality::Ready,
            super::preview_execution::PreviewDecodeExecutionSummary::default(),
        )
        .with_render_cache_source_fingerprint(Some(render_cache_source_fingerprint)),
        transform,
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "Pass the exact frozen child canvas, color, sources and execution authority together without a second context model"
)]
fn materialize_prepared_nested_node(
    sequence_id: SequenceId,
    author_resolution: Resolution,
    frame: i64,
    target_resolution: Resolution,
    materialization: PreparedVisualMaterializationContract,
    parent_working_color_space: mondrian_core::WorkingColorSpace,
    color_context: ProgramColorContext,
    resolved: Vec<ResolvedPreviewElement>,
    resolved_visual_identity: Option<mondrian_renderer::ResolvedVisualFrameIdentity>,
    scratch: &RefCell<TimelineCompositeScratch>,
    facts: &mut Vec<PreviewTimelineExecutionFact>,
    control: &mut mondrian_effects::EffectProcessorControl<'_>,
    inputs: Option<&mut mondrian_renderer::TimelineProcessorInputExecution<'_>>,
    blocked: &mut Option<Arc<mondrian_effects::EffectProcessorFeedbackReceipt>>,
) -> Result<MediaPreviewFrame, PreviewTimelineAbort> {
    let presentation_quality = resolved_preview_presentation_quality(&resolved);
    let decode_execution = resolved_preview_decode_execution(&resolved);
    let nested_plan_identity = viewer_preview_cache_key_for_resolved_plan(
        sequence_id,
        target_resolution.width,
        target_resolution.height,
        &resolved,
        &color_context,
        materialization.image_sampling(target_resolution).map_err(|error| {
            PreviewTimelineAbort::Unavailable(PreviewUnavailability::failed(
                PreviewOutputStage::TimelineComposite,
                error.to_string(),
            ))
        })?,
    )
    .plan_identity;
    let cross_call_reusable = viewer_preview_plan_allows_cross_call_reuse(&resolved);
    let mut scratch = scratch.borrow_mut();
    let output = super::preview_cpu_execution::composite_resolved_preview_working_with_inputs(
        target_resolution.width,
        target_resolution.height,
        &resolved,
        &color_context,
        materialization.image_sampling(target_resolution).map_err(|error| {
            PreviewTimelineAbort::Unavailable(PreviewUnavailability::failed(
                PreviewOutputStage::TimelineComposite,
                error.to_string(),
            ))
        })?,
        &mut scratch,
        control,
        inputs,
    )
    .map_err(|error| {
        *blocked = error.processor_blocked().map(Arc::clone);
        PreviewTimelineAbort::Unavailable(
            error
                .unavailability()
                .with_context(format_args!("nested Sequence {sequence_id} composition")),
        )
    })?;
    facts.push(PreviewTimelineExecutionFact::Composite(
        output.composite_diagnostics,
    ));
    facts.extend(
        output
            .input_color_diagnostics
            .into_iter()
            .map(PreviewTimelineExecutionFact::ColorTransform),
    );
    facts.push(PreviewTimelineExecutionFact::ColorStage(
        output.input_color_stage_diagnostics,
    ));
    facts.push(PreviewTimelineExecutionFact::CpuExecution(
        output.execution_durations,
    ));

    let mut working_frame = output.frame;
    if working_frame.descriptor().color_space.working() != Some(parent_working_color_space) {
        let converted = WorkingColorModule::execute_cpu(
            &working_frame,
            parent_working_color_space,
            color_context.engine().clone(),
            scratch.color_execution_mut(),
        )
        .map_err(|error| {
            PreviewTimelineAbort::Unavailable(PreviewUnavailability::failed(
                PreviewOutputStage::InputAdaptation,
                format!(
                    "nested Sequence {} working conversion failed: {error}",
                    sequence_id
                ),
            ))
        })?;
        facts.push(PreviewTimelineExecutionFact::ColorTransform(
            converted.transform_diagnostics(),
        ));
        facts.push(PreviewTimelineExecutionFact::ColorStage(
            converted.stage_diagnostics(),
        ));
        working_frame = converted.into_frame();
    }
    let frame_identity = nested_preview_frame_identity(
        sequence_id,
        frame.max(0),
        target_resolution,
        nested_plan_identity,
        parent_working_color_space,
        &working_frame,
    );
    if let Err(reason) = control.checkpoint() {
        return Err(PreviewTimelineAbort::Unavailable(
            PreviewUnavailability::stopped(
                PreviewOutputStage::TimelineComposite,
                format!("nested execution stopped: {reason:?}"),
            ),
        ));
    }
    Ok(MediaPreviewFrame::from_working(
        working_frame,
        author_resolution,
        frame_identity,
        presentation_quality,
        decode_execution,
    )
    .with_render_cache_source_fingerprint(
        resolved_visual_identity.map(|identity| identity.digest()),
    )
    .with_cross_call_reuse(cross_call_reusable))
}

fn preview_timeline_media_request(
    media: &TimelineMediaPlan,
    target_resolution: Resolution,
    color_context: &ProgramColorContext,
    routes: &PreparedTimelinePreviewEffectRoutes,
) -> Result<PreviewTimelineMediaRequest, TimelinePreviewEffectRouteError> {
    let route = routes.route_for(media.placement, &media.effect_graph)?;
    Ok(PreviewTimelineMediaRequest {
        asset_id: media.asset_id,
        color_space_override: media.color_space_override,
        alpha_interpretation: media.alpha_interpretation,
        picture_overrides: mondrian_core::PictureInterpretationOverrides {
            pixel_aspect_ratio: media.pixel_aspect_ratio_override,
            field_order: media.field_order_override,
        },
        source_sample: media.source_sample,
        target_resolution,
        input_color: color_context.media_input(media.auto_tone_map),
        image_scale_requirement: media.effect_graph.image_scale_requirement(),
        cpu_working_required: route.requires_cpu_working_frame(),
    })
}

fn preview_route_unavailability(error: TimelinePreviewEffectRouteError) -> PreviewUnavailability {
    PreviewUnavailability::blocked(PreviewOutputStage::TimelineEvaluation, error.to_string())
}

fn preview_route_abort(error: TimelinePreviewEffectRouteError) -> PreviewTimelineAbort {
    PreviewTimelineAbort::Unavailable(preview_route_unavailability(error))
}

fn prepared_preview_processor_input_plan(
    routes: &PreparedTimelinePreviewEffectRoutes,
    placement: TimelineClipExecutionRef,
    graph: &mondrian_effects::CompiledEffectGraph,
) -> Result<Option<Arc<mondrian_renderer::PreparedTimelineClipInputPlan>>, PreviewTimelineAbort> {
    routes
        .processor_input_plan_for(placement, graph)
        .map(|plan| plan.cloned())
        .map_err(preview_route_abort)
}

fn prepared_preview_effect_route(
    routes: &PreparedTimelinePreviewEffectRoutes,
    placement: TimelineClipExecutionRef,
    graph: &mondrian_effects::CompiledEffectGraph,
) -> Result<Option<Box<mondrian_renderer::PreparedTimelinePreviewEffectRoute>>, PreviewTimelineAbort>
{
    let route = routes.route_for(placement, graph).map_err(preview_route_abort)?;
    Ok(
        (route.heterogeneous().is_some() || route.processor_input_plan().is_some())
            .then(|| Box::new(route.clone())),
    )
}

fn prepared_preview_heterogeneous_route(
    routes: &PreparedTimelinePreviewEffectRoutes,
    placement: TimelineClipExecutionRef,
    graph: &CompiledEffectGraph,
) -> Result<Option<Box<PreparedHeterogeneousEffectRoute>>, PreviewTimelineAbort> {
    Ok(routes
        .route_for(placement, graph)
        .map_err(preview_route_abort)?
        .heterogeneous()
        .cloned()
        .map(Box::new))
}

fn preview_temporal_media_request(
    asset_id: AssetId,
    source_sample: mondrian_core::SourceSampleTarget,
    color_space_override: Option<ColorSpace>,
    alpha_interpretation: AlphaInterpretation,
    picture_overrides: mondrian_core::PictureInterpretationOverrides,
    auto_tone_map: bool,
    target_resolution: Resolution,
    color_context: &ProgramColorContext,
    image_scale_requirement: mondrian_effects::EffectImageScaleRequirement,
) -> PreviewTimelineMediaRequest {
    PreviewTimelineMediaRequest {
        image_scale_requirement,
        asset_id,
        color_space_override,
        alpha_interpretation,
        picture_overrides,
        source_sample,
        target_resolution,
        input_color: color_context.media_input(auto_tone_map),
        cpu_working_required: true,
    }
}

fn nested_preview_frame_identity(
    sequence_id: SequenceId,
    frame: i64,
    target_resolution: Resolution,
    nested_plan_identity: PreviewSemanticIdentity,
    parent_working_color_space: mondrian_core::WorkingColorSpace,
    working: &CpuColorFrame,
) -> PreviewSemanticIdentity {
    let mut builder =
        PreviewSemanticIdentityBuilder::new(b"mondrian.preview.nested-sequence-frame.v2");
    sequence_id.hash(&mut builder);
    frame.hash(&mut builder);
    target_resolution.width.hash(&mut builder);
    target_resolution.height.hash(&mut builder);
    nested_plan_identity.hash(&mut builder);
    parent_working_color_space.hash(&mut builder);
    working.descriptor().hash(&mut builder);
    builder.finish_identity()
}

fn basic_title_preview_frame_identity(raster: &BasicTitleRasterFrame) -> PreviewSemanticIdentity {
    let mut builder = PreviewSemanticIdentityBuilder::new(b"mondrian.preview.basic-title-frame.v2");
    raster.identity().hash(&mut builder);
    raster.frame().descriptor().hash(&mut builder);
    for value in raster.sampled_source_to_author() {
        value.to_bits().hash(&mut builder);
    }
    builder.finish_identity()
}

#[derive(Debug)]
enum PreviewTimelineAbort {
    Pending {
        dependency: PreviewTimelinePendingDependency,
    },
    Unavailable(PreviewUnavailability),
}

#[cfg(test)]
pub(crate) use tests::unit_scale_test_effect;

#[cfg(test)]
#[path = "preview_timeline_execution/tests.rs"]
mod tests;
