use super::*;
use crate::*;
use mondrian_core::{
    AssetId, ColorFrameAlpha, ImageSampling, PropertyBag, PropertyDescriptor, PropertyValue,
    VisualSourceReference, VisualSourceRetime,
};

#[derive(Clone, Copy)]
enum Case {
    Normal,
    Partial,
    Forged,
    WrongOwner,
    WrongReference,
    WrongFormat,
    History,
    WrongRoi,
}

fn source(id: AssetId) -> VisualSourceReference {
    VisualSourceReference::Asset {
        asset_id: id,
        retime: VisualSourceRetime::default(),
    }
}

fn matte(reference: VisualSourceReference, case: Case) -> EffectNode {
    let kind = EffectType::External(format!("test.preview-auxiliary.{}", EffectId::new()));
    let parameter = kind.parameter_id("source").unwrap();
    let mut properties = PropertyBag::default();
    properties.define(
        PropertyDescriptor::new(
            "source",
            "Picture source",
            PropertyValue::VisualSource(reference),
        )
        .with_parameter_id(parameter.clone()),
    );
    register_effect_definition(
        EffectDefinition::new(
            kind.key(),
            "Preview source",
            properties,
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            temporal_input: if matches!(case, Case::History) {
                EffectTemporalInputExtent::UNBOUNDED
            } else {
                EffectTemporalInputExtent::CURRENT_FRAME
            },
            ..EffectExecutionContract::IDENTITY
        })
        .with_branching_graph_builder(Arc::new(move |effect, context, builder| {
            if matches!(case, Case::Forged) {
                return Err(EffectGraphBuildError::MissingAuxiliaryInput {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    parameter_id: parameter.clone(),
                    slot: 0,
                });
            }
            if matches!(case, Case::Partial) {
                builder.append_unary(EffectRenderOp::ColorAdjust {
                    exposure: 1.0,
                    contrast: 1.0,
                    saturation: 1.0,
                    working_color_space: context.working_color_space,
                });
            }
            if matches!(case, Case::WrongRoi) {
                builder.append_unary(EffectRenderOp::GaussianBlur { radius: 1.0 });
            }
            let mut author = effect.clone();
            if matches!(case, Case::WrongOwner) {
                author.id = EffectId::new();
            }
            if matches!(case, Case::WrongReference) {
                author
                    .set_static_value_by_parameter(
                        &parameter,
                        PropertyValue::VisualSource(VisualSourceReference::Unbound),
                    )
                    .unwrap();
            }
            let input = builder.current_output();
            let auxiliary = builder.append_visual_source_input(
                &author,
                &parameter,
                EffectValueFormat::new(
                    if matches!(case, Case::WrongFormat) {
                        EffectWorkingPrecision::Float16
                    } else {
                        EffectWorkingPrecision::Float32
                    },
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

fn exposure() -> EffectNode {
    let kind = EffectType::External(format!(
        "test.preview-remaining-exposure.{}",
        EffectId::new()
    ));
    register_effect_definition(
        EffectDefinition::new(
            kind.key(),
            "Remaining exposure",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            ..EffectExecutionContract::IDENTITY
        })
        .with_graph_builder(Arc::new(|_, context, builder| {
            builder.append_unary(EffectRenderOp::ColorAdjust {
                exposure: 1.0,
                contrast: 1.0,
                saturation: 1.0,
                working_color_space: context.working_color_space,
            });
            Ok(())
        })),
    )
    .unwrap();
    instantiate_effect_node(kind).unwrap()
}

fn view(pixels: &[[f32; 4]]) -> EffectFloatFrameView<'_> {
    EffectFloatFrameView::new(
        EffectFrameExtent::new(1, 1),
        EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::StraightCoverage,
        ),
        ImageSampling::UNIT_SQUARE,
        pixels,
    )
}

fn render(graph: &CompiledEffectGraph) -> [f32; 4] {
    EffectExecutionSession::default()
        .apply_compiled_rgba_f32(
            view(&[[0.0, 0.0, 2.0, 1.0]]),
            graph,
            0,
            &mut EffectProcessorControl::uncancelled(),
        )
        .unwrap()
        .pixels()[0]
}

#[test]
fn auxiliary_preview_rolls_back_the_complete_missing_definition_and_preserves_following_effects() {
    let missing = matte(VisualSourceReference::Unbound, Case::Partial);
    let effects = [missing.clone(), exposure()];
    let before = serde_json::to_value(&effects).unwrap();
    assert!(matches!(
        PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec709),
        Err(EffectGraphBuildError::MissingAuxiliaryInput { .. })
    ));
    let preview = PreparedEffectProgram::prepare_for_auxiliary_preview(
        &effects,
        &[],
        WorkingColorSpace::LinearRec709,
    )
    .unwrap();
    for time in [TimelineTime::ZERO, TimelineTime::ONE] {
        let graph = preview.evaluate(time).unwrap();
        assert_eq!(render(&graph), [0.0, 0.0, 4.0, 1.0]);
        assert!(graph.auxiliary_inputs().is_empty());
        let issues = graph.auxiliary_preview_fallbacks().collect::<Vec<_>>();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].effect_id(), missing.id);
        assert_eq!(
            issues[0].parameter_id(),
            &missing.effect_type.parameter_id("source").unwrap()
        );
        assert_eq!(issues[0].cause(), AuxiliaryPreviewFallbackCause::Unbound);
        assert!(!graph
            .graph()
            .nodes
            .iter()
            .any(|node| matches!(node.kind, EffectGraphNodeKind::ExternalInput { .. })));
    }
    assert_eq!(serde_json::to_value(&effects).unwrap(), before);
}

#[test]
fn auxiliary_preview_unavailable_decisions_are_frame_local_and_strict_programs_refuse_them() {
    let reference = source(AssetId::new());
    let effects = [matte(reference, Case::Normal), exposure()];
    let strict =
        PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec709).unwrap();
    assert!(strict.evaluate_auxiliary_preview(TimelineTime::ZERO, &[reference]).is_err());
    let preview = PreparedEffectProgram::prepare_for_auxiliary_preview(
        &effects,
        &[],
        WorkingColorSpace::LinearRec709,
    )
    .unwrap();
    assert_ne!(strict.dependency_identity(), preview.dependency_identity());
    let available = preview.evaluate(TimelineTime::ZERO).unwrap();
    let missing = preview.evaluate_auxiliary_preview(TimelineTime::ZERO, &[reference]).unwrap();
    assert_eq!(render(&missing), [0.0, 0.0, 4.0, 1.0]);
    assert_eq!(
        missing.auxiliary_preview_fallbacks().next().unwrap().cause(),
        AuxiliaryPreviewFallbackCause::Unavailable
    );
    assert_eq!(
        missing.auxiliary_preview_fallbacks().next().unwrap().reference(),
        reference
    );
    assert_eq!(available.auxiliary_inputs().len(), 1);
    assert_eq!(
        preview.evaluate(TimelineTime::ZERO).unwrap().semantic_fingerprint(),
        available.semantic_fingerprint()
    );
    assert_ne!(
        missing.semantic_fingerprint(),
        available.semantic_fingerprint()
    );
}

#[test]
fn auxiliary_preview_does_not_accept_forged_or_misattributed_missing_input_errors() {
    for (reference, case) in [
        (VisualSourceReference::Unbound, Case::Forged),
        (VisualSourceReference::Unbound, Case::WrongOwner),
        (source(AssetId::new()), Case::WrongReference),
        (VisualSourceReference::Unbound, Case::WrongFormat),
    ] {
        assert!(PreparedEffectProgram::prepare_for_auxiliary_preview(
            &[matte(reference, case)],
            &[],
            WorkingColorSpace::LinearRec709
        )
        .is_err());
    }
}

#[test]
fn auxiliary_preview_identity_retains_its_notice_without_retaining_history_debt() {
    let effect = matte(VisualSourceReference::Unbound, Case::History);
    let preview = PreparedEffectProgram::prepare_for_auxiliary_preview(
        &[effect],
        &[],
        WorkingColorSpace::LinearRec709,
    )
    .unwrap();
    let graph = preview.evaluate(TimelineTime::ZERO).unwrap();
    assert!(graph.graph().is_identity());
    assert_eq!(graph.auxiliary_preview_fallbacks().count(), 1);
    assert_eq!(
        graph.execution_envelope().aggregate().temporal_input,
        EffectTemporalInputExtent::CURRENT_FRAME
    );
    assert!(!graph.requires_primary_temporal_execution());
    assert_eq!(render(&graph), [0.0, 0.0, 2.0, 1.0]);
    assert!(
        !Arc::ptr_eq(&graph, &identity_compiled_effect_graph().unwrap()),
        "a global bare identity must not discard consumer diagnostics"
    );
}

#[test]
fn auxiliary_preview_keeps_unrelated_contract_errors_strict() {
    assert!(matches!(
        PreparedEffectProgram::prepare_for_auxiliary_preview(
            &[matte(source(AssetId::new()), Case::WrongRoi)],
            &[],
            WorkingColorSpace::LinearRec709
        ),
        Err(EffectGraphBuildError::ExecutionContractViolation { .. })
    ));
}

fn grade(effect: EffectNode) -> GradeGraph {
    let input = GradeGraphNodeId::new();
    let output = GradeGraphNodeId::new();
    GradeGraph {
        nodes: vec![
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

#[test]
fn auxiliary_preview_preserves_shared_grade_origins_and_unfailed_consumers() {
    let missing = source(AssetId::new());
    let available = source(AssetId::new());
    let author = matte(missing, Case::Normal);
    let grade = PreparedGradeGraph::prepare_for_auxiliary_preview_with_lut_cache(
        &grade(author.clone()),
        WorkingColorSpace::LinearRec709,
        &LutPreparationCache::uncached(),
    )
    .unwrap();
    let effects = [matte(available, Case::Normal), exposure()];
    assert!(
        PreparedEffectProgram::prepare_hierarchical_with_lut_cache(
            &effects,
            &[],
            std::slice::from_ref(&grade),
            &[],
            WorkingColorSpace::LinearRec709,
            &LutPreparationCache::uncached()
        )
        .is_err(),
        "Strict programs must not reuse a Preview-prepared grade"
    );
    let preview = PreparedEffectProgram::prepare_hierarchical_for_auxiliary_preview(
        &effects,
        &[],
        std::slice::from_ref(&grade),
        std::slice::from_ref(&grade),
        WorkingColorSpace::LinearRec709,
        None,
        &LutPreparationCache::uncached(),
    )
    .unwrap();
    let graph = preview.evaluate_auxiliary_preview(TimelineTime::ONE, &[missing]).unwrap();
    let missing_stages = graph
        .stage_bindings()
        .iter()
        .filter(|stage| stage.auxiliary_preview_fallback().is_some())
        .collect::<Vec<_>>();
    assert_eq!(missing_stages.len(), 2);
    assert!(!missing_stages[0].shares_origin(missing_stages[1]));
    assert!(missing_stages.iter().all(|stage| stage
        .auxiliary_preview_fallback()
        .unwrap()
        .effect_id()
        == author.id));
    assert_eq!(graph.auxiliary_inputs().len(), 1);
    assert_eq!(
        graph.auxiliary_source_bindings().next().unwrap().reference(),
        available
    );
    let slot = graph.auxiliary_inputs()[0].slot();
    let output = EffectExecutionSession::default()
        .apply_compiled_rgba_f32_with_auxiliary(
            view(&[[0.0, 0.0, 2.0, 1.0]]),
            &graph,
            0,
            &[AuxiliaryFloatInput {
                slot,
                raster: AuxiliaryFloatFrame::new(
                    view(&[[0.0, 0.0, 0.0, 0.5]]),
                    AuxiliaryPolicy::MATCH_PRIMARY,
                ),
            }],
            &mut EffectProcessorControl::uncancelled(),
        )
        .unwrap();
    assert_eq!(output.pixels(), [[1.0, 0.0, 2.0, 1.0]]);
}

#[test]
fn auxiliary_preview_views_keep_one_source_context_for_every_evaluation_entry() {
    let missing = source(AssetId::new());
    let other = source(AssetId::new());
    let effects = [matte(missing, Case::History), exposure()];
    let base = PreparedEffectProgram::prepare_for_auxiliary_preview(
        &effects,
        &[],
        WorkingColorSpace::LinearRec709,
    )
    .unwrap();
    let selected = base
        .with_auxiliary_preview_unavailable_sources(&[missing, other, missing])
        .unwrap();
    let reordered = base.with_auxiliary_preview_unavailable_sources(&[other, missing]).unwrap();
    assert_eq!(
        selected.dependency_identity(),
        reordered.dependency_identity()
    );
    assert_ne!(selected.dependency_identity(), base.dependency_identity());
    assert_eq!(
        base.execution_contract().temporal_input,
        EffectTemporalInputExtent::UNBOUNDED
    );
    assert_eq!(
        selected.execution_contract().temporal_input,
        EffectTemporalInputExtent::CURRENT_FRAME
    );
    let mut session = EffectExecutionSession::default();
    for time in [
        TimelineTime::ZERO,
        TimelineTime::new(1, 2).unwrap(),
        TimelineTime::ONE,
    ] {
        for graph in [
            selected.evaluate(time).unwrap(),
            selected.evaluate_with_session(time, &mut session).unwrap(),
        ] {
            assert!(graph.auxiliary_inputs().is_empty());
            assert_eq!(graph.auxiliary_preview_fallbacks().count(), 1);
            assert!(!graph.requires_primary_temporal_execution());
            assert_eq!(render(&graph), [0.0, 0.0, 4.0, 1.0]);
        }
    }
    let restored = selected.with_auxiliary_preview_unavailable_sources(&[]).unwrap();
    assert_eq!(restored.dependency_identity(), base.dependency_identity());
    assert_eq!(restored.execution_contract(), base.execution_contract());
    assert_eq!(
        restored.evaluate(TimelineTime::ZERO).unwrap().auxiliary_inputs().len(),
        1
    );
    assert_eq!(
        base.evaluate(TimelineTime::ZERO).unwrap().auxiliary_preview_fallbacks().count(),
        0
    );
    let strict =
        PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec709).unwrap();
    assert!(strict.with_auxiliary_preview_unavailable_sources(&[]).is_err());
}

#[test]
fn auxiliary_preview_view_input_capabilities_keep_their_context_and_do_not_cross_owners() {
    let before_source = source(AssetId::new());
    let after_source = source(AssetId::new());
    let prepare_grade = |effect| {
        PreparedGradeGraph::prepare_for_auxiliary_preview_with_lut_cache(
            &grade(effect),
            WorkingColorSpace::LinearRec709,
            &LutPreparationCache::uncached(),
        )
        .unwrap()
    };
    let before = prepare_grade(matte(before_source, Case::Normal));
    let after = prepare_grade(matte(after_source, Case::Normal));
    let base = PreparedEffectProgram::prepare_hierarchical_for_auxiliary_preview(
        &[exposure(), exposure()],
        &[],
        &[before],
        &[after],
        WorkingColorSpace::LinearRec709,
        None,
        &LutPreparationCache::uncached(),
    )
    .unwrap();
    let view = base
        .with_auxiliary_preview_unavailable_sources(&[before_source, after_source])
        .unwrap();
    let original = base.evaluate(TimelineTime::ZERO).unwrap();
    let evaluated = view.evaluate(TimelineTime::ZERO).unwrap();
    let original_stage = original
        .stage_bindings()
        .iter()
        .find(|stage| stage.origin() == Some(EffectStageOrigin::Stack { index: 1 }))
        .unwrap();
    let stage = evaluated
        .stage_bindings()
        .iter()
        .find(|stage| stage.origin() == Some(EffectStageOrigin::Stack { index: 1 }))
        .unwrap();
    assert!(base.input_for_stage(stage).is_none());
    assert!(view.input_for_stage(original_stage).is_none());
    let entering = view.input_for_stage(stage).unwrap();
    for time in [TimelineTime::ZERO, TimelineTime::ONE] {
        let prefix = entering.evaluate(time).unwrap();
        assert!(prefix.auxiliary_inputs().is_empty());
        assert_eq!(prefix.auxiliary_preview_fallbacks().count(), 1);
        assert_eq!(render(&prefix), [0.0, 0.0, 4.0, 1.0]);
    }
    // Entering the post-Clip grade includes both exposures and the pre-Clip
    // fallback, while excluding the waiting Definition itself.
    let after_stage = evaluated
        .stage_bindings()
        .iter()
        .find(|stage| matches!(stage.origin(), Some(EffectStageOrigin::AfterGrade { .. })))
        .unwrap();
    let prefix = view.input_for_stage(after_stage).unwrap().evaluate(TimelineTime::ONE).unwrap();
    assert_eq!(render(&prefix), [0.0, 0.0, 8.0, 1.0]);
    assert_eq!(prefix.auxiliary_preview_fallbacks().count(), 1);
    assert_eq!(original.auxiliary_inputs().len(), 2);
}
