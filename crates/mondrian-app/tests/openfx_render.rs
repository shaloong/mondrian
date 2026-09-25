//! Real OpenFX Float32 filter admission through the supervised product worker.

#![cfg(windows)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mondrian_app::openfx_adapter::inspect_openfx_binary;
use mondrian_app::openfx_render::{
    render_openfx_filter_frame, OpenFxFloatFrame, OpenFxRenderError, OpenFxRenderTiming,
    OpenFxScalarValue,
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

    let mut invalid = input;
    invalid.pixels[7][2] = f32::NAN;
    let rejected = render_openfx_filter_frame(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        &timing,
        &parameters,
        &invalid,
    );
    assert!(matches!(rejected, Err(OpenFxRenderError::Invalid(_))));
}
