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
        worker.pending_reprepare().map(crate::NativeObservedRequest::request),
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
        assert_eq!(
            worker.pending_reprepare().map(crate::NativeObservedRequest::request),
            None
        );
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
            matches!(worker.enter_state(73), Err(AudioProcessorHostError::ReprepareRequired(observed)) if observed.request() == mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags })
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
            crate::NativeObservedRequest::Cohort(crate::NativeRequestCohort {
                request: mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 },
                ..
            })
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
    assert_eq!(
        worker.pending_reprepare().map(crate::NativeObservedRequest::request),
        None
    );
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
            crate::NativeObservedRequest::Cohort(crate::NativeRequestCohort {
                request: mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 },
                ..
            })
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
    assert_eq!(
        worker.pending_reprepare().map(crate::NativeObservedRequest::request),
        None
    );
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
            crate::NativeObservedRequest::Cohort(crate::NativeRequestCohort {
                request: mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 },
                ..
            })
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

fn entered_rescan_worker() -> (Vst3Worker, crate::NativeAudioObjectBindRequest) {
    let mut worker = prepare(0.375, 13);
    worker.enter_state(73).expect("real initial-only handoff and first Enter");
    let binding = crate::NativeAudioObjectBindRequest {
        binding: mondrian_core::AudioProcessorBindingId::new(),
        expected_object: worker.native_audio_object_observation().unwrap().unwrap(),
    };
    assert!(matches!(
        worker.bind_native_audio_object(binding).unwrap(),
        crate::NativeAudioObjectBindOutcome::Bound(_)
    ));
    assert_eq!(worker.initial_signal_phase, InitialSignalPhase::Entered);
    assert!(worker.native_author_delivery.is_none());
    (worker, binding)
}

fn late_rescan_dsp(worker: &mut Vst3Worker, changes: &[(i32, f64)]) -> Vec<f32> {
    let start = worker.last_end.expect("real first Enter completed");
    let render = worker.render;
    let ids = [worker.payload.parameters[0].parameter_id().unwrap()];
    let events = changes
        .iter()
        .map(|&(offset, value)| SharedParameterEvent {
            lane: 0,
            sample_offset: u32::try_from(offset).unwrap(),
            value,
        })
        .collect::<Vec<_>>();
    let auxiliary = AudioProcessorAuxiliaryInputContract::default();
    let mut pcm = vec![1.0; 8];
    {
        let mut roles = worker.split_native_processing().unwrap();
        std::thread::scope(|scope| {
            let processing = &mut roles.processing;
            let pcm = &mut pcm;
            let auxiliary = &auxiliary;
            let ids = &ids;
            let events = &events;
            scope
                .spawn(move || {
                    processing
                        .process(IsolatedAudioProcessorWorkerBlock::new(
                            start,
                            4,
                            render,
                            pcm,
                            auxiliary,
                            &[],
                            ids,
                            events,
                        ))
                        .unwrap();
                })
                .join()
                .unwrap();
        });
        let unserviced = roles.main.preserve_native_observations().unwrap();
        assert!(unserviced.author.unwrap().source.closed_empty());
        assert!(roles.main.pending_reprepare().unwrap().is_none());
        // Deliberately do not call finish_processing_scope: that method already
        // services the mirror. Join/return these original borrows, then test the
        // full-owner observer's own explicit service, without a dispatcher claim.
    }
    assert_eq!(worker.last_end, Some(start + 4));
    let editor = worker.plugin.as_mut().unwrap().borrow_editor_control().unwrap();
    assert_eq!(editor.parameter(0).unwrap(), 0.375);
    assert!(editor.editor_event_source_status().unwrap().closed_empty());
    assert!(!editor.pending_restart_cohort().unwrap().unwrap().pending());
    for frame in pcm.chunks_exact(2) {
        assert_eq!(frame[0].to_bits(), frame[1].to_bits());
    }
    pcm.chunks_exact(2).map(|frame| frame[0]).collect()
}

fn author_flush_eligible(
    worker: &mut Vst3Worker,
    binding: crate::NativeAudioObjectBindRequest,
) -> bool {
    let roles = worker.split_native_processing().unwrap();
    // Dropping these borrowed roles does not service controller work or issue
    // a DSP job. This checks the actual native-main object's command gate.
    roles.main.native_author_binding_matches(binding).unwrap()
}

fn rescan_source_snapshot(
    worker: &Vst3Worker,
) -> (
    vst3_host::plugin::NativeEditorEventSourceStatus,
    Vec<vst3_host::plugin::NativeEditorEvent>,
    Option<vst3_host::plugin::RestartCohort>,
) {
    let plugin = worker.plugin.as_ref().unwrap();
    let mut events = Vec::with_capacity(crate::NATIVE_AUDIO_AUTHOR_EVENTS);
    let mut feedback = Vec::new();
    let source = plugin
        .observe_editor_source_bounded(
            crate::NATIVE_AUDIO_AUTHOR_EVENTS,
            &mut events,
            &mut feedback,
        )
        .unwrap();
    assert!(feedback.is_empty());
    (source, events, plugin.pending_restart_cohort())
}

#[test]
#[ignore = "requires rebuilt owned mode13 SDK fixture under native Job runner"]
fn native_late_rescan_collects_and_settles_one_current_proposal_exactly_once() {
    use crate::{
        NativeAudioAuthorDeliveryState as Delivery, NativeAudioAuthorEvent as Event,
        NativeAudioObservationConsumption as Consumption,
    };
    let (mut worker, binding) = entered_rescan_worker();
    let object = worker.native_audio_object_observation().unwrap();
    let seed = worker.payload.state.clone();
    let initial_cursor = worker.last_end;
    let before = audit(&worker); // Must precede queue: save_state would service the mirror.
    assert_eq!(
        late_rescan_dsp(&mut worker, &[(2, 0.75)]),
        [0.375, 0.375, 0.75, 0.75]
    );
    let cursor = worker.last_end;
    assert_eq!(cursor, initial_cursor.map(|start| start + 4));
    let actual = worker.poll_native_observations(Consumption::DrainAuthor(binding)).unwrap();
    let author = actual.author.unwrap();
    let delivery = match author.delivery {
        Delivery::Delivered(delivery) => delivery,
        other => panic!("actual singleton rescan must deliver: {other:?}"),
    };
    assert_eq!(delivery.binding(), binding);
    assert_eq!(delivery.events().len(), 1);
    let Event::ParameterValuesRescanned { kind, values } = &delivery.events()[0] else {
        panic!("one typed current-value scan");
    };
    assert_eq!(*kind, mondrian_core::NativeAudioParameterRescanKind::Values);
    assert_eq!(values.len(), 1);
    assert_eq!(
        values[0].parameter(),
        &worker.payload.parameters[0].parameter_id().unwrap()
    );
    assert_eq!(
        values[0].value().unwrap().value().to_bits(),
        0.75_f64.to_bits()
    );
    let configuration = delivery.configuration().unwrap();
    assert_eq!(configuration.len(), 28 + 24 + 44);
    assert_eq!(&configuration[28..32], b"MIV1");
    assert_eq!(&configuration[36..44], &0.75_f64.to_le_bytes());
    let retained = match worker.native_author_delivery.as_ref().unwrap() {
        super::native_author::PendingDelivery::Delivered {
            rescan_witness: Some(witness),
            configuration: Some(configuration),
            request_cohort: Some(cohort),
            ..
        } => {
            assert_eq!(witness.source().revision(), delivery.revision());
            assert_eq!(witness.source().pending_events(), 1);
            assert_eq!(cohort.serial, witness.cohort().serial());
            assert_eq!(
                cohort.request,
                mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 }
            );
            assert_eq!(Some(*cohort), delivery.request_cohort());
            assert_eq!(configuration.bytes(), delivery.configuration().unwrap());
            (*witness, *cohort)
        }
        _ => panic!("same original pending owner must retain its actual witness"),
    };
    let after = audit(&worker); // Explicit collector service has completed.
    assert_eq!(after.process, before.process + 1);
    assert_eq!(after.setters, before.setters + 1);
    assert_eq!(after.metadata, before.metadata + 1);
    // One deliberate unserviced role getter, then the actual bounded scan.
    assert_eq!(after.values, before.values + 2);
    assert_eq!(after.observed.to_bits(), 0.75_f64.to_bits());
    assert_eq!(after.wrong_thread, 0);
    let source = rescan_source_snapshot(&worker);
    assert!(source.0.closed_empty() && source.1.is_empty());
    assert_eq!(source.2, Some(retained.0.cohort()));
    assert_eq!(
        actual.pending_reprepare,
        Some(crate::NativeObservedRequest::Cohort(retained.1))
    );
    assert!(!author_flush_eligible(&mut worker, binding));
    for _ in 0..2 {
        let repeated = worker.poll_native_observations(Consumption::DrainAuthor(binding)).unwrap();
        assert!(matches!(repeated.author.unwrap().delivery,
            Delivery::AwaitingAcknowledgement { binding: same, revision, request_cohort }
                if same == binding && revision == delivery.revision()
                    && request_cohort == delivery.request_cohort()));
    }
    assert_eq!(worker.payload.state, seed);
    let installed = crate::NativeAudioObjectBindRequest {
        binding: mondrian_core::AudioProcessorBindingId::new(),
        expected_object: binding.expected_object,
    };
    assert!(matches!(
        worker.bind_native_audio_object(installed).unwrap(),
        crate::NativeAudioObjectBindOutcome::Bound(_)
    ));
    let ack = crate::NativeAudioAuthorAcknowledgement {
        source_binding: binding,
        installed_binding: installed,
        revision: delivery.revision(),
    };
    // A malformed retained association must be rejected before SDK retirement.
    if let Some(super::native_author::PendingDelivery::Delivered { request_cohort, .. }) =
        worker.native_author_delivery.as_mut()
    {
        request_cohort.as_mut().unwrap().serial += 1;
    }
    let refused = worker.poll_native_observations(Consumption::AcknowledgeAuthor(ack)).unwrap();
    assert!(matches!(
        refused.author.unwrap().delivery,
        Delivery::Refused(crate::NativeAudioAuthorRefusal::StaleDelivery)
    ));
    assert_eq!(rescan_source_snapshot(&worker), source);
    assert_eq!(worker.payload.state, seed);
    if let Some(super::native_author::PendingDelivery::Delivered { request_cohort, .. }) =
        worker.native_author_delivery.as_mut()
    {
        *request_cohort = Some(retained.1);
    }
    let accepted = worker.poll_native_observations(Consumption::AcknowledgeAuthor(ack)).unwrap();
    let accepted_author = accepted.author.unwrap();
    assert_eq!(accepted_author.source, author.source);
    assert_eq!(
        accepted_author.delivery,
        Delivery::Acknowledged {
            acknowledgement: ack,
            request_cohort: Some(retained.1),
        }
    );
    assert!(accepted.pending_reprepare.is_none());
    assert!(worker.native_author_delivery.is_none());
    assert!(author_flush_eligible(&mut worker, installed));
    assert!(!author_flush_eligible(&mut worker, binding));
    assert_eq!(worker.payload.state.as_deref(), delivery.configuration());
    let settled = rescan_source_snapshot(&worker);
    assert_eq!((settled.0, settled.1), (source.0, source.1.clone()));
    assert!(!settled.2.unwrap().pending());
    assert_eq!(settled.2.unwrap().serial(), retained.0.cohort().serial());
    let repeated = worker.poll_native_observations(Consumption::AcknowledgeAuthor(ack)).unwrap();
    assert!(matches!(
        repeated.author.unwrap().delivery,
        Delivery::Refused(crate::NativeAudioAuthorRefusal::StaleDelivery)
    ));
    assert_eq!(rescan_source_snapshot(&worker).0, source.0);
    assert_eq!(worker.payload.state.as_deref(), delivery.configuration());
    let unchanged = audit(&worker);
    assert_eq!(
        (
            unchanged.metadata,
            unchanged.values,
            unchanged.setters,
            unchanged.process
        ),
        (after.metadata, after.values, after.setters, after.process)
    );
    assert_eq!(worker.native_audio_object_observation().unwrap(), object);
    assert_eq!(worker.native_binding.unwrap().binding(), installed.binding);
    assert_eq!(worker.last_end, cursor);
}

#[test]
#[ignore = "requires rebuilt owned mode13/mode9 SDK fixture under native Job runner"]
fn native_late_rescan_ack_preserves_new_suffix_and_refuses_external_retirement() {
    use crate::{
        NativeAudioAuthorDeliveryState as Delivery, NativeAudioAuthorRefusal as Refusal,
        NativeAudioObservationConsumption as Consumption,
    };
    use vst3_host::plugin::{NativeEditorEvent as Raw, RestartCohort};

    type Snapshot = (
        vst3_host::plugin::NativeEditorEventSourceStatus,
        Vec<Raw>,
        Vec<(u32, f64)>,
        Option<RestartCohort>,
    );

    fn snapshot(worker: &Vst3Worker) -> Snapshot {
        let plugin = worker.plugin.as_ref().unwrap();
        let mut events = Vec::with_capacity(crate::NATIVE_AUDIO_AUTHOR_EVENTS);
        let mut feedback = Vec::with_capacity(crate::NATIVE_AUDIO_AUTHOR_EVENTS);
        let source = plugin
            .observe_editor_source_bounded(
                crate::NATIVE_AUDIO_AUTHOR_EVENTS,
                &mut events,
                &mut feedback,
            )
            .unwrap();
        (source, events, feedback, plugin.pending_restart_cohort())
    }

    for case in ["same_bit", "feedback", "external_retire"] {
        let (mut worker, binding) = entered_rescan_worker();
        let _ = late_rescan_dsp(&mut worker, &[(2, 0.75)]);
        let collected = worker.poll_native_observations(Consumption::DrainAuthor(binding)).unwrap();
        let Delivery::Delivered(delivery) = collected.author.unwrap().delivery else {
            panic!("real A must have a retained proposal");
        };
        let (witness, prefix) = match worker.native_author_delivery.as_ref().unwrap() {
            super::native_author::PendingDelivery::Delivered {
                rescan_witness: Some(witness),
                request_cohort: Some(cohort),
                ..
            } => (*witness, *cohort),
            _ => panic!("real A owns its final witness and prefix cohort"),
        };
        let seed = worker.payload.state.clone();
        let object = worker.native_audio_object_observation().unwrap();
        let cursor = worker.last_end;
        if case == "external_retire" {
            assert!(worker
                .plugin
                .as_mut()
                .unwrap()
                .retire_restart_cohort(witness.cohort())
                .unwrap());
        }
        if case == "feedback" {
            // Explicit native restore/getter is test setup only. The actual
            // callback appends later exact-4 markers and real DSP feedback;
            // acknowledgement itself must perform no restore or getter.
            worker.plugin.as_mut().unwrap().load_state(&state(0.625, 9)).unwrap();
            let editor = worker.plugin.as_mut().unwrap().borrow_editor_control().unwrap();
            assert_eq!(editor.parameter(0).unwrap(), 0.625);
        } else {
            // A real armed controller setter creates the later same-bit B.
            // Its queued processor value remains pending across acknowledgement.
            worker.plugin.as_mut().unwrap().set_parameter_at(0, 0.625, 1).unwrap();
        }
        let suffix = snapshot(&worker);
        let live = suffix.3.unwrap();
        assert_eq!(live.flags().bits(), 4);
        assert!(live.serial() > witness.cohort().serial());
        assert!(!live.exhausted());
        assert!(suffix.1.iter().any(|event| matches!(event,
            Raw::ParameterRescanRequested { flags: 4, cohort } if *cohort == live)));
        if case == "feedback" {
            assert_eq!(suffix.2, [(0, 0.875)]);
            assert_eq!(suffix.0.pending_dsp_feedback_values(), 1);
        } else {
            assert!(suffix.2.is_empty());
        }
        let pending_input =
            worker.plugin.as_ref().unwrap().pending_processor_parameter_input().unwrap();
        assert_eq!(pending_input, case != "feedback");
        let pending = worker.pending_reprepare();
        assert!(
            matches!(pending, Some(crate::NativeObservedRequest::Cohort(cohort))
            if cohort.serial == live.serial()
                && cohort.request == mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 })
        );
        assert!(!author_flush_eligible(&mut worker, binding));
        let held = worker.poll_native_observations(Consumption::DrainAuthor(binding)).unwrap();
        assert!(matches!(held.author.unwrap().delivery,
            Delivery::AwaitingAcknowledgement { binding: same, revision, request_cohort }
                if same == binding && revision == delivery.revision()
                    && request_cohort == Some(prefix)));
        assert_eq!(snapshot(&worker), suffix);
        assert_eq!(
            worker.plugin.as_ref().unwrap().pending_processor_parameter_input().unwrap(),
            pending_input
        );
        assert_eq!(worker.pending_reprepare(), pending);
        assert_eq!(worker.payload.state, seed);
        assert_eq!(worker.last_end, cursor);
        assert_eq!(worker.native_audio_object_observation().unwrap(), object);
        // save_state refuses an outstanding processor point before native code.
        // Do not consume that point merely to obtain a counter observation.
        // These two pending-input cases have no dynamic DSP-counter claim;
        // the no-suffix A control above has the complete before/after audit.
        let before = if pending_input {
            assert!(matches!(
                worker.plugin.as_ref().unwrap().save_state(),
                Err(vst3_host::Error::PendingParameterDelivery)
            ));
            None
        } else {
            Some(audit(&worker))
        };
        assert_eq!(snapshot(&worker), suffix);
        let installed = crate::NativeAudioObjectBindRequest {
            binding: mondrian_core::AudioProcessorBindingId::new(),
            expected_object: binding.expected_object,
        };
        assert!(matches!(
            worker.bind_native_audio_object(installed).unwrap(),
            crate::NativeAudioObjectBindOutcome::Bound(_)
        ));
        let ack = crate::NativeAudioAuthorAcknowledgement {
            source_binding: binding,
            installed_binding: installed,
            revision: delivery.revision(),
        };
        for attempt in 0..2 {
            let observed =
                worker.poll_native_observations(Consumption::AcknowledgeAuthor(ack)).unwrap();
            let author = observed.author.unwrap();
            assert_eq!(
                author.source,
                super::native_author::status(suffix.0).unwrap()
            );
            if case == "external_retire" {
                assert_eq!(author.delivery, Delivery::Refused(Refusal::StaleDelivery));
                assert_eq!(worker.payload.state, seed);
                assert!(matches!(worker.native_author_delivery.as_ref(),
                    Some(super::native_author::PendingDelivery::Delivered {
                        binding: held_binding, revision, rescan_witness: Some(actual),
                        configuration: Some(configuration), request_cohort,
                    }) if *held_binding == binding && *revision == delivery.revision()
                        && *actual == witness && *request_cohort == Some(prefix)
                        && Some(configuration.bytes()) == delivery.configuration()));
            } else if worker.native_author_delivery.is_none() {
                // The first call accepts A; repeating that exact receipt cannot
                // retire B or publish another acknowledgement.
                let expected = if attempt == 0 {
                    Delivery::Acknowledged { acknowledgement: ack, request_cohort: Some(prefix) }
                } else {
                    Delivery::Refused(Refusal::StaleDelivery)
                };
                assert_eq!(author.delivery, expected);
                assert_eq!(worker.payload.state.as_deref(), delivery.configuration());
            } else {
                panic!("a successful SDK settlement must retire the held A receipt");
            }
            assert_eq!(observed.pending_reprepare, pending);
            assert_eq!(snapshot(&worker), suffix);
            assert_eq!(
                worker.plugin.as_ref().unwrap().pending_processor_parameter_input().unwrap(),
                pending_input
            );
            assert_eq!(worker.last_end, cursor);
            assert_eq!(worker.native_audio_object_observation().unwrap(), object);
            assert_eq!(worker.native_binding.unwrap().binding(), installed.binding);
        }
        if let Some(before) = before {
            let after = audit(&worker);
            assert_eq!(
                (
                    after.metadata,
                    after.values,
                    after.setters,
                    after.process,
                    after.wrong_thread
                ),
                (
                    before.metadata,
                    before.values,
                    before.setters,
                    before.process,
                    0
                )
            );
            assert_eq!(after.observed.to_bits(), before.observed.to_bits());
        } else {
            assert!(matches!(
                worker.plugin.as_ref().unwrap().save_state(),
                Err(vst3_host::Error::PendingParameterDelivery)
            ));
        }
        assert_eq!(snapshot(&worker), suffix);
    }
}

#[test]
#[ignore = "requires rebuilt owned mode13 SDK fixture under native Job runner"]
fn native_late_rescan_refuses_multiple_retired_and_metadata_drift_without_draining() {
    use crate::{
        NativeAudioAuthorDeliveryState as Delivery, NativeAudioAuthorRefusal as Refusal,
        NativeAudioObservationConsumption as Consumption,
    };
    for case in ["multiple", "retired", "id", "title", "flags"] {
        let (mut worker, binding) = entered_rescan_worker();
        let changes: &[(i32, f64)] = if case == "multiple" {
            &[(0, 0.625), (2, 0.75)]
        } else {
            &[(2, 0.75)]
        };
        let _ = late_rescan_dsp(&mut worker, changes);
        worker
            .plugin
            .as_mut()
            .unwrap()
            .borrow_editor_control()
            .unwrap()
            .service_controller()
            .unwrap();
        if case == "retired" {
            assert_eq!(
                worker.plugin.as_mut().unwrap().take_restart_flags().bits(),
                4
            );
        }
        match case {
            "id" => worker.payload.parameters[0].id = 99,
            "title" => worker.payload.parameters[0].name.push_str(" drift"),
            "flags" => worker.payload.parameters[0].flags ^= 1,
            _ => {}
        }
        let before = audit(&worker); // Mirror already served; no observation-induced callback.
        let snapshot = rescan_source_snapshot(&worker);
        assert_eq!(snapshot.1.len(), if case == "multiple" { 2 } else { 1 });
        let seed = worker.payload.state.clone();
        let cursor = worker.last_end;
        let object = worker.native_audio_object_observation().unwrap();
        let observed = worker.poll_native_observations(Consumption::DrainAuthor(binding)).unwrap();
        let reason = if matches!(case, "multiple" | "retired") {
            Refusal::UnsupportedAdapter
        } else {
            Refusal::SourceChanged
        };
        assert!(
            matches!(observed.author.unwrap().delivery, Delivery::Refused(actual)
            if actual == reason),
            "case={case}"
        );
        assert_eq!(rescan_source_snapshot(&worker), snapshot, "case={case}");
        assert!(worker.native_author_delivery.is_none());
        assert_eq!(worker.payload.state, seed);
        assert_eq!(worker.last_end, cursor);
        assert_eq!(worker.native_audio_object_observation().unwrap(), object);
        assert_eq!(worker.native_binding.unwrap().binding(), binding.binding);
        let after = audit(&worker);
        let queries = u32::from(!matches!(case, "multiple" | "retired"));
        assert_eq!(after.metadata, before.metadata + queries);
        assert_eq!(after.values, before.values + queries);
        assert_eq!(
            (after.setters, after.process, after.wrong_thread),
            (before.setters, before.process, 0)
        );
    }
}

#[test]
#[ignore = "requires owned mode13/mode9 SDK fixture under native Job runner"]
fn native_late_rescan_existing_feedback_and_failure_refuse_before_deferred_service() {
    use crate::{
        NativeAudioAuthorDeliveryState as Delivery, NativeAudioAuthorRefusal as Refusal,
        NativeAudioObservationConsumption as Consumption,
    };

    fn deferred_still_pending(worker: &Vst3Worker) {
        let mut frame = vst3_host::controller_parameters::ControllerParameterQueryFrame::default();
        let mut callbacks = 0;
        let result = worker.plugin.as_ref().unwrap().visit_controller_parameters_bounded(
            1,
            &mut frame,
            &mut |_, _| {
                callbacks += 1;
                Ok(())
            },
        );
        assert!(matches!(result, Err(vst3_host::Error::Other(detail))
            if detail == "deferred controller synchronization is pending"));
        assert_eq!(callbacks, 0);
    }

    for overflow in [false, true] {
        let (mut worker, binding) = entered_rescan_worker();
        let before = audit(&worker); // Only before the original enqueue.
        let _ = late_rescan_dsp(&mut worker, &[(2, 0.75)]);
        deferred_still_pending(&worker);
        // Test setup deliberately uses real Stop/Deactivate/restore/setup. It
        // retains this SDK owner's deferred queue, proved again after setup.
        // The collector itself performs none of these lifecycle operations.
        worker.plugin.as_mut().unwrap().load_state(&state(0.375, 9)).unwrap();
        {
            let editor = worker.plugin.as_mut().unwrap().borrow_editor_control().unwrap();
            let limit = if overflow { 1366 } else { 1 };
            for _ in 0..limit {
                assert_eq!(editor.parameter(0).unwrap(), 0.375);
                if editor.editor_event_source_status().unwrap().failure().is_some() {
                    break;
                }
            }
        }
        if !overflow {
            worker.plugin.as_mut().unwrap().load_state(&state(0.375, 13)).unwrap();
        }
        // The failure stimulus stays in getter mode9; it does not claim a
        // mode13-specific failure or a successful post-failure state restore.
        deferred_still_pending(&worker);
        let plugin = worker.plugin.as_ref().unwrap();
        assert!(!plugin.pending_processor_parameter_input().unwrap());
        let source = plugin.editor_event_source_status().unwrap();
        let cohort = plugin.pending_restart_cohort();
        assert!(source.pending_dsp_feedback_values() > 0);
        assert_eq!(
            source.failure(),
            overflow.then_some(vst3_host::plugin::NativeEditorEventFailure::Overflow)
        );
        let mut events = Vec::with_capacity(crate::NATIVE_AUDIO_AUTHOR_EVENTS);
        let mut feedback = Vec::with_capacity(crate::NATIVE_AUDIO_AUTHOR_EVENTS);
        let observed = plugin.observe_editor_source_bounded(
            crate::NATIVE_AUDIO_AUTHOR_EVENTS,
            &mut events,
            &mut feedback,
        );
        if overflow {
            assert!(observed.is_err()); // Incomplete history cannot be peeked as complete.
            assert!(events.is_empty() && feedback.is_empty());
        } else {
            assert_eq!(observed.unwrap(), source);
        }
        let cursor = worker.last_end;
        let object = worker.native_audio_object_observation().unwrap();
        let expected = if overflow {
            Refusal::InvalidSource
        } else {
            Refusal::DspFeedbackPending
        };
        let refused = worker.poll_native_observations(Consumption::DrainAuthor(binding)).unwrap();
        assert!(
            matches!(refused.author.unwrap().delivery, Delivery::Refused(reason)
            if reason == expected)
        );
        deferred_still_pending(&worker);
        let plugin = worker.plugin.as_ref().unwrap();
        assert_eq!(plugin.editor_event_source_status().unwrap(), source);
        assert_eq!(plugin.pending_restart_cohort(), cohort);
        let mut after_events = Vec::with_capacity(crate::NATIVE_AUDIO_AUTHOR_EVENTS);
        let mut after_feedback = Vec::with_capacity(crate::NATIVE_AUDIO_AUTHOR_EVENTS);
        let after = plugin.observe_editor_source_bounded(
            crate::NATIVE_AUDIO_AUTHOR_EVENTS,
            &mut after_events,
            &mut after_feedback,
        );
        if overflow {
            assert!(after.is_err());
            assert!(after_events.is_empty() && after_feedback.is_empty());
        } else {
            assert_eq!(after.unwrap(), source);
            assert_eq!(after_events, events);
            assert_eq!(after_feedback, feedback);
        }
        assert!(worker.native_author_delivery.is_none());
        assert_eq!(worker.last_end, cursor);
        assert_eq!(worker.native_audio_object_observation().unwrap(), object);
        assert_eq!(worker.native_binding.unwrap().binding(), binding.binding);
        // This separate action is intentionally after every refusal assertion.
        // Only it consumes the real deferred setter; save_state is safe now.
        worker
            .plugin
            .as_mut()
            .unwrap()
            .borrow_editor_control()
            .unwrap()
            .service_controller()
            .unwrap();
        let after = audit(&worker);
        assert_eq!(after.setters, before.setters + 1);
        assert_eq!(after.process, before.process + 1);
        assert_eq!(after.wrong_thread, 0);
        assert_eq!(after.observed.to_bits(), 0.375_f64.to_bits());
    }
}

#[test]
#[ignore = "requires rebuilt owned mode13 SDK fixture under native Job runner"]
fn native_pending_zero_offset_flush_uses_same_worker_and_preserves_source() {
    fn flush(worker: &mut Vst3Worker) -> crate::NativeAudioAuthorParameterFlushOutcome {
        let mut roles = worker.split_native_processing().unwrap();
        std::thread::scope(|scope| {
            let processing = &mut roles.processing;
            scope
                .spawn(move || processing.flush_native_author_parameters())
                .join()
                .unwrap()
                .unwrap()
        })
        // No finish_processing_scope: do not service or reclassify callbacks.
    }
    let (mut worker, binding) = entered_rescan_worker();
    let object = worker.native_audio_object_observation().unwrap();
    let seed = worker.payload.state.clone();
    let cursor = worker.last_end;
    let before = audit(&worker); // Before the real host parameter enqueue only.
    worker.plugin.as_mut().unwrap().set_parameter_at(0, 0.875, 0).unwrap();
    let source = rescan_source_snapshot(&worker);
    assert_eq!(source.1.len(), 1);
    assert_eq!(source.2.unwrap().flags().bits(), 4);
    assert!(matches!(
        worker.plugin.as_ref().unwrap().save_state(),
        Err(vst3_host::Error::PendingParameterDelivery)
    ));
    let buffer_extent = worker.buffers.block_size;
    assert_eq!(
        flush(&mut worker),
        crate::NativeAudioAuthorParameterFlushOutcome::Flushed
    );
    assert_eq!(worker.buffers.block_size, buffer_extent);
    assert_eq!(worker.last_end, cursor);
    assert_eq!(rescan_source_snapshot(&worker), source);
    let after = audit(&worker);
    assert_eq!(after.process, before.process + 1);
    assert_eq!(after.setters, before.setters + 1);
    assert_eq!(
        (after.metadata, after.values, after.wrong_thread),
        (before.metadata, before.values, 0)
    );
    assert_eq!(worker.payload.state, seed);
    assert_eq!(worker.native_audio_object_observation().unwrap(), object);
    assert_eq!(worker.native_binding.unwrap().binding(), binding.binding);
    assert!(!worker.plugin.as_ref().unwrap().pending_processor_parameter_input().unwrap());
    // This local retained DSP control does not grant parent Runtime permission:
    // the raw Source/cohort remain held. No parameters are replayed in this call.
    let render = worker.render;
    let ids = [worker.payload.parameters[0].parameter_id().unwrap()];
    let auxiliary = AudioProcessorAuxiliaryInputContract::default();
    let mut pcm = vec![1.0; 8];
    {
        let mut roles = worker.split_native_processing().unwrap();
        std::thread::scope(|scope| {
            let processing = &mut roles.processing;
            let pcm = &mut pcm;
            let auxiliary = &auxiliary;
            let ids = &ids;
            scope
                .spawn(move || {
                    processing.process(IsolatedAudioProcessorWorkerBlock::new(
                        cursor.unwrap(),
                        4,
                        render,
                        pcm,
                        auxiliary,
                        &[],
                        ids,
                        &[],
                    ))
                })
                .join()
                .unwrap()
                .unwrap();
        });
    }
    assert_eq!(pcm, [0.875; 8]);
    assert_eq!(worker.last_end, cursor.map(|s| s + 4));
    assert_eq!(rescan_source_snapshot(&worker), source);
    let later = audit(&worker);
    assert_eq!(later.process, after.process + 1);
    assert_eq!(
        (
            later.setters,
            later.metadata,
            later.values,
            later.wrong_thread
        ),
        (after.setters, after.metadata, after.values, 0)
    );
    // An empty host Vec still runs the old Source replay path's native flush;
    // NoPending must not be mistaken for completion of the whole consumer.
    let later_cursor = worker.last_end;
    assert_eq!(
        flush(&mut worker),
        crate::NativeAudioAuthorParameterFlushOutcome::Flushed
    );
    assert_eq!(audit(&worker).process, later.process + 1);
    assert_eq!(worker.last_end, later_cursor);
    assert_eq!(rescan_source_snapshot(&worker), source);

    // Normal GUI Source/feedback with an empty host Vec must still take the
    // old same-Source replay route, rather than being rejected by the new API.
    {
        let (mut replay, _) = entered_rescan_worker();
        replay.plugin.as_mut().unwrap().load_state(&state(0.625, 9)).unwrap();
        let editor = replay.plugin.as_mut().unwrap().borrow_editor_control().unwrap();
        assert_eq!(editor.parameter(0).unwrap(), 0.625);
        drop(editor);
        let plugin = replay.plugin.as_ref().unwrap();
        assert!(!plugin.pending_processor_parameter_input().unwrap());
        let mut events = Vec::with_capacity(crate::NATIVE_AUDIO_AUTHOR_EVENTS);
        let mut feedback = Vec::with_capacity(crate::NATIVE_AUDIO_AUTHOR_EVENTS);
        let source = plugin
            .observe_editor_source_bounded(
                crate::NATIVE_AUDIO_AUTHOR_EVENTS,
                &mut events,
                &mut feedback,
            )
            .unwrap();
        assert_eq!(feedback, [(0, 0.875)]);
        let raw = plugin.pending_restart_cohort();
        let before = audit(&replay);
        let cursor = replay.last_end;
        let object = replay.native_audio_object_observation().unwrap();
        assert_eq!(
            flush(&mut replay),
            crate::NativeAudioAuthorParameterFlushOutcome::Flushed
        );
        let after = audit(&replay);
        assert_eq!(after.process, before.process + 1);
        assert_eq!(
            (after.setters, after.metadata, after.values),
            (before.setters, before.metadata, before.values)
        );
        let retained = rescan_source_snapshot(&replay);
        assert_eq!(retained.0.revision(), source.revision());
        assert_eq!(retained.1, events);
        assert_eq!(retained.2, raw);
        assert_eq!(retained.0.pending_dsp_feedback_values(), 0);
        assert_eq!(replay.last_end, cursor);
        assert_eq!(replay.native_audio_object_observation().unwrap(), object);
    }

    for offset in [-1, 1] {
        let (mut worker, binding) = entered_rescan_worker();
        let cursor = worker.last_end;
        let object = worker.native_audio_object_observation().unwrap();
        worker.plugin.as_mut().unwrap().set_parameter_at(0, 0.875, offset).unwrap();
        let source = rescan_source_snapshot(&worker);
        assert_eq!(
            flush(&mut worker),
            crate::NativeAudioAuthorParameterFlushOutcome::Refused(
                crate::NativeAudioAuthorRefusal::InvalidValue
            )
        );
        assert!(matches!(
            worker.plugin.as_ref().unwrap().save_state(),
            Err(vst3_host::Error::PendingParameterDelivery)
        ));
        assert_eq!(rescan_source_snapshot(&worker), source);
        assert_eq!(worker.last_end, cursor);
        assert_eq!(worker.native_audio_object_observation().unwrap(), object);
        assert_eq!(worker.native_binding.unwrap().binding(), binding.binding);
        // No counter audit is available while this original point is pending;
        // the SDK real-COM controls independently assert native call count zero.
    }
}

#[test]
#[ignore = "requires rebuilt owned mode14 SDK fixture under native Job runner"]
fn native_initial_com_origin_survives_entry_and_joined_worker_poll() {
    use crate::{
        NativeAudioAuthorDeliveryState as Delivery,
        NativeAudioObservationConsumption as Consumption,
    };
    // This is a real SDK DLL + joined Worker control, not Product IPC evidence.
    // Both sides of first Enter must use actual COM provenance, not phase rights.
    for enter_before_flush in [false, true] {
        let mut worker = prepare(0.375, 14);
        let object = worker.native_audio_object_observation().unwrap().unwrap();
        let before = audit(&worker); // Only before initial COM publication.
        assert_eq!(before.process, 0);
        let initial_bytes = worker.plugin.as_ref().unwrap().save_state().unwrap();
        assert_eq!(
            f64::from_le_bytes(initial_bytes[36..44].try_into().unwrap()),
            0.375
        );
        let seed = worker.payload.state.clone();
        let mut binding = crate::NativeAudioObjectBindRequest {
            binding: mondrian_core::AudioProcessorBindingId::new(),
            expected_object: object,
        };
        assert!(matches!(
            worker.bind_native_audio_object(binding).unwrap(),
            crate::NativeAudioObjectBindOutcome::Bound(_)
        ));
        if enter_before_flush {
            worker.enter_state(73).unwrap();
            binding.binding = mondrian_core::AudioProcessorBindingId::new();
            assert!(matches!(
                worker.bind_native_audio_object(binding).unwrap(),
                crate::NativeAudioObjectBindOutcome::Bound(_)
            ));
        }
        let cursor = worker.last_end;
        let phase = worker.initial_signal_phase;
        let source = rescan_source_snapshot(&worker);
        assert!(source.0.closed_empty());
        assert_eq!(source.0.pending_dsp_feedback_values(), 0);
        assert!(worker.plugin.as_ref().unwrap().pending_processor_parameter_input().unwrap());
        assert!(matches!(
            worker.plugin.as_ref().unwrap().save_state(),
            Err(vst3_host::Error::PendingParameterDelivery)
        ));
        // Reproduce the actual command-service ordering: scoped DSP operation,
        // explicit join, then the original full owner's Source observation.
        let poll = |worker: &mut Vst3Worker| {
            {
                let mut roles = worker.split_native_processing().unwrap();
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
            let observed =
                worker.poll_native_observations(Consumption::DrainAuthor(binding)).unwrap();
            assert_eq!(observed.object, Some(object));
            let author = observed.author.unwrap();
            assert_eq!(
                (
                    author.source.pending_events(),
                    author.source.pending_dsp_feedback_values()
                ),
                (0, 0)
            );
            assert!(matches!(author.delivery, Delivery::Preserved));
        };
        poll(&mut worker);
        let after = audit(&worker);
        assert_eq!(
            (after.process, after.setters, after.wrong_thread),
            (before.process + 1, before.setters, 0)
        );
        assert_eq!(worker.last_end, cursor);
        assert_eq!(worker.initial_signal_phase, phase);
        assert_eq!(rescan_source_snapshot(&worker), source);
        assert_eq!(worker.payload.state, seed);
        assert_eq!(
            worker.native_audio_object_observation().unwrap(),
            Some(object)
        );
        assert_eq!(worker.native_binding.unwrap().binding(), binding.binding);
        assert!(!worker.plugin.as_ref().unwrap().pending_processor_parameter_input().unwrap());
        // NoPending is not a new skip permission: the complete second Worker
        // Poll still performs old Source replay's real zero-sample native call.
        poll(&mut worker);
        assert_eq!(audit(&worker).process, after.process + 1);
        assert_eq!(worker.last_end, cursor);
        assert_eq!(rescan_source_snapshot(&worker), source);
        if !enter_before_flush {
            worker.enter_state(73).unwrap();
        }
        let render = worker.render;
        let ids = [worker.payload.parameters[0].parameter_id().unwrap()];
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        let mut pcm = vec![1.0; 8];
        worker
            .process(IsolatedAudioProcessorWorkerBlock::new(
                73,
                4,
                render,
                &mut pcm,
                &auxiliary,
                &[],
                &ids,
                &[],
            ))
            .unwrap();
        assert_eq!(pcm, vec![0.75; 8]);
        assert_eq!(worker.last_end, Some(77));
        assert_eq!(audit(&worker).process, after.process + 2);
        assert_eq!(
            worker.native_audio_object_observation().unwrap(),
            Some(object)
        );
        assert_eq!(rescan_source_snapshot(&worker), source);
    }
}

#[test]
#[ignore = "requires rebuilt owned mode14 SDK fixture under native Job runner"]
fn native_rejected_author_delivery_blocks_an_extra_drain_flush() {
    use crate::{
        NativeAudioAuthorDeliveryState as Delivery, NativeAudioAuthorRefusal as Refusal,
        NativeAudioObservationConsumption as Consumption,
    };
    let mut worker = prepare(0.375, 0);
    worker.enter_state(73).unwrap();
    let object = worker.native_audio_object_observation().unwrap().unwrap();
    let binding = crate::NativeAudioObjectBindRequest {
        binding: mondrian_core::AudioProcessorBindingId::new(),
        expected_object: object,
    };
    assert!(matches!(
        worker.bind_native_audio_object(binding).unwrap(),
        crate::NativeAudioObjectBindOutcome::Bound(_)
    ));
    assert!(author_flush_eligible(&mut worker, binding));
    // Controlled real lifecycle setup: mode14 emits an actual Value-only .75
    // callback. The collector itself neither restores nor invents callbacks.
    worker.plugin.as_mut().unwrap().load_state(&state(0.375, 14)).unwrap();
    assert_eq!(
        worker
            .plugin
            .as_ref()
            .unwrap()
            .editor_event_source_status()
            .unwrap()
            .pending_dsp_feedback_values(),
        1
    );
    {
        let mut roles = worker.split_native_processing().unwrap();
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
    // An explicit test metadata adversary makes the real normal-UI projector
    // retain a Rejected batch. This is not evidence of a plugin metadata drift.
    worker.payload.parameters[0].read_only = true;
    let actual = worker.poll_native_observations(Consumption::DrainAuthor(binding)).unwrap();
    assert!(matches!(
        actual.author.unwrap().delivery,
        Delivery::Refused(Refusal::ReadOnlyParameter)
    ));
    let retained = match worker.native_author_delivery.as_ref().unwrap() {
        super::native_author::PendingDelivery::Rejected {
            binding: held,
            revision,
            reason,
            events,
        } => {
            assert_eq!(*held, binding);
            assert_eq!(*reason, Refusal::ReadOnlyParameter);
            assert_eq!(events.len(), 1);
            (*revision, events.clone())
        }
        _ => panic!("the actual rejected raw batch must remain retained"),
    };
    let source = rescan_source_snapshot(&worker);
    let before = audit(&worker);
    let cursor = worker.last_end;
    let seed = worker.payload.state.clone();
    assert!(!author_flush_eligible(&mut worker, binding));
    let repeated = worker.poll_native_observations(Consumption::DrainAuthor(binding)).unwrap();
    assert!(matches!(
        repeated.author.unwrap().delivery,
        Delivery::Refused(Refusal::ReadOnlyParameter)
    ));
    let after = audit(&worker);
    assert_eq!(
        (after.process, after.setters, after.metadata, after.values),
        (
            before.process,
            before.setters,
            before.metadata,
            before.values
        )
    );
    assert_eq!(rescan_source_snapshot(&worker), source);
    assert_eq!(worker.payload.state, seed);
    assert_eq!(worker.last_end, cursor);
    assert_eq!(
        worker.native_audio_object_observation().unwrap(),
        Some(object)
    );
    assert!(matches!(worker.native_author_delivery.as_ref().unwrap(),
        super::native_author::PendingDelivery::Rejected { revision, events, .. }
        if *revision == retained.0 && *events == retained.1));
}

#[test]
#[ignore = "requires owned SDK fixture under native Job runner"]
fn native_canonical_peer_prepared_entry_keeps_author_flush_eligible() {
    use crate::{
        NativeAudioAuthorDeliveryState as Delivery,
        NativeAudioObservationConsumption as Consumption,
    };
    let mut worker = prepare(0.375, 0);
    worker.enter_state(73).unwrap();
    let old_object = worker.native_audio_object_observation().unwrap().unwrap();
    let binding = crate::NativeAudioObjectBindRequest {
        binding: mondrian_core::AudioProcessorBindingId::new(),
        expected_object: old_object,
    };
    assert!(matches!(
        worker.bind_native_audio_object(binding).unwrap(),
        crate::NativeAudioObjectBindOutcome::Bound(_)
    ));
    let source = worker.plugin.as_ref().unwrap().editor_event_source_status().unwrap();
    assert!(source.closed_empty());
    let configuration = worker.payload.state.clone().unwrap();
    let request = crate::NativeAudioAuthorStateEntryRequest {
        binding,
        source: crate::NativeAudioAuthorStateEntrySource::CanonicalPeer {
            revision: source.revision(),
        },
        format: super::super::NativeConfigurationFormat::Vst3ComponentAndController,
        configuration: &configuration,
    };
    let crate::NativeAudioAuthorStateEntryOutcome::Prepared(receipt) =
        worker.prepare_native_author_state_entry(request).unwrap()
    else {
        panic!("a closed canonical peer must prepare through the real state-entry path");
    };
    let object = worker.native_audio_object_observation().unwrap().unwrap();
    assert_eq!(object, receipt.object());
    assert!(object.object_serial() > old_object.object_serial());
    let installed = crate::NativeAudioObjectBindRequest {
        binding: mondrian_core::AudioProcessorBindingId::new(),
        expected_object: object,
    };
    assert!(matches!(
        worker.bind_native_audio_object(installed).unwrap(),
        crate::NativeAudioObjectBindOutcome::Bound(_)
    ));
    assert!(worker.prepared_author_entry.as_ref().is_some_and(|entry| !entry.awaiting_ack()));
    assert!(worker.native_author_delivery.is_none());
    assert!(!author_flush_eligible(&mut worker, binding));
    assert!(author_flush_eligible(&mut worker, installed));
    let before = audit(&worker);
    let cursor = worker.last_end;
    {
        let mut roles = worker.split_native_processing().unwrap();
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
    let observed = worker.poll_native_observations(Consumption::DrainAuthor(installed)).unwrap();
    assert!(matches!(
        observed.author.unwrap().delivery,
        Delivery::Preserved
    ));
    assert_eq!(audit(&worker).process, before.process + 1);
    assert_eq!(worker.last_end, cursor);
    assert_eq!(
        worker.native_audio_object_observation().unwrap(),
        Some(object)
    );
    assert!(worker.prepared_author_entry.as_ref().is_some_and(|entry| !entry.awaiting_ack()));
    // CanonicalPeer is a real non-awaiting Some entry, not a simulated or
    // dynamically qualified SelectedTerminal -> reentered-Ack sequence.
}

#[derive(Debug, PartialEq, Eq)]
struct SelectedEntryDeliverySnapshot {
    binding: crate::NativeAudioObjectBindRequest,
    revision: u64,
    cohort: Option<crate::NativeRequestCohort>,
    witness: Option<vst3_host::plugin::RestartSourceWitness>,
    configuration: Vec<u8>,
    address: usize,
    capacity: usize,
}
fn selected_entry_delivery_snapshot(worker: &Vst3Worker) -> SelectedEntryDeliverySnapshot {
    let Some(super::native_author::PendingDelivery::Delivered {
        binding,
        revision,
        request_cohort,
        rescan_witness,
        configuration: Some(configuration),
    }) = &worker.native_author_delivery
    else {
        panic!("the original actual coherent delivery must remain owned");
    };
    SelectedEntryDeliverySnapshot {
        binding: *binding,
        revision: *revision,
        cohort: *request_cohort,
        witness: *rescan_witness,
        configuration: configuration.bytes().to_vec(),
        address: configuration.bytes().as_ptr() as usize,
        capacity: configuration.retained_capacity(),
    }
}

#[test]
#[ignore = "requires rebuilt owned mode14 SDK fixture under native Job runner"]
fn native_selected_terminal_keeps_old_delivery_through_fresh_initial_input_and_reentered_ack() {
    use crate::{
        NativeAudioAuthorDeliveryState as Delivery, NativeAudioAuthorStateEntryOutcome as Outcome,
        NativeAudioAuthorStateEntryRequest as Request, NativeAudioAuthorStateEntrySource as Source,
        NativeAudioObservationConsumption as Consumption,
    };
    let mut worker = prepare(0.375, 0);
    worker.enter_state(73).unwrap();
    let old = worker.native_audio_object_observation().unwrap().unwrap();
    let binding = crate::NativeAudioObjectBindRequest {
        binding: mondrian_core::AudioProcessorBindingId::new(),
        expected_object: old,
    };
    assert!(matches!(
        worker.bind_native_audio_object(binding).unwrap(),
        crate::NativeAudioObjectBindOutcome::Bound(_)
    ));
    // Legitimate late restore-time Value-only callback, not getter reentry or
    // a fabricated delivery. Apply its real feedback on the original DSP role.
    worker.plugin.as_mut().unwrap().load_state(&state(0.375, 14)).unwrap();
    {
        let mut roles = worker.split_native_processing().unwrap();
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
    let actual = worker.poll_native_observations(Consumption::DrainAuthor(binding)).unwrap();
    let Delivery::Delivered(delivery) = actual.author.unwrap().delivery else {
        panic!("real closed Value-only Source must produce a coherent delivery");
    };
    assert_eq!(delivery.request_cohort(), None);
    assert_eq!(
        delivery.completion(),
        mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration
    );
    assert_eq!(delivery.events().len(), 1);
    let retained = selected_entry_delivery_snapshot(&worker);
    assert_eq!(retained.cohort, None);
    assert_eq!(retained.witness, None);
    let configuration = delivery.configuration().unwrap();
    assert_eq!(configuration.len(), 96);
    let revision = std::num::NonZeroU64::new(delivery.revision()).unwrap();
    let seed = worker.payload.state.clone();
    let cursor = worker.last_end;
    let source = rescan_source_snapshot(&worker);
    assert!(source.0.closed_empty());
    assert_eq!(source.0.revision(), revision.get());
    assert!(!source.2.unwrap().pending());
    assert!(
        !worker.service_fresh_parameter_values(None),
        "ordinary admission cannot ignore A"
    );
    let before = audit(&worker); // Old input is already genuinely delivered.
                                 // Exercise the loader's own final proof directly: none of these requests
                                 // may replace even the cold seed before rejecting the old-owner mismatch.
    for case in ["binding", "revision", "configuration", "accepted"] {
        let mut changed = configuration.to_vec();
        changed[36] ^= 1;
        let accepted = if matches!(case, "configuration" | "accepted") {
            changed.clone()
        } else {
            configuration.to_vec()
        };
        let request = Request {
            binding: if case == "binding" {
                crate::NativeAudioObjectBindRequest {
                    binding: mondrian_core::AudioProcessorBindingId::new(),
                    ..binding
                }
            } else {
                binding
            },
            source: Source::SelectedTerminal {
                revision: if case == "revision" {
                    std::num::NonZeroU64::new(revision.get().checked_add(1).unwrap()).unwrap()
                } else {
                    revision
                },
            },
            format: super::super::NativeConfigurationFormat::Vst3ComponentAndController,
            configuration: if case == "configuration" {
                &changed
            } else {
                configuration
            },
        };
        assert!(
            matches!(worker.rebuild_native_state(Some((&request, accepted))),
            Err(AudioProcessorHostError::StateEntry(detail))
                if detail == "VST3 author state-entry source changed before retirement"),
            "case={case}"
        );
        assert_eq!(worker.payload.state, seed, "case={case}");
        assert_eq!(worker.last_end, cursor, "case={case}");
        assert_eq!(
            worker.native_binding.unwrap().binding(),
            binding.binding,
            "case={case}"
        );
        assert_eq!(
            worker.native_audio_object_observation().unwrap(),
            Some(old),
            "case={case}"
        );
        assert_eq!(rescan_source_snapshot(&worker), source, "case={case}");
        assert_eq!(
            selected_entry_delivery_snapshot(&worker),
            retained,
            "case={case}"
        );
    }
    let unchanged = audit(&worker);
    assert_eq!(
        (
            unchanged.process,
            unchanged.metadata,
            unchanged.values,
            unchanged.setters,
            unchanged.wrong_thread
        ),
        (
            before.process,
            before.metadata,
            before.values,
            before.setters,
            0
        )
    );
    let request = Request {
        binding,
        source: Source::SelectedTerminal { revision },
        format: super::super::NativeConfigurationFormat::Vst3ComponentAndController,
        configuration,
    };
    let Outcome::Prepared(receipt) = worker.prepare_native_author_state_entry(request).unwrap()
    else {
        panic!("a real retained None-prefix must prepare its new incarnation");
    };
    let fresh = worker.native_audio_object_observation().unwrap().unwrap();
    assert_eq!(fresh, receipt.object());
    assert_eq!(fresh.process_id(), old.process_id());
    assert!(fresh.object_serial() > old.object_serial());
    assert_eq!(selected_entry_delivery_snapshot(&worker), retained);
    assert_eq!(worker.payload.state.as_deref(), Some(configuration));
    assert_eq!(worker.last_end, None);
    let fresh_source = rescan_source_snapshot(&worker);
    assert!(fresh_source.0.closed_empty());
    assert!(!fresh_source.2.unwrap().pending());
    let assert_input_held = |worker: &Vst3Worker| {
        assert!(worker.plugin.as_ref().unwrap().pending_processor_parameter_input().unwrap());
        assert!(matches!(
            worker.plugin.as_ref().unwrap().save_state(),
            Err(vst3_host::Error::PendingParameterDelivery)
        ));
    };
    assert_input_held(&worker); // No state audit while the actual initial COM point is pending.
    let installed = crate::NativeAudioObjectBindRequest {
        binding: mondrian_core::AudioProcessorBindingId::new(),
        expected_object: fresh,
    };
    assert!(matches!(
        worker.bind_native_audio_object(installed).unwrap(),
        crate::NativeAudioObjectBindOutcome::Bound(_)
    ));
    assert!(!author_flush_eligible(&mut worker, installed));
    assert_input_held(&worker);
    let ack = crate::NativeAudioReenteredAuthorAcknowledgement {
        preparation: receipt,
        installed_binding: installed,
    };
    let mut wrong_receipt = receipt;
    wrong_receipt.configuration_sha256[0] ^= 1;
    for wrong in [
        crate::NativeAudioReenteredAuthorAcknowledgement { preparation: wrong_receipt, ..ack },
        crate::NativeAudioReenteredAuthorAcknowledgement { installed_binding: binding, ..ack },
    ] {
        let refused = worker
            .poll_native_observations(Consumption::AcknowledgeReenteredAuthor(wrong))
            .unwrap();
        assert!(matches!(
            refused.author.unwrap().delivery,
            Delivery::Refused(crate::NativeAudioAuthorRefusal::StaleDelivery)
        ));
        assert_eq!(selected_entry_delivery_snapshot(&worker), retained);
        assert_eq!(rescan_source_snapshot(&worker), fresh_source);
        assert!(worker.prepared_author_entry.as_ref().unwrap().awaiting_ack());
        assert_input_held(&worker);
    }
    let accepted = worker
        .poll_native_observations(Consumption::AcknowledgeReenteredAuthor(ack))
        .unwrap();
    assert_eq!(accepted.object, Some(fresh));
    assert_eq!(
        accepted.author.unwrap().delivery,
        Delivery::ReenteredAcknowledged(ack)
    );
    assert!(worker.native_author_delivery.is_none());
    assert!(!worker.prepared_author_entry.as_ref().unwrap().awaiting_ack());
    assert!(author_flush_eligible(&mut worker, installed));
    assert_input_held(&worker);
    let repeated = worker
        .poll_native_observations(Consumption::AcknowledgeReenteredAuthor(ack))
        .unwrap();
    assert!(matches!(
        repeated.author.unwrap().delivery,
        Delivery::Refused(crate::NativeAudioAuthorRefusal::StaleDelivery)
    ));
    assert_eq!(rescan_source_snapshot(&worker), fresh_source);
    assert_input_held(&worker);
    worker.enter_state(73).unwrap();
    assert_input_held(&worker);
    assert_eq!(
        worker.native_audio_object_observation().unwrap(),
        Some(fresh)
    );
    let render = worker.render;
    let ids = [worker.payload.parameters[0].parameter_id().unwrap()];
    let auxiliary = AudioProcessorAuxiliaryInputContract::default();
    let mut pcm = [1.0; 8];
    {
        let mut roles = worker.split_native_processing().unwrap();
        std::thread::scope(|scope| {
            let processing = &mut roles.processing;
            let pcm = &mut pcm;
            let auxiliary = &auxiliary;
            let ids = &ids;
            scope
                .spawn(move || {
                    processing.process(IsolatedAudioProcessorWorkerBlock::new(
                        73,
                        4,
                        render,
                        pcm,
                        auxiliary,
                        &[],
                        ids,
                        &[],
                    ))
                })
                .join()
                .unwrap()
                .unwrap();
        });
    }
    assert_eq!(pcm, [0.75; 8]);
    assert_eq!(worker.last_end, Some(77));
    assert!(!worker.plugin.as_ref().unwrap().pending_processor_parameter_input().unwrap());
    let after = audit(&worker); // First legal audit of this actual fresh object.
    assert_eq!((after.process, after.wrong_thread), (1, 0));
    // The fixed ABI exposes Process count, not a per-point counter. The pending
    // input transition and SDK initial-COM controls do not become a new point audit.
    assert_eq!(
        worker.native_audio_object_observation().unwrap(),
        Some(fresh)
    );
    assert_eq!(worker.payload.state.as_deref(), Some(configuration));
    assert_eq!(rescan_source_snapshot(&worker), fresh_source);
}

#[test]
#[ignore = "requires rebuilt owned mode13 SDK fixture under native Job runner"]
fn native_externally_retired_rescan_cannot_enter_selected_terminal_rebuild() {
    use crate::{
        NativeAudioAuthorDeliveryState as Delivery, NativeAudioAuthorStateEntryOutcome as Outcome,
        NativeAudioAuthorStateEntryRefusal as Refusal,
        NativeAudioAuthorStateEntryRequest as Request, NativeAudioAuthorStateEntrySource as Source,
        NativeAudioObservationConsumption as Consumption,
    };
    let (mut worker, binding) = entered_rescan_worker();
    let _ = late_rescan_dsp(&mut worker, &[(2, 0.75)]);
    let collected = worker.poll_native_observations(Consumption::DrainAuthor(binding)).unwrap();
    let Delivery::Delivered(delivery) = collected.author.unwrap().delivery else {
        panic!("a real exact-4 source must retain its witness");
    };
    let retained = selected_entry_delivery_snapshot(&worker);
    let witness = retained.witness.unwrap();
    assert_eq!(delivery.request_cohort(), retained.cohort);
    assert!(retained.cohort.is_some());
    assert!(worker.plugin.as_mut().unwrap().retire_restart_cohort(witness.cohort()).unwrap());
    let source = rescan_source_snapshot(&worker);
    assert!(source.0.closed_empty());
    assert!(!source.2.unwrap().pending());
    assert!(worker.pending_reprepare().is_none());
    let seed = worker.payload.state.clone();
    let cursor = worker.last_end;
    let phase = worker.initial_signal_phase;
    let values = worker.last_parameter_values.clone();
    let object = worker.native_audio_object_observation().unwrap();
    let before = audit(&worker); // Real DSP/controller delivery completed before capture.
    let request = Request {
        binding,
        source: Source::SelectedTerminal {
            revision: std::num::NonZeroU64::new(delivery.revision()).unwrap(),
        },
        format: super::super::NativeConfigurationFormat::Vst3ComponentAndController,
        configuration: delivery.configuration().unwrap(),
    };
    assert!(matches!(
        worker.prepare_native_author_state_entry(request).unwrap(),
        Outcome::Refused(Refusal::SourcePending)
    ));
    let request = Request {
        binding,
        source: Source::SelectedTerminal {
            revision: std::num::NonZeroU64::new(delivery.revision()).unwrap(),
        },
        format: super::super::NativeConfigurationFormat::Vst3ComponentAndController,
        configuration: delivery.configuration().unwrap(),
    };
    assert!(
        matches!(worker.rebuild_native_state(Some((&request, request.configuration.to_vec()))),
        Err(AudioProcessorHostError::StateEntry(detail))
            if detail == "VST3 author state-entry source changed before retirement")
    );
    assert_eq!(worker.native_audio_object_observation().unwrap(), object);
    assert_eq!(worker.native_binding.unwrap().binding(), binding.binding);
    assert_eq!(worker.payload.state, seed);
    assert_eq!(worker.last_end, cursor);
    assert_eq!(worker.initial_signal_phase, phase);
    assert_eq!(worker.last_parameter_values, values);
    assert!(worker.prepared_author_entry.is_none());
    assert_eq!(selected_entry_delivery_snapshot(&worker), retained);
    assert_eq!(rescan_source_snapshot(&worker), source);
    let after = audit(&worker);
    assert_eq!(
        (
            after.process,
            after.metadata,
            after.values,
            after.setters,
            after.wrong_thread
        ),
        (
            before.process,
            before.metadata,
            before.values,
            before.setters,
            0
        )
    );
}
