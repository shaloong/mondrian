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
    qualify_public_generator_history(false);
}

#[test]
#[ignore = "requires native-v12 Product and published Misc Constant Generator"]
fn contextual_generator_public_resize_rebind_reset_history_and_reopen_preserve_output() {
    qualify_public_generator_history(true);
}

#[test]
#[ignore = "requires wire14 Product and published Misc Constant under native supervision"]
fn native_generator_timeline_preview_export_history_and_reopen_share_pixels() {
    use super::preview_cpu_execution::composite_resolved_preview_working;
    use super::preview_timeline_execution::{
        resolve_preview_timeline_with_programs, PreviewTimelineResolution,
    };
    use mondrian_core::Color;
    use mondrian_renderer::{PreparedVisualProgramCache, TimelineCompositeScratch};
    use std::cell::RefCell;

    let _guard = crate::app_ui::test_utils::theme_test_guard();
    let helper = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_PRODUCT_BINARY").unwrap());
    let binary = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_INVERT_BINARY").unwrap());
    let inspection = crate::openfx_adapter::inspect_openfx_binary(&helper, &binary).unwrap();
    let kind = crate::openfx_effect::register_selected_openfx_generator(
        &helper,
        &inspection,
        "net.sf.openfx.ConstantPlugin",
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut state = AppState::new();
    state
        .create_new_project_at(
            directory.path().join("timeline.mdp"),
            "Generator Timeline",
            16,
            16,
            Rational::FPS_24,
        )
        .unwrap();
    let track_id = state.active_sequence().unwrap().video_tracks[0].id;
    let clip_id = state
        .create_solid_color_on_video_track_with_color(
            track_id,
            Some(0),
            mondrian_timeline::ClipOverlapMode::Overwrite,
            Color { r: 0.75, g: 0.125, b: 0.5, a: 1.0 },
        )
        .unwrap();
    state.add_effect_to_clip(clip_id, kind).unwrap();
    let effect = state.active_sequence().unwrap().find_clip(clip_id).unwrap().effects[0].clone();
    let effect_id = effect.id;
    let color = effect
        .properties
        .iter()
        .find(|(_, p)| matches!(p.static_value(), PropertyValue::DoubleColor(_)))
        .unwrap()
        .1
        .address();
    let write_color = |state: &mut AppState, value| {
        state
            .dispatch_action(
                ProductAction::VisualEffect(VisualEffectProductAction::SetParameterValue(
                    Box::new(VisualEffectSetParameterValuePayload {
                        clip_id,
                        effect_id,
                        write: ParameterValueWrite::Value {
                            parameter: color.clone(),
                            value: PropertyValue::DoubleColor(value),
                        },
                    }),
                ))
                .into_external_action(),
            )
            .unwrap();
    };
    let scratch = RefCell::new(TimelineCompositeScratch::default());
    let programs = RefCell::new(PreparedVisualProgramCache::default());
    let execute = |state: &AppState,
                   expected: [f32; 4],
                   scratch: &RefCell<TimelineCompositeScratch>,
                   programs: &RefCell<PreparedVisualProgramCache>| {
        let sequence = state.active_sequence().unwrap();
        let target = sequence.settings.resolution;
        let context = sequence
            .settings
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
            .unwrap();
        let sampling = ImageSampling::new(
            sequence.settings.pixel_aspect_ratio.exact_ratio().unwrap(),
            [Rational::new(1, 1); 2],
        )
        .unwrap();
        let range = mondrian_export::preset::TimelineExportRange::WorkArea {
            start_frame: 0,
            end_frame_exclusive: 12,
        };
        let prepared =
            mondrian_export::prepare_timeline_export_dependencies(sequence, &[], range, false)
                .unwrap();
        let export_snapshot = mondrian_export::preset::TimelineExportSnapshot::captured(
            mondrian_core::ProjectColorEnvironment::default(),
            sequence.clone(),
            vec![],
            std::collections::HashMap::new(),
            range,
            prepared.execution_snapshot().clone(),
        );
        let mut owner = None;
        let mut invocation = 0;
        for frame in [0, 6, 3, 6] {
            let resolved = resolve_preview_timeline_with_programs(
                sequence,
                &[],
                frame,
                target,
                mondrian_playback::PreviewResolutionScale::Full,
                context.clone(),
                &mut |_| panic!("generated Solid has no media dependency"),
                &mut |_| panic!("generated Solid has no title dependency"),
                programs,
                scratch,
            );
            let resolved = match resolved {
                PreviewTimelineResolution::Ready(resolved) => resolved,
                PreviewTimelineResolution::Unavailable { reason } => panic!("{}", reason.detail()),
                _ => panic!("complete generated Timeline must resolve"),
            };
            let mut control = EffectProcessorControl::uncancelled();
            let output = composite_resolved_preview_working(
                target.width,
                target.height,
                &resolved.plan.elements,
                &resolved.plan.color_context,
                sampling,
                &mut scratch.borrow_mut(),
                &mut control,
            )
            .unwrap();
            assert_eq!(output.frame.rgba_f32().data.len(), 256);
            for pixel in &output.frame.rgba_f32().data {
                assert_eq!(pixel.map(f32::to_bits), expected.map(f32::to_bits));
            }
            assert_eq!(output.composite_diagnostics.float_linear_composites, 1);
            let feedback = control.take_feedback().unwrap();
            assert_eq!(feedback.len(), 1);
            let receipt = feedback.iter().next().unwrap();
            assert_eq!(
                receipt.preparation.input_role,
                mondrian_effects::EffectFloatProcessorInputRole::NoImage
            );
            assert_eq!(
                receipt.retention,
                mondrian_effects::EffectProcessorRetention::Retained
            );
            let current = (
                receipt.feedback.owner,
                receipt.feedback.origin,
                receipt.feedback.snapshot.native_owner(),
            );
            if let Some(previous) = owner {
                assert_eq!(current, previous);
            } else {
                owner = Some(current);
            }
            assert!(receipt.feedback.snapshot.invocation() > invocation);
            invocation = receipt.feedback.snapshot.invocation();
            let diagnostics = scratch.borrow().effect_execution_diagnostics();
            assert_eq!(diagnostics.processor_instances, 1);
            assert_eq!(diagnostics.cache_entries, 0);
            if frame == 3 {
                let export = mondrian_export::queue::export_visual_frame_validation(
                    &export_snapshot,
                    frame,
                    target,
                )
                .unwrap();
                assert_eq!(resolved.semantic_trace, export.semantic_trace);
                assert_eq!(
                    output.frame, export.working_frame,
                    "same prepared semantics and actual native working pixels"
                );
            }
        }
        owner.unwrap()
    };
    let edited = [0.5, 0.25, 0.75, 0.5];
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let initial = execute(&state, [0.0; 4], &scratch, &programs);
        write_color(&mut state, edited.map(f64::from));
        assert_eq!(execute(&state, edited, &scratch, &programs), initial);
        assert!(state.undo_timeline().unwrap());
        assert_eq!(execute(&state, [0.0; 4], &scratch, &programs), initial);
        assert!(state.redo_timeline().unwrap());
        assert_eq!(execute(&state, edited, &scratch, &programs), initial);
        let saved =
            serde_json::to_value(state.active_sequence().unwrap().find_clip(clip_id).unwrap())
                .unwrap();
        let path = directory.path().join("saved.mdp");
        state.save_project_file_as(path.clone()).unwrap();
        (path, saved, initial)
    }));
    *scratch.borrow_mut() = TimelineCompositeScratch::default();
    assert_eq!(
        scratch.borrow().effect_execution_diagnostics().processor_instances,
        0
    );
    let (path, saved, initial) = result.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    state.close_project().unwrap();
    drop(state);
    let mut reopened = AppState::new();
    reopened.open_project_file(path).unwrap();
    assert_eq!(
        serde_json::to_value(reopened.active_sequence().unwrap().find_clip(clip_id).unwrap())
            .unwrap(),
        saved
    );
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        execute(&reopened, edited, &scratch, &programs)
    }));
    *scratch.borrow_mut() = TimelineCompositeScratch::default();
    assert_eq!(
        scratch.borrow().effect_execution_diagnostics().processor_instances,
        0
    );
    assert_ne!(
        result.unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
        initial
    );
    reopened.close_project().unwrap();
}

fn qualify_public_generator_history(resize: bool) {
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
            16,
            16,
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
    let mut initial = snapshot(&state);
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
    assert_eq!(
        size.static_value(),
        &PropertyValue::DoubleVec2([16.0, 16.0])
    );
    let size_address = size.address();
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
                    EffectFrameExtent::new(geometry.width(), geometry.height()),
                    format,
                    ImageSampling::new(geometry.pixel_aspect_ratio(), [Rational::new(1, 1); 2])
                        .unwrap(),
                    &graph,
                    0,
                    &mut control,
                )
                .expect("actual native pixels");
            assert_eq!(output.format(), format);
            assert_eq!(
                output.pixels().len(),
                geometry.width() as usize * geometry.height() as usize
            );
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
        write(
            &mut state,
            ParameterValueWrite::Value {
                parameter: size_address.clone(),
                value: PropertyValue::DoubleVec2([1.0, 1.0]),
            },
        );
        let explicit_seed = snapshot(&state);
        assert_eq!(
            explicit_seed
                .properties
                .property_by_address(&size_address)
                .unwrap()
                .1
                .static_value(),
            &PropertyValue::DoubleVec2([1.0, 1.0])
        );
        write(
            &mut state,
            ParameterValueWrite::ResetToDefault { parameter: size_address.clone() },
        );
        assert_eq!(
            snapshot(&state),
            initial,
            "public Reset restores the resolved schema default"
        );
        assert!(state.undo_timeline().unwrap());
        assert_eq!(snapshot(&state), explicit_seed);
        assert!(state.redo_timeline().unwrap());
        assert_eq!(snapshot(&state), initial);
        let previous_owner = if resize {
            Some(execute(&state, &mut session, [0.0; 4]))
        } else {
            None
        };
        if resize {
            let previous_settings = state.active_sequence().unwrap().settings.clone();
            let mut settings = previous_settings.clone();
            settings.resolution = Resolution { width: 32, height: 16 };
            settings.pixel_aspect_ratio =
                mondrian_timeline::sequence::PixelAspectRatio::Anamorphic2x;
            let generation = state.project_author_generation();
            state
                .update_active_sequence_settings(settings.clone())
                .expect("public geometry transaction");
            assert_eq!(state.project_author_generation(), generation + 1);
            let migrated = snapshot(&state);
            let mut expected = initial.clone();
            let mut property =
                expected.properties.property_by_address(&size_address).unwrap().1.clone();
            property.descriptor.schema.default_value = PropertyValue::DoubleVec2([64.0, 16.0]);
            expected.properties.upsert(property);
            expected.params = migrated.params.clone();
            assert_ne!(initial.params, migrated.params);
            assert_eq!(
                migrated, expected,
                "current canonical values and every identity are preserved"
            );
            assert!(state.undo_timeline().unwrap());
            assert_eq!(state.active_sequence().unwrap().settings, previous_settings);
            assert_eq!(snapshot(&state), initial);
            assert!(state.redo_timeline().unwrap());
            assert_eq!(state.active_sequence().unwrap().settings, settings);
            assert_eq!(snapshot(&state), migrated);
            // Retire this test-owned old-window bank before admitting a fresh
            // native owner. This is not Preview lifecycle qualification.
            session.retire_processor_resources();
            assert_eq!(session.diagnostics().processor_instances, 0);
            // Keep this qualification inside the existing full-window Generator
            // boundary. Partial/expanded RoD needs its separate scheduling work.
            write(
                &mut state,
                ParameterValueWrite::ResetToDefault { parameter: size_address.clone() },
            );
            initial = snapshot(&state);
            assert_eq!(
                initial.properties.property_by_address(&size_address).unwrap().1.static_value(),
                &PropertyValue::DoubleVec2([64.0, 16.0])
            );
        }
        let owner = execute(&state, &mut session, [0.0; 4]);
        if let Some(previous) = previous_owner {
            assert_ne!(owner, previous, "new window requires a new native owner");
        }
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
    let saved_settings = state.active_sequence().unwrap().settings.clone();
    state.close_project().expect("Close Project");
    drop(state);
    let mut reopened = AppState::new();
    reopened.open_project_file(path).expect("fresh public Open Project");
    assert_eq!(reopened.active_sequence().unwrap().settings, saved_settings);
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
