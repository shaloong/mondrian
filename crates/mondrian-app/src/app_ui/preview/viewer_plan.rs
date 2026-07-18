//! Stable viewer-plan representation and lowering.
//!
//! Timeline evaluation produces [`ResolvedPreviewElement`] values. This module
//! gives that result a stable cache identity and lowers the supported subset to
//! renderer-owned GPU execution layers. It deliberately contains no decode,
//! scheduling, presentation, or diagnostics side effects.

use super::*;

pub(super) enum ResolvedPreviewElement {
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
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ViewerPreviewCacheKey {
    pub(super) sequence_id: SequenceId,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) plan_signature: u64,
}

impl ViewerPreviewCacheKey {
    pub(super) fn with_monitor_adaptation(&self, adaptation: &RenderMonitorAdaptation) -> Self {
        let mut hasher = DefaultHasher::new();
        self.plan_signature.hash(&mut hasher);
        adaptation.hash(&mut hasher);
        Self {
            sequence_id: self.sequence_id,
            width: self.width,
            height: self.height,
            plan_signature: hasher.finish(),
        }
    }
}

pub(super) fn viewer_preview_cache_key_for_resolved_plan(
    sequence_id: SequenceId,
    width: u32,
    height: u32,
    elements: &[ResolvedPreviewElement],
    color_context: &ColorContext,
) -> ViewerPreviewCacheKey {
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
                frame.signature.hash(&mut hasher);
                frame.width().hash(&mut hasher);
                frame.height().hash(&mut hasher);
                opacity.to_bits().hash(&mut hasher);
                blend_mode.hash(&mut hasher);
                hash_transform(*transform, &mut hasher);
                hash_effect_graph_signature(effect_graph, *frame_seed, &mut hasher);
            }
        }
    }
    ViewerPreviewCacheKey {
        sequence_id,
        width,
        height,
        plan_signature: hasher.finish(),
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

pub(super) fn gpu_composite_layers_for_resolved(
    _width: u32,
    _height: u32,
    resolved: &[ResolvedPreviewElement],
    working_color_space: WorkingColorSpace,
) -> Result<Vec<mondrian_renderer::ViewerGpuExecutionLayer>, GpuCompositingBlockerReason> {
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
                let effect_plan = get_or_lower_effect_graph_to_gpu_plan(effect_graph)
                    .map_err(|_| GpuCompositingBlockerReason::EffectRequiresCpu)?;
                if *blend_mode != BlendMode::Normal {
                    return Err(GpuCompositingBlockerReason::UnsupportedBlendMode);
                }
                let layer_working_color_space = frame
                    .frame
                    .as_ref()
                    .and_then(|working| working.descriptor().color_space.working())
                    .or_else(|| {
                        frame
                            .gpu_source
                            .as_ref()
                            .map(|source| source.input_transform.working_color_space)
                    })
                    .or_else(|| {
                        frame
                            .native_source
                            .as_ref()
                            .map(|source| source.input_transform.working_color_space)
                    })
                    .ok_or(GpuCompositingBlockerReason::GpuUnavailable)?;
                if layer_working_color_space != working_color_space {
                    return Err(GpuCompositingBlockerReason::UnsupportedTransform);
                }
                if !is_preview_gpu_media_transform_supported(*transform) {
                    return Err(GpuCompositingBlockerReason::UnsupportedTransform);
                }
                layers.push(mondrian_renderer::ViewerGpuExecutionLayer::Media {
                    frame: frame.frame.clone(),
                    gpu_source: frame.gpu_source(),
                    native_source: frame.native_source(),
                    opacity: *opacity,
                    transform: *transform,
                    effect_plan,
                    frame_seed: *frame_seed,
                });
                has_composited_layer = true;
            }
            ResolvedPreviewElement::SolidColor(layer) => {
                let effect_plan = get_or_lower_effect_graph_to_gpu_plan(&layer.effect_graph)
                    .map_err(|_| GpuCompositingBlockerReason::EffectRequiresCpu)?;
                if layer.blend_mode != BlendMode::Normal {
                    return Err(GpuCompositingBlockerReason::UnsupportedBlendMode);
                }
                if !is_preview_identity_transform(layer.transform) {
                    return Err(GpuCompositingBlockerReason::UnsupportedTransform);
                }
                layers.push(mondrian_renderer::ViewerGpuExecutionLayer::SolidColor {
                    layer: layer.clone(),
                    effect_plan,
                });
                has_composited_layer = true;
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
                layers.push(mondrian_renderer::ViewerGpuExecutionLayer::Adjustment {
                    effect_plan,
                    opacity: layer.opacity,
                    blend_mode,
                    frame_seed: layer.frame_seed,
                });
            }
        }
    }
    if layers.len() > 5 {
        return Err(GpuCompositingBlockerReason::TooManyLayers);
    }
    Ok(layers)
}

pub(super) fn preview_elements_require_deferred_composite(
    resolved: &[ResolvedPreviewElement],
) -> bool {
    resolved
        .iter()
        .any(|element| matches!(element, ResolvedPreviewElement::Media { .. }))
}

pub(super) fn resolved_preview_presentation_quality(
    resolved: &[ResolvedPreviewElement],
) -> mondrian_playback::FramePresentationQuality {
    if resolved.iter().any(|element| {
        matches!(
            element,
            ResolvedPreviewElement::Media { frame, .. }
                if frame.presentation_quality()
                    == mondrian_playback::FramePresentationQuality::Degraded
        )
    }) {
        mondrian_playback::FramePresentationQuality::Degraded
    } else {
        mondrian_playback::FramePresentationQuality::Ready
    }
}

pub(super) fn resolved_preview_decode_execution(
    resolved: &[ResolvedPreviewElement],
) -> AppUiPreviewDecodeExecutionSummary {
    let mut summary = AppUiPreviewDecodeExecutionSummary::default();
    for element in resolved {
        if let ResolvedPreviewElement::Media { frame, .. } = element {
            summary.accumulate(frame.decode_execution());
        }
    }
    summary
}

fn is_preview_identity_transform(transform: [f32; 6]) -> bool {
    const EPSILON: f32 = 1.0e-6;
    (transform[0] - 1.0).abs() <= EPSILON
        && transform[1].abs() <= EPSILON
        && transform[2].abs() <= EPSILON
        && transform[3].abs() <= EPSILON
        && (transform[4] - 1.0).abs() <= EPSILON
        && transform[5].abs() <= EPSILON
}

fn is_preview_gpu_media_transform_supported(transform: [f32; 6]) -> bool {
    let det = transform[0] * transform[4] - transform[3] * transform[1];
    det.abs() > 1.0e-8
}

pub(super) fn viewer_raster_frame_key(cache_key: &ViewerPreviewCacheKey) -> String {
    format!(
        "app-ui.viewer.raster:{}:{}x{}:{:016x}",
        cache_key.sequence_id, cache_key.width, cache_key.height, cache_key.plan_signature
    )
}

pub(super) fn uncached_viewer_raster_frame_key(
    sequence_id: SequenceId,
    frame: i64,
    width: u32,
    height: u32,
) -> String {
    format!(
        "app-ui.viewer.raster-uncached:{sequence_id}:{width}x{height}:f{}",
        frame.max(0)
    )
}
