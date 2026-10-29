//! Real Filter pixels and identity at different raster densities.
use super::*;
use mondrian_core::{ColorFrameAlpha, ImageSampling};
use mondrian_effects::{
    EffectColorDomain, EffectFloatFrame, EffectFrameExtent, EffectProjectGeometry,
    EffectValueFormat, EffectWorkingPrecision,
};

#[test]
#[ignore = "requires normal Product and MONDRIAN_OPENFX_REFERENCE_BINARY=official Basic.ofx.bundle binary"]
fn official_basic_filter_preserves_sampling_pixels_and_identity_in_one_native_owner() {
    let binary = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_REFERENCE_BINARY").expect("official Basic reference"),
    );
    let inspection = inspect_openfx_binary(product_executable(), &binary).unwrap();
    let format = EffectValueFormat::new(
        EffectWorkingPrecision::Float32,
        EffectColorDomain::SceneLinearRgb,
        ColorFrameAlpha::StraightFloat,
    );
    let mut session = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        4,
        4,
        format,
        EffectProjectGeometry::new(10, 7, SampleAspectRatio::new(2, 1).unwrap()).unwrap(),
        &mut || Ok(()),
    )
    .unwrap();
    let worker = session.worker_process_id();
    #[cfg(target_os = "linux")]
    assert_selected_product_worker_image(worker);
    let mut identity = None;
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut invocation = 0;
        for scale in [
            [Rational::new(1, 2); 2],
            [Rational::new(3, 4), Rational::new(1, 2)],
            [Rational::new(1, 1); 2],
            [Rational::new(1, 2); 2],
        ] {
            let sampling =
                ImageSampling::new(SampleAspectRatio::new(3, 2).unwrap(), scale).unwrap();
            let input = EffectFloatFrame::from_pixels(
                EffectFrameExtent::new(4, 4),
                format,
                sampling,
                (0..16).map(|i| [i as f32 / 16.0 - 0.25, 1.5, -0.5, 0.25]).collect(),
            )
            .unwrap();
            let original = input.clone();
            for gain in [0.5_f32, 1.0] {
                let parameters = constant_parameters([
                    (
                        "scale".into(),
                        OpenFxParameterValue::Double(f64::from(gain)),
                    ),
                    (
                        "scaleComponents".into(),
                        OpenFxParameterValue::Boolean(false),
                    ),
                ]);
                let timing = OpenFxRenderTiming {
                    frame: invocation as f64,
                    frame_rate: 24.0,
                    first_frame: 0.0,
                    last_frame: 12.0,
                };
                let output = session.render(&timing, &parameters, &input, &mut || Ok(())).unwrap();
                assert_eq!(output.sampling(), sampling);
                assert_eq!(output.extent(), input.extent());
                assert_eq!(output.format(), format);
                for (actual, source) in output.pixels().iter().zip(input.pixels()) {
                    assert_eq!(
                        actual.map(f32::to_bits),
                        source.map(|v| (v * gain).to_bits())
                    );
                }
                assert_eq!(input, original);
                let receipt = session.take_feedback().unwrap();
                let observed = (
                    receipt.owner,
                    receipt.origin,
                    receipt.snapshot.native_owner(),
                );
                if let Some(expected) = identity {
                    assert_eq!(observed, expected);
                }
                identity = Some(observed);
                invocation += 1;
                assert_eq!(receipt.snapshot.invocation(), invocation);
                assert_eq!(session.worker_process_id(), worker);
            }
        }
    }));
    let cleanup = session.shutdown();
    if let Err(panic) = target {
        std::panic::resume_unwind(panic);
    }
    cleanup.expect("original Filter child must settle after successful qualification");
}
