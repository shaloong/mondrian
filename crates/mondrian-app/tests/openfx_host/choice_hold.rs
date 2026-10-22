//! Owned OFX Choice/Hold contract through the existing Product and RenderGraph paths.

use super::*;
use mondrian_core::{AnimatedProperty, InterpolationType, Keyframe, PropertyDescriptor, TimeScale};

const IDENTIFIER: &str = "uk.co.thefoundry.BasicGainPlugin";

fn hold_property(mut property: AnimatedProperty, endpoint: i32) -> AnimatedProperty {
    for (at, value) in [(TimelineTime::ZERO, 1), (TimelineTime::ONE, endpoint)] {
        property
            .set_exact_keyframe(Keyframe::from_preset(
                at,
                PropertyValue::Enum(value.to_string()),
                InterpolationType::Hold,
            ))
            .expect("canonical Choice Hold key");
    }
    property
}

fn curve(property: AnimatedProperty) -> OpenFxParameterBinding {
    OpenFxParameterBinding::curve(
        OpenFxParameterValue::Choice(1),
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

fn assert_pixels(output: &EffectFloatFrame, input: &EffectFloatFrame, index: i32) {
    assert_eq!(output.format(), input.format());
    assert_eq!(
        (output.width(), output.height()),
        (input.width(), input.height())
    );
    let gain = (index + 1) as f32;
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
#[ignore = "requires current Product and SessionProbe mode choice_hold"]
fn product_worker_preserves_choice_hold_indices_queries_and_graph_pixels() {
    let inspection = cpu_contract_probe("choice_hold");
    let description = describe_openfx_filter(product_executable(), &inspection, IDENTIFIER)
        .expect("Choice capability and native metadata");
    let enabled = description
        .parameters
        .iter()
        .find(|parameter| parameter.name == "mode")
        .expect("animatable Choice");
    let fixed = description
        .parameters
        .iter()
        .find(|parameter| parameter.name == "fixed")
        .expect("nonanimatable Choice");
    assert!(enabled.can_animate);
    assert!(!fixed.can_animate);
    assert_eq!(enabled.default_value, OpenFxParameterValue::Choice(1));
    assert_eq!(fixed.default_value, OpenFxParameterValue::Choice(0));
    for parameter in [enabled, fixed] {
        assert_eq!(
            parameter.preferred_interpolation,
            mondrian_core::automation::ParameterInterpolation::Hold
        );
        assert!(parameter.numeric_channels.is_empty());
        assert_eq!(
            parameter.choices.iter().map(|option| option.key.as_str()).collect::<Vec<_>>(),
            ["0", "1", "2"]
        );
        assert_eq!(parameter.choices[0].label, parameter.choices[1].label);
        assert_eq!(
            parameter.choices.iter().map(|option| option.display_rank).collect::<Vec<_>>(),
            [Some(i32::MAX), Some(i32::MIN), Some(0)]
        );
    }
    let kind = register_selected_openfx_filter(product_executable(), &inspection, IDENTIFIER)
        .expect("register real Filter");
    let mut effect = instantiate_effect_node(kind).expect("canonical effect");
    let property = effect
        .properties
        .iter()
        .find(|(_, property)| property.descriptor.display_name == enabled.label)
        .expect("native Choice author property")
        .1
        .clone();
    assert!(property.descriptor.schema.is_animatable);
    let first_property = hold_property(property.clone(), 2);
    let first = curve(first_property.clone());
    let changed = curve(hold_property(property, 0));
    let constant = OpenFxParameterBinding::constant(OpenFxParameterValue::Choice(1));
    let sampled = OpenFxParameterBinding::current_frame(OpenFxParameterValue::Choice(1));
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
    // Every current value is index 1, independent of duplicate labels/ranks.
    // Only the off-time endpoint/source changes,
    // so the fixture's real InstanceChanged counts exercise revision invalidation.
    for binding in [
        &first, &first, &changed, &changed, &constant, &constant, &sampled, &sampled, &first,
    ] {
        let bindings = BTreeMap::from([
            ("mode".to_owned(), binding.clone()),
            (
                "fixed".to_owned(),
                OpenFxParameterBinding::constant(OpenFxParameterValue::Choice(1)),
            ),
        ]);
        let before = bindings.clone();
        let output = session
            .render(
                &timing(0.0),
                &bindings,
                &input,
                &mut checkpoint(&cancellation),
            )
            .expect("native Choice values/keys/animation/source revisions");
        assert_pixels(&output, &input, 1);
        assert_eq!(session.worker_process_id(), worker);
        assert_eq!(bindings, before);
        assert_eq!(input.pixels(), original_pixels);
    }
    session.shutdown().expect("immutable last curve survives native teardown");
    let mut session = prepare();
    let worker = session.worker_process_id();
    let bindings = BTreeMap::from([("mode".to_owned(), first.clone())]);
    for frame in [-6.0, 0.0, 23.0, 24.0, 30.0, 6.0, 24.0] {
        let before = bindings.clone();
        let output = session
            .render(
                &timing(frame),
                &bindings,
                &input,
                &mut checkpoint(&cancellation),
            )
            .expect("native Hold queries across forward and reverse seeks");
        assert_pixels(&output, &input, if frame >= 24.0 { 2 } else { 1 });
        assert_eq!(session.worker_process_id(), worker);
        assert_eq!(bindings, before);
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
            .expect("canonical Choice evaluation");
        let output = apply_compiled_effect_graph_rgba_f32(input.view(), &graph, 0)
            .expect("real native RenderGraph path");
        assert_pixels(&output, &input, if frame >= 24 { 2 } else { 1 });
        assert_eq!(effect.properties, authored);
        assert_eq!(input.pixels(), original_pixels);
    }
}

#[test]
#[ignore = "requires current Product, SessionProbe mode choice_hold and a fresh action receipt directory"]
fn product_worker_rejects_curves_for_nonanimatable_choice_parameters() {
    let directory = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_DESCRIBE_ACTION_RECEIPT_DIRECTORY")
            .expect("fresh action receipts"),
    );
    assert!(directory.is_absolute() && directory.is_dir());
    assert!(std::fs::read_dir(&directory).unwrap().next().is_none());
    let inspection = cpu_contract_probe("choice_hold");
    let description = describe_openfx_filter(product_executable(), &inspection, IDENTIFIER)
        .expect("native metadata");
    let fixed = description
        .parameters
        .iter()
        .find(|parameter| parameter.name == "fixed")
        .unwrap();
    assert!(!fixed.can_animate);
    let property = hold_property(
        AnimatedProperty::from_descriptor(
            PropertyDescriptor::new(
                "native.refusal",
                "Refused curve",
                PropertyValue::Enum("1".into()),
            )
            .with_enum_options(
                (0..3)
                    .map(|index| {
                        mondrian_core::automation::ParameterEnumOption::new(
                            index.to_string(),
                            format!("Option {index}"),
                            format!("option.{index}"),
                        )
                    })
                    .collect(),
            ),
        ),
        2,
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
    let resident_rows = resident_action_rows(&directory);
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

fn resident_action_rows(directory: &Path) -> Vec<String> {
    use std::io::Read;
    let mut resident_rows = Vec::new();
    for entry in std::fs::read_dir(directory).unwrap() {
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
    resident_rows
}

#[test]
#[ignore = "requires current Product, the selected choice_hold domain case and fresh action receipts"]
fn product_worker_rejects_choice_domain_drift_before_publication() {
    let mode = std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("mode");
    assert!(matches!(
        mode.as_str(),
        "choice_hold" | "choice_hold_create_domain_drift" | "choice_hold_render_domain_drift"
    ));
    let directory = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_DESCRIBE_ACTION_RECEIPT_DIRECTORY")
            .expect("fresh receipts"),
    );
    assert!(directory.is_absolute() && directory.is_dir());
    assert!(std::fs::read_dir(&directory).unwrap().next().is_none());
    let inspection = cpu_contract_probe(&mode);
    let kind = register_selected_openfx_filter(product_executable(), &inspection, IDENTIFIER)
        .expect("actual static Choice metadata");
    let effect = instantiate_effect_node(kind).unwrap();
    let mut property = effect
        .properties
        .iter()
        .find(|(_, property)| property.descriptor.display_name == "mode")
        .unwrap()
        .1
        .clone();
    if mode == "choice_hold" {
        // A valid generic/canonical curve with a different complete ordinal
        // domain must be rejected against the worker's actual three options.
        property.descriptor.schema.enum_options.push(
            mondrian_core::automation::ParameterEnumOption::new("3", "Fourth", "option.3")
                .with_display_rank(1),
        );
    }
    let binding = if mode == "choice_hold_render_domain_drift" {
        // No complete curve/getter supplies a domain during the mutation.
        // The same instance's constructor-derived domain must remain pinned.
        OpenFxParameterBinding::current_frame(OpenFxParameterValue::Choice(1))
    } else {
        curve(hold_property(property, 2))
    };
    let bindings = BTreeMap::from([("mode".to_owned(), binding)]);
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
    .expect("actual lazy resident");
    let worker = session.worker_process_id();
    assert!(
        matches!(session.render(&timing(0.0), &bindings, &input, &mut checkpoint(&cancellation)), Err(OpenFxHostError::WorkerFailed(detail)) if detail == "native status 7")
    );
    assert_eq!(session.worker_process_id(), worker);
    assert_eq!(bindings, before);
    assert_eq!(input.pixels(), original_pixels);
    session.shutdown().expect("domain refusal retains original retirement");
    let rows = resident_action_rows(&directory);
    let created = rows.iter().any(|row| {
        row == "OfxActionCreateInstance return attempted=0 accepted=0 status=0 complete=1"
    });
    let rendered = rows.iter().any(|row| {
        row == "OfxImageEffectActionRender return attempted=0 accepted=0 status=0 complete=1"
    });
    assert_eq!(created, mode != "choice_hold");
    assert_eq!(rendered, mode == "choice_hold_render_domain_drift");
    if mode == "choice_hold" {
        assert!(
            !rows.iter().any(|row| row.starts_with("OfxActionCreateInstance ")
                || row.starts_with("OfxImageEffectActionRender "))
        );
    }
    assert!(rows
        .iter()
        .any(|row| row == "OfxActionUnload return attempted=0 accepted=0 status=0 complete=1"));
}
