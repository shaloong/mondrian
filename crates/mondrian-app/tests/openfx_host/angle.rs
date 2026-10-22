//! Scalar Angle admission and unwrapped degree values through the actual worker.

use super::*;
use mondrian_core::{AnimatedProperty, InterpolationType, Keyframe, ParameterUnit, TimeScale};
use mondrian_effects::{
    EffectExecutionSession, EffectExecutionSessionConfig, EffectProcessorControl,
};

const IDENTIFIER: &str = "uk.co.thefoundry.BasicGainPlugin";

fn timing(frame: f64) -> OpenFxRenderTiming {
    OpenFxRenderTiming {
        frame,
        frame_rate: 24.0,
        first_frame: -12.0,
        last_frame: 60.0,
        pixel_aspect_ratio: 1.0,
    }
}

fn linear(mut property: AnimatedProperty, endpoint: f64) -> AnimatedProperty {
    for (time, degrees) in [(TimelineTime::ZERO, 350.0), (TimelineTime::ONE, endpoint)] {
        property
            .set_exact_keyframe(Keyframe::from_preset(
                time,
                PropertyValue::Double(degrees),
                InterpolationType::Linear,
            ))
            .expect("ordinary unwrapped degree key");
    }
    property
}

fn curve(property: AnimatedProperty) -> OpenFxParameterBinding {
    OpenFxParameterBinding::curve(
        OpenFxParameterValue::Double(350.0),
        property,
        TimeScale::new(24, 1).unwrap(),
    )
}

fn bindings(
    angle: OpenFxParameterBinding,
    source: i32,
) -> BTreeMap<String, OpenFxParameterBinding> {
    BTreeMap::from([
        ("scale".to_owned(), angle),
        (
            "angleSource".to_owned(),
            OpenFxParameterBinding::constant(OpenFxParameterValue::Integer(source)),
        ),
    ])
}

fn assert_pixels(output: &EffectFloatFrame, input: &EffectFloatFrame, degrees: f64) {
    assert_eq!(output.format(), input.format());
    assert_eq!(
        (output.width(), output.height()),
        (input.width(), input.height())
    );
    let gain = (degrees / 360.0) as f32;
    assert_eq!(
        output.pixels(),
        input
            .pixels()
            .iter()
            .map(|pixel| [pixel[0] * gain, pixel[1] * gain, pixel[2] * gain, pixel[3]])
            .collect::<Vec<_>>()
    );
}

fn angle_effect(
    inspection: &mondrian_app::openfx_adapter::OpenFxBinaryInspection,
) -> (mondrian_core::effect_data::EffectNode, AnimatedProperty) {
    let description = describe_openfx_filter(product_executable(), inspection, IDENTIFIER)
        .expect("actual scalar Angle metadata");
    let angle = description
        .parameters
        .iter()
        .find(|parameter| parameter.name == "scale")
        .expect("native scale Angle");
    assert_eq!(angle.double_type, Some(OpenFxDoubleType::Angle));
    assert_eq!(angle.default_value, OpenFxParameterValue::Double(350.0));
    assert!(angle.can_animate);
    assert_eq!(angle.numeric_channels.len(), 1);
    let range = &angle.numeric_channels[0];
    assert_eq!((range.minimum, range.maximum), (-1440.0, 1440.0));
    assert_eq!(
        (range.display_minimum, range.display_maximum),
        (-720.0, 720.0)
    );
    let kind = register_selected_openfx_filter(product_executable(), inspection, IDENTIFIER)
        .expect("canonical scalar Angle definition");
    let effect = instantiate_effect_node(kind).expect("canonical native node");
    let property = effect
        .properties
        .iter()
        .find(|(_, property)| property.descriptor.display_name == angle.label)
        .expect("degree author property")
        .1
        .clone();
    assert_eq!(property.descriptor.schema.unit, ParameterUnit::Degrees);
    (effect, property)
}

fn fresh_actions() -> PathBuf {
    let directory = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_DESCRIBE_ACTION_RECEIPT_DIRECTORY")
            .expect("fresh action receipts"),
    );
    assert!(directory.is_absolute() && directory.is_dir());
    assert!(std::fs::read_dir(&directory).unwrap().next().is_none());
    directory
}

// Preserve the existing bounded, per-actual-PID action receipt. Render queries
// and literal pixels are checked in-process; no synthetic Render rows are added.
fn action_rows(directory: &Path) -> BTreeMap<u32, Vec<String>> {
    use std::io::Read;
    let entries = std::fs::read_dir(directory)
        .unwrap()
        .take(17)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(!entries.is_empty() && entries.len() <= 16);
    let mut result = BTreeMap::new();
    for entry in entries {
        let kind = entry.file_type().unwrap();
        assert!(kind.is_file() && !kind.is_symlink());
        let name = entry.file_name().into_string().unwrap();
        let pid = name
            .strip_prefix("actions-")
            .and_then(|name| name.strip_suffix(".txt"))
            .expect("actual native PID filename")
            .parse::<u32>()
            .unwrap();
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
        let mut rows = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            assert_eq!(fields.len(), 9);
            assert_eq!(fields[0].parse::<usize>().unwrap(), index + 1);
            assert!(matches!(fields[1], "D=0" | "D=1") && matches!(fields[2], "I=0" | "I=1"));
            assert_eq!(fields[8], "complete=1");
            rows.push(fields[1..].join(" "));
        }
        assert!(result.insert(pid, rows).is_none());
    }
    result
}

fn assert_one_closed_resident(directory: &Path, expected_pid: Option<u32>) {
    let all = action_rows(directory);
    let residents = all
        .iter()
        .filter(|(_, rows)| {
            rows.iter().any(|row| row.starts_with("D=0 I=0 OfxActionCreateInstance "))
        })
        .collect::<Vec<_>>();
    assert_eq!(residents.len(), 1, "one actual owner survives all seeks");
    let (pid, rows) = residents[0];
    if let Some(expected) = expected_pid {
        assert_eq!(*pid, expected);
    }
    for action in [
        "OfxActionCreateInstance",
        "OfxActionDestroyInstance",
        "OfxActionUnload",
    ] {
        assert_eq!(
            rows.iter()
                .filter(|row| **row
                    == format!(
                        "D=0 I=0 {action} return attempted=0 accepted=0 status=0 complete=1"
                    ))
                .count(),
            1
        );
    }
}

#[test]
#[ignore = "requires current Product, SessionProbe angle and fresh action receipts"]
fn product_worker_preserves_scalar_angle_degrees_curves_and_pixels() {
    let directory = fresh_actions();
    let inspection = cpu_contract_probe("angle");
    let (_effect, property) = angle_effect(&inspection);
    let first = curve(linear(property.clone(), 370.0));
    let changed = curve(linear(property, 390.0));
    let constant = OpenFxParameterBinding::constant(OpenFxParameterValue::Double(350.0));
    let sampled = OpenFxParameterBinding::current_frame(OpenFxParameterValue::Double(350.0));
    let input = cpu_contract_input();
    let original = input.clone();
    let cancel = mondrian_core::ExecutionCancellationToken::new();
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
        &mut checkpoint(&cancel),
    )
    .expect("actual Angle resident");
    let pid = session.worker_process_id();
    // All nine current values are 350. The native suite verifies off-time
    // values, keys, calculus, read-only setters and exact revision notifications.
    for (binding, source) in [
        (&first, 2),
        (&first, 2),
        (&changed, 3),
        (&changed, 3),
        (&constant, 4),
        (&constant, 4),
        (&sampled, 1),
        (&sampled, 1),
        (&first, 2),
    ] {
        let parameters = bindings(binding.clone(), source);
        let before = parameters.clone();
        let output = session
            .render(&timing(0.0), &parameters, &input, &mut checkpoint(&cancel))
            .expect("native Angle temporal source contract");
        assert_pixels(&output, &input, 350.0);
        assert_eq!(parameters, before);
        assert_eq!(session.worker_process_id(), pid);
        assert_eq!(input, original);
    }
    let parameters = bindings(first, 2);
    let before = parameters.clone();
    for (frame, degrees) in [
        (-6.0, 350.0),
        (0.0, 350.0),
        (12.0, 360.0),
        (24.0, 370.0),
        (30.0, 370.0),
        (6.0, 355.0),
        (12.0, 360.0),
    ] {
        let output = session
            .render(
                &timing(frame),
                &parameters,
                &input,
                &mut checkpoint(&cancel),
            )
            .expect("same Angle owner across seeks");
        assert_pixels(&output, &input, degrees);
        assert_eq!(session.worker_process_id(), pid);
        assert_eq!(parameters, before);
        assert_eq!(input, original);
    }
    for degrees in [370.0, -1080.0, 1080.0] {
        let parameters = bindings(
            OpenFxParameterBinding::constant(OpenFxParameterValue::Double(degrees)),
            0,
        );
        let before = parameters.clone();
        let output = session
            .render(&timing(12.0), &parameters, &input, &mut checkpoint(&cancel))
            .expect("raw unwrapped constant degrees");
        assert_pixels(&output, &input, degrees);
        assert_eq!(session.worker_process_id(), pid);
        assert_eq!(parameters, before);
        assert_eq!(input, original);
    }
    session.shutdown().expect("original shared shutdown deadline");
    assert_one_closed_resident(&directory, Some(pid));
}

#[test]
#[ignore = "requires current Product, SessionProbe angle and fresh action receipts"]
fn canonical_graph_preserves_unwrapped_angle_degrees_across_seeks() {
    let directory = fresh_actions();
    let inspection = cpu_contract_probe("angle");
    let (mut effect, property) = angle_effect(&inspection);
    effect.properties.upsert(linear(property, 370.0));
    let source_path = effect
        .properties
        .iter()
        .find(|(_, property)| property.descriptor.display_name == "Angle source oracle")
        .expect("existing native test oracle property")
        .0
        .to_owned();
    effect
        .properties
        .set_static_value(&source_path, PropertyValue::Int(5))
        .expect("explicit Graph Curve oracle, independent of queried native flags");
    let authored = effect.properties.clone();
    let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
        std::slice::from_ref(&effect),
        &[],
        &[],
        &[],
        WorkingColorSpace::LinearRec709,
        EffectFrameContext::new(
            Rational::FPS_24,
            mondrian_effects::EffectProjectGeometry::new(
                3,
                5,
                SampleAspectRatio::new(1, 1).unwrap(),
            )
            .expect("explicit owner project geometry"),
            TimelineTimeRange::new(
                TimelineTime::new(-1, 2).unwrap(),
                TimelineTime::new(5, 2).unwrap(),
            )
            .unwrap(),
        )
        .unwrap(),
        &LutPreparationCache::uncached(),
    )
    .expect("canonical degree graph");
    let input = cpu_contract_input();
    let original = input.clone();
    let mut retained = EffectExecutionSession::new(EffectExecutionSessionConfig {
        max_processor_instances: 1,
        max_processor_bytes: 64 * 1024 * 1024,
        max_working_bytes: 128 * 1024 * 1024,
        max_cache_entries: 0,
        max_cache_bytes: 0,
        max_gpu_plan_entries: 0,
        max_gpu_plan_bytes: 0,
    });
    retained.bind_generation(1);
    for (frame, degrees) in [
        (-6, 350.0),
        (0, 350.0),
        (12, 360.0),
        (24, 370.0),
        (30, 370.0),
        (6, 355.0),
        (12, 360.0),
    ] {
        let graph = program
            .evaluate(TimelineTime::new(frame, 24).unwrap())
            .expect("canonical authored Angle evaluation");
        // The adapter retains the enabled animated canonical property as a
        // complete native Curve, alongside its evaluated current value.
        let output = retained
            .apply_compiled_rgba_f32(
                input.view(),
                &graph,
                frame,
                &mut EffectProcessorControl::uncancelled(),
            )
            .expect("actual retained native Graph render");
        assert_pixels(&output, &input, degrees);
        assert_eq!(retained.diagnostics().processor_instances, 1);
        assert_eq!(retained.diagnostics().cache_entries, 0);
        assert_eq!(effect.properties, authored);
        assert_eq!(input, original);
    }
    retained.retire_processor_resources();
    assert_eq!(retained.diagnostics().processor_instances, 0);
    drop(retained);
    assert_one_closed_resident(&directory, None);
}

#[test]
#[ignore = "requires current Product, SessionProbe angle_time or angle_compound and fresh action receipts"]
fn product_worker_rejects_unadmitted_double_semantics_before_create() {
    let directory = fresh_actions();
    let mode = std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("mode");
    assert!(matches!(mode.as_str(), "angle_time" | "angle_compound"));
    let inspection = cpu_contract_probe(&mode);
    let error = describe_openfx_filter(product_executable(), &inspection, IDENTIFIER)
        .expect_err("unadmitted Double semantics must fail Describe");
    assert!(
        matches!(&error, OpenFxHostError::WorkerFailed(detail) if detail == "native status 7: "),
        "unexpected Describe refusal: {error:?}"
    );
    let all = action_rows(&directory);
    let rows = all.values().flatten().collect::<Vec<_>>();
    assert!(rows.iter().any(|row| row.contains(
        "OfxImageEffectActionDescribeInContext return attempted=0 accepted=0 status=0 complete=1"
    )));
    assert!(rows.iter().any(
        |row| row.contains("OfxActionUnload return attempted=0 accepted=0 status=0 complete=1")
    ));
    assert!(
        !rows.iter().any(|row| row.contains("OfxActionCreateInstance ")
            || row.contains("OfxImageEffectActionRender "))
    );
    // This is an end-to-end admission assertion. The C++ kind restriction is
    // independently source-reviewed; a Rust refusal is not labelled C++ proof.
}
