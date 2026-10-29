//! Independent source-free CPU Generator through the actual product child.
use super::*;
use mondrian_app::openfx_effect::register_selected_openfx_generator;
use mondrian_app::openfx_host::describe_openfx_generator;
use mondrian_core::{ColorFrameAlpha, ExecutionCancellationToken};
use mondrian_effects::{
    EffectColorDomain, EffectExecutionSession, EffectExecutionSessionConfig, EffectFloatFrame,
    EffectFrameExtent, EffectGraphNodeKind, EffectProcessorControl, EffectValueFormat,
    EffectWorkingPrecision,
};

const IDENTIFIER: &str = "org.mondrian.fixture.ofx-generator";
fn format() -> EffectValueFormat {
    EffectValueFormat::new(
        EffectWorkingPrecision::Float32,
        EffectColorDomain::SceneLinearRgb,
        ColorFrameAlpha::StraightFloat,
    )
}
fn inspection() -> mondrian_app::openfx_adapter::OpenFxBinaryInspection {
    let binary = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_GENERATOR_BINARY").expect("GeneratorProbe binary"),
    );
    inspect_openfx_binary(product_executable(), &binary)
        .expect("actual supervised Generator discovery")
}
fn timing(frame: f64) -> OpenFxRenderTiming {
    OpenFxRenderTiming {
        frame,
        frame_rate: 24.0,
        first_frame: 0.0,
        last_frame: 12.0,
    }
}
fn literal(output: &EffectFloatFrame, frame: f32, gain: f32) {
    assert_eq!(output.extent(), EffectFrameExtent::new(3, 2));
    assert_eq!(output.format(), format());
    for (index, actual) in output.pixels().iter().enumerate() {
        let x = index % 3;
        let native_y = 1 - index / 3;
        let expected = [
            gain,
            (x + 1) as f32 / 16.0,
            (native_y + 1) as f32 / 8.0 + frame / 64.0,
            0.75,
        ];
        assert_eq!(
            actual.map(f32::to_bits),
            expected.map(f32::to_bits),
            "native coordinate/time/parameter literal at {index}"
        );
    }
}

#[test]
#[ignore = "requires normal Product and MONDRIAN_OPENFX_RECTANGLE_BINARY=official Rectangle.ofx.bundle binary"]
fn official_rectangle_generator_preserves_geometry_parameters_and_output_signal() {
    use mondrian_effects::{
        instantiate_effect_node_with_project_geometry, EffectProcessorExecutionOrigin,
        EffectProjectGeometry,
    };

    let binary = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_RECTANGLE_BINARY").expect("official Rectangle reference"),
    );
    let inspection = inspect_openfx_binary(product_executable(), &binary).expect("real discovery");
    let identifier = "uk.co.thefoundry.GeneratorExample";
    let description = describe_openfx_generator(product_executable(), &inspection, identifier)
        .expect("independently selected Output-only context");
    let kind = register_selected_openfx_generator(product_executable(), &inspection, identifier)
        .expect("native Generator registration");
    let (width, height) = (8, 6);
    let par = SampleAspectRatio::new(2, 1).unwrap();
    let geometry = EffectProjectGeometry::new(width, height, par).unwrap();
    let mut effect = instantiate_effect_node_with_project_geometry(kind, geometry)
        .expect("canonical project-relative parameter initialization");
    let context = EffectFrameContext::new(
        Rational::FPS_24,
        geometry,
        TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::new(13, 24).unwrap()).unwrap(),
    )
    .unwrap();
    // Explicit canonical corners and colours, independent of project or SDK
    // defaults. At PAR2, canonical X coordinates are twice raster X coordinates.
    let cases = [
        (
            [2.0, 1.0],
            [10.0, 4.0],
            [0.25, 0.5, 0.75, 1.0],
            [1, 1, 5, 4],
        ),
        (
            [4.0, 2.0],
            [14.0, 5.0],
            [0.5, 0.25, 0.125, 0.5],
            [2, 2, 7, 5],
        ),
        (
            [2.0, 1.0],
            [10.0, 4.0],
            [0.25, 0.5, 0.75, 0.0],
            [1, 1, 5, 4],
        ),
    ];
    let mut programs = Vec::new();
    for (corner1, corner2, colour, _) in cases {
        for (name, value) in [
            ("corner1", PropertyValue::DoubleVec2(corner1)),
            ("corner2", PropertyValue::DoubleVec2(corner2)),
            ("colour", PropertyValue::DoubleColor(colour)),
        ] {
            // Resolve the actual descriptor label rather than duplicating the
            // adapter's private native-name-to-ParameterId encoding.
            let native = description.parameters.iter().find(|p| p.name == name).unwrap();
            let mut matches = effect
                .properties
                .iter()
                .filter(|(_, p)| p.descriptor.display_name == native.label);
            let id =
                matches.next().expect("authored parameter").1.descriptor.parameter_id().clone();
            assert!(
                matches.next().is_none(),
                "reference labels must be unambiguous"
            );
            effect
                .set_static_value_by_parameter(&id, value)
                .expect("canonical parameter edit");
        }
        programs.push(
            PreparedEffectProgram::prepare_hierarchical_with_frame_context(
                &[effect.clone()],
                &[],
                &[],
                &[],
                WorkingColorSpace::LinearRec709,
                context,
                &LutPreparationCache::uncached(),
            )
            .expect("source-free native program"),
        );
    }
    let author = serde_json::to_value(&effect).unwrap();
    let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
        max_processor_instances: 1,
        max_processor_bytes: 512 * 1024 * 1024,
        ..EffectExecutionSessionConfig::uncached(512 * 1024 * 1024)
    });
    let mut identity = None;
    let mut invocation = 0;
    #[cfg(target_os = "linux")]
    let mut process_start = None;
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for (frame, case) in [(0, 0), (6, 1), (3, 0), (9, 2), (6, 1)] {
            let graph = programs[case].evaluate(TimelineTime::new(frame, 24).unwrap()).unwrap();
            assert!(graph.node_profiles().keys().all(|id| !matches!(
                graph.graph().node(*id).unwrap().kind,
                EffectGraphNodeKind::Source
            )));
            let mut control = EffectProcessorControl::uncancelled();
            let output = session
                .generate_compiled_rgba_f32(
                    EffectFrameExtent::new(width, height),
                    format(),
                    cpu_contract_sampling(par.to_f64()),
                    &graph,
                    -999,
                    &mut control,
                )
                .expect("actual Output-only native Render");
            assert_eq!(output.extent(), EffectFrameExtent::new(width, height));
            assert_eq!(output.format().alpha(), ColorFrameAlpha::PremultipliedFloat);
            assert_eq!(output.sampling(), cpu_contract_sampling(par.to_f64()));
            let (_, _, colour, [x1, y1, x2, y2]) = cases[case];
            for (index, actual) in output.pixels().iter().enumerate() {
                let x = index as u32 % width;
                let y = height - 1 - index as u32 / width;
                // This SDK example declares PM output and emits literal RGBA
                // parameter values without further scaling. Preserve that
                // producer signal, including nonzero RGB at zero alpha; do not
                // infer its colour-picker semantics from a host pixel result.
                let expected = if x >= x1 && x < x2 && y >= y1 && y < y2 {
                    colour.map(|v| v as f32)
                } else {
                    [0.0; 4]
                };
                assert_eq!(
                    actual.map(f32::to_bits),
                    expected.map(f32::to_bits),
                    "frame{frame} pixel{index}"
                );
            }
            let batch = control.take_feedback().expect("actual native invocation");
            assert_eq!(batch.len(), 1);
            let receipt = batch.iter().next().unwrap();
            assert_eq!(
                receipt.preparation.input_role,
                mondrian_effects::EffectFloatProcessorInputRole::NoImage
            );
            let observed = (
                receipt.feedback.owner,
                receipt.feedback.origin,
                receipt.feedback.snapshot.native_owner(),
            );
            if let Some(expected) = identity {
                assert_eq!(
                    observed, expected,
                    "parameter/Seek changes retain one native owner"
                );
            } else {
                identity = Some(observed);
                let EffectProcessorExecutionOrigin::SupervisedChild { process_id } =
                    receipt.feedback.origin
                else {
                    panic!("real reference must execute in Product child");
                };
                #[cfg(target_os = "linux")]
                {
                    let pid = process_id.get();
                    assert_selected_product_worker_image(pid);
                    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
                    let start = stat
                        .rsplit_once(')')
                        .unwrap()
                        .1
                        .split_whitespace()
                        .nth(19)
                        .unwrap()
                        .to_owned();
                    process_start = Some((pid, start));
                }
                #[cfg(not(target_os = "linux"))]
                let _ = process_id;
            }
            assert!(receipt.feedback.snapshot.invocation() > invocation);
            invocation = receipt.feedback.snapshot.invocation();
            assert_eq!(session.diagnostics().processor_instances, 1);
            assert_eq!(session.diagnostics().cache_entries, 0);
        }
    }));
    session.retire_processor_resources();
    assert_eq!(session.diagnostics().processor_instances, 0);
    drop(session);
    if let Err(panic) = target {
        std::panic::resume_unwind(panic);
    }
    assert_eq!(serde_json::to_value(&effect).unwrap(), author);
    #[cfg(target_os = "linux")]
    if let Some((pid, start)) = process_start {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(error) if matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH)) => {}
            Ok(stat) => assert_ne!(
                stat.rsplit_once(')').unwrap().1.split_whitespace().nth(19).unwrap(),
                start,
                "original native child must be reaped"
            ),
            Err(error) => panic!("native retirement observation failed: {error}"),
        }
    }
}

#[test]
#[ignore = "requires rebuilt normal product and MONDRIAN_OPENFX_GENERATOR_BINARY=GeneratorProbe; no fault mode"]
fn output_only_generator_direct_and_prepared_graph_preserve_time_extent_and_owner() {
    assert!(std::env::var_os("MONDRIAN_OPENFX_GENERATOR_MODE").is_none());
    generator_direct_and_prepared_graph_preserve_time_extent_and_owner();
}

#[test]
#[ignore = "requires rebuilt product and GeneratorProbe with MONDRIAN_OPENFX_GENERATOR_MODE=optional_inputs"]
fn optional_inputs_generator_direct_and_prepared_graph_keep_inputs_disconnected() {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_GENERATOR_MODE").expect("mode"),
        "optional_inputs"
    );
    generator_direct_and_prepared_graph_preserve_time_extent_and_owner();
}

fn generator_direct_and_prepared_graph_preserve_time_extent_and_owner() {
    let inspection = inspection();
    let description = describe_openfx_generator(product_executable(), &inspection, IDENTIFIER)
        .expect("Generator descriptor");
    assert_eq!(description.parameters.len(), 1);
    assert_eq!(description.parameters[0].name, "gain");
    assert_eq!(
        description.parameters[0].default_value,
        OpenFxParameterValue::Double(0.5)
    );
    // A Generator-only class cannot be admitted by the unchanged Filter API.
    assert!(describe_openfx_filter(product_executable(), &inspection, IDENTIFIER).is_err());
    let token = ExecutionCancellationToken::new();
    let mut direct = OpenFxFilterSession::prepare_generator(
        product_executable(),
        &inspection,
        IDENTIFIER,
        3,
        2,
        format(),
        mondrian_effects::EffectProjectGeometry::new(
            3,
            2,
            mondrian_core::SampleAspectRatio::new(2, 1).unwrap(),
        )
        .expect("explicit owner project geometry"),
        &mut checkpoint(&token),
    )
    .expect("one Generator owner");
    let pid = direct.worker_process_id();
    let owner = direct.processor_owner();
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for (frame, gain) in [(0.0, 0.25), (6.0, 0.5), (3.0, 0.25), (6.0, 0.5)] {
            let parameters =
                constant_parameters([("gain".to_owned(), OpenFxParameterValue::Double(gain))]);
            let output = direct
                .generate(
                    &timing(frame),
                    cpu_contract_sampling(2.0),
                    &parameters,
                    &mut checkpoint(&token),
                )
                .expect("actual no-input Generator Render");
            literal(&output, frame as f32, gain as f32);
            assert_eq!(direct.worker_process_id(), pid);
            assert_eq!(direct.processor_owner(), owner);
        }
        let mut invalid = timing(0.0);
        invalid.frame = 13.0;
        assert!(matches!(
            direct.generate(
                &invalid,
                cpu_contract_sampling(2.0),
                &BTreeMap::new(),
                &mut checkpoint(&token)
            ),
            Err(OpenFxHostError::Invalid(_))
        ));
        assert_eq!(
            direct.worker_process_id(),
            pid,
            "invalid metadata does not replace original owner"
        );
    }));
    let shutdown = direct.shutdown();
    drop(direct);
    if let Err(panic) = target {
        std::panic::resume_unwind(panic);
    }
    shutdown.expect("original bounded Generator shutdown");

    let kind = register_selected_openfx_generator(product_executable(), &inspection, IDENTIFIER)
        .expect("Generator graph definition");
    let mut effect = instantiate_effect_node(kind).expect("source occurrence");
    let gain_id = effect
        .properties
        .iter()
        .find(|(_, p)| p.descriptor.display_name == "Gain")
        .expect("gain")
        .1
        .descriptor
        .parameter_id()
        .clone();
    effect
        .set_static_value_by_parameter(&gain_id, PropertyValue::Double(0.25))
        .expect("canonical gain");
    let frame_context = EffectFrameContext::new(
        Rational::FPS_24,
        mondrian_effects::EffectProjectGeometry::new(3, 2, SampleAspectRatio::new(2, 1).unwrap())
            .expect("explicit owner project geometry"),
        TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::new(13, 24).unwrap()).unwrap(),
    )
    .unwrap();
    let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
        &[effect.clone()],
        &[],
        &[],
        &[],
        WorkingColorSpace::LinearRec709,
        frame_context,
        &LutPreparationCache::uncached(),
    )
    .expect("real source-free PreparedEffectProgram");
    effect
        .set_static_value_by_parameter(&gain_id, PropertyValue::Double(0.5))
        .expect("canonical changed gain");
    let changed = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
        &[effect],
        &[],
        &[],
        &[],
        WorkingColorSpace::LinearRec709,
        frame_context,
        &LutPreparationCache::uncached(),
    )
    .expect("same occurrence with changed canonical parameters");
    let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
        max_processor_instances: 1,
        max_processor_bytes: 512 * 1024 * 1024,
        ..EffectExecutionSessionConfig::uncached(512 * 1024 * 1024)
    });
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut actual_native_identity = None;
        let mut invocation = 0;
        for (frame, gain, program) in [
            (0, 0.25, &program),
            (6, 0.5, &changed),
            (3, 0.25, &program),
            (6, 0.5, &changed),
        ] {
            let graph = program
                .evaluate(TimelineTime::new(frame, 24).unwrap())
                .expect("exact current frame");
            // Public profiles cover exactly the compiled reachable nodes; the
            // executor's private schedule stays inside mondrian-effects.
            assert!(graph.node_profiles().keys().all(|id| !matches!(
                graph.graph().node(*id).unwrap().kind,
                EffectGraphNodeKind::Source
            )));
            assert!(graph.node_profiles().keys().any(|id| matches!(
                graph.graph().node(*id).unwrap().kind,
                EffectGraphNodeKind::CustomSource { .. }
            )));
            let mut control = EffectProcessorControl::uncancelled();
            let output = session
                .generate_compiled_rgba_f32(
                    EffectFrameExtent::new(3, 2),
                    format(),
                    cpu_contract_sampling(2.0),
                    &graph,
                    -999,
                    &mut control,
                )
                .expect("same-bank zero-input graph");
            literal(&output, frame as f32, gain);
            let batch = control.take_feedback().expect("actual source feedback");
            assert_eq!(batch.len(), 1);
            let receipt = batch.iter().next().unwrap();
            assert_eq!(
                receipt.preparation.input_role,
                mondrian_effects::EffectFloatProcessorInputRole::NoImage
            );
            let identity = (
                receipt.feedback.owner,
                receipt.feedback.origin,
                receipt.feedback.snapshot.native_owner(),
            );
            assert!(matches!(
                receipt.feedback.origin,
                mondrian_effects::EffectProcessorExecutionOrigin::SupervisedChild { .. }
            ));
            assert_ne!(
                receipt.feedback.owner, owner,
                "direct and Graph have separate original owners"
            );
            if let Some(expected) = actual_native_identity {
                assert_eq!(identity, expected);
            } else {
                actual_native_identity = Some(identity);
            }
            assert!(receipt.feedback.snapshot.invocation() > invocation);
            invocation = receipt.feedback.snapshot.invocation();
            assert_eq!(session.diagnostics().processor_instances, 1);
            assert_eq!(session.diagnostics().cache_entries, 0);
        }
    }));
    session.retire_processor_resources();
    assert_eq!(session.diagnostics().processor_instances, 0);
    drop(session);
    if let Err(panic) = target {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[ignore = "requires rebuilt product and GeneratorProbe with MONDRIAN_OPENFX_GENERATOR_MODE=required_source or required_aux; run each separately"]
fn generator_with_required_input_is_rejected_before_processing() {
    let mode = std::env::var("MONDRIAN_OPENFX_GENERATOR_MODE").expect("mode");
    assert!(matches!(mode.as_str(), "required_source" | "required_aux"));
    let actions = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_GENERATOR_ACTION_LOG").expect("fresh action log"),
    );
    assert!(!actions.exists(), "action log must start absent");
    let inspection = inspection();
    let description = describe_openfx_generator(product_executable(), &inspection, IDENTIFIER);
    let token = ExecutionCancellationToken::new();
    let mut session = OpenFxFilterSession::prepare_generator(
        product_executable(),
        &inspection,
        IDENTIFIER,
        3,
        2,
        format(),
        mondrian_effects::EffectProjectGeometry::new(3, 2, SampleAspectRatio::new(2, 1).unwrap())
            .expect("explicit owner project geometry"),
        &mut checkpoint(&token),
    )
    .expect("transport owner; native instance admission is lazy");
    let result = session.generate(
        &timing(0.0),
        cpu_contract_sampling(2.0),
        &BTreeMap::new(),
        &mut checkpoint(&token),
    );
    let shutdown = session.shutdown();
    drop(session);
    assert!(
        matches!(&description, Err(OpenFxHostError::WorkerFailed(detail)) if detail == "native status 7: "),
        "required input must fail Describe: {description:?}"
    );
    assert!(
        matches!(&result, Err(OpenFxHostError::WorkerFailed(detail)) if detail == "native status 7"),
        "required input must fail the first native request: {result:?}"
    );
    shutdown.expect("original bounded refused-owner shutdown");
    let actions = std::fs::read_to_string(actions).expect("actual plugin actions");
    for expected in [
        "OfxActionLoad",
        "OfxImageEffectActionDescribeInContext",
        "OfxActionUnload",
    ] {
        assert!(
            actions.lines().filter(|line| *line == expected).count() >= 2,
            "Describe and native request must both reach {expected}: {actions}"
        );
    }
    assert!(
        !actions.lines().any(|line| matches!(
            line,
            "OfxActionCreateInstance" | "OfxImageEffectActionRender"
        )),
        "required input must be refused before instance creation or rendering: {actions}"
    );
}

#[test]
#[ignore = "requires rebuilt product and GeneratorProbe with MONDRIAN_OPENFX_GENERATOR_MODE=no_output_fetch"]
fn output_only_generator_cannot_publish_success_without_current_output_fetch() {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_GENERATOR_MODE").expect("mode"),
        "no_output_fetch"
    );
    let inspection = inspection();
    let token = ExecutionCancellationToken::new();
    let mut session = OpenFxFilterSession::prepare_generator(
        product_executable(),
        &inspection,
        IDENTIFIER,
        3,
        2,
        format(),
        mondrian_effects::EffectProjectGeometry::new(
            3,
            2,
            mondrian_core::SampleAspectRatio::new(2, 1).unwrap(),
        )
        .expect("explicit owner project geometry"),
        &mut checkpoint(&token),
    )
    .expect("Generator owner");
    let result = session.generate(
        &timing(3.0),
        cpu_contract_sampling(2.0),
        &BTreeMap::new(),
        &mut checkpoint(&token),
    );
    let shutdown = session.shutdown();
    drop(session);
    assert!(
        matches!(result, Err(OpenFxHostError::WorkerFailed(_))),
        "unfetched Output is never a published frame: {result:?}"
    );
    shutdown.expect("original faulted owner cleanup");
}
