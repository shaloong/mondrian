use super::product_action::{
    ProductAction, TimelineProductAction, VisualEffectProductAction,
    VisualEffectSetParameterValuePayload,
};
use super::*;
use mondrian_core::{
    automation::ParameterValueWrite, ColorFrameAlpha, ImageSampling, PropertyValue,
    TimelineTimeRange,
};
use mondrian_effects::{
    EffectColorDomain, EffectExecutionSession, EffectExecutionSessionConfig, EffectFrameContext,
    EffectFrameExtent, EffectGraphNodeKind, EffectProcessorControl, EffectProjectGeometry,
    EffectValueFormat, EffectWorkingPrecision, LutPreparationCache, PreparedEffectProgram,
};

#[test]
#[ignore = "requires native-v12 Product and published Misc Constant Generator"]
fn contextual_generator_public_reset_history_save_and_reopen_preserve_output() {
    let _guard = crate::app_ui::test_utils::theme_test_guard();
    let helper =
        PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_PRODUCT_BINARY").expect("product"));
    let binary = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_INVERT_BINARY").expect("Misc"));
    let inspection =
        crate::openfx_adapter::inspect_openfx_binary(&helper, &binary).expect("inspect");
    let kind = crate::openfx_effect::register_selected_openfx_generator(
        &helper,
        &inspection,
        "net.sf.openfx.ConstantPlugin",
    )
    .expect("register contextual Generator");
    let description = crate::openfx_host::describe_openfx_generator(
        &helper,
        &inspection,
        "net.sf.openfx.ConstantPlugin",
    )
    .expect("real descriptor");
    let size_label = &description.parameters.iter().find(|p| p.name == "size").expect("size").label;
    let directory = tempfile::tempdir().expect("project directory");
    let mut state = AppState::new();
    state
        .create_new_project_at(
            directory.path().join("owner.mdp"),
            "Spatial",
            6,
            2,
            Rational::FPS_24,
        )
        .expect("Project");
    state
        .dispatch_action(
            ProductAction::Timeline(TimelineProductAction::CreateBasicTitle).into_external_action(),
        )
        .expect("title");
    let clip_id = state
        .active_sequence()
        .unwrap()
        .video_tracks
        .iter()
        .find_map(|track| track.clips.first().map(|clip| clip.id))
        .expect("Clip");
    state.select_clip_by_id(clip_id);
    state.add_effect_to_clip(clip_id, kind).expect("owner-aware add effect");
    let effect_id = state
        .active_sequence()
        .unwrap()
        .find_clip(clip_id)
        .unwrap()
        .effects
        .last()
        .unwrap()
        .id;
    let snapshot = |state: &AppState| {
        state
            .active_sequence()
            .unwrap()
            .find_clip(clip_id)
            .unwrap()
            .effects
            .iter()
            .find(|effect| effect.id == effect_id)
            .unwrap()
            .clone()
    };
    let initial = snapshot(&state);
    let color = initial
        .properties
        .iter()
        .find(|(_, p)| matches!(p.static_value(), PropertyValue::DoubleColor(_)))
        .expect("RGBA")
        .1
        .address();
    let size = initial
        .properties
        .iter()
        .find(|(_, p)| &p.descriptor.display_name == size_label)
        .expect("Size")
        .1;
    assert_eq!(size.static_value(), &PropertyValue::DoubleVec2([6.0, 2.0]));
    assert_ne!(
        initial.params,
        serde_json::json!({}),
        "construction binding must persist"
    );
    let write = |state: &mut AppState, write| {
        state
            .dispatch_action(
                ProductAction::VisualEffect(VisualEffectProductAction::SetParameterValue(
                    Box::new(VisualEffectSetParameterValuePayload { clip_id, effect_id, write }),
                ))
                .into_external_action(),
            )
            .expect("canonical parameter transaction")
    };
    let make_session = || {
        EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_processor_instances: 1,
            max_processor_bytes: 512 * 1024 * 1024,
            ..EffectExecutionSessionConfig::uncached(512 * 1024 * 1024)
        })
    };
    let execute = |state: &AppState, session: &mut EffectExecutionSession, expected: [f32; 4]| {
        let sequence = state.active_sequence().unwrap();
        let geometry = EffectProjectGeometry::new(
            sequence.settings.resolution.width,
            sequence.settings.resolution.height,
            sequence.settings.pixel_aspect_ratio.exact_ratio().unwrap(),
        )
        .unwrap();
        let context = EffectFrameContext::new(
            sequence.settings.frame_rate,
            geometry,
            TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
        )
        .unwrap();
        let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
            &sequence.find_clip(clip_id).unwrap().effects,
            &[],
            &[],
            &[],
            sequence.settings.color.working_color_space,
            context,
            &LutPreparationCache::uncached(),
        )
        .expect("saved author graph");
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::PremultipliedFloat,
        );
        let mut owner = None;
        for frame in [0, 6, 3, 6] {
            let graph = program.evaluate(TimelineTime::new(frame, 24).unwrap()).unwrap();
            assert!(graph.node_profiles().keys().all(|id| !matches!(
                graph.graph().node(*id).unwrap().kind,
                EffectGraphNodeKind::Source
            )));
            let mut control = EffectProcessorControl::uncancelled();
            let output = session
                .generate_compiled_rgba_f32(
                    EffectFrameExtent::new(6, 2),
                    format,
                    ImageSampling::UNIT_SQUARE,
                    &graph,
                    0,
                    &mut control,
                )
                .expect("actual native pixels");
            assert_eq!(output.format(), format);
            assert_eq!(output.pixels().len(), 12);
            for pixel in output.pixels() {
                assert_eq!(pixel.map(f32::to_bits), expected.map(f32::to_bits));
            }
            let feedback = control.take_feedback().expect("native feedback");
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
            assert_eq!(session.diagnostics().processor_instances, 1);
            assert_eq!(session.diagnostics().cache_entries, 0);
        }
        owner.unwrap()
    };
    let mut session = make_session();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let owner = execute(&state, &mut session, [0.0; 4]);
        let edited = [0.25, 0.5, 0.75, 1.0];
        write(
            &mut state,
            ParameterValueWrite::Value {
                parameter: color.clone(),
                value: PropertyValue::DoubleColor(edited.map(f64::from)),
            },
        );
        let saved = snapshot(&state);
        assert_eq!(execute(&state, &mut session, edited), owner);
        write(
            &mut state,
            ParameterValueWrite::ResetToDefault { parameter: color.clone() },
        );
        assert_eq!(snapshot(&state), initial);
        assert_eq!(execute(&state, &mut session, [0.0; 4]), owner);
        assert!(state.undo_timeline().unwrap());
        assert_eq!(snapshot(&state), saved);
        assert_eq!(execute(&state, &mut session, edited), owner);
        assert!(state.redo_timeline().unwrap());
        assert_eq!(snapshot(&state), initial);
        assert_eq!(execute(&state, &mut session, [0.0; 4]), owner);
        assert!(state.undo_timeline().unwrap());
        let path = directory.path().join("saved.mdp");
        state.save_project_file_as(path.clone()).expect("public durable Save As");
        (path, saved)
    }));
    session.retire_processor_resources();
    assert_eq!(session.diagnostics().processor_instances, 0);
    drop(session);
    let (path, saved) = result.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    state.close_project().expect("Close Project");
    drop(state);
    let mut reopened = AppState::new();
    reopened.open_project_file(path).expect("fresh public Open Project");
    assert_eq!(snapshot(&reopened), saved);
    assert!(!reopened.can_undo_action());
    assert!(!reopened.can_redo_action());
    assert_eq!(
        reopened.asset_library().unwrap().list_assets().unwrap().len(),
        0
    );
    let mut session = make_session();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        execute(&reopened, &mut session, [0.25, 0.5, 0.75, 1.0])
    }));
    session.retire_processor_resources();
    assert_eq!(session.diagnostics().processor_instances, 0);
    drop(session);
    result.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    reopened.close_project().expect("close reopened project");
}
