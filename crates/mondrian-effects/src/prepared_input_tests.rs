use super::*;
use crate::*;
use mondrian_core::{
    effect_data::EffectType, BlendMode, ExecutionCancellationToken, Keyframe, PropertyValue,
};
use std::sync::atomic::{AtomicUsize, Ordering};

fn tt(n: i64, d: i64) -> TimelineTime {
    TimelineTime::new(n, d).unwrap()
}

#[test]
fn processor_input_requires_the_exact_direct_definition_input() {
    let kind = EffectType::External(format!("fixture.input-nodes.{}", EffectId::new()));
    let processor =
        crate::effect::CustomEffectProcessorBinding::new_f32(Arc::new(|_, _, _| Ok(())));
    register_effect_definition(
        EffectDefinition::new(
            kind.key(),
            "Two processor nodes",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
            ..EffectExecutionContract::IDENTITY
        })
        .with_graph_builder(Arc::new(move |_, _, graph| {
            let op = EffectRenderOp::Custom {
                key: "fixture.node-boundary".into(),
                params: serde_json::json!({}),
                cache_key: None,
                cache_policy: EffectCachePolicy::Deterministic,
                processor: Some(processor.clone()),
            };
            graph.append_unary(op.clone());
            graph.append_unary(op);
            Ok(())
        })),
    )
    .unwrap();
    let effects = [exposure(1.0), EffectNode::new(kind)];
    let program =
        PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec709).unwrap();
    let other =
        PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec709).unwrap();
    let graph = program.evaluate(TimelineTime::ZERO).unwrap();
    let stage = &graph.stage_bindings()[1];
    assert_eq!(stage.emitted_nodes().len(), 2);
    let direct = stage.emitted_nodes()[0];
    let internal = stage.emitted_nodes()[1];
    let cap = program.input_for_processor(&graph, direct).expect("direct Custom input");
    assert_eq!(cap.evaluate(tt(1, 2)).unwrap().stage_bindings().len(), 1);
    assert!(
        program.input_for_processor(&graph, internal).is_none(),
        "internal input must not resolve to Definition entry"
    );
    assert!(
        other.input_for_processor(&graph, direct).is_none(),
        "same IDs do not prove same immutable snapshot"
    );
    assert!(
        program
            .input_for_processor(&graph, graph.stage_bindings()[0].emitted_nodes()[0])
            .is_none(),
        "builtin node is not a processor"
    );
    assert!(program.input_for_processor(&graph, EffectGraphNodeId(u32::MAX)).is_none());
}

fn exposure(stops: f32) -> EffectNode {
    let mut effect = EffectNode::with_defaults(EffectType::BasicCorrection);
    let id = EffectType::BasicCorrection.parameter_id("exposure").unwrap();
    effect.set_static_value_by_parameter(&id, PropertyValue::Float(stops)).unwrap();
    effect
}

fn sentinel(calls: &Arc<AtomicUsize>) -> EffectNode {
    let kind = EffectType::External(format!("fixture.input-sentinel.{}", EffectId::new()));
    let calls = calls.clone();
    register_effect_definition(
        EffectDefinition::new(
            kind.key(),
            "Input boundary",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: EffectDeterminism::Deterministic,
            state_model: EffectStateModel::Stateless,
            temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
            roi_propagation: EffectRoiPropagation::PixelLocal,
            resource_lifetime: EffectResourceLifetime::Frame,
            topology: EffectGraphTopology::LinearChain,
        })
        .with_graph_builder(Arc::new(move |_, context, graph| {
            calls.fetch_add(1, Ordering::SeqCst);
            if context.time != TimelineTime::ZERO {
                return Err(EffectGraphBuildError::InvalidGraph);
            }
            graph.append_unary(EffectRenderOp::Vignette { intensity: 0.25, feather: 0.75 });
            Ok(())
        })),
    )
    .unwrap();
    EffectNode::new(kind)
}

fn serial_grade(effect: EffectNode) -> PreparedGradeGraph {
    let input = GradeGraphNodeId::new();
    let output = GradeGraphNodeId::new();
    PreparedGradeGraph::prepare(
        &GradeGraph {
            nodes: mondrian_core::AuthoringList::from([
                mondrian_core::GradeGraphNode { id: input, kind: GradeGraphNodeKind::Input },
                mondrian_core::GradeGraphNode {
                    id: output,
                    kind: GradeGraphNodeKind::Effect { input, effect },
                },
            ]),
            output,
        },
        WorkingColorSpace::LinearRec709,
    )
    .unwrap()
}

fn execute(
    input: &PreparedEffectInput,
    time: TimelineTime,
    budget: usize,
    cancellation: ExecutionCancellationToken,
) -> Result<EffectTemporalExecutionOutput, EffectTemporalExecutionError> {
    let extent = EffectFrameExtent::new(1, 1);
    let sampling = mondrian_core::ImageSampling::UNIT_SQUARE;
    let request = EffectTemporalExecutionRequest::new(
        19,
        EffectExecutionContinuity::Discontinuous,
        time,
        extent,
        sampling,
        extent.full_frame_roi(),
        cancellation,
    );
    let prepared = input.prepare_execution(&request, |_| Ok(0))?;
    let batch = prepared.demands().clone();
    let resolved = batch
        .requests()
        .iter()
        .copied()
        .map(|r| {
            (
                r,
                EffectFrameTileF32::new(
                    r.time(),
                    extent,
                    sampling,
                    extent.full_frame_roi(),
                    0,
                    vec![[0.5, -0.25, 2.0, 1.0]],
                )
                .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    // Complete closed fixture source: one constant unit-square, linear Rec709
    // coverage pixel at every Clip time. No decode/nested work in copy_frame.
    let mut source = PreparedTemporalFrameSet::prepare(
        EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([73; 32]),
        batch,
        resolved,
    )
    .unwrap();
    EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(budget))
        .execute_prepared_temporal_f32(&prepared, &mut source)
}

fn assert_pixel(actual: &[f32; 4], scale: f32) {
    let expected = [0.5 * scale, -0.25 * scale, 2.0 * scale, 1.0];
    for lane in 0..4 {
        assert!(
            (actual[lane] - expected[lane]).abs()
                <= 32.0 * f32::EPSILON * expected[lane].abs().max(1.0),
            "lane {lane}: {actual:?} != {expected:?}"
        );
    }
}

#[test]
fn exact_input_uses_frozen_upstream_animation_without_waiting_or_downstream_evaluation() {
    let mut first = exposure(0.0);
    let parameter = EffectType::BasicCorrection.parameter_id("exposure").unwrap();
    let path = first
        .properties
        .iter()
        .find(|(_, p)| p.descriptor.parameter_id() == &parameter)
        .unwrap()
        .1
        .descriptor
        .path
        .clone();
    first
        .properties
        .set_keyframe(
            &path,
            Keyframe::linear(tt(-1, 1), PropertyValue::Float(-1.0)),
        )
        .unwrap();
    first
        .properties
        .set_keyframe(&path, Keyframe::linear(tt(1, 1), PropertyValue::Float(1.0)))
        .unwrap();
    let waiting = Arc::new(AtomicUsize::new(0));
    let downstream = Arc::new(AtomicUsize::new(0));
    let effects = vec![first.clone(), sentinel(&waiting), sentinel(&downstream)];
    let program =
        PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec709).unwrap();
    let graph = program.evaluate(TimelineTime::ZERO).unwrap();
    let input = program.input_for_stage(&graph.stage_bindings()[1]).unwrap();
    let counts = (
        waiting.load(Ordering::SeqCst),
        downstream.load(Ordering::SeqCst),
    );
    for (time, scale) in [(tt(-1, 1), 0.5), (tt(1, 1), 2.0), (tt(-1, 1), 0.5)] {
        let output = execute(&input, time, 64 * 1024, ExecutionCancellationToken::new()).unwrap();
        assert_eq!(output.provider_requests(), 1);
        assert!(output.peak_working_bytes() <= 64 * 1024);
        assert_pixel(&output.tile().pixels()[0], scale);
    }
    assert_eq!(
        counts,
        (
            waiting.load(Ordering::SeqCst),
            downstream.load(Ordering::SeqCst)
        )
    );
    assert!(
        program.evaluate(tt(-1, 1)).is_err(),
        "full graph must encounter the waiting sentinel"
    );
    let other =
        PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec709).unwrap();
    assert!(
        other.input_for_stage(&graph.stage_bindings()[1]).is_none(),
        "same IDs/values are not the same frozen snapshot"
    );
    assert!(matches!(
        execute(&input, tt(-1, 1), 0, ExecutionCancellationToken::new()),
        Err(EffectTemporalExecutionError::WorkingSetBudgetExceeded { budget_bytes: 0, .. })
    ));
    let canceled = ExecutionCancellationToken::new();
    canceled.cancel();
    assert!(matches!(
        execute(&input, tt(-1, 1), 64 * 1024, canceled),
        Err(EffectTemporalExecutionError::Canceled)
    ));
    let extent = EffectFrameExtent::new(1, 1);
    let request = EffectTemporalExecutionRequest::new(
        19,
        EffectExecutionContinuity::Discontinuous,
        tt(-1, 1),
        extent,
        mondrian_core::ImageSampling::UNIT_SQUARE,
        extent.full_frame_roi(),
        ExecutionCancellationToken::new(),
    );
    let plain =
        PreparedEffectProgram::prepare(&[exposure(0.0)], &[], WorkingColorSpace::LinearRec709)
            .unwrap();
    assert!(
        matches!(
            prepare_temporal_frame_execution(&plain, &request, |_| Ok(0)),
            Err(EffectTemporalExecutionError::UnsupportedTemporalShape { .. })
        ),
        "legacy temporal-only guard remains intact"
    );
}

#[test]
fn repeated_shared_grade_has_distinct_before_stack_after_input_capabilities() {
    let shared = serial_grade(exposure(1.0));
    let program = PreparedEffectProgram::prepare_hierarchical_with_lut_cache(
        &[exposure(-1.0)],
        &[],
        std::slice::from_ref(&shared),
        &[shared],
        WorkingColorSpace::LinearRec709,
        &LutPreparationCache::uncached(),
    )
    .unwrap();
    let graph = program.evaluate(tt(1, 1)).unwrap();
    assert_eq!(graph.stage_bindings().len(), 3);
    for (index, scale) in [(0, 1.0), (1, 2.0), (2, 1.0)] {
        let input = program.input_for_stage(&graph.stage_bindings()[index]).unwrap();
        let output = execute(
            &input,
            tt(-1, 1),
            64 * 1024,
            ExecutionCancellationToken::new(),
        )
        .unwrap();
        assert_pixel(&output.tile().pixels()[0], scale);
        assert_eq!(
            input.evaluate(tt(-1, 1)).unwrap().stage_bindings().len(),
            index
        );
    }
    let after = program.input_for_stage(&graph.stage_bindings()[2]).unwrap();
    let prefix = after.evaluate(tt(-1, 1)).unwrap();
    let nested = program.input_for_stage(&prefix.stage_bindings()[1]).unwrap();
    assert_pixel(
        &execute(
            &nested,
            tt(-1, 1),
            64 * 1024,
            ExecutionCancellationToken::new(),
        )
        .unwrap()
        .tile()
        .pixels()[0],
        2.0,
    );
}

#[test]
fn grade_input_evaluates_only_the_selected_branch_ancestors() {
    let waiting = Arc::new(AtomicUsize::new(0));
    let sibling = Arc::new(AtomicUsize::new(0));
    let input = GradeGraphNodeId::new();
    let up = GradeGraphNodeId::new();
    let target = GradeGraphNodeId::new();
    let right = GradeGraphNodeId::new();
    let output = GradeGraphNodeId::new();
    let grade = PreparedGradeGraph::prepare(
        &GradeGraph {
            nodes: mondrian_core::AuthoringList::from([
                mondrian_core::GradeGraphNode { id: input, kind: GradeGraphNodeKind::Input },
                mondrian_core::GradeGraphNode {
                    id: up,
                    kind: GradeGraphNodeKind::Effect { input, effect: exposure(1.0) },
                },
                mondrian_core::GradeGraphNode {
                    id: target,
                    kind: GradeGraphNodeKind::Effect { input: up, effect: sentinel(&waiting) },
                },
                mondrian_core::GradeGraphNode {
                    id: right,
                    kind: GradeGraphNodeKind::Effect { input, effect: sentinel(&sibling) },
                },
                mondrian_core::GradeGraphNode {
                    id: output,
                    kind: GradeGraphNodeKind::Parallel {
                        inputs: mondrian_core::AuthoringList::from([right, target]),
                        blend_mode: BlendMode::Normal,
                        opacity: 0.5,
                    },
                },
            ]),
            output,
        },
        WorkingColorSpace::LinearRec709,
    )
    .unwrap();
    let program = PreparedEffectProgram::prepare_hierarchical_with_lut_cache(
        &[],
        &[],
        &[grade],
        &[],
        WorkingColorSpace::LinearRec709,
        &LutPreparationCache::uncached(),
    )
    .unwrap();
    let graph = program.evaluate(TimelineTime::ZERO).unwrap();
    assert_eq!(graph.stage_bindings().len(), 4);
    let cap = program.input_for_stage(&graph.stage_bindings()[2]).unwrap();
    assert!(
        program.input_for_stage(&graph.stage_bindings()[3]).is_none(),
        "synthetic composite has no Definition input"
    );
    let counts = (
        waiting.load(Ordering::SeqCst),
        sibling.load(Ordering::SeqCst),
    );
    assert_pixel(
        &execute(
            &cap,
            tt(-1, 1),
            64 * 1024,
            ExecutionCancellationToken::new(),
        )
        .unwrap()
        .tile()
        .pixels()[0],
        2.0,
    );
    assert_eq!(
        counts,
        (
            waiting.load(Ordering::SeqCst),
            sibling.load(Ordering::SeqCst)
        )
    );
    let prefix = cap.evaluate(tt(-1, 1)).unwrap();
    assert_eq!(prefix.stage_bindings().len(), 1);
    // The surviving upstream stage is now index0 but was index1 in the full
    // graph; nested capability selection must use placement, not that ordinal.
    let nested = program.input_for_stage(&prefix.stage_bindings()[0]).unwrap();
    assert_pixel(
        &execute(
            &nested,
            tt(-1, 1),
            64 * 1024,
            ExecutionCancellationToken::new(),
        )
        .unwrap()
        .tile()
        .pixels()[0],
        1.0,
    );
}

#[derive(Debug)]
struct SourceOnlyFactory;
impl EffectFloatProcessorFactory for SourceOnlyFactory {
    fn input_role(&self) -> EffectFloatProcessorInputRole {
        EffectFloatProcessorInputRole::NoImage
    }
    fn alpha_contract(&self) -> EffectAlphaContract {
        EffectAlphaContract::SOURCE_FLOAT
    }
    fn feedback_budget_bytes(&self) -> usize {
        0
    }
    fn storage(
        &self,
        _: EffectFloatProcessorSizing,
    ) -> mondrian_core::Result<EffectFloatProcessorStorage> {
        EffectFloatProcessorStorage::new(0, 0)
    }
    fn prepare(
        &self,
        _: EffectFloatProcessorPreparation,
        _: &mut EffectProcessorControl<'_>,
    ) -> mondrian_core::Result<Box<dyn EffectFloatProcessorInstance>> {
        panic!("binding input metadata must never prepare or execute a source")
    }
}

#[test]
fn no_image_factory_has_no_implicit_entering_image() {
    let kind = EffectType::External(format!("fixture.no-input.{}", EffectId::new()));
    register_effect_definition(
        EffectDefinition::new(
            kind.key(),
            "Source",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: EffectDeterminism::Nondeterministic,
            roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
            ..EffectExecutionContract::IDENTITY
        })
        .with_custom_float_processor_factory(
            Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
            None,
            EffectCachePolicy::Uncacheable,
            Arc::new(SourceOnlyFactory),
        ),
    )
    .unwrap();
    let program = PreparedEffectProgram::prepare(
        &[EffectNode::new(kind)],
        &[],
        WorkingColorSpace::LinearRec709,
    )
    .unwrap();
    let graph = program.evaluate(TimelineTime::ZERO).unwrap();
    assert!(program.input_for_stage(&graph.stage_bindings()[0]).is_none());
}

#[test]
fn clip_mask_is_after_stack_inputs_and_before_after_grade_inputs() {
    let before = serial_grade(exposure(1.0));
    let after = serial_grade(exposure(1.0));
    let mask = MaskComponent::new(
        "half window".into(),
        MaskEvaluation {
            shape: MaskShape::Rectangle {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                corner_radius: 0.0,
            },
            feather: 0.0,
            opacity: 0.5,
            ..MaskEvaluation::default()
        },
    );
    let program = PreparedEffectProgram::prepare_hierarchical_with_lut_cache(
        &[exposure(1.0)],
        &[mask],
        &[before],
        &[after],
        WorkingColorSpace::LinearRec709,
        &LutPreparationCache::uncached(),
    )
    .unwrap();
    let graph = program.evaluate(TimelineTime::ZERO).unwrap();
    assert_eq!(graph.stage_bindings().len(), 4);
    assert!(program.input_for_stage(&graph.stage_bindings()[2]).is_none());
    let stack_input = program.input_for_stage(&graph.stage_bindings()[1]).unwrap();
    let after_input = program.input_for_stage(&graph.stage_bindings()[3]).unwrap();
    assert_pixel(
        &execute(
            &stack_input,
            tt(1, 1),
            64 * 1024,
            ExecutionCancellationToken::new(),
        )
        .unwrap()
        .tile()
        .pixels()[0],
        2.0,
    );
    // Grade-before is the mask base (2x); the graded stack is4x. Half
    // coverage mixes those values to3x, rather than reapplying Clip alpha.
    assert_pixel(
        &execute(
            &after_input,
            tt(1, 1),
            64 * 1024,
            ExecutionCancellationToken::new(),
        )
        .unwrap()
        .tile()
        .pixels()[0],
        3.0,
    );
}

#[test]
fn input_provenance_does_not_split_semantically_identical_gpu_plans() {
    let prepare = |stops| {
        PreparedEffectProgram::prepare_hierarchical_with_lut_cache(
            &[],
            &[],
            &[serial_grade(exposure(stops))],
            &[],
            WorkingColorSpace::LinearRec709,
            &LutPreparationCache::uncached(),
        )
        .unwrap()
    };
    let first = prepare(1.0);
    let second = prepare(1.0);
    let a = first.evaluate(TimelineTime::ZERO).unwrap();
    let b = second.evaluate(TimelineTime::ZERO).unwrap();
    assert_eq!(a.semantic_fingerprint(), b.semantic_fingerprint());
    assert_ne!(
        a.stage_bindings()[0].origin(),
        b.stage_bindings()[0].origin()
    );
    assert!(second.input_for_stage(&a.stage_bindings()[0]).is_none());
    let mut session = EffectExecutionSession::default();
    let plan_a = session.get_or_lower_gpu_plan(&a).unwrap();
    let plan_b = session.get_or_lower_gpu_plan(&b).unwrap();
    assert!(Arc::ptr_eq(&plan_a, &plan_b));
    assert_eq!(session.diagnostics().gpu_plan_entries, 1);

    let changed = prepare(-1.0).evaluate(TimelineTime::ZERO).unwrap();
    let changed_plan = session.get_or_lower_gpu_plan(&changed).unwrap();
    assert!(!Arc::ptr_eq(&plan_a, &changed_plan));
    assert_eq!(session.diagnostics().gpu_plan_entries, 2);
}
