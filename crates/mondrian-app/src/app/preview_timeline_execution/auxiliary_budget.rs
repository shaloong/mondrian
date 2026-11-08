//! Source-ready auxiliary preparation bounds, before any child or plugin DSP.

use super::*;

type ImageDomain = (EffectFrameExtent, mondrian_core::ImageSampling);

pub(super) fn prepared_auxiliary_working_bytes(
    closure: &PreparedVisualFrameClosure<PreparedTimelinePreviewEffectRoutes>,
    sources: &PreviewSourcePreparation,
) -> Result<usize, PreviewUnavailability> {
    let mut peak = 0usize;
    for node in closure.nodes() {
        let resolution = node.execution_resolution();
        let canvas = (
            EffectFrameExtent::new(resolution.width, resolution.height),
            node.materialization_contract()
                .image_sampling(resolution)
                .map_err(|error| refused(error.to_string()))?,
        );
        let mut local = 0usize;
        let mut registered = HashSet::new();
        let mut observe = |placement,
                           graph: &Arc<CompiledEffectGraph>,
                           primary: ImageDomain|
         -> Result<(), PreviewUnavailability> {
            for spec in graph.auxiliary_inputs() {
                let binding = graph
                    .auxiliary_source_binding(spec.slot())
                    .ok_or_else(|| refused("auxiliary budget lost its frozen source binding"))?;
                if matches!(
                    binding.reference(),
                    VisualSourceReference::Clip { .. } | VisualSourceReference::Sequence { .. }
                ) {
                    let (_, evidence, child) = prepared_auxiliary_visual_evidence(
                        closure,
                        node,
                        placement,
                        graph,
                        spec.slot(),
                        binding.reference(),
                    )?;
                    let bytes = super::super::preview_auxiliary::visual_auxiliary_working_bytes(
                        spec.role(),
                        primary,
                        evidence.domain,
                        child.is_some(),
                    )
                    .map_err(|error| error.unavailability())?;
                    local = local
                        .checked_add(bytes)
                        .ok_or_else(|| refused("auxiliary working pixel size overflowed"))?;
                    continue;
                }
                if !matches!(binding.reference(), VisualSourceReference::Asset { .. }) {
                    return Err(refused(
                        "unbound auxiliary source escaped frame preparation",
                    ));
                }
                let request = PreviewAuxiliaryAssetRequest {
                    reference: binding.reference(),
                    sequence_time: node.time(),
                    target_resolution: resolution,
                    input_color: node.color_context().media_input(false),
                };
                let plan = sources
                    .auxiliary
                    .get(&request)
                    .ok_or_else(|| refused("auxiliary budget lost its original source plan"))?;
                let frame = match &plan.media_request {
                    Some(request) => match sources.media.get(request) {
                        Some(PreviewTimelineMediaFrame::Ready(frame)) => Some(frame),
                        _ => {
                            return Err(refused(
                                "auxiliary budget requires the original Ready source",
                            ))
                        }
                    },
                    None => None,
                };
                let registration = super::super::preview_auxiliary::plan_auxiliary_registration(
                    plan,
                    frame,
                    spec.role(),
                    primary,
                    request.input_color.working_color_space,
                )
                .map_err(|error| error.unavailability())?;
                if registered.insert(registration.key) {
                    let bytes =
                        registration.working_bytes().map_err(|error| error.unavailability())?;
                    local = local
                        .checked_add(bytes)
                        .ok_or_else(|| refused("auxiliary working pixel size overflowed"))?;
                }
            }
            Ok(())
        };
        for element in &node.evaluation().plan().elements {
            match element {
                TimelineRenderPlanElement::Media(value) => observe(
                    Some(value.placement),
                    &value.effect_graph,
                    media_domain(node, value, sources, canvas)?,
                )?,
                TimelineRenderPlanElement::SolidColor(value) => {
                    observe(Some(value.placement), &value.effect_graph, canvas)?
                }
                TimelineRenderPlanElement::Adjustment(value) => {
                    observe(Some(value.placement), &value.effect_graph, canvas)?
                }
                TimelineRenderPlanElement::TimelineGrade(value) => {
                    observe(None, &value.effect_graph, canvas)?
                }
                TimelineRenderPlanElement::BasicTitle(value) => observe(
                    Some(value.placement),
                    &value.effect_graph,
                    title_domain(node, value, sources, canvas)?,
                )?,
                TimelineRenderPlanElement::NestedSequence(value) => observe(
                    Some(value.placement),
                    &value.effect_graph,
                    nested_domain(closure, node, value.placement, canvas, &value.effect_graph)?,
                )?,
                TimelineRenderPlanElement::CrossDissolve(value) => {
                    for input in [&value.left, &value.right] {
                        match input {
                            TimelineTransitionInputPlan::Transparent => {}
                            TimelineTransitionInputPlan::Media(value) => observe(
                                Some(value.placement),
                                &value.effect_graph,
                                media_domain(node, value, sources, canvas)?,
                            )?,
                            TimelineTransitionInputPlan::SolidColor(value) => {
                                observe(Some(value.placement), &value.effect_graph, canvas)?
                            }
                            TimelineTransitionInputPlan::BasicTitle(value) => observe(
                                Some(value.placement),
                                &value.effect_graph,
                                title_domain(node, value, sources, canvas)?,
                            )?,
                            TimelineTransitionInputPlan::NestedSequence(value) => observe(
                                Some(value.placement),
                                &value.effect_graph,
                                nested_domain(
                                    closure,
                                    node,
                                    value.placement,
                                    canvas,
                                    &value.effect_graph,
                                )?,
                            )?,
                        }
                    }
                }
            }
        }
        // Each child composite retires its registered planes before the next
        // node runs. Retained child outputs are in the existing closure bound.
        peak = peak.max(local);
    }
    Ok(peak)
}

fn refused(detail: impl Into<String>) -> PreviewUnavailability {
    PreviewUnavailability::blocked(PreviewOutputStage::InputAdaptation, detail.into())
}

fn media_domain(
    node: &PreparedVisualFrameNode<PreparedTimelinePreviewEffectRoutes>,
    media: &TimelineMediaPlan,
    sources: &PreviewSourcePreparation,
    canvas: ImageDomain,
) -> Result<ImageDomain, PreviewUnavailability> {
    if media.effect_graph.auxiliary_inputs().is_empty() {
        return Ok(canvas);
    }
    let request = preview_timeline_media_request(
        media,
        node.execution_resolution(),
        node.color_context(),
        node.evaluation().payload(),
    )
    .map_err(preview_route_unavailability)?;
    match sources.media.get(&request) {
        Some(PreviewTimelineMediaFrame::Ready(frame)) => {
            frame.cpu_image_domain().ok_or_else(|| {
                refused("auxiliary primary budget requires the actual CPU media domain")
            })
        }
        _ => Err(refused(
            "auxiliary primary budget lost its original Ready media",
        )),
    }
}

fn title_domain(
    node: &PreparedVisualFrameNode<PreparedTimelinePreviewEffectRoutes>,
    title: &TimelineBasicTitlePlan,
    sources: &PreviewSourcePreparation,
    canvas: ImageDomain,
) -> Result<ImageDomain, PreviewUnavailability> {
    if title.effect_graph.auxiliary_inputs().is_empty() {
        return Ok(canvas);
    }
    let request = preview_timeline_title_request(
        node.materialization_contract(),
        title,
        node.execution_resolution(),
        node.color_context().working_color_space(),
    );
    match sources.titles.get(&request.identity()) {
        Some(PreviewTimelineTitleFrame::Ready(frame)) => {
            let frame = frame.frame().rgba_f32();
            Ok((
                EffectFrameExtent::new(frame.width, frame.height),
                frame.sampling,
            ))
        }
        _ => Err(refused(
            "auxiliary primary budget lost its original Ready title",
        )),
    }
}

fn nested_domain(
    closure: &PreparedVisualFrameClosure<PreparedTimelinePreviewEffectRoutes>,
    node: &PreparedVisualFrameNode<PreparedTimelinePreviewEffectRoutes>,
    placement: TimelineClipExecutionRef,
    canvas: ImageDomain,
    graph: &CompiledEffectGraph,
) -> Result<ImageDomain, PreviewUnavailability> {
    if graph.auxiliary_inputs().is_empty() {
        return Ok(canvas);
    }
    let child = node
        .nested_child(placement, PreparedVisualNestedSample::Current)
        .and_then(|child| closure.node(child))
        .ok_or_else(|| refused("auxiliary primary budget lost its frozen nested binding"))?;
    let resolution = child.execution_resolution();
    Ok((
        EffectFrameExtent::new(resolution.width, resolution.height),
        child
            .materialization_contract()
            .image_sampling(resolution)
            .map_err(|error| refused(error.to_string()))?,
    ))
}
