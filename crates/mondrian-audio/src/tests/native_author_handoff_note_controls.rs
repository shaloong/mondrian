//! Candidate child of tests/native_author_handoff.rs; owned model, not a DLL.
//! Requires the separately reviewed cfg(test) seams. No production gate change.
use super::*;
use mondrian_core::{AudioNoteId, AudioNoteLaneId, AuthoringList};
use mondrian_timeline::audio::{AudioNote, AudioNoteLane, AudioNoteTarget};

#[derive(Default)]
struct NoteCalls {
    creates: usize,
    binds: usize,
    processes: usize,
    notes: [Option<AudioNoteEvent>; 8],
    note_count: usize,
    ends: usize,
}
struct NoteModelResolver {
    probe: Arc<Mutex<HandoffProbe>>,
    calls: Arc<Mutex<NoteCalls>>,
    completion: AudioNoteCompletionPolicy,
    off_result: NoteModelReturn,
}
struct NoteModelFactory {
    inner: HandoffFactory,
    calls: Arc<Mutex<NoteCalls>>,
    completion: AudioNoteCompletionPolicy,
    off_result: NoteModelReturn,
}
struct NoteModelProcessor {
    inner: HandoffProcessor,
    calls: Arc<Mutex<NoteCalls>>,
    completion: AudioNoteCompletionPolicy,
    off_result: NoteModelReturn,
}

#[derive(Clone, Copy)]
enum NoteModelReturn {
    Success,
    Failed,
    Deferred,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct CursorObservation {
    sessions: [Option<AudioSessionCursorEvidence>; 2],
    processors: [Option<AudioProcessorCursorEvidence>; 2],
    session_count: usize,
    processor_count: usize,
}
impl AudioContinuityEvidenceVisitor for CursorObservation {
    fn session(&mut self, evidence: AudioSessionCursorEvidence) -> Result<(), AudioExecutionError> {
        assert!(self.session_count < self.sessions.len());
        self.sessions[self.session_count] = Some(evidence);
        self.session_count += 1;
        Ok(())
    }
    fn processor(
        &mut self,
        evidence: AudioProcessorCursorEvidence,
    ) -> Result<(), AudioExecutionError> {
        assert!(self.processor_count < self.processors.len());
        self.processors[self.processor_count] = Some(evidence);
        self.processor_count += 1;
        Ok(())
    }
}
fn cursors(runtime: &AudioProgramRuntime) -> CursorObservation {
    let mut observed = CursorObservation::default();
    runtime.visit_continuity_evidence(&mut observed).unwrap();
    observed
}

fn offered_model_notes() -> AudioProcessorNoteInputContract {
    AudioProcessorNoteInputContract {
        completion: crate::AudioNoteCompletionPolicy::NativeEnd,
        ports: vec![AudioProcessorNoteInputPort {
            channel_count: 16,
            port_key: "clap.note-input.0".to_owned(),
            native_id: 0,
            native_index: 0,
        }],
        limits: AudioNoteExecutionLimits {
            input_events: 2,
            output_ends: 2,
            retained_records: 2,
            live_voices: 2,
        },
    }
}
impl AudioProcessorResolver for NoteModelResolver {
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        if matches!(request.definition(), mondrian_timeline::AudioProcessorDefinitionRef::Clap { plugin_id, .. }
            if plugin_id == "test.mondrian.handoff")
            || matches!(request.definition(), mondrian_timeline::AudioProcessorDefinitionRef::Vst3 { class_id, .. }
                if class_id == "portable.actual.rescan")
        {
            Ok(Arc::new(NoteModelFactory {
                inner: HandoffFactory { probe: Arc::clone(&self.probe), seed: None },
                calls: Arc::clone(&self.calls),
                completion: self.completion,
                off_result: self.off_result,
            }))
        } else {
            BuiltInAudioProcessorResolver.prepare(request)
        }
    }
}
impl AudioProcessorFactory for NoteModelFactory {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        self.inner.execution_contract()
    }
    fn note_input_contract(&self) -> AudioProcessorNoteInputContract {
        let mut contract = offered_model_notes();
        if self.inner.probe.lock().unwrap().vst_rescan_abi {
            contract.ports[0].port_key = "vst3.note-input.0".to_owned();
        }
        contract.completion = self.completion;
        if self.completion == AudioNoteCompletionPolicy::SuccessfulOffBlock {
            contract.limits.output_ends = 0;
            contract.limits.live_voices = 0;
            contract.limits.retained_records = 1;
        }
        contract
    }
    fn visit_host_owned_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(
            AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), AudioProcessorFactoryStorageError>,
    ) -> Result<(), AudioProcessorFactoryStorageError> {
        self.inner.visit_host_owned_storage(visitor)?;
        visitor(AudioProcessorFactoryStorageBorrow::arc_payload(&self.calls))
    }
    fn with_native_configuration_seed(
        &self,
        format: NativeConfigurationFormat,
        configuration: &[u8],
    ) -> Result<Option<Arc<dyn AudioProcessorFactory>>, AudioProcessorHostError> {
        // Same bounded owned scalar ABI as the original model factory. The
        // cold seed remains a future configuration, never loaded into this DSP.
        assert_eq!(
            format,
            if self.inner.probe.lock().unwrap().vst_rescan_abi {
                NativeConfigurationFormat::Vst3ComponentAndController
            } else {
                NativeConfigurationFormat::ClapState
            }
        );
        assert_eq!(configuration.len(), 8);
        Ok(Some(Arc::new(Self {
            inner: HandoffFactory {
                probe: Arc::clone(&self.inner.probe),
                seed: Some(configuration.to_vec()),
            },
            calls: Arc::clone(&self.calls),
            completion: self.completion,
            off_result: self.off_result,
        })))
    }
    fn create(&self) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        self.calls.lock().unwrap().creates += 1;
        Ok(Box::new(NoteModelProcessor {
            inner: HandoffProcessor {
                probe: Arc::clone(&self.inner.probe),
                object_serial: NonZeroU64::new(1).unwrap(),
            },
            calls: Arc::clone(&self.calls),
            completion: self.completion,
            off_result: self.off_result,
        }))
    }
}
impl AudioProcessor for NoteModelProcessor {
    fn native_observations_supported(&self) -> bool {
        self.inner.native_observations_supported()
    }
    fn native_audio_object_observation(
        &mut self,
    ) -> Result<Option<NativeAudioObjectObservation>, AudioProcessorHostError> {
        self.inner.native_audio_object_observation()
    }
    fn poll_native_observations(
        &mut self,
        consumption: NativeAudioObservationConsumption,
    ) -> Result<NativeAudioObservations, AudioProcessorHostError> {
        self.inner.poll_native_observations(consumption)
    }
    fn bind_native_audio_object(
        &mut self,
        request: NativeAudioObjectBindRequest,
    ) -> Result<NativeAudioObjectBindOutcome, AudioProcessorHostError> {
        self.calls.lock().unwrap().binds += 1;
        self.inner.bind_native_audio_object(request)
    }
    fn enter_state(&mut self, start: i64) -> Result<(), AudioProcessorHostError> {
        self.inner.enter_state(start)
    }
    fn process(
        &mut self,
        context: AudioProcessorProcessContext,
        audio: &mut dyn AudioProcessorAudioIo,
        parameters: AudioParameterEventBatch<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        let mut off = [None; 2];
        let mut off_count = 0;
        {
            let mut calls = self.calls.lock().unwrap();
            calls.processes += 1;
            for &event in audio.note_events() {
                assert!(
                    calls.note_count < calls.notes.len(),
                    "owned observation capacity"
                );
                let index = calls.note_count;
                calls.notes[index] = Some(event);
                calls.note_count += 1;
                if event.kind == AudioNoteEventKind::Off {
                    assert!(off_count < off.len());
                    off[off_count] = Some(event);
                    off_count += 1;
                }
            }
        }
        if off_count != 0 {
            match self.off_result {
                NoteModelReturn::Success => {}
                NoteModelReturn::Failed => {
                    return Err(AudioProcessorHostError::Process(
                        "owned Off refusal".to_owned(),
                    ))
                }
                NoteModelReturn::Deferred => {
                    return Err(AudioProcessorHostError::ReprepareRequired(
                        crate::NativeObservedRequest::RequestOnly(
                            mondrian_core::NativeAudioReprepareRequest::ClapRestart,
                        ),
                    ))
                }
            }
        }
        self.inner.process(context, audio, parameters)?;
        for event in off[..off_count]
            .iter()
            .flatten()
            .filter(|_| self.completion == AudioNoteCompletionPolicy::NativeEnd)
        {
            assert!(
                audio.try_push_note_end(AudioNoteEndEvent {
                    native_note_id: event.native_note_id,
                    port_index: event.port_index as i16,
                    channel: i16::from(event.channel),
                    key: i16::from(event.key),
                    flags: 0,
                }),
                "the same actual process must retain its completion"
            );
            self.calls.lock().unwrap().ends += 1;
        }
        Ok(())
    }
}

#[test]
fn owned_installed_note_nonzero_entry_keeps_old_owner_and_original_off() {
    use mondrian_core::automation::{ParameterSchema, PropertyValue};
    let probe = Arc::new(Mutex::new(HandoffProbe::default()));
    let calls = Arc::new(Mutex::new(NoteCalls::default()));
    let mut sequence = sequence_with_audio_clip();
    let track = sequence.audio_tracks[0].id;
    let output = sequence.audio_program.outputs[0].id;
    let parameter = ParameterId::new_static("native.gain");
    let mut processor = AudioProcessorInstance {
        id: mondrian_core::AudioProcessorInstanceId::new(),
        definition: mondrian_timeline::AudioProcessorDefinitionRef::Clap {
            plugin_id: "test.mondrian.handoff".to_owned(),
            schema_version: 1,
            binary_sha256: None,
        },
        bypassed: false,
        parameters: Default::default(),
        opaque_state: None,
    };
    processor.parameters.insert(
        parameter.clone(),
        mondrian_timeline::audio::AudioProcessorParameter::from_schema(ParameterSchema::v1(
            parameter,
            PropertyValue::Double(0.5),
        ))
        .unwrap(),
    );
    let processor_id = processor.id;
    sequence
        .audio_program
        .track_channels
        .get_mut(&track)
        .unwrap()
        .strip
        .pre_fader
        .processors
        .push(processor);
    let contract = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 1,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    };
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &RampResolver,
        &NoteModelResolver {
            probe: Arc::clone(&probe),
            calls: Arc::clone(&calls),
            completion: AudioNoteCompletionPolicy::NativeEnd,
            off_result: NoteModelReturn::Success,
        },
        contract,
        None,
        AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 64 * 1024 * 1024),
    )
    .expect("scoreless cached owned factory graph");
    sequence.audio_program.note_lanes.push(AudioNoteLane {
        schema_version: 1,
        id: AudioNoteLaneId::new(),
        target: AudioNoteTarget {
            track_id: track,
            processor_id,
            input_port_key: "clap.note-input.0".to_owned(),
        },
        notes: AuthoringList::from(
            vec![(0, 1), (2, 3)]
                .into_iter()
                .map(|(start, end)| AudioNote {
                    id: AudioNoteId::new(),
                    start: tt(start, 48_000),
                    end: tt(end, 48_000),
                    channel: 4,
                    key: 60,
                    on_velocity: -0.0,
                    off_velocity: 0.5,
                })
                .collect::<Vec<_>>(),
        ),
    });
    let program = compile_audio_program(&sequence, AudioCompileRequest::program(output))
        .expect("actual author validation and selected Note IR");
    runtime
        .prepare_owned_note_model_for_test(Arc::new(program))
        .expect("test-only cached IR/footprint preparation before registration or entry");

    // Same installation protocol as HandoffFixture. None of this constructs a
    // lease or LedgerOrigin. Initial observation binding has no entered epoch.
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    let (registration, verifier) = source.register().unwrap();
    runtime.bind_native_owner_installation(verifier).unwrap();
    let claim = source
        .begin_commit(source.reserve_install(&registration, None).unwrap())
        .unwrap()
        .commit()
        .unwrap();
    runtime
        .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
        .unwrap();
    let epoch = AudioContinuityEpoch::new(47);
    runtime
        .enter_state(AudioStateEntry { epoch, start_sample: 0 })
        .expect("actual Runtime cold entry after installed Source");
    let identity = runtime.resource_root_identity();
    let address = AudioNativeProcessorAddress {
        root: identity,
        local: identity,
        epoch: Some(epoch),
        occurrence: AudioProcessorOccurrence {
            instance_id: processor_id,
            owner: AudioProcessorOccurrenceOwner::Track(track),
            insertion: AudioProcessorInsertionPoint::PreFader,
        },
    };
    let lease = runtime.native_owner_lease(address).expect("actual entered installed lease");
    let mut pcm = [91.0];
    runtime
        .render_into(AudioRenderRequest { start_sample: 0, frames: 1 }, &mut pcm)
        .unwrap();
    assert_eq!(pcm, [0.5]);
    let on = calls.lock().unwrap().notes[0].unwrap();
    assert_eq!(on.kind, AudioNoteEventKind::On);
    assert_eq!(on.native_note_id, 0);
    assert_eq!(on.velocity.to_bits(), (-0.0_f64).to_bits());
    let before_calls = {
        let c = calls.lock().unwrap();
        (c.binds, c.processes, c.note_count, c.ends)
    };
    let before_entries = probe.lock().unwrap().entries;
    let before_binding = probe.lock().unwrap().binding;
    let before_cursors = cursors(&runtime);
    let before_claim = source.current_claim().unwrap().unwrap();
    assert!(matches!(
        runtime.enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(48),
            start_sample: 1,
        }),
        Err(AudioExecutionError::ProcessorHost(
            AudioProcessorHostError::NoteExecutionRefused(
                "Note score cold entry requires public origin zero; seek replay is unsupported"
            )
        ))
    ));
    assert_eq!(runtime.native_owner_lease(address).unwrap(), lease);
    assert_eq!(runtime.resource_root_identity(), identity);
    assert_eq!(cursors(&runtime), before_cursors);
    assert_eq!(
        source.current_claim().unwrap().unwrap().generation(),
        before_claim.generation()
    );
    runtime
        .native_owner_installation_verifier()
        .unwrap()
        .validate(&before_claim)
        .unwrap();
    runtime
        .validate_native_owner(lease)
        .expect("actual old installed binding still agrees");
    assert_eq!(probe.lock().unwrap().entries, before_entries);
    assert_eq!(probe.lock().unwrap().binding, before_binding);
    {
        let c = calls.lock().unwrap();
        assert_eq!((c.binds, c.processes, c.note_count, c.ends), before_calls);
    }

    runtime
        .render_into(AudioRenderRequest { start_sample: 1, frames: 1 }, &mut pcm)
        .expect("the old owner remains able to deliver its original Off");
    assert_eq!(pcm, [1.0]);
    runtime
        .render_into(AudioRenderRequest { start_sample: 2, frames: 1 }, &mut pcm)
        .unwrap();
    assert_eq!(pcm, [1.5]);
    let c = calls.lock().unwrap();
    let off = c.notes[1].unwrap();
    let next = c.notes[2].unwrap();
    assert_eq!(off.kind, AudioNoteEventKind::Off);
    assert_eq!(
        (off.native_note_id, off.port_index, off.channel, off.key),
        (on.native_note_id, on.port_index, on.channel, on.key)
    );
    assert_eq!(next.kind, AudioNoteEventKind::On);
    assert_eq!(
        next.native_note_id, 1,
        "rejection must not reset or replay the consumed namespace"
    );
    assert_eq!(c.ends, 1);
    assert_eq!(c.processes, before_calls.1 + 2);
    assert_eq!(c.binds, before_calls.0);
    assert_eq!(probe.lock().unwrap().entries, 1);
    assert_eq!(runtime.native_owner_lease(address).unwrap(), lease);
}

fn installed_off_model(
    result: NoteModelReturn,
) -> (
    AudioProgramRuntime,
    mondrian_core::AudioOwnerInstallationSource,
    AudioNativeProcessorAddress,
    Arc<Mutex<NoteCalls>>,
) {
    let (fixture, address, calls) = installed_note_fixture(result, false);
    (fixture.runtime, fixture.source, address, calls)
}

fn installed_note_fixture(
    result: NoteModelReturn,
    vst_rescan: bool,
) -> (
    HandoffFixture,
    AudioNativeProcessorAddress,
    Arc<Mutex<NoteCalls>>,
) {
    use mondrian_core::automation::{ParameterSchema, PropertyValue};
    let calls = Arc::new(Mutex::new(NoteCalls::default()));
    let mut sequence = sequence_with_audio_clip();
    let probe = Arc::new(Mutex::new(HandoffProbe {
        vst_rescan_abi: vst_rescan,
        ..Default::default()
    }));
    if vst_rescan {
        sequence.audio_program.musical_time_reference =
            Some(mondrian_core::MusicalTimeReference::new(tt(0, 1), tt(1, 1)).unwrap());
    }
    let track = sequence.audio_tracks[0].id;
    let parameter = ParameterId::new_static("native.gain");
    let mut processor = AudioProcessorInstance {
        id: mondrian_core::AudioProcessorInstanceId::new(),
        definition: if vst_rescan {
            mondrian_timeline::AudioProcessorDefinitionRef::Vst3 {
                class_id: "portable.actual.rescan".to_owned(),
                vendor: None,
                schema_version: 1,
                binary_sha256: None,
            }
        } else {
            mondrian_timeline::AudioProcessorDefinitionRef::Clap {
                plugin_id: "test.mondrian.handoff".to_owned(),
                schema_version: 1,
                binary_sha256: None,
            }
        },
        bypassed: false,
        parameters: Default::default(),
        opaque_state: None,
    };
    processor.parameters.insert(
        parameter.clone(),
        mondrian_timeline::audio::AudioProcessorParameter::from_schema(ParameterSchema::v1(
            parameter,
            PropertyValue::Double(0.5),
        ))
        .unwrap(),
    );
    let processor_id = processor.id;
    sequence
        .audio_program
        .track_channels
        .get_mut(&track)
        .unwrap()
        .strip
        .pre_fader
        .processors
        .push(processor);
    let contract = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 1,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    };
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &RampResolver,
        &NoteModelResolver {
            probe: Arc::clone(&probe),
            calls: Arc::clone(&calls),
            completion: AudioNoteCompletionPolicy::SuccessfulOffBlock,
            off_result: result,
        },
        contract,
        None,
        AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 64 * 1024 * 1024),
    )
    .unwrap();
    sequence.audio_program.note_lanes.push(AudioNoteLane {
        schema_version: 1,
        id: AudioNoteLaneId::new(),
        target: AudioNoteTarget {
            track_id: track,
            processor_id,
            input_port_key: if vst_rescan {
                "vst3.note-input.0"
            } else {
                "clap.note-input.0"
            }
            .to_owned(),
        },
        notes: AuthoringList::from(
            (0..3)
                .map(|n| AudioNote {
                    id: AudioNoteId::new(),
                    start: tt(n * 2, 48_000),
                    end: tt(n * 2 + 1, 48_000),
                    channel: 4,
                    key: 60,
                    on_velocity: -0.0,
                    off_velocity: 0.5,
                })
                .collect::<Vec<_>>(),
        ),
    });
    let program = compile_audio_program(
        &sequence,
        AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
    )
    .unwrap();
    runtime.prepare_owned_note_model_for_test(Arc::new(program)).unwrap();
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    let (registration, verifier) = source.register().unwrap();
    runtime.bind_native_owner_installation(verifier).unwrap();
    let claim = source
        .begin_commit(source.reserve_install(&registration, None).unwrap())
        .unwrap()
        .commit()
        .unwrap();
    runtime
        .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
        .unwrap();
    let epoch = AudioContinuityEpoch::new(51);
    runtime.enter_state(AudioStateEntry { epoch, start_sample: 0 }).unwrap();
    let identity = runtime.resource_root_identity();
    let address = AudioNativeProcessorAddress {
        root: identity,
        local: identity,
        epoch: Some(epoch),
        occurrence: AudioProcessorOccurrence {
            instance_id: processor_id,
            owner: AudioProcessorOccurrenceOwner::Track(track),
            insertion: AudioProcessorInsertionPoint::PreFader,
        },
    };
    let target = runtime.native_owner_lease(address).unwrap().editor_target();
    (
        HandoffFixture {
            sequence,
            runtime,
            source,
            probe,
            target,
            attachment: target,
        },
        address,
        calls,
    )
}

#[test]
fn owned_installed_successful_off_retires_only_after_success_and_never_replays_failure() {
    for result in [
        NoteModelReturn::Success,
        NoteModelReturn::Failed,
        NoteModelReturn::Deferred,
    ] {
        let (mut runtime, source, address, calls) = installed_off_model(result);
        let lease = runtime.native_owner_lease(address).unwrap();
        let claim = source.current_claim().unwrap().unwrap();
        let mut pcm = [91.0];
        runtime
            .render_into(AudioRenderRequest { start_sample: 0, frames: 1 }, &mut pcm)
            .unwrap();
        assert_eq!(pcm, [0.5]);
        let off = runtime.render_into(AudioRenderRequest { start_sample: 1, frames: 1 }, &mut pcm);
        match result {
            NoteModelReturn::Success => {
                off.unwrap();
                assert_eq!(pcm, [1.0]);
                for start in 2..6 {
                    runtime
                        .render_into(
                            AudioRenderRequest { start_sample: start, frames: 1 },
                            &mut pcm,
                        )
                        .unwrap();
                    assert_eq!(pcm, [(start + 1) as f32 * 0.5]);
                }
                let c = calls.lock().unwrap();
                assert_eq!((c.processes, c.note_count, c.ends), (6, 6, 0));
                for pair in 0..3 {
                    let on = c.notes[pair * 2].unwrap();
                    let off = c.notes[pair * 2 + 1].unwrap();
                    assert_eq!(on.kind, AudioNoteEventKind::On);
                    assert_eq!(off.kind, AudioNoteEventKind::Off);
                    assert_eq!(on.native_note_id, pair as i32);
                    assert_eq!(
                        (off.native_note_id, off.port_index, off.channel, off.key),
                        (on.native_note_id, on.port_index, on.channel, on.key)
                    );
                }
                assert_eq!(runtime.native_owner_lease(address).unwrap(), lease);
            }
            NoteModelReturn::Failed | NoteModelReturn::Deferred => {
                let expected = match result {
                    NoteModelReturn::Failed => {
                        AudioProcessorHostError::Process("owned Off refusal".to_owned())
                    }
                    NoteModelReturn::Deferred => AudioProcessorHostError::ReprepareRequired(
                        crate::NativeObservedRequest::RequestOnly(
                            mondrian_core::NativeAudioReprepareRequest::ClapRestart,
                        ),
                    ),
                    NoteModelReturn::Success => unreachable!(),
                };
                assert!(
                    matches!(off, Err(AudioExecutionError::ProcessorHost(ref error)) if error == &expected)
                );
                assert!(runtime
                    .render_into(AudioRenderRequest { start_sample: 1, frames: 1 }, &mut pcm)
                    .is_err());
                assert!(runtime
                    .render_into(AudioRenderRequest { start_sample: 2, frames: 1 }, &mut pcm)
                    .is_err());
                let c = calls.lock().unwrap();
                assert_eq!((c.processes, c.note_count, c.ends), (2, 2, 0));
                assert_eq!(
                    c.notes[0].unwrap().native_note_id,
                    c.notes[1].unwrap().native_note_id
                );
            }
        }
        runtime.native_owner_installation_verifier().unwrap().validate(&claim).unwrap();
    }
}

#[test]
fn owned_vst_rescan_handoff_moves_ledger_origin_once_and_keeps_on_off_ids() {
    // This is the retained owned Note model. It is not a Note-capable DLL with
    // restart4 callbacks, and gives no additional native VST3 qualification.
    use super::continuing_rescan_controls::{hold, install, prepare, rescan};
    let (mut f, old_address, calls) = installed_note_fixture(NoteModelReturn::Success, true);
    let lease = f.runtime.native_owner_lease(old_address).unwrap();
    let mut pcm = [91.0];
    f.runtime
        .render_into(AudioRenderRequest { start_sample: 0, frames: 1 }, &mut pcm)
        .unwrap();
    assert_eq!(pcm, [0.5]);
    let on = calls.lock().unwrap().notes[0].unwrap();
    assert_eq!(on.kind, AudioNoteEventKind::On);
    assert_eq!(on.native_note_id, 0);
    let before_cursors = cursors(&f.runtime);
    let before_entries = f.probe.lock().unwrap().entries;
    let before_binds = calls.lock().unwrap().binds;
    let before_creates = calls.lock().unwrap().creates;
    let a = crate::NativeRequestCohort::new(
        151,
        mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 },
    )
    .unwrap();
    f.probe.lock().unwrap().live_request = Some(crate::NativeObservedRequest::Cohort(a));
    hold(&mut f);
    let proposal = rescan(&mut f, a);
    let (mut prepared, mut authorization) = prepare(&mut f, &proposal);
    let claim = f.source.current_claim().unwrap().unwrap();
    install(&mut f, &proposal, &mut prepared, &mut authorization);
    let next_address = AudioNativeProcessorAddress {
        root: f.runtime.resource_root_identity(),
        local: f.runtime.resource_root_identity(),
        ..old_address
    };
    let next_lease = f.runtime.native_owner_lease(next_address).unwrap();
    assert_ne!(next_lease, lease);
    assert_eq!(next_lease.binding(), f.target.binding);
    assert_eq!(
        f.source.current_claim().unwrap().unwrap().generation().get(),
        claim.generation().get() + 1
    );
    let installed = cursors(&f.runtime);
    assert_eq!(
        installed.sessions[0].unwrap().state,
        before_cursors.sessions[0].unwrap().state
    );
    assert_eq!(
        installed.processors[0].unwrap().state,
        before_cursors.processors[0].unwrap().state
    );
    f.ack(&proposal);
    assert_eq!(cursors(&f.runtime), installed);
    assert!(f
        .runtime
        .acknowledge_native_author_parameter_handoff(&proposal, &ExecutionCancellationToken::new())
        .is_err());
    assert_eq!(calls.lock().unwrap().binds, before_binds + 1);
    assert_eq!(calls.lock().unwrap().creates, before_creates);
    assert_eq!(f.probe.lock().unwrap().entries, before_entries);
    {
        let p = f.probe.lock().unwrap();
        assert_eq!((p.ack_attempts, p.accepted_acks), (1, 1));
    }
    // Off must be validated with the moved origin while retaining the original
    // Pressed record/ID. SuccessfulOffBlock retires only this actual success.
    f.runtime
        .render_into(AudioRenderRequest { start_sample: 1, frames: 1 }, &mut pcm)
        .unwrap();
    assert_eq!(pcm, [1.5]);
    f.runtime
        .render_into(AudioRenderRequest { start_sample: 2, frames: 1 }, &mut pcm)
        .unwrap();
    assert_eq!(pcm, [2.25]);
    let c = calls.lock().unwrap();
    assert_eq!((c.processes, c.note_count, c.ends), (3, 3, 0));
    let off = c.notes[1].unwrap();
    let next = c.notes[2].unwrap();
    assert_eq!(off.kind, AudioNoteEventKind::Off);
    assert_eq!(
        (off.native_note_id, off.port_index, off.channel, off.key),
        (on.native_note_id, on.port_index, on.channel, on.key)
    );
    assert_eq!(off.velocity.to_bits(), 0.5_f64.to_bits());
    assert_eq!(next.kind, AudioNoteEventKind::On);
    assert_eq!(next.native_note_id, 1);
    assert_eq!(
        f.runtime.native_owner_lease(next_address).unwrap(),
        next_lease
    );
    assert_eq!(f.probe.lock().unwrap().entries, before_entries);
    drop(prepared);
}
