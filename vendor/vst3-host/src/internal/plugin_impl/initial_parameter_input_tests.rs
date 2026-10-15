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
