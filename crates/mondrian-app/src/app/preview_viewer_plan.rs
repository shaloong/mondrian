//! Stable viewer-plan representation and lowering.
//!
//! Timeline evaluation produces [`ResolvedPreviewElement`] values. This module
//! gives that result a stable cache identity and lowers the supported subset to
//! renderer-owned GPU execution layers. It deliberately contains no decode,
//! scheduling, presentation, or diagnostics side effects.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use mondrian_core::types::{BlendMode, SequenceId};
use mondrian_core::WorkingColorSpace;
use mondrian_effects::{
    get_or_lower_effect_graph_to_gpu_plan, CompiledEffectGraph, EffectCachePolicy,
};
use mondrian_playback::FramePresentationQuality;
use mondrian_renderer::{
    GpuCompositingBlockerReason, TimelineAdjustmentLayer, TimelineSolidColorLayer,
    ViewerGpuExecutionLayer, ViewerGpuSourceLayer, ViewerGpuTransitionInput,
};
use mondrian_timeline::sequence::ColorContext;

use super::preview_execution::{PreviewDecodeExecutionSummary, PreviewOutputKey};
use super::preview_media_frame::MediaPreviewFrame;

pub(crate) enum ResolvedPreviewElement {
    SolidColor(TimelineSolidColorLayer),
    Adjustment(TimelineAdjustmentLayer),
    Media {
        frame: MediaPreviewFrame,
        opacity: f32,
        blend_mode: BlendMode,
        transform: [f32; 6],
        effect_graph: Arc<CompiledEffectGraph>,
        frame_seed: i64,
    },
    CrossDissolve {
        left: ResolvedPreviewTransitionInput,
        right: ResolvedPreviewTransitionInput,
        progress: f32,
    },
}

pub(crate) enum ResolvedPreviewTransitionInput {
    Transparent,
    SolidColor(TimelineSolidColorLayer),
    Media {
        frame: MediaPreviewFrame,
        opacity: f32,
        blend_mode: BlendMode,
        transform: [f32; 6],
        effect_graph: Arc<CompiledEffectGraph>,
        frame_seed: i64,
    },
}

pub(crate) fn viewer_preview_cache_key_for_resolved_plan(
    sequence_id: SequenceId,
    width: u32,
    height: u32,
    elements: &[ResolvedPreviewElement],
    color_context: &ColorContext,
) -> PreviewOutputKey {
    let mut hasher = DefaultHasher::new();
    color_context.working_color_space.hash(&mut hasher);
    color_context.output_color_space.hash(&mut hasher);
    color_context.tone_map.hash(&mut hasher);
    color_context.engine.hash(&mut hasher);
    color_context.display_management.hash(&mut hasher);
    color_context.output_transform.hash(&mut hasher);
    mondrian_core::ocio_config_generation().hash(&mut hasher);
    elements.len().hash(&mut hasher);
    for element in elements {
        match element {
            ResolvedPreviewElement::SolidColor(solid) => {
                0u8.hash(&mut hasher);
                hash_color(solid.color, &mut hasher);
                solid.opacity.to_bits().hash(&mut hasher);
                solid.blend_mode.hash(&mut hasher);
                hash_transform(solid.transform, &mut hasher);
                hash_effect_graph_signature(&solid.effect_graph, solid.frame_seed, &mut hasher);
            }
            ResolvedPreviewElement::Adjustment(adjustment) => {
                1u8.hash(&mut hasher);
                adjustment.opacity.to_bits().hash(&mut hasher);
                adjustment.blend_mode.hash(&mut hasher);
                hash_effect_graph_signature(
                    &adjustment.effect_graph,
                    adjustment.frame_seed,
                    &mut hasher,
                );
            }
            ResolvedPreviewElement::Media {
                frame,
                opacity,
                blend_mode,
                transform,
                effect_graph,
                frame_seed,
            } => {
                2u8.hash(&mut hasher);
                frame.signature().hash(&mut hasher);
                frame.width().hash(&mut hasher);
                frame.height().hash(&mut hasher);
                opacity.to_bits().hash(&mut hasher);
                blend_mode.hash(&mut hasher);
                hash_transform(*transform, &mut hasher);
                hash_effect_graph_signature(effect_graph, *frame_seed, &mut hasher);
            }
            ResolvedPreviewElement::CrossDissolve { left, right, progress } => {
                3u8.hash(&mut hasher);
                hash_transition_input(left, &mut hasher);
                hash_transition_input(right, &mut hasher);
                progress.to_bits().hash(&mut hasher);
            }
        }
    }
    PreviewOutputKey::new(sequence_id, width, height, hasher.finish())
}

fn hash_transition_input(input: &ResolvedPreviewTransitionInput, hasher: &mut impl Hasher) {
    match input {
        ResolvedPreviewTransitionInput::Transparent => 0u8.hash(hasher),
        ResolvedPreviewTransitionInput::SolidColor(solid) => {
            1u8.hash(hasher);
            hash_color(solid.color, hasher);
            solid.opacity.to_bits().hash(hasher);
            solid.blend_mode.hash(hasher);
            hash_transform(solid.transform, hasher);
            hash_effect_graph_signature(&solid.effect_graph, solid.frame_seed, hasher);
        }
        ResolvedPreviewTransitionInput::Media {
            frame,
            opacity,
            blend_mode,
            transform,
            effect_graph,
            frame_seed,
        } => {
            2u8.hash(hasher);
            frame.signature().hash(hasher);
            frame.width().hash(hasher);
            frame.height().hash(hasher);
            opacity.to_bits().hash(hasher);
            blend_mode.hash(hasher);
            hash_transform(*transform, hasher);
            hash_effect_graph_signature(effect_graph, *frame_seed, hasher);
        }
    }
}

fn hash_color(color: mondrian_core::Color, hasher: &mut impl Hasher) {
    color.r.to_bits().hash(hasher);
    color.g.to_bits().hash(hasher);
    color.b.to_bits().hash(hasher);
    color.a.to_bits().hash(hasher);
}

fn hash_transform(transform: [f32; 6], hasher: &mut impl Hasher) {
    for value in transform {
        value.to_bits().hash(hasher);
    }
}

fn hash_effect_graph_signature(
    graph: &CompiledEffectGraph,
    frame_seed: i64,
    hasher: &mut impl Hasher,
) {
    graph.signature_hash.hash(hasher);
    graph.output_cache_policy.hash(hasher);
    if graph.output_cache_policy == EffectCachePolicy::FrameDependent {
        frame_seed.hash(hasher);
    }
}

pub(crate) fn gpu_composite_layers_for_resolved(
    resolved: &[ResolvedPreviewElement],
    working_color_space: WorkingColorSpace,
) -> Result<Vec<ViewerGpuExecutionLayer>, GpuCompositingBlockerReason> {
    let mut layers = Vec::with_capacity(resolved.len());
    let mut has_composited_layer = false;
    for element in resolved {
        match element {
            ResolvedPreviewElement::Media {
                frame,
                opacity,
                blend_mode,
                transform,
                effect_graph,
                frame_seed,
            } => {
                layers.push(ViewerGpuExecutionLayer::Source(gpu_media_source(
                    frame,
                    *opacity,
                    *blend_mode,
                    *transform,
                    effect_graph,
                    *frame_seed,
                    working_color_space,
                )?));
                has_composited_layer |= opacity.clamp(0.0, 1.0) > 0.0;
            }
            ResolvedPreviewElement::SolidColor(layer) => {
                layers.push(ViewerGpuExecutionLayer::Source(gpu_solid_source(layer)?));
                has_composited_layer |= layer.opacity.clamp(0.0, 1.0) > 0.0;
            }
            ResolvedPreviewElement::Adjustment(layer) => {
                if !has_composited_layer
                    || layer.opacity <= 1.0e-4
                    || layer.effect_graph.graph.is_identity()
                {
                    continue;
                }
                let blend_mode = layer.blend_mode.unwrap_or(BlendMode::Normal);
                if blend_mode != BlendMode::Normal {
                    return Err(GpuCompositingBlockerReason::UnsupportedBlendMode);
                }
                let effect_plan = get_or_lower_effect_graph_to_gpu_plan(&layer.effect_graph)
                    .map_err(|_| GpuCompositingBlockerReason::EffectRequiresCpu)?;
                layers.push(ViewerGpuExecutionLayer::Adjustment {
                    effect_plan,
                    opacity: layer.opacity,
                    blend_mode,
                    frame_seed: layer.frame_seed,
                });
            }
            ResolvedPreviewElement::CrossDissolve { left, right, progress } => {
                let progress = progress.clamp(0.0, 1.0);
                layers.push(ViewerGpuExecutionLayer::CrossDissolve {
                    left: gpu_transition_input(left, working_color_space)?,
                    right: gpu_transition_input(right, working_color_space)?,
                    progress,
                });
                has_composited_layer |= transition_input_has_contribution(left, 1.0 - progress)
                    || transition_input_has_contribution(right, progress);
            }
        }
    }
    if layers.len() > 5 {
        return Err(GpuCompositingBlockerReason::TooManyLayers);
    }
    Ok(layers)
}

pub(crate) fn preview_elements_require_deferred_composite(
    resolved: &[ResolvedPreviewElement],
) -> bool {
    resolved.iter().any(|element| match element {
        ResolvedPreviewElement::Media { .. } => true,
        ResolvedPreviewElement::CrossDissolve { left, right, .. } => {
            transition_input_contains_media(left) || transition_input_contains_media(right)
        }
        ResolvedPreviewElement::SolidColor(_) | ResolvedPreviewElement::Adjustment(_) => false,
    })
}

#[allow(clippy::too_many_arguments)]
fn gpu_media_source(
    frame: &MediaPreviewFrame,
    opacity: f32,
    blend_mode: BlendMode,
    transform: [f32; 6],
    effect_graph: &Arc<CompiledEffectGraph>,
    frame_seed: i64,
    working_color_space: WorkingColorSpace,
) -> Result<ViewerGpuSourceLayer, GpuCompositingBlockerReason> {
    if blend_mode != BlendMode::Normal {
        return Err(GpuCompositingBlockerReason::UnsupportedBlendMode);
    }
    let layer_working_color_space =
        frame.working_color_space().ok_or(GpuCompositingBlockerReason::GpuUnavailable)?;
    if layer_working_color_space != working_color_space
        || !is_preview_gpu_transform_supported(transform)
    {
        return Err(GpuCompositingBlockerReason::UnsupportedTransform);
    }
    let effect_plan = get_or_lower_effect_graph_to_gpu_plan(effect_graph)
        .map_err(|_| GpuCompositingBlockerReason::EffectRequiresCpu)?;
    Ok(ViewerGpuSourceLayer::Media {
        frame: frame.working_payload(),
        gpu_source: frame.gpu_source(),
        native_source: frame.native_source(),
        opacity,
        transform,
        effect_plan,
        frame_seed,
    })
}

fn gpu_solid_source(
    layer: &TimelineSolidColorLayer,
) -> Result<ViewerGpuSourceLayer, GpuCompositingBlockerReason> {
    if layer.blend_mode != BlendMode::Normal {
        return Err(GpuCompositingBlockerReason::UnsupportedBlendMode);
    }
    if !is_preview_gpu_transform_supported(layer.transform) {
        return Err(GpuCompositingBlockerReason::UnsupportedTransform);
    }
    let effect_plan = get_or_lower_effect_graph_to_gpu_plan(&layer.effect_graph)
        .map_err(|_| GpuCompositingBlockerReason::EffectRequiresCpu)?;
    Ok(ViewerGpuSourceLayer::SolidColor { layer: layer.clone(), effect_plan })
}

fn gpu_transition_input(
    input: &ResolvedPreviewTransitionInput,
    working_color_space: WorkingColorSpace,
) -> Result<ViewerGpuTransitionInput, GpuCompositingBlockerReason> {
    let source = match input {
        ResolvedPreviewTransitionInput::Transparent => {
            return Ok(ViewerGpuTransitionInput::Transparent);
        }
        ResolvedPreviewTransitionInput::SolidColor(layer) => gpu_solid_source(layer)?,
        ResolvedPreviewTransitionInput::Media {
            frame,
            opacity,
            blend_mode,
            transform,
            effect_graph,
            frame_seed,
        } => gpu_media_source(
            frame,
            *opacity,
            *blend_mode,
            *transform,
            effect_graph,
            *frame_seed,
            working_color_space,
        )?,
    };
    Ok(ViewerGpuTransitionInput::Source(source))
}

fn transition_input_has_contribution(
    input: &ResolvedPreviewTransitionInput,
    transition_weight: f32,
) -> bool {
    if transition_weight <= 0.0 {
        return false;
    }
    match input {
        ResolvedPreviewTransitionInput::Transparent => false,
        ResolvedPreviewTransitionInput::SolidColor(layer) => layer.opacity.clamp(0.0, 1.0) > 0.0,
        ResolvedPreviewTransitionInput::Media { opacity, .. } => opacity.clamp(0.0, 1.0) > 0.0,
    }
}

fn transition_input_contains_media(input: &ResolvedPreviewTransitionInput) -> bool {
    matches!(input, ResolvedPreviewTransitionInput::Media { .. })
}

pub(crate) fn resolved_preview_presentation_quality(
    resolved: &[ResolvedPreviewElement],
) -> FramePresentationQuality {
    if resolved.iter().any(resolved_element_is_degraded) {
        FramePresentationQuality::Degraded
    } else {
        FramePresentationQuality::Ready
    }
}

fn resolved_element_is_degraded(element: &ResolvedPreviewElement) -> bool {
    match element {
        ResolvedPreviewElement::Media { frame, .. } => {
            frame.presentation_quality() == FramePresentationQuality::Degraded
        }
        ResolvedPreviewElement::CrossDissolve { left, right, .. } => {
            transition_input_is_degraded(left) || transition_input_is_degraded(right)
        }
        ResolvedPreviewElement::SolidColor(_) | ResolvedPreviewElement::Adjustment(_) => false,
    }
}

fn transition_input_is_degraded(input: &ResolvedPreviewTransitionInput) -> bool {
    matches!(
        input,
        ResolvedPreviewTransitionInput::Media { frame, .. }
            if frame.presentation_quality() == FramePresentationQuality::Degraded
    )
}

pub(crate) fn resolved_preview_decode_execution(
    resolved: &[ResolvedPreviewElement],
) -> PreviewDecodeExecutionSummary {
    let mut summary = PreviewDecodeExecutionSummary::default();
    for element in resolved {
        match element {
            ResolvedPreviewElement::Media { frame, .. } => {
                summary.accumulate(frame.decode_execution());
            }
            ResolvedPreviewElement::CrossDissolve { left, right, .. } => {
                for input in [left, right] {
                    if let ResolvedPreviewTransitionInput::Media { frame, .. } = input {
                        summary.accumulate(frame.decode_execution());
                    }
                }
            }
            ResolvedPreviewElement::SolidColor(_) | ResolvedPreviewElement::Adjustment(_) => {}
        }
    }
    summary
}

fn is_preview_gpu_transform_supported(transform: [f32; 6]) -> bool {
    if !transform.iter().all(|value| value.is_finite()) {
        return false;
    }
    let det = transform[0] * transform[4] - transform[3] * transform[1];
    det.abs() > 1.0e-8
}
