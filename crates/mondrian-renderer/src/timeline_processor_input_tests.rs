fn processor_input_boundary(calls: &Arc<AtomicU64>) -> EffectNode {
    let kind = EffectType::External(format!(
        "test.timeline.input.{}",
        mondrian_core::EffectId::new()
    ));
    let calls = calls.clone();
    register_effect_definition(
        EffectDefinition::new(
            kind.key(),
            "Waiting processor",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            roi_propagation: mondrian_effects::EffectRoiPropagation::UnknownRequiresFullFrame,
            ..EffectExecutionContract::IDENTITY
        })
        .with_custom_float_render_backend(
            Arc::new(move |_, context| {
                calls.fetch_add(1, Ordering::SeqCst);
                let _ = context;
                Ok(Some(serde_json::json!({})))
            }),
            None,
            mondrian_effects::EffectCachePolicy::Deterministic,
            Arc::new(|_, _, _| panic!("waiting/downstream processor must never run in its input")),
        ),
    )
    .unwrap();
    EffectNode::new(kind)
}

fn processor_input_scene(content: ClipContent) -> (Sequence, Arc<AtomicU64>) {
    let mut sequence = Sequence::new("Processor input source");
    sequence.settings.frame_rate = Rational::new(24, 1);
    sequence.settings.resolution = mondrian_core::Resolution { width: 1, height: 1 };
    sequence.video_tracks.clear();
    let mut track = Track::new_video("V1");
    let mut clip = Clip::new(
        AssetId::new(),
        TimelineTime::new(2, 1).unwrap(),
        TimelineTime::new(5, 1).unwrap(),
    )
    .unwrap();
    clip.content = content;
    clip.set_constant_source_time_map(
        TimelineTime::new(10, 1).unwrap(),
        TimeScale::new(2, 1).unwrap(),
    )
    .unwrap();
    clip.add_effect_node(temporal_sample_effect(TimelineTime::new(-1, 8).unwrap()));
    let calls = Arc::new(AtomicU64::new(0));
    clip.add_effect_node(processor_input_boundary(&calls));
    clip.add_effect_node(processor_input_boundary(&calls));
    track.add_clip(clip).unwrap();
    sequence.video_tracks.push(track);
    (sequence, calls)
}

fn input_scene_plan(program: &PreparedVisualProgram) -> TimelineRenderPlan {
    evaluate_prepared_visual_program(
        program,
        crate::TimelineEvaluationRequest::export(FramePosition::new(48, Rational::new(1, 24))),
    )
    .unwrap()
}

fn input_scene_binding(
    plan: &TimelineRenderPlan,
) -> (
    TimelineClipExecutionRef,
    &Arc<CompiledEffectGraph>,
    mondrian_effects::EffectGraphNodeId,
) {
    let (placement, graph) = match &plan.elements[0] {
        TimelineRenderPlanElement::Media(p) => (p.placement, &p.effect_graph),
        TimelineRenderPlanElement::NestedSequence(p) => (p.placement, &p.effect_graph),
        TimelineRenderPlanElement::SolidColor(p) => (p.placement, &p.effect_graph),
        _ => panic!("single source plan"),
    };
    (
        placement,
        graph,
        graph.stage_bindings()[1].emitted_nodes()[0],
    )
}

fn input_scene_request(time: TimelineTime) -> EffectTemporalExecutionRequest {
    let extent = EffectFrameExtent::new(1, 1);
    EffectTemporalExecutionRequest::new(
        89,
        EffectExecutionContinuity::Continuous,
        time,
        extent,
        mondrian_core::ImageSampling::UNIT_SQUARE,
        extent.full_frame_roi(),
        ExecutionCancellationToken::new(),
    )
    .with_output_frame_seed(i64::MIN) // Caller cannot choose a second seed authority.
}

#[test]
fn cpu_job_input_plan_rebinds_authority_without_losing_original_evaluation_grid() {
    let (sequence, _) = processor_input_scene(ClipContent::Media {
        asset_id: AssetId::new(),
        interpretation: Default::default(),
    });
    let program = Arc::new(PreparedVisualProgram::prepare(&sequence).unwrap());
    let plan = input_scene_plan(&program);
    let (placement, _, _) = input_scene_binding(&plan);
    let extent = EffectFrameExtent::new(1, 1);
    let old = ExecutionCancellationToken::new();
    let owner = TimelineFrameExecutionRequest::new(
        crate::TimelineEvaluationRequest::export(FramePosition::new(96, Rational::new(1, 48))),
        7,
        EffectExecutionContinuity::Continuous,
        extent,
        extent.full_frame_roi(),
        old.clone(),
    );
    let source_plan = program.clip_processor_input_plan(placement, &owner).unwrap();
    old.cancel();
    let actual = ExecutionCancellationToken::new();
    let authority = TimelineFrameExecutionAuthority::new(
        99,
        EffectExecutionContinuity::Discontinuous,
        actual.clone(),
    );
    let rebound = source_plan.bind(&authority);
    assert_eq!(rebound.evaluation().position, owner.evaluation().position);
    assert_eq!(rebound.evaluation().intent, owner.evaluation().intent);
    assert_eq!(rebound.frame_extent(), extent);
    assert_eq!(rebound.output_roi(), extent.full_frame_roi());
    assert_eq!(rebound.generation(), 99);
    assert_eq!(
        rebound.continuity(),
        EffectExecutionContinuity::Discontinuous
    );
    assert!(!rebound.cancellation().is_canceled());
    actual.cancel();
    assert!(rebound.cancellation().is_canceled());
}

#[test]
fn cpu_job_available_sources_never_substitute_current_media_for_missing_history() {
    let (sequence, _) = processor_input_scene(ClipContent::Media {
        asset_id: AssetId::new(),
        interpretation: Default::default(),
    });
    let program = Arc::new(PreparedVisualProgram::prepare(&sequence).unwrap());
    let plan = input_scene_plan(&program);
    let (placement, graph, node) = input_scene_binding(&plan);
    let extent = EffectFrameExtent::new(1, 1);
    let owner = TimelineFrameExecutionRequest::new(
        crate::TimelineEvaluationRequest::export(FramePosition::new(48, Rational::new(1, 24))),
        89,
        EffectExecutionContinuity::Continuous,
        extent,
        extent.full_frame_roi(),
        ExecutionCancellationToken::new(),
    );
    let source_plan = program.clip_processor_input_plan(placement, &owner).unwrap();
    let batch = program
        .prepare_clip_processor_input(
            placement,
            graph,
            node,
            &input_scene_request(TimelineTime::ZERO),
        )
        .unwrap();
    let current = crate::CpuColorFrame::working(mondrian_core::WorkingRgbaF32Frame {
        width: 1,
        height: 1,
        sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
        color_space: mondrian_core::WorkingColorSpace::LinearRec709,
        alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
        data: vec![[0.75, 0.0, 0.0, 1.0]],
    });
    let mut admissions = 0;
    let result = source_plan.prepare_available_sources(
        &batch,
        Some(&current),
        mondrian_core::WorkingColorSpace::LinearRec709,
        &mut |_| {
            admissions += 1;
            Ok(())
        },
        &mut mondrian_effects::EffectProcessorControl::uncancelled(),
    );
    assert!(matches!(
        result,
        Err(EffectTemporalExecutionError::CustomProcessor(
            mondrian_effects::EffectExecutionError::InputUnavailable { .. }
        ))
    ));
    assert_eq!(
        admissions, 0,
        "the complete source set is checked before tile allocation"
    );
}

use mondrian_timeline::{Clip, Sequence, Track};
use std::sync::atomic::{AtomicU64, Ordering};

#[test]
fn borrowed_clip_service_uses_original_attempt_grid_and_admits_before_source_materialization() {
    use mondrian_effects::{
        EffectProcessorControl, EffectProcessorInputRequest, EffectProcessorInputService,
    };
    use std::{
        cell::Cell,
        time::{Duration, Instant},
    };
    let (sequence, _) = processor_input_scene(ClipContent::Media {
        asset_id: AssetId::new(),
        interpretation: Default::default(),
    });
    let program = PreparedVisualProgram::prepare(&sequence).unwrap();
    let plan = input_scene_plan(&program);
    let (placement, graph, _) = input_scene_binding(&plan);
    let extent = EffectFrameExtent::new(2, 1);
    let sampling = mondrian_core::ImageSampling::new(
        mondrian_core::SampleAspectRatio::new(2, 1).unwrap(),
        [Rational::new(1, 2), Rational::new(3, 4)],
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let earlier = deadline - Duration::from_secs(10);
    let cancellation = ExecutionCancellationToken::new().with_deadline(earlier);
    let owner = TimelineFrameExecutionRequest::new(
        crate::TimelineEvaluationRequest::export(FramePosition::new(96, Rational::new(1, 48))),
        89,
        EffectExecutionContinuity::Discontinuous,
        EffectFrameExtent::new(1, 1),
        EffectFrameExtent::new(1, 1).full_frame_roi(),
        cancellation.clone(),
    );
    let granted = Cell::new(0);
    let calls = Cell::new(0);
    let mut resolve = |batch: &TimelineTemporalDemandBatch,
                       _: &mut dyn FnMut(usize) -> Result<(), EffectTemporalExecutionError>,
                       control: &mut EffectProcessorControl<'_>| {
        control.check().map_err(|_| EffectTemporalExecutionError::Canceled)?;
        assert!(granted.get() >= batch.effect_demands().coverage_bytes());
        calls.set(calls.get() + 1);
        let tiles = batch
            .source_demands()
            .iter()
            .map(|demand| {
                (
                    demand.effect_request,
                    EffectFrameTileF32::new(
                        demand.effect_request.time(),
                        extent,
                        sampling,
                        extent.full_frame_roi(),
                        0,
                        vec![[0.25, 0.5, 1.0, 1.0]; 2],
                    )
                    .unwrap(),
                )
            })
            .collect::<Vec<_>>();
        Ok(PreparedTemporalFrameSet::prepare(
            EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([91; 32]),
            batch.effect_demands().clone(),
            tiles,
        )
        .unwrap())
    };
    let mut service = program.clip_processor_inputs(placement, &owner, &mut resolve).unwrap();
    let mut admit = |bytes| {
        granted.set(granted.get().max(bytes));
        Ok(())
    };
    let request = EffectProcessorInputRequest {
        time: TimelineTime::new(-1, 48).unwrap(),
        frame_extent: extent,
        format: mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightCoverage,
        ),
        sampling,
        deadline,
    };
    let prepared = service
        .prepare_input(
            &graph.stage_bindings()[1],
            request,
            &mut admit,
            &mut EffectProcessorControl::uncancelled(),
        )
        .unwrap();
    assert_eq!(
        prepared.execution.request().output_frame_seed(),
        95,
        "original doubled field grid, not base-frame hash"
    );
    assert_eq!(prepared.execution.request().generation(), 89);
    assert_eq!(
        prepared.execution.request().continuity(),
        EffectExecutionContinuity::Discontinuous
    );
    assert_eq!(
        prepared.execution.request().cancellation().deadline(),
        Some(earlier)
    );
    assert_eq!(prepared.execution.request().sampling(), sampling);
    assert_eq!(prepared.execution.request().frame_extent(), extent);
    assert_eq!(prepared.origin, [0, 0]);
    assert_eq!(calls.get(), 1);
    cancellation.cancel();
    assert!(prepared.execution.request().cancellation().is_canceled());
}

#[test]
fn borrowed_clip_service_rejects_foreign_scope_and_native_float_alpha_before_sources() {
    use mondrian_effects::{
        EffectProcessorControl, EffectProcessorInputRequest, EffectProcessorInputService,
    };
    use std::{
        cell::Cell,
        time::{Duration, Instant},
    };
    let (sequence, _) = processor_input_scene(ClipContent::Media {
        asset_id: AssetId::new(),
        interpretation: Default::default(),
    });
    let program = PreparedVisualProgram::prepare(&sequence).unwrap();
    let other = PreparedVisualProgram::prepare(&sequence).unwrap();
    let plan = input_scene_plan(&program);
    let foreign = input_scene_plan(&other);
    let (placement, graph, _) = input_scene_binding(&plan);
    let (_, foreign_graph, _) = input_scene_binding(&foreign);
    let extent = EffectFrameExtent::new(1, 1);
    let make_owner = |frame| {
        TimelineFrameExecutionRequest::new(
            crate::TimelineEvaluationRequest::export(FramePosition::new(
                frame,
                Rational::new(1, 24),
            )),
            89,
            EffectExecutionContinuity::Continuous,
            extent,
            extent.full_frame_roi(),
            ExecutionCancellationToken::new(),
        )
    };
    let owner = make_owner(48);
    let wrong_owner = make_owner(49);
    let calls = Cell::new(0);
    let mut resolve = |_: &TimelineTemporalDemandBatch,
                       _: &mut dyn FnMut(usize) -> Result<(), EffectTemporalExecutionError>,
                       _: &mut EffectProcessorControl<'_>| {
        calls.set(calls.get() + 1);
        Err(EffectTemporalExecutionError::Canceled)
    };
    assert!(program.clip_processor_inputs(placement, &wrong_owner, &mut resolve).is_err());
    let mut service = program.clip_processor_inputs(placement, &owner, &mut resolve).unwrap();
    let mut request = EffectProcessorInputRequest {
        time: TimelineTime::ZERO,
        frame_extent: extent,
        format: mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
        deadline: Instant::now() + Duration::from_secs(30),
    };
    let mut admit = |_| panic!("no source allocation is legal on a rejected request");
    assert!(matches!(
        service.prepare_input(
            &graph.stage_bindings()[1],
            request,
            &mut admit,
            &mut EffectProcessorControl::uncancelled()
        ),
        Err(EffectTemporalExecutionError::FrameContract(_))
    ));
    request.format = request.format.with_alpha(mondrian_core::ColorFrameAlpha::StraightCoverage);
    assert!(service
        .prepare_input(
            &foreign_graph.stage_bindings()[1],
            request,
            &mut admit,
            &mut EffectProcessorControl::uncancelled()
        )
        .is_err());
    assert_eq!(calls.get(), 0);
}

#[test]
fn processor_input_uses_original_clip_handles_interpretation_and_temporal_pixels() {
    let asset_id = AssetId::new();
    let (sequence, calls) = processor_input_scene(ClipContent::Media {
        asset_id,
        interpretation: MediaInterpretation {
            frame_rate_override: Some(Rational::new(30, 1)),
            color_space_override: Some(ColorSpace::Rec2100Pq),
            alpha: AlphaInterpretation::Premultiplied,
            ..Default::default()
        },
    });
    let program = PreparedVisualProgram::prepare(&sequence).unwrap();
    let plan = input_scene_plan(&program);
    let (placement, graph, node) = input_scene_binding(&plan);
    let count = calls.load(Ordering::SeqCst);
    let batch = program
        .prepare_clip_processor_input(
            placement,
            graph,
            node,
            &input_scene_request(TimelineTime::new(-1, 12).unwrap()),
        )
        .unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        count,
        "no waiting/downstream evaluation"
    );
    assert_eq!(batch.execution_request().generation(), 89);
    assert_eq!(
        batch.execution_request().continuity(),
        EffectExecutionContinuity::Continuous
    );
    assert_eq!(
        batch.execution_request().output_frame_seed(),
        46,
        "Sequence frame, not retimed source frame"
    );
    assert_eq!(batch.graph().stage_bindings().len(), 1);
    let source_times = [
        TimelineTime::new(59, 6).unwrap(),
        TimelineTime::new(287, 30).unwrap(),
    ];
    let effect_times = [
        TimelineTime::new(-1, 12).unwrap(),
        TimelineTime::new(-5, 24).unwrap(),
    ];
    assert_eq!(batch.source_demands().len(), 2);
    let mut tiles = Vec::new();
    for ((demand, source_time), effect_time) in
        batch.source_demands().iter().zip(source_times).zip(effect_times)
    {
        let TimelineTemporalSource::Media {
            asset_id: actual,
            source_sample,
            color_space_override,
            alpha_interpretation,
            picture_overrides,
            auto_tone_map,
        } = demand.source
        else {
            panic!("media source");
        };
        assert_eq!(actual, asset_id);
        assert_eq!(source_sample.time(), source_time);
        assert_eq!(demand.effect_request.time(), effect_time);
        assert_eq!(color_space_override, Some(ColorSpace::Rec2100Pq));
        assert_eq!(alpha_interpretation, AlphaInterpretation::Premultiplied);
        assert_eq!(picture_overrides, Default::default());
        assert_eq!(
            auto_tone_map,
            sequence.settings.color.input.auto_tone_map_media
        );
        let tile = EffectFrameTileF32::new(
            effect_time,
            EffectFrameExtent::new(1, 1),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            EffectFrameExtent::new(1, 1).full_frame_roi(),
            0,
            vec![[source_time.to_f64() as f32, 0.0, 0.0, 1.0]],
        )
        .unwrap();
        tiles.push((demand.effect_request, tile));
    }
    let mut frames = PreparedTemporalFrameSet::prepare(
        EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([79; 32]),
        batch.effect_demands().clone(),
        tiles,
    )
    .unwrap();
    let mut session =
        EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(1024 * 1024));
    let result =
        execute_prepared_timeline_temporal_batch(&mut session, &batch, &mut frames).unwrap();
    let expected = (0.75 * 59.0 / 6.0 + 0.25 * 287.0 / 30.0) as f32;
    assert!((result.tile().pixels()[0][0] - expected).abs() < 2.0 * f32::EPSILON * expected);
    assert_eq!(calls.load(Ordering::SeqCst), count);

    // The existing complete-frame path uses exactly the same prepared source facts.
    let ordinary = collect_timeline_temporal_demands(
        &program,
        &plan,
        89,
        EffectExecutionContinuity::Continuous,
        EffectFrameExtent::new(1, 1),
        EffectFrameExtent::new(1, 1).full_frame_roi(),
        ExecutionCancellationToken::new(),
    )
    .unwrap();
    let entering = program
        .prepare_clip_processor_input(
            placement,
            graph,
            node,
            &input_scene_request(TimelineTime::ZERO),
        )
        .unwrap();
    assert_eq!(
        ordinary[0].source_demands().len(),
        entering.source_demands().len()
    );
    for (a, b) in ordinary[0].source_demands().iter().zip(entering.source_demands()) {
        let (
            TimelineTemporalSource::Media { source_sample: a, .. },
            TimelineTemporalSource::Media { source_sample: b, .. },
        ) = (&a.source, &b.source)
        else {
            panic!("media");
        };
        assert_eq!(a, b);
    }
}

#[test]
fn processor_input_keeps_nested_and_solid_sources_in_the_existing_algebra() {
    let child = SequenceId::new();
    for content in [
        ClipContent::NestedSequence {
            sequence_id: child,
            color_processing: NestedColorProcessing::PreserveChildWorkingSpace,
        },
        ClipContent::SolidColor {
            asset_id: AssetId::new(),
            color: Color { r: 0.25, g: -0.5, b: 2.0, a: 1.0 },
        },
    ] {
        let (sequence, calls) = processor_input_scene(content.clone());
        let program = PreparedVisualProgram::prepare(&sequence).unwrap();
        let plan = input_scene_plan(&program);
        let (placement, graph, node) = input_scene_binding(&plan);
        let before = calls.load(Ordering::SeqCst);
        let batch = program
            .prepare_clip_processor_input(
                placement,
                graph,
                node,
                &input_scene_request(TimelineTime::new(-1, 12).unwrap()),
            )
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), before);
        for demand in batch.source_demands() {
            match (&content, &demand.source) {
                (
                    ClipContent::NestedSequence { .. },
                    TimelineTemporalSource::NestedSequence {
                        sequence_id,
                        source_sample,
                        color_processing,
                    },
                ) => {
                    assert_eq!(*sequence_id, child);
                    assert_eq!(
                        *color_processing,
                        NestedColorProcessing::PreserveChildWorkingSpace
                    );
                    assert_eq!(
                        source_sample.time(),
                        TimelineTime::new(10, 1)
                            .unwrap()
                            .checked_add(
                                demand
                                    .effect_request
                                    .time()
                                    .checked_scale(TimeScale::new(2, 1).unwrap())
                                    .unwrap()
                            )
                            .unwrap()
                    );
                }
                (
                    ClipContent::SolidColor { color, .. },
                    TimelineTemporalSource::SolidColor { color: actual },
                ) => assert_eq!(actual, color),
                _ => panic!("source kind changed"),
            }
        }
    }
}

#[test]
fn processor_input_refuses_foreign_snapshot_placement_and_node_before_evaluation() {
    let (sequence, calls) = processor_input_scene(ClipContent::Media {
        asset_id: AssetId::new(),
        interpretation: Default::default(),
    });
    let program = PreparedVisualProgram::prepare(&sequence).unwrap();
    let foreign = PreparedVisualProgram::prepare(&sequence).unwrap();
    let plan = input_scene_plan(&program);
    let foreign_plan = input_scene_plan(&foreign);
    let (placement, graph, node) = input_scene_binding(&plan);
    let (_, foreign_graph, foreign_node) = input_scene_binding(&foreign_plan);
    let request = input_scene_request(TimelineTime::new(-1, 12).unwrap());
    let before = calls.load(Ordering::SeqCst);
    assert!(program
        .prepare_clip_processor_input(placement, foreign_graph, foreign_node, &request)
        .is_err());
    let mut wrong_placement = placement;
    wrong_placement.sequence_id = SequenceId::new();
    assert!(program
        .prepare_clip_processor_input(wrong_placement, graph, node, &request)
        .is_err());
    assert!(program
        .prepare_clip_processor_input(
            placement,
            graph,
            mondrian_effects::EffectGraphNodeId(u32::MAX),
            &request
        )
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), before);
}
use super::tests::temporal_sample_effect;
use super::*;
use crate::evaluate_prepared_visual_program;
use mondrian_core::timeline_data::MediaInterpretation;
use mondrian_core::{FramePosition, Rational, TimeScale};
use mondrian_effects::{
    register_effect_definition, EffectColorDomainContract, EffectDefinition,
    EffectExecutionContract, EffectExecutionModes, EffectExecutionSessionConfig,
    EffectFrameTileF32, EffectNode, EffectTemporalSourceIdentity, EffectType,
};

#[test]
fn historical_frame_seed_matches_the_original_sequence_grid_in_existing_temporal_execution() {
    assert_temporal_frame_seeds(
        Rational::new(24, 1),
        FramePosition::new(72, Rational::new(1, 24)),
        TimelineTime::new(-1, 8).unwrap(),
        69,
    );
}

#[test]
fn historical_frame_seed_matches_the_original_doubled_field_grid() {
    assert_temporal_frame_seeds(
        Rational::new(24, 1),
        FramePosition::new(145, Rational::new(1, 48)),
        TimelineTime::new(-1, 8).unwrap(),
        139,
    );
}

#[test]
fn historical_frame_seed_matches_the_original_fractional_field_grid() {
    assert_temporal_frame_seeds(
        Rational::new(30000, 1001),
        FramePosition::new(145, Rational::new(1001, 60000)),
        TimelineTime::new(-1001, 10000).unwrap(),
        139,
    );
}

fn assert_temporal_frame_seeds(
    rate: Rational,
    position: FramePosition,
    sample_offset: TimelineTime,
    historical_seed: i64,
) {
    let kind = EffectType::External(format!(
        "test.timeline.seed.{}",
        mondrian_core::EffectId::new()
    ));
    register_effect_definition(
        EffectDefinition::new(
            kind.key(),
            "Frame seed probe",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: mondrian_effects::EffectDeterminism::FrameSeeded,
            roi_propagation: mondrian_effects::EffectRoiPropagation::UnknownRequiresFullFrame,
            ..EffectExecutionContract::IDENTITY
        })
        .with_custom_float_render_backend(
            Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
            None,
            mondrian_effects::EffectCachePolicy::FrameDependent,
            Arc::new(|frame, _, seed| {
                for pixel in frame.pixels_mut() {
                    pixel[0] = seed as f32;
                }
                Ok(())
            }),
        ),
    )
    .unwrap();
    let mut sequence = Sequence::new("Original temporal seed");
    sequence.settings.frame_rate = rate;
    sequence.settings.resolution = mondrian_core::Resolution { width: 1, height: 1 };
    sequence.video_tracks.clear();
    let mut track = Track::new_video("V1");
    let mut clip = Clip::new(
        AssetId::new(),
        TimelineTime::new(2, 1).unwrap(),
        TimelineTime::new(5, 1).unwrap(),
    )
    .unwrap();
    clip.add_effect_node(EffectNode::new(kind));
    clip.add_effect_node(temporal_sample_effect(sample_offset));
    track.add_clip(clip).unwrap();
    sequence.video_tracks.push(track);
    let program = PreparedVisualProgram::prepare(&sequence).unwrap();
    let extent = EffectFrameExtent::new(1, 1);
    for evaluation in [
        crate::TimelineEvaluationRequest::preview(position, 1.0),
        crate::TimelineEvaluationRequest::export(position),
    ] {
        for with_session in [false, true] {
            let mut scratch = crate::TimelineCompositeScratch::default();
            let batches = if with_session {
                let prepared = scratch
                    .prepare_timeline_frame_execution(
                        &program,
                        TimelineFrameExecutionRequest::new(
                            evaluation,
                            89,
                            EffectExecutionContinuity::Continuous,
                            extent,
                            extent.full_frame_roi(),
                            ExecutionCancellationToken::new(),
                        ),
                    )
                    .unwrap();
                prepared
                    .bind_temporal_media_image_domains(|_| {
                        Ok((
                            extent,
                            program.materialization_contract().image_sampling(
                                mondrian_core::Resolution {
                                    width: extent.width(),
                                    height: extent.height(),
                                },
                            )?,
                        ))
                    })
                    .unwrap()
                    .try_into_parts()
                    .unwrap()
                    .2
            } else {
                let plan = evaluate_prepared_visual_program(&program, evaluation).unwrap();
                collect_timeline_temporal_demands(
                    &program,
                    &plan,
                    89,
                    EffectExecutionContinuity::Continuous,
                    extent,
                    extent.full_frame_roi(),
                    ExecutionCancellationToken::new(),
                )
                .unwrap()
            };
            let batch = &batches[0];
            assert_eq!(
                batch.execution_request().output_frame_seed(),
                position.frame
            );
            let tiles = batch
                .source_demands()
                .iter()
                .map(|demand| {
                    (
                        demand.effect_request,
                        EffectFrameTileF32::new(
                            demand.effect_request.time(),
                            extent,
                            mondrian_core::ImageSampling::UNIT_SQUARE,
                            extent.full_frame_roi(),
                            0,
                            vec![[0.0, 0.0, 0.0, 1.0]],
                        )
                        .unwrap(),
                    )
                })
                .collect::<Vec<_>>();
            let mut frames = PreparedTemporalFrameSet::prepare(
                EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([89; 32]),
                batch.effect_demands().clone(),
                tiles,
            )
            .unwrap();
            let mut session =
                EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(1024 * 1024));
            let result =
                execute_prepared_timeline_temporal_batch(&mut session, batch, &mut frames).unwrap();
            // Source tile seed0 is not the authority for processing either occurrence.
            assert_eq!(
                result.tile().pixels()[0][0],
                0.75 * position.frame as f32 + 0.25 * historical_seed as f32,
                "intent={:?}, session={with_session}, position={position:?}",
                evaluation.intent
            );
        }
    }
}

#[test]
fn processor_input_preserves_source_sampling_independent_of_the_project_canvas() {
    let (sequence, _) = processor_input_scene(ClipContent::Media {
        asset_id: AssetId::new(),
        interpretation: Default::default(),
    });
    let program = PreparedVisualProgram::prepare(&sequence).unwrap();
    let plan = input_scene_plan(&program);
    let (placement, graph, node) = input_scene_binding(&plan);
    let extent = EffectFrameExtent::new(2, 1);
    let sampling = mondrian_core::ImageSampling::new(
        mondrian_core::SampleAspectRatio::new(2, 1).unwrap(),
        [Rational::new(1, 2), Rational::new(3, 4)],
    )
    .unwrap();
    assert_ne!(
        sampling,
        program
            .materialization_contract()
            .image_sampling(mondrian_core::Resolution { width: 2, height: 1 })
            .unwrap()
    );
    let request = EffectTemporalExecutionRequest::new(
        89,
        EffectExecutionContinuity::Continuous,
        TimelineTime::new(-1, 12).unwrap(),
        extent,
        sampling,
        extent.full_frame_roi(),
        ExecutionCancellationToken::new(),
    );
    let batch = program.prepare_clip_processor_input(placement, graph, node, &request).unwrap();
    assert_eq!(batch.execution_request().frame_extent(), extent);
    assert_eq!(batch.execution_request().sampling(), sampling);
    let tiles = batch
        .source_demands()
        .iter()
        .map(|demand| {
            (
                demand.effect_request,
                EffectFrameTileF32::new(
                    demand.effect_request.time(),
                    extent,
                    sampling,
                    extent.full_frame_roi(),
                    0,
                    vec![[0.5, -0.25, 2.0, 1.0]; 2],
                )
                .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let mut frames = PreparedTemporalFrameSet::prepare(
        EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([83; 32]),
        batch.effect_demands().clone(),
        tiles,
    )
    .unwrap();
    let mut session =
        EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(1024 * 1024));
    let result =
        execute_prepared_timeline_temporal_batch(&mut session, &batch, &mut frames).unwrap();
    assert_eq!(result.tile().frame_extent(), extent);
    assert_eq!(result.tile().sampling(), sampling);
    assert_eq!(result.tile().pixels(), &[[0.5, -0.25, 2.0, 1.0]; 2]);
}
