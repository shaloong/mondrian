//! Recursive Timeline evaluation for Viewer preview.
//!
//! This Module owns traversal from the canonical Timeline render plan into
//! resolved Viewer elements. Decode scheduling and final Viewer execution stay
//! behind their existing seams; callers submit one Sequence/frame request.

use super::*;

pub(super) struct ResolvedPreviewPlan {
    pub(super) elements: Vec<ResolvedPreviewElement>,
    pub(super) cache_key: Option<ViewerPreviewCacheKey>,
    pub(super) color_context: ColorContext,
}

impl AppUiPreviewService {
    pub(super) fn render_nested_sequence_frame(
        &self,
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        depth: usize,
        parent_color_context: ColorContext,
    ) -> Option<MediaPreviewFrame> {
        if depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH {
            return None;
        }
        let (width, height) = preview_dimensions_for_state(state, sequence);
        let parent_working_color_space = parent_color_context.working_color_space;
        let color_context = sequence.settings.nested_render_color_context(parent_color_context);
        let resolved = self
            .resolve_sequence_elements(
                state,
                sequence,
                frame.max(0),
                width,
                height,
                depth,
                color_context.clone(),
            )?
            .elements;
        let presentation_quality = resolved_preview_presentation_quality(&resolved);
        let decode_execution = resolved_preview_decode_execution(&resolved);
        let mut scratch = TimelineCompositeScratch::default();
        let output = composite_resolved_preview_working(
            width,
            height,
            &resolved,
            &color_context,
            &mut scratch,
        )
        .ok()?;
        let render_stage_durations = output.render_stage_durations;
        let render_total_us = render_stage_durations
            .working_prepare_us
            .saturating_add(render_stage_durations.cpu_composite_us);
        self.record_composite(output.composite_diagnostics);
        for diagnostics in output.input_color_diagnostics {
            self.record_color_transform(diagnostics);
        }
        self.record_color_stage(output.input_color_stage_diagnostics);
        self.record_render_stage_durations(render_total_us, render_stage_durations);
        let mut working_frame = output.frame;
        if working_frame.descriptor().color_space.working() != Some(parent_working_color_space) {
            let converted = execute_cpu_working_transform(
                &working_frame,
                parent_working_color_space,
                color_context.engine.clone(),
            )
            .ok()?;
            self.record_color_transform(converted.result.diagnostics);
            self.record_color_stage(converted.stage_diagnostics);
            working_frame = converted.result.frame;
        }
        let signature = nested_preview_frame_signature(
            sequence.id,
            frame.max(0),
            width,
            height,
            &working_frame,
        );
        Some(MediaPreviewFrame::from_working(
            working_frame,
            sequence.settings.resolution,
            signature,
            presentation_quality,
            decode_execution,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn resolve_sequence_elements(
        &self,
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        width: u32,
        height: u32,
        depth: usize,
        color_context: ColorContext,
    ) -> Option<ResolvedPreviewPlan> {
        let evaluation = evaluate_timeline_render_plan(
            sequence,
            TimelineEvaluationRequest::preview(
                frame,
                normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale),
            ),
        )
        .ok()?;
        if evaluation.is_empty() {
            return None;
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
                    let frame = self.media_frame_for_plan(
                        state,
                        &media.asset_id,
                        media.color_space_override,
                        media.alpha_interpretation,
                        media.source_frame,
                        media.source_secs,
                        width,
                        height,
                        &color_context,
                        sequence.settings.frame_rate,
                    )?;
                    let transform = project_preview_media_transform(
                        media.transform,
                        &frame,
                        sequence.settings.resolution,
                        Resolution { width, height },
                    )?;
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
                    let nested_sequence = state.sequence_by_id(nested.sequence_id)?;
                    let frame = self.render_nested_sequence_frame(
                        state,
                        nested_sequence,
                        nested.source_frame,
                        depth + 1,
                        color_context.clone(),
                    )?;
                    let transform = project_preview_media_transform(
                        nested.transform,
                        &frame,
                        sequence.settings.resolution,
                        Resolution { width, height },
                    )?;
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
        let cache_key = Some(viewer_preview_cache_key_for_resolved_plan(
            sequence.id,
            width,
            height,
            &resolved,
            &color_context,
        ));

        Some(ResolvedPreviewPlan { elements: resolved, cache_key, color_context })
    }
}
