//! Owned shared-DAG controls; these are not native-plugin qualifications.
use super::*;

struct MixedAuxResolver;
struct MixedAuxFactory;
struct MixedAuxProcessor;

impl AudioProcessorResolver for MixedAuxResolver {
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        if matches!(request.definition(),
            mondrian_timeline::AudioProcessorDefinitionRef::Clap { plugin_id, .. }
                if plugin_id == "test.mondrian.mixed_aux")
        {
            Ok(Arc::new(MixedAuxFactory))
        } else {
            TestDelayProcessorResolver {
                latency_frames: 2,
                realtime_capable: true,
                ..Default::default()
            }
            .prepare(request)
        }
    }
}

impl AudioProcessorFactory for MixedAuxFactory {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        AudioProcessorExecutionContract::stateless()
    }
    fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract {
        AudioProcessorAuxiliaryInputContract {
            buses: [
                ("mono", AudioChannelLayout::Mono),
                ("stereo", AudioChannelLayout::Stereo),
                ("silent", AudioChannelLayout::Mono),
            ]
            .into_iter()
            .map(
                |(key, channel_layout)| AudioProcessorAuxiliaryInputBusContract {
                    bus_key: key.into(),
                    channel_layout,
                },
            )
            .collect(),
        }
    }
    fn create(
        &self,
        _: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        Ok(Box::new(MixedAuxProcessor))
    }
}

impl AudioProcessor for MixedAuxProcessor {
    fn enter_state(&mut self, _: i64) -> Result<(), AudioProcessorHostError> {
        Ok(())
    }
    fn process(
        &mut self,
        _: AudioProcessorProcessContext,
        audio: &mut dyn AudioProcessorAudioIo,
        _: AudioParameterEventBatch<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        let frames = audio.frames();
        for (key, layout) in [
            ("mono", AudioChannelLayout::Mono),
            ("stereo", AudioChannelLayout::Stereo),
            ("silent", AudioChannelLayout::Mono),
        ] {
            let bus = audio.auxiliary_input(key).expect("keyed port exists");
            assert_eq!(bus.channel_layout, layout);
            assert_eq!(bus.frames, frames);
            assert_eq!(bus.interleaved.len(), frames * layout.channel_count());
            if key == "silent" {
                assert!(bus.interleaved.iter().all(|v| *v == 0.0));
            }
        }
        assert!(audio.auxiliary_input("absent").is_none());
        let channels = audio.main_layout().channel_count();
        let mono = audio.main_and_auxiliary_input("mono").expect("disjoint Main/Mono borrows");
        assert_eq!(mono.auxiliary.channel_layout, AudioChannelLayout::Mono);
        for (frame, value) in
            mono.main_interleaved.chunks_exact_mut(channels).zip(mono.auxiliary.interleaved)
        {
            for sample in frame {
                *sample += 2.0 * value;
            }
        }
        let stereo =
            audio.main_and_auxiliary_input("stereo").expect("disjoint Main/Stereo borrows");
        assert_eq!(stereo.auxiliary.channel_layout, AudioChannelLayout::Stereo);
        for (frame, pair) in stereo
            .main_interleaved
            .chunks_exact_mut(channels)
            .zip(stereo.auxiliary.interleaved.chunks_exact(2))
        {
            for sample in frame {
                *sample += 3.0 * pair[0] + 5.0 * pair[1];
            }
        }
        Ok(())
    }
}

fn instance(id: &str) -> AudioProcessorInstance {
    AudioProcessorInstance {
        id: mondrian_core::AudioProcessorInstanceId::new(),
        definition: mondrian_timeline::AudioProcessorDefinitionRef::Clap {
            plugin_id: id.into(),
            schema_version: 1,
            binary_sha256: None,
        },
        bypassed: false,
        parameters: Default::default(),
        opaque_state: None,
    }
}

fn fixture(
    layout: AudioChannelLayout,
    delayed_main: bool,
    automated: bool,
) -> (Arc<CompiledAudioProgram>, AudioComponentEditId) {
    let mut sequence = Sequence::new("mixed auxiliary inputs");
    sequence.settings.audio_channel_layout = layout;
    let main = sequence.audio_tracks[0].id;
    let detector = sequence.add_audio_track();
    for track in [main, detector] {
        sequence
            .add_media_audio_clip(
                track,
                Clip::new(AssetId::new(), TimelineTime::ZERO, tt(4, 1)).unwrap(),
                AudioSourceComponentId::primary(),
            )
            .unwrap();
    }
    let main_edit = sequence.audio_tracks.iter().find(|t| t.id == main).unwrap().clips[0]
        .audio_components[0]
        .id;
    if delayed_main {
        let scope_id = sequence.audio_tracks.iter().find(|t| t.id == main).unwrap().clips[0]
            .audio_components[0]
            .processing
            .scope_id;
        sequence
            .audio_program
            .processing_scopes
            .iter_mut()
            .find(|s| s.id == scope_id)
            .unwrap()
            .processors
            .processors
            .push(instance("test.mondrian.delay"));
    }
    let processor = instance("test.mondrian.mixed_aux");
    let processor_id = processor.id;
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(processor);
    sequence.audio_program.routes.retain(|route|
        matches!(route.source, AudioRouteSource::Track { track_id, .. } if track_id == main));
    for key in ["mono", "stereo"] {
        // Multiple routes must sum, not overwrite the same mapped port.
        for _ in 0..2 {
            let mut route = AudioProcessorSidechainRoute::new(
                AudioRouteSource::Track {
                    track_id: detector,
                    port: AudioChannelStripOutputPort::PostMute,
                },
                processor_id,
                key,
            );
            if automated {
                route.gain_automation = Some(route_gain_curve(0.0, 6.0));
            }
            sequence.audio_program.sidechain_routes.push(route);
        }
    }
    let output = sequence.audio_program.outputs[0].id;
    (
        Arc::new(compile_audio_program(&sequence, AudioCompileRequest::program(output)).unwrap()),
        main_edit,
    )
}

struct MixedSource(AudioComponentEditId);
impl AudioPcmSource for MixedSource {
    fn read_indexed_interleaved(
        &mut self,
        edit: AudioComponentEditId,
        frames: &[i64],
        layout: AudioChannelLayout,
        destination: &mut [f32],
    ) -> Result<(), AudioExecutionError> {
        for (frame, position) in destination.chunks_exact_mut(layout.channel_count()).zip(frames) {
            for (channel, sample) in frame.iter_mut().enumerate() {
                *sample = if *position < 0 {
                    0.0
                } else {
                    (*position as f32 + 1.0)
                        * if edit == self.0 {
                            0.25 * (channel as f32 + 1.0)
                        } else if channel == 0 {
                            1.0
                        } else {
                            3.0
                        }
                };
            }
        }
        Ok(())
    }
}

fn check_mixed_auxiliary_inputs(delayed_main: bool, automated: bool) {
    for layout in [AudioChannelLayout::Mono, AudioChannelLayout::Stereo] {
        let (compiled, main_edit) = fixture(layout, delayed_main, automated);
        for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
            for backend in [
                AudioKernelBackend::ScalarReference,
                AudioKernelBackend::RuntimeVectorized,
            ] {
                let delay_samples = if delayed_main { 2 * 2 * (1 + 2) } else { 0 };
                let contract = AudioRenderContract {
                    sample_rate: 8,
                    channel_layout: layout,
                    max_block_frames: 8,
                    processing_mode: mode,
                    processor_session_scratch_budget_bytes:
                        AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
                    public_output_lookahead_budget_frames: 2,
                    compensation_delay_scratch_budget_bytes: delay_samples * 4,
                };
                let plan = Arc::new(
                    PreparedAudioPlan::prepare_with_processor_resolver(
                        Arc::clone(&compiled),
                        contract,
                        backend,
                        &MixedAuxResolver,
                    )
                    .expect("exact per-port layouts and PDC budget must prepare"),
                );
                assert_eq!(
                    plan.schedule_summary().compensation_delay_samples,
                    delay_samples
                );
                assert_eq!(
                    plan.session_resource_footprint().unwrap().compensation_delay_bytes,
                    delay_samples * 4
                );
                if delayed_main {
                    assert!(matches!(
                        PreparedAudioPlan::prepare_with_processor_resolver(
                            Arc::clone(&compiled),
                            AudioRenderContract {
                                compensation_delay_scratch_budget_bytes: delay_samples * 4 - 1,
                                ..contract
                            },
                            backend,
                            &MixedAuxResolver
                        ),
                        Err(AudioCompileError::CompensationScratchBudgetExceeded { .. })
                    ));
                }
                assert_eq!(
                    plan.schedule_summary().maximum_processor_auxiliary_channels,
                    4
                );
                assert_eq!(
                    plan.schedule_summary().maximum_sidechain_mix_channels,
                    if layout == AudioChannelLayout::Mono {
                        2
                    } else {
                        1
                    }
                );
                let mut session = AudioRenderSession::new(plan).unwrap();
                assert_eq!(
                    session.capacity().processor_auxiliary_scratch_samples,
                    8 * (1 + 2 + 1)
                );
                for (epoch, start, partitions) in [
                    (1, 0, vec![8]),
                    (2, 0, vec![1, 2, 5]),
                    (3, 3, vec![3, 1, 4]),
                ] {
                    session
                        .enter_state(AudioStateEntry {
                            epoch: AudioContinuityEpoch::new(epoch),
                            start_sample: start,
                        })
                        .unwrap();
                    let mut source = MixedSource(main_edit);
                    let mut position = start;
                    for frames in partitions {
                        let mut block = vec![0.0; frames * layout.channel_count()];
                        session
                            .render_into(
                                &mut source,
                                AudioRenderRequest {
                                    transport_activity: None,
                                    start_sample: position,
                                    frames,
                                },
                                &mut block,
                            )
                            .unwrap();
                        for (offset, frame) in
                            block.chunks_exact(layout.channel_count()).enumerate()
                        {
                            let time = position + offset as i64;
                            let ramp = time as f32 + 1.0;
                            let gain = if automated {
                                10.0_f64.powf(6.0 * (time as f64 / 8.0).min(1.0) / 20.0) as f32
                            } else {
                                1.0
                            };
                            // Independent literal standard matrices: mono duplicates;
                            // stereo downmix is (L+R)/2. Both ports have two routes.
                            let detector = if layout == AudioChannelLayout::Mono {
                                20.0
                            } else {
                                44.0
                            };
                            for (channel, actual) in frame.iter().enumerate() {
                                let expected =
                                    ramp * (0.25 * (channel as f32 + 1.0) + detector * gain);
                                assert!((actual - expected).abs() <= expected.abs() * 2.0e-6,
                                    "{layout:?} {mode:?} {backend:?} epoch{epoch} sample{time}: {actual} != {expected}");
                            }
                        }
                        position += frames as i64;
                    }
                }
            }
        }
    }
}

#[test]
fn mixed_auxiliary_inputs_preserve_port_layout_and_partition_pcm() {
    check_mixed_auxiliary_inputs(false, false);
}

#[test]
fn mixed_auxiliary_input_pdc_and_target_time_gain() {
    check_mixed_auxiliary_inputs(true, true);
}

#[test]
fn mixed_auxiliary_input_contract_keeps_key_and_mapping_refusals() {
    let mut contract = MixedAuxFactory.auxiliary_input_contract();
    contract.validate(AudioChannelLayout::Stereo).unwrap();
    contract.buses[0].channel_layout = AudioChannelLayout::discrete(3).unwrap();
    assert!(contract.validate(AudioChannelLayout::Stereo).is_err());
    for key in ["stereo", "", "bad\nkey"] {
        let mut contract = MixedAuxFactory.auxiliary_input_contract();
        contract.buses[0].bus_key = key.into();
        assert!(contract.validate(AudioChannelLayout::Stereo).is_err());
    }
}
