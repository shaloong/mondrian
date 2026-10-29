//! Actual owned SDK bootstrap and real parameter-only DSP; not GUI compatibility.
use super::*;
use crate::initial_parameter_input::{InitialParameterStorage, InitialParameterValue};
use crate::plugin::PluginProcessingRole;

#[test]
#[ignore = "requires owned SDK initial-values DLL under native lab runner"]
fn native_initial_input_barriers_and_real_paused_flush_share_the_existing_queue() {
    let path = std::path::PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_INITIAL_VALUES_TEST_PLUGIN")
            .expect("owned existing SDK fixture"),
    );
    assert!(path.is_absolute());
    let policy = crate::PluginInterfaceSupport::none()
        .with(crate::PluginInterface::Component)
        .with(crate::PluginInterface::AudioProcessor)
        .with(crate::PluginInterface::EditController)
        .with(crate::PluginInterface::ConnectionPoint);
    let mut native =
        PluginImpl::load_with_class(&path, Some("61921CF43A2B4A68AE5AE08241D85E22"), policy)
            .unwrap();
    native.start_processing().unwrap();
    let retained = native.save_state().unwrap();
    let mut frame = crate::controller_parameters::ControllerParameterQueryFrame::default();
    let mut current = InitialParameterValue::default();
    assert_eq!(
        native
            .visit_controller_parameters_bounded(1, &mut frame, &mut |index, value| {
                assert_eq!(index, 0);
                current = InitialParameterValue {
                    id: value.id(),
                    normalized: value.normalized(),
                };
                Ok(())
            })
            .unwrap(),
        1
    );
    let handler = native.component_handler.as_ref().unwrap().clone();
    // An owned host callback control, delivered to the actual SDK handler.
    unsafe {
        assert_eq!(
            handler.performEdit(current.id, current.normalized),
            kResultOk
        );
        handler.restartComponent(20);
    }
    let source = native.editor_event_source_status().unwrap();
    let cohort = native.pending_restart_cohort().unwrap();
    native
        .handoff_initial_parameter_input(
            cohort,
            source,
            &[current],
            InitialParameterStorage::for_parameter_count(1).unwrap(),
        )
        .unwrap();
    assert!(native.pending_processor_parameter_input().unwrap());
    assert!(native.pending_param_changes.is_empty());
    assert!(native.editor_event_source_status().unwrap().closed_empty());
    assert!(matches!(
        native.save_state(),
        Err(Error::PendingParameterDelivery)
    ));
    assert!(matches!(
        native.load_state_with_context(&retained, &StateContext::default()),
        Err(Error::PendingParameterDelivery)
    ));
    assert!(matches!(
        native.setup_processing(),
        Err(Error::PendingParameterDelivery)
    ));
    assert!(matches!(
        native.set_process_mode(crate::plugin::ProcessMode::Offline),
        Err(Error::PendingParameterDelivery)
    ));
    assert!(matches!(
        native.capture_author_state_bounded(128 * 1024, 3 * 128 * 1024),
        Err(crate::configuration_capture::ConfigurationCaptureError::PendingParameterDelivery)
    ));
    assert!(matches!(
        native.reactivate_configuration_stopped(),
        Err(crate::configuration_capture::ConfigurationCaptureError::PendingParameterDelivery)
    ));
    assert!(native.is_processing && native.is_active);
    assert_eq!(handler.editor_event_source_status().pending_events(), 0);

    // Reach legal inactive-processing setup with real pending COM points and an
    // empty pending Vec. Refusals must precede native deactivation/replacement.
    native.stop_processing().unwrap();
    native.activate().unwrap();
    unsafe {
        assert_eq!(handler.restartComponent(2), kResultOk);
    }
    let retained_cohort = native.pending_restart_cohort();
    let retained_source = native.editor_event_source_status().unwrap();
    let snapshot = |native: &PluginImpl| {
        let data = native.process_data.as_ref().unwrap();
        let queues = data.input_param_changes.queues.lock().unwrap();
        let entries = queues
            .iter()
            .map(|queue| {
                let points = queue.points.lock().unwrap();
                (
                    queue.param_id(),
                    points.as_ptr() as usize,
                    points.capacity(),
                    points
                        .iter()
                        .map(|(offset, value)| (*offset, value.to_bits()))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        (
            native.current_setup(),
            native.applied_setup,
            native.is_active,
            native.is_processing,
            (
                native.bus_activation.audio_inputs.clone(),
                native.bus_activation.audio_outputs.clone(),
                native.bus_activation.event_inputs.clone(),
                native.bus_activation.event_outputs.clone(),
            ),
            (
                std::ptr::from_ref(data.as_ref()) as usize,
                queues.as_ptr() as usize,
                queues.capacity(),
                entries,
            ),
        )
    };
    let before_rejection = snapshot(&native);
    assert!(matches!(
        native.reconfigure(96000.0, 16),
        Err(Error::PendingParameterDelivery)
    ));
    assert!(matches!(
        native.set_bus_arrangements(
            &[crate::audio::SpeakerArrangement::STEREO],
            &[crate::audio::SpeakerArrangement::STEREO]
        ),
        Err(Error::PendingParameterDelivery)
    ));
    assert!(matches!(
        native.set_bus_active(
            crate::audio::MediaType::Audio,
            crate::audio::BusDirection::Input,
            0,
            false
        ),
        Err(Error::PendingParameterDelivery)
    ));
    assert!(matches!(
        native.create_process_data(),
        Err(Error::PendingParameterDelivery)
    ));
    assert!(matches!(
        native.service_host_requests(),
        Err(Error::PendingParameterDelivery)
    ));
    assert_eq!(snapshot(&native), before_rejection);
    assert_eq!(native.pending_restart_cohort(), retained_cohort);
    assert_eq!(
        native.editor_event_source_status().unwrap(),
        retained_source
    );
    assert!(native.pending_param_changes.is_empty());

    // set_audio_config only stages an existing host-side request. Its changed
    // setup cannot trigger any native work from start_processing while pending.
    let (old_rate, old_block) = (native.sample_rate, native.block_size);
    native.set_audio_config(96000.0, 16);
    let staged_request = snapshot(&native);
    assert!(matches!(
        native.start_processing(),
        Err(Error::PendingParameterDelivery)
    ));
    assert_eq!(snapshot(&native), staged_request);
    native.set_audio_config(old_rate, old_block);
    assert_eq!(snapshot(&native), before_rejection);
    // An unchanged start is still legal: it enables actual pending delivery.
    native.start_processing().unwrap();

    // Actual controller edit setup, then the same unconsumed Source enters the
    // existing pending Vec. No second parameter sink or fabricated completion.
    unsafe {
        assert_eq!(
            native
                .controller
                .as_ref()
                .unwrap()
                .setParamNormalized(current.id, 0.75),
            kResultOk
        );
        assert_eq!(handler.beginEdit(current.id), kResultOk);
        assert_eq!(handler.performEdit(current.id, 0.75), kResultOk);
        assert_eq!(handler.endEdit(current.id), kResultOk);
    }
    let mut buffers = BusAudioBuffers::new(
        &native.current_audio_bus_layout().unwrap(),
        8,
        native.sample_rate,
    );
    buffers.block_size = 0;
    for bus in buffers.inputs.iter_mut().chain(&mut buffers.outputs) {
        for channel in &mut bus.channels {
            channel.clear();
        }
    }
    {
        let mut dsp = native.process_fields();
        assert!(dsp.queue_unconsumed_editor_values().unwrap());
        dsp.process_bus_audio(&mut buffers).unwrap();
    }
    assert!(!native.pending_processor_parameter_input().unwrap());
    assert_eq!(
        handler
            .editor_event_source_status()
            .pending_dsp_feedback_values(),
        0
    );
    assert_eq!(handler.editor_event_source_status().pending_events(), 3);
    // Only after actual delivery may the original IO request be serviced. No
    // special flag clearing or replacement acknowledgement is synthesized.
    assert_eq!(native.pending_restart_cohort(), retained_cohort);
    assert_eq!(native.service_host_requests().unwrap().bits(), 2);
    assert!(!native.pending_restart_cohort().unwrap().pending());
    let captured = native
        .capture_author_state_bounded(128 * 1024, 3 * 128 * 1024)
        .unwrap()
        .into_bytes();
    let snapshot = decode_state_snapshot(&captured).unwrap();
    assert_eq!(&snapshot.component[..4], b"MIV1");
    assert_eq!(
        f64::from_le_bytes(snapshot.component[8..16].try_into().unwrap()),
        0.75
    );
    assert_eq!(
        u32::from_le_bytes(snapshot.component[20..24].try_into().unwrap()),
        1
    );
    // Capture is not an author Ack and leaves the real user source retained.
    assert_eq!(handler.editor_event_source_status().pending_events(), 3);

    // Real SDK rejection of an illegal parameter-only offset is not success.
    // The host's existing failed-Process policy clears attempted input, while
    // the unacknowledged author Source remains owned and uncaptured here.
    let source_before_failure = native.editor_event_source_status().unwrap();
    // Inject into the real COM input owner: ordinary host pending offsets are
    // intentionally clamped to this zero-sized chunk before they reach DSP.
    native
        .process_data
        .as_ref()
        .unwrap()
        .input_param_changes
        .enqueue(current.id, 1, 0.9);
    {
        let mut dsp = native.process_fields();
        assert!(
            matches!(dsp.process_bus_audio(&mut buffers), Err(Error::ProcessFailed(result)) if result == kInvalidArgument)
        );
    }
    assert_eq!(
        native.editor_event_source_status().unwrap(),
        source_before_failure
    );
    let failed_audit = native.save_state().unwrap();
    let failed_snapshot = decode_state_snapshot(&failed_audit).unwrap();
    assert_eq!(
        u32::from_le_bytes(failed_snapshot.component[20..24].try_into().unwrap()),
        1
    );
    assert_eq!(
        f64::from_le_bytes(failed_snapshot.component[8..16].try_into().unwrap()),
        0.75
    );
}

fn load_late_rescan_fixture(mode: u32) -> PluginImpl {
    let path = std::path::PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_INITIAL_VALUES_TEST_PLUGIN")
            .expect("rebuilt owned initial-values DLL with mode13"),
    );
    assert!(path.is_absolute() && path.is_file());
    let policy = crate::PluginInterfaceSupport::none()
        .with(crate::PluginInterface::Component)
        .with(crate::PluginInterface::AudioProcessor)
        .with(crate::PluginInterface::EditController)
        .with(crate::PluginInterface::ConnectionPoint);
    let mut native =
        PluginImpl::load_with_class(&path, Some("61921CF43A2B4A68AE5AE08241D85E22"), policy)
            .unwrap();
    let mut configuration = b"MIV1".to_vec();
    configuration.extend_from_slice(&1_u32.to_le_bytes());
    configuration.extend_from_slice(&0.375_f64.to_le_bytes());
    configuration.extend_from_slice(&mode.to_le_bytes());
    let state = encode_state_snapshot(&StateSnapshot {
        component: configuration.clone(),
        controller: Some(configuration),
    })
    .unwrap();
    native
        .load_state_with_context(&state, &StateContext::Project)
        .unwrap();
    native.reconfigure(48_000.0, 8).unwrap();
    assert!(!native.has_editor());
    native
}

#[derive(Debug)]
struct LateRescanAudit {
    component_gain: u64,
    controller_gain: u64,
    process: u32,
    metadata: u32,
    reads: u32,
    setters: u32,
    last_observed: u64,
    wrong_thread: u32,
}

fn late_rescan_audit(native: &PluginImpl, mode: u32) -> LateRescanAudit {
    // save_state services deferred controller values. Call only before enqueue
    // or after the explicit controller service, never to observe an unserviced queue.
    let bytes = native.save_state().unwrap();
    let state = decode_state_snapshot(&bytes).unwrap();
    let controller = state.controller.as_ref().unwrap();
    assert_eq!(state.component.len(), 24);
    assert_eq!(controller.len(), 44);
    for configuration in [&state.component, controller] {
        assert_eq!(&configuration[..4], b"MIV1");
        assert_eq!(&configuration[4..8], &1_u32.to_le_bytes());
        assert_eq!(&configuration[16..20], &mode.to_le_bytes());
    }
    LateRescanAudit {
        component_gain: u64::from_le_bytes(state.component[8..16].try_into().unwrap()),
        controller_gain: u64::from_le_bytes(controller[8..16].try_into().unwrap()),
        process: u32::from_le_bytes(state.component[20..24].try_into().unwrap()),
        metadata: u32::from_le_bytes(controller[20..24].try_into().unwrap()),
        reads: u32::from_le_bytes(controller[24..28].try_into().unwrap()),
        setters: u32::from_le_bytes(controller[28..32].try_into().unwrap()),
        last_observed: u64::from_le_bytes(controller[32..40].try_into().unwrap()),
        wrong_thread: u32::from_le_bytes(controller[40..44].try_into().unwrap()),
    }
}

#[test]
#[ignore = "requires rebuilt owned mode13 initial-values DLL under native lab runner"]
fn native_deferred_controller_mode13_retains_source_until_explicit_service() {
    use crate::plugin::{NativeEditorEvent, PluginEditorProcessingSplit};

    for mode in [0, 13] {
        let mut native = load_late_rescan_fixture(mode);
        native.start_processing().unwrap();
        let owner = native.native_editor_attachment_owner().unwrap();
        let mut frame = crate::controller_parameters::ControllerParameterQueryFrame::default();
        let mut current = InitialParameterValue::default();
        assert_eq!(
            native
                .visit_controller_parameters_bounded(1, &mut frame, &mut |index, value| {
                    assert_eq!(index, 0);
                    current = InitialParameterValue {
                        id: value.id(),
                        normalized: value.normalized(),
                    };
                    Ok(())
                })
                .unwrap(),
            1
        );
        assert_eq!((current.id, current.normalized), (0, 0.375));
        let initial_source = native.editor_event_source_status().unwrap();
        let initial_cohort = native.pending_restart_cohort().unwrap();
        assert_eq!(
            initial_cohort.flags().bits(),
            if mode == 13 { 4 } else { 0 }
        );
        assert_eq!(initial_source.pending_dsp_feedback_values(), 0);
        native
            .handoff_initial_parameter_input(
                initial_cohort,
                initial_source,
                &[current],
                InitialParameterStorage::for_parameter_count(1).unwrap(),
            )
            .unwrap();
        assert!(!native.pending_processor_parameter_input().unwrap());
        assert!(native.pending_param_changes.is_empty());
        let source = native.editor_event_source_status().unwrap();
        let cohort = native.pending_restart_cohort().unwrap();
        assert!(source.closed_empty() && !cohort.pending());
        let before = late_rescan_audit(&native, mode);
        assert_eq!((before.process, before.wrong_thread), (0, 0));
        assert_eq!(before.component_gain, 0.375_f64.to_bits());
        assert_eq!(before.controller_gain, 0.375_f64.to_bits());
        let setup = native.applied_setup;
        let data = std::ptr::from_ref(native.process_data.as_ref().unwrap().as_ref());
        let mut buffers = BusAudioBuffers::new(
            &native.current_audio_bus_layout().unwrap(),
            8,
            native.sample_rate,
        );
        assert_eq!((buffers.inputs.len(), buffers.outputs.len()), (1, 1));
        for (channel, samples) in buffers.inputs[0].channels.iter_mut().enumerate() {
            for (index, value) in samples.iter_mut().enumerate() {
                *value = ((channel + 1) * (index + 1)) as f32 / 16.0;
            }
        }
        let inputs = buffers.inputs.clone();
        for channel in &mut buffers.outputs[0].channels {
            channel.fill(-99.0);
        }
        {
            let PluginEditorProcessingSplit {
                editor,
                mut processor,
            } = native.split_editor_processing().unwrap();
            assert_eq!(editor.attachment_owner().unwrap(), owner);
            std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        processor.queue_parameter_at(0, 0.75, 3).unwrap();
                        processor.process_bus_audio(&mut buffers).unwrap();
                    })
                    .join()
                    .unwrap();
            });
            // These role observations do not service the deferred controller queue.
            assert_eq!(editor.pending_restart_cohort().unwrap(), Some(cohort));
            assert_eq!(editor.editor_event_source_status().unwrap(), source);
            assert_eq!(editor.parameter(0).unwrap().to_bits(), 0.375_f64.to_bits());
            assert_eq!(editor.pending_restart_cohort().unwrap(), Some(cohort));
            assert_eq!(editor.editor_event_source_status().unwrap(), source);
        }
        assert_eq!(buffers.inputs, inputs);
        for (input, output) in inputs[0].channels.iter().zip(&buffers.outputs[0].channels) {
            for (index, (&input, &output)) in input.iter().zip(output).enumerate() {
                let gain = if index < 3 { 0.375 } else { 0.75 };
                assert_eq!(output.to_bits(), (input * gain).to_bits());
            }
        }
        assert!(native.pending_param_changes.is_empty());
        assert!(!native.pending_processor_parameter_input().unwrap());
        assert_eq!(native.deferred_controller_sync.len(), 1);
        let (served_source, served_cohort);
        {
            // A full/joined owner can service the same no-GUI controller role.
            let mut editor = native.borrow_editor_control().unwrap();
            assert_eq!(editor.attachment_owner().unwrap(), owner);
            editor.service_controller().unwrap();
            served_source = editor.editor_event_source_status().unwrap();
            served_cohort = editor.pending_restart_cohort().unwrap().unwrap();
            assert_eq!(editor.parameter(0).unwrap().to_bits(), 0.75_f64.to_bits());
            assert_eq!(editor.editor_event_source_status().unwrap(), served_source);
            assert_eq!(
                editor.pending_restart_cohort().unwrap(),
                Some(served_cohort)
            );
            editor.service_controller().unwrap();
            assert_eq!(editor.parameter(0).unwrap().to_bits(), 0.75_f64.to_bits());
            assert_eq!(editor.editor_event_source_status().unwrap(), served_source);
            assert_eq!(
                editor.pending_restart_cohort().unwrap(),
                Some(served_cohort)
            );
        }
        assert!(native.deferred_controller_sync.is_empty());
        assert_eq!(served_source.pending_dsp_feedback_values(), 0);
        assert_eq!(served_source.open_parameter_edits(), 0);
        assert_eq!(served_source.group_depth(), 0);
        assert_eq!(served_source.failure(), None);
        let mut events = Vec::with_capacity(1);
        let mut feedback = Vec::with_capacity(1);
        assert_eq!(
            native
                .observe_editor_source_bounded(1, &mut events, &mut feedback)
                .unwrap(),
            served_source
        );
        assert!(feedback.is_empty());
        if mode == 13 {
            assert_eq!(served_cohort.serial(), cohort.serial() + 1);
            assert_eq!(served_cohort.flags().bits(), 4);
            assert_eq!(served_source.revision(), source.revision() + 1);
            assert_eq!(served_source.pending_events(), 1);
            assert_eq!(
                events,
                vec![NativeEditorEvent::ParameterRescanRequested {
                    flags: 4,
                    cohort: served_cohort,
                }]
            );
        } else {
            assert_eq!(served_cohort, cohort);
            assert_eq!(served_source, source);
            assert!(events.is_empty());
        }
        let after = late_rescan_audit(&native, mode);
        assert_eq!(after.process, before.process + 1);
        assert_eq!(after.setters, before.setters + 1);
        assert_eq!(after.reads, before.reads + 3);
        assert_eq!(after.metadata, before.metadata);
        assert_eq!(after.wrong_thread, 0);
        assert_eq!(after.last_observed, 0.75_f64.to_bits());
        assert_eq!(after.component_gain, 0.75_f64.to_bits());
        assert_eq!(after.controller_gain, 0.75_f64.to_bits());
        assert_eq!(native.native_editor_attachment_owner(), Some(owner));
        assert_eq!(native.applied_setup, setup);
        assert_eq!(native.current_setup().sample_rate, 48_000.0);
        assert_eq!(native.current_setup().block_size, 8);
        assert_eq!(
            std::ptr::from_ref(native.process_data.as_ref().unwrap().as_ref()),
            data
        );
        assert!(native.is_active && native.is_processing);
        assert_eq!(native.editor_event_source_status().unwrap(), served_source);
        assert_eq!(native.pending_restart_cohort(), Some(served_cohort));
        // Neither observation nor controller service acknowledges the new Source/raw request.
        native.stop_processing().unwrap();
    }
}

#[test]
#[ignore = "requires rebuilt owned mode13 initial-values DLL under native lab runner"]
fn native_mode13_invalid_unarmed_and_missing_handler_setters_do_not_fake_rescans() {
    let mut native = load_late_rescan_fixture(13);
    let controller = native.controller.as_ref().unwrap().clone();
    let handler = native.component_handler.as_ref().unwrap().clone();
    let source = native.editor_event_source_status().unwrap();
    let cohort = native.pending_restart_cohort().unwrap();
    let before = late_rescan_audit(&native, 13);
    unsafe {
        // Only setProcessing(true) arms the actual fixture through its connection.
        assert_eq!(controller.setParamNormalized(0, 0.5), kResultOk);
        assert_eq!(
            controller.getParamNormalized(0).to_bits(),
            0.5_f64.to_bits()
        );
    }
    assert_eq!(native.editor_event_source_status().unwrap(), source);
    assert_eq!(native.pending_restart_cohort(), Some(cohort));
    native.start_processing().unwrap();
    let invalid = [
        (1, 0.75),
        (0, f64::NAN),
        (0, f64::INFINITY),
        (0, f64::NEG_INFINITY),
        (0, -0.125),
        (0, 1.125),
    ];
    for (id, value) in invalid {
        unsafe {
            assert_eq!(controller.setParamNormalized(id, value), kInvalidArgument);
            assert_eq!(
                controller.getParamNormalized(0).to_bits(),
                0.5_f64.to_bits()
            );
        }
        assert_eq!(native.editor_event_source_status().unwrap(), source);
        assert_eq!(native.pending_restart_cohort(), Some(cohort));
    }
    unsafe {
        assert_eq!(
            controller.setComponentHandler(std::ptr::null_mut()),
            kResultOk
        );
        assert_eq!(controller.setParamNormalized(0, 0.75), kResultFalse);
        // A refused callback does not roll back an already accepted controller value.
        assert_eq!(
            controller.getParamNormalized(0).to_bits(),
            0.75_f64.to_bits()
        );
        let retained = handler.to_com_ptr::<IComponentHandler>().unwrap();
        assert_eq!(controller.setComponentHandler(retained.as_ptr()), kResultOk);
    }
    assert_eq!(native.editor_event_source_status().unwrap(), source);
    assert_eq!(native.pending_restart_cohort(), Some(cohort));
    let after = late_rescan_audit(&native, 13);
    assert_eq!(after.setters, before.setters + invalid.len() as u32 + 2);
    assert_eq!(after.reads, before.reads + invalid.len() as u32 + 2);
    assert_eq!(after.process, before.process);
    assert_eq!(after.wrong_thread, 0);
    assert_eq!(after.component_gain, 0.375_f64.to_bits());
    assert_eq!(after.controller_gain, 0.75_f64.to_bits());
    assert_eq!(after.last_observed, 0.75_f64.to_bits());
    native.stop_processing().unwrap();
}

#[test]
#[ignore = "requires rebuilt owned mode14 initial-values DLL under native lab runner"]
fn native_initial_com_origin_flushes_real_handoff_without_context_or_source_replay() {
    use crate::plugin::PendingParameterFlushOutcome as Outcome;
    let mut native = load_late_rescan_fixture(14);
    native.start_processing().unwrap();
    let owner = native.native_editor_attachment_owner();
    let before = late_rescan_audit(&native, 14); // Before initial input is published.
    assert_eq!(before.component_gain, 0.375_f64.to_bits());
    assert_eq!(before.controller_gain, 0.75_f64.to_bits());
    assert_eq!(before.process, 0);
    let mut frame = crate::controller_parameters::ControllerParameterQueryFrame::default();
    let mut current = InitialParameterValue::default();
    assert_eq!(
        native
            .visit_controller_parameters_bounded(1, &mut frame, &mut |index, value| {
                assert_eq!(index, 0);
                current = InitialParameterValue {
                    id: value.id(),
                    normalized: value.normalized(),
                };
                Ok(())
            })
            .unwrap(),
        1
    );
    assert_eq!((current.id, current.normalized), (0, 0.75));
    let source = native.editor_event_source_status().unwrap();
    assert_eq!(
        (
            source.pending_events(),
            source.pending_dsp_feedback_values()
        ),
        (1, 1)
    );
    let cohort = native.pending_restart_cohort().unwrap();
    assert!(!cohort.pending());
    native
        .handoff_initial_parameter_input(
            cohort,
            source,
            &[current],
            InitialParameterStorage::for_parameter_count(1).unwrap(),
        )
        .unwrap();
    let source = native.editor_event_source_status().unwrap();
    assert!(source.closed_empty());
    assert!(native.pending_param_changes.is_empty());
    assert!(native.pending_processor_parameter_input().unwrap());
    assert!(matches!(
        native.save_state(),
        Err(Error::PendingParameterDelivery)
    ));
    native
        .process_fields()
        .set_process_timeline_context(crate::audio::ProcessTimelineContext {
            sample_position: -19,
            sample_rate: 48_000,
        })
        .unwrap();
    let context = native.process_data.as_ref().unwrap().process_context;
    let input_pointer = native
        .process_data
        .as_ref()
        .unwrap()
        .process_data
        .inputParameterChanges;
    assert_eq!(
        native
            .process_fields()
            .flush_pending_parameter_changes()
            .unwrap(),
        Outcome::Flushed
    );
    assert_eq!(native.native_editor_attachment_owner(), owner);
    assert_eq!(native.editor_event_source_status().unwrap(), source);
    assert_eq!(native.pending_restart_cohort(), Some(cohort));
    assert_eq!(
        native
            .process_data
            .as_ref()
            .unwrap()
            .process_data
            .inputParameterChanges,
        input_pointer
    );
    let after_context = native.process_data.as_ref().unwrap().process_context;
    assert_eq!(
        (
            after_context.projectTimeSamples,
            after_context.continousTimeSamples,
            after_context.state
        ),
        (
            context.projectTimeSamples,
            context.continousTimeSamples,
            context.state
        )
    );
    assert!(!native.pending_processor_parameter_input().unwrap());
    let after = late_rescan_audit(&native, 14);
    assert_eq!(after.component_gain, 0.75_f64.to_bits());
    assert_eq!(after.controller_gain, 0.75_f64.to_bits());
    assert_eq!(
        (after.process, after.setters, after.wrong_thread),
        (before.process + 1, before.setters, 0)
    );
    assert_eq!(
        native
            .process_fields()
            .flush_pending_parameter_changes()
            .unwrap(),
        Outcome::NoPending
    );
    assert_eq!(late_rescan_audit(&native, 14).process, after.process);
    let mut buffers = BusAudioBuffers::new(
        &native.current_audio_bus_layout().unwrap(),
        4,
        native.sample_rate,
    );
    for bus in &mut buffers.inputs {
        for channel in &mut bus.channels {
            channel.fill(1.0);
        }
    }
    native
        .process_fields()
        .process_bus_audio(&mut buffers)
        .unwrap();
    for bus in &buffers.outputs {
        for channel in &bus.channels {
            assert_eq!(channel, &[0.75; 4]);
        }
    }
    assert_eq!(late_rescan_audit(&native, 14).process, after.process + 1);
    assert_eq!(native.editor_event_source_status().unwrap(), source);
    assert_eq!(native.native_editor_attachment_owner(), owner);
}
