//! Actual SDK DLL controls for the initial-only consumer; ignored without fixture.
use super::super::protocol::SharedParameterEvent;
use super::*;

pub(super) fn state(gain: f64, mode: u32) -> Vec<u8> {
    let mut native = Vec::from(&b"MIV1"[..]);
    native.extend_from_slice(&1_u32.to_le_bytes());
    native.extend_from_slice(&gain.to_le_bytes());
    native.extend_from_slice(&mode.to_le_bytes());
    let mut bytes = Vec::from(&b"VST3HOST_STATE\0\0"[..]);
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(&(native.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(native.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&native);
    bytes.extend_from_slice(&native);
    bytes
}

pub(super) fn prepare(gain: f64, mode: u32) -> Vst3Worker {
    let binary_path = std::env::var_os("MONDRIAN_VST3_INITIAL_VALUES_TEST_PLUGIN")
        .map(PathBuf::from)
        .expect("set exact newly built initial-values DLL path");
    assert!(binary_path.is_absolute());
    let parameter = Vst3ParameterDescriptor {
        id: 0,
        name: "Gain".to_owned(),
        default_normalized: 0.5,
        step_count: 0,
        can_automate: true,
        read_only: false,
        flags: 1,
    };
    let parameter_id = parameter.parameter_id().expect("native parameter identity");
    let render_contract = crate::AudioRenderContract {
        sample_rate: 48000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 8,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: usize::MAX,
        public_output_lookahead_budget_frames: usize::MAX,
        compensation_delay_scratch_budget_bytes: usize::MAX,
    };
    let payload = Vst3Payload {
        schema_version: PAYLOAD_VERSION,
        main_audio_input: Vst3MainAudioInput::Present,
        event_bus_contract: Vst3EventBusContractV1::empty(),
        class_id: "61921CF43A2B4A68AE5AE08241D85E22".to_owned(),
        vendor: Some("Vendor".to_owned()),
        binary_sha256: fingerprint(&binary_path).expect("fixture hash"),
        binary_path,
        state: Some(state(gain, mode)),
        parameters: vec![parameter],
        parameter_ids: vec![0],
    };
    let request = IsolatedAudioProcessorWorkerPrepareRequest {
        musical_time_reference: None,
        auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
        note_input_contract: crate::AudioProcessorNoteInputContract::default(),
        native_lane: native_worker_lane_profile_for_parameters(&payload.parameters)
            .expect("actual known layout"),
        payload: serde_json::to_vec(&payload).expect("exact authored fixture payload"),
        plugin_execution_contract: AudioProcessorExecutionContract::new(
            0,
            AudioProcessorTail::None,
            true,
            true,
            true,
            0,
        )
        .expect("zero latency fixture"),
        auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
        parameter_ids: vec![parameter_id],
        render_contract,
    };
    Vst3AudioProcessorWorkerFactory
        .prepare_native(request)
        .expect("real native factory prepare")
}

#[derive(Debug)]
pub(super) struct Audit {
    pub(super) process: u32,
    pub(super) metadata: u32,
    pub(super) values: u32,
    pub(super) setters: u32,
    pub(super) observed: f64,
    pub(super) wrong_thread: u32,
}
pub(super) fn audit(worker: &Vst3Worker) -> Audit {
    let bytes = worker
        .plugin
        .as_ref()
        .expect("same actual object")
        .save_state()
        .expect("actual native component/controller audit streams");
    assert_eq!(&bytes[..16], b"VST3HOST_STATE\0\0");
    assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 1);
    let component = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as usize;
    let controller = u32::from_le_bytes(bytes[24..28].try_into().unwrap()) as usize;
    assert_eq!(component, 24);
    assert_eq!(controller, 44);
    assert_eq!(bytes.len(), 28 + component + controller);
    let native = &bytes[28..28 + component];
    let control = &bytes[28 + component..];
    assert_eq!(&native[..4], b"MIV1");
    assert_eq!(&control[..4], b"MIV1");
    let u32_at =
        |bytes: &[u8], index| u32::from_le_bytes(bytes[index..index + 4].try_into().unwrap());
    Audit {
        process: u32_at(native, 20),
        metadata: u32_at(control, 20),
        values: u32_at(control, 24),
        setters: u32_at(control, 28),
        observed: f64::from_le_bytes(control[32..40].try_into().unwrap()),
        wrong_thread: u32_at(control, 40),
    }
}

fn process(worker: &mut Vst3Worker, start: i64) -> Vec<f32> {
    let mut pcm = vec![1.0; 8];
    let events = [SharedParameterEvent { lane: 0, sample_offset: 2, value: 0.75 }];
    let ids = [worker.payload.parameters[0].parameter_id().unwrap()];
    let auxiliary = AudioProcessorAuxiliaryInputContract::default();
    worker
        .process(IsolatedAudioProcessorWorkerBlock::new(
            start,
            4,
            worker.render,
            &mut pcm,
            &auxiliary,
            &[],
            &ids,
            &events,
        ))
        .expect("real finite SDK process");
    pcm
}

fn pending(worker: &Vst3Worker, flags: u32) {
    assert_eq!(
        worker.pending_reprepare(),
        Some(mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags })
    );
    assert!(worker.last_parameter_values.iter().all(Option::is_none));
}

#[test]
#[ignore = "requires newly built owned SDK initial-values DLL under native lab runner"]
fn native_initial_values_reads_controller_without_setter_or_synthetic_process() {
    for gain in [0.375, 0.8125] {
        let mut worker = prepare(gain, 1);
        pending(&worker, 4);
        let cohort = worker.plugin.as_ref().unwrap().pending_restart_cohort().unwrap();
        let object = worker.native_audio_object_observation().unwrap();
        let payload = worker.payload.state.clone();
        let before = audit(&worker);
        worker.enter_state(73).expect("pure initial cohort genuinely consumed");
        let after = audit(&worker);
        assert_eq!(worker.native_audio_object_observation().unwrap(), object);
        assert_eq!(worker.payload.state, payload);
        assert!(worker.last_parameter_values.iter().all(Option::is_none));
        assert_eq!(worker.pending_reprepare(), None);
        let retired = worker.plugin.as_ref().unwrap().pending_restart_cohort().unwrap();
        assert_eq!(retired.serial(), cohort.serial());
        assert!(!retired.pending());
        assert_eq!(after.metadata, before.metadata + 1);
        assert_eq!(after.values, before.values + 1);
        assert_eq!(after.observed, gain);
        assert_eq!(after.setters, before.setters);
        assert_eq!(after.process, 0);
        assert_eq!(after.wrong_thread, 0);
        assert_eq!(
            process(&mut worker, 73),
            vec![
                gain as f32,
                gain as f32,
                gain as f32,
                gain as f32,
                0.75,
                0.75,
                0.75,
                0.75
            ]
        );
        assert_eq!(worker.last_parameter_values, vec![Some(0.75)]);
        assert_eq!(audit(&worker).process, 1);
    }
}

#[test]
#[ignore = "requires newly built owned SDK initial-values DLL under native lab runner"]
fn native_initial_values_refuses_mixed_schema_invalid_reentrant_and_count_controls() {
    for mode in 2..=12 {
        let mut worker = prepare(0.375, mode);
        let flags = match mode {
            2 => 12,
            3 => 16,
            _ => 4,
        };
        pending(&worker, flags);
        let cohort = worker.plugin.as_ref().unwrap().pending_restart_cohort().unwrap();
        let object = worker.native_audio_object_observation().unwrap();
        let payload = worker.payload.state.clone();
        let before = audit(&worker);
        assert!(
            matches!(worker.enter_state(73), Err(AudioProcessorHostError::ReprepareRequired(
            mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: actual })) if actual == flags)
        );
        pending(&worker, flags);
        let after = audit(&worker);
        let retained = worker.plugin.as_ref().unwrap().pending_restart_cohort().unwrap();
        if mode == 6 {
            assert!(retained.serial() > cohort.serial());
        } else {
            assert_eq!(retained, cohort);
        }
        assert_eq!(worker.native_audio_object_observation().unwrap(), object);
        assert_eq!(worker.payload.state, payload);
        assert_eq!(after.setters, before.setters);
        assert_eq!(after.process, 0);
        assert_eq!(after.wrong_thread, 0);
        if matches!(mode, 2 | 3 | 10 | 11 | 12) {
            assert_eq!(after.metadata, before.metadata);
            assert_eq!(after.values, before.values);
        }
        if mode == 9 {
            assert!(!worker
                .plugin
                .as_ref()
                .unwrap()
                .editor_event_source_status()
                .unwrap()
                .closed_empty());
        }
        let mut pcm = [1.0; 8];
        let ids = [worker.payload.parameters[0].parameter_id().unwrap()];
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        assert_eq!(worker.last_end, None);
        let source_before_process =
            worker.plugin.as_ref().unwrap().editor_event_source_status().unwrap();
        // Enter was refused, so this direct worker call has no accepted anchor.
        // The existing signal continuity guard owns its precise Process error;
        // the parent preflight owns restart refusal before dispatching DSP.
        let refused = worker.process(IsolatedAudioProcessorWorkerBlock::new(
            73,
            4,
            worker.render,
            &mut pcm,
            &auxiliary,
            &[],
            &ids,
            &[],
        ));
        assert!(
            matches!(&refused, Err(AudioProcessorHostError::Process(message))
                if message == "VST3 block violates continuity, extent, or parameter lanes"),
            "mode {mode}: {refused:?}"
        );
        assert_eq!(worker.last_end, None);
        pending(&worker, flags);
        assert_eq!(
            worker.plugin.as_ref().unwrap().pending_restart_cohort(),
            Some(retained)
        );
        assert_eq!(
            worker.plugin.as_ref().unwrap().editor_event_source_status().unwrap(),
            source_before_process
        );
        assert_eq!(pcm, [1.0; 8]);
        assert_eq!(audit(&worker).process, 0);
    }
}

#[test]
#[ignore = "requires newly built owned SDK initial-values DLL under native lab runner"]
fn native_initial_values_runtime_reset_cannot_reenter_initial_phase() {
    let mut worker = prepare(0.375, 1);
    worker.enter_state(73).unwrap();
    process(&mut worker, 73);
    worker
        .plugin
        .as_mut()
        .unwrap()
        .load_state(&state(0.625, 1))
        .expect("real runtime callback");
    // Mimic None fields after a runtime reconfiguration, without changing the
    // consumed one-shot lifecycle authority. These fields cannot re-admit4.
    worker.last_parameter_values.fill(None);
    worker.last_end = None;
    let before = audit(&worker);
    let cohort = worker.plugin.as_ref().unwrap().pending_restart_cohort();
    assert!(matches!(
        worker.enter_state(100),
        Err(AudioProcessorHostError::ReprepareRequired(
            mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 }
        ))
    ));
    let after = audit(&worker);
    assert_eq!(after.values, before.values);
    assert_eq!(after.metadata, before.metadata);
    assert_eq!(
        worker.plugin.as_ref().unwrap().pending_restart_cohort(),
        cohort
    );
    assert_eq!(worker.initial_signal_phase, InitialSignalPhase::Entered);
}

#[test]
#[ignore = "requires newly built owned SDK initial-values DLL under native lab runner"]
fn native_initial_values_fresh_reload_uses_new_object_without_dsp_checkpoint() {
    let mut worker = prepare(0.375, 1);
    worker.enter_state(73).unwrap();
    process(&mut worker, 73);
    let old = worker.native_audio_object_observation().unwrap().unwrap();
    let payload = worker.payload.state.clone();
    worker.enter_state(100).expect("fresh restored cohort genuinely consumed");
    let fresh = worker.native_audio_object_observation().unwrap().unwrap();
    assert_eq!(fresh.process_id(), old.process_id());
    assert!(fresh.object_serial() > old.object_serial());
    assert_eq!(worker.payload.state, payload);
    assert_eq!(worker.pending_reprepare(), None);
    assert!(worker.last_parameter_values.iter().all(Option::is_none));
    let actual = audit(&worker);
    assert_eq!(actual.process, 0);
    assert_eq!(actual.setters, 0);
    assert_eq!(actual.wrong_thread, 0);
    assert_eq!(actual.observed, 0.375);
    assert_eq!(
        process(&mut worker, 100),
        vec![0.375, 0.375, 0.375, 0.375, 0.75, 0.75, 0.75, 0.75]
    );
}

#[test]
#[ignore = "requires newly built owned SDK initial-values DLL under native lab runner"]
fn native_initial_values_retains_preexisting_author_source_without_querying() {
    let mut worker = prepare(0.375, 9);
    worker
        .plugin
        .as_ref()
        .unwrap()
        .get_parameter(0)
        .expect("real controller author callbacks");
    let source = worker.plugin.as_ref().unwrap().editor_event_source_status().unwrap();
    assert!(!source.closed_empty());
    let before = audit(&worker);
    let cohort = worker.plugin.as_ref().unwrap().pending_restart_cohort();
    assert!(matches!(
        worker.enter_state(73),
        Err(AudioProcessorHostError::ReprepareRequired(
            mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 }
        ))
    ));
    let after = audit(&worker);
    assert_eq!(after.values, before.values);
    assert_eq!(after.metadata, before.metadata);
    assert_eq!(
        worker.plugin.as_ref().unwrap().pending_restart_cohort(),
        cohort
    );
    assert_eq!(
        worker.plugin.as_ref().unwrap().editor_event_source_status().unwrap(),
        source
    );
    assert_eq!(after.process, 0);
    assert_eq!(after.setters, before.setters);
    assert_eq!(after.wrong_thread, 0);
}

#[test]
#[ignore = "requires owned SDK fixture DLL under native lab runner"]
fn native_initial_bind_rejects_wrong_object_before_metadata_or_phase_mutation() {
    let mut worker = prepare(0.375, 1);
    let actual = worker.native_audio_object_observation().unwrap().unwrap();
    let wrong = crate::NativeAudioObjectObservation::new(
        actual.process_id(),
        std::num::NonZeroU64::new(actual.object_serial().get().checked_add(1).unwrap()).unwrap(),
    );
    let source = worker.plugin.as_ref().unwrap().editor_event_source_status().unwrap();
    let cohort = worker.plugin.as_ref().unwrap().pending_restart_cohort();
    let before = audit(&worker);
    let request = crate::NativeAudioObjectBindRequest {
        binding: mondrian_core::AudioProcessorBindingId::new(),
        expected_object: wrong,
    };
    assert_eq!(
        worker.bind_native_audio_object(request).unwrap(),
        crate::NativeAudioObjectBindOutcome::Refused(
            crate::NativeAudioObjectBindRefusal::ObjectChanged
        )
    );
    let after = audit(&worker);
    assert_eq!(after.metadata, before.metadata);
    assert_eq!(after.values, before.values);
    assert_eq!(after.setters, before.setters);
    assert_eq!(after.process, before.process);
    assert_eq!(worker.initial_signal_phase, InitialSignalPhase::Unserviced);
    assert!(worker.native_binding.is_none());
    assert_eq!(worker.last_end, None);
    assert_eq!(
        worker.plugin.as_ref().unwrap().editor_event_source_status().unwrap(),
        source
    );
    assert_eq!(
        worker.plugin.as_ref().unwrap().pending_restart_cohort(),
        cohort
    );
}

#[test]
#[ignore = "requires owned SDK fixture DLL under native lab runner"]
fn native_initial_paused_bind_closes_window_and_first_enter_keeps_same_object() {
    let mut worker = prepare(0.375, 1);
    let actual = worker.native_audio_object_observation().unwrap().unwrap();
    let before = audit(&worker);
    let request = crate::NativeAudioObjectBindRequest {
        binding: mondrian_core::AudioProcessorBindingId::new(),
        expected_object: actual,
    };
    assert!(matches!(
        worker.bind_native_audio_object(request).unwrap(),
        crate::NativeAudioObjectBindOutcome::Bound(_)
    ));
    assert_eq!(
        worker.initial_signal_phase,
        InitialSignalPhase::AwaitingFirstSignal
    );
    assert_eq!(worker.last_end, None);
    assert_eq!(worker.pending_reprepare(), None);
    let bound = audit(&worker);
    assert_eq!(bound.metadata, before.metadata + 1);
    assert_eq!(bound.values, before.values + 1);
    assert_eq!(bound.process, before.process);
    assert_eq!(bound.setters, before.setters);
    worker.enter_state(73).unwrap();
    let entered = audit(&worker);
    assert_eq!(entered.metadata, bound.metadata);
    assert_eq!(entered.values, bound.values);
    assert_eq!(entered.process, bound.process);
    assert_eq!(entered.setters, bound.setters);
    assert_eq!(worker.initial_signal_phase, InitialSignalPhase::Entered);
    assert!(worker.native_binding.is_none());
    assert_eq!(
        worker.native_audio_object_observation().unwrap(),
        Some(actual)
    );
    assert_eq!(worker.last_end, Some(73));
    assert_eq!(process(&mut worker, 73)[..4], [0.375; 4]);
}

#[test]
#[ignore = "requires owned SDK fixture DLL under native lab runner"]
fn native_initial_paused_bind_cannot_reclassify_later_native_callbacks_as_startup() {
    let mut worker = prepare(0.375, 1);
    let actual = worker.native_audio_object_observation().unwrap().unwrap();
    let request = crate::NativeAudioObjectBindRequest {
        binding: mondrian_core::AudioProcessorBindingId::new(),
        expected_object: actual,
    };
    assert!(matches!(
        worker.bind_native_audio_object(request).unwrap(),
        crate::NativeAudioObjectBindOutcome::Bound(_)
    ));
    // Real SDK restore emits a new runtime restart notification. This controlled
    // callback is not claimed to be physical GUI interaction.
    worker.plugin.as_mut().unwrap().load_state(&state(0.625, 9)).unwrap();
    worker.plugin.as_ref().unwrap().get_parameter(0).unwrap();
    let source = worker.plugin.as_ref().unwrap().editor_event_source_status().unwrap();
    assert_eq!(source.pending_events(), 4);
    assert_eq!(source.pending_dsp_feedback_values(), 1);
    let cohort = worker.plugin.as_ref().unwrap().pending_restart_cohort();
    let mut events = Vec::with_capacity(crate::NATIVE_AUDIO_AUTHOR_EVENTS);
    let mut feedback = Vec::with_capacity(crate::NATIVE_AUDIO_AUTHOR_EVENTS);
    assert_eq!(
        worker
            .plugin
            .as_ref()
            .unwrap()
            .observe_editor_source_bounded(
                crate::NATIVE_AUDIO_AUTHOR_EVENTS,
                &mut events,
                &mut feedback,
            )
            .unwrap(),
        source
    );
    assert!(events.iter().any(|event| matches!(event,
        vst3_host::plugin::NativeEditorEvent::ParameterRescanRequested { flags: 4, cohort: observed }
            if Some(*observed) == cohort
    )));
    let before = audit(&worker);
    assert!(matches!(
        worker.enter_state(73),
        Err(AudioProcessorHostError::ReprepareRequired(
            mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 }
        ))
    ));
    let after = audit(&worker);
    assert_eq!(after.metadata, before.metadata);
    assert_eq!(after.values, before.values);
    assert_eq!(after.process, before.process);
    assert_eq!(after.setters, before.setters);
    assert_eq!(
        worker.initial_signal_phase,
        InitialSignalPhase::AwaitingFirstSignal
    );
    assert_eq!(
        worker.native_audio_object_observation().unwrap(),
        Some(actual)
    );
    assert_eq!(
        worker.plugin.as_ref().unwrap().pending_restart_cohort(),
        cohort
    );
    assert_eq!(
        worker.plugin.as_ref().unwrap().editor_event_source_status().unwrap(),
        source
    );
    assert_eq!(worker.last_end, None);
    assert_eq!(
        worker.native_binding.map(|receipt| receipt.binding()),
        Some(request.binding)
    );
    {
        let mut roles = worker.split_native_processing().unwrap();
        // The same check used before the real owner loop submits a DSP flush.
        assert!(roles.main.native_author_binding_matches(request).unwrap());
        std::thread::scope(|scope| {
            let processing = &mut roles.processing;
            assert_eq!(
                scope
                    .spawn(move || processing.flush_native_author_parameters())
                    .join()
                    .unwrap()
                    .unwrap(),
                crate::NativeAudioAuthorParameterFlushOutcome::Flushed
            );
        });
    }
    // An authorized paused Process is a later transaction, not initial service.
    assert_eq!(audit(&worker).process, before.process + 1);
    assert_eq!(
        worker
            .plugin
            .as_ref()
            .unwrap()
            .editor_event_source_status()
            .unwrap()
            .pending_events(),
        4
    );
    assert_eq!(
        worker.plugin.as_ref().unwrap().pending_restart_cohort(),
        cohort
    );
    assert_eq!(worker.last_end, None);
}
