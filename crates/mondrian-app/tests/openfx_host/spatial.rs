//! Project-bound native defaults; contextual author templates remain explicit.
use super::*;
use mondrian_app::openfx_host::{describe_openfx_generator, OpenFxSpatialDefaults};
use mondrian_core::{ColorFrameAlpha, ExecutionCancellationToken};
use mondrian_effects::{EffectColorDomain, EffectValueFormat, EffectWorkingPrecision};

fn format(alpha: ColorFrameAlpha) -> EffectValueFormat {
    EffectValueFormat::new(
        EffectWorkingPrecision::Float32,
        EffectColorDomain::SceneLinearRgb,
        alpha,
    )
}

fn timing(frame: f64) -> OpenFxRenderTiming {
    OpenFxRenderTiming {
        frame,
        frame_rate: 24.0,
        first_frame: 0.0,
        last_frame: 12.0,
    }
}

#[test]
#[ignore = "requires rebuilt Product/GeneratorProbe and MONDRIAN_OPENFX_GENERATOR_MODE=spatial_defaults"]
fn spatial_defaults_resolve_once_before_create_and_range_admission() {
    let actions = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_GENERATOR_ACTION_LOG").expect("fresh action log"),
    );
    assert!(!actions.exists());
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_GENERATOR_MODE").unwrap(),
        "spatial_defaults"
    );
    let binary = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_GENERATOR_BINARY").unwrap());
    let inspection = inspect_openfx_binary(product_executable(), &binary).expect("discovery");
    let identifier = "org.mondrian.fixture.ofx-generator";
    let description = describe_openfx_generator(product_executable(), &inspection, identifier)
        .expect("spatial descriptor");
    let position = description.parameters.iter().find(|p| p.name == "position").unwrap();
    assert_eq!(position.double_type, Some(OpenFxDoubleType::XyAbsolute));
    assert_eq!(
        position.spatial_defaults,
        Some(OpenFxSpatialDefaults::Normalised)
    );
    assert_eq!(
        position.default_value,
        OpenFxParameterValue::Double2([0.5, 0.25])
    );
    assert_eq!(
        position.numeric_channels[0].minimum, 1.0,
        "raw seed is outside the canonical bound"
    );
    let canonical = description.parameters.iter().find(|p| p.name == "canonical").unwrap();
    assert_eq!(canonical.double_type, Some(OpenFxDoubleType::Xy));
    assert_eq!(
        canonical.spatial_defaults,
        Some(OpenFxSpatialDefaults::Canonical)
    );
    let error = mondrian_app::openfx_effect::register_selected_openfx_generator(
        product_executable(),
        &inspection,
        identifier,
    )
    .expect_err("no unresolved defaults in global author registry");
    assert!(error.to_string().contains("owner project geometry"));
    let token = ExecutionCancellationToken::new();
    for (width, height) in [(3, 2), (5, 4)] {
        let geometry = mondrian_effects::EffectProjectGeometry::new(
            width,
            height,
            SampleAspectRatio::new(2, 1).unwrap(),
        )
        .unwrap();
        let project = geometry.canonical_size();
        let mut session = OpenFxFilterSession::prepare_generator(
            product_executable(),
            &inspection,
            identifier,
            3,
            2,
            format(ColorFrameAlpha::StraightFloat),
            geometry,
            &mut checkpoint(&token),
        )
        .expect("transport owner");
        let pid = session.worker_process_id();
        let owner = session.processor_owner();
        let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for (frame, explicit_seed) in [(0.0, false), (6.0, true), (3.0, false)] {
                let parameters = if explicit_seed {
                    constant_parameters([(
                        "size".into(),
                        OpenFxParameterValue::Double2([1.0, 1.0]),
                    )])
                } else {
                    BTreeMap::new()
                };
                let output = session
                    .generate(
                        &timing(frame),
                        cpu_contract_sampling(2.0),
                        &parameters,
                        &mut checkpoint(&token),
                    )
                    .expect("actual spatial Generate");
                let size = if explicit_seed { [1.0, 1.0] } else { project };
                let expected = [
                    size[0] as f32,
                    size[1] as f32,
                    (project[0] * 0.5 + project[1] * 0.25 / 16.0 + 4.0 / 128.0 + 5.0 / 1024.0)
                        as f32,
                    0.75,
                ];
                assert_eq!(output.format(), format(ColorFrameAlpha::StraightFloat));
                assert_eq!(output.pixels().len(), 6);
                for actual in output.pixels() {
                    assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
                }
                assert_eq!(session.worker_process_id(), pid);
                assert_eq!(session.processor_owner(), owner);
            }
        }));
        let shutdown = session.shutdown();
        drop(session);
        if let Err(panic) = target {
            std::panic::resume_unwind(panic);
        }
        shutdown.expect("original bounded spatial owner shutdown");
    }
    // Resolving 0.5 against a one-pixel project stays below hard minimum1.
    // It must fail native construction rather than clamp or substitute a seed.
    let before = std::fs::read_to_string(&actions).expect("actual successful actions").len();
    let geometry =
        mondrian_effects::EffectProjectGeometry::new(1, 1, SampleAspectRatio::new(1, 1).unwrap())
            .unwrap();
    let mut rejected = OpenFxFilterSession::prepare_generator(
        product_executable(),
        &inspection,
        identifier,
        3,
        2,
        format(ColorFrameAlpha::StraightFloat),
        geometry,
        &mut checkpoint(&token),
    )
    .expect("lazy transport owner");
    let result = rejected.generate(
        &timing(0.0),
        cpu_contract_sampling(2.0),
        &BTreeMap::new(),
        &mut checkpoint(&token),
    );
    let shutdown = rejected.shutdown();
    drop(rejected);
    assert!(
        matches!(result, Err(OpenFxHostError::WorkerFailed(_))),
        "invalid resolved default must not publish: {result:?}"
    );
    shutdown.expect("original bounded rejected-default shutdown");
    let actions = std::fs::read_to_string(actions).expect("actual rejected-instance actions");
    let rejected = &actions[before..];
    assert!(rejected.lines().any(|line| line == "OfxImageEffectActionDescribeInContext"));
    assert!(rejected.lines().any(|line| line == "OfxActionUnload"));
    assert!(
        !rejected.lines().any(|line| matches!(
            line,
            "OfxActionCreateInstance" | "OfxImageEffectActionRender"
        )),
        "resolved bounds must be admitted before plugin callbacks: {rejected}"
    );
}

#[test]
#[ignore = "requires rebuilt Product and published Natron-2.3.16-beta3 Misc.ofx via MONDRIAN_OPENFX_INVERT_BINARY"]
fn published_constant_generator_preserves_float_pixels_and_resident_owner() {
    assert!(std::env::var_os("MONDRIAN_OPENFX_GENERATOR_MODE").is_none());
    let binary = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_INVERT_BINARY").unwrap());
    let inspection =
        inspect_openfx_binary(product_executable(), &binary).expect("published discovery");
    let identifier = "net.sf.openfx.ConstantPlugin";
    let description = describe_openfx_generator(product_executable(), &inspection, identifier)
        .expect("published Generator descriptor");
    for name in ["bottomLeft", "size"] {
        assert_eq!(
            description.parameters.iter().find(|p| p.name == name).unwrap().spatial_defaults,
            Some(OpenFxSpatialDefaults::Normalised)
        );
    }
    let checker = describe_openfx_generator(
        product_executable(),
        &inspection,
        "net.sf.openfx.CheckerBoardPlugin",
    )
    .expect("CheckerBoard metadata only");
    assert_eq!(
        checker
            .parameters
            .iter()
            .find(|p| p.name == "boxSize")
            .unwrap()
            .spatial_defaults,
        Some(OpenFxSpatialDefaults::Canonical)
    );
    let geometry =
        mondrian_effects::EffectProjectGeometry::new(3, 2, SampleAspectRatio::new(2, 1).unwrap())
            .unwrap();
    let token = ExecutionCancellationToken::new();
    let mut session = OpenFxFilterSession::prepare_generator(
        product_executable(),
        &inspection,
        identifier,
        3,
        2,
        format(ColorFrameAlpha::PremultipliedFloat),
        geometry,
        &mut checkpoint(&token),
    )
    .expect("actual published Generator owner");
    let pid = session.worker_process_id();
    let owner = session.processor_owner();
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for (frame, color) in [
            (0.0, [0.25, 0.5, 0.75, 1.0]),
            (6.0, [-0.125, 1.5, 0.25, 1.0]),
            (3.0, [0.5, 0.25, 0.125, 1.0]),
            (6.0, [-0.125, 1.5, 0.25, 1.0]),
        ] {
            let parameters =
                constant_parameters([("color".into(), OpenFxParameterValue::Rgba(color))]);
            let output = session
                .generate(
                    &timing(frame),
                    cpu_contract_sampling(2.0),
                    &parameters,
                    &mut checkpoint(&token),
                )
                .expect("published actual CPU Render");
            assert_eq!(output.format(), format(ColorFrameAlpha::PremultipliedFloat));
            assert_eq!(output.pixels().len(), 6);
            for actual in output.pixels() {
                assert_eq!(
                    actual.map(f32::to_bits),
                    color.map(|x| (x as f32).to_bits())
                );
            }
            assert_eq!(session.worker_process_id(), pid);
            assert_eq!(session.processor_owner(), owner);
        }
    }));
    let shutdown = session.shutdown();
    drop(session);
    if let Err(panic) = target {
        std::panic::resume_unwind(panic);
    }
    shutdown.expect("original bounded published Generator shutdown");
}
