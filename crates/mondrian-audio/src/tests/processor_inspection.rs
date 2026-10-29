use super::*;
use mondrian_core::{AudioProcessingScopeId, AudioProcessorInstanceId, ClipId};
use mondrian_timeline::{AudioChannelStripOwner, AudioChannelStripRack, AudioComponentAddress};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy)]
enum FactoryMode {
    Valid,
    DuplicateKeys,
    WrongLayout,
    MixedLayout,
    OfflineOnly,
    Panic,
    UnknownNameKey,
    DuplicateNameKey,
    InvalidName,
    PanicNames,
}

struct InspectionResolver {
    expected: AudioProcessorOccurrence,
    mode: FactoryMode,
    calls: AtomicUsize,
    creates: Arc<AtomicUsize>,
}

impl InspectionResolver {
    fn new(expected: AudioProcessorOccurrence, mode: FactoryMode) -> Self {
        Self {
            expected,
            mode,
            calls: AtomicUsize::new(0),
            creates: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl AudioProcessorResolver for InspectionResolver {
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.occurrence(), self.expected);
        assert!(
            matches!(request.definition(), mondrian_timeline::AudioProcessorDefinitionRef::Clap { plugin_id, .. } if plugin_id == "test.inspection")
        );
        assert!(request.parameters().is_empty());
        if matches!(self.mode, FactoryMode::Panic) {
            panic!("injected control resolver panic");
        }
        let state = request.opaque_state().expect("exact author state");
        assert!(matches!(state, [0] | [1]));
        let layout = request.render_contract().channel_layout;
        let bus = |key: &str| AudioProcessorAuxiliaryInputBusContract {
            bus_key: key.into(),
            channel_layout: layout,
        };
        let mut buses = if state == [0] {
            vec![bus("detector.11"), bus("detector.7")]
        } else {
            vec![bus("detector.7")]
        };
        if matches!(self.mode, FactoryMode::DuplicateKeys) {
            buses.push(buses[0].clone());
        }
        if matches!(self.mode, FactoryMode::WrongLayout) {
            buses[0].channel_layout = AudioChannelLayout::discrete(3).unwrap();
        }
        if matches!(self.mode, FactoryMode::MixedLayout) {
            buses[0].channel_layout = AudioChannelLayout::Mono;
        }
        Ok(Arc::new(InspectionFactory {
            buses,
            mode: self.mode,
            offline_only: matches!(self.mode, FactoryMode::OfflineOnly),
            creates: Arc::clone(&self.creates),
        }))
    }
}

struct InspectionFactory {
    mode: FactoryMode,
    buses: Vec<AudioProcessorAuxiliaryInputBusContract>,
    offline_only: bool,
    creates: Arc<AtomicUsize>,
}

impl AudioProcessorFactory for InspectionFactory {
    fn auxiliary_input_names(&self) -> Vec<AudioProcessorInputName> {
        if matches!(self.mode, FactoryMode::PanicNames) {
            panic!("injected name query panic");
        }
        let mut names = self
            .buses
            .iter()
            .map(|bus| AudioProcessorInputName {
                bus_key: bus.bus_key.clone(),
                name: "压缩探测 🙂".into(),
            })
            .collect::<Vec<_>>();
        match self.mode {
            FactoryMode::UnknownNameKey => names[0].bus_key = "not-advertised".into(),
            FactoryMode::DuplicateNameKey => names[1] = names[0].clone(),
            FactoryMode::InvalidName => names[0].name = "line\nbreak".into(),
            _ => {}
        }
        names
    }

    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        AudioProcessorExecutionContract::new(
            3,
            AudioProcessorTail::Finite(3),
            true,
            !self.offline_only,
            true,
            24,
        )
        .expect("fixture contract")
    }

    fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract {
        AudioProcessorAuxiliaryInputContract { buses: self.buses.clone() }
    }

    fn create(
        &self,
        _native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        Err(AudioProcessorHostError::InstanceCreation(
            "control inspection created a Session".into(),
        ))
    }
}

fn author_processor(state: u8) -> AudioProcessorInstance {
    AudioProcessorInstance {
        id: AudioProcessorInstanceId::new(),
        definition: mondrian_timeline::AudioProcessorDefinitionRef::Clap {
            plugin_id: "test.inspection".into(),
            schema_version: 1,
            binary_sha256: None,
        },
        bypassed: false,
        parameters: Default::default(),
        opaque_state: Some(vec![state].into()),
    }
}

fn inspection_contract(
    layout: AudioChannelLayout,
    mode: AudioProcessingMode,
) -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: layout,
        max_block_frames: 64,
        processing_mode: mode,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    }
}

#[test]
fn processor_input_inspection_preserves_channel_addresses_modes_and_author_state() {
    for state in [0, 1] {
        for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
            let mut sequence = Sequence::new("Control inspection");
            let track_id = sequence.audio_tracks[0].id;
            sequence.audio_tracks[0].is_locked = true;
            let bus_id = mondrian_core::MixBusId::new();
            sequence.audio_program.buses.push(AudioMixBus {
                id: bus_id,
                name: "Bus".into(),
                strip: Default::default(),
            });
            let output_id = sequence.audio_program.outputs[0].id;
            for (owner, expected_owner) in [
                (
                    AudioChannelStripOwner::Track { track_id },
                    AudioProcessorOccurrenceOwner::Track(track_id),
                ),
                (
                    AudioChannelStripOwner::Bus { bus_id },
                    AudioProcessorOccurrenceOwner::Bus(bus_id),
                ),
                (
                    AudioChannelStripOwner::ProgramOutput { output_id },
                    AudioProcessorOccurrenceOwner::Output(output_id),
                ),
            ] {
                for (rack, insertion) in [
                    (
                        AudioChannelStripRack::PreFader,
                        AudioProcessorInsertionPoint::PreFader,
                    ),
                    (
                        AudioChannelStripRack::PostFader,
                        AudioProcessorInsertionPoint::PostFader,
                    ),
                ] {
                    let processor = author_processor(state);
                    let processor_id = processor.id;
                    let strip = match owner {
                        AudioChannelStripOwner::Track { track_id } => {
                            &mut sequence
                                .audio_program
                                .track_channels
                                .get_mut(&track_id)
                                .expect("Track")
                                .strip
                        }
                        AudioChannelStripOwner::Bus { .. } => {
                            &mut sequence.audio_program.buses[0].strip
                        }
                        AudioChannelStripOwner::ProgramOutput { .. } => {
                            &mut sequence.audio_program.outputs[0].strip
                        }
                    };
                    match rack {
                        AudioChannelStripRack::PreFader => {
                            strip.pre_fader.processors.push(processor)
                        }
                        AudioChannelStripRack::PostFader => {
                            strip.post_fader.processors.push(processor)
                        }
                    }
                    let before = serde_json::to_value(&sequence).expect("author snapshot");
                    let expected = AudioProcessorOccurrence {
                        instance_id: processor_id,
                        owner: expected_owner,
                        insertion,
                    };
                    let resolver = InspectionResolver::new(expected, FactoryMode::Valid);
                    let request = AudioProcessorInputInspectionRequest {
                        target: AudioProcessorInputInspectionTarget::ChannelStrip { owner, rack },
                        processor_id,
                        render_contract: inspection_contract(AudioChannelLayout::Stereo, mode),
                    };
                    let result = inspect_audio_processor_inputs(&sequence, request, &resolver)
                        .expect("input facts");
                    assert_eq!(result.occurrence, expected);
                    assert_eq!(result.render_contract, request.render_contract);
                    assert_eq!(
                        result
                            .auxiliary_inputs
                            .buses
                            .iter()
                            .map(|bus| bus.bus_key.as_str())
                            .collect::<Vec<_>>(),
                        if state == 0 {
                            vec!["detector.11", "detector.7"]
                        } else {
                            vec!["detector.7"]
                        }
                    );
                    assert_eq!(
                        result
                            .input_names
                            .iter()
                            .map(|name| name.bus_key.as_str())
                            .collect::<Vec<_>>(),
                        result
                            .auxiliary_inputs
                            .buses
                            .iter()
                            .map(|bus| bus.bus_key.as_str())
                            .collect::<Vec<_>>()
                    );
                    assert!(result.input_names.iter().all(|name| name.name == "压缩探测 🙂"));
                    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
                    assert_eq!(resolver.creates.load(Ordering::SeqCst), 0);
                    assert_eq!(
                        serde_json::to_value(&sequence).expect("unchanged author"),
                        before
                    );
                }
            }
        }
    }
}

#[test]
fn processor_input_inspection_requires_the_exact_shared_scope_occurrence() {
    let mut sequence = sequence_with_audio_clip();
    let track_id = sequence.audio_tracks[0].id;
    let mut second = sequence.audio_tracks[0].clips[0].clone();
    second.id = ClipId::new();
    second.position = tt(4, 1);
    second.audio_components[0].id = AudioComponentEditId::new();
    sequence.audio_tracks[0].add_clip(second).expect("shared Scope placement");
    sequence.audio_tracks[0].is_locked = true;
    let scope_id = sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
    let processor = author_processor(1);
    let processor_id = processor.id;
    sequence
        .audio_program
        .processing_scopes
        .iter_mut()
        .find(|scope| scope.id == scope_id)
        .expect("Scope")
        .processors
        .processors
        .push(processor);
    for clip in &sequence.audio_tracks[0].clips {
        let occurrence = AudioComponentAddress {
            track_id,
            clip_id: clip.id,
            edit_id: clip.audio_components[0].id,
        };
        let expected = AudioProcessorOccurrence {
            instance_id: processor_id,
            owner: AudioProcessorOccurrenceOwner::Contribution {
                edit_id: occurrence.edit_id,
                scope_id,
            },
            insertion: AudioProcessorInsertionPoint::Scope,
        };
        let resolver = InspectionResolver::new(expected, FactoryMode::Valid);
        let request = AudioProcessorInputInspectionRequest {
            target: AudioProcessorInputInspectionTarget::ProcessingScope { scope_id, occurrence },
            processor_id,
            render_contract: inspection_contract(
                AudioChannelLayout::Mono,
                AudioProcessingMode::Realtime,
            ),
        };
        let result = inspect_audio_processor_inputs(&sequence, request, &resolver)
            .expect("locked shared Scope inspection");
        assert_eq!(result.occurrence, expected);
        assert_eq!(
            result.auxiliary_inputs.buses[0].channel_layout,
            AudioChannelLayout::Mono
        );
        let stale_scope = AudioProcessingScopeId::new();
        let stale = AudioProcessorInputInspectionRequest {
            target: AudioProcessorInputInspectionTarget::ProcessingScope {
                scope_id: stale_scope,
                occurrence,
            },
            ..request
        };
        assert_eq!(
            inspect_audio_processor_inputs(&sequence, stale, &resolver),
            Err(AudioProcessorInputInspectionError::ScopeBindingChanged(
                stale_scope
            ))
        );
        assert_eq!(
            resolver.calls.load(Ordering::SeqCst),
            1,
            "stale bindings must not start native preparation"
        );
        assert_eq!(resolver.creates.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn processor_input_inspection_reuses_mode_layout_key_and_panic_admission() {
    let mut sequence = Sequence::new("Inspection refusal");
    let output_id = sequence.audio_program.outputs[0].id;
    let processor = author_processor(0);
    let processor_id = processor.id;
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(processor);
    let expected = AudioProcessorOccurrence {
        instance_id: processor_id,
        owner: AudioProcessorOccurrenceOwner::Output(output_id),
        insertion: AudioProcessorInsertionPoint::PreFader,
    };
    let request = AudioProcessorInputInspectionRequest {
        target: AudioProcessorInputInspectionTarget::ChannelStrip {
            owner: AudioChannelStripOwner::ProgramOutput { output_id },
            rack: AudioChannelStripRack::PreFader,
        },
        processor_id,
        render_contract: inspection_contract(
            AudioChannelLayout::Stereo,
            AudioProcessingMode::Realtime,
        ),
    };
    for mode in [
        FactoryMode::DuplicateKeys,
        FactoryMode::WrongLayout,
        FactoryMode::OfflineOnly,
        FactoryMode::Panic,
        FactoryMode::UnknownNameKey,
        FactoryMode::DuplicateNameKey,
        FactoryMode::InvalidName,
        FactoryMode::PanicNames,
    ] {
        let resolver = InspectionResolver::new(expected, mode);
        let result = inspect_audio_processor_inputs(&sequence, request, &resolver);
        assert!(matches!(
            result,
            Err(AudioProcessorInputInspectionError::Processor(_))
        ));
        assert_eq!(resolver.creates.load(Ordering::SeqCst), 0);
    }
    let before = serde_json::to_value(&sequence).unwrap();
    let resolver = InspectionResolver::new(expected, FactoryMode::MixedLayout);
    let facts = inspect_audio_processor_inputs(&sequence, request, &resolver)
        .expect("inspection preserves supported per-port layouts");
    assert_eq!(
        facts
            .auxiliary_inputs
            .buses
            .iter()
            .map(|bus| (bus.bus_key.as_str(), bus.channel_layout))
            .collect::<Vec<_>>(),
        vec![
            ("detector.11", AudioChannelLayout::Mono),
            ("detector.7", AudioChannelLayout::Stereo)
        ]
    );
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    assert_eq!(resolver.creates.load(Ordering::SeqCst), 0);
    assert_eq!(serde_json::to_value(&sequence).unwrap(), before);
    let resolver = InspectionResolver::new(expected, FactoryMode::Valid);
    let overflow = usize::try_from(u64::from(u32::MAX) + 1).ok();
    for (sample_rate, max_block_frames) in [(0, 64), (48_000, 0)]
        .into_iter()
        .chain(overflow.map(|frames| (48_000, frames)))
    {
        let invalid = AudioProcessorInputInspectionRequest {
            render_contract: AudioRenderContract {
                sample_rate,
                max_block_frames,
                ..request.render_contract
            },
            ..request
        };
        assert_eq!(
            inspect_audio_processor_inputs(&sequence, invalid, &resolver),
            Err(AudioProcessorInputInspectionError::RenderContract(
                AudioCompileError::InvalidRenderContract
            ))
        );
    }
    let missing = AudioProcessorInstanceId::new();
    assert_eq!(
        inspect_audio_processor_inputs(
            &sequence,
            AudioProcessorInputInspectionRequest { processor_id: missing, ..request },
            &resolver
        ),
        Err(AudioProcessorInputInspectionError::Rack(
            mondrian_timeline::AudioProcessorRackEditError::UnknownProcessor(missing)
        ))
    );
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn processor_input_inspection_reads_the_builtin_factory_contract() {
    let mut sequence = Sequence::new("Built-in inspection");
    let output_id = sequence.audio_program.outputs[0].id;
    let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
    let processor_id = processor.id;
    sequence.audio_program.outputs[0].strip.post_fader.processors.push(processor);
    let request = AudioProcessorInputInspectionRequest {
        target: AudioProcessorInputInspectionTarget::ChannelStrip {
            owner: AudioChannelStripOwner::ProgramOutput { output_id },
            rack: AudioChannelStripRack::PostFader,
        },
        processor_id,
        render_contract: inspection_contract(
            AudioChannelLayout::Stereo,
            AudioProcessingMode::Offline,
        ),
    };
    let result = inspect_audio_processor_inputs(&sequence, request, &BuiltInAudioProcessorResolver)
        .expect("real built-in preparation facts");
    assert!(result.auxiliary_inputs.buses.is_empty());
    assert_eq!(
        result.execution_contract,
        // 64 f64 gain factors and 128 f32 input samples remain accounted for.
        AudioProcessorExecutionContract::new(0, AudioProcessorTail::None, false, true, true, 1024)
            .expect("prepared gain storage")
    );
    assert_eq!(
        result.occurrence.insertion,
        AudioProcessorInsertionPoint::PostFader
    );
}

#[test]
fn processor_input_names_bound_unicode_without_changing_bus_identity() {
    let japanese = "探".repeat(127);
    let captured = AudioProcessorInputName::capture("detector.7", &format!("  {japanese}  "))
        .expect("full native UTF-16 name remains valid UTF-8");
    assert_eq!(captured.name, japanese);
    for invalid in [
        "",
        "   ",
        "two\nrows",
        "null\0byte",
        "unicode\u{2028}line",
        "unicode\u{2029}paragraph",
        &"x".repeat(1025),
    ] {
        assert!(AudioProcessorInputName::capture("detector.7", invalid).is_none());
    }
    let contract = AudioProcessorAuxiliaryInputContract {
        buses: vec![AudioProcessorAuxiliaryInputBusContract {
            bus_key: "detector.7".into(),
            channel_layout: AudioChannelLayout::Stereo,
        }],
    };
    AudioProcessorInputName::validate_all(std::slice::from_ref(&captured), &contract)
        .expect("admitted key");
    assert!(
        AudioProcessorInputName::validate_all(&[captured.clone(), captured], &contract).is_err()
    );
    assert!(serde_json::from_str::<AudioProcessorInputName>(
        r#"{"bus_key":"detector.7","name":"Probe","unknown":true}"#
    )
    .is_err());
}
