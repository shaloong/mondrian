//! Real OpenFX Float32 filter admission through the supervised product worker.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mondrian_app::openfx_adapter::inspect_openfx_binary;
use mondrian_app::openfx_effect::register_selected_openfx_filter;
use mondrian_app::openfx_host::{
    describe_openfx_filter, render_openfx_filter_frame, OpenFxDoubleType, OpenFxFloatFrame,
    OpenFxHostError, OpenFxRenderTiming, OpenFxScalarValue,
};
use mondrian_core::automation::PropertyValue;
use mondrian_core::{
    Rational, SampleAspectRatio, TimelineTime, TimelineTimeRange, WorkingColorSpace,
};
use mondrian_effects::{
    apply_compiled_effect_graph_rgba_f32, instantiate_effect_node, EffectFrameContext,
    LutPreparationCache, PreparedEffectProgram,
};

fn product_executable() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_mondrian"))
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
    assert_eq!(scale.default_value, OpenFxScalarValue::Double(1.0));
    assert_eq!(scale.double_type, Some(OpenFxDoubleType::Scale));
    assert_eq!(scale.minimum, Some(0.0));
    assert_eq!(scale.display_maximum, Some(100.0));
    let switch = description
        .parameters
        .iter()
        .find(|parameter| parameter.name == "scaleComponents")
        .expect("component scaling switch");
    assert_eq!(switch.default_value, OpenFxScalarValue::Boolean(false));
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
    let input = OpenFxFloatFrame {
        width: 4,
        height: 4,
        pixels: (0..16).map(|index| [index as f32 / 16.0, 0.2, 0.3, 0.4]).collect(),
    };
    let parameters = BTreeMap::from([
        ("scale".to_owned(), OpenFxScalarValue::Double(1.5)),
        (
            "scaleComponents".to_owned(),
            OpenFxScalarValue::Boolean(false),
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
    )
    .expect("render actual native plugin in isolated child");
    assert_eq!((output.width, output.height), (4, 4));
    for (actual, source) in output.pixels.iter().zip(&input.pixels) {
        for (channel, expected) in actual.iter().zip(source.iter().map(|value| value * 1.5)) {
            assert!((channel - expected).abs() < 1e-6, "{channel} != {expected}");
        }
    }

    let mut invalid = input.clone();
    invalid.pixels[7][2] = f32::NAN;
    let rejected = render_openfx_filter_frame(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        &timing,
        &parameters,
        &invalid,
    );
    assert!(matches!(rejected, Err(OpenFxHostError::Invalid(_))));

    let out_of_range = BTreeMap::from([
        ("scale".to_owned(), OpenFxScalarValue::Double(-0.5)),
        (
            "scaleComponents".to_owned(),
            OpenFxScalarValue::Boolean(false),
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
        SampleAspectRatio::SQUARE,
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
    let integrated =
        apply_compiled_effect_graph_rgba_f32(&input.pixels, input.width, input.height, &graph, 991)
            .expect("render the native Filter through the shared graph");
    for (actual, expected) in integrated.iter().zip(&output.pixels) {
        for (actual, expected) in actual.iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
        }
    }
}
