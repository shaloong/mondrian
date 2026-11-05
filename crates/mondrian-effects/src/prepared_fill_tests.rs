use super::*;
use crate::{instantiate_effect_node, EffectRenderOp, EffectType, MaskEvaluation, MaskShape};
use mondrian_core::automation::{Keyframe, PropertyMutation, PropertyValue};
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
fn authored_fill_mask_paints_only_the_selected_shape() {
    let mask = rectangle(0.0, 0.5, crate::MaskOp::Add);
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
    let masks = [
        rectangle(0.0, 1.0, crate::MaskOp::Add),
        rectangle(0.75, 0.25, crate::MaskOp::Subtract),
    ];
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
