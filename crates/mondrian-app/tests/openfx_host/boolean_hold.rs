//! Owned OFX Bool/Hold contract through the existing Product and RenderGraph paths.

use super::*;
use mondrian_core::{AnimatedProperty, InterpolationType, Keyframe, PropertyDescriptor, TimeScale};

const IDENTIFIER: &str = "uk.co.thefoundry.BasicGainPlugin";

fn hold_property(mut property: AnimatedProperty, endpoint: bool) -> AnimatedProperty {
    for (at, value) in [(TimelineTime::ZERO, false), (TimelineTime::ONE, endpoint)] {
        property
            .set_exact_keyframe(Keyframe::from_preset(
                at,
                PropertyValue::Bool(value),
                InterpolationType::Hold,
            ))
            .expect("canonical Bool Hold key");
    }
    property
}

fn curve(property: AnimatedProperty) -> OpenFxParameterBinding {
    OpenFxParameterBinding::curve(
        OpenFxParameterValue::Boolean(false),
        property,
        TimeScale::new(24, 1).expect("native frame grid"),
    )
}

fn timing(frame: f64) -> OpenFxRenderTiming {
    OpenFxRenderTiming {
        frame,
        frame_rate: 24.0,
        first_frame: -12.0,
        last_frame: 60.0,
    }
}

fn assert_pixels(output: &EffectFloatFrame, input: &EffectFloatFrame, enabled: bool) {
    assert_eq!(output.format(), input.format());
    assert_eq!(
        (output.width(), output.height()),
        (input.width(), input.height())
    );
    let gain = if enabled { 1.0 } else { 0.0 };
    assert_eq!(
        output.pixels(),
        input
            .pixels()
            .iter()
            .map(|pixel| pixel.map(|value| value * gain))
            .collect::<Vec<_>>()
    );
}

#[test]
#[ignore = "requires current Product and SessionProbe mode boolean_hold"]
fn product_worker_preserves_boolean_hold_animation_queries_and_graph_pixels() {
    let inspection = cpu_contract_probe("boolean_hold");
    let description = describe_openfx_filter(product_executable(), &inspection, IDENTIFIER)
        .expect("Bool capability and native metadata");
    let enabled = description
        .parameters
        .iter()
        .find(|parameter| parameter.name == "enabled")
        .expect("animatable Boolean");
    let fixed = description
        .parameters
        .iter()
        .find(|parameter| parameter.name == "fixed")
        .expect("nonanimatable Boolean");
    assert!(enabled.can_animate);
    assert!(!fixed.can_animate);
    for parameter in [enabled, fixed] {
        assert_eq!(
            parameter.default_value,
            OpenFxParameterValue::Boolean(false)
        );
        assert_eq!(
            parameter.preferred_interpolation,
            mondrian_core::automation::ParameterInterpolation::Hold
        );
        assert!(parameter.numeric_channels.is_empty() && parameter.choices.is_empty());
    }
    let kind = register_selected_openfx_filter(product_executable(), &inspection, IDENTIFIER)
        .expect("register real Filter");
    let mut effect = instantiate_effect_node(kind).expect("canonical effect");
    let property = effect
        .properties
        .iter()
        .find(|(_, property)| property.descriptor.display_name == enabled.label)
        .expect("native Bool author property")
        .1
        .clone();
    assert!(property.descriptor.schema.is_animatable);
    let first_property = hold_property(property.clone(), true);
    let first = curve(first_property.clone());
    let changed = curve(hold_property(property, false));
    let constant = OpenFxParameterBinding::constant(OpenFxParameterValue::Boolean(false));
    let sampled = OpenFxParameterBinding::current_frame(OpenFxParameterValue::Boolean(false));
    let input = cpu_contract_input();
    let original_pixels = input.pixels().to_vec();
    let cancellation = mondrian_core::ExecutionCancellationToken::new();
    let prepare = || {
        OpenFxFilterSession::prepare(
            product_executable(),
            &inspection,
            IDENTIFIER,
            input.width(),
            input.height(),
            input.format(),
            mondrian_effects::EffectProjectGeometry::new(
                input.width(),
                input.height(),
                mondrian_core::SampleAspectRatio::SQUARE,
            )
            .expect("explicit owner project geometry"),
            &mut checkpoint(&cancellation),
        )
        .expect("supervised native owner")
    };
    let mut session = prepare();
    let worker = session.worker_process_id();
    // Every current value is false. Only the off-time endpoint/source changes,
    // so the fixture's real InstanceChanged counts exercise revision invalidation.
    for binding in [
        &first, &first, &changed, &changed, &constant, &constant, &sampled, &sampled, &first,
    ] {
        let bindings = BTreeMap::from([
            ("enabled".to_owned(), binding.clone()),
            (
                "fixed".to_owned(),
                OpenFxParameterBinding::constant(OpenFxParameterValue::Boolean(true)),
            ),
        ]);
        let output = session
            .render(
                &timing(0.0),
                &bindings,
                &input,
                &mut checkpoint(&cancellation),
            )
            .expect("native Bool values/keys/animation/source revisions");
        assert_pixels(&output, &input, false);
        assert_eq!(session.worker_process_id(), worker);
        assert_eq!(input.pixels(), original_pixels);
    }
    session.shutdown().expect("immutable last curve survives native teardown");
    let mut session = prepare();
    let worker = session.worker_process_id();
    let bindings = BTreeMap::from([("enabled".to_owned(), first.clone())]);
    for frame in [-6.0, 0.0, 23.0, 24.0, 30.0, 6.0, 24.0] {
        let output = session
            .render(
                &timing(frame),
                &bindings,
                &input,
                &mut checkpoint(&cancellation),
            )
            .expect("native Hold queries across forward and reverse seeks");
        assert_pixels(&output, &input, frame >= 24.0);
        assert_eq!(session.worker_process_id(), worker);
        assert_eq!(input.pixels(), original_pixels);
    }
    session
        .shutdown()
        .expect("seek owner naturally exits under the original deadline");
    effect.properties.upsert(first_property);
    let authored = effect.properties.clone();
    let frame_context = EffectFrameContext::new(
        Rational::FPS_24,
        mondrian_effects::EffectProjectGeometry::new(3, 5, SampleAspectRatio::new(1, 1).unwrap())
            .expect("explicit owner project geometry"),
        TimelineTimeRange::new(
            TimelineTime::new(-1, 2).unwrap(),
            TimelineTime::new(3, 1).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
        std::slice::from_ref(&effect),
        &[],
        &[],
        &[],
        WorkingColorSpace::LinearRec709,
        frame_context,
        &LutPreparationCache::uncached(),
    )
    .expect("existing canonical graph preparation");
    for frame in [0, 23, 24, 6, 30] {
        let graph = program
            .evaluate(TimelineTime::new(frame, 24).unwrap())
            .expect("canonical Bool evaluation");
        let output = apply_compiled_effect_graph_rgba_f32(input.view(), &graph, 0)
            .expect("real native RenderGraph path");
        assert_pixels(&output, &input, frame >= 24);
        assert_eq!(effect.properties, authored);
        assert_eq!(input.pixels(), original_pixels);
    }
}

#[test]
#[ignore = "requires current Product, SessionProbe mode boolean_hold and a fresh action receipt directory"]
fn product_worker_rejects_curves_for_nonanimatable_boolean_parameters() {
    use std::io::Read;
    let directory = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_DESCRIBE_ACTION_RECEIPT_DIRECTORY")
            .expect("fresh action receipts"),
    );
    assert!(directory.is_absolute() && directory.is_dir());
    assert!(std::fs::read_dir(&directory).unwrap().next().is_none());
    let inspection = cpu_contract_probe("boolean_hold");
    let description = describe_openfx_filter(product_executable(), &inspection, IDENTIFIER)
        .expect("native metadata");
    let fixed = description
        .parameters
        .iter()
        .find(|parameter| parameter.name == "fixed")
        .unwrap();
    assert!(!fixed.can_animate);
    let property = hold_property(
        AnimatedProperty::from_descriptor(PropertyDescriptor::new(
            "native.refusal",
            "Refused curve",
            PropertyValue::Bool(false),
        )),
        true,
    );
    let bindings = BTreeMap::from([("fixed".to_owned(), curve(property))]);
    let before = bindings.clone();
    let input = cpu_contract_input();
    let original_pixels = input.pixels().to_vec();
    let cancellation = mondrian_core::ExecutionCancellationToken::new();
    let mut session = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        IDENTIFIER,
        input.width(),
        input.height(),
        input.format(),
        mondrian_effects::EffectProjectGeometry::new(
            input.width(),
            input.height(),
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut checkpoint(&cancellation),
    )
    .expect("actual worker admission precedes native instance creation");
    assert!(
        matches!(session.render(&timing(0.0), &bindings, &input, &mut checkpoint(&cancellation)), Err(OpenFxHostError::WorkerFailed(detail)) if detail == "native status 4")
    );
    assert_eq!(bindings, before);
    assert_eq!(input.pixels(), original_pixels);
    session.shutdown().expect("refused owner retires under the original deadline");
    let mut resident_rows = Vec::new();
    for entry in std::fs::read_dir(&directory).unwrap() {
        let entry = entry.unwrap();
        assert!(entry.file_type().unwrap().is_file() && !entry.file_type().unwrap().is_symlink());
        let mut bytes = Vec::new();
        std::fs::File::open(entry.path())
            .unwrap()
            .take(4097)
            .read_to_end(&mut bytes)
            .unwrap();
        assert!(
            !bytes.is_empty()
                && bytes.len() <= 4096
                && bytes.is_ascii()
                && bytes.last() == Some(&b'\n')
        );
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.lines().count() < 32);
        for (index, row) in text.lines().enumerate() {
            let fields = row.split_whitespace().collect::<Vec<_>>();
            assert_eq!(fields.len(), 9);
            assert_eq!(fields[0].parse::<usize>().unwrap(), index + 1);
            assert_eq!(fields[8], "complete=1");
            if fields[1..3] == ["D=0", "I=0"] {
                resident_rows.push(fields[3..].join(" "));
            }
        }
    }
    assert!(resident_rows
        .iter()
        .any(|row| row == "OfxActionDescribe return attempted=0 accepted=0 status=0 complete=1"));
    assert!(
        !resident_rows.iter().any(|row| row.starts_with("OfxActionCreateInstance ")
            || row.starts_with("OfxImageEffectActionRender ")),
        "nonanimatable curve is rejected before Create/Render"
    );
    assert!(resident_rows
        .iter()
        .any(|row| row == "OfxActionUnload return attempted=0 accepted=0 status=0 complete=1"));
}

#[test]
#[ignore = "requires current Product, SessionProbe mode boolean_hold and the existing schema5 close observation env"]
fn product_worker_zero_render_shutdown_observes_original_exit_deadline() {
    let inspection = cpu_contract_probe("boolean_hold");
    let format = mondrian_effects::EffectValueFormat::new(
        mondrian_effects::EffectWorkingPrecision::Float32,
        mondrian_effects::EffectColorDomain::SceneLinearRgb,
        mondrian_core::ColorFrameAlpha::StraightFloat,
    );
    let mut session = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        IDENTIFIER,
        3,
        5,
        format,
        mondrian_effects::EffectProjectGeometry::new(
            3,
            5,
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut || Ok(()),
    )
    .expect("same lazy Product session without native instance creation");
    let worker = session.worker_process_id();
    assert!(worker > 0);
    println!("boolean-hold-zero-render-worker-pid={worker}");
    // No Render: the ordinary session still owns its mappings and control pump.
    // This contrast cannot identify an exit cause or close any rendered failure.
    session.shutdown().expect("zero-render owner obeys the original exit deadline");
    assert_eq!(session.worker_process_id(), worker);
}
