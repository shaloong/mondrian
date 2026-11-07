//! Invocation-local registration; no decoder or independent frame cache.

use std::collections::{HashMap, HashSet};

use super::{
    PreviewAuxiliaryAssetPlan, ResolvedPreviewAuxiliaryInputs, ResolvedPreviewAuxiliarySource,
};
use crate::app::preview_access_mode::MediaPreviewKey;
use crate::app::preview_cpu_execution::PreviewCpuExecutionError;
use crate::app::preview_execution::PreviewSemanticIdentity;
use crate::app::preview_media_frame::MediaPreviewFrame;
use crate::app::preview_viewer_plan::{ResolvedPreviewElement, ResolvedPreviewTransitionInput};
use mondrian_core::{ColorFrameAlpha, ImageSampling, WorkingColorSpace, WorkingRgbaF32Frame};
use mondrian_effects::{
    AuxiliaryFloatFrame, AuxiliaryFloatInput, AuxiliaryPolicy, AuxiliaryResolution, AuxiliaryRole,
    EffectColorDomain, EffectFloatFrameError, EffectFloatFrameView, EffectFrameExtent,
    EffectProcessorControl, EffectValueFormat, EffectWorkingPrecision,
};
use mondrian_renderer::{CpuColorFrame, TimelineCompositeScratch, TimelineCpuCompositePrecision};

pub(crate) struct PreparedAuxiliaryRasters<'a> {
    pub(crate) binding: &'a ResolvedPreviewAuxiliaryInputs,
    frames: Vec<(u32, CpuColorFrame, AuxiliaryPolicy)>,
}

impl PreparedAuxiliaryRasters<'_> {
    pub(crate) fn inputs(&self) -> Vec<AuxiliaryFloatInput<'_>> {
        self.frames
            .iter()
            .map(|(slot, frame, policy)| AuxiliaryFloatInput {
                slot: *slot,
                raster: AuxiliaryFloatFrame::new(view(frame), *policy),
            })
            .collect()
    }
}

fn overflow() -> PreviewCpuExecutionError {
    mondrian_renderer::TimelineCompositeError::CpuWorkingSet(
        mondrian_renderer::TimelineCpuWorkingSetError::ArithmeticOverflow,
    )
    .into()
}

fn bytes(extent: EffectFrameExtent) -> Result<u64, PreviewCpuExecutionError> {
    u64::from(extent.width())
        .checked_mul(u64::from(extent.height()))
        .and_then(|size| size.checked_mul(16))
        .ok_or_else(overflow)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct AuxiliaryRegistrationKey<'a> {
    source: &'a MediaPreviewKey,
    ready: Option<PreviewSemanticIdentity>,
    extent: EffectFrameExtent,
    sampling: ImageSampling,
    working_color: WorkingColorSpace,
}

pub(crate) struct AuxiliaryRegistration<'a> {
    pub(crate) key: AuxiliaryRegistrationKey<'a>,
    source_extent: Option<EffectFrameExtent>,
    policy: AuxiliaryPolicy,
}

impl AuxiliaryRegistration<'_> {
    pub(crate) fn working_bytes(&self) -> Result<usize, PreviewCpuExecutionError> {
        auxiliary_source_working_bytes(self.source_extent, self.key.extent)
    }
}

pub(crate) fn plan_auxiliary_registration<'a>(
    plan: &'a PreviewAuxiliaryAssetPlan,
    frame: Option<&MediaPreviewFrame>,
    role: AuxiliaryRole,
    primary: (EffectFrameExtent, ImageSampling),
    working_color: WorkingColorSpace,
) -> Result<AuxiliaryRegistration<'a>, PreviewCpuExecutionError> {
    let source_domain = frame
        .map(|frame| {
            frame.cpu_image_domain().ok_or_else(|| {
                PreviewCpuExecutionError::AuxiliaryRegistration {
                    detail: "auxiliary source has no actual CPU image domain".into(),
                }
            })
        })
        .transpose()?;
    let ((extent, sampling), policy) = if role == AuxiliaryRole::Map {
        let domain = if let Some(domain) = source_domain {
            domain
        } else {
            let resolution = plan.validated_key.residency_resolution();
            let sampling = plan.validated_key.picture_geometry.image_sampling(resolution).map_err(
                |error| PreviewCpuExecutionError::AuxiliaryRegistration {
                    detail: error.to_string(),
                },
            )?;
            (
                EffectFrameExtent::new(resolution.width, resolution.height),
                sampling,
            )
        };
        (domain, AuxiliaryPolicy::new(AuxiliaryResolution::Authored))
    } else {
        (primary, AuxiliaryPolicy::MATCH_PRIMARY)
    };
    Ok(AuxiliaryRegistration {
        key: AuxiliaryRegistrationKey {
            source: &plan.validated_key,
            ready: frame.map(MediaPreviewFrame::identity),
            extent,
            sampling,
            working_color,
        },
        source_extent: source_domain.map(|domain| domain.0),
        policy,
    })
}

impl ResolvedPreviewAuxiliaryInputs {
    pub(super) fn registration<'a>(
        &self,
        source: &'a ResolvedPreviewAuxiliarySource,
    ) -> Result<AuxiliaryRegistration<'a>, PreviewCpuExecutionError> {
        let spec = self
            .graph
            .auxiliary_inputs()
            .iter()
            .find(|spec| spec.slot() == source.slot)
            .ok_or_else(|| PreviewCpuExecutionError::AuxiliaryRegistration {
                detail: "resolved auxiliary source has no compiled slot".into(),
            })?;
        plan_auxiliary_registration(
            &source.plan,
            source.frame.as_ref(),
            spec.role(),
            (self.primary_extent, self.primary_sampling),
            source.request.input_color.working_color_space,
        )
    }
}

/// Cold canonical conversion workspace plus the retained registered plane.
/// Source Store payloads keep their existing leases; no source lease is forged.
fn auxiliary_source_working_bytes(
    source_extent: Option<EffectFrameExtent>,
    registered_extent: EffectFrameExtent,
) -> Result<usize, PreviewCpuExecutionError> {
    let source = source_extent
        .map(bytes)
        .transpose()?
        .unwrap_or(0)
        .checked_mul(2)
        .ok_or_else(overflow)?;
    usize::try_from(source.checked_add(bytes(registered_extent)?).ok_or_else(overflow)?)
        .map_err(|_| overflow())
}

fn frame_error(error: EffectFloatFrameError) -> PreviewCpuExecutionError {
    mondrian_renderer::TimelineCompositeError::FloatEffect {
        reason: mondrian_effects::EffectFloatExecutionError::FrameContract(error),
    }
    .into()
}

fn checkpoint(control: &mut EffectProcessorControl<'_>) -> Result<(), PreviewCpuExecutionError> {
    control.checkpoint().map_err(|reason| {
        mondrian_renderer::TimelineCompositeError::FloatEffect {
            reason: mondrian_effects::EffectFloatExecutionError::Stopped { reason },
        }
        .into()
    })
}

fn view(frame: &CpuColorFrame) -> EffectFloatFrameView<'_> {
    let frame = frame.rgba_f32();
    EffectFloatFrameView::new(
        EffectFrameExtent::new(frame.width, frame.height),
        EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            frame.alpha,
        ),
        frame.sampling,
        &frame.data,
    )
}

/// Charge cold source conversion, registered planes, primary preparation and
/// graph peaks before allocating any registration/working pixels. Store leases
/// remain separately owned. Equal registration targets share their pixel charge.
pub(crate) fn preflight_auxiliary_composite(
    width: u32,
    height: u32,
    elements: &[ResolvedPreviewElement],
    bindings: &[&ResolvedPreviewAuxiliaryInputs],
    scratch: &TimelineCompositeScratch,
) -> Result<(), PreviewCpuExecutionError> {
    if bindings.is_empty() {
        return Ok(());
    }
    let canvas = EffectFrameExtent::new(width, height);
    let mut owned = bytes(canvas)?.checked_mul(3).ok_or_else(overflow)?;
    let mut graph_peak = 0u64;
    let mut registered = HashSet::new();
    for binding in bindings {
        let mut extents = Vec::new();
        for source in &binding.sources {
            let registration = binding.registration(source)?;
            extents.push((source.slot, registration.key.extent));
            if registered.insert(registration.key) {
                let working = registration.working_bytes()?;
                owned = owned
                    .checked_add(u64::try_from(working).map_err(|_| overflow())?)
                    .ok_or_else(overflow)?;
            }
        }
        let peak = mondrian_effects::estimate_compiled_float_raster_working_bytes(
            &binding.graph,
            binding.primary_extent,
            &extents,
        )
        .map_err(
            |error| mondrian_renderer::TimelineCompositeError::FloatEffect { reason: error },
        )?;
        graph_peak = graph_peak.max(u64::try_from(peak).map_err(|_| overflow())?);
    }
    let mut graph = |graph: &mondrian_effects::CompiledEffectGraph,
                     extent: EffectFrameExtent|
     -> Result<(), PreviewCpuExecutionError> {
        if graph.auxiliary_inputs().is_empty() {
            let peak =
                mondrian_effects::estimate_compiled_float_raster_working_bytes(graph, extent, &[])
                    .map_err(
                        |error| mondrian_renderer::TimelineCompositeError::FloatEffect {
                            reason: error,
                        },
                    )?;
            graph_peak = graph_peak.max(u64::try_from(peak).map_err(|_| overflow())?);
        }
        Ok(())
    };
    let mut media = |frame: &crate::app::preview_media_frame::MediaPreviewFrame,
                     effect: &mondrian_effects::CompiledEffectGraph|
     -> Result<(), PreviewCpuExecutionError> {
        let extent = frame
            .cpu_image_domain()
            .map(|domain| domain.0)
            .unwrap_or_else(|| EffectFrameExtent::new(frame.width(), frame.height()));
        owned = owned
            .checked_add(bytes(extent)?.checked_mul(2).ok_or_else(overflow)?)
            .ok_or_else(overflow)?;
        graph(effect, extent)
    };
    // Separate passes keep the mutable peak observer shared without duplicating
    // any pixel preparation or invoking the media Adapter.
    for element in elements {
        match element.without_auxiliary() {
            ResolvedPreviewElement::Media { frame, effect_graph, .. } => {
                media(frame, effect_graph)?
            }
            ResolvedPreviewElement::CrossDissolve { left, right, .. } => {
                for input in [left, right] {
                    if let ResolvedPreviewTransitionInput::Media { frame, effect_graph, .. } =
                        input.as_ref()
                    {
                        media(frame, effect_graph)?;
                    }
                }
            }
            ResolvedPreviewElement::SolidColor(_)
            | ResolvedPreviewElement::HeterogeneousSolidColor { .. }
            | ResolvedPreviewElement::Adjustment(_) => {}
            ResolvedPreviewElement::WithAuxiliary { .. } => unreachable!("context was peeled"),
        }
    }
    for element in elements {
        match element.without_auxiliary() {
            ResolvedPreviewElement::SolidColor(layer)
            | ResolvedPreviewElement::HeterogeneousSolidColor { layer, .. } => {
                graph(&layer.effect_graph, canvas)?
            }
            ResolvedPreviewElement::Adjustment(layer) => graph(&layer.effect_graph, canvas)?,
            ResolvedPreviewElement::CrossDissolve { left, right, .. } => {
                for input in [left, right] {
                    if let ResolvedPreviewTransitionInput::SolidColor(layer)
                    | ResolvedPreviewTransitionInput::HeterogeneousSolidColor {
                        layer, ..
                    } = input.as_ref()
                    {
                        graph(&layer.effect_graph, canvas)?;
                    }
                }
            }
            ResolvedPreviewElement::Media { .. } => {}
            ResolvedPreviewElement::WithAuxiliary { .. } => unreachable!("context was peeled"),
        }
    }
    scratch
        .admit_cpu_active_working_set(
            owned.checked_add(graph_peak).ok_or_else(overflow)?,
            TimelineCpuCompositePrecision::Float32,
        )
        .map_err(|error| mondrian_renderer::TimelineCompositeError::CpuWorkingSet(error).into())
}

pub(crate) fn prepare_auxiliary_rasters<'a>(
    bindings: &[&'a ResolvedPreviewAuxiliaryInputs],
    scratch: &mut TimelineCompositeScratch,
    control: &mut EffectProcessorControl<'_>,
    color_diagnostics: &mut Vec<mondrian_renderer::RenderColorTransformDiagnostics>,
    stage_diagnostics: &mut mondrian_renderer::color::RenderColorStageDiagnostics,
) -> Result<Vec<PreparedAuxiliaryRasters<'a>>, PreviewCpuExecutionError> {
    let mut prepared = Vec::new();
    let mut registered = HashMap::<AuxiliaryRegistrationKey<'_>, CpuColorFrame>::new();
    for binding in bindings {
        if let Some(context) = &binding.projection_context {
            context.validate()?;
        }
        if binding.sources.is_empty() {
            continue;
        }
        let mut frames = Vec::new();
        for source in &binding.sources {
            checkpoint(control)?;
            let registration = binding.registration(source)?;
            if let Some(frame) = registered.get(&registration.key) {
                frames.push((source.slot, frame.clone(), registration.policy));
                continue;
            }
            let AuxiliaryRegistrationKey { extent, sampling, working_color, .. } = registration.key;
            let frame = if let Some(source_frame) = &source.frame {
                let working =
                    source_frame.working_frame_with_session(scratch.color_execution_mut())?;
                if let Some(diagnostics) = working.color_diagnostics {
                    color_diagnostics.push(diagnostics);
                }
                stage_diagnostics.accumulate(working.stage_diagnostics);
                let working = working.frame;
                if working.rgba_f32().color_space != working_color {
                    return Err(PreviewCpuExecutionError::AuxiliaryRegistration {
                        detail: "auxiliary working payload contradicts its frozen color context"
                            .into(),
                    });
                }
                let actual = view(&working);
                if source_frame.cpu_image_domain() != Some((actual.extent(), actual.sampling())) {
                    return Err(PreviewCpuExecutionError::AuxiliaryRegistration {
                        detail: "auxiliary working payload contradicts its admitted image domain"
                            .into(),
                    });
                }
                register(&working, extent, sampling, control)?
            } else {
                let count = usize::try_from(bytes(extent)? / 16).map_err(|_| overflow())?;
                let mut data = Vec::new();
                data.try_reserve_exact(count).map_err(|_| {
                    frame_error(EffectFloatFrameError::AllocationUnavailable {
                        required_bytes: count.saturating_mul(16),
                    })
                })?;
                for index in 0..count {
                    if index % 65_536 == 0 {
                        checkpoint(control)?;
                    }
                    data.push([0.0; 4]);
                }
                CpuColorFrame::working(WorkingRgbaF32Frame {
                    width: extent.width(),
                    height: extent.height(),
                    sampling,
                    alpha: ColorFrameAlpha::StraightCoverage,
                    color_space: working_color,
                    data,
                })
            };
            registered.insert(registration.key, frame.clone());
            frames.push((source.slot, frame, registration.policy));
        }
        prepared.push(PreparedAuxiliaryRasters { binding, frames });
    }
    Ok(prepared)
}

fn register(
    source: &CpuColorFrame,
    extent: EffectFrameExtent,
    sampling: mondrian_core::ImageSampling,
    control: &mut EffectProcessorControl<'_>,
) -> Result<CpuColorFrame, PreviewCpuExecutionError> {
    let view = view(source);
    view.validate_controlled(&mut || checkpoint(control))?;
    if view.extent() == extent
        && view.sampling() == sampling
        && !view.format().alpha().is_premultiplied()
    {
        return Ok(source.clone());
    }
    let alpha = view.format().alpha();
    let output_alpha = if alpha.is_premultiplied() {
        ColorFrameAlpha::StraightFloat
    } else {
        alpha
    };
    let count = usize::try_from(bytes(extent)? / 16).map_err(|_| overflow())?;
    let mut data = Vec::new();
    data.try_reserve_exact(count).map_err(|_| {
        frame_error(EffectFloatFrameError::AllocationUnavailable {
            required_bytes: count.saturating_mul(16),
        })
    })?;
    let source_width = view.extent().width();
    let source_height = view.extent().height();
    if source_width == 0 || source_height == 0 || extent.is_empty() {
        return Err(PreviewCpuExecutionError::AuxiliaryRegistration {
            detail: "auxiliary registration has an empty grid".into(),
        });
    }
    for y in 0..extent.height() {
        checkpoint(control)?;
        let py = ((y as f32 + mondrian_effects::PIXEL_CENTRE) * source_height as f32
            / extent.height() as f32
            - mondrian_effects::PIXEL_CENTRE)
            .clamp(0.0, (source_height - 1) as f32);
        for x in 0..extent.width() {
            if x % 65_536 == 0 {
                checkpoint(control)?;
            }
            let px = ((x as f32 + mondrian_effects::PIXEL_CENTRE) * source_width as f32
                / extent.width() as f32
                - mondrian_effects::PIXEL_CENTRE)
                .clamp(0.0, (source_width - 1) as f32);
            let ix = px.floor() as u32;
            let iy = py.floor() as u32;
            let weights = [
                (1.0 - px.fract()) * (1.0 - py.fract()),
                px.fract() * (1.0 - py.fract()),
                (1.0 - px.fract()) * py.fract(),
                px.fract() * py.fract(),
            ];
            let mut associated = [0.0; 4];
            for (tap, weight) in mondrian_effects::BILINEAR_TAPS.iter().zip(weights) {
                let sx = (ix + tap[0] as u32).min(source_width - 1);
                let sy = (iy + tap[1] as u32).min(source_height - 1);
                let pixel = mondrian_effects::convert_alpha_sample(
                    view.pixels()[sy as usize * source_width as usize + sx as usize],
                    alpha,
                    ColorFrameAlpha::PremultipliedFloat,
                )
                .map_err(frame_error)?;
                for lane in 0..4 {
                    associated[lane] += pixel[lane] * weight;
                }
            }
            if output_alpha == ColorFrameAlpha::Opaque {
                associated[3] = 1.0;
            }
            data.push(
                mondrian_effects::convert_alpha_sample(
                    associated,
                    ColorFrameAlpha::PremultipliedFloat,
                    output_alpha,
                )
                .map_err(frame_error)?,
            );
        }
    }
    Ok(CpuColorFrame::working(WorkingRgbaF32Frame {
        width: extent.width(),
        height: extent.height(),
        sampling,
        alpha: output_alpha,
        color_space: source.rgba_f32().color_space,
        data,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{ImageSampling, WorkingColorSpace};

    #[test]
    fn auxiliary_authored_maps_preserve_the_source_grid_even_at_transparent_boundaries() {
        use crate::app::preview_auxiliary::{
            resolve_preview_auxiliary_asset,
            tests::{asset_fixture_with_picture, request},
            ResolvedPreviewAuxiliarySource,
        };
        use crate::app::preview_hardware_admission::PreviewHardwareDecodeAdmissionState;
        use crate::app::preview_media_frame::MediaPreviewFrame;
        use mondrian_core::{
            PictureStreamMetadata, SampleAspectRatio, TimelineTime, VisualSourceBoundary,
        };
        use mondrian_effects::{
            compile_reference_render_graph, DisplacementMapBehavior, DisplacementMapChannel,
            DisplacementMapGrade, EffectColorDomainContract, EffectGraphBuilderState,
            EffectRenderOp,
        };
        let (_root, asset) = asset_fixture_with_picture(
            Some(std::time::Duration::from_secs(2)),
            PictureStreamMetadata {
                sample_aspect_ratio: SampleAspectRatio::new(4, 3),
                ..Default::default()
            },
        );
        let mut builder = EffectGraphBuilderState::new();
        let input = builder.current_output();
        let auxiliary = builder.append_external_input(
            0,
            EffectValueFormat::new(
                EffectWorkingPrecision::Float32,
                EffectColorDomain::SceneLinearRgb,
                ColorFrameAlpha::StraightCoverage,
            ),
            EffectColorDomainContract::SCENE_LINEAR,
        );
        let output = builder.add_auxiliary_effect_with_role(
            input,
            auxiliary,
            EffectRenderOp::DisplacementMap {
                grade: DisplacementMapGrade::new(
                    [0.0; 2],
                    [DisplacementMapChannel::Red, DisplacementMapChannel::Green],
                    DisplacementMapBehavior::Stretch,
                    false,
                    false,
                )
                .unwrap(),
            },
            AuxiliaryRole::Map,
        );
        builder.set_current_output(output);
        let graph = compile_reference_render_graph(builder.finish()).unwrap();
        for transparent in [false, true] {
            let request = request(
                &asset,
                if transparent {
                    TimelineTime::new(50, 1).unwrap()
                } else {
                    TimelineTime::ZERO
                },
                if transparent {
                    VisualSourceBoundary::Transparent
                } else {
                    VisualSourceBoundary::Clamp
                },
            );
            let plan = resolve_preview_auxiliary_asset(
                &request,
                &asset,
                PreviewHardwareDecodeAdmissionState::default(),
            )
            .unwrap();
            let sampling = plan
                .validated_key
                .picture_geometry
                .image_sampling(plan.validated_key.residency_resolution())
                .unwrap();
            let working = CpuColorFrame::working(WorkingRgbaF32Frame {
                width: 4,
                height: 2,
                sampling,
                alpha: ColorFrameAlpha::StraightCoverage,
                color_space: request.input_color.working_color_space,
                data: vec![[2.0, -0.5, 0.25, 0.5]; 8],
            });
            let frame = (!transparent).then(|| {
                MediaPreviewFrame::from_working(
                    working.clone(),
                    mondrian_core::Resolution { width: 4, height: 2 },
                    crate::app::preview_execution::PreviewSemanticIdentity::from_test_fingerprint(
                        [63; 32],
                    ),
                    mondrian_playback::FramePresentationQuality::Ready,
                    crate::app::preview_execution::PreviewDecodeExecutionSummary::default(),
                )
                .with_source_key(plan.validated_key.clone())
            });
            let binding = ResolvedPreviewAuxiliaryInputs {
                graph: std::sync::Arc::clone(&graph),
                primary_extent: EffectFrameExtent::new(3, 1),
                primary_sampling: ImageSampling::UNIT_SQUARE,
                sources: vec![ResolvedPreviewAuxiliarySource { slot: 0, request, plan, frame }]
                    .into_boxed_slice(),
                projection_context: None,
            };
            let mut scratch = TimelineCompositeScratch::default();
            let prepared = prepare_auxiliary_rasters(
                &[&binding],
                &mut scratch,
                &mut EffectProcessorControl::uncancelled(),
                &mut Vec::new(),
                &mut Default::default(),
            )
            .unwrap();
            let inputs = prepared[0].inputs();
            assert_eq!(
                inputs[0].raster.policy.resolution(),
                AuxiliaryResolution::Authored
            );
            assert_eq!(
                inputs[0].raster.frame.extent(),
                EffectFrameExtent::new(4, 2)
            );
            assert_eq!(inputs[0].raster.frame.sampling(), sampling);
            assert_ne!(sampling, binding.primary_sampling);
            if transparent {
                assert!(inputs[0].raster.frame.pixels().iter().all(|pixel| *pixel == [0.0; 4]));
            } else {
                assert_eq!(
                    inputs[0].raster.frame.pixels().as_ptr(),
                    working.rgba_f32().data.as_ptr(),
                    "authored maps borrow the original HDR working plane without resizing"
                );
            }
        }
    }

    #[test]
    fn auxiliary_registration_filters_associated_hdr_without_hidden_color_bleed() {
        let source = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 2,
            height: 1,
            sampling: ImageSampling::UNIT_SQUARE,
            alpha: ColorFrameAlpha::StraightCoverage,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![[4.0, 2.0, -1.0, 1.0], [100.0, 0.0, 0.0, 0.0]],
        });
        let output = register(
            &source,
            EffectFrameExtent::new(3, 1),
            ImageSampling::UNIT_SQUARE,
            &mut EffectProcessorControl::uncancelled(),
        )
        .unwrap();
        assert_eq!(
            output.rgba_f32().data,
            [[4.0, 2.0, -1.0, 1.0], [4.0, 2.0, -1.0, 0.5], [0.0; 4]]
        );
    }

    #[test]
    fn auxiliary_registration_preserves_float_alpha_and_refuses_zero_alpha_emission() {
        for sign in [-1.0, 1.0] {
            let source = CpuColorFrame::working(WorkingRgbaF32Frame {
                width: 2,
                height: 1,
                sampling: ImageSampling::UNIT_SQUARE,
                alpha: ColorFrameAlpha::StraightFloat,
                color_space: WorkingColorSpace::LinearRec709,
                data: vec![[4.0, 0.0, 0.0, sign * 2.0], [8.0, 0.0, 0.0, sign * 4.0]],
            });
            let output = register(
                &source,
                EffectFrameExtent::new(3, 1),
                ImageSampling::UNIT_SQUARE,
                &mut EffectProcessorControl::uncancelled(),
            )
            .unwrap();
            assert_eq!(output.rgba_f32().alpha, ColorFrameAlpha::StraightFloat);
            let middle = output.rgba_f32().data[1];
            assert!((middle[0] - 20.0 / 3.0).abs() < 1e-6);
            assert_eq!(middle[3], sign * 3.0);
        }
        let source = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 2,
            height: 1,
            sampling: ImageSampling::UNIT_SQUARE,
            alpha: ColorFrameAlpha::StraightFloat,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![[4.0, 0.0, 0.0, 1.0], [8.0, 0.0, 0.0, -1.0]],
        });
        assert!(matches!(
            register(
                &source,
                EffectFrameExtent::new(3, 1),
                ImageSampling::UNIT_SQUARE,
                &mut EffectProcessorControl::uncancelled()
            ),
            Err(PreviewCpuExecutionError::TimelineComposite(
                mondrian_renderer::TimelineCompositeError::FloatEffect {
                    reason: mondrian_effects::EffectFloatExecutionError::FrameContract(
                        EffectFloatFrameError::AlphaConversion {
                            reason:
                                mondrian_effects::EffectAlphaConversionFailure::ZeroAlphaEmission,
                            ..
                        }
                    ),
                }
            ))
        ));
    }
}
