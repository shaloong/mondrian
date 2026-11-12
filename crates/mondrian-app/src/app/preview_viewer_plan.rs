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
use mondrian_effects::{
    identity_compiled_effect_graph, CompiledEffectGpuPipeline, CompiledEffectGpuPlan,
    CompiledEffectGraph, EffectFrameExtent,
};
use mondrian_playback::FramePresentationQuality;
use mondrian_renderer::spatial_driver::gpu_spatial_chain_plan_with_auxiliary;
use mondrian_renderer::{
    choose_spatial_chain_for_plan, CpuColorFrame, GpuCompositingBlockerReason, GpuRoute,
    GpuVisualSpatialPlan, HeterogeneousCpuPrefixBatchError, HeterogeneousCpuPrefixBatchGrant,
    HeterogeneousCpuPrefixBatchItem, HeterogeneousCpuPrefixBatchRequest,
    HeterogeneousGpuContinuationBinding, HeterogeneousGpuContinuationRequest,
    HeterogeneousGpuResourceGrant, PreparedHeterogeneousEffectRoute, TimelineAdjustmentLayer,
    TimelineAuxiliarySide, TimelineCompositeScratch, TimelineSolidColorLayer,
    ViewerGpuCrossDissolveLayer, ViewerGpuExecutionLayer, ViewerGpuSourceLayer,
    ViewerGpuTransitionInput,
};
use mondrian_timeline::sequence::ProgramColorContext;

use super::preview_auxiliary::PreparedAuxiliaryRasters;
use super::preview_execution::{
    PreviewDecodeExecutionSummary, PreviewOutputKey, PreviewSemanticIdentityBuilder,
};
use super::preview_media_frame::MediaPreviewFrame;
use super::preview_media_residency::PreviewMediaResidencyGuard;

pub(crate) enum ResolvedPreviewElement {
    /// Invocation-scoped sources for this element, including both endpoints
    /// of a transition. This context does not add a compositing layer.
    WithAuxiliary {
        element: Box<ResolvedPreviewElement>,
        inputs: Box<[super::preview_auxiliary::ResolvedPreviewAuxiliaryInputs]>,
    },
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
        prepared_effect_route: Option<Box<mondrian_renderer::PreparedTimelinePreviewEffectRoute>>,
        frame_seed: i64,
    },
    CrossDissolve {
        left: Box<ResolvedPreviewTransitionInput>,
        right: Box<ResolvedPreviewTransitionInput>,
        progress: f32,
    },
}

impl ResolvedPreviewElement {
    fn without_auxiliary_metadata(&self) -> &Self {
        match self {
            Self::WithAuxiliary { element, inputs }
                if inputs.iter().all(|input| input.sources.is_empty()) =>
            {
                element.without_auxiliary_metadata()
            }
            element => element,
        }
    }

    pub(crate) fn without_auxiliary(&self) -> &Self {
        match self {
            Self::WithAuxiliary { element, .. } => element.without_auxiliary(),
            element => element,
        }
    }
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
        prepared_effect_route: Option<Box<mondrian_renderer::PreparedTimelinePreviewEffectRoute>>,
        frame_seed: i64,
    },
}

/// Retain physical Store leases and any Current protection for one Viewer candidate.
pub(crate) fn resolved_preview_media_residency(
    elements: &[ResolvedPreviewElement],
) -> Vec<PreviewMediaResidencyGuard> {
    let mut protections = Vec::new();
    for element in elements {
        match element {
            ResolvedPreviewElement::WithAuxiliary { element, inputs } => {
                protections.extend(resolved_preview_media_residency(std::slice::from_ref(
                    element,
                )));
                for input in inputs {
                    for source in &input.sources {
                        protections.extend(
                            source.frame.as_ref().and_then(MediaPreviewFrame::residency_guard),
                        );
                    }
                }
            }
            ResolvedPreviewElement::Media { frame, .. } => {
                protections.extend(frame.residency_guard());
            }
            ResolvedPreviewElement::CrossDissolve { left, right, .. } => {
                protections.extend(transition_input_residency(left));
                protections.extend(transition_input_residency(right));
            }
            ResolvedPreviewElement::SolidColor(_)
            | ResolvedPreviewElement::HeterogeneousSolidColor { .. }
            | ResolvedPreviewElement::Adjustment(_) => {}
        }
    }
    protections
}

fn transition_input_residency(
    input: &ResolvedPreviewTransitionInput,
) -> Option<PreviewMediaResidencyGuard> {
    match input {
        ResolvedPreviewTransitionInput::Media { frame, .. } => frame.residency_guard(),
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
    sampling: mondrian_core::ImageSampling,
) -> PreviewOutputKey {
    // v6 binds authored auxiliary sources, exact media revisions and grids.
    let mut builder = PreviewSemanticIdentityBuilder::new(b"mondrian.preview.viewer-plan.v6");
    sampling.hash(&mut builder);
    color_context.working_color_space().hash(&mut builder);
    color_context.output_color_space().hash(&mut builder);
    color_context.output_tone_map().hash(&mut builder);
    color_context.engine().hash(&mut builder);
    color_context.output_transform().hash(&mut builder);
    elements.len().hash(&mut builder);
    for element in elements {
        hash_resolved_element(element, &mut builder);
    }
    let mut key = PreviewOutputKey::new(sequence_id, width, height, builder.finish_identity());
    key.requires_auxiliary_revalidation = elements
        .iter()
        .any(|element| matches!(element, ResolvedPreviewElement::WithAuxiliary { .. }));
    key
}

fn hash_resolved_element(element: &ResolvedPreviewElement, mut builder: &mut impl Hasher) {
    match element {
        ResolvedPreviewElement::WithAuxiliary { element, inputs } => {
            4u8.hash(builder);
            inputs.len().hash(builder);
            for input in inputs {
                input.hash_identity(builder);
            }
            hash_resolved_element(element, builder);
        }
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
            adjustment.blend_policy.hash(&mut builder);
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
        ResolvedPreviewElement::WithAuxiliary { element, inputs } => {
            inputs.iter().all(|input| input.permits_cross_call_reuse())
                && viewer_preview_plan_allows_cross_call_reuse(std::slice::from_ref(element))
        }
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

#[cfg(test)]
pub(crate) fn gpu_composite_layers_for_resolved_with_session(
    resolved: &[ResolvedPreviewElement],
    working_color_space: WorkingColorSpace,
    scratch: &mut TimelineCompositeScratch,
) -> Result<Vec<ViewerGpuExecutionLayer>, GpuCompositingBlockerReason> {
    gpu_composite_layers_with_auxiliary(
        resolved,
        working_color_space,
        scratch,
        &[],
        EffectFrameExtent::new(1, 1),
        mondrian_core::ImageSampling::UNIT_SQUARE,
    )
}

fn gpu_composite_layers_with_auxiliary(
    resolved: &[ResolvedPreviewElement],
    working_color_space: WorkingColorSpace,
    scratch: &mut TimelineCompositeScratch,
    registered: &[PreparedAuxiliaryRasters<'_>],
    canvas_extent: EffectFrameExtent,
    canvas_sampling: mondrian_core::ImageSampling,
) -> Result<Vec<ViewerGpuExecutionLayer>, GpuCompositingBlockerReason> {
    let mut layers = Vec::with_capacity(resolved.len());
    let mut has_composited_layer = false;
    for mut element in resolved {
        let [mut layer_auxiliary, mut left_auxiliary, mut right_auxiliary] = [None; 3];
        while let ResolvedPreviewElement::WithAuxiliary { element: inner, inputs } = element {
            for binding in inputs.iter().filter(|binding| !binding.sources.is_empty()) {
                let registered = registered
                    .iter()
                    .find(|input| std::ptr::eq(input.binding, binding))
                    .ok_or(GpuCompositingBlockerReason::EffectRequiresCpu)?;
                let destination = match binding.side {
                    TimelineAuxiliarySide::Element => &mut layer_auxiliary,
                    TimelineAuxiliarySide::TransitionLeft => &mut left_auxiliary,
                    TimelineAuxiliarySide::TransitionRight => &mut right_auxiliary,
                };
                if destination.replace(registered).is_some() {
                    return Err(GpuCompositingBlockerReason::EffectRequiresCpu);
                }
            }
            element = inner;
        }
        let transition = matches!(element, ResolvedPreviewElement::CrossDissolve { .. });
        if (transition && layer_auxiliary.is_some())
            || (!transition && (left_auxiliary.is_some() || right_auxiliary.is_some()))
        {
            return Err(GpuCompositingBlockerReason::EffectRequiresCpu);
        }
        match element {
            ResolvedPreviewElement::WithAuxiliary { .. } => {
                return Err(GpuCompositingBlockerReason::EffectRequiresCpu);
            }
            ResolvedPreviewElement::Media {
                frame,
                opacity,
                blend_mode,
                transform,
                effect_graph,
                prepared_effect_route,
                frame_seed,
            } => {
                layers.push(ViewerGpuExecutionLayer::Source(Box::new(gpu_media_source(
                    frame,
                    *opacity,
                    *blend_mode,
                    *transform,
                    effect_graph,
                    prepared_effect_route.as_deref(),
                    *frame_seed,
                    working_color_space,
                    scratch,
                    layer_auxiliary,
                )?)));
                has_composited_layer |= opacity.clamp(0.0, 1.0) > 0.0;
            }
            ResolvedPreviewElement::SolidColor(layer)
            | ResolvedPreviewElement::HeterogeneousSolidColor { layer, .. } => {
                layers.push(ViewerGpuExecutionLayer::Source(Box::new(gpu_solid_source(
                    layer,
                    scratch,
                    layer_auxiliary,
                    canvas_extent,
                    canvas_sampling,
                )?)));
                has_composited_layer |= layer.opacity.clamp(0.0, 1.0) > 0.0;
            }
            ResolvedPreviewElement::Adjustment(layer) => {
                if layer_auxiliary.is_some() {
                    return Err(GpuCompositingBlockerReason::EffectRequiresCpu);
                }
                if !has_composited_layer
                    || !opacity_has_contribution(layer.opacity)
                    || (layer.effect_graph.graph().is_identity()
                        && layer.blend_policy == mondrian_effects::EffectPassBlendPolicy::Mix)
                {
                    continue;
                }
                let blend_policy = layer.blend_policy;
                let effect_plan = scratch
                    .get_or_lower_effect_gpu_plan(&layer.effect_graph)
                    .map_err(|_| GpuCompositingBlockerReason::EffectRequiresCpu)?;
                layers.push(ViewerGpuExecutionLayer::Adjustment {
                    effect_plan,
                    opacity: layer.opacity,
                    blend_policy,
                    frame_seed: layer.frame_seed,
                });
            }
            ResolvedPreviewElement::CrossDissolve { left, right, progress } => {
                let progress = progress.clamp(0.0, 1.0);
                layers.push(ViewerGpuExecutionLayer::CrossDissolve(Box::new(
                    ViewerGpuCrossDissolveLayer {
                        left: gpu_transition_input(
                            left,
                            working_color_space,
                            scratch,
                            left_auxiliary,
                            canvas_extent,
                            canvas_sampling,
                        )?,
                        right: gpu_transition_input(
                            right,
                            working_color_space,
                            scratch,
                            right_auxiliary,
                            canvas_extent,
                            canvas_sampling,
                        )?,
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
    Ok(ViewerGpuExecutionLayer::Source(Box::new(
        ViewerGpuSourceLayer::Media {
            frame: Some(frame),
            is_data_texture: false,
            gpu_source: None,
            native_source: None,
            cpu_yuv_source: None,
            heterogeneous_input: None,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan,
            spatial_plan: None,
            frame_seed: 0,
        },
    )))
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
    sampling: mondrian_core::ImageSampling,
    scratch: &mut TimelineCompositeScratch,
    cpu_prefix_grant: HeterogeneousCpuPrefixBatchGrant,
    registered: &[PreparedAuxiliaryRasters<'_>],
    canvas_extent: EffectFrameExtent,
) -> Result<PreparedPreviewViewerGpuLayers, PreviewViewerGpuLayerPreparationError> {
    match gpu_composite_layers_with_auxiliary(
        resolved,
        working_color_space,
        scratch,
        registered,
        canvas_extent,
        sampling,
    ) {
        Ok(layers) => return Ok(PreparedPreviewViewerGpuLayers::Ordinary { layers }),
        Err(GpuCompositingBlockerReason::EffectRequiresCpu) => {}
        Err(reason) => {
            return Err(PreviewViewerGpuLayerPreparationError::Compositing { reason });
        }
    }

    let mut builder = HeterogeneousPreviewLayerBuilder::new(
        working_color_space,
        sampling,
        scratch,
        canvas_extent,
    );
    let mut layers = Vec::with_capacity(resolved.len());
    let mut has_composited_layer = false;
    for element in resolved {
        match element.without_auxiliary_metadata() {
            ResolvedPreviewElement::WithAuxiliary { .. } => {
                return Err(PreviewViewerGpuLayerPreparationError::Compositing {
                    reason: GpuCompositingBlockerReason::EffectRequiresCpu,
                });
            }
            ResolvedPreviewElement::Media {
                frame,
                opacity,
                blend_mode,
                transform,
                effect_graph,
                prepared_effect_route,
                frame_seed,
            } => {
                if !opacity_has_contribution(*opacity) {
                    continue;
                }
                layers.push(ViewerGpuExecutionLayer::Source(Box::new(
                    builder.media_source(
                        frame,
                        *opacity,
                        *blend_mode,
                        *transform,
                        effect_graph,
                        prepared_effect_route.as_deref(),
                        *frame_seed,
                    )?,
                )));
                has_composited_layer = true;
            }
            ResolvedPreviewElement::SolidColor(layer) => {
                if !opacity_has_contribution(layer.opacity) {
                    continue;
                }
                layers.push(ViewerGpuExecutionLayer::Source(Box::new(
                    gpu_solid_source(layer, builder.scratch, None, canvas_extent, sampling)
                        .map_err(
                            |reason| PreviewViewerGpuLayerPreparationError::Compositing { reason },
                        )?,
                )));
                has_composited_layer = true;
            }
            ResolvedPreviewElement::HeterogeneousSolidColor { layer, prepared_route } => {
                if !opacity_has_contribution(layer.opacity) {
                    continue;
                }
                layers.push(ViewerGpuExecutionLayer::Source(Box::new(
                    builder.solid_source(layer, prepared_route)?,
                )));
                has_composited_layer = true;
            }
            ResolvedPreviewElement::Adjustment(layer) => {
                if !has_composited_layer
                    || !opacity_has_contribution(layer.opacity)
                    || (layer.effect_graph.graph().is_identity()
                        && layer.blend_policy == mondrian_effects::EffectPassBlendPolicy::Mix)
                {
                    continue;
                }
                let blend_policy = layer.blend_policy;
                let effect_plan = builder
                    .scratch
                    .get_or_lower_effect_gpu_plan(&layer.effect_graph)
                    .map_err(|_| PreviewViewerGpuLayerPreparationError::Compositing {
                        reason: GpuCompositingBlockerReason::EffectRequiresCpu,
                    })?;
                layers.push(ViewerGpuExecutionLayer::Adjustment {
                    effect_plan,
                    opacity: layer.opacity,
                    blend_policy,
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
    sampling: mondrian_core::ImageSampling,
    scratch: &'a mut TimelineCompositeScratch,
    canvas_extent: EffectFrameExtent,
    identity_effect_plan: Option<Arc<mondrian_effects::CompiledEffectGpuPlan>>,
    items: Vec<HeterogeneousCpuPrefixBatchItem>,
    continuations: Vec<PreviewHeterogeneousGpuContinuationMetadata>,
}

impl<'a> HeterogeneousPreviewLayerBuilder<'a> {
    fn new(
        working_color_space: WorkingColorSpace,
        sampling: mondrian_core::ImageSampling,
        scratch: &'a mut TimelineCompositeScratch,
        canvas_extent: EffectFrameExtent,
    ) -> Self {
        Self {
            working_color_space,
            sampling,
            scratch,
            canvas_extent,
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
        prepared_effect_route: Option<&mondrian_renderer::PreparedTimelinePreviewEffectRoute>,
        frame_seed: i64,
    ) -> Result<ViewerGpuSourceLayer, PreviewViewerGpuLayerPreparationError> {
        // The Renderer already selected the complete compositor, for example
        // to preserve a native effect's source raster. Honor that route before
        // requiring a working payload or a heterogeneous prefix. The existing
        // CPU task owns conversion and late processor-input requests.
        if matches!(
            prepared_effect_route,
            Some(mondrian_renderer::PreparedTimelinePreviewEffectRoute::Cpu { .. })
        ) {
            return Err(PreviewViewerGpuLayerPreparationError::Compositing {
                reason: GpuCompositingBlockerReason::EffectRequiresCpu,
            });
        }
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
            let (native_source, transform) =
                native_source_for_projected_transform(frame, transform);
            return Ok(ViewerGpuSourceLayer::Media {
                frame: frame.working_payload(),
                is_data_texture: frame.is_data_texture(),
                gpu_source: frame.gpu_source(),
                native_source,
                cpu_yuv_source: frame.cpu_yuv_source(),
                heterogeneous_input: None,
                opacity,
                blend_mode,
                transform,
                effect_plan,
                spatial_plan: None,
                frame_seed,
            });
        }

        let address = u32::try_from(self.items.len())
            .map_err(|_| PreviewViewerGpuLayerPreparationError::AddressSpaceExhausted)?;
        let input = frame
            .working_payload()
            .ok_or(PreviewViewerGpuLayerPreparationError::MissingCpuWorkingPayload { address })?;
        let route = prepared_effect_route
            .and_then(|route| route.heterogeneous())
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
            is_data_texture: false,
            gpu_source: None,
            native_source: None,
            cpu_yuv_source: None,
            heterogeneous_input: Some(address),
            opacity,
            blend_mode,
            transform,
            effect_plan: identity_effect_plan,
            spatial_plan: None,
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
            self.sampling,
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
            is_data_texture: false,
            gpu_source: None,
            native_source: None,
            cpu_yuv_source: None,
            heterogeneous_input: Some(address),
            opacity: layer.opacity,
            blend_mode: layer.blend_mode,
            transform: layer.transform,
            effect_plan: identity_effect_plan,
            spatial_plan: None,
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
                gpu_solid_source(layer, self.scratch, None, self.canvas_extent, self.sampling)
                    .map_err(
                        |reason| PreviewViewerGpuLayerPreparationError::Compositing { reason },
                    )?
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
                prepared_effect_route,
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
                    prepared_effect_route.as_deref(),
                    *frame_seed,
                )?
            }
        };
        Ok(ViewerGpuTransitionInput::Source(Box::new(source)))
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
    prepared_effect_route: Option<&mondrian_renderer::PreparedTimelinePreviewEffectRoute>,
    frame_seed: i64,
    working_color_space: WorkingColorSpace,
    scratch: &mut TimelineCompositeScratch,
    auxiliary: Option<&PreparedAuxiliaryRasters<'_>>,
) -> Result<ViewerGpuSourceLayer, GpuCompositingBlockerReason> {
    if matches!(
        prepared_effect_route,
        Some(mondrian_renderer::PreparedTimelinePreviewEffectRoute::Cpu { .. })
    ) {
        return Err(GpuCompositingBlockerReason::EffectRequiresCpu);
    }
    let layer_working_color_space =
        frame.working_color_space().ok_or(GpuCompositingBlockerReason::GpuUnavailable)?;
    if layer_working_color_space != working_color_space
        || !is_preview_gpu_transform_supported(transform)
    {
        return Err(GpuCompositingBlockerReason::UnsupportedTransform);
    }
    let route = scratch
        .get_or_lower_effect_gpu_route(effect_graph)
        .map_err(|_| GpuCompositingBlockerReason::EffectRequiresCpu)?;
    let needs_source_grid =
        matches!(&route, GpuRoute::Spatial(pipeline) if pipeline.requires_image_sampling());
    let (effect_plan, spatial_plan) = match route {
        GpuRoute::Point(effect_plan) if auxiliary.is_none() => (effect_plan, None),
        GpuRoute::Point(_) => return Err(GpuCompositingBlockerReason::EffectRequiresCpu),
        GpuRoute::Spatial(pipeline) => {
            if frame.is_data_texture() {
                return Err(GpuCompositingBlockerReason::EffectRequiresCpu);
            }
            let pipeline = if needs_source_grid {
                let sampling = frame
                    .spatial_image_sampling()
                    .ok_or(GpuCompositingBlockerReason::EffectRequiresCpu)?;
                Arc::new(pipeline.with_image_sampling(sampling))
            } else {
                pipeline
            };
            let (identity, spatial) = gpu_spatial_source_plan(
                pipeline,
                effect_graph,
                EffectFrameExtent::new(frame.width(), frame.height()),
                frame.gpu_source().is_some() || frame.native_source().is_some(),
                auxiliary,
                scratch,
            )?;
            (identity, Some(spatial))
        }
    };
    let (native_source, transform) =
        if auxiliary.is_some() || effect_graph.has_raster_growth() || needs_source_grid {
            // Registration and raster expansion use the exact CPU Preview grid.
            (frame.native_source(), transform)
        } else {
            native_source_for_projected_transform(frame, transform)
        };
    Ok(ViewerGpuSourceLayer::Media {
        frame: frame.working_payload(),
        is_data_texture: frame.is_data_texture(),
        gpu_source: frame.gpu_source(),
        native_source,
        cpu_yuv_source: frame.cpu_yuv_source(),
        heterogeneous_input: None,
        opacity,
        blend_mode,
        transform,
        effect_plan,
        spatial_plan,
        frame_seed,
    })
}

fn gpu_spatial_source_plan(
    pipeline: Arc<CompiledEffectGpuPipeline>,
    graph: &Arc<CompiledEffectGraph>,
    extent: EffectFrameExtent,
    source_on_gpu: bool,
    auxiliary: Option<&PreparedAuxiliaryRasters<'_>>,
    scratch: &mut TimelineCompositeScratch,
) -> Result<(Arc<CompiledEffectGpuPlan>, GpuVisualSpatialPlan), GpuCompositingBlockerReason> {
    if let Some(auxiliary) = auxiliary
        && (!Arc::ptr_eq(&auxiliary.binding.graph, graph)
            || auxiliary.binding.primary_extent != extent)
    {
        return Err(GpuCompositingBlockerReason::EffectRequiresCpu);
    }
    let auxiliary = auxiliary.map(PreparedAuxiliaryRasters::gpu_inputs).unwrap_or_default();
    let slots = auxiliary.iter().map(|input| input.slot).collect::<Vec<_>>();
    let plan =
        gpu_spatial_chain_plan_with_auxiliary(&pipeline, extent.width(), extent.height(), &slots);
    // The per-extent choice is recomputed for each frame. Only lowering is cached.
    let choice = choose_spatial_chain_for_plan(
        plan.as_ref(),
        extent.width(),
        extent.height(),
        source_on_gpu,
        scratch.spatial_chain_preference(),
    );
    scratch.record_spatial_chain_choice(&choice);
    if !choice.use_gpu {
        return Err(GpuCompositingBlockerReason::EffectRequiresCpu);
    }
    let identity =
        identity_compiled_effect_graph().ok_or(GpuCompositingBlockerReason::EffectRequiresCpu)?;
    let effect_plan = scratch
        .get_or_lower_effect_gpu_plan(&identity)
        .map_err(|_| GpuCompositingBlockerReason::EffectRequiresCpu)?;
    Ok((effect_plan, GpuVisualSpatialPlan { pipeline, auxiliary }))
}

fn native_source_for_projected_transform(
    frame: &MediaPreviewFrame,
    mut transform: [f32; 6],
) -> (Option<mondrian_renderer::ViewerGpuNativeSource>, [f32; 6]) {
    let Some(mut source) = frame.native_source() else {
        return (None, transform);
    };
    let source_width = source.materialization_width;
    let source_height = source.materialization_height;
    let materialization_width =
        native_materialization_axis(source_width, transform[0], transform[3]);
    let materialization_height =
        native_materialization_axis(source_height, transform[1], transform[4]);

    if materialization_width != source_width || materialization_height != source_height {
        let source_x = source_width as f32 / materialization_width as f32;
        let source_y = source_height as f32 / materialization_height as f32;
        transform[0] *= source_x;
        transform[3] *= source_x;
        transform[1] *= source_y;
        transform[4] *= source_y;
        source.materialization_width = materialization_width;
        source.materialization_height = materialization_height;
    }
    (Some(source), transform)
}

fn native_materialization_axis(source_extent: u32, x: f32, y: f32) -> u32 {
    let projected_extent = (x.hypot(y) * source_extent as f32).ceil();
    if !projected_extent.is_finite() {
        return source_extent;
    }
    (projected_extent as u32).clamp(1, source_extent)
}

fn gpu_solid_source(
    layer: &TimelineSolidColorLayer,
    scratch: &mut TimelineCompositeScratch,
    auxiliary: Option<&PreparedAuxiliaryRasters<'_>>,
    canvas_extent: EffectFrameExtent,
    canvas_sampling: mondrian_core::ImageSampling,
) -> Result<ViewerGpuSourceLayer, GpuCompositingBlockerReason> {
    if !is_preview_gpu_transform_supported(layer.transform) {
        return Err(GpuCompositingBlockerReason::UnsupportedTransform);
    }
    let route = scratch
        .get_or_lower_effect_gpu_route(&layer.effect_graph)
        .map_err(|_| GpuCompositingBlockerReason::EffectRequiresCpu)?;
    let (effect_plan, spatial_plan) = match route {
        GpuRoute::Spatial(pipeline)
            if auxiliary.is_some()
                || layer.effect_graph.has_raster_growth()
                || pipeline.requires_image_sampling() =>
        {
            let pipeline = if pipeline.requires_image_sampling() {
                Arc::new(pipeline.with_image_sampling(canvas_sampling))
            } else {
                pipeline
            };
            let (identity, spatial) = gpu_spatial_source_plan(
                pipeline,
                &layer.effect_graph,
                canvas_extent,
                true,
                auxiliary,
                scratch,
            )?;
            (identity, Some(spatial))
        }
        GpuRoute::Point(plan) if auxiliary.is_none() && !layer.effect_graph.has_raster_growth() => {
            (plan, None)
        }
        _ => return Err(GpuCompositingBlockerReason::EffectRequiresCpu),
    };
    Ok(ViewerGpuSourceLayer::SolidColor { layer: layer.clone(), effect_plan, spatial_plan })
}

fn gpu_transition_input(
    input: &ResolvedPreviewTransitionInput,
    working_color_space: WorkingColorSpace,
    scratch: &mut TimelineCompositeScratch,
    auxiliary: Option<&PreparedAuxiliaryRasters<'_>>,
    canvas_extent: EffectFrameExtent,
    canvas_sampling: mondrian_core::ImageSampling,
) -> Result<ViewerGpuTransitionInput, GpuCompositingBlockerReason> {
    let source = match input {
        ResolvedPreviewTransitionInput::Transparent => {
            if auxiliary.is_some() {
                return Err(GpuCompositingBlockerReason::EffectRequiresCpu);
            }
            return Ok(ViewerGpuTransitionInput::Transparent);
        }
        ResolvedPreviewTransitionInput::SolidColor(layer)
        | ResolvedPreviewTransitionInput::HeterogeneousSolidColor { layer, .. } => {
            gpu_solid_source(layer, scratch, auxiliary, canvas_extent, canvas_sampling)?
        }
        ResolvedPreviewTransitionInput::Media {
            frame,
            opacity,
            blend_mode,
            transform,
            effect_graph,
            prepared_effect_route,
            frame_seed,
        } => gpu_media_source(
            frame,
            *opacity,
            *blend_mode,
            *transform,
            effect_graph,
            prepared_effect_route.as_deref(),
            *frame_seed,
            working_color_space,
            scratch,
            auxiliary,
        )?,
    };
    Ok(ViewerGpuTransitionInput::Source(Box::new(source)))
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
        ResolvedPreviewElement::WithAuxiliary { element, inputs } => {
            resolved_element_is_degraded(element)
                || inputs.iter().any(|input| {
                    input.sources.iter().any(|source| {
                        source.frame.as_ref().is_some_and(|frame| {
                            frame.presentation_quality() == FramePresentationQuality::Degraded
                        })
                    })
                })
        }
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
            ResolvedPreviewElement::WithAuxiliary { element, inputs } => {
                summary.accumulate(resolved_preview_decode_execution(std::slice::from_ref(
                    element,
                )));
                for input in inputs {
                    for source in &input.sources {
                        if let Some(frame) = &source.frame {
                            summary.accumulate(frame.decode_execution());
                        }
                    }
                }
            }
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
    use crate::app::preview_media_frame::MediaPreviewGpuSourceFrame;
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
    fn viewer_adjustment_identity_keeps_explicit_blend_and_skips_only_mix() {
        use mondrian_effects::EffectPassBlendPolicy::{Blend, Mix};
        let graph = identity_compiled_effect_graph().expect("identity graph");
        for (policy, expected_layers) in [(Mix, 1), (Blend(BlendMode::Normal), 2)] {
            let resolved = [
                ordinary_solid(Arc::clone(&graph), 3),
                ResolvedPreviewElement::Adjustment(TimelineAdjustmentLayer {
                    effect_graph: Arc::clone(&graph),
                    opacity: 0.5,
                    blend_policy: policy,
                    frame_seed: 4,
                }),
            ];
            let ordinary = gpu_composite_layers_for_resolved(&resolved, WORKING_SPACE)
                .expect("ordinary Viewer lowering");
            assert_eq!(ordinary.len(), expected_layers);
            let prepared = prepare_gpu_composite_layers_with_heterogeneous_effects(
                &resolved,
                WORKING_SPACE,
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &mut TimelineCompositeScratch::default(),
                cpu_grant(),
                &[],
                EffectFrameExtent::new(1, 1),
            )
            .expect("same canonical Viewer preparation");
            let PreparedPreviewViewerGpuLayers::Ordinary { layers } = prepared else {
                panic!("identity Adjustment needs no heterogeneous processor");
            };
            assert_eq!(layers.len(), expected_layers);
            if let Blend(_) = policy {
                assert!(matches!(
                    layers.last(),
                    Some(ViewerGpuExecutionLayer::Adjustment { blend_policy, frame_seed: 4, .. })
                        if *blend_policy == policy
                ));
            }
        }
    }

    #[test]
    fn verified_render_cache_frame_lowers_as_one_identity_working_layer() {
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: WIDTH,
            height: HEIGHT,
            data: vec![[0.25, 0.5, 1.0, 1.0]; (WIDTH * HEIGHT) as usize],
            color_space: WORKING_SPACE,
        });
        let mut scratch = TimelineCompositeScratch::default();
        let layer = gpu_layer_for_cached_working(frame.clone(), &mut scratch)
            .expect("identity cached layer");
        let ViewerGpuExecutionLayer::Source(source) = layer else {
            panic!("cache hit must be one ordinary source");
        };
        let ViewerGpuSourceLayer::Media {
            frame: Some(lowered),
            gpu_source: None,
            native_source: None,
            cpu_yuv_source: None,
            heterogeneous_input: None,
            opacity,
            blend_mode,
            transform,
            frame_seed,
            ..
        } = *source
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
                sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
                alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
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

    fn data_texture_frame(identity_salt: u64) -> MediaPreviewFrame {
        let mut identity =
            PreviewSemanticIdentityBuilder::new(b"mondrian.preview.data-texture-plan-test.v1");
        identity_salt.hash(&mut identity);
        let numeric = CpuColorFrame::working(WorkingRgbaF32Frame {
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            width: WIDTH,
            height: HEIGHT,
            color_space: WORKING_SPACE,
            data: vec![[-0.25, 0.5, 1.75, 0.8]; (WIDTH * HEIGHT) as usize],
        });
        MediaPreviewFrame::from_source(
            MediaPreviewGpuSourceFrame::new_data_texture(numeric, WORKING_SPACE),
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
        let prepared_effect_route = PreparedHeterogeneousEffectRoute::prepare(
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
            prepared_effect_route: prepared_effect_route.map(|route| {
                Box::new(
                    mondrian_renderer::PreparedTimelinePreviewEffectRoute::Heterogeneous(*route),
                )
            }),
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

                processor_input_plan: None,
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

            processor_input_plan: None,
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
            let ViewerGpuExecutionLayer::Source(source) = layer else {
                panic!("expected source layer");
            };
            assert!(matches!(
                source.as_ref(),
                ViewerGpuSourceLayer::SolidColor { layer, .. }
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
            mondrian_core::ImageSampling::UNIT_SQUARE,
            &mut TimelineCompositeScratch::default(),
            cpu_grant(),
            &[],
            EffectFrameExtent::new(1, 1),
        )
        .expect("ordinary Viewer plan");

        let PreparedPreviewViewerGpuLayers::Ordinary { layers } = prepared else {
            panic!("identity graph must remain on the ordinary Viewer path");
        };
        let [ViewerGpuExecutionLayer::Source(source)] = &layers[..] else {
            panic!("expected one source layer");
        };
        assert!(matches!(
            source.as_ref(),
            ViewerGpuSourceLayer::Media { blend_mode: BlendMode::Screen, .. }
        ));
    }

    #[test]
    fn data_texture_lowers_to_typed_cpu_numeric_upload_without_a_color_source() {
        let graph = identity_compiled_effect_graph().expect("identity graph");
        let resolved = [ResolvedPreviewElement::Media {
            frame: data_texture_frame(9),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: graph,
            prepared_effect_route: None,
            frame_seed: 9,
        }];

        let layers = gpu_composite_layers_for_resolved(&resolved, WORKING_SPACE)
            .expect("DataTexture Viewer plan");
        let [ViewerGpuExecutionLayer::Source(source)] = &layers[..] else {
            panic!("expected one DataTexture source layer");
        };
        let ViewerGpuSourceLayer::Media {
            frame: Some(frame),
            is_data_texture: true,
            gpu_source: None,
            native_source: None,
            cpu_yuv_source: None,
            heterogeneous_input: None,
            ..
        } = source.as_ref()
        else {
            panic!("DataTexture must use the typed CPU numeric upload seam");
        };
        assert_eq!(frame.rgba_f32().data[0], [-0.25, 0.5, 1.75, 0.8]);
    }

    #[test]
    fn tracer_media_produces_an_addressed_cpu_request_and_gpu_binding_metadata() {
        let graph = tracer_graph();
        let resolved = [media(Arc::clone(&graph), 7)];
        let prepared = prepare_gpu_composite_layers_with_heterogeneous_effects(
            &resolved,
            WORKING_SPACE,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            &mut TimelineCompositeScratch::default(),
            cpu_grant(),
            &[],
            EffectFrameExtent::new(1, 1),
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
        let ViewerGpuExecutionLayer::Source(source) = &layers[0] else {
            panic!("expected source layer");
        };
        assert!(matches!(
            source.as_ref(),
            ViewerGpuSourceLayer::Media {
                heterogeneous_input: Some(0),
                frame: None,
                gpu_source: None,
                native_source: None,
                ..
            }
        ));
    }

    #[test]
    fn tracer_solid_produces_a_procedural_cpu_request_without_a_media_payload() {
        let graph = tracer_graph();
        let prepared = prepare_gpu_composite_layers_with_heterogeneous_effects(
            &[solid(Arc::clone(&graph))],
            WORKING_SPACE,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            &mut TimelineCompositeScratch::default(),
            cpu_grant(),
            &[],
            EffectFrameExtent::new(1, 1),
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
        let [ViewerGpuExecutionLayer::Source(source)] = &layers[..] else {
            panic!("expected one source layer");
        };
        assert!(matches!(
            source.as_ref(),
            ViewerGpuSourceLayer::Media {
                heterogeneous_input: Some(0),
                frame: None,
                gpu_source: None,
                native_source: None,
                blend_mode: BlendMode::Screen,
                ..
            }
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
            mondrian_core::ImageSampling::UNIT_SQUARE,
            &mut TimelineCompositeScratch::default(),
            cpu_grant(),
            &[],
            EffectFrameExtent::new(1, 1),
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
                    prepared_effect_route,
                    frame_seed,
                } => ResolvedPreviewTransitionInput::Media {
                    frame,
                    opacity,
                    blend_mode,
                    transform,
                    effect_graph,
                    prepared_effect_route,
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
                    prepared_effect_route,
                    frame_seed,
                } => ResolvedPreviewTransitionInput::Media {
                    frame,
                    opacity,
                    blend_mode,
                    transform,
                    effect_graph,
                    prepared_effect_route,
                    frame_seed,
                },
                _ => unreachable!("media helper"),
            }),
            progress: 0.0,
        };
        let prepared = prepare_gpu_composite_layers_with_heterogeneous_effects(
            &[transition],
            WORKING_SPACE,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            &mut TimelineCompositeScratch::default(),
            cpu_grant(),
            &[],
            EffectFrameExtent::new(1, 1),
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

    /// One-effect graph with the author values the test needs.
    fn single_effect_graph(effect: EffectType, values: &[(&str, f32)]) -> Arc<CompiledEffectGraph> {
        let mutations: Vec<PropertyMutation> = values
            .iter()
            .map(|(parameter, value)| PropertyMutation::SetStaticValue {
                path: effect.property_path(parameter),
                value: PropertyValue::Float(*value),
            })
            .collect();
        let mut node = EffectNode::with_defaults(effect);
        for mutation in mutations {
            node.apply_property_mutation(mutation).expect("set author value");
        }
        PreparedEffectProgram::prepare(&[node], &[], WORKING_SPACE)
            .expect("prepare single-effect program")
            .evaluate(TimelineTime::ZERO)
            .expect("compile single-effect graph")
    }

    fn media_at(
        graph: Arc<CompiledEffectGraph>,
        width: u32,
        height: u32,
        identity_salt: u64,
    ) -> ResolvedPreviewElement {
        let mut identity =
            PreviewSemanticIdentityBuilder::new(b"mondrian.preview.spatial-chain-lowering-test.v1");
        identity_salt.hash(&mut identity);
        let frame = MediaPreviewFrame::from_working(
            CpuColorFrame::working(WorkingRgbaF32Frame {
                sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
                alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
                width,
                height,
                color_space: WORKING_SPACE,
                data: vec![[0.2, 0.4, 0.6, 1.0]; (width * height) as usize],
            }),
            mondrian_core::Resolution { width, height },
            identity.finish_identity(),
            FramePresentationQuality::Ready,
            PreviewDecodeExecutionSummary::default(),
        );
        ResolvedPreviewElement::Media {
            frame,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: graph,
            prepared_effect_route: None,
            frame_seed: identity_salt as i64,
        }
    }

    fn source_layer(layers: &[ViewerGpuExecutionLayer]) -> &ViewerGpuSourceLayer {
        match &layers[0] {
            ViewerGpuExecutionLayer::Source(source) => source,
            _ => panic!("expected one source layer"),
        }
    }

    #[test]
    fn a_twirl_layer_composites_on_the_gpu() {
        let graph = single_effect_graph(EffectType::Twirl, &[("angle", 45.0)]);
        let resolved = [media_at(Arc::clone(&graph), 640, 360, 1)];
        let mut scratch = TimelineCompositeScratch::default();
        let layers =
            gpu_composite_layers_for_resolved_with_session(&resolved, WORKING_SPACE, &mut scratch)
                .expect("a Twirl layer is admitted on the GPU");
        let ViewerGpuSourceLayer::Media { effect_plan, spatial_plan, .. } = source_layer(&layers)
        else {
            panic!("expected a media layer");
        };
        assert!(
            effect_plan.is_identity(),
            "the chain replaces the fused point plan"
        );
        let spatial_plan = spatial_plan.as_ref().expect("the layer carries a spatial chain");
        assert!(spatial_plan.pipeline.spatial_stage_count() >= 1);
        let diagnostics = scratch.spatial_chain_choice_diagnostics();
        assert_eq!(diagnostics.gpu_choices, 1);
        assert_eq!(diagnostics.cpu_choices, 0);
    }

    #[test]
    fn a_gaussian_blur_layer_still_falls_back_to_the_cpu() {
        let graph = single_effect_graph(EffectType::GaussianBlur, &[("radius", 12.0)]);
        let resolved = [media_at(graph, 640, 360, 2)];
        let blocked = match gpu_composite_layers_for_resolved(&resolved, WORKING_SPACE) {
            Ok(_) => panic!("a Gaussian Blur layer is not admitted on the GPU"),
            Err(blocker) => blocker,
        };
        assert_eq!(blocked, GpuCompositingBlockerReason::EffectRequiresCpu);
    }

    #[test]
    fn a_point_only_layer_keeps_the_existing_fused_path() {
        let graph = single_effect_graph(EffectType::BasicCorrection, &[("exposure", 0.25)]);
        let resolved = [media_at(graph, 64, 64, 3)];
        let mut scratch = TimelineCompositeScratch::default();
        let layers =
            gpu_composite_layers_for_resolved_with_session(&resolved, WORKING_SPACE, &mut scratch)
                .expect("a point-only layer stays on the fused path");
        let ViewerGpuSourceLayer::Media { effect_plan, spatial_plan, .. } = source_layer(&layers)
        else {
            panic!("expected a media layer");
        };
        assert!(!effect_plan.is_identity());
        assert!(
            spatial_plan.is_none(),
            "a fused point graph never lowers a chain"
        );
        assert!(scratch.spatial_chain_choice_diagnostics().is_empty());
    }

    #[test]
    fn a_grow_bounds_solid_uses_the_viewer_canvas_grid() {
        let mut builder = mondrian_effects::EffectGraphBuilderState::new();
        builder.append_unary(mondrian_effects::EffectRenderOp::GrowBounds {
            grade: mondrian_effects::GrowBoundsGrade::new(2.5).unwrap(),
        });
        let graph = mondrian_effects::compile_reference_render_graph(builder.finish()).unwrap();
        let extent = EffectFrameExtent::new(7, 5);
        let mut scratch = TimelineCompositeScratch::default();
        scratch.set_spatial_chain_preference(mondrian_renderer::SpatialChainPreference::ForceGpu);
        let plan = prepare_gpu_composite_layers_with_heterogeneous_effects(
            &[ordinary_solid(graph, 31)],
            WORKING_SPACE,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            &mut scratch,
            cpu_grant(),
            &[],
            extent,
        )
        .unwrap();
        let PreparedPreviewViewerGpuLayers::Ordinary { layers } = plan else {
            panic!("Grow Bounds has a whole-layer GPU pass");
        };
        let ViewerGpuSourceLayer::SolidColor { spatial_plan: Some(plan), .. } =
            source_layer(&layers)
        else {
            panic!("the source carries a spatial plan");
        };
        let admitted = gpu_spatial_chain_plan_with_auxiliary(&plan.pipeline, 7, 5, &[]).unwrap();
        assert_eq!(
            admitted.output_bounds.extent(),
            EffectFrameExtent::new(13, 11)
        );
        assert_eq!(admitted.output_bounds.origin(), [-3; 2]);
    }

    #[test]
    fn a_smear_solid_binds_the_actual_viewer_canvas_sampling() {
        use mondrian_core::{ImageSampling, Rational, SampleAspectRatio};
        use mondrian_effects::{
            mask_deformation::{MaskDeformation, MaskDeformationControls},
            EffectGpuSpatialPass, EffectGpuStage, MaskShape,
        };

        let shape =
            |x, width| MaskShape::Rectangle { x, y: x, width, height: width, corner_radius: 0.0 };
        let grade = MaskDeformation::new(
            &shape(0.3, 0.4),
            &shape(0.05, 0.9),
            MaskDeformationControls {
                target: [0.62, 0.43],
                angle_degrees: 23.0,
                scale: 1.0,
                strength: 1.0,
                elasticity: 1,
                density: 2,
            },
        )
        .unwrap();
        let mut builder = mondrian_effects::EffectGraphBuilderState::new();
        builder.append_unary(mondrian_effects::EffectRenderOp::Smear { grade: Arc::new(grade) });
        let graph = mondrian_effects::compile_reference_render_graph(builder.finish()).unwrap();
        let extent = EffectFrameExtent::new(37, 23);
        let sampling = ImageSampling::new(
            SampleAspectRatio::new(4, 3).unwrap(),
            [Rational::new(1, 2), Rational::new(2, 3)],
        )
        .unwrap();
        let mut scratch = TimelineCompositeScratch::default();
        scratch.set_spatial_chain_preference(mondrian_renderer::SpatialChainPreference::ForceGpu);
        let PreparedPreviewViewerGpuLayers::Ordinary { layers } =
            prepare_gpu_composite_layers_with_heterogeneous_effects(
                &[ordinary_solid(graph, 31)],
                WORKING_SPACE,
                sampling,
                &mut scratch,
                cpu_grant(),
                &[],
                extent,
            )
            .unwrap()
        else {
            panic!("Smear needs an ordinary GPU spatial plan");
        };
        let ViewerGpuSourceLayer::SolidColor { spatial_plan: Some(plan), .. } =
            source_layer(&layers)
        else {
            panic!("the solid must retain the Smear pass");
        };
        assert!(matches!(plan.pipeline.stages(), [EffectGpuStage::Spatial {
            pass: EffectGpuSpatialPass::Smear { sampling: Some(bound), .. }, ..
        }] if *bound == sampling));
        let admitted = gpu_spatial_chain_plan_with_auxiliary(
            &plan.pipeline,
            extent.width(),
            extent.height(),
            &[],
        )
        .unwrap();
        assert_eq!(admitted.spatial_passes, 1);
        assert_eq!(admitted.output_bounds.extent(), extent);
    }

    #[test]
    fn a_spatial_layer_with_a_cpu_only_node_stays_whole_on_the_cpu() {
        let mut twirl = EffectNode::with_defaults(EffectType::Twirl);
        twirl
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Twirl.property_path("angle"),
                value: PropertyValue::Float(45.0),
            })
            .expect("set Twirl angle");
        let blur = EffectNode::with_defaults(EffectType::GaussianBlur);
        let graph = PreparedEffectProgram::prepare(&[twirl, blur], &[], WORKING_SPACE)
            .expect("prepare mixed program")
            .evaluate(TimelineTime::ZERO)
            .expect("compile mixed program");
        let resolved = [media_at(graph, 640, 360, 4)];
        let blocked = match gpu_composite_layers_for_resolved(&resolved, WORKING_SPACE) {
            Ok(_) => panic!("a chain with a CPU-only node is not admitted on the GPU"),
            Err(blocker) => blocker,
        };
        assert_eq!(blocked, GpuCompositingBlockerReason::EffectRequiresCpu);
    }
}
