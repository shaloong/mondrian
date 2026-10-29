use super::*;
use mondrian_core::automation::{InterpolationType, ParameterValueWrite, PropertyHost};
use mondrian_core::ColorFrameAlpha;
use mondrian_effects::{
    instantiate_effect_node_with_project_geometry, EffectColorDomain, EffectExecutionSession,
    EffectExecutionSessionConfig, EffectFrameExtent, EffectGraphNodeKind, EffectProcessorControl,
    EffectValueFormat, EffectWorkingPrecision,
};

fn size_id(effect: &mondrian_effects::EffectNode) -> mondrian_core::ParameterId {
    effect
        .properties
        .iter()
        .find(|(_, p)| p.descriptor.display_name == "size")
        .unwrap()
        .1
        .descriptor
        .parameter_id()
        .clone()
}
fn frame_context(geometry: mondrian_effects::EffectProjectGeometry) -> EffectFrameContext {
    EffectFrameContext::new(
        Rational::FPS_24,
        geometry,
        TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::new(13, 24).unwrap()).unwrap(),
    )
    .unwrap()
}
fn program(
    effect: &mondrian_effects::EffectNode,
    geometry: mondrian_effects::EffectProjectGeometry,
) -> Result<PreparedEffectProgram, mondrian_effects::EffectGraphBuildError> {
    PreparedEffectProgram::prepare_hierarchical_with_frame_context(
        std::slice::from_ref(effect),
        &[],
        &[],
        &[],
        WorkingColorSpace::LinearRec709,
        frame_context(geometry),
        &LutPreparationCache::uncached(),
    )
}

#[test]
#[ignore = "requires rebuilt parent, native-v12 Product, GeneratorProbe spatial_defaults and a fresh action log"]
fn contextual_generator_graph_preserves_owner_defaults_animation_reset_and_schema_admission() {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_GENERATOR_MODE").unwrap(),
        "spatial_defaults"
    );
    let binary = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_GENERATOR_BINARY").unwrap());
    let inspection = inspect_openfx_binary(product_executable(), &binary).unwrap();
    let kind = mondrian_app::openfx_effect::register_selected_openfx_generator(
        product_executable(),
        &inspection,
        "org.mondrian.fixture.ofx-generator",
    )
    .unwrap();
    assert!(instantiate_effect_node(kind.clone()).is_err());
    assert!(
        instantiate_effect_node_with_project_geometry(
            kind.clone(),
            mondrian_effects::EffectProjectGeometry::new(1, 1, SampleAspectRatio::SQUARE).unwrap()
        )
        .is_err(),
        "resolve before author schema bounds, without publishing a partial node"
    );
    let format = EffectValueFormat::new(
        EffectWorkingPrecision::Float32,
        EffectColorDomain::SceneLinearRgb,
        ColorFrameAlpha::StraightFloat,
    );
    for (width, height) in [(3, 2), (5, 4)] {
        let geometry = mondrian_effects::EffectProjectGeometry::new(
            width,
            height,
            SampleAspectRatio::new(2, 1).unwrap(),
        )
        .unwrap();
        let canonical = geometry.canonical_size();
        let mut effect =
            instantiate_effect_node_with_project_geometry(kind.clone(), geometry).unwrap();
        let id = size_id(&effect);
        assert_eq!(
            effect.evaluate_parameter(&id, TimelineTime::ZERO),
            Some(PropertyValue::DoubleVec2(canonical))
        );
        let defaults = program(&effect, geometry).unwrap();
        let binding = effect.params.clone();
        let address = effect.properties.unique_address_for_parameter_id(&id).unwrap();
        let (path, p) = effect.properties.property_by_address(&address).unwrap();
        let path = path.to_owned();
        let mut property = p.clone();
        property.set_animation_enabled(true);
        property
            .write_channels(
                TimelineTime::ZERO,
                &[(0, 1.0), (1, 1.0)],
                InterpolationType::Linear,
            )
            .unwrap();
        property
            .write_channels(
                TimelineTime::new(1, 4).unwrap(),
                &[(0, 2.0), (1, 3.0)],
                InterpolationType::Linear,
            )
            .unwrap();
        effect.properties.upsert(property);
        let animated = program(&effect, geometry).unwrap();
        let reset = effect
            .properties
            .prepare_parameter_write(
                &ParameterValueWrite::ResetToDefault { parameter: address },
                TimelineTime::ZERO,
            )
            .unwrap()
            .unwrap();
        effect.apply_property_mutation(reset).unwrap();
        assert_eq!(effect.params, binding);
        assert_eq!(
            effect.properties.property(&path).unwrap().static_value(),
            &PropertyValue::DoubleVec2(canonical)
        );
        assert!(!effect.properties.property(&path).unwrap().is_animated());
        let reset = program(&effect, geometry).unwrap();
        let reopened: mondrian_effects::EffectNode =
            serde_json::from_slice(&serde_json::to_vec(&effect).unwrap()).unwrap();
        assert_eq!(effect, reopened);
        let reopened = program(&reopened, geometry).unwrap();
        let wrong = mondrian_effects::EffectProjectGeometry::new(
            width + 1,
            height,
            SampleAspectRatio::new(2, 1).unwrap(),
        )
        .unwrap();
        assert!(
            program(&effect, wrong).is_err(),
            "a new canvas must not reinterpret persisted defaults"
        );
        let fresh = instantiate_effect_node_with_project_geometry(kind.clone(), geometry).unwrap();
        assert_eq!(
            fresh.evaluate_parameter(&id, TimelineTime::ZERO),
            Some(PropertyValue::DoubleVec2(canonical)),
            "registry was not edited by an instance"
        );
        let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_processor_instances: 1,
            max_processor_bytes: 512 * 1024 * 1024,
            ..EffectExecutionSessionConfig::uncached(512 * 1024 * 1024)
        });
        let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut owner = None;
            let mut invocation = 0;
            for (frame, size, program) in [
                (0, canonical, &defaults),
                (0, [1.0, 1.0], &animated),
                (6, [2.0, 3.0], &animated),
                (3, [1.5, 2.0], &animated),
                (6, canonical, &reset),
                (3, canonical, &reopened),
            ] {
                let graph = program.evaluate(TimelineTime::new(frame, 24).unwrap()).unwrap();
                assert!(graph.node_profiles().keys().all(|id| !matches!(
                    graph.graph().node(*id).unwrap().kind,
                    EffectGraphNodeKind::Source
                )));
                assert!(graph.node_profiles().keys().any(|id| matches!(
                    graph.graph().node(*id).unwrap().kind,
                    EffectGraphNodeKind::CustomSource { .. }
                )));
                let mut control = EffectProcessorControl::uncancelled();
                let output = session
                    .generate_compiled_rgba_f32(
                        EffectFrameExtent::new(3, 2),
                        format,
                        cpu_contract_sampling(2.0),
                        &graph,
                        0,
                        &mut control,
                    )
                    .unwrap();
                let expected = [
                    size[0] as f32,
                    size[1] as f32,
                    (canonical[0] * 0.5 + canonical[1] * 0.25 / 16.0 + 4.0 / 128.0 + 5.0 / 1024.0)
                        as f32,
                    0.75,
                ];
                assert_eq!(output.format(), format);
                assert_eq!(output.pixels().len(), 6);
                for pixel in output.pixels() {
                    assert_eq!(pixel.map(f32::to_bits), expected.map(f32::to_bits));
                }
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
        if let Err(panic) = target {
            std::panic::resume_unwind(panic);
        }
    }
}
