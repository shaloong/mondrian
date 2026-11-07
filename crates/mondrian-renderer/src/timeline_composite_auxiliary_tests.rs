use super::*;
use mondrian_core::{
    effect_data::EffectType, AssetId, Color, ImageSampling, PropertyBag, PropertyDescriptor,
    PropertyValue, TimelineTime, VisualSourceReference, VisualSourceRetime,
};
use mondrian_effects::{
    AuxiliaryFloatFrame, AuxiliaryFloatInput, AuxiliaryPolicy, AuxiliaryRole,
    EffectColorDomainContract, EffectDefinition, EffectExecutionContract, EffectExecutionModes,
    EffectFloatFrameView, EffectFrameExtent, EffectGraphBuildError, EffectGraphNodeKind,
    EffectRenderOp, EffectRoiPropagation, EffectValueFormat, EffectWorkingPrecision, FillGrade,
    PreparedEffectProgram,
};

static ENGINE: ColorEngine = ColorEngine::mondrian_standard();

fn format() -> EffectValueFormat {
    EffectValueFormat::new(
        EffectWorkingPrecision::Float32,
        EffectColorDomain::SceneLinearRgb,
        ColorFrameAlpha::StraightCoverage,
    )
}

fn graph(asset_id: AssetId, color_domain: bool) -> Arc<CompiledEffectGraph> {
    graph_with_retime(asset_id, color_domain, VisualSourceRetime::default())
}

fn graph_with_retime(
    asset_id: AssetId,
    color_domain: bool,
    retime: VisualSourceRetime,
) -> Arc<CompiledEffectGraph> {
    let effect = effect_with_retime(asset_id, color_domain, retime);
    PreparedEffectProgram::prepare(&[effect], &[], WorkingColorSpace::LinearRec709)
        .unwrap()
        .evaluate(TimelineTime::ZERO)
        .unwrap()
}

fn effect_with_retime(
    asset_id: AssetId,
    color_domain: bool,
    retime: VisualSourceRetime,
) -> mondrian_effects::EffectNode {
    let kind = EffectType::External(format!(
        "fixture.renderer-auxiliary.{}",
        mondrian_core::EffectId::new()
    ));
    let parameter = kind.parameter_id("source").unwrap();
    let mut properties = PropertyBag::default();
    properties.define(
        PropertyDescriptor::new(
            "source",
            "Picture source",
            PropertyValue::VisualSource(VisualSourceReference::Asset { asset_id, retime }),
        )
        .with_parameter_id(parameter.clone()),
    );
    mondrian_effects::register_effect_definition(
        EffectDefinition::new(
            kind.key(),
            "CPU host auxiliary",
            properties,
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            roi_propagation: EffectRoiPropagation::PixelLocal,
            temporal_input: mondrian_effects::EffectTemporalInputExtent::UNBOUNDED,
            ..EffectExecutionContract::IDENTITY
        })
        .with_branching_graph_builder(Arc::new(move |effect, _, builder| {
            if color_domain {
                builder.append_unary_in_domain(
                    EffectRenderOp::ColorAdjust {
                        exposure: 0.0,
                        contrast: 1.0,
                        saturation: 1.0,
                        working_color_space: WorkingColorSpace::LinearRec709,
                    },
                    EffectColorDomainContract::preserving(EffectColorDomain::DisplayEncodedRgb {
                        color_space: mondrian_core::ColorSpace::Rec709,
                    }),
                );
            }
            let input = builder.current_output();
            let auxiliary = builder.append_visual_source_input(
                effect,
                &parameter,
                format(),
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
            Ok::<_, EffectGraphBuildError>(())
        })),
    )
    .unwrap();
    mondrian_effects::instantiate_effect_node(kind).unwrap()
}

fn solid(graph: Arc<CompiledEffectGraph>) -> TimelineSolidColorLayer {
    TimelineSolidColorLayer {
        color: Color { r: 0.0, g: 0.0, b: 2.0, a: 1.0 },
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph: graph,
        frame_seed: 0,
        processor_input_plan: None,
    }
}

fn binding(pixels: &[[f32; 4]]) -> AuxiliaryFloatInput<'_> {
    AuxiliaryFloatInput {
        slot: 0,
        raster: AuxiliaryFloatFrame::new(
            EffectFloatFrameView::new(
                EffectFrameExtent::new(1, 1),
                format(),
                ImageSampling::UNIT_SQUARE,
                pixels,
            ),
            AuxiliaryPolicy::MATCH_PRIMARY,
        ),
    }
}

fn render(
    elements: &[TimelineCompositeElement<'_>],
    inputs: &[TimelineAuxiliaryInputs<'_>],
) -> Result<CpuColorFrame, TimelineCompositeError> {
    composite_timeline_elements_color_frame(
        1,
        1,
        elements,
        TimelineCompositeOptions::default(),
        TimelineEffectColorRuntime::new(
            &ENGINE,
            WorkingColorSpace::LinearRec709,
            ImageSampling::UNIT_SQUARE,
        )
        .with_auxiliary_inputs(inputs),
        &mut TimelineCompositeScratch::default(),
        &mut mondrian_effects::EffectProcessorControl::uncancelled(),
    )
}

fn assert_pixel(frame: CpuColorFrame, expected: [f32; 4]) {
    let actual = frame.rgba_f32().data[0];
    for lane in 0..4 {
        assert!(
            (actual[lane] - expected[lane]).abs() < 1e-4,
            "{actual:?} != {expected:?}"
        );
    }
}

#[test]
fn cpu_auxiliary_solid_and_media_layers_use_the_bound_raster() {
    let graph = graph(AssetId::new(), false);
    let pixels = [[0.0, 0.0, 0.0, 0.25]];
    let rasters = [binding(&pixels)];
    let inputs = [TimelineAuxiliaryInputs::new(
        &graph,
        EffectFrameExtent::new(1, 1),
        &rasters,
    )];
    assert_pixel(
        render(
            &[TimelineCompositeElement::SolidColor(solid(graph.clone()))],
            &inputs,
        )
        .unwrap(),
        [0.25, 0.0, 1.5, 1.0],
    );
    let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
        sampling: ImageSampling::UNIT_SQUARE,
        alpha: ColorFrameAlpha::StraightCoverage,
        width: 1,
        height: 1,
        data: vec![[0.0, 0.0, 2.0, 1.0]],
        color_space: WorkingColorSpace::LinearRec709,
    });
    let layer = TimelineMediaLayer {
        frame: &frame,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph: graph.clone(),
        frame_seed: 0,
        processor_input_plan: None,
    };
    assert_pixel(
        render(&[TimelineCompositeElement::Media(layer)], &inputs).unwrap(),
        [0.25, 0.0, 1.5, 1.0],
    );
}

#[test]
fn cpu_auxiliary_lookup_distinguishes_equal_pixel_graphs_with_different_assets() {
    let first = graph(AssetId::new(), false);
    let second = graph(AssetId::new(), false);
    assert_eq!(first.semantic_fingerprint(), second.semantic_fingerprint());
    assert_ne!(
        first.auxiliary_source_binding(0).unwrap().reference(),
        second.auxiliary_source_binding(0).unwrap().reference()
    );
    let pixels_a = [[0.0, 0.0, 0.0, 0.25]];
    let pixels_b = [[0.0, 0.0, 0.0, 0.5]];
    let a = [binding(&pixels_a)];
    let b = [binding(&pixels_b)];
    let inputs = [
        TimelineAuxiliaryInputs::new(&first, EffectFrameExtent::new(1, 1), &a),
        TimelineAuxiliaryInputs::new(&second, EffectFrameExtent::new(1, 1), &b),
    ];
    assert_pixel(
        render(
            &[
                TimelineCompositeElement::SolidColor(solid(first.clone())),
                TimelineCompositeElement::SolidColor(solid(second.clone())),
            ],
            &inputs,
        )
        .unwrap(),
        [0.5, 0.0, 1.0, 1.0],
    );
}

#[test]
fn cpu_auxiliary_adjustment_keeps_the_existing_effect_pass_mix() {
    let graph = graph(AssetId::new(), false);
    let pixels = [[0.0, 0.0, 0.0, 0.25]];
    let rasters = [binding(&pixels)];
    let inputs = [TimelineAuxiliaryInputs::new(
        &graph,
        EffectFrameExtent::new(1, 1),
        &rasters,
    )];
    let identity = mondrian_effects::identity_compiled_effect_graph().unwrap();
    let adjustment = TimelineAdjustmentLayer {
        effect_graph: graph.clone(),
        opacity: 0.5,
        blend_policy: mondrian_core::EffectPassBlendPolicy::Mix,
        frame_seed: 0,
    };
    assert_pixel(
        render(
            &[
                TimelineCompositeElement::SolidColor(solid(identity)),
                TimelineCompositeElement::Adjustment(adjustment),
            ],
            &inputs,
        )
        .unwrap(),
        [0.125, 0.0, 1.75, 1.0],
    );
}

#[test]
fn cpu_auxiliary_transition_endpoint_reaches_the_same_effect_runtime() {
    let graph = graph(AssetId::new(), false);
    let pixels = [[0.0, 0.0, 0.0, 0.25]];
    let rasters = [binding(&pixels)];
    let inputs = [TimelineAuxiliaryInputs::new(
        &graph,
        EffectFrameExtent::new(1, 1),
        &rasters,
    )];
    let transition = TimelineCrossDissolveLayer {
        left: TimelineTransitionInput::SolidColor(solid(graph.clone())),
        right: TimelineTransitionInput::Transparent,
        progress: 0.5,
    };
    assert_pixel(
        render(
            &[TimelineCompositeElement::CrossDissolve(transition)],
            &inputs,
        )
        .unwrap(),
        [0.25, 0.0, 1.5, 0.5],
    );
}

#[test]
fn cpu_auxiliary_and_stock_ocio_domain_edges_execute_together() {
    let graph = graph(AssetId::new(), true);
    assert_eq!(graph.domain_plan().transitions.len(), 2);
    let pixels = [[0.0, 0.0, 0.0, 0.25]];
    let rasters = [binding(&pixels)];
    let inputs = [TimelineAuxiliaryInputs::new(
        &graph,
        EffectFrameExtent::new(1, 1),
        &rasters,
    )];
    assert_pixel(
        render(
            &[TimelineCompositeElement::SolidColor(solid(graph.clone()))],
            &inputs,
        )
        .unwrap(),
        [0.25, 0.0, 1.5, 1.0],
    );
}

#[test]
fn cpu_auxiliary_missing_ambiguous_and_wrong_extent_sets_fail_closed() {
    let graph = graph(AssetId::new(), false);
    assert!(graph
        .graph()
        .nodes
        .iter()
        .any(|n| matches!(n.kind, EffectGraphNodeKind::ExternalInput { .. })));
    let elements = [TimelineCompositeElement::SolidColor(solid(graph.clone()))];
    let pixels = [[0.0, 0.0, 0.0, 0.25]];
    let rasters = [binding(&pixels)];
    let set = TimelineAuxiliaryInputs::new(&graph, EffectFrameExtent::new(1, 1), &rasters);
    for sets in [
        Vec::new(),
        vec![TimelineAuxiliaryInputs::new(
            &graph,
            EffectFrameExtent::new(2, 1),
            &rasters,
        )],
    ] {
        assert!(matches!(
            render(&elements, &sets),
            Err(TimelineCompositeError::FloatEffect {
                reason: EffectFloatExecutionError::AuxiliaryInput(
                    EffectExecutionError::AuxiliaryFrameMissing { slot: 0 }
                )
            })
        ));
    }
    assert!(matches!(
        render(&elements, &[set, set]),
        Err(TimelineCompositeError::FloatEffect {
            reason: EffectFloatExecutionError::AuxiliaryInput(
                EffectExecutionError::InvalidAuxiliaryFrame {
                    reason: "ambiguous_auxiliary_bindings",
                    ..
                }
            )
        })
    ));
    let mut undeclared = rasters;
    undeclared[0].slot = 42;
    assert!(matches!(
        render(
            &elements,
            &[TimelineAuxiliaryInputs::new(
                &graph,
                EffectFrameExtent::new(1, 1),
                &undeclared
            )]
        ),
        Err(TimelineCompositeError::FloatEffect {
            reason: EffectFloatExecutionError::AuxiliaryInput(
                EffectExecutionError::InvalidAuxiliaryFrame { slot: 42, reason: "undeclared_slot" }
            )
        })
    ));
}

#[test]
fn cpu_auxiliary_resolved_retime_does_not_request_primary_history() {
    let offset = TimelineTime::new(-1001, 24000).unwrap();
    let graph = graph_with_retime(
        AssetId::new(),
        false,
        VisualSourceRetime {
            time_offset: offset,
            mode: mondrian_core::VisualSourceBoundary::Clamp,
        },
    );
    assert_eq!(graph.auxiliary_inputs()[0].time_offset(), offset);
    assert!(!graph.requires_primary_temporal_execution());
    let pixels = [[0.0, 0.0, 0.0, 0.25]];
    let rasters = [binding(&pixels)];
    let inputs = [TimelineAuxiliaryInputs::new(
        &graph,
        EffectFrameExtent::new(1, 1),
        &rasters,
    )];
    assert_pixel(
        render(
            &[TimelineCompositeElement::SolidColor(solid(graph.clone()))],
            &inputs,
        )
        .unwrap(),
        [0.25, 0.0, 1.5, 1.0],
    );
}

#[test]
fn cpu_auxiliary_working_set_excludes_the_borrowed_authored_plane() {
    let graph = graph(AssetId::new(), false);
    let pixels = [[0.0, 0.0, 0.0, 0.25]; 4];
    let rasters = [AuxiliaryFloatInput {
        slot: 0,
        raster: AuxiliaryFloatFrame::new(
            EffectFloatFrameView::new(
                EffectFrameExtent::new(4, 1),
                format(),
                ImageSampling::UNIT_SQUARE,
                &pixels,
            ),
            AuxiliaryPolicy::new(mondrian_effects::AuxiliaryResolution::Authored),
        ),
    }];
    let inputs = [TimelineAuxiliaryInputs::new(
        &graph,
        EffectFrameExtent::new(1, 1),
        &rasters,
    )];
    let elements = [TimelineCompositeElement::SolidColor(solid(graph.clone()))];
    let runtime = TimelineEffectColorRuntime::new(
        &ENGINE,
        WorkingColorSpace::LinearRec709,
        ImageSampling::UNIT_SQUARE,
    )
    .with_auxiliary_inputs(&inputs);
    let estimate = estimate_timeline_cpu_working_set_with_auxiliary(
        1,
        1,
        &elements,
        TimelineCpuCompositePrecision::Float32,
        runtime,
    )
    .unwrap();
    // The generated source/result (32 B) use retained scratch, and the
    // authored plane stays with its caller. The canvas requires 16 active bytes.
    assert_eq!(estimate.active_bytes, 16);
    assert_eq!(estimate.retained_scratch_bytes, 32);
    let mut scratch = TimelineCompositeScratch::default();
    scratch.reconfigure_cpu_working_set(TimelineCpuWorkingSetGrant {
        max_active_bytes: 15,
        max_retained_scratch_bytes: 32,
    });
    assert!(matches!(
        composite_timeline_elements_color_frame(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            runtime,
            &mut scratch,
            &mut mondrian_effects::EffectProcessorControl::uncancelled(),
        ),
        Err(TimelineCompositeError::CpuWorkingSet(
            TimelineCpuWorkingSetError::ActiveGrantExceeded {
                required_bytes: 16,
                granted_bytes: 15,
                ..
            }
        ))
    ));
    assert_eq!(
        scratch.cpu_working_set_diagnostics().retained_scratch_bytes,
        0
    );
    scratch.reconfigure_cpu_working_set(TimelineCpuWorkingSetGrant {
        max_active_bytes: 16,
        max_retained_scratch_bytes: 32,
    });
    assert_pixel(
        composite_timeline_elements_color_frame(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            runtime,
            &mut scratch,
            &mut mondrian_effects::EffectProcessorControl::uncancelled(),
        )
        .unwrap(),
        [0.25, 0.0, 1.5, 1.0],
    );
}

#[test]
fn cpu_auxiliary_working_set_admits_a_graph_local_matte_without_external_slots() {
    let mut builder = mondrian_effects::EffectGraphBuilderState::new();
    let primary = builder.current_output();
    builder.append_unary(EffectRenderOp::ColorAdjust {
        exposure: 0.0,
        contrast: 1.0,
        saturation: 1.0,
        working_color_space: WorkingColorSpace::LinearRec709,
    });
    let output = builder.add_auxiliary_effect_with_role(
        primary,
        builder.current_output(),
        EffectRenderOp::Fill {
            grade: FillGrade::new(0.0, 0.0, false, [1.0, 0.0, 0.0], 1.0).unwrap(),
        },
        AuxiliaryRole::Matte,
    );
    builder.set_current_output(output);
    let graph = mondrian_effects::compile_reference_render_graph(builder.finish()).unwrap();
    assert!(graph.auxiliary_inputs().is_empty());
    let elements = [TimelineCompositeElement::SolidColor(solid(graph))];
    let precision = TimelineCpuCompositePrecision::Float32;
    let runtime = TimelineEffectColorRuntime::new(
        &ENGINE,
        WorkingColorSpace::LinearRec709,
        ImageSampling::UNIT_SQUARE,
    );
    let estimate = estimate_timeline_cpu_working_set(1, 1, &elements, precision).unwrap();
    assert_eq!(estimate.active_bytes, 32);
    assert_eq!(estimate.retained_scratch_bytes, 32);
    assert_eq!(
        estimate_timeline_cpu_working_set_with_auxiliary(1, 1, &elements, precision, runtime)
            .unwrap(),
        estimate
    );
    let mut scratch = TimelineCompositeScratch::default();
    let compose = |scratch: &mut TimelineCompositeScratch| {
        composite_timeline_elements_color_frame(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            runtime,
            scratch,
            &mut mondrian_effects::EffectProcessorControl::uncancelled(),
        )
    };
    scratch.reconfigure_cpu_working_set(TimelineCpuWorkingSetGrant {
        max_active_bytes: 31,
        max_retained_scratch_bytes: 32,
    });
    assert!(matches!(
        compose(&mut scratch),
        Err(TimelineCompositeError::CpuWorkingSet(
            TimelineCpuWorkingSetError::ActiveGrantExceeded {
                required_bytes: 32,
                granted_bytes: 31,
                ..
            }
        ))
    ));
    assert_eq!(
        scratch.cpu_working_set_diagnostics().retained_scratch_bytes,
        0
    );
    scratch.reconfigure_cpu_working_set(TimelineCpuWorkingSetGrant {
        max_active_bytes: 32,
        max_retained_scratch_bytes: 32,
    });
    assert_pixel(compose(&mut scratch).unwrap(), [1.0, 0.0, 0.0, 1.0]);
}

#[test]
fn cpu_auxiliary_prepared_timeline_does_not_expand_the_primary_media_history() {
    let offset = TimelineTime::new(-1001, 24000).unwrap();
    let asset_id = AssetId::new();
    let effect = effect_with_retime(
        asset_id,
        false,
        VisualSourceRetime {
            time_offset: offset,
            mode: mondrian_core::VisualSourceBoundary::Clamp,
        },
    );
    let mut sequence = mondrian_timeline::Sequence::new("native auxiliary retime");
    sequence.video_tracks.clear();
    let mut track = mondrian_timeline::Track::new_video("V1");
    let mut clip = mondrian_timeline::Clip::new(
        AssetId::new(),
        TimelineTime::ZERO,
        TimelineTime::new(5, 1).unwrap(),
    )
    .unwrap();
    clip.add_effect_node(effect);
    track.add_clip(clip).unwrap();
    sequence.video_tracks.push(track);
    let program = crate::PreparedVisualProgram::prepare(&sequence).unwrap();
    let extent = EffectFrameExtent::new(4, 2);
    let mut scratch = TimelineCompositeScratch::default();
    let prepared = scratch
        .prepare_timeline_frame_execution(
            &program,
            crate::TimelineFrameExecutionRequest::new(
                crate::TimelineEvaluationRequest::export(mondrian_core::FramePosition::new(
                    24,
                    sequence.time_base(),
                )),
                73,
                mondrian_effects::EffectExecutionContinuity::Discontinuous,
                extent,
                extent.full_frame_roi(),
                mondrian_core::ExecutionCancellationToken::new(),
            ),
        )
        .unwrap();
    assert!(prepared.batches().is_empty());
    assert!(prepared.temporal_media_inputs().is_empty());
    let crate::TimelineRenderPlanElement::Media(media) = &prepared.execution_plan().elements[0]
    else {
        panic!("original media placement retained");
    };
    assert_eq!(
        media.effect_graph.auxiliary_inputs()[0].time_offset(),
        offset
    );
    assert!(matches!(
        media.effect_graph.auxiliary_source_binding(0).unwrap().reference(),
        VisualSourceReference::Asset { asset_id: actual, .. } if actual == asset_id
    ));
}
