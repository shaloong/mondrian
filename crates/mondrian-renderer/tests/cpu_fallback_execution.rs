use mondrian_core::{types::BlendMode, ColorEngine, WorkingColorSpace, WorkingRgbaF32Frame};
use mondrian_effects::{
    compile_reference_effect_graph, compile_reference_render_graph, EffectRenderGraph,
    EffectRenderOp, EffectRenderPlan,
};
use mondrian_renderer::{
    composite_timeline_elements_color_frame_with_diagnostics, CpuColorFrame,
    TimelineAdjustmentLayer, TimelineCompositeElement, TimelineCompositeOptions,
    TimelineCompositeScratch, TimelineCpuExecutionPolicy, TimelineCpuWorkingSetGrant,
    TimelineCrossDissolveLayer, TimelineEffectColorRuntime, TimelineMediaLayer,
    TimelineTransitionInput,
};
use std::sync::Arc;

const WIDTH: u32 = 640;
const HEIGHT: u32 = 480;
const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
static COLOR_ENGINE: ColorEngine = ColorEngine::mondrian_standard();

fn working_frame(color: [f32; 4]) -> CpuColorFrame {
    CpuColorFrame::working(WorkingRgbaF32Frame {
        width: WIDTH,
        height: HEIGHT,
        data: vec![color; WIDTH as usize * HEIGHT as usize],
        color_space: WorkingColorSpace::LinearRec709,
    })
}

fn identity_graph() -> Arc<mondrian_effects::CompiledEffectGraph> {
    compile_reference_render_graph(EffectRenderGraph::identity()).expect("compile identity graph")
}

fn adjustment_graph() -> Arc<mondrian_effects::CompiledEffectGraph> {
    compile_reference_effect_graph(&EffectRenderPlan {
        ops: vec![EffectRenderOp::ColorAdjust {
            exposure: 0.25,
            contrast: 1.0,
            saturation: 1.0,
            working_color_space: WorkingColorSpace::LinearRec709,
        }],
    })
    .expect("compile adjustment graph")
}

fn media<'a>(frame: &'a CpuColorFrame, opacity: f32) -> TimelineCompositeElement<'a> {
    TimelineCompositeElement::Media(TimelineMediaLayer {
        frame,
        opacity,
        blend_mode: BlendMode::Normal,
        transform: IDENTITY,
        effect_graph: identity_graph(),
        frame_seed: 0,
    })
}

fn runtime() -> TimelineEffectColorRuntime<'static> {
    TimelineEffectColorRuntime::new(&COLOR_ENGINE, WorkingColorSpace::LinearRec709)
}

#[test]
fn preview_export_cpu_seam_proves_parallel_simd_reuse_and_owned_adjustment() {
    let red = working_frame([0.8, 0.1, 0.05, 1.0]);
    let green = working_frame([0.05, 0.7, 0.1, 1.0]);
    let blue = working_frame([0.1, 0.2, 0.9, 1.0]);
    let mut scratch = TimelineCompositeScratch::default();
    scratch.reconfigure_cpu_execution(TimelineCpuExecutionPolicy::new(2, 1));

    let layered = composite_timeline_elements_color_frame_with_diagnostics(
        WIDTH,
        HEIGHT,
        &[media(&red, 1.0), media(&green, 0.6), media(&blue, 0.4)],
        TimelineCompositeOptions::default(),
        runtime(),
        &mut scratch,
    )
    .expect("parallel CPU fallback composite");
    assert!(layered.execution.owner_parallel_kernel_dispatches >= 1);

    let transition = TimelineCompositeElement::CrossDissolve(TimelineCrossDissolveLayer {
        left: TimelineTransitionInput::Media(match media(&red, 1.0) {
            TimelineCompositeElement::Media(layer) => layer,
            _ => unreachable!("fixture is media"),
        }),
        right: TimelineTransitionInput::Media(match media(&blue, 1.0) {
            TimelineCompositeElement::Media(layer) => layer,
            _ => unreachable!("fixture is media"),
        }),
        progress: 0.37,
    });
    let first_transition = composite_timeline_elements_color_frame_with_diagnostics(
        WIDTH,
        HEIGHT,
        std::slice::from_ref(&transition),
        TimelineCompositeOptions::default(),
        runtime(),
        &mut scratch,
    )
    .expect("first transition composite");
    assert_eq!(
        first_transition.execution.runtime_vectorized_pixels,
        u64::from(WIDTH) * u64::from(HEIGHT)
    );
    let expected_transition =
        mondrian_effects::mix_straight_rgba([0.8, 0.1, 0.05, 1.0], [0.1, 0.2, 0.9, 1.0], 0.37);
    for (actual, expected) in
        first_transition.frame.rgba_f32().data[0].iter().zip(expected_transition)
    {
        assert!(
            (*actual - expected).abs() <= 1.0e-6,
            "actual={actual}, expected={expected}"
        );
    }
    let second_transition = composite_timeline_elements_color_frame_with_diagnostics(
        WIDTH,
        HEIGHT,
        &[transition],
        TimelineCompositeOptions::default(),
        runtime(),
        &mut scratch,
    )
    .expect("second transition composite");
    assert_eq!(
        second_transition.execution.reused_transition_scratch_buffers,
        2
    );
    assert!(second_transition.execution.owner_parallel_kernel_dispatches >= 1);

    let adjustment = composite_timeline_elements_color_frame_with_diagnostics(
        WIDTH,
        HEIGHT,
        &[
            media(&green, 1.0),
            TimelineCompositeElement::Adjustment(TimelineAdjustmentLayer {
                effect_graph: adjustment_graph(),
                opacity: 0.75,
                blend_mode: Some(BlendMode::Normal),
                frame_seed: 7,
            }),
        ],
        TimelineCompositeOptions::default(),
        runtime(),
        &mut scratch,
    )
    .expect("owned adjustment composite");
    assert_eq!(adjustment.execution.owned_adjustment_base_reuses, 1);
}

#[test]
fn optimized_cpu_execution_matches_serial_reference_pixels() {
    let left = working_frame([0.8, 0.1, 0.05, 1.0]);
    let right = working_frame([0.05, 0.2, 0.9, 1.0]);
    let elements = [media(&left, 1.0), media(&right, 0.37), media(&left, 0.21)];

    let mut serial = TimelineCompositeScratch::default();
    serial.reconfigure_cpu_execution(TimelineCpuExecutionPolicy::new(1, 1));
    let expected = composite_timeline_elements_color_frame_with_diagnostics(
        WIDTH,
        HEIGHT,
        &elements,
        TimelineCompositeOptions::default(),
        runtime(),
        &mut serial,
    )
    .expect("serial reference");

    let mut parallel = TimelineCompositeScratch::default();
    parallel.reconfigure_cpu_execution(TimelineCpuExecutionPolicy::new(2, 1));
    let actual = composite_timeline_elements_color_frame_with_diagnostics(
        WIDTH,
        HEIGHT,
        &elements,
        TimelineCompositeOptions::default(),
        runtime(),
        &mut parallel,
    )
    .expect("parallel execution");

    assert_eq!(actual.frame.rgba_f32().data, expected.frame.rgba_f32().data);
    assert!(actual.execution.owner_parallel_kernel_dispatches >= 1);
    assert_eq!(expected.execution.owner_parallel_kernel_dispatches, 0);
}

#[test]
fn optional_transition_reuse_never_bypasses_the_retained_scratch_grant() {
    let left = working_frame([0.8, 0.1, 0.05, 1.0]);
    let right = working_frame([0.05, 0.2, 0.9, 1.0]);
    let left = match media(&left, 1.0) {
        TimelineCompositeElement::Media(layer) => layer,
        _ => unreachable!("fixture is media"),
    };
    let right = match media(&right, 1.0) {
        TimelineCompositeElement::Media(layer) => layer,
        _ => unreachable!("fixture is media"),
    };
    let mut scratch = TimelineCompositeScratch::default();
    scratch.reconfigure_cpu_working_set(TimelineCpuWorkingSetGrant {
        max_active_bytes: u64::MAX,
        max_retained_scratch_bytes: 0,
    });

    composite_timeline_elements_color_frame_with_diagnostics(
        WIDTH,
        HEIGHT,
        &[TimelineCompositeElement::CrossDissolve(
            TimelineCrossDissolveLayer {
                left: TimelineTransitionInput::Media(left),
                right: TimelineTransitionInput::Media(right),
                progress: 0.5,
            },
        )],
        TimelineCompositeOptions::default(),
        runtime(),
        &mut scratch,
    )
    .expect("transition executes within the active-frame grant");

    assert_eq!(
        scratch.cpu_working_set_diagnostics().retained_scratch_bytes,
        0
    );
}
