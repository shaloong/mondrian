//! Owned shared-graph controls; these are not native or third-party compatibility claims.
use super::*;
use mondrian_timeline::{AudioProcessorDefinitionRef, AudioProcessorOutputPort};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Default)]
pub(crate) struct OutputResolver {
    multi_latency: usize,
    after_latency: usize,
    calls: Arc<AtomicUsize>,
    creates: Arc<AtomicUsize>,
    revision: Arc<AtomicUsize>,
    fail_at: Option<i64>,
    fail_after: bool,
}
#[derive(Clone)]
struct OutputFactory {
    resolver: OutputResolver,
    factor: Option<f32>,
    layout: AudioChannelLayout,
    latency: usize,
    frozen_revision: Option<usize>,
    detector: bool,
}
impl AudioProcessorResolver for OutputResolver {
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        let AudioProcessorDefinitionRef::Clap { plugin_id, .. } = request.definition() else {
            return BuiltInAudioProcessorResolver.prepare(request);
        };
        let factor = if plugin_id == "owned.outputs.after" || plugin_id == "owned.outputs.detector"
        {
            None
        } else {
            Some(
                plugin_id
                    .strip_prefix("owned.outputs.")
                    .ok_or_else(|| {
                        AudioProcessorHostError::InvalidContract(
                            "unrecognized owned test processor".into(),
                        )
                    })?
                    .parse::<f32>()
                    .unwrap(),
            )
        };
        Ok(Arc::new(OutputFactory {
            resolver: self.clone(),
            factor,
            layout: request.render_contract().channel_layout,
            latency: if factor.is_some() {
                self.multi_latency
            } else {
                self.after_latency
            },
            frozen_revision: None,
            detector: plugin_id == "owned.outputs.detector",
        }))
    }
}
impl AudioProcessorFactory for OutputFactory {
    fn visit_host_owned_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(
            AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), AudioProcessorFactoryStorageError>,
    ) -> Result<(), AudioProcessorFactoryStorageError> {
        visitor(AudioProcessorFactoryStorageBorrow::arc_payload(
            &self.resolver.calls,
        ))?;
        visitor(AudioProcessorFactoryStorageBorrow::arc_payload(
            &self.resolver.creates,
        ))?;
        visitor(AudioProcessorFactoryStorageBorrow::arc_payload(
            &self.resolver.revision,
        ))
    }
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        AudioProcessorExecutionContract::new(
            self.latency,
            AudioProcessorTail::None,
            self.latency != 0,
            true,
            true,
            self.latency
                * self.layout.channel_count()
                * std::mem::size_of::<f32>()
                * if self.factor.is_some() { 3 } else { 1 },
        )
        .unwrap()
    }
    fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract {
        AudioProcessorAuxiliaryInputContract {
            buses: if self.detector {
                vec![AudioProcessorAuxiliaryInputBusContract {
                    bus_key: "side".into(),
                    channel_layout: self.layout,
                }]
            } else {
                Vec::new()
            },
        }
    }
    fn auxiliary_output_contract(&self) -> AudioProcessorAuxiliaryOutputContract {
        if self.factor.is_none() {
            return AudioProcessorAuxiliaryOutputContract::default();
        }
        let revision = self
            .frozen_revision
            .unwrap_or_else(|| self.resolver.revision.load(Ordering::SeqCst));
        let mut buses = vec![
            AudioProcessorAuxiliaryOutputBusContract {
                bus_key: if revision == 1 { "changed" } else { "half" }.into(),
                channel_layout: self.layout,
            },
            AudioProcessorAuxiliaryOutputBusContract {
                bus_key: "quarter".into(),
                channel_layout: self.layout,
            },
        ];
        if revision == 2 {
            buses[1].bus_key = buses[0].bus_key.clone();
        }
        if revision == 3 {
            buses[0].channel_layout = AudioChannelLayout::Stereo;
        }
        if revision == 4 {
            buses.reverse();
        }
        if revision == 5 {
            buses.reserve(16);
            for bus in &mut buses {
                bus.bus_key.reserve(128);
            }
        }
        AudioProcessorAuxiliaryOutputContract { buses }
    }
    fn with_native_configuration_seed(
        &self,
        _: NativeConfigurationFormat,
        configuration: &[u8],
    ) -> Result<Option<Arc<dyn AudioProcessorFactory>>, AudioProcessorHostError> {
        let mut next = self.clone();
        next.frozen_revision = Some(if configuration == [1] { 4 } else { 0 });
        Ok(Some(Arc::new(next)))
    }
    fn create(
        &self,
        _native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        self.resolver.creates.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(OutputProcessor {
            resolver: self.resolver.clone(),
            factor: self.factor,
            detector: self.detector,
            delays: std::array::from_fn(|index| {
                crate::delay::FixedDelayLine::new(
                    if self.factor.is_some() || index == 0 {
                        self.latency
                    } else {
                        0
                    },
                    self.layout.channel_count(),
                )
                .unwrap()
            }),
        }))
    }
}
struct OutputProcessor {
    resolver: OutputResolver,
    factor: Option<f32>,
    detector: bool,
    delays: [crate::delay::FixedDelayLine; 3],
}
impl AudioProcessor for OutputProcessor {
    fn enter_state(&mut self, _: i64) -> Result<(), AudioProcessorHostError> {
        for delay in &mut self.delays {
            delay.reset();
        }
        Ok(())
    }
    fn process(
        &mut self,
        context: AudioProcessorProcessContext,
        audio: &mut dyn AudioProcessorAudioIo,
        _: AudioParameterEventBatch<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        if let Some(factor) = self.factor {
            self.resolver.calls.fetch_add(1, Ordering::SeqCst);
            let channels = audio.main_layout().channel_count();
            let fill = |pcm: &mut [f32], gain: f32| {
                for (sample, value) in pcm.iter_mut().enumerate() {
                    let frame = sample / channels;
                    *value = (context.request().start_sample + frame as i64 + 1) as f32
                        * (sample % channels + 1) as f32
                        * factor
                        * gain;
                }
            };
            fill(audio.main_interleaved(), 1.0);
            self.delays[0].delay_interleaved_in_place(audio.main_interleaved());
            for (index, (key, gain)) in [("half", -0.5), ("quarter", 0.25)].into_iter().enumerate()
            {
                let output = audio
                    .auxiliary_output(key)
                    .expect("all outputs, including the unconnected output, have actual storage");
                fill(output.interleaved, gain);
                self.delays[index + 1].delay_interleaved_in_place(output.interleaved);
            }
            if !self.resolver.fail_after
                && self.resolver.fail_at.is_some_and(|at| context.request().start_sample >= at)
            {
                return Err(AudioProcessorHostError::InvalidContract(
                    "owned failure after partial output writes".into(),
                ));
            }
        } else {
            if self.detector {
                let buses =
                    audio.main_and_auxiliary_input("side").expect("actual prepared sidechain");
                for (main, input) in
                    buses.main_interleaved.iter_mut().zip(buses.auxiliary.interleaved)
                {
                    *main += input;
                }
            } else {
                for value in audio.main_interleaved() {
                    *value *= 2.0;
                }
            }
            self.delays[0].delay_interleaved_in_place(audio.main_interleaved());
            if self.resolver.fail_after
                && self.resolver.fail_at.is_some_and(|at| context.request().start_sample >= at)
            {
                return Err(AudioProcessorHostError::InvalidContract(
                    "owned downstream failure after producer tap".into(),
                ));
            }
        }
        Ok(())
    }
}
fn instance(id: &str) -> AudioProcessorInstance {
    AudioProcessorInstance {
        id: mondrian_core::AudioProcessorInstanceId::new(),
        definition: AudioProcessorDefinitionRef::Clap {
            plugin_id: id.into(),
            schema_version: 1,
            binary_sha256: None,
        },
        bypassed: false,
        parameters: Default::default(),
        opaque_state: None,
    }
}
fn raw(
    id: mondrian_core::AudioProcessorInstanceId,
    port: AudioProcessorOutputPort,
) -> AudioRouteSource {
    AudioRouteSource::ProcessorOutput { processor_id: id, port }
}
pub(crate) fn fixture(layout: AudioChannelLayout) -> Sequence {
    let mut sequence = Sequence::new("owned processor output graph");
    sequence.settings.audio_channel_layout = layout;
    let track = sequence.audio_tracks[0].id;
    let output = sequence.audio_program.outputs[0].id;
    let producer = instance("owned.outputs.1");
    let id = producer.id;
    sequence
        .audio_program
        .track_channels
        .get_mut(&track)
        .unwrap()
        .strip
        .pre_fader
        .processors
        .extend([producer, instance("owned.outputs.after")]);
    sequence.audio_program.routes.push(AudioRoute::new(
        raw(id, AudioProcessorOutputPort::Main),
        AudioRouteDestination::Output(output),
    ));
    for name in ["A", "B"] {
        let bus = mondrian_core::MixBusId::new();
        sequence.audio_program.buses.push(AudioMixBus {
            id: bus,
            name: name.into(),
            strip: Default::default(),
        });
        sequence.audio_program.routes.push(AudioRoute::new(
            raw(
                id,
                AudioProcessorOutputPort::Auxiliary { bus_key: "half".into() },
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
    sequence
}
fn contract(sequence: &Sequence, mode: AudioProcessingMode) -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: sequence.settings.audio_channel_layout,
        max_block_frames: 16,
        processing_mode: mode,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    }
}
pub(crate) fn plan(
    sequence: &Sequence,
    resolver: &OutputResolver,
) -> Result<PreparedAudioPlan, AudioCompileError> {
    PreparedAudioPlan::prepare_with_processor_resolver(
        Arc::new(compile_audio_program(
            sequence,
            AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
        )?),
        contract(sequence, AudioProcessingMode::Offline),
        AudioKernelBackend::ScalarReference,
        resolver,
    )
}
fn render(runtime: &mut AudioProgramRuntime, partitions: &[usize]) -> Vec<f32> {
    if runtime.requires_state_entry() {
        runtime
            .enter_state(AudioStateEntry {
                epoch: AudioContinuityEpoch::new(1),
                start_sample: 0,
            })
            .unwrap();
    }
    let channels = runtime.render_contract().channel_count();
    let mut result = Vec::new();
    let mut start_sample = 0;
    for &frames in partitions {
        let mut pcm = vec![0.0; frames * channels];
        runtime
            .render_into(AudioRenderRequest { start_sample, frames }, &mut pcm)
            .unwrap();
        result.extend(pcm);
        start_sample += frames as i64;
    }
    result
}

#[test]
fn processor_outputs_route_intermediate_main_and_aux_fanout_once_per_partition() {
    for layout in [AudioChannelLayout::Mono, AudioChannelLayout::Stereo] {
        let sequence = fixture(layout);
        for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
            for partitions in [&[8][..], &[3, 1, 4][..]] {
                let resolver = OutputResolver::default();
                let mut runtime = AudioProgramRuntime::build_with_processor_resolver(
                    &sequence,
                    &[],
                    &RampResolver,
                    &resolver,
                    contract(&sequence, mode),
                    None,
                )
                .unwrap();
                let pcm = render(&mut runtime, partitions);
                let expected = (0..8)
                    .flat_map(|frame| {
                        (0..layout.channel_count())
                            .map(move |channel| 2.0 * (frame + 1) as f32 * (channel + 1) as f32)
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    pcm, expected,
                    "Main=2v, raw Main=v, two independent half sends=-v; not an end-of-rack alias"
                );
                assert_eq!(
                    resolver.calls.load(Ordering::SeqCst),
                    partitions.len(),
                    "fanout must not create or execute another instance"
                );
                assert_eq!(resolver.creates.load(Ordering::SeqCst), 2);
            }
        }
    }
}

#[test]
fn processor_output_latency_is_the_producer_not_the_rest_of_the_rack() {
    let sequence = fixture(AudioChannelLayout::Mono);
    let resolver = OutputResolver {
        multi_latency: 2,
        after_latency: 3,
        ..Default::default()
    };
    let prepared = plan(&sequence, &resolver).unwrap();
    assert_eq!(prepared.public_output_lookahead_frames(), 5);
    let main = prepared
        .schedule
        .routes
        .iter()
        .find(|route| {
            route.processor_output.is_some()
                && route.destination_slot == prepared.schedule.output_slot
        })
        .unwrap();
    assert_eq!(
        main.compensation_delay_frames, 3,
        "raw producer Main arrives after 2, later Main arrives after 5"
    );
    for partitions in [&[8][..], &[2, 3, 3][..]] {
        let mut runtime = AudioProgramRuntime::build_with_processor_resolver(
            &sequence,
            &[],
            &RampResolver,
            &resolver,
            contract(&sequence, AudioProcessingMode::Offline),
            None,
        )
        .unwrap();
        assert_eq!(
            render(&mut runtime, partitions),
            [2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0]
        );
    }
}

#[test]
fn processor_output_contract_key_order_layout_and_creation_are_revalidated() {
    let mut sequence = fixture(AudioChannelLayout::Mono);
    let resolver = OutputResolver::default();
    let prepared = plan(&sequence, &resolver).unwrap();
    let factory = &prepared.schedule.processors[0].factory;
    assert!(factory
        .with_native_author_seed(NativeConfigurationFormat::ClapState, &[0])
        .is_ok());
    assert!(
        factory
            .with_native_author_seed(NativeConfigurationFormat::ClapState, &[1])
            .is_err(),
        "same output names in a changed native binding order are a changed contract"
    );
    resolver.revision.store(1, Ordering::SeqCst);
    assert!(AudioRenderSession::new(Arc::new(prepared)).is_err());
    assert_eq!(resolver.creates.load(Ordering::SeqCst), 0);
    for revision in [2, 3] {
        resolver.revision.store(revision, Ordering::SeqCst);
        assert!(
            plan(&sequence, &resolver).is_err(),
            "duplicate keys or incompatible layouts cannot reach Session creation"
        );
    }
    resolver.revision.store(0, Ordering::SeqCst);
    for route in &mut sequence.audio_program.routes {
        if let AudioRouteSource::ProcessorOutput {
            port: AudioProcessorOutputPort::Auxiliary { bus_key },
            ..
        } = &mut route.source
        {
            *bus_key = "absent".into();
        }
    }
    assert!(plan(&sequence, &resolver).is_err());
    assert_eq!(resolver.creates.load(Ordering::SeqCst), 0);
}

#[test]
fn processor_output_fixed_grant_counts_unconnected_auxiliary_storage() {
    let sequence = fixture(AudioChannelLayout::Mono);
    let resolver = OutputResolver::default();
    let prepared = plan(&sequence, &resolver).unwrap();
    assert_eq!(
        prepared.schedule_summary().processor_output_buffer_count,
        3,
        "quarter is unrouted but requires writable native output storage"
    );
    let session = AudioRenderSession::new(Arc::new(prepared)).unwrap();
    assert_eq!(session.capacity().processor_output_samples, 3 * 16);
    let baseline = AudioProgramRuntime::build_with_processor_resolver(
        &sequence,
        &[],
        &RampResolver,
        &resolver,
        contract(&sequence, AudioProcessingMode::Offline),
        None,
    )
    .unwrap();
    let footprint = baseline.resource_footprint();
    let fresh = OutputResolver::default();
    assert!(matches!(
        AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
            &sequence,
            &[],
            &RampResolver,
            &fresh,
            contract(&sequence, AudioProcessingMode::Offline),
            None,
            AudioRuntimeResourceGrant::new(
                footprint.runtime_occurrences,
                footprint.fixed_resident_bytes - 1,
                footprint.prepared_logical_bytes
            )
        ),
        Err(AudioRuntimeBuildError::ResourceGrantExceeded {
            category: AudioRuntimeResourceCategory::FixedResidentBytes,
            ..
        })
    ));
    assert_eq!(fresh.creates.load(Ordering::SeqCst), 0);
    let mut exact = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &RampResolver,
        &fresh,
        contract(&sequence, AudioProcessingMode::Offline),
        None,
        AudioRuntimeResourceGrant::new(
            footprint.runtime_occurrences,
            footprint.fixed_resident_bytes,
            footprint.prepared_logical_bytes,
        ),
    )
    .unwrap();
    assert_eq!(exact.resource_footprint(), footprint);
    assert_eq!(render(&mut exact, &[2]), [2.0, 4.0]);
}

#[test]
fn processor_output_partial_failure_does_not_publish_or_reexecute_a_poisoned_window() {
    for fail_after in [false, true] {
        let sequence = fixture(AudioChannelLayout::Mono);
        let resolver = OutputResolver { fail_at: Some(2), fail_after, ..Default::default() };
        let mut runtime = AudioProgramRuntime::build_with_processor_resolver(
            &sequence,
            &[],
            &RampResolver,
            &resolver,
            contract(&sequence, AudioProcessingMode::Offline),
            None,
        )
        .unwrap();
        assert_eq!(render(&mut runtime, &[2]), [2.0, 4.0]);
        let mut pcm = [99.0; 2];
        assert!(runtime
            .render_into(AudioRenderRequest { start_sample: 2, frames: 2 }, &mut pcm)
            .is_err());
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            pcm, [0.0; 2],
            "failed window cannot expose an older or partial tap"
        );
        assert!(matches!(
            runtime.render_into(AudioRenderRequest { start_sample: 2, frames: 2 }, &mut pcm),
            Err(AudioExecutionError::ProcessorHost(
                AudioProcessorHostError::PoisonedInstance
            ))
        ));
        assert_eq!(pcm, [0.0; 2]);
        assert_eq!(
            resolver.calls.load(Ordering::SeqCst),
            2,
            "failed root cannot replay the DSP or publish partially written aux output"
        );
    }
}

#[test]
fn processor_outputs_use_the_same_direct_and_precompiled_range_graph() {
    let sequence = fixture(AudioChannelLayout::Mono);
    let range = TimelineTimeRange::new(TimelineTime::ZERO, tt(8, 48_000)).unwrap();
    let closure = compile_audio_dependency_closure(&sequence, &[], None, range).unwrap();
    let resolver = OutputResolver::default();
    let mut direct = AudioProgramRuntime::build_with_processor_resolver(
        &sequence,
        &[],
        &RampResolver,
        &resolver,
        contract(&sequence, AudioProcessingMode::Offline),
        None,
    )
    .unwrap();
    let mut precompiled = AudioProgramRuntime::build_from_precompiled_closure_for_range_with_processor_resolver_and_resource_grant(&sequence, &[], &RampResolver, &resolver, contract(&sequence, AudioProcessingMode::Offline), None, range, &closure, AudioRuntimeResourceGrant::new(16, usize::MAX, usize::MAX)).unwrap();
    assert_eq!(render(&mut precompiled, &[3, 5]), render(&mut direct, &[8]));
}

#[test]
fn processor_output_bypass_and_unknown_identity_fail_before_factory_creation() {
    let mut sequence = fixture(AudioChannelLayout::Mono);
    let resolver = OutputResolver::default();
    let track = sequence.audio_tracks[0].id;
    let producer = &mut sequence
        .audio_program
        .track_channels
        .get_mut(&track)
        .unwrap()
        .strip
        .pre_fader
        .processors[0];
    producer.bypassed = true;
    let id = producer.id;
    assert!(
        matches!(compile_audio_program(&sequence, AudioCompileRequest::program(sequence.audio_program.outputs[0].id)), Err(AudioCompileError::BypassedProcessorOutput(actual)) if actual == id)
    );
    assert_eq!(resolver.creates.load(Ordering::SeqCst), 0);
    sequence
        .audio_program
        .track_channels
        .get_mut(&track)
        .unwrap()
        .strip
        .pre_fader
        .processors[0]
        .bypassed = false;
    for route in &mut sequence.audio_program.routes {
        if let AudioRouteSource::ProcessorOutput { processor_id, .. } = &mut route.source {
            *processor_id = mondrian_core::AudioProcessorInstanceId::new();
        }
    }
    assert!(matches!(
        compile_audio_program(
            &sequence,
            AudioCompileRequest::program(sequence.audio_program.outputs[0].id)
        ),
        Err(AudioCompileError::InvalidAuthoring(
            mondrian_timeline::AudioAuthoringError::InvalidProcessorOutputSource(_)
        ))
    ));
}

#[test]
fn processor_output_instances_remain_distinct_and_raw_send_bypasses_mute_but_respects_solo() {
    let mut sequence = Sequence::new("independent raw outputs");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let first = sequence.audio_tracks[0].id;
    let second = sequence.add_audio_track();
    let output = sequence.audio_program.outputs[0].id;
    for (track, factor) in [(first, "1"), (second, "4")] {
        let processor = instance(&format!("owned.outputs.{factor}"));
        let id = processor.id;
        sequence
            .audio_program
            .track_channels
            .get_mut(&track)
            .unwrap()
            .strip
            .pre_fader
            .processors
            .push(processor);
        sequence.audio_program.routes.push(AudioRoute::new(
            raw(
                id,
                AudioProcessorOutputPort::Auxiliary { bus_key: "quarter".into() },
            ),
            AudioRouteDestination::Output(output),
        ));
    }
    sequence
        .audio_tracks
        .iter_mut()
        .find(|track| track.id == second)
        .unwrap()
        .is_muted = true;
    for solo in [false, true] {
        let mut request = AudioCompileRequest::program(output);
        if solo {
            request.audition.soloed_tracks.insert(first);
        }
        let resolver = OutputResolver::default();
        let mut runtime =
            AudioProgramRuntime::build_with_compile_request_processor_resolver_and_resource_grant(
                &sequence,
                &[],
                &RampResolver,
                &resolver,
                contract(&sequence, AudioProcessingMode::Offline),
                request,
                AudioRuntimeResourceGrant::new(16, usize::MAX, usize::MAX),
            )
            .unwrap();
        let factor = if solo { 1.25 } else { 2.25 };
        assert_eq!(render(&mut runtime, &[3, 1]), [factor, factor * 2.0, factor * 3.0, factor * 4.0],
            "raw bypasses final owner mute, while unsoloed Track sources share the existing audition filter");
        assert_eq!(
            resolver.calls.load(Ordering::SeqCst),
            if solo { 2 } else { 4 },
            "the unsoloed zero-input producer cannot materialize or leak an auxiliary output"
        );
        assert_eq!(
            resolver.creates.load(Ordering::SeqCst),
            if solo { 1 } else { 2 }
        );
    }
}

#[test]
fn processor_output_auxiliary_source_feeds_existing_sidechain_and_pdc() {
    for same_owner in [false, true] {
        let mut sequence = Sequence::new("raw output sidechain");
        sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
        let first = sequence.audio_tracks[0].id;
        let second = if same_owner {
            first
        } else {
            sequence.add_audio_track()
        };
        let producer = instance("owned.outputs.1");
        let id = producer.id;
        let detector = instance("owned.outputs.detector");
        let target = detector.id;
        sequence
            .audio_program
            .track_channels
            .get_mut(&first)
            .unwrap()
            .strip
            .pre_fader
            .processors
            .push(producer);
        sequence
            .audio_program
            .track_channels
            .get_mut(&second)
            .unwrap()
            .strip
            .post_fader
            .processors
            .push(detector);
        sequence.audio_program.routes.clear();
        sequence.audio_program.routes.push(AudioRoute::new(
            AudioRouteSource::Track {
                track_id: second,
                port: AudioChannelStripOutputPort::PostMute,
            },
            AudioRouteDestination::Output(sequence.audio_program.outputs[0].id),
        ));
        sequence.audio_program.sidechain_routes.push(AudioProcessorSidechainRoute::new(
            raw(
                id,
                AudioProcessorOutputPort::Auxiliary { bus_key: "half".into() },
            ),
            target,
            "side",
        ));
        let resolver = OutputResolver { multi_latency: 2, ..Default::default() };
        for partitions in [&[8][..], &[3, 1, 4][..]] {
            let mut runtime = AudioProgramRuntime::build_with_processor_resolver(
                &sequence,
                &[],
                &RampResolver,
                &resolver,
                contract(&sequence, AudioProcessingMode::Offline),
                None,
            )
            .unwrap();
            assert_eq!(
                render(&mut runtime, partitions),
                if same_owner {
                    [0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0]
                } else {
                    [-0.5, -1.0, -1.5, -2.0, -2.5, -3.0, -3.5, -4.0]
                }
            );
        }
    }
}

#[test]
fn processor_output_bus_rack_and_post_fader_main_tap_keep_their_actual_positions() {
    let mut sequence = Sequence::new("Bus output tap");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
    sequence.audio_program.routes.clear();
    let producer = instance("owned.outputs.2");
    let pre_id = producer.id;
    let post = instance("owned.outputs.after");
    let post_id = post.id;
    let bus = mondrian_core::MixBusId::new();
    let mut strip = mondrian_timeline::AudioChannelStrip {
        fader_db: 6.020_599_913_279_624,
        ..Default::default()
    };
    strip.pre_fader.processors.push(producer);
    strip.post_fader.processors.push(post);
    sequence
        .audio_program
        .buses
        .push(AudioMixBus { id: bus, name: "Raw Bus".into(), strip });
    let destination = AudioRouteDestination::Output(sequence.audio_program.outputs[0].id);
    sequence.audio_program.routes.extend([
        AudioRoute::new(
            AudioRouteSource::Bus {
                bus_id: bus,
                port: AudioChannelStripOutputPort::PostMute,
            },
            destination,
        ),
        AudioRoute::new(raw(post_id, AudioProcessorOutputPort::Main), destination),
        AudioRoute::new(
            raw(
                pre_id,
                AudioProcessorOutputPort::Auxiliary { bus_key: "half".into() },
            ),
            destination,
        ),
    ]);
    let resolver = OutputResolver::default();
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver(
        &sequence,
        &[],
        &RampResolver,
        &resolver,
        contract(&sequence, AudioProcessingMode::Offline),
        None,
    )
    .unwrap();
    assert_eq!(
        render(&mut runtime, &[4]),
        [15.0, 30.0, 45.0, 60.0],
        "post source includes the preceding fader; aux remains the pre-fader producer's own value"
    );
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn processor_output_metadata_fee_and_retained_clone_ledger_use_real_capacities() {
    let sequence = fixture(AudioChannelLayout::Mono);
    let ordinary = OutputResolver::default();
    let reserved = OutputResolver::default();
    reserved.revision.store(5, Ordering::SeqCst);
    let ordinary_plan = plan(&sequence, &ordinary).unwrap();
    let reserved_plan = plan(&sequence, &reserved).unwrap();
    assert_eq!(
        ordinary_plan.schedule_summary(),
        reserved_plan.schedule_summary(),
        "identical routing/key values and structural counts"
    );
    let old_bytes = ordinary_plan.schedule.output_metadata_bytes().unwrap();
    let reserved_bytes = reserved_plan.schedule.output_metadata_bytes().unwrap();
    assert!(
        reserved_bytes > old_bytes,
        "same two short keys retain extra binding Vec and String capacity"
    );
    assert_eq!(
        reserved_plan.session_resource_footprint().unwrap().fixed_resident_bytes
            - ordinary_plan.session_resource_footprint().unwrap().fixed_resident_bytes,
        reserved_bytes - old_bytes
    );
    let cloned = reserved_plan.clone();
    assert!(
        cloned.schedule.output_metadata_bytes().unwrap() < reserved_bytes,
        "Vec/String clone owns its actual smaller capacity"
    );
    assert_eq!(
        reserved_plan.session_resource_footprint().unwrap().fixed_resident_bytes
            - cloned.session_resource_footprint().unwrap().fixed_resident_bytes,
        reserved_bytes - cloned.schedule.output_metadata_bytes().unwrap()
    );
    let mut owners = Vec::new();
    for candidate in [&reserved_plan, &cloned] {
        candidate
            .visit_native_host_owned_storage(&mut |owner| {
                owners.push(owner);
                Ok(())
            })
            .unwrap();
    }
    let mut expected = Vec::new();
    for candidate in [&reserved_plan, &cloned] {
        expected.push(
            AudioProcessorFactoryStorageBorrow::complete(
                &candidate.schedule.processor_output_buses,
            )
            .unwrap(),
        );
        for bus in &candidate.schedule.processor_output_buses {
            expected.push(AudioProcessorFactoryStorageBorrow::complete(&bus.bus_key).unwrap());
        }
        for processor in &candidate.schedule.processors {
            processor
                .factory
                .visit_auxiliary_output_storage(&mut |owner| {
                    if owner.layout().size() != 0 {
                        expected.push(owner);
                    }
                    Ok(())
                })
                .unwrap();
        }
    }
    for wanted in &expected {
        assert!(owners.iter().any(|owner| owner.address() == wanted.address() && owner.layout() == wanted.layout() && owner.complete_allocation().is_some()), "old/new descriptor and binding output owners are not aliases merely because keys compare equal");
    }
    let mut ledger = NativeHostAllocationLedger::new(expected.len(), usize::MAX).unwrap();
    for wanted in &expected {
        let actual = owners
            .iter()
            .find(|owner| owner.address() == wanted.address() && owner.layout() == wanted.layout())
            .unwrap()
            .complete_allocation()
            .unwrap();
        ledger.record(actual).unwrap();
        ledger.record(actual).unwrap();
    }
    assert_eq!(
        ledger.finish().unwrap().allocation_bytes(),
        expected.iter().map(|owner| owner.layout().size()).sum::<usize>()
            + NativeHostAllocationLedger::storage_layout(expected.len())
                .unwrap()
                .bytes()
                .unwrap()
    );
    let runtime = AudioProgramRuntime::build_with_processor_resolver(
        &sequence,
        &[],
        &RampResolver,
        &reserved,
        contract(&sequence, AudioProcessingMode::Offline),
        None,
    )
    .unwrap();
    let initial = runtime.resource_footprint();
    let current = runtime.find_resource_session(runtime.resource_root_identity()).unwrap();
    // The actual replacement must share the installed render backend and have
    // a real fresh preparation identity. The earlier ScalarReference clone is
    // only the independent capacity/owner control, not this Runtime's candidate.
    let actual_prepared = PreparedAudioPlan::prepare_with_processor_resolver(
        Arc::new(current.resource_plan().program().clone()),
        current.resource_plan().contract(),
        current.resource_plan().kernel_backend(),
        &reserved,
    )
    .unwrap();
    let actual_prepared_metadata_bytes = actual_prepared.schedule.output_metadata_bytes().unwrap();
    let actual_next = actual_prepared.clone();
    assert_eq!(actual_next.contract(), current.resource_plan().contract());
    assert_eq!(
        actual_next.kernel_backend(),
        current.resource_plan().kernel_backend()
    );
    assert_eq!(
        actual_next.schedule_summary(),
        current.resource_plan().schedule_summary()
    );
    assert_ne!(
        actual_next.preparation_id(),
        current.resource_plan().preparation_id()
    );
    assert!(
        actual_next.schedule.output_metadata_bytes().unwrap() < actual_prepared_metadata_bytes,
        "the same-backend fresh candidate clone retains its actual smaller metadata capacity"
    );
    drop(actual_prepared);
    let old = current.current_resource_footprint().unwrap();
    let next = actual_next.session_resource_footprint().unwrap();
    let events = Vec::new();
    let lanes = Vec::new();
    let replacements = [AudioParameterPlanResourceReplacement::new(
        current,
        &actual_next,
        &events,
        &lanes,
    )];
    let mut extra = Vec::new();
    for candidate in [current.resource_plan(), &actual_next] {
        candidate
            .visit_native_host_owned_storage(&mut |owner| {
                if let Some(allocation) = owner.complete_allocation() {
                    extra.push(allocation);
                }
                Ok(())
            })
            .unwrap();
    }
    let commit = runtime
        .admit_parameter_plan_overlap(&replacements, &extra)
        .unwrap()
        .into_commit(&runtime, &replacements)
        .unwrap();
    let published = commit.footprint();
    assert_eq!(
        published.render_scratch_bytes,
        initial.render_scratch_bytes - old.render_scratch_bytes + next.render_scratch_bytes
    );
    assert_eq!(
        published.fixed_resident_bytes,
        initial.fixed_resident_bytes - old.render_scratch_bytes + next.render_scratch_bytes
            - old.parameter_event_bytes
    );
    assert!(
        published.fixed_resident_bytes < initial.fixed_resident_bytes,
        "the actual next clone's smaller metadata capacity replaces the old cached fee"
    );
    assert_eq!(
        runtime.resource_footprint(),
        initial,
        "resource commit alone does not install or publish a new author/Source owner"
    );
    let baseline = AudioProgramRuntime::build_with_processor_resolver(
        &sequence,
        &[],
        &RampResolver,
        &reserved,
        contract(&sequence, AudioProcessingMode::Offline),
        None,
    )
    .unwrap();
    let footprint = baseline.resource_footprint();
    let fresh = OutputResolver::default();
    fresh.revision.store(5, Ordering::SeqCst);
    assert!(matches!(
        AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
            &sequence,
            &[],
            &RampResolver,
            &fresh,
            contract(&sequence, AudioProcessingMode::Offline),
            None,
            AudioRuntimeResourceGrant::new(
                footprint.runtime_occurrences,
                footprint.fixed_resident_bytes - 1,
                footprint.prepared_logical_bytes
            )
        ),
        Err(AudioRuntimeBuildError::ResourceGrantExceeded {
            category: AudioRuntimeResourceCategory::FixedResidentBytes,
            ..
        })
    ));
    assert_eq!(fresh.creates.load(Ordering::SeqCst), 0);
    let exact = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &RampResolver,
        &fresh,
        contract(&sequence, AudioProcessingMode::Offline),
        None,
        AudioRuntimeResourceGrant::new(
            footprint.runtime_occurrences,
            footprint.fixed_resident_bytes,
            footprint.prepared_logical_bytes,
        ),
    )
    .unwrap();
    assert_eq!(exact.resource_footprint(), footprint);
}
