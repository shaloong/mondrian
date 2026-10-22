//! Real OpenFX Float32 filter admission through the supervised product worker.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mondrian_app::openfx_adapter::inspect_openfx_binary;
use mondrian_app::openfx_effect::register_selected_openfx_filter;
use mondrian_app::openfx_host::{
    describe_openfx_filter, render_openfx_filter_frame, EffectFloatFrame, OpenFxDoubleType,
    OpenFxFilterSession, OpenFxHostError, OpenFxParameterBinding, OpenFxParameterValue,
    OpenFxRenderTiming, OpenFxStringMode, OPENFX_MAX_STRING_BYTES,
};
use mondrian_core::automation::PropertyValue;
use mondrian_core::{
    Rational, SampleAspectRatio, TimelineTime, TimelineTimeRange, WorkingColorSpace,
};
use mondrian_effects::{
    apply_compiled_effect_graph_rgba_f32, instantiate_effect_node, EffectFrameContext,
    EffectNodeExt, LutPreparationCache, PreparedEffectProgram,
};

#[path = "openfx_host/boolean_hold.rs"]
mod boolean_hold;

#[path = "openfx_host/choice_hold.rs"]
mod choice_hold;

#[path = "openfx_host/angle.rs"]
mod angle;

fn constant_parameters<const N: usize>(
    values: [(String, OpenFxParameterValue); N],
) -> BTreeMap<String, OpenFxParameterBinding> {
    values
        .into_iter()
        .map(|(name, value)| (name, OpenFxParameterBinding::constant(value)))
        .collect()
}

#[test]
#[ignore = "requires rebuilt product and SessionProbe mode canonical_curves"]
fn product_worker_queries_canonical_sparse_curves_across_seeks() {
    use mondrian_core::{ExactBezierHandle, InterpolationType, KeyframeInterpolation, TimeScale};
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("mode"),
        "canonical_curves"
    );
    let binary =
        PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("probe"));
    let inspection = inspect_openfx_binary(product_executable(), &binary).expect("inspect");
    let identifier = "uk.co.thefoundry.BasicGainPlugin";
    let kind = register_selected_openfx_filter(product_executable(), &inspection, identifier)
        .expect("register actual native curve definitions");
    let mut effect = instantiate_effect_node(kind).expect("effect");
    let description =
        describe_openfx_filter(product_executable(), &inspection, identifier).expect("describe");
    let mut bindings = BTreeMap::new();
    for parameter in description.parameters.iter().filter(|parameter| {
        matches!(
            parameter.name.as_str(),
            "scale" | "point2" | "point3" | "rgb" | "rgba" | "integer2" | "integer3"
        )
    }) {
        assert!(parameter.can_animate);
        let mut property = effect
            .properties
            .iter()
            .find(|(_, property)| property.descriptor.display_name == parameter.label)
            .expect("canonical provider author property")
            .1
            .clone();
        let channel = if parameter.name == "scale" { 0 } else { 1 };
        let default = property.static_value().to_channel_values()[channel];
        property.set_animation_enabled(true);
        let integer = matches!(
            parameter.default_value,
            OpenFxParameterValue::Integer2(_) | OpenFxParameterValue::Integer3(_)
        );
        let interpolation = if integer {
            InterpolationType::Hold
        } else {
            InterpolationType::Linear
        };
        property
            .write_channels(TimelineTime::ZERO, &[(channel, default)], interpolation)
            .expect("sparse start");
        property
            .write_channels(
                TimelineTime::ONE,
                &[(channel, default + 1.0)],
                interpolation,
            )
            .expect("sparse end");
        if !integer {
            property
                .update_channel_keyframe_handles(
                    TimelineTime::ZERO,
                    channel,
                    KeyframeInterpolation::Linear,
                    KeyframeInterpolation::Bezier(ExactBezierHandle {
                        time_offset: TimelineTime::new(1, 3).expect("outgoing time"),
                        value_offset: 0.0,
                    }),
                )
                .expect("outgoing cubic");
            property
                .update_channel_keyframe_handles(
                    TimelineTime::ONE,
                    channel,
                    KeyframeInterpolation::Bezier(ExactBezierHandle {
                        time_offset: TimelineTime::new(-1, 3).expect("incoming time"),
                        value_offset: 0.0,
                    }),
                    KeyframeInterpolation::Linear,
                )
                .expect("incoming cubic");
        }
        assert!(
            property.channels().iter().enumerate().all(|(index, channel)| index
                == if parameter.name == "scale" { 0 } else { 1 }
                || channel.keyframes().is_empty())
        );
        effect.properties.upsert(property.clone());
        bindings.insert(
            parameter.name.clone(),
            OpenFxParameterBinding::curve(
                parameter.default_value.clone(),
                property,
                TimeScale::new(24, 1).expect("SDK grid"),
            ),
        );
    }
    assert_eq!(bindings.len(), 7);
    let input = mondrian_effects::EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(2, 2),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        vec![[-0.25, 0.5, 2.0, 0.25]; 4],
    )
    .expect("owned Float fixture");
    let cancellation = mondrian_core::ExecutionCancellationToken::new();
    let mut session = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        identifier,
        input.width(),
        input.height(),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        mondrian_effects::EffectProjectGeometry::new(
            input.width(),
            input.height(),
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut checkpoint(&cancellation),
    )
    .expect("prepare");
    let worker = session.worker_process_id();
    for frame in [6.0_f64, 18.0, 0.0, 24.0, 30.0, -6.0, 6.0] {
        let timing = OpenFxRenderTiming {
            frame,
            frame_rate: 24.0,
            first_frame: -12.0,
            last_frame: 60.0,
            pixel_aspect_ratio: 1.0,
        };
        let output = session
            .render(&timing, &bindings, &input, &mut checkpoint(&cancellation))
            .expect("native independent SDK curve assertions");
        let t = (frame / 24.0).clamp(0.0, 1.0);
        let smooth = 3.0 * t * t - 2.0 * t * t * t;
        let step = if frame >= 24.0 { 1.0 } else { 0.0 };
        let gain = (1.0 + (20.0 * smooth + 22.0 * step) / 128.0) as f32;
        assert_eq!(
            output.pixels(),
            input
                .pixels()
                .iter()
                .map(|pixel| pixel.map(|value| (value * (1.0 + smooth) as f32) * gain))
                .collect::<Vec<_>>(),
            "independent raster at frame {frame}"
        );
        assert_eq!(
            session.worker_process_id(),
            worker,
            "seeking must reuse the native owner"
        );
    }
    session.shutdown().expect("curve-querying native teardown");
    let frame_context = EffectFrameContext::new(
        Rational::new(24, 1),
        mondrian_effects::EffectProjectGeometry::new(
            2,
            2,
            SampleAspectRatio::new(1, 1).expect("square samples"),
        )
        .expect("explicit owner project geometry"),
        TimelineTimeRange::new(
            TimelineTime::new(-1, 2).expect("start"),
            TimelineTime::new(3, 1).expect("duration"),
        )
        .expect("owner range"),
    )
    .expect("frame context");
    let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
        &[effect],
        &[],
        &[],
        &[],
        WorkingColorSpace::LinearRec709,
        frame_context,
        &LutPreparationCache::uncached(),
    )
    .expect("production graph preparation");
    for frame in [6, 18] {
        let graph = program
            .evaluate(TimelineTime::new(frame, 24).expect("exact owner time"))
            .expect("production author evaluation");
        let output = apply_compiled_effect_graph_rgba_f32(input.view(), &graph, 0)
            .expect("canonical queries through the production compiled graph");
        let t = frame as f64 / 24.0;
        let smooth = 3.0 * t * t - 2.0 * t * t * t;
        let gain = (1.0 + 20.0 * smooth / 128.0) as f32;
        assert_eq!(
            output.pixels(),
            input
                .pixels()
                .iter()
                .map(|pixel| pixel.map(|value| (value * (1.0 + smooth) as f32) * gain))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
#[ignore = "requires SessionProbe mode curve_lifecycle and current product"]
fn product_worker_preserves_curve_source_lifecycle_at_equal_current_values() {
    curve_source_lifecycle("curve_lifecycle");
}

#[test]
#[ignore = "requires SessionProbe mode curve_animation_state and current product"]
fn product_worker_reports_parameter_animation_state_from_source_bindings() {
    curve_source_lifecycle("curve_animation_state");
}

fn curve_source_lifecycle(expected_mode: &str) {
    use mondrian_core::{InterpolationType, TimeScale};
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("mode"),
        expected_mode
    );
    let binary =
        PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("probe"));
    let inspection = inspect_openfx_binary(product_executable(), &binary).expect("inspect");
    let identifier = "uk.co.thefoundry.BasicGainPlugin";
    let kind = register_selected_openfx_filter(product_executable(), &inspection, identifier)
        .expect("register");
    let effect = instantiate_effect_node(kind).expect("effect");
    let description =
        describe_openfx_filter(product_executable(), &inspection, identifier).expect("describe");
    let scale = description
        .parameters
        .iter()
        .find(|parameter| parameter.name == "scale")
        .expect("scale");
    let property = effect
        .properties
        .iter()
        .find(|(_, property)| property.descriptor.display_name == scale.label)
        .expect("canonical property")
        .1;
    let curve = |endpoint| {
        let mut property = property.clone();
        property.set_animation_enabled(true);
        property
            .write_channels(TimelineTime::ZERO, &[(0, 1.0)], InterpolationType::Linear)
            .expect("start");
        property
            .write_channels(
                TimelineTime::ONE,
                &[(0, endpoint)],
                InterpolationType::Linear,
            )
            .expect("end");
        OpenFxParameterBinding::curve(
            OpenFxParameterValue::Double(1.0),
            property,
            TimeScale::new(24, 1).expect("grid"),
        )
    };
    let first = curve(2.0);
    let changed = curve(3.0);
    let constant = OpenFxParameterBinding::constant(OpenFxParameterValue::Double(1.0));
    let sampled = OpenFxParameterBinding::current_frame(OpenFxParameterValue::Double(1.0));
    let input = mondrian_effects::EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(2, 2),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        vec![[-0.25, 0.5, 2.0, 0.25]; 4],
    )
    .expect("owned Float fixture");
    let timing = OpenFxRenderTiming {
        frame: 0.0,
        frame_rate: 24.0,
        first_frame: 0.0,
        last_frame: 48.0,
        pixel_aspect_ratio: 1.0,
    };
    let cancellation = mondrian_core::ExecutionCancellationToken::new();
    let mut session = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        identifier,
        2,
        2,
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        mondrian_effects::EffectProjectGeometry::new(
            2,
            2,
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut checkpoint(&cancellation),
    )
    .expect("prepare");
    let worker = session.worker_process_id();
    if expected_mode == "curve_animation_state" {
        println!("curve-animation-worker-pid={worker}");
    }
    for (phase, (binding, gain)) in [
        (&first, 2.0),
        (&first, 2.0),
        (&changed, 3.0),
        (&changed, 3.0),
        (&constant, 1.0),
        (&constant, 1.0),
        (&sampled, 1.0),
        (&sampled, 1.0),
        (&first, 2.0),
    ]
    .into_iter()
    .enumerate()
    {
        let bindings = BTreeMap::from([("scale".to_owned(), binding.clone())]);
        let output = session
            .render(&timing, &bindings, &input, &mut checkpoint(&cancellation))
            .expect("SDK queries and change notification counts");
        assert_eq!(
            output.pixels(),
            input
                .pixels()
                .iter()
                .map(|pixel| pixel.map(|value| value * gain))
                .collect::<Vec<_>>(),
            "phase {phase}"
        );
        assert_eq!(session.worker_process_id(), worker);
    }
    session
        .shutdown()
        .expect("last immutable curve remains alive during SDK destruction");
}

fn product_executable() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_mondrian"))
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and mode compound_constant_animatable"]
fn product_worker_preserves_animatable_constant_query_authority() {
    qualify_product_compound_query_authority(false);
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and mode compound_frame_sample"]
fn product_worker_preserves_current_frame_query_authority() {
    qualify_product_compound_query_authority(true);
}

fn qualify_product_compound_query_authority(sampled: bool) {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("mode"),
        if sampled {
            "compound_frame_sample"
        } else {
            "compound_constant_animatable"
        }
    );
    let binary =
        PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("probe"));
    let inspection = inspect_openfx_binary(product_executable(), &binary).expect("inspect");
    let identifier = "uk.co.thefoundry.BasicGainPlugin";
    let description =
        describe_openfx_filter(product_executable(), &inspection, identifier).expect("describe");
    let parameters = description
        .parameters
        .iter()
        .filter(|parameter| {
            matches!(
                parameter.name.as_str(),
                "point2" | "point3" | "rgb" | "rgba" | "integer2" | "integer3"
            )
        })
        .map(|parameter| {
            assert!(parameter.can_animate);
            let value = parameter.default_value.clone();
            let binding = if sampled {
                OpenFxParameterBinding::current_frame(value)
            } else {
                OpenFxParameterBinding::constant(value)
            };
            (parameter.name.clone(), binding)
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(parameters.len(), 6);
    let input = mondrian_effects::EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(2, 2),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        vec![[-0.25, 0.5, 2.0, 0.25]; 4],
    )
    .expect("owned Float fixture");
    let timing = OpenFxRenderTiming {
        frame: 0.0,
        frame_rate: 24.0,
        first_frame: 0.0,
        last_frame: 10.0,
        pixel_aspect_ratio: 1.0,
    };
    let cancellation = mondrian_core::ExecutionCancellationToken::new();
    let mut session = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        identifier,
        input.width(),
        input.height(),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        mondrian_effects::EffectProjectGeometry::new(
            input.width(),
            input.height(),
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut checkpoint(&cancellation),
    )
    .expect("prepare actual product worker");
    let worker = session.worker_process_id();
    // Instance creation and both renders run the SDK probe's independent
    // off-time reads, calculus and unchanged-output-on-failure assertions.
    // Repeating the same snapshot also exercises reuse without change callbacks.
    for _ in 0..2 {
        let output = session
            .render(&timing, &parameters, &input, &mut checkpoint(&cancellation))
            .expect("query authority survives the product protocol");
        assert_eq!(output, input);
        assert_eq!(session.worker_process_id(), worker);
    }
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and probe mode strings"]
fn string_filter_description_preserves_all_native_modes() {
    let binary =
        PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("probe"));
    let inspection =
        inspect_openfx_binary(product_executable(), &binary).expect("inspect String probe");
    let description = describe_openfx_filter(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
    )
    .expect("describe all SDK String modes");
    let modes = [
        ("single", OpenFxStringMode::SingleLine),
        ("multi", OpenFxStringMode::MultiLine),
        (
            "existingFile",
            OpenFxStringMode::FilePath { must_exist: true },
        ),
        ("newFile", OpenFxStringMode::FilePath { must_exist: false }),
        (
            "existingDirectory",
            OpenFxStringMode::DirectoryPath { must_exist: true },
        ),
        (
            "newDirectory",
            OpenFxStringMode::DirectoryPath { must_exist: false },
        ),
        ("status", OpenFxStringMode::Label),
        ("rich", OpenFxStringMode::RichTextFormat),
    ];
    let mut defaults = BTreeMap::new();
    for (name, mode) in modes {
        let parameter = description.parameters.iter().find(|p| p.name == name).expect("String");
        assert_eq!(parameter.string_mode, Some(mode));
        assert!(!parameter.can_animate);
        let OpenFxParameterValue::String(value) = &parameter.default_value else {
            panic!("String type");
        };
        defaults.insert(name.to_owned(), value.clone());
    }
    assert_eq!(defaults["single"], "汉字🙂");
    assert_eq!(defaults["multi"], "first\n第二行\r\nlast\tvalue");
    let input = mondrian_effects::EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(2, 2),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        vec![[0.1, 0.2, 0.3, 0.05]; 4],
    )
    .expect("owned Float fixture");
    let timing = OpenFxRenderTiming {
        frame: 1.0,
        frame_rate: 24.0,
        first_frame: 0.0,
        last_frame: 10.0,
        pixel_aspect_ratio: 1.0,
    };
    let mut session = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        2,
        2,
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        mondrian_effects::EffectProjectGeometry::new(
            2,
            2,
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut || Ok(()),
    )
    .expect("String worker");
    let worker = session.worker_process_id();
    let authored: BTreeMap<String, OpenFxParameterBinding> = constant_parameters([
        ("single".into(), OpenFxParameterValue::String("".into())),
        (
            "multi".into(),
            OpenFxParameterValue::String("行一\r\n🙂\t行二".into()),
        ),
        (
            "newFile".into(),
            OpenFxParameterValue::String("Z:/不存在/新文件.cube".into()),
        ),
        (
            "newDirectory".into(),
            OpenFxParameterValue::String("Z:/不存在/新目录".into()),
        ),
        (
            "status".into(),
            OpenFxParameterValue::String("完成 ✓".into()),
        ),
        (
            "rich".into(),
            OpenFxParameterValue::String("{\\rtf1\\ansi changed}".into()),
        ),
    ]);
    let boundary = constant_parameters([(
        "single".into(),
        OpenFxParameterValue::String("x".repeat(OPENFX_MAX_STRING_BYTES)),
    )]);
    for values in [&BTreeMap::new(), &authored, &boundary, &BTreeMap::new()] {
        let output =
            session.render(&timing, values, &input, &mut || Ok(())).expect("String render");
        assert_eq!(session.worker_process_id(), worker);
        let bytes: usize = defaults
            .iter()
            .map(
                |(name, default)| match values.get(name).map(|binding| &binding.value) {
                    Some(OpenFxParameterValue::String(value)) => value.len(),
                    _ => default.len(),
                },
            )
            .sum();
        let gain = 1.0 + (bytes % 11) as f32 / 10.0;
        for (actual, original) in output.pixels().iter().zip(input.pixels()) {
            for channel in 0..4 {
                assert!((actual[channel] - original[channel] * gain).abs() < 1e-6);
            }
        }
    }
    for invalid in ["a\0b".to_owned(), "x".repeat(OPENFX_MAX_STRING_BYTES + 1)] {
        let values =
            constant_parameters([("single".into(), OpenFxParameterValue::String(invalid))]);
        assert!(session.render(&timing, &values, &input, &mut || Ok(())).is_err());
        assert_eq!(session.worker_process_id(), worker);
    }
    session
        .render(&timing, &authored, &input, &mut || Ok(()))
        .expect("invalid snapshot leaves worker reusable");
    // Native metadata admission cannot fabricate a faithful path/Label/RTF editor.
    assert!(register_selected_openfx_filter(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin"
    )
    .is_err());
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and probe mode strings_text"]
fn string_text_modes_execute_through_authored_compiled_graph() {
    use mondrian_core::ParameterTextMode;
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("mode"),
        "strings_text"
    );
    let binary =
        PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("probe"));
    let inspection = inspect_openfx_binary(product_executable(), &binary).expect("inspect");
    let kind = register_selected_openfx_filter(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
    )
    .expect("register textual modes");
    let mut effect = instantiate_effect_node(kind).expect("author instance");
    let values = [
        ("single", ParameterTextMode::SingleLine, ""),
        ("multi", ParameterTextMode::MultiLine, "行一\r\n🙂\t行二"),
    ];
    for (name, mode, value) in values {
        let descriptor = &effect
            .properties
            .iter()
            .find(|(_, p)| p.descriptor.display_name == name)
            .expect("String property")
            .1
            .descriptor;
        assert_eq!(descriptor.schema.text_mode, Some(mode));
        let id = descriptor.parameter_id().clone();
        effect
            .set_static_value_by_parameter(&id, PropertyValue::Text(value.into()))
            .expect("author String");
    }
    let range = TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::new(10, 24).expect("end"))
        .expect("range");
    let context = EffectFrameContext::new(
        Rational::FPS_24,
        mondrian_effects::EffectProjectGeometry::new(2, 2, SampleAspectRatio::SQUARE)
            .expect("explicit owner project geometry"),
        range,
    )
    .expect("context");
    let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
        &[effect],
        &[],
        &[],
        &[],
        WorkingColorSpace::LinearRec709,
        context,
        &LutPreparationCache::uncached(),
    )
    .expect("compile");
    let graph = program.evaluate(TimelineTime::new(1, 24).expect("frame")).expect("evaluate");
    let input = vec![[0.1, 0.2, 0.3, 0.05]; 4];
    let pixels = apply_compiled_effect_graph_rgba_f32(
        mondrian_effects::EffectFloatFrameView::new(
            mondrian_effects::EffectFrameExtent::new(2, 2),
            mondrian_effects::EffectValueFormat::new(
                mondrian_effects::EffectWorkingPrecision::Float32,
                mondrian_effects::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightFloat,
            ),
            &input,
        ),
        &graph,
        1192,
    )
    .expect("native graph");
    let gain =
        1.0 + (values.iter().map(|(_, _, value)| value.len()).sum::<usize>() % 11) as f32 / 10.0;
    for (actual, original) in pixels.iter().zip(&input) {
        for channel in 0..4 {
            assert!((actual[channel] - original[channel] * gain).abs() < 1e-6);
        }
    }
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_REFERENCE_BINARY to the official Basic.ofx.bundle"]
fn official_basic_filter_renders_authored_float32_pixels_in_child() {
    let binary = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_REFERENCE_BINARY")
            .expect("official Basic bundle path is required"),
    );
    let inspection = inspect_openfx_binary(product_executable(), &binary)
        .expect("discover pinned official Basic binary");
    let description = describe_openfx_filter(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
    )
    .expect("describe the selected native Filter context in a child");
    assert!(!description.label.is_empty());
    assert_ne!(description.label, description.identifier);
    assert_eq!(description.parameters.len(), 6);
    let scale = description
        .parameters
        .iter()
        .find(|parameter| parameter.name == "scale")
        .expect("overall gain control");
    assert_eq!(scale.default_value, OpenFxParameterValue::Double(1.0));
    assert_eq!(scale.double_type, Some(OpenFxDoubleType::Scale));
    assert_eq!(scale.numeric_channels.len(), 1);
    assert_eq!(scale.numeric_channels[0].minimum, 0.0);
    assert_eq!(scale.numeric_channels[0].display_maximum, 100.0);
    let switch = description
        .parameters
        .iter()
        .find(|parameter| parameter.name == "scaleComponents")
        .expect("component scaling switch");
    assert_eq!(switch.default_value, OpenFxParameterValue::Boolean(false));
    assert_eq!(switch.label, "Scale Individual Components");
    let mut stale = inspection.clone();
    stale.binary_sha256 = "0".repeat(64);
    assert!(matches!(
        describe_openfx_filter(
            product_executable(),
            &stale,
            "uk.co.thefoundry.BasicGainPlugin",
        ),
        Err(OpenFxHostError::Invalid(_))
    ));
    let input = mondrian_effects::EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(4, 4),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        (0..16).map(|index| [index as f32 / 16.0, 0.2, 0.3, 0.4]).collect(),
    )
    .expect("owned Float fixture");
    let parameters = constant_parameters([
        ("scale".to_owned(), OpenFxParameterValue::Double(1.5)),
        (
            "scaleComponents".to_owned(),
            OpenFxParameterValue::Boolean(false),
        ),
    ]);
    let timing = OpenFxRenderTiming {
        frame: 1.0,
        frame_rate: 24.0,
        first_frame: 0.0,
        last_frame: 100.0,
        pixel_aspect_ratio: 1.0,
    };
    let output = render_openfx_filter_frame(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        &timing,
        &parameters,
        &input,
        mondrian_effects::EffectProjectGeometry::new(
            (input).width(),
            (input).height(),
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
    )
    .expect("render actual native plugin in isolated child");
    assert_eq!((output.width(), output.height()), (4, 4));
    for (actual, source) in output.pixels().iter().zip(input.pixels()) {
        for (channel, expected) in actual.iter().zip(source.iter().map(|value| value * 1.5)) {
            assert!((channel - expected).abs() < 1e-6, "{channel} != {expected}");
        }
    }

    let component_input = mondrian_effects::EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(3, 5),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        (0..15)
            .map(|index| {
                let index = index as f32;
                [
                    index / 16.0 - 0.2,
                    0.1 + index / 32.0,
                    1.25 - index / 64.0,
                    0.6 + index / 16.0,
                ]
            })
            .collect(),
    )
    .expect("owned Float fixture");
    let component_parameters = constant_parameters([
        ("scale".to_owned(), OpenFxParameterValue::Double(0.5)),
        (
            "scaleComponents".to_owned(),
            OpenFxParameterValue::Boolean(true),
        ),
        ("scaleR".to_owned(), OpenFxParameterValue::Double(2.0)),
        ("scaleG".to_owned(), OpenFxParameterValue::Double(0.5)),
        ("scaleB".to_owned(), OpenFxParameterValue::Double(1.0)),
        ("scaleA".to_owned(), OpenFxParameterValue::Double(3.0)),
    ]);
    let component_timing = OpenFxRenderTiming {
        frame: 42.0,
        pixel_aspect_ratio: 1.2,
        ..timing.clone()
    };
    let component_output = render_openfx_filter_frame(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        &component_timing,
        &component_parameters,
        &component_input,
        mondrian_effects::EffectProjectGeometry::new(
            (component_input).width(),
            (component_input).height(),
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
    )
    .expect("render per-component authored gains without clipping HDR or alpha");
    for (actual, source) in component_output.pixels().iter().zip(component_input.pixels()) {
        for (actual, expected) in actual
            .iter()
            .zip(source.iter().zip([1.0, 0.25, 0.5, 1.5]).map(|(value, gain)| value * gain))
        {
            assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
        }
    }

    let mut invalid = input.clone();
    invalid.pixels_mut()[7][2] = f32::NAN;
    let rejected = render_openfx_filter_frame(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        &timing,
        &parameters,
        &invalid,
        mondrian_effects::EffectProjectGeometry::new(
            (invalid).width(),
            (invalid).height(),
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
    );
    assert!(
        matches!(
            rejected,
            Err(OpenFxHostError::Frame(
                mondrian_effects::EffectFloatFrameError::InvalidSample {
                    index: 7,
                    alpha: mondrian_core::ColorFrameAlpha::StraightFloat,
                }
            ))
        ),
        "{rejected:?}"
    );

    let out_of_range = constant_parameters([
        ("scale".to_owned(), OpenFxParameterValue::Double(-0.5)),
        (
            "scaleComponents".to_owned(),
            OpenFxParameterValue::Boolean(false),
        ),
    ]);
    assert!(matches!(
        render_openfx_filter_frame(
            product_executable(),
            &inspection,
            "uk.co.thefoundry.BasicGainPlugin",
            &timing,
            &out_of_range,
            &input,
            mondrian_effects::EffectProjectGeometry::new(
                (input).width(),
                (input).height(),
                mondrian_core::SampleAspectRatio::SQUARE
            )
            .expect("explicit owner project geometry"),
        ),
        Err(OpenFxHostError::WorkerFailed(_))
    ));

    let effect_type = register_selected_openfx_filter(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
    )
    .expect("register the selected native Filter as a visual effect");
    let mut effect = instantiate_effect_node(effect_type).expect("insert the Filter");
    let scale_parameter = effect
        .properties
        .iter()
        .find(|(_, property)| property.descriptor.display_name == scale.label)
        .expect("described scale control")
        .1
        .descriptor
        .parameter_id()
        .clone();
    effect
        .set_static_value_by_parameter(&scale_parameter, PropertyValue::Double(1.5))
        .expect("author the gain control");
    let frame_context = EffectFrameContext::new(
        Rational::FPS_24,
        mondrian_effects::EffectProjectGeometry::new(4, 4, SampleAspectRatio::SQUARE)
            .expect("explicit owner project geometry"),
        TimelineTimeRange::new(
            TimelineTime::ZERO,
            TimelineTime::new(101, 24).expect("source duration"),
        )
        .expect("source range"),
    )
    .expect("effect frame contract");
    let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
        &[effect],
        &[],
        &[],
        &[],
        WorkingColorSpace::LinearRec709,
        frame_context,
        &LutPreparationCache::uncached(),
    )
    .expect("compile the selected Filter into the shared graph");
    let graph = program
        .evaluate(TimelineTime::new(1, 24).expect("frame time"))
        .expect("evaluate the authored Filter");
    let integrated = apply_compiled_effect_graph_rgba_f32(input.view(), &graph, 991)
        .expect("render the native Filter through the shared graph");
    for (actual, expected) in integrated.iter().zip(output.pixels()) {
        for (actual, expected) in actual.iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
        }
    }
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_REFERENCE_BINARY to the official Basic.ofx.bundle"]
fn official_basic_resident_sessions_reuse_workers_and_isolate_frames() {
    let binary = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_REFERENCE_BINARY").expect("official Basic binary"),
    );
    let inspection =
        inspect_openfx_binary(product_executable(), &binary).expect("inspect reference");
    let cancellation = mondrian_core::ExecutionCancellationToken::new();
    let mut preview = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        2,
        3,
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        mondrian_effects::EffectProjectGeometry::new(
            2,
            3,
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut checkpoint(&cancellation),
    )
    .expect("preview worker");
    let mut export = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        2,
        3,
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        mondrian_effects::EffectProjectGeometry::new(
            2,
            3,
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut checkpoint(&cancellation),
    )
    .expect("export worker");
    let preview_id = preview.worker_process_id();
    let export_id = export.worker_process_id();
    assert_ne!(preview_id, export_id);
    let mut timing = OpenFxRenderTiming {
        frame: 0.0,
        frame_rate: 24.0,
        first_frame: 0.0,
        last_frame: 100.0,
        pixel_aspect_ratio: 1.0,
    };
    let mut input = mondrian_effects::EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(2, 3),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        vec![[-0.25, 0.5, 2.0, 0.25]; 6],
    )
    .expect("owned Float fixture");
    for (frame, gain, aspect) in [
        (0.0, 1.5, 1.0),
        (0.0, 2.0, 1.0),
        (42.0, 0.75, 1.2),
        (1.0, 1.25, 1.0),
    ] {
        timing.frame = frame;
        timing.pixel_aspect_ratio = aspect;
        let values =
            constant_parameters([("scale".to_owned(), OpenFxParameterValue::Double(gain))]);
        let output = preview
            .render(&timing, &values, &input, &mut checkpoint(&cancellation))
            .expect("resident render");
        assert_eq!(preview.worker_process_id(), preview_id);
        for (actual, source) in output.pixels().iter().zip(input.pixels()) {
            for (actual, source) in actual.iter().zip(source) {
                assert_eq!(*actual, *source * gain as f32);
            }
        }
        let unmodified = export
            .render(
                &timing,
                &BTreeMap::new(),
                &input,
                &mut checkpoint(&cancellation),
            )
            .expect("independent export");
        assert_eq!(unmodified, input);
        assert_eq!(export.worker_process_id(), export_id);
    }
    input.pixels_mut().fill([0.5, -1.0, 3.0, 0.75]);
    assert_eq!(
        preview
            .render(
                &timing,
                &BTreeMap::new(),
                &input,
                &mut checkpoint(&cancellation)
            )
            .expect("changed input/defaults"),
        input
    );
    cancellation.cancel();
    assert!(matches!(
        preview.render(
            &timing,
            &BTreeMap::new(),
            &input,
            &mut checkpoint(&cancellation)
        ),
        Err(OpenFxHostError::Canceled)
    ));
    let after_cancellation = preview.render(&timing, &BTreeMap::new(), &input, &mut || Ok(()));
    assert!(
        matches!(after_cancellation, Err(OpenFxHostError::Unavailable(_))),
        "{after_cancellation:?}"
    );
    // Retiring one consumer never releases the other consumer's worker/mapping.
    let live = mondrian_core::ExecutionCancellationToken::new();
    assert_eq!(
        export
            .render(&timing, &BTreeMap::new(), &input, &mut checkpoint(&live))
            .expect("surviving export"),
        input
    );
    assert!(matches!(
        export.render(&timing, &BTreeMap::new(), &input, &mut || {
            Err(OpenFxHostError::DeadlineExceeded)
        }),
        Err(OpenFxHostError::DeadlineExceeded)
    ));
    let after_deadline = export.render(&timing, &BTreeMap::new(), &input, &mut || Ok(()));
    assert!(
        matches!(after_deadline, Err(OpenFxHostError::Unavailable(_))),
        "{after_deadline:?}"
    );
    export.shutdown().expect("native destruction and child reap");
    export.shutdown().expect("idempotent shutdown");
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and MONDRIAN_OPENFX_SESSION_PROBE_MODE=hang"]
fn resident_worker_cancellation_retires_and_reaps_a_hung_native_render() {
    let mode = std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("hang fixture mode");
    assert!(mode == "hang" || mode == "thread_hang");
    let binary = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("probe binary"),
    );
    let inspection =
        inspect_openfx_binary(product_executable(), &binary).expect("probe inspection");
    let cancellation = mondrian_core::ExecutionCancellationToken::new();
    let mut session = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        1,
        1,
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        mondrian_effects::EffectProjectGeometry::new(
            1,
            1,
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut checkpoint(&cancellation),
    )
    .expect("resident worker");
    let token = cancellation.clone();
    let canceler = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(150));
        token.cancel();
    });
    let timing = OpenFxRenderTiming {
        frame: 0.0,
        frame_rate: 24.0,
        first_frame: 0.0,
        last_frame: 10.0,
        pixel_aspect_ratio: 1.0,
    };
    let input = mondrian_effects::EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(1, 1),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        vec![[0.25; 4]],
    )
    .expect("owned Float fixture");
    let started = std::time::Instant::now();
    assert!(matches!(
        session.render(
            &timing,
            &BTreeMap::new(),
            &input,
            &mut checkpoint(&cancellation)
        ),
        Err(OpenFxHostError::Canceled)
    ));
    canceler.join().expect("canceler");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "five-second native hang was interrupted"
    );
    assert!(matches!(
        session.render(&timing, &BTreeMap::new(), &input, &mut || Ok(())),
        Err(OpenFxHostError::Unavailable(_))
    ));
    session.shutdown().expect("failed worker already reaped");
    assert_eq!(input.pixels(), [[0.25; 4]]);
}

fn checkpoint(
    token: &mondrian_core::ExecutionCancellationToken,
) -> impl FnMut() -> Result<(), OpenFxHostError> + '_ {
    || {
        if token.is_canceled() {
            Err(OpenFxHostError::Canceled)
        } else {
            Ok(())
        }
    }
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and MONDRIAN_OPENFX_SESSION_PROBE_MODE=hang"]
fn execution_owner_deadline_interrupts_native_render_without_a_mirror_token() {
    use mondrian_effects::{EffectProcessorControl, EffectProcessorStopReason};
    use std::time::{Duration, Instant};
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("hang mode"),
        "hang"
    );
    let binary = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("probe binary"),
    );
    let inspection = inspect_openfx_binary(product_executable(), &binary).expect("inspection");
    let mut session = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        1,
        1,
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        mondrian_effects::EffectProjectGeometry::new(
            1,
            1,
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut || Ok(()),
    )
    .expect("prepare worker");
    let timing = OpenFxRenderTiming {
        frame: 0.0,
        frame_rate: 24.0,
        first_frame: 0.0,
        last_frame: 10.0,
        pixel_aspect_ratio: 1.0,
    };
    let input = mondrian_effects::EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(1, 1),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        vec![[0.25; 4]],
    )
    .expect("owned Float fixture");
    let started = Instant::now();
    let mut scheduler = || {
        (started.elapsed() >= Duration::from_millis(150))
            .then_some(EffectProcessorStopReason::DeadlineExpired)
    };
    let mut control = EffectProcessorControl::new(&mut scheduler);
    let result = session.render(&timing, &BTreeMap::new(), &input, &mut || {
        control.check().map_err(|_| OpenFxHostError::DeadlineExceeded)
    });
    assert!(matches!(result, Err(OpenFxHostError::DeadlineExceeded)));
    assert_eq!(
        control.stop_reason(),
        Some(EffectProcessorStopReason::DeadlineExpired)
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(matches!(
        session.render(&timing, &BTreeMap::new(), &input, &mut || Ok(())),
        Err(OpenFxHostError::Unavailable(_))
    ));
    session.shutdown().expect("failed worker reaped");
    assert_eq!(input.pixels(), [[0.25; 4]]);
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_REFERENCE_BINARY to the official Basic.ofx.bundle"]
fn official_basic_resident_transfer_preserves_partial_blocks_and_changed_rasters() {
    let binary = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_REFERENCE_BINARY").expect("official Basic"),
    );
    let inspection = inspect_openfx_binary(product_executable(), &binary).expect("inspection");
    // More than one transport copy block, with a partial final block and row.
    let mut session = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        257,
        257,
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        mondrian_effects::EffectProjectGeometry::new(
            257,
            257,
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut || Ok(()),
    )
    .expect("resident worker");
    let mut timing = OpenFxRenderTiming {
        frame: 0.0,
        frame_rate: 24.0,
        first_frame: 0.0,
        last_frame: 100.0,
        pixel_aspect_ratio: 1.0,
    };
    let mut input = mondrian_effects::EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(257, 257),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        (0..257 * 257)
            .map(|index| {
                [
                    (index % 257) as f32 / 128.0 - 0.5,
                    (index / 257) as f32 / 128.0,
                    1.0 + (index % 17) as f32 / 16.0,
                    (index % 257) as f32 / 256.0,
                ]
            })
            .collect(),
    )
    .expect("owned Float fixture");
    let gain = constant_parameters([("scale".to_owned(), OpenFxParameterValue::Double(1.5))]);
    let output = session
        .render(&timing, &gain, &input, &mut || Ok(()))
        .expect("complete transfer");
    assert_eq!(output.pixels().len(), input.pixels().len());
    for (actual, source) in output.pixels().iter().zip(input.pixels()) {
        for (actual, source) in actual.iter().zip(source) {
            assert_eq!(*actual, *source * 1.5);
        }
    }
    input.pixels_mut().reverse();
    timing.frame = 41.0;
    assert_eq!(
        session
            .render(&timing, &BTreeMap::new(), &input, &mut || Ok(()))
            .expect("fresh borrowed Source raster"),
        input
    );
    session.shutdown().expect("shutdown");
}

fn authored_filter_graph(
    binary_environment: &str,
    gpu_tail: bool,
) -> std::sync::Arc<mondrian_effects::CompiledEffectGraph> {
    let binary = PathBuf::from(std::env::var_os(binary_environment).expect("filter binary"));
    let inspection = inspect_openfx_binary(product_executable(), &binary).expect("inspect probe");
    let effect_type = register_selected_openfx_filter(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
    )
    .expect("register probe");
    let mut effects =
        vec![instantiate_effect_node(effect_type).expect("authored native occurrence")];
    if gpu_tail {
        let mut correction = mondrian_effects::EffectNode::with_defaults(
            mondrian_effects::EffectType::BasicCorrection,
        );
        correction
            .set_static_value_by_parameter(
                &mondrian_effects::EffectType::BasicCorrection
                    .parameter_id("exposure")
                    .expect("exposure"),
                PropertyValue::Float(0.5),
            )
            .expect("GPU tail");
        effects.push(correction);
    }
    let frame_context = EffectFrameContext::new(
        Rational::FPS_24,
        mondrian_effects::EffectProjectGeometry::new(2, 2, SampleAspectRatio::SQUARE)
            .expect("explicit owner project geometry"),
        TimelineTimeRange::new(
            TimelineTime::ZERO,
            TimelineTime::new(10, 24).expect("duration"),
        )
        .expect("range"),
    )
    .expect("frame context");
    PreparedEffectProgram::prepare_hierarchical_with_frame_context(
        &effects,
        &[],
        &[],
        &[],
        WorkingColorSpace::LinearRec709,
        frame_context,
        &LutPreparationCache::uncached(),
    )
    .expect("author probe")
    .evaluate(TimelineTime::ZERO)
    .expect("compile probe")
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and MONDRIAN_OPENFX_SESSION_PROBE_MODE=unfetched"]
fn authored_native_factory_reuses_instances_and_rejects_stale_output() {
    use mondrian_effects::{EffectExecutionSession, EffectExecutionSessionConfig};
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("probe mode"),
        "unfetched"
    );
    let graph = authored_filter_graph("MONDRIAN_OPENFX_SESSION_PROBE_BINARY", false);
    let input = [[-0.25, 1.5, 0.75, 0.5]];
    let key = graph
        .graph()
        .nodes
        .iter()
        .find_map(|node| match &node.kind {
            mondrian_effects::EffectGraphNodeKind::UnaryEffect {
                op: mondrian_effects::EffectRenderOp::Custom { key, .. },
                ..
            } => Some(key.clone()),
            _ => None,
        })
        .expect("compiled native occurrence");
    let runtime_before = mondrian_effects::effect_extension_runtime_status(&key)
        .expect("registered native definition");
    assert!(!runtime_before.disabled);
    assert!(runtime_before.last_error.is_none());
    let host_storage = OpenFxFilterSession::storage(1, 1).expect("native owner charge");
    let raster_bytes = input.len() * std::mem::size_of::<[f32; 4]>();
    let executor_raster_bytes = 2 * raster_bytes;
    // This old lower bound deliberately omits factory/route-owned metadata.
    // Query only the existing cold typed admission refusal; never prepare/DSP.
    let mut config = EffectExecutionSessionConfig {
        max_working_bytes: host_storage.active_peak_bytes() + executor_raster_bytes - 1,
        ..EffectExecutionSessionConfig::default()
    };
    let mut session = EffectExecutionSession::new(config);
    session.bind_generation(1);
    let cold_required = |session: &mut EffectExecutionSession, budget: usize| {
        let error = session
            .apply_compiled_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(1, 1),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightFloat,
                    ),
                    &input,
                ),
                &graph,
                0,
                &mut mondrian_effects::EffectProcessorControl::uncancelled(),
            )
            .expect_err("cold insufficient grant must refuse before native prepare");
        let required = match error {
            mondrian_effects::EffectFloatExecutionError::CustomProcessor(
                mondrian_effects::EffectExecutionError::CustomProcessorBudgetExceeded {
                    key: rejected_key,
                    required_bytes,
                    budget_bytes,
                },
            ) => {
                assert_eq!(rejected_key, key);
                assert_eq!(budget_bytes, budget);
                assert!(required_bytes > budget_bytes);
                required_bytes
            }
            other => panic!("expected typed cold working-grant refusal: {other:?}"),
        };
        assert_eq!(session.diagnostics().processor_instances, 0);
        assert_eq!(session.diagnostics().processor_bytes, 0);
        assert_eq!(
            mondrian_effects::effect_extension_runtime_status(&key)
                .expect("budget refusal preserves definition"),
            runtime_before,
        );
        assert_eq!(input, [[-0.25, 1.5, 0.75, 0.5]]);
        required
    };
    let minimum = cold_required(&mut session, config.max_working_bytes);
    config.max_working_bytes = minimum.checked_sub(1).expect("positive cold minimum");
    session.reconfigure(config);
    assert_eq!(
        cold_required(&mut session, config.max_working_bytes),
        minimum
    );

    // Fresh uncancelled controls above/below own no observer, prior receipts or
    // route plan. Public graph requirements and receipt-slot policy describe
    // the separate executor metadata grant; no GUI Weak layout is duplicated.
    let requirements =
        mondrian_effects::EffectProcessorFeedbackRequirements::for_compiled_graph(&graph);
    assert_eq!(requirements.occurrences(), 1);
    let route_bytes = mondrian_effects::EFFECT_PROCESSOR_FEEDBACK_SLOTS
        .checked_mul(std::mem::size_of::<
            mondrian_effects::EffectProcessorFeedbackReceipt,
        >())
        .and_then(|bytes| bytes.checked_add(requirements.maximum_owned_bytes()))
        .expect("complete public feedback route grant");
    let host_transient = host_storage
        .active_peak_bytes()
        .checked_sub(host_storage.retained_bytes())
        .expect("native active/retained contract");
    let expected_retained = minimum
        .checked_sub(executor_raster_bytes)
        .and_then(|bytes| bytes.checked_sub(route_bytes))
        .and_then(|bytes| bytes.checked_sub(host_transient))
        .expect("complete factory retained grant from cold admission");
    assert!(expected_retained >= host_storage.retained_bytes());
    config.max_working_bytes = minimum;
    session.reconfigure(config);
    assert_eq!(
        (session
            .apply_compiled_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(1, 1),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightFloat
                    ),
                    &input
                ),
                &graph,
                0,
                &mut mondrian_effects::EffectProcessorControl::uncancelled()
            )
            .expect("first native frame"))
        .pixels(),
        input.as_slice()
    );
    assert_eq!(session.diagnostics().processor_instances, 1);
    assert_eq!(session.diagnostics().processor_bytes, expected_retained);
    // The probe only omits Output on its second render in the same native
    // instance. A fresh process per frame would incorrectly succeed here.
    // A new frame generation must preserve that same worker; pixel residency
    // remains separate from frame-independent native resources.
    session.bind_generation(2);
    assert!(session
        .apply_compiled_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(1, 1),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightFloat
                ),
                &input
            ),
            &graph,
            1,
            &mut mondrian_effects::EffectProcessorControl::uncancelled()
        )
        .is_err());
    assert_eq!(input.as_slice(), [[-0.25, 1.5, 0.75, 0.5]]);
    assert_eq!(session.diagnostics().processor_instances, 0);
    session.bind_generation(3);
    assert_eq!(
        (session
            .apply_compiled_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(1, 1),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightFloat
                    ),
                    &input
                ),
                &graph,
                0,
                &mut mondrian_effects::EffectProcessorControl::uncancelled()
            )
            .expect("fresh generation"))
        .pixels(),
        input.as_slice()
    );
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and MONDRIAN_OPENFX_SESSION_PROBE_MODE=hang"]
fn authored_native_factory_observes_mixed_execution_deadline() {
    use mondrian_effects::{
        effect_extension_runtime_status, EffectColorDomain, EffectExecutionEnvironment,
        EffectExecutionLane, EffectExecutionLaneId, EffectExecutionSession,
        EffectExecutionTransfer, EffectFrameExtent, EffectGraphExecutionBudget,
        EffectGraphExecutionRequest, EffectProcessingBackend, EffectValueFormat,
        EffectValueResidency, EffectWorkingPrecision, EffectWorkingPrecisions,
        HeterogeneousCpuExecutionStopReason, PreparedHeterogeneousEffectWork,
        PreparedHeterogeneousEffectWorkError,
    };
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("probe mode"),
        "hang"
    );
    let graph = authored_filter_graph("MONDRIAN_OPENFX_SESSION_PROBE_BINARY", true);
    let key = graph
        .graph()
        .nodes
        .iter()
        .find_map(|node| match &node.kind {
            mondrian_effects::EffectGraphNodeKind::UnaryEffect {
                op: mondrian_effects::EffectRenderOp::Custom { key, .. },
                ..
            } => Some(key.clone()),
            _ => None,
        })
        .expect("native key");
    let cpu = EffectExecutionLaneId::new(0);
    let gpu = EffectExecutionLaneId::new(1);
    let environment = EffectExecutionEnvironment::new(
        Arc::from([
            EffectExecutionLane::new(
                cpu,
                EffectProcessingBackend::Cpu,
                EffectWorkingPrecisions::FLOAT32,
                10,
            ),
            EffectExecutionLane::new(
                gpu,
                EffectProcessingBackend::Gpu,
                EffectWorkingPrecisions::FLOAT32,
                1,
            ),
        ]),
        Arc::from([EffectExecutionTransfer::new(
            cpu,
            EffectWorkingPrecision::Float32,
            gpu,
            EffectWorkingPrecision::Float32,
            2,
        )]),
    )
    .expect("execution lanes");
    let format = EffectValueFormat::new(
        EffectWorkingPrecision::Float32,
        EffectColorDomain::SceneLinearRgb,
        mondrian_core::ColorFrameAlpha::StraightCoverage,
    );
    let work = PreparedHeterogeneousEffectWork::prepare(
        graph,
        &environment,
        EffectGraphExecutionRequest::new(
            EffectFrameExtent::new(1, 1),
            EffectValueResidency::new(cpu, format),
            EffectValueResidency::new(gpu, format),
            EffectGraphExecutionBudget::new(1024, 1024, 1024, 128, 256),
        ),
    )
    .expect("mixed route");
    let mut session = EffectExecutionSession::default();
    session.bind_generation(42);
    let input = [[0.25, 1.5, -0.75, 0.5]];
    let started = Instant::now();
    let result = work.execute_cpu_prefix_with_checkpoint(
        &mut session,
        42,
        mondrian_effects::EffectFloatFrameView::new(EffectFrameExtent::new(1, 1), format, &input),
        0,
        WorkingColorSpace::LinearRec709,
        &mut || {
            (started.elapsed() >= Duration::from_millis(150))
                .then_some(HeterogeneousCpuExecutionStopReason::DeadlineExpired)
        },
    );
    assert!(
        matches!(
            result,
            Err(PreparedHeterogeneousEffectWorkError::ExecutionStopped {
                reason: HeterogeneousCpuExecutionStopReason::DeadlineExpired,
                ..
            })
        ),
        "{result:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "native five-second wait must honor the execution owner"
    );
    assert_eq!(input.as_slice(), [[0.25, 1.5, -0.75, 0.5]]);
    assert_eq!(session.diagnostics().processor_instances, 0);
    let status = effect_extension_runtime_status(&key).expect("plugin status");
    assert!(!status.disabled);
    assert_eq!(status.last_error, None);
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and MONDRIAN_OPENFX_SESSION_PROBE_MODE=hang"]
fn authored_native_factory_observes_scalar_and_timeline_owner_stops() {
    use mondrian_effects::{
        EffectExecutionSession, EffectFloatExecutionError, EffectProcessorControl,
        EffectProcessorStopReason,
    };
    use mondrian_renderer::{
        composite_timeline_elements_color_frame_with_diagnostics, TimelineCompositeElement,
        TimelineCompositeError, TimelineCompositeOptions, TimelineCompositeScratch,
        TimelineEffectColorRuntime, TimelineSolidColorLayer,
    };
    use std::time::{Duration, Instant};
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("probe mode"),
        "hang"
    );
    let graph = authored_filter_graph("MONDRIAN_OPENFX_SESSION_PROBE_BINARY", false);
    let input = [[0.25, 1.5, -0.75, 0.5]];
    let mut session = EffectExecutionSession::default();
    let started = Instant::now();
    let mut checkpoint = || {
        (started.elapsed() >= Duration::from_millis(150))
            .then_some(EffectProcessorStopReason::DeadlineExpired)
    };
    let result = session.apply_compiled_rgba_f32(
        mondrian_effects::EffectFloatFrameView::new(
            mondrian_effects::EffectFrameExtent::new(1, 1),
            mondrian_effects::EffectValueFormat::new(
                mondrian_effects::EffectWorkingPrecision::Float32,
                mondrian_effects::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightFloat,
            ),
            &input,
        ),
        &graph,
        0,
        &mut EffectProcessorControl::new(&mut checkpoint),
    );
    assert!(
        matches!(
            result,
            Err(EffectFloatExecutionError::Stopped {
                reason: EffectProcessorStopReason::DeadlineExpired
            })
        ),
        "{result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(session.diagnostics().processor_instances, 0);
    assert_eq!(input.as_slice(), [[0.25, 1.5, -0.75, 0.5]]);

    let cancellation = mondrian_core::ExecutionCancellationToken::new();
    let cancel_owner = cancellation.clone();
    let owner = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        cancel_owner.cancel();
    });
    let elements = [TimelineCompositeElement::SolidColor(
        TimelineSolidColorLayer {
            color: mondrian_core::Color { r: 0.25, g: 1.5, b: -0.75, a: 0.5 },
            opacity: 1.0,
            blend_mode: mondrian_core::types::BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            effect_graph: graph.clone(),
            frame_seed: 0,
        },
    )];
    let color = mondrian_core::ProjectColorEnvironment::default();
    let sequence = mondrian_timeline::Sequence::new("native cancellation");
    let context = sequence.settings.root_program_color_context(&color).expect("color context");
    let started = Instant::now();
    let mut checkpoint =
        || cancellation.is_canceled().then_some(EffectProcessorStopReason::Canceled);
    let result = composite_timeline_elements_color_frame_with_diagnostics(
        1,
        1,
        &elements,
        TimelineCompositeOptions::default(),
        TimelineEffectColorRuntime::new(context.engine(), context.working_color_space()),
        &mut TimelineCompositeScratch::default(),
        &mut EffectProcessorControl::new(&mut checkpoint),
    );
    owner.join().expect("owner");
    assert!(
        matches!(
            result,
            Err(TimelineCompositeError::FloatEffect {
                reason: EffectFloatExecutionError::Stopped {
                    reason: EffectProcessorStopReason::Canceled
                }
            })
        ),
        "{result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    let key = graph
        .graph()
        .nodes
        .iter()
        .find_map(|node| match &node.kind {
            mondrian_effects::EffectGraphNodeKind::UnaryEffect {
                op: mondrian_effects::EffectRenderOp::Custom { key, .. },
                ..
            } => Some(key),
            _ => None,
        })
        .expect("native key");
    let status = mondrian_effects::effect_extension_runtime_status(key).expect("plugin status");
    assert!(!status.disabled);
    assert_eq!(status.last_error, None);
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_REFERENCE_BINARY to the pinned official Basic bundle"]
fn authored_uhd_filter_fits_professional_working_grant() {
    use mondrian_effects::{
        EffectExecutionSession, EffectExecutionSessionConfig, EffectProcessorControl,
    };
    let graph = authored_filter_graph("MONDRIAN_OPENFX_REFERENCE_BINARY", false);
    let width = 3840;
    let height = 2160;
    let count = width as usize * height as usize;
    let host_required = OpenFxFilterSession::storage(width, height)
        .expect("admitted native owner storage")
        .active_peak_bytes();
    let required = count * 16 * 2 + host_required; // Executor rasters and complete controlled native owner.
    assert!(required < 768 * 1024 * 1024);
    let mut input = vec![[1.5, -0.25, 0.75, 0.5]; count];
    input[0] = [0.125, 2.5, 0.25, 0.75];
    input[count - 1] = [0.875, 0.25, -0.5, 0.125];
    let mut config = EffectExecutionSessionConfig {
        max_working_bytes: required - 1,
        max_processor_instances: 8,
        max_processor_bytes: 384 * 1024 * 1024,
        ..EffectExecutionSessionConfig::uncached(required - 1)
    };
    let mut session = EffectExecutionSession::new(config);
    assert!(session
        .apply_compiled_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(width, height),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightFloat
                ),
                &input
            ),
            &graph,
            0,
            &mut EffectProcessorControl::uncancelled()
        )
        .is_err());
    assert_eq!(session.diagnostics().processor_instances, 0);
    assert!(
        host_required > config.max_processor_bytes,
        "active peak includes output storage even when retained backing fits optional residency"
    );
    config.max_working_bytes = 768 * 1024 * 1024;
    session.reconfigure(config);
    let output = session
        .apply_compiled_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(width, height),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightFloat,
                ),
                &input,
            ),
            &graph,
            0,
            &mut EffectProcessorControl::uncancelled(),
        )
        .expect("UHD native Filter within Professional grant");
    assert_eq!((output).pixels(), input.as_slice());
    drop(output);
    assert_eq!(
        session.diagnostics().processor_instances,
        1,
        "the native instance retains backing but hands its output owner to Effects"
    );
    assert_eq!(
        session.diagnostics().processor_bytes,
        OpenFxFilterSession::storage(width, height)
            .expect("retained backing")
            .retained_bytes()
    );
    // Seek with changed Source content. Pixel residency is cleared, while the
    // same frame-independent processor backing remains within the optional cap.
    session.bind_generation(2);
    input.reverse();
    let output = session
        .apply_compiled_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(width, height),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightFloat,
                ),
                &input,
            ),
            &graph,
            7,
            &mut EffectProcessorControl::uncancelled(),
        )
        .expect("changed UHD frame through the retained admitted owner");
    assert_eq!((output).pixels(), input.as_slice());
    drop(output);
    assert_eq!(
        session.diagnostics().processor_instances,
        1,
        "seek reuses the same fully charged retained native backing"
    );
    config.max_processor_bytes = 0;
    session.reconfigure(config);
    assert_eq!(session.diagnostics().processor_instances, 0);
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and MONDRIAN_OPENFX_SESSION_PROBE_MODE=integer"]
fn integer_filter_preserves_typed_authoring_and_native_pixels() {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("mode"),
        "integer"
    );
    let binary =
        PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("probe"));
    let inspection = inspect_openfx_binary(product_executable(), &binary).expect("inspect");
    let identifier = "uk.co.thefoundry.BasicGainPlugin";
    let description =
        describe_openfx_filter(product_executable(), &inspection, identifier).expect("describe");
    let count = description.parameters.iter().find(|p| p.name == "count").expect("integer");
    assert_eq!(count.default_value, OpenFxParameterValue::Integer(2));
    assert_eq!(count.numeric_channels.len(), 1);
    assert_eq!(count.numeric_channels[0].minimum, 0.0);
    assert_eq!(count.numeric_channels[0].maximum, 9.0);
    assert!(count.can_animate);
    let input = mondrian_effects::EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(2, 2),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        vec![[0.1, 0.2, 0.3, 0.05]; 4],
    )
    .expect("owned Float fixture");
    let timing = OpenFxRenderTiming {
        frame: 1.0,
        frame_rate: 24.0,
        first_frame: 0.0,
        last_frame: 10.0,
        pixel_aspect_ratio: 1.0,
    };
    for value in [0, 1, 9] {
        let output = render_openfx_filter_frame(
            product_executable(),
            &inspection,
            identifier,
            &timing,
            &constant_parameters([("count".to_owned(), OpenFxParameterValue::Integer(value))]),
            &input,
            mondrian_effects::EffectProjectGeometry::new(
                (input).width(),
                (input).height(),
                mondrian_core::SampleAspectRatio::SQUARE,
            )
            .expect("explicit owner project geometry"),
        )
        .expect("native integer");
        for (actual, original) in output.pixels().iter().zip(input.pixels()) {
            for channel in 0..4 {
                assert!((actual[channel] - original[channel] * value as f32).abs() < 1e-6);
            }
        }
    }
    for value in [
        OpenFxParameterValue::Integer(-1),
        OpenFxParameterValue::Integer(10),
        OpenFxParameterValue::Double(1.5),
        OpenFxParameterValue::Boolean(true),
    ] {
        assert!(render_openfx_filter_frame(
            product_executable(),
            &inspection,
            identifier,
            &timing,
            &constant_parameters([("count".to_owned(), value)]),
            &input,
            mondrian_effects::EffectProjectGeometry::new(
                (input).width(),
                (input).height(),
                mondrian_core::SampleAspectRatio::SQUARE
            )
            .expect("explicit owner project geometry"),
        )
        .is_err());
    }
    let kind = register_selected_openfx_filter(product_executable(), &inspection, identifier)
        .expect("register");
    let mut effect = instantiate_effect_node(kind).expect("author instance");
    let id = effect
        .properties
        .iter()
        .find(|(_, p)| p.descriptor.display_name == "Count")
        .expect("integer property")
        .1
        .descriptor
        .parameter_id()
        .clone();
    effect
        .set_static_value_by_parameter(&id, PropertyValue::Int(3))
        .expect("author exact integer");
    assert!(effect.set_static_value_by_parameter(&id, PropertyValue::Int(10)).is_err());
    let path = effect
        .properties
        .iter()
        .find(|(_, p)| p.descriptor.parameter_id() == &id)
        .expect("integer property")
        .0
        .to_owned();
    effect
        .properties
        .set_keyframe(
            &path,
            mondrian_core::automation::Keyframe::linear(TimelineTime::ZERO, PropertyValue::Int(3)),
        )
        .expect("first integer key");
    effect
        .properties
        .set_keyframe(
            &path,
            mondrian_core::automation::Keyframe::linear(
                TimelineTime::new(2, 24).expect("second key time"),
                PropertyValue::Int(5),
            ),
        )
        .expect("second integer key");
    let range = TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::new(10, 24).expect("end"))
        .expect("range");
    let context = EffectFrameContext::new(
        Rational::FPS_24,
        mondrian_effects::EffectProjectGeometry::new(2, 2, SampleAspectRatio::SQUARE)
            .expect("explicit owner project geometry"),
        range,
    )
    .expect("context");
    let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
        &[effect],
        &[],
        &[],
        &[],
        WorkingColorSpace::LinearRec709,
        context,
        &LutPreparationCache::uncached(),
    )
    .expect("compile");
    let graph = program.evaluate(TimelineTime::new(1, 24).expect("frame")).expect("evaluate");
    let pixels =
        apply_compiled_effect_graph_rgba_f32(input.view(), &graph, 1191).expect("native graph");
    for (actual, original) in pixels.iter().zip(input.pixels()) {
        for channel in 0..4 {
            assert!((actual[channel] - original[channel] * 4.0).abs() < 1e-6);
        }
    }
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and MONDRIAN_OPENFX_SESSION_PROBE_MODE=choice"]
fn choice_filter_preserves_canonical_indices_and_native_pixels() {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("mode"),
        "choice"
    );
    qualify_native_discrete_filter(false);
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and MONDRIAN_OPENFX_SESSION_PROBE_MODE=strchoice or strchoice_default"]
fn string_choice_filter_preserves_keys_and_native_pixels() {
    let mode = std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("mode");
    assert!(matches!(mode.as_str(), "strchoice" | "strchoice_default"));
    qualify_native_discrete_filter(true);
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and malformed strchoice probe mode"]
fn malformed_string_choice_is_rejected_before_registration() {
    let mode = std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("mode");
    assert!(matches!(
        mode.as_str(),
        "strchoice_duplicate"
            | "strchoice_gap"
            | "strchoice_missing_enum"
            | "strchoice_bad_default"
    ));
    let binary =
        PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("probe"));
    let inspection =
        inspect_openfx_binary(product_executable(), &binary).expect("inspect malformed probe");
    let identifier = "uk.co.thefoundry.BasicGainPlugin";
    assert!(matches!(
        describe_openfx_filter(product_executable(), &inspection, identifier),
        Err(OpenFxHostError::WorkerFailed(_))
    ));
    assert!(
        register_selected_openfx_filter(product_executable(), &inspection, identifier).is_err()
    );
}

fn qualify_native_discrete_filter(string_choice: bool) {
    let keys = if string_choice {
        ["vendor.red", "vendor.green", "vendor.blue"]
    } else {
        ["0", "1", "2"]
    };
    let native_value = |index: usize| {
        if string_choice {
            OpenFxParameterValue::StrChoice(keys[index].into())
        } else {
            OpenFxParameterValue::Choice(index as i32)
        }
    };
    let default_index = usize::from(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("mode") != "strchoice_default",
    );
    let binary =
        PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("probe"));
    let inspection = inspect_openfx_binary(product_executable(), &binary).expect("inspect");
    let identifier = "uk.co.thefoundry.BasicGainPlugin";
    let description =
        describe_openfx_filter(product_executable(), &inspection, identifier).expect("describe");
    let count = description.parameters.iter().find(|p| p.name == "mode").expect("choice");
    assert_eq!(count.default_value, native_value(default_index));
    assert_eq!(
        count.choices.iter().map(|option| option.key.as_str()).collect::<Vec<_>>(),
        keys
    );
    assert_eq!(count.choices.len(), 3);
    assert_eq!(count.choices[0].label, count.choices[1].label);
    assert_eq!(
        count.choices.iter().map(|option| option.display_rank).collect::<Vec<_>>(),
        vec![Some(i32::MAX), Some(i32::MIN), Some(0)]
    );
    assert!(!count.can_animate);
    let input = mondrian_effects::EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(2, 2),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        vec![[0.1, 0.2, 0.3, 0.05]; 4],
    )
    .expect("owned Float fixture");
    let timing = OpenFxRenderTiming {
        frame: 1.0,
        frame_rate: 24.0,
        first_frame: 0.0,
        last_frame: 10.0,
        pixel_aspect_ratio: 1.0,
    };
    let mut resident = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        identifier,
        2,
        2,
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        mondrian_effects::EffectProjectGeometry::new(
            2,
            2,
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut || Ok(()),
    )
    .expect("resident choice worker");
    let worker = resident.worker_process_id();
    // This fixture's calculus refusal control requires a real sampled source.
    // Omitted parameters and the later canonical Graph are true Constants.
    let sampled_control = |mut values: BTreeMap<String, OpenFxParameterBinding>| {
        values.insert(
            "scaleR".into(),
            OpenFxParameterBinding::current_frame(OpenFxParameterValue::Double(1.0)),
        );
        values
    };
    for index in [None, Some(0), Some(2), Some(1), None] {
        let values = sampled_control(
            index
                .map(|value| constant_parameters([("mode".into(), native_value(value))]))
                .unwrap_or_default(),
        );
        let output = resident
            .render(&timing, &values, &input, &mut || Ok(()))
            .expect("resident choice edit");
        assert_eq!(resident.worker_process_id(), worker);
        let gain = (index.unwrap_or(default_index) + 1) as f32;
        for (actual, original) in output.pixels().iter().zip(input.pixels()) {
            for channel in 0..4 {
                assert!((actual[channel] - original[channel] * gain).abs() < 1e-6);
            }
        }
    }
    // The probe performs derivative/integral suite queries against these
    // constants without changing image gain. Include zero, subnormals, signed
    // values and representable products of overflowing time differences.
    for value in [
        0.0,
        f64::from_bits(1),
        -f64::from_bits(1),
        0.25,
        -0.25,
        f64::MAX,
    ] {
        let output = resident
            .render(
                &timing,
                &sampled_control(constant_parameters([(
                    "calculus".into(),
                    OpenFxParameterValue::Double(value),
                )])),
                &input,
                &mut || Ok(()),
            )
            .expect("static native derivative and integral contract");
        assert_eq!(resident.worker_process_id(), worker);
        let gain = (default_index + 1) as f32;
        for (actual, original) in output.pixels().iter().zip(input.pixels()) {
            for channel in 0..4 {
                assert!((actual[channel] - original[channel] * gain).abs() < 1e-6);
            }
        }
    }
    for value in [0, 1, 2] {
        let output = render_openfx_filter_frame(
            product_executable(),
            &inspection,
            identifier,
            &timing,
            &sampled_control(constant_parameters([(
                "mode".to_owned(),
                native_value(value),
            )])),
            &input,
            mondrian_effects::EffectProjectGeometry::new(
                (input).width(),
                (input).height(),
                mondrian_core::SampleAspectRatio::SQUARE,
            )
            .expect("explicit owner project geometry"),
        )
        .expect("native choice");
        for (actual, original) in output.pixels().iter().zip(input.pixels()) {
            for channel in 0..4 {
                assert!((actual[channel] - original[channel] * (value + 1) as f32).abs() < 1e-6);
            }
        }
    }
    for value in [
        if string_choice {
            OpenFxParameterValue::StrChoice("vendor.unknown".into())
        } else {
            OpenFxParameterValue::Choice(-1)
        },
        if string_choice {
            OpenFxParameterValue::Integer(1)
        } else {
            OpenFxParameterValue::Choice(3)
        },
        OpenFxParameterValue::Double(1.5),
        OpenFxParameterValue::Boolean(true),
    ] {
        assert!(render_openfx_filter_frame(
            product_executable(),
            &inspection,
            identifier,
            &timing,
            &sampled_control(constant_parameters([("mode".to_owned(), value)])),
            &input,
            mondrian_effects::EffectProjectGeometry::new(
                (input).width(),
                (input).height(),
                mondrian_core::SampleAspectRatio::SQUARE
            )
            .expect("explicit owner project geometry"),
        )
        .is_err());
    }
    let kind = register_selected_openfx_filter(product_executable(), &inspection, identifier)
        .expect("register");
    let mut effect = instantiate_effect_node(kind).expect("author instance");
    let id = effect
        .properties
        .iter()
        .find(|(_, p)| p.descriptor.display_name == "Mode")
        .expect("choice property")
        .1
        .descriptor
        .parameter_id()
        .clone();
    effect
        .set_static_value_by_parameter(&id, PropertyValue::Enum(keys[2].into()))
        .expect("author canonical choice");
    assert!(effect
        .set_static_value_by_parameter(&id, PropertyValue::Enum("unknown".into()))
        .is_err());
    let range = TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::new(10, 24).expect("end"))
        .expect("range");
    let context = EffectFrameContext::new(
        Rational::FPS_24,
        mondrian_effects::EffectProjectGeometry::new(2, 2, SampleAspectRatio::SQUARE)
            .expect("explicit owner project geometry"),
        range,
    )
    .expect("context");
    let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
        &[effect],
        &[],
        &[],
        &[],
        WorkingColorSpace::LinearRec709,
        context,
        &LutPreparationCache::uncached(),
    )
    .expect("compile");
    let graph = program.evaluate(TimelineTime::new(1, 24).expect("frame")).expect("evaluate");
    let pixels =
        apply_compiled_effect_graph_rgba_f32(input.view(), &graph, 1191).expect("native graph");
    for (actual, original) in pixels.iter().zip(input.pixels()) {
        for channel in 0..4 {
            assert!((actual[channel] - original[channel] * 3.0).abs() < 1e-6);
        }
    }
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and MONDRIAN_OPENFX_SESSION_PROBE_MODE=interpolation"]
fn plugin_interpolation_metadata_controls_new_author_keys_and_readonly_instances() {
    use mondrian_core::automation::{InterpolationType, ParameterInterpolation};
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("mode"),
        "interpolation"
    );
    let binary =
        PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("probe"));
    let inspection = inspect_openfx_binary(product_executable(), &binary).expect("inspect");
    let identifier = "uk.co.thefoundry.BasicGainPlugin";
    let description =
        describe_openfx_filter(product_executable(), &inspection, identifier).expect("describe");
    assert_eq!(
        description
            .parameters
            .iter()
            .find(|p| p.name == "count")
            .expect("integer")
            .preferred_interpolation,
        ParameterInterpolation::Hold
    );
    assert_eq!(
        description
            .parameters
            .iter()
            .find(|p| p.name == "scale")
            .expect("double")
            .preferred_interpolation,
        ParameterInterpolation::Bezier
    );
    let kind = register_selected_openfx_filter(product_executable(), &inspection, identifier)
        .expect("register");
    let mut effect = instantiate_effect_node(kind).expect("instance");
    let count = effect
        .properties
        .iter()
        .find(|(_, p)| p.descriptor.display_name == "Count")
        .expect("count")
        .0
        .to_owned();
    assert_eq!(
        effect
            .properties
            .property(&count)
            .expect("count")
            .default_interpolation()
            .expect("default"),
        InterpolationType::Hold
    );
    effect
        .properties
        .enable_animation(&count, TimelineTime::ZERO)
        .expect("initial key");
    effect
        .properties
        .write_value(
            &count,
            TimelineTime::new(2, 24).expect("time"),
            PropertyValue::Int(6),
            InterpolationType::Hold,
        )
        .expect("second key");
    let range = TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::new(10, 24).expect("end"))
        .expect("range");
    let context = EffectFrameContext::new(
        Rational::FPS_24,
        mondrian_effects::EffectProjectGeometry::new(2, 2, SampleAspectRatio::SQUARE)
            .expect("explicit owner project geometry"),
        range,
    )
    .expect("context");
    let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
        &[effect],
        &[],
        &[],
        &[],
        WorkingColorSpace::LinearRec709,
        context,
        &LutPreparationCache::uncached(),
    )
    .expect("compile");
    let input = vec![[0.1, 0.2, 0.3, 0.05]; 4];
    for (frame, multiplier) in [(1, 2.0), (2, 6.0)] {
        let graph =
            program.evaluate(TimelineTime::new(frame, 24).expect("time")).expect("evaluate");
        let output = apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(2, 2),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightFloat,
                ),
                &input,
            ),
            &graph,
            1291,
        )
        .expect("render readonly instance");
        for (actual, original) in output.iter().zip(&input) {
            for channel in 0..4 {
                assert!((actual[channel] - original[channel] * multiplier).abs() < 1e-6);
            }
        }
    }
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and MONDRIAN_OPENFX_SESSION_PROBE_MODE=bad_interpolation"]
fn unknown_plugin_interpolation_is_rejected_before_registration() {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("mode"),
        "bad_interpolation"
    );
    let binary =
        PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("probe"));
    let inspection = inspect_openfx_binary(product_executable(), &binary).expect("inspect");
    let error = describe_openfx_filter(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
    )
    .expect_err("unknown mode must fail closed");
    assert!(
        matches!(error, OpenFxHostError::WorkerFailed(ref reason) if reason.contains("native status 7")),
        "{error}"
    );
}

fn cpu_contract_probe(mode: &str) -> mondrian_app::openfx_adapter::OpenFxBinaryInspection {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("mode"),
        mode
    );
    let binary = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("probe binary"),
    );
    inspect_openfx_binary(product_executable(), &binary).expect("inspect CPU contract probe")
}

fn cpu_contract_input() -> EffectFloatFrame {
    mondrian_effects::EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(3, 5),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        (0..15)
            .map(|index| {
                let value = index as f32;
                [
                    -0.5 + value / 16.0,
                    0.125 + value / 32.0,
                    2.0 - value / 64.0,
                    0.25 + value / 32.0,
                ]
            })
            .collect(),
    )
    .expect("owned Float fixture")
}

fn cpu_contract_timing(par: f64) -> OpenFxRenderTiming {
    OpenFxRenderTiming {
        frame: 0.0,
        frame_rate: 24.0,
        first_frame: 0.0,
        last_frame: 100.0,
        pixel_aspect_ratio: par,
    }
}

fn assert_cpu_contract_pixels(
    actual: &[[f32; 4]],
    input: &EffectFloatFrame,
    gain: f32,
    par: Option<f64>,
) {
    assert_eq!(actual.len(), input.pixels().len());
    for (index, (actual, source)) in actual.iter().zip(input.pixels()).enumerate() {
        let x = index % input.width() as usize;
        let y = input.height() as usize - 1 - index / input.width() as usize;
        let offsets = par.map_or([0.0; 4], |par| {
            [
                ((x as f64 + 0.5) * par / 32.0) as f32,
                (y as f32 + 0.5) / 64.0,
                0.0,
                0.0,
            ]
        });
        for channel in 0..4 {
            let expected = source[channel] * gain + offsets[channel];
            assert!(
                (actual[channel] - expected).abs() < 1e-6,
                "pixel {index} channel {channel}: {} != {expected}",
                actual[channel]
            );
        }
    }
}

fn qualify_cpu_contract_resident_and_graph(mode: &str, coordinate_output: bool) {
    let inspection = cpu_contract_probe(mode);
    assert_eq!(inspection.plugins.len(), 1);
    let identifier = "uk.co.thefoundry.BasicGainPlugin";
    let description = describe_openfx_filter(product_executable(), &inspection, identifier)
        .expect("native capabilities admit the probe");
    let scale = description
        .parameters
        .iter()
        .find(|parameter| parameter.name == "scale")
        .expect("gain control");
    let input = cpu_contract_input();
    let cancellation = mondrian_core::ExecutionCancellationToken::new();
    for _ in 0..2 {
        let mut session = OpenFxFilterSession::prepare(
            product_executable(),
            &inspection,
            identifier,
            input.width(),
            input.height(),
            mondrian_effects::EffectValueFormat::new(
                mondrian_effects::EffectWorkingPrecision::Float32,
                mondrian_effects::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightFloat,
            ),
            mondrian_effects::EffectProjectGeometry::new(
                input.width(),
                input.height(),
                mondrian_core::SampleAspectRatio::new(2, 1).unwrap(),
            )
            .expect("explicit owner project geometry"),
            &mut checkpoint(&cancellation),
        )
        .expect("exclusive native session");
        let worker = session.worker_process_id();
        for (frame, par, gain) in [
            (0.0, 2.0, 0.5_f32),
            (0.0, 2.0, 0.75),
            (42.0, 0.5, 1.5),
            (0.0, 2.0, 0.5),
        ] {
            let timing = OpenFxRenderTiming { frame, ..cpu_contract_timing(par) };
            let parameters = constant_parameters([(
                "scale".to_owned(),
                OpenFxParameterValue::Double(f64::from(gain)),
            )]);
            let output = session
                .render(&timing, &parameters, &input, &mut checkpoint(&cancellation))
                .expect("progressive exact-PAR native output");
            assert_eq!(session.worker_process_id(), worker);
            assert_cpu_contract_pixels(
                output.pixels(),
                &input,
                gain,
                coordinate_output.then_some(par),
            );
        }
        let invalid = cpu_contract_timing(f64::MAX);
        assert!(matches!(
            session.render(
                &invalid,
                &BTreeMap::new(),
                &input,
                &mut checkpoint(&cancellation)
            ),
            Err(OpenFxHostError::Invalid(_))
        ));
        let valid = cpu_contract_timing(2.0);
        let parameters =
            constant_parameters([("scale".to_owned(), OpenFxParameterValue::Double(0.5))]);
        let output = session
            .render(&valid, &parameters, &input, &mut checkpoint(&cancellation))
            .expect("invalid geometry did not mutate or retire the valid session");
        assert_eq!(session.worker_process_id(), worker);
        assert_cpu_contract_pixels(
            output.pixels(),
            &input,
            0.5,
            coordinate_output.then_some(2.0),
        );
        session.shutdown().expect("retire and reap native session");
    }
    let effect_type =
        register_selected_openfx_filter(product_executable(), &inspection, identifier)
            .expect("register native contract probe");
    let mut effect =
        instantiate_effect_node(effect_type).expect("canonical authored native effect");
    let parameter = effect
        .properties
        .iter()
        .find(|(_, property)| property.descriptor.display_name == scale.label)
        .expect("native gain author parameter")
        .1
        .descriptor
        .parameter_id()
        .clone();
    effect
        .set_static_value_by_parameter(&parameter, PropertyValue::Double(0.5))
        .expect("author gain");
    let frame_context = EffectFrameContext::new(
        Rational::FPS_24,
        mondrian_effects::EffectProjectGeometry::new(
            3,
            5,
            SampleAspectRatio::new(2, 1).expect("non-square samples"),
        )
        .expect("explicit owner project geometry"),
        TimelineTimeRange::new(
            TimelineTime::ZERO,
            TimelineTime::new(101, 24).expect("duration"),
        )
        .expect("source range"),
    )
    .expect("canonical effect frame context");
    let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
        &[effect],
        &[],
        &[],
        &[],
        WorkingColorSpace::LinearRec709,
        frame_context,
        &LutPreparationCache::uncached(),
    )
    .expect("shared effect compilation");
    let graph = program
        .evaluate(TimelineTime::new(42, 24).expect("seek time"))
        .expect("exact native graph frame");
    let output = apply_compiled_effect_graph_rgba_f32(input.view(), &graph, 991)
        .expect("canonical compiled graph output");
    assert_cpu_contract_pixels(&output, &input, 0.5, coordinate_output.then_some(2.0));
}

#[test]
#[ignore = "requires current product and SessionProbe mode host_capabilities"]
fn product_worker_advertises_actual_float_cpu_capabilities() {
    qualify_cpu_contract_resident_and_graph("host_capabilities", false);
}

#[test]
#[ignore = "requires current product and SessionProbe mode host_properties_readonly"]
fn product_worker_preserves_readonly_host_properties_and_writable_plugin_labels() {
    qualify_cpu_contract_resident_and_graph("host_properties_readonly", false);
}

#[test]
#[ignore = "requires current product and SessionProbe mode host_parameter_limit"]
fn product_worker_advertises_the_actual_per_descriptor_parameter_limit() {
    qualify_cpu_contract_resident_and_graph("host_parameter_limit", false);
}

#[test]
#[ignore = "requires current product and SessionProbe mode host_parameters_4096"]
fn product_worker_accepts_4096_parameters_including_group_and_page_descriptors() {
    qualify_cpu_contract_resident_and_graph("host_parameters_4096", false);
}

#[test]
#[ignore = "requires current product and SessionProbe mode host_parameters_4096"]
fn product_worker_describes_4096_total_parameters_including_group_and_page() {
    let inspection = cpu_contract_probe("host_parameters_4096");
    let identifier = "uk.co.thefoundry.BasicGainPlugin";
    let description = describe_openfx_filter(product_executable(), &inspection, identifier)
        .expect("4096 total descriptors admit complete metadata before resident processing");
    assert_eq!(description.identifier, identifier);
    // The pinned Basic's Group/Page and the extra Groups count toward admission,
    // while only its six value parameters enter the authored description.
    let expected = [
        ("scale", OpenFxParameterValue::Double(1.0)),
        ("scaleComponents", OpenFxParameterValue::Boolean(false)),
        ("scaleR", OpenFxParameterValue::Double(1.0)),
        ("scaleG", OpenFxParameterValue::Double(1.0)),
        ("scaleB", OpenFxParameterValue::Double(1.0)),
        ("scaleA", OpenFxParameterValue::Double(1.0)),
    ];
    assert_eq!(description.parameters.len(), expected.len());
    for (parameter, (name, default)) in description.parameters.iter().zip(expected) {
        assert_eq!(parameter.name, name);
        assert_eq!(parameter.default_value, default);
        match default {
            OpenFxParameterValue::Double(_) => {
                assert_eq!(parameter.double_type, Some(OpenFxDoubleType::Scale));
                assert_eq!(parameter.numeric_channels.len(), 1);
            }
            OpenFxParameterValue::Boolean(_) => {
                assert_eq!(parameter.double_type, None);
                assert!(parameter.numeric_channels.is_empty());
            }
            _ => unreachable!("the pinned Basic has only Double and Boolean values"),
        }
    }
}

#[test]
#[ignore = "requires current product and SessionProbe mode host_parameters_4097"]
fn product_worker_refuses_4097_descriptor_parameters_before_instance_creation() {
    let inspection = cpu_contract_probe("host_parameters_4097");
    let error = describe_openfx_filter(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
    )
    .expect_err("all descriptor parameters count toward the admission limit");
    assert!(
        matches!(error, OpenFxHostError::WorkerFailed(ref reason) if reason == "native status 7: "),
        "expected the exact descriptor refusal, not a native abort or transport failure: {error}"
    );
}

#[test]
#[ignore = "requires current product and SessionProbe mode host_numeric_parameters_4096"]
fn product_worker_describes_all_4096_unique_numeric_parameters() {
    let inspection = cpu_contract_probe("host_numeric_parameters_4096");
    let description = describe_openfx_filter(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
    )
    .expect("the entire advertised number of numeric parameters must be describable");
    assert_eq!(description.parameters.len(), 4096);
    for (index, parameter) in description.parameters.iter().enumerate() {
        assert_eq!(parameter.name, format!("host_value_{index}"));
        assert_eq!(parameter.label, parameter.name);
        assert_eq!(parameter.default_value, OpenFxParameterValue::Double(0.5));
        assert_eq!(parameter.double_type, Some(OpenFxDoubleType::Plain));
        assert_eq!(parameter.numeric_channels.len(), 1);
        assert_eq!(parameter.numeric_channels[0].minimum, 0.0);
        assert_eq!(parameter.numeric_channels[0].maximum, 1.0);
        assert_eq!(parameter.numeric_channels[0].display_minimum, 0.0);
        assert_eq!(parameter.numeric_channels[0].display_maximum, 1.0);
    }
}

#[test]
#[ignore = "requires current product and SessionProbe mode host_numeric_parameters_4096_plus_group"]
fn product_worker_refuses_4096_numeric_parameters_plus_one_group() {
    let inspection = cpu_contract_probe("host_numeric_parameters_4096_plus_group");
    let error = describe_openfx_filter(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
    )
    .expect_err("an unexported Group still counts toward the descriptor limit");
    assert!(
        matches!(error, OpenFxHostError::WorkerFailed(ref reason) if reason == "native status 7: "),
        "expected the exact descriptor refusal without partial metadata: {error}"
    );
}

#[test]
#[ignore = "requires current product and SessionProbe mode canonical_progressive"]
fn product_worker_preserves_par_canonical_coordinates_and_progressive_fields() {
    qualify_cpu_contract_resident_and_graph("canonical_progressive", true);
}

#[test]
#[ignore = "requires current product and SessionProbe mode bootstrap_required"]
fn product_worker_bootstraps_host_before_enumeration_and_native_actions() {
    qualify_cpu_contract_resident_and_graph("bootstrap_required", false);
}

#[test]
#[ignore = "requires current product and SessionProbe mode bootstrap_skip"]
fn product_discovery_silently_skips_plugins_declining_the_host() {
    let inspection = cpu_contract_probe("bootstrap_skip");
    assert!(inspection.plugins.is_empty());
    assert!(matches!(
        describe_openfx_filter(
            product_executable(),
            &inspection,
            "uk.co.thefoundry.BasicGainPlugin"
        ),
        Err(OpenFxHostError::Invalid(_))
    ));
    assert!(register_selected_openfx_filter(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin"
    )
    .is_err());
}

#[test]
#[ignore = "requires current product and SessionProbe mode bootstrap_error"]
fn product_discovery_rejects_non_successful_host_bootstrap() {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("mode"),
        "bootstrap_error"
    );
    let binary =
        PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("probe"));
    assert!(
        matches!(inspect_openfx_binary(product_executable(), &binary),
        Err(mondrian_app::openfx_adapter::OpenFxDiscoveryError::Unavailable(reason))
            if reason.contains("bootstrap failed"))
    );
}

// Observe only the existing supervised discovery/Describe children. A complete
// native action chain is required before interpreting the metadata outcome.
fn temporal_clip_description(
    mode: &str,
) -> Result<mondrian_app::openfx_host::OpenFxFilterDescription, OpenFxHostError> {
    use std::io::Read;
    let directory = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_DESCRIBE_ACTION_RECEIPT_DIRECTORY")
            .expect("new empty native action directory"),
    );
    assert!(directory.is_absolute() && directory.is_dir());
    assert!(std::fs::read_dir(&directory).unwrap().next().is_none());
    let inspection = cpu_contract_probe(mode);
    let result = describe_openfx_filter(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
    );
    let entries = std::fs::read_dir(&directory)
        .unwrap()
        .take(3)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!((1..=2).contains(&entries.len()));
    let mut discovery = Vec::new();
    let mut describe = Vec::new();
    for entry in entries {
        let kind = entry.file_type().unwrap();
        assert!(kind.is_file() && !kind.is_symlink());
        let name = entry.file_name().into_string().unwrap();
        let pid = name
            .strip_prefix("actions-")
            .and_then(|name| name.strip_suffix(".txt"))
            .expect("fixed action filename");
        assert!(pid.bytes().all(|byte| byte.is_ascii_digit()) && pid.parse::<u32>().unwrap() > 0);
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
        assert!(text.lines().count() < 32, "overflow/prefix is not proof");
        for (index, row) in text.lines().enumerate() {
            let fields: Vec<_> = row.split_whitespace().collect();
            assert_eq!(fields.len(), 9);
            assert_eq!(fields[0].parse::<usize>().unwrap(), index + 1);
            assert_eq!(fields[8], "complete=1");
            match &fields[1..3] {
                ["D=0", "I=1"] => discovery.push(fields[3..].join(" ")),
                ["D=1", "I=0"] => describe.push(fields[3..].join(" ")),
                _ => panic!("unexpected native request role"),
            }
        }
    }
    assert_eq!(
        discovery,
        vec!["OfxSetHost enter attempted=0 accepted=0 status=0 complete=1".to_owned()]
    );
    let creates = describe
        .iter()
        .filter(|row| row.starts_with("OfxActionCreateInstance "))
        .count();
    let renders = describe
        .iter()
        .filter(|row| row.starts_with("OfxImageEffectActionRender "))
        .count();
    assert_eq!((creates, renders), (0, 0));
    let expected = [
        ("OfxSetHost", "enter", 0),
        ("OfxActionLoad", "enter", 0),
        ("OfxActionLoad", "return", 0),
        ("OfxActionDescribe", "enter", 0),
        ("OfxActionDescribe", "return", 0),
        ("OfxImageEffectActionDescribeInContext", "enter", 0),
        ("OfxImageEffectActionDescribeInContext", "return", 0),
        ("OfxActionUnload", "enter", 0),
        ("descriptor-base", "check", 1),
        ("descriptor-context", "check", 1),
        ("OfxActionUnload", "return", 0),
    ]
    .into_iter()
    .map(|(action, point, status)| {
        format!("{action} {point} attempted=0 accepted=0 status={status} complete=1")
    })
    .collect::<Vec<_>>();
    assert_eq!(
        describe, expected,
        "successful descriptor actions and exact retirement, with no Create/Render"
    );
    println!("temporal-clip mode={mode} complete_descriptor=1 create={creates} render={renders}");
    result
}

#[test]
#[ignore = "requires current Product, SessionProbe clip_temporal_source, and an empty action directory"]
fn product_worker_refuses_source_clip_temporal_access_before_instance_creation() {
    let result = temporal_clip_description("clip_temporal_source");
    assert!(matches!(&result, Err(OpenFxHostError::WorkerFailed(reason)) if reason == "native status 7: "),
        "expected exact temporal descriptor refusal after successful Describe, not abort/transport failure: {result:?}");
}

#[test]
#[ignore = "requires current Product, SessionProbe clip_temporal_output, and an empty action directory"]
fn product_worker_refuses_output_clip_temporal_access_before_instance_creation() {
    let result = temporal_clip_description("clip_temporal_output");
    assert!(matches!(&result, Err(OpenFxHostError::WorkerFailed(reason)) if reason == "native status 7: "),
        "expected exact temporal descriptor refusal after successful Describe, not abort/transport failure: {result:?}");
}

#[test]
#[ignore = "requires current Product, SessionProbe clip_temporal_optional, and an empty action directory"]
fn product_worker_admits_disconnected_optional_temporal_clip() {
    let description = temporal_clip_description("clip_temporal_optional")
        .expect("unused optional clip must not impose a connected temporal dependency");
    assert_eq!(description.identifier, "uk.co.thefoundry.BasicGainPlugin");
    assert!(!description.label.is_empty());
    assert!(
        description.parameters.is_empty(),
        "independently defined descriptor-only fixture"
    );
}

#[test]
#[ignore = "requires current Product, SessionProbe optional_unmapped and an empty action-receipt directory"]
fn product_worker_reports_none_for_disconnected_optional_clip_unmapped_format() {
    use std::io::Read;
    let directory = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_DESCRIBE_ACTION_RECEIPT_DIRECTORY")
            .expect("new empty native action directory"),
    );
    assert!(directory.is_absolute() && directory.is_dir());
    assert!(std::fs::read_dir(&directory).unwrap().next().is_none());
    let inspection = cpu_contract_probe("optional_unmapped");
    assert_eq!(inspection.plugins.len(), 1);
    let input = cpu_contract_input();
    let original_input = input.clone();
    let mut session = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        input.width(),
        input.height(),
        input.format(),
        mondrian_effects::EffectProjectGeometry::new(
            input.width(),
            input.height(),
            mondrian_core::SampleAspectRatio::new(2, 1).unwrap(),
        )
        .expect("explicit owner project geometry"),
        &mut || Ok(()),
    )
    .expect("actual native session with an unconnected optional clip");
    let pid = session.worker_process_id();
    let parameters = constant_parameters([("scale".to_owned(), OpenFxParameterValue::Double(0.5))]);
    let rendered = session.render(&cpu_contract_timing(2.0), &parameters, &input, &mut || {
        Ok(())
    });
    // Read the actual resident's checks before asserting render success: a RED
    // must identify the raw metadata mismatch rather than merely native failure.
    let mut bytes = Vec::new();
    std::fs::File::open(directory.join(format!("actions-{pid}.txt")))
        .expect("same-child native action checks")
        .take(4097)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(!bytes.is_empty() && bytes.len() <= 4096 && bytes.is_ascii());
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(text.ends_with('\n') && text.lines().count() < 32);
    let mut checks = Vec::new();
    for (index, row) in text.lines().enumerate() {
        let fields: Vec<_> = row.split_whitespace().collect();
        assert_eq!(fields.len(), 9);
        assert_eq!(fields[0].parse::<usize>().unwrap(), index + 1);
        assert_eq!(
            &fields[1..3],
            ["D=0", "I=0"],
            "resident PID, not discovery/Describe"
        );
        assert_eq!(fields[8], "complete=1");
        if fields[3] == "optional-unmapped" {
            checks.push(fields[3..].join(" "));
        }
    }
    assert_eq!(
        checks,
        [
            "optional-unmapped connected-zero attempted=1 accepted=1 status=0 complete=1",
            "optional-unmapped unmapped-depth-none attempted=1 accepted=1 status=0 complete=1",
            "optional-unmapped unmapped-components-none attempted=1 accepted=1 status=0 complete=1",
            "optional-unmapped image-unavailable attempted=1 accepted=1 status=1 complete=1",
            "optional-unmapped Source attempted=1 accepted=1 status=0 complete=1",
            "optional-unmapped Output attempted=1 accepted=1 status=0 complete=1",
        ],
        "unconnected raw format is None while connected mapped/unmapped formats remain Float RGBA"
    );
    let output = rendered.expect("ordinary Basic rendering survives correct optional metadata");
    assert_eq!(session.worker_process_id(), pid);
    assert_eq!(output.format(), input.format());
    assert_eq!(output.pixels().len(), input.pixels().len());
    for (actual, source) in output.pixels().iter().zip(input.pixels()) {
        for (&actual, &source) in actual.iter().zip(source) {
            assert_eq!(actual.to_bits(), (source * 0.5).to_bits());
        }
    }
    assert_eq!(
        input, original_input,
        "unconnected clip does not alter supplied Source"
    );
    session.shutdown().expect("original bounded shutdown of the same resident");
}

fn property_query_action_rows(directory: &Path, pid: u32) -> Vec<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(directory.join(format!("actions-{pid}.txt")))
        .expect("actual resident action file")
        .take(4097)
        .read_to_end(&mut bytes)
        .expect("bounded resident actions");
    assert!(!bytes.is_empty() && bytes.len() <= 4096 && bytes.is_ascii());
    let text = std::str::from_utf8(&bytes).expect("ASCII actions");
    assert!(text.ends_with('\n') && text.lines().count() < 32);
    text.lines()
        .enumerate()
        .map(|(index, row)| {
            let fields: Vec<_> = row.split_whitespace().collect();
            assert_eq!(fields.len(), 9);
            assert_eq!(fields[0].parse::<usize>().expect("ordinal"), index + 1);
            assert_eq!(&fields[1..3], ["D=0", "I=0"], "actual resident role");
            assert_eq!(fields[8], "complete=1");
            fields[3..].join(" ")
        })
        .collect()
}

fn assert_property_query_actions(
    rows: &[String],
    marker: &str,
    expected_checks: &[&str],
    cleanup: &str,
) {
    let marker_prefix = format!("{marker} ");
    let actual: Vec<_> = rows
        .iter()
        .filter(|row| {
            [
                "OfxActionCreateInstance ",
                "OfxActionDestroyInstance ",
                "OfxImageEffectActionGetClipPreferences ",
                "OfxImageEffectActionRender ",
                marker_prefix.as_str(),
            ]
            .iter()
            .any(|prefix| row.starts_with(prefix))
        })
        .map(|row| {
            // Clip preferences permit ReplyDefault as well as OK. This gate
            // checks query counts independently of Basic's optional action.
            if row.starts_with("OfxImageEffectActionGetClipPreferences return ") {
                assert!(
                    row.ends_with("status=0 complete=1") || row.ends_with("status=14 complete=1")
                );
                row.replace("status=14 complete=1", "status=0 complete=1")
            } else {
                row.clone()
            }
        })
        .collect();
    let mut expected = vec![
        "OfxActionCreateInstance enter attempted=0 accepted=0 status=0 complete=1".to_owned(),
        "OfxActionCreateInstance return attempted=0 accepted=0 status=0 complete=1".to_owned(),
    ];
    for _ in 0..2 {
        expected.extend([
            "OfxImageEffectActionGetClipPreferences enter attempted=0 accepted=0 status=0 complete=1".to_owned(),
            "OfxImageEffectActionGetClipPreferences return attempted=0 accepted=0 status=0 complete=1".to_owned(),
            "OfxImageEffectActionRender enter attempted=0 accepted=0 status=0 complete=1".to_owned(),
        ]);
        expected.extend(expected_checks.iter().map(|row| (*row).to_owned()));
        expected.push(
            "OfxImageEffectActionRender return attempted=0 accepted=0 status=0 complete=1"
                .to_owned(),
        );
    }
    expected.extend([
        "OfxActionDestroyInstance enter attempted=0 accepted=0 status=0 complete=1".to_owned(),
        "OfxActionDestroyInstance return attempted=0 accepted=0 status=0 complete=1".to_owned(),
    ]);
    assert_eq!(
        actual, expected,
        "native query-count contract; original cleanup: {cleanup}"
    );
}

fn qualify_property_query_counts_resident_and_graph(
    mode: &str,
    action_marker: &str,
    expected_checks: &[&str],
) {
    use mondrian_effects::{
        EffectExecutionSession, EffectExecutionSessionConfig, EffectProcessorControl,
    };
    let directory = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_DESCRIBE_ACTION_RECEIPT_DIRECTORY")
            .expect("new empty native action directory"),
    );
    assert!(directory.is_absolute() && directory.is_dir());
    assert!(std::fs::read_dir(&directory).expect("action directory").next().is_none());
    let inspection = cpu_contract_probe(mode);
    assert_eq!(inspection.plugins.len(), 1);
    let identifier = "uk.co.thefoundry.BasicGainPlugin";
    let description = describe_openfx_filter(product_executable(), &inspection, identifier)
        .expect("actual Basic descriptor");
    let input = cpu_contract_input();
    let original_input = input.clone();
    let assert_half_gain = |output: &EffectFloatFrame| {
        assert_eq!(
            (output.width(), output.height(), output.format()),
            (input.width(), input.height(), input.format())
        );
        assert_eq!(output.pixels().len(), input.pixels().len());
        for (actual, source) in output.pixels().iter().zip(input.pixels()) {
            assert_eq!(
                actual.map(f32::to_bits),
                source.map(|value| (value * 0.5).to_bits())
            );
        }
    };
    let parameters = constant_parameters([("scale".to_owned(), OpenFxParameterValue::Double(0.5))]);
    let mut direct = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        identifier,
        input.width(),
        input.height(),
        input.format(),
        mondrian_effects::EffectProjectGeometry::new(
            input.width(),
            input.height(),
            mondrian_core::SampleAspectRatio::new(2, 1).unwrap(),
        )
        .expect("explicit owner project geometry"),
        &mut || Ok(()),
    )
    .expect("actual native query-count resident");
    let direct_pid = direct.worker_process_id();
    let mut direct_outputs = Vec::new();
    for frame in [0.0, 42.0] {
        let output = direct.render(
            &OpenFxRenderTiming { frame, ..cpu_contract_timing(2.0) },
            &parameters,
            &input,
            &mut || Ok(()),
        );
        let failed = output.is_err();
        direct_outputs.push(output);
        if failed {
            break;
        }
        assert_eq!(direct.worker_process_id(), direct_pid);
    }
    // Always consume the same owner before asserting the target RED. Its
    // original shutdown failure remains visible beside a query failure.
    let shutdown = direct.shutdown();
    let direct_rows = property_query_action_rows(&directory, direct_pid);
    assert_property_query_actions(
        &direct_rows,
        action_marker,
        expected_checks,
        &format!("{shutdown:?}"),
    );
    assert_eq!(direct_outputs.len(), 2);
    for output in direct_outputs {
        assert_half_gain(&output.expect("Basic renders after all real property queries"));
    }
    assert_eq!(
        input, original_input,
        "native queries and Basic preserve Source"
    );
    // Preserve the original close failure while checking the independent Graph
    // owner. Drop may retry cleanup, but cannot replace this frozen result.
    if let Err(error) = &shutdown {
        eprintln!("native query-count original direct shutdown retained: {error:?}");
    }
    drop(direct);

    let kind = register_selected_openfx_filter(product_executable(), &inspection, identifier)
        .expect("canonical native provider");
    let mut effect = instantiate_effect_node(kind).expect("canonical author node");
    let scale = description
        .parameters
        .iter()
        .find(|parameter| parameter.name == "scale")
        .expect("Basic gain metadata");
    let parameter_id = effect
        .properties
        .iter()
        .find(|(_, property)| property.descriptor.display_name == scale.label)
        .expect("canonical gain control")
        .1
        .descriptor
        .parameter_id()
        .clone();
    effect
        .set_static_value_by_parameter(&parameter_id, PropertyValue::Double(0.5))
        .expect("author half gain");
    let original_effect = effect.clone();
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
                SampleAspectRatio::new(2, 1).expect("PAR two"),
            )
            .expect("explicit owner project geometry"),
            TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::new(101, 24).expect("end"))
                .expect("source range"),
        )
        .expect("canonical frame context"),
        &LutPreparationCache::uncached(),
    )
    .expect("canonical graph");
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
    let mut graph_outputs = Vec::new();
    for frame in [0, 42] {
        let graph = program
            .evaluate(TimelineTime::new(frame, 24).expect("exact seek"))
            .expect("canonical evaluated graph");
        let output = retained.apply_compiled_rgba_f32(
            input.view(),
            &graph,
            frame,
            &mut EffectProcessorControl::uncancelled(),
        );
        let failed = output.is_err();
        graph_outputs.push(output);
        if failed {
            break;
        }
        assert_eq!(retained.diagnostics().processor_instances, 1);
        assert_eq!(retained.diagnostics().cache_entries, 0);
    }
    retained.retire_processor_resources();
    assert_eq!(retained.diagnostics().processor_instances, 0);
    drop(retained);
    let entries: Vec<_> = std::fs::read_dir(&directory)
        .expect("closed action directory")
        .take(17)
        .collect::<Result<_, _>>()
        .expect("bounded native action entries");
    assert!(entries.len() <= 16);
    let mut rendered_pids = Vec::new();
    for entry in entries {
        use std::io::Read;
        assert!(entry.file_type().expect("file type").is_file());
        let name = entry.file_name().into_string().expect("ASCII native action filename");
        let pid = name
            .strip_prefix("actions-")
            .and_then(|name| name.strip_suffix(".txt"))
            .expect("native PID filename")
            .parse::<u32>()
            .expect("actual PID");
        let mut bytes = Vec::new();
        std::fs::File::open(entry.path())
            .expect("closed native action file")
            .take(4097)
            .read_to_end(&mut bytes)
            .expect("bounded native action read");
        assert!(bytes.len() <= 4096 && bytes.is_ascii());
        if std::str::from_utf8(&bytes)
            .expect("ASCII actions")
            .contains(&format!(" {action_marker} "))
        {
            rendered_pids.push(pid);
        }
    }
    assert_eq!(
        rendered_pids.len(),
        2,
        "only direct and retained-Graph residents rendered"
    );
    assert!(rendered_pids.contains(&direct_pid));
    let graph_pid = *rendered_pids
        .iter()
        .find(|pid| **pid != direct_pid)
        .expect("distinct actual Graph resident");
    assert_property_query_actions(
        &property_query_action_rows(&directory, graph_pid),
        action_marker,
        expected_checks,
        "retired Graph owner",
    );
    assert_eq!(graph_outputs.len(), 2);
    for output in graph_outputs {
        assert_half_gain(&output.expect("canonical Basic output after all real property queries"));
    }
    assert_eq!(input, original_input);
    assert_eq!(
        effect, original_effect,
        "queries do not mutate canonical author state"
    );
    shutdown.expect("same resident closes within the original budget");
}

#[test]
#[ignore = "requires current Product, SessionProbe clip_integer_query_count and an empty action-receipt directory"]
fn product_worker_preserves_clip_integer_query_counts_in_resident_and_graph() {
    qualify_property_query_counts_resident_and_graph(
        "clip_integer_query_count",
        "clip-int-count",
        &["clip-int-count checks attempted=24 accepted=24 status=0 complete=1"],
    );
}

#[test]
#[ignore = "requires current Product, SessionProbe clip_double_query_count and an empty action-receipt directory"]
fn product_worker_preserves_clip_double_query_counts_in_resident_and_graph() {
    qualify_property_query_counts_resident_and_graph(
        "clip_double_query_count",
        "clip-double-count",
        &[
            "clip-double-count bulk attempted=66 accepted=66 status=0 complete=1",
            "clip-double-count scalar attempted=30 accepted=30 status=0 complete=1",
        ],
    );
}

#[test]
#[ignore = "requires current Product, SessionProbe effect_double_query_count and an empty action-receipt directory"]
fn product_worker_preserves_effect_double_query_counts_in_resident_and_graph() {
    qualify_property_query_counts_resident_and_graph(
        "effect_double_query_count",
        "effect-double-count",
        &[
            "effect-double-count bulk attempted=27 accepted=27 status=0 complete=1",
            "effect-double-count scalar attempted=12 accepted=12 status=0 complete=1",
        ],
    );
}

#[test]
#[ignore = "requires current Product, SessionProbe raw_property_query_count and an empty action-receipt directory"]
fn product_worker_preserves_raw_property_query_counts_in_resident_and_graph() {
    qualify_property_query_counts_resident_and_graph(
        "raw_property_query_count",
        "raw-property-count",
        &[
            "raw-property-count image attempted=18 accepted=18 status=0 complete=1",
            "raw-property-count strings attempted=12 accepted=12 status=0 complete=1",
        ],
    );
}
#[test]
#[ignore = "requires current Product, SessionProbe fixed_property_set_count and an empty action-receipt directory"]
fn product_worker_preserves_fixed_property_set_counts_in_resident_and_graph() {
    qualify_property_query_counts_resident_and_graph(
        "fixed_property_set_count",
        "fixed-property-set",
        &[
            "fixed-property-set fixed attempted=24 accepted=24 status=0 complete=1",
            "fixed-property-set variable attempted=8 accepted=8 status=0 complete=1",
        ],
    );
}

#[test]
#[ignore = "requires current product and SessionProbe mode cpu_unsupported_descriptor"]
fn product_worker_refuses_gpu_only_descriptor_before_registration() {
    let inspection = cpu_contract_probe("cpu_unsupported_descriptor");
    assert!(
        matches!(describe_openfx_filter(product_executable(), &inspection,
        "uk.co.thefoundry.BasicGainPlugin"), Err(OpenFxHostError::WorkerFailed(reason))
            if reason.contains("native status 7"))
    );
    assert!(register_selected_openfx_filter(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin"
    )
    .is_err());
}

#[test]
#[ignore = "requires current product and SessionProbe mode cpu_unsupported_create"]
fn product_worker_refuses_cpu_disabled_instance_before_rendering() {
    let inspection = cpu_contract_probe("cpu_unsupported_create");
    describe_openfx_filter(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
    )
    .expect("descriptor still supports CPU");
    let output = render_openfx_filter_frame(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        &cpu_contract_timing(1.0),
        &BTreeMap::new(),
        &cpu_contract_input(),
        mondrian_effects::EffectProjectGeometry::new(
            (cpu_contract_input()).width(),
            (cpu_contract_input()).height(),
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
    );
    assert!(
        matches!(output, Err(OpenFxHostError::WorkerFailed(reason)) if reason.contains("native status 7"))
    );
}

#[test]
#[ignore = "requires current product and SessionProbe mode cpu_unsupported_change"]
fn product_worker_refuses_cpu_disabled_changed_instance_without_stale_output() {
    let inspection = cpu_contract_probe("cpu_unsupported_change");
    let input = cpu_contract_input();
    let cancellation = mondrian_core::ExecutionCancellationToken::new();
    let mut session = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        input.width(),
        input.height(),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        mondrian_effects::EffectProjectGeometry::new(
            input.width(),
            input.height(),
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut checkpoint(&cancellation),
    )
    .expect("prepare CPU session");
    let timing = cpu_contract_timing(1.0);
    let initial = constant_parameters([("scale".to_owned(), OpenFxParameterValue::Double(0.5))]);
    let output = session
        .render(&timing, &initial, &input, &mut checkpoint(&cancellation))
        .expect("first frame supports CPU");
    assert_cpu_contract_pixels(output.pixels(), &input, 0.5, None);
    let changed = constant_parameters([("scale".to_owned(), OpenFxParameterValue::Double(0.75))]);
    assert!(
        matches!(session.render(&timing, &changed, &input, &mut checkpoint(&cancellation)),
        Err(OpenFxHostError::WorkerFailed(reason)) if reason.contains("native status 7"))
    );
    assert!(
        session
            .render(&timing, &initial, &input, &mut checkpoint(&cancellation))
            .is_err(),
        "a retired instance cannot publish a prior valid frame"
    );
}

#[test]
#[ignore = "requires SessionProbe mode mixed_api and current product"]
fn product_worker_skips_unsupported_apis_and_preserves_original_export_indices() {
    // The supported Filter is export index 2, with an unknown API, an unknown
    // image-effect API version and another unknown API around it. Their callback
    // guards abort if any unsupported entry receives setHost or effect actions.
    qualify_cpu_contract_resident_and_graph("mixed_api", false);
}

#[test]
#[ignore = "requires SessionProbe mode unsupported_api and current product"]
fn product_discovery_admits_an_empty_supported_api_intersection() {
    let inspection = cpu_contract_probe("unsupported_api");
    assert!(inspection.plugins.is_empty());
    assert!(describe_openfx_filter(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin"
    )
    .is_err());
}

fn output_alpha_input() -> EffectFloatFrame {
    mondrian_effects::EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(2, 4),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        vec![
            [-0.25, 0.5, 2.0, 0.0],
            [2.0, -4.0, 8.0, f32::MIN_POSITIVE],
            [-0.5, 0.75, 4.0, 0.25],
            [1.0, -2.0, 0.5, 1.0],
            [4.0, 2.0, -1.0, 0.5],
            [-2.0, 8.0, 1.0, 0.125],
            [0.125, -0.25, 2.0, 1.0 / 65536.0],
            [0.5, 4.0, -8.0, 0.75],
        ],
    )
    .expect("owned Float fixture")
}

fn assert_negotiated_alpha_pixels(
    actual: &[[f32; 4]],
    input: &EffectFloatFrame,
    mode: i32,
    gain: f32,
) {
    assert_eq!(actual.len(), input.pixels().len());
    for (index, (actual, source)) in actual.iter().zip(input.pixels()).enumerate() {
        let expected = if mode == 1 && source[3] == 0.0 {
            [0.0; 4]
        } else {
            [
                source[0] * gain * if mode == 1 { source[3] * gain } else { 1.0 },
                source[1] * gain * if mode == 1 { source[3] * gain } else { 1.0 },
                source[2] * gain * if mode == 1 { source[3] * gain } else { 1.0 },
                if mode == 2 { 1.0 } else { source[3] * gain },
            ]
        };
        for channel in 0..4 {
            assert!(actual[channel].is_finite());
            assert!(
                (actual[channel] - expected[channel]).abs() < 1e-6,
                "pixel {index}, channel {channel}: {} != {}",
                actual[channel],
                expected[channel]
            );
        }
        if index == 1 && mode == 1 {
            assert_eq!(
                *actual, expected,
                "positive tiny coverage must not be discarded or clamped"
            );
        }
    }
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and MONDRIAN_OPENFX_SESSION_PROBE_MODE=output_alpha"]
fn product_negotiates_native_output_alpha_for_resident_and_compiled_graph() {
    let inspection = cpu_contract_probe("output_alpha");
    let identifier = "uk.co.thefoundry.BasicGainPlugin";
    let description = describe_openfx_filter(product_executable(), &inspection, identifier)
        .expect("describe negotiated alpha choice");
    let input = output_alpha_input();
    for _ in 0..2 {
        let mut session = OpenFxFilterSession::prepare(
            product_executable(),
            &inspection,
            identifier,
            input.width(),
            input.height(),
            mondrian_effects::EffectValueFormat::new(
                mondrian_effects::EffectWorkingPrecision::Float32,
                mondrian_effects::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightFloat,
            ),
            mondrian_effects::EffectProjectGeometry::new(
                input.width(),
                input.height(),
                mondrian_core::SampleAspectRatio::new(2, 1).unwrap(),
            )
            .expect("explicit owner project geometry"),
            &mut || Ok(()),
        )
        .expect("resident alpha host");
        let worker = session.worker_process_id();
        for (frame, mode, gain) in [
            (0.0, 0, 0.5_f32),
            (0.0, 1, 0.5),
            (42.0, 2, 0.75),
            (0.0, 1, 0.75),
            (0.0, 0, 0.5),
        ] {
            let timing = OpenFxRenderTiming { frame, ..cpu_contract_timing(2.0) };
            let parameters = constant_parameters([
                (
                    "scale".into(),
                    OpenFxParameterValue::Double(f64::from(gain)),
                ),
                ("alphaMode".into(), OpenFxParameterValue::Choice(mode)),
            ]);
            let output = session
                .render(&timing, &parameters, &input, &mut || Ok(()))
                .expect("raw native output retains actual association");
            assert_eq!(session.worker_process_id(), worker);
            assert_negotiated_alpha_pixels(output.pixels(), &input, mode, gain);
        }
        // Float Straight output remains raw finite OFX data, including signed
        // and over-one alpha. This is not premultiplied coverage normalization.
        let mut raw = input.clone();
        raw.pixels_mut()[2][3] = -0.5;
        raw.pixels_mut()[3][3] = 2.5;
        let output = session
            .render(
                &cpu_contract_timing(2.0),
                &constant_parameters([
                    ("scale".into(), OpenFxParameterValue::Double(0.5)),
                    ("alphaMode".into(), OpenFxParameterValue::Choice(0)),
                ]),
                &raw,
                &mut || Ok(()),
            )
            .expect("raw Straight alpha range");
        assert_negotiated_alpha_pixels(output.pixels(), &raw, 0, 0.5);
        session.shutdown().expect("alpha session teardown");
    }
    let kind = register_selected_openfx_filter(product_executable(), &inspection, identifier)
        .expect("register alpha negotiation");
    for mode in 0..3 {
        let mut effect = instantiate_effect_node(kind.clone()).expect("canonical author effect");
        for (name, value) in [
            ("scale", PropertyValue::Double(0.5)),
            (
                "alphaMode",
                PropertyValue::Enum(
                    description
                        .parameters
                        .iter()
                        .find(|parameter| parameter.name == "alphaMode")
                        .expect("alpha choice")
                        .choices[mode]
                        .key
                        .clone(),
                ),
            ),
        ] {
            let label = &description
                .parameters
                .iter()
                .find(|parameter| parameter.name == name)
                .expect("native parameter")
                .label;
            let id = effect
                .properties
                .iter()
                .find(|(_, property)| &property.descriptor.display_name == label)
                .expect("canonical parameter")
                .1
                .descriptor
                .parameter_id()
                .clone();
            effect.set_static_value_by_parameter(&id, value).expect("author alpha controls");
        }
        let context = EffectFrameContext::new(
            Rational::FPS_24,
            mondrian_effects::EffectProjectGeometry::new(
                2,
                2,
                SampleAspectRatio::new(2, 1).expect("PAR"),
            )
            .expect("explicit owner project geometry"),
            TimelineTimeRange::new(
                TimelineTime::ZERO,
                TimelineTime::new(101, 24).expect("duration"),
            )
            .expect("range"),
        )
        .expect("canonical frame context");
        let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
            &[effect],
            &[],
            &[],
            &[],
            WorkingColorSpace::LinearRec709,
            context,
            &LutPreparationCache::uncached(),
        )
        .expect("compile graph");
        let graph = program
            .evaluate(TimelineTime::new(42, 24).expect("seek"))
            .expect("evaluate graph");
        let output = apply_compiled_effect_graph_rgba_f32(input.view(), &graph, 977)
            .expect("one canonical graph execution");
        assert_negotiated_alpha_pixels(&output, &input, mode as i32, 0.5);
    }
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and MONDRIAN_OPENFX_SESSION_PROBE_MODE=output_alpha"]
fn product_rejects_unknown_representation_or_nonfinite_native_pixels_without_publication() {
    let inspection = cpu_contract_probe("output_alpha");
    let input = output_alpha_input();
    for mode in 3..5 {
        let mut session = OpenFxFilterSession::prepare(
            product_executable(),
            &inspection,
            "uk.co.thefoundry.BasicGainPlugin",
            input.width(),
            input.height(),
            mondrian_effects::EffectValueFormat::new(
                mondrian_effects::EffectWorkingPrecision::Float32,
                mondrian_effects::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightFloat,
            ),
            mondrian_effects::EffectProjectGeometry::new(
                input.width(),
                input.height(),
                mondrian_core::SampleAspectRatio::SQUARE,
            )
            .expect("explicit owner project geometry"),
            &mut || Ok(()),
        )
        .expect("resident failure probe");
        let parameters = constant_parameters([
            ("scale".into(), OpenFxParameterValue::Double(0.5)),
            ("alphaMode".into(), OpenFxParameterValue::Choice(mode)),
        ]);
        assert!(
            session
                .render(&cpu_contract_timing(1.0), &parameters, &input, &mut || Ok(
                    ()
                ))
                .is_err(),
            "invalid or unsupported alpha mode {mode} published an output"
        );
        assert!(
            session
                .render(
                    &cpu_contract_timing(1.0),
                    &constant_parameters([("alphaMode".into(), OpenFxParameterValue::Choice(0)),]),
                    &input,
                    &mut || Ok(())
                )
                .is_err(),
            "failed alpha worker must remain retired"
        );
        session.shutdown().expect("reap failed alpha worker");
    }
}

#[test]
#[ignore = "requires release product, official Basic bundle and explicit OpenFX perf receipt/budget"]
fn official_basic_production_resident_steady_frame_performance_gate() {
    use sha2::Digest;
    use std::time::Instant;

    if cfg!(debug_assertions) {
        panic!("frame-budget qualification requires --release");
    }
    assert!(
        std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_MODE").is_none(),
        "use untouched official Basic, with no SessionProbe behavior"
    );
    let integer = |name: &str| -> u32 { std::env::var(name).expect(name).parse().expect(name) };
    let width = integer("MONDRIAN_OPENFX_PERF_WIDTH");
    let height = integer("MONDRIAN_OPENFX_PERF_HEIGHT");
    OpenFxFilterSession::storage(width, height)
        .expect("dimensions must be admitted before source allocation")
        .active_peak_bytes();
    let rate_num = integer("MONDRIAN_OPENFX_PERF_FPS_NUM");
    let rate_den = integer("MONDRIAN_OPENFX_PERF_FPS_DEN");
    let stage_percent = integer("MONDRIAN_OPENFX_PERF_STAGE_BUDGET_PERCENT");
    let count = integer("MONDRIAN_OPENFX_PERF_FRAMES");
    let warmup = integer("MONDRIAN_OPENFX_PERF_WARMUP");
    assert!(rate_num > 0 && rate_den > 0 && (1..=100).contains(&stage_percent));
    assert!((200..=10_000).contains(&count) && (16..=1000).contains(&warmup));
    let frame_budget_ns = 1_000_000_000_u64 * u64::from(rate_den) / u64::from(rate_num);
    let stage_budget_ns =
        u64::try_from(u128::from(frame_budget_ns) * u128::from(stage_percent) / 100)
            .expect("stage budget must be representable");
    assert!(stage_budget_ns > 0);
    let machine_path = PathBuf::from(
        std::env::var_os("MONDRIAN_NATIVE_PERF_CONTEXT").expect("machine/build receipt path"),
    );
    let context: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&machine_path).expect("read machine/build receipt"))
            .expect("machine/build receipt JSON");
    assert_eq!(context["schema_version"], 1);
    assert_eq!(context["machine"]["cpu_identity_and_ram_available"], true);
    assert_eq!(context["build"]["profile"], "release");
    assert!(context["build"]["commands"]
        .as_array()
        .is_some_and(|commands| !commands.is_empty()));
    assert!(context["build"]["rustc_verbose"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    let hash = |path: &Path| -> String {
        format!(
            "{:x}",
            sha2::Sha256::digest(std::fs::read(path).expect("receipt identity"))
        )
    };
    let worktree =
        PathBuf::from(std::env::var_os("MONDRIAN_PERF_WORKTREE").expect("receipt worktree"));
    assert!(worktree.is_absolute() && worktree.is_dir());
    assert_eq!(
        hash(&worktree.join("Cargo.lock")),
        context["build"]["cargo_lock_sha256"]
    );
    let transcript =
        Path::new(context["build"]["transcript"]["path"].as_str().expect("build transcript path"));
    assert_eq!(hash(transcript), context["build"]["transcript"]["sha256"]);
    let sources = context["source_receipts"].as_array().expect("source receipts");
    assert!(!sources.is_empty());
    for source in sources {
        let path = Path::new(source["path"].as_str().expect("relative source path"));
        assert!(
            path.components().all(|part| matches!(part, std::path::Component::Normal(_))),
            "source receipt must stay inside the worktree"
        );
        assert_eq!(
            hash(&worktree.join(path)),
            source["sha256"],
            "source changed after the build receipt"
        );
    }
    for required in [
        "crates/mondrian-app/src/openfx_host.rs",
        "crates/mondrian-app/src/openfx_host/resident.rs",
        "crates/mondrian-app/native/openfx/host/render.cpp",
        "crates/mondrian-app/tests/openfx_host.rs",
    ] {
        assert!(
            sources.iter().any(|source| source["path"] == required),
            "source receipt missing {required}"
        );
    }
    if let Some(external) = context["external_source_receipts"].as_array() {
        for source in external {
            let path = Path::new(source["path"].as_str().expect("external source path"));
            assert!(path.is_absolute(), "external receipt path must be explicit");
            assert_eq!(
                hash(path),
                source["sha256"],
                "reference source changed after the build receipt"
            );
        }
    }
    let binary = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_REFERENCE_BINARY").expect("official Basic binary"),
    );
    let test_executable = std::env::current_exe().expect("current test executable");
    let artifacts = context["artifacts"].as_array().expect("artifact receipts");
    for (role, actual) in [
        ("openfx_worker", product_executable()),
        ("openfx_reference", binary.as_path()),
        ("openfx_test_executable", test_executable.as_path()),
    ] {
        let entries = artifacts.iter().filter(|entry| entry["role"] == role).collect::<Vec<_>>();
        assert_eq!(
            entries.len(),
            1,
            "artifact identity missing or ambiguous: {role}"
        );
        let recorded = Path::new(entries[0]["path"].as_str().expect("artifact path"));
        assert_eq!(
            std::fs::canonicalize(recorded).expect("recorded artifact"),
            std::fs::canonicalize(actual).expect("actual artifact"),
            "wrong artifact selected: {role}"
        );
        assert_eq!(
            hash(actual),
            entries[0]["sha256"],
            "artifact changed: {role}"
        );
    }
    let setup = Instant::now();
    let inspection = inspect_openfx_binary(product_executable(), &binary).expect("inspect Basic");
    let gain = constant_parameters([("scale".to_owned(), OpenFxParameterValue::Double(0.5))]);
    let pixels = usize::try_from(u64::from(width) * u64::from(height)).expect("pixel count");
    let mut input = mondrian_effects::EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(width, height),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        (0..pixels)
            .map(|index| {
                let x = (index % width as usize) as f32 / width as f32;
                let y = (index / width as usize) as f32 / height as f32;
                [-0.25 + x, 0.5 + y, 2.0 - x, 0.25 + y / 4.0]
            })
            .collect(),
    )
    .expect("owned Float fixture");
    let mut session = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        width,
        height,
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        mondrian_effects::EffectProjectGeometry::new(
            width,
            height,
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut || Ok(()),
    )
    .expect("prepare real production resident worker");
    let pid = session.worker_process_id();
    let setup_ns = u64::try_from(setup.elapsed().as_nanos()).expect("setup duration");
    let mut timing = OpenFxRenderTiming {
        frame: 0.0,
        frame_rate: f64::from(rate_num) / f64::from(rate_den),
        first_frame: 0.0,
        last_frame: f64::from(count + warmup),
        pixel_aspect_ratio: 1.0,
    };
    let mut durations = Vec::with_capacity(count as usize);
    let mut errors = Vec::new();
    for index in 0..(warmup + count) {
        timing.frame = f64::from(index);
        // Vary a deterministic source pixel so retained output cannot masquerade
        // as completed native work. Correctness is checked outside the timer.
        input.pixels_mut()[0][0] = -0.25 + index as f32 / 65536.0;
        let begin = Instant::now();
        let result = session.render(&timing, &gain, &input, &mut || Ok(()));
        let elapsed = u64::try_from(begin.elapsed().as_nanos()).expect("render duration");
        if index >= warmup {
            durations.push(elapsed);
        }
        match result {
            Ok(output) => {
                if session.worker_process_id() != pid {
                    errors.push("steady execution respawned a worker".into());
                    break;
                }
                let correct = output.pixels().len() == input.pixels().len()
                    && output.pixels().iter().zip(input.pixels()).all(|(actual, source)| {
                        (0..4).all(|channel| {
                            actual[channel].is_finite()
                                && (actual[channel] - source[channel] * 0.5).abs() < 1e-6
                        })
                    });
                if !correct {
                    errors.push(format!("independent gain oracle failed at frame {index}"));
                    break;
                }
            }
            Err(error) => {
                errors.push(error.to_string());
                break;
            }
        }
    }
    let shutdown = Instant::now();
    let shutdown_result = session.shutdown();
    let shutdown_ns = u64::try_from(shutdown.elapsed().as_nanos()).expect("shutdown duration");
    if let Err(error) = shutdown_result {
        errors.push(error.to_string());
    }
    let mut sorted = durations.clone();
    sorted.sort_unstable();
    let percentile = |percent: usize| -> Option<u64> {
        sorted.get((sorted.len() * percent).div_ceil(100).saturating_sub(1)).copied()
    };
    let missed_frames = durations.iter().filter(|&&nanos| nanos > frame_budget_ns).count();
    let missed_stage_budget = durations.iter().filter(|&&nanos| nanos > stage_budget_ns).count();
    let worker_hash = format!(
        "{:x}",
        sha2::Sha256::digest(
            std::fs::read(product_executable()).expect("actual product worker identity")
        )
    );
    let receipt = serde_json::json!({ "gate": "openfx_production_resident_cpu_frame_v1",
        "context": context, "context_sha256": hash(&machine_path),
        "fixture_sha256": inspection.binary_sha256,
        "worker_sha256": worker_hash, "width": width, "height": height,
        "fps_num": rate_num, "fps_den": rate_den, "stage_budget_percent": stage_percent,
        "frame_budget_ns": frame_budget_ns, "stage_budget_ns": stage_budget_ns,
        "warmup_excluded": warmup, "samples_requested": count, "samples": durations.len(),
        "setup_ns": setup_ns, "shutdown_ns": shutdown_ns, "p50_ns": percentile(50),
        "p95_ns": percentile(95), "p99_ns": percentile(99), "max_ns": sorted.last(),
        "frame_budget_misses": missed_frames, "stage_budget_misses": missed_stage_budget,
        "render_ns": durations, "errors": errors,
        "oracle": "independent asymmetric HDR/negative/alpha gain on every output; timed render excludes oracle",
        "timed_boundary": "public OpenFxFilterSession::render including validation, hashes, copies, IPC, native work and output allocation" });
    let output_path = PathBuf::from(
        std::env::var_os("MONDRIAN_PERF_OUTPUT").expect("qualification JSON output path"),
    );
    std::fs::write(
        output_path,
        serde_json::to_vec_pretty(&receipt).expect("serialize receipt"),
    )
    .expect("write qualification receipt before budget verdict");
    assert!(errors.is_empty(), "native work failed: {errors:?}");
    assert_eq!(sorted.len(), count as usize);
    assert_eq!(
        missed_stage_budget, 0,
        "real resident frame exceeded declared NLE stage budget: {receipt}"
    );
}

#[test]
#[ignore = "requires SessionProbe mode mixed_api_long and current product"]
fn product_skips_long_unsupported_api_names_and_preserves_original_selected_index() {
    qualify_cpu_contract_resident_and_graph("mixed_api_long", false);
}

#[test]
#[ignore = "requires SessionProbe mode bootstrap_memory and current product"]
fn product_bootstrap_image_memory_uses_a_live_host_in_discovery_and_resident_execution() {
    qualify_cpu_contract_resident_and_graph("bootstrap_memory", false);
}

// This test intentionally uses only the current public API. The typed-frame
// production migration updates its caller/field access after genuine RED.
#[test]
#[ignore = "requires current product and SessionProbe mode output_alpha"]
fn product_preserves_complete_finite_associated_output_without_retiring_valid_worker() {
    let inspection = cpu_contract_probe("output_alpha");
    let input = output_alpha_input();
    let mut session = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        input.width(),
        input.height(),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        mondrian_effects::EffectProjectGeometry::new(
            input.width(),
            input.height(),
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut || Ok(()),
    )
    .expect("owned native Float session");
    let worker = session.worker_process_id();
    for mode in [1, 5, 6, 7, 8, 2, 0] {
        let output = session
            .render(
                &cpu_contract_timing(1.0),
                &constant_parameters([
                    ("scale".into(), OpenFxParameterValue::Double(1.0)),
                    ("alphaMode".into(), OpenFxParameterValue::Choice(mode)),
                ]),
                &input,
                &mut || Ok(()),
            )
            .expect("finite associated output is a valid complete image value");
        assert_eq!(
            session.worker_process_id(),
            worker,
            "association admission must not retire valid native state"
        );
        let modified_native_first = (input.height() as usize - 1) * input.width() as usize;
        // Begin with positive alpha so RED proves association semantics before
        // any signed-zero bit distinction is inspected.
        for index in (1..input.pixels().len()).chain(std::iter::once(0)) {
            let actual = output.pixels()[index];
            let input = input.pixels()[index];
            let mut expected = input;
            if mode == 2 {
                expected[3] = 1.0;
            } else if mode != 0 {
                for channel in expected.iter_mut().take(3) {
                    *channel *= input[3];
                }
                if mode == 8 && input[3] == 0.0 {
                    expected = [7.0, -3.0, 11.0, 0.0];
                }
            }
            if index == modified_native_first {
                match mode {
                    5 => {
                        expected[0] = f32::MAX;
                        expected[3] = f32::MIN_POSITIVE;
                    }
                    6 => expected[3] = -0.5,
                    7 => expected[3] = 1.5,
                    _ => {}
                }
            }
            assert_eq!(
                actual.map(f32::to_bits),
                expected.map(f32::to_bits),
                "raw associated image changed at mode {mode}, pixel {index}"
            );
        }
    }
    session.shutdown().expect("normal native teardown");
}

#[test]
#[ignore = "requires current product and SessionProbe source_alpha mode"]
fn native_source_reports_actual_association_and_refuses_rebinding_before_dsp() {
    use mondrian_core::ColorFrameAlpha;
    let inspection = cpu_contract_probe("source_alpha");
    for (alpha, mode, expected_alpha, sample) in [
        (
            ColorFrameAlpha::StraightCoverage,
            0,
            ColorFrameAlpha::StraightFloat,
            [2.0, -1.0, 4.0, 0.5],
        ),
        (
            ColorFrameAlpha::StraightFloat,
            0,
            ColorFrameAlpha::StraightFloat,
            [2.0, -1.0, 4.0, -0.5],
        ),
        (
            ColorFrameAlpha::PremultipliedCoverage,
            1,
            ColorFrameAlpha::PremultipliedFloat,
            [7.0, -3.0, 11.0, 0.0],
        ),
        (
            ColorFrameAlpha::PremultipliedFloat,
            1,
            ColorFrameAlpha::PremultipliedFloat,
            [-2.0, 4.0, 8.0, 1.5],
        ),
        (
            ColorFrameAlpha::Opaque,
            2,
            ColorFrameAlpha::Opaque,
            [2.0, -1.0, 4.0, 1.0],
        ),
    ] {
        let format = mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            alpha,
        );
        let input = EffectFloatFrame::from_pixels(
            mondrian_effects::EffectFrameExtent::new(1, 1),
            format,
            vec![sample],
        )
        .expect("exact complete input");
        let mut session = OpenFxFilterSession::prepare(
            product_executable(),
            &inspection,
            "uk.co.thefoundry.BasicGainPlugin",
            1,
            1,
            format,
            mondrian_effects::EffectProjectGeometry::new(
                1,
                1,
                mondrian_core::SampleAspectRatio::SQUARE,
            )
            .expect("explicit owner project geometry"),
            &mut || Ok(()),
        )
        .expect("typed session");
        let parameters = constant_parameters([
            ("scale".into(), OpenFxParameterValue::Double(1.0)),
            ("alphaMode".into(), OpenFxParameterValue::Choice(mode)),
        ]);
        let worker = session.worker_process_id();
        let output = session
            .render(&cpu_contract_timing(1.0), &parameters, &input, &mut || {
                Ok(())
            })
            .expect("independent plugin checked Source clip and image association");
        assert_eq!(output.alpha(), expected_alpha);
        assert_eq!(
            output.pixels()[0].map(f32::to_bits),
            sample.map(f32::to_bits),
            "associated emission must never be divided through straight storage"
        );
        let changed_alpha = if alpha == ColorFrameAlpha::StraightFloat {
            ColorFrameAlpha::PremultipliedFloat
        } else {
            ColorFrameAlpha::StraightFloat
        };
        let rebound = EffectFloatFrame::from_pixels(
            input.extent(),
            format.with_alpha(changed_alpha),
            vec![sample],
        )
        .expect("same bytes with a different declaration");
        assert!(matches!(
            session.render(
                &cpu_contract_timing(1.0),
                &parameters,
                &rebound,
                &mut || Ok(())
            ),
            Err(OpenFxHostError::Invalid(_))
        ));
        assert_eq!(
            session.worker_process_id(),
            worker,
            "caller format mismatch is not a plugin fault"
        );
        session
            .render(&cpu_contract_timing(1.0), &parameters, &input, &mut || {
                Ok(())
            })
            .expect("unchanged valid session remains reusable");
        session.shutdown().expect("complete native lifecycle");
    }
}

#[test]
#[ignore = "requires current product and SessionProbe output_alpha mode"]
fn raw_native_float_survives_disk_cache_nested_over_and_real_opaque_output() {
    use mondrian_core::{ColorFrameAlpha, ColorFrameAlphaFormats, WorkingRgbaF32Frame};
    use mondrian_render_cache::{
        TimelineRenderCacheConfig, TimelineRenderCacheFormat, TimelineRenderCacheFrame,
        TimelineRenderCacheIdentity, TimelineRenderCacheLookup, TimelineRenderCacheResult,
        TimelineRenderCacheService, TimelineRenderCacheSubmission,
    };
    use mondrian_renderer::{
        CpuColorFrame, TimelineCompositeBackground, TimelineCompositeElement,
        TimelineCompositeOptions, TimelineCompositeScratch, TimelineEffectColorRuntime,
        TimelineMediaLayer,
    };
    fn wait(service: &TimelineRenderCacheService) -> TimelineRenderCacheResult {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(result) = service.try_poll() {
                return result;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "bounded cache worker did not return its terminal"
            );
            std::thread::yield_now();
        }
    }
    mondrian_core::ensure_mondrian_default_ocio_loaded().expect("stock OCIO");
    let inspection = cpu_contract_probe("output_alpha");
    let input = output_alpha_input();
    let mut native = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        input.width(),
        input.height(),
        input.format(),
        mondrian_effects::EffectProjectGeometry::new(
            input.width(),
            input.height(),
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut || Ok(()),
    )
    .expect("native Float session");
    let raw = native
        .render(
            &cpu_contract_timing(1.0),
            &constant_parameters([
                ("scale".into(), OpenFxParameterValue::Double(1.0)),
                ("alphaMode".into(), OpenFxParameterValue::Choice(8)),
            ]),
            &input,
            &mut || Ok(()),
        )
        .expect("finite native zero-alpha emission");
    assert_eq!(raw.alpha(), ColorFrameAlpha::PremultipliedFloat);
    let raw_pixels = raw.pixels().to_vec();
    let mut canonical = vec![raw.alpha().identity_code()];
    for sample in &raw_pixels {
        for component in sample {
            canonical.extend_from_slice(&component.to_bits().to_le_bytes());
        }
    }
    let identity = TimelineRenderCacheIdentity::for_resolved_visual(
        mondrian_core::ResolvedVisualFrameIdentity::from_materialization(
            mondrian_core::ResolvedVisualNodeMaterializationIdentity::from_canonical_bytes(
                &canonical,
            ),
        ),
        raw.width(),
        raw.height(),
        TimelineRenderCacheFormat::LosslessRgba32FloatZstd,
        ColorFrameAlphaFormats::ALL,
    );
    // This explicit fixture identity is exhaustive over actual raster bytes;
    // it does not grant production caching to an uncacheable native graph.
    let temp = tempfile::tempdir().expect("cache owner");
    let config = TimelineRenderCacheConfig::new(temp.path().to_path_buf(), 1_048_576, 1_048_576, 2)
        .expect("bounded cache policy");
    let cache = TimelineRenderCacheService::start(config.clone()).expect("cache worker");
    let cached = TimelineRenderCacheFrame::new(
        identity,
        WorkingRgbaF32Frame {
            width: raw.width(),
            height: raw.height(),
            alpha: raw.alpha(),
            data: raw_pixels.clone(),
            color_space: WorkingColorSpace::LinearRec709,
        },
    )
    .expect("complete artifact");
    assert_eq!(
        cache.publish(cached),
        TimelineRenderCacheSubmission::Scheduled
    );
    assert!(
        matches!(wait(&cache), TimelineRenderCacheResult::Published { identity: published, .. } if published == identity)
    );
    cache.shutdown_and_wait();
    let reopened = TimelineRenderCacheService::start(config).expect("independent disk reader");
    assert_eq!(
        reopened.lookup(identity, WorkingColorSpace::LinearRec709),
        TimelineRenderCacheSubmission::Scheduled
    );
    let TimelineRenderCacheResult::Lookup { result: TimelineRenderCacheLookup::Hit(read), .. } =
        wait(&reopened)
    else {
        panic!("current artifact must be read from disk");
    };
    assert_eq!(read.frame().alpha, ColorFrameAlpha::PremultipliedFloat);
    for (actual, expected) in read.frame().data.iter().zip(&raw_pixels) {
        assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
    }
    reopened.shutdown_and_wait();
    let source = CpuColorFrame::working(read.frame().clone());
    let identity_graph = PreparedEffectProgram::prepare(&[], &[], WorkingColorSpace::LinearRec709)
        .expect("identity definition")
        .evaluate(TimelineTime::ZERO)
        .expect("sole compiled identity");
    let engine = mondrian_core::ColorEngine::mondrian_standard();
    let runtime = TimelineEffectColorRuntime::new(&engine, WorkingColorSpace::LinearRec709);
    let mut scratch = TimelineCompositeScratch::default();
    let nested_layer = TimelineMediaLayer {
        frame: &source,
        opacity: 1.0,
        blend_mode: mondrian_core::types::BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph: identity_graph.clone(),
        frame_seed: 0,
    };
    let nested = mondrian_renderer::composite_timeline_elements_color_frame(
        raw.width(),
        raw.height(),
        &[TimelineCompositeElement::Media(nested_layer)],
        TimelineCompositeOptions::default(),
        runtime,
        &mut scratch,
        &mut mondrian_effects::EffectProcessorControl::uncancelled(),
    )
    .expect("transparent nested working value");
    assert_eq!(
        nested.descriptor().alpha,
        ColorFrameAlpha::PremultipliedFloat
    );
    assert_eq!(nested.rgba_f32().data, raw_pixels);
    let parent = TimelineMediaLayer {
        frame: &nested,
        opacity: 0.25,
        blend_mode: mondrian_core::types::BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph: identity_graph,
        frame_seed: 0,
    };
    let delivered = mondrian_renderer::composite_timeline_elements_color_frame(
        raw.width(),
        raw.height(),
        &[TimelineCompositeElement::Media(parent)],
        TimelineCompositeOptions {
            background: TimelineCompositeBackground::OpaqueBlack,
        },
        runtime,
        &mut scratch,
        &mut mondrian_effects::EffectProcessorControl::uncancelled(),
    )
    .expect("explicit opaque delivery");
    let expected = raw_pixels
        .iter()
        .map(|sample| {
            [
                (f64::from(sample[0]) * 0.25) as f32,
                (f64::from(sample[1]) * 0.25) as f32,
                (f64::from(sample[2]) * 0.25) as f32,
                1.0,
            ]
        })
        .collect::<Vec<_>>();
    assert_eq!(delivered.descriptor().alpha, ColorFrameAlpha::Opaque);
    assert_eq!(delivered.rgba_f32().data, expected);
    let transform = mondrian_renderer::RenderColorTransform::export(
        mondrian_core::ColorSpace::Rec709,
        false,
        engine,
    );
    let actual = mondrian_renderer::CpuColorTransformExecutor::transform(&delivered, &transform)
        .expect("actual stock OCIO encoder");
    let oracle = CpuColorFrame::working(WorkingRgbaF32Frame {
        width: raw.width(),
        height: raw.height(),
        alpha: ColorFrameAlpha::Opaque,
        data: expected,
        color_space: WorkingColorSpace::LinearRec709,
    });
    let expected_output =
        mondrian_renderer::CpuColorTransformExecutor::transform(&oracle, &transform)
            .expect("independent f64-law raster to delivery");
    assert_eq!(actual.frame.rgba(), expected_output.frame.rgba());
    assert!(actual.frame.rgba().chunks_exact(4).all(|pixel| pixel[3] == 255));
    native.shutdown().expect("native teardown after complete chain");
}

#[test]
#[ignore = "set SessionProbe binary and MONDRIAN_OPENFX_SESSION_PROBE_MODE=image_uid"]
fn product_actual_image_identifiers_invalidate_cached_analysis_across_equal_time_and_seeks() {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("UID probe mode"),
        "image_uid"
    );
    product_actual_image_metadata_frames();
}

#[test]
#[ignore = "set SessionProbe binary and MONDRIAN_OPENFX_SESSION_PROBE_MODE=image_field"]
fn product_actual_progressive_image_metadata_reports_field_none() {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("field probe mode"),
        "image_field"
    );
    product_actual_image_metadata_frames();
}

fn product_actual_image_metadata_frames() {
    let binary = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("owned actual-image probe"),
    );
    let inspection = inspect_openfx_binary(product_executable(), &binary)
        .expect("inspect actual probe through supervised product");
    let cancellation = mondrian_core::ExecutionCancellationToken::new();
    let format = mondrian_effects::EffectValueFormat::new(
        mondrian_effects::EffectWorkingPrecision::Float32,
        mondrian_effects::EffectColorDomain::SceneLinearRgb,
        mondrian_core::ColorFrameAlpha::StraightCoverage,
    );
    let mut session = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        1,
        1,
        format,
        mondrian_effects::EffectProjectGeometry::new(
            1,
            1,
            mondrian_core::SampleAspectRatio::new(2, 1).unwrap(),
        )
        .expect("explicit owner project geometry"),
        &mut checkpoint(&cancellation),
    )
    .expect("prepare bounded actual-image worker");
    let worker = session.worker_process_id();
    let mut timing = OpenFxRenderTiming {
        frame: 0.0,
        frame_rate: 24.0,
        first_frame: 0.0,
        last_frame: 100.0,
        pixel_aspect_ratio: 2.0,
    };
    for (time, pixel) in [
        (0.0, [0.25, -0.5, 1.5, 0.75]),
        (0.0, [0.5, -0.25, 2.0, 0.75]),
        (42.0, [0.125, -1.0, 4.0, 0.75]),
        (0.0, [0.75, -0.125, 8.0, 0.75]),
    ] {
        timing.frame = time;
        let frame = mondrian_effects::EffectFloatFrame::from_pixels(
            mondrian_effects::EffectFrameExtent::new(1, 1),
            format,
            vec![pixel],
        )
        .expect("actual finite source frame");
        let output = session
            .render(
                &timing,
                &constant_parameters([("scale".into(), OpenFxParameterValue::Double(1.0))]),
                &frame,
                &mut checkpoint(&cancellation),
            )
            .expect("native actual-image metadata and cached analysis are valid");
        assert_eq!(
            output.pixels()[0].map(f32::to_bits),
            pixel.map(f32::to_bits),
            "independent raster oracle across changed input and seek"
        );
        assert_eq!(
            output.alpha(),
            mondrian_core::ColorFrameAlpha::StraightFloat
        );
        assert_eq!(
            session.worker_process_id(),
            worker,
            "valid metadata never retires the worker"
        );
    }
    session.shutdown().expect("destroy native image owner and reap worker");
}

#[test]
#[ignore = "set owned SessionProbe mode mutex_contract"]
fn product_native_mutex_suite_retains_worker_and_correct_pixels() {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("mutex probe mode"),
        "mutex_contract"
    );
    product_actual_image_metadata_frames();
}
#[test]
#[ignore = "set owned SessionProbe mode thread_contract"]
fn product_native_thread_suite_retains_worker_and_correct_pixels() {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("thread probe mode"),
        "thread_contract"
    );
    product_actual_image_metadata_frames();
}
#[test]
#[ignore = "set owned SessionProbe mode thread_concurrent"]
fn product_native_thread_suite_preserves_concurrent_completion_ownership() {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("concurrent thread probe mode"),
        "thread_concurrent"
    );
    product_actual_image_metadata_frames();
}
#[test]
#[ignore = "set owned SessionProbe mode thread_hang"]
fn product_cancellation_reaps_stuck_owned_callback_thread() {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("thread hang mode"),
        "thread_hang"
    );
    resident_worker_cancellation_retires_and_reaps_a_hung_native_render();
}

#[test]
#[ignore = "requires current product, SessionProbe image_uid_persisted and a fresh MONDRIAN_OPENFX_IMAGE_ANALYSIS_FILE"]
fn product_workers_bind_parent_namespace_and_restore_plugin_private_analysis_correctly() {
    let inspection = cpu_contract_probe("image_uid_persisted");
    let path = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_IMAGE_ANALYSIS_FILE")
            .expect("fresh owned private cache file"),
    );
    assert!(
        !path.exists(),
        "qualification owns a fresh plugin-private analysis record"
    );
    let parameters = constant_parameters([("scale".to_owned(), OpenFxParameterValue::Double(2.0))]);
    let mut previous = None;
    for pixel in [
        [0.25, -0.5, 1.5, 0.25],
        [5.0, -7.0, 11.0, 0.75],
        [0.25, -0.5, 1.5, 0.25],
    ] {
        let input = mondrian_effects::EffectFloatFrame::from_pixels(
            mondrian_effects::EffectFrameExtent::new(1, 1),
            mondrian_effects::EffectValueFormat::new(
                mondrian_effects::EffectWorkingPrecision::Float32,
                mondrian_effects::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightCoverage,
            ),
            vec![pixel],
        )
        .expect("owned source");
        let mut session = OpenFxFilterSession::prepare(
            product_executable(),
            &inspection,
            "uk.co.thefoundry.BasicGainPlugin",
            1,
            1,
            input.format(),
            mondrian_effects::EffectProjectGeometry::new(
                1,
                1,
                mondrian_core::SampleAspectRatio::SQUARE,
            )
            .expect("explicit owner project geometry"),
            &mut || Ok(()),
        )
        .expect("fresh parent lifetime");
        let owner = session.processor_owner();
        let output = session
            .render(&cpu_contract_timing(1.0), &parameters, &input, &mut || {
                Ok(())
            })
            .expect("correct analysis after real process/cache reopen");
        assert_eq!(output.pixels(), [pixel.map(|channel| channel * 2.0)]);
        let record = std::fs::read(&path).expect("actual plugin-private persisted analysis");
        let length = u32::from_ne_bytes(record[..4].try_into().expect("record length")) as usize;
        let uid = std::str::from_utf8(&record[4..4 + length]).expect("actual UID").to_owned();
        assert!(
            uid.starts_with(&format!("mondrian-ofx:{}:", owner.0.simple())),
            "the parent Prepare owner reaches the actual native image: {uid}"
        );
        if let Some((old_owner, old_uid)) = previous {
            assert_ne!(owner, old_owner);
            assert_ne!(uid, old_uid);
        }
        previous = Some((owner, uid));
        session.shutdown().expect("actual owner teardown before reprepare/reopen");
    }
}
fn qualify_product_legacy_render_priority(selected: &str, recoverable: bool) {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").as_deref(),
        Ok(selected)
    );
    let binary = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("priority fixture"),
    );
    let inspection =
        inspect_openfx_binary(product_executable(), &binary).expect("actual product discovery");
    let format = mondrian_effects::EffectValueFormat::new(
        mondrian_effects::EffectWorkingPrecision::Float32,
        mondrian_effects::EffectColorDomain::SceneLinearRgb,
        mondrian_core::ColorFrameAlpha::StraightFloat,
    );
    let input = mondrian_effects::EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(2, 2),
        format,
        vec![
            [-0.0, -0.25, 2.0, 0.0],
            [2.0, -4.0, 8.0, f32::MIN_POSITIVE],
            [-0.5, 0.75, 4.0, 0.25],
            [1.0, -2.0, 0.5, 1.0],
        ],
    )
    .expect("independent typed Source");
    let before = input.pixels().iter().map(|pixel| pixel.map(f32::to_bits)).collect::<Vec<_>>();
    let timing = OpenFxRenderTiming {
        frame: 3.5,
        frame_rate: 24.0,
        first_frame: 0.0,
        last_frame: 100.0,
        pixel_aspect_ratio: 1.5,
    };
    for _ in 0..2 {
        let mut session = OpenFxFilterSession::prepare(
            product_executable(),
            &inspection,
            "uk.co.thefoundry.BasicGainPlugin",
            2,
            2,
            format,
            mondrian_effects::EffectProjectGeometry::new(
                2,
                2,
                mondrian_core::SampleAspectRatio::SQUARE,
            )
            .expect("explicit owner project geometry"),
            &mut || Ok(()),
        )
        .expect("actual native owner");
        let pid = session.worker_process_id();
        assert!(pid > 0);
        let values = constant_parameters([("scale".to_owned(), OpenFxParameterValue::Double(1.5))]);
        let result = session.render(&timing, &values, &input, &mut || Ok(()));
        if recoverable {
            let Err(OpenFxHostError::UserBlocked(first)) = result else {
                panic!("complete frame plus Error remains recoverable: {result:?}");
            };
            assert!(first.snapshot.is_blocked());
            assert_eq!(
                first.snapshot.persistent().expect("actual Error").id(),
                "render.repair"
            );
            assert_eq!(
                first.snapshot.persistent().expect("actual Error").text(),
                "Change scale to recover render"
            );
            assert!(
                session.take_feedback().is_none(),
                "blocked private output is not successful frame"
            );
            let again = session.render(&timing, &values, &input, &mut || Ok(()));
            let Err(OpenFxHostError::UserBlocked(second)) = again else {
                panic!("unchanged author remains blocked: {again:?}");
            };
            assert_eq!(second.owner, first.owner);
            assert_eq!(
                second.snapshot.native_owner(),
                first.snapshot.native_owner()
            );
            assert_eq!(
                second.snapshot.persistent_revision(),
                first.snapshot.persistent_revision()
            );
            let repaired =
                constant_parameters([("scale".to_owned(), OpenFxParameterValue::Double(2.0))]);
            let output = session
                .render(&timing, &repaired, &input, &mut || Ok(()))
                .expect("same owner author repair");
            assert_eq!(output.extent(), input.extent());
            assert_eq!(output.format(), input.format());
            for (actual, source) in output.pixels().iter().zip(input.pixels()) {
                for (actual, source) in actual.iter().zip(source) {
                    assert_eq!(actual.to_bits(), (source * 2.0).to_bits());
                }
            }
            let feedback = session.take_feedback().expect("repaired successful publication");
            assert_eq!(feedback.owner, first.owner);
            assert_eq!(
                feedback.snapshot.native_owner(),
                first.snapshot.native_owner()
            );
            assert!(!feedback.snapshot.is_blocked());
            assert!(feedback.snapshot.persistent().is_none());
        } else {
            let Err(OpenFxHostError::WorkerFailed(detail)) = result else {
                panic!(
                    "{selected}: native failure cannot become recoverable UserBlocked: {result:?}"
                );
            };
            assert_eq!(detail, "native status 5");
            assert!(
                session.take_feedback().is_none(),
                "rejected frame cannot publish successful feedback"
            );
            let again = session.render(&timing, &values, &input, &mut || Ok(()));
            let Err(OpenFxHostError::Unavailable(reason)) = again else {
                panic!("{selected}: rejected native owner must remain unusable: {again:?}");
            };
            assert_eq!(reason, "native session has been retired");
        }
        assert_eq!(session.worker_process_id(), pid, "no replacement owner");
        assert_eq!(
            input.pixels().iter().map(|pixel| pixel.map(f32::to_bits)).collect::<Vec<_>>(),
            before
        );
        session.shutdown().expect("actual owner cleanup settles or reports failure");
        session.shutdown().expect("settled owner cleanup is idempotent");
    }
}

#[test]
#[ignore = "requires actual product and owned legacy Render conjunction mode"]
fn product_native_render_failures_retire_owner_despite_persistent_error() {
    let selected =
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("controlled native mode");
    assert!(matches!(
        selected.as_str(),
        "legacy_priority_render_fatal_error"
            | "legacy_priority_roi_failed_error"
            | "legacy_priority_end_failed_error"
    ));
    qualify_product_legacy_render_priority(&selected, false);
}

#[test]
#[ignore = "requires actual product and owned legacy_priority_error_only mode"]
fn product_complete_render_error_recovers_same_native_owner() {
    qualify_product_legacy_render_priority("legacy_priority_error_only", true);
}

#[test]
#[ignore = "requires current product and owned SessionProbe aux_error bundle"]
fn native_persistent_error_repairs_through_real_effects_residency_without_owner_replacement() {
    use mondrian_effects::{
        EffectExecutionError, EffectExecutionSession, EffectExecutionSessionConfig,
        EffectFloatExecutionError, EffectProcessorControl, EffectProcessorExecutionOrigin,
        EffectProcessorRetention,
    };
    let inspection = cpu_contract_probe("aux_error");
    let kind = register_selected_openfx_filter(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
    )
    .expect("native definition");
    let mondrian_effects::EffectType::External(ref provider_key) = kind else {
        panic!("native provider key");
    };
    let mut effect = instantiate_effect_node(kind.clone()).expect("actual authored occurrence");
    let description = describe_openfx_filter(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
    )
    .expect("actual native author schema");
    let scale_label = &description
        .parameters
        .iter()
        .find(|parameter| parameter.name == "scale")
        .expect("official Basic scale")
        .label;
    let scale = effect
        .properties
        .iter()
        .find(|(_, property)| &property.descriptor.display_name == scale_label)
        .map(|(_, property)| property.descriptor.parameter_id().clone())
        .expect("actual parameter id");
    // One author occurrence survives a genuine parameter revision; it is not
    // re-instantiated from the new graph or replaced by an App-held instance.
    effect
        .set_static_value_by_parameter(&scale, PropertyValue::Double(1.0))
        .expect("blocked author state");
    let compile = |node: &mondrian_effects::EffectNode| {
        let frame_context = EffectFrameContext::new(
            Rational::FPS_24,
            mondrian_effects::EffectProjectGeometry::new(2, 2, SampleAspectRatio::SQUARE)
                .expect("explicit owner project geometry"),
            TimelineTimeRange::new(
                TimelineTime::ZERO,
                TimelineTime::new(10, 24).expect("canonical available duration"),
            )
            .expect("canonical nonempty owner range"),
        )
        .expect("canonical sequence frame context");
        PreparedEffectProgram::prepare_hierarchical_with_frame_context(
            std::slice::from_ref(node),
            &[],
            &[],
            &[],
            WorkingColorSpace::LinearRec709,
            frame_context,
            &LutPreparationCache::uncached(),
        )
        .expect("author program")
        .evaluate(TimelineTime::ZERO)
        .expect("canonical graph")
    };
    let input = EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(1, 1),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightCoverage,
        ),
        vec![[0.25, -0.5, 1.5, 0.25]],
    )
    .expect("complete frame");
    for optional_count in [1, 0] {
        let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_processor_instances: optional_count,
            max_processor_bytes: 64 * 1024 * 1024,
            max_working_bytes: 128 * 1024 * 1024,
            max_cache_bytes: 0,
            max_cache_entries: 0,
            ..Default::default()
        });
        session.bind_generation(7);
        let mut control = EffectProcessorControl::uncancelled();
        let Err(EffectFloatExecutionError::CustomProcessor(
            EffectExecutionError::ProcessorUserBlocked { receipt: blocked, .. },
        )) = session.apply_compiled_rgba_f32(input.view(), &compile(&effect), 0, &mut control)
        else {
            panic!("actual persistent Error must return recoverable typed receipt");
        };
        assert!(blocked.feedback.snapshot.is_blocked());
        assert_eq!(blocked.preparation.effect_id, blocked.effect_id);
        assert_eq!(blocked.feedback.snapshot.invocation(), 1);
        assert_eq!(
            blocked.retention,
            if optional_count == 1 {
                EffectProcessorRetention::Retained
            } else {
                EffectProcessorRetention::Evicted
            }
        );
        assert_eq!(session.diagnostics().processor_instances, optional_count);
        assert_eq!(
            input.pixels(),
            [[0.25, -0.5, 1.5, 0.25]],
            "no staged pixels published on blocked work"
        );
        let EffectProcessorExecutionOrigin::SupervisedChild { process_id: blocked_pid } =
            blocked.feedback.origin
        else {
            panic!("actual child PID required");
        };
        let owner = blocked.feedback.owner;
        let revision = blocked.provider_revision;
        let mut repaired = effect.clone();
        repaired
            .set_static_value_by_parameter(&scale, PropertyValue::Double(2.0))
            .expect("genuine authored repair");
        session.bind_generation(8);
        let mut control = EffectProcessorControl::uncancelled();
        let output = session
            .apply_compiled_rgba_f32(input.view(), &compile(&repaired), 1, &mut control)
            .expect("InstanceChanged cleared Error before exactly one Render");
        assert_eq!(
            output.pixels(),
            [[0.5, -1.0, 3.0, 0.5]],
            "official Basic analytic output; no advanced-DSP replay"
        );
        let reports = control.take_feedback().expect("pre-admitted feedback slots");
        assert_eq!(reports.len(), 1);
        let recovered = reports.iter().next().expect("actual recovered feedback");
        assert!(!recovered.feedback.snapshot.is_blocked());
        assert!(recovered.feedback.snapshot.persistent().is_none());
        assert_eq!(recovered.effect_id, blocked.effect_id);
        assert_eq!(
            recovered.provider_revision, revision,
            "author parameters are not immutable provider replacement"
        );
        let EffectProcessorExecutionOrigin::SupervisedChild { process_id: recovered_pid } =
            recovered.feedback.origin
        else {
            panic!("actual child PID required");
        };
        if optional_count == 1 {
            assert_eq!(recovered.feedback.owner, owner);
            assert_eq!(
                recovered_pid, blocked_pid,
                "same actual valid resident child, not UI inference"
            );
            assert_eq!(recovered.feedback.snapshot.invocation(), 2);
            assert!(
                recovered.feedback.snapshot.persistent_revision()
                    > blocked.feedback.snapshot.persistent_revision()
            );
        } else {
            assert_ne!(
                recovered.feedback.owner, owner,
                "an evicted owner cannot be resurrected"
            );
            // OS may legally recycle a reaped PID. Parent-owned UUID is the
            // unambiguous new processor lifetime identity, not PID inequality.
            assert_eq!(recovered.feedback.snapshot.invocation(), 1);
            assert_eq!(
                recovered.feedback.snapshot.persistent_revision(),
                0,
                "new valid author state did not clear another owner's private Error"
            );
            assert_eq!(recovered.retention, EffectProcessorRetention::Evicted);
        }
        assert_eq!(session.diagnostics().processor_instances, optional_count);
        let status = mondrian_effects::effect_extension_runtime_status(provider_key)
            .expect("registry state");
        assert!(
            !status.disabled,
            "persistent user error is not plugin quarantine"
        );
        assert!(status.last_error.is_none());
        session.retire_processor_resources();
        assert_eq!(session.diagnostics().processor_instances, 0);
    }
}

#[test]
#[ignore = "requires current product and owned SessionProbe aux_transient_instance bundle"]
fn native_log_messages_reach_product_tracing_with_exact_supervised_owner() {
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::Layer;

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<BTreeMap<String, String>>>>);
    struct Fields(BTreeMap<String, String>);
    impl tracing::field::Visit for Fields {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.insert(field.name().to_owned(), value.to_owned());
        }
        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            self.0.insert(field.name().to_owned(), value.to_string());
        }
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.insert(field.name().to_owned(), format!("{value:?}"));
        }
    }
    impl<S: tracing::Subscriber> Layer<S> for Capture {
        fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
            if event.metadata().target() == "mondrian.openfx.plugin" {
                let mut fields = Fields(BTreeMap::new());
                event.record(&mut fields);
                self.0.lock().expect("test capture").push(fields.0);
            }
        }
    }

    let inspection = cpu_contract_probe("aux_transient_instance");
    let input = EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(1, 1),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightCoverage,
        ),
        vec![[0.25, -0.5, 1.5, 0.25]],
    )
    .expect("complete independent frame");
    let mut session = mondrian_app::openfx_host::OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        1,
        1,
        input.format(),
        mondrian_effects::EffectProjectGeometry::new(
            1,
            1,
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut || Ok(()),
    )
    .expect("actual supervised owner");
    let owner = session.processor_owner().to_string();
    let process_id = session.worker_process_id().to_string();
    let records = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(Capture(Arc::clone(&records)));
    tracing::subscriber::with_default(subscriber, || {
        for invocation in [1_u64, 2] {
            let timing = OpenFxRenderTiming {
                frame: (invocation - 1) as f64,
                frame_rate: 24.0,
                first_frame: 0.0,
                last_frame: 100.0,
                pixel_aspect_ratio: 2.0,
            };
            let output = session
                .render(&timing, &BTreeMap::new(), &input, &mut || Ok(()))
                .expect("real official Basic gain render");
            assert_eq!(
                output.pixels(),
                input.pixels(),
                "independent unit-gain native output"
            );
            let feedback = session.take_feedback().expect("actual complete typed report");
            assert_eq!(feedback.owner.to_string(), owner);
            assert_eq!(feedback.snapshot.invocation(), invocation);
            assert_eq!(feedback.snapshot.notices().len(), 6);
        }
    });
    let records = records.lock().expect("captured parent diagnostics");
    assert_eq!(records.len(), 4, "exactly the two genuine Log notices per invocation; information/warning are not log duplicates");
    for (index, fields) in records.iter().enumerate() {
        assert_eq!(fields.get("processor_owner"), Some(&owner));
        assert_eq!(fields.get("worker_process_id"), Some(&process_id));
        assert_eq!(
            fields.get("native_invocation"),
            Some(&(index / 2 + 1).to_string())
        );
        assert_eq!(fields.get("notice_id").map(String::as_str), Some(""));
        assert_eq!(
            fields.get("plugin_message").map(String::as_str),
            Some(if index % 2 == 0 {
                "说明🙂 7 1.250"
            } else {
                "第二条通知✓"
            })
        );
    }
    drop(records);
    session.shutdown().expect("same actual owner gracefully destroyed");
}

#[test]
#[ignore = "requires current product and owned cooperative_progress fixture"]
fn native_live_progress_flows_through_real_effects_residency_before_frame_completion() {
    use mondrian_effects::{
        EffectExecutionSession, EffectExecutionSessionConfig, EffectProcessorControl,
        EffectProcessorExecutionOrigin, EffectProcessorObservation, EffectProcessorObserver,
        EffectProcessorProgressReceipt,
    };
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct ActualConsumer {
        thread: std::thread::ThreadId,
        started: PathBuf,
        release: PathBuf,
        observed: Mutex<Option<EffectProcessorProgressReceipt>>,
    }
    impl EffectProcessorObserver for ActualConsumer {
        fn question_service(
            &self,
        ) -> Option<&dyn mondrian_effects::EffectProcessorQuestionService> {
            None
        }
        fn maximum_storage_bytes(&self) -> usize {
            // One receipt, plus the exact owned fixture paths/test state.
            EffectProcessorProgressReceipt::maximum_retained_bytes()
                + std::mem::size_of::<Self>()
                + self.started.as_os_str().len() * 2
                + self.release.as_os_str().len() * 2
        }
        fn observe(&self, receipt: &EffectProcessorProgressReceipt) {
            assert_eq!(
                std::thread::current().id(),
                self.thread,
                "product owner callback remains on the Effects execution thread"
            );
            if receipt.progress.progress.active() && receipt.progress.progress.fraction() == 0.375 {
                let started = std::fs::read_to_string(&self.started)
                    .expect("native Render already reached its independent receipt");
                assert_eq!(started.lines().count(), 1, "one stateful native action");
                let mut observed = self.observed.lock().expect("one bounded receipt");
                assert!(observed.is_none(), "the .375 observation is not replayed");
                *observed = Some(receipt.clone());
                std::fs::write(&self.release, b"continue this actual owner")
                    .expect("actual live consumer releases the fixture");
            }
        }
    }
    let inspection = cpu_contract_probe("cooperative_progress");
    let kind = register_selected_openfx_filter(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
    )
    .expect("actual immutable App factory");
    let mondrian_effects::EffectType::External(ref key) = kind else {
        panic!("actual provider");
    };
    let provider = key.clone();
    let effect = instantiate_effect_node(kind).expect("real authored occurrence");
    let frame_context = EffectFrameContext::new(
        Rational::FPS_24,
        mondrian_effects::EffectProjectGeometry::new(2, 2, SampleAspectRatio::SQUARE)
            .expect("explicit owner project geometry"),
        TimelineTimeRange::new(
            TimelineTime::ZERO,
            TimelineTime::new(10, 24).expect("canonical available duration"),
        )
        .expect("canonical nonempty owner range"),
    )
    .expect("canonical sequence frame context");
    let graph = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
        std::slice::from_ref(&effect),
        &[],
        &[],
        &[],
        WorkingColorSpace::LinearRec709,
        frame_context,
        &LutPreparationCache::uncached(),
    )
    .expect("one canonical author program")
    .evaluate(TimelineTime::ZERO)
    .expect("one compiled execution contract");
    let pixels = [[0.25_f32, -0.5, 1.5, 0.25]];
    let input = EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(1, 1),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightCoverage,
        ),
        pixels.to_vec(),
    )
    .expect("actual finite authored input");
    let sink = Arc::new(ActualConsumer {
        thread: std::thread::current().id(),
        started: PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_COOPERATIVE_STARTED")
                .expect("root-owned exact native receipt path"),
        ),
        release: PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_COOPERATIVE_RELEASE")
                .expect("root-owned exact continuation path"),
        ),
        observed: Mutex::new(None),
    });
    let mut control = EffectProcessorControl::uncancelled();
    control.bind_observation(EffectProcessorObservation {
        sink: Some(sink.clone()),
        deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(5)),
    });
    let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
        max_processor_instances: 1,
        max_processor_bytes: 64 * 1024 * 1024,
        max_working_bytes: 128 * 1024 * 1024,
        max_cache_bytes: 0,
        max_cache_entries: 0,
        ..Default::default()
    });
    let output = session
        .apply_compiled_rgba_f32(input.view(), &graph, 0, &mut control)
        .expect("live native consumer permits the sole real output");
    assert_eq!(
        output.pixels(),
        pixels,
        "independent unit-gain Basic oracle"
    );
    assert_eq!(
        input.pixels(),
        pixels,
        "no second author/raster interpretation"
    );
    let observed = sink.observed.lock().expect("bounded current fact");
    let live = observed.as_ref().expect("terminal-only receipt cannot release Render");
    let terminal = control.take_feedback().expect("canonical completed owner facts");
    assert_eq!(terminal.len(), 1);
    let terminal = terminal.iter().next().expect("one actual invocation");
    assert_eq!(live.preparation, terminal.preparation);
    assert_eq!(live.preparation.effect_id, effect.id);
    assert_eq!(live.provider_revision, terminal.provider_revision);
    assert_eq!(live.progress.owner, terminal.feedback.owner);
    assert_eq!(live.progress.origin, terminal.feedback.origin);
    assert_eq!(
        live.progress.invocation,
        terminal.feedback.snapshot.invocation()
    );
    assert_eq!(
        live.progress.native_owner,
        terminal.feedback.snapshot.native_owner()
    );
    let EffectProcessorExecutionOrigin::SupervisedChild { process_id } = live.progress.origin
    else {
        panic!("real child origin");
    };
    let started = std::fs::read_to_string(&sink.started).expect("independent native receipt");
    assert_eq!(started.lines().count(), 1);
    let (pid, uid) = started.trim_end().split_once('\t').expect("actual PID and Source UID");
    assert_eq!(
        pid.parse::<u32>().expect("actual receipt PID"),
        process_id.get()
    );
    assert!(uid.starts_with(&format!("mondrian-ofx:{}:", live.progress.owner.0.simple())));
    assert_eq!(
        session.diagnostics().processor_instances,
        1,
        "the sole bank retained its actual valid owner, not an App-held clone"
    );
    let status =
        mondrian_effects::effect_extension_runtime_status(&provider).expect("actual provider");
    assert!(!status.disabled);
    assert!(status.last_error.is_none());
    session.retire_processor_resources();
    assert_eq!(session.diagnostics().processor_instances, 0);
}

fn assert_fixed_canvas_literal(actual: &EffectFloatFrame, phase: usize) {
    // Independent top-down tables. The fixture writes in OFX pixel coordinates
    // through the actual negative row stride; no production helper is this oracle.
    const FULL: [[f32; 4]; 15] = [
        [-0.25, 2.5, 0.125, 0.5],
        [-0.125, 2.75, 0.1875, 0.5],
        [0.0, 3.0, 0.25, 0.5],
        [-0.3125, 2.375, 0.15625, 0.5],
        [-0.1875, 2.625, 0.21875, 0.5],
        [-0.0625, 2.875, 0.28125, 0.5],
        [-0.375, 2.25, 0.1875, 0.5],
        [-0.25, 2.5, 0.25, 0.5],
        [-0.125, 2.75, 0.3125, 0.5],
        [-0.4375, 2.125, 0.21875, 0.5],
        [-0.3125, 2.375, 0.28125, 0.5],
        [-0.1875, 2.625, 0.34375, 0.5],
        [-0.5, 2.0, 0.25, 0.5],
        [-0.375, 2.25, 0.3125, 0.5],
        [-0.25, 2.5, 0.375, 0.5],
    ];
    const SUBSET: [[f32; 4]; 15] = [
        [0.0; 4],
        [0.0; 4],
        [0.0; 4],
        [0.0; 4],
        [0.0; 4],
        [0.0; 4],
        [0.0; 4],
        [1.625, -0.375, 0.5625, 0.25],
        [1.75, -0.375, 0.59375, 0.25],
        [0.0; 4],
        [1.625, -0.3125, 0.546875, 0.25],
        [1.75, -0.3125, 0.578125, 0.25],
        [0.0; 4],
        [0.0; 4],
        [0.0; 4],
    ];
    const EMPTY: [[f32; 4]; 15] = [[0.0; 4]; 15];
    let expected = match phase {
        0 => &FULL,
        1 => &SUBSET,
        2 => &EMPTY,
        _ => panic!("only the three actual frame times are admitted"),
    };
    assert_eq!((actual.width(), actual.height()), (3, 5));
    assert_eq!(
        actual.alpha(),
        mondrian_core::ColorFrameAlpha::StraightFloat
    );
    assert_eq!(actual.pixels().len(), expected.len());
    for (index, (actual, expected)) in actual.pixels().iter().zip(expected).enumerate() {
        assert_eq!(
            actual.map(f32::to_bits),
            expected.map(f32::to_bits),
            "phase {phase}, pixel {index}: signed/HDR channels and transparent clear must be exact"
        );
    }
}

#[test]
#[ignore = "requires current Product, SessionProbe identity_current and an empty action-receipt directory"]
fn product_worker_obeys_current_source_identity_and_default_render() {
    use std::io::Read;
    let directory = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_DESCRIBE_ACTION_RECEIPT_DIRECTORY")
            .expect("new empty native action directory"),
    );
    assert!(directory.is_absolute() && directory.is_dir());
    assert!(std::fs::read_dir(&directory).expect("action directory").next().is_none());
    let inspection = cpu_contract_probe("identity_current");
    assert_eq!(inspection.plugins.len(), 1);
    // Independent dyadic tables: signed/HDR RGB, unequal rows/channels and
    // varied straight alpha (including nonzero RGB at alpha zero).
    const SOURCES: [[[f32; 4]; 6]; 3] = [
        [
            [-0.5, 2.0, 0.125, 0.5],
            [3.0, -2.0, 0.25, 0.25],
            [0.125, 8.0, -4.0, 0.0],
            [1.5, -0.25, 4.0, 1.0],
            [-8.0, 0.5, 2.0, 0.75],
            [2.0, -4.0, 0.0625, 0.125],
        ],
        [
            [2.0, -1.0, 0.5, 0.5],
            [-0.25, 4.0, 2.0, 0.25],
            [8.0, -2.0, 0.125, 0.0],
            [-4.0, 1.5, 0.25, 1.0],
            [0.5, -8.0, 4.0, 0.75],
            [1.0, -0.125, -2.0, 0.125],
        ],
        [
            [-2.0, 0.125, 8.0, 0.25],
            [0.25, 2.0, -0.5, 0.5],
            [4.0, -8.0, 1.0, 1.0],
            [0.5, 1.5, -4.0, 0.0],
            [-1.0, 4.0, 0.0625, 0.125],
            [8.0, -0.5, 2.0, 0.75],
        ],
    ];
    const HALF: [[f32; 4]; 6] = [
        [1.0, -0.5, 0.25, 0.25],
        [-0.125, 2.0, 1.0, 0.125],
        [4.0, -1.0, 0.0625, 0.0],
        [-2.0, 0.75, 0.125, 0.5],
        [0.25, -4.0, 2.0, 0.375],
        [0.5, -0.0625, -1.0, 0.0625],
    ];
    let format = cpu_contract_input().format();
    let mut session = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        3,
        2,
        format,
        mondrian_effects::EffectProjectGeometry::new(
            3,
            2,
            mondrian_core::SampleAspectRatio::new(2, 1).unwrap(),
        )
        .expect("explicit owner project geometry"),
        &mut || Ok(()),
    )
    .expect("one actual current-source identity resident");
    let pid = session.worker_process_id();
    for (phase, pixels) in SOURCES.into_iter().enumerate() {
        let input = EffectFloatFrame::from_pixels(
            mondrian_effects::EffectFrameExtent::new(3, 2),
            format,
            pixels.to_vec(),
        )
        .expect("literal Float32 straight Source");
        let original = input.clone();
        let timing = OpenFxRenderTiming { frame: phase as f64, ..cpu_contract_timing(2.0) };
        let parameters = constant_parameters([(
            "scale".to_owned(),
            OpenFxParameterValue::Double(if phase == 1 { 0.5 } else { 1.0 }),
        )]);
        let output = session
            .render(&timing, &parameters, &input, &mut || Ok(()))
            .expect("identity, default Render, then identity on the same resident");
        assert_eq!(session.worker_process_id(), pid);
        assert_eq!(
            (output.width(), output.height(), output.format()),
            (3, 2, format)
        );
        let expected = if phase == 1 { &HALF } else { &pixels };
        assert_eq!(output.pixels().len(), expected.len());
        for (index, (actual, expected)) in output.pixels().iter().zip(expected).enumerate() {
            assert_eq!(
                actual.map(f32::to_bits),
                expected.map(f32::to_bits),
                "phase {phase}, pixel {index}"
            );
        }
        assert_eq!(input, original, "supplied Source remains immutable");
    }
    let read_actions = || -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        std::fs::File::open(directory.join(format!("actions-{pid}.txt")))?
            .take(4097)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    // Save the actual action facts without panicking, then always attempt the
    // original cleanup before the target assertion. Keep a simultaneous old
    // shutdown failure visible rather than mislabeling it as this target RED.
    let action_bytes = read_actions();
    let shutdown = session.shutdown();
    let bytes = action_bytes.expect("bounded actual resident receipt, not a discovery PID");
    assert!(!bytes.is_empty() && bytes.len() <= 4096 && bytes.is_ascii());
    let text = std::str::from_utf8(&bytes).expect("fixed ASCII rows");
    assert!(text.ends_with('\n'), "no incomplete final row");
    let rows: Vec<_> = text.lines().collect();
    assert!(rows.len() <= 32);
    let mut ordered = Vec::new();
    let mut queries = 0;
    let mut renders = 0;
    for (index, row) in rows.iter().enumerate() {
        let fields: Vec<_> = row.split_whitespace().collect();
        assert_eq!(fields.len(), 9);
        assert_eq!(fields[0].parse::<usize>().expect("ordinal"), index + 1);
        assert_eq!(&fields[1..3], ["D=0", "I=0"]);
        assert_eq!(fields[8], "complete=1");
        if fields[3] == "OfxImageEffectActionIsIdentity"
            || fields[3] == "OfxImageEffectActionRender"
            || fields[3] == "identity-current"
        {
            queries +=
                usize::from(fields[3] == "OfxImageEffectActionIsIdentity" && fields[4] == "enter");
            renders +=
                usize::from(fields[3] == "OfxImageEffectActionRender" && fields[4] == "enter");
            ordered.push(fields[3..].join(" "));
        }
    }
    assert_eq!(
        (queries, renders),
        (3, 1),
        "current-source identity query/render contract; original shutdown result: {shutdown:?}"
    );
    shutdown.expect("same resident closes and reaps within the original budget");
    let closed_bytes = read_actions().expect("bounded closed resident receipt");
    assert!(closed_bytes.len() <= 4096 && closed_bytes.is_ascii());
    let closed = std::str::from_utf8(&closed_bytes).expect("closed ASCII rows");
    assert!(
        closed.starts_with(text),
        "original action rows are retained"
    );
    assert!(closed.lines().count() <= 32);
    assert!(
        closed.ends_with(" OfxActionUnload return attempted=0 accepted=0 status=0 complete=1\n")
    );
    assert_eq!(
        ordered,
        [
            "OfxImageEffectActionIsIdentity enter attempted=0 accepted=0 status=0 complete=1",
            "identity-current contract attempted=1 accepted=1 status=0 complete=1",
            "OfxImageEffectActionIsIdentity return attempted=0 accepted=0 status=0 complete=1",
            "OfxImageEffectActionIsIdentity enter attempted=0 accepted=0 status=0 complete=1",
            "identity-current contract attempted=1 accepted=1 status=14 complete=1",
            "OfxImageEffectActionIsIdentity return attempted=0 accepted=0 status=14 complete=1",
            "OfxImageEffectActionRender enter attempted=0 accepted=0 status=0 complete=1",
            "OfxImageEffectActionRender return attempted=0 accepted=0 status=0 complete=1",
            "OfxImageEffectActionIsIdentity enter attempted=0 accepted=0 status=0 complete=1",
            "identity-current contract attempted=1 accepted=1 status=0 complete=1",
            "OfxImageEffectActionIsIdentity return attempted=0 accepted=0 status=0 complete=1",
        ],
        "real ordered action calls, with no fabricated Output fetch"
    );
    // This public session gate does not observe a private raw output sentinel,
    // off-time/other-clip identities, graph caching or performance.
}

fn assert_fixed_canvas_actions(directory: &Path, pid: u32) {
    use std::io::Read;
    let path = directory.join(format!("actions-{pid}.txt"));
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .expect("actual native action receipt")
        .take(4097)
        .read_to_end(&mut bytes)
        .expect("bounded native action receipt");
    assert!(!bytes.is_empty() && bytes.len() <= 4096 && bytes.is_ascii());
    let text = std::str::from_utf8(&bytes).expect("fixed ASCII action rows");
    assert!(text.ends_with('\n'), "no incomplete final row");
    let rows: Vec<_> = text.lines().collect();
    assert!(rows.len() <= 32);
    let mut actual = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let fields: Vec<_> = row.split_whitespace().collect();
        assert_eq!(fields.len(), 9);
        assert_eq!(fields[0].parse::<usize>().expect("ordinal"), index + 1);
        assert_eq!(
            &fields[1..3],
            ["D=0", "I=0"],
            "actual resident, not a Describe/discovery child"
        );
        assert_eq!(fields[8], "complete=1");
        if fields[3] == "OfxImageEffectActionRender" || fields[3].starts_with("fixed-canvas-") {
            actual.push(fields[3..].join(" "));
        }
    }
    assert!(rows
        .last()
        .expect("unload row")
        .ends_with(" OfxActionUnload return attempted=0 accepted=0 status=0 complete=1"));
    let mut expected = Vec::new();
    for phase in 0..3 {
        let fetches = if phase == 2 { 1 } else { 2 };
        expected.extend([
            "OfxImageEffectActionRender enter attempted=0 accepted=0 status=0 complete=1".to_owned(),
            format!("fixed-canvas-{phase} source-fetch attempted=1 accepted=1 status=0 complete=1"),
            format!("fixed-canvas-{phase} output-fetch attempted={fetches} accepted={fetches} status=0 complete=1"),
            "OfxImageEffectActionRender return attempted=0 accepted=0 status=0 complete=1".to_owned(),
        ]);
    }
    assert_eq!(
        actual, expected,
        "three real ordered Render actions, with genuine fetch/release"
    );
}

#[test]
#[ignore = "requires current Product, SessionProbe fixed_canvas_partial_output and an empty action-receipt directory"]
fn product_worker_preserves_fixed_canvas_subset_and_empty_outputs() {
    use mondrian_effects::{
        EffectExecutionSession, EffectExecutionSessionConfig, EffectProcessorControl,
    };
    let directory = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_DESCRIBE_ACTION_RECEIPT_DIRECTORY")
            .expect("root-owned new native action directory"),
    );
    assert!(directory.is_absolute() && directory.is_dir());
    assert!(std::fs::read_dir(&directory).expect("action directory").next().is_none());
    let inspection = cpu_contract_probe("fixed_canvas_partial_output");
    assert_eq!(inspection.plugins.len(), 1);
    let identifier = "uk.co.thefoundry.BasicGainPlugin";
    let input = cpu_contract_input();
    let original_input = input.clone();
    let mut direct = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        identifier,
        input.width(),
        input.height(),
        input.format(),
        mondrian_effects::EffectProjectGeometry::new(
            input.width(),
            input.height(),
            mondrian_core::SampleAspectRatio::new(2, 1).unwrap(),
        )
        .expect("explicit owner project geometry"),
        &mut || Ok(()),
    )
    .expect("actual fixed-canvas resident");
    let direct_pid = direct.worker_process_id();
    for phase in 0..3 {
        let timing = OpenFxRenderTiming {
            frame: f64::from(phase as u32),
            ..cpu_contract_timing(2.0)
        };
        let output = direct
            .render(&timing, &BTreeMap::new(), &input, &mut || Ok(()))
            .expect("real full, subset, then Output-fetched no-write frame");
        assert_eq!(direct.worker_process_id(), direct_pid);
        assert_fixed_canvas_literal(&output, phase);
        assert_eq!(input, original_input, "Source remains immutable");
    }
    direct.shutdown().expect("same resident gracefully destroys and reaps");
    assert_fixed_canvas_actions(&directory, direct_pid);

    let kind = register_selected_openfx_filter(product_executable(), &inspection, identifier)
        .expect("canonical native provider");
    let effect = instantiate_effect_node(kind).expect("one canonical authored node");
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
                SampleAspectRatio::new(2, 1).expect("PAR two"),
            )
            .expect("explicit owner project geometry"),
            TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::new(101, 24).expect("end"))
                .expect("source range"),
        )
        .expect("canonical full-canvas context"),
        &LutPreparationCache::uncached(),
    )
    .expect("canonical graph preparation");
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
    for phase in 0..3 {
        let graph = program
            .evaluate(TimelineTime::new(phase as i64, 24).expect("real frame time"))
            .expect("same authored node evaluated at the next frame");
        let output = retained
            .apply_compiled_rgba_f32(
                input.view(),
                &graph,
                phase as i64,
                &mut EffectProcessorControl::uncancelled(),
            )
            .expect("canonical retained native frame");
        assert_fixed_canvas_literal(&output, phase);
        assert_eq!(input, original_input);
        assert_eq!(retained.diagnostics().processor_instances, 1);
        assert_eq!(
            retained.diagnostics().cache_entries,
            0,
            "no frame/identity cache substitutes Render"
        );
    }
    retained.retire_processor_resources();
    assert_eq!(retained.diagnostics().processor_instances, 0);
    drop(retained);
    // No PID is inferred from cache counts: locate only the actual fixture rows
    // in this fresh private directory, then check its complete action sequence.
    let entries: Vec<_> = std::fs::read_dir(&directory)
        .expect("closed action directory")
        .take(17)
        .collect::<Result<_, _>>()
        .expect("bounded entries");
    assert!(entries.len() <= 16);
    let mut actual_pids = Vec::new();
    for entry in entries {
        use std::io::Read;
        assert!(entry.file_type().expect("file type").is_file());
        let name = entry.file_name().into_string().expect("ASCII action filename");
        let pid = name
            .strip_prefix("actions-")
            .and_then(|name| name.strip_suffix(".txt"))
            .expect("only native PID action files")
            .parse::<u32>()
            .expect("actual PID");
        let mut bytes = Vec::new();
        std::fs::File::open(entry.path())
            .expect("closed action file")
            .take(4097)
            .read_to_end(&mut bytes)
            .expect("bounded action read");
        assert!(bytes.len() <= 4096 && bytes.is_ascii());
        if std::str::from_utf8(&bytes).expect("ASCII rows").contains(" fixed-canvas-") {
            actual_pids.push(pid);
        }
    }
    assert_eq!(
        actual_pids.len(),
        2,
        "only direct and retained-Graph native owners rendered"
    );
    assert!(actual_pids.contains(&direct_pid));
    let graph_pid = *actual_pids
        .iter()
        .find(|pid| **pid != direct_pid)
        .expect("distinct Graph child");
    assert_ne!(graph_pid, direct_pid);
    assert_fixed_canvas_actions(&directory, graph_pid);
}

#[path = "openfx_host/generator.rs"]
mod generator;

#[path = "openfx_host/project_geometry.rs"]
mod project_geometry;
