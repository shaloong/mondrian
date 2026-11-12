use super::*;
use crate::{instantiate_effect_node, EffectRenderOp, EffectType, MaskEvaluation, MaskShape};
use mondrian_core::automation::{Keyframe, PropertyMutation, PropertyValue};
use mondrian_core::mask_data::MaskParticipation;
use mondrian_core::PropertyHost;

fn fill(index: f32, all: f32, invert: bool) -> EffectNode {
    let mut effect = instantiate_effect_node(EffectType::Fill).unwrap();
    for (parameter, value) in [
        ("fill_mask", PropertyValue::Float(index)),
        ("all_masks", PropertyValue::Float(all)),
        ("invert", PropertyValue::Bool(invert)),
    ] {
        effect
            .apply_property_mutation(PropertyMutation::SetStaticValue {
                path: EffectType::Fill.property_path(parameter),
                value,
            })
            .unwrap();
    }
    effect
}

fn rectangle(x: f32, width: f32, op: crate::MaskOp) -> MaskComponent {
    MaskComponent::new(
        "fill source".to_owned(),
        MaskEvaluation {
            shape: MaskShape::Rectangle { x, y: 0.0, width, height: 1.0, corner_radius: 0.0 },
            mask_op: op,
            ..MaskEvaluation::default()
        },
    )
}

fn execute(graph: &CompiledEffectGraph) -> Vec<[f32; 4]> {
    let input = [[0.0, 0.0, 2.0, 1.0]; 4];
    crate::apply_compiled_effect_graph_rgba_f32(
        crate::EffectFloatFrameView::new(
            crate::EffectFrameExtent::new(4, 1),
            crate::EffectValueFormat::new(
                crate::EffectWorkingPrecision::Float32,
                crate::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightCoverage,
            ),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            &input,
        ),
        graph,
        0,
    )
    .unwrap()
    .pixels()
    .to_vec()
}

fn stack_graph(
    effect: EffectNode,
    masks: &[MaskComponent],
) -> (PreparedEffectStack, Arc<CompiledEffectGraph>) {
    let stack =
        PreparedEffectStack::prepare_with_masks(&[effect], masks, WorkingColorSpace::LinearRec709)
            .unwrap();
    let graph =
        crate::compile_reference_render_graph(stack.evaluate_graph(TimelineTime::ZERO).unwrap())
            .unwrap();
    (stack, graph)
}

#[test]
fn effect_mask_reference_survives_rename_and_reorder_and_evaluates_at_graph_time() {
    use crate::{EffectColorDomainContract, EffectDefinition, EffectMaskReferenceError, FillGrade};
    use mondrian_core::automation::{PropertyBag, PropertyDescriptor};
    use mondrian_core::mask_data::{MaskShapeInterpolation, MaskShapeKeyframe};

    let mut selected = rectangle(0.0, 0.25, crate::MaskOp::Add);
    selected.participation = MaskParticipation::EffectOnly;
    let other = rectangle(0.25, 0.25, crate::MaskOp::Add);
    let end = TimelineTime::new(2, 1).unwrap();
    let midpoint = TimelineTime::new(1, 1).unwrap();
    selected.shape_animation_enabled = true;
    selected.shape_keyframes_mut()[0].interpolation = MaskShapeInterpolation::Linear;
    selected.shape_keyframes_mut().push(MaskShapeKeyframe::new(
        end,
        MaskShape::Rectangle {
            x: 0.5,
            y: 0.0,
            width: 0.25,
            height: 1.0,
            corner_radius: 0.0,
        },
        MaskShapeInterpolation::Hold,
    ));

    let kind = EffectType::External("fixture.effect_mask_reference".to_owned());
    let parameter = kind.parameter_id("geometry").unwrap();
    let mut properties = PropertyBag::default();
    properties.define(
        PropertyDescriptor::new("geometry", "Geometry", PropertyValue::MaskReference(None))
            .with_parameter_id(parameter.clone()),
    );
    let builder_parameter = parameter.clone();
    crate::register_effect_definition(
        EffectDefinition::new(
            kind.key(),
            "Mask reference fixture",
            properties,
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            topology: EffectGraphTopology::GeneralDag,
            ..EffectExecutionContract::IDENTITY
        })
        .with_branching_graph_builder(Arc::new(move |effect, _, graph| {
            let invalid = |reason: String| EffectGraphBuildError::InvalidAuthorState {
                effect_key: effect.effect_type.key(),
                effect_id: effect.id,
                reason,
            };
            let id = effect
                .evaluate_mask_reference_parameter(&builder_parameter)
                .flatten()
                .ok_or_else(|| invalid("mask is unbound".to_owned()))?;
            let mask = graph.resolve_effect_mask(id).map_err(|error| invalid(error.to_string()))?;
            let matte = graph.add_mask_source(
                mask.shape.clone(),
                mask.feather,
                mask.expansion,
                mask.opacity,
            );
            let grade = FillGrade::new(0.0, 0.0, false, [1.0, 0.0, 0.0], 1.0).unwrap();
            let output = graph.add_auxiliary_effect_with_role(
                graph.current_output(),
                matte,
                EffectRenderOp::Fill { grade },
                crate::AuxiliaryRole::Matte,
            );
            graph.set_current_output(output);
            Ok(())
        })),
    )
    .unwrap();
    let mut effect = instantiate_effect_node(kind).unwrap();
    effect
        .set_static_value_by_parameter(&parameter, PropertyValue::MaskReference(Some(selected.id)))
        .unwrap();

    let prepare = |masks: &[MaskComponent]| {
        PreparedEffectStack::prepare_with_masks(
            std::slice::from_ref(&effect),
            masks,
            WorkingColorSpace::LinearRec709,
        )
    };
    let original = prepare(&[selected.clone(), other.clone()]).unwrap();
    selected.name = "Renamed".to_owned();
    let reordered = prepare(&[other.clone(), selected.clone()]).unwrap();
    let evaluate = |stack: &PreparedEffectStack, time| {
        crate::compile_reference_render_graph(stack.evaluate_graph(time).unwrap()).unwrap()
    };
    let red = [1.0, 0.0, 0.0, 1.0];
    let blue = [0.0, 0.0, 2.0, 1.0];
    let first = evaluate(&reordered, TimelineTime::ZERO);
    assert_eq!(
        first.identity(),
        evaluate(&original, TimelineTime::ZERO).identity()
    );
    assert_eq!(execute(&first), [red, blue, blue, blue]);
    assert_eq!(
        execute(&evaluate(&reordered, midpoint)),
        [blue, red, blue, blue]
    );
    assert_eq!(execute(&evaluate(&reordered, end)), [blue, blue, red, blue]);

    let mixed = PreparedEffectProgram::prepare(
        std::slice::from_ref(&effect),
        &[selected.clone(), other.clone()],
        WorkingColorSpace::LinearRec709,
    )
    .unwrap();
    let mixed_at_start = mixed.evaluate(TimelineTime::ZERO).unwrap();
    assert!(mixed_at_start
        .graph()
        .nodes
        .iter()
        .any(|node| matches!(node.kind, EffectGraphNodeKind::MatteMix { .. })));
    assert!(!mixed_at_start
        .graph()
        .nodes
        .iter()
        .any(|node| matches!(node.kind, EffectGraphNodeKind::MaskCombine { .. })));
    assert_eq!(execute(&mixed_at_start), [blue; 4]);
    assert_eq!(
        execute(&mixed.evaluate(midpoint).unwrap()),
        [blue, red, blue, blue]
    );

    let mut disabled = selected.clone();
    disabled.enabled = false;
    for (masks, expected) in [
        (vec![other], EffectMaskReferenceError::Missing(selected.id)),
        (
            vec![disabled.clone()],
            EffectMaskReferenceError::Disabled(selected.id),
        ),
        (
            vec![selected.clone(), disabled],
            EffectMaskReferenceError::Ambiguous(selected.id),
        ),
    ] {
        assert!(
            matches!(prepare(&masks), Err(EffectGraphBuildError::InvalidAuthorState { reason, .. }) if reason == expected.to_string())
        );
    }
}

#[test]
fn authored_fill_mask_paints_only_the_selected_shape() {
    let mut mask = rectangle(0.0, 0.5, crate::MaskOp::Add);
    mask.participation = MaskParticipation::EffectOnly;
    let (stack, graph) = stack_graph(fill(1.0, 0.0, false), &[mask]);
    assert_eq!(
        stack.execution_contract().execution_modes,
        EffectExecutionModes::CPU_F32
    );
    assert_eq!(
        stack.execution_contract().topology,
        EffectGraphTopology::GeneralDag
    );
    assert!(graph.graph().nodes.iter().any(|node| matches!(
        node.kind,
        EffectGraphNodeKind::AuxiliaryEffect { op: EffectRenderOp::Fill { .. }, .. }
    )));
    assert_eq!(
        execute(&graph),
        [
            [1.0, 0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 2.0, 1.0],
            [0.0, 0.0, 2.0, 1.0]
        ]
    );
}

#[test]
fn unmasked_fill_retains_its_gpu_contract_and_default_pixels() {
    let (stack, graph) = stack_graph(
        fill(0.0, 0.0, false),
        &[rectangle(0.0, 0.5, crate::MaskOp::Add)],
    );
    assert!(stack
        .execution_contract()
        .execution_modes
        .is_subset_of(EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32)));
    assert!(stack.execution_contract().execution_modes.contains(
        crate::EffectProcessingBackend::Gpu,
        crate::EffectWorkingPrecision::Float32
    ));
    assert!(!graph
        .graph()
        .nodes
        .iter()
        .any(|node| matches!(node.kind, EffectGraphNodeKind::AuxiliaryEffect { .. })));
    assert_eq!(execute(&graph), [[1.0, 0.0, 0.0, 1.0]; 4]);
}

#[test]
fn fill_all_masks_honours_authored_algebra_and_overrides_the_index() {
    let mut masks = [
        rectangle(0.0, 1.0, crate::MaskOp::Add),
        rectangle(0.75, 0.25, crate::MaskOp::Subtract),
    ];
    masks[1].participation = MaskParticipation::EffectOnly;
    let (_, graph) = stack_graph(fill(99.0, 1.0, false), &masks);
    assert!(graph
        .graph()
        .nodes
        .iter()
        .any(|node| matches!(node.kind, EffectGraphNodeKind::MaskCombine { .. })));
    assert_eq!(
        execute(&graph),
        [
            [1.0, 0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 2.0, 1.0]
        ]
    );
}

#[test]
fn authored_fill_matte_admits_gpu_nodes_within_a_cpu_raster_stage() {
    let masks = [
        rectangle(0.0, 1.0, crate::MaskOp::Add),
        rectangle(0.75, 0.25, crate::MaskOp::Subtract),
    ];
    let mut effect = fill(0.0, 1.0, false);
    effect.instantiate_for_clip("Fill".to_owned());
    let graph = PreparedEffectProgram::prepare(&[effect], &masks, WorkingColorSpace::LinearRec709)
        .unwrap()
        .evaluate(TimelineTime::ZERO)
        .unwrap();
    let stage = graph.stage_bindings().iter().find(|stage| stage.native_fill_matte()).unwrap();
    assert_eq!(
        stage.contract().execution_modes,
        EffectExecutionModes::CPU_F32
    );
    assert_eq!(stage.emitted_nodes().len(), 4);
    for id in stage.emitted_nodes() {
        let kind = &graph.graph().node(*id).unwrap().kind;
        let expected = match kind {
            EffectGraphNodeKind::MaskSource { .. } => EffectExecutionModes::CPU_F32,
            EffectGraphNodeKind::MaskCombine { .. }
            | EffectGraphNodeKind::AuxiliaryEffect { .. } => {
                EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32)
            }
            other => panic!("unexpected masked Fill node {other:?}"),
        };
        assert_eq!(graph.node_execution_modes(*id), Some(expected));
    }
    assert_eq!(graph.auxiliary_inputs(), &[]);
}

#[test]
fn disabled_masks_do_not_renumber_authored_fill_references() {
    let mut disabled = rectangle(0.0, 0.5, crate::MaskOp::Add);
    disabled.enabled = false;
    let masks = [disabled, rectangle(0.5, 0.5, crate::MaskOp::Add)];
    let (_, graph) = stack_graph(fill(2.0, 0.0, false), &masks);
    assert_eq!(
        execute(&graph),
        [
            [0.0, 0.0, 2.0, 1.0],
            [0.0, 0.0, 2.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0]
        ]
    );
    assert!(PreparedEffectStack::prepare_with_masks(
        &[fill(1.0, 0.0, false)],
        &masks,
        WorkingColorSpace::LinearRec709
    )
    .is_err());
}

#[test]
fn inverting_a_hard_selected_mask_is_not_the_unmasked_identity() {
    let (_, graph) = stack_graph(
        fill(1.0, 0.0, true),
        &[rectangle(0.0, 0.5, crate::MaskOp::Add)],
    );
    assert_eq!(
        execute(&graph),
        [
            [0.0, 0.0, 2.0, 1.0],
            [0.0, 0.0, 2.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0]
        ]
    );
}

#[test]
fn missing_and_fractional_fill_mask_intent_fails_closed() {
    for effect in [
        fill(1.0, 0.0, false),
        fill(0.0, 1.0, false),
        fill(1.5, 0.0, false),
        fill(-1.0, 0.0, false),
    ] {
        assert!(matches!(
            PreparedEffectStack::prepare_with_masks(
                &[effect],
                &[],
                WorkingColorSpace::LinearRec709
            ),
            Err(EffectGraphBuildError::InvalidAuthorState { .. })
        ));
    }
}

#[test]
fn animated_fill_mask_selection_has_a_stable_conservative_contract() {
    let mut effect = fill(0.0, 0.0, false);
    let path = EffectType::Fill.property_path("fill_mask");
    effect
        .properties
        .set_keyframe(
            &path,
            Keyframe::linear(TimelineTime::ZERO, PropertyValue::Float(0.0)),
        )
        .unwrap();
    let later = TimelineTime::new(1, 1).unwrap();
    effect
        .properties
        .set_keyframe(&path, Keyframe::linear(later, PropertyValue::Float(1.0)))
        .unwrap();
    let mask = rectangle(0.0, 0.5, crate::MaskOp::Add);
    let program =
        PreparedEffectProgram::prepare(&[effect], &[mask], WorkingColorSpace::LinearRec709)
            .unwrap();
    let first = program.evaluate(TimelineTime::ZERO).unwrap();
    let second = program.evaluate(later).unwrap();
    assert_ne!(first.identity(), second.identity());
    assert_eq!(
        first.execution_envelope().aggregate().execution_modes,
        EffectExecutionModes::CPU_F32
    );
    assert_eq!(
        second.execution_envelope().aggregate().execution_modes,
        EffectExecutionModes::CPU_F32
    );
    assert_eq!(
        execute(&second),
        [
            [1.0, 0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 2.0, 1.0],
            [0.0, 0.0, 2.0, 1.0]
        ]
    );
}
