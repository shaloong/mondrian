//! Stable viewer-plan representation and lowering.
//!
//! Timeline evaluation produces [`ResolvedPreviewElement`] values. This module
//! gives that result a stable cache identity and lowers the supported subset to
//! renderer-owned GPU execution layers. It deliberately contains no decode,
//! scheduling, presentation, or diagnostics side effects. Production lowering
//! receives the Preview-owned Effect Session explicitly; there is no hidden
//! process-global planning residency.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use mondrian_core::types::{BlendMode, SequenceId};
use mondrian_core::WorkingColorSpace;
use mondrian_effects::{identity_compiled_effect_graph, CompiledEffectGraph, EffectFrameExtent};
use mondrian_playback::FramePresentationQuality;
use mondrian_renderer::{
    CpuColorFrame, GpuCompositingBlockerReason, HeterogeneousCpuPrefixBatchError,
    HeterogeneousCpuPrefixBatchGrant, HeterogeneousCpuPrefixBatchItem,
    HeterogeneousCpuPrefixBatchRequest, HeterogeneousGpuContinuationBinding,
    HeterogeneousGpuContinuationRequest, HeterogeneousGpuResourceGrant,
    PreparedHeterogeneousEffectRoute, TimelineAdjustmentLayer, TimelineCompositeScratch,
    TimelineSolidColorLayer, ViewerGpuCrossDissolveLayer, ViewerGpuExecutionLayer,
    ViewerGpuSourceLayer, ViewerGpuTransitionInput,
};
use mondrian_timeline::sequence::ProgramColorContext;

use super::preview_execution::{
    PreviewDecodeExecutionSummary, PreviewOutputKey, PreviewSemanticIdentity,
    PreviewSemanticIdentityBuilder,
};
use super::preview_media_frame::MediaPreviewFrame;

pub(crate) enum ResolvedPreviewElement {
    SolidColor(TimelineSolidColorLayer),
    HeterogeneousSolidColor {
        layer: TimelineSolidColorLayer,
        prepared_route: Box<PreparedHeterogeneousEffectRoute>,
    },
    Adjustment(TimelineAdjustmentLayer),
    Media {
        frame: MediaPreviewFrame,
        opacity: f32,
        blend_mode: BlendMode,
        transform: [f32; 6],
        effect_graph: Arc<CompiledEffectGraph>,
        prepared_heterogeneous_route: Option<Box<PreparedHeterogeneousEffectRoute>>,
        frame_seed: i64,
    },
    CrossDissolve {
        left: Box<ResolvedPreviewTransitionInput>,
        right: Box<ResolvedPreviewTransitionInput>,
        progress: f32,
    },
}

pub(crate) enum ResolvedPreviewTransitionInput {
    Transparent,
    SolidColor(TimelineSolidColorLayer),
    HeterogeneousSolidColor {
        layer: TimelineSolidColorLayer,
        prepared_route: Box<PreparedHeterogeneousEffectRoute>,
    },
    Media {
        frame: MediaPreviewFrame,
        opacity: f32,
        blend_mode: BlendMode,
        transform: [f32; 6],
        effect_graph: Arc<CompiledEffectGraph>,
        prepared_heterogeneous_route: Option<Box<PreparedHeterogeneousEffectRoute>>,
        frame_seed: i64,
    },
}

/// Retain every Store protection needed by one current Viewer candidate.
pub(crate) fn resolved_preview_media_protections(
    elements: &[ResolvedPreviewElement],
) -> Vec<mondrian_playback::MediaFrameProtectionLease> {
    let mut protections = Vec::new();
    for element in elements {
        match element {
            ResolvedPreviewElement::Media { frame, .. } => {
                protections.extend(frame.residency_protection());
            }
            ResolvedPreviewElement::CrossDissolve { left, right, .. } => {
                protections.extend(transition_input_protection(left));
                protections.extend(transition_input_protection(right));
            }
            ResolvedPreviewElement::SolidColor(_)
            | ResolvedPreviewElement::HeterogeneousSolidColor { .. }
            | ResolvedPreviewElement::Adjustment(_) => {}
        }
    }
    protections
}

fn transition_input_protection(
    input: &ResolvedPreviewTransitionInput,
) -> Option<mondrian_playback::MediaFrameProtectionLease> {
    match input {
        ResolvedPreviewTransitionInput::Media { frame, .. } => frame.residency_protection(),
        ResolvedPreviewTransitionInput::Transparent
        | ResolvedPreviewTransitionInput::SolidColor(_)
        | ResolvedPreviewTransitionInput::HeterogeneousSolidColor { .. } => None,
    }
}

/// Pure Viewer lowering result before any CPU-prefix work is scheduled.
///
/// `Ordinary` retains the existing full-GPU path. `Heterogeneous` carries an
/// immutable renderer batch request plus address metadata; no CPU pixels are
/// executed and no move-only completion exists at this planning seam.
pub(crate) enum PreparedPreviewViewerGpuLayers {
    Ordinary {
        layers: Vec<ViewerGpuExecutionLayer>,
    },
    Heterogeneous {
        layers: Vec<ViewerGpuExecutionLayer>,
        cpu_prefix: HeterogeneousCpuPrefixBatchRequest,
        continuations: Box<[PreviewHeterogeneousGpuContinuationMetadata]>,
    },
}

/// Immutable address binding retained until a CPU completion can be paired
/// with its exact GPU continuation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewHeterogeneousGpuContinuationMetadata {
    address: u32,
    graph_fingerprint: [u8; 32],
    frame_extent: EffectFrameExtent,
    frame_seed: i64,
    working_color_space: WorkingColorSpace,
}

impl PreviewHeterogeneousGpuContinuationMetadata {
    /// Address shared by the CPU batch and Viewer layer placeholder.
    pub(crate) const fn address(self) -> u32 {
        self.address
    }

    /// Build the exact renderer continuation binding after the CPU prefix
    /// completes under the selected Preview generation.
    pub(crate) const fn gpu_continuation_request(
        self,
        generation: u64,
        grant: HeterogeneousGpuResourceGrant,
    ) -> HeterogeneousGpuContinuationRequest {
        HeterogeneousGpuContinuationRequest::new(
            HeterogeneousGpuContinuationBinding::new(
                self.graph_fingerprint,
                generation,
                self.frame_extent,
                self.frame_seed,
                self.working_color_space,
            ),
            grant,
        )
    }
}

/// Fail-closed reason why a resolved Viewer plan could not enter the ordinary
/// or explicit heterogeneous GPU path.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PreviewViewerGpuLayerPreparationError {
    #[error("Viewer GPU compositing is blocked by {reason:?}")]
    Compositing { reason: GpuCompositingBlockerReason },
    #[error("renderer identity Effect graph is unavailable")]
    IdentityEffectGraphUnavailable,
    #[error("renderer identity Effect graph could not be lowered to GPU")]
    IdentityGpuPlanUnavailable,
    #[error("heterogeneous CPU-working source address space is exhausted")]
    AddressSpaceExhausted,
    #[error("heterogeneous source input {address} has no CPU working payload")]
    MissingCpuWorkingPayload { address: u32 },
    #[error("heterogeneous source input {address} has no pre-materialization route")]
    MissingPreparedRoute { address: u32 },
    #[error("heterogeneous CPU-prefix batch is invalid: {0}")]
    InvalidCpuPrefixBatch(#[from] HeterogeneousCpuPrefixBatchError),
}

pub(crate) fn viewer_preview_cache_key_for_resolved_plan(
    sequence_id: SequenceId,
    width: u32,
    height: u32,
    elements: &[ResolvedPreviewElement],
    color_context: &ProgramColorContext,
) -> PreviewOutputKey {
    let mut builder = PreviewSemanticIdentityBuilder::new(b"mondrian.preview.viewer-plan.v1");
    color_context.working_color_space.hash(&mut builder);
    color_context.output_color_space.hash(&mut builder);
    color_context.output_tone_map.hash(&mut builder);
    color_context.engine.hash(&mut builder);
    color_context.output_transform.hash(&mut builder);
    elements.len().hash(&mut builder);
    for element in elements {
        match element {
            ResolvedPreviewElement::SolidColor(solid) => {
                0u8.hash(&mut builder);
                hash_color(solid.color, &mut builder);
                solid.opacity.to_bits().hash(&mut builder);
                solid.blend_mode.hash(&mut builder);
                hash_transform(solid.transform, &mut builder);
                hash_effect_graph_identity(&solid.effect_graph, solid.frame_seed, &mut builder);
            }
            ResolvedPreviewElement::HeterogeneousSolidColor { layer: solid, .. } => {
                0u8.hash(&mut builder);
                hash_color(solid.color, &mut builder);
                solid.opacity.to_bits().hash(&mut builder);
                solid.blend_mode.hash(&mut builder);
                hash_transform(solid.transform, &mut builder);
                hash_effect_graph_identity(&solid.effect_graph, solid.frame_seed, &mut builder);
            }
            ResolvedPreviewElement::Adjustment(adjustment) => {
                1u8.hash(&mut builder);
                adjustment.opacity.to_bits().hash(&mut builder);
                adjustment.blend_mode.hash(&mut builder);
                hash_effect_graph_identity(
                    &adjustment.effect_graph,
                    adjustment.frame_seed,
                    &mut builder,
                );
            }
            ResolvedPreviewElement::Media {
                frame,
                opacity,
                blend_mode,
                transform,
                effect_graph,
                frame_seed,
                ..
            } => {
                2u8.hash(&mut builder);
                frame.identity().hash(&mut builder);
                frame.width().hash(&mut builder);
                frame.height().hash(&mut builder);
                opacity.to_bits().hash(&mut builder);
                blend_mode.hash(&mut builder);
                hash_transform(*transform, &mut builder);
                hash_effect_graph_identity(effect_graph, *frame_seed, &mut builder);
            }
            ResolvedPreviewElement::CrossDissolve { left, right, progress } => {
                3u8.hash(&mut builder);
                hash_transition_input(left, &mut builder);
                hash_transition_input(right, &mut builder);
                progress.to_bits().hash(&mut builder);
            }
        }
    }
    PreviewOutputKey::new(sequence_id, width, height, builder.finish_identity())
}

/// Stable identity of only the resolved media revisions/source samples in one plan.
///
/// Generated layers are represented by their position tags so a media-free plan
/// still has one deterministic fingerprint. Effect and color semantics remain
/// separate cache-key components.
pub(crate) fn viewer_preview_media_fingerprint(
    elements: &[ResolvedPreviewElement],
) -> PreviewSemanticIdentity {
    let mut builder = PreviewSemanticIdentityBuilder::new(b"mondrian.preview.media-set.v1");
    elements.len().hash(&mut builder);
    for element in elements {
        match element {
            ResolvedPreviewElement::Media { frame, .. } => {
                1_u8.hash(&mut builder);
                frame.identity().hash(&mut builder);
            }
            ResolvedPreviewElement::CrossDissolve { left, right, .. } => {
                2_u8.hash(&mut builder);
                hash_transition_media_identity(left, &mut builder);
                hash_transition_media_identity(right, &mut builder);
            }
            ResolvedPreviewElement::SolidColor(_)
            | ResolvedPreviewElement::HeterogeneousSolidColor { .. }
            | ResolvedPreviewElement::Adjustment(_) => 0_u8.hash(&mut builder),
        }
    }
    builder.finish_identity()
}

/// Stable identity of the exact working/output color contract shaping a plan.
pub(crate) fn viewer_preview_color_fingerprint(
    color_context: &ProgramColorContext,
) -> PreviewSemanticIdentity {
    let mut builder = PreviewSemanticIdentityBuilder::new(b"mondrian.preview.color-context.v1");
    color_context.working_color_space.hash(&mut builder);
    color_context.output_color_space.hash(&mut builder);
    color_context.output_tone_map.hash(&mut builder);
    color_context.engine.hash(&mut builder);
    color_context.output_transform.hash(&mut builder);
    builder.finish_identity()
}

fn hash_transition_media_identity(
    input: &ResolvedPreviewTransitionInput,
    builder: &mut PreviewSemanticIdentityBuilder,
) {
    match input {
        ResolvedPreviewTransitionInput::Media { frame, .. } => {
            1_u8.hash(builder);
            frame.identity().hash(builder);
        }
        ResolvedPreviewTransitionInput::Transparent
        | ResolvedPreviewTransitionInput::SolidColor(_)
        | ResolvedPreviewTransitionInput::HeterogeneousSolidColor { .. } => {
            0_u8.hash(builder);
        }
    }
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
            hash_effect_graph_identity(&solid.effect_graph, solid.frame_seed, hasher);
        }
        ResolvedPreviewTransitionInput::HeterogeneousSolidColor { layer: solid, .. } => {
            1u8.hash(hasher);
            hash_color(solid.color, hasher);
            solid.opacity.to_bits().hash(hasher);
            solid.blend_mode.hash(hasher);
            hash_transform(solid.transform, hasher);
            hash_effect_graph_identity(&solid.effect_graph, solid.frame_seed, hasher);
        }
        ResolvedPreviewTransitionInput::Media {
            frame,
            opacity,
            blend_mode,
            transform,
            effect_graph,
            frame_seed,
            ..
        } => {
            2u8.hash(hasher);
            frame.identity().hash(hasher);
            frame.width().hash(hasher);
            frame.height().hash(hasher);
            opacity.to_bits().hash(hasher);
            blend_mode.hash(hasher);
            hash_transform(*transform, hasher);
            hash_effect_graph_identity(effect_graph, *frame_seed, hasher);
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

fn hash_effect_graph_identity(
    graph: &CompiledEffectGraph,
    frame_seed: i64,
    hasher: &mut impl Hasher,
) {
    graph.semantic_fingerprint().hash(hasher);
    graph.output_cache_policy().hash(hasher);
    if graph.output_cache_policy().requires_frame_seed() {
        frame_seed.hash(hasher);
    }
}

/// Whether every resolved effect output admits semantic cross-call reuse.
///
/// A mandatory Viewer output identity still exists for non-reusable plans, but
/// it may only correlate one concrete execution/presentation attempt. Callers
/// must not use that semantic identity as cache admission.
pub(crate) fn viewer_preview_plan_allows_cross_call_reuse(
    elements: &[ResolvedPreviewElement],
) -> bool {
    fn graph_reusable(graph: &CompiledEffectGraph) -> bool {
        graph.output_cache_policy().permits_cross_call_reuse()
    }

    fn transition_input_reusable(input: &ResolvedPreviewTransitionInput) -> bool {
        match input {
            ResolvedPreviewTransitionInput::Transparent => true,
            ResolvedPreviewTransitionInput::SolidColor(layer) => {
                graph_reusable(&layer.effect_graph)
            }
            ResolvedPreviewTransitionInput::HeterogeneousSolidColor { layer, .. } => {
                graph_reusable(&layer.effect_graph)
            }
            ResolvedPreviewTransitionInput::Media { frame, effect_graph, .. } => {
                frame.permits_cross_call_reuse() && graph_reusable(effect_graph)
            }
        }
    }

    elements.iter().all(|element| match element {
        ResolvedPreviewElement::SolidColor(layer) => graph_reusable(&layer.effect_graph),
        ResolvedPreviewElement::HeterogeneousSolidColor { layer, .. } => {
            graph_reusable(&layer.effect_graph)
        }
        ResolvedPreviewElement::Adjustment(layer) => graph_reusable(&layer.effect_graph),
        ResolvedPreviewElement::Media { frame, effect_graph, .. } => {
            frame.permits_cross_call_reuse() && graph_reusable(effect_graph)
        }
        ResolvedPreviewElement::CrossDissolve { left, right, .. } => {
            transition_input_reusable(left) && transition_input_reusable(right)
        }
    })
}

pub(crate) fn gpu_composite_layers_for_resolved_with_session(
    resolved: &[ResolvedPreviewElement],
    working_color_space: WorkingColorSpace,
    scratch: &mut TimelineCompositeScratch,
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
                ..
            } => {
                layers.push(ViewerGpuExecutionLayer::Source(gpu_media_source(
                    frame,
                    *opacity,
                    *blend_mode,
                    *transform,
                    effect_graph,
                    *frame_seed,
                    working_color_space,
                    scratch,
                )?));
                has_composited_layer |= opacity.clamp(0.0, 1.0) > 0.0;
            }
            ResolvedPreviewElement::SolidColor(layer) => {
                layers.push(ViewerGpuExecutionLayer::Source(gpu_solid_source(
                    layer, scratch,
                )?));
                has_composited_layer |= layer.opacity.clamp(0.0, 1.0) > 0.0;
            }
            ResolvedPreviewElement::HeterogeneousSolidColor { layer, .. } => {
                layers.push(ViewerGpuExecutionLayer::Source(gpu_solid_source(
                    layer, scratch,
                )?));
                has_composited_layer |= layer.opacity.clamp(0.0, 1.0) > 0.0;
            }
            ResolvedPreviewElement::Adjustment(layer) => {
                if !has_composited_layer
                    || layer.opacity <= 1.0e-4
                    || layer.effect_graph.graph().is_identity()
                {
                    continue;
                }
                let blend_mode = layer.blend_mode.unwrap_or(BlendMode::Normal);
                let effect_plan = scratch
                    .get_or_lower_effect_gpu_plan(&layer.effect_graph)
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
                layers.push(ViewerGpuExecutionLayer::CrossDissolve(Box::new(
                    ViewerGpuCrossDissolveLayer {
                        left: gpu_transition_input(left, working_color_space, scratch)?,
                        right: gpu_transition_input(right, working_color_space, scratch)?,
                        progress,
                    },
                )));
                has_composited_layer |= transition_input_has_contribution(left, 1.0 - progress)
                    || transition_input_has_contribution(right, progress);
            }
        }
    }
    Ok(layers)
}

/// Lower one verified post-composite Timeline cache hit as a single identity
/// working layer. The ordinary Viewer output/monitor stages remain unchanged.
pub(crate) fn gpu_layer_for_cached_working(
    frame: CpuColorFrame,
    scratch: &mut TimelineCompositeScratch,
) -> Result<ViewerGpuExecutionLayer, GpuCompositingBlockerReason> {
    let graph =
        identity_compiled_effect_graph().ok_or(GpuCompositingBlockerReason::EffectRequiresCpu)?;
    let effect_plan = scratch
        .get_or_lower_effect_gpu_plan(&graph)
        .map_err(|_| GpuCompositingBlockerReason::EffectRequiresCpu)?;
    Ok(ViewerGpuExecutionLayer::Source(
        ViewerGpuSourceLayer::Media {
            frame: Some(frame),
            gpu_source: None,
            native_source: None,
            heterogeneous_input: None,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan,
            frame_seed: 0,
        },
    ))
}

/// Lower one resolved Viewer plan without executing heterogeneous work.
///
/// Full-GPU lowering is attempted first and remains byte-for-byte the ordinary
/// path. Only an `EffectRequiresCpu` result opens the explicit CPU-working
/// source seam used by Media, Basic Title, and materialized parent Nested Clip
/// inputs. Exact graph-value routes were already prepared by the
/// renderer-owned Timeline route ledger before source materialization. This
/// function only binds working pixels, validates the immutable batch, and
/// creates Viewer placeholders.
pub(crate) fn prepare_gpu_composite_layers_with_heterogeneous_effects(
    resolved: &[ResolvedPreviewElement],
    working_color_space: WorkingColorSpace,
    scratch: &mut TimelineCompositeScratch,
    cpu_prefix_grant: HeterogeneousCpuPrefixBatchGrant,
) -> Result<PreparedPreviewViewerGpuLayers, PreviewViewerGpuLayerPreparationError> {
    match gpu_composite_layers_for_resolved_with_session(resolved, working_color_space, scratch) {
        Ok(layers) => return Ok(PreparedPreviewViewerGpuLayers::Ordinary { layers }),
        Err(GpuCompositingBlockerReason::EffectRequiresCpu) => {}
        Err(reason) => {
            return Err(PreviewViewerGpuLayerPreparationError::Compositing { reason });
        }
    }

    let mut builder = HeterogeneousPreviewLayerBuilder::new(working_color_space, scratch);
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
                prepared_heterogeneous_route,
                frame_seed,
            } => {
                if !opacity_has_contribution(*opacity) {
                    continue;
                }
                layers.push(ViewerGpuExecutionLayer::Source(builder.media_source(
                    frame,
                    *opacity,
                    *blend_mode,
                    *transform,
                    effect_graph,
                    prepared_heterogeneous_route.as_deref(),
                    *frame_seed,
                )?));
                has_composited_layer = true;
            }
            ResolvedPreviewElement::SolidColor(layer) => {
                if !opacity_has_contribution(layer.opacity) {
                    continue;
                }
                layers.push(ViewerGpuExecutionLayer::Source(
                    gpu_solid_source(layer, builder.scratch).map_err(|reason| {
                        PreviewViewerGpuLayerPreparationError::Compositing { reason }
                    })?,
                ));
                has_composited_layer = true;
            }
            ResolvedPreviewElement::HeterogeneousSolidColor { layer, prepared_route } => {
                if !opacity_has_contribution(layer.opacity) {
                    continue;
                }
                layers.push(ViewerGpuExecutionLayer::Source(
                    builder.solid_source(layer, prepared_route)?,
                ));
                has_composited_layer = true;
            }
            ResolvedPreviewElement::Adjustment(layer) => {
                if !has_composited_layer
                    || layer.opacity <= 1.0e-4
                    || layer.effect_graph.graph().is_identity()
                {
                    continue;
                }
                let blend_mode = layer.blend_mode.unwrap_or(BlendMode::Normal);
                let effect_plan = builder
                    .scratch
                    .get_or_lower_effect_gpu_plan(&layer.effect_graph)
                    .map_err(|_| PreviewViewerGpuLayerPreparationError::Compositing {
                        reason: GpuCompositingBlockerReason::EffectRequiresCpu,
                    })?;
                layers.push(ViewerGpuExecutionLayer::Adjustment {
                    effect_plan,
                    opacity: layer.opacity,
                    blend_mode,
                    frame_seed: layer.frame_seed,
                });
            }
            ResolvedPreviewElement::CrossDissolve { left, right, progress } => {
                let progress = progress.clamp(0.0, 1.0);
                let left = builder.transition_input(left, 1.0 - progress)?;
                let right = builder.transition_input(right, progress)?;
                if transition_gpu_input_is_transparent(&left)
                    && transition_gpu_input_is_transparent(&right)
                {
                    continue;
                }
                layers.push(ViewerGpuExecutionLayer::CrossDissolve(Box::new(
                    ViewerGpuCrossDissolveLayer { left, right, progress },
                )));
                has_composited_layer = true;
            }
        }
    }
    let (items, continuations) = builder.into_parts();
    if items.is_empty() {
        return Ok(PreparedPreviewViewerGpuLayers::Ordinary { layers });
    }
    let cpu_prefix = HeterogeneousCpuPrefixBatchRequest::new(cpu_prefix_grant, items);
    cpu_prefix.validate()?;
    Ok(PreparedPreviewViewerGpuLayers::Heterogeneous {
        layers,
        cpu_prefix,
        continuations: continuations.into_boxed_slice(),
    })
}

struct HeterogeneousPreviewLayerBuilder<'a> {
    working_color_space: WorkingColorSpace,
    scratch: &'a mut TimelineCompositeScratch,
    identity_effect_plan: Option<Arc<mondrian_effects::CompiledEffectGpuPlan>>,
    items: Vec<HeterogeneousCpuPrefixBatchItem>,
    continuations: Vec<PreviewHeterogeneousGpuContinuationMetadata>,
}

impl<'a> HeterogeneousPreviewLayerBuilder<'a> {
    fn new(
        working_color_space: WorkingColorSpace,
        scratch: &'a mut TimelineCompositeScratch,
    ) -> Self {
        Self {
            working_color_space,
            scratch,
            identity_effect_plan: None,
            items: Vec::new(),
            continuations: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn media_source(
        &mut self,
        frame: &MediaPreviewFrame,
        opacity: f32,
        blend_mode: BlendMode,
        transform: [f32; 6],
        effect_graph: &Arc<CompiledEffectGraph>,
        prepared_heterogeneous_route: Option<&PreparedHeterogeneousEffectRoute>,
        frame_seed: i64,
    ) -> Result<ViewerGpuSourceLayer, PreviewViewerGpuLayerPreparationError> {
        let layer_working_color_space = frame.working_color_space().ok_or(
            PreviewViewerGpuLayerPreparationError::Compositing {
                reason: GpuCompositingBlockerReason::GpuUnavailable,
            },
        )?;
        if layer_working_color_space != self.working_color_space
            || !is_preview_gpu_transform_supported(transform)
        {
            return Err(PreviewViewerGpuLayerPreparationError::Compositing {
                reason: GpuCompositingBlockerReason::UnsupportedTransform,
            });
        }

        if let Ok(effect_plan) = self.scratch.get_or_lower_effect_gpu_plan(effect_graph) {
            return Ok(ViewerGpuSourceLayer::Media {
                frame: frame.working_payload(),
                gpu_source: frame.gpu_source(),
                native_source: frame.native_source(),
                heterogeneous_input: None,
                opacity,
                blend_mode,
                transform,
                effect_plan,
                frame_seed,
            });
        }

        let address = u32::try_from(self.items.len())
            .map_err(|_| PreviewViewerGpuLayerPreparationError::AddressSpaceExhausted)?;
        let input = frame
            .working_payload()
            .ok_or(PreviewViewerGpuLayerPreparationError::MissingCpuWorkingPayload { address })?;
        let route = prepared_heterogeneous_route
            .cloned()
            .ok_or(PreviewViewerGpuLayerPreparationError::MissingPreparedRoute { address })?;
        let descriptor = input.descriptor();
        let identity_effect_plan = self.identity_effect_plan()?;
        self.items.push(HeterogeneousCpuPrefixBatchItem::new(
            address,
            route,
            input,
            self.working_color_space,
            frame_seed,
        ));
        self.continuations.push(PreviewHeterogeneousGpuContinuationMetadata {
            address,
            graph_fingerprint: effect_graph.semantic_fingerprint(),
            frame_extent: EffectFrameExtent::new(descriptor.width, descriptor.height),
            frame_seed,
            working_color_space: self.working_color_space,
        });
        Ok(ViewerGpuSourceLayer::Media {
            frame: None,
            gpu_source: None,
            native_source: None,
            heterogeneous_input: Some(address),
            opacity,
            blend_mode,
            transform,
            effect_plan: identity_effect_plan,
            frame_seed,
        })
    }

    fn solid_source(
        &mut self,
        layer: &TimelineSolidColorLayer,
        prepared_route: &PreparedHeterogeneousEffectRoute,
    ) -> Result<ViewerGpuSourceLayer, PreviewViewerGpuLayerPreparationError> {
        if !is_preview_gpu_transform_supported(layer.transform) {
            return Err(PreviewViewerGpuLayerPreparationError::Compositing {
                reason: GpuCompositingBlockerReason::UnsupportedTransform,
            });
        }
        let address = u32::try_from(self.items.len())
            .map_err(|_| PreviewViewerGpuLayerPreparationError::AddressSpaceExhausted)?;
        let route = prepared_route.clone();
        let frame_extent = route.frame_extent();
        let identity_effect_plan = self.identity_effect_plan()?;
        self.items.push(HeterogeneousCpuPrefixBatchItem::new_solid_color(
            address,
            route,
            layer.color,
            self.working_color_space,
            layer.frame_seed,
        ));
        self.continuations.push(PreviewHeterogeneousGpuContinuationMetadata {
            address,
            graph_fingerprint: layer.effect_graph.semantic_fingerprint(),
            frame_extent,
            frame_seed: layer.frame_seed,
            working_color_space: self.working_color_space,
        });
        Ok(ViewerGpuSourceLayer::Media {
            frame: None,
            gpu_source: None,
            native_source: None,
            heterogeneous_input: Some(address),
            opacity: layer.opacity,
            blend_mode: layer.blend_mode,
            transform: layer.transform,
            effect_plan: identity_effect_plan,
            frame_seed: layer.frame_seed,
        })
    }

    fn transition_input(
        &mut self,
        input: &ResolvedPreviewTransitionInput,
        transition_weight: f32,
    ) -> Result<ViewerGpuTransitionInput, PreviewViewerGpuLayerPreparationError> {
        if transition_weight <= 0.0 {
            return Ok(ViewerGpuTransitionInput::Transparent);
        }
        let source = match input {
            ResolvedPreviewTransitionInput::Transparent => {
                return Ok(ViewerGpuTransitionInput::Transparent);
            }
            ResolvedPreviewTransitionInput::SolidColor(layer) => {
                if !opacity_has_contribution(layer.opacity) {
                    return Ok(ViewerGpuTransitionInput::Transparent);
                }
                gpu_solid_source(layer, self.scratch).map_err(|reason| {
                    PreviewViewerGpuLayerPreparationError::Compositing { reason }
                })?
            }
            ResolvedPreviewTransitionInput::HeterogeneousSolidColor { layer, prepared_route } => {
                if !opacity_has_contribution(layer.opacity) {
                    return Ok(ViewerGpuTransitionInput::Transparent);
                }
                self.solid_source(layer, prepared_route)?
            }
            ResolvedPreviewTransitionInput::Media {
                frame,
                opacity,
                blend_mode,
                transform,
                effect_graph,
                prepared_heterogeneous_route,
                frame_seed,
            } => {
                if !opacity_has_contribution(*opacity) {
                    return Ok(ViewerGpuTransitionInput::Transparent);
                }
                self.media_source(
                    frame,
                    *opacity,
                    *blend_mode,
                    *transform,
                    effect_graph,
                    prepared_heterogeneous_route.as_deref(),
                    *frame_seed,
                )?
            }
        };
        Ok(ViewerGpuTransitionInput::Source(source))
    }

    fn identity_effect_plan(
        &mut self,
    ) -> Result<Arc<mondrian_effects::CompiledEffectGpuPlan>, PreviewViewerGpuLayerPreparationError>
    {
        if let Some(plan) = &self.identity_effect_plan {
            return Ok(Arc::clone(plan));
        }
        let graph = identity_compiled_effect_graph()
            .ok_or(PreviewViewerGpuLayerPreparationError::IdentityEffectGraphUnavailable)?;
        let plan = self
            .scratch
            .get_or_lower_effect_gpu_plan(&graph)
            .map_err(|_| PreviewViewerGpuLayerPreparationError::IdentityGpuPlanUnavailable)?;
        self.identity_effect_plan = Some(Arc::clone(&plan));
        Ok(plan)
    }

    fn into_parts(
        self,
    ) -> (
        Vec<HeterogeneousCpuPrefixBatchItem>,
        Vec<PreviewHeterogeneousGpuContinuationMetadata>,
    ) {
        (self.items, self.continuations)
    }
}

fn opacity_has_contribution(opacity: f32) -> bool {
    opacity.clamp(0.0, 1.0) > 0.0
}

fn transition_gpu_input_is_transparent(input: &ViewerGpuTransitionInput) -> bool {
    matches!(input, ViewerGpuTransitionInput::Transparent)
}

#[cfg(test)]
pub(crate) fn gpu_composite_layers_for_resolved(
    resolved: &[ResolvedPreviewElement],
    working_color_space: WorkingColorSpace,
) -> Result<Vec<ViewerGpuExecutionLayer>, GpuCompositingBlockerReason> {
    let mut scratch = TimelineCompositeScratch::default();
    gpu_composite_layers_for_resolved_with_session(resolved, working_color_space, &mut scratch)
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
    scratch: &mut TimelineCompositeScratch,
) -> Result<ViewerGpuSourceLayer, GpuCompositingBlockerReason> {
    let layer_working_color_space =
        frame.working_color_space().ok_or(GpuCompositingBlockerReason::GpuUnavailable)?;
    if layer_working_color_space != working_color_space
        || !is_preview_gpu_transform_supported(transform)
    {
        return Err(GpuCompositingBlockerReason::UnsupportedTransform);
    }
    let effect_plan = scratch
        .get_or_lower_effect_gpu_plan(effect_graph)
        .map_err(|_| GpuCompositingBlockerReason::EffectRequiresCpu)?;
    Ok(ViewerGpuSourceLayer::Media {
        frame: frame.working_payload(),
        gpu_source: frame.gpu_source(),
        native_source: frame.native_source(),
        heterogeneous_input: None,
        opacity,
        blend_mode,
        transform,
        effect_plan,
        frame_seed,
    })
}

fn gpu_solid_source(
    layer: &TimelineSolidColorLayer,
    scratch: &mut TimelineCompositeScratch,
) -> Result<ViewerGpuSourceLayer, GpuCompositingBlockerReason> {
    if !is_preview_gpu_transform_supported(layer.transform) {
        return Err(GpuCompositingBlockerReason::UnsupportedTransform);
    }
    let effect_plan = scratch
        .get_or_lower_effect_gpu_plan(&layer.effect_graph)
        .map_err(|_| GpuCompositingBlockerReason::EffectRequiresCpu)?;
    Ok(ViewerGpuSourceLayer::SolidColor { layer: layer.clone(), effect_plan })
}

fn gpu_transition_input(
    input: &ResolvedPreviewTransitionInput,
    working_color_space: WorkingColorSpace,
    scratch: &mut TimelineCompositeScratch,
) -> Result<ViewerGpuTransitionInput, GpuCompositingBlockerReason> {
    let source = match input {
        ResolvedPreviewTransitionInput::Transparent => {
            return Ok(ViewerGpuTransitionInput::Transparent);
        }
        ResolvedPreviewTransitionInput::SolidColor(layer) => gpu_solid_source(layer, scratch)?,
        ResolvedPreviewTransitionInput::HeterogeneousSolidColor { layer, .. } => {
            gpu_solid_source(layer, scratch)?
        }
        ResolvedPreviewTransitionInput::Media {
            frame,
            opacity,
            blend_mode,
            transform,
            effect_graph,
            frame_seed,
            ..
        } => gpu_media_source(
            frame,
            *opacity,
            *blend_mode,
            *transform,
            effect_graph,
            *frame_seed,
            working_color_space,
            scratch,
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
        ResolvedPreviewTransitionInput::HeterogeneousSolidColor { layer, .. } => {
            layer.opacity.clamp(0.0, 1.0) > 0.0
        }
        ResolvedPreviewTransitionInput::Media { opacity, .. } => opacity.clamp(0.0, 1.0) > 0.0,
    }
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
        ResolvedPreviewElement::SolidColor(_)
        | ResolvedPreviewElement::HeterogeneousSolidColor { .. }
        | ResolvedPreviewElement::Adjustment(_) => false,
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
                    if let ResolvedPreviewTransitionInput::Media { frame, .. } = input.as_ref() {
                        summary.accumulate(frame.decode_execution());
                    }
                }
            }
            ResolvedPreviewElement::SolidColor(_)
            | ResolvedPreviewElement::HeterogeneousSolidColor { .. }
            | ResolvedPreviewElement::Adjustment(_) => {}
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

#[cfg(test)]
mod heterogeneous_tests {
    use super::*;
    use mondrian_core::automation::{PropertyHost, PropertyMutation, PropertyValue};
    use mondrian_core::{TimelineTime, WorkingRgbaF32Frame};
    use mondrian_effects::{
        EffectExecutionSessionConfig, EffectGraphExecutionBudget, EffectNode, EffectNodeExt,
        EffectType, PreparedEffectProgram,
    };
    use mondrian_renderer::{CpuColorFrame, HeterogeneousGpuResourceGrant};

    const WIDTH: u32 = 4;
    const HEIGHT: u32 = 3;
    const WORKING_SPACE: WorkingColorSpace = WorkingColorSpace::LinearRec2020;

    fn cpu_grant() -> HeterogeneousCpuPrefixBatchGrant {
        HeterogeneousCpuPrefixBatchGrant::new(
            EffectExecutionSessionConfig::uncached(1 << 20),
            EffectGraphExecutionBudget::new(1 << 20, 1 << 20, 1 << 20, 64, 192),
            2,
            2 << 20,
        )
    }

    fn tracer_graph() -> Arc<CompiledEffectGraph> {
        let blur = EffectNode::with_defaults(EffectType::GaussianBlur);
        let mut correction = EffectNode::with_defaults(EffectType::BasicCorrection);
        correction
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::BasicCorrection.property_path("exposure"),
                value: PropertyValue::Float(0.25),
            })
            .expect("set Basic Correction exposure");
        let mut grain = EffectNode::with_defaults(EffectType::Grain);
        grain
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Grain.property_path("amount"),
                value: PropertyValue::Float(0.1),
            })
            .expect("set Grain amount");
        PreparedEffectProgram::prepare(&[blur, correction, grain], &[], WORKING_SPACE)
            .expect("prepare heterogeneous tracer")
            .evaluate(TimelineTime::ZERO)
            .expect("compile heterogeneous tracer")
    }

    #[test]
    fn verified_render_cache_frame_lowers_as_one_identity_working_layer() {
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: WIDTH,
            height: HEIGHT,
            data: vec![[0.25, 0.5, 1.0, 1.0]; (WIDTH * HEIGHT) as usize],
            color_space: WORKING_SPACE,
        });
        let mut scratch = TimelineCompositeScratch::default();
        let layer = gpu_layer_for_cached_working(frame.clone(), &mut scratch)
            .expect("identity cached layer");
        let ViewerGpuExecutionLayer::Source(ViewerGpuSourceLayer::Media {
            frame: Some(lowered),
            gpu_source: None,
            native_source: None,
            heterogeneous_input: None,
            opacity,
            blend_mode,
            transform,
            frame_seed,
            ..
        }) = layer
        else {
            panic!("cache hit must be one ordinary CPU-working source");
        };
        assert!(lowered.shares_storage_with(&frame));
        assert_eq!(opacity, 1.0);
        assert_eq!(blend_mode, BlendMode::Normal);
        assert_eq!(transform, [1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        assert_eq!(frame_seed, 0);
    }

    fn working_frame(identity_salt: u64) -> MediaPreviewFrame {
        let mut identity =
            PreviewSemanticIdentityBuilder::new(b"mondrian.preview.heterogeneous-plan-test.v1");
        identity_salt.hash(&mut identity);
        MediaPreviewFrame::from_working(
            CpuColorFrame::working(WorkingRgbaF32Frame {
                width: WIDTH,
                height: HEIGHT,
                color_space: WORKING_SPACE,
                data: vec![[0.2, 0.4, 0.6, 1.0]; (WIDTH * HEIGHT) as usize],
            }),
            mondrian_core::Resolution { width: WIDTH, height: HEIGHT },
            identity.finish_identity(),
            FramePresentationQuality::Ready,
            PreviewDecodeExecutionSummary::default(),
        )
    }

    fn media(graph: Arc<CompiledEffectGraph>, identity_salt: u64) -> ResolvedPreviewElement {
        media_with_blend(graph, identity_salt, BlendMode::Normal)
    }

    fn media_with_blend(
        graph: Arc<CompiledEffectGraph>,
        identity_salt: u64,
        blend_mode: BlendMode,
    ) -> ResolvedPreviewElement {
        let prepared_heterogeneous_route = PreparedHeterogeneousEffectRoute::prepare(
            Arc::clone(&graph),
            EffectFrameExtent::new(WIDTH, HEIGHT),
            cpu_grant().graph_execution(),
        )
        .ok()
        .map(Box::new);
        ResolvedPreviewElement::Media {
            frame: working_frame(identity_salt),
            opacity: 1.0,
            blend_mode,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: graph,
            prepared_heterogeneous_route,
            frame_seed: identity_salt as i64,
        }
    }

    fn solid(graph: Arc<CompiledEffectGraph>) -> ResolvedPreviewElement {
        let prepared_route = PreparedHeterogeneousEffectRoute::prepare(
            Arc::clone(&graph),
            EffectFrameExtent::new(WIDTH, HEIGHT),
            cpu_grant().graph_execution(),
        )
        .expect("prepare Solid Color heterogeneous route");
        ResolvedPreviewElement::HeterogeneousSolidColor {
            layer: TimelineSolidColorLayer {
                color: mondrian_core::Color { r: 0.2, g: 0.4, b: 0.6, a: 0.75 },
                opacity: 0.8,
                blend_mode: BlendMode::Screen,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: graph,
                frame_seed: 31,
            },
            prepared_route: Box::new(prepared_route),
        }
    }

    fn ordinary_solid(graph: Arc<CompiledEffectGraph>, frame_seed: i64) -> ResolvedPreviewElement {
        ResolvedPreviewElement::SolidColor(TimelineSolidColorLayer {
            color: mondrian_core::Color { r: 0.2, g: 0.4, b: 0.6, a: 0.75 },
            opacity: 0.8,
            blend_mode: BlendMode::Screen,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: graph,
            frame_seed,
        })
    }

    #[test]
    fn ordinary_gpu_lowering_preserves_stacks_beyond_the_legacy_five_layer_limit() {
        let graph = identity_compiled_effect_graph().expect("identity graph");
        let resolved: Vec<_> = (0..9)
            .map(|frame_seed| ordinary_solid(Arc::clone(&graph), frame_seed))
            .collect();

        let layers = gpu_composite_layers_for_resolved(&resolved, WORKING_SPACE)
            .expect("layer count is not a compositing blocker");

        assert_eq!(layers.len(), 9);
        for (index, layer) in layers.iter().enumerate() {
            assert!(matches!(
                layer,
                ViewerGpuExecutionLayer::Source(ViewerGpuSourceLayer::SolidColor { layer, .. })
                    if layer.frame_seed == index as i64
            ));
        }
    }

    #[test]
    fn exact_full_gpu_media_preserves_non_normal_blend_on_the_ordinary_path() {
        let resolved = [media_with_blend(
            identity_compiled_effect_graph().expect("identity graph"),
            1,
            BlendMode::Screen,
        )];
        let prepared = prepare_gpu_composite_layers_with_heterogeneous_effects(
            &resolved,
            WORKING_SPACE,
            &mut TimelineCompositeScratch::default(),
            cpu_grant(),
        )
        .expect("ordinary Viewer plan");

        let PreparedPreviewViewerGpuLayers::Ordinary { layers } = prepared else {
            panic!("identity graph must remain on the ordinary Viewer path");
        };
        assert!(matches!(
            &layers[..],
            [ViewerGpuExecutionLayer::Source(
                ViewerGpuSourceLayer::Media { blend_mode: BlendMode::Screen, .. }
            )]
        ));
    }

    #[test]
    fn tracer_media_produces_an_addressed_cpu_request_and_gpu_binding_metadata() {
        let graph = tracer_graph();
        let resolved = [media(Arc::clone(&graph), 7)];
        let prepared = prepare_gpu_composite_layers_with_heterogeneous_effects(
            &resolved,
            WORKING_SPACE,
            &mut TimelineCompositeScratch::default(),
            cpu_grant(),
        )
        .expect("heterogeneous Viewer plan");
        let PreparedPreviewViewerGpuLayers::Heterogeneous { layers, cpu_prefix, continuations } =
            prepared
        else {
            panic!("tracer must not become an ordinary full-GPU plan");
        };

        assert_eq!(layers.len(), 1);
        assert_eq!(cpu_prefix.items().len(), 1);
        assert_eq!(cpu_prefix.items()[0].address(), 0);
        assert_eq!(continuations.len(), 1);
        assert_eq!(continuations[0].address(), 0);
        let gpu = continuations[0].gpu_continuation_request(
            41,
            HeterogeneousGpuResourceGrant::new(1 << 20, 1 << 20, 64, 0),
        );
        assert_eq!(
            gpu.binding().graph_fingerprint(),
            graph.semantic_fingerprint()
        );
        assert_eq!(gpu.binding().generation(), 41);
        assert_eq!(
            gpu.binding().frame_extent(),
            EffectFrameExtent::new(WIDTH, HEIGHT)
        );
        assert!(matches!(
            &layers[0],
            ViewerGpuExecutionLayer::Source(ViewerGpuSourceLayer::Media {
                heterogeneous_input: Some(0),
                frame: None,
                gpu_source: None,
                native_source: None,
                ..
            })
        ));
    }

    #[test]
    fn tracer_solid_produces_a_procedural_cpu_request_without_a_media_payload() {
        let graph = tracer_graph();
        let prepared = prepare_gpu_composite_layers_with_heterogeneous_effects(
            &[solid(Arc::clone(&graph))],
            WORKING_SPACE,
            &mut TimelineCompositeScratch::default(),
            cpu_grant(),
        )
        .expect("heterogeneous Solid Color Viewer plan");
        let PreparedPreviewViewerGpuLayers::Heterogeneous { layers, cpu_prefix, continuations } =
            prepared
        else {
            panic!("the procedural tracer must not require a fabricated media frame");
        };

        assert_eq!(cpu_prefix.items().len(), 1);
        assert_eq!(cpu_prefix.items()[0].descriptor().width, WIDTH);
        assert_eq!(cpu_prefix.items()[0].descriptor().height, HEIGHT);
        assert_eq!(
            continuations[0].frame_extent,
            EffectFrameExtent::new(WIDTH, HEIGHT)
        );
        assert!(matches!(
            &layers[..],
            [ViewerGpuExecutionLayer::Source(
                ViewerGpuSourceLayer::Media {
                    heterogeneous_input: Some(0),
                    frame: None,
                    gpu_source: None,
                    native_source: None,
                    blend_mode: BlendMode::Screen,
                    ..
                }
            )]
        ));
    }

    #[test]
    fn cross_dissolve_procedural_solid_endpoint_uses_the_same_addressed_batch() {
        let graph = tracer_graph();
        let ResolvedPreviewElement::HeterogeneousSolidColor { layer, prepared_route } =
            solid(graph)
        else {
            unreachable!("Solid helper must retain its procedural route")
        };
        let transition = ResolvedPreviewElement::CrossDissolve {
            left: Box::new(ResolvedPreviewTransitionInput::HeterogeneousSolidColor {
                layer,
                prepared_route,
            }),
            right: Box::new(ResolvedPreviewTransitionInput::Transparent),
            progress: 0.5,
        };
        let prepared = prepare_gpu_composite_layers_with_heterogeneous_effects(
            &[transition],
            WORKING_SPACE,
            &mut TimelineCompositeScratch::default(),
            cpu_grant(),
        )
        .expect("procedural Solid Cross Dissolve endpoint");
        let PreparedPreviewViewerGpuLayers::Heterogeneous { layers, cpu_prefix, continuations } =
            prepared
        else {
            panic!("procedural endpoint requires its prepared CPU prefix");
        };
        assert_eq!(cpu_prefix.items().len(), 1);
        assert_eq!(continuations.len(), 1);
        assert!(matches!(
            &layers[..],
            [ViewerGpuExecutionLayer::CrossDissolve(transition)]
                if matches!(transition.left, ViewerGpuTransitionInput::Source(_))
                    && matches!(transition.right, ViewerGpuTransitionInput::Transparent)
        ));
    }

    #[test]
    fn cross_dissolve_zero_weight_endpoint_creates_no_cpu_prefix_item() {
        let graph = tracer_graph();
        let transition = ResolvedPreviewElement::CrossDissolve {
            left: Box::new(match media(Arc::clone(&graph), 11) {
                ResolvedPreviewElement::Media {
                    frame,
                    opacity,
                    blend_mode,
                    transform,
                    effect_graph,
                    prepared_heterogeneous_route,
                    frame_seed,
                } => ResolvedPreviewTransitionInput::Media {
                    frame,
                    opacity,
                    blend_mode,
                    transform,
                    effect_graph,
                    prepared_heterogeneous_route,
                    frame_seed,
                },
                _ => unreachable!("media helper"),
            }),
            right: Box::new(match media(graph, 12) {
                ResolvedPreviewElement::Media {
                    frame,
                    opacity,
                    blend_mode,
                    transform,
                    effect_graph,
                    prepared_heterogeneous_route,
                    frame_seed,
                } => ResolvedPreviewTransitionInput::Media {
                    frame,
                    opacity,
                    blend_mode,
                    transform,
                    effect_graph,
                    prepared_heterogeneous_route,
                    frame_seed,
                },
                _ => unreachable!("media helper"),
            }),
            progress: 0.0,
        };
        let prepared = prepare_gpu_composite_layers_with_heterogeneous_effects(
            &[transition],
            WORKING_SPACE,
            &mut TimelineCompositeScratch::default(),
            cpu_grant(),
        )
        .expect("heterogeneous Cross Dissolve plan");
        let PreparedPreviewViewerGpuLayers::Heterogeneous { layers, cpu_prefix, continuations } =
            prepared
        else {
            panic!("the contributing tracer endpoint requires heterogeneous execution");
        };

        assert_eq!(cpu_prefix.items().len(), 1);
        assert_eq!(continuations.len(), 1);
        assert!(matches!(
            &layers[0],
            ViewerGpuExecutionLayer::CrossDissolve(transition)
                if matches!(transition.left, ViewerGpuTransitionInput::Source(_))
                    && matches!(transition.right, ViewerGpuTransitionInput::Transparent)
        ));
    }
}
