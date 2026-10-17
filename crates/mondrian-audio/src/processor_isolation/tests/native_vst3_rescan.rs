//! Actual Product IPC for a retained VST3 parameter-rescan prefix, without a suffix.
//! Child Ack is not a Source installation or Session recovery. Completed may precede
//! the deferred main callback; one normal Preserve and an explicit preflight retain A.
use super::*;
use crate::{
    AudioProcessorFactory, NativeAudioAuthorAcknowledgement, NativeAudioAuthorDeliveryState,
    NativeAudioAuthorEvent, NativeAudioAuthorRefusal, NativeAudioObjectBindOutcome,
    NativeAudioObjectBindRequest, NativeAudioObservationConsumption, NativeObservedRequest,
};
use mondrian_core::{AudioProcessorBindingId, AudioProcessorInstanceId};
use mondrian_timeline::audio::AudioProcessorDefinitionRef;
use std::collections::BTreeMap;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, WaitForSingleObject};

fn mode13_state(gain: f64) -> Vec<u8> {
    let mut native = b"MIV1".to_vec();
    native.extend_from_slice(&1_u32.to_le_bytes());
    native.extend_from_slice(&gain.to_le_bytes());
    native.extend_from_slice(&13_u32.to_le_bytes());
    let mut state = b"VST3HOST_STATE\0\0".to_vec();
    for word in [1_u32, 20, 20] {
        state.extend_from_slice(&word.to_le_bytes());
    }
    state.extend_from_slice(&native);
    state.extend_from_slice(&native);
    state
}

fn block_context(start: i64) -> AudioProcessorProcessContext {
    AudioProcessorProcessContext::new(
        AudioRenderRequest { start_sample: start, frames: 4 },
        AudioSampleRate::new(48_000).unwrap(),
        AudioChannelLayout::Stereo,
        AudioProcessingMode::Offline,
        AudioKernelBackend::ScalarReference,
    )
}

fn assert_captured_mode13(state: &[u8]) {
    // Independent native state envelope and fixture audit, not a second SDK object.
    assert_eq!(state.len(), 28 + 24 + 44);
    assert_eq!(&state[..16], b"VST3HOST_STATE\0\0");
    for (at, expected) in [(16, 1_u32), (20, 24), (24, 44)] {
        assert_eq!(&state[at..at + 4], &expected.to_le_bytes());
    }
    let expected = mode13_state(0.75);
    assert_eq!(&state[28..48], &expected[28..48]);
    assert_eq!(&state[52..72], &expected[48..68]);
    let calls = u32::from_le_bytes(state[48..52].try_into().unwrap());
    assert!(
        calls >= 2,
        "one PCM call plus the real DrainAuthor zero-sample bridge: {calls}"
    );
    assert_eq!(
        &state[92..96],
        &0_u32.to_le_bytes(),
        "no wrong-thread controller operation"
    );
}

#[test]
#[ignore = "current normal Product IPC18 and rebuilt owned initial-values mode13 DLL; no suffix or Runtime recovery qualification"]
fn installed_vst3_rescan_prefix_crosses_product_ipc_and_ack_keeps_parent_hold() {
    use NativeAudioAuthorDeliveryState as Delivery;
    use NativeAudioObservationConsumption as Consumption;

    let material = |name| {
        let path = PathBuf::from(std::env::var_os(name).expect("explicit current native material"));
        assert!(path.is_absolute() && path.is_file(), "{name}");
        path
    };
    let helper = material("MONDRIAN_VST3_TEST_HELPER");
    let binary = material("MONDRIAN_VST3_INITIAL_VALUES_TEST_PLUGIN");
    let render = AudioRenderContract {
        sample_rate: 48_000,
        max_block_frames: 4,
        processing_mode: AudioProcessingMode::Offline,
        ..test_render_contract()
    };
    let state = mode13_state(0.375);
    let registration = probe_vst3_plugin_registration(
        &helper,
        &binary,
        "61921CF43A2B4A68AE5AE08241D85E22",
        render,
        Some(&state),
    )
    .expect("actual current Product restored-instance probe");
    assert_eq!(registration.main_audio_input, Vst3MainAudioInput::Present);
    assert!(registration.auxiliary_inputs.buses.is_empty());
    assert!(registration.event_bus_contract.inputs.is_empty());
    assert!(registration.event_bus_contract.outputs.is_empty());
    assert_eq!(registration.parameters.len(), 1);
    assert_eq!(registration.parameters[0].id, 0);
    let definition = AudioProcessorDefinitionRef::Vst3 {
        class_id: registration.class_id.clone(),
        vendor: registration.vendor.clone(),
        schema_version: 1,
        binary_sha256: Some(registration.binary_sha256),
    };
    let resolver = Vst3AudioProcessorSpecResolver::new(helper, [registration]).unwrap();
    let spec = resolver
        .resolve(AudioProcessorPrepareRequest::new(
            AudioProcessorOccurrence {
                instance_id: AudioProcessorInstanceId::new(),
                owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
                insertion: AudioProcessorInsertionPoint::PreFader,
            },
            &definition,
            &BTreeMap::new(),
            Some(&state),
            render,
        ))
        .expect("original explicit no-Note VST3 spec");
    assert!(spec.note_input_contract.ports.is_empty());
    assert!(spec.musical_time_reference.is_none());
    let ids = spec.parameter_ids.clone();
    assert_eq!(ids.len(), 1);
    let factory = IsolatedAudioProcessorFactory::prepare(spec)
        .expect("original unprobed validation launch and strict shutdown");
    let mut processor = factory.create().expect("actual Product retained Ready owner");
    let object = processor.native_audio_object_observation().unwrap().unwrap();
    assert_ne!(object.process_id().get(), std::process::id());
    // SAFETY: the authenticated supervised owner supplies this live PID. This
    // query/synchronize-only HANDLE never waits/reaps/kills instead of its owner.
    let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, object.process_id().get()) };
    assert!(!raw.is_null());
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
        258
    );
    processor.enter_state(73).expect("real first Enter, with no later re-entry");
    assert_eq!(
        processor.native_audio_object_observation().unwrap(),
        Some(object)
    );
    let source_binding = NativeAudioObjectBindRequest {
        binding: AudioProcessorBindingId::new(),
        expected_object: object,
    };
    let NativeAudioObjectBindOutcome::Bound(bound) =
        processor.bind_native_audio_object(source_binding).unwrap()
    else {
        panic!("actual Entered object must bind");
    };
    assert_eq!(
        (bound.binding(), bound.object()),
        (source_binding.binding, object)
    );
    let initial = processor.poll_native_observations(Consumption::Preserve).unwrap();
    assert_eq!(initial.object, Some(object));
    assert!(initial.pending_reprepare.is_none());
    let initial_author = initial.author.unwrap();
    assert!(initial_author.source.closed_empty());
    assert_eq!(initial_author.delivery, Delivery::Preserved);

    let mut audio = TestAudioIo { main: vec![1.0; 8], auxiliary: Vec::new() };
    let events = [AudioParameterEvent { sample_offset: 2, value: 0.75 }];
    let range = 0..1;
    processor
        .process(
            block_context(73),
            &mut audio,
            AudioParameterEventBatch::new(73, 4, &ids, &[range], &events),
        )
        .expect("actual successful PCM prefix, not Deferred or a replayed block");
    assert_eq!(
        audio.main,
        [0.375, 0.375, 0.375, 0.375, 0.75, 0.75, 0.75, 0.75]
    );

    // The normal main loop services the real deferred setter before answering
    // this one Preserve command. Its callback need not have preceded Completed.
    let live = processor.poll_native_observations(Consumption::Preserve).unwrap();
    assert_eq!(live.object, Some(object));
    let Some(NativeObservedRequest::Cohort(prefix)) = live.pending_reprepare else {
        panic!(
            "actual main-thread setter must publish a full cohort: {:?}",
            live.pending_reprepare
        );
    };
    assert!(prefix.serial != 0 && prefix.serial != u32::MAX);
    assert_eq!(
        prefix.request,
        mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 }
    );
    let live_author = live.author.unwrap();
    assert_eq!(live_author.delivery, Delivery::Preserved);
    assert_eq!(live_author.source.pending_events(), 1);
    assert_eq!(
        live_author.source.revision(),
        initial_author.source.revision().checked_add(1).unwrap()
    );
    let pending = NativeObservedRequest::Cohort(prefix);
    assert_eq!(
        processor.preflight_reprepare().unwrap(),
        Some(pending),
        "explicit existing preflight retains this observed A; no backfill into Completed"
    );

    let delivered = processor
        .poll_native_observations(Consumption::DrainAuthor(source_binding))
        .unwrap();
    assert_eq!(delivered.object, Some(object));
    assert_eq!(delivered.pending_reprepare, Some(pending));
    let author = delivered.author.unwrap();
    let Delivery::Delivered(delivery) = author.delivery else {
        panic!("real singleton rescan must cross Product IPC as a delivery");
    };
    assert!(author.source.closed_empty());
    delivery.validate(author.source).unwrap();
    assert_eq!(delivery.binding(), source_binding);
    assert_eq!(delivery.request_cohort(), Some(prefix));
    assert_eq!(delivery.revision(), live_author.source.revision());
    assert_eq!(
        delivery.completion(),
        mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration
    );
    let [NativeAudioAuthorEvent::ParameterValuesRescanned { kind, values }] = delivery.events()
    else {
        panic!("one exact typed rescan, not synthetic gestures or partial values");
    };
    assert_eq!(*kind, mondrian_core::NativeAudioParameterRescanKind::Values);
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].parameter(), &ids[0]);
    assert_eq!(
        values[0].value().unwrap().value().to_bits(),
        0.75_f64.to_bits()
    );
    assert_captured_mode13(
        delivery.configuration().expect("same actual component/controller capture"),
    );
    let expected_await = Delivery::AwaitingAcknowledgement {
        binding: source_binding,
        revision: delivery.revision(),
        request_cohort: Some(prefix),
    };
    for _ in 0..2 {
        // Preserve has no zero-sample bridge and returns no fresh configuration:
        // this is the original retained binding/revision/cohort, not recapture.
        let repeated = processor.poll_native_observations(Consumption::Preserve).unwrap();
        assert_eq!(repeated.object, Some(object));
        assert_eq!(repeated.pending_reprepare, Some(pending));
        let repeated = repeated.author.unwrap();
        assert_eq!(repeated.source, author.source);
        assert_eq!(repeated.delivery, expected_await);
    }
    let installed_binding = NativeAudioObjectBindRequest {
        binding: AudioProcessorBindingId::new(),
        expected_object: object,
    };
    let NativeAudioObjectBindOutcome::Bound(rebound) =
        processor.bind_native_audio_object(installed_binding).unwrap()
    else {
        panic!("same actual object must accept a fresh binding");
    };
    assert_eq!(
        (rebound.binding(), rebound.object()),
        (installed_binding.binding, object)
    );
    let ack = NativeAudioAuthorAcknowledgement {
        source_binding,
        installed_binding,
        revision: delivery.revision(),
    };
    for wrong in [
        NativeAudioAuthorAcknowledgement { installed_binding: source_binding, ..ack },
        NativeAudioAuthorAcknowledgement { source_binding: installed_binding, ..ack },
        NativeAudioAuthorAcknowledgement {
            revision: ack.revision.checked_add(1).unwrap(),
            ..ack
        },
    ] {
        let refused = processor
            .poll_native_observations(Consumption::AcknowledgeAuthor(wrong))
            .unwrap();
        assert_eq!(refused.object, Some(object));
        assert_eq!(refused.pending_reprepare, Some(pending));
        let refused = refused.author.unwrap();
        assert_eq!(refused.source, author.source);
        assert_eq!(
            refused.delivery,
            Delivery::Refused(NativeAudioAuthorRefusal::StaleDelivery)
        );
        let retained = processor.poll_native_observations(Consumption::Preserve).unwrap();
        assert_eq!(retained.author.unwrap().delivery, expected_await);
    }
    let accepted = processor.poll_native_observations(Consumption::AcknowledgeAuthor(ack)).unwrap();
    assert_eq!(accepted.object, Some(object));
    assert!(
        accepted.pending_reprepare.is_none(),
        "actual child raw A has settled"
    );
    let accepted = accepted.author.unwrap();
    assert_eq!(accepted.source, author.source);
    assert_eq!(
        accepted.delivery,
        Delivery::Acknowledged { acknowledgement: ack, request_cohort: Some(prefix) }
    );
    let repeated = processor.poll_native_observations(Consumption::AcknowledgeAuthor(ack)).unwrap();
    assert_eq!(repeated.object, Some(object));
    assert!(repeated.pending_reprepare.is_none());
    let repeated = repeated.author.unwrap();
    assert_eq!(repeated.source, author.source);
    assert_eq!(
        repeated.delivery,
        Delivery::Refused(NativeAudioAuthorRefusal::StaleDelivery)
    );

    // Poll does not merge or clear the parent's old sticky request. A successful
    // child Ack alone is not an installed Runtime/Session replacement.
    assert_eq!(processor.preflight_reprepare().unwrap(), Some(pending));
    let context = block_context(77);
    let mut unpublished = TestAudioIo { main: vec![99.0; 8], auxiliary: Vec::new() };
    let next_events = [AudioParameterEvent { sample_offset: 0, value: 0.125 }];
    let next_range = 0..1;
    assert_eq!(
        processor.process(
            context,
            &mut unpublished,
            AudioParameterEventBatch::new(77, 4, &ids, &[next_range], &next_events)
        ),
        Err(AudioProcessorHostError::ReprepareRequired(pending))
    );
    assert_eq!(unpublished.main, [99.0; 8]);
    assert_eq!(context.request().start_sample, 77);
    assert_eq!(context.request().frames, 4);
    assert_eq!(
        (next_events[0].sample_offset, next_events[0].value.to_bits()),
        (0, 0.125_f64.to_bits())
    );
    let held = processor.poll_native_observations(Consumption::Preserve).unwrap();
    assert_eq!(held.object, Some(object));
    assert!(held.pending_reprepare.is_none());
    let held = held.author.unwrap();
    assert_eq!(held.source, author.source);
    assert_eq!(held.delivery, Delivery::Preserved);
    // The pure parent refusal and unchanged source/PCM are observed here. The
    // fixture exposes no terminal process counter over IPC; do not claim one.
    drop(processor);
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 2_000) },
        0
    );
    let mut exit = u32::MAX;
    assert_ne!(
        unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) },
        0
    );
    assert_eq!(exit, 0);
    // Same observed child OS Exit0 within 2s; not proof of 100ms graceful exit.
}
