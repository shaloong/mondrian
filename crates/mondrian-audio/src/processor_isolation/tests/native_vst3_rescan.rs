//! Actual Product IPC and a finite canonical Runtime parameter-rescan handoff.
//! The original worker-only gates do not prove Session recovery. The Runtime gate
//! below uses actual canonical transactions and the installed Source/owner chain.
use super::*;
use crate::{
    AudioProcessorFactory, NativeAudioAuthorAcknowledgement, NativeAudioAuthorDeliveryState,
    NativeAudioAuthorEvent, NativeAudioAuthorRefusal, NativeAudioObjectBindOutcome,
    NativeAudioObjectBindRequest, NativeAudioObservationConsumption, NativeObservedRequest,
};
use mondrian_core::{AudioProcessorBindingId, AudioProcessorInstanceId};
use mondrian_timeline::audio::{AudioProcessorDefinitionRef, AudioProcessorParameter};
use std::collections::BTreeMap;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, WaitForSingleObject};

fn fixture_state(gain: f64, mode: u32) -> Vec<u8> {
    let mut native = b"MIV1".to_vec();
    native.extend_from_slice(&1_u32.to_le_bytes());
    native.extend_from_slice(&gain.to_le_bytes());
    native.extend_from_slice(&mode.to_le_bytes());
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
    let expected = fixture_state(0.75, 13);
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
#[ignore = "current normal Product IPC19 and rebuilt owned initial-values mode13 DLL; no suffix or Runtime recovery qualification"]
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
    let state = fixture_state(0.375, 13);
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
    let descriptor = &registration.parameters[0];
    assert!(!descriptor.read_only);
    let expected_id = descriptor.parameter_id().expect("actual native parameter identity");
    let schema = descriptor.authoring_schema().expect("actual probed parameter schema");
    assert_eq!(schema.parameter_id, expected_id);
    let parameters = BTreeMap::from([(
        schema.parameter_id.clone(),
        AudioProcessorParameter::from_schema(schema).expect("valid author parameter definition"),
    )]);
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
            &parameters,
            Some(&state),
            render,
        ))
        .expect("original explicit no-Note VST3 spec");
    assert!(spec.note_input_contract.ports.is_empty());
    assert!(spec.musical_time_reference.is_none());
    let ids = spec.parameter_ids.clone();
    assert_eq!(ids.len(), 1);
    assert_eq!(ids, [expected_id]);
    let factory = IsolatedAudioProcessorFactory::prepare(spec)
        .expect("original unprobed validation launch and strict shutdown");
    let mut processor = factory.create(None).expect("actual Product retained Ready owner");
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
    for (wrong, expected_refusal) in [
        (
            NativeAudioAuthorAcknowledgement { installed_binding: source_binding, ..ack },
            NativeAudioAuthorRefusal::OwnerChanged,
        ),
        (
            NativeAudioAuthorAcknowledgement { source_binding: installed_binding, ..ack },
            NativeAudioAuthorRefusal::StaleDelivery,
        ),
        (
            NativeAudioAuthorAcknowledgement {
                revision: ack.revision.checked_add(1).unwrap(),
                ..ack
            },
            NativeAudioAuthorRefusal::StaleDelivery,
        ),
    ] {
        let refused = processor
            .poll_native_observations(Consumption::AcknowledgeAuthor(wrong))
            .unwrap();
        assert_eq!(refused.object, Some(object));
        assert_eq!(refused.pending_reprepare, Some(pending));
        let refused = refused.author.unwrap();
        assert_eq!(refused.source, author.source);
        assert_eq!(refused.delivery, Delivery::Refused(expected_refusal));
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

    // Exact matching SomeA Ack now settles only that adapter-cache cohort.
    // The separate portable actual-Session control proves a previously issued
    // ticket still holds until a real Source/Plan handoff, even with live None.
    assert_eq!(processor.preflight_reprepare().unwrap(), None);
    let held = processor.poll_native_observations(Consumption::Preserve).unwrap();
    assert_eq!(held.object, Some(object));
    assert!(held.pending_reprepare.is_none());
    let held = held.author.unwrap();
    assert_eq!(held.source, author.source);
    assert_eq!(held.delivery, Delivery::Preserved);
    // No additional process command was sent after Ack. This is adapter-cache
    // settlement plus unchanged native Source, not Runtime/Session recovery or
    // a zero native-callback count (DrainAuthor can process a zero-sample bridge).
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

#[test]
#[ignore = "current normal Product IPC19 and owned initial-values mode14 DLL; no native counter or Runtime recovery qualification"]
fn installed_vst3_initial_input_crosses_product_poll_without_authored_lanes() {
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
    // The component independently restores .375; only the controller's actual
    // restore callback reports .75. No setter or authored lane delivers it.
    let state = fixture_state(0.375, 14);
    let registration = probe_vst3_plugin_registration(
        &helper,
        &binary,
        "61921CF43A2B4A68AE5AE08241D85E22",
        render,
        Some(&state),
    )
    .expect("actual restored mode14 Product probe");
    assert_eq!(registration.main_audio_input, Vst3MainAudioInput::Present);
    assert!(registration.auxiliary_inputs.buses.is_empty());
    assert!(registration.event_bus_contract.inputs.is_empty());
    assert!(registration.event_bus_contract.outputs.is_empty());
    assert_eq!(registration.parameters.len(), 1);
    assert_eq!(registration.parameters[0].id, 0);
    assert!(!registration.parameters[0].read_only);
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
        .expect("actual spec with initial native input and no author automation");
    assert!(
        spec.parameter_ids.is_empty(),
        "no authored lane supplies .75"
    );
    assert!(spec.note_input_contract.ports.is_empty());
    assert!(spec.musical_time_reference.is_none());
    let factory = IsolatedAudioProcessorFactory::prepare(spec).unwrap();
    let mut processor = factory.create(None).unwrap();
    let object = processor.native_audio_object_observation().unwrap().unwrap();
    assert_ne!(object.process_id().get(), std::process::id());
    // SAFETY: this authenticated live child identity came from its retained
    // supervisor. This handle has only query/synchronize rights.
    let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, object.process_id().get()) };
    assert!(!raw.is_null());
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
        258
    );
    processor.enter_state(73).unwrap();
    let binding = NativeAudioObjectBindRequest {
        binding: AudioProcessorBindingId::new(),
        expected_object: object,
    };
    let NativeAudioObjectBindOutcome::Bound(bound) =
        processor.bind_native_audio_object(binding).unwrap()
    else {
        panic!("the same initialized object must bind");
    };
    assert_eq!((bound.binding(), bound.object()), (binding.binding, object));
    let initial = processor.poll_native_observations(Consumption::Preserve).unwrap();
    assert_eq!(initial.object, Some(object));
    assert!(initial.pending_reprepare.is_none());
    let initial = initial.author.unwrap();
    assert!(
        initial.source.closed_empty(),
        "initial handoff consumed its Source"
    );
    assert_eq!(initial.delivery, Delivery::Preserved);
    for _ in 0..2 {
        // The first bridge must deliver the original initial COM input. The
        // second retains the old NoPending/GUI replay contract and may process;
        // this IPC lane exposes no native call/point counter.
        let observed =
            processor.poll_native_observations(Consumption::DrainAuthor(binding)).unwrap();
        assert_eq!(observed.object, Some(object));
        assert!(observed.pending_reprepare.is_none());
        let observed = observed.author.unwrap();
        assert_eq!(observed.source, initial.source);
        assert!(observed.source.closed_empty());
        assert_eq!(observed.delivery, Delivery::Preserved);
    }
    for start in [73, 77] {
        let mut audio = TestAudioIo { main: vec![1.0; 8], auxiliary: Vec::new() };
        processor
            .process(
                block_context(start),
                &mut audio,
                AudioParameterEventBatch::new(start, 4, &[], &[], &[]),
            )
            .expect("contiguous PCM after the original initial input was delivered");
        assert_eq!(
            audio.main, [0.75; 8],
            "no authored event can replace a dropped initial point"
        );
        let observed = processor.poll_native_observations(Consumption::Preserve).unwrap();
        assert_eq!(observed.object, Some(object));
        assert!(observed.pending_reprepare.is_none());
        let observed = observed.author.unwrap();
        assert_eq!(observed.source, initial.source);
        assert_eq!(observed.delivery, Delivery::Preserved);
    }
    assert!(processor.preflight_reprepare().unwrap().is_none());
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
    // Same-child OS Exit0 within 2s is not a new 100ms graceful-exit proof.
}

// The actual single-owner and peer-pending Runtime gates share only their
// deterministic media input and read-only continuity collector.
mod runtime_rescan_support {
    use super::*;
    use crate::{
        AudioContinuityEvidenceVisitor, AudioDecodedSource, AudioExecutionError,
        AudioMediaResolver, AudioProcessorCursorEvidence, AudioSessionCursorEvidence,
        ResolvedAudioSource,
    };
    use mondrian_core::{AssetId, AudioSourceComponentId, ExecutionCancellationToken};
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Independent dyadic PCM, consumed by the actual bound media Source.
    pub(super) const INPUT: [[f32; 2]; 8] = [
        [1.0, -0.5],
        [0.25, 0.75],
        [-1.0, 0.5],
        [2.0, -0.25],
        [0.5, 1.0],
        [-0.75, 0.25],
        [1.5, -1.0],
        [-0.5, 2.0],
    ];
    struct IndexedPcm(Arc<AtomicUsize>);
    impl AudioDecodedSource for IndexedPcm {
        fn read_interleaved(
            &self,
            start: i64,
            frames: usize,
            output: &mut [f32],
            cancellation: &ExecutionCancellationToken,
        ) -> Result<(), String> {
            if cancellation.is_canceled() || output.len() != frames * 2 {
                return Err("cancelled or invalid owned stereo extent".into());
            }
            self.0.fetch_add(1, Ordering::Relaxed);
            for (offset, pair) in output.chunks_exact_mut(2).enumerate() {
                let sample = start
                    .checked_add(i64::try_from(offset).map_err(|_| "offset")?)
                    .ok_or("owned source sample overflow")?;
                let value = usize::try_from(sample)
                    .ok()
                    .and_then(|i| INPUT.get(i))
                    .copied()
                    .unwrap_or([0.0; 2]);
                pair.copy_from_slice(&value);
            }
            Ok(())
        }
    }
    pub(super) struct Media {
        pub(super) resolves: AtomicUsize,
        pub(super) reads: Arc<AtomicUsize>,
    }
    impl AudioMediaResolver for Media {
        fn resolve(
            &self,
            _: AssetId,
            _: AudioSourceComponentId,
            rate: u32,
        ) -> Result<ResolvedAudioSource, String> {
            if rate != 48_000 {
                return Err("owned source rate".into());
            }
            self.resolves.fetch_add(1, Ordering::Relaxed);
            Ok(ResolvedAudioSource::new(
                AudioChannelLayout::Stereo,
                Arc::new(IndexedPcm(Arc::clone(&self.reads))),
            ))
        }
    }
    #[derive(Debug, Default, PartialEq, Eq)]
    pub(super) struct Cursors {
        pub(super) sessions: Vec<AudioSessionCursorEvidence>,
        pub(super) processors: Vec<AudioProcessorCursorEvidence>,
    }
    impl AudioContinuityEvidenceVisitor for Cursors {
        fn session(
            &mut self,
            value: AudioSessionCursorEvidence,
        ) -> Result<(), AudioExecutionError> {
            self.sessions.push(value);
            Ok(())
        }
        fn processor(
            &mut self,
            value: AudioProcessorCursorEvidence,
        ) -> Result<(), AudioExecutionError> {
            self.processors.push(value);
            Ok(())
        }
    }
}

#[test]
#[ignore = "current normal Product IPC19 and owned initial-values mode13 DLL; finite direct-root audio handoff, no Note/ticket/100ms/RT qualification"]
fn installed_vst3_runtime_rescan_handoff_resumes_pcm() {
    use crate::{
        compile_audio_program, AudioCompileRequest, AudioContinuityEpoch, AudioExecutionError,
        AudioNativeProcessorAddress, AudioNativeProcessorOwnerLease, AudioProcessorCursorState,
        AudioProgramRuntime, AudioRuntimeResourceGrant, AudioStateEntry,
        NativeAudioAuthorAdmissionError, NativeAudioAuthorHoldReason, NativeAudioAuthorProgram,
        NativeAudioParameterHandoffContinuity,
    };
    use mondrian_core::{
        AssetId, AudioOwnerInstallationError, AudioOwnerInstallationSource, AudioSourceComponentId,
        ExactAutomationCurve, ExecutionCancellationToken, NativeAudioAuthorCallback,
        NativeAudioAuthorCompletion, NativeAudioParameterRescanKind, TimelineTime,
    };
    use mondrian_timeline::audio::AudioChannelStripOwner;
    use mondrian_timeline::{
        apply_audio_processor_rack_edit, AudioChannelStripRack, AudioProcessorRackAddress,
        AudioProcessorRackEdit, AudioProcessorRackEditRequest, Clip, NativeAudioAuthorUpdate,
        NativeAudioParameterWrite, NativeAudioParameterWriteMode, Sequence,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    use runtime_rescan_support::{Cursors, Media, INPUT};
    fn cursors(runtime: &AudioProgramRuntime) -> Cursors {
        let mut value = Cursors::default();
        runtime.visit_continuity_evidence(&mut value).unwrap();
        assert_eq!((value.sessions.len(), value.processors.len()), (1, 1));
        value
    }
    fn owner(runtime: &AudioProgramRuntime) -> AudioNativeProcessorOwnerLease {
        let value = cursors(runtime);
        let processor = value.processors[0];
        let epoch = match processor.state {
            AudioProcessorCursorState::Unentered => None,
            AudioProcessorCursorState::WaitingForFirstSignal { epoch }
            | AudioProcessorCursorState::Active { epoch, .. } => Some(epoch),
            other => panic!("exact owned Runtime is not cold/active: {other:?}"),
        };
        runtime
            .native_owner_lease(AudioNativeProcessorAddress {
                root: value.sessions[0].identity,
                local: processor.identity,
                occurrence: processor.occurrence,
                epoch,
            })
            .unwrap()
    }
    fn same_progress(before: &Cursors, after: &Cursors) {
        assert_eq!(
            after.sessions[0].identity.session_id,
            before.sessions[0].identity.session_id
        );
        assert_eq!(after.sessions[0].state, before.sessions[0].state);
        assert_eq!(
            after.sessions[0].public_output_lookahead_frames,
            before.sessions[0].public_output_lookahead_frames
        );
        assert_eq!(
            after.processors[0].identity.session_id,
            before.processors[0].identity.session_id
        );
        assert_eq!(
            after.processors[0].occurrence,
            before.processors[0].occurrence
        );
        assert_eq!(after.processors[0].state, before.processors[0].state);
    }
    fn pcm(runtime: &mut AudioProgramRuntime, start: usize) {
        let mut output = [93.0; 8];
        runtime
            .render_into(
                AudioRenderRequest { start_sample: start as i64, frames: 4 },
                &mut output,
            )
            .expect("actual contiguous Runtime PCM after exact Source/Ack admission");
        let expected: Vec<_> = INPUT[start..start + 4]
            .iter()
            .flatten()
            .map(|sample| sample * 0.75_f32)
            .collect();
        assert_eq!(output.as_slice(), expected.as_slice());
    }

    let material = |name| {
        let path = PathBuf::from(std::env::var_os(name).expect("explicit current native material"));
        assert!(path.is_absolute() && path.is_file(), "{name}");
        path
    };
    let helper = material("MONDRIAN_VST3_TEST_HELPER");
    let binary = material("MONDRIAN_VST3_INITIAL_VALUES_TEST_PLUGIN");
    let catalog = Arc::new(
        DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [binary])
            .expect("actual current Product discovery"),
    );
    let render = AudioRenderContract {
        sample_rate: 48_000,
        max_block_frames: 4,
        processing_mode: AudioProcessingMode::Offline,
        // Finite single-owner fixture allowance; not a budget-edge/whole-heap qualification.
        processor_session_scratch_budget_bytes: 32 * 1024 * 1024,
        ..test_render_contract()
    };
    let state = fixture_state(0.375, 13);
    let mut author = catalog
        .create_instance(
            "61921CF43A2B4A68AE5AE08241D85E22",
            render,
            Some(state.clone()),
        )
        .expect("actual restored-mode13 author and parameter schema");
    assert_eq!(author.parameters.len(), 1);
    assert!(!author.bypassed);
    assert_eq!(
        author.opaque_state.as_deref().map(|bytes| bytes.as_slice()),
        Some(state.as_slice())
    );
    let gain = author.parameters.keys().next().unwrap().clone();
    assert_eq!(gain, ParameterId::new_static("vst3.param.0"));
    author
        .set_parameter_automation(ExactAutomationCurve::new(gain.clone(), 0.75).unwrap())
        .unwrap();
    let instance = author.id;
    let mut sequence = Sequence::new("actual VST3 Runtime retained rescan and canonical handoff");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    sequence
        .add_media_audio_clip(
            sequence.audio_tracks[0].id,
            Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
            AudioSourceComponentId::primary(),
        )
        .unwrap();
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(author);
    let resolver = IsolatedAudioProcessorResolver::new(catalog);
    let media = Media {
        resolves: AtomicUsize::new(0),
        reads: Arc::new(AtomicUsize::new(0)),
    };
    let cancel = ExecutionCancellationToken::new();
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &media,
        &resolver,
        render,
        None,
        AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 32 * 1024 * 1024),
    )
    .expect("actual canonical Output effect, media and retained native owner");
    assert_eq!(media.resolves.load(Ordering::Relaxed), 1);
    let source = AudioOwnerInstallationSource::new();
    let (registration, verifier) = source.register().unwrap();
    runtime.bind_native_owner_installation(verifier).unwrap();
    let initial_claim = source
        .reserve_install(&registration, None)
        .and_then(|reservation| source.begin_commit(reservation))
        .unwrap()
        .commit()
        .unwrap();
    runtime.install_native_owner_bindings(&initial_claim, &cancel).unwrap();
    runtime.validate_native_author_source_closed(&cancel).unwrap();
    let cold = owner(&runtime);
    assert_eq!(cold.editor_target().instance, instance);
    let object = runtime.observe_native_owner_object(cold).unwrap();
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(613),
            start_sample: 0,
        })
        .expect("one initial public Enter, no later re-entry/rebuild");
    let first = owner(&runtime);
    assert_eq!(first.editor_target().continuity_epoch, Some(613));
    assert_eq!(runtime.observe_native_owner_object(first).unwrap(), object);
    assert_ne!(object.process_id().get(), std::process::id());
    // SAFETY: this is the real authenticated native owner, observed before any
    // handoff. The query/synchronize-only handle never owns reaping/termination.
    let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, object.process_id().get()) };
    assert!(!raw.is_null());
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
        258
    );
    pcm(&mut runtime, 0);
    let mut previous = None;
    // Two fixed real prefixes: A from the first successful PCM block, then the
    // natural B from the resumed block's same-value setter. This is no Poll loop.
    for next in [4, 8] {
        let old = owner(&runtime);
        let old_root = runtime.resource_root_identity();
        let before = cursors(&runtime);
        let reads = media.reads.load(Ordering::Relaxed);
        let old_claim = source.current_claim().unwrap().unwrap();
        let old_verifier = runtime.native_owner_installation_verifier().unwrap();
        let mut sentinel = [93.0; 8];
        let error = runtime
            .render_into(
                AudioRenderRequest { start_sample: next, frames: 4 },
                &mut sentinel,
            )
            .expect_err("ordered real rescan Source must hold before any next PCM");
        let AudioExecutionError::NativeAuthorPending(held) = error else {
            panic!("expected Source-first hold, actual={error:?}");
        };
        assert_eq!(held.target, old.editor_target());
        assert_eq!(held.reason, NativeAudioAuthorHoldReason::PendingCallbacks);
        assert_eq!(sentinel, [93.0; 8]);
        assert_eq!(cursors(&runtime), before);
        assert_eq!(media.reads.load(Ordering::Relaxed), reads);
        assert_eq!(owner(&runtime), old);
        assert_eq!(runtime.observe_native_owner_object(old).unwrap(), object);
        old_verifier.validate(&source.current_claim().unwrap().unwrap()).unwrap();
        assert_eq!(
            source.current_claim().unwrap().unwrap().generation(),
            old_claim.generation()
        );

        // One ordinary real DrainAuthor round trip, including its existing
        // possible zero-sample parameter bridge. No exact native call count.
        let proposal = runtime
            .poll_native_observations(&cancel)
            .unwrap()
            .expect("single actual source completes its retained rescan")
            .author
            .expect("actual native rescan author prefix, never a synthetic proposal");
        assert_eq!(proposal.sequence(), sequence.id);
        assert_eq!(proposal.target(), old.editor_target());
        assert!(proposal.terminal());
        assert_eq!(
            proposal.completion(),
            NativeAudioAuthorCompletion::CoherentConfiguration
        );
        assert_eq!(
            (proposal.open_parameter_edits(), proposal.group_depth()),
            (0, 0)
        );
        let [NativeAudioAuthorCallback::ParameterValuesRescanned { kind, values }] =
            proposal.callbacks()
        else {
            panic!("one actual ordered typed rescan");
        };
        assert_eq!(*kind, NativeAudioParameterRescanKind::Values);
        assert_eq!(values.as_slice(), [(gain.clone(), 0.75_f64.to_bits())]);
        assert_captured_mode13(proposal.state().unwrap().as_slice());
        let a = runtime
            .find_resource_session(old_root)
            .unwrap()
            .continuing_rescan_cohort(&proposal, old_root, old_claim.generation())
            .unwrap()
            .expect("authentic retained prefix's Some cohort, not a later live lookup");
        assert_eq!(
            a.request,
            mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 }
        );
        assert!(a.serial != 0 && a.serial != u32::MAX);
        if let Some(prior) = previous {
            assert!(a.serial > prior);
        }
        previous = Some(a.serial);
        assert_eq!(cursors(&runtime), before);
        let pending = runtime.validate_native_author_source_closed(&cancel).unwrap_err();
        assert!(
            matches!(
                pending,
                NativeAudioAuthorAdmissionError::Pending {
                    reason: NativeAudioAuthorHoldReason::OutstandingDelivery,
                    ..
                }
            ),
            "actual={pending:?}"
        );

        let current = &sequence.audio_program.outputs[0].strip.pre_fader.processors[0];
        assert_eq!(current.id, proposal.target().instance);
        let request = AudioProcessorRackEditRequest {
            address: AudioProcessorRackAddress::ChannelStrip {
                owner: AudioChannelStripOwner::ProgramOutput {
                    output_id: sequence.audio_program.outputs[0].id,
                },
                rack: AudioChannelStripRack::PreFader,
            },
            edit: AudioProcessorRackEdit::SetNativeAuthorState {
                processor_id: current.id,
                expected_definition: current.definition.clone(),
                update: NativeAudioAuthorUpdate {
                    parameters: vec![NativeAudioParameterWrite {
                        schema: Box::new(current.parameters.get(&gain).unwrap().schema.clone()),
                        value: f64::from_bits(values[0].1),
                        mode: NativeAudioParameterWriteMode::Unkeyed,
                    }
                    .into()],
                    opaque_state: proposal.state().cloned(),
                },
            },
        };
        let transaction = apply_audio_processor_rack_edit(&mut sequence, &request).expect(
            "real atomic canonical author transaction accepts only the observed schema/state",
        );
        assert!(transaction.changed);
        assert_eq!(
            sequence.audio_program.outputs[0].strip.pre_fader.processors[0]
                .opaque_state
                .as_ref(),
            proposal.state()
        );
        let programs = [NativeAudioAuthorProgram {
            sequence: sequence.id,
            program: Arc::new(
                compile_audio_program(
                    &sequence,
                    AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
                )
                .expect(
                    "compile the accepted canonical model, not a second hand-built author graph",
                ),
            ),
        }];
        let (registration, destination) = source.register().unwrap();
        let reservation = source.reserve_install(&registration, Some(&old_claim)).unwrap();
        let mut authorization = source.authorize_runtime_installation(reservation).unwrap();
        let mut prepared = runtime
            .prepare_native_author_parameter_handoff(&programs, &proposal, destination, &cancel)
            .expect("authentic SomeA direct-root continuing permission");
        old_verifier.validate(&source.current_claim().unwrap().unwrap()).unwrap();
        assert_eq!(
            source.current_claim().unwrap().unwrap().generation(),
            old_claim.generation()
        );
        assert_eq!(cursors(&runtime), before);
        assert_eq!(owner(&runtime), old);
        assert_eq!(runtime.observe_native_owner_object(old).unwrap(), object);
        let installed = runtime
            .install_native_author_parameter_handoff(&mut prepared, &mut authorization, &cancel)
            .expect("actual Source-authorized same-owner plan publication");
        assert_eq!(
            installed.continuity,
            NativeAudioParameterHandoffContinuity::Preserved
        );
        let lease = owner(&runtime);
        assert_eq!(lease.editor_target(), installed.target);
        assert_ne!(lease.binding(), old.binding());
        assert_eq!(
            installed.target.local_session,
            old.editor_target().local_session
        );
        assert_eq!(installed.target.continuity_epoch, Some(613));
        assert_eq!(
            installed.target.installed_generation,
            installed.claim.generation()
        );
        assert_eq!(
            source.current_claim().unwrap().unwrap().generation(),
            installed.claim.generation()
        );
        let verifier = runtime.native_owner_installation_verifier().unwrap();
        verifier.validate(&installed.claim).unwrap();
        assert!(matches!(
            authorization.begin_commit(&verifier),
            Err(AudioOwnerInstallationError::ConsumedAuthorization)
        ));
        assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), object);
        let after = cursors(&runtime);
        same_progress(&before, &after);
        assert_ne!(
            runtime.resource_root_identity().prepared_plan_id,
            old_root.prepared_plan_id
        );
        assert_eq!(
            runtime.resource_root_identity().session_id,
            old_root.session_id
        );
        assert_eq!(media.resolves.load(Ordering::Relaxed), 1);
        assert_eq!(media.reads.load(Ordering::Relaxed), reads);
        drop(prepared); // Retired plans are outside the Source guard before Ack.
        let acknowledged =
            runtime.acknowledge_native_author_parameter_handoff(&proposal, &cancel).expect(
                "exact authentic SomeA Ack only after canonical transaction and Source install",
            );
        assert_eq!(acknowledged.target, installed.target);
        assert_eq!(
            acknowledged.continuity,
            NativeAudioParameterHandoffContinuity::Preserved
        );
        assert_eq!(
            acknowledged.claim.generation(),
            installed.claim.generation()
        );
        verifier.validate(&acknowledged.claim).unwrap();
        assert_eq!(
            source.current_claim().unwrap().unwrap().generation(),
            installed.claim.generation()
        );
        assert_eq!(cursors(&runtime), after);
        assert_eq!(owner(&runtime), lease);
        assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), object);
        assert_eq!(media.reads.load(Ordering::Relaxed), reads);
        runtime
            .validate_native_author_source_closed(&cancel)
            .expect("real child source and parent outstanding are closed after exact Ack");
        assert!(runtime
            .find_resource_session(runtime.resource_root_identity())
            .unwrap()
            .pending_reprepare_ticket()
            .is_none());
        if next == 4 {
            pcm(&mut runtime, 4);
        }
    }
    assert!(media.reads.load(Ordering::Relaxed) > 0);
    assert_eq!(media.resolves.load(Ordering::Relaxed), 1);
    // Main-input effect only; no Note ledger, live suffix arriving during Ack,
    // native callback count, ReleasePending, physical GUI or RT qualification.
    drop(runtime);
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
    // Same observed child's OS Exit0 within 2s does not prove 100ms graceful exit.
}

#[test]
#[ignore = "current normal Product IPC19 and owned initial-values mode13 DLL; two real pending owners preserve single-selected refusal, not a joint handoff qualification"]
fn installed_vst3_runtime_peer_pending_preserves_single_owner_admission() {
    run_terminal_peer_contract(false);
}

#[test]
#[ignore = "current normal Product IPC19 and owned initial-values mode13 DLL; finite terminal same-Root batch, two exact ACKs and preserved PCM, not App/GUI/RT/100ms qualification"]
fn installed_vst3_runtime_terminal_batch_installs_joint_frontier_and_acknowledges_each_owner() {
    run_terminal_peer_contract(true);
}

fn run_terminal_peer_contract(install_batch: bool) {
    use crate::{
        compile_audio_program, AudioCompileRequest, AudioContinuityEpoch,
        AudioNativeProcessorAddress, AudioProcessorCursorState, AudioProgramRuntime,
        AudioRuntimeResourceGrant, AudioStateEntry, NativeAudioAuthorAdmissionError,
        NativeAudioAuthorHoldReason, NativeAudioAuthorProgram, NativeAudioParameterHandoffError,
    };
    use mondrian_core::{
        AssetId, AudioOwnerInstallationSource, AudioSourceComponentId, ExactAutomationCurve,
        ExecutionCancellationToken, NativeAudioAuthorCallback, NativeAudioAuthorCompletion,
        NativeAudioAuthorProposal, NativeAudioParameterRescanKind, TimelineTime,
    };
    use mondrian_timeline::audio::AudioChannelStripOwner;
    use mondrian_timeline::{
        apply_audio_processor_rack_edit, AudioChannelStripRack, AudioProcessorRackAddress,
        AudioProcessorRackEdit, AudioProcessorRackEditRequest, Clip, NativeAudioAuthorUpdate,
        NativeAudioParameterWrite, NativeAudioParameterWriteMode, Sequence,
    };
    use runtime_rescan_support::{Cursors, Media, INPUT};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn cursors(runtime: &AudioProgramRuntime) -> Cursors {
        let mut value = Cursors::default();
        runtime.visit_continuity_evidence(&mut value).unwrap();
        assert_eq!((value.sessions.len(), value.processors.len()), (1, 2));
        value
    }
    fn program_for(
        sequence: &Sequence,
        proposal: &NativeAudioAuthorProposal,
    ) -> [NativeAudioAuthorProgram; 1] {
        programs_for(sequence, std::slice::from_ref(proposal))
    }
    fn programs_for(
        sequence: &Sequence,
        proposals: &[NativeAudioAuthorProposal],
    ) -> [NativeAudioAuthorProgram; 1] {
        let mut candidate = sequence.clone();
        for proposal in proposals {
            apply_proposal(&mut candidate, proposal);
        }
        [NativeAudioAuthorProgram {
            sequence: candidate.id,
            program: Arc::new(
                compile_audio_program(
                    &candidate,
                    AudioCompileRequest::program(candidate.audio_program.outputs[0].id),
                )
                .unwrap(),
            ),
        }]
    }
    fn apply_proposal(candidate: &mut Sequence, proposal: &NativeAudioAuthorProposal) {
        // Only actually delivered prefixes enter this one private candidate.
        // A singleton still never silently folds in its unrelated pending peer.
        let current = candidate.audio_program.outputs[0]
            .strip
            .pre_fader
            .processors
            .iter()
            .find(|processor| processor.id == proposal.target().instance)
            .unwrap();
        let gain = ParameterId::new_static("vst3.param.0");
        let [NativeAudioAuthorCallback::ParameterValuesRescanned { kind, values }] =
            proposal.callbacks()
        else {
            panic!("one real retained rescan per selected owner");
        };
        assert_eq!(*kind, NativeAudioParameterRescanKind::Values);
        assert_eq!(values.as_slice(), [(gain.clone(), 0.75_f64.to_bits())]);
        assert!(proposal.terminal());
        assert_eq!(
            proposal.completion(),
            NativeAudioAuthorCompletion::CoherentConfiguration
        );
        assert_eq!(
            (proposal.open_parameter_edits(), proposal.group_depth()),
            (0, 0)
        );
        assert_captured_mode13(proposal.state().unwrap().as_slice());
        let request = AudioProcessorRackEditRequest {
            address: AudioProcessorRackAddress::ChannelStrip {
                owner: AudioChannelStripOwner::ProgramOutput {
                    output_id: candidate.audio_program.outputs[0].id,
                },
                rack: AudioChannelStripRack::PreFader,
            },
            edit: AudioProcessorRackEdit::SetNativeAuthorState {
                processor_id: current.id,
                expected_definition: current.definition.clone(),
                update: NativeAudioAuthorUpdate {
                    parameters: vec![NativeAudioParameterWrite {
                        schema: Box::new(current.parameters.get(&gain).unwrap().schema.clone()),
                        value: f64::from_bits(values[0].1),
                        mode: NativeAudioParameterWriteMode::Unkeyed,
                    }
                    .into()],
                    opaque_state: proposal.state().cloned(),
                },
            },
        };
        assert!(apply_audio_processor_rack_edit(candidate, &request).unwrap().changed);
    }

    let material = |name| {
        let path = PathBuf::from(std::env::var_os(name).expect("explicit current native material"));
        assert!(path.is_absolute() && path.is_file(), "{name}");
        path
    };
    let catalog = Arc::new(
        DiscoveredVst3AudioProcessorSpecResolver::discover(
            material("MONDRIAN_VST3_TEST_HELPER"),
            [material("MONDRIAN_VST3_INITIAL_VALUES_TEST_PLUGIN")],
        )
        .expect("actual current normal Product discovery"),
    );
    let render = AudioRenderContract {
        sample_rate: 48_000,
        max_block_frames: 4,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 32 * 1024 * 1024,
        ..test_render_contract()
    };
    let mut sequence = Sequence::new("two actual native rescan owners, one Source");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    sequence
        .add_media_audio_clip(
            sequence.audio_tracks[0].id,
            Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
            AudioSourceComponentId::primary(),
        )
        .unwrap();
    let mut instances = Vec::with_capacity(2);
    for _ in 0..2 {
        // Two independent discoveries/instance identities, never a cloned rack
        // ID or a fabricated native target. Their actual children stay distinct.
        let state = fixture_state(0.375, 13);
        let mut author = catalog
            .create_instance(
                "61921CF43A2B4A68AE5AE08241D85E22",
                render,
                Some(state.clone()),
            )
            .expect("actual mode13 schema/state");
        assert_eq!(author.parameters.len(), 1);
        assert!(!author.bypassed);
        assert_eq!(
            author.opaque_state.as_deref().unwrap().as_slice(),
            state.as_slice()
        );
        author
            .set_parameter_automation(
                ExactAutomationCurve::new(ParameterId::new_static("vst3.param.0"), 0.75).unwrap(),
            )
            .unwrap();
        instances.push(author.id);
        sequence.audio_program.outputs[0].strip.pre_fader.processors.push(author);
    }
    assert_ne!(instances[0], instances[1]);
    let resolver = IsolatedAudioProcessorResolver::new(catalog);
    let media = Media {
        resolves: AtomicUsize::new(0),
        reads: Arc::new(AtomicUsize::new(0)),
    };
    let cancel = ExecutionCancellationToken::new();
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &media,
        &resolver,
        render,
        None,
        // Explicit finite two-owner allowance, not a whole-heap/budget-edge proof.
        AudioRuntimeResourceGrant::new(1, 128 * 1024 * 1024, 64 * 1024 * 1024),
    )
    .expect("one actual Source tree with two independent Hosted owners");
    let source = AudioOwnerInstallationSource::new();
    let (registration, verifier) = source.register().unwrap();
    runtime.bind_native_owner_installation(verifier.clone()).unwrap();
    let initial_claim = source
        .reserve_install(&registration, None)
        .and_then(|reservation| source.begin_commit(reservation))
        .unwrap()
        .commit()
        .unwrap();
    runtime.install_native_owner_bindings(&initial_claim, &cancel).unwrap();
    runtime.validate_native_author_source_closed(&cancel).unwrap();
    let root = runtime.resource_root_identity();
    let cold = cursors(&runtime);
    let leases_for = |runtime: &AudioProgramRuntime, evidence: &Cursors| {
        let root = runtime.resource_root_identity();
        evidence
            .processors
            .iter()
            .map(|processor| {
                let epoch = match processor.state {
                    AudioProcessorCursorState::Unentered => None,
                    AudioProcessorCursorState::WaitingForFirstSignal { epoch }
                    | AudioProcessorCursorState::Active { epoch, .. } => Some(epoch),
                    other => panic!("actual native owner is cold/active: {other:?}"),
                };
                runtime
                    .native_owner_lease(AudioNativeProcessorAddress {
                        root,
                        local: processor.identity,
                        occurrence: processor.occurrence,
                        epoch,
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>()
    };
    let cold_leases = leases_for(&runtime, &cold);
    let objects: Vec<_> = cold_leases
        .iter()
        .map(|lease| runtime.observe_native_owner_object(*lease).unwrap())
        .collect();
    assert_ne!(objects[0].process_id(), objects[1].process_id());
    let handles: Vec<_> = objects
        .iter()
        .map(|object| {
            assert_ne!(object.process_id().get(), std::process::id());
            // SAFETY: observation names this actual owned child. The limited query/
            // synchronization handle never owns native lifecycle or termination.
            let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, object.process_id().get()) };
            assert!(!raw.is_null());
            unsafe { OwnedHandle::from_raw_handle(raw.cast()) }
        })
        .collect();
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(619),
            start_sample: 0,
        })
        .unwrap();
    let mut first_pcm = [93.0; 8];
    runtime
        .render_into(
            AudioRenderRequest { start_sample: 0, frames: 4 },
            &mut first_pcm,
        )
        .expect("both real native effects process their first four frames");
    let expected: Vec<_> =
        INPUT[..4].iter().flatten().map(|sample| sample * 0.75_f32 * 0.75_f32).collect();
    assert_eq!(first_pcm.as_slice(), expected.as_slice());
    let before = cursors(&runtime);
    let leases = leases_for(&runtime, &before);
    for instance in &instances {
        assert!(leases.iter().any(|lease| lease.editor_target().instance == *instance));
    }
    assert!(
        before.processors.iter().all(|processor| matches!(processor.state,
        AudioProcessorCursorState::Active { epoch, next_input_signal_sample }
            if epoch == AudioContinuityEpoch::new(619)
                && next_input_signal_sample.sample() == 4))
    );
    let reads = media.reads.load(Ordering::Relaxed);
    assert!(reads > 0);
    assert_eq!(media.resolves.load(Ordering::Relaxed), 1);

    let unchanged = |runtime: &mut AudioProgramRuntime| {
        assert_eq!(runtime.resource_root_identity(), root);
        assert_eq!(cursors(runtime), before);
        assert_eq!(leases_for(runtime, &before), leases);
        assert_eq!(media.reads.load(Ordering::Relaxed), reads);
        assert_eq!(media.resolves.load(Ordering::Relaxed), 1);
        let claim = source.current_claim().unwrap().unwrap();
        assert_eq!(claim.generation(), initial_claim.generation());
        verifier.validate(&claim).unwrap();
        for (lease, object) in leases.iter().zip(&objects) {
            assert_eq!(
                runtime.observe_native_owner_object(*lease).unwrap(),
                *object
            );
        }
    };
    unchanged(&mut runtime);
    // Runtime starts cursor 0 and advances one actual Hosted index per Poll.
    // Match returned identities, rather than assigning A/B from authored order.
    let selected = runtime
        .poll_native_observations(&cancel)
        .unwrap()
        .unwrap()
        .author
        .expect("first actual native owner retains its rescan");
    let selected_index = leases
        .iter()
        .position(|lease| lease.editor_target() == selected.target())
        .unwrap();
    assert_eq!(selected.sequence(), sequence.id);
    let peer = leases[1 - selected_index];
    assert_ne!(selected.target().instance, peer.editor_target().instance);
    let programs = program_for(&sequence, &selected);
    let (destination_registration, destination) = source.register().unwrap();
    let error = match runtime.prepare_native_author_parameter_handoff(
        &programs,
        &selected,
        destination.clone(),
        &cancel,
    ) {
        Err(error) => error,
        Ok(_) => panic!("an unpolled peer's real pending source cannot be accepted"),
    };
    assert!(
        matches!(&error,
        NativeAudioParameterHandoffError::Admission(NativeAudioAuthorAdmissionError::Pending {
            target, reason: NativeAudioAuthorHoldReason::PendingCallbacks,
        }) if target.as_ref() == &peer.editor_target()),
        "actual={error:?}"
    );
    unchanged(&mut runtime);

    let other = runtime
        .poll_native_observations(&cancel)
        .unwrap()
        .unwrap()
        .author
        .expect("next actual Hosted occurrence, not a guessed second proposal");
    assert_eq!(other.target(), peer.editor_target());
    assert_eq!(other.sequence(), sequence.id);
    assert_ne!(selected.target().instance, other.target().instance);
    let other_programs = program_for(&sequence, &other);
    for (proposal, programs, blocked_by) in [
        (&selected, &programs, other.target()),
        (&other, &other_programs, selected.target()),
    ] {
        let error = match runtime.prepare_native_author_parameter_handoff(
            programs,
            proposal,
            destination.clone(),
            &cancel,
        ) {
            Err(error) => error,
            Ok(_) => panic!("single-selected admission cannot authorize another retained owner"),
        };
        assert!(
            matches!(&error,
            NativeAudioParameterHandoffError::Admission(NativeAudioAuthorAdmissionError::Pending {
                target, reason: NativeAudioAuthorHoldReason::OutstandingDelivery,
            }) if target.as_ref() == &blocked_by),
            "actual={error:?}"
        );
        unchanged(&mut runtime);
    }
    // Failed preparation may have obtained an unpublished same-object Bind
    // receipt before meeting its peer. Neither prefix is acknowledged/rebound
    // in the installed parent: real ordinary reoffers must retain both originals.
    for original in [&selected, &other] {
        let repeated = runtime
            .poll_native_observations(&cancel)
            .unwrap()
            .unwrap()
            .author
            .expect("both retained real prefixes remain available");
        assert!(repeated.same_retained_proposal(original));
    }
    unchanged(&mut runtime);
    // The original negative gate stops here. Its false branch still attempts
    // no Source publication, Ack, second Enter or second PCM.
    if install_batch {
        let members = [selected.clone(), other.clone()];
        let joint = programs_for(&sequence, &members);
        for invalid in [
            Vec::new(),
            vec![selected.clone(), selected.clone()],
            vec![selected.clone()],
        ] {
            assert!(
                runtime
                    .prepare_native_author_terminal_batch(
                        &joint,
                        &invalid,
                        destination.clone(),
                        &cancel
                    )
                    .is_err(),
                "empty/duplicate/omitted member never publishes"
            );
            unchanged(&mut runtime);
        }
        let mut prepared = runtime
            .prepare_native_author_terminal_batch(&joint, &members, destination.clone(), &cancel)
            .expect("all real retained members jointly prepare one canonical Plan");
        for member in &members {
            assert!(prepared.installed_target(member).is_none());
        }
        unchanged(&mut runtime);
        let reservation =
            source.reserve_install(&destination_registration, Some(&initial_claim)).unwrap();
        let mut authorization = source.authorize_runtime_installation(reservation).unwrap();
        let acquire_failure = runtime.install_native_author_terminal_batch_with_commit_guard(
            &mut prepared,
            &mut authorization,
            &cancel,
            || Err::<(), _>(crate::AudioNativeOwnerError::OwnerUnavailable.into()),
        );
        assert!(matches!(
            acquire_failure,
            Err(NativeAudioParameterHandoffError::Owner(_))
        ));
        for member in &members {
            assert!(prepared.installed_target(member).is_none());
        }
        unchanged(&mut runtime);
        // A plain local guard models only the callback lifetime. It confers no
        // canonical author permission; this real-module gate exercises Audio.
        struct Guard<'a>(&'a std::cell::Cell<bool>);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        let guarded = std::cell::Cell::new(false);
        let (installed, guard) = runtime
            .install_native_author_terminal_batch_with_commit_guard(
                &mut prepared,
                &mut authorization,
                &cancel,
                || {
                    guarded.set(true);
                    Ok(Guard(&guarded))
                },
            )
            .expect("one original Source CAS publishes the whole frontier");
        assert!(guarded.get());
        assert_eq!(
            installed.continuity,
            crate::NativeAudioParameterHandoffContinuity::Preserved
        );
        assert_ne!(installed.claim.generation(), initial_claim.generation());
        for member in &members {
            assert!(prepared.installed_target(member).is_some());
        }
        drop(guard); // No actual object RPC/Ack/retirement while this guard lives.
        assert!(!guarded.get());
        let after = cursors(&runtime);
        let installed_leases = leases_for(&runtime, &after);
        assert_eq!(runtime.resource_root_identity().session_id, root.session_id);
        assert_ne!(
            runtime.resource_root_identity().prepared_plan_id,
            root.prepared_plan_id
        );
        assert_eq!(after.sessions[0].state, before.sessions[0].state);
        for (old, current) in before.processors.iter().zip(&after.processors) {
            assert_eq!(current.identity.session_id, old.identity.session_id);
            assert_eq!(current.occurrence, old.occurrence);
            assert_eq!(current.state, old.state);
        }
        for (index, old) in leases.iter().enumerate() {
            assert!(
                runtime.observe_native_owner_object(*old).is_err(),
                "old installed target is stale"
            );
            assert_eq!(
                runtime.observe_native_owner_object(installed_leases[index]).unwrap(),
                objects[index]
            );
        }
        for member in &members {
            let current = installed_leases
                .iter()
                .find(|lease| lease.editor_target().instance == member.target().instance)
                .unwrap();
            assert_eq!(
                prepared.installed_target(member),
                Some(current.editor_target())
            );
        }
        assert_eq!(
            source.current_claim().unwrap().unwrap().generation(),
            installed.claim.generation()
        );
        assert_eq!(media.reads.load(Ordering::Relaxed), reads);
        let first_ack = runtime
            .acknowledge_native_author_parameter_handoff(&selected, &cancel)
            .expect("only this exact first retained source confirms its own ACK");
        assert_eq!(Some(first_ack.target), prepared.installed_target(&selected));
        let cancelled = ExecutionCancellationToken::new();
        cancelled.cancel();
        assert!(matches!(
            runtime.acknowledge_native_author_parameter_handoff(&other, &cancelled),
            Err(NativeAudioParameterHandoffError::Admission(
                NativeAudioAuthorAdmissionError::Cancelled
            ))
        ));
        assert_eq!(
            source.current_claim().unwrap().unwrap().generation(),
            installed.claim.generation()
        );
        assert_eq!(cursors(&runtime), after);
        let mut held = [93.0; 8];
        assert!(
            matches!(
                runtime.render_into(AudioRenderRequest { start_sample: 4, frames: 4 }, &mut held),
                Err(crate::AudioExecutionError::NativeAuthorPending(_))
            ),
            "unacknowledged member still owns HOLD"
        );
        assert_eq!(held, [93.0; 8]);
        assert_eq!(cursors(&runtime), after);
        assert_eq!(media.reads.load(Ordering::Relaxed), reads);
        let second_ack = runtime
            .acknowledge_native_author_parameter_handoff(&other, &cancel)
            .expect("retry only the remaining member, never re-install/re-ACK the first");
        assert_eq!(Some(second_ack.target), prepared.installed_target(&other));
        assert_eq!(second_ack.claim.generation(), installed.claim.generation());
        runtime
            .validate_native_author_source_closed(&cancel)
            .expect("both genuine original prefixes are confirmed; no invented empty proof");
        assert_eq!(cursors(&runtime), after);
        // Every new target remains an observation from the owning prepared
        // records; no separate target Vec survives its fee/owner retirement.
        drop(prepared);
        let mut resumed = [93.0; 8];
        runtime
            .render_into(
                AudioRenderRequest { start_sample: 4, frames: 4 },
                &mut resumed,
            )
            .expect("one continued epoch produces full joint canonical PCM");
        let expected: Vec<_> = INPUT[4..8]
            .iter()
            .flatten()
            .map(|sample| sample * 0.75_f32 * 0.75_f32)
            .collect();
        assert_eq!(resumed.as_slice(), expected.as_slice());
    }
    drop(runtime);
    // Both post-Drop OS-exit observations share one two-second deadline. This
    // neither bounds the preceding Drop nor proves the native 100ms exit gate.
    let exit_deadline = Instant::now() + Duration::from_secs(2);
    for handle in &handles {
        let remaining = exit_deadline.saturating_duration_since(Instant::now());
        let remaining_ms = u32::try_from(remaining.as_millis()).unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), remaining_ms) },
            0
        );
        let mut exit = u32::MAX;
        assert_ne!(
            unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) },
            0
        );
        assert_eq!(exit, 0);
    }
}
