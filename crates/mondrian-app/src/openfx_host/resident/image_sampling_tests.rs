//! Actual sampling at lazy native Create, without a second owner authority.
use super::*;
use mondrian_core::{ImageSampling, Rational, SampleAspectRatio};
use mondrian_effects::{
    EffectColorDomain, EffectFrameExtent, EffectValueFormat, EffectWorkingPrecision,
};

fn input(par: SampleAspectRatio, scale: [Rational; 2]) -> EffectFloatFrame {
    EffectFloatFrame::from_pixels(
        EffectFrameExtent::new(3, 5),
        EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        ImageSampling::new(par, scale).unwrap(),
        vec![[-0.25, 0.5, 2.0, 0.25]; 15],
    )
    .unwrap()
}

#[test]
#[ignore = "requires current Product and owned SessionProbe image_sampling mode"]
fn actual_image_sampling_binds_create_and_preserves_owner_across_density_changes() {
    const TEST: &str = "openfx_host::resident::image_sampling_tests::actual_image_sampling_binds_create_and_preserves_owner_across_density_changes";
    if !super::super::shared_frame_tests::qualification_child(
        TEST,
        &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "image_sampling")],
    ) {
        return;
    }
    let helper =
        PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_TEST_HELPER").expect("pinned Product"));
    let binary = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY")
            .expect("owned image sampling probe"),
    );
    let inspection = crate::openfx_adapter::inspect_openfx_binary(&helper, &binary).unwrap();
    let first = input(
        SampleAspectRatio::new(6, 5).unwrap(),
        [Rational::new(1, 1); 2],
    );
    let mut session = OpenFxFilterSession::prepare(
        &helper,
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        3,
        5,
        first.format(),
        EffectProjectGeometry::new(10, 7, SampleAspectRatio::new(2, 1).unwrap()).unwrap(),
        &mut || Ok(()),
    )
    .unwrap();
    let owner = session.processor_owner();
    let worker = session.worker_process_id();
    let timing = OpenFxRenderTiming {
        frame: 0.0,
        frame_rate: 24.0,
        first_frame: 0.0,
        last_frame: 100.0,
    };
    let parameters = BTreeMap::from([(
        "scale".to_owned(),
        OpenFxParameterBinding::constant(OpenFxParameterValue::Double(0.5)),
    )]);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut native_owner = None;
        let mut invocation = 0;
        for (numerator, denominator, blue) in [
            (6, 5, 1.075_f32),
            (1, 1, 1.0625),
            (2, 1, 1.125),
            (6, 5, 1.075),
        ] {
            let par = SampleAspectRatio::new(numerator, denominator).unwrap();
            for scale in [
                [Rational::new(1, 2); 2],
                [Rational::new(1, 2), Rational::new(3, 4)],
                [Rational::new(1, 1); 2],
            ] {
                let source = input(par, scale);
                let original = source.clone();
                let timing = OpenFxRenderTiming { frame: invocation as f64, ..timing.clone() };
                let output = session.render(&timing, &parameters, &source, &mut || Ok(())).unwrap();
                assert_eq!(output.sampling(), source.sampling());
                assert_eq!(output.extent(), source.extent());
                for pixel in output.pixels() {
                    assert_eq!(
                        pixel.map(f32::to_bits),
                        [0.1875_f32, 0.359375, blue, 0.125].map(f32::to_bits)
                    );
                }
                assert_eq!(source, original);
                invocation += 1;
                let feedback = session.take_feedback().unwrap();
                assert_eq!(feedback.owner, owner);
                assert_eq!(feedback.snapshot.invocation(), invocation);
                if let Some(expected) = native_owner {
                    assert_eq!(feedback.snapshot.native_owner(), expected);
                }
                native_owner = Some(feedback.snapshot.native_owner());
                assert_eq!(session.worker_process_id(), worker);
                assert_eq!(session.sequence, invocation);
                assert_eq!(session.process.state, ProcessState::Running);
                assert!(session.process.child.try_wait().unwrap().is_none());
            }
        }
    }));
    let cleanup = session.shutdown();
    if let Err(failure) = result {
        std::panic::resume_unwind(failure);
    }
    cleanup.expect("consume the original native worker after success");
}
