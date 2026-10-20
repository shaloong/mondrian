//! Native output contract controls. Owned DLL/Windows controls remain ignored until
//! an external gate supplies a retained normal Product and the exact producer DLL.
//! Direct SDK callback counts and canonical Runtime PCM are separate observations.
use super::*;
use crate::{AudioProcessorAuxiliaryOutputBusContract, AudioProcessorAuxiliaryOutputContract};
use mondrian_core::AudioChannelLayout;

fn outputs(layout: AudioChannelLayout) -> AudioProcessorAuxiliaryOutputContract {
    AudioProcessorAuxiliaryOutputContract {
        buses: [2, 3]
            .into_iter()
            .map(|id| AudioProcessorAuxiliaryOutputBusContract {
                bus_key: format!("clap.audio-output.{id}"),
                channel_layout: layout,
            })
            .collect(),
    }
}
fn render(layout: AudioChannelLayout) -> crate::AudioRenderContract {
    crate::AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: layout,
        max_block_frames: 14,
        processing_mode: crate::AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 16 * 1024 * 1024,
        public_output_lookahead_budget_frames: 4096,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    }
}
#[test]
fn clap_output_contract_preserves_native_order_and_rejects_ambiguous_identity_layout_and_bound() {
    let valid = outputs(AudioChannelLayout::Mono);
    super::super::clap_discovery::validate_audio_outputs(1, &valid, AudioChannelLayout::Mono)
        .unwrap();
    let mut reversed = valid.clone();
    reversed.buses.reverse();
    super::super::clap_discovery::validate_audio_outputs(1, &reversed, AudioChannelLayout::Mono)
        .unwrap();
    assert_ne!(
        valid, reversed,
        "native binding order is part of the contract"
    );
    for key in [
        "clap.audio-output.1",
        "clap.audio-output.3",
        "clap.audio-output.02",
        "clap.audio-output.4294967295",
        "other.2",
    ] {
        let mut bad = valid.clone();
        bad.buses[0].bus_key = key.into();
        assert!(
            super::super::clap_discovery::validate_audio_outputs(1, &bad, AudioChannelLayout::Mono)
                .is_err(),
            "{key}"
        );
    }
    assert!(super::super::clap_discovery::validate_audio_outputs(
        u32::MAX,
        &valid,
        AudioChannelLayout::Mono
    )
    .is_err());
    assert!(super::super::clap_discovery::validate_audio_outputs(
        1,
        &valid,
        AudioChannelLayout::Stereo
    )
    .is_err());
    let bounded = AudioProcessorAuxiliaryOutputContract {
        buses: (2..33)
            .map(|id| AudioProcessorAuxiliaryOutputBusContract {
                bus_key: format!("clap.audio-output.{id}"),
                channel_layout: AudioChannelLayout::Mono,
            })
            .collect(),
    };
    super::super::clap_discovery::validate_audio_outputs(1, &bounded, AudioChannelLayout::Mono)
        .unwrap();
    let mut excess = bounded;
    excess.buses.push(AudioProcessorAuxiliaryOutputBusContract {
        bus_key: "clap.audio-output.33".into(),
        channel_layout: AudioChannelLayout::Mono,
    });
    assert!(super::super::clap_discovery::validate_audio_outputs(
        1,
        &excess,
        AudioChannelLayout::Mono
    )
    .is_err());
}
#[test]
fn clap_output_actual_abi_scan_accepts_direction_local_ids_and_refuses_invalid_output_shapes() {
    let good = super::tests::output_metadata_entry::<11>();
    let render = crate::AudioRenderContract {
        max_block_frames: 4,
        ..render(AudioChannelLayout::Stereo)
    };
    let probe = super::super::clap_discovery::probe_loaded(
        &good,
        "org.mondrian.test.sidechain",
        render,
        None,
        false,
    )
    .unwrap();
    assert_eq!(probe.main_audio_output_id, 0);
    let inputs = super::super::protocol::auxiliary_from_wire(probe.auxiliary_buses);
    let outputs = super::super::protocol::outputs_from_wire(probe.auxiliary_outputs);
    assert_eq!(
        inputs.buses.iter().map(|b| b.bus_key.as_str()).collect::<Vec<_>>(),
        vec!["clap.audio-input.11", "clap.audio-input.7"]
    );
    assert_eq!(
        outputs.buses.iter().map(|b| b.bus_key.as_str()).collect::<Vec<_>>(),
        vec!["clap.audio-output.11", "clap.audio-output.7"]
    );
    for (entry, reason) in [
        (
            super::tests::output_metadata_entry::<13>(),
            "bounded inputs",
        ),
        (
            super::tests::output_metadata_entry::<14>(),
            "bounded inputs",
        ),
        (
            super::tests::output_metadata_entry::<15>(),
            "reason=main_role direction=output",
        ),
        (
            super::tests::output_metadata_entry::<16>(),
            "reason=duplicate_id direction=output",
        ),
        (
            super::tests::output_metadata_entry::<17>(),
            "reason=main_role direction=output",
        ),
        (
            super::tests::output_metadata_entry::<18>(),
            "reason=channel_count direction=output",
        ),
    ] {
        let failure = super::super::clap_discovery::probe_loaded(
            &entry,
            "org.mondrian.test.sidechain",
            render,
            None,
            false,
        );
        assert!(
            matches!(failure,Err(AudioProcessorHostError::InvalidContract(detail)) if detail.contains(reason)),
            "{reason}"
        );
    }
}
#[test]
fn clap_output_audio_storage_accounts_actual_pcm_descriptors_keys_and_unrouted_ports() {
    for layout in [AudioChannelLayout::Mono, AudioChannelLayout::Stereo] {
        let render = render(layout);
        let channels = render.channel_count();
        let source_contract = outputs(layout);
        let outputs = source_contract.clone(); // The child retains this actual prepared clone.
        let parent_instance = source_contract.clone(); // Distinct from child and Factory/spec.
        let parent_reserved =
            super::super::isolated_output_contract_clone_reservation_bytes(&source_contract)
                .unwrap();
        assert!(parent_reserved > 0);
        assert_eq!(
            super::super::isolated_output_contract_allocated_bytes(&parent_instance).unwrap(),
            parent_reserved
        );
        for vector_only in [true, false] {
            let mut oversized_parent = parent_instance.clone();
            if vector_only {
                oversized_parent.buses.reserve(32);
            } else {
                oversized_parent.buses[0].bus_key.reserve(128);
            }
            assert!(
                super::super::isolated_output_contract_allocated_bytes(&oversized_parent).unwrap()
                    > parent_reserved,
                "parent metadata Vec and String spare capacities are independent owners"
            );
        }
        assert_eq!(
            super::super::isolated_output_contract_clone_reservation_bytes(
                &AudioProcessorAuxiliaryOutputContract::default()
            )
            .unwrap(),
            0
        );
        assert_eq!(
            super::super::isolated_output_contract_allocated_bytes(
                &AudioProcessorAuxiliaryOutputContract::default()
            )
            .unwrap(),
            0
        );
        for input_count in [0, 3] {
            let input = if input_count == 0 {
                Vec::new()
            } else {
                vec![vec![vec![0.0; 14]; channels]; input_count]
            };
            let mut output = vec![vec![vec![0.0; 14]; channels]; 3];
            let ip = AudioPorts::with_capacity(channels * input_count, input_count);
            let op = AudioPorts::with_capacity(channels * 3, 3);
            let reserved = clap_audio_storage_reservation(render, input_count, &outputs).unwrap();
            let actual = clap_actual_audio_storage(
                &input,
                &output,
                input.capacity() + output.capacity(),
                &ip,
                &op,
                &outputs,
            )
            .unwrap();
            assert_eq!(
                actual, reserved,
                "same requested complete capacities, including both unrouted outputs"
            );
            for vector_only in [true, false] {
                let mut oversized = outputs.clone();
                if vector_only {
                    oversized.buses.reserve(32);
                } else {
                    oversized.buses[0].bus_key.reserve(128);
                }
                assert!(
                    clap_actual_audio_storage(
                        &input,
                        &output,
                        input.capacity() + output.capacity(),
                        &ip,
                        &op,
                        &oversized
                    )
                    .unwrap()
                        > reserved,
                    "child metadata Vec and String capacities are checked independently of PCM"
                );
            }
            output[2][0].reserve(128);
            assert!(
                clap_actual_audio_storage(
                    &input,
                    &output,
                    input.capacity() + output.capacity(),
                    &ip,
                    &op,
                    &outputs
                )
                .unwrap()
                    > reserved
            );
        }
    }
}
#[test]
fn clap_output_metadata_is_revalidated_after_configuration_without_reinterpreting_routes() {
    use super::super::clap_discovery::{
        processing_result_for_notes, revalidate_processing_metadata_for_notes,
        ClapAudioPortMetadata,
    };
    let audio = ClapAudioPortMetadata {
        main_audio_input: ClapMainAudioInput::Absent,
        contract: AudioProcessorAuxiliaryInputContract::default(),
        names: Vec::new(),
        main_audio_output_id: 1,
        outputs: outputs(AudioChannelLayout::Mono),
    };
    let processing = processing_result_for_notes(
        "fixture",
        render(AudioChannelLayout::Mono),
        Vec::new(),
        &audio,
        0,
        TailLength::Finite(0),
    )
    .unwrap();
    revalidate_processing_metadata_for_notes(&processing, &[], &audio).unwrap();
    for revision in 0..4 {
        let mut changed = ClapAudioPortMetadata {
            main_audio_input: audio.main_audio_input,
            contract: audio.contract.clone(),
            names: Vec::new(),
            main_audio_output_id: audio.main_audio_output_id,
            outputs: audio.outputs.clone(),
        };
        match revision {
            0 => changed.main_audio_output_id = 7,
            1 => changed.outputs.buses.reverse(),
            2 => {
                changed.outputs.buses.pop();
            }
            _ => changed.outputs.buses[0].channel_layout = AudioChannelLayout::Stereo,
        }
        assert!(matches!(
            revalidate_processing_metadata_for_notes(&processing, &[], &changed),
            Err(AudioProcessorHostError::Unavailable(_))
        ));
    }
}

#[test]
fn native_output_seed_keeps_concrete_identity_and_vst3_refuses_unimplemented_auxiliary_outputs() {
    let output_contract = outputs(AudioChannelLayout::Mono);
    let payload = ClapPayload {
        main_audio_input: ClapMainAudioInput::Absent,
        main_audio_output_id: 1,
        auxiliary_outputs: super::super::protocol::outputs_to_wire(&output_contract),
        library_path: PathBuf::from("explicit-test-library"),
        plugin_id: "owned".into(),
        binary_sha256: [7; 32],
        state: Some(vec![1]),
        parameters: Vec::new(),
        parameter_ids: Vec::new(),
    };
    let rebound =
        payload_with_configuration_seed(&serde_json::to_vec(&payload).unwrap(), &[2, 3]).unwrap();
    let actual: ClapPayload = serde_json::from_slice(&rebound).unwrap();
    assert_eq!(actual.main_audio_output_id, 1);
    assert_eq!(
        super::super::protocol::outputs_from_wire(actual.auxiliary_outputs),
        output_contract
    );
    assert_eq!(actual.state, Some(vec![2, 3]));
    let request = IsolatedAudioProcessorWorkerPrepareRequest {
        musical_time_reference: None,
        native_lane: None,
        payload: Vec::new(),
        plugin_execution_contract: AudioProcessorExecutionContract::stateless(),
        auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
        auxiliary_outputs: output_contract,
        note_input_contract: crate::AudioProcessorNoteInputContract::default(),
        parameter_ids: Vec::new(),
        render_contract: render(AudioChannelLayout::Mono),
    };
    assert!(
        matches!(super::super::Vst3AudioProcessorWorkerFactory.prepare(request),
        Err(AudioProcessorHostError::Unavailable(detail)) if detail=="VST3 auxiliary outputs are not supported by this adapter")
    );
}

#[test]
fn clap_output_process_validates_every_port_before_publication_and_reads_complete_constant_buffers()
{
    for (entry, succeeds) in [
        (super::tests::output_metadata_entry::<19>(), false),
        (super::tests::output_metadata_entry::<20>(), false),
        (super::tests::output_metadata_entry::<21>(), true),
    ] {
        let render = crate::AudioRenderContract {
            max_block_frames: 4,
            ..render(AudioChannelLayout::Stereo)
        };
        let probe = super::super::clap_discovery::probe_loaded(
            &entry,
            "org.mondrian.test.sidechain",
            render,
            None,
            false,
        )
        .unwrap();
        let inputs = super::super::protocol::auxiliary_from_wire(probe.auxiliary_buses);
        let outputs = super::super::protocol::outputs_from_wire(probe.auxiliary_outputs);
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
            musical_time_reference: None,
            native_lane: clap_native_worker_lane_profile().unwrap(),
            payload: Vec::new(),
            plugin_execution_contract: AudioProcessorExecutionContract::new(
                probe.latency_frames as usize,
                crate::AudioProcessorTail::None,
                true,
                false,
                true,
                0,
            )
            .unwrap(),
            auxiliary_inputs: inputs.clone(),
            auxiliary_outputs: outputs.clone(),
            note_input_contract: crate::AudioProcessorNoteInputContract::default(),
            parameter_ids: Vec::new(),
            render_contract: render,
        };
        let mut worker = prepare_loaded(
            request,
            entry,
            c"org.mondrian.test.sidechain",
            None,
            &[],
            &[],
        )
        .unwrap();
        worker.enter_state(0).unwrap();
        let mut main = [99.0; 4];
        let mut output = [99.0; 8];
        let input = [0.0; 8];
        let result = worker.process(
            IsolatedAudioProcessorWorkerBlock::new(
                0,
                2,
                render,
                &mut main,
                &inputs,
                &input,
                &[],
                &[],
            )
            .with_auxiliary_outputs(&outputs, &mut output),
        );
        if succeeds {
            result.unwrap();
            assert_eq!(main, [0.25; 4]);
            assert_eq!(&output[..4], [0.5; 4]);
            assert_eq!(&output[4..], [0.75; 4]);
        } else {
            assert!(result.is_err());
            assert_eq!(main, [99.0; 4]);
            assert_eq!(
                output, [99.0; 8],
                "late auxiliary NaN/process error never publishes earlier valid ports"
            );
        }
    }
}

#[cfg(windows)]
mod installed {
    use super::super::super::native_worker_processing::with_processing_scope;
    use super::super::super::protocol::{
        CommandKind, SharedBlockLayout, SharedNoteEvent, WorkerCommand,
    };
    use super::super::super::worker::WorkerProcessContext;
    use super::*;
    use crate::{
        AudioContinuityEpoch, AudioMediaResolver, AudioProgramRuntime, AudioRenderRequest,
        AudioRuntimeResourceGrant, AudioStateEntry, ResolvedAudioSource,
    };
    use clack_extensions::params::PluginParams;
    use mondrian_core::{
        AssetId, AudioNoteId, AudioNoteLaneId, AudioOwnerInstallationSource,
        AudioSourceComponentId, AuthoringList, ExecutionCancellationToken, TimelineTime,
    };
    use mondrian_timeline::audio::{AudioNote, AudioNoteLane, AudioNoteTarget};
    use mondrian_timeline::{
        AudioChannelStripOutputPort, AudioMixBus, AudioProcessorOutputPort, AudioRoute,
        AudioRouteDestination, AudioRouteSource, Sequence,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    const ID: &str = "org.mondrian.fixture.note-gate.zero-input.three-outputs";
    fn dll() -> PathBuf {
        let path = PathBuf::from(
            std::env::var_os("MONDRIAN_THREE_OUTPUT_NOTE_GATE_DLL")
                .expect("explicit separately produced three-output fixture DLL"),
        );
        let expected = std::env::var("MONDRIAN_THREE_OUTPUT_NOTE_GATE_DLL_SHA256")
            .expect("explicit retained fixture SHA256");
        assert!(
            path.is_absolute()
                && expected.len() == 64
                && expected.bytes().all(|b| b.is_ascii_hexdigit())
        );
        let actual = fingerprint_clap_binary(&path).unwrap();
        assert_eq!(
            actual.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            expected.to_ascii_lowercase()
        );
        path
    }
    fn state() -> Vec<u8> {
        let mut value = vec![0; 16];
        value[..4].copy_from_slice(b"MNG1");
        value[4..12].copy_from_slice(&0.5_f64.to_le_bytes());
        value
    }
    fn counter(worker: &mut ClapWorker) -> f64 {
        assert!(worker.shared.is_main_thread());
        assert!(!worker.shared.is_audio_thread());
        let handle = worker.instance.plugin_handle();
        handle
            .get_extension::<PluginParams>()
            .expect("real standard Params")
            .get_value(&handle, ClapId::new(29))
            .expect("same actual readonly native get_value")
    }
    fn direct(render: crate::AudioRenderContract) -> Box<ClapWorker> {
        let path = dll();
        // SAFETY: the external gate pins this explicitly selected owned DLL.
        let entry = unsafe { PluginEntry::load(&path) }.unwrap();
        let state = state();
        let offer = super::super::super::clap_note_input_contract::probe_loaded_processing_v5(
            &entry,
            ID,
            fingerprint_clap_binary(&path).unwrap(),
            Some(&state),
            render,
        )
        .unwrap();
        assert_eq!(offer.main_audio_input(), ClapMainAudioInput::Absent);
        assert_eq!(offer.main_audio_output_id(), 1);
        assert_eq!(offer.auxiliary_outputs(), outputs(AudioChannelLayout::Mono));
        let observed = offer.parameters().iter().find(|p| p.id == 29).unwrap();
        assert!(observed.read_only && !observed.automatable);
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
            musical_time_reference: None,
            native_lane: clap_native_worker_lane_profile().unwrap(),
            payload: Vec::new(),
            plugin_execution_contract: AudioProcessorExecutionContract::new(
                offer.latency_frames() as usize,
                offer.tail(),
                true,
                false,
                true,
                0,
            )
            .unwrap(),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            auxiliary_outputs: offer.auxiliary_outputs(),
            note_input_contract: offer.notes.contract.clone(),
            parameter_ids: vec![
                mondrian_core::ParameterId::new_static("clap.param.17"),
                mondrian_core::ParameterId::new_static("clap.param.19"),
            ],
            render_contract: render,
        };
        let native_main =
            std::rc::Rc::new(vst3_host::window::WindowsEditorMainThread::initialize().unwrap());
        prepare_loaded_on_main(
            native_main,
            request,
            entry,
            std::ffi::CString::new(ID).unwrap().as_c_str(),
            Some(&state),
            offer.main_audio_input(),
            offer.main_audio_output_id(),
            offer.parameters(),
            &[17, 18, 20],
        )
        .unwrap()
    }
    fn process_scoped(
        worker: &mut ClapWorker,
        render: crate::AudioRenderContract,
        start: i64,
        frames: usize,
        input: &[SharedNoteEvent],
    ) -> (Vec<f32>, Vec<f32>) {
        let notes = worker.note_input_contract();
        let outputs = worker.auxiliary_output_contract();
        let inputs = worker.auxiliary_input_contract();
        let ids = [
            mondrian_core::ParameterId::new_static("clap.param.17"),
            mondrian_core::ParameterId::new_static("clap.param.19"),
        ];
        let layout = SharedBlockLayout::new(
            render.max_block_frames,
            render.channel_count(),
            inputs.buses.len(),
            ids.len(),
        )
        .unwrap()
        .with_auxiliary_outputs(outputs.buses.len())
        .unwrap()
        .with_notes(notes.limits.input_events, notes.limits.output_ends)
        .unwrap();
        // Typed backing guarantees the protocol's maximum eight-byte alignment;
        // all allocation and encoding precede the original DSP scope.
        let mut storage = vec![0_u64; layout.total_bytes.div_ceil(size_of::<u64>())];
        let shared = &mut bytemuck::cast_slice_mut::<u64, u8>(&mut storage)[..layout.total_bytes];
        let input_bytes = bytemuck::cast_slice(input);
        shared[layout.note_events_offset..layout.note_events_offset + input_bytes.len()]
            .copy_from_slice(input_bytes);
        let command = WorkerCommand {
            kind: CommandKind::Process,
            start_sample: start,
            frames: frames.try_into().unwrap(),
            event_count: 0,
            note_count: input.len().try_into().unwrap(),
        };
        let profile = worker.native_worker_lane_resources().expect("real native lane");
        let split = worker.split_native_processing().unwrap();
        let completion = with_processing_scope(
            split,
            profile,
            shared,
            WorkerProcessContext {
                musical_time_reference: None,
                layout,
                auxiliary_inputs: &inputs,
                auxiliary_outputs: &outputs,
                note_input_contract: &notes,
                parameter_ids: &ids,
                render_contract: render,
            },
            |main, dsp| {
                assert!(
                    dsp.scheduling_entry().is_none(),
                    "Offline has no RT scheduling lease"
                );
                dsp.submit(command)?;
                dsp.complete(main)
            },
        )
        .unwrap();
        assert!(completion.scheduling_exit.is_none());
        // The original Context rejects any refused Note output before returning
        // this exact count. The scope has joined DSP and finished main here.
        assert_eq!(completion.value.note_end_count, Some(0));
        let bytes = frames * render.channel_count() * size_of::<f32>();
        let main = bytemuck::cast_slice::<u8, f32>(
            &shared[layout.main_offset..layout.main_offset + bytes],
        )
        .to_vec();
        let aux = bytemuck::cast_slice::<u8, f32>(
            &shared[layout.auxiliary_outputs_offset
                ..layout.auxiliary_outputs_offset + bytes * outputs.buses.len()],
        )
        .to_vec();
        (main, aux)
    }

    #[test]
    #[ignore = "requires pinned actual three-output owned DLL; scoped DSP then same-object main-role get_value control"]
    fn installed_three_output_clap_same_native_object_counts_one_sdk_process_per_block_and_resets()
    {
        for partitions in [&[14usize][..], &[3, 2, 4, 5][..], &[1usize; 14][..]] {
            let render = render(AudioChannelLayout::Mono);
            let mut worker = direct(render);
            assert_eq!(counter(&mut worker), 0.0);
            for epoch in 0..2 {
                worker.enter_state(0).unwrap();
                assert_eq!(counter(&mut worker), 0.0);
                let mut start = 0;
                for (index, &frames) in partitions.iter().enumerate() {
                    let before = counter(&mut worker);
                    let input = if start == 0 {
                        vec![SharedNoteEvent {
                            sample_offset: 0,
                            native_id: 71,
                            port_index: 0,
                            channel: 0,
                            key: 60,
                            kind: 1,
                            velocity: 0.0,
                        }]
                    } else {
                        Vec::new()
                    };
                    let (main, aux) =
                        process_scoped(&mut worker, render, start as i64, frames, &input);
                    assert_eq!(main, vec![1.0 / 64.0; frames]);
                    assert_eq!(&aux[..frames], vec![-1.0 / 128.0; frames]);
                    assert_eq!(&aux[frames..], vec![1.0 / 256.0; frames]);
                    assert_eq!(counter(&mut worker), before + 1.0);
                    assert_eq!(counter(&mut worker), (index + 1) as f64);
                    start += frames;
                }
                assert_eq!(start, 14);
                println!(
                    "{}",
                    serde_json::json!({"case":"direct-three-output-owned-sdk-counter","epoch":epoch,"partitions":partitions,"process_callbacks":counter(&mut worker),"counter_origin":"same native instance standard PluginParams.get_value on main role after DSP scope join and main finish","canonical_runtime_counter_observed":false})
                );
            }
            drop(worker);
            dll();
        }
    }
    struct NoMedia(Arc<AtomicUsize>);
    impl AudioMediaResolver for NoMedia {
        fn resolve(
            &self,
            _: AssetId,
            _: AudioSourceComponentId,
            _: u32,
        ) -> Result<ResolvedAudioSource, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err("three-output zero-input case must not resolve media".into())
        }
    }
    #[derive(Default)]
    struct Cursors {
        sessions: Vec<crate::AudioSessionCursorEvidence>,
        processors: Vec<crate::AudioProcessorCursorEvidence>,
    }
    impl crate::AudioContinuityEvidenceVisitor for Cursors {
        fn session(
            &mut self,
            evidence: crate::AudioSessionCursorEvidence,
        ) -> Result<(), crate::AudioExecutionError> {
            self.sessions.push(evidence);
            Ok(())
        }
        fn processor(
            &mut self,
            evidence: crate::AudioProcessorCursorEvidence,
        ) -> Result<(), crate::AudioExecutionError> {
            self.processors.push(evidence);
            Ok(())
        }
    }
    fn owner(runtime: &AudioProgramRuntime) -> crate::AudioNativeProcessorOwnerLease {
        let mut cursors = Cursors::default();
        runtime.visit_continuity_evidence(&mut cursors).unwrap();
        assert_eq!((cursors.sessions.len(), cursors.processors.len()), (1, 1));
        let processor = cursors.processors[0];
        let epoch = match processor.state {
            crate::AudioProcessorCursorState::Unentered => None,
            crate::AudioProcessorCursorState::Active { epoch, .. } => Some(epoch),
            _ => panic!("expected retained native owner"),
        };
        runtime
            .native_owner_lease(crate::AudioNativeProcessorAddress {
                root: cursors.sessions[0].identity,
                local: processor.identity,
                occurrence: processor.occurrence,
                epoch,
            })
            .unwrap()
    }
    fn time(sample: i64, rate: u32) -> TimelineTime {
        TimelineTime::new(sample, i64::from(rate)).unwrap()
    }
    fn source(
        id: mondrian_core::AudioProcessorInstanceId,
        port: AudioProcessorOutputPort,
    ) -> AudioRouteSource {
        AudioRouteSource::ProcessorOutput { processor_id: id, port }
    }
    fn authored(
        catalog: &DiscoveredClapAudioProcessorSpecResolver,
        render: crate::AudioRenderContract,
        case: u8,
    ) -> Sequence {
        let instance = catalog.create_instance(ID, render, Some(state())).unwrap();
        let id = instance.id;
        assert!(
            !instance
                .parameters
                .contains_key(&mondrian_core::ParameterId::new_static("clap.param.29")),
            "readonly diagnostic never becomes authored automation"
        );
        let mut sequence = Sequence::new("owned native three-output canonical graph");
        sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
        let track = sequence.audio_tracks[0].id;
        sequence
            .audio_program
            .track_channels
            .get_mut(&track)
            .unwrap()
            .strip
            .pre_fader
            .processors
            .push(instance);
        sequence.audio_program.note_lanes.push(AudioNoteLane {
            schema_version: 1,
            id: AudioNoteLaneId::new(),
            target: AudioNoteTarget {
                track_id: track,
                processor_id: id,
                input_port_key: "clap.note-input.0".into(),
            },
            notes: AuthoringList::from(vec![AudioNote {
                id: AudioNoteId::new(),
                start: time(0, render.sample_rate),
                end: time(16, render.sample_rate),
                channel: 0,
                key: 60,
                on_velocity: 0.0,
                off_velocity: 0.0,
            }]),
        });
        let output = sequence.audio_program.outputs[0].id;
        sequence.audio_program.routes.clear();
        let port = match case {
            0 => AudioProcessorOutputPort::Main,
            1 => AudioProcessorOutputPort::Auxiliary { bus_key: "clap.audio-output.2".into() },
            _ => AudioProcessorOutputPort::Auxiliary { bus_key: "clap.audio-output.3".into() },
        };
        sequence.audio_program.routes.push(AudioRoute::new(
            source(id, port),
            AudioRouteDestination::Output(output),
        ));
        if case == 3 {
            for name in ["fanout A", "fanout B"] {
                let bus = mondrian_core::MixBusId::new();
                sequence.audio_program.buses.push(AudioMixBus {
                    id: bus,
                    name: name.into(),
                    strip: Default::default(),
                });
                sequence.audio_program.routes.push(AudioRoute::new(
                    source(
                        id,
                        AudioProcessorOutputPort::Auxiliary {
                            bus_key: "clap.audio-output.2".into(),
                        },
                    ),
                    AudioRouteDestination::Bus(bus),
                ));
                sequence.audio_program.routes.push(AudioRoute::new(
                    AudioRouteSource::Bus {
                        bus_id: bus,
                        port: AudioChannelStripOutputPort::PostMute,
                    },
                    AudioRouteDestination::Output(output),
                ));
            }
        }
        let bytes = serde_json::to_vec(&sequence).unwrap();
        let reopened: Sequence = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            serde_json::to_value(&sequence).unwrap(),
            serde_json::to_value(&reopened).unwrap()
        );
        reopened
    }
    #[test]
    #[ignore = "requires current retained normal Product helper and pinned three-output owned DLL; canonical Runtime/DAG PCM"]
    fn installed_three_output_clap_canonical_routes_each_port_fanout_and_state_reentry_without_media(
    ) {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, WaitForSingleObject,
        };
        let helper = PathBuf::from(std::env::var_os("MONDRIAN_CLAP_TEST_HELPER").unwrap());
        assert!(helper.is_absolute() && helper.is_file());
        let catalog =
            Arc::new(DiscoveredClapAudioProcessorSpecResolver::discover(helper, [dll()]).unwrap());
        let processors = super::super::super::IsolatedAudioProcessorResolver::new(catalog.clone());
        for rate in [44_100, 48_000] {
            for case in 0..4 {
                for partitions in [&[14usize][..], &[3, 2, 4, 5][..]] {
                    let render = crate::AudioRenderContract {
                        sample_rate: rate,
                        ..render(AudioChannelLayout::Mono)
                    };
                    let sequence = authored(&catalog, render, case);
                    let resolves = Arc::new(AtomicUsize::new(0));
                    let mut runtime =
                        AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
                            &sequence,
                            &[],
                            &NoMedia(resolves.clone()),
                            &processors,
                            render,
                            None,
                            AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, usize::MAX),
                        )
                        .unwrap();
                    let source = AudioOwnerInstallationSource::new();
                    let (registration, verifier) = source.register().unwrap();
                    runtime.bind_native_owner_installation(verifier).unwrap();
                    let claim = source
                        .reserve_install(&registration, None)
                        .and_then(|r| source.begin_commit(r))
                        .unwrap()
                        .commit()
                        .unwrap();
                    runtime
                        .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
                        .unwrap();
                    let cold = runtime.observe_native_owner_object(owner(&runtime)).unwrap();
                    let pid = cold.process_id().get();
                    assert_ne!(pid, std::process::id());
                    // SAFETY: the authenticated same Runtime object supplies PID;
                    // retain synchronize/query rights only through its exact Drop.
                    let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
                    assert!(!raw.is_null());
                    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
                    assert_eq!(
                        unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
                        258
                    );
                    let expected = match case {
                        0 => 1.0_f32 / 64.0,
                        1 => -1.0 / 128.0,
                        2 => 1.0 / 256.0,
                        _ => -3.0 / 256.0,
                    };
                    for epoch in 1..=2 {
                        runtime
                            .enter_state(AudioStateEntry {
                                epoch: AudioContinuityEpoch::new(epoch),
                                start_sample: 0,
                            })
                            .unwrap();
                        let lease = owner(&runtime);
                        assert_eq!(lease.editor_target().continuity_epoch, Some(epoch));
                        assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), cold);
                        let mut start = 0;
                        for &frames in partitions {
                            let mut pcm = vec![99.0; frames];
                            runtime
                                .render_into(
                                    AudioRenderRequest { start_sample: start as i64, frames },
                                    &mut pcm,
                                )
                                .unwrap();
                            assert_eq!(
                                pcm,
                                vec![expected; frames],
                                "rate={rate} case={case} epoch={epoch}"
                            );
                            start += frames;
                        }
                        assert_eq!(start, 14);
                        assert_eq!(resolves.load(Ordering::SeqCst), 0);
                        assert_eq!(runtime.resource_footprint().media_window_bytes, 0);
                    }
                    runtime
                        .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                        .unwrap();
                    let alive = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
                    assert_eq!(alive, 258);
                    drop(runtime);
                    let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
                    let mut exit = 0;
                    let queried =
                        unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) };
                    println!(
                        "{}",
                        serde_json::json!({"case":"canonical-three-output-owned-runtime","route_case":case,"rate":rate,"partitions":partitions,"epochs":2,"media_resolves":resolves.load(Ordering::SeqCst),"runtime_process_callback_count_observed":false,"direct_counter_control_is_separate":true,"unconnected_native_outputs_still_prepared":true,"grant_is_explicit_unbounded":true,"native_pid":pid,"same_native_object_across_epochs":true,"alive_before_drop":alive,"post_drop_wait_status":settled,"exit_query_succeeded":queried!=0,"exit_code":exit,"third_party_multiout_or_rt_qualified":false})
                    );
                    assert_eq!(settled, 0);
                    assert_ne!(queried, 0);
                    assert_eq!(exit, 0);
                    dll();
                }
            }
        }
    }
}
