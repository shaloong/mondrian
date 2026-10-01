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
    EffectNodeExt, LutPreparationCache, PreparedEffectProgram,
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
        SampleAspectRatio::SQUARE,
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
    // Two executor rasters plus three controlled host rasters must fit before
    // any native session is constructed. The one-byte shortfall stays retryable.
    let mut config = EffectExecutionSessionConfig {
        max_working_bytes: 79,
        ..EffectExecutionSessionConfig::default()
    };
    let mut session = EffectExecutionSession::new(config);
    session.bind_generation(1);
    assert!(session
        .apply_compiled_rgba_f32(
            &input,
            1,
            1,
            &graph,
            0,
            &mut mondrian_effects::EffectProcessorControl::uncancelled()
        )
        .is_err());
    assert_eq!(session.diagnostics().processor_instances, 0);
    config.max_working_bytes = 80;
    session.reconfigure(config);
    assert_eq!(
        session
            .apply_compiled_rgba_f32(
                &input,
                1,
                1,
                &graph,
                0,
                &mut mondrian_effects::EffectProcessorControl::uncancelled()
            )
            .expect("first native frame"),
        input
    );
    assert_eq!(session.diagnostics().processor_instances, 1);
    assert_eq!(session.diagnostics().processor_bytes, 48);
    // The probe only omits Output on its second render in the same native
    // instance. A fresh process per frame would incorrectly succeed here.
    // A new frame generation must preserve that same worker; pixel residency
    // remains separate from frame-independent native resources.
    session.bind_generation(2);
    assert!(session
        .apply_compiled_rgba_f32(
            &input,
            1,
            1,
            &graph,
            1,
            &mut mondrian_effects::EffectProcessorControl::uncancelled()
        )
        .is_err());
    assert_eq!(input, [[-0.25, 1.5, 0.75, 0.5]]);
    assert_eq!(session.diagnostics().processor_instances, 0);
    session.bind_generation(3);
    assert_eq!(
        session
            .apply_compiled_rgba_f32(
                &input,
                1,
                1,
                &graph,
                0,
                &mut mondrian_effects::EffectProcessorControl::uncancelled()
            )
            .expect("fresh generation"),
        input
    );
}

#[test]
#[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY and MONDRIAN_OPENFX_SESSION_PROBE_MODE=hang"]
fn authored_native_factory_observes_mixed_execution_deadline() {
    use mondrian_effects::{
        effect_plugin_runtime_status, EffectColorDomain, EffectExecutionEnvironment,
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
        &input,
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
    assert_eq!(input, [[0.25, 1.5, -0.75, 0.5]]);
    assert_eq!(session.diagnostics().processor_instances, 0);
    let status = effect_plugin_runtime_status(&key).expect("plugin status");
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
        &input,
        1,
        1,
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
    assert_eq!(input, [[0.25, 1.5, -0.75, 0.5]]);

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
    let status = mondrian_effects::effect_plugin_runtime_status(key).expect("plugin status");
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
    let required = count * 16 * 5; // Two executor rasters and three host rasters.
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
            &input,
            width,
            height,
            &graph,
            0,
            &mut EffectProcessorControl::uncancelled()
        )
        .is_err());
    assert_eq!(session.diagnostics().processor_instances, 0);
    config.max_working_bytes = 768 * 1024 * 1024;
    session.reconfigure(config);
    let output = session
        .apply_compiled_rgba_f32(
            &input,
            width,
            height,
            &graph,
            0,
            &mut EffectProcessorControl::uncancelled(),
        )
        .expect("UHD native Filter within Professional grant");
    assert_eq!(output, input);
    drop(output);
    assert_eq!(session.diagnostics().processor_instances, 1);
    assert_eq!(session.diagnostics().processor_bytes, count * 16 * 3);
    // Seek with changed Source content; only pixel residency is generation-bound.
    session.bind_generation(2);
    input.reverse();
    let output = session
        .apply_compiled_rgba_f32(
            &input,
            width,
            height,
            &graph,
            7,
            &mut EffectProcessorControl::uncancelled(),
        )
        .expect("changed UHD frame in retained bank");
    assert_eq!(output, input);
    drop(output);
    assert_eq!(session.diagnostics().processor_instances, 1);
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
    assert_eq!(count.default_value, OpenFxScalarValue::Integer(2));
    assert_eq!(count.minimum, Some(0.0));
    assert_eq!(count.maximum, Some(9.0));
    assert!(count.can_animate);
    let input = OpenFxFloatFrame {
        width: 2,
        height: 2,
        pixels: vec![[0.1, 0.2, 0.3, 0.05]; 4],
    };
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
            &BTreeMap::from([("count".to_owned(), OpenFxScalarValue::Integer(value))]),
            &input,
        )
        .expect("native integer");
        for (actual, original) in output.pixels.iter().zip(&input.pixels) {
            for channel in 0..4 {
                assert!((actual[channel] - original[channel] * value as f32).abs() < 1e-6);
            }
        }
    }
    for value in [
        OpenFxScalarValue::Integer(-1),
        OpenFxScalarValue::Integer(10),
        OpenFxScalarValue::Double(1.5),
        OpenFxScalarValue::Boolean(true),
    ] {
        assert!(render_openfx_filter_frame(
            product_executable(),
            &inspection,
            identifier,
            &timing,
            &BTreeMap::from([("count".to_owned(), value)]),
            &input
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
    let context = EffectFrameContext::new(Rational::FPS_24, SampleAspectRatio::SQUARE, range)
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
    let pixels = apply_compiled_effect_graph_rgba_f32(&input.pixels, 2, 2, &graph, 1191)
        .expect("native graph");
    for (actual, original) in pixels.iter().zip(&input.pixels) {
        for channel in 0..4 {
            assert!((actual[channel] - original[channel] * 4.0).abs() < 1e-6);
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
    let context = EffectFrameContext::new(Rational::FPS_24, SampleAspectRatio::SQUARE, range)
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
        let output = apply_compiled_effect_graph_rgba_f32(&input, 2, 2, &graph, 1291)
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
