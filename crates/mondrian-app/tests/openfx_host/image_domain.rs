//! Native full-RoD images placed into the existing fixed Effects viewport.
use super::*;
use mondrian_core::{ColorFrameAlpha, ExecutionCancellationToken};
use mondrian_effects::{
    EffectColorDomain, EffectProjectGeometry, EffectValueFormat, EffectWorkingPrecision,
};

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
fn owner_geometry() -> EffectProjectGeometry {
    EffectProjectGeometry::new(3, 2, SampleAspectRatio::new(2, 1).unwrap()).unwrap()
}

#[test]
#[ignore = "requires rebuilt v13 Product and GeneratorProbe image_domain mode"]
fn native_full_rod_preserves_pixel_bounds_placement_empty_default_and_refusal() {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_GENERATOR_MODE").unwrap(),
        "image_domain"
    );
    let binary = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_GENERATOR_BINARY").unwrap());
    let inspection = inspect_openfx_binary(product_executable(), &binary).unwrap();
    let token = ExecutionCancellationToken::new();
    let prepare = || {
        OpenFxFilterSession::prepare_generator(
            product_executable(),
            &inspection,
            "org.mondrian.fixture.ofx-generator",
            3,
            2,
            format(ColorFrameAlpha::PremultipliedFloat),
            owner_geometry(),
            &mut checkpoint(&token),
        )
        .unwrap()
    };
    let mut direct = prepare();
    let pid = direct.worker_process_id();
    let identity = direct.processor_owner();
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Deliberately interleave empty/full/subrect/expanded images on one owner.
        for frame in [0, 1, 2, 3, 4, 5, 0, 2, 1] {
            let output = direct
                .generate(
                    &timing(f64::from(frame)),
                    cpu_contract_sampling(2.0),
                    &BTreeMap::new(),
                    &mut checkpoint(&token),
                )
                .unwrap();
            let partial = frame == 0 || frame == 4;
            let empty = frame == 2;
            assert_eq!(
                output.format(),
                format(if partial || empty {
                    ColorFrameAlpha::StraightFloat
                } else {
                    ColorFrameAlpha::Opaque
                })
            );
            for (index, actual) in output.pixels().iter().enumerate() {
                let x = index % 3;
                let y = 1 - index / 3;
                let expected = if empty || (partial && x == 0) {
                    [0.0; 4]
                } else {
                    [
                        (x + 4) as f32 / 16.0,
                        (y + 3) as f32 / 8.0,
                        frame as f32 / 16.0,
                        1.0,
                    ]
                };
                assert_eq!(
                    actual.map(f32::to_bits),
                    expected.map(f32::to_bits),
                    "frame{frame}/pixel{index}"
                );
            }
            assert_eq!(direct.worker_process_id(), pid);
            assert_eq!(direct.processor_owner(), identity);
        }
    }));
    let shutdown = direct.shutdown();
    drop(direct);
    if let Err(panic) = target {
        std::panic::resume_unwind(panic);
    }
    shutdown.unwrap();
    // Each malformed/unsupported native domain gets its own owner: failed DSP
    // must not be relabelled as a reusable session or an accepted black frame.
    for frame in [6, 7, 8, 9, 10] {
        let mut bad = prepare();
        let result = bad.generate(
            &timing(f64::from(frame)),
            cpu_contract_sampling(2.0),
            &BTreeMap::new(),
            &mut checkpoint(&token),
        );
        assert!(
            result.is_err(),
            "invalid/unbounded domain frame{frame} published pixels"
        );
        bad.shutdown().unwrap();
    }
}

#[test]
#[ignore = "requires rebuilt v13 Product and published Natron-2.3.16-beta3 Misc.ofx"]
fn published_constant_finite_rod_places_transparency_and_preserves_resident_owner() {
    let binary = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_INVERT_BINARY").unwrap());
    let inspection = inspect_openfx_binary(product_executable(), &binary).unwrap();
    let token = ExecutionCancellationToken::new();
    let mut direct = OpenFxFilterSession::prepare_generator(
        product_executable(),
        &inspection,
        "net.sf.openfx.ConstantPlugin",
        3,
        2,
        format(ColorFrameAlpha::PremultipliedFloat),
        owner_geometry(),
        &mut checkpoint(&token),
    )
    .unwrap();
    let pid = direct.worker_process_id();
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Exact pixel-aligned canonical boxes at PAR2; negative origin forces a
        // complete image beyond the viewport rather than a partial tile.
        for (origin, size, first_column) in [
            ([2.0, 0.0], [4.0, 2.0], 1),
            ([-2.0, -1.0], [10.0, 4.0], 0),
            ([2.0, 0.0], [4.0, 2.0], 1),
        ] {
            let parameters = constant_parameters([
                ("bottomLeft".into(), OpenFxParameterValue::Double2(origin)),
                ("size".into(), OpenFxParameterValue::Double2(size)),
                (
                    "color".into(),
                    OpenFxParameterValue::Rgba([0.125, 0.25, 0.375, 0.5]),
                ),
            ]);
            let output = direct
                .generate(
                    &timing(0.0),
                    cpu_contract_sampling(2.0),
                    &parameters,
                    &mut checkpoint(&token),
                )
                .unwrap();
            assert_eq!(output.format(), format(ColorFrameAlpha::PremultipliedFloat));
            for (index, actual) in output.pixels().iter().enumerate() {
                let expected: [f32; 4] = if index % 3 < first_column {
                    [0.0; 4]
                } else {
                    [0.125, 0.25, 0.375, 0.5]
                };
                assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
            }
            assert_eq!(direct.worker_process_id(), pid);
        }
    }));
    let shutdown = direct.shutdown();
    drop(direct);
    if let Err(panic) = target {
        std::panic::resume_unwind(panic);
    }
    shutdown.unwrap();
}
