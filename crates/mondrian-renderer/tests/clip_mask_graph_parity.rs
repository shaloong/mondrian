//! Production Clip mask preparation → CPU raster frontier → wgpu stack/local grade.

#[path = "support/gpu_availability.rs"]
mod gpu_availability;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use glam::Vec2;
use mondrian_core::{
    automation::PropertyValue,
    effect_data::{EffectNode, EffectType},
    mask_data::{
        BezierPoint, MaskComponent, MaskEvaluation, MaskOp, MaskShape, MaskShapeInterpolation,
    },
    timeline_data::FieldOrder,
    AssetId, Color, ColorFrameAlpha, ExecutionCancellationToken, FramePosition, ImageSampling,
    Rational, TimelineTime, WorkingColorSpace,
};
use mondrian_effects::{
    apply_compiled_effect_graph_rgba_f32, EffectColorDomain, EffectExecutionSession,
    EffectExecutionSessionConfig, EffectFloatFrameView, EffectFrameExtent,
    EffectGraphExecutionBudget, EffectGraphNodeKind, EffectNodeExt, EffectValueFormat,
    EffectWorkingPrecision, PreparedHeterogeneousCpuCompletion, PreparedHeterogeneousGpuDispatch,
    PreparedHeterogeneousGpuStep,
};
use mondrian_renderer::{
    evaluate_prepared_visual_program_with_session, GpuColorFrameWgpuResourcePoolOptions,
    GpuContext, HeterogeneousGpuContinuationBinding, HeterogeneousGpuContinuationRequest,
    HeterogeneousGpuContinuationRuntime, HeterogeneousGpuResourceGrant,
    PreparedHeterogeneousEffectRoute, PreparedVisualProgram, TimelineEvaluationRequest,
    TimelineRenderPlanElement, TimelineSolidColorPlan,
};
use mondrian_timeline::{Clip, Sequence, Track};

const EXTENT: EffectFrameExtent = EffectFrameExtent::new(32, 24);
const GENERATION: u64 = 1;
const BUDGET_BYTES: u64 = 16 * 1024 * 1024;
const WORKING_SPACE: WorkingColorSpace = WorkingColorSpace::LinearRec2020;
const SAMPLE_TICKS: [i64; 2] = [10, 11];
const OPS: [MaskOp; 4] = [
    MaskOp::Add,
    MaskOp::Subtract,
    MaskOp::Intersect,
    MaskOp::Difference,
];

fn time(ticks: i64) -> TimelineTime {
    TimelineTime::new(ticks, 50).unwrap()
}

fn rect(x: f32, y: f32, width: f32, height: f32) -> MaskShape {
    MaskShape::Rectangle { x, y, width, height, corner_radius: 0.0 }
}

fn ellipse(x: f32, y: f32, rx: f32, ry: f32) -> MaskShape {
    MaskShape::Ellipse { center: Vec2::new(x, y), radii: Vec2::new(rx, ry) }
}

fn cubic(x: f32) -> MaskShape {
    // Nonzero relative handles make this a curved, four-segment closed path.
    let points = [
        (Vec2::new(x, 0.25), Vec2::new(0.075, 0.0)),
        (Vec2::new(x + 0.125, 0.375), Vec2::new(0.0, 0.075)),
        (Vec2::new(x, 0.50), Vec2::new(-0.075, 0.0)),
        (Vec2::new(x - 0.125, 0.375), Vec2::new(0.0, -0.075)),
    ]
    .into_iter()
    .map(|(position, tangent)| BezierPoint {
        position,
        control_in: -tangent,
        control_out: tangent,
    })
    .collect();
    MaskShape::Path { points, closed: true }
}

fn authored_program() -> PreparedVisualProgram {
    let mut sequence = Sequence::new("native Clip mask stack");
    sequence.settings.frame_rate = Rational::FPS_25;
    // Keep the default 1080p extent for field-order admission. The effect
    // graph is then evaluated separately on our small, shared CPU/GPU grid.
    sequence.settings.field_order = FieldOrder::UpperFirst;
    sequence.settings.color.working_color_space = WORKING_SPACE;
    let mut clip = Clip::new_solid_color(
        AssetId::new(),
        Color { r: 1.5, g: 0.40, b: 0.18, a: 0.375 },
        time(8),
        time(30),
    )
    .expect("solid Clip");
    clip.clip_time_in = time(10);
    let mut correction = EffectNode::with_defaults(EffectType::BasicCorrection);
    correction
        .set_static_value_by_parameter(
            &EffectType::BasicCorrection
                .parameter_id("exposure")
                .expect("exposure parameter ID"),
            PropertyValue::Float(0.75),
        )
        .expect("author local grade");
    clip.add_effect_node(correction);

    // The first mask seeds the stack. Four subsequent masks exercise all four joins.
    clip.add_mask_component(MaskComponent::new(
        "seed rectangle".into(),
        MaskEvaluation {
            shape: rect(0.05, 0.10, 0.49, 0.68),
            ..MaskEvaluation::default()
        },
    ));
    clip.add_mask_component(MaskComponent::new(
        "add ellipse".into(),
        MaskEvaluation {
            shape: ellipse(0.48, 0.55, 0.27, 0.30),
            feather: 1.5,
            opacity: 0.65,
            expansion: 0.5,
            ..MaskEvaluation::default()
        },
    ));
    let mut moving = MaskComponent::new(
        "subtract animated cubic".into(),
        MaskEvaluation {
            shape: cubic(0.125),
            feather: 0.75,
            opacity: 0.8,
            expansion: -0.5,
            mask_op: MaskOp::Subtract,
            ..MaskEvaluation::default()
        },
    );
    moving
        .set_shape_animation_enabled(true, time(11))
        .expect("enable shape animation");
    for (tick, x) in [(11, 0.125), (15, 0.625)] {
        moving
            .write_shape(time(tick), cubic(x), MaskShapeInterpolation::Linear)
            .expect("author exact shape key");
    }
    clip.add_mask_component(moving);
    clip.add_mask_component(MaskComponent::new(
        "intersect inverted rectangle".into(),
        MaskEvaluation {
            shape: rect(0.12, 0.56, 0.38, 0.24),
            opacity: 0.7,
            invert: true,
            mask_op: MaskOp::Intersect,
            ..MaskEvaluation::default()
        },
    ));
    clip.add_mask_component(MaskComponent::new(
        "difference ellipse".into(),
        MaskEvaluation {
            shape: ellipse(0.66, 0.51, 0.13, 0.25),
            opacity: 0.5,
            mask_op: MaskOp::Difference,
            ..MaskEvaluation::default()
        },
    ));
    let mut track = Track::new_video("V1");
    track.add_clip(clip).expect("place Clip");
    sequence.video_tracks = vec![track].into();
    PreparedVisualProgram::prepare(&sequence).expect("production visual program")
}

fn sample(
    program: &PreparedVisualProgram,
    tick: i64,
    session: &mut EffectExecutionSession,
) -> TimelineSolidColorPlan {
    let position = FramePosition::new(tick, Rational::new(1, 50));
    let plan = evaluate_prepared_visual_program_with_session(
        program,
        TimelineEvaluationRequest::export(position),
        session,
    )
    .expect("production timeline plan at exact field position");
    assert_eq!(plan.position, position);
    let [TimelineRenderPlanElement::SolidColor(layer)] = plan.elements.as_slice() else {
        panic!("expected only the solid Clip");
    };
    assert_eq!(layer.placement.clip_time, time(tick - 8 + 10));
    layer.clone()
}

fn input_pixels(layer: &TimelineSolidColorPlan) -> Vec<[f32; 4]> {
    let Color { r, g, b, a } = layer.color;
    assert!(
        r > 1.0 && a > 0.0 && a < 1.0,
        "the source must be HDR and translucent"
    );
    vec![[r, g, b, a]; (EXTENT.width() * EXTENT.height()) as usize]
}

fn frame_view(pixels: &[[f32; 4]]) -> EffectFloatFrameView<'_> {
    EffectFloatFrameView::new(
        EXTENT,
        EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::StraightCoverage,
        ),
        ImageSampling::UNIT_SQUARE,
        pixels,
    )
}

fn route_and_prefix(
    layer: &TimelineSolidColorPlan,
    input: &[[f32; 4]],
) -> (
    PreparedHeterogeneousEffectRoute,
    PreparedHeterogeneousCpuCompletion,
) {
    let route = PreparedHeterogeneousEffectRoute::prepare(
        Arc::clone(&layer.effect_graph),
        EXTENT,
        EffectGraphExecutionBudget::new(BUDGET_BYTES, BUDGET_BYTES, BUDGET_BYTES, 32, 64),
    )
    .expect("production CPU → GPU mask route");
    let work = route.prepared_work();
    let graph = layer.effect_graph.graph();
    let mut sources = graph
        .nodes
        .iter()
        .filter(|n| matches!(&n.kind, EffectGraphNodeKind::MaskSource { .. }));
    let source_ids: Vec<_> = sources.by_ref().map(|n| n.id).collect();
    assert_eq!(source_ids.len(), 5);
    assert_eq!(work.cpu_nodes().len(), source_ids.len());
    assert!(source_ids.iter().all(|id| work.cpu_nodes().contains(id)));

    let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(
        BUDGET_BYTES as usize,
    ));
    session.bind_generation(GENERATION);
    let prefix = work
        .execute_cpu_prefix_uncancelled(
            &mut session,
            GENERATION,
            frame_view(input),
            layer.frame_seed,
            WORKING_SPACE,
        )
        .expect("rasterize all authored masks on CPU");
    let uploads = prefix.evidence().transfers();
    assert_eq!(uploads.len(), 6, "five mattes plus the scene-linear source");
    for id in &source_ids {
        assert_eq!(
            uploads
                .iter()
                .filter(|transfer| {
                    transfer.format().domain() == EffectColorDomain::AlphaMask
                        && work
                            .plan()
                            .materialization(transfer.cpu_materialization())
                            .unwrap()
                            .value()
                            == *id
                })
                .count(),
            1,
            "one exact AlphaMask frontier upload for {id:?}"
        );
    }
    assert_eq!(
        uploads
            .iter()
            .filter(|u| u.format().domain() == EffectColorDomain::SceneLinearRgb)
            .count(),
        1
    );

    let dispatches: Vec<_> = work
        .gpu_suffix()
        .steps()
        .iter()
        .filter_map(|step| match step {
            PreparedHeterogeneousGpuStep::Dispatch(dispatch) => Some(dispatch),
            PreparedHeterogeneousGpuStep::Release { .. } => None,
        })
        .collect();
    assert_eq!(
        dispatches.len(),
        6,
        "one grade, four stack joins, one local grade mix"
    );
    let mut prior = source_ids[0];
    let mut seen_ops = Vec::new();
    for dispatch in &dispatches {
        if let PreparedHeterogeneousGpuDispatch::MaskCombine {
            node,
            left,
            right,
            waits,
            mask_op,
            ..
        } = dispatch
        {
            let rhs = source_ids[seen_ops.len() + 1];
            assert_eq!(work.plan().materialization(*left).unwrap().value(), prior);
            assert_eq!(work.plan().materialization(*right).unwrap().value(), rhs);
            assert_eq!(waits.len(), 2);
            let upload = uploads.iter().find(|u| u.gpu_materialization() == *right).unwrap();
            assert!(waits.contains(&upload.pending_gpu_input_token()));
            prior = *node;
            seen_ops.push(*mask_op);
        }
    }
    assert_eq!(seen_ops, OPS);
    let Some(PreparedHeterogeneousGpuDispatch::MatteMix { node, matte, waits, .. }) =
        dispatches.last().copied()
    else {
        panic!("local grade mix must be the final GPU dispatch");
    };
    assert_eq!(Some(*node), graph.output);
    assert_eq!(work.plan().materialization(*matte).unwrap().value(), prior);
    assert!(dispatches[..dispatches.len() - 1].iter().any(|dispatch| matches!(
        dispatch, PreparedHeterogeneousGpuDispatch::PointChain { signal, .. } if waits.contains(signal)
    )));
    assert!(dispatches.iter().any(|dispatch| matches!(
        dispatch, PreparedHeterogeneousGpuDispatch::MaskCombine { node, signal, .. }
            if *node == prior && waits.contains(signal)
    )));
    (route, prefix)
}

#[test]
fn production_clip_mask_graph_samples_exact_time_and_prepares_gpu_suffix() {
    let program = authored_program();
    let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig::default());
    let mut frames = Vec::new();
    let mut fingerprints = Vec::new();
    for (tick, path_x) in [(10, 0.25), (11, 0.375)] {
        let layer = sample(&program, tick, &mut session);
        let graph = &layer.effect_graph;
        let paths: Vec<_> = graph
            .graph()
            .nodes
            .iter()
            .filter_map(|node| match &node.kind {
                EffectGraphNodeKind::MaskSource {
                    shape: shape @ MaskShape::Path { .. },
                    feather,
                    expansion,
                    opacity,
                    invert,
                } => {
                    assert_eq!(
                        (*feather, *expansion, *opacity, *invert),
                        (0.75, -0.5, 0.8, false)
                    );
                    Some(shape)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            paths,
            [&cubic(path_x)],
            "shape and handles at exact Clip-local author time"
        );
        let input = input_pixels(&layer);
        let _ = route_and_prefix(&layer, &input);
        let cpu = apply_compiled_effect_graph_rgba_f32(frame_view(&input), graph, layer.frame_seed)
            .expect("complete CPU Float32 mask graph");
        let (width, height) = (EXTENT.width() as usize, EXTENT.height() as usize);
        assert_eq!(
            cpu[width * height - 1],
            input[0],
            "outside the matte the base is unchanged"
        );
        assert!(
            cpu.iter().any(|pixel| pixel[0] > input[0][0] + 0.05),
            "covered HDR pixels must be graded"
        );
        assert!(
            cpu.iter().all(|pixel| pixel[3] == input[0][3]),
            "local grade preserves base coverage"
        );
        fingerprints.push(graph.semantic_fingerprint());
        frames.push(cpu);
    }
    assert_ne!(fingerprints[0], fingerprints[1]);
    assert!(
        frames[0].iter().zip(frames[1].iter()).any(|(a, b)| (a[0] - b[0]).abs() > 0.05),
        "the fractional field time must visibly move the cubic subtraction"
    );
}

#[test]
fn production_clip_mask_graph_wgpu_matches_cpu_f32() {
    if !gpu_availability::gpu_is_available("production_clip_mask_graph_wgpu_matches_cpu_f32") {
        return;
    }
    let context =
        pollster::block_on(GpuContext::new()).expect("initialize wgpu on an exposed adapter");
    let mut runtime = HeterogeneousGpuContinuationRuntime::new(
        context,
        GpuColorFrameWgpuResourcePoolOptions::default(),
    )
    .expect("create actual wgpu continuation runtime");
    let program = authored_program();
    let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig::default());
    for tick in SAMPLE_TICKS {
        let layer = sample(&program, tick, &mut session);
        let input = input_pixels(&layer);
        let expected = apply_compiled_effect_graph_rgba_f32(
            frame_view(&input),
            &layer.effect_graph,
            layer.frame_seed,
        )
        .expect("complete CPU Float32 reference at the same sample");
        let (route, prefix) = route_and_prefix(&layer, &input);
        let uploads = prefix.evidence().transfers().to_vec();
        let request = HeterogeneousGpuContinuationRequest::new(
            HeterogeneousGpuContinuationBinding::new(
                route.graph_fingerprint(),
                GENERATION,
                EXTENT,
                layer.frame_seed,
                WORKING_SPACE,
            ),
            HeterogeneousGpuResourceGrant::new(BUDGET_BYTES, BUDGET_BYTES, 64, BUDGET_BYTES),
        );
        let completed = runtime
            .execute_to_cpu(
                request,
                prefix,
                &ExecutionCancellationToken::new(),
                Instant::now() + Duration::from_secs(30),
                ImageSampling::UNIT_SQUARE,
            )
            .expect("submit, complete and read back actual GPU mask combines and local grade mix");
        let evidence = completed.evidence();
        let recorded = evidence.recorded();
        assert_eq!(recorded.graph_fingerprint(), route.graph_fingerprint());
        assert_eq!(recorded.frame_seed(), layer.frame_seed);
        assert_eq!(recorded.frame_extent(), EXTENT);
        assert_eq!(
            recorded.gpu_nodes(),
            route.prepared_work().gpu_suffix().node_ids()
        );
        assert_eq!(evidence.completed_uploads(), uploads);
        assert_eq!(
            recorded.upload_bytes(),
            u64::from(EXTENT.width()) * u64::from(EXTENT.height()) * 16 * 6
        );
        assert_eq!(
            recorded.recorded_device_materializations(),
            12,
            "six input textures and six dispatch outputs"
        );
        assert!(evidence.readback_bytes().is_some_and(|bytes| bytes > 0));
        let actual = &completed.frame().rgba_f32().data;
        assert_eq!(actual.len(), input.len());
        assert_eq!(expected.len(), input.len());
        for (index, (actual, expected)) in actual.iter().zip(expected.iter()).enumerate() {
            for channel in 0..4 {
                assert!(
                    (actual[channel] - expected[channel]).abs() <= 1.0e-5,
                    "tick {tick}, pixel {index}, channel {channel}: GPU={} CPU={}",
                    actual[channel],
                    expected[channel]
                );
            }
            assert!((actual[3] - input[index][3]).abs() <= 1.0e-6);
        }
    }
}
