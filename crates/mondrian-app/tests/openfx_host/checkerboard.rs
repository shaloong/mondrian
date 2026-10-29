//! Published plugin qualification using hand-written full-image literals.
use super::*;
use mondrian_core::{
    automation::{InterpolationType, ParameterValueWrite, PropertyHost},
    ColorFrameAlpha, ExecutionCancellationToken,
};
use mondrian_effects::{
    instantiate_effect_node_with_project_geometry, EffectColorDomain, EffectExecutionSession,
    EffectExecutionSessionConfig, EffectFrameExtent, EffectGraphNodeKind, EffectProcessorControl,
    EffectProjectGeometry, EffectValueFormat, EffectWorkingPrecision,
};

const COLORS: [[f64; 4]; 4] = [
    [0.125, 0.25, 0.5, 1.0],
    [-0.125, 1.5, 0.25, 1.0],
    [0.75, 0.125, 0.5, 1.0],
    [0.25, 0.5, 0.75, 1.0],
];

fn assert_board(output: &EffectFloatFrame, par: u32, phase: usize) {
    // Explicit top-down rows, independently fixed before invoking the plugin.
    // Phases: canonical box width4, width2, interpolated width3, Reset64.
    // Each row repeats twice in the 8x4 image. Four distinct colors expose
    // vertical reversal; PAR1/PAR2 expose accidental coordinate rescaling.
    let (top, bottom) = match (par, phase) {
        (1, 0) | (1, 3) | (2, 3) => ([1, 1, 1, 1, 0, 0, 0, 0], [2, 2, 2, 2, 3, 3, 3, 3]),
        (1, 1) | (2, 0) => ([0, 0, 1, 1, 0, 0, 1, 1], [3, 3, 2, 2, 3, 3, 2, 2]),
        (1, 2) => ([0, 1, 1, 1, 0, 0, 0, 1], [3, 2, 2, 2, 3, 3, 3, 2]),
        (2, 1) => ([0, 1, 0, 1, 0, 1, 0, 1], [3, 2, 3, 2, 3, 2, 3, 2]),
        (2, 2) => ([1, 0, 0, 1, 0, 0, 1, 0], [2, 3, 3, 2, 3, 3, 2, 3]),
        _ => panic!("unsupported literal"),
    };
    assert_eq!(output.extent(), EffectFrameExtent::new(8, 4));
    // This published Generator uses the base Generator ClipPreferences and
    // negotiates Opaque for this case. Keep the PM request below and require
    // the actual negotiated signal, including through the source-free graph.
    assert_eq!(
        output.format(),
        EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::Opaque,
        )
    );
    assert_eq!(output.pixels().len(), 32);
    for (index, pixel) in output.pixels().iter().enumerate() {
        let row = if index / 8 < 2 { top } else { bottom };
        let expected = COLORS[row[index % 8]].map(|v| (v as f32).to_bits());
        assert_eq!(
            pixel.map(f32::to_bits),
            expected,
            "PAR{par}/phase{phase}/pixel{index}"
        );
    }
}
fn format() -> EffectValueFormat {
    EffectValueFormat::new(
        EffectWorkingPrecision::Float32,
        EffectColorDomain::SceneLinearRgb,
        ColorFrameAlpha::PremultipliedFloat,
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

#[test]
#[ignore = "requires native-v12 Product and published Natron-2.3.16-beta3 Misc.ofx"]
fn published_checkerboard_direct_and_graph_preserve_par_orientation_curves_and_reset() {
    let binary =
        PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_INVERT_BINARY").expect("published Misc"));
    let inspection = inspect_openfx_binary(product_executable(), &binary).unwrap();
    let identifier = "net.sf.openfx.CheckerBoardPlugin";
    let description = mondrian_app::openfx_host::describe_openfx_generator(
        product_executable(),
        &inspection,
        identifier,
    )
    .unwrap();
    assert_eq!(description.parameters.len(), 18);
    let box_description = description.parameters.iter().find(|p| p.name == "boxSize").unwrap();
    assert_eq!(
        box_description.default_value,
        OpenFxParameterValue::Double2([64.0, 64.0])
    );
    assert_eq!(
        box_description.spatial_defaults,
        Some(mondrian_app::openfx_host::OpenFxSpatialDefaults::Canonical)
    );
    let kind = mondrian_app::openfx_effect::register_selected_openfx_generator(
        product_executable(),
        &inspection,
        identifier,
    )
    .unwrap();
    for par in [1, 2] {
        let geometry =
            EffectProjectGeometry::new(8, 4, SampleAspectRatio::new(par, 1).unwrap()).unwrap();
        let sampling = cpu_contract_sampling(f64::from(par));
        let mut values = BTreeMap::new();
        for (index, color) in COLORS.iter().enumerate() {
            values.insert(
                format!("color{index}"),
                OpenFxParameterBinding::constant(OpenFxParameterValue::Rgba(*color)),
            );
        }
        for name in ["lineWidth", "centerlineWidth"] {
            values.insert(
                name.into(),
                OpenFxParameterBinding::constant(OpenFxParameterValue::Double(0.0)),
            );
        }
        let token = ExecutionCancellationToken::new();
        let mut direct = OpenFxFilterSession::prepare_generator(
            product_executable(),
            &inspection,
            identifier,
            8,
            4,
            format(),
            geometry,
            &mut checkpoint(&token),
        )
        .unwrap();
        let owner = direct.processor_owner();
        let pid = direct.worker_process_id();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for (frame, width, phase) in
                [(0.0, 4.0, 0), (6.0, 2.0, 1), (3.0, 3.0, 2), (6.0, 2.0, 1)]
            {
                values.insert(
                    "boxSize".into(),
                    OpenFxParameterBinding::constant(OpenFxParameterValue::Double2([width, 2.0])),
                );
                let output = direct
                    .generate(&timing(frame), sampling, &values, &mut checkpoint(&token))
                    .expect("actual published Direct Render");
                assert_board(&output, par, phase);
                assert_eq!(direct.processor_owner(), owner);
                assert_eq!(direct.worker_process_id(), pid);
            }
        }));
        let shutdown = direct.shutdown();
        drop(direct);
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
        shutdown.expect("original bounded Direct shutdown");

        let mut effect =
            instantiate_effect_node_with_project_geometry(kind.clone(), geometry).unwrap();
        let id = |name: &str| {
            let label = &description.parameters.iter().find(|p| p.name == name).unwrap().label;
            effect
                .properties
                .iter()
                .find(|(_, p)| &p.descriptor.display_name == label)
                .unwrap()
                .1
                .descriptor
                .parameter_id()
                .clone()
        };
        let box_id = id("boxSize");
        let author_values = values
            .iter()
            .filter(|(name, _)| name.as_str() != "boxSize")
            .map(|(name, binding)| {
                let value = match &binding.value {
                    OpenFxParameterValue::Rgba(v) => PropertyValue::DoubleColor(*v),
                    OpenFxParameterValue::Double(v) => PropertyValue::Double(*v),
                    _ => unreachable!(),
                };
                (id(name), value)
            })
            .collect::<Vec<_>>();
        for (id, value) in author_values {
            effect.set_static_value_by_parameter(&id, value).unwrap();
        }
        assert_eq!(
            effect.evaluate_parameter(&box_id, TimelineTime::ZERO),
            Some(PropertyValue::DoubleVec2([64.0, 64.0])),
            "canonical defaults must not scale with PAR"
        );
        let address = effect.properties.unique_address_for_parameter_id(&box_id).unwrap();
        let mut property = effect.properties.property_by_address(&address).unwrap().1.clone();
        property.set_animation_enabled(true);
        for (time, width) in [
            (TimelineTime::ZERO, 4.0),
            (TimelineTime::new(1, 4).unwrap(), 2.0),
        ] {
            property
                .write_channels(time, &[(0, width), (1, 2.0)], InterpolationType::Linear)
                .unwrap();
        }
        effect.properties.upsert(property);
        let frame_context = EffectFrameContext::new(
            Rational::FPS_24,
            geometry,
            TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::new(13, 24).unwrap()).unwrap(),
        )
        .unwrap();
        let prepare = |effect: &mondrian_effects::EffectNode| {
            PreparedEffectProgram::prepare_hierarchical_with_frame_context(
                std::slice::from_ref(effect),
                &[],
                &[],
                &[],
                WorkingColorSpace::LinearRec709,
                frame_context,
                &LutPreparationCache::uncached(),
            )
            .unwrap()
        };
        let animated = prepare(&effect);
        let restored: mondrian_effects::EffectNode =
            serde_json::from_slice(&serde_json::to_vec(&effect).unwrap()).unwrap();
        assert_eq!(restored, effect);
        let restored = prepare(&restored);
        let reset = effect
            .properties
            .prepare_parameter_write(
                &ParameterValueWrite::ResetToDefault { parameter: address },
                TimelineTime::ZERO,
            )
            .unwrap()
            .unwrap();
        effect.apply_property_mutation(reset).unwrap();
        assert_eq!(
            effect.evaluate_parameter(&box_id, TimelineTime::ZERO),
            Some(PropertyValue::DoubleVec2([64.0, 64.0]))
        );
        let reset = prepare(&effect);
        let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_processor_instances: 1,
            max_processor_bytes: 512 * 1024 * 1024,
            ..EffectExecutionSessionConfig::uncached(512 * 1024 * 1024)
        });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut owner = None;
            let mut invocation = 0;
            for (frame, phase, program) in [
                (0, 0, &animated),
                (6, 1, &animated),
                (3, 2, &animated),
                (6, 1, &animated),
                (0, 3, &reset),
                (3, 2, &restored),
            ] {
                let graph = program.evaluate(TimelineTime::new(frame, 24).unwrap()).unwrap();
                assert!(graph.node_profiles().keys().all(|id| !matches!(
                    graph.graph().node(*id).unwrap().kind,
                    EffectGraphNodeKind::Source
                )));
                let mut control = EffectProcessorControl::uncancelled();
                let output = session
                    .generate_compiled_rgba_f32(
                        EffectFrameExtent::new(8, 4),
                        format(),
                        sampling,
                        &graph,
                        0,
                        &mut control,
                    )
                    .expect("actual published Graph Render");
                assert_board(&output, par, phase);
                let feedback = control.take_feedback().unwrap();
                assert_eq!(feedback.len(), 1);
                let receipt = feedback.iter().next().unwrap();
                assert_eq!(
                    receipt.preparation.input_role,
                    mondrian_effects::EffectFloatProcessorInputRole::NoImage
                );
                let identity = (
                    receipt.feedback.owner,
                    receipt.feedback.origin,
                    receipt.feedback.snapshot.native_owner(),
                );
                assert!(matches!(
                    receipt.feedback.origin,
                    mondrian_effects::EffectProcessorExecutionOrigin::SupervisedChild { .. }
                ));
                if let Some(previous) = owner {
                    assert_eq!(identity, previous);
                } else {
                    owner = Some(identity);
                }
                assert!(receipt.feedback.snapshot.invocation() > invocation);
                invocation = receipt.feedback.snapshot.invocation();
                assert_eq!(session.diagnostics().processor_instances, 1);
                assert_eq!(session.diagnostics().cache_entries, 0);
            }
        }));
        session.retire_processor_resources();
        assert_eq!(session.diagnostics().processor_instances, 0);
        drop(session);
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
