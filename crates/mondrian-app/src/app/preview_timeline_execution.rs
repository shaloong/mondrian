//! UI-independent recursive Timeline execution for Viewer Preview.
//!
//! This deep Module owns canonical render-plan traversal, nested Sequence
//! lookup and depth limits, nested working-space composition/conversion,
//! resolved Viewer elements, and their stable cache identity. A caller supplies
//! only immutable Sequence snapshots and a narrow media-frame closure; Window
//! and Headless Adapters cannot maintain separate recursion or color math.

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use mondrian_core::timeline_data::AlphaInterpretation;
use mondrian_core::types::{AssetId, ColorSpace, SequenceId};
use mondrian_core::{FrameRounding, Resolution, TimelineTime};
use mondrian_playback::PreviewResolutionScale;
use mondrian_renderer::{
    basic_title_raster_request_key, evaluate_timeline_render_plan, execute_cpu_working_transform,
    project_basic_title_transform, BasicTitleRasterFrame, CpuColorFrame,
    RenderColorStageDiagnostics, RenderColorTransformDiagnostics, TimelineAdjustmentLayer,
    TimelineBasicTitlePlan, TimelineCompositeDiagnostics, TimelineCompositeScratch,
    TimelineEvaluationRequest, TimelineMediaPlan, TimelineRenderPlan, TimelineRenderPlanElement,
    TimelineSolidColorLayer, TimelineTransitionInputPlan,
};
use mondrian_timeline::sequence::{
    MediaInputColorContext, ProgramColorContext, Sequence, MAX_NESTED_SEQUENCE_RENDER_DEPTH,
};
use mondrian_timeline::PreparedVisualScheduleCache;

use super::preview_cpu_execution::{
    composite_resolved_preview_working, PreviewCpuExecutionDurations,
};
use super::preview_execution::PreviewOutputKey;
use super::preview_media_frame::{project_preview_media_transform, MediaPreviewFrame};
use super::preview_quality::{normalize_preview_resolution_scale, preview_execution_resolution};
use super::preview_unavailability::{PreviewOutputStage, PreviewUnavailability};
use super::preview_viewer_plan::{
    resolved_preview_decode_execution, resolved_preview_presentation_quality,
    viewer_preview_cache_key_for_resolved_plan, ResolvedPreviewElement,
    ResolvedPreviewTransitionInput,
};

/// Complete media-layer request emitted by canonical Timeline traversal.
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
}

/// Exhaustive Adapter response for one media layer needed by Timeline execution.
pub(crate) enum PreviewTimelineMediaFrame {
    Ready(MediaPreviewFrame),
    Pending,
    Unavailable { reason: PreviewUnavailability },
}

/// Complete generated-title request emitted by canonical Timeline traversal.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PreviewTimelineTitleRequest {
    pub(crate) title: mondrian_core::EvaluatedBasicTitle,
    pub(crate) author_resolution: Resolution,
    pub(crate) title_safe_margin: f32,
    pub(crate) target_resolution: Resolution,
    pub(crate) working_color_space: mondrian_core::WorkingColorSpace,
}

impl PreviewTimelineTitleRequest {
    pub(crate) fn key(&self) -> u64 {
        basic_title_raster_request_key(
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
    BasicTitle(u64),
}

/// Resolved root Viewer plan with an always-present semantic cache identity.
pub(crate) struct ResolvedPreviewPlan {
    pub(crate) elements: Vec<ResolvedPreviewElement>,
    pub(crate) cache_key: PreviewOutputKey,
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

/// Collect the media dependencies of one frame without executing or scheduling them.
///
/// This is the only traversal used by prefetch and preroll Adapters. It shares
/// nested lookup, depth, quality, and color-context rules with real execution.
pub(crate) fn collect_preview_timeline_media_demands(
    sequence: &Sequence,
    sequences: &[Sequence],
    frame: i64,
    target_resolution: Resolution,
    runtime_scale: PreviewResolutionScale,
    color_context: ProgramColorContext,
) -> Result<Vec<PreviewTimelineMediaRequest>, PreviewUnavailability> {
    let schedules = RefCell::new(PreparedVisualScheduleCache::default());
    collect_preview_timeline_media_demands_with_schedules(
        sequence,
        sequences,
        frame,
        target_resolution,
        runtime_scale,
        color_context,
        &schedules,
    )
}

pub(crate) fn collect_preview_timeline_media_demands_with_schedules(
    sequence: &Sequence,
    sequences: &[Sequence],
    frame: i64,
    target_resolution: Resolution,
    runtime_scale: PreviewResolutionScale,
    color_context: ProgramColorContext,
    schedules: &RefCell<PreparedVisualScheduleCache>,
) -> Result<Vec<PreviewTimelineMediaRequest>, PreviewUnavailability> {
    let graph = PreviewTimelineGraph {
        root_sequence: sequence,
        sequences,
        runtime_scale,
        schedules,
    };
    let mut demands = Vec::new();
    collect_sequence_media_demands(
        graph,
        sequence,
        frame.max(0),
        target_resolution,
        0,
        color_context,
        &mut demands,
    )?;
    Ok(demands)
}

/// Resolve one root Sequence through the same recursion for Window and Headless.
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
    let schedules = RefCell::new(PreparedVisualScheduleCache::default());
    resolve_preview_timeline_with_schedules(
        sequence,
        sequences,
        frame,
        target_resolution,
        runtime_scale,
        color_context,
        media_frame,
        title_frame,
        &schedules,
    )
}

pub(crate) fn resolve_preview_timeline_with_schedules(
    sequence: &Sequence,
    sequences: &[Sequence],
    frame: i64,
    target_resolution: Resolution,
    runtime_scale: PreviewResolutionScale,
    color_context: ProgramColorContext,
    media_frame: &mut impl FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    title_frame: &mut impl FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
    schedules: &RefCell<PreparedVisualScheduleCache>,
) -> PreviewTimelineResolution {
    let graph = PreviewTimelineGraph {
        root_sequence: sequence,
        sequences,
        runtime_scale,
        schedules,
    };
    let mut execution =
        PreviewTimelineExecutionContext { graph, media_frame, title_frame, facts: Vec::new() };
    let elements = match resolve_sequence_elements(
        &mut execution,
        sequence,
        frame.max(0),
        target_resolution,
        0,
        color_context.clone(),
    ) {
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
    PreviewTimelineResolution::Ready(ResolvedPreviewTimeline {
        plan: ResolvedPreviewPlan { elements, cache_key, color_context },
        facts: execution.facts,
    })
}

#[derive(Clone, Copy)]
struct PreviewTimelineGraph<'a> {
    root_sequence: &'a Sequence,
    sequences: &'a [Sequence],
    runtime_scale: PreviewResolutionScale,
    schedules: &'a RefCell<PreparedVisualScheduleCache>,
}

struct NestedPreviewSequence<'a> {
    sequence: &'a Sequence,
    target_resolution: Resolution,
    color_context: ProgramColorContext,
}

impl<'a> PreviewTimelineGraph<'a> {
    fn evaluate(
        self,
        sequence: &Sequence,
        frame: i64,
    ) -> Result<TimelineRenderPlan, PreviewUnavailability> {
        let schedule = self.schedules.borrow_mut().prepare(sequence).map_err(|error| {
            PreviewUnavailability::blocked(
                PreviewOutputStage::TimelineEvaluation,
                format!(
                    "Sequence {} visual-schedule preparation failed: {error}",
                    sequence.id
                ),
            )
        })?;
        evaluate_timeline_render_plan(
            schedule.as_ref(),
            TimelineEvaluationRequest::preview(
                frame.max(0),
                normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale),
            ),
        )
        .map_err(|error| {
            PreviewUnavailability::blocked(
                PreviewOutputStage::TimelineEvaluation,
                format!(
                    "Sequence {} render-plan evaluation failed: {error}",
                    sequence.id
                ),
            )
        })
    }

    fn nested(
        self,
        sequence_id: SequenceId,
        parent_color_context: &ProgramColorContext,
        color_processing: mondrian_core::timeline_data::NestedColorProcessing,
    ) -> Result<NestedPreviewSequence<'a>, PreviewUnavailability> {
        let sequence =
            sequence_by_id(self.root_sequence, self.sequences, sequence_id).ok_or_else(|| {
                PreviewUnavailability::blocked(
                    PreviewOutputStage::TimelineEvaluation,
                    format!("nested Sequence {sequence_id} does not exist"),
                )
            })?;
        Ok(NestedPreviewSequence {
            sequence,
            target_resolution: preview_execution_resolution(
                sequence.settings.resolution,
                sequence.settings.preview.resolution_scale,
                self.runtime_scale,
            ),
            color_context: sequence
                .settings
                .nested_render_color_context(parent_color_context.clone(), color_processing),
        })
    }
}

struct PreviewTimelineExecutionContext<'a, MediaFrame, TitleFrame> {
    graph: PreviewTimelineGraph<'a>,
    media_frame: &'a mut MediaFrame,
    title_frame: &'a mut TitleFrame,
    facts: Vec<PreviewTimelineExecutionFact>,
}

fn collect_sequence_media_demands(
    graph: PreviewTimelineGraph<'_>,
    sequence: &Sequence,
    frame: i64,
    target_resolution: Resolution,
    depth: usize,
    color_context: ProgramColorContext,
    demands: &mut Vec<PreviewTimelineMediaRequest>,
) -> Result<(), PreviewUnavailability> {
    validate_nested_depth(sequence, depth)?;
    for element in graph.evaluate(sequence, frame)?.elements {
        match element {
            TimelineRenderPlanElement::Media(media) => {
                demands.push(preview_timeline_media_request(
                    &media,
                    target_resolution,
                    &color_context,
                ));
            }
            TimelineRenderPlanElement::NestedSequence(nested_plan) => {
                let nested = graph.nested(
                    nested_plan.sequence_id,
                    &color_context,
                    nested_plan.color_processing,
                )?;
                let nested_frame = nested_sequence_frame(nested_plan.source_time, nested.sequence)?;
                collect_sequence_media_demands(
                    graph,
                    nested.sequence,
                    nested_frame,
                    nested.target_resolution,
                    depth + 1,
                    nested.color_context,
                    demands,
                )?;
            }
            TimelineRenderPlanElement::SolidColor(_)
            | TimelineRenderPlanElement::BasicTitle(_)
            | TimelineRenderPlanElement::Adjustment(_) => {}
            TimelineRenderPlanElement::CrossDissolve(transition) => {
                collect_transition_input_media_demands(
                    graph,
                    sequence,
                    &transition.left,
                    target_resolution,
                    depth,
                    color_context.clone(),
                    demands,
                )?;
                collect_transition_input_media_demands(
                    graph,
                    sequence,
                    &transition.right,
                    target_resolution,
                    depth,
                    color_context.clone(),
                    demands,
                )?;
            }
        }
    }
    Ok(())
}

fn resolve_sequence_elements<MediaFrame, TitleFrame>(
    execution: &mut PreviewTimelineExecutionContext<'_, MediaFrame, TitleFrame>,
    sequence: &Sequence,
    frame: i64,
    target_resolution: Resolution,
    depth: usize,
    color_context: ProgramColorContext,
) -> Result<Option<Vec<ResolvedPreviewElement>>, PreviewTimelineAbort>
where
    MediaFrame: FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    TitleFrame: FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
{
    validate_nested_depth(sequence, depth).map_err(PreviewTimelineAbort::Unavailable)?;
    let evaluation = execution
        .graph
        .evaluate(sequence, frame)
        .map_err(PreviewTimelineAbort::Unavailable)?;
    if evaluation.is_empty() {
        return Ok(None);
    }

    let mut resolved = Vec::with_capacity(evaluation.len());
    for element in evaluation.elements {
        match element {
            TimelineRenderPlanElement::SolidColor(solid) => {
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
            TimelineRenderPlanElement::Media(media) => {
                let asset_id = media.asset_id;
                let request =
                    preview_timeline_media_request(&media, target_resolution, &color_context);
                let frame = match (execution.media_frame)(request) {
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
                };
                let transform = project_preview_media_transform(
                    media.transform,
                    &frame,
                    sequence.settings.resolution,
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
                    sequence,
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
                let graph = execution.graph;
                let nested_execution = graph
                    .nested(nested.sequence_id, &color_context, nested.color_processing)
                    .map_err(PreviewTimelineAbort::Unavailable)?;
                let nested_sequence = nested_execution.sequence;
                let nested_frame = nested_sequence_frame(nested.source_time, nested_sequence)
                    .map_err(PreviewTimelineAbort::Unavailable)?;
                let parent_working_color_space = color_context.working_color_space;
                let nested_elements = resolve_sequence_elements(
                    execution,
                    nested_sequence,
                    nested_frame,
                    nested_execution.target_resolution,
                    depth + 1,
                    nested_execution.color_context.clone(),
                )?
                .unwrap_or_default();
                let frame = render_nested_sequence(
                    nested_sequence,
                    nested_frame,
                    nested_execution.target_resolution,
                    parent_working_color_space,
                    nested_execution.color_context,
                    nested_elements,
                    &mut execution.facts,
                )?;
                let transform = project_preview_media_transform(
                    nested.transform,
                    &frame,
                    sequence.settings.resolution,
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
                    sequence,
                    transition.left,
                    target_resolution,
                    depth,
                    color_context.clone(),
                )?;
                let right = resolve_transition_input(
                    execution,
                    sequence,
                    transition.right,
                    target_resolution,
                    depth,
                    color_context.clone(),
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

fn collect_transition_input_media_demands(
    graph: PreviewTimelineGraph<'_>,
    _parent_sequence: &Sequence,
    input: &TimelineTransitionInputPlan,
    target_resolution: Resolution,
    depth: usize,
    color_context: ProgramColorContext,
    demands: &mut Vec<PreviewTimelineMediaRequest>,
) -> Result<(), PreviewUnavailability> {
    match input {
        TimelineTransitionInputPlan::Transparent
        | TimelineTransitionInputPlan::SolidColor(_)
        | TimelineTransitionInputPlan::BasicTitle(_) => {}
        TimelineTransitionInputPlan::Media(media) => demands.push(preview_timeline_media_request(
            media,
            target_resolution,
            &color_context,
        )),
        TimelineTransitionInputPlan::NestedSequence(nested) => {
            let child =
                graph.nested(nested.sequence_id, &color_context, nested.color_processing)?;
            let child_frame = nested_sequence_frame(nested.source_time, child.sequence)?;
            collect_sequence_media_demands(
                graph,
                child.sequence,
                child_frame,
                child.target_resolution,
                depth + 1,
                child.color_context,
                demands,
            )?;
        }
    }
    Ok(())
}

fn resolve_transition_input<MediaFrame, TitleFrame>(
    execution: &mut PreviewTimelineExecutionContext<'_, MediaFrame, TitleFrame>,
    parent_sequence: &Sequence,
    input: TimelineTransitionInputPlan,
    target_resolution: Resolution,
    depth: usize,
    color_context: ProgramColorContext,
) -> Result<ResolvedPreviewTransitionInput, PreviewTimelineAbort>
where
    MediaFrame: FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    TitleFrame: FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
{
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
            let request = preview_timeline_media_request(&media, target_resolution, &color_context);
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
                parent_sequence.settings.resolution,
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
                parent_sequence,
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
            let graph = execution.graph;
            let child = graph
                .nested(nested.sequence_id, &color_context, nested.color_processing)
                .map_err(PreviewTimelineAbort::Unavailable)?;
            let child_frame = nested_sequence_frame(nested.source_time, child.sequence)
                .map_err(PreviewTimelineAbort::Unavailable)?;
            let child_elements = resolve_sequence_elements(
                execution,
                child.sequence,
                child_frame,
                child.target_resolution,
                depth + 1,
                child.color_context.clone(),
            )?
            .unwrap_or_default();
            let frame = render_nested_sequence(
                child.sequence,
                child_frame,
                child.target_resolution,
                color_context.working_color_space,
                child.color_context,
                child_elements,
                &mut execution.facts,
            )?;
            let transform = project_preview_media_transform(
                nested.transform,
                &frame,
                parent_sequence.settings.resolution,
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
    sequence: &Sequence,
    title: &TimelineBasicTitlePlan,
    target_resolution: Resolution,
    working_color_space: mondrian_core::WorkingColorSpace,
) -> Result<(MediaPreviewFrame, [f32; 6]), PreviewTimelineAbort>
where
    MediaFrame: FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    TitleFrame: FnMut(PreviewTimelineTitleRequest) -> PreviewTimelineTitleFrame,
{
    let request = PreviewTimelineTitleRequest {
        title: title.title.clone(),
        author_resolution: sequence.settings.resolution,
        title_safe_margin: sequence.settings.title_safe_margin,
        target_resolution,
        working_color_space,
    };
    let request_key = request.key();
    let raster = match (execution.title_frame)(request) {
        PreviewTimelineTitleFrame::Ready(frame) => frame,
        PreviewTimelineTitleFrame::Pending => {
            return Err(PreviewTimelineAbort::Pending {
                dependency: PreviewTimelinePendingDependency::BasicTitle(request_key),
            });
        }
        PreviewTimelineTitleFrame::Unavailable { reason } => {
            return Err(PreviewTimelineAbort::Unavailable(reason.with_context(
                format_args!("Basic Title raster request {request_key:016x}"),
            )));
        }
    };
    let transform = project_basic_title_transform(
        title.transform,
        raster.sampled_source_to_author,
        sequence.settings.resolution,
        target_resolution,
    )
    .ok_or_else(|| {
        PreviewTimelineAbort::Unavailable(PreviewUnavailability::blocked(
            PreviewOutputStage::GeneratedSource,
            "Basic Title has invalid Preview transform geometry",
        ))
    })?;
    Ok((
        MediaPreviewFrame::from_working(
            raster.frame,
            sequence.settings.resolution,
            raster.signature,
            mondrian_playback::FramePresentationQuality::Ready,
            super::preview_execution::PreviewDecodeExecutionSummary::default(),
        ),
        transform,
    ))
}

fn render_nested_sequence(
    sequence: &Sequence,
    frame: i64,
    target_resolution: Resolution,
    parent_working_color_space: mondrian_core::WorkingColorSpace,
    color_context: ProgramColorContext,
    resolved: Vec<ResolvedPreviewElement>,
    facts: &mut Vec<PreviewTimelineExecutionFact>,
) -> Result<MediaPreviewFrame, PreviewTimelineAbort> {
    let presentation_quality = resolved_preview_presentation_quality(&resolved);
    let decode_execution = resolved_preview_decode_execution(&resolved);
    let mut scratch = TimelineCompositeScratch::default();
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
                .with_context(format_args!("nested Sequence {} composition", sequence.id)),
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
        let converted = execute_cpu_working_transform(
            &working_frame,
            parent_working_color_space,
            color_context.engine.clone(),
        )
        .map_err(|error| {
            PreviewTimelineAbort::Unavailable(PreviewUnavailability::failed(
                PreviewOutputStage::InputAdaptation,
                format!(
                    "nested Sequence {} working conversion failed: {error}",
                    sequence.id
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
    let signature = nested_preview_frame_signature(
        sequence.id,
        frame.max(0),
        target_resolution,
        &working_frame,
    );
    Ok(MediaPreviewFrame::from_working(
        working_frame,
        sequence.settings.resolution,
        signature,
        presentation_quality,
        decode_execution,
    ))
}

fn preview_timeline_media_request(
    media: &TimelineMediaPlan,
    target_resolution: Resolution,
    color_context: &ProgramColorContext,
) -> PreviewTimelineMediaRequest {
    PreviewTimelineMediaRequest {
        asset_id: media.asset_id,
        color_space_override: media.color_space_override,
        alpha_interpretation: media.alpha_interpretation,
        source_time: media.source_time,
        target_resolution,
        input_color: color_context.media_input(media.auto_tone_map),
    }
}

fn nested_sequence_frame(
    source_time: TimelineTime,
    sequence: &Sequence,
) -> Result<i64, PreviewUnavailability> {
    let frame = source_time
        .to_frame_position(sequence.settings.frame_rate, FrameRounding::Floor)
        .map_err(|error| {
            PreviewUnavailability::blocked(
                PreviewOutputStage::TimelineEvaluation,
                format!(
                    "nested Sequence {} source target {source_time} is invalid: {error}",
                    sequence.id
                ),
            )
        })?
        .frame;
    if frame < 0 {
        return Err(PreviewUnavailability::blocked(
            PreviewOutputStage::TimelineEvaluation,
            format!(
                "nested Sequence {} has insufficient source handle for target {source_time}",
                sequence.id
            ),
        ));
    }
    Ok(frame)
}

fn validate_nested_depth(sequence: &Sequence, depth: usize) -> Result<(), PreviewUnavailability> {
    if depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH {
        Err(PreviewUnavailability::blocked(
            PreviewOutputStage::TimelineEvaluation,
            format!(
                "nested Sequence depth exceeded {} at {}",
                MAX_NESTED_SEQUENCE_RENDER_DEPTH, sequence.id
            ),
        ))
    } else {
        Ok(())
    }
}

fn sequence_by_id<'a>(
    root_sequence: &'a Sequence,
    sequences: &'a [Sequence],
    id: SequenceId,
) -> Option<&'a Sequence> {
    if root_sequence.id == id {
        Some(root_sequence)
    } else {
        sequences.iter().find(|sequence| sequence.id == id)
    }
}

fn nested_preview_frame_signature(
    sequence_id: SequenceId,
    frame: i64,
    target_resolution: Resolution,
    working: &CpuColorFrame,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    sequence_id.hash(&mut hasher);
    frame.hash(&mut hasher);
    target_resolution.width.hash(&mut hasher);
    target_resolution.height.hash(&mut hasher);
    working.descriptor().hash(&mut hasher);
    for pixel in &working.rgba_f32().data {
        for channel in pixel {
            channel.to_bits().hash(&mut hasher);
        }
    }
    hasher.finish()
}

enum PreviewTimelineAbort {
    Pending {
        dependency: PreviewTimelinePendingDependency,
    },
    Unavailable(PreviewUnavailability),
}

#[cfg(test)]
#[path = "preview_timeline_execution/tests.rs"]
mod tests;
