use mondrian_core::ColorFrameAlpha;
use mondrian_effects::{
    convert_alpha_sample, over_float_sample, EffectAlphaConversionFailure, EffectColorDomain,
    EffectFloatFrame, EffectFloatFrameError, EffectFrameExtent, EffectValueFormat,
    EffectWorkingPrecision,
};

fn format(alpha: ColorFrameAlpha) -> EffectValueFormat {
    EffectValueFormat::new(
        EffectWorkingPrecision::Float32,
        EffectColorDomain::SceneLinearRgb,
        alpha,
    )
}

#[test]
fn associated_float_frame_preserves_emission_signed_alpha_and_extended_color() {
    let pixels = vec![
        [7.0, -3.0, 11.0, 0.0],
        [-2.0, 4.0, 0.25, -0.5],
        [0.5, -1.0, 8.0, 1.5],
        [f32::MAX, -f32::MAX, f32::MIN_POSITIVE, f32::from_bits(1)],
    ];
    let frame = EffectFloatFrame::from_pixels(
        EffectFrameExtent::new(4, 1),
        format(ColorFrameAlpha::PremultipliedFloat),
        mondrian_core::ImageSampling::UNIT_SQUARE,
        pixels.clone(),
    )
    .expect("finite OFX Float domain retains complete association");
    assert_eq!(frame.format(), format(ColorFrameAlpha::PremultipliedFloat));
    assert_eq!(frame.pixels(), pixels);
    assert_eq!(frame.view().format(), frame.format());
    assert_eq!(frame.clone().into_parts().3, pixels);
}

#[test]
fn association_conversion_refuses_emission_without_an_epsilon_or_color_erasure() {
    let emission = [7.0, -3.0, 11.0, 0.0];
    assert_eq!(
        convert_alpha_sample(
            emission,
            ColorFrameAlpha::PremultipliedFloat,
            ColorFrameAlpha::StraightFloat
        ),
        Err(EffectFloatFrameError::AlphaConversion {
            from: ColorFrameAlpha::PremultipliedFloat,
            to: ColorFrameAlpha::StraightFloat,
            reason: EffectAlphaConversionFailure::ZeroAlphaEmission
        })
    );
    assert_eq!(
        convert_alpha_sample(
            [0.0; 4],
            ColorFrameAlpha::PremultipliedFloat,
            ColorFrameAlpha::StraightFloat
        ),
        Ok([0.0; 4])
    );
    for alpha in [f32::from_bits(1), 1.0 / 65_535.0, -0.5, 1.5] {
        let straight = [-2.0, 4.0, 1.0, alpha];
        let associated = [
            straight[0] * alpha,
            straight[1] * alpha,
            straight[2] * alpha,
            alpha,
        ];
        let actual = convert_alpha_sample(
            associated,
            ColorFrameAlpha::PremultipliedFloat,
            ColorFrameAlpha::StraightFloat,
        )
        .expect("every nonzero alpha is divided by its real value");
        assert_eq!(actual, straight);
    }
    assert!(matches!(
        convert_alpha_sample(
            [f32::MAX, 0.0, 0.0, f32::MIN_POSITIVE],
            ColorFrameAlpha::PremultipliedFloat,
            ColorFrameAlpha::StraightFloat
        ),
        Err(EffectFloatFrameError::AlphaConversion {
            reason: EffectAlphaConversionFailure::NumericOverflow,
            ..
        })
    ));
}

#[test]
fn finite_float_and_normalized_coverage_are_distinct_exact_contracts() {
    for alpha in [-0.5, 1.5] {
        let pixel = [2.0, -1.0, 4.0, alpha];
        assert!(EffectFloatFrame::from_pixels(
            EffectFrameExtent::new(1, 1),
            format(ColorFrameAlpha::StraightFloat),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            vec![pixel]
        )
        .is_ok());
        assert_eq!(
            EffectFloatFrame::from_pixels(
                EffectFrameExtent::new(1, 1),
                format(ColorFrameAlpha::StraightCoverage),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                vec![pixel]
            ),
            Err(EffectFloatFrameError::InvalidSample {
                index: 0,
                alpha: ColorFrameAlpha::StraightCoverage
            })
        );
    }
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        for channel in 0..4 {
            let mut pixel = [0.0, 0.0, 0.0, 1.0];
            pixel[channel] = value;
            assert!(EffectFloatFrame::from_pixels(
                EffectFrameExtent::new(1, 1),
                format(ColorFrameAlpha::PremultipliedFloat),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                vec![pixel]
            )
            .is_err());
        }
    }
    assert_ne!(
        format(ColorFrameAlpha::StraightFloat),
        format(ColorFrameAlpha::PremultipliedFloat)
    );
    assert_ne!(
        format(ColorFrameAlpha::StraightFloat),
        format(ColorFrameAlpha::StraightCoverage)
    );
}

#[test]
fn normal_over_opaque_background_keeps_zero_alpha_emission_and_extended_alpha() {
    let background = [0.25_f32, 0.5, 0.75, 1.0];
    for source in [
        [7.0_f32, -3.0, 11.0, 0.0],
        [-2.0, 4.0, 0.25, -0.5],
        [0.5, -1.0, 8.0, 1.5],
        [0.001, -0.002, 0.004, 1.0 / 65_535.0],
    ] {
        for opacity in [0.0_f32, 0.25, 1.0] {
            let actual = over_float_sample(
                background,
                ColorFrameAlpha::Opaque,
                source,
                ColorFrameAlpha::PremultipliedFloat,
                opacity,
            )
            .expect("finite associated Over is defined on these operands");
            let a = f64::from(source[3]) * f64::from(opacity);
            for channel in 0..3 {
                let expected = f64::from(source[channel]) * f64::from(opacity)
                    + f64::from(background[channel]) * (1.0 - a);
                assert!(
                    (f64::from(actual[channel]) - expected).abs() <= 1e-6 * expected.abs().max(1.0)
                );
            }
            assert_eq!(actual[3], 1.0);
        }
    }
    assert_eq!(
        over_float_sample(
            background,
            ColorFrameAlpha::Opaque,
            [7.0, -3.0, 11.0, 0.0],
            ColorFrameAlpha::PremultipliedFloat,
            1.0
        ),
        Ok([7.25, -2.5, 11.75, 1.0])
    );
}

#[test]
fn normal_over_checks_final_float_representability_after_exact_opaque_algebra() {
    assert_eq!(
        over_float_sample(
            [0.0, 0.0, 0.0, 1.0],
            ColorFrameAlpha::Opaque,
            [f32::MAX, 0.0, 0.0, 2.0],
            ColorFrameAlpha::StraightFloat,
            0.5
        ),
        Ok([f32::MAX, 0.0, 0.0, 1.0])
    );
    assert_eq!(
        over_float_sample(
            [f32::MAX, -f32::MAX, f32::MAX, f32::MAX],
            ColorFrameAlpha::StraightFloat,
            [0.25, -0.5, 2.0, 1.0],
            ColorFrameAlpha::Opaque,
            1.0
        ),
        Ok([0.25, -0.5, 2.0, 1.0])
    );
    let huge_alpha = over_float_sample(
        [0.0, 0.0, 0.0, 1.0],
        ColorFrameAlpha::Opaque,
        [1.0, -1.0, 2.0, f32::MAX],
        ColorFrameAlpha::PremultipliedFloat,
        1.0,
    )
    .expect("opaque alpha is algebraically one even at maximum finite source alpha");
    assert_eq!(huge_alpha, [1.0, -1.0, 2.0, 1.0]);
    assert!(matches!(
        over_float_sample(
            [0.0, 0.0, 0.0, 1.0],
            ColorFrameAlpha::Opaque,
            [f32::MAX, 0.0, 0.0, 2.0],
            ColorFrameAlpha::StraightFloat,
            1.0
        ),
        Err(EffectFloatFrameError::AlphaConversion {
            reason: EffectAlphaConversionFailure::NumericOverflow,
            ..
        })
    ));
}

// Generated by verify_over_oracle.py from exact rational Float32 operands.
// Expected values have one nearest-even IEEE32 rounding, independent of the
// production polynomial evaluation. At most one Float32 ULP is admitted.
#[test]
fn compensated_normal_over_matches_independent_exact_float32_oracles() {
    let cases = [
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x5a800000],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x5a800000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x5a800000],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x5a800000, 0xdb000000, 0x59000000, 0x5a800000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x5a800000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x00000000,
            [0x5a800000, 0xdb000000, 0x59000000, 0x5a800000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x5a800000],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x5a800000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x5a800000],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x5a800000, 0xdb000000, 0x59000000, 0x5a800000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x5a800000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x00000001,
            [0x5a800000, 0xdb000000, 0x59000000, 0x5a800000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x5a800000],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x5a800000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x5a800000],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0xf5000000, 0x75800000, 0xf3800000, 0xf5000000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x5a800000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x3f000000,
            [0x5a000000, 0xda800000, 0x58800000, 0x5a000000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x5a800000],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x5a800000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x5a800000],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0xf5800000, 0x76000000, 0xf4000000, 0xf5800000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x5a800000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x3f800000,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0xda800000],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0xda800000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0xda800000],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0xda800000, 0x5b000000, 0xd9000000, 0xda800000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0xda800000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x00000000,
            [0xda800000, 0x5b000000, 0xd9000000, 0xda800000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0xda800000],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0xda800000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0xda800000],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0xda800000, 0x5b000000, 0xd9000000, 0xda800000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0xda800000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x00000001,
            [0xda800000, 0x5b000000, 0xd9000000, 0xda800000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0xda800000],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0xda800000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0xda800000],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0xf5000000, 0x75800000, 0xf3800000, 0xf5000000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0xda800000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x3f000000,
            [0xda000000, 0x5a800000, 0xd8800000, 0xda000000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0xda800000],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0xda800000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0xda800000],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0xf5800000, 0x76000000, 0xf4000000, 0xf5800000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0xda800000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x3f800000,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x7f7fffff, 0xff800000, 0x7dffffff, 0x7f7fffff],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x00000000,
            [0x7f7fffff, 0xff800000, 0x7dffffff, 0x7f7fffff],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x7f7ffff7, 0xff800000, 0x7dfffff7, 0x7f7ffff7],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x00000001,
            [0x7f7fffff, 0xff800000, 0x7dffffff, 0x7f7fffff],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0xff800000, 0x7f800000, 0xff800000, 0xff800000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x3f000000,
            [0x7effffff, 0xff7fffff, 0x7d7fffff, 0x7effffff],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0xff800000, 0x7f800000, 0xff800000, 0xff800000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x3f800000,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0xff7fffff, 0x7f800000, 0xfdffffff, 0xff7fffff],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x00000000,
            [0xff7fffff, 0x7f800000, 0xfdffffff, 0xff7fffff],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0xff800000, 0x7f800000, 0xfe000003, 0xff800000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x00000001,
            [0xff7fffff, 0x7f800000, 0xfdffffff, 0xff7fffff],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0xff800000, 0x7f800000, 0xff800000, 0xff800000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x3f000000,
            [0xfeffffff, 0x7f7fffff, 0xfd7fffff, 0xfeffffff],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0xff800000, 0x7f800000, 0xff800000, 0xff800000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x3f800000,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000001],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000001],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000001],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x00000001, 0x80000002, 0x00000000, 0x00000001],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000001],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x00000000,
            [0x00000001, 0x80000002, 0x00000000, 0x00000001],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000001],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000001],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000001],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x00000001, 0x80000002, 0x00000000, 0x00000001],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000001],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x00000001,
            [0x00000004, 0x00000005, 0x80000005, 0x00000002],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000001],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000001],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000001],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0x00000001, 0x80000003, 0x00000000, 0x00000001],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000001],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x3f000000,
            [0x3fc00000, 0x40600000, 0xc0200000, 0x3f000000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000001],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000001],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000001],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x00000002, 0x80000004, 0x00000000, 0x00000002],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000001],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x3f800000,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x80000001],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x80000001],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x80000001],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x80000001, 0x00000002, 0x80000000, 0x80000001],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x80000001],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x00000000,
            [0x80000001, 0x00000002, 0x80000000, 0x80000001],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x80000001],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x80000001],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x80000001],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x80000001, 0x00000002, 0x80000000, 0x80000001],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x80000001],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x00000001,
            [0x00000002, 0x00000009, 0x80000005, 0x00000000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x80000001],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x80000001],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x80000001],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0x80000002, 0x00000003, 0x80000000, 0x80000002],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x80000001],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x3f000000,
            [0x3fc00000, 0x40600000, 0xc0200000, 0x3f000000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x80000001],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x80000001],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x80000001],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x80000002, 0x00000004, 0x80000000, 0x80000002],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x80000001],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x3f800000,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000000],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000000],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x00000000, 0x00000000, 0x00000000, 0x00000000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x00000000,
            [0x00000000, 0x00000000, 0x00000000, 0x00000000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000000],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000000],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x00000000, 0x00000000, 0x00000000, 0x00000000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x00000001,
            [0x00000003, 0x00000007, 0x80000005, 0x00000001],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000000],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000000],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0x00000000, 0x00000000, 0x00000000, 0x00000000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x3f000000,
            [0x3fc00000, 0x40600000, 0xc0200000, 0x3f000000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000000],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000000],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x00000000, 0x00000000, 0x00000000, 0x00000000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x00000000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x3f800000,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f000000],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f000000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f000000],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x3f000000, 0xbf800000, 0x3d800000, 0x3f000000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f000000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x00000000,
            [0x3f000000, 0xbf800000, 0x3d800000, 0x3f000000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f000000],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f000000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f000000],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x3f000000, 0xbf800000, 0x3d800000, 0x3f000000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f000000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x00000001,
            [0x3f000000, 0xbf800000, 0x3d800000, 0x3f000000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f000000],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f000000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f000000],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0x3f200000, 0xbfa00000, 0x3da00000, 0x3f200000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f000000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x3f000000,
            [0x3fe00000, 0x40400000, 0xc01e0000, 0x3f400000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f000000],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f000000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f000000],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x3f400000, 0xbfc00000, 0x3dc00000, 0x3f400000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f000000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x3f800000,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x00000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x00000001,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x3f000000,
            [0x40000000, 0x40200000, 0xc01c0000, 0x3f800000],
        ),
        (
            "equal_straight_opaque",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "equal_straight_signed_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::StraightFloat,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
        ),
        (
            "source_opaque_cancels_large_base",
            [0x3f800000, 0xc0000000, 0x3e000000, 0x3f800000],
            ColorFrameAlpha::StraightFloat,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            0x3f800000,
            [0x40400000, 0x40e00000, 0xc0a00000, 0x3f800000],
        ),
        (
            "zero_alpha_emission",
            [0x40400000, 0xc0000000, 0x3f800000, 0x00000000],
            ColorFrameAlpha::PremultipliedFloat,
            [0x40e00000, 0xc0400000, 0x41300000, 0x00000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f800000,
            [0x41200000, 0xc0a00000, 0x41400000, 0x00000000],
        ),
        (
            "half_emission",
            [0x00000000, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x40e00000, 0xc0400000, 0x41300000, 0x00000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f000000,
            [0x40600000, 0xbfc00000, 0x40b00000, 0x3f800000],
        ),
        (
            "subnormal_tiny_coverage",
            [0x00000001, 0x80000001, 0x00000000, 0x00000001],
            ColorFrameAlpha::StraightFloat,
            [0x00000001, 0x00000000, 0x00000001, 0x00000001],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x00000000, 0x80000000, 0x00000000, 0x00000002],
        ),
        (
            "genuine_association_overflow",
            [0x00000000, 0x00000000, 0x00000000, 0x00000000],
            ColorFrameAlpha::PremultipliedFloat,
            [0x7f7fffff, 0x3f800000, 0xbf800000, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x7f800000, 0x7f7fffff, 0xff7fffff, 0x7f7fffff],
        ),
        (
            "final_overflow_midpoint_neighbor",
            [0x7f7fffff, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x66ffffff, 0x00000000, 0x00000000, 0xb3000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f800000,
            [0x7f7fffff, 0x00000000, 0x00000000, 0x3f800000],
        ),
        (
            "final_overflow_midpoint_neighbor",
            [0x7f7fffff, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x67000000, 0x00000000, 0x00000000, 0xb3000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f800000,
            [0x7f800000, 0x00000000, 0x00000000, 0x3f800000],
        ),
        (
            "final_overflow_midpoint_neighbor",
            [0x7f7fffff, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x67000001, 0x00000000, 0x00000000, 0xb3000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f800000,
            [0x7f800000, 0x00000000, 0x00000000, 0x3f800000],
        ),
        (
            "final_one_midpoint_neighbor",
            [0x3f800000, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x337fffff, 0x00000000, 0x00000000, 0x00000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f800000,
            [0x3f800000, 0x00000000, 0x00000000, 0x3f800000],
        ),
        (
            "final_one_midpoint_neighbor",
            [0x3f800000, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x33800000, 0x00000000, 0x00000000, 0x00000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f800000,
            [0x3f800000, 0x00000000, 0x00000000, 0x3f800000],
        ),
        (
            "final_one_midpoint_neighbor",
            [0x3f800000, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x33800001, 0x00000000, 0x00000000, 0x00000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f800000,
            [0x3f800001, 0x00000000, 0x00000000, 0x3f800000],
        ),
        (
            "final_subnormal_halfway",
            [0x00000000, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x00000001, 0x00000000, 0x00000000, 0x00000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f000000,
            [0x00000000, 0x00000000, 0x00000000, 0x3f800000],
        ),
        (
            "final_subnormal_halfway",
            [0x00000000, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x00000002, 0x00000000, 0x00000000, 0x00000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f000000,
            [0x00000001, 0x00000000, 0x00000000, 0x3f800000],
        ),
        (
            "final_subnormal_halfway",
            [0x00000000, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x00000003, 0x00000000, 0x00000000, 0x00000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f000000,
            [0x00000002, 0x00000000, 0x00000000, 0x3f800000],
        ),
        (
            "final_overflow_midpoint_neighbor",
            [0xff7fffff, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xe6ffffff, 0x00000000, 0x00000000, 0xb3000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f800000,
            [0xff7fffff, 0x00000000, 0x00000000, 0x3f800000],
        ),
        (
            "final_overflow_midpoint_neighbor",
            [0xff7fffff, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xe7000000, 0x00000000, 0x00000000, 0xb3000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f800000,
            [0xff800000, 0x00000000, 0x00000000, 0x3f800000],
        ),
        (
            "final_overflow_midpoint_neighbor",
            [0xff7fffff, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xe7000001, 0x00000000, 0x00000000, 0xb3000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f800000,
            [0xff800000, 0x00000000, 0x00000000, 0x3f800000],
        ),
        (
            "final_one_midpoint_neighbor",
            [0xbf800000, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xb37fffff, 0x00000000, 0x00000000, 0x00000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f800000,
            [0xbf800000, 0x00000000, 0x00000000, 0x3f800000],
        ),
        (
            "final_one_midpoint_neighbor",
            [0xbf800000, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xb3800000, 0x00000000, 0x00000000, 0x00000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f800000,
            [0xbf800000, 0x00000000, 0x00000000, 0x3f800000],
        ),
        (
            "final_one_midpoint_neighbor",
            [0xbf800000, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xb3800001, 0x00000000, 0x00000000, 0x00000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f800000,
            [0xbf800001, 0x00000000, 0x00000000, 0x3f800000],
        ),
        (
            "final_subnormal_halfway",
            [0x00000000, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x80000001, 0x00000000, 0x00000000, 0x00000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f000000,
            [0x80000000, 0x00000000, 0x00000000, 0x3f800000],
        ),
        (
            "final_subnormal_halfway",
            [0x00000000, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x80000002, 0x00000000, 0x00000000, 0x00000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f000000,
            [0x80000001, 0x00000000, 0x00000000, 0x3f800000],
        ),
        (
            "final_subnormal_halfway",
            [0x00000000, 0x00000000, 0x00000000, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x80000003, 0x00000000, 0x00000000, 0x00000000],
            ColorFrameAlpha::PremultipliedFloat,
            0x3f000000,
            [0x80000002, 0x00000000, 0x00000000, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f7fffff,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x7f7fffef, 0xff7fffef, 0x7f7fffef, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0xff800000, 0x7f800000, 0xff800000, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f7fffff,
            [0xff800000, 0x7f800000, 0xff800000, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0xff800000, 0x7f800000, 0xff800000, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f7fffff,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0x7f800000, 0xff800000, 0x7f800000, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0x7f800000, 0xff800000, 0x7f800000, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f7fffff,
            [0x7f800000, 0xff800000, 0x7f800000, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x7f800000, 0xff800000, 0x7f800000, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f7fffff,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0xff7fffef, 0x7f7fffef, 0xff7fffef, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0x7f800000, 0xff800000, 0x7f800000, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f7fffff,
            [0x7f800000, 0xff800000, 0x7f800000, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0x7f7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0x7f800000, 0xff800000, 0x7f800000, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f7fffff,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000000,
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x00000001,
            [0xff800000, 0x7f800000, 0xff800000, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f000000,
            [0xff800000, 0x7f800000, 0xff800000, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f7fffff,
            [0xff800000, 0x7f800000, 0xff800000, 0x3f800000],
        ),
        (
            "huge_rgb_products_cancel_before_opacity",
            [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
            ColorFrameAlpha::Opaque,
            [0x7f7fffff, 0xff7fffff, 0x7f7fffff, 0xff7fffff],
            ColorFrameAlpha::StraightFloat,
            0x3f800000,
            [0xff800000, 0x7f800000, 0xff800000, 0x3f800000],
        ),
    ];
    let ordered = |value: u32| -> i64 {
        if value & 0x8000_0000 == 0 {
            i64::from(value) + 0x8000_0000
        } else {
            0x8000_0000 - i64::from(value & 0x7fff_ffff)
        }
    };
    for (name, base, base_alpha, source, source_alpha, opacity, expected) in cases {
        let result = over_float_sample(
            base.map(f32::from_bits),
            base_alpha,
            source.map(f32::from_bits),
            source_alpha,
            f32::from_bits(opacity),
        );
        if expected.iter().any(|bits| !f32::from_bits(*bits).is_finite()) {
            assert!(
                matches!(
                    result,
                    Err(EffectFloatFrameError::AlphaConversion {
                        reason: EffectAlphaConversionFailure::NumericOverflow,
                        ..
                    })
                ),
                "{name}: true final Float32 overflow {expected:x?}: {result:?}"
            );
            continue;
        }
        let actual = result
            .unwrap_or_else(|error| panic!("{name}: finite exact output was refused: {error:?}"));
        for channel in 0..4 {
            assert!(
                actual[channel].is_finite(),
                "{name}: nonfinite channel {channel}"
            );
            assert!(
                (ordered(actual[channel].to_bits()) - ordered(expected[channel])).abs() <= 1,
                "{name}: channel {channel}, actual {:08x}, exact {:08x}",
                actual[channel].to_bits(),
                expected[channel]
            );
        }
    }
}

#[test]
fn typed_normal_blend_keeps_huge_alpha_equal_color_identity_and_no_op_base() {
    use mondrian_core::types::BlendMode;
    use mondrian_effects::blend_float_frames_in_place;
    for source_alpha in [
        f32::from_bits(0x5a80_0000),
        -f32::from_bits(0x5a80_0000),
        f32::MAX,
        -f32::MAX,
    ] {
        let mut base = EffectFloatFrame::from_pixels(
            EffectFrameExtent::new(1, 1),
            format(ColorFrameAlpha::Opaque),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            vec![[1.0, -2.0, 0.125, 1.0]],
        )
        .expect("opaque base");
        let source = EffectFloatFrame::from_pixels(
            EffectFrameExtent::new(1, 1),
            format(ColorFrameAlpha::StraightFloat),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            vec![[1.0, -2.0, 0.125, source_alpha]],
        )
        .expect("finite extended straight alpha");
        blend_float_frames_in_place(&mut base, source.view(), 1.0, BlendMode::Normal, 0)
            .expect("representable exact cancellation");
        assert_eq!(base.alpha(), ColorFrameAlpha::Opaque);
        assert_eq!(base.pixels(), [[1.0, -2.0, 0.125, 1.0]]);
        let mut no_op = source.clone();
        blend_float_frames_in_place(&mut no_op, base.view(), 0.0, BlendMode::Normal, 0)
            .expect("frame opacity zero retains the original complete value");
        assert_eq!(no_op, source);
    }
}

#[test]
fn typed_normal_blend_cancels_huge_products_before_applying_nontrivial_opacity() {
    use mondrian_core::types::BlendMode;
    use mondrian_effects::blend_float_frames_in_place;
    for alpha in [f32::MAX, -f32::MAX] {
        for opacity in [
            0.0,
            f32::from_bits(1),
            0.5,
            f32::from_bits(0x3f7f_ffff),
            1.0,
        ] {
            let original = EffectFloatFrame::from_pixels(
                EffectFrameExtent::new(1, 1),
                format(ColorFrameAlpha::Opaque),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                vec![[f32::MAX, -f32::MAX, f32::MAX, 1.0]],
            )
            .expect("finite opaque HDR raster");
            let mut base = original.clone();
            let source = EffectFloatFrame::from_pixels(
                EffectFrameExtent::new(1, 1),
                format(ColorFrameAlpha::StraightFloat),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                vec![[f32::MAX, -f32::MAX, f32::MAX, alpha]],
            )
            .expect("valid finite straight values do not require intermediate Float32 association");
            blend_float_frames_in_place(&mut base, source.view(), opacity, BlendMode::Normal, 0)
                .expect("matching products cancel before the opacity term");
            assert_eq!(base, original);
        }
    }
}
