use super::*;
use crate::*;
use mondrian_core::{
    effect_data::EffectType, AssetId, ColorFrameAlpha, ImageSampling, ParameterId, PropertyBag,
    PropertyDescriptor, PropertyValue, VisualSourceBoundary, VisualSourceReference,
    VisualSourceRetime,
};

#[derive(Clone, Copy)]
enum Tamper {
    None,
    Owner,
    Reference,
    Opaque,
}

fn source(asset_id: AssetId) -> VisualSourceReference {
    VisualSourceReference::Asset { asset_id, retime: VisualSourceRetime::default() }
}

fn format() -> EffectValueFormat {
    EffectValueFormat::new(
        EffectWorkingPrecision::Float32,
        EffectColorDomain::SceneLinearRgb,
        ColorFrameAlpha::StraightCoverage,
    )
}

fn fixture(
    reference: VisualSourceReference,
    color: [f32; 3],
    temporal: EffectTemporalInputExtent,
    tamper: Tamper,
) -> EffectNode {
    let kind = EffectType::External(format!("fixture.auxiliary.{}", EffectId::new()));
    let parameter_id = kind.parameter_id("source").unwrap();
    let mut properties = PropertyBag::default();
    properties.define(
        PropertyDescriptor::new(
            "source",
            "Picture source",
            PropertyValue::VisualSource(reference),
        )
        .with_parameter_id(parameter_id.clone()),
    );
    register_effect_definition(
        EffectDefinition::new(
            kind.key(),
            "Bound raster",
            properties,
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            roi_propagation: if matches!(tamper, Tamper::Opaque) {
                EffectRoiPropagation::UnknownRequiresFullFrame
            } else {
                EffectRoiPropagation::PixelLocal
            },
            temporal_input: temporal,
            ..EffectExecutionContract::IDENTITY
        })
        .with_branching_graph_builder(Arc::new(move |effect, _, graph| {
            let mut author = effect.clone();
            match tamper {
                Tamper::None | Tamper::Opaque => {}
                Tamper::Owner => author.id = EffectId::new(),
                Tamper::Reference => author
                    .set_static_value_by_parameter(
                        &parameter_id,
                        PropertyValue::VisualSource(source(AssetId::new())),
                    )
                    .unwrap(),
            }
            let input = graph.current_output();
            let auxiliary = graph.append_visual_source_input(
                &author,
                &parameter_id,
                format(),
                EffectColorDomainContract::SCENE_LINEAR,
            )?;
            let output = graph.add_auxiliary_effect_with_role(
                input,
                auxiliary,
                EffectRenderOp::Fill {
                    grade: FillGrade::new(0.0, 0.0, false, color, 1.0).unwrap(),
                },
                AuxiliaryRole::Matte,
            );
            graph.set_current_output(output);
            if matches!(tamper, Tamper::Opaque) {
                graph.append_unary(EffectRenderOp::Custom {
                    key: "fixture.opaque-history".into(),
                    params: serde_json::json!({}),
                    cache_key: None,
                    cache_policy: EffectCachePolicy::Deterministic,
                    processor: Some(CustomEffectProcessorBinding::new_f32(Arc::new(
                        |_, _, _| Ok(()),
                    ))),
                });
            }
            Ok(())
        })),
    )
    .unwrap();
    instantiate_effect_node(kind).unwrap()
}

fn fill(reference: VisualSourceReference, color: [f32; 3]) -> EffectNode {
    fixture(
        reference,
        color,
        EffectTemporalInputExtent::CURRENT_FRAME,
        Tamper::None,
    )
}

fn program(effects: &[EffectNode]) -> PreparedEffectProgram {
    PreparedEffectProgram::prepare(effects, &[], WorkingColorSpace::LinearRec709).unwrap()
}

fn serial_grade(effect: EffectNode) -> GradeGraph {
    let input = GradeGraphNodeId::new();
    let output = GradeGraphNodeId::new();
    GradeGraph {
        nodes: [
            mondrian_core::GradeGraphNode { id: input, kind: GradeGraphNodeKind::Input },
            mondrian_core::GradeGraphNode {
                id: output,
                kind: GradeGraphNodeKind::Effect { input, effect },
            },
        ]
        .into(),
        output,
    }
}

fn view(pixels: &[[f32; 4]]) -> EffectFloatFrameView<'_> {
    EffectFloatFrameView::new(
        EffectFrameExtent::new(1, 1),
        format(),
        ImageSampling::UNIT_SQUARE,
        pixels,
    )
}

fn render(graph: &CompiledEffectGraph, a: AssetId, b: AssetId) -> [f32; 4] {
    let first = [[0.0, 0.0, 0.0, 0.25]];
    let second = [[0.0, 0.0, 0.0, 0.5]];
    let bindings = graph
        .auxiliary_source_bindings()
        .map(|binding| {
            let VisualSourceReference::Asset { asset_id, .. } = binding.reference() else {
                panic!("Asset")
            };
            let pixels = if asset_id == a {
                &first
            } else {
                assert_eq!(asset_id, b);
                &second
            };
            AuxiliaryFloatInput {
                slot: binding.slot(),
                raster: AuxiliaryFloatFrame::new(view(pixels), AuxiliaryPolicy::MATCH_PRIMARY),
            }
        })
        .collect::<Vec<_>>();
    let mut session = EffectExecutionSession::default();
    session
        .apply_compiled_rgba_f32_with_auxiliary(
            view(&[[0.0, 0.0, 2.0, 1.0]]),
            graph,
            0,
            &bindings,
            &mut EffectProcessorControl::uncancelled(),
        )
        .unwrap()
        .pixels()[0]
}

fn assert_pixel(actual: [f32; 4], expected: [f32; 4]) {
    for lane in 0..4 {
        assert!(
            (actual[lane] - expected[lane]).abs() < 1e-6,
            "{actual:?} != {expected:?}"
        );
    }
}

#[test]
fn auxiliary_source_binding_is_frozen_outside_the_pixel_graph_identity() {
    let a = AssetId::new();
    let b = AssetId::new();
    let mut effect = fill(source(a), [1.0, 0.0, 0.0]);
    let frozen = program(std::slice::from_ref(&effect));
    let graph = frozen.evaluate(TimelineTime::ZERO).unwrap();
    let parameter = effect.effect_type.parameter_id("source").unwrap();
    effect
        .set_static_value_by_parameter(&parameter, PropertyValue::VisualSource(source(b)))
        .unwrap();
    let changed = program(std::slice::from_ref(&effect)).evaluate(TimelineTime::ZERO).unwrap();
    assert_eq!(
        graph.semantic_fingerprint(),
        changed.semantic_fingerprint(),
        "source selection belongs to host cache identity, not pixel operations"
    );
    let binding = graph.auxiliary_source_binding(0).unwrap();
    assert_eq!(binding.effect_id(), effect.id);
    assert_eq!(binding.parameter_id(), &parameter);
    assert_eq!(binding.reference(), source(a));
    assert_eq!(
        changed.auxiliary_source_binding(0).unwrap().reference(),
        source(b)
    );
    assert_eq!(
        frozen
            .evaluate(TimelineTime::ONE)
            .unwrap()
            .auxiliary_source_binding(0)
            .unwrap()
            .reference(),
        source(a)
    );
    assert!(graph.auxiliary_source_binding(99).is_none());
}

#[test]
fn multiple_effect_sources_get_distinct_slots_and_feed_their_own_cpu_inputs() {
    let a = AssetId::new();
    let b = AssetId::new();
    let effects = [
        fill(source(a), [1.0, 0.0, 0.0]),
        fill(source(b), [0.0, 1.0, 0.0]),
    ];
    let graph = program(&effects).evaluate(TimelineTime::ZERO).unwrap();
    assert_eq!(
        graph.auxiliary_inputs().iter().map(|s| s.slot()).collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(
        graph.auxiliary_source_bindings().map(|s| s.effect_id()).collect::<Vec<_>>(),
        effects.iter().map(|e| e.id).collect::<Vec<_>>()
    );
    assert_pixel(render(&graph, a, b), [0.125, 0.5, 0.75, 1.0]);
}

#[test]
fn repeated_shared_grade_sources_are_remapped_with_their_producer_nodes() {
    let a = AssetId::new();
    let b = AssetId::new();
    let red = fill(source(a), [1.0, 0.0, 0.0]);
    let red_id = red.id;
    let green = fill(source(b), [0.0, 1.0, 0.0]);
    let green_id = green.id;
    let grade =
        PreparedGradeGraph::prepare(&serial_grade(red), WorkingColorSpace::LinearRec709).unwrap();
    let prepared = PreparedEffectProgram::prepare_hierarchical_with_lut_cache(
        &[green],
        &[],
        &[grade.clone(), grade.clone()],
        &[grade],
        WorkingColorSpace::LinearRec709,
        &LutPreparationCache::uncached(),
    )
    .unwrap();
    let graph = prepared.evaluate(TimelineTime::ZERO).unwrap();
    let bindings = graph.auxiliary_source_bindings().collect::<Vec<_>>();
    assert_eq!(
        bindings.iter().map(|s| s.slot()).collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    assert_eq!(
        bindings.iter().map(|s| s.effect_id()).collect::<Vec<_>>(),
        [red_id, red_id, green_id, red_id]
    );
    let mut producers = HashSet::new();
    for binding in bindings {
        assert!(producers.insert(binding.producer()));
        assert!(
            matches!(graph.graph().node(binding.producer()).unwrap().kind,
            EffectGraphNodeKind::ExternalInput { slot, .. } if slot == binding.slot())
        );
    }
    assert_pixel(render(&graph, a, b), [0.4140625, 0.375, 0.421875, 1.0]);
}

#[test]
fn a_required_unbound_visual_source_has_a_structured_author_error() {
    let effect = fill(VisualSourceReference::Unbound, [1.0, 0.0, 0.0]);
    let error = PreparedEffectProgram::prepare(
        std::slice::from_ref(&effect),
        &[],
        WorkingColorSpace::LinearRec709,
    )
    .unwrap_err();
    assert!(
        matches!(error, EffectGraphBuildError::MissingAuxiliaryInput {
        effect_id, parameter_id, slot: 0, ..
    } if effect_id == effect.id && parameter_id == effect.effect_type.parameter_id("source").unwrap())
    );
}

#[test]
fn source_bindings_cannot_claim_another_effect_or_a_mutated_author_copy() {
    for tamper in [Tamper::Owner, Tamper::Reference] {
        let effect = fixture(
            source(AssetId::new()),
            [1.0, 0.0, 0.0],
            EffectTemporalInputExtent::CURRENT_FRAME,
            tamper,
        );
        assert!(
            matches!(PreparedEffectProgram::prepare(std::slice::from_ref(&effect), &[], WorkingColorSpace::LinearRec709),
            Err(EffectGraphBuildError::InvalidAuthorState { effect_id, reason, .. })
                if effect_id == effect.id && reason.contains("frozen Effect parameter"))
        );
    }
}

#[test]
fn visual_source_retime_enters_the_spec_and_the_existing_temporal_contract_gate() {
    let offset = TimelineTime::new(-1001, 24000).unwrap();
    let reference = VisualSourceReference::Asset {
        asset_id: AssetId::new(),
        retime: VisualSourceRetime {
            time_offset: offset,
            mode: VisualSourceBoundary::Transparent,
        },
    };
    let effect = fixture(
        reference,
        [1.0, 0.0, 0.0],
        EffectTemporalInputExtent {
            past: EffectTemporalSpan::Finite(TimelineTime::ZERO.checked_sub(offset).unwrap()),
            future: EffectTemporalSpan::None,
        },
        Tamper::None,
    );
    let graph = program(&[effect]).evaluate(TimelineTime::ZERO).unwrap();
    assert_eq!(graph.auxiliary_inputs()[0].time_offset(), offset);
    assert_eq!(
        graph.auxiliary_source_binding(0).unwrap().reference(),
        reference
    );
    let optimistic = fill(reference, [1.0, 0.0, 0.0]);
    assert!(
        matches!(PreparedEffectProgram::prepare(&[optimistic], &[], WorkingColorSpace::LinearRec709),
        Err(EffectGraphBuildError::ExecutionContractViolation { violation, .. })
        if matches!(*violation, EffectExecutionContractViolation::TemporalExtentTooOptimistic { .. }))
    );
}

#[test]
fn compiled_source_evidence_rejects_wrong_slots_producers_and_time_offsets() {
    let graph = program(&[fill(source(AssetId::new()), [1.0, 0.0, 0.0])])
        .evaluate(TimelineTime::ZERO)
        .unwrap();
    let stage = graph.stage_bindings()[0].clone();
    let original = stage.auxiliary_sources()[0].clone();
    for sources in [
        vec![original.remapped(original.producer(), 99)],
        vec![original.remapped(graph.graph().output.unwrap(), 0)],
        vec![original.clone(), original.clone()],
    ] {
        let stage = stage.clone().with_auxiliary_sources(sources.into());
        let topology = prepare_effect_graph_topology(graph.graph()).unwrap();
        assert!(topology
            .bind_with_execution_bindings(
                graph.graph().clone(),
                graph.execution_envelope().clone(),
                Arc::from([stage])
            )
            .is_none());
    }
    let mut changed = graph.graph().clone();
    for node in &mut changed.nodes {
        if let EffectGraphNodeKind::ExternalInput { time_offset, .. } = &mut node.kind {
            *time_offset = TimelineTime::ONE;
        }
    }
    let topology = prepare_effect_graph_topology(&changed).unwrap();
    assert!(topology
        .bind_with_execution_bindings(
            changed,
            graph.execution_envelope().clone(),
            Arc::from([stage])
        )
        .is_none());
}

#[test]
fn a_failed_visual_source_lookup_does_not_consume_a_slot_or_graph_node() {
    let effect = fill(source(AssetId::new()), [1.0, 0.0, 0.0]);
    let parameter = effect.effect_type.parameter_id("source").unwrap();
    let mut builder = EffectGraphBuilderState::new();
    assert!(builder
        .append_visual_source_input(
            &effect,
            &ParameterId::new_static("test.missing"),
            format(),
            EffectColorDomainContract::SCENE_LINEAR
        )
        .is_err());
    assert_eq!(builder.current_output(), builder.source());
    let producer = builder
        .append_visual_source_input(
            &effect,
            &parameter,
            format(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .unwrap();
    assert_eq!(producer, EffectGraphNodeId(1));
    let graph = builder.finish();
    assert_eq!(graph.nodes.len(), 2);
    assert!(matches!(
        graph.node(producer).unwrap().kind,
        EffectGraphNodeKind::ExternalInput { slot: 0, .. }
    ));
}

#[test]
fn a_noncolliding_explicit_external_slot_keeps_its_existing_binding_number() {
    let kind = EffectType::External(format!("fixture.raw-auxiliary.{}", EffectId::new()));
    register_effect_definition(
        EffectDefinition::new(
            kind.key(),
            "Explicit slot",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            roi_propagation: EffectRoiPropagation::PixelLocal,
            ..EffectExecutionContract::IDENTITY
        })
        .with_branching_graph_builder(Arc::new(|_, _, graph| {
            let input = graph.current_output();
            let auxiliary =
                graph.append_external_input(42, format(), EffectColorDomainContract::SCENE_LINEAR);
            let output = graph.add_auxiliary_effect_with_role(
                input,
                auxiliary,
                EffectRenderOp::Fill {
                    grade: FillGrade::new(0.0, 0.0, false, [1.0, 0.0, 0.0], 1.0).unwrap(),
                },
                AuxiliaryRole::Matte,
            );
            graph.set_current_output(output);
            Ok(())
        })),
    )
    .unwrap();
    let graph = program(&[instantiate_effect_node(kind).unwrap()])
        .evaluate(TimelineTime::ZERO)
        .unwrap();
    assert_eq!(graph.auxiliary_inputs()[0].slot(), 42);
    assert!(
        graph.auxiliary_source_binding(42).is_none(),
        "raw slots do not invent author references"
    );
    let matte = [[0.0, 0.0, 0.0, 0.25]];
    let binding = AuxiliaryFloatInput {
        slot: 42,
        raster: AuxiliaryFloatFrame::new(view(&matte), AuxiliaryPolicy::MATCH_PRIMARY),
    };
    let output = EffectExecutionSession::default()
        .apply_compiled_rgba_f32_with_auxiliary(
            view(&[[0.0, 0.0, 2.0, 1.0]]),
            &graph,
            0,
            &[binding],
            &mut EffectProcessorControl::uncancelled(),
        )
        .unwrap();
    assert_pixel(output.pixels()[0], [0.25, 0.0, 1.5, 1.0]);
}

#[test]
fn authored_visual_sources_require_the_canonical_host_frame_contract() {
    let effect = fill(source(AssetId::new()), [1.0, 0.0, 0.0]);
    let parameter = effect.effect_type.parameter_id("source").unwrap();
    let display =
        EffectColorDomain::DisplayEncodedRgb { color_space: mondrian_core::ColorSpace::Rec709 };
    let mut builder = EffectGraphBuilderState::new();
    for (format, domain) in [
        (
            EffectValueFormat::new(
                EffectWorkingPrecision::Float16,
                EffectColorDomain::SceneLinearRgb,
                ColorFrameAlpha::StraightCoverage,
            ),
            EffectColorDomainContract::SCENE_LINEAR,
        ),
        (
            format().with_alpha(ColorFrameAlpha::PremultipliedCoverage),
            EffectColorDomainContract::SCENE_LINEAR,
        ),
        (
            format().with_domain(display),
            EffectColorDomainContract::preserving(display),
        ),
        (format(), EffectColorDomainContract::preserving(display)),
    ] {
        assert!(matches!(
            builder.append_visual_source_input(&effect, &parameter, format, domain),
            Err(EffectGraphBuildError::InvalidAuthorState { .. })
        ));
    }
    assert_eq!(
        builder
            .append_visual_source_input(
                &effect,
                &parameter,
                format(),
                EffectColorDomainContract::SCENE_LINEAR
            )
            .unwrap(),
        EffectGraphNodeId(1)
    );
    for alpha in [ColorFrameAlpha::Opaque, ColorFrameAlpha::StraightFloat] {
        let mut builder = EffectGraphBuilderState::new();
        let producer = builder
            .append_visual_source_input(
                &effect,
                &parameter,
                format().with_alpha(alpha),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .unwrap();
        let graph = builder.finish();
        assert!(matches!(graph.node(producer).unwrap().kind,
            EffectGraphNodeKind::ExternalInput { format, .. } if format.alpha() == alpha));
    }
}

#[test]
fn resolved_native_auxiliary_time_does_not_become_primary_echo_history() {
    let a = AssetId::new();
    let reference = VisualSourceReference::Asset {
        asset_id: a,
        retime: VisualSourceRetime {
            time_offset: TimelineTime::NEGATIVE_ONE,
            mode: VisualSourceBoundary::Clamp,
        },
    };
    let graph = program(&[fixture(
        reference,
        [1.0, 0.0, 0.0],
        EffectTemporalInputExtent::UNBOUNDED,
        Tamper::None,
    )])
    .evaluate(TimelineTime::ZERO)
    .unwrap();
    assert!(matches!(
        graph.execution_envelope().admit_single_frame_backend(
            EffectProcessingBackend::Cpu,
            EffectWorkingPrecision::Float32
        ),
        Err(EffectExecutionAdmissionError::TemporalInputRequired { .. })
    ));
    assert!(!graph.requires_primary_temporal_execution());
    graph
        .admit_single_frame_backend_with_resolved_auxiliary(
            EffectProcessingBackend::Cpu,
            EffectWorkingPrecision::Float32,
        )
        .unwrap();
    let matte = [[0.0, 0.0, 0.0, 0.25]];
    let binding = AuxiliaryFloatInput {
        slot: 0,
        raster: AuxiliaryFloatFrame::new(view(&matte), AuxiliaryPolicy::MATCH_PRIMARY),
    };
    let result = EffectExecutionSession::default()
        .apply_compiled_pass_rgba_f32_owned_with_auxiliary_and_domain_processor(
            view(&[[0.0, 0.0, 2.0, 1.0]]).to_owned().unwrap(),
            &graph,
            0.5,
            mondrian_core::EffectPassBlendPolicy::Mix,
            0,
            &[binding],
            EffectDomainProcessorCacheKey::from_complete_semantic_fingerprint([93; 32]),
            |_, _| panic!("scene-linear graph has no domain edges"),
            &mut EffectProcessorControl::uncancelled(),
        )
        .unwrap();
    assert_pixel(result.pixels()[0], [0.125, 0.0, 1.75, 1.0]);
    // Resolving time cannot grant a GPU backend that was never declared.
    assert!(matches!(
        graph.admit_single_frame_backend_with_resolved_auxiliary(
            EffectProcessingBackend::Gpu,
            EffectWorkingPrecision::Float32
        ),
        Err(EffectExecutionAdmissionError::ExecutionModeNotAdmitted { .. })
    ));
    let original = &graph.stage_bindings()[0];
    let contract = EffectExecutionContract {
        resource_lifetime: EffectResourceLifetime::ContinuitySession,
        ..original.contract()
    };
    let stage = CompiledEffectStageBinding::new(
        0,
        contract,
        original.input_value(),
        original.output_value(),
        original.emitted_nodes().to_vec(),
    )
    .with_auxiliary_sources(original.auxiliary_sources().to_vec().into());
    let guarded = prepare_effect_graph_topology(graph.graph())
        .unwrap()
        .bind_with_execution_bindings(
            graph.graph().clone(),
            EffectExecutionEnvelope::new(contract, Arc::from([contract])),
            Arc::from([stage]),
        )
        .unwrap();
    assert!(matches!(
        guarded.admit_single_frame_backend_with_resolved_auxiliary(
            EffectProcessingBackend::Cpu,
            EffectWorkingPrecision::Float32
        ),
        Err(EffectExecutionAdmissionError::ContinuitySessionRequired)
    ));
}

#[test]
fn auxiliary_time_discharge_keeps_opaque_and_nonauxiliary_temporal_declarations() {
    let opaque = program(&[fixture(
        source(AssetId::new()),
        [1.0, 0.0, 0.0],
        EffectTemporalInputExtent::UNBOUNDED,
        Tamper::Opaque,
    )])
    .evaluate(TimelineTime::ZERO)
    .unwrap();
    assert!(opaque.requires_primary_temporal_execution());
    assert!(matches!(
        opaque.admit_single_frame_backend_with_resolved_auxiliary(
            EffectProcessingBackend::Cpu,
            EffectWorkingPrecision::Float32
        ),
        Err(EffectExecutionAdmissionError::TemporalInputRequired { .. })
    ));
    let ordinary = compile_reference_effect_graph(&EffectRenderPlan {
        ops: vec![EffectRenderOp::ColorAdjust {
            exposure: 1.0,
            contrast: 1.0,
            saturation: 1.0,
            working_color_space: WorkingColorSpace::LinearRec709,
        }],
    })
    .unwrap();
    let original = &ordinary.stage_bindings()[0];
    let contract = EffectExecutionContract {
        temporal_input: EffectTemporalInputExtent::UNBOUNDED,
        ..original.contract()
    };
    let stage = CompiledEffectStageBinding::new(
        0,
        contract,
        original.input_value(),
        original.output_value(),
        original.emitted_nodes().to_vec(),
    );
    let ordinary = prepare_effect_graph_topology(ordinary.graph())
        .unwrap()
        .bind_with_execution_bindings(
            ordinary.graph().clone(),
            EffectExecutionEnvelope::new(contract, Arc::from([contract])),
            Arc::from([stage]),
        )
        .unwrap();
    assert!(ordinary.requires_primary_temporal_execution());
    assert!(matches!(
        ordinary.admit_single_frame_backend_with_resolved_auxiliary(
            EffectProcessingBackend::Cpu,
            EffectWorkingPrecision::Float32
        ),
        Err(EffectExecutionAdmissionError::TemporalInputRequired { .. })
    ));
}
