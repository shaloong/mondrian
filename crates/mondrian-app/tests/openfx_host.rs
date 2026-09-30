//! Real OpenFX Float32 filter admission through the supervised product worker.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mondrian_app::openfx_adapter::inspect_openfx_binary;
use mondrian_app::openfx_effect::register_selected_openfx_filter;
use mondrian_app::openfx_host::{
    describe_openfx_filter, render_openfx_filter_frame, OpenFxDoubleType, OpenFxFilterSession,
    OpenFxFloatFrame, OpenFxHostError, OpenFxRenderTiming, OpenFxScalarValue,
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

    let component_input = OpenFxFloatFrame {
        width: 3,
        height: 5,
        pixels: (0..15)
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
    };
    let component_parameters = BTreeMap::from([
        ("scale".to_owned(), OpenFxScalarValue::Double(0.5)),
        (
            "scaleComponents".to_owned(),
            OpenFxScalarValue::Boolean(true),
        ),
        ("scaleR".to_owned(), OpenFxScalarValue::Double(2.0)),
        ("scaleG".to_owned(), OpenFxScalarValue::Double(0.5)),
        ("scaleB".to_owned(), OpenFxScalarValue::Double(1.0)),
        ("scaleA".to_owned(), OpenFxScalarValue::Double(3.0)),
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
    )
    .expect("render per-component authored gains without clipping HDR or alpha");
    for (actual, source) in component_output.pixels.iter().zip(&component_input.pixels) {
        for (actual, expected) in actual
            .iter()
            .zip(source.iter().zip([1.0, 0.25, 0.5, 1.5]).map(|(value, gain)| value * gain))
        {
            assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
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
        &mut checkpoint(&cancellation),
    )
    .expect("preview worker");
    let mut export = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
        2,
        3,
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
    let mut input = OpenFxFloatFrame {
        width: 2,
        height: 3,
        pixels: vec![[-0.25, 0.5, 2.0, 0.25]; 6],
    };
    for (frame, gain, aspect) in [
        (0.0, 1.5, 1.0),
        (0.0, 2.0, 1.0),
        (42.0, 0.75, 1.2),
        (1.0, 1.25, 1.0),
    ] {
        timing.frame = frame;
        timing.pixel_aspect_ratio = aspect;
        let values = BTreeMap::from([("scale".to_owned(), OpenFxScalarValue::Double(gain))]);
        let output = preview
            .render(&timing, &values, &input, &mut checkpoint(&cancellation))
            .expect("resident render");
        assert_eq!(preview.worker_process_id(), preview_id);
        for (actual, source) in output.pixels.iter().zip(&input.pixels) {
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
    input.pixels.fill([0.5, -1.0, 3.0, 0.75]);
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
    assert!(matches!(
        preview.render(&timing, &BTreeMap::new(), &input, &mut || Ok(())),
        Err(OpenFxHostError::Unavailable(_))
    ));
    // Retiring one consumer never releases the other consumer's worker/mapping.
    let live = mondrian_core::ExecutionCancellationToken::new();
    assert_eq!(
        export
            .render(&timing, &BTreeMap::new(), &input, &mut checkpoint(&live))
            .expect("surviving export"),
        input
    );
    export.shutdown().expect("native destruction and child reap");
    export.shutdown().expect("idempotent shutdown");
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and MONDRIAN_OPENFX_SESSION_PROBE_MODE=hang"]
fn resident_worker_cancellation_retires_and_reaps_a_hung_native_render() {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("hang fixture mode"),
        "hang"
    );
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
    let input = OpenFxFloatFrame { width: 1, height: 1, pixels: vec![[0.25; 4]] };
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
    assert_eq!(input.pixels, [[0.25; 4]]);
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
    let input = OpenFxFloatFrame { width: 1, height: 1, pixels: vec![[0.25; 4]] };
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
    assert_eq!(input.pixels, [[0.25; 4]]);
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
    let mut input = OpenFxFloatFrame {
        width: 257,
        height: 257,
        pixels: (0..257 * 257)
            .map(|index| {
                [
                    (index % 257) as f32 / 128.0 - 0.5,
                    (index / 257) as f32 / 128.0,
                    1.0 + (index % 17) as f32 / 16.0,
                    (index % 257) as f32 / 256.0,
                ]
            })
            .collect(),
    };
    let gain = BTreeMap::from([("scale".to_owned(), OpenFxScalarValue::Double(1.5))]);
    let output = session
        .render(&timing, &gain, &input, &mut || Ok(()))
        .expect("complete transfer");
    assert_eq!(output.pixels.len(), input.pixels.len());
    for (actual, source) in output.pixels.iter().zip(&input.pixels) {
        for (actual, source) in actual.iter().zip(source) {
            assert_eq!(*actual, *source * 1.5);
        }
    }
    input.pixels.reverse();
    timing.frame = 41.0;
    assert_eq!(
        session
            .render(&timing, &BTreeMap::new(), &input, &mut || Ok(()))
            .expect("fresh borrowed Source raster"),
        input
    );
    session.shutdown().expect("shutdown");
}
