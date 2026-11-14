//! Strict CPU materialization of frozen second-picture sources.

use super::*;
use mondrian_core::timeline_data::TimelineClipEndpointContext;
use mondrian_core::{ImageSampling, SourceSampleTarget, VisualSourceReference, VisualSourceRetime};
use mondrian_effects::{
    AuxiliaryFloatFrame, AuxiliaryFloatInput, AuxiliaryPolicy, AuxiliaryResolution, AuxiliaryRole,
    CompiledEffectGraph, EffectProcessorControl,
};
use mondrian_renderer::{
    register_timeline_auxiliary_raster, timeline_auxiliary_frame_view,
    timeline_auxiliary_registration_geometry, GpuVisualAuxiliaryInput, GpuVisualAuxiliaryPicture,
    PreparedAuxiliaryVisualSource, PreparedVisualAuxiliaryAddress, PreparedVisualAuxiliaryKey,
    TimelineAuxiliarySide,
};
use mondrian_timeline::PictureSourceExtent;

type ImageDomain = (EffectFrameExtent, ImageSampling);
type Media = HashMap<AssetId, crate::preset::ExportMediaDependency>;

enum Source<'a> {
    Visual(Option<&'a CpuColorFrame>),
    Asset(Box<(ExportDecodeCacheKey, ExportVideoLayerDecodeRequest<'a>)>),
}

struct RasterPlan<'a> {
    slot: u32,
    source: Source<'a>,
    domain: ImageDomain,
    policy: AuxiliaryPolicy,
}

struct GraphPlan<'a> {
    index: usize,
    side: TimelineAuxiliarySide,
    graph: Arc<CompiledEffectGraph>,
    primary: EffectFrameExtent,
    rasters: Vec<RasterPlan<'a>>,
}

pub(super) struct RegisteredInputs {
    pub(super) index: usize,
    pub(super) side: TimelineAuxiliarySide,
    pub(super) graph: Arc<CompiledEffectGraph>,
    pub(super) primary: EffectFrameExtent,
    rasters: Vec<(u32, CpuColorFrame, AuxiliaryPolicy)>,
}

impl RegisteredInputs {
    pub(super) fn inputs(&self) -> Vec<AuxiliaryFloatInput<'_>> {
        self.rasters
            .iter()
            .map(|(slot, frame, policy)| AuxiliaryFloatInput {
                slot: *slot,
                raster: AuxiliaryFloatFrame::new(timeline_auxiliary_frame_view(frame), *policy),
            })
            .collect()
    }
}

/// Validate Asset references using only frozen snapshot metadata, before
/// executing any of the frame's producers.
pub(super) fn validate_assets(
    closure: &PreparedExportVisualClosure,
    media: &Media,
) -> Result<(), String> {
    for node in closure.nodes() {
        for element in &node.evaluation().plan().elements {
            element.visit_effect_graphs(|_, graph| {
                for source in graph.auxiliary_source_bindings() {
                    if let VisualSourceReference::Asset { asset_id, retime } = source.reference() {
                        plan_asset(node, media, asset_id, retime, node.time())?;
                    }
                }
                Ok::<(), String>(())
            })?;
        }
        node.evaluation().visit_temporal_auxiliary_demands(
            |key, _, _| {
                if let PreparedVisualAuxiliaryAddress::Authored(VisualSourceReference::Asset {
                    asset_id,
                    retime,
                }) = key.address
                {
                    plan_asset(
                        node,
                        media,
                        asset_id,
                        retime,
                        temporal_consumer_time(node, key)?,
                    )?;
                }
                Ok(())
            },
            |demand| {
                format!(
                    "temporal auxiliary slot {} at {:?} has no binding",
                    demand.slot(),
                    demand.context_time()
                )
            },
        )?;
    }
    Ok(())
}

pub(super) fn prepare_gpu_rasters(
    context: &mut ExportFrameRenderContext<'_>,
    inputs: &ExportNodeInputs<'_>,
    graph: &CompiledEffectGraph,
    placement: TimelineClipExecutionRef,
    primary: (u32, u32),
    cache: &mut HashMap<ExportDecodeCacheKey, Arc<DecodedVideoLayer>>,
) -> Result<Vec<GpuVisualAuxiliaryInput>, String> {
    graph.auxiliary_inputs().iter().map(|spec| {
        let key = auxiliary_key_for_slot(graph, Some(placement), spec.slot())?;
        use PreparedVisualAuxiliaryAddress::{Authored, InputHistory};
        let (picture, (extent, sampling)) = match key.address {
            Authored(VisualSourceReference::Asset { asset_id, retime }) => {
                let (source, domain) = plan_asset(inputs.node(), context.media, asset_id, retime, inputs.node().time())?;
                match source {
                    Source::Asset(request) => {
                        let (cache_key, request) = *request;
                        let decoded = decode_export_picture(
                            cache_key, request, cache, context.stage_diagnostics.as_deref_mut(),
                            context.visual_session, context.cancellation, ExportDecodedSourceRoute::CpuWorking,
                        )?;
                        let frame = decoded.cpu_frame()?;
                        (GpuVisualAuxiliaryPicture::Cpu(Arc::new(frame.clone())), frame_domain(frame))
                    }
                    Source::Visual(None) => transparent_gpu_picture(domain),
                    Source::Visual(Some(_)) => return Err(format!("required export auxiliary Asset {asset_id} resolved a visual child")),
                }
            }
            Authored(VisualSourceReference::Clip { .. } | VisualSourceReference::Sequence { .. }) | InputHistory(_) => {
                match prepared_visual_source(inputs, key)? {
                    PreparedAuxiliaryVisualSource::Child { child, .. } => {
                        let (frame, native_sampling) = match inputs.auxiliary_output(key) {
                            Some(PreparedExportVisualOutput::NestedGpu(frame)) => (frame, None),
                            Some(PreparedExportVisualOutput::EffectInputGpu(frame, sampling)) => (frame, Some(*sampling)),
                            _ => return Err(format!("required export auxiliary source produced no GPU picture: {key:?}")),
                        };
                        let descriptor = frame.descriptor();
                        let node = inputs.closure().node(*child).ok_or("missing auxiliary GPU picture node")?;
                        let resolution = Resolution { width: descriptor.width, height: descriptor.height };
                        let sampling = match native_sampling {
                            Some(sampling) => sampling,
                            None => node.materialization_contract().image_sampling(resolution)
                                .map_err(|error| error.to_string())?,
                        };
                        (GpuVisualAuxiliaryPicture::Gpu(frame.clone()), (EffectFrameExtent::new(descriptor.width, descriptor.height), sampling))
                    }
                    PreparedAuxiliaryVisualSource::Transparent { resolution, sampling, .. } => {
                        transparent_gpu_picture((EffectFrameExtent::new(resolution.width, resolution.height), *sampling))
                    }
                    PreparedAuxiliaryVisualSource::Unavailable(reason) => {
                        return Err(format!("required export auxiliary source {key:?} is unavailable: {reason:?}"));
                    }
                }
            }
            Authored(VisualSourceReference::Unbound) => return Err(format!("required export auxiliary slot {} is unbound", key.slot)),
        };
        // Coverage-only mattes can reconstruct their alpha directly. Layers
        // keep the authored grid so the kernel reconstructs each channel once.
        let policy = if spec.role() == AuxiliaryRole::Matte && (extent.width(), extent.height()) == primary {
            AuxiliaryPolicy::MATCH_PRIMARY
        } else {
            AuxiliaryPolicy::new(AuxiliaryResolution::Authored)
        };
        Ok(GpuVisualAuxiliaryInput { slot: key.slot, picture, sampling, policy, transform: mondrian_effects::IDENTITY_AFFINE })
    }).collect()
}

fn transparent_gpu_picture(domain: ImageDomain) -> (GpuVisualAuxiliaryPicture, ImageDomain) {
    (
        GpuVisualAuxiliaryPicture::Transparent {
            width: domain.0.width(),
            height: domain.0.height(),
        },
        domain,
    )
}

pub(super) fn prepare_rasters(
    inputs: &ExportNodeInputs<'_>,
    elements: &[TimelineCompositeElement<'_>],
    canvas: ImageDomain,
    context: &mut ExportFrameRenderContext<'_>,
    cache: &mut HashMap<ExportDecodeCacheKey, Arc<DecodedVideoLayer>>,
    control: &mut EffectProcessorControl<'_>,
) -> Result<Vec<RegisteredInputs>, String> {
    let node = inputs.node();
    let plan = node.evaluation().plan();
    if plan.elements.len() != elements.len() {
        return Err("export composite lost its planned auxiliary occurrence ordering".into());
    }
    let mut plans = Vec::new();
    let mut registration_bytes = 0u64;
    let mut graph_peak = 0u64;
    for (index, (planned, element)) in plan.elements.iter().zip(elements).enumerate() {
        planned.visit_effect_graphs(|placement, graph| {
            if graph.auxiliary_inputs().is_empty() {
                return Ok(());
            }
            let side = match (node.clip_effect_input(), placement.map(|placement| placement.endpoint)) {
                (Some(_), _) | (_, None | Some(TimelineClipEndpointContext::Ordinary)) => TimelineAuxiliarySide::Element,
                (_, Some(TimelineClipEndpointContext::TransitionLeft { .. })) => TimelineAuxiliarySide::TransitionLeft,
                (_, Some(TimelineClipEndpointContext::TransitionRight { .. })) => TimelineAuxiliarySide::TransitionRight,
            };
            let primary = primary_domain(element, side, graph, canvas)?;
            let mut rasters = Vec::new();
            let mut extents = Vec::new();
            for spec in graph.auxiliary_inputs() {
                let key = auxiliary_key_for_slot(graph, placement, spec.slot())?;
                use PreparedVisualAuxiliaryAddress::{Authored, InputHistory};
                let (source, source_domain) = match key.address {
                    Authored(VisualSourceReference::Clip { .. } | VisualSourceReference::Sequence { .. }) | InputHistory(_) => visual_source(inputs, key)?,
                    Authored(VisualSourceReference::Asset { asset_id, retime }) => plan_asset(node, context.media, asset_id, retime, node.time())?,
                    Authored(VisualSourceReference::Unbound) => return Err(format!("required export auxiliary slot {} is unbound", key.slot)),
                };
                let (domain, policy) = timeline_auxiliary_registration_geometry(spec.role(), primary, source_domain);
                // Source decodes and every possible newly registered plane are
                // charged before any decode or registration allocation.
                if matches!(source, Source::Asset(_)) {
                    registration_bytes = add_bytes(registration_bytes, frame_bytes(source_domain.0)?)?;
                }
                if !matches!(&source, Source::Visual(Some(frame)) if source_domain == domain && !frame.rgba_f32().alpha.is_premultiplied()) {
                    registration_bytes = add_bytes(registration_bytes, frame_bytes(domain.0)?)?;
                }
                extents.push((spec.slot(), domain.0));
                rasters.push(RasterPlan { slot: spec.slot(), source, domain, policy });
            }
            let peak = mondrian_effects::estimate_compiled_float_raster_working_bytes(graph, primary.0, &extents)
                .map_err(|error| format!("export auxiliary graph cannot be sized: {error:?}"))?;
            graph_peak = graph_peak.max(u64::try_from(peak).map_err(|error| error.to_string())?);
            plans.push(GraphPlan { index, side, graph: Arc::clone(graph), primary: primary.0, rasters });
            Ok::<(), String>(())
        })?;
    }
    if plans.is_empty() {
        return Ok(Vec::new());
    }
    let closure_bytes = inputs
        .closure()
        .conservative_cpu_materialization_active_bytes()
        .map_err(|error| error.to_string())?;
    let required = add_bytes(add_bytes(closure_bytes, registration_bytes)?, graph_peak)?;
    context
        .visual_session
        .composite_scratch
        .admit_cpu_active_working_set(required, TimelineCpuCompositePrecision::Float32)
        .map_err(|error| {
            format!("export auxiliary rasters exceed the CPU working-set grant: {error}")
        })?;

    let color = node.color_context().working_color_space();
    let mut registered = Vec::with_capacity(plans.len());
    for plan in plans {
        let mut rasters = Vec::with_capacity(plan.rasters.len());
        for raster in plan.rasters {
            control.check().map_err(|error| error.to_string())?;
            let decoded;
            let source = match raster.source {
                Source::Visual(frame) => frame,
                Source::Asset(request) => {
                    let (key, request) = *request;
                    decoded = decode_export_picture(
                        key,
                        request,
                        cache,
                        context.stage_diagnostics.as_deref_mut(),
                        context.visual_session,
                        context.cancellation,
                        ExportDecodedSourceRoute::CpuWorking,
                    )?;
                    Some(decoded.cpu_frame()?)
                }
            };
            let frame = register_timeline_auxiliary_raster(
                source,
                raster.domain.0,
                raster.domain.1,
                color,
                control,
            )
            .map_err(|error| {
                format!(
                    "export auxiliary slot {} registration failed: {error}",
                    raster.slot
                )
            })?;
            rasters.push((raster.slot, frame, raster.policy));
        }
        registered.push(RegisteredInputs {
            index: plan.index,
            side: plan.side,
            graph: plan.graph,
            primary: plan.primary,
            rasters,
        });
    }
    Ok(registered)
}

fn frame_bytes(extent: EffectFrameExtent) -> Result<u64, String> {
    (u64::from(extent.width()) * u64::from(extent.height()))
        .checked_mul(16)
        .ok_or_else(|| "export auxiliary extent exceeds the CPU byte range".into())
}

fn add_bytes(total: u64, added: u64) -> Result<u64, String> {
    total
        .checked_add(added)
        .ok_or_else(|| "export auxiliary working bytes overflowed".into())
}

fn frame_domain(frame: &CpuColorFrame) -> ImageDomain {
    let frame = frame.rgba_f32();
    (
        EffectFrameExtent::new(frame.width, frame.height),
        frame.sampling,
    )
}

fn primary_domain(
    element: &TimelineCompositeElement<'_>,
    side: TimelineAuxiliarySide,
    planned: &Arc<CompiledEffectGraph>,
    canvas: ImageDomain,
) -> Result<ImageDomain, String> {
    let invalid = || "export auxiliary consumer lost its planned graph or endpoint".to_owned();
    let (graph, domain) = match (element, side) {
        (TimelineCompositeElement::Media(layer), TimelineAuxiliarySide::Element) => {
            (&layer.effect_graph, frame_domain(layer.frame))
        }
        (TimelineCompositeElement::SolidColor(layer), TimelineAuxiliarySide::Element) => {
            (&layer.effect_graph, canvas)
        }
        (TimelineCompositeElement::Adjustment(layer), TimelineAuxiliarySide::Element) => {
            (&layer.effect_graph, canvas)
        }
        (TimelineCompositeElement::CrossDissolve(transition), side) => {
            let input = match side {
                TimelineAuxiliarySide::TransitionLeft => &transition.left,
                TimelineAuxiliarySide::TransitionRight => &transition.right,
                TimelineAuxiliarySide::Element => return Err(invalid()),
            };
            match input {
                TimelineTransitionInput::Media(layer) => {
                    (&layer.effect_graph, frame_domain(layer.frame))
                }
                TimelineTransitionInput::SolidColor(layer) => (&layer.effect_graph, canvas),
                TimelineTransitionInput::Transparent => return Err(invalid()),
            }
        }
        _ => return Err(invalid()),
    };
    if !Arc::ptr_eq(graph, planned) {
        return Err(invalid());
    }
    Ok(domain)
}

fn visual_source<'a>(
    inputs: &'a ExportNodeInputs<'_>,
    key: PreparedVisualAuxiliaryKey,
) -> Result<(Source<'a>, ImageDomain), String> {
    match prepared_visual_source(inputs, key)? {
        PreparedAuxiliaryVisualSource::Child { .. } => {
            let Some(PreparedExportVisualOutput::NestedCpu(frame)) = inputs.auxiliary_output(key)
            else {
                return Err(format!(
                    "required export auxiliary source produced no CPU picture: {key:?}"
                ));
            };
            Ok((Source::Visual(Some(frame)), frame_domain(frame)))
        }
        PreparedAuxiliaryVisualSource::Transparent { resolution, sampling, .. } => Ok((
            Source::Visual(None),
            (
                EffectFrameExtent::new(resolution.width, resolution.height),
                *sampling,
            ),
        )),
        PreparedAuxiliaryVisualSource::Unavailable(reason) => Err(format!(
            "required export auxiliary source {key:?} is unavailable: {reason:?}"
        )),
    }
}

fn prepared_visual_source<'a>(
    inputs: &'a ExportNodeInputs<'_>,
    key: PreparedVisualAuxiliaryKey,
) -> Result<&'a PreparedAuxiliaryVisualSource, String> {
    inputs
        .node()
        .auxiliary_visual_sources()
        .iter()
        .find_map(|(candidate, source)| (*candidate == key).then_some(source))
        .ok_or_else(|| format!("required export auxiliary source has no frozen binding: {key:?}"))
}

fn auxiliary_key_for_slot(
    graph: &CompiledEffectGraph,
    placement: Option<TimelineClipExecutionRef>,
    slot: u32,
) -> Result<PreparedVisualAuxiliaryKey, String> {
    PreparedVisualAuxiliaryKey::for_slot(graph, placement, slot)
        .ok_or_else(|| format!("required export auxiliary slot {slot} has no frozen source"))
}

fn plan_asset<'a>(
    node: &PreparedVisualFrameNode<Vec<PreparedExportHeterogeneousElement>>,
    media: &'a Media,
    asset_id: AssetId,
    retime: VisualSourceRetime,
    consumer_time: TimelineTime,
) -> Result<(Source<'a>, ImageDomain), String> {
    let dependency = media
        .get(&asset_id)
        .ok_or_else(|| format!("export snapshot is missing required auxiliary Asset {asset_id}"))?;
    if dependency.interpretation.payload.is_non_color_data() {
        return Err(format!(
            "required auxiliary Asset {asset_id} is not a color-managed picture"
        ));
    }
    let (sample, proof_sample) = match dependency.picture_source_extent {
        Some(PictureSourceExtent::Still) => {
            consumer_time
                .checked_add(retime.time_offset)
                .map_err(|error| error.to_string())?;
            let sample = SourceSampleTarget::covering(TimelineTime::ZERO);
            (Some(sample), sample)
        }
        Some(PictureSourceExtent::TimelineRange(range)) => {
            let time = retime.sample(consumer_time, range).map_err(|error| error.to_string())?;
            (
                time.source_sample,
                SourceSampleTarget::covering(range.start),
            )
        }
        None => {
            return Err(format!(
                "required auxiliary Asset {asset_id} has no frozen picture extent"
            ))
        }
    };
    let (key, request) = prepare_export_picture_decode(
        media,
        ExportPictureDecodeSource {
            asset_id,
            source_sample: sample.unwrap_or(proof_sample),
            alpha_interpretation: AlphaInterpretation::Straight,
            color_space_override: None,
            auto_tone_map: false,
            pixel_aspect_ratio_override: None,
            field_order_override: None,
            unit: true,
        },
        node.execution_resolution(),
        node.color_context(),
        None,
        ExportDecodedSourceRoute::CpuWorking,
    )?;
    let domain = (
        EffectFrameExtent::new(
            request.source_resolution.width,
            request.source_resolution.height,
        ),
        request
            .picture_geometry
            .image_sampling(request.source_resolution)
            .map_err(|error| error.to_string())?,
    );
    let source = match sample {
        Some(_) => Source::Asset(Box::new((key, request))),
        None => Source::Visual(None),
    };
    Ok((source, domain))
}

fn temporal_consumer_time(
    node: &PreparedVisualFrameNode<Vec<PreparedExportHeterogeneousElement>>,
    key: PreparedVisualAuxiliaryKey,
) -> Result<TimelineTime, String> {
    let placement = key.placement.ok_or("temporal auxiliary has no Clip placement")?;
    node.program()
        .schedule()
        .clip_to_sequence_time(placement, placement.clip_time)
        .map_err(|error| error.to_string())
}

pub(super) fn prepare_temporal_rasters(
    inputs: &ExportNodeInputs<'_>,
    batch: &TimelineTemporalDemandBatch,
    context: &mut ExportFrameRenderContext<'_>,
    cache: &mut HashMap<ExportDecodeCacheKey, Arc<DecodedVideoLayer>>,
    control: &mut EffectProcessorControl<'_>,
) -> Result<Option<mondrian_effects::PreparedTemporalAuxiliarySet>, String> {
    let demands = batch.auxiliary_demands();
    if demands.is_empty() {
        return Ok(None);
    }
    let node = inputs.node();
    let request = batch.execution_request();
    let primary = (request.frame_extent(), request.sampling());
    let mut planned = Vec::with_capacity(demands.demands().len());
    let mut bytes = inputs
        .closure()
        .conservative_cpu_materialization_active_bytes()
        .map_err(|error| error.to_string())?;
    batch.visit_auxiliary_demands(
        |key, graph, demand| {
            let (source, domain) = match key.address {
                PreparedVisualAuxiliaryAddress::Authored(VisualSourceReference::Asset {
                    asset_id,
                    retime,
                }) => plan_asset(
                    node,
                    context.media,
                    asset_id,
                    retime,
                    temporal_consumer_time(node, key)?,
                )?,
                _ => visual_source(inputs, key)?,
            };
            let spec = graph
                .auxiliary_inputs()
                .iter()
                .find(|spec| spec.slot() == key.slot)
                .ok_or_else(|| format!("temporal auxiliary lost slot: {key:?}"))?;
            let (registered, _) =
                timeline_auxiliary_registration_geometry(spec.role(), primary, domain);
            bytes = add_bytes(
                bytes,
                add_bytes(frame_bytes(registered.0)?, frame_bytes(registered.0)?)?,
            )?;
            if matches!(source, Source::Asset(_)) {
                bytes = add_bytes(bytes, frame_bytes(domain.0)?)?;
            }
            planned.push((demand, source, registered));
            Ok(())
        },
        |demand| {
            format!(
                "temporal auxiliary slot {} at {:?} has no binding",
                demand.slot(),
                demand.context_time()
            )
        },
    )?;
    context
        .visual_session
        .composite_scratch
        .admit_cpu_active_working_set(bytes, TimelineCpuCompositePrecision::Float32)
        .map_err(|error| error.to_string())?;
    let mut resolved = Vec::with_capacity(planned.len());
    for (demand, source, (extent, sampling)) in planned {
        control.check().map_err(|error| error.to_string())?;
        let decoded;
        let frame = match source {
            Source::Visual(frame) => frame,
            Source::Asset(request) => {
                let (key, request) = *request;
                decoded = decode_export_picture(
                    key,
                    request,
                    cache,
                    context.stage_diagnostics.as_deref_mut(),
                    context.visual_session,
                    context.cancellation,
                    ExportDecodedSourceRoute::CpuWorking,
                )?;
                Some(decoded.cpu_frame()?)
            }
        };
        let registered = register_timeline_auxiliary_raster(
            frame,
            extent,
            sampling,
            node.color_context().working_color_space(),
            control,
        )
        .map_err(|error| error.to_string())?;
        let view = timeline_auxiliary_frame_view(&registered);
        let raster = mondrian_effects::EffectFloatFrame::from_pixels(
            view.extent(),
            view.format(),
            view.sampling(),
            view.pixels().to_vec(),
        )
        .map_err(|error| error.to_string())?;
        resolved.push((
            demand.request(demands.generation(), extent, sampling),
            raster,
        ));
    }
    mondrian_effects::PreparedTemporalAuxiliarySet::prepare(
        EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([0; 32]),
        demands.generation(),
        resolved,
    )
    .map(Some)
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset::TimelineExportRange;
    use mondrian_core::{Color, ColorFrameAlpha, PropertyBag, PropertyDescriptor, PropertyValue};
    use mondrian_effects::{
        instantiate_effect_node, register_effect_definition, AuxiliaryRole, EffectColorDomain,
        EffectColorDomainContract, EffectDefinition, EffectExecutionContract, EffectExecutionModes,
        EffectRenderOp, EffectRoiPropagation, EffectType, EffectValueFormat, FillGrade,
    };
    use mondrian_timeline::{Clip, Sequence};

    fn solid(color: Color, start: TimelineTime) -> Clip {
        Clip::new_solid_color(AssetId::new(), color, start, TimelineTime::ONE).unwrap()
    }

    fn matte(reference: VisualSourceReference) -> mondrian_effects::EffectNode {
        let kind = EffectType::External(format!(
            "test.export.auxiliary.{}",
            mondrian_core::EffectId::new()
        ));
        let parameter = kind.parameter_id("source").unwrap();
        let mut properties = PropertyBag::default();
        properties.define(
            PropertyDescriptor::new(
                "source",
                "Matte source",
                PropertyValue::VisualSource(reference),
            )
            .with_parameter_id(parameter.clone()),
        );
        register_effect_definition(
            EffectDefinition::new(
                kind.key(),
                "Export matte",
                properties,
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
                roi_propagation: EffectRoiPropagation::PixelLocal,
                ..EffectExecutionContract::IDENTITY
            })
            .with_branching_graph_builder(Arc::new(move |effect, _, builder| {
                let input = builder.current_output();
                let auxiliary = builder.append_visual_source_input(
                    effect,
                    &parameter,
                    EffectValueFormat::new(
                        EffectWorkingPrecision::Float32,
                        EffectColorDomain::SceneLinearRgb,
                        ColorFrameAlpha::StraightCoverage,
                    ),
                    EffectColorDomainContract::SCENE_LINEAR,
                )?;
                let output = builder.add_auxiliary_effect_with_role(
                    input,
                    auxiliary,
                    EffectRenderOp::Fill {
                        grade: FillGrade::new(0.0, 0.0, false, [1.0, 0.0, 0.0], 1.0).unwrap(),
                    },
                    AuxiliaryRole::Matte,
                );
                builder.set_current_output(output);
                Ok(())
            })),
        )
        .unwrap();
        instantiate_effect_node(kind).unwrap()
    }

    fn snapshot(reference: VisualSourceReference) -> TimelineExportSnapshot {
        snapshot_with_effect(matte(reference))
    }

    fn snapshot_with_effect(effect: mondrian_effects::EffectNode) -> TimelineExportSnapshot {
        let mut sequence = Sequence::new("Export auxiliary consumer");
        sequence.settings.resolution = Resolution { width: 16, height: 16 };
        let mut consumer = solid(Color::from_rgba8(0, 0, 0, 255), TimelineTime::ZERO);
        consumer.add_effect_node(effect);
        sequence.video_tracks[1].add_clip(consumer).unwrap();
        TimelineExportSnapshot {
            sequence,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::SequenceInOut,
        }
    }

    fn render(
        timeline: &TimelineExportSnapshot,
        output: &mut Option<CpuColorFrame>,
    ) -> Result<(), String> {
        let color_context = timeline
            .sequence
            .settings
            .root_program_color_context(&timeline.color_environment)
            .unwrap();
        let cancellation = ExecutionCancellationToken::new();
        let mut visual_session = ExportVisualRenderSession::default();
        let mut context = ExportFrameRenderContext {
            media: &timeline.media,
            color_environment: &timeline.color_environment,
            alpha_mode: ExportAlphaMode::Preserve,
            delivery_pixels: ExportDeliveryPixelContract::unmodified(
                ExportFrameContract::EncodedRgba8Unorm,
            ),
            input_color_counts: None,
            stage_diagnostics: None,
            composite_diagnostics: None,
            export_diagnostics: None,
            visual_session: &mut visual_session,
            cancellation: &cancellation,
        };
        render_sequence_frame_into(
            timeline,
            &mut context,
            &timeline.sequence,
            0,
            timeline.sequence.settings.resolution,
            color_context,
            SequenceRenderTarget::Working(output),
        )
    }

    #[test]
    fn export_auxiliary_clip_renders_its_own_clamped_frame() {
        let source = solid(Color::from_rgba8(0, 255, 0, 255), TimelineTime::ONE);
        let mut timeline = snapshot(VisualSourceReference::Clip {
            clip_id: source.id,
            retime: Default::default(),
        });
        timeline.sequence.video_tracks[0].add_clip(source).unwrap();
        let mut output = None;
        render(&timeline, &mut output).unwrap();
        assert!(output
            .unwrap()
            .rgba_f32()
            .data
            .iter()
            .all(|pixel| *pixel == [1.0, 0.0, 0.0, 1.0]));
    }

    #[test]
    fn export_auxiliary_sequence_registers_its_actual_grid() {
        let mut source = Sequence::new("Export auxiliary source");
        source.settings.resolution = Resolution { width: 32, height: 32 };
        source.video_tracks[0]
            .add_clip(solid(Color::from_rgba8(0, 255, 0, 255), TimelineTime::ZERO))
            .unwrap();
        let mut timeline = snapshot(VisualSourceReference::Sequence {
            sequence_id: source.id,
            retime: Default::default(),
        });
        timeline.sequences.push(source);
        let mut output = None;
        render(&timeline, &mut output).unwrap();
        assert!(output
            .unwrap()
            .rgba_f32()
            .data
            .iter()
            .all(|pixel| *pixel == [1.0, 0.0, 0.0, 1.0]));
    }

    #[test]
    #[ignore = "real GPU Export second-picture qualification; run independently"]
    fn export_gpu_auxiliary_sequence_matches_cpu_float32() {
        assert_export_gpu_auxiliary_sequence(
            matte,
            Color { r: 0.0, g: 1.0, b: 0.0, a: 0.5 },
            [0.5, 0.0, 0.0, 1.0],
        );
    }

    #[test]
    #[ignore = "real GPU Export second-picture qualification; run independently"]
    fn export_gpu_channel_combiner_second_layer_matches_cpu_float32() {
        assert_export_gpu_auxiliary_sequence(
            |reference| {
                let kind = EffectType::ChannelCombiner;
                let mut effect = instantiate_effect_node(kind.clone()).unwrap();
                for (key, value) in [
                    ("use_second_layer", PropertyValue::Bool(true)),
                    (
                        "second_layer_source",
                        PropertyValue::VisualSource(reference),
                    ),
                    ("to", PropertyValue::Enum("red".into())),
                ] {
                    effect
                        .set_static_value_by_parameter(&kind.parameter_id(key).unwrap(), value)
                        .unwrap();
                }
                effect
            },
            Color { r: 0.0, g: 1.0, b: 0.0, a: 0.5 },
            [0.5, 0.0, 0.0, 1.0],
        );
    }

    #[test]
    #[ignore = "real GPU Export second-picture qualification; run independently"]
    fn export_gpu_displacement_map_expands_the_layer_without_shifting_the_original() {
        assert_export_gpu_auxiliary_sequence(
            |reference| {
                let kind = EffectType::DisplacementMap;
                let mut effect = instantiate_effect_node(kind.clone()).unwrap();
                for (key, value) in [
                    ("map_source", PropertyValue::VisualSource(reference)),
                    ("horizontal_channel", PropertyValue::Enum("alpha".into())),
                    ("vertical_channel", PropertyValue::Enum("alpha".into())),
                ] {
                    effect
                        .set_static_value_by_parameter(&kind.parameter_id(key).unwrap(), value)
                        .unwrap();
                }
                effect
            },
            Color { r: 0.5, g: 0.5, b: 0.0, a: 0.5 },
            [0.0, 0.0, 0.0, 1.0],
        );
    }

    #[test]
    #[ignore = "real GPU Export second-picture qualification; run independently"]
    fn export_glass_with_a_separate_sequence_bump_map_matches_cpu_float32() {
        let mut source = Sequence::new("Glass bump map");
        source.settings.resolution = Resolution { width: 31, height: 17 };
        let mut bump = solid(Color::BLACK, TimelineTime::ZERO);
        let mut ramp = instantiate_effect_node(EffectType::Ramp).unwrap();
        for (key, point) in [("start", [0.0_f32, 0.0]), ("end", [31.0, 17.0])] {
            ramp.set_static_value_by_parameter(
                &EffectType::Ramp.parameter_id(key).unwrap(),
                PropertyValue::Vec2(point.into()),
            )
            .unwrap();
        }
        bump.add_effect_node(ramp);
        source.video_tracks[0].add_clip(bump).unwrap();
        let mut glass = instantiate_effect_node(EffectType::Glass).unwrap();
        for (key, value) in [
            (
                "bump_map",
                PropertyValue::VisualSource(VisualSourceReference::Sequence {
                    sequence_id: source.id,
                    retime: Default::default(),
                }),
            ),
            ("softness", PropertyValue::Float(0.0)),
            ("metal", PropertyValue::Float(0.0)),
            ("roughness", PropertyValue::Float(0.25)),
            ("light_type", PropertyValue::Enum("point".to_owned())),
        ] {
            glass
                .set_static_value_by_parameter(&EffectType::Glass.parameter_id(key).unwrap(), value)
                .unwrap();
        }
        let mut timeline = snapshot_with_effect(glass);
        timeline.sequences.push(source);
        let (cpu, gpu) = render_export_gpu_auxiliary_pair(&timeline, 0, 1);
        let alpha_error =
            cpu.chunks_exact(4).map(|pixel| (pixel[3] - 1.0).abs()).fold(0.0, f32::max);
        assert!(
            alpha_error <= 1.0e-6,
            "opaque fixture alpha error {alpha_error:e}"
        );
        assert!(cpu.chunks_exact(4).any(|pixel| pixel[0] > 0.0));
        let first = cpu[0];
        assert!(cpu.chunks_exact(4).any(|pixel| (pixel[0] - first).abs() > 0.01));
        assert_auxiliary_float32_parity(&cpu, &gpu);
    }

    fn assert_export_gpu_auxiliary_sequence(
        effect: impl FnOnce(VisualSourceReference) -> mondrian_effects::EffectNode,
        map_color: Color,
        expected: [f32; 4],
    ) {
        let mut source = Sequence::new("Export GPU matte source");
        source.settings.resolution = Resolution { width: 31, height: 17 };
        source.video_tracks[0].add_clip(solid(map_color, TimelineTime::ZERO)).unwrap();
        let mut timeline = snapshot_with_effect(effect(VisualSourceReference::Sequence {
            sequence_id: source.id,
            retime: Default::default(),
        }));
        timeline.sequences.push(source);
        let mut working = None;
        render(&timeline, &mut working).expect("CPU working reference");
        assert!(working.unwrap().rgba_f32().data.iter().all(|pixel| pixel
            .iter()
            .zip(expected)
            .all(|(actual, expected)| (actual - expected).abs() < 1.0e-6)));
        let (cpu, gpu) = render_export_gpu_auxiliary_pair(&timeline, 0, 1);
        assert!(cpu.chunks_exact(4).all(|pixel| pixel[3] == 1.0));
        assert_auxiliary_float32_parity(&cpu, &gpu);
    }

    fn render_float_auxiliary_delivery(
        timeline: &TimelineExportSnapshot,
        frame: i64,
        session: &mut ExportVisualRenderSession,
        allow_gpu: bool,
    ) -> Vec<f32> {
        let color = timeline
            .sequence
            .settings
            .root_program_color_context(&timeline.color_environment)
            .unwrap();
        let cancellation = ExecutionCancellationToken::new();
        let contract = ExportFrameContract::FloatMasterRgba32;
        session.resource_policy.opportunistic_gpu_acceleration = allow_gpu;
        session
            .composite_scratch
            .set_spatial_chain_preference(mondrian_renderer::SpatialChainPreference::ForceGpu);
        let mut packed = Vec::new();
        let mut context = ExportFrameRenderContext {
            media: &timeline.media,
            color_environment: &timeline.color_environment,
            alpha_mode: ExportAlphaMode::Preserve,
            delivery_pixels: ExportDeliveryPixelContract::unmodified(contract),
            input_color_counts: None,
            stage_diagnostics: None,
            composite_diagnostics: None,
            export_diagnostics: None,
            visual_session: session,
            cancellation: &cancellation,
        };
        render_sequence_frame_into(
            timeline,
            &mut context,
            &timeline.sequence,
            frame,
            timeline.sequence.settings.resolution,
            color,
            SequenceRenderTarget::Deliverable(&mut packed),
        )
        .expect("Export float deliverable");
        contract.to_rgba_f32(&packed).expect("float pipe frame")
    }

    fn render_export_gpu_auxiliary_pair(
        timeline: &TimelineExportSnapshot,
        frame: i64,
        expected_nested_outputs: u64,
    ) -> (Vec<f32>, Vec<f32>) {
        let _gpu = GpuVisualExecutionGuard::activate();
        let mut session = ExportVisualRenderSession::default();
        let cpu = render_float_auxiliary_delivery(timeline, frame, &mut session, false);
        let gpu = render_float_auxiliary_delivery(timeline, frame, &mut session, true);
        let diagnostics = session.visual_diagnostics();
        assert_eq!(diagnostics.gpu_visual_spatial_effect_layers, 1);
        assert_eq!(
            diagnostics.gpu_visual_nested_outputs,
            expected_nested_outputs
        );
        assert_eq!(diagnostics.gpu_visual_output_readbacks, 1);
        assert_eq!(diagnostics.gpu_visual_spatial_cpu_retries, 0);
        assert_eq!(gpu.len(), 16 * 16 * 4);
        assert_eq!(gpu.len(), cpu.len());
        (cpu, gpu)
    }

    #[test]
    #[ignore = "real GPU historical Video Collage qualification; run independently"]
    fn export_historical_video_collage_samples_the_entering_effect_without_a_readback() {
        use mondrian_core::Keyframe;
        let kind = EffectType::VideoCollage;
        let mut collage = instantiate_effect_node(kind.clone()).unwrap();
        for (key, value) in [
            ("columns", PropertyValue::Int(2)),
            ("rows", PropertyValue::Int(1)),
            ("spacing", PropertyValue::Float(0.0)),
            ("tile_delay", PropertyValue::Float(0.25)),
        ] {
            collage
                .set_static_value_by_parameter(&kind.parameter_id(key).unwrap(), value)
                .unwrap();
        }
        let mut timeline = snapshot_with_effect(collage);
        timeline.sequence.settings.frame_rate = Rational::new(8, 1);
        let mut prefix = instantiate_effect_node(EffectType::BasicCorrection).unwrap();
        let exposure = EffectType::BasicCorrection.property_path("exposure");
        for (time, stops) in [
            (TimelineTime::ZERO, 0.0),
            (TimelineTime::new(1, 2).unwrap(), 2.0),
        ] {
            prefix
                .properties
                .set_keyframe(
                    &exposure,
                    Keyframe::linear(time, PropertyValue::Float(stops)),
                )
                .unwrap();
        }
        let clip = &mut timeline.sequence.video_tracks[1].clips[0];
        let collage = clip.effects[0].clone();
        clip.effects = Default::default();
        clip.content = mondrian_core::timeline_data::ClipContent::SolidColor {
            asset_id: AssetId::new(),
            color: Color::from_rgba8(40, 64, 80, 255),
        };
        clip.add_effect_node(prefix);
        let independent = timeline.sequence.clone();
        timeline.sequence.video_tracks[1].clips[0].add_effect_node(collage);
        let mut reference = ExportVisualRenderSession::default();
        let source = TimelineExportSnapshot {
            sequence: independent,
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: timeline.color_environment.clone(),
            prepared_execution: None,
            range: TimelineExportRange::EntireSequence,
        };
        let expected = [4, 2].map(|frame| {
            render_float_auxiliary_delivery(&source, frame, &mut reference, false)[..4]
                .try_into()
                .unwrap()
        });
        let (cpu, gpu) = render_export_gpu_auxiliary_pair(&timeline, 4, 1);
        assert_ne!(expected[0], expected[1]);
        for row in cpu.chunks_exact(16 * 4) {
            for (column, pixel) in row.chunks_exact(4).enumerate() {
                let expected: [f32; 4] = expected[usize::from(column >= 8)];
                assert!(
                    pixel.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1.0e-5),
                    "column {column}: {pixel:?}, expected {expected:?}"
                );
            }
        }
        assert_auxiliary_float32_parity(&cpu, &gpu);
    }

    #[test]
    #[ignore = "real GPU Export Video Collage qualification; run independently"]
    fn export_video_collage_resolves_two_distinct_sequence_grids_and_tile_times() {
        let rate = mondrian_core::Rational::new(8, 1);
        let child = |name: &str, resolution, colors: [Color; 2]| {
            let mut sequence = Sequence::new(name);
            sequence.settings.resolution = resolution;
            sequence.settings.frame_rate = rate;
            for (index, color) in colors.into_iter().enumerate() {
                let duration = TimelineTime::new(1, 2).unwrap();
                let clip = Clip::new_solid_color(
                    AssetId::new(),
                    color,
                    TimelineTime::new(index as i64, 2).unwrap(),
                    duration,
                )
                .unwrap();
                sequence.video_tracks[0].add_clip(clip).unwrap();
            }
            sequence
        };
        let first = child(
            "Red at output time",
            Resolution { width: 31, height: 17 },
            [Color::BLACK, Color::from_hex(0xff0000)],
        );
        let second = child(
            "Green one tile delay earlier",
            Resolution { width: 17, height: 23 },
            [Color::from_hex(0x00ff00), Color::from_hex(0x0000ff)],
        );
        let kind = EffectType::VideoCollage;
        let mut effect = instantiate_effect_node(kind.clone()).unwrap();
        let source = |sequence_id| {
            PropertyValue::VisualSource(VisualSourceReference::Sequence {
                sequence_id,
                retime: Default::default(),
            })
        };
        for (key, value) in [
            ("columns", PropertyValue::Int(2)),
            ("rows", PropertyValue::Int(1)),
            ("spacing", PropertyValue::Float(0.0)),
            ("tile_delay", PropertyValue::Float(0.25)),
            ("tile_source_01", source(first.id)),
            ("tile_source_02", source(second.id)),
        ] {
            effect
                .set_static_value_by_parameter(&kind.parameter_id(key).unwrap(), value)
                .unwrap();
        }
        let mut timeline = snapshot_with_effect(effect);
        timeline.sequence.settings.frame_rate = rate;
        timeline.sequences.extend([first, second]);
        let mut reference = ExportVisualRenderSession::default();
        let expected = [(0, 4), (1, 2)].map(|(child, time)| {
            let independent = TimelineExportSnapshot {
                sequence: timeline.sequences[child].clone(),
                sequences: Vec::new(),
                media: HashMap::new(),
                color_environment: timeline.color_environment.clone(),
                prepared_execution: None,
                range: TimelineExportRange::EntireSequence,
            };
            render_float_auxiliary_delivery(&independent, time, &mut reference, false)[..4]
                .try_into()
                .unwrap()
        });
        let (cpu, gpu) = render_export_gpu_auxiliary_pair(&timeline, 4, 2);
        for row in cpu.chunks_exact(16 * 4) {
            for (column, pixel) in row.chunks_exact(4).enumerate() {
                let expected: [f32; 4] = expected[usize::from(column >= 8)];
                assert!(
                    pixel.iter().zip(expected).all(|(a, b)| (a - b).abs() <= 1.0e-5),
                    "column {column}: {pixel:?}, independent source {expected:?}"
                );
            }
        }
        assert_auxiliary_float32_parity(&cpu, &gpu);
    }

    fn assert_auxiliary_float32_parity(cpu: &[f32], gpu: &[f32]) {
        let max_error = gpu
            .iter()
            .zip(cpu)
            .map(|(a, b)| {
                assert!(a.is_finite() && b.is_finite());
                (a - b).abs()
            })
            .fold(0.0, f32::max);
        eprintln!("Export auxiliary Float32 maximum error: {max_error:e}");
        assert!(max_error <= 1.0e-4);
    }

    #[test]
    fn export_auxiliary_asset_decodes_a_frozen_picture() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("matte.ppm");
        std::fs::write(&path, b"P6\n1 1\n255\n\0\xff\0").unwrap();
        let asset_id = AssetId::new();
        let mut timeline =
            snapshot(VisualSourceReference::Asset { asset_id, retime: Default::default() });
        timeline.media.insert(
            asset_id,
            super::super::tests::test_media_dependency(path, None, Default::default(), None),
        );
        let mut output = None;
        render(&timeline, &mut output).unwrap();
        assert!(output
            .unwrap()
            .rgba_f32()
            .data
            .iter()
            .all(|pixel| *pixel == [1.0, 0.0, 0.0, 1.0]));
    }

    #[test]
    fn export_range_captures_assets_and_sequences_used_only_as_auxiliary_sources() {
        let asset = AssetId::new();
        let mut child =
            snapshot(VisualSourceReference::Asset { asset_id: asset, retime: Default::default() })
                .sequence;
        child.name = "Auxiliary Sequence only".into();
        let timeline = snapshot(VisualSourceReference::Sequence {
            sequence_id: child.id,
            retime: Default::default(),
        });
        let dependencies = crate::capture::prepare_timeline_export_dependencies(
            &timeline.sequence,
            std::slice::from_ref(&child),
            TimelineExportRange::EntireSequence,
            false,
        )
        .unwrap();
        assert!(dependencies.sequence_ids().contains(&child.id));
        assert_eq!(dependencies.media_components().len(), 1);
        assert!(dependencies.media_components()[&asset].is_empty());
    }

    #[test]
    fn an_auxiliary_input_outside_the_export_range_fails_the_dependency_closure() {
        let source = solid(Color::from_rgba8(0, 255, 0, 255), TimelineTime::ONE);
        let clip_id = source.id;
        let mut timeline =
            snapshot(VisualSourceReference::Clip { clip_id, retime: Default::default() });
        timeline.sequence.video_tracks[0].add_clip(source).unwrap();
        let error = crate::capture::prepare_timeline_export_dependencies(
            &timeline.sequence,
            &[],
            TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 1 },
            false,
        )
        .expect_err("selected range excludes the retimed source Clip");
        assert!(
            matches!(
                error,
                crate::capture::TimelineExportDependencyError::VisualClosure(
                    mondrian_renderer::PreparedVisualRangeClosureError::AuxiliaryClipOutsideRange {
                        clip_id: actual, first, last, source_first, source_last, ..
                    }
                ) if actual == clip_id && first == TimelineTime::ZERO && last == TimelineTime::ZERO
                    && source_first == TimelineTime::ONE && source_last == TimelineTime::ONE
            ),
            "{error}"
        );
        crate::capture::prepare_timeline_export_dependencies(
            &timeline.sequence,
            &[],
            TimelineExportRange::EntireSequence,
            false,
        )
        .expect("full range includes the exact Clip sample");
    }

    #[test]
    fn export_video_collage_preflight_checks_the_derived_tile_offset() {
        let source = solid(Color::WHITE, TimelineTime::ZERO);
        let clip_id = source.id;
        let kind = EffectType::VideoCollage;
        let mut effect = instantiate_effect_node(kind.clone()).unwrap();
        for (key, value) in [
            ("columns", PropertyValue::Int(2)),
            ("rows", PropertyValue::Int(1)),
            ("tile_delay", PropertyValue::Float(0.25)),
            (
                "tile_source_02",
                PropertyValue::VisualSource(VisualSourceReference::Clip {
                    clip_id,
                    retime: Default::default(),
                }),
            ),
        ] {
            effect
                .set_static_value_by_parameter(&kind.parameter_id(key).unwrap(), value)
                .unwrap();
        }
        let mut timeline = snapshot_with_effect(effect);
        timeline.sequence.settings.frame_rate = mondrian_core::Rational::new(8, 1);
        timeline.sequence.video_tracks[0].add_clip(source).unwrap();
        let error = crate::capture::prepare_timeline_export_dependencies(
            &timeline.sequence,
            &[],
            TimelineExportRange::WorkArea { start_frame: 4, end_frame_exclusive: 5 },
            false,
        )
        .expect_err("the second tile samples two frames before the one-frame export range");
        assert!(
            matches!(error,
                crate::capture::TimelineExportDependencyError::VisualClosure(
                    mondrian_renderer::PreparedVisualRangeClosureError::AuxiliaryClipOutsideRange {
                        clip_id: actual, first, last, source_first, source_last, ..
                    }
                ) if actual == clip_id && first == TimelineTime::new(1, 2).unwrap() && last == first
                    && source_first == TimelineTime::new(1, 4).unwrap() && source_last == source_first
            ),
            "{error}"
        );
        crate::capture::prepare_timeline_export_dependencies(
            &timeline.sequence,
            &[],
            TimelineExportRange::EntireSequence,
            false,
        )
        .unwrap();
    }

    #[test]
    fn echo_export_uses_each_temporal_auxiliary_context_and_extends_clip_preflight() {
        let half = TimelineTime::new(1, 2).unwrap();
        let mut child = Sequence::new("animated matte");
        child.settings.resolution = Resolution { width: 16, height: 16 };
        child.settings.frame_rate = Rational::new(8, 1);
        for (start, color) in [
            (TimelineTime::ZERO, Color::WHITE),
            (half, Color::TRANSPARENT),
        ] {
            child.video_tracks[0]
                .add_clip(Clip::new_solid_color(AssetId::new(), color, start, half).unwrap())
                .unwrap();
        }
        let mut matte = instantiate_effect_node(EffectType::SetMatte).unwrap();
        for (field, value) in [
            (
                "matte_source",
                PropertyValue::VisualSource(VisualSourceReference::Sequence {
                    sequence_id: child.id,
                    retime: Default::default(),
                }),
            ),
            ("stretch", PropertyValue::Bool(true)),
        ] {
            matte
                .set_static_value_by_parameter(
                    &EffectType::SetMatte.parameter_id(field).unwrap(),
                    value,
                )
                .unwrap();
        }
        let mut echo = instantiate_effect_node(EffectType::Echo).unwrap();
        for (field, value) in [
            ("echo_time", PropertyValue::Float(-0.25)),
            ("operator", PropertyValue::Enum("normal".to_owned())),
            ("starting_intensity", PropertyValue::Float(0.5)),
        ] {
            echo.set_static_value_by_parameter(
                &EffectType::Echo.parameter_id(field).unwrap(),
                value,
            )
            .unwrap();
        }
        let mut timeline = snapshot_with_effect(matte);
        timeline.sequence.settings.frame_rate = child.settings.frame_rate;
        timeline.sequences.push(child);
        let clip = &mut timeline.sequence.video_tracks[1].clips[0];
        clip.set_solid_color(Color::WHITE).unwrap();
        clip.add_effect_node(echo);
        let mut session = ExportVisualRenderSession::default();
        let mixed = render_float_auxiliary_delivery(&timeline, 4, &mut session, false);
        assert!(
            mixed.chunks_exact(4).all(|pixel| (pixel[3] - 0.5).abs() < 1e-6),
            "mixed alpha {}",
            mixed[3]
        );
        let earlier = render_float_auxiliary_delivery(&timeline, 2, &mut session, false);
        assert!(earlier.chunks_exact(4).all(|pixel| (pixel[3] - 1.0).abs() < 1e-6));
        assert_eq!(
            render_float_auxiliary_delivery(&timeline, 4, &mut session, false),
            mixed
        );

        let earlier_clip = solid(Color::WHITE, TimelineTime::ZERO);
        let earlier_clip_id = earlier_clip.id;
        timeline.sequence.video_tracks[0].add_clip(earlier_clip).unwrap();
        timeline.sequence.video_tracks[1].clips[0].effects[0]
            .set_static_value_by_parameter(
                &EffectType::SetMatte.parameter_id("matte_source").unwrap(),
                PropertyValue::VisualSource(VisualSourceReference::Clip {
                    clip_id: earlier_clip_id,
                    retime: Default::default(),
                }),
            )
            .unwrap();
        let result = crate::capture::prepare_timeline_export_dependencies(
            &timeline.sequence,
            &timeline.sequences,
            TimelineExportRange::WorkArea { start_frame: 4, end_frame_exclusive: 5 },
            false,
        );
        assert!(
            matches!(result, Err(crate::capture::TimelineExportDependencyError::VisualClosure(
            mondrian_renderer::PreparedVisualRangeClosureError::AuxiliaryClipOutsideRange {
                clip_id, source_first, ..
            }
        )) if clip_id == earlier_clip_id && source_first == TimelineTime::new(1, 4).unwrap())
        );
    }

    #[test]
    fn export_historical_video_collage_range_includes_the_prefixs_earlier_clip_dependency() {
        let source = solid(Color::WHITE, TimelineTime::ZERO);
        let clip_id = source.id;
        let mut timeline =
            snapshot(VisualSourceReference::Clip { clip_id, retime: Default::default() });
        timeline.sequence.settings.frame_rate = Rational::new(8, 1);
        timeline.sequence.video_tracks[0].add_clip(source).unwrap();
        let kind = EffectType::VideoCollage;
        let mut collage = instantiate_effect_node(kind.clone()).unwrap();
        for (key, value) in [
            ("columns", PropertyValue::Int(2)),
            ("rows", PropertyValue::Int(1)),
            ("tile_delay", PropertyValue::Float(0.25)),
        ] {
            collage
                .set_static_value_by_parameter(&kind.parameter_id(key).unwrap(), value)
                .unwrap();
        }
        timeline.sequence.video_tracks[1].clips[0].add_effect_node(collage);
        let preflight = |start_frame| {
            crate::capture::prepare_timeline_export_dependencies(
                &timeline.sequence,
                &[],
                TimelineExportRange::WorkArea { start_frame, end_frame_exclusive: 5 },
                false,
            )
        };
        let error = preflight(4).expect_err("prefix needs the earlier auxiliary Clip");
        assert!(
            matches!(
                error,
                crate::capture::TimelineExportDependencyError::VisualClosure(
                    mondrian_renderer::PreparedVisualRangeClosureError::AuxiliaryClipOutsideRange {
                        clip_id: actual, source_first, source_last, ..
                    }
                ) if actual == clip_id
                    && source_first == TimelineTime::new(1, 4).unwrap()
                    && source_last == TimelineTime::new(1, 2).unwrap()
            ),
            "{error}"
        );
        crate::capture::prepare_timeline_export_dependencies(
            &timeline.sequence,
            &[],
            TimelineExportRange::EntireSequence,
            false,
        )
        .expect("full range includes the prefix's clamped history");
    }

    #[test]
    fn export_fails_the_frame_when_a_required_auxiliary_is_missing() {
        let mut output = None;
        for (reference, expected) in [
            (
                VisualSourceReference::Sequence {
                    sequence_id: mondrian_core::SequenceId::new(),
                    retime: Default::default(),
                },
                "auxiliary visual source",
            ),
            (
                VisualSourceReference::Asset {
                    asset_id: AssetId::new(),
                    retime: Default::default(),
                },
                "missing required auxiliary Asset",
            ),
        ] {
            let error = render(&snapshot(reference), &mut output).unwrap_err();
            assert!(error.contains(expected), "{error}");
            assert!(output.is_none());
        }
    }
}
