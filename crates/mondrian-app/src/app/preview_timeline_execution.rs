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
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::Arc;

use mondrian_core::timeline_data::{AlphaInterpretation, TimelineClipExecutionRef};
use mondrian_core::types::{AssetId, ColorSpace, SequenceId};
use mondrian_core::{
    ExecutionCancellationToken, FramePosition, Resolution, TimelineTime, WorkingRgbaF32Frame,
};
use mondrian_effects::{
    EffectFrameExtent, EffectFrameTileF32, EffectTemporalSourceIdentity, PreparedTemporalFrameSet,
};
use mondrian_playback::{FramePresentationQuality, PreviewResolutionScale};
use mondrian_renderer::{
    admit_timeline_render_plan_for_cpu_compositor, basic_title_raster_request_identity,
    execute_cpu_working_transform_with_session, prepare_bound_visual_frame_closure,
    prepare_timeline_temporal_execution, project_basic_title_transform, BasicTitleRasterFrame,
    BasicTitleRasterRequestIdentity, ColorFrameAlpha, CpuColorFrame,
    PreparedVisualAuthorSnapshotIdentity, PreparedVisualChildCanvasPolicy,
    PreparedVisualFrameClosure, PreparedVisualFrameClosureRequest, PreparedVisualFrameEvaluation,
    PreparedVisualFrameNode, PreparedVisualFrameNodeId, PreparedVisualMaterializationContract,
    PreparedVisualNestedSample, PreparedVisualProgramBinding, PreparedVisualProgramCache,
    RenderColorStageDiagnostics, RenderColorTransformDiagnostics, TimelineAdjustmentLayer,
    TimelineBasicTitlePlan, TimelineCompositeDiagnostics, TimelineCompositeScratch,
    TimelineCpuCompositePrecision, TimelineEvaluationRequest, TimelineMediaPlan,
    TimelineRenderPlanElement, TimelineSolidColorLayer, TimelineTemporalDemandBatch,
    TimelineTemporalSource, TimelineTemporalSourceDemand, TimelineTransitionInputPlan,
};
#[cfg(test)]
use mondrian_renderer::{
    prepared_visual_execution_semantic_trace, PreparedVisualExecutionSemanticTrace,
};
use mondrian_timeline::sequence::{MediaInputColorContext, ProgramColorContext, Sequence};

use super::preview_cpu_execution::{
    composite_resolved_preview_working, PreviewCpuExecutionDurations, PreviewCpuExecutionError,
};
use super::preview_execution::{
    PreviewDecodeExecutionSummary, PreviewOutputKey, PreviewSemanticIdentity,
    PreviewSemanticIdentityBuilder,
};
use super::preview_media_frame::{project_preview_media_transform, MediaPreviewFrame};
use super::preview_unavailability::{PreviewOutputStage, PreviewUnavailability};
use super::preview_viewer_plan::{
    resolved_preview_decode_execution, resolved_preview_presentation_quality,
    viewer_preview_cache_key_for_resolved_plan, viewer_preview_plan_allows_cross_call_reuse,
    ResolvedPreviewElement, ResolvedPreviewTransitionInput,
};

/// Complete media-layer request emitted while materializing a prepared node.
#[derive(Debug, Clone)]
pub(crate) struct PreviewTimelineMediaRequest {
    pub(crate) asset_id: AssetId,
    pub(crate) color_space_override: Option<ColorSpace>,
    pub(crate) alpha_interpretation: AlphaInterpretation,
    /// Exact source-local decode target. The media Adapter alone lowers this
    /// value into an FFmpeg stream PTS.
    pub(crate) source_time: TimelineTime,
    pub(crate) target_resolution: Resolution,
    pub(crate) input_color: MediaInputColorContext,
    /// Temporal CPU execution and renderer-owned heterogeneous Effect
    /// execution require a working-memory result and therefore cannot accept
    /// an opaque native decoder surface. Ordinary full-GPU graphs retain the
    /// native/source-domain path.
    pub(crate) cpu_working_required: bool,
}

/// Exhaustive Adapter response for one media layer needed by Timeline execution.
pub(crate) enum PreviewTimelineMediaFrame {
    Ready(MediaPreviewFrame),
    Pending,
    Unavailable { reason: PreviewUnavailability },
}

/// Complete generated-title request emitted while materializing a prepared node.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PreviewTimelineTitleRequest {
    pub(crate) title: mondrian_core::EvaluatedBasicTitle,
    pub(crate) author_resolution: Resolution,
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
    Media(AssetId),
    BasicTitle(BasicTitleRasterRequestIdentity),
    Temporal {
        clip_id: mondrian_core::ClipId,
        pending_sources: usize,
    },
}

/// Resolved root Viewer plan with an always-present semantic output identity
/// and separate cross-call reuse evidence.
pub(crate) struct ResolvedPreviewPlan {
    pub(crate) elements: Vec<ResolvedPreviewElement>,
    pub(crate) cache_key: PreviewOutputKey,
    pub(crate) cache_reusable: bool,
    pub(crate) color_context: ProgramColorContext,
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
pub(crate) enum PreviewTimelineResolution {
    Ready(ResolvedPreviewTimeline),
    Empty,
    Pending {
        dependency: PreviewTimelinePendingDependency,
    },
    Unavailable {
        reason: PreviewUnavailability,
    },
}

/// Complete immutable frame contract for one Preview Timeline evaluation.
pub(crate) struct PreviewTimelineFrameRequest<'a> {
    root_sequence: &'a Sequence,
    sequences: &'a [Sequence],
    frame: i64,
    target_resolution: Resolution,
    runtime_scale: PreviewResolutionScale,
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
            color_context,
        }
    }
}

/// Narrow source-materialization Adapters used by prepared Timeline execution.
pub(crate) struct PreviewTimelineSourceAdapters<'a, MediaFrame, TitleFrame> {
    media_frame: &'a mut MediaFrame,
    title_frame: &'a mut TitleFrame,
}

impl<'a, MediaFrame, TitleFrame> PreviewTimelineSourceAdapters<'a, MediaFrame, TitleFrame> {
    /// Bind media and generated-title resolution for one evaluation call.
    pub(crate) fn new(media_frame: &'a mut MediaFrame, title_frame: &'a mut TitleFrame) -> Self {
        Self { media_frame, title_frame }
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
    ) -> Self {
        Self {
            programs,
            scratch,
            generation,
            cancellation,
            author_snapshot,
            dependency_observer,
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
        color_context,
        &programs,
        &scratch,
        None,
    )
}

pub(crate) fn collect_preview_timeline_media_demands_with_programs(
    sequence: &Sequence,
    sequences: &[Sequence],
    frame: i64,
    target_resolution: Resolution,
    runtime_scale: PreviewResolutionScale,
    color_context: ProgramColorContext,
    programs: &RefCell<PreparedVisualProgramCache>,
    scratch: &RefCell<TimelineCompositeScratch>,
    author_snapshot: Option<PreparedVisualAuthorSnapshotIdentity>,
) -> Result<Vec<PreviewTimelineMediaRequest>, PreviewUnavailability> {
    let cancellation = ExecutionCancellationToken::new();
    let graph = PreviewTimelineGraph {
        programs,
        scratch,
        dependency_observer: None,
        generation: 0,
        cancellation: &cancellation,
        author_snapshot,
    };
    let closure = prepare_preview_frame_closure(
        graph,
        sequence,
        sequences,
        frame.max(0),
        target_resolution,
        color_context,
        runtime_scale,
    )?;
    Ok(collect_prepared_visual_media_demands(&closure, scratch))
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
    )
}

pub(crate) fn resolve_preview_timeline_with_programs_and_observer<MediaFrame, TitleFrame>(
    request: PreviewTimelineFrameRequest<'_>,
    adapters: PreviewTimelineSourceAdapters<'_, MediaFrame, TitleFrame>,
    binding: PreviewTimelineExecutionBinding<'_>,
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
    };
    resolve_preview_timeline_with_graph(request, adapters, graph)
}

fn resolve_preview_timeline_with_graph<MediaFrame, TitleFrame>(
    request: PreviewTimelineFrameRequest<'_>,
    adapters: PreviewTimelineSourceAdapters<'_, MediaFrame, TitleFrame>,
    graph: PreviewTimelineGraph<'_>,
) -> PreviewTimelineResolution
where
    MediaFrame: FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    TitleFrame: FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
{
    let PreviewTimelineFrameRequest {
        root_sequence: sequence,
        sequences,
        frame,
        target_resolution,
        runtime_scale,
        color_context,
    } = request;
    let PreviewTimelineSourceAdapters { media_frame, title_frame } = adapters;
    let scratch = graph.scratch;
    let closure = match prepare_preview_frame_closure(
        graph,
        sequence,
        sequences,
        frame.max(0),
        target_resolution,
        color_context.clone(),
        runtime_scale,
    ) {
        Ok(closure) => closure,
        Err(reason) => return PreviewTimelineResolution::Unavailable { reason },
    };
    #[cfg(test)]
    let semantic_trace = match prepared_visual_execution_semantic_trace(&closure) {
        Ok(trace) => trace,
        Err(error) => {
            return PreviewTimelineResolution::Unavailable {
                reason: PreviewUnavailability::failed(
                    PreviewOutputStage::TimelineEvaluation,
                    format!("Preview visual validation trace failed closed: {error}"),
                ),
            };
        }
    };
    let materialization_bytes = match closure.conservative_cpu_materialization_active_bytes() {
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
    if let Err(error) = scratch.borrow().admit_cpu_active_working_set(
        materialization_bytes,
        TimelineCpuCompositePrecision::Float32,
    ) {
        return PreviewTimelineResolution::Unavailable {
            reason: PreviewUnavailability::blocked(
                PreviewOutputStage::TimelineEvaluation,
                format!("Preview visual closure exceeds its CPU working-set grant: {error}"),
            ),
        };
    }
    let mut execution = PreviewTimelineExecutionContext {
        closure: &closure,
        media_frame,
        title_frame,
        scratch,
        facts: Vec::new(),
    };
    let elements = match resolve_prepared_visual_node(&mut execution, closure.root()) {
        Ok(Some(elements)) => elements,
        Ok(None) => return PreviewTimelineResolution::Empty,
        Err(PreviewTimelineAbort::Pending { dependency }) => {
            return PreviewTimelineResolution::Pending { dependency };
        }
        Err(PreviewTimelineAbort::Unavailable(reason)) => {
            return PreviewTimelineResolution::Unavailable { reason };
        }
    };
    let cache_key = viewer_preview_cache_key_for_resolved_plan(
        sequence.id,
        target_resolution.width,
        target_resolution.height,
        &elements,
        &color_context,
    );
    let cache_reusable = viewer_preview_plan_allows_cross_call_reuse(&elements);
    PreviewTimelineResolution::Ready(ResolvedPreviewTimeline {
        plan: ResolvedPreviewPlan { elements, cache_key, cache_reusable, color_context },
        facts: execution.facts,
        #[cfg(test)]
        semantic_trace,
    })
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
        frame: i64,
        target_resolution: Resolution,
        normalized_preview_resolution_scale: f32,
    ) -> Result<PreparedVisualFrameEvaluation<()>, PreviewUnavailability> {
        let plan = self
            .scratch
            .borrow_mut()
            .evaluate_prepared_visual_program(
                program.as_ref(),
                TimelineEvaluationRequest::preview(
                    FramePosition::new(frame, program.evaluation_time_base()),
                    normalized_preview_resolution_scale,
                ),
            )
            .map_err(|error| {
                PreviewUnavailability::blocked(
                    PreviewOutputStage::TimelineEvaluation,
                    format!(
                        "Sequence {} render-plan evaluation failed: {error}",
                        program.sequence_id()
                    ),
                )
            })?;
        let extent = EffectFrameExtent::new(target_resolution.width, target_resolution.height);
        let prepared = prepare_timeline_temporal_execution(
            program.as_ref(),
            &plan,
            self.generation,
            mondrian_effects::EffectExecutionContinuity::Discontinuous,
            extent,
            extent.full_frame_roi(),
            self.cancellation.clone(),
        )
        .map_err(|error| {
            PreviewUnavailability::blocked(
                PreviewOutputStage::TimelineEvaluation,
                format!(
                    "Sequence {} frame {} temporal preparation failed closed: {error}",
                    program.sequence_id(),
                    frame.max(0)
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
        let (execution_plan, temporal_batches) = prepared.into_parts();
        admit_timeline_render_plan_for_cpu_compositor(&execution_plan).map_err(|error| {
            PreviewUnavailability::blocked(
                PreviewOutputStage::TimelineEvaluation,
                format!(
                    "Sequence {} frame {} cannot enter the current Preview compositor: {error}",
                    program.sequence_id(),
                    frame.max(0)
                ),
            )
        })?;
        Ok(PreparedVisualFrameEvaluation::new(
            execution_plan,
            temporal_batches,
            (),
        ))
    }
}

struct PreviewTimelineExecutionContext<'a, MediaFrame, TitleFrame> {
    closure: &'a PreparedVisualFrameClosure<()>,
    media_frame: &'a mut MediaFrame,
    title_frame: &'a mut TitleFrame,
    scratch: &'a RefCell<TimelineCompositeScratch>,
    facts: Vec<PreviewTimelineExecutionFact>,
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

#[derive(Clone)]
struct PreparedPreviewTemporalLayer {
    frame: MediaPreviewFrame,
}

struct ResolvedPreviewTemporalSource {
    tile: EffectFrameTileF32,
    logical_resolution: Resolution,
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
    presentation_quality: FramePresentationQuality,
    decode_execution: PreviewDecodeExecutionSummary,
    cross_call_reusable: bool,
}

fn resolve_preview_temporal_batches<MediaFrame, TitleFrame>(
    execution: &mut PreviewTimelineExecutionContext<'_, MediaFrame, TitleFrame>,
    node_id: PreparedVisualFrameNodeId,
    batches: &[TimelineTemporalDemandBatch],
) -> Result<HashMap<TimelineClipExecutionRef, PreparedPreviewTemporalLayer>, PreviewTimelineAbort>
where
    MediaFrame: FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    TitleFrame: FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
{
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
        let mut presentation_quality = FramePresentationQuality::Ready;
        let mut decode_execution = PreviewDecodeExecutionSummary::default();
        let mut cross_call_reusable = batch.graph().output_cache_enabled();
        let mut identity =
            PreviewSemanticIdentityBuilder::new(b"mondrian.preview.temporal-source.v1");
        batch.placement().hash(&mut identity);
        batch.graph().semantic_fingerprint().hash(&mut identity);
        color_context.working_color_space.hash(&mut identity);
        let mut batch_pending = false;

        for demand in batch.source_demands() {
            demand.effect_request.hash(&mut identity);
            match resolve_preview_temporal_source(
                execution,
                node_id,
                materialization,
                target_resolution,
                &color_context,
                demand,
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
        let output = execution
            .scratch
            .borrow_mut()
            .execute_prepared_temporal_batch(batch, &mut ready.prepared)
            .map_err(|error| {
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
                width: tile.frame_extent().width(),
                height: tile.frame_extent().height(),
                data: tile.pixels().to_vec(),
                color_space: color_context.working_color_space,
            }),
            ready.logical_resolution,
            PreviewSemanticIdentity::from_complete_fingerprint(output.cache_identity()),
            ready.presentation_quality,
            ready.decode_execution,
        )
        .with_cross_call_reuse(ready.cross_call_reusable);
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
    execution: &mut PreviewTimelineExecutionContext<'_, MediaFrame, TitleFrame>,
    parent_node_id: PreparedVisualFrameNodeId,
    materialization: PreparedVisualMaterializationContract,
    target_resolution: Resolution,
    color_context: &ProgramColorContext,
    demand: &TimelineTemporalSourceDemand,
) -> Result<PreviewTemporalSourceResolution, PreviewTimelineAbort>
where
    MediaFrame: FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    TitleFrame: FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
{
    match &demand.source {
        TimelineTemporalSource::Media {
            asset_id,
            source_time,
            color_space_override,
            alpha_interpretation,
            auto_tone_map,
        } => {
            let request = preview_temporal_media_request(
                *asset_id,
                *source_time,
                *color_space_override,
                *alpha_interpretation,
                *auto_tone_map,
                target_resolution,
                color_context,
            );
            let frame = match (execution.media_frame)(request) {
                PreviewTimelineMediaFrame::Ready(frame) => frame,
                PreviewTimelineMediaFrame::Pending => {
                    return Ok(PreviewTemporalSourceResolution::Pending);
                }
                PreviewTimelineMediaFrame::Unavailable { reason } => {
                    return Err(PreviewTimelineAbort::Unavailable(reason.with_context(
                        format_args!(
                            "temporal media source for Clip {} at {}",
                            demand.placement.clip_id,
                            demand.effect_request.time()
                        ),
                    )));
                }
            };
            preview_temporal_source_from_media_frame(
                execution,
                demand,
                frame,
                color_context.working_color_space,
            )
            .map(PreviewTemporalSourceResolution::Ready)
        }
        TimelineTemporalSource::NestedSequence { sequence_id, source_time, .. } => {
            let child_id = prepared_nested_child(
                execution.closure,
                parent_node_id,
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
            let child_sequence_id = child_node.sequence_id();
            let child_author_resolution = child_node.author_resolution();
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
            if child_node.time() > *source_time {
                return Err(PreviewTimelineAbort::Unavailable(
                    PreviewUnavailability::failed(
                        PreviewOutputStage::TimelineEvaluation,
                        format!(
                            "temporal nested closure projected Sequence {sequence_id} beyond requested source time {source_time}"
                        ),
                    ),
                ));
            }
            let child_frame = child_node.frame();
            let child_resolution = child_node.execution_resolution();
            let child_context = child_node.color_context().clone();
            let child_elements = match resolve_prepared_visual_node(execution, child_id) {
                Ok(elements) => elements.unwrap_or_default(),
                Err(PreviewTimelineAbort::Pending { .. }) => {
                    return Ok(PreviewTemporalSourceResolution::Pending);
                }
                Err(error) => return Err(error),
            };
            let frame = materialize_prepared_nested_node(
                child_sequence_id,
                child_author_resolution,
                child_frame,
                child_resolution,
                color_context.working_color_space,
                child_context,
                child_elements,
                execution.scratch,
                &mut execution.facts,
            )?;
            preview_temporal_source_from_media_frame(
                execution,
                demand,
                frame,
                color_context.working_color_space,
            )
            .map(PreviewTemporalSourceResolution::Ready)
        }
        TimelineTemporalSource::SolidColor { color } => {
            let extent = demand.effect_request.frame_extent();
            let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
                width: extent.width(),
                height: extent.height(),
                data: vec![
                    [color.r, color.g, color.b, color.a];
                    extent.width() as usize * extent.height() as usize
                ],
                color_space: color_context.working_color_space,
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
    execution: &mut PreviewTimelineExecutionContext<'_, MediaFrame, TitleFrame>,
    demand: &TimelineTemporalSourceDemand,
    frame: MediaPreviewFrame,
    working_color_space: mondrian_core::WorkingColorSpace,
) -> Result<ResolvedPreviewTemporalSource, PreviewTimelineAbort> {
    let identity = frame.identity();
    let logical_resolution = frame.logical_resolution();
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
        || descriptor.alpha != ColorFrameAlpha::StraightCoverage
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
        pixels.extend_from_slice(row);
    }
    EffectFrameTileF32::new(
        demand.effect_request.time(),
        demand.effect_request.frame_extent(),
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
) -> Result<PreparedVisualFrameClosure<()>, PreviewUnavailability> {
    let child_canvas_policy =
        PreparedVisualChildCanvasPolicy::preview_scaled(runtime_scale.dimension_divisor())
            .map_err(|error| {
                PreviewUnavailability::blocked(
                    PreviewOutputStage::TimelineEvaluation,
                    error.to_string(),
                )
            })?;
    prepare_bound_visual_frame_closure(
        PreparedVisualFrameClosureRequest {
            root_sequence,
            sequences,
            root_frame: frame,
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
    })
}

fn collect_prepared_visual_media_demands(
    closure: &PreparedVisualFrameClosure<()>,
    scratch: &RefCell<TimelineCompositeScratch>,
) -> Vec<PreviewTimelineMediaRequest> {
    let mut demands = Vec::new();
    for node in closure.nodes() {
        let color_context = node.color_context();
        let target_resolution = node.execution_resolution();
        let temporal_placements = node
            .evaluation()
            .temporal_batches()
            .iter()
            .map(TimelineTemporalDemandBatch::placement)
            .collect::<HashSet<_>>();
        for batch in node.evaluation().temporal_batches() {
            for demand in batch.source_demands() {
                if let TimelineTemporalSource::Media {
                    asset_id,
                    source_time,
                    color_space_override,
                    alpha_interpretation,
                    auto_tone_map,
                } = &demand.source
                {
                    demands.push(preview_temporal_media_request(
                        *asset_id,
                        *source_time,
                        *color_space_override,
                        *alpha_interpretation,
                        *auto_tone_map,
                        target_resolution,
                        color_context,
                    ));
                }
            }
        }
        for element in &node.evaluation().plan().elements {
            match element {
                TimelineRenderPlanElement::Media(media) => {
                    if !temporal_placements.contains(&media.placement) {
                        demands.push(preview_timeline_media_request(
                            media,
                            target_resolution,
                            color_context,
                            scratch,
                        ));
                    }
                }
                TimelineRenderPlanElement::NestedSequence(_) => {}
                TimelineRenderPlanElement::SolidColor(_)
                | TimelineRenderPlanElement::BasicTitle(_)
                | TimelineRenderPlanElement::Adjustment(_) => {}
                TimelineRenderPlanElement::CrossDissolve(transition) => {
                    collect_prepared_transition_input_media_demand(
                        &transition.left,
                        target_resolution,
                        color_context,
                        scratch,
                        &temporal_placements,
                        &mut demands,
                    );
                    collect_prepared_transition_input_media_demand(
                        &transition.right,
                        target_resolution,
                        color_context,
                        scratch,
                        &temporal_placements,
                        &mut demands,
                    );
                }
            }
        }
    }
    demands
}

fn resolve_prepared_visual_node<MediaFrame, TitleFrame>(
    execution: &mut PreviewTimelineExecutionContext<'_, MediaFrame, TitleFrame>,
    node_id: PreparedVisualFrameNodeId,
) -> Result<Option<Vec<ResolvedPreviewElement>>, PreviewTimelineAbort>
where
    MediaFrame: FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    TitleFrame: FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
{
    let (materialization, target_resolution, color_context, elements, temporal_batches) = {
        let node = prepared_visual_node(execution.closure, node_id)?;
        (
            node.materialization_contract(),
            node.execution_resolution(),
            node.color_context().clone(),
            node.evaluation().plan().elements.clone(),
            node.evaluation().temporal_batches().to_vec(),
        )
    };
    let author_resolution = materialization.author_resolution();
    if elements.is_empty() {
        return Ok(None);
    }
    let temporal_layers = resolve_preview_temporal_batches(execution, node_id, &temporal_batches)?;

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
                        effect_graph: solid.effect_graph,
                        frame_seed: solid.frame_seed,
                    });
                } else {
                    resolved.push(ResolvedPreviewElement::SolidColor(
                        TimelineSolidColorLayer {
                            color: solid.color,
                            opacity: solid.opacity,
                            blend_mode: solid.blend_mode,
                            transform: solid.transform,
                            effect_graph: solid.effect_graph,
                            frame_seed: solid.frame_seed,
                        },
                    ));
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
                        execution.scratch,
                    );
                    match (execution.media_frame)(request) {
                        PreviewTimelineMediaFrame::Ready(frame) => frame,
                        PreviewTimelineMediaFrame::Pending => {
                            return Err(PreviewTimelineAbort::Pending {
                                dependency: PreviewTimelinePendingDependency::Media(asset_id),
                            });
                        }
                        PreviewTimelineMediaFrame::Unavailable { reason } => {
                            return Err(PreviewTimelineAbort::Unavailable(
                                reason.with_context(format_args!("media {asset_id}")),
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
                    color_context.working_color_space,
                )?;
                resolved.push(ResolvedPreviewElement::Media {
                    frame,
                    opacity: title.opacity,
                    blend_mode: title.blend_mode,
                    transform,
                    effect_graph: title.effect_graph,
                    frame_seed: title.frame_seed,
                });
            }
            TimelineRenderPlanElement::Adjustment(adjustment) => {
                resolved.push(ResolvedPreviewElement::Adjustment(
                    TimelineAdjustmentLayer {
                        effect_graph: adjustment.effect_graph,
                        opacity: adjustment.opacity,
                        blend_mode: Some(adjustment.blend_mode),
                        frame_seed: adjustment.frame_seed,
                    },
                ));
            }
            TimelineRenderPlanElement::NestedSequence(nested) => {
                let frame = if let Some(temporal) = temporal_layers.get(&nested.placement) {
                    temporal.frame.clone()
                } else {
                    let child_id = prepared_nested_child(
                        execution.closure,
                        node_id,
                        nested.placement,
                        PreparedVisualNestedSample::Current,
                    )?;
                    let child_node = prepared_visual_node(execution.closure, child_id)?;
                    let nested_sequence_id = child_node.sequence_id();
                    let nested_author_resolution = child_node.author_resolution();
                    let parent_working_color_space = color_context.working_color_space;
                    let child_frame = child_node.frame();
                    let child_resolution = child_node.execution_resolution();
                    let child_context = child_node.color_context().clone();
                    let nested_elements =
                        resolve_prepared_visual_node(execution, child_id)?.unwrap_or_default();
                    let scratch = execution.scratch;
                    materialize_prepared_nested_node(
                        nested_sequence_id,
                        nested_author_resolution,
                        child_frame,
                        child_resolution,
                        parent_working_color_space,
                        child_context,
                        nested_elements,
                        scratch,
                        &mut execution.facts,
                    )?
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
                    effect_graph: nested.effect_graph,
                    frame_seed: nested.frame_seed,
                });
            }
            TimelineRenderPlanElement::CrossDissolve(transition) => {
                let left = resolve_transition_input(
                    execution,
                    node_id,
                    materialization,
                    transition.left,
                    target_resolution,
                    color_context.clone(),
                    &temporal_layers,
                )?;
                let right = resolve_transition_input(
                    execution,
                    node_id,
                    materialization,
                    transition.right,
                    target_resolution,
                    color_context.clone(),
                    &temporal_layers,
                )?;
                resolved.push(ResolvedPreviewElement::CrossDissolve {
                    left,
                    right,
                    progress: transition.progress,
                });
            }
        }
    }
    Ok(Some(resolved))
}

fn collect_prepared_transition_input_media_demand(
    input: &TimelineTransitionInputPlan,
    target_resolution: Resolution,
    color_context: &ProgramColorContext,
    scratch: &RefCell<TimelineCompositeScratch>,
    temporal_placements: &HashSet<TimelineClipExecutionRef>,
    demands: &mut Vec<PreviewTimelineMediaRequest>,
) {
    if transition_input_placement(input)
        .is_some_and(|placement| temporal_placements.contains(&placement))
    {
        return;
    }
    match input {
        TimelineTransitionInputPlan::Transparent
        | TimelineTransitionInputPlan::SolidColor(_)
        | TimelineTransitionInputPlan::BasicTitle(_)
        | TimelineTransitionInputPlan::NestedSequence(_) => {}
        TimelineTransitionInputPlan::Media(media) => demands.push(preview_timeline_media_request(
            media,
            target_resolution,
            color_context,
            scratch,
        )),
    }
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
    execution: &mut PreviewTimelineExecutionContext<'_, MediaFrame, TitleFrame>,
    parent_node_id: PreparedVisualFrameNodeId,
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
    if let Some(placement) = transition_input_placement(&input) {
        if let Some(temporal) = temporal_layers.get(&placement) {
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
                effect_graph,
                frame_seed,
            });
        }
    }
    Ok(match input {
        TimelineTransitionInputPlan::Transparent => ResolvedPreviewTransitionInput::Transparent,
        TimelineTransitionInputPlan::SolidColor(solid) => {
            ResolvedPreviewTransitionInput::SolidColor(TimelineSolidColorLayer {
                color: solid.color,
                opacity: solid.opacity,
                blend_mode: solid.blend_mode,
                transform: solid.transform,
                effect_graph: solid.effect_graph,
                frame_seed: solid.frame_seed,
            })
        }
        TimelineTransitionInputPlan::Media(media) => {
            let asset_id = media.asset_id;
            let request = preview_timeline_media_request(
                &media,
                target_resolution,
                &color_context,
                execution.scratch,
            );
            let frame = match (execution.media_frame)(request) {
                PreviewTimelineMediaFrame::Ready(frame) => frame,
                PreviewTimelineMediaFrame::Pending => {
                    return Err(PreviewTimelineAbort::Pending {
                        dependency: PreviewTimelinePendingDependency::Media(asset_id),
                    });
                }
                PreviewTimelineMediaFrame::Unavailable { reason } => {
                    return Err(PreviewTimelineAbort::Unavailable(
                        reason.with_context(format_args!("Transition media {asset_id}")),
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
                color_context.working_color_space,
            )?;
            ResolvedPreviewTransitionInput::Media {
                frame,
                opacity: title.opacity,
                blend_mode: title.blend_mode,
                transform,
                effect_graph: title.effect_graph,
                frame_seed: title.frame_seed,
            }
        }
        TimelineTransitionInputPlan::NestedSequence(nested) => {
            let child_id = prepared_nested_child(
                execution.closure,
                parent_node_id,
                nested.placement,
                PreparedVisualNestedSample::Current,
            )?;
            let child_node = prepared_visual_node(execution.closure, child_id)?;
            let child_sequence_id = child_node.sequence_id();
            let child_author_resolution = child_node.author_resolution();
            let child_frame = child_node.frame();
            let child_resolution = child_node.execution_resolution();
            let child_context = child_node.color_context().clone();
            let child_elements =
                resolve_prepared_visual_node(execution, child_id)?.unwrap_or_default();
            let scratch = execution.scratch;
            let frame = materialize_prepared_nested_node(
                child_sequence_id,
                child_author_resolution,
                child_frame,
                child_resolution,
                color_context.working_color_space,
                child_context,
                child_elements,
                scratch,
                &mut execution.facts,
            )?;
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
                effect_graph: nested.effect_graph,
                frame_seed: nested.frame_seed,
            }
        }
    })
}

fn resolve_basic_title_frame<MediaFrame, TitleFrame>(
    execution: &mut PreviewTimelineExecutionContext<'_, MediaFrame, TitleFrame>,
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
    let request = PreviewTimelineTitleRequest {
        title: title.title.clone(),
        author_resolution,
        title_safe_margin: materialization.title_safe_margin(),
        target_resolution,
        working_color_space,
    };
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
    Ok((
        MediaPreviewFrame::from_working(
            raster.into_frame(),
            author_resolution,
            frame_identity,
            mondrian_playback::FramePresentationQuality::Ready,
            super::preview_execution::PreviewDecodeExecutionSummary::default(),
        ),
        transform,
    ))
}

fn materialize_prepared_nested_node(
    sequence_id: SequenceId,
    author_resolution: Resolution,
    frame: i64,
    target_resolution: Resolution,
    parent_working_color_space: mondrian_core::WorkingColorSpace,
    color_context: ProgramColorContext,
    resolved: Vec<ResolvedPreviewElement>,
    scratch: &RefCell<TimelineCompositeScratch>,
    facts: &mut Vec<PreviewTimelineExecutionFact>,
) -> Result<MediaPreviewFrame, PreviewTimelineAbort> {
    let presentation_quality = resolved_preview_presentation_quality(&resolved);
    let decode_execution = resolved_preview_decode_execution(&resolved);
    let nested_plan_identity = viewer_preview_cache_key_for_resolved_plan(
        sequence_id,
        target_resolution.width,
        target_resolution.height,
        &resolved,
        &color_context,
    )
    .plan_identity;
    let cross_call_reusable = viewer_preview_plan_allows_cross_call_reuse(&resolved);
    let mut scratch = scratch.borrow_mut();
    let output = composite_resolved_preview_working(
        target_resolution.width,
        target_resolution.height,
        &resolved,
        &color_context,
        &mut scratch,
    )
    .map_err(|error| {
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
        let converted = execute_cpu_working_transform_with_session(
            &working_frame,
            parent_working_color_space,
            color_context.engine.clone(),
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
            converted.result.diagnostics,
        ));
        facts.push(PreviewTimelineExecutionFact::ColorStage(
            converted.stage_diagnostics,
        ));
        working_frame = converted.result.frame;
    }
    let frame_identity = nested_preview_frame_identity(
        sequence_id,
        frame.max(0),
        target_resolution,
        nested_plan_identity,
        parent_working_color_space,
        &working_frame,
    );
    Ok(MediaPreviewFrame::from_working(
        working_frame,
        author_resolution,
        frame_identity,
        presentation_quality,
        decode_execution,
    )
    .with_cross_call_reuse(cross_call_reusable))
}

fn preview_timeline_media_request(
    media: &TimelineMediaPlan,
    target_resolution: Resolution,
    color_context: &ProgramColorContext,
    scratch: &RefCell<TimelineCompositeScratch>,
) -> PreviewTimelineMediaRequest {
    PreviewTimelineMediaRequest {
        asset_id: media.asset_id,
        color_space_override: media.color_space_override,
        alpha_interpretation: media.alpha_interpretation,
        source_time: media.source_time,
        target_resolution,
        input_color: color_context.media_input(media.auto_tone_map),
        cpu_working_required: scratch
            .borrow_mut()
            .get_or_lower_effect_gpu_plan(&media.effect_graph)
            .is_err(),
    }
}

fn preview_temporal_media_request(
    asset_id: AssetId,
    source_time: TimelineTime,
    color_space_override: Option<ColorSpace>,
    alpha_interpretation: AlphaInterpretation,
    auto_tone_map: bool,
    target_resolution: Resolution,
    color_context: &ProgramColorContext,
) -> PreviewTimelineMediaRequest {
    PreviewTimelineMediaRequest {
        asset_id,
        color_space_override,
        alpha_interpretation,
        source_time,
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
#[path = "preview_timeline_execution/tests.rs"]
mod tests;
