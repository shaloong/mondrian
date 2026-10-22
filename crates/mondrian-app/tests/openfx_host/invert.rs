//! Published OpenFX-Misc Invert 2.0 through the original Product owner.
use super::*;
use mondrian_effects::{
    EffectColorDomain, EffectFrameExtent, EffectValueFormat, EffectWorkingPrecision,
};

const IDENTIFIER: &str = "net.sf.openfx.Invert";
const RED: &str = "NatronOfxParamProcessR";
const GREEN: &str = "NatronOfxParamProcessG";
const BLUE: &str = "NatronOfxParamProcessB";
const ALPHA: &str = "NatronOfxParamProcessA";

const INPUT: [[f32; 4]; 6] = [
    [0.125, 0.25, 0.75, 0.25],
    [0.5, 0.375, 0.875, 0.75],
    [0.0, 1.0, 0.5, 1.0],
    [0.25, 0.75, 0.125, 0.75],
    [0.875, 0.5, 0.375, 0.25],
    [1.0, 0.0, 0.25, 1.0],
];

// Independent Float32 literals: RGB = 1 - input, alpha unchanged. The pinned
// upstream Invert.cpp:258-262 performs this with premult=false/mix=1/no Mask.
// Non-half alpha also distinguishes our explicit A=false from its true default.
const INVERTED_BITS: [[u32; 4]; 6] = [
    [0x3f600000, 0x3f400000, 0x3e800000, 0x3e800000],
    [0x3f000000, 0x3f200000, 0x3e000000, 0x3f400000],
    [0x3f800000, 0x00000000, 0x3f000000, 0x3f800000],
    [0x3f400000, 0x3e800000, 0x3f600000, 0x3f400000],
    [0x3e000000, 0x3f000000, 0x3f200000, 0x3e800000],
    [0x00000000, 0x3f800000, 0x3f400000, 0x3f800000],
];
const RED_UNCHANGED_BITS: [[u32; 4]; 6] = [
    [0x3e000000, 0x3f400000, 0x3e800000, 0x3e800000],
    [0x3f000000, 0x3f200000, 0x3e000000, 0x3f400000],
    [0x00000000, 0x00000000, 0x3f000000, 0x3f800000],
    [0x3e800000, 0x3e800000, 0x3f600000, 0x3f400000],
    [0x3f600000, 0x3f000000, 0x3f200000, 0x3e800000],
    [0x3f800000, 0x3f800000, 0x3f400000, 0x3f800000],
];

#[test]
#[ignore = "requires current Product and published Natron-2.3.16-beta3 Misc.ofx via MONDRIAN_OPENFX_INVERT_BINARY"]
fn third_party_invert_preserves_pixels_parameters_and_owner() {
    let binary = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_INVERT_BINARY")
            .expect("published OpenFX-Misc binary is required"),
    );
    assert!(binary.is_absolute() && binary.is_file());
    let inspection = inspect_openfx_binary(product_executable(), &binary)
        .expect("discover the external bundle in the supervised Product");
    let selected = inspection
        .plugins
        .iter()
        .filter(|plugin| plugin.identifier == IDENTIFIER)
        .collect::<Vec<_>>();
    assert_eq!(selected.len(), 1, "one exact published Invert class");
    assert_eq!(selected[0].api_version, 1);
    assert_eq!(
        (selected[0].version_major, selected[0].version_minor),
        (2, 0)
    );
    let description = describe_openfx_filter(product_executable(), &inspection, IDENTIFIER)
        .expect("admit the actual external Filter descriptor");
    assert_eq!(description.identifier, IDENTIFIER);
    assert_eq!(description.label, "InvertOFX");

    // The published CI enables OFX_EXTENSIONS_NATRON. These are its actual
    // IDs, not the source's non-Natron processR/G/B/A fallback names. The
    // DaVinciResolve-only `mask` control is absent for the Mondrian host.
    let defaults = [
        (RED, OpenFxParameterValue::Boolean(true), false, true),
        (GREEN, OpenFxParameterValue::Boolean(true), false, true),
        (BLUE, OpenFxParameterValue::Boolean(true), false, true),
        (ALPHA, OpenFxParameterValue::Boolean(true), false, true),
        ("premult", OpenFxParameterValue::Boolean(false), false, true),
        (
            "premultChannel",
            OpenFxParameterValue::Choice(3),
            true,
            true,
        ),
        (
            "maskInvert",
            OpenFxParameterValue::Boolean(false),
            false,
            true,
        ),
        ("mix", OpenFxParameterValue::Double(1.0), false, true),
        (
            "premultChanged",
            OpenFxParameterValue::Boolean(false),
            true,
            false,
        ),
    ];
    assert_eq!(description.parameters.len(), defaults.len());
    for (actual, (name, value, secret, enabled)) in description.parameters.iter().zip(defaults) {
        assert_eq!(actual.name, name);
        assert_eq!(actual.default_value, value, "{name} default");
        assert_eq!(
            (actual.secret, actual.enabled),
            (secret, enabled),
            "{name} flags"
        );
    }
    assert_eq!(
        description.parameters.iter().filter(|p| !p.secret && p.enabled).count(),
        7
    );
    assert_eq!(
        description.parameters.iter().filter(|p| p.secret).count(),
        2
    );
    let channel = &description.parameters[5];
    assert_eq!(
        channel
            .choices
            .iter()
            .map(|choice| (choice.key.as_str(), choice.label.as_str()))
            .collect::<Vec<_>>(),
        [("0", "R"), ("1", "G"), ("2", "B"), ("3", "A")]
    );
    assert!(channel.choices.iter().all(|choice| choice.display_rank.is_none()));
    let mix = &description.parameters[7];
    assert_eq!(mix.double_type, Some(OpenFxDoubleType::Plain));
    assert_eq!(mix.numeric_channels.len(), 1);
    assert_eq!(
        (
            mix.numeric_channels[0].minimum,
            mix.numeric_channels[0].maximum
        ),
        (0.0, 1.0)
    );
    assert_eq!(
        (
            mix.numeric_channels[0].display_minimum,
            mix.numeric_channels[0].display_maximum
        ),
        (0.0, 1.0)
    );
    assert!(!description.parameters[8].can_animate);

    let input = EffectFloatFrame::from_pixels(
        EffectFrameExtent::new(3, 2),
        EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        mondrian_core::ImageSampling::UNIT_SQUARE,
        INPUT.to_vec(),
    )
    .expect("nonuniform dyadic Straight Float input");
    let original = input.clone();
    let parameters = constant_parameters([
        (RED.to_owned(), OpenFxParameterValue::Boolean(true)),
        (GREEN.to_owned(), OpenFxParameterValue::Boolean(true)),
        (BLUE.to_owned(), OpenFxParameterValue::Boolean(true)),
        (ALPHA.to_owned(), OpenFxParameterValue::Boolean(false)),
        ("premult".to_owned(), OpenFxParameterValue::Boolean(false)),
        ("mix".to_owned(), OpenFxParameterValue::Double(1.0)),
    ]);
    let mut red_unchanged = parameters.clone();
    red_unchanged.insert(
        RED.to_owned(),
        OpenFxParameterBinding::constant(OpenFxParameterValue::Boolean(false)),
    );
    let timing = OpenFxRenderTiming {
        frame: 0.0,
        frame_rate: 24.0,
        first_frame: 0.0,
        last_frame: 12.0,
    };
    let cancellation = mondrian_core::ExecutionCancellationToken::new();
    let mut session = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        IDENTIFIER,
        3,
        2,
        input.format(),
        mondrian_effects::EffectProjectGeometry::new(3, 2, SampleAspectRatio::SQUARE)
            .expect("explicit full-frame project geometry"),
        &mut checkpoint(&cancellation),
    )
    .expect("original external Filter owner");
    let pid = session.worker_process_id();
    let owner = session.processor_owner();
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut native_owner = None;
        for (index, (bindings, expected)) in [
            (&parameters, &INVERTED_BITS),
            (&red_unchanged, &RED_UNCHANGED_BITS),
            (&parameters, &INVERTED_BITS),
        ]
        .into_iter()
        .enumerate()
        {
            let output = session
                .render(&timing, bindings, &input, &mut checkpoint(&cancellation))
                .expect("real external inversion and warm control change");
            assert_eq!(output.extent(), EffectFrameExtent::new(3, 2));
            assert_eq!(output.format(), input.format());
            assert_eq!(output.sampling(), input.sampling());
            assert_eq!(output.pixels().len(), expected.len());
            for (pixel, (actual, expected)) in output.pixels().iter().zip(expected).enumerate() {
                assert_eq!(
                    actual.map(f32::to_bits),
                    *expected,
                    "phase {index}, pixel {pixel}"
                );
            }
            assert_eq!(input, original, "external processing cannot mutate Source");
            assert_eq!(session.worker_process_id(), pid);
            assert_eq!(session.processor_owner(), owner);
            let feedback = session.take_feedback().expect("actual accepted native invocation");
            assert_eq!(feedback.owner, owner);
            assert_eq!(feedback.snapshot.invocation(), index as u64 + 1);
            let actual = feedback.snapshot.native_owner();
            if let Some(previous) = native_owner {
                assert_eq!(
                    actual, previous,
                    "warm controls retain the original native instance"
                );
            }
            native_owner = Some(actual);
        }
    }));
    // Settle the original owner even on RED, preserving the primary assertion.
    let cleanup = session.shutdown();
    drop(session);
    if let Err(failure) = target {
        std::panic::resume_unwind(failure);
    }
    cleanup.expect("original external Filter bounded shutdown");
}
