//! UI-independent recursive Timeline execution for Viewer Preview.
//!
//! This deep Module owns canonical render-plan traversal, nested Sequence
//! lookup and depth limits, nested working-space composition/conversion,
//! resolved Viewer elements, and their stable cache identity. A caller supplies
//! only immutable Sequence snapshots and a narrow media-frame closure; Window
//! and Headless Adapters cannot maintain separate recursion or color math.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use mondrian_core::timeline_data::AlphaInterpretation;
use mondrian_core::types::{AssetId, ColorSpace, SequenceId};
use mondrian_core::{FrameRounding, Resolution, TimelineTime};
use mondrian_playback::PreviewResolutionScale;
use mondrian_renderer::{
    evaluate_timeline_render_plan, execute_cpu_working_transform, CpuColorFrame,
    RenderColorStageDiagnostics, RenderColorTransformDiagnostics, TimelineAdjustmentLayer,
    TimelineCompositeDiagnostics, TimelineCompositeScratch, TimelineEvaluationRequest,
    TimelineMediaPlan, TimelineRenderPlan, TimelineRenderPlanElement, TimelineSolidColorLayer,
};
use mondrian_timeline::sequence::{ColorContext, Sequence, MAX_NESTED_SEQUENCE_RENDER_DEPTH};

use super::preview_cpu_execution::{
    composite_resolved_preview_working, PreviewCpuExecutionDurations,
};
use super::preview_execution::PreviewOutputKey;
use super::preview_media_frame::{project_preview_media_transform, MediaPreviewFrame};
use super::preview_quality::{normalize_preview_resolution_scale, preview_execution_resolution};
use super::preview_viewer_plan::{
    resolved_preview_decode_execution, resolved_preview_presentation_quality,
    viewer_preview_cache_key_for_resolved_plan, ResolvedPreviewElement,
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
    pub(crate) color_context: ColorContext,
}

/// Exhaustive Adapter response for one media layer needed by Timeline execution.
pub(crate) enum PreviewTimelineMediaFrame {
    Ready(MediaPreviewFrame),
    Pending,
    Unavailable { reason: String },
}

/// Resolved root Viewer plan with an always-present semantic cache identity.
pub(crate) struct ResolvedPreviewPlan {
    pub(crate) elements: Vec<ResolvedPreviewElement>,
    pub(crate) cache_key: PreviewOutputKey,
    pub(crate) color_context: ColorContext,
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
    Pending { asset_id: AssetId },
    Unavailable { reason: String },
}

/// Why canonical media-demand traversal could not describe a complete frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreviewTimelineDemandError {
    pub(crate) reason: String,
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
    color_context: ColorContext,
) -> Result<Vec<PreviewTimelineMediaRequest>, PreviewTimelineDemandError> {
    let graph = PreviewTimelineGraph { root_sequence: sequence, sequences, runtime_scale };
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
    color_context: ColorContext,
    media_frame: &mut impl FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
) -> PreviewTimelineResolution {
    let graph = PreviewTimelineGraph { root_sequence: sequence, sequences, runtime_scale };
    let mut execution = PreviewTimelineExecutionContext { graph, media_frame, facts: Vec::new() };
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
        Err(PreviewTimelineAbort::Pending { asset_id }) => {
            return PreviewTimelineResolution::Pending { asset_id };
        }
        Err(PreviewTimelineAbort::Unavailable { reason }) => {
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
}

struct NestedPreviewSequence<'a> {
    sequence: &'a Sequence,
    target_resolution: Resolution,
    color_context: ColorContext,
}

impl<'a> PreviewTimelineGraph<'a> {
    fn evaluate(
        self,
        sequence: &Sequence,
        frame: i64,
    ) -> Result<TimelineRenderPlan, PreviewTimelineDemandError> {
        evaluate_timeline_render_plan(
            sequence,
            TimelineEvaluationRequest::preview(
                frame.max(0),
                normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale),
            ),
        )
        .map_err(|error| PreviewTimelineDemandError { reason: error.to_string() })
    }

    fn nested(
        self,
        sequence_id: SequenceId,
        parent_color_context: &ColorContext,
    ) -> Result<NestedPreviewSequence<'a>, PreviewTimelineDemandError> {
        let sequence =
            sequence_by_id(self.root_sequence, self.sequences, sequence_id).ok_or_else(|| {
                PreviewTimelineDemandError {
                    reason: format!("nested Sequence {sequence_id} does not exist"),
                }
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
                .nested_render_color_context(parent_color_context.clone()),
        })
    }
}

struct PreviewTimelineExecutionContext<'a, MediaFrame> {
    graph: PreviewTimelineGraph<'a>,
    media_frame: &'a mut MediaFrame,
    facts: Vec<PreviewTimelineExecutionFact>,
}

fn collect_sequence_media_demands(
    graph: PreviewTimelineGraph<'_>,
    sequence: &Sequence,
    frame: i64,
    target_resolution: Resolution,
    depth: usize,
    color_context: ColorContext,
    demands: &mut Vec<PreviewTimelineMediaRequest>,
) -> Result<(), PreviewTimelineDemandError> {
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
                let nested = graph.nested(nested_plan.sequence_id, &color_context)?;
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
            TimelineRenderPlanElement::SolidColor(_) | TimelineRenderPlanElement::Adjustment(_) => {
            }
        }
    }
    Ok(())
}

fn resolve_sequence_elements<MediaFrame>(
    execution: &mut PreviewTimelineExecutionContext<'_, MediaFrame>,
    sequence: &Sequence,
    frame: i64,
    target_resolution: Resolution,
    depth: usize,
    color_context: ColorContext,
) -> Result<Option<Vec<ResolvedPreviewElement>>, PreviewTimelineAbort>
where
    MediaFrame: FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
{
    validate_nested_depth(sequence, depth)
        .map_err(|error| PreviewTimelineAbort::Unavailable { reason: error.reason })?;
    let evaluation = execution
        .graph
        .evaluate(sequence, frame)
        .map_err(|error| PreviewTimelineAbort::Unavailable { reason: error.reason })?;
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
                        return Err(PreviewTimelineAbort::Pending { asset_id });
                    }
                    PreviewTimelineMediaFrame::Unavailable { reason } => {
                        return Err(PreviewTimelineAbort::Unavailable {
                            reason: format!("media {asset_id} unavailable: {reason}"),
                        });
                    }
                };
                let transform = project_preview_media_transform(
                    media.transform,
                    &frame,
                    sequence.settings.resolution,
                    target_resolution,
                )
                .ok_or_else(|| PreviewTimelineAbort::Unavailable {
                    reason: format!("media {asset_id} has invalid Preview transform geometry"),
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
                    .nested(nested.sequence_id, &color_context)
                    .map_err(|error| PreviewTimelineAbort::Unavailable { reason: error.reason })?;
                let nested_sequence = nested_execution.sequence;
                let nested_frame = nested_sequence_frame(nested.source_time, nested_sequence)
                    .map_err(|error| PreviewTimelineAbort::Unavailable { reason: error.reason })?;
                let parent_working_color_space = color_context.working_color_space;
                let nested_elements = resolve_sequence_elements(
                    execution,
                    nested_sequence,
                    nested_frame,
                    nested_execution.target_resolution,
                    depth + 1,
                    nested_execution.color_context.clone(),
                )?
                .ok_or_else(|| PreviewTimelineAbort::Unavailable {
                    reason: format!(
                        "nested Sequence {} resolved to no elements",
                        nested.sequence_id
                    ),
                })?;
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
                .ok_or_else(|| PreviewTimelineAbort::Unavailable {
                    reason: format!(
                        "nested Sequence {} has invalid Preview transform geometry",
                        nested.sequence_id
                    ),
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
        }
    }
    Ok(Some(resolved))
}

fn render_nested_sequence(
    sequence: &Sequence,
    frame: i64,
    target_resolution: Resolution,
    parent_working_color_space: mondrian_core::WorkingColorSpace,
    color_context: ColorContext,
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
    .map_err(|reason| PreviewTimelineAbort::Unavailable {
        reason: format!(
            "nested Sequence {} composition failed: {reason}",
            sequence.id
        ),
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
        .map_err(|error| PreviewTimelineAbort::Unavailable {
            reason: format!(
                "nested Sequence {} working conversion failed: {error}",
                sequence.id
            ),
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
    color_context: &ColorContext,
) -> PreviewTimelineMediaRequest {
    PreviewTimelineMediaRequest {
        asset_id: media.asset_id,
        color_space_override: media.color_space_override,
        alpha_interpretation: media.alpha_interpretation,
        source_time: media.source_time,
        target_resolution,
        color_context: color_context.clone(),
    }
}

fn nested_sequence_frame(
    source_time: TimelineTime,
    sequence: &Sequence,
) -> Result<i64, PreviewTimelineDemandError> {
    source_time
        .to_frame_position(sequence.settings.frame_rate, FrameRounding::Floor)
        .map(|position| position.frame.max(0))
        .map_err(|error| PreviewTimelineDemandError {
            reason: format!(
                "nested Sequence {} source target {source_time} is invalid: {error}",
                sequence.id
            ),
        })
}

fn validate_nested_depth(
    sequence: &Sequence,
    depth: usize,
) -> Result<(), PreviewTimelineDemandError> {
    if depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH {
        Err(PreviewTimelineDemandError {
            reason: format!(
                "nested Sequence depth exceeded {} at {}",
                MAX_NESTED_SEQUENCE_RENDER_DEPTH, sequence.id
            ),
        })
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
    Pending { asset_id: AssetId },
    Unavailable { reason: String },
}

#[cfg(test)]
#[path = "preview_timeline_execution/tests.rs"]
mod tests;
