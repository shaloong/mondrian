use mondrian_core::{
    automation::PropertyValue, AssetId, Color, ColorEngine, ExecutionCancellationToken,
    FramePosition, GradeDefinitionId, GradeGraphNode, GradeGraphNodeId, GradeGraphNodeKind,
    TimelineTime, WorkingColorSpace,
};
use mondrian_effects::{
    register_effect_definition, EffectColorDomainContract, EffectDefinition, EffectDeterminism,
    EffectExecutionContinuity, EffectExecutionContract, EffectExecutionModes, EffectFrameExtent,
    EffectGraphTopology, EffectNode, EffectNodeExt, EffectPixelRoi, EffectRenderOp,
    EffectResourceLifetime, EffectRoiPropagation, EffectStateModel, EffectTemporalInputExtent,
    EffectTemporalSpan, EffectType,
};
use mondrian_renderer::{
    admit_timeline_render_plan_for_cpu_compositor, composite_timeline_elements_color_frame,
    evaluate_prepared_visual_program, PreparedVisualProgram, PreparedVisualProgramCache,
    PreparedVisualProgramError, TimelineAdjustmentLayer, TimelineCompositeElement,
    TimelineCompositeOptions, TimelineCompositeScratch, TimelineEffectColorRuntime,
    TimelineEvaluationRequest, TimelineFrameExecutionRequest, TimelineFramePreparationError,
    TimelineRenderPlan, TimelineRenderPlanElement, TimelineSolidColorLayer,
    TimelineTemporalPreparationError,
};
use mondrian_timeline::{clip::ClipContent, Clip, GradeGroup, GradeScope, Sequence, Track};
use std::sync::Arc;

fn tt(sequence: &Sequence, frame: i64) -> TimelineTime {
    TimelineTime::from_frame_position(FramePosition::new(frame, sequence.time_base()))
        .expect("valid test time")
}

fn fp(sequence: &Sequence, frame: i64) -> FramePosition {
    FramePosition::new(frame, sequence.time_base())
}

fn prepare_stable(sequence: &Sequence) -> PreparedVisualProgram {
    for _ in 0..32 {
        match PreparedVisualProgram::prepare(sequence) {
            Ok(program) => return program,
            Err(PreparedVisualProgramError::EffectRegistryChanged { .. }) => {}
            Err(error) => panic!("visual preparation failed: {error}"),
        }
    }
    panic!("Effect registry did not stabilize during test")
}

fn single_solid_sequence(effect: Option<EffectNode>) -> Sequence {
    let mut sequence = Sequence::new("grade hierarchy");
    sequence.video_tracks.clear();
    let mut track = Track::new_video("V1");
    let mut clip = Clip::new_solid_color(
        AssetId::new(),
        Color::BLACK,
        tt(&sequence, 0),
        tt(&sequence, 20),
    )
    .expect("solid Clip");
    if let Some(effect) = effect {
        clip.add_effect_node(effect);
    }
    track.add_clip(clip).expect("add solid Clip");
    sequence.video_tracks.push(track);
    sequence
}

fn append_grade_effect(
    sequence: &mut Sequence,
    definition_id: GradeDefinitionId,
    effect: EffectNode,
) -> GradeGraphNodeId {
    let definition = sequence
        .grade_definitions
        .iter_mut()
        .find(|definition| definition.id == definition_id)
        .expect("grade definition");
    let version = definition
        .versions
        .iter_mut()
        .find(|version| version.id == definition.active_version)
        .expect("active grade version");
    let id = GradeGraphNodeId::new();
    let input = version.graph.output;
    version.graph.nodes.push(GradeGraphNode {
        id,
        kind: GradeGraphNodeKind::Effect { input, effect },
    });
    version.graph.output = id;
    id
}

fn exposure_effect(stops: f32) -> EffectNode {
    let mut effect = EffectNode::with_defaults(EffectType::BasicCorrection);
    let parameter = EffectType::BasicCorrection
        .parameter_id("exposure")
        .expect("exposure parameter ID");
    effect
        .set_static_value_by_parameter(&parameter, PropertyValue::Float(stops))
        .expect("set exposure");
    effect
}

fn commercial_grade_hierarchy_sequence() -> Sequence {
    let mut sequence = single_solid_sequence(Some(exposure_effect(0.25)));
    let ClipContent::SolidColor { color, .. } = &mut sequence.video_tracks[0].clips[0].content
    else {
        panic!("fixture must remain a Solid Color Clip");
    };
    *color = Color { r: 0.1, g: 0.2, b: 0.3, a: 1.0 };
    let clip_id = sequence.video_tracks[0].clips[0].id;
    let group_pre = sequence.add_grade_definition("Group Pre");
    let clip_grade = sequence.add_grade_definition("Clip Grade");
    let group_post = sequence.add_grade_definition("Group Post");
    let timeline_grade = sequence.add_grade_definition("Timeline Grade");
    for definition_id in [group_pre, clip_grade, group_post, timeline_grade] {
        append_grade_effect(&mut sequence, definition_id, exposure_effect(0.25));
    }
    let group = GradeGroup::new("Scene");
    let group_id = group.id;
    sequence.grade_groups.push(group);
    sequence
        .assign_grade(GradeScope::GroupPre(group_id), Some(group_pre))
        .expect("assign group pre");
    sequence
        .assign_grade(GradeScope::Clip(clip_id), Some(clip_grade))
        .expect("assign Clip grade");
    sequence
        .assign_grade(GradeScope::GroupPost(group_id), Some(group_post))
        .expect("assign group post");
    sequence
        .assign_grade(GradeScope::Timeline, Some(timeline_grade))
        .expect("assign Timeline Grade");
    sequence.video_tracks[0].clips[0].grade_group = Some(group_id);
    sequence
}

#[test]
fn grade_hierarchy_enters_clip_plan_in_exact_order_and_timeline_grade_runs_once_last() {
    let sequence = commercial_grade_hierarchy_sequence();
    let program = prepare_stable(&sequence);
    let plan = evaluate_prepared_visual_program(
        &program,
        TimelineEvaluationRequest::export(fp(&sequence, 0)),
    )
    .expect("evaluate grade hierarchy");

    assert_eq!(plan.elements.len(), 2);
    let TimelineRenderPlanElement::SolidColor(clip) = &plan.elements[0] else {
        panic!("Clip must lower before Timeline Grade");
    };
    assert_eq!(
        clip.effect_graph.stage_bindings().len(),
        4,
        "Group Pre, Clip stack, Clip Grade, and Group Post retain exact stage order"
    );
    assert!(matches!(
        plan.elements.last(),
        Some(TimelineRenderPlanElement::TimelineGrade(_))
    ));
    assert_eq!(
        plan.elements
            .iter()
            .filter(|element| matches!(element, TimelineRenderPlanElement::TimelineGrade(_)))
            .count(),
        1
    );
}

#[test]
fn preview_and_export_share_exact_clip_and_timeline_grade_graphs() {
    let sequence = commercial_grade_hierarchy_sequence();
    let program = prepare_stable(&sequence);
    let preview = evaluate_prepared_visual_program(
        &program,
        TimelineEvaluationRequest::preview(fp(&sequence, 0), 1.0),
    )
    .expect("Preview grade plan");
    let export = evaluate_prepared_visual_program(
        &program,
        TimelineEvaluationRequest::export(fp(&sequence, 0)),
    )
    .expect("Export grade plan");

    let TimelineRenderPlanElement::SolidColor(preview_clip) = &preview.elements[0] else {
        panic!("Preview Clip plan");
    };
    let TimelineRenderPlanElement::SolidColor(export_clip) = &export.elements[0] else {
        panic!("Export Clip plan");
    };
    assert!(Arc::ptr_eq(
        &preview_clip.effect_graph,
        &export_clip.effect_graph
    ));
    let Some(TimelineRenderPlanElement::TimelineGrade(preview_timeline)) = preview.elements.last()
    else {
        panic!("Preview Timeline Grade");
    };
    let Some(TimelineRenderPlanElement::TimelineGrade(export_timeline)) = export.elements.last()
    else {
        panic!("Export Timeline Grade");
    };
    assert!(Arc::ptr_eq(
        &preview_timeline.effect_graph,
        &export_timeline.effect_graph
    ));

    let preview_pixels = composite_grade_plan(&preview);
    let export_pixels = composite_grade_plan(&export);
    assert_eq!(preview_pixels, export_pixels);
    assert!(
        preview_pixels[0][0] > 0.1,
        "five ordered exposure stages must affect pixels"
    );
    assert_eq!(preview_pixels[0][3], 1.0);
}

fn composite_grade_plan(plan: &TimelineRenderPlan) -> Vec<[f32; 4]> {
    let mut elements = Vec::with_capacity(plan.elements.len());
    for element in &plan.elements {
        match element {
            TimelineRenderPlanElement::SolidColor(layer) => {
                elements.push(TimelineCompositeElement::SolidColor(
                    TimelineSolidColorLayer {
                        color: layer.color,
                        opacity: layer.opacity,
                        blend_mode: layer.blend_mode,
                        transform: layer.transform,
                        effect_graph: Arc::clone(&layer.effect_graph),
                        frame_seed: layer.frame_seed,
                    },
                ));
            }
            TimelineRenderPlanElement::TimelineGrade(grade) => {
                elements.push(TimelineCompositeElement::Adjustment(
                    TimelineAdjustmentLayer {
                        effect_graph: Arc::clone(&grade.effect_graph),
                        opacity: 1.0,
                        blend_mode: None,
                        frame_seed: grade.frame_seed,
                    },
                ));
            }
            other => panic!("unexpected grade test plan element: {other:?}"),
        }
    }
    let engine = ColorEngine::default();
    composite_timeline_elements_color_frame(
        1,
        1,
        &elements,
        TimelineCompositeOptions::default(),
        TimelineEffectColorRuntime::new(&engine, WorkingColorSpace::LinearRec709),
        &mut TimelineCompositeScratch::default(),
    )
    .expect("composite grade plan")
    .rgba_f32()
    .data
    .clone()
}

#[test]
fn clip_program_reuse_invalidates_only_the_grade_definition_it_references() {
    let mut sequence = single_solid_sequence(None);
    let second = Clip::new_solid_color(
        AssetId::new(),
        Color::WHITE,
        tt(&sequence, 30),
        tt(&sequence, 10),
    )
    .expect("second Clip");
    sequence.video_tracks[0].add_clip(second).expect("add second Clip");
    let left_id = sequence.video_tracks[0].clips[0].id;
    let right_id = sequence.video_tracks[0].clips[1].id;
    let left_grade = sequence.add_grade_definition("Left");
    let right_grade = sequence.add_grade_definition("Right");
    append_grade_effect(
        &mut sequence,
        left_grade,
        EffectNode::with_defaults(EffectType::BasicCorrection),
    );
    append_grade_effect(
        &mut sequence,
        right_grade,
        EffectNode::with_defaults(EffectType::BasicCorrection),
    );
    sequence
        .assign_grade(GradeScope::Clip(left_id), Some(left_grade))
        .expect("left");
    sequence
        .assign_grade(GradeScope::Clip(right_id), Some(right_grade))
        .expect("right");

    let mut cache = PreparedVisualProgramCache::new(4);
    cache.prepare(&sequence).expect("initial grade programs");
    append_grade_effect(
        &mut sequence,
        left_grade,
        EffectNode::with_defaults(EffectType::ColorWheel),
    );
    sequence.revision = sequence.revision.checked_next().expect("grade revision");
    let replacement = cache.prepare(&sequence).expect("replacement grade programs");
    assert_eq!(replacement.diagnostics().prepared_clips, 2);
    assert_eq!(replacement.diagnostics().reused_clips, 1);
}

#[test]
fn timeline_grade_blocker_fails_preflight_and_active_frame_closed() {
    let mut sequence = single_solid_sequence(None);
    let timeline_grade = sequence.add_grade_definition("Unavailable Timeline Grade");
    append_grade_effect(
        &mut sequence,
        timeline_grade,
        EffectNode::new(EffectType::Plugin("missing.timeline.grade".to_owned())),
    );
    sequence
        .assign_grade(GradeScope::Timeline, Some(timeline_grade))
        .expect("assign unavailable Timeline Grade");
    let program = prepare_stable(&sequence);
    assert!(program.preflight().is_err());
    assert!(evaluate_prepared_visual_program(
        &program,
        TimelineEvaluationRequest::export(fp(&sequence, 0)),
    )
    .is_err());
}

#[test]
fn temporal_timeline_grade_fails_closed_until_full_stack_history_is_admitted() {
    let effect_type = EffectType::Plugin("test.timeline.grade.temporal".to_owned());
    let offset = TimelineTime::new(1, 24).expect("temporal offset");
    register_effect_definition(
        EffectDefinition::new(
            effect_type.key(),
            "Temporal Timeline Grade",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: EffectDeterminism::Deterministic,
            state_model: EffectStateModel::Stateless,
            temporal_input: EffectTemporalInputExtent {
                past: EffectTemporalSpan::Finite(offset),
                future: EffectTemporalSpan::None,
            },
            roi_propagation: EffectRoiPropagation::PixelLocal,
            resource_lifetime: EffectResourceLifetime::Frame,
            topology: EffectGraphTopology::LinearChain,
        })
        .with_graph_builder(Arc::new(move |_, _, graph| {
            graph.append_unary(EffectRenderOp::TemporalFrameBlend {
                sample_offset: TimelineTime::ZERO.checked_sub(offset).expect("past sample offset"),
                mix: 0.5,
            });
            Ok(())
        })),
    )
    .expect("register temporal Timeline Grade");

    let mut sequence = single_solid_sequence(None);
    let timeline_grade = sequence.add_grade_definition("Temporal");
    append_grade_effect(&mut sequence, timeline_grade, EffectNode::new(effect_type));
    sequence
        .assign_grade(GradeScope::Timeline, Some(timeline_grade))
        .expect("assign temporal Timeline Grade");
    let program = prepare_stable(&sequence);
    let extent = EffectFrameExtent::new(8, 4);
    let request = TimelineFrameExecutionRequest::new(
        TimelineEvaluationRequest::export(fp(&sequence, 0)),
        1,
        EffectExecutionContinuity::Discontinuous,
        extent,
        EffectPixelRoi::new(0, 0, 8, 4),
        ExecutionCancellationToken::new(),
    );
    let error = TimelineCompositeScratch::default()
        .prepare_timeline_frame_execution(&program, request)
        .expect_err("temporal Timeline Grade must fail closed");
    assert!(matches!(
        error,
        TimelineFramePreparationError::Temporal(
            TimelineTemporalPreparationError::TimelineGradeUnsupported
        )
    ));
}

#[test]
fn empty_transparent_timeline_retains_explicit_grade_but_skips_pixel_execution() {
    let mut sequence = Sequence::new("empty Timeline Grade");
    let timeline_grade = sequence.add_grade_definition("Timeline");
    append_grade_effect(
        &mut sequence,
        timeline_grade,
        EffectNode::with_defaults(EffectType::BasicCorrection),
    );
    sequence
        .assign_grade(GradeScope::Timeline, Some(timeline_grade))
        .expect("assign Timeline Grade");
    let program = prepare_stable(&sequence);
    let plan = evaluate_prepared_visual_program(
        &program,
        TimelineEvaluationRequest::export(fp(&sequence, 0)),
    )
    .expect("empty Timeline Grade plan");

    assert!(matches!(
        plan.elements.as_slice(),
        [TimelineRenderPlanElement::TimelineGrade(_)]
    ));
    let admission = admit_timeline_render_plan_for_cpu_compositor(&plan)
        .expect("alpha-preserving grade over no picture is a safe no-op");
    assert_eq!(admission.effect_graphs, 0);
}
