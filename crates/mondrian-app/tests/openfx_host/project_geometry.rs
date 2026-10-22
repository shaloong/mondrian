//! Owner project coordinates are independent of the fixed Filter image raster.
use super::*;
use mondrian_effects::{
    EffectExecutionSession, EffectExecutionSessionConfig, EffectFloatFrame, EffectFrameExtent,
    EffectProcessorControl, EffectProjectGeometry,
};

fn geometry(width: u32, height: u32) -> EffectProjectGeometry {
    EffectProjectGeometry::new(width, height, SampleAspectRatio::new(2, 1).unwrap()).unwrap()
}
fn input() -> EffectFloatFrame {
    EffectFloatFrame::from_pixels(
        EffectFrameExtent::new(3, 5),
        cpu_contract_input().format(),
        mondrian_core::ImageSampling::UNIT_SQUARE,
        vec![[-0.25, 0.5, 2.0, 0.25]; 15],
    )
    .unwrap()
}
fn literal(actual: &EffectFloatFrame, expected: [f32; 4]) {
    assert_eq!(actual.extent(), EffectFrameExtent::new(3, 5));
    assert_eq!(actual.format(), input().format());
    assert_eq!(actual.pixels().len(), 15);
    for pixel in actual.pixels() {
        assert_eq!(pixel.map(f32::to_bits), expected.map(f32::to_bits));
    }
}

#[test]
#[ignore = "current Product and SessionProbe project_geometry; independent 3x5 raster/owner windows, actual native owner reuse/replacement"]
fn owner_project_geometry_is_independent_of_filter_raster_and_replaces_native_owner() {
    let inspection = cpu_contract_probe("project_geometry");
    let identifier = "uk.co.thefoundry.BasicGainPlugin";
    let input = input();
    let original = input.clone();
    let first_geometry = geometry(10, 7);
    let second_geometry = geometry(12, 9);
    let mut direct = OpenFxFilterSession::prepare(
        product_executable(),
        &inspection,
        identifier,
        3,
        5,
        input.format(),
        first_geometry,
        &mut || Ok(()),
    )
    .expect("one explicitly bound project owner");
    let direct_owner = direct.processor_owner();
    let direct_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut native_owner = None;
        let mut invocation = 0;
        // Literal output encodes actual project Size/Extent, image PAR, and
        // the unchanged Basic gain algorithm. Project PAR remains 2 throughout.
        for (frame, image_par, gain, expected) in [
            (0.0, 1.0, 0.5, [0.1875, 0.359375, 1.0625, 0.125]),
            (6.0, 1.2, 0.25, [0.25, 0.234375, 0.575, 0.0625]),
            (3.0, 1.0, 0.5, [0.1875, 0.359375, 1.0625, 0.125]),
        ] {
            let timing = OpenFxRenderTiming { frame, ..cpu_contract_timing(image_par) };
            let parameters =
                constant_parameters([("scale".to_owned(), OpenFxParameterValue::Double(gain))]);
            let output = direct.render(&timing, &parameters, &input, &mut || Ok(())).unwrap();
            literal(&output, expected);
            let feedback = direct.take_feedback().expect("actual native receipt");
            assert_eq!(feedback.owner, direct_owner);
            let actual = feedback.snapshot.native_owner();
            if let Some(previous) = native_owner {
                assert_eq!(actual, previous);
            }
            native_owner = Some(actual);
            assert_eq!(feedback.snapshot.invocation(), invocation + 1);
            invocation = feedback.snapshot.invocation();
            assert_eq!(input, original);
        }
    }));
    let direct_cleanup = direct.shutdown();
    if let Err(failure) = direct_result {
        std::panic::resume_unwind(failure);
    }
    direct_cleanup.expect("consume direct native owner");

    let description =
        describe_openfx_filter(product_executable(), &inspection, identifier).unwrap();
    let scale_label = &description.parameters.iter().find(|row| row.name == "scale").unwrap().label;
    let kind =
        register_selected_openfx_filter(product_executable(), &inspection, identifier).unwrap();
    let mut effect = instantiate_effect_node(kind.clone()).unwrap();
    let scale = effect
        .properties
        .iter()
        .find(|(_, row)| row.descriptor.display_name == *scale_label)
        .expect("official Basic gain")
        .1
        .descriptor
        .parameter_id()
        .clone();
    let status = mondrian_effects::effect_extension_runtime_status(&kind.key()).unwrap();
    assert!(!status.disabled);
    assert!(matches!(
        PreparedEffectProgram::prepare(
            std::slice::from_ref(&effect),
            &[],
            WorkingColorSpace::LinearRec709,
        ),
        Err(mondrian_effects::EffectGraphBuildError::InvalidAuthorState { .. })
    ));
    assert_eq!(
        mondrian_effects::effect_extension_runtime_status(&kind.key()).unwrap(),
        status
    );
    let range =
        TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::new(101, 24).unwrap()).unwrap();
    let prepare = |effect: &mondrian_effects::EffectNode, geometry| {
        PreparedEffectProgram::prepare_hierarchical_with_frame_context(
            std::slice::from_ref(effect),
            &[],
            &[],
            &[],
            WorkingColorSpace::LinearRec709,
            EffectFrameContext::new(Rational::FPS_24, geometry, range).unwrap(),
            &LutPreparationCache::uncached(),
        )
        .unwrap()
    };
    effect
        .set_static_value_by_parameter(&scale, PropertyValue::Double(0.5))
        .unwrap();
    let first = prepare(&effect, first_geometry);
    let second = prepare(&effect, second_geometry);
    effect
        .set_static_value_by_parameter(&scale, PropertyValue::Double(0.25))
        .unwrap();
    let first_changed = prepare(&effect, first_geometry);
    let second_changed = prepare(&effect, second_geometry);
    let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
        max_processor_instances: 1,
        max_processor_bytes: 64 * 1024 * 1024,
        max_working_bytes: 128 * 1024 * 1024,
        ..EffectExecutionSessionConfig::uncached(128 * 1024 * 1024)
    });
    assert_eq!(session.diagnostics().processor_instances, 0);
    let graph_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut previous: Option<(
            EffectProjectGeometry,
            (
                mondrian_core::EffectProcessorOwnerId,
                mondrian_effects::EffectProcessorExecutionOrigin,
                u64,
            ),
        )> = None;
        for (frame, project, program, expected) in [
            (0, first_geometry, &first, [0.1875, 0.359375, 1.125, 0.125]),
            (
                6,
                first_geometry,
                &first_changed,
                [0.25, 0.234375, 0.625, 0.0625],
            ),
            (3, second_geometry, &second, [0.25, 0.390625, 1.125, 0.125]),
            (
                6,
                second_geometry,
                &second_changed,
                [0.3125, 0.265625, 0.625, 0.0625],
            ),
            (0, first_geometry, &first, [0.1875, 0.359375, 1.125, 0.125]),
        ] {
            let graph = program.evaluate(TimelineTime::new(frame, 24).unwrap()).unwrap();
            let mut control = EffectProcessorControl::uncancelled();
            let output = session
                .apply_compiled_rgba_f32(input.view(), &graph, -999, &mut control)
                .unwrap();
            literal(&output, expected);
            let batch = control.take_feedback().unwrap();
            assert_eq!(batch.len(), 1);
            let receipt = batch.iter().next().unwrap();
            assert_eq!(receipt.preparation.project_geometry, Some(project));
            assert_eq!(
                (receipt.preparation.width, receipt.preparation.height),
                (3, 5)
            );
            assert_ne!(receipt.feedback.owner, direct_owner);
            let actual = (
                receipt.feedback.owner,
                receipt.feedback.origin,
                receipt.feedback.snapshot.native_owner(),
            );
            assert!(matches!(
                receipt.feedback.origin,
                mondrian_effects::EffectProcessorExecutionOrigin::SupervisedChild { .. }
            ));
            if let Some((previous_project, previous_owner)) = previous {
                if previous_project == project {
                    assert_eq!(actual, previous_owner);
                } else {
                    assert_ne!(
                        actual.0, previous_owner.0,
                        "same raster with changed project replaces the actual owner"
                    );
                }
            }
            previous = Some((project, actual));
            assert_eq!(session.diagnostics().processor_instances, 1);
            assert_eq!(session.diagnostics().cache_entries, 0);
            assert_eq!(input, original);
        }
    }));
    session.retire_processor_resources();
    let retired = session.diagnostics().processor_instances == 0;
    drop(session);
    if let Err(failure) = graph_result {
        std::panic::resume_unwind(failure);
    }
    assert!(retired);
    assert_eq!(
        mondrian_effects::effect_extension_runtime_status(&kind.key()).unwrap(),
        status
    );
}
