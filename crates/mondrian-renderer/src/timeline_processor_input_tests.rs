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

use mondrian_timeline::{Clip, Sequence, Track};
use std::sync::atomic::{AtomicU64, Ordering};

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
    clip.add_effect_node(EffectNode::new(kind));
    clip.add_effect_node(temporal_sample_effect(TimelineTime::new(-1, 8).unwrap()));
    track.add_clip(clip).unwrap();
    sequence.video_tracks.push(track);
    let program = PreparedVisualProgram::prepare(&sequence).unwrap();
    let plan = evaluate_prepared_visual_program(
        &program,
        crate::TimelineEvaluationRequest::export(FramePosition::new(72, sequence.time_base())),
    )
    .unwrap();
    let extent = EffectFrameExtent::new(1, 1);
    let batches = collect_timeline_temporal_demands(
        &program,
        &plan,
        89,
        EffectExecutionContinuity::Continuous,
        extent,
        extent.full_frame_roi(),
        ExecutionCancellationToken::new(),
    )
    .unwrap();
    let batch = &batches[0];
    assert_eq!(batch.execution_request().output_frame_seed(), 72);
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
    // Current Sequence frame72; historical Clip7/8s + placement2s = frame69.
    // Source tile seed0 is not the authority for processing either occurrence.
    assert_eq!(result.tile().pixels()[0][0], 0.75 * 72.0 + 0.25 * 69.0);
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
